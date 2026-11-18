//! Best-effort CPU temperature from the ACPI thermal zone perf counter.
//!
//! This is the *platform* thermal zone (often `\_TZ.THRM`), not an Intel DTS /
//! AMD Tctl core sensor. It is only reported when the user opted into ACPI
//! (`cpu_temp_source = "acpi"` or `"auto"`), and every value is validated for
//! range before it is exposed.
//!
//! ACPI zones are notorious for getting stuck at a plausible-looking constant
//! (firmware stops updating `_TMP`). A reading that does not move for five
//! minutes while the CPU load is visibly changing is therefore treated as
//! stale and suppressed (`None`); it starts being reported again as soon as the
//! value actually moves. When the system is idle or the load is constant there
//! is no evidence either way, so a frozen value is still shown.

use std::time::{Duration, Instant};

use oxide_core::CpuTempSource;

use crate::pdh::{CounterValue, PdhCounter, PdhQuery};

const THERMAL_COUNTER: &str = r"\Thermal Zone Information(*)\High Precision Temperature";

const MIN_PLAUSIBLE_C: f32 = -40.0;
const MAX_PLAUSIBLE_C: f32 = 150.0;
/// A raw `0` / exactly `0.0 °C` is a placeholder on many firmware builds.
const ZERO_EPSILON_C: f32 = 0.05;
/// Movement smaller than this is counter noise, not a real temperature change.
const FROZEN_EPSILON_C: f32 = 0.1;
/// CPU load must move by at least this many percentage points before the
/// frozen-value heuristic considers the load "changing".
const LOAD_CHANGE_PCT: f32 = 5.0;
/// A value stuck for this long while the load changed is considered stale.
const FROZEN_AFTER: Duration = Duration::from_secs(5 * 60);

/// Convert the counter's tenths-of-Kelvin value to degrees Celsius.
pub(crate) fn kelvin_tenths_to_celsius(value: f64) -> f32 {
    (value / 10.0 - 273.15) as f32
}

fn plausible_temp(celsius: f32) -> bool {
    celsius.is_finite()
        && (MIN_PLAUSIBLE_C..=MAX_PLAUSIBLE_C).contains(&celsius)
        && celsius.abs() > ZERO_EPSILON_C
}

/// Prefer the `THRM` zone, then the first plausible zone.
fn pick_zone(values: &[CounterValue]) -> Option<f32> {
    let mut fallback = None;
    for value in values {
        let celsius = kelvin_tenths_to_celsius(value.value);
        if !plausible_temp(celsius) {
            continue;
        }
        if value.name.to_ascii_uppercase().contains("THRM") {
            return Some(celsius);
        }
        if fallback.is_none() {
            fallback = Some(celsius);
        }
    }
    fallback
}

/// Pure decision helper: a frozen reading is stale only when the load has
/// changed during the frozen window. Time is injected so this is testable
/// without sleeping.
pub(crate) fn frozen_is_stale(
    now: Instant,
    last_change: Instant,
    load_changed: bool,
    window: Duration,
) -> bool {
    load_changed && now.saturating_duration_since(last_change) >= window
}

/// Tracks whether the chosen thermal zone is stuck. `observe` is called once
/// per poll with the freshly read value and the current CPU load; it returns
/// `None` while the value is considered stale.
#[derive(Clone, Debug)]
pub(crate) struct FrozenTracker {
    window: Duration,
    last_value: Option<f32>,
    last_change: Instant,
    last_load: Option<f32>,
    load_changed: bool,
    stale: bool,
}

