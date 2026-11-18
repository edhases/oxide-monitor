//! Vendor-neutral GPU utilization via PDH `\GPU Engine(*)\Utilization Percentage`.
//!
//! Every WDDM adapter exposes one instance per engine per process, e.g.
//! `pid_4840_luid_0x00000000_0x000104fb_phys_0_eng_0_engtype_3d`. Instances are
//! grouped by adapter LUID and engine type, percentages are summed inside an
//! engine type and the busiest engine type is reported for that LUID. That is
//! closer to Task Manager than summing every instance, which can exceed 100 %
//! when several engines work in parallel.
//!
//! The parser and the aggregation are pure functions so multi-GPU and malformed
//! instance names can be tested without a GPU or PDH.

use std::collections::HashMap;
use std::mem::size_of;

use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE,
    PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW, PdhCloseQuery,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::core::PCWSTR;

/// English counter path; `PdhAddEnglishCounterW` resolves it on localized Windows.
const COUNTER_PATH: &str = r"\GPU Engine(*)\Utilization Percentage";

/// `ERROR_SUCCESS` as returned by the PDH functions (they use plain `u32` codes).
const PDH_SUCCESS: u32 = 0;

/// Defensive cap for the instance array; the real buffer is ~65 KB on a hybrid laptop.
const MAX_BUFFER_BYTES: usize = 16 * 1024 * 1024;

/// One parsed `GPU Engine` instance name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EngineInstance<'a> {
    pub luid: u64,
    pub engine: &'a str,
}

/// Parse `pid_<pid>_luid_0x<high>_0x<low>_phys_<n>_eng_<n>_engtype_<type>`.
///
/// Returns `None` for names that do not carry both a LUID and an engine type.
/// Parsing is token-based and accepts variable-width hexadecimal LUID parts (the
/// PDH provider does not zero-pad them on every Windows build).
pub(crate) fn parse_instance(instance: &str) -> Option<EngineInstance<'_>> {
    let mut parts = instance.split('_');
    let mut luid_high: Option<u32> = None;
    let mut luid_low: Option<u32> = None;
    let mut engine: Option<&str> = None;

    while let Some(token) = parts.next() {
        if token.eq_ignore_ascii_case("luid") && luid_high.is_none() {
            let high = parts.next().and_then(parse_hex_u32);
            let low = parts.next().and_then(parse_hex_u32);
            if let (Some(high), Some(low)) = (high, low) {
                luid_high = Some(high);
                luid_low = Some(low);
            }
        } else if token.eq_ignore_ascii_case("engtype") && engine.is_none() {
            engine = parts.next().filter(|kind| !kind.is_empty());
        }
    }

    let (Some(high), Some(low), Some(engine)) = (luid_high, luid_low, engine) else {
        return None;
    };

    Some(EngineInstance {
        luid: ((high as u64) << 32) | low as u64,
        engine,
    })
}

