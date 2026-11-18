//! NVIDIA name, utilization and temperature via NVML.
//!
//! `nvml.dll` ships with the display driver in `System32`; it is loaded lazily and
//! only from there. A machine without an NVIDIA driver, or an ARM64 machine with no
//! NVML build, simply disables this backend.
//!
//! The discrete GPU of an Optimus laptop can be in D3cold. Calling NVML every
//! second would keep waking it, so the owner probes at most once per configured
//! interval while the adapter is idle and immediately when PDH shows real work.
//! All devices are enumerated, not just device 0, because a workstation can have
//! several NVIDIA GPUs.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use crate::dynlib::Library;

/// A discrete GPU above this vendor-neutral PDH load is treated as busy.
pub(crate) const BUSY_USAGE_PCT: f32 = 2.0;

/// Readings outside this range are driver noise (e.g. an unsupported sensor returning
/// 0) and must not be shown as a real temperature.
const MIN_TEMP_C: f32 = 1.0;
const MAX_TEMP_C: f32 = 125.0;

/// Keep the last good temperature through short probe gaps, but do not show a
/// reading that is minutes old after the dGPU went to sleep.
const TEMP_CACHE_MAX_AGE: Duration = Duration::from_secs(60);

/// NVML utilization is only a fallback for adapters without PDH counters.
const USAGE_CACHE_MAX_AGE: Duration = Duration::from_secs(15);

/// When NVML is absent (no driver) or the driver is not ready (D3cold), retry
/// slowly instead of hammering it every tick.
const RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// Defensive cap; no consumer machine exposes this many NVIDIA GPUs.
const MAX_DEVICES: u32 = 64;

/// NVML's maximum device-name length, including the terminator.
const NAME_BUFFER_LEN: usize = 96;

type NvmlReturn = i32;
type NvmlDeviceHandle = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NvmlMemory {
    total: u64,
    free: u64,
    used: u64,
}

type InitFn = unsafe extern "C" fn() -> NvmlReturn;
type ShutdownFn = unsafe extern "C" fn() -> NvmlReturn;
type DeviceCountFn = unsafe extern "C" fn(*mut u32) -> NvmlReturn;
type DeviceByIndexFn = unsafe extern "C" fn(u32, *mut NvmlDeviceHandle) -> NvmlReturn;
type DeviceNameFn = unsafe extern "C" fn(NvmlDeviceHandle, *mut u8, u32) -> NvmlReturn;
type DeviceTemperatureFn = unsafe extern "C" fn(NvmlDeviceHandle, u32, *mut u32) -> NvmlReturn;
type DeviceUtilizationFn =
    unsafe extern "C" fn(NvmlDeviceHandle, *mut NvmlUtilization) -> NvmlReturn;
type DeviceMemoryFn = unsafe extern "C" fn(NvmlDeviceHandle, *mut NvmlMemory) -> NvmlReturn;

/// One adapter's NVML reading, ready to be matched against a DXGI adapter.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NvidiaReading {
    pub name: String,
    pub usage_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub dedicated_vram_bytes: Option<u64>,
}

struct NvmlLib {
    /// Kept alive for the function pointers and until `nvmlShutdown` has run.
    _library: Library,
    shutdown: ShutdownFn,
    device_count: DeviceCountFn,
    device_by_index: DeviceByIndexFn,
    device_name: DeviceNameFn,
    device_temperature: DeviceTemperatureFn,
    device_utilization: DeviceUtilizationFn,
    device_memory: DeviceMemoryFn,
}

