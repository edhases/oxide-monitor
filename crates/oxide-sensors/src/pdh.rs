//! Minimal safe wrapper around the Windows PDH performance-counter API.
//!
//! All counters are added with [`PdhAddEnglishCounterW`] so the English counter
//! paths work on localized Windows installations, and every value is formatted
//! as [`PDH_FMT_DOUBLE`]. Query handles are closed on drop.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Performance::{
    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE,
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhGetFormattedCounterValue, PdhOpenQueryW,
};
use windows::core::PCWSTR;

/// Upper bound for a wildcard counter array. `GPU Engine` is the largest set in
/// practice (~65 KB for 400+ instances); 16 MiB leaves ample headroom while
/// preventing a corrupted counter provider from forcing a huge allocation.
const MAX_ARRAY_BYTES: u32 = 16 * 1024 * 1024;

/// Encode a counter path as a NUL-terminated UTF-16 buffer.
pub(crate) fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Owns a PDH query handle. Counter handles are children of the query and are
/// closed together with it.
pub(crate) struct PdhQuery {
    handle: PDH_HQUERY,
}

impl PdhQuery {
    pub(crate) fn open() -> Option<Self> {
        let mut handle = PDH_HQUERY(std::ptr::null_mut());
        let status = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut handle) };
        if status == ERROR_SUCCESS.0 && !handle.is_invalid() {
            Some(Self { handle })
        } else {
            None
        }
    }

    pub(crate) fn add_counter(&self, path: &str) -> Option<PdhCounter> {
        let path = wide_null(path);
        let mut handle = PDH_HCOUNTER(std::ptr::null_mut());
        let status = unsafe {
            PdhAddEnglishCounterW(self.handle, PCWSTR::from_raw(path.as_ptr()), 0, &mut handle)
        };
        if status == ERROR_SUCCESS.0 && !handle.is_invalid() {
            Some(PdhCounter { handle })
        } else {
            None
        }
    }

    /// Collect one sample for every counter in the query. The first call only
    /// primes delta counters and does not produce a usable rate.
    pub(crate) fn collect(&self) -> bool {
        unsafe { PdhCollectQueryData(self.handle) == ERROR_SUCCESS.0 }
    }
}

impl Drop for PdhQuery {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            unsafe {
                let _ = PdhCloseQuery(self.handle);
            }
        }
    }
}

/// A counter belonging to a [`PdhQuery`]. It is only valid while the query is
/// alive; the borrow relationship is upheld by the samplers that own both.
#[derive(Clone, Copy)]
pub(crate) struct PdhCounter {
    handle: PDH_HCOUNTER,
}

/// One named instance from a wildcard counter, e.g. a thermal zone or adapter.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CounterValue {
    pub name: String,
    pub value: f64,
}

impl PdhCounter {
    /// Read a single formatted value. Returns `None` for invalid/stale data.
    pub(crate) fn value(&self) -> Option<f64> {
        let mut value = PDH_FMT_COUNTERVALUE::default();
        let status =
            unsafe { PdhGetFormattedCounterValue(self.handle, PDH_FMT_DOUBLE, None, &mut value) };
        if status != ERROR_SUCCESS.0 || !status_is_valid(value.CStatus) {
            return None;
        }
        // `PDH_FMT_COUNTERVALUE` is repr(C): CStatus (u32) is padded to 8 bytes
        // before the anonymous union, so reading `doubleValue` is correct here.
        let value = unsafe { value.Anonymous.doubleValue };
        value.is_finite().then_some(value)
    }

    /// Read all instances of a wildcard counter using the documented two-pass
    /// `PdhGetFormattedCounterArrayW` protocol.
    pub(crate) fn array(&self) -> Option<Vec<CounterValue>> {
        let mut size = 0u32;
        let mut count = 0u32;
        let status = unsafe {
            PdhGetFormattedCounterArrayW(self.handle, PDH_FMT_DOUBLE, &mut size, &mut count, None)
        };

        // No instances: the counter set exists but is empty.
        if status == ERROR_SUCCESS.0 {
            return Some(Vec::new());
        }
        if status != PDH_MORE_DATA || size == 0 || count == 0 || size > MAX_ARRAY_BYTES {
            return None;
        }

        // Over-allocate by one word and use a `u64` buffer so the returned
        // `PDH_FMT_COUNTERVALUE_ITEM_W` array is suitably aligned.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8) + 1];
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                self.handle,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                Some(buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>()),
            )
        };
        if status != ERROR_SUCCESS.0 || count == 0 {
            return None;
        }

        // Defensive bound: never trust an item count that cannot fit the buffer
        // PDH reported in the first pass.
        let item_size = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let allocated = buffer.len() * std::mem::size_of::<u64>();
        if (count as usize).saturating_mul(item_size) > allocated {
            return None;
        }

        // SAFETY: PDH wrote `count` items into the buffer we sized above.
        let items = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                count as usize,
            )
        };
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            if !status_is_valid(item.FmtValue.CStatus) {
                continue;
            }
            let value = unsafe { item.FmtValue.Anonymous.doubleValue };
            if !value.is_finite() {
                continue;
            }
            // The name points into the same PDH-owned buffer.
            let name = unsafe { item.szName.to_string() }.unwrap_or_default();
            values.push(CounterValue { name, value });
        }
        Some(values)
    }
}

fn status_is_valid(status: u32) -> bool {
    status == PDH_CSTATUS_VALID_DATA || status == PDH_CSTATUS_NEW_DATA
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_string_is_nul_terminated() {
        let path = r"\Processor Information(_Total)\% Processor Utility";
        let wide = wide_null(path);
        assert_eq!(wide.last().copied(), Some(0));
        assert_eq!(wide.len(), path.encode_utf16().count() + 1);
    }

    #[test]
    fn pdh_abi_layout_matches_documentation() {
        assert_eq!(PDH_FMT_DOUBLE.0, 0x0000_0200);
        assert_eq!(PDH_MORE_DATA, 0x8000_07D2);
        assert_eq!(std::mem::offset_of!(PDH_FMT_COUNTERVALUE, CStatus), 0);
        assert_eq!(std::mem::offset_of!(PDH_FMT_COUNTERVALUE, Anonymous), 8);
        assert_eq!(std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>(), 24);
    }
}