/// Parse a hexadecimal LUID component, with or without the `0x` prefix.
fn parse_hex_u32(value: &str) -> Option<u32> {
    let digits = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .unwrap_or(value);
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// Aggregate `(instance_name, percent)` pairs into the busiest engine type per LUID.
///
/// Malformed names, non-finite values and empty values are ignored. An empty map
/// means PDH returned no usable GPU instance at all.
pub(crate) fn aggregate_usage<'a, I>(items: I) -> HashMap<u64, f32>
where
    I: IntoIterator<Item = (&'a str, f64)>,
{
    struct PerLuid<'a> {
        luid: u64,
        engines: Vec<(&'a str, f64)>,
    }

    let mut by_luid: Vec<PerLuid<'_>> = Vec::new();

    for (instance, value) in items {
        if !value.is_finite() {
            continue;
        }
        let Some(parsed) = parse_instance(instance) else {
            continue;
        };

        let entry = match by_luid.iter_mut().find(|entry| entry.luid == parsed.luid) {
            Some(entry) => entry,
            None => {
                by_luid.push(PerLuid {
                    luid: parsed.luid,
                    engines: Vec::new(),
                });
                by_luid.last_mut().expect("just pushed")
            }
        };

        match entry
            .engines
            .iter_mut()
            .find(|(kind, _)| kind.eq_ignore_ascii_case(parsed.engine))
        {
            Some((_, sum)) => *sum += value,
            None => entry.engines.push((parsed.engine, value)),
        }
    }

    let mut usage = HashMap::with_capacity(by_luid.len());
    for entry in by_luid {
        let busiest = entry
            .engines
            .iter()
            .map(|(_, value)| *value)
            .fold(f64::NEG_INFINITY, f64::max);
        if busiest.is_finite() {
            usage.insert(entry.luid, busiest.clamp(0.0, 100.0) as f32);
        }
    }
    usage
}

/// Query over all GPU engine instances.
pub(crate) struct GpuEngineQuery {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    /// Reused formatted-counter buffer; avoids a ~65 KB allocation per poll.
    items: Vec<PDH_FMT_COUNTERVALUE_ITEM_W>,
    collect_error_logged: bool,
    read_error_logged: bool,
}

// SAFETY: `PDH_HQUERY`/`PDH_HCOUNTER` are opaque process-wide handles, and this type
// never shares them: `Send` only permits moving ownership between threads, which is
// exactly what the sampler thread does. `Sync` is deliberately not implemented.
unsafe impl Send for GpuEngineQuery {}

impl GpuEngineQuery {
    pub(crate) fn new() -> Self {
        let mut query = PDH_HQUERY::default();
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) };
        if status != PDH_SUCCESS {
            tracing::warn!(
                "PDH: cannot open GPU Engine query (status 0x{status:08X}); GPU load unavailable"
            );
            return Self::disabled();
        }

        let path: Vec<u16> = COUNTER_PATH
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut counter = PDH_HCOUNTER::default();
        let status = unsafe {
            PdhAddEnglishCounterW(query, PCWSTR::from_raw(path.as_ptr()), 0, &mut counter)
        };
        if status != PDH_SUCCESS {
            tracing::warn!(
                "PDH: cannot add GPU Engine counter (status 0x{status:08X}); GPU load unavailable"
            );
            unsafe { PdhCloseQuery(query) };
            return Self::disabled();
        }

        // Prime the rate counter so the first poll has a baseline sample.
        unsafe { PdhCollectQueryData(query) };

        Self {
            query,
            counter,
            items: Vec::new(),
            collect_error_logged: false,
            read_error_logged: false,
        }
    }

    fn disabled() -> Self {
        Self {
            query: PDH_HQUERY::default(),
            counter: PDH_HCOUNTER::default(),
            items: Vec::new(),
            collect_error_logged: false,
            read_error_logged: false,
        }
    }

    /// Collect a new sample and return usage keyed by adapter LUID.
    pub(crate) fn poll(&mut self) -> HashMap<u64, f32> {
        if self.query.is_invalid() || self.counter.is_invalid() {
            return HashMap::new();
        }

        let status = unsafe { PdhCollectQueryData(self.query) };
        if status != PDH_SUCCESS {
            if !self.collect_error_logged {
                self.collect_error_logged = true;
                tracing::debug!("PDH GPU Engine collect failed (status 0x{status:08X})");
            }
            return HashMap::new();
        }
        self.collect_error_logged = false;

        if !self.refresh() {
            return HashMap::new();
        }

        let mut values: Vec<(String, f64)> = Vec::with_capacity(self.items.len());
        for item in &self.items {
            if item.FmtValue.CStatus != PDH_CSTATUS_VALID_DATA
                && item.FmtValue.CStatus != PDH_CSTATUS_NEW_DATA
            {
                continue;
            }

            // SAFETY: the array was requested with PDH_FMT_DOUBLE, so the union holds f64.
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            if !value.is_finite() {
                continue;
            }

            // SAFETY: PDH owns NUL-terminated wide strings for the lifetime of this buffer.
            let Ok(name) = (unsafe { item.szName.to_string() }) else {
                continue;
            };
            if name.is_empty() {
                continue;
            }

            values.push((name, value));
        }

        aggregate_usage(values.iter().map(|(name, value)| (name.as_str(), *value)))
    }

    /// Two-pass `PdhGetFormattedCounterArrayW` call; retries once if instances
    /// appeared between the size query and the data query.
    fn refresh(&mut self) -> bool {
        for _attempt in 0..2 {
            let mut size = 0u32;
            let mut count = 0u32;
            let status = unsafe {
                PdhGetFormattedCounterArrayW(
                    self.counter,
                    PDH_FMT_DOUBLE,
                    &mut size,
                    &mut count,
                    None,
                )
            };

            if status == PDH_SUCCESS {
                // A NULL buffer never receives items; treat this as "no instances".
                self.items.clear();
                return false;
            }
            if status != PDH_MORE_DATA {
                self.log_read_error(status);
                return false;
            }
            if size == 0 || size as usize > MAX_BUFFER_BYTES {
                self.log_read_error(status);
                return false;
            }

            let item_size = size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            let needed_items = size as usize / item_size + 1;
            self.items.clear();
            self.items.reserve(needed_items);

            let mut buffer_bytes = (self.items.capacity() * item_size) as u32;
            let mut count = 0u32;
            let status = unsafe {
                PdhGetFormattedCounterArrayW(
                    self.counter,
                    PDH_FMT_DOUBLE,
                    &mut buffer_bytes,
                    &mut count,
                    Some(self.items.as_mut_ptr()),
                )
            };

            if status == PDH_SUCCESS {
                if count as usize > self.items.capacity() {
                    return false;
                }
                // SAFETY: PDH initialized exactly `count` items in the buffer and
                // `count` was checked against the allocation.
                unsafe { self.items.set_len(count as usize) };
                return true;
            }
            if status != PDH_MORE_DATA {
                self.log_read_error(status);
                return false;
            }
            // The instance set grew between the two calls; retry with a larger buffer.
        }

        false
    }

    fn log_read_error(&mut self, status: u32) {
        if self.read_error_logged {
            return;
        }
        self.read_error_logged = true;
        tracing::debug!("PDH GPU Engine read failed (status 0x{status:08X})");
    }
}