impl NvmlLib {
    fn load() -> Option<Self> {
        let library = Library::load_system32("nvml.dll")?;

        // SAFETY: symbol names and function-pointer types match nvml.h. `nvmlInit_v2`
        // and the `_v2` enumeration helpers exist in every driver supported by
        // Windows 10/11; the `_v1` names are a compatibility fallback.
        let init = unsafe {
            library
                .symbol::<InitFn>(b"nvmlInit_v2\0")
                .or_else(|| library.symbol::<InitFn>(b"nvmlInit\0"))
        }?;
        let shutdown = unsafe { library.symbol::<ShutdownFn>(b"nvmlShutdown\0") }?;
        let device_count = unsafe {
            library
                .symbol::<DeviceCountFn>(b"nvmlDeviceGetCount_v2\0")
                .or_else(|| library.symbol::<DeviceCountFn>(b"nvmlDeviceGetCount\0"))
        }?;
        let device_by_index = unsafe {
            library
                .symbol::<DeviceByIndexFn>(b"nvmlDeviceGetHandleByIndex_v2\0")
                .or_else(|| library.symbol::<DeviceByIndexFn>(b"nvmlDeviceGetHandleByIndex\0"))
        }?;
        let device_name = unsafe { library.symbol::<DeviceNameFn>(b"nvmlDeviceGetName\0") }?;
        let device_temperature =
            unsafe { library.symbol::<DeviceTemperatureFn>(b"nvmlDeviceGetTemperature\0") }?;
        let device_utilization =
            unsafe { library.symbol::<DeviceUtilizationFn>(b"nvmlDeviceGetUtilizationRates\0") }?;
        let device_memory =
            unsafe { library.symbol::<DeviceMemoryFn>(b"nvmlDeviceGetMemoryInfo\0") }?;

        let status = unsafe { init() };
        if status != 0 {
            tracing::debug!(status, "NVML present but initialization failed");
            return None;
        }

        Some(Self {
            _library: library,
            shutdown,
            device_count,
            device_by_index,
            device_name,
            device_temperature,
            device_utilization,
            device_memory,
        })
    }

    fn device_count(&self) -> Option<u32> {
        let mut count = 0u32;
        let status = unsafe { (self.device_count)(&mut count) };
        (status == 0).then_some(count)
    }

    fn device_by_index(&self, index: u32) -> Option<NvmlDeviceHandle> {
        let mut handle: NvmlDeviceHandle = std::ptr::null_mut();
        let status = unsafe { (self.device_by_index)(index, &mut handle) };
        (status == 0 && !handle.is_null()).then_some(handle)
    }

    fn name(&self, handle: NvmlDeviceHandle) -> Option<String> {
        let mut buffer = [0u8; NAME_BUFFER_LEN];
        let status =
            unsafe { (self.device_name)(handle, buffer.as_mut_ptr(), buffer.len() as u32) };
        if status != 0 {
            return None;
        }
        let len = buffer
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(buffer.len());
        let name = String::from_utf8_lossy(&buffer[..len]).trim().to_owned();
        (!name.is_empty()).then_some(name)
    }

    /// Sensor type 0 is `NVML_TEMPERATURE_GPU`.
    fn temperature(&self, handle: NvmlDeviceHandle) -> Option<f32> {
        let mut value = 0u32;
        let status = unsafe { (self.device_temperature)(handle, 0, &mut value) };
        if status != 0 {
            return None;
        }
        let value = value as f32;
        (MIN_TEMP_C..=MAX_TEMP_C).contains(&value).then_some(value)
    }

    fn utilization(&self, handle: NvmlDeviceHandle) -> Option<f32> {
        let mut utilization = NvmlUtilization::default();
        let status = unsafe { (self.device_utilization)(handle, &mut utilization) };
        if status != 0 {
            return None;
        }
        Some((utilization.gpu as f32).clamp(0.0, 100.0))
    }

    fn memory(&self, handle: NvmlDeviceHandle) -> Option<u64> {
        let mut memory = NvmlMemory::default();
        let status = unsafe { (self.device_memory)(handle, &mut memory) };
        if status != 0 || memory.total == 0 {
            return None;
        }
        Some(memory.total)
    }
}

impl Drop for NvmlLib {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.shutdown)();
        }
    }
}

struct NvmlDevice {
    handle: NvmlDeviceHandle,
    name: String,
    dedicated_vram_bytes: Option<u64>,
    temp_c: Option<f32>,
    temp_read_at: Option<Instant>,
    usage_pct: Option<f32>,
    usage_read_at: Option<Instant>,
}

enum State {
    /// NVML has not been touched yet; initialization is deferred to the first probe.
    Pending,
    /// `Nvml` is boxed to keep `GpuSampler` small.
    Ready(Box<NvmlLib>),
    /// No usable NVIDIA driver/library; retried after a slow backoff.
    Unavailable { retry_at: Instant },
}

/// Multi-device NVML backend.
pub(crate) struct NvmlBackend {
    state: State,
    devices: Vec<NvmlDevice>,
    last_probe: Option<Instant>,
    error_logged: bool,
    empty_logged: bool,
}

// SAFETY: every NVML handle is used only through `&mut self` on the sampler thread.
// NVML itself is documented as thread-safe, but `Send` alone is all `GpuSampler`
// needs (it is moved to the sampler thread and never shared).
unsafe impl Send for NvmlBackend {}

