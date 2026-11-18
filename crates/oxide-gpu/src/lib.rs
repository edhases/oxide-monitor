//! Universal, multi-adapter GPU load and temperature sampling.
//!
//! The sampler is vendor-neutral by default and only uses vendor SDKs for data
//! that WDDM does not expose:
//!
//! | Data | Source |
//! |---|---|
//! | Adapter identity, vendor, LUID, VRAM | DXGI `EnumAdapters1` |
//! | Utilization (all vendors, including Microsoft Basic Render) | PDH `\GPU Engine(*)\Utilization Percentage`, max per engine type per LUID |
//! | NVIDIA name/temp/usage/VRAM | NVML from `System32\nvml.dll`, all devices |
//! | Intel temp/usage | Level Zero Sysman from `System32\ze_loader.dll` |
//! | AMD temp | probe-only ADLX (see [`adlx`]); AMD adapters still get DXGI + PDH |
//!
//! Every backend is optional and dynamically loaded. A missing DLL, an RDP
//! session, a VM, Microsoft Basic Render or a sleeping Optimus dGPU degrades to
//! `None` for the affected field; the sampler never panics and never produces a
//! duplicate adapter.

mod adlx;
mod dxgi;
mod dynlib;
mod intel;
mod merge;
mod nvml_gpu;
mod pdh_gpu;

use std::time::Duration;

use oxide_core::{GpuSample, GpuVendor, TempSource};

use crate::adlx::AdlxBackend;
use crate::intel::IntelBackend;
use crate::merge::VendorGpuReading;
use crate::nvml_gpu::{BUSY_USAGE_PCT, NvmlBackend};
use crate::pdh_gpu::GpuEngineQuery;

/// Default interval between idle NVML temperature probes.
const DEFAULT_GPU_POLL_INTERVAL_MS: u64 = 3_000;

/// Protect the driver from a pathological config value.
const MIN_POLL_INTERVAL_MS: u64 = 250;
const MAX_POLL_INTERVAL_MS: u64 = 60_000;

/// Samples every GPU adapter and merges vendor data into a per-adapter list.
pub struct GpuSampler {
    poll_interval: Duration,
    engines: GpuEngineQuery,
    nvidia: NvmlBackend,
    amd: AdlxBackend,
    intel: IntelBackend,
}

impl Default for GpuSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuSampler {
    pub fn new() -> Self {
        Self::new_with_interval(DEFAULT_GPU_POLL_INTERVAL_MS)
    }

    /// `poll_interval_ms` is the minimum time between NVML probes while a
    /// discrete GPU is idle. Values are clamped to 250..=60_000 ms.
    pub fn new_with_interval(poll_interval_ms: u64) -> Self {
        Self {
            poll_interval: Duration::from_millis(
                poll_interval_ms.clamp(MIN_POLL_INTERVAL_MS, MAX_POLL_INTERVAL_MS),
            ),
            engines: GpuEngineQuery::new(),
            nvidia: NvmlBackend::new(),
            amd: AdlxBackend::new(),
            intel: IntelBackend::new(),
        }
    }

    /// Collect one sample per discovered adapter, in DXGI order.
    ///
    /// Never fails: an empty list means neither DXGI nor NVML saw a GPU.
    pub fn poll_all(&mut self) -> Vec<GpuSample> {
        let adapters = dxgi::enumerate();
        let usage_by_luid = self.engines.poll();

        // Probe NVML immediately when PDH shows real work; otherwise only once
        // per configured interval so an idle D3cold dGPU is not kept awake.
        let nvidia_busy = adapters.iter().any(|adapter| {
            adapter.vendor == GpuVendor::Nvidia
                && usage_by_luid
                    .get(&adapter.luid)
                    .is_some_and(|usage| *usage > BUSY_USAGE_PCT)
        });
        if self.nvidia.probe_due(self.poll_interval, nvidia_busy) {
            self.nvidia.probe();
        }

        let nvidia: Vec<VendorGpuReading> = self
            .nvidia
            .readings()
            .into_iter()
            .map(|reading| VendorGpuReading {
                name: reading.name,
                usage_pct: reading.usage_pct,
                temp_c: reading.temp_c,
                temp_source: if reading.temp_c.is_some() {
                    TempSource::Nvml
                } else {
                    TempSource::None
                },
                dedicated_vram_bytes: reading.dedicated_vram_bytes,
            })
            .collect();

        // Only touch vendor runtimes when DXGI actually shows their hardware.
        let amd_present = adapters
            .iter()
            .any(|adapter| adapter.vendor == GpuVendor::Amd);
        if amd_present {
            self.amd.probe();
        }
        let amd = if amd_present {
            self.amd.readings()
        } else {
            Vec::new()
        };

        let intel_present = adapters
            .iter()
            .any(|adapter| adapter.vendor == GpuVendor::Intel);
        let intel: Vec<VendorGpuReading> = if intel_present {
            self.intel
                .poll()
                .into_iter()
                .map(|reading| VendorGpuReading {
                    name: String::new(),
                    usage_pct: reading.usage_pct,
                    temp_c: reading.temp_c,
                    temp_source: if reading.temp_c.is_some() {
                        TempSource::Intel
                    } else {
                        TempSource::None
                    },
                    dedicated_vram_bytes: None,
                })
                .collect()
        } else {
            Vec::new()
        };

        merge::build_samples(&adapters, &usage_by_luid, &nvidia, &amd, &intel)
    }

    /// Primary GPU for the single-GPU widget.
    ///
    /// Prefers a discrete NVIDIA/AMD adapter with data (largest dedicated VRAM),
    /// then the first non-Microsoft adapter, then the first adapter, then an
    /// empty default.
    pub fn poll(&mut self) -> GpuSample {
        let samples = self.poll_all();
        match merge::select_primary_index(&samples) {
            Some(index) => samples[index].clone(),
            None => GpuSample::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::GpuSampler;
    use oxide_core::GpuVendor;

    #[test]
    fn sampler_can_move_to_the_sampler_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<GpuSampler>();
    }

    #[test]
    fn sampler_constructs_and_polls_without_panicking() {
        let mut sampler = GpuSampler::new_with_interval(0);
        let all = sampler.poll_all();
        let primary = sampler.poll();

        for sample in &all {
            assert!(!sample.name.is_empty(), "adapter must have a display name");
            if let Some(usage) = sample.usage_pct {
                assert!(
                    (0.0..=100.0).contains(&usage),
                    "usage out of range: {usage}"
                );
            }
            if let Some(temp) = sample.temp_c {
                assert!(
                    (-40.0..=150.0).contains(&temp),
                    "temperature out of range: {temp}"
                );
            }
            assert!(sample.vendor != GpuVendor::Unknown || !sample.name.is_empty());
        }

        if let Some(usage) = primary.usage_pct {
            assert!((0.0..=100.0).contains(&usage));
        }
    }

    #[test]
    fn default_matches_new() {
        let mut sampler = GpuSampler::default();
        let _ = sampler.poll_all();
    }
}
