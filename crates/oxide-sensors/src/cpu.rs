//! CPU load and frequency.
//!
//! Load is primarily PDH `\Processor Information(_Total)\% Processor Utility`
//! (the Task Manager metric), added through `PdhAddEnglishCounterW` so it works
//! on localized Windows. Any failure — no counter, invalid data, virtual
//! machine, ARM64 — falls back to `GetSystemTimes`. The first sample is always
//! zero and every value is range-checked before it is exposed.
//!
//! Frequency prefers `CallNtPowerInformation(ProcessorInformation)` and falls
//! back to PDH `Processor Frequency`, then `% Processor Performance` multiplied
//! by the nominal maximum. On systems where the power API fails or reports a
//! zero `MhzLimit`/`CurrentMhz` (some ARM64 devices and VMs) the PDH path is
//! used; when no plausible value exists the sample carries `None` instead of a
//! fake 0 MHz.

use std::mem::size_of;

use oxide_core::CpuSample;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Power::{
    CallNtPowerInformation, PROCESSOR_POWER_INFORMATION, ProcessorInformation,
};
use windows::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetSystemTimes,
};

use crate::pdh::{PdhCounter, PdhQuery};

const UTILITY_COUNTER: &str = r"\Processor Information(_Total)\% Processor Utility";
const PERFORMANCE_COUNTER: &str = r"\Processor Information(_Total)\% Processor Performance";
const FREQUENCY_COUNTER: &str = r"\Processor Information(_Total)\Processor Frequency";

/// Upper plausibility bound for a CPU clock. No shipping Windows part exceeds
/// ~6 GHz; 20 GHz still rejects counter garbage and VM placeholders.
const MAX_PLAUSIBLE_MHZ: f32 = 20_000.0;
/// A nominal maximum below this is a placeholder (0/1), not a real CPU.
const MIN_PLAUSIBLE_MAX_MHZ: u32 = 100;
/// `% Processor Utility` legitimately exceeds 100 % with Turbo. Values above
/// this are a broken counter or a confused VM, not real headroom, so they are
/// rejected and the fallback is used rather than clamped into a fake 100 %.
const MAX_PLAUSIBLE_UTILITY: f32 = 200.0;
/// `GetActiveProcessorCount` is trusted, but a corrupted/emulated value must
/// not allocate an unbounded buffer.
const MAX_PROCESSOR_COUNT: u32 = 1_024;