impl NvmlBackend {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Pending,
            devices: Vec::new(),
            last_probe: None,
            error_logged: false,
            empty_logged: false,
        }
    }

    /// Whether this tick should touch NVML at all.
    ///
    /// The first call always probes. Afterwards a probe is due when the configured
    /// interval elapsed, or when PDH shows the GPU is actually busy. An idle dGPU is
    /// therefore queried once per interval instead of every tick.
    pub(crate) fn probe_due(&self, interval: Duration, busy: bool) -> bool {
        match self.last_probe {
            None => true,
            Some(at) => busy || at.elapsed() >= interval,
        }
    }

    /// Read name (once), utilization and temperature for every NVIDIA device.
    pub(crate) fn probe(&mut self) {
        self.last_probe = Some(Instant::now());

        if !self.ensure_init() {
            return;
        }
        if self.devices.is_empty() {
            self.enumerate();
        }

        let State::Ready(lib) = &self.state else {
            return;
        };

        let now = Instant::now();
        let mut probed = 0usize;
        for device in &mut self.devices {
            let mut any_success = false;

            if let Some(usage) = lib.utilization(device.handle) {
                device.usage_pct = Some(usage);
                device.usage_read_at = Some(now);
                any_success = true;
            }
            if let Some(temp) = lib.temperature(device.handle) {
                device.temp_c = Some(temp);
                device.temp_read_at = Some(now);
                any_success = true;
            }

            if any_success {
                probed += 1;
            } else {
                // The dGPU may be powered down (Optimus D3cold) or the driver is
                // temporarily unavailable. Keep the last good readings until their
                // cache age expires; do not log per tick.
                tracing::trace!("NVML device did not answer; keeping cached values");
            }
        }

        if probed == 0 && !self.empty_logged && !self.devices.is_empty() {
            self.empty_logged = true;
            tracing::debug!("NVML devices are present but did not answer");
        }
    }

    /// All NVIDIA adapters, in NVML enumeration order.
    pub(crate) fn readings(&self) -> Vec<NvidiaReading> {
        let now = Instant::now();
        self.devices
            .iter()
            .map(|device| NvidiaReading {
                name: device.name.clone(),
                usage_pct: device.usage_pct.filter(|_| {
                    device
                        .usage_read_at
                        .is_some_and(|at| now.duration_since(at) <= USAGE_CACHE_MAX_AGE)
                }),
                temp_c: device.temp_c.filter(|_| {
                    device
                        .temp_read_at
                        .is_some_and(|at| now.duration_since(at) <= TEMP_CACHE_MAX_AGE)
                }),
                dedicated_vram_bytes: device.dedicated_vram_bytes,
            })
            .collect()
    }

    fn ensure_init(&mut self) -> bool {
        match &self.state {
            State::Ready(_) => return true,
            State::Unavailable { retry_at } if Instant::now() < *retry_at => return false,
            State::Pending | State::Unavailable { .. } => {}
        }

        match NvmlLib::load() {
            Some(lib) => {
                self.state = State::Ready(Box::new(lib));
                self.empty_logged = false;
                true
            }
            None => {
                if !self.error_logged {
                    self.error_logged = true;
                    // Expected on AMD/Intel-only machines; debug-level and only once.
                    tracing::debug!("NVML unavailable; NVIDIA GPU data disabled until retry");
                }
                self.state = State::Unavailable {
                    retry_at: Instant::now() + RETRY_INTERVAL,
                };
                false
            }
        }
    }

    fn enumerate(&mut self) {
        let State::Ready(lib) = &self.state else {
            return;
        };

        let Some(count) = lib.device_count() else {
            return;
        };
        let count = count.min(MAX_DEVICES);
        if count == 0 {
            if !self.empty_logged {
                self.empty_logged = true;
                tracing::debug!("NVML reports no devices (dGPU may be in D3cold)");
            }
            return;
        }

        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let Some(handle) = lib.device_by_index(index) else {
                continue;
            };
            let name = lib
                .name(handle)
                .unwrap_or_else(|| format!("NVIDIA GPU {index}"));
            devices.push(NvmlDevice {
                handle,
                name,
                dedicated_vram_bytes: lib.memory(handle),
                temp_c: None,
                temp_read_at: None,
                usage_pct: None,
                usage_read_at: None,
            });
        }

        self.devices = devices;
    }
}
