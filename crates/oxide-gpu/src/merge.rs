//! Merge DXGI adapters, PDH load and vendor readings into `GpuSample`s.
//!
//! This module is pure: it takes plain data and returns plain data. That keeps the
//! multi-adapter policy testable without a GPU, PDH, NVML, ADLX or Level Zero.
//!
//! Policy:
//! 1. DXGI adapters are the source of truth and keep their enumeration order.
//! 2. PDH usage is attached by LUID.
//! 3. Vendor readings are attached only to adapters of the same vendor, each
//!    reading at most once — no duplicate adapters.
//! 4. If DXGI exposes nothing but a vendor backend does, samples are synthesized
//!    from the vendor readings (headless/Server Core fallback).
//! 5. `poll()` prefers a discrete NVIDIA/AMD adapter with data, then the first
//!    non-Microsoft adapter, then the first adapter.

use std::collections::HashMap;

use oxide_core::{GpuSample, GpuVendor, TempSource};

use crate::dxgi::AdapterInfo;

/// A vendor backend reading, already normalized for merging.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VendorGpuReading {
    pub name: String,
    pub usage_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub temp_source: TempSource,
    pub dedicated_vram_bytes: Option<u64>,
}

/// Normalize a GPU name for DXGI ↔ vendor matching.
///
/// Removes vendor marketing noise and punctuation so `NVIDIA GeForce GTX 1650`
/// matches the same NVML name, and `AMD Radeon(TM) Graphics` matches
/// `AMD Radeon Graphics`. The result is only used for equality, never displayed.
pub(crate) fn normalize_gpu_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    for token in name.split(|character: char| !character.is_ascii_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        let lower = token.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "nvidia" | "geforce" | "amd" | "radeon" | "intel" | "tm" | "r" | "c"
        ) {
            continue;
        }
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(&lower);
    }
    normalized
}

/// Assign readings to adapters of one vendor by normalized name.
///
/// `DXGI_ADAPTER_DESC1` does not expose a PCI bus number, so the adapter
/// description is the only stable join key available without a SetupAPI/D3DKMT
/// dependency. Returns a `reading index` for every adapter slot. A reading is
/// used at most once. When there is exactly one adapter of the vendor and one
/// reading but the names differ (driver names are not standardized), the reading
/// is still attached; that is the common single-dGPU case.
fn assign_by_name(
    adapters: &[AdapterInfo],
    vendor: GpuVendor,
    readings: &[VendorGpuReading],
) -> Vec<Option<usize>> {
    let mut assignment = vec![None; adapters.len()];
    let mut used = vec![false; readings.len()];

    let vendor_adapters: Vec<usize> = adapters
        .iter()
        .enumerate()
        .filter(|(_, adapter)| adapter.vendor == vendor)
        .map(|(index, _)| index)
        .collect();

    for &adapter_index in &vendor_adapters {
        let adapter_name = normalize_gpu_name(&adapters[adapter_index].name);
        let matched = readings
            .iter()
            .enumerate()
            .find_map(|(reading_index, reading)| {
                if used[reading_index] {
                    return None;
                }
                let reading_name = normalize_gpu_name(&reading.name);
                (!reading_name.is_empty() && reading_name == adapter_name).then_some(reading_index)
            });

        if let Some(reading_index) = matched {
            assignment[adapter_index] = Some(reading_index);
            used[reading_index] = true;
        }
    }

    if vendor_adapters.len() == 1 && readings.len() == 1 {
        let adapter_index = vendor_adapters[0];
        if assignment[adapter_index].is_none() {
            assignment[adapter_index] = Some(0);
        }
    }

    assignment
}

