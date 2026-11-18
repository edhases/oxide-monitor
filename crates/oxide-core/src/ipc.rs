//! Named Pipe IPC protocol between `oxide-monitor.exe` and `oxide-settings.exe`.
//!
//! Framing: 4-byte little-endian `u32` payload length followed by UTF-8 JSON bytes.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::history::HistorySliceDto;
use crate::inventory::{HardwareInventory, ProviderStatus};
use crate::metrics::{
    CpuSample, DiskSample, GpuSample, GpuVendor, MemorySample, NetworkSample, Snapshot, TempSource,
};
use crate::provider_update::ProviderUpdateStatus;

pub const IPC_PROTOCOL_VERSION: u32 = 1;
pub const MAX_IPC_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// Build the per-process Named Pipe path for `oxide-monitor.exe`.
pub fn settings_pipe_name(monitor_pid: u32) -> String {
    format!(r"\\.\pipe\oxide-ui-v1-{monitor_pid}")
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorDescriptor {
    pub device: String,
    pub friendly_name: String,
    pub is_primary: bool,
    pub dpi: u32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkSampleDto {
    pub adapter: String,
    pub up_bps: f64,
    pub down_bps: f64,
    pub connected: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuSampleDto {
    pub usage_pct: f32,
    pub freq_mhz: Option<f32>,
    pub temp_c: Option<f32>,
    pub temp_source: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MemorySampleDto {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub usage_pct: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuSampleDto {
    pub name: String,
    pub vendor: String,
    pub luid: Option<u64>,
    pub usage_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub temp_source: String,
    pub dedicated_vram_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DiskSampleDto {
    pub name: String,
    pub index: Option<u32>,
    pub is_nvme: bool,
    pub activity_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub temp_source: String,
}

/// Serializable representation of `oxide_core::Snapshot` (without `Instant`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDto {
    pub seq: u64,
    pub network: NetworkSampleDto,
    pub cpu: CpuSampleDto,
    pub memory: MemorySampleDto,
    pub gpu: GpuSampleDto,
    pub gpus: Vec<GpuSampleDto>,
    pub disk: DiskSampleDto,
    pub disks: Vec<DiskSampleDto>,
    /// External provider health; defaults to empty for older peers/records.
    #[serde(default)]
    pub providers: Vec<ProviderStatus>,
    /// Official version / staged-update status; additive and optional.
    #[serde(default)]
    pub provider_updates: Vec<ProviderUpdateStatus>,
    /// Optional elevated PawnIO helper status; additive and optional.
    #[serde(default)]
    pub pawnio: crate::pawnio::PawnioStatus,
}

impl From<&Snapshot> for SnapshotDto {
    fn from(s: &Snapshot) -> Self {
        Self {
            seq: s.seq,
            network: NetworkSampleDto {
                adapter: s.network.adapter.clone(),
                up_bps: s.network.up_bps,
                down_bps: s.network.down_bps,
                connected: s.network.connected,
            },
            cpu: CpuSampleDto {
                usage_pct: s.cpu.usage_pct,
                freq_mhz: s.cpu.freq_mhz,
                temp_c: s.cpu.temp_c,
                temp_source: s.cpu.temp_source.as_str().to_string(),
            },
            memory: MemorySampleDto {
                used_bytes: s.memory.used_bytes,
                total_bytes: s.memory.total_bytes,
                usage_pct: s.memory.usage_pct,
            },
            gpu: gpu_to_dto(&s.gpu),
            gpus: s.gpus.iter().map(gpu_to_dto).collect(),
            disk: disk_to_dto(&s.disk),
            disks: s.disks.iter().map(disk_to_dto).collect(),
            providers: s.providers.clone(),
            provider_updates: s.provider_updates.clone(),
            pawnio: s.pawnio.clone(),
        }
    }
}

impl SnapshotDto {
    /// Reconstruct a runtime `Snapshot` inside `oxide-settings.exe` for Live Preview.
    pub fn to_snapshot(&self) -> Snapshot {
        Snapshot {
            seq: self.seq,
            captured_at: std::time::Instant::now(),
            network: NetworkSample {
                adapter: self.network.adapter.clone(),
                up_bps: self.network.up_bps,
                down_bps: self.network.down_bps,
                connected: self.network.connected,
            },
            cpu: CpuSample {
                usage_pct: self.cpu.usage_pct,
                freq_mhz: self.cpu.freq_mhz,
                temp_c: self.cpu.temp_c,
                temp_source: parse_temp_source(&self.cpu.temp_source),
            },
            memory: MemorySample {
                used_bytes: self.memory.used_bytes,
                total_bytes: self.memory.total_bytes,
                usage_pct: self.memory.usage_pct,
            },
            gpu: dto_to_gpu(&self.gpu),
            gpus: self.gpus.iter().map(dto_to_gpu).collect(),
            disk: dto_to_disk(&self.disk),
            disks: self.disks.iter().map(dto_to_disk).collect(),
            providers: self.providers.clone(),
            provider_updates: self.provider_updates.clone(),
            pawnio: self.pawnio.clone(),
        }
    }
}

fn gpu_to_dto(g: &GpuSample) -> GpuSampleDto {
    GpuSampleDto {
        name: g.name.clone(),
        vendor: g.vendor.as_str().to_string(),
        luid: g.luid,
        usage_pct: g.usage_pct,
        temp_c: g.temp_c,
        temp_source: g.temp_source.as_str().to_string(),
        dedicated_vram_bytes: g.dedicated_vram_bytes,
    }
}

fn dto_to_gpu(g: &GpuSampleDto) -> GpuSample {
    GpuSample {
        name: g.name.clone(),
        vendor: parse_gpu_vendor(&g.vendor),
        luid: g.luid,
        usage_pct: g.usage_pct,
        temp_c: g.temp_c,
        temp_source: parse_temp_source(&g.temp_source),
        dedicated_vram_bytes: g.dedicated_vram_bytes,
    }
}

fn disk_to_dto(d: &DiskSample) -> DiskSampleDto {
    DiskSampleDto {
        name: d.name.clone(),
        index: d.index,
        is_nvme: d.is_nvme,
        activity_pct: d.activity_pct,
        temp_c: d.temp_c,
        temp_source: d.temp_source.as_str().to_string(),
    }
}

fn dto_to_disk(d: &DiskSampleDto) -> DiskSample {
    DiskSample {
        name: d.name.clone(),
        index: d.index,
        is_nvme: d.is_nvme,
        activity_pct: d.activity_pct,
        temp_c: d.temp_c,
        temp_source: parse_temp_source(&d.temp_source),
    }
}

fn parse_temp_source(s: &str) -> TempSource {
    match s {
        "acpi" => TempSource::Acpi,
        "nvml" => TempSource::Nvml,
        "adlx" => TempSource::Adlx,
        "intel" => TempSource::Intel,
        "nvme" => TempSource::Nvme,
        "hwinfo" => TempSource::Hwinfo,
        "lhm" => TempSource::Lhm,
        "pawnio" => TempSource::PawnIo,
        _ => TempSource::None,
    }
}

fn parse_gpu_vendor(s: &str) -> GpuVendor {
    match s {
        "nvidia" => GpuVendor::Nvidia,
        "amd" => GpuVendor::Amd,
        "intel" => GpuVendor::Intel,
        "microsoft" => GpuVendor::Microsoft,
        "qualcomm" => GpuVendor::Qualcomm,
        _ => GpuVendor::Unknown,
    }
}

/// Messages sent from `oxide-monitor.exe` to `oxide-settings.exe`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MonitorToSettings {
    HelloAck {
        protocol_version: u32,
        app_version: String,
        arch: String,
        config_path: String,
        portable_mode: bool,
        active_config: Box<Config>,
        monitors: Vec<MonitorDescriptor>,
    },
    TelemetryTick {
        snapshot: SnapshotDto,
        history: HistorySliceDto,
        inventory: HardwareInventory,
    },
    MonitorsChanged {
        monitors: Vec<MonitorDescriptor>,
    },
    CommitResult {
        ok: bool,
        error: Option<String>,
    },
    FocusTab {
        tab: String,
    },
}

/// Messages sent from `oxide-settings.exe` to `oxide-monitor.exe`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingsToMonitor {
    Hello {
        protocol_version: u32,
        focus_tab: Option<String>,
    },
    PreviewConfig {
        config: Box<Config>,
    },
    CommitConfig {
        config: Box<Config>,
    },
    RevertConfig,
    ProbeProvider {
        provider_id: String,
    },
    Ping,
}

/// Write a length-prefixed payload frame (`u32 LE` + payload bytes).
pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_IPC_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame exceeds MAX_IPC_FRAME_BYTES",
        ));
    }
    let len = payload.len() as u32;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}