impl FrozenTracker {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            window,
            last_value: None,
            last_change: Instant::now(),
            last_load: None,
            load_changed: false,
            stale: false,
        }
    }

    /// Forget all history. Called when a poll could not read a value, so an
    /// availability gap is not mistaken for a frozen sensor.
    pub(crate) fn reset(&mut self) {
        self.last_value = None;
        self.last_load = None;
        self.load_changed = false;
        self.stale = false;
    }

    pub(crate) fn stale(&self) -> bool {
        self.stale
    }

    pub(crate) fn observe(&mut self, value: f32, cpu_load: f32, now: Instant) -> Option<f32> {
        let load = cpu_load.is_finite().then_some(cpu_load.clamp(0.0, 100.0));

        let Some(previous) = self.last_value else {
            self.last_value = Some(value);
            self.last_change = now;
            self.last_load = load;
            self.load_changed = false;
            self.stale = false;
            return Some(value);
        };

        if (value - previous).abs() > FROZEN_EPSILON_C {
            self.last_value = Some(value);
            self.last_change = now;
            self.last_load = load;
            self.load_changed = false;
            self.stale = false;
            return Some(value);
        }

        // The value is frozen. Remember whether the CPU load moved while it has
        // been stuck; only then is a constant temperature suspicious.
        if let (Some(previous_load), Some(current_load)) = (self.last_load, load)
            && (current_load - previous_load).abs() >= LOAD_CHANGE_PCT
        {
            self.load_changed = true;
        }
        self.last_load = load;
        // Deliberately keep `last_value` at the level recorded when the zone
        // last *moved*: slow drift then accumulates until it exceeds the
        // epsilon and counts as a real change instead of looking frozen.

        if frozen_is_stale(now, self.last_change, self.load_changed, self.window) {
            self.stale = true;
        }

        (!self.stale).then_some(value)
    }
}

pub(crate) struct AcpiSampler {
    enabled: bool,
    query: Option<PdhQuery>,
    counter: Option<PdhCounter>,
    tracker: FrozenTracker,
    warned: bool,
    warned_frozen: bool,
}

impl AcpiSampler {
    pub(crate) fn new(source: CpuTempSource) -> Self {
        Self::with_window(source, FROZEN_AFTER)
    }

    fn with_window(source: CpuTempSource, window: Duration) -> Self {
        let enabled = matches!(source, CpuTempSource::Acpi | CpuTempSource::Auto);
        if !enabled {
            return Self {
                enabled,
                query: None,
                counter: None,
                tracker: FrozenTracker::new(window),
                warned: true,
                warned_frozen: false,
            };
        }

        let query = PdhQuery::open();
        let counter = query
            .as_ref()
            .and_then(|query| query.add_counter(THERMAL_COUNTER));
        let warned = counter.is_none();
        if warned {
            // On many desktops there is simply no thermal zone counter; this is
            // not an error for `auto` mode.
            tracing::debug!("ACPI thermal zone PDH counter unavailable");
        }

        Self {
            enabled,
            query,
            counter,
            tracker: FrozenTracker::new(window),
            warned,
            warned_frozen: false,
        }
    }