fn apply_reading(sample: &mut GpuSample, reading: &VendorGpuReading) {
    if sample.usage_pct.is_none() {
        sample.usage_pct = reading.usage_pct.map(|usage| usage.clamp(0.0, 100.0));
    }

    if sample.temp_c.is_none()
        && let Some(temp) = reading.temp_c.filter(|temp| temp.is_finite())
    {
        sample.temp_c = Some(temp);
        sample.temp_source = reading.temp_source;
    }

    if sample.dedicated_vram_bytes.is_none() {
        sample.dedicated_vram_bytes = reading.dedicated_vram_bytes;
    }

    if (sample.name.is_empty() || sample.name == "GPU") && !reading.name.is_empty() {
        sample.name.clone_from(&reading.name);
    }
}

fn synthesized_sample(reading: &VendorGpuReading, vendor: GpuVendor, name: String) -> GpuSample {
    let temp_c = reading.temp_c.filter(|temp| temp.is_finite());
    GpuSample {
        name,
        vendor,
        luid: None,
        usage_pct: reading.usage_pct.map(|usage| usage.clamp(0.0, 100.0)),
        temp_c,
        temp_source: if temp_c.is_some() {
            reading.temp_source
        } else {
            TempSource::None
        },
        dedicated_vram_bytes: reading.dedicated_vram_bytes,
    }
}

/// Build one sample per adapter, with vendor readings merged in.
pub(crate) fn build_samples(
    adapters: &[AdapterInfo],
    usage_by_luid: &HashMap<u64, f32>,
    nvidia: &[VendorGpuReading],
    amd: &[VendorGpuReading],
    intel: &[VendorGpuReading],
) -> Vec<GpuSample> {
    let mut samples: Vec<GpuSample> = adapters
        .iter()
        .map(|adapter| GpuSample {
            name: adapter.name.clone(),
            vendor: adapter.vendor,
            luid: Some(adapter.luid),
            usage_pct: usage_by_luid
                .get(&adapter.luid)
                .copied()
                .map(|usage| usage.clamp(0.0, 100.0)),
            temp_c: None,
            temp_source: TempSource::None,
            dedicated_vram_bytes: adapter.dedicated_vram_bytes,
        })
        .collect();

    for (adapter_index, reading_index) in assign_by_name(adapters, GpuVendor::Nvidia, nvidia)
        .into_iter()
        .enumerate()
    {
        if let Some(reading_index) = reading_index {
            apply_reading(&mut samples[adapter_index], &nvidia[reading_index]);
        }
    }

    for (adapter_index, reading_index) in assign_by_name(adapters, GpuVendor::Amd, amd)
        .into_iter()
        .enumerate()
    {
        if let Some(reading_index) = reading_index {
            apply_reading(&mut samples[adapter_index], &amd[reading_index]);
        }
    }

    // Level Zero readings have no reliable public name without the versioned
    // `zes_device_properties_t`, so they are attached to Intel adapters in order.
    let mut intel_readings = intel.iter();
    for (adapter_index, sample) in samples.iter_mut().enumerate() {
        if adapters[adapter_index].vendor == GpuVendor::Intel
            && let Some(reading) = intel_readings.next()
        {
            apply_reading(sample, reading);
        }
    }

    if samples.is_empty() {
        // No DXGI at all (Server Core, broken display stack, RDP edge cases):
        // synthesize from whatever vendor backend still sees hardware.
        for reading in nvidia {
            samples.push(synthesized_sample(
                reading,
                GpuVendor::Nvidia,
                reading.name.clone(),
            ));
        }
        for (index, reading) in amd.iter().enumerate() {
            let name = if amd.len() == 1 {
                "AMD GPU".to_owned()
            } else {
                format!("AMD GPU {}", index + 1)
            };
            samples.push(synthesized_sample(reading, GpuVendor::Amd, name));
        }
        for (index, reading) in intel.iter().enumerate() {
            let name = if intel.len() == 1 {
                "Intel GPU".to_owned()
            } else {
                format!("Intel GPU {}", index + 1)
            };
            samples.push(synthesized_sample(reading, GpuVendor::Intel, name));
        }
    }

    samples
}