/// Task Manager clamps the bar to 0..100 even though utility can exceed 100 %.
fn clamp_pct(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// Validate a PDH percentage. `None` means "the source failed; use the
/// fallback". Real Turbo headroom (100..1000 %) is accepted and clamped for
/// display; negative, non-finite or absurd values are rejected.
fn validate_pct(value: f64) -> Option<f32> {
    if !value.is_finite() {
        return None;
    }
    let value = value as f32;
    if !(0.0..=MAX_PLAUSIBLE_UTILITY).contains(&value) {
        return None;
    }
    Some(clamp_pct(value))
}

fn filetime_to_u64(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

/// Busy percentage from two `GetSystemTimes` snapshots. `None` means "no
/// usable delta": the first sample, a counter reset (suspend/resume), or an
/// interval with no elapsed time.
fn busy_pct(previous: Option<(u64, u64)>, current: (u64, u64)) -> Option<f32> {
    let (prev_idle, prev_total) = previous?;
    let (idle, total) = current;
    if total < prev_total || idle < prev_idle {
        return None;
    }
    let delta_total = total - prev_total;
    if delta_total == 0 {
        return None;
    }
    let delta_idle = (idle - prev_idle).min(delta_total);
    Some(clamp_pct(
        ((delta_total - delta_idle) as f64 / delta_total as f64 * 100.0) as f32,
    ))
}

/// Nominal maximum clock reported by the OS. 0/1 and absurdly large values are
/// placeholders, not a real CPU scale, and must not poison the plausibility
/// checks.
fn plausible_nominal_mhz(mhz: u32) -> Option<u32> {
    (MIN_PLAUSIBLE_MAX_MHZ..=MAX_PLAUSIBLE_MHZ as u32)
        .contains(&mhz)
        .then_some(mhz)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PowerFrequency {
    current_mhz: u32,
    max_mhz: u32,
    limit_mhz: u32,
}

/// Query `PROCESSOR_POWER_INFORMATION` into a caller-owned buffer. Returns
/// `None` when the call fails or when the OS reports an entirely zeroed result
/// (seen on some ARM64/VM configurations even on success).
fn query_power_frequency(buffer: &mut [PROCESSOR_POWER_INFORMATION]) -> Option<PowerFrequency> {
    if buffer.is_empty() {
        return None;
    }
    let bytes = buffer
        .len()
        .saturating_mul(size_of::<PROCESSOR_POWER_INFORMATION>()) as u32;
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            None,
            0,
            Some(buffer.as_mut_ptr().cast()),
            bytes,
        )
    };
    if status.0 < 0 {
        return None;
    }

    let info = buffer.first()?;
    let frequency = PowerFrequency {
        current_mhz: info.CurrentMhz,
        max_mhz: info.MaxMhz,
        limit_mhz: info.MhzLimit,
    };
    (frequency.current_mhz > 0 || frequency.max_mhz > 0 || frequency.limit_mhz > 0)
        .then_some(frequency)
}

/// Reject 0, negative, non-finite and >20 GHz values. When the nominal maximum
/// is known, values several times above it are also rejected (bad PDH instance,
/// VM placeholder, unit confusion).
fn validate_mhz(value: f32, max_mhz: Option<f32>) -> Option<f32> {
    if !value.is_finite() || value <= 0.0 || value > MAX_PLAUSIBLE_MHZ {
        return None;
    }
    if let Some(max_mhz) = max_mhz.filter(|max| max.is_finite() && *max > 0.0)
        && value > max_mhz * 3.0
    {
        return None;
    }
    Some(value)
}

/// `% Processor Performance` is relative to the nominal clock.
fn performance_to_mhz(performance_pct: f32, max_mhz: f32) -> Option<f32> {
    if !performance_pct.is_finite() || performance_pct <= 0.0 || performance_pct > 500.0 {
        return None;
    }
    validate_mhz(max_mhz * performance_pct / 100.0, Some(max_mhz))
}

/// Suppresses a single wildly discontinuous *upward* frequency sample (resume,
/// VM migration, counter glitch) while still accepting a real transition on
/// the following poll. Downward transitions and normal idle→Turbo jumps are
/// always accepted so a bursty laptop does not flicker between a value and
/// `None`. A missing sample resets the baseline.
#[derive(Default)]
struct FrequencyGuard {
    last: Option<f32>,
}

impl FrequencyGuard {
    fn accept(&mut self, value: Option<f32>, max_mhz: Option<f32>) -> Option<f32> {
        let Some(value) = value else {
            self.last = None;
            return None;
        };

        // Nominal max is not the Turbo ceiling; 1.5x covers real boost clocks.
        let ceiling = max_mhz
            .filter(|max| max.is_finite() && *max > 0.0)
            .map(|max| max * 1.5)
            .unwrap_or(MAX_PLAUSIBLE_MHZ);

        let upward_spike = self
            .last
            .is_some_and(|previous| value > previous + 1_000.0 && value > ceiling);

        // Remember the new level even when it is suppressed: if the value
        // persists it is the real clock and is reported next tick.
        self.last = Some(value);
        (!upward_spike).then_some(value)
    }
}

pub(crate) struct CpuSampler {
    query: Option<PdhQuery>,
    utility: Option<PdhCounter>,
    performance: Option<PdhCounter>,
    frequency: Option<PdhCounter>,
    primed: bool,
    last_times: Option<(u64, u64)>,
    power_buffer: Vec<PROCESSOR_POWER_INFORMATION>,
    max_mhz: Option<u32>,
    freq_guard: FrequencyGuard,
    warned: bool,
}

impl CpuSampler {
    pub(crate) fn new() -> Self {
        let query = PdhQuery::open();
        let (utility, performance, frequency) = match &query {
            Some(query) => (
                query.add_counter(UTILITY_COUNTER),
                query.add_counter(PERFORMANCE_COUNTER),
                query.add_counter(FREQUENCY_COUNTER),
            ),
            None => (None, None, None),
        };

        let count = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) }
            .clamp(1, MAX_PROCESSOR_COUNT) as usize;
        let mut power_buffer = vec![PROCESSOR_POWER_INFORMATION::default(); count];
        let max_mhz = query_power_frequency(&mut power_buffer)
            .map(|frequency| frequency.max_mhz)
            .and_then(plausible_nominal_mhz);

        Self {
            query,
            utility,
            performance,
            frequency,
            primed: false,
            last_times: None,
            power_buffer,
            max_mhz,
            freq_guard: FrequencyGuard::default(),
            warned: false,
        }
    }

    pub(crate) fn poll(&mut self) -> CpuSample {
        let collected = match &self.query {
            Some(query) => query.collect(),
            None => false,
        };

        let mut usage = None;
        if collected {
            if self.primed {
                usage = self
                    .utility
                    .and_then(|counter| counter.value())
                    .and_then(validate_pct);
            } else {
                // First PDH sample is only a baseline.
                self.primed = true;
            }
        }

        if self.utility.is_none() && !self.warned {
            // Do not spam the log; PDH can be unavailable on stripped-down
            // images, VMs and some ARM64 configurations.
            tracing::debug!("PDH CPU counters unavailable; using GetSystemTimes fallback");
            self.warned = true;
        }

        let usage_pct = usage.unwrap_or_else(|| self.fallback_usage());

        let power = query_power_frequency(&mut self.power_buffer);
        if let Some(power) = power
            && let Some(max_mhz) = plausible_nominal_mhz(power.max_mhz)
        {
            self.max_mhz = Some(max_mhz);
        }
        let freq_mhz = self.frequency_mhz(power);

        CpuSample {
            usage_pct,
            freq_mhz,
            temp_c: None,
            temp_source: Default::default(),
        }
    }

    /// `GetSystemTimes` based total-busy percentage. `KernelTime` includes
    /// `IdleTime`, so busy = 1 - idle / (kernel + user). The first call and any
    /// counter reset (suspend/resume) produce 0 instead of a bogus delta.
    fn fallback_usage(&mut self) -> f32 {
        let mut idle = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let result = unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) };
        if result.is_err() {
            return 0.0;
        }

        let idle = filetime_to_u64(idle);
        let total = filetime_to_u64(kernel).saturating_add(filetime_to_u64(user));
        busy_pct(self.last_times.replace((idle, total)), (idle, total)).unwrap_or(0.0)
    }

    fn frequency_mhz(&mut self, power: Option<PowerFrequency>) -> Option<f32> {
        let max_mhz = self.max_mhz.map(|mhz| mhz as f32);

        // CallNtPowerInformation is the cheapest stable source, but a zero
        // MhzLimit/CurrentMhz means "not implemented here" (ARM64/VM), so the
        // PDH counters take over.
        let power_mhz = power
            .filter(|power| power.current_mhz > 0 && power.limit_mhz > 0)
            .map(|power| power.current_mhz as f32)
            .and_then(|mhz| validate_mhz(mhz, max_mhz));

        let pdh_frequency = self
            .frequency
            .and_then(|counter| counter.value())
            .and_then(|value| validate_mhz(value as f32, max_mhz));

        let pdh_performance = self
            .performance
            .and_then(|counter| counter.value())
            .and_then(|value| {
                let max_mhz = max_mhz?;
                performance_to_mhz(value as f32, max_mhz)
            });

        let candidate = power_mhz.or(pdh_frequency).or(pdh_performance);
        self.freq_guard.accept(candidate, max_mhz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentage_is_clamped_and_nan_safe() {
        assert_eq!(clamp_pct(42.5), 42.5);
        assert_eq!(clamp_pct(140.0), 100.0);
        assert_eq!(clamp_pct(-3.0), 0.0);
        assert_eq!(clamp_pct(f32::NAN), 0.0);
    }

    #[test]
    fn percentage_validation_handles_zero_nan_and_huge() {
        assert_eq!(validate_pct(42.5), Some(42.5));
        assert_eq!(validate_pct(0.0), Some(0.0));
        assert_eq!(validate_pct(140.0), Some(100.0));
        assert_eq!(validate_pct(250.0), None);
        assert_eq!(validate_pct(f64::NAN), None);
        assert_eq!(validate_pct(f64::INFINITY), None);
        assert_eq!(validate_pct(-1.0), None);
        assert_eq!(validate_pct(1.0e9), None);
    }

    #[test]
    fn frequency_validation_rejects_zero_nan_negative_and_huge() {
        assert_eq!(validate_mhz(2_011.0, Some(3_301.0)), Some(2_011.0));
        assert_eq!(validate_mhz(4_200.0, Some(3_301.0)), Some(4_200.0));
        assert_eq!(validate_mhz(0.0, None), None);
        assert_eq!(validate_mhz(-12.0, None), None);
        assert_eq!(validate_mhz(f32::NAN, None), None);
        assert_eq!(validate_mhz(f32::INFINITY, None), None);
        assert_eq!(validate_mhz(25_000.0, None), None);
        // Three times the nominal clock is the plausibility ceiling.
        assert_eq!(validate_mhz(9_903.0, Some(3_301.0)), Some(9_903.0));
        assert_eq!(validate_mhz(9_904.0, Some(3_301.0)), None);
    }

    #[test]
    fn nominal_max_frequency_must_be_plausible() {
        assert_eq!(plausible_nominal_mhz(3_301), Some(3_301));
        assert_eq!(plausible_nominal_mhz(0), None);
        assert_eq!(plausible_nominal_mhz(1), None);
        assert_eq!(plausible_nominal_mhz(25_000), None);
    }

    #[test]
    fn frequency_guard_skips_one_discontinuous_sample() {
        let mut guard = FrequencyGuard::default();
        assert_eq!(guard.accept(Some(2_000.0), Some(3_301.0)), Some(2_000.0));
        assert_eq!(guard.accept(Some(2_100.0), Some(3_301.0)), Some(2_100.0));

        // Resume/VM glitch above the plausible boost ceiling: hidden for one
        // tick, then accepted as the baseline.
        assert_eq!(guard.accept(Some(8_000.0), Some(3_301.0)), None);
        assert_eq!(guard.accept(Some(8_000.0), Some(3_301.0)), Some(8_000.0));

        // Missing data resets the baseline, so a legitimate level change after
        // a provider gap is not hidden.
        assert_eq!(guard.accept(None, Some(3_301.0)), None);
        assert_eq!(guard.accept(Some(400.0), Some(3_301.0)), Some(400.0));
    }

    #[test]
    fn frequency_guard_allows_normal_idle_to_turbo_transitions() {
        let mut guard = FrequencyGuard::default();
        assert_eq!(guard.accept(Some(400.0), Some(3_301.0)), Some(400.0));
        // 4.2 GHz is above nominal but inside the real boost range.
        assert_eq!(guard.accept(Some(4_200.0), Some(3_301.0)), Some(4_200.0));
        assert_eq!(guard.accept(Some(3_900.0), Some(3_301.0)), Some(3_900.0));
        assert_eq!(guard.accept(Some(800.0), Some(3_301.0)), Some(800.0));
    }

    #[test]
    fn performance_estimate_is_validated() {
        let mhz = performance_to_mhz(58.0, 3_301.0).expect("frequency");
        assert!((mhz - 1_914.58).abs() < 0.1);
        assert_eq!(performance_to_mhz(0.0, 3_301.0), None);
        assert_eq!(performance_to_mhz(-10.0, 3_301.0), None);
        assert_eq!(performance_to_mhz(1_000.0, 3_301.0), None);
        assert_eq!(performance_to_mhz(f32::NAN, 3_301.0), None);
    }

    #[test]
    fn system_times_first_sample_and_reset_are_zero() {
        // No previous sample: the first poll must report 0.
        assert_eq!(busy_pct(None, (100, 200)), None);
        // No elapsed time.
        assert_eq!(busy_pct(Some((100, 200)), (100, 200)), None);
        // Counters moved backwards (resume): restart the baseline.
        assert_eq!(busy_pct(Some((100, 200)), (90, 190)), None);

        let pct = busy_pct(Some((100, 1_000)), (200, 1_200)).expect("percentage");
        assert!((pct - 50.0).abs() < 0.01);

        // A corrupt idle delta larger than the total is clamped, not negative.
        assert_eq!(busy_pct(Some((0, 100)), (200, 150)), Some(0.0));
    }

    #[test]
    fn filetime_combines_words_in_order() {
        let value = FILETIME {
            dwLowDateTime: 0x1234_5678,
            dwHighDateTime: 0x9ABC_DEF0,
        };
        assert_eq!(filetime_to_u64(value), 0x9ABC_DEF0_1234_5678);
    }
}