    /// `cpu_load` is the current CPU usage percentage (0..100). It is only used
    /// by the frozen-value heuristic; a missing/failed load reading disables
    /// the heuristic for that tick rather than suppressing a valid temperature.
    pub(crate) fn poll(&mut self, cpu_load: f32) -> Option<f32> {
        if !self.enabled {
            return None;
        }

        let (Some(query), Some(counter)) = (&self.query, self.counter) else {
            if !self.warned {
                tracing::debug!("ACPI thermal zone PDH counter unavailable");
                self.warned = true;
            }
            self.tracker.reset();
            return None;
        };

        if !query.collect() {
            self.tracker.reset();
            return None;
        }
        let Some(values) = counter.array() else {
            self.tracker.reset();
            return None;
        };
        let Some(celsius) = pick_zone(&values) else {
            self.tracker.reset();
            return None;
        };

        let reported = self.tracker.observe(celsius, cpu_load, Instant::now());
        if reported.is_none() && self.tracker.stale() {
            if !self.warned_frozen {
                tracing::debug!(
                    temperature = celsius,
                    "ACPI thermal zone value is frozen while CPU load changes; suppressing it"
                );
                self.warned_frozen = true;
            }
        } else if reported.is_some() {
            self.warned_frozen = false;
        }
        reported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(name: &str, value: f64) -> CounterValue {
        CounterValue {
            name: name.to_string(),
            value,
        }
    }

    #[test]
    fn tenths_kelvin_to_celsius() {
        assert!((kelvin_tenths_to_celsius(3212.0) - 48.05).abs() < 0.01);
        assert!((kelvin_tenths_to_celsius(2731.5) - 0.0).abs() < 0.01);
    }

    #[test]
    fn prefers_thrm_zone() {
        let values = vec![reading(r"\_TZ.TZ00", 3100.0), reading(r"\_TZ.THRM", 3212.0)];
        let temp = pick_zone(&values).expect("temperature");
        assert!((temp - 48.05).abs() < 0.01);
    }

    #[test]
    fn rejects_implausible_values() {
        for raw in [0.0, f64::NAN, f64::INFINITY, 400.0, 5000.0] {
            let values = vec![reading(r"\_TZ.THRM", raw)];
            assert_eq!(pick_zone(&values), None, "raw value {raw}");
        }
    }

    #[test]
    fn rejects_exactly_zero_celsius() {
        // 2731.5 tenths K = 0.0 °C, a common firmware placeholder.
        let values = vec![reading(r"\_TZ.THRM", 2731.5)];
        assert_eq!(pick_zone(&values), None);
    }

    #[test]
    fn falls_back_to_first_valid_zone() {
        let values = vec![reading("invalid", 0.0), reading(r"\_TZ.TZ01", 3002.0)];
        let temp = pick_zone(&values).expect("temperature");
        assert!((temp - 27.05).abs() < 0.01);
    }

    #[test]
    fn frozen_decision_requires_load_change_and_window() {
        let start = Instant::now();
        let window = Duration::from_secs(300);
        assert!(!frozen_is_stale(
            start + Duration::from_secs(301),
            start,
            false,
            window
        ));
        assert!(!frozen_is_stale(
            start + Duration::from_secs(299),
            start,
            true,
            window
        ));
        assert!(frozen_is_stale(
            start + Duration::from_secs(300),
            start,
            true,
            window
        ));
    }

    #[test]
    fn tracker_reports_moving_values() {
        let start = Instant::now();
        let mut tracker = FrozenTracker::new(Duration::from_secs(300));
        assert_eq!(tracker.observe(48.0, 5.0, start), Some(48.0));
        assert_eq!(
            tracker.observe(49.0, 50.0, start + Duration::from_secs(10)),
            Some(49.0)
        );
        assert!(!tracker.stale());
    }

    #[test]
    fn tracker_counts_slow_drift_as_movement() {
        let start = Instant::now();
        let mut tracker = FrozenTracker::new(Duration::from_secs(300));
        assert_eq!(tracker.observe(48.0, 5.0, start), Some(48.0));
        // Each step is below the 0.1 °C epsilon, but the accumulated drift is a
        // real change and must reset the frozen window.
        assert_eq!(
            tracker.observe(48.09, 60.0, start + Duration::from_secs(60)),
            Some(48.09)
        );
        assert_eq!(
            tracker.observe(48.18, 60.0, start + Duration::from_secs(120)),
            Some(48.18)
        );
        assert_eq!(
            tracker.observe(48.27, 60.0, start + Duration::from_secs(180)),
            Some(48.27)
        );
        assert!(!tracker.stale());
    }

    #[test]
    fn tracker_suppresses_value_frozen_while_load_changes() {
        let start = Instant::now();
        let mut tracker = FrozenTracker::new(Duration::from_secs(300));

        assert_eq!(tracker.observe(48.0, 5.0, start), Some(48.0));
        // Load changes but the zone stays at 48.0 for the whole window.
        assert_eq!(
            tracker.observe(48.0, 55.0, start + Duration::from_secs(30)),
            Some(48.0)
        );
        assert_eq!(
            tracker.observe(48.0, 55.0, start + Duration::from_secs(299)),
            Some(48.0)
        );
        assert_eq!(
            tracker.observe(48.0, 55.0, start + Duration::from_secs(301)),
            None
        );
        assert!(tracker.stale());

        // A real movement resets the detector.
        assert_eq!(
            tracker.observe(50.5, 55.0, start + Duration::from_secs(302)),
            Some(50.5)
        );
        assert!(!tracker.stale());
    }

    #[test]
    fn tracker_keeps_frozen_value_when_system_is_idle() {
        let start = Instant::now();
        let mut tracker = FrozenTracker::new(Duration::from_secs(300));
        assert_eq!(tracker.observe(40.0, 0.5, start), Some(40.0));
        // Stable idle load: a constant zone is expected and still reported.
        assert_eq!(
            tracker.observe(40.0, 0.5, start + Duration::from_secs(600)),
            Some(40.0)
        );
        assert!(!tracker.stale());
    }

    #[test]
    fn tracker_reset_clears_stale_state() {
        let start = Instant::now();
        let mut tracker = FrozenTracker::new(Duration::from_secs(60));
        assert_eq!(tracker.observe(48.0, 5.0, start), Some(48.0));
        assert_eq!(
            tracker.observe(48.0, 55.0, start + Duration::from_secs(61)),
            None
        );
        tracker.reset();
        assert!(!tracker.stale());
        assert_eq!(
            tracker.observe(48.0, 55.0, start + Duration::from_secs(62)),
            Some(48.0)
        );
    }
}
