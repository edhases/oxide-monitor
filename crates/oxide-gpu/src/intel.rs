//! Intel GPU temperature and utilization via oneAPI Level Zero Sysman (optional).
//!
//! `ze_loader.dll` is installed with Intel graphics drivers. It is probed from
//! `System32` and used through plain C exports — no build-time SDK dependency and
//! no structs with ABI-versioned layouts. Only two stable outputs are read:
//!
//! * `zesTemperatureGetState` — a `double` in degrees Celsius;
//! * `zesEngineGetActivity` — two `u64` counters (`activeTime`, `timestamp`).
//!
//! Every GPU still appears through DXGI and gets PDH utilization when Level Zero
//! is absent. The loader also exists on non-Intel machines, so this backend is
//! only touched when DXGI actually reports an Intel adapter.
//!
//! Temperatures read here are reported with `TempSource::Intel` by the sampler.

use std::ffi::c_void;

use crate::dynlib::Library;

const MAX_DRIVERS: u32 = 16;
const MAX_DEVICES: u32 = 32;
const MAX_SENSORS_PER_DEVICE: u32 = 16;
const MAX_ENGINES_PER_DEVICE: u32 = 16;

const MIN_TEMP_C: f64 = -40.0;
const MAX_TEMP_C: f64 = 150.0;

type ZeResult = i32;
type Handle = *mut c_void;

type ZeInitFn = unsafe extern "C" fn(u32) -> ZeResult;
type ZesInitFn = unsafe extern "C" fn(u32) -> ZeResult;
type ZesDriverGetFn = unsafe extern "C" fn(*mut u32, *mut Handle) -> ZeResult;
type ZesDeviceGetFn = unsafe extern "C" fn(Handle, *mut u32, *mut Handle) -> ZeResult;
type ZesDeviceEnumTemperatureSensorsFn =
    unsafe extern "C" fn(Handle, *mut u32, *mut Handle) -> ZeResult;
type ZesTemperatureGetStateFn = unsafe extern "C" fn(Handle, *mut f64) -> ZeResult;
type ZesDeviceEnumEngineGroupsFn = unsafe extern "C" fn(Handle, *mut u32, *mut Handle) -> ZeResult;
type ZesEngineGetActivityFn = unsafe extern "C" fn(Handle, *mut ZesEngineStats) -> ZeResult;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ZesEngineStats {
    active_time: u64,
    timestamp: u64,
}

/// One Intel device's current readings, in ZES enumeration order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct IntelReading {
    pub usage_pct: Option<f32>,
    pub temp_c: Option<f32>,
}

struct IntelLib {
    _library: Library,
    temperature_get_state: Option<ZesTemperatureGetStateFn>,
    engine_get_activity: Option<ZesEngineGetActivityFn>,
}

struct EngineState {
    handle: Handle,
    last: Option<(u64, u64)>,
}

struct IntelDevice {
    temperature_sensors: Vec<Handle>,
    engines: Vec<EngineState>,
}

pub(crate) struct IntelBackend {
    state: State,
    devices: Vec<IntelDevice>,
}

enum State {
    Pending,
    Ready(Box<IntelLib>),
    Unavailable,
}

// SAFETY: all Level Zero handles are used only through `&mut self` on the sampler
// thread; nothing shares them across threads.
unsafe impl Send for IntelBackend {}

impl IntelBackend {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Pending,
            devices: Vec::new(),
        }
    }

    /// Read every Intel Sysman device. Returns an empty list when Level Zero is
    /// missing, old, or has no devices.
    pub(crate) fn poll(&mut self) -> Vec<IntelReading> {
        if !self.ensure_init() {
            return Vec::new();
        }

        let State::Ready(lib) = &self.state else {
            return Vec::new();
        };

        let mut readings = Vec::with_capacity(self.devices.len());
        for device in &mut self.devices {
            readings.push(IntelReading {
                usage_pct: read_usage(lib, device),
                temp_c: read_temperature(lib, device),
            });
        }
        readings
    }

    fn ensure_init(&mut self) -> bool {
        if matches!(self.state, State::Ready(_)) {
            return true;
        }
        if matches!(self.state, State::Unavailable) {
            return false;
        }

        match load_level_zero() {
            Some((lib, devices)) => {
                self.devices = devices;
                self.state = State::Ready(Box::new(lib));
                true
            }
            None => {
                self.state = State::Unavailable;
                false
            }
        }
    }
}

