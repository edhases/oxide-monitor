//! Physical memory usage via `GlobalMemoryStatusEx`.
//!
//! The API is cheap and non-admin. Every field is validated before use: a
//! successful call with a mismatched `dwLength`, a zero physical total or a
//! bogus load percentage degrades to a default sample instead of panicking or
//! showing a nonsense value.

use oxide_core::MemorySample;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

pub(crate) struct MemorySampler;

impl MemorySampler {
    pub(crate) fn new() -> Self {
        Self
    }

    /// `used = total - available`, while the percentage comes from
    /// `dwMemoryLoad` so it matches Task Manager (available includes standby).
    pub(crate) fn poll(&mut self) -> MemorySample {
        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };

        if unsafe { GlobalMemoryStatusEx(&mut status) }.is_err() {
            return MemorySample::default();
        }

        sample_from_status(&status)
    }
}

/// Validate and convert a filled `MEMORYSTATUSEX`.
fn sample_from_status(status: &MEMORYSTATUSEX) -> MemorySample {
    if status.dwLength != std::mem::size_of::<MEMORYSTATUSEX>() as u32 {
        return MemorySample::default();
    }

    let total_bytes = status.ullTotalPhys;
    if total_bytes == 0 {
        return MemorySample::default();
    }

    // Available can only be a subset of physical; clamp a corrupt value rather
    // than underflowing.
    let available_bytes = status.ullAvailPhys.min(total_bytes);
    let used_bytes = total_bytes - available_bytes;

    let computed = used_bytes as f64 / total_bytes as f64 * 100.0;
    let usage_pct = if status.dwMemoryLoad <= 100 {
        status.dwMemoryLoad as f32
    } else {
        computed as f32
    };

    MemorySample {
        used_bytes,
        total_bytes,
        usage_pct: if usage_pct.is_finite() {
            usage_pct.clamp(0.0, 100.0)
        } else {
            0.0
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(total: u64, available: u64, load: u32) -> MEMORYSTATUSEX {
        MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            dwMemoryLoad: load,
            ullTotalPhys: total,
            ullAvailPhys: available,
            ..Default::default()
        }
    }

    #[test]
    fn normal_status_is_converted() {
        let total = 32 * 1024 * 1024 * 1024;
        let available = 5 * 1024 * 1024 * 1024;
        let sample = sample_from_status(&status(total, available, 84));
        assert_eq!(sample.total_bytes, total);
        assert_eq!(sample.used_bytes, total - available);
        assert_eq!(sample.usage_pct, 84.0);
    }

    #[test]
    fn zero_total_is_rejected() {
        let sample = sample_from_status(&status(0, 0, 0));
        assert_eq!(sample.total_bytes, 0);
        assert_eq!(sample.used_bytes, 0);
        assert_eq!(sample.usage_pct, 0.0);
    }

    #[test]
    fn wrong_length_is_rejected() {
        let mut value = status(1024, 512, 50);
        value.dwLength = 0;
        assert_eq!(sample_from_status(&value).total_bytes, 0);
    }

    #[test]
    fn available_above_total_is_clamped() {
        let sample = sample_from_status(&status(1024, u64::MAX, 50));
        assert_eq!(sample.used_bytes, 0);
        assert_eq!(sample.usage_pct, 50.0);
    }

    #[test]
    fn invalid_load_falls_back_to_computed_and_clamps() {
        let sample = sample_from_status(&status(1_000, 250, 250));
        assert_eq!(sample.used_bytes, 750);
        assert!((sample.usage_pct - 75.0).abs() < 0.01);

        let sample = sample_from_status(&status(1_000, 0, 100));
        assert_eq!(sample.usage_pct, 100.0);
    }
}