/// Choose the sample `poll()` should return.
///
/// Prefers a discrete NVIDIA/AMD adapter that has data (load or temperature) and
/// dedicated VRAM, picking the largest VRAM; then the first non-Microsoft
/// adapter; then the first adapter. Returns `None` for an empty list.
pub(crate) fn select_primary_index(samples: &[GpuSample]) -> Option<usize> {
    let mut best: Option<(usize, u64)> = None;

    for (index, sample) in samples.iter().enumerate() {
        if !matches!(sample.vendor, GpuVendor::Nvidia | GpuVendor::Amd) {
            continue;
        }
        let has_data = sample.usage_pct.is_some() || sample.temp_c.is_some();
        let vram = sample.dedicated_vram_bytes.unwrap_or(0);
        if !has_data || vram == 0 {
            continue;
        }
        if best.is_none_or(|(_, best_vram)| vram > best_vram) {
            best = Some((index, vram));
        }
    }

    if let Some((index, _)) = best {
        return Some(index);
    }

    samples
        .iter()
        .position(|sample| sample.vendor != GpuVendor::Microsoft)
        .or_else(|| (!samples.is_empty()).then_some(0))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use oxide_core::{GpuSample, GpuVendor, TempSource};

    use super::{VendorGpuReading, build_samples, normalize_gpu_name, select_primary_index};
    use crate::dxgi::AdapterInfo;

    fn adapter(name: &str, vendor: GpuVendor, luid: u64, vram: Option<u64>) -> AdapterInfo {
        AdapterInfo {
            name: name.to_owned(),
            vendor,
            luid,
            dedicated_vram_bytes: vram,
            software: vendor == GpuVendor::Microsoft,
        }
    }

    fn reading(
        name: &str,
        usage: Option<f32>,
        temp: Option<f32>,
        source: TempSource,
        vram: Option<u64>,
    ) -> VendorGpuReading {
        VendorGpuReading {
            name: name.to_owned(),
            usage_pct: usage,
            temp_c: temp,
            temp_source: source,
            dedicated_vram_bytes: vram,
        }
    }

    #[test]
    fn normalizes_vendor_marketing_names() {
        assert_eq!(
            normalize_gpu_name("NVIDIA GeForce GTX 1650"),
            normalize_gpu_name("NVIDIA GeForce GTX 1650")
        );
        assert_eq!(
            normalize_gpu_name("AMD Radeon(TM) Graphics"),
            normalize_gpu_name("AMD Radeon Graphics")
        );
        assert_eq!(
            normalize_gpu_name("Intel(R) UHD Graphics 630"),
            "uhd graphics 630"
        );
        assert_eq!(normalize_gpu_name(""), "");
    }

    #[test]
    fn merges_nvidia_dgpu_and_amd_igpu_without_duplicates() {
        let adapters = [
            adapter(
                "AMD Radeon(TM) Graphics",
                GpuVendor::Amd,
                0x0001_04FB,
                Some(512 * 1024 * 1024),
            ),
            adapter(
                "NVIDIA GeForce GTX 1650",
                GpuVendor::Nvidia,
                0x0001_1B4C,
                Some(4 * 1024 * 1024 * 1024),
            ),
        ];
        let usage = HashMap::from([(0x0001_04FB, 7.0), (0x0001_1B4C, 42.0)]);
        let nvidia = [reading(
            "NVIDIA GeForce GTX 1650",
            None,
            Some(55.0),
            TempSource::Nvml,
            Some(4 * 1024 * 1024 * 1024),
        )];
        let amd = [];
        let intel = [];

        let samples = build_samples(&adapters, &usage, &nvidia, &amd, &intel);
        assert_eq!(samples.len(), 2);

        assert_eq!(samples[0].vendor, GpuVendor::Amd);
        assert_eq!(samples[0].luid, Some(0x0001_04FB));
        assert_eq!(samples[0].usage_pct, Some(7.0));
        assert_eq!(samples[0].temp_c, None);
        assert_eq!(samples[0].temp_source, TempSource::None);

        assert_eq!(samples[1].vendor, GpuVendor::Nvidia);
        assert_eq!(samples[1].usage_pct, Some(42.0));
        assert_eq!(samples[1].temp_c, Some(55.0));
        assert_eq!(samples[1].temp_source, TempSource::Nvml);
        assert_eq!(
            samples[1].dedicated_vram_bytes,
            Some(4 * 1024 * 1024 * 1024)
        );
    }

    #[test]
    fn pdh_only_fallback_keeps_adapters_and_usage() {
        let adapters = [
            adapter("AMD Radeon(TM) Graphics", GpuVendor::Amd, 0xAA, None),
            adapter("NVIDIA GeForce GTX 1650", GpuVendor::Nvidia, 0xBB, None),
        ];
        let usage = HashMap::from([(0xAA, 12.0), (0xBB, 3.0)]);
        let samples = build_samples(&adapters, &usage, &[], &[], &[]);

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].usage_pct, Some(12.0));
        assert_eq!(samples[1].usage_pct, Some(3.0));
        assert!(samples.iter().all(|sample| sample.temp_c.is_none()));
        assert!(
            samples
                .iter()
                .all(|sample| sample.temp_source == TempSource::None)
        );
    }

    #[test]
    fn maps_multiple_nvidia_devices_by_name() {
        let adapters = [
            adapter(
                "NVIDIA GeForce RTX 3060",
                GpuVendor::Nvidia,
                1,
                Some(12 * 1024 * 1024 * 1024),
            ),
            adapter(
                "NVIDIA GeForce GTX 1650",
                GpuVendor::Nvidia,
                2,
                Some(4 * 1024 * 1024 * 1024),
            ),
        ];
        let nvidia = [
            reading(
                "NVIDIA GeForce GTX 1650",
                None,
                Some(44.0),
                TempSource::Nvml,
                None,
            ),
            reading(
                "NVIDIA GeForce RTX 3060",
                None,
                Some(61.0),
                TempSource::Nvml,
                None,
            ),
        ];
        let samples = build_samples(&adapters, &HashMap::new(), &nvidia, &[], &[]);

        assert_eq!(samples[0].temp_c, Some(61.0));
        assert_eq!(samples[1].temp_c, Some(44.0));
    }

    #[test]
    fn attaches_single_nvidia_reading_when_driver_names_differ() {
        let adapters = [adapter(
            "NVIDIA GeForce GTX 1650",
            GpuVendor::Nvidia,
            1,
            None,
        )];
        let nvidia = [reading(
            "NVIDIA GeForce GTX 1650 with Max-Q Design",
            None,
            Some(50.0),
            TempSource::Nvml,
            None,
        )];
        let samples = build_samples(&adapters, &HashMap::new(), &nvidia, &[], &[]);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].temp_c, Some(50.0));
    }

    #[test]
    fn does_not_duplicate_adapter_when_nvml_matches() {
        let adapters = [adapter(
            "NVIDIA GeForce RTX 3060",
            GpuVendor::Nvidia,
            7,
            Some(12 * 1024 * 1024 * 1024),
        )];
        let nvidia = [reading(
            "NVIDIA GeForce RTX 3060",
            Some(99.0),
            Some(70.0),
            TempSource::Nvml,
            Some(12 * 1024 * 1024 * 1024),
        )];
        let samples = build_samples(&adapters, &HashMap::new(), &nvidia, &[], &[]);
        assert_eq!(samples.len(), 1);
        // PDH is absent, so the NVML utilization is the fallback.
        assert_eq!(samples[0].usage_pct, Some(99.0));
    }

    #[test]
    fn synthesizes_nvml_samples_when_dxgi_is_empty() {
        let nvidia = [
            reading(
                "NVIDIA GeForce GTX 1650",
                Some(5.0),
                Some(42.0),
                TempSource::Nvml,
                Some(4 * 1024 * 1024 * 1024),
            ),
            reading(
                "NVIDIA GeForce RTX 3060",
                Some(0.0),
                Some(39.0),
                TempSource::Nvml,
                Some(12 * 1024 * 1024 * 1024),
            ),
        ];
        let samples = build_samples(&[], &HashMap::new(), &nvidia, &[], &[]);
        assert_eq!(samples.len(), 2);
        assert!(samples.iter().all(|sample| sample.luid.is_none()));
        assert_eq!(samples[0].vendor, GpuVendor::Nvidia);
        assert_eq!(samples[0].temp_source, TempSource::Nvml);
        assert_eq!(samples[1].name, "NVIDIA GeForce RTX 3060");
    }

    #[test]
    fn intel_temperature_keeps_the_intel_source() {
        let adapters = [adapter(
            "Intel(R) Arc(TM) A770",
            GpuVendor::Intel,
            0xDD,
            Some(8 * 1024 * 1024 * 1024),
        )];
        let intel = [reading("", Some(4.0), Some(52.0), TempSource::Intel, None)];
        let samples = build_samples(&adapters, &HashMap::new(), &[], &[], &intel);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].temp_c, Some(52.0));
        assert_eq!(samples[0].temp_source, TempSource::Intel);
    }

    #[test]
    fn microsoft_basic_render_stays_vendor_neutral() {
        let adapters = [adapter(
            "Microsoft Basic Render Driver",
            GpuVendor::Microsoft,
            0xCC,
            None,
        )];
        let samples = build_samples(&adapters, &HashMap::new(), &[], &[], &[]);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].vendor, GpuVendor::Microsoft);
        assert_eq!(samples[0].temp_c, None);
        assert_eq!(samples[0].usage_pct, None);
    }

    fn sample(name: &str, vendor: GpuVendor, vram: Option<u64>, temp: Option<f32>) -> GpuSample {
        GpuSample {
            name: name.to_owned(),
            vendor,
            dedicated_vram_bytes: vram,
            usage_pct: Some(1.0),
            temp_c: temp,
            ..Default::default()
        }
    }

    #[test]
    fn primary_prefers_the_larger_discrete_vendor_gpu() {
        let samples = [
            sample(
                "AMD Radeon(TM) Graphics",
                GpuVendor::Amd,
                Some(512 << 20),
                Some(47.0),
            ),
            sample(
                "NVIDIA GeForce GTX 1650",
                GpuVendor::Nvidia,
                Some(4 << 30),
                Some(42.0),
            ),
            sample(
                "Microsoft Basic Render Driver",
                GpuVendor::Microsoft,
                None,
                None,
            ),
        ];
        assert_eq!(select_primary_index(&samples), Some(1));

        let samples = [
            sample(
                "NVIDIA GeForce RTX 3060",
                GpuVendor::Nvidia,
                Some(12 << 30),
                Some(60.0),
            ),
            sample(
                "AMD Radeon RX 7900",
                GpuVendor::Amd,
                Some(20 << 30),
                Some(55.0),
            ),
        ];
        assert_eq!(select_primary_index(&samples), Some(1));
    }

    #[test]
    fn primary_skips_microsoft_and_falls_back_to_the_first_adapter() {
        let samples = [
            sample(
                "Microsoft Basic Render Driver",
                GpuVendor::Microsoft,
                None,
                None,
            ),
            sample("Intel(R) UHD Graphics 630", GpuVendor::Intel, None, None),
        ];
        assert_eq!(select_primary_index(&samples), Some(1));

        let only_software = [sample(
            "Microsoft Basic Render Driver",
            GpuVendor::Microsoft,
            None,
            None,
        )];
        assert_eq!(select_primary_index(&only_software), Some(0));
        assert_eq!(select_primary_index(&[]), None);
    }
}