fn load_level_zero() -> Option<(IntelLib, Vec<IntelDevice>)> {
    let library = Library::load_system32("ze_loader.dll")?;

    // SAFETY: names and signatures match zes_api.h/ze_api.h. Missing optional
    // exports simply disable that metric.
    let ze_init = unsafe { library.symbol::<ZeInitFn>(b"zeInit\0") };
    let zes_init = unsafe { library.symbol::<ZesInitFn>(b"zesInit\0") }?;
    let zes_driver_get = unsafe { library.symbol::<ZesDriverGetFn>(b"zesDriverGet\0") }?;
    let zes_device_get = unsafe { library.symbol::<ZesDeviceGetFn>(b"zesDeviceGet\0") }?;

    let device_enum_temperature = unsafe {
        library.symbol::<ZesDeviceEnumTemperatureSensorsFn>(b"zesDeviceEnumTemperatureSensors\0")
    };
    let temperature_get_state =
        unsafe { library.symbol::<ZesTemperatureGetStateFn>(b"zesTemperatureGetState\0") };
    let device_enum_engines =
        unsafe { library.symbol::<ZesDeviceEnumEngineGroupsFn>(b"zesDeviceEnumEngineGroups\0") };
    let engine_get_activity =
        unsafe { library.symbol::<ZesEngineGetActivityFn>(b"zesEngineGetActivity\0") };

    // `zesInit` initializes Sysman only; `zeInit` initializes the core. Calling
    // both is allowed in either order and covers loaders that expect core init.
    if let Some(ze_init) = ze_init {
        let _ = unsafe { ze_init(0) };
    }
    let status = unsafe { zes_init(0) };
    if status != 0 {
        tracing::debug!(status, "Level Zero Sysman initialization failed");
        return None;
    }

    let mut drivers: Vec<Handle> = Vec::new();
    let mut driver_count = 0u32;
    let status = unsafe { zes_driver_get(&mut driver_count, std::ptr::null_mut()) };
    if status != 0 || driver_count == 0 {
        return None;
    }
    driver_count = driver_count.min(MAX_DRIVERS);
    drivers.resize(driver_count as usize, std::ptr::null_mut());
    let mut actual = driver_count;
    let status = unsafe { zes_driver_get(&mut actual, drivers.as_mut_ptr()) };
    if status != 0 {
        return None;
    }
    drivers.truncate(actual.min(driver_count) as usize);

    let mut devices: Vec<IntelDevice> = Vec::new();
    for driver in drivers {
        let mut count = 0u32;
        let status = unsafe { zes_device_get(driver, &mut count, std::ptr::null_mut()) };
        if status != 0 || count == 0 {
            continue;
        }
        let remaining = MAX_DEVICES.saturating_sub(devices.len() as u32);
        let count = count.min(remaining);
        if count == 0 {
            break;
        }
        let mut handles = vec![std::ptr::null_mut(); count as usize];
        let mut actual = count;
        let status = unsafe { zes_device_get(driver, &mut actual, handles.as_mut_ptr()) };
        if status != 0 {
            continue;
        }
        handles.truncate(actual.min(count) as usize);

        for handle in handles {
            let temperature_sensors = match (device_enum_temperature, temperature_get_state) {
                (Some(enum_sensors), Some(_)) => {
                    enumerate_components(|pcount, out| unsafe { enum_sensors(handle, pcount, out) })
                        .into_iter()
                        .take(MAX_SENSORS_PER_DEVICE as usize)
                        .collect()
                }
                _ => Vec::new(),
            };

            let engines = match (device_enum_engines, engine_get_activity) {
                (Some(enum_engines), Some(_)) => {
                    enumerate_components(|pcount, out| unsafe { enum_engines(handle, pcount, out) })
                        .into_iter()
                        .take(MAX_ENGINES_PER_DEVICE as usize)
                        .map(|handle| EngineState { handle, last: None })
                        .collect()
                }
                _ => Vec::new(),
            };

            devices.push(IntelDevice {
                temperature_sensors,
                engines,
            });
        }
    }

    if devices.is_empty() {
        tracing::debug!("Level Zero Sysman reported no devices");
        return None;
    }

    Some((
        IntelLib {
            _library: library,
            temperature_get_state,
            engine_get_activity,
        },
        devices,
    ))
}

/// Enumerate opaque handles with the standard Level Zero two-call pattern.
fn enumerate_components<F>(mut call: F) -> Vec<Handle>
where
    F: FnMut(*mut u32, *mut Handle) -> ZeResult,
{
    let mut count = 0u32;
    let status = call(&mut count, std::ptr::null_mut());
    if status != 0 || count == 0 {
        return Vec::new();
    }

    let count = count.min(MAX_SENSORS_PER_DEVICE.max(MAX_ENGINES_PER_DEVICE));
    let mut handles = vec![std::ptr::null_mut(); count as usize];
    let mut actual = count;
    let status = call(&mut actual, handles.as_mut_ptr());
    if status != 0 {
        return Vec::new();
    }
    handles.truncate(actual.min(count) as usize);
    handles
}

fn read_temperature(lib: &IntelLib, device: &IntelDevice) -> Option<f32> {
    let get_state = lib.temperature_get_state?;
    let mut best: Option<f64> = None;
    for sensor in &device.temperature_sensors {
        let mut celsius = 0.0f64;
        let status = unsafe { get_state(*sensor, &mut celsius) };
        if status != 0 || !celsius.is_finite() {
            continue;
        }
        if (MIN_TEMP_C..=MAX_TEMP_C).contains(&celsius) {
            best = Some(best.map_or(celsius, |current| current.max(celsius)));
        }
    }
    best.map(|value| value as f32)
}

fn read_usage(lib: &IntelLib, device: &mut IntelDevice) -> Option<f32> {
    let get_activity = lib.engine_get_activity?;
    let mut best: Option<f64> = None;

    for engine in &mut device.engines {
        let mut stats = ZesEngineStats::default();
        let status = unsafe { get_activity(engine.handle, &mut stats) };
        if status != 0 {
            continue;
        }

        if let Some((last_active, last_timestamp)) = engine.last
            && stats.timestamp > last_timestamp
        {
            let elapsed = stats.timestamp - last_timestamp;
            let active = stats.active_time.saturating_sub(last_active);
            if elapsed > 0 {
                let percent = (active as f64 / elapsed as f64) * 100.0;
                if percent.is_finite() {
                    best = Some(best.map_or(percent, |current| current.max(percent)));
                }
            }
        }

        engine.last = Some((stats.active_time, stats.timestamp));
    }

    best.map(|value| value.clamp(0.0, 100.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::IntelReading;

    #[test]
    fn intel_reading_is_plain_data() {
        let reading = IntelReading {
            usage_pct: Some(12.5),
            temp_c: Some(48.0),
        };
        assert_eq!(reading, reading);
        assert_eq!(IntelReading::default().usage_pct, None);
        assert_eq!(IntelReading::default().temp_c, None);
    }
}
