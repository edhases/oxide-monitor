//! Serializable hardware discovery inventory for the Settings UI and Flyout.

use serde::{Deserialize, Serialize};

use crate::metrics::{GpuVendor, Snapshot, TempSource};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuRole {
    #[default]
    Unknown,
    Integrated,
    Discrete,
    Software,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskBus {
    #[default]
    Unknown,
    Nvme,
    Sata,
    Usb,
    Raid,
    Virtual,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealth {
    #[default]
    Disabled,
    Connected,
    NotRunning,
    Stale,
    LimitExpired,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AdapterDescriptor {
    pub luid: u64,
    pub alias: String,
    pub description: String,
    pub link_speed_bps: u64,
    pub is_wireless: bool,
    pub is_virtual: bool,
    pub connected: bool,
    pub is_active_route: bool,
    pub up_bps: f64,
    pub down_bps: f64,
    pub ipv4: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuDescriptor {
    pub index: usize,
    pub luid: Option<u64>,
    pub name: String,
    pub vendor: String,
    pub role: GpuRole,
    pub is_primary: bool,
    pub usage_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub temp_source: String,
    pub vram_used_bytes: Option<u64>,
    pub vram_total_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiskDescriptor {
    pub index: u32,
    pub name: String,
    pub bus: DiskBus,
    pub drive_letters: Vec<String>,
    pub is_primary: bool,
    pub activity_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub temp_source: String,
    pub warning_temp_c: Option<f32>,
    pub critical_temp_c: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuDescriptor {
    pub model_name: String,
    pub logical_cores: u32,
    pub usage_pct: f32,
    pub per_core_pct: Vec<f32>,
    pub freq_mhz: Option<f32>,
    pub temp_c: Option<f32>,
    pub temp_source: String,
    pub acpi_temp_c: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderStatus {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub health: ProviderHealth,
    pub sensor_count: usize,
    pub detail: String,
    pub last_update_age_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExternalSensorDescriptor {
    pub provider: String,
    pub group: String,
    pub name: String,
    pub unit: String,
    pub value: f64,
}

/// Complete live hardware inventory presented in the "Sensors & Hardware" tab
/// and used to populate device pickers and Flyout details.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HardwareInventory {
    pub cpu: CpuDescriptor,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub memory_usage_pct: f32,
    pub adapters: Vec<AdapterDescriptor>,
    pub gpus: Vec<GpuDescriptor>,
    pub disks: Vec<DiskDescriptor>,
    pub providers: Vec<ProviderStatus>,
    /// Official version / staged-update status.
    pub provider_updates: Vec<crate::provider_update::ProviderUpdateStatus>,
    /// Optional elevated PawnIO helper status.
    pub pawnio: crate::pawnio::PawnioStatus,
    pub external_sensors: Vec<ExternalSensorDescriptor>,
}

impl HardwareInventory {
    /// Build a baseline inventory directly from a `Snapshot` (useful when
    /// extended adapter/per-core telemetry is not yet populated).
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let primary_gpu_luid = snapshot.primary_gpu().luid;
        let gpus = snapshot
            .gpus
            .iter()
            .enumerate()
            .map(|(index, gpu)| {
                let role = infer_gpu_role(gpu.vendor, &gpu.name, gpu.dedicated_vram_bytes);
                let is_primary = match (gpu.luid, primary_gpu_luid) {
                    (Some(a), Some(b)) => a == b,
                    _ => index == 0,
                };
                GpuDescriptor {
                    index,
                    luid: gpu.luid,
                    name: gpu.name.clone(),
                    vendor: gpu.vendor.as_str().to_string(),
                    role,
                    is_primary,
                    usage_pct: gpu.usage_pct,
                    temp_c: gpu.temp_c,
                    temp_source: gpu.temp_source.as_str().to_string(),
                    vram_used_bytes: None,
                    vram_total_bytes: gpu.dedicated_vram_bytes,
                }
            })
            .collect();

        let primary_disk_idx = snapshot.disk.index;
        let disks = snapshot
            .disks
            .iter()
            .enumerate()
            .map(|(pos, disk)| {
                let idx = disk.index.unwrap_or(pos as u32);
                DiskDescriptor {
                    index: idx,
                    name: disk.name.clone(),
                    bus: if disk.is_nvme {
                        DiskBus::Nvme
                    } else {
                        DiskBus::Sata
                    },
                    drive_letters: Vec::new(),
                    is_primary: primary_disk_idx == Some(idx)
                        || (primary_disk_idx.is_none() && pos == 0),
                    activity_pct: disk.activity_pct,
                    temp_c: disk.temp_c,
                    temp_source: disk.temp_source.as_str().to_string(),
                    warning_temp_c: None,
                    critical_temp_c: None,
                }
            })
            .collect();

        let mut adapters = Vec::new();
        if !snapshot.network.adapter.is_empty() {
            adapters.push(AdapterDescriptor {
                luid: 0,
                alias: snapshot.network.adapter.clone(),
                description: snapshot.network.adapter.clone(),
                link_speed_bps: 0,
                is_wireless: snapshot
                    .network
                    .adapter
                    .to_ascii_lowercase()
                    .contains("wi-fi"),
                is_virtual: false,
                connected: snapshot.network.connected,
                is_active_route: true,
                up_bps: snapshot.network.up_bps,
                down_bps: snapshot.network.down_bps,
                ipv4: None,
            });
        }

        Self {
            cpu: CpuDescriptor {
                model_name: String::new(),
                logical_cores: 0,
                usage_pct: snapshot.cpu.usage_pct,
                per_core_pct: Vec::new(),
                freq_mhz: snapshot.cpu.freq_mhz,
                temp_c: snapshot.cpu.temp_c,
                temp_source: snapshot.cpu.temp_source.as_str().to_string(),
                acpi_temp_c: (snapshot.cpu.temp_source == TempSource::Acpi)
                    .then_some(snapshot.cpu.temp_c)
                    .flatten(),
            },
            memory_used_bytes: snapshot.memory.used_bytes,
            memory_total_bytes: snapshot.memory.total_bytes,
            memory_usage_pct: snapshot.memory.usage_pct,
            adapters,
            gpus,
            disks,
            providers: snapshot.providers.clone(),
            provider_updates: snapshot.provider_updates.clone(),
            pawnio: snapshot.pawnio.clone(),
            external_sensors: Vec::new(),
        }
    }
}

/// Heuristic classification of integrated vs discrete GPU based on vendor, name,
/// and dedicated VRAM size.
pub fn infer_gpu_role(vendor: GpuVendor, name: &str, dedicated_vram_bytes: Option<u64>) -> GpuRole {
    let lower = name.to_ascii_lowercase();
    if vendor == GpuVendor::Microsoft || lower.contains("basic render") || lower.contains("warp") {
        return GpuRole::Software;
    }
    if lower.contains("uhd")
        || lower.contains("iris")
        || lower.contains("vega")
        || lower.contains("radeon(tm) graphics")
        || lower.contains("adreno")
    {
        return GpuRole::Integrated;
    }
    if vendor == GpuVendor::Nvidia
        || lower.contains("geforce")
        || lower.contains("rtx")
        || lower.contains("gtx")
        || lower.contains("rx ")
    {
        return GpuRole::Discrete;
    }
    if let Some(vram) = dedicated_vram_bytes {
        // Dedicated GPUs typically report >= 1.5 GiB dedicated VRAM; iGPUs usually <= 1 GiB.
        if vram >= 1_500_000_000 {
            return GpuRole::Discrete;
        }
        return GpuRole::Integrated;
    }
    GpuRole::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_role_heuristic_distinguishes_igpu_and_dgpu() {
        assert_eq!(
            infer_gpu_role(
                GpuVendor::Nvidia,
                "NVIDIA GeForce GTX 1650",
                Some(4_294_967_296)
            ),
            GpuRole::Discrete
        );
        assert_eq!(
            infer_gpu_role(GpuVendor::Amd, "AMD Radeon(TM) Graphics", Some(536_870_912)),
            GpuRole::Integrated
        );
        assert_eq!(
            infer_gpu_role(
                GpuVendor::Microsoft,
                "Microsoft Basic Render Driver",
                Some(0)
            ),
            GpuRole::Software
        );
    }

    #[test]
    fn inventory_clones_provider_status_from_the_snapshot() {
        let mut snap = Snapshot::default();
        snap.providers = vec![ProviderStatus {
            id: "hwinfo".into(),
            display_name: "HWiNFO".into(),
            enabled: true,
            health: ProviderHealth::Connected,
            sensor_count: 2,
            detail: String::new(),
            last_update_age_ms: Some(250),
        }];
        snap.provider_updates = vec![crate::provider_update::ProviderUpdateStatus {
            id: "pawnio".into(),
            display_name: "PawnIO".into(),
            latest_version: Some("2.2.0".into()),
            ..Default::default()
        }];

        let inventory = HardwareInventory::from_snapshot(&snap);
        assert_eq!(inventory.providers, snap.providers);
        assert_eq!(inventory.provider_updates, snap.provider_updates);
    }
}