/// Read one length-prefixed payload frame (`u32 LE` + payload bytes).
pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header) as usize;
    if len == 0 || len > MAX_IPC_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid IPC frame length: {len}"),
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_roundtrip_and_limit() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"{\"type\":\"ping\"}").unwrap();

        let mut cursor = Cursor::new(wire);
        let decoded = read_frame(&mut cursor).unwrap();
        assert_eq!(decoded, b"{\"type\":\"ping\"}");
    }

    #[test]
    fn snapshot_dto_roundtrip() {
        let mut snap = Snapshot::default();
        snap.seq = 7;
        snap.cpu.usage_pct = 33.0;
        snap.cpu.temp_c = Some(52.0);
        snap.cpu.temp_source = TempSource::Hwinfo;

        let dto = SnapshotDto::from(&snap);
        let back = dto.to_snapshot();
        assert_eq!(back.seq, 7);
        assert_eq!(back.cpu.temp_source, TempSource::Hwinfo);
        assert_eq!(back.cpu.temp_c, Some(52.0));
    }

    #[test]
    fn snapshot_dto_roundtrips_provider_status() {
        use crate::inventory::{ProviderHealth, ProviderStatus};

        let mut snap = Snapshot::default();
        snap.providers = vec![
            ProviderStatus {
                id: "hwinfo".into(),
                display_name: "HWiNFO".into(),
                enabled: true,
                health: ProviderHealth::NotRunning,
                sensor_count: 0,
                detail: "shared memory not found".into(),
                last_update_age_ms: None,
            },
            ProviderStatus {
                id: "lhm_http".into(),
                display_name: "LibreHardwareMonitor".into(),
                enabled: true,
                health: ProviderHealth::Connected,
                sensor_count: 3,
                detail: String::new(),
                last_update_age_ms: Some(1200),
            },
        ];

        snap.provider_updates = vec![crate::provider_update::ProviderUpdateStatus {
            id: "lhm".into(),
            display_name: "LibreHardwareMonitor".into(),
            installed_version: Some("0.9.5".into()),
            latest_version: Some("0.9.6".into()),
            install_kind: crate::provider_update::ProviderInstallKind::Managed,
            state: crate::provider_update::ProviderUpdateState::UpdateAvailable,
            auto_update: true,
            official_url: crate::provider_update::LHM_OFFICIAL_URL.into(),
            staged_path: None,
            detail: "update available".into(),
            last_error: None,
            checked_at_unix_ms: Some(5),
        }];
        snap.pawnio = crate::pawnio::PawnioStatus {
            installed: true,
            installed_version: Some("2.1.0.0".into()),
            enabled: true,
            state: crate::pawnio::PawnioState::Running,
            temperature_c: Some(72.0),
            helper_version: Some("0.1.0".into()),
            detail: "connected".into(),
            updated_at_unix_ms: Some(6),
            ..crate::pawnio::PawnioStatus::default()
        };

        let dto = SnapshotDto::from(&snap);
        let back = dto.to_snapshot();
        assert_eq!(back.providers, snap.providers);
        assert_eq!(back.provider_updates, snap.provider_updates);
        assert_eq!(back.pawnio, snap.pawnio);

        // Serialized IPC payload keeps the status and stays backward compatible.
        let json = serde_json::to_string(&dto).unwrap();
        let decoded: SnapshotDto = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.providers, snap.providers);
        assert_eq!(decoded.provider_updates, snap.provider_updates);
        assert_eq!(decoded.pawnio, snap.pawnio);

        let legacy = r#"{"seq":1,"network":{"adapter":"","up_bps":0.0,"down_bps":0.0,"connected":false},"cpu":{"usage_pct":0.0,"freq_mhz":null,"temp_c":null,"temp_source":"none"},"memory":{"used_bytes":0,"total_bytes":0,"usage_pct":0.0},"gpu":{"name":"","vendor":"unknown","luid":null,"usage_pct":null,"temp_c":null,"temp_source":"none","dedicated_vram_bytes":null},"gpus":[],"disk":{"name":"","index":null,"is_nvme":false,"activity_pct":null,"temp_c":null,"temp_source":"none"},"disks":[]}"#;
        let decoded: SnapshotDto = serde_json::from_str(legacy).unwrap();
        assert!(decoded.providers.is_empty());
        assert!(decoded.provider_updates.is_empty());
        assert_eq!(decoded.pawnio.state, crate::pawnio::PawnioState::Disabled);
    }
}
