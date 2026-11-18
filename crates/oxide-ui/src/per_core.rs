//! Lightweight on-demand per-logical-core CPU utilization sampler.
//!
//! Polled ONLY while the Flyout card is open and cleared immediately on hide,
//! keeping background CPU and memory overhead at zero when the Flyout is closed.

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{s, w};

const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;
const MAX_CORES: usize = 256;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProcessorPerformanceInfo {
    idle_time: i64,
    kernel_time: i64,
    user_time: i64,
    dpc_time: i64,
    interrupt_time: i64,
    interrupt_count: u32,
    _pad: u32,
}

type NtQuerySystemInformationFn = unsafe extern "system" fn(
    system_information_class: u32,
    system_information: *mut c_void,
    system_information_length: u32,
    return_length: *mut u32,
) -> i32;

pub struct PerCoreSampler {
    query_fn: Option<NtQuerySystemInformationFn>,
    prev: Vec<(i64, i64)>,
    last_pct: Vec<f32>,
}

impl Default for PerCoreSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl PerCoreSampler {
    pub fn new() -> Self {
        let query_fn = resolve_nt_query();
        Self {
            query_fn,
            prev: Vec::new(),
            last_pct: Vec::new(),
        }
    }

    /// Release all buffers when the Flyout closes.
    pub fn clear(&mut self) {
        self.prev.clear();
        self.prev.shrink_to_fit();
        self.last_pct.clear();
        self.last_pct.shrink_to_fit();
    }

    #[cfg(test)]
    pub fn last_values(&self) -> &[f32] {
        &self.last_pct
    }

    /// Take one delta sample across all logical cores. Returns `&[f32]` in `0.0..=100.0`.
    pub fn poll(&mut self) -> &[f32] {
        let Some(query) = self.query_fn else {
            return &[];
        };
        let mut buf = [ProcessorPerformanceInfo::default(); MAX_CORES];
        let mut returned = 0u32;
        let status = unsafe {
            query(
                SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
                buf.as_mut_ptr().cast::<c_void>(),
                (buf.len() * size_of::<ProcessorPerformanceInfo>()) as u32,
                &mut returned,
            )
        };
        if status < 0 || returned == 0 {
            return &self.last_pct;
        }
        let count = (returned as usize / size_of::<ProcessorPerformanceInfo>()).min(MAX_CORES);
        if count == 0 {
            return &self.last_pct;
        }

        let current: Vec<(i64, i64)> = buf[..count]
            .iter()
            .map(|info| {
                // In Windows kernel accounting, `kernel_time` includes `idle_time`.
                let total = info.kernel_time.saturating_add(info.user_time);
                (info.idle_time, total)
            })
            .collect();

        if self.prev.len() == count {
            self.last_pct.clear();
            self.last_pct.reserve(count);
            for (&(p_idle, p_total), &(c_idle, c_total)) in self.prev.iter().zip(current.iter()) {
                let d_total = c_total.saturating_sub(p_total);
                let d_idle = c_idle.saturating_sub(p_idle);
                let pct = if d_total <= 0 {
                    0.0
                } else {
                    let busy = d_total.saturating_sub(d_idle).max(0);
                    ((busy as f64 / d_total as f64) * 100.0).clamp(0.0, 100.0) as f32
                };
                self.last_pct.push(pct);
            }
        } else {
            self.last_pct.resize(count, 0.0);
        }

        self.prev = current;
        &self.last_pct
    }
}

fn resolve_nt_query() -> Option<NtQuerySystemInformationFn> {
    let module = unsafe { GetModuleHandleW(w!("ntdll.dll")) }.ok()?;
    let proc = unsafe { GetProcAddress(module, s!("NtQuerySystemInformation")) }?;
    // SAFETY: `NtQuerySystemInformation` has the exact `system` calling convention defined above.
    Some(unsafe { std::mem::transmute::<_, NtQuerySystemInformationFn>(proc) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_core_sampler_polls_and_clears_cleanly() {
        let mut sampler = PerCoreSampler::new();
        let first = sampler.poll().to_vec();
        assert!(!first.is_empty(), "expected at least 1 logical core");
        let second = sampler.poll();
        assert_eq!(second.len(), first.len());
        assert!(second.iter().all(|v| (0.0..=100.0).contains(v)));
        sampler.clear();
        assert!(sampler.last_values().is_empty());
    }
}
