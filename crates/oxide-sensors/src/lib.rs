//! Native, non-admin sensor providers.
//!
//! [`NativeSensors`] owns the network, CPU, memory, disk, ACPI and GPU samplers.
//! It deliberately contains no kernel driver and requires no elevation:
//! everything here is IP Helper, PDH, `GlobalMemoryStatusEx` or a storage
//! property IOCTL. Every source degrades to an empty/`None` value instead of
//! failing the snapshot.

mod acpi;
mod cpu;
mod disk;
mod memory;
mod network;
mod pdh;

use std::time::Instant;

use oxide_core::{Config, GpuVendor, Snapshot, TempSource};
use oxide_gpu::GpuSampler;

use crate::acpi::AcpiSampler;
use crate::cpu::CpuSampler;
use crate::disk::DiskSampler;
use crate::memory::MemorySampler;
use crate::network::NetworkSampler;

/// Owns the native samplers and produces one immutable [`Snapshot`] per poll.
pub struct NativeSensors {
    network: NetworkSampler,
    cpu: CpuSampler,
    memory: MemorySampler,
    disk: DiskSampler,
    acpi: AcpiSampler,
    gpu: GpuSampler,
    seq: u64,
}

impl NativeSensors {
    pub fn new(config: Config) -> Self {
        let network = NetworkSampler::new(&config);
        let disk = DiskSampler::new(config.general.disk_index);
        let acpi = AcpiSampler::new(config.general.cpu_temp_source);
        let gpu = GpuSampler::new_with_interval(config.general.gpu_poll_ms);

        Self {
            network,
            cpu: CpuSampler::new(),
            memory: MemorySampler::new(),
            disk,
            acpi,
            gpu,
            seq: 0,
        }
    }

    /// Sample every source once. Never panics: unavailable sources yield
    /// defaults or `None`. The snapshot owns sequencing and its capture time.
    pub fn poll(&mut self) -> Snapshot {
        let network = self.network.poll();
        let mut cpu = self.cpu.poll();
        let memory = self.memory.poll();
        let disks = self.disk.poll_all();
        let disk = self.disk.poll();
        let gpus = self.gpu.poll_all();
        let gpu = gpus
            .iter()
            .filter(|gpu| matches!(gpu.vendor, GpuVendor::Nvidia | GpuVendor::Amd))
            .max_by_key(|gpu| gpu.dedicated_vram_bytes.unwrap_or(0))
            .or_else(|| gpus.iter().find(|gpu| gpu.vendor != GpuVendor::Microsoft))
            .or_else(|| gpus.first())
            .cloned()
            .unwrap_or_default();

        if let Some(temp_c) = self.acpi.poll(cpu.usage_pct) {
            cpu.temp_c = Some(temp_c);
            cpu.temp_source = TempSource::Acpi;
        }

        self.seq = self.seq.wrapping_add(1);
        Snapshot {
            seq: self.seq,
            captured_at: Instant::now(),
            network,
            cpu,
            memory,
            gpu,
            gpus,
            disk,
            disks,
            providers: Vec::new(),
            provider_updates: Vec::new(),
            pawnio: oxide_core::PawnioStatus::default(),
        }
    }
}
