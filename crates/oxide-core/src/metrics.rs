use std::time::Instant;

/// Where a temperature reading came from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TempSource {
    #[default]
    None,
    /// ACPI thermal zone (platform zone, not CPU core).
    Acpi,
    /// NVIDIA NVML.
    Nvml,
    /// AMD ADLX/ADL.
    Adlx,
    /// Intel Level Zero Sysman.
    Intel,
    /// NVMe SMART/health log via IOCTL.
    Nvme,
    /// HWiNFO shared memory.
    Hwinfo,
    /// LibreHardwareMonitor HTTP.
    Lhm,
    /// Optional elevated PawnIO helper.
    PawnIo,
}

impl TempSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Acpi => "acpi",
            Self::Nvml => "nvml",
            Self::Adlx => "adlx",
            Self::Intel => "intel",
            Self::Nvme => "nvme",
            Self::Hwinfo => "hwinfo",
            Self::Lhm => "lhm",
            Self::PawnIo => "pawnio",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct NetworkSample {
    pub adapter: String,
    pub up_bps: f64,
    pub down_bps: f64,
    pub connected: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CpuSample {
    pub usage_pct: f32,
    pub freq_mhz: Option<f32>,
    pub temp_c: Option<f32>,
    pub temp_source: TempSource,
}

#[derive(Clone, Debug, Default)]
pub struct MemorySample {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub usage_pct: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpuVendor {
    #[default]
    Unknown,
    Nvidia,
    Amd,
    Intel,
    Microsoft,
    Qualcomm,
}

impl GpuVendor {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Nvidia => "nvidia",
            Self::Amd => "amd",
            Self::Intel => "intel",
            Self::Microsoft => "microsoft",
            Self::Qualcomm => "qualcomm",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct GpuSample {
    pub name: String,
    pub vendor: GpuVendor,
    /// Windows LUID when known; lets callers correlate PDH/DXGI/vendor data.
    pub luid: Option<u64>,
    pub usage_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub temp_source: TempSource,
    pub dedicated_vram_bytes: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct DiskSample {
    pub name: String,
    /// `PhysicalDriveN` index when known.
    pub index: Option<u32>,
    pub is_nvme: bool,
    pub activity_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub temp_source: TempSource,
}

/// Immutable snapshot published by the sensor thread once per second.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub seq: u64,
    pub captured_at: Instant,
    pub network: NetworkSample,
    pub cpu: CpuSample,
    pub memory: MemorySample,
    /// Primary GPU (the one shown by the taskbar widget).
    pub gpu: GpuSample,
    /// All discovered GPUs, in DXGI order.
    pub gpus: Vec<GpuSample>,
    /// Primary disk (the one shown by the taskbar widget).
    pub disk: DiskSample,
    /// All discovered physical disks.
    pub disks: Vec<DiskSample>,
    /// External temperature provider health, in UI order (HWiNFO, LHM).
    pub providers: Vec<crate::inventory::ProviderStatus>,
    /// Official version / staged-update status (LHM, PawnIO).
    pub provider_updates: Vec<crate::provider_update::ProviderUpdateStatus>,
    /// Optional elevated PawnIO helper state (CPU temperature).
    pub pawnio: crate::pawnio::PawnioStatus,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            seq: 0,
            captured_at: Instant::now(),
            network: NetworkSample::default(),
            cpu: CpuSample::default(),
            memory: MemorySample::default(),
            gpu: GpuSample::default(),
            gpus: Vec::new(),
            disk: DiskSample::default(),
            disks: Vec::new(),
            providers: Vec::new(),
            provider_updates: Vec::new(),
            pawnio: crate::pawnio::PawnioStatus::default(),
        }
    }
}

impl Snapshot {
    /// Primary GPU for the single-GPU widget. Prefers a discrete vendor GPU,
    /// then the first discovered adapter, then an empty sample.
    pub fn primary_gpu(&self) -> &GpuSample {
        self.gpu
            .vendor
            .ne(&GpuVendor::Unknown)
            .then_some(&self.gpu)
            .or_else(|| self.gpus.first())
            .unwrap_or(&self.gpu)
    }
}