impl Drop for GpuEngineQuery {
    fn drop(&mut self) {
        if !self.query.is_invalid() {
            unsafe { PdhCloseQuery(self.query) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{aggregate_usage, parse_hex_u32, parse_instance};

    const LUID_A: &str = "pid_4840_luid_0x00000000_0x000104FB_phys_0_eng_0_engtype_3D";
    const LUID_B: &str = "pid_12_luid_0x00000000_0x0011B4C_phys_0_eng_1_engtype_Copy";

    #[test]
    fn parses_luid_and_engine_type() {
        let parsed = parse_instance(LUID_A).expect("valid instance");
        assert_eq!(parsed.luid, 0x0001_04FB);
        assert_eq!(parsed.engine, "3D");

        let parsed =
            parse_instance("pid_1_luid_0x00000001_0x00000002_phys_0_eng_7_engtype_VideoDecode")
                .expect("valid instance");
        assert_eq!(parsed.luid, 0x0000_0001_0000_0002);
        assert_eq!(parsed.engine, "VideoDecode");
    }

    #[test]
    fn parses_variable_width_and_uppercase_hex() {
        let parsed = parse_instance("pid_7_luid_0x1_0xAB_phys_0_eng_0_engtype_3d")
            .expect("variable width LUID");
        assert_eq!(parsed.luid, 0x0000_0001_0000_00AB);
        assert_eq!(parsed.engine, "3d");

        let parsed =
            parse_instance("pid_7_LUID_0X00000000_0X0000000A_phys_0_eng_0_engtype_Compute")
                .expect("uppercase tokens");
        assert_eq!(parsed.luid, 10);
        assert_eq!(parsed.engine, "Compute");
    }

    #[test]
    fn rejects_malformed_instance_names() {
        assert_eq!(parse_instance(""), None);
        assert_eq!(parse_instance("luid_0x00000000_0x000104FB_phys_0"), None);
        assert_eq!(
            parse_instance("pid_1_luid_0xZZ_0x1_phys_0_eng_0_engtype_3D"),
            None
        );
        assert_eq!(
            parse_instance("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_"),
            None
        );
        assert_eq!(parse_instance("pid_1_luid_0x0_0x1_phys_0"), None);
        assert_eq!(parse_instance("not-an-engine-instance"), None);
        assert_eq!(parse_instance("pid_1_engtype_3D"), None);
    }

    #[test]
    fn parses_hex_components_with_bounds() {
        assert_eq!(parse_hex_u32("0x000104FB"), Some(0x0001_04FB));
        assert_eq!(parse_hex_u32("11B4C"), Some(0x0001_1B4C));
        assert_eq!(parse_hex_u32("0x"), None);
        assert_eq!(parse_hex_u32("0x100000000"), None);
        assert_eq!(parse_hex_u32("nope"), None);
    }

    #[test]
    fn keeps_multiple_luids_independent() {
        let usage = aggregate_usage([
            (LUID_A, 10.0),
            (LUID_A, 20.0),
            (
                "pid_1_luid_0x00000000_0x000104FB_phys_0_eng_1_engtype_Copy",
                35.0,
            ),
            (LUID_B, 5.0),
            (
                "pid_13_luid_0x00000000_0x0011B4C_phys_0_eng_0_engtype_3D",
                60.0,
            ),
        ]);
        assert_eq!(usage.len(), 2);
        assert_eq!(usage.get(&0x0001_04FB).copied(), Some(35.0));
        assert_eq!(usage.get(&0x0001_1B4C).copied(), Some(60.0));
    }

    #[test]
    fn sums_per_engine_type_and_takes_the_maximum_per_luid() {
        let usage = aggregate_usage([
            (LUID_A, 10.0),
            (
                "pid_2_luid_0x00000000_0x000104FB_phys_0_eng_0_engtype_3d",
                20.0,
            ),
            (
                "pid_1_luid_0x00000000_0x000104FB_phys_0_eng_1_engtype_Copy",
                35.0,
            ),
            (
                "pid_1_luid_0x00000000_0x000104FB_phys_0_eng_2_engtype_VideoDecode",
                5.0,
            ),
        ]);
        assert_eq!(usage.get(&0x0001_04FB).copied(), Some(35.0));
    }

    #[test]
    fn groups_engine_types_case_insensitively() {
        let usage = aggregate_usage([
            (LUID_A, 50.0),
            (
                "pid_2_luid_0x00000000_0x000104FB_phys_0_eng_0_engtype_3d",
                10.0,
            ),
            (
                "pid_3_luid_0x00000000_0x000104FB_phys_0_eng_1_engtype_copy",
                30.0,
            ),
        ]);
        assert_eq!(usage.get(&0x0001_04FB).copied(), Some(60.0));
    }

    #[test]
    fn clamps_the_result_to_a_percentage() {
        let usage = aggregate_usage([(LUID_A, 80.0), (LUID_B, 120.0)]);
        assert_eq!(usage.get(&0x0001_04FB).copied(), Some(80.0));
        assert_eq!(usage.get(&0x0001_1B4C).copied(), Some(100.0));
    }

    #[test]
    fn ignores_malformed_and_non_finite_values() {
        assert!(aggregate_usage(std::iter::empty::<(&str, f64)>()).is_empty());
        assert!(
            aggregate_usage([
                ("not-an-engine-instance", 10.0),
                (LUID_A, f64::NAN),
                (LUID_A, f64::INFINITY),
                ("pid_1_luid_0x0_0x1_phys_0_eng_0_engtype_", 5.0),
            ])
            .is_empty()
        );
    }
}
