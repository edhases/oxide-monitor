//! Disk inventory, per-disk activity and NVMe temperatures.
//!
//! * Inventory: `\\.\PhysicalDrive0..31` opened with access 0 (unelevated) and
//!   queried with `IOCTL_STORAGE_QUERY_PROPERTY` + `StorageDeviceProperty`.
//! * Activity: PDH `\PhysicalDisk(*)\% Idle Time` via
//!   `PdhGetFormattedCounterArrayW`, converted to `100 - idle`, clamped to
//!   0..100 and mapped to the physical disk index from the instance name.
//! * Temperature: `StorageDeviceTemperatureProperty` (52) for NVMe, with an
//!   NVMe SMART/health log page 0x02 fallback. Both paths are non-admin
//!   user-mode IOCTLs.
//!
//! SATA, USB and RAID devices are intentionally inventoried but report
//! `temp_c = None`: Windows only exposes temperature through the NVMe paths
//! above without elevation. A RAID controller that hides the NVMe bus type is
//! treated as RAID and therefore also has no native temperature.
//!
//! Every failure degrades to `None` for the affected field; a broken disk never
//! fails the whole snapshot.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::mem::size_of;
use std::time::{Duration, Instant};

use oxide_core::{DiskSample, TempSource};
use tracing::debug;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION,
    ERROR_IO_DEVICE, ERROR_NOT_READY, ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND, GetLastError,
    HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    BusTypeNvme, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    IOCTL_STORAGE_QUERY_PROPERTY, NVMeDataTypeLogPage, PropertyStandardQuery, ProtocolTypeNvme,
    STORAGE_PROPERTY_ID, STORAGE_PROPERTY_QUERY, STORAGE_PROTOCOL_DATA_DESCRIPTOR,
    STORAGE_PROTOCOL_SPECIFIC_DATA, StorageAdapterProtocolSpecificProperty, StorageDeviceProperty,
    StorageDeviceProtocolSpecificProperty, StorageDeviceTemperatureProperty,
};
use windows::core::PCWSTR;

use crate::pdh::{CounterValue, PdhCounter, PdhQuery, wide_null};

const IDLE_COUNTER: &str = r"\PhysicalDisk(*)\% Idle Time";

/// Windows enumerates physical drives as `PhysicalDrive0..PhysicalDriveN`.
/// 32 entries covers every realistic desktop/workstation layout.
const MAX_PHYSICAL_DRIVE: u32 = 31;

/// Hot-plug/rescan cadence. Rescanning is deliberately not tied to the 1 Hz
/// poll loop: each scan opens up to 32 device paths.
const RESCAN_INTERVAL: Duration = Duration::from_secs(45);
/// Retry sooner while no disk is visible at all (common for USB-only systems).
const EMPTY_RESCAN_INTERVAL: Duration = Duration::from_secs(30);
/// `poll()` and `poll_all()` are usually called together for one snapshot.
/// Reuse the result of a just-completed sample instead of collecting PDH
/// counters twice within the same tick (a second immediate collect produces
/// invalid rate deltas).
const SAMPLE_CACHE_TTL: Duration = Duration::from_millis(250);

const TEMP_HEADER_LEN: usize = 24;
const TEMP_ENTRY_LEN: usize = 16;
const TEMP_COUNT_OFFSET: usize = 12;
const TEMP_MIN_C: f32 = -40.0;
const TEMP_MAX_C: f32 = 150.0;

const DEVICE_DESCRIPTOR_HEADER_LEN: usize = 36;

const NVME_HEALTH_LOG_LEN: usize = 512;
/// NVMe SMART/Health log: the composite temperature is the 2nd-3rd byte
/// (1-based), i.e. offset 1, little-endian Kelvin.
const NVME_COMPOSITE_OFFSET: usize = 1;
/// `Temperature Sensor 1..8` start at byte 200 (16-bit little-endian Kelvin).
const NVME_SENSOR_OFFSET: usize = 200;
const NVME_SENSOR_COUNT: usize = 8;

/// `STORAGE_PROPERTY_QUERY` (8 bytes before AdditionalParameters) followed by
/// one `STORAGE_PROTOCOL_SPECIFIC_DATA` (40 bytes).
const PROTOCOL_INPUT_LEN: usize = 8 + size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>();
/// 48-byte `STORAGE_PROTOCOL_DATA_DESCRIPTOR` + 512-byte SMART log + slack.
const PROTOCOL_OUTPUT_LEN: usize = 1024;

#[derive(Clone, Debug)]
struct DriveInfo {
    bus_type: i32,
    name: String,
}

impl DriveInfo {
    fn is_nvme(&self) -> bool {
        self.bus_type == BusTypeNvme.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TempState {
    Unknown,
    Ok,
    Unsupported,
    Error(u32),
}

#[derive(Clone, Debug)]
struct DriveReading {
    index: u32,
    name: String,
    bus_type: Option<i32>,
    is_nvme: bool,
    temp_c: Option<f32>,
    temp_state: TempState,
    missing: bool,
}

impl DriveReading {
    fn new(index: u32, info: Option<&DriveInfo>) -> Self {
        let bus_type = info.map(|info| info.bus_type);
        let is_nvme = info.is_some_and(DriveInfo::is_nvme);
        let name = info
            .map(|info| info.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| format!("PhysicalDrive{index}"));

        Self {
            index,
            name,
            bus_type,
            is_nvme,
            temp_c: None,
            temp_state: TempState::Unknown,
            missing: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TempProbe {
    Value(f32),
    Unsupported,
    Error(u32),
    Missing,
}

struct DriveHandle(HANDLE);

impl Drop for DriveHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open_physical_drive(index: u32) -> Result<DriveHandle, u32> {
    let path = wide_null(&format!(r"\\.\PhysicalDrive{index}"));
    let handle = unsafe {
        CreateFileW(
            PCWSTR::from_raw(path.as_ptr()),
            // Access 0 is enough for storage property IOCTLs and works unelevated.
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    };
    // Capture the error before any other Win32 call can overwrite it.
    let error = unsafe { GetLastError() }.0;
    match handle {
        Ok(handle) if !handle.is_invalid() => Ok(DriveHandle(handle)),
        _ => Err(error),
    }
}

/// Send `IOCTL_STORAGE_QUERY_PROPERTY`. On failure returns the Win32 error code.
fn query_property(
    handle: HANDLE,
    property: STORAGE_PROPERTY_ID,
    buffer: &mut [u8],
) -> Result<u32, u32> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: property,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut returned = 0u32;
    let result = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&query as *const STORAGE_PROPERTY_QUERY).cast::<c_void>()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(buffer.as_mut_ptr().cast::<c_void>()),
            buffer.len() as u32,
            Some(&mut returned),
            None,
        )
    };
    // Capture the error before any other Win32 call can overwrite it.
    let error = unsafe { GetLastError() };
    match result {
        Ok(()) => Ok(returned),
        Err(_) => Err(error.0),
    }
}

fn is_drive_gone(error: u32) -> bool {
    error == ERROR_FILE_NOT_FOUND.0
        || error == ERROR_PATH_NOT_FOUND.0
        || error == ERROR_DEVICE_NOT_CONNECTED.0
}

fn is_unsupported_error(error: u32) -> bool {
    error == ERROR_INVALID_FUNCTION.0
        || error == ERROR_NOT_SUPPORTED.0
        || error == ERROR_IO_DEVICE.0
        || error == ERROR_NOT_READY.0
}

fn read_u16(buffer: &[u8], offset: usize) -> Option<u16> {
    buffer
        .get(offset..offset + 2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_i16(buffer: &[u8], offset: usize) -> Option<i16> {
    buffer
        .get(offset..offset + 2)
        .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(buffer: &[u8], offset: usize) -> Option<u32> {
    buffer
        .get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_i32(buffer: &[u8], offset: usize) -> Option<i32> {
    buffer
        .get(offset..offset + 4)
        .map(|bytes| i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn plausible_temperature(celsius: f32) -> bool {
    celsius.is_finite() && (TEMP_MIN_C..=TEMP_MAX_C).contains(&celsius)
}

fn kelvin_to_celsius(kelvin: u16) -> Option<f32> {
    // 0 K and 0xFFFF are "not reported" sentinels in the NVMe log.
    if kelvin == 0 || kelvin == u16::MAX {
        return None;
    }
    let celsius = kelvin as f32 - 273.15;
    plausible_temperature(celsius).then_some(celsius)
}

/// Parse `STORAGE_TEMPERATURE_DATA_DESCRIPTOR`.
///
/// Header (24 bytes): Version u32, Size u32, CriticalTemperature i16,
/// WarningTemperature i16, InfoCount u16 at offset 12, padding.
/// Each `STORAGE_TEMPERATURE_INFO` is 16 bytes: Index u16, Temperature i16, ...
///
/// Index 0 is the composite sensor and is preferred; otherwise the first
/// plausible entry is used.
fn parse_temperature_descriptor(buffer: &[u8]) -> Option<f32> {
    if buffer.len() < TEMP_HEADER_LEN {
        return None;
    }

    let info_count = read_u16(buffer, TEMP_COUNT_OFFSET)? as usize;
    let available = (buffer.len() - TEMP_HEADER_LEN) / TEMP_ENTRY_LEN;
    let count = info_count.min(available);

    let mut fallback = None;
    for position in 0..count {
        let offset = TEMP_HEADER_LEN + position * TEMP_ENTRY_LEN;
        let index = read_u16(buffer, offset)?;
        let temperature = read_i16(buffer, offset + 2)? as f32;

        if !plausible_temperature(temperature) {
            continue;
        }
        if index == 0 {
            return Some(temperature);
        }
        if fallback.is_none() {
            fallback = Some(temperature);
        }
    }
    fallback
}

/// Parse the 512-byte NVMe SMART/Health Information Log (log page 0x02).
///
/// The composite temperature is the 2nd-3rd byte (offset 1) in Kelvin. When it
/// is missing or implausible, the individual temperature sensors at byte 200
/// are tried in order.
fn parse_nvme_health_temperature(log: &[u8]) -> Option<f32> {
    if log.len() < NVME_COMPOSITE_OFFSET + 2 {
        return None;
    }

    let composite = read_u16(log, NVME_COMPOSITE_OFFSET)?;
    if let Some(celsius) = kelvin_to_celsius(composite) {
        return Some(celsius);
    }

    for sensor in 0..NVME_SENSOR_COUNT {
        let offset = NVME_SENSOR_OFFSET + sensor * 2;
        let Some(kelvin) = read_u16(log, offset) else {
            break;
        };
        if let Some(celsius) = kelvin_to_celsius(kelvin) {
            return Some(celsius);
        }
    }
    None
}

/// Parse the variable-length `STORAGE_DEVICE_DESCRIPTOR` header and its strings.
///
/// `Size` and `RawPropertiesLength` are treated as untrusted: every offset and
/// every string is bounds-checked against the returned buffer.
fn parse_device_descriptor(buffer: &[u8]) -> Option<DriveInfo> {
    if buffer.len() < DEVICE_DESCRIPTOR_HEADER_LEN {
        return None;
    }

    let declared_size = read_u32(buffer, 4)? as usize;
    let limit = if declared_size >= DEVICE_DESCRIPTOR_HEADER_LEN && declared_size <= buffer.len() {
        declared_size
    } else {
        buffer.len()
    };

    let vendor_offset = read_u32(buffer, 12)?;
    let product_offset = read_u32(buffer, 16)?;
    let bus_type = read_i32(buffer, 28)?;
    let raw_length = read_u32(buffer, 32)? as usize;

    // String offsets are relative to the descriptor start and point into the
    // raw-properties area. Prefer RawPropertiesLength when it is usable, but
    // fall back to the declared/buffer size for drivers that leave it at 0.
    let raw_end = DEVICE_DESCRIPTOR_HEADER_LEN
        .saturating_add(raw_length)
        .min(limit);
    let string_limit = if raw_length > 0 && raw_end > DEVICE_DESCRIPTOR_HEADER_LEN {
        raw_end
    } else {
        limit
    };

    let vendor = read_c_string(buffer, vendor_offset, string_limit).unwrap_or_default();
    let product = read_c_string(buffer, product_offset, string_limit).unwrap_or_default();
    let name = format!("{vendor} {product}").trim().to_string();

    Some(DriveInfo { bus_type, name })
}

fn read_c_string(buffer: &[u8], offset: u32, limit: usize) -> Option<String> {
    let offset = offset as usize;
    let limit = limit.min(buffer.len());
    if offset < DEVICE_DESCRIPTOR_HEADER_LEN || offset >= limit {
        return None;
    }

    let tail = &buffer[offset..limit];
    let end = tail
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(tail.len());
    let value = String::from_utf8_lossy(&tail[..end]).trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn probe_drive(index: u32) -> Option<DriveReading> {
    let handle = open_physical_drive(index).ok()?;

    let mut descriptor = [0u8; 1024];
    let info = match query_property(handle.0, StorageDeviceProperty, &mut descriptor) {
        Ok(returned) => {
            let returned = (returned as usize).min(descriptor.len());
            parse_device_descriptor(&descriptor[..returned])
        }
        Err(error) if is_drive_gone(error) || error == ERROR_NOT_READY.0 => return None,
        // The drive exists (the handle opened), we just cannot name it.
        Err(_) => None,
    };

    Some(DriveReading::new(index, info.as_ref()))
}

fn enumerate_disks() -> Vec<DriveReading> {
    (0..=MAX_PHYSICAL_DRIVE).filter_map(probe_drive).collect()
}

/// Query the NVMe SMART/Health log through the protocol-specific storage
/// property. Both the device-level (50) and adapter-level (49) IDs are tried.
fn query_nvme_health_temperature(handle: HANDLE) -> Option<f32> {
    for property in [
        StorageDeviceProtocolSpecificProperty,
        StorageAdapterProtocolSpecificProperty,
    ] {
        let mut output = [0u8; PROTOCOL_OUTPUT_LEN];
        if let Some(temperature) = query_protocol_temperature(handle, property, &mut output) {
            return Some(temperature);
        }
    }
    None
}

fn query_protocol_temperature(
    handle: HANDLE,
    property: STORAGE_PROPERTY_ID,
    output: &mut [u8],
) -> Option<f32> {
    // The input is `STORAGE_PROPERTY_QUERY` immediately followed by
    // `STORAGE_PROTOCOL_SPECIFIC_DATA` at the AdditionalParameters offset (8).
    let mut input = [0u8; PROTOCOL_INPUT_LEN];
    input[0..4].copy_from_slice(&(property.0 as u32).to_le_bytes());
    input[4..8].copy_from_slice(&(PropertyStandardQuery.0 as u32).to_le_bytes());

    let specific = STORAGE_PROTOCOL_SPECIFIC_DATA {
        ProtocolType: ProtocolTypeNvme,
        DataType: NVMeDataTypeLogPage.0 as u32,
        ProtocolDataRequestValue: 0x02, // SMART/Health Information Log
        ProtocolDataRequestSubValue: 0,
        ProtocolDataOffset: size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>() as u32,
        ProtocolDataLength: NVME_HEALTH_LOG_LEN as u32,
        FixedProtocolReturnData: 0,
        ProtocolDataRequestSubValue2: 0,
        ProtocolDataRequestSubValue3: 0,
        ProtocolDataRequestSubValue4: 0,
    };
    // SAFETY: `input` is at least 8 + size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>()
    // bytes and the destination is properly aligned for `u8` writes.
    unsafe {
        std::ptr::copy_nonoverlapping(
            (&specific as *const STORAGE_PROTOCOL_SPECIFIC_DATA).cast::<u8>(),
            input.as_mut_ptr().add(8),
            size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>(),
        );
    }

    let mut returned = 0u32;
    let result = unsafe {
        DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(input.as_ptr().cast::<c_void>()),
            input.len() as u32,
            Some(output.as_mut_ptr().cast::<c_void>()),
            output.len() as u32,
            Some(&mut returned),
            None,
        )
    };
    if result.is_err() {
        return None;
    }

    let returned = (returned as usize).min(output.len());
    parse_protocol_descriptor_temperature(&output[..returned])
}

/// Parse `STORAGE_PROTOCOL_DATA_DESCRIPTOR` and the NVMe log that follows it.
///
/// `ProtocolDataOffset` is documented relative to the nested
/// `STORAGE_PROTOCOL_SPECIFIC_DATA`, but a few drivers report it relative to
/// the descriptor start. Candidate offsets are validated by requiring a
/// plausible NVMe temperature, so either interpretation is accepted.
fn parse_protocol_descriptor_temperature(buffer: &[u8]) -> Option<f32> {
    let descriptor_size = size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>();
    if buffer.len() < descriptor_size {
        return None;
    }

    // The byte buffer is not guaranteed to be aligned for the descriptor.
    let descriptor = unsafe {
        std::ptr::read_unaligned(buffer.as_ptr().cast::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>())
    };
    let specific = descriptor.ProtocolSpecificData;
    let offset = specific.ProtocolDataOffset as usize;

    for start in [8usize.saturating_add(offset), descriptor_size, offset] {
        if start < descriptor_size || start >= buffer.len() {
            continue;
        }
        let available = buffer.len() - start;
        let requested = specific.ProtocolDataLength as usize;
        let length = if requested == 0 {
            available
        } else {
            requested.min(available)
        };
        if length < NVME_COMPOSITE_OFFSET + 2 {
            continue;
        }
        if let Some(temperature) = parse_nvme_health_temperature(&buffer[start..start + length]) {
            return Some(temperature);
        }
    }
    None
}

fn probe_nvme_temperature(index: u32) -> TempProbe {
    let handle = match open_physical_drive(index) {
        Ok(handle) => handle,
        Err(error) if is_drive_gone(error) => return TempProbe::Missing,
        Err(error) => return TempProbe::Error(error),
    };

    let mut buffer = [0u8; 1024];
    let first_error = match query_property(handle.0, StorageDeviceTemperatureProperty, &mut buffer)
    {
        Ok(returned) => {
            let returned = (returned as usize).min(buffer.len());
            if let Some(temperature) = parse_temperature_descriptor(&buffer[..returned]) {
                return TempProbe::Value(temperature);
            }
            // Descriptor present but no usable sensor: try the log page.
            None
        }
        Err(error) if is_drive_gone(error) => return TempProbe::Missing,
        Err(error) => Some(error),
    };

    if let Some(temperature) = query_nvme_health_temperature(handle.0) {
        return TempProbe::Value(temperature);
    }

    match first_error {
        Some(error) if !is_unsupported_error(error) => TempProbe::Error(error),
        _ => TempProbe::Unsupported,
    }
}

/// Map a PDH `PhysicalDisk` instance name to its physical disk index.
///
/// PDH uses names such as `0 C:`, `1 D:`, `0 C: E:` and the special `_Total`.
fn parse_instance_index(name: &str) -> Option<u32> {
    let trimmed = name.trim();
    let digits = trimmed
        .as_bytes()
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }

    let rest = &trimmed[digits..];
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    trimmed[..digits].parse().ok()
}

fn activity_from_idle(idle: f64) -> Option<f32> {
    if !idle.is_finite() {
        return None;
    }
    // NVMe multi-queue counters can report more than 100 % idle; clamp both
    // ends so the widget always gets a valid percentage.
    let busy = (100.0 - idle).clamp(0.0, 100.0);
    busy.is_finite().then_some(busy as f32)
}

/// Split a wildcard PDH sample into per-disk activity and the `_Total` value.
fn group_activity(values: Vec<CounterValue>) -> (BTreeMap<u32, f32>, Option<f32>) {
    let mut by_index = BTreeMap::new();
    let mut total = None;

    for CounterValue { name, value } in values {
        let activity = activity_from_idle(value);
        if name.trim().eq_ignore_ascii_case("_Total") {
            if activity.is_some() {
                total = activity;
            }
            continue;
        }
        let Some(index) = parse_instance_index(&name) else {
            continue;
        };
        if let Some(activity) = activity {
            by_index.insert(index, activity);
        }
    }
    (by_index, total)
}

fn apply_activities(disks: &mut [DiskSample], activities: &BTreeMap<u32, f32>) {
    for disk in disks {
        if let Some(index) = disk.index {
            disk.activity_pct = activities.get(&index).copied();
        }
    }
}

/// Pick the disk shown by the single-disk widget.
///
/// Explicit configuration wins, then the first NVMe with a valid temperature,
/// then the first discovered disk, then an empty `_Total` sample. If the
/// chosen disk has no per-instance PDH activity, the `_Total` counter is used.
fn select_primary(
    disks: &[DiskSample],
    explicit_index: Option<u32>,
    total_activity: Option<f32>,
) -> DiskSample {
    let preferred =
        explicit_index.and_then(|index| disks.iter().find(|disk| disk.index == Some(index)));
    let chosen = preferred
        .or_else(|| {
            disks
                .iter()
                .find(|disk| disk.is_nvme && disk.temp_c.is_some())
        })
        .or_else(|| disks.first());

    match chosen {
        Some(disk) => {
            let mut primary = disk.clone();
            if primary.activity_pct.is_none() {
                primary.activity_pct = total_activity;
            }
            primary
        }
        None => DiskSample {
            name: "_Total".to_string(),
            activity_pct: total_activity,
            ..DiskSample::default()
        },
    }
}

pub(crate) struct DiskSampler {
    query: Option<PdhQuery>,
    idle: Option<PdhCounter>,
    primed: bool,
    warned_pdh: bool,
    drives: Vec<DriveReading>,
    explicit_index: Option<u32>,
    next_rescan: Instant,
    total_activity: Option<f32>,
    cache: Option<(Instant, Vec<DiskSample>)>,
}

impl DiskSampler {
    pub(crate) fn new(disk_index: Option<u32>) -> Self {
        let query = PdhQuery::open();
        let idle = query
            .as_ref()
            .and_then(|query| query.add_counter(IDLE_COUNTER));

        let mut sampler = Self {
            query,
            idle,
            primed: false,
            warned_pdh: false,
            drives: Vec::new(),
            explicit_index: disk_index,
            next_rescan: Instant::now(),
            total_activity: None,
            cache: None,
        };
        sampler.rescan(Instant::now());
        sampler
    }

    /// Primary disk for the single-disk widget.
    pub(crate) fn poll(&mut self) -> DiskSample {
        let disks = self.poll_all();
        select_primary(&disks, self.explicit_index, self.total_activity)
    }

    /// All discovered physical disks, in `PhysicalDriveN` order.
    pub(crate) fn poll_all(&mut self) -> Vec<DiskSample> {
        if let Some((sampled_at, disks)) = &self.cache
            && sampled_at.elapsed() < SAMPLE_CACHE_TTL
        {
            return disks.clone();
        }

        let disks = self.collect_samples();
        self.cache = Some((Instant::now(), disks.clone()));
        disks
    }

    fn collect_samples(&mut self) -> Vec<DiskSample> {
        let now = Instant::now();
        if now >= self.next_rescan {
            self.rescan(now);
        }

        self.refresh_temperatures();
        let activities = self.sample_activity();

        let mut disks: Vec<DiskSample> = self
            .drives
            .iter()
            .map(|drive| DiskSample {
                name: drive.name.clone(),
                index: Some(drive.index),
                is_nvme: drive.is_nvme,
                activity_pct: None,
                temp_c: drive.temp_c,
                temp_source: if drive.temp_c.is_some() {
                    TempSource::Nvme
                } else {
                    TempSource::None
                },
            })
            .collect();

        apply_activities(&mut disks, &activities);
        disks
    }

    fn sample_activity(&mut self) -> BTreeMap<u32, f32> {
        self.total_activity = None;

        if self.query.is_none() || self.idle.is_none() {
            if !self.warned_pdh {
                debug!("PDH PhysicalDisk counters unavailable");
                self.warned_pdh = true;
            }
            return BTreeMap::new();
        }

        let collected = self.query.as_ref().is_some_and(|query| query.collect());
        if !collected {
            return BTreeMap::new();
        }

        // The first collect only primes delta counters; activity is reported
        // from the second poll onwards.
        if !self.primed {
            self.primed = true;
            return BTreeMap::new();
        }

        let Some(values) = self.idle.and_then(|counter| counter.array()) else {
            return BTreeMap::new();
        };
        let (activities, total) = group_activity(values);
        self.total_activity = total;
        activities
    }

    fn refresh_temperatures(&mut self) {
        let watched_index = self.explicit_index;
        let mut disappeared = Vec::new();

        for drive in &mut self.drives {
            if !drive.is_nvme {
                // SATA/USB/RAID: no non-admin temperature source. Probe the
                // configured drive's existence so its removal is noticed
                // without waiting for the periodic rescan.
                if Some(drive.index) == watched_index {
                    match open_physical_drive(drive.index) {
                        Ok(_) => {}
                        Err(error) if is_drive_gone(error) => drive.missing = true,
                        Err(_) => {}
                    }
                }
                if drive.missing {
                    disappeared.push(drive.index);
                    continue;
                }
                drive.temp_c = None;
                update_temp_state(drive, TempState::Unsupported);
                continue;
            }

            let (temperature, state) = match probe_nvme_temperature(drive.index) {
                TempProbe::Value(temperature) => (Some(temperature), TempState::Ok),
                TempProbe::Unsupported => (None, TempState::Unsupported),
                TempProbe::Error(error) => (None, TempState::Error(error)),
                TempProbe::Missing => {
                    drive.missing = true;
                    disappeared.push(drive.index);
                    continue;
                }
            };

            drive.temp_c = temperature;
            update_temp_state(drive, state);
        }

        if !disappeared.is_empty() {
            debug!(indices = ?disappeared, "physical disk disappeared");
            self.drives.retain(|drive| !drive.missing);
            // Re-enumerate on the next sample so a replugged disk is picked up.
            self.next_rescan = Instant::now();
        }
    }

    fn rescan(&mut self, now: Instant) {
        let previous: BTreeMap<u32, TempState> = self
            .drives
            .iter()
            .map(|drive| (drive.index, drive.temp_state))
            .collect();
        let old_indices: Vec<u32> = self.drives.iter().map(|drive| drive.index).collect();

        let mut drives = enumerate_disks();
        // Preserve the logged state of disks that are still present so a
        // periodic rescan does not repeat "no temperature" debug messages.
        for drive in &mut drives {
            if let Some(state) = previous.get(&drive.index) {
                drive.temp_state = *state;
            }
        }

        let indices: Vec<u32> = drives.iter().map(|drive| drive.index).collect();
        if indices != old_indices {
            debug!(indices = ?indices, "disk inventory changed");
        }

        self.drives = drives;
        self.cache = None;
        self.next_rescan = now
            + if self.drives.is_empty() {
                EMPTY_RESCAN_INTERVAL
            } else {
                RESCAN_INTERVAL
            };
    }
}

fn update_temp_state(drive: &mut DriveReading, next: TempState) {
    if drive.temp_state == next {
        return;
    }
    let previous = std::mem::replace(&mut drive.temp_state, next);
    match next {
        TempState::Ok => debug!(
            drive = drive.index,
            bus = ?drive.bus_type,
            temp_c = drive.temp_c,
            "disk temperature available"
        ),
        _ => debug!(
            drive = drive.index,
            bus = ?drive.bus_type,
            ?previous,
            ?next,
            "disk temperature unavailable"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Storage::FileSystem::{BusTypeRAID, BusTypeSata, BusTypeUsb};
    use windows::Win32::System::Ioctl::{
        STORAGE_TEMPERATURE_DATA_DESCRIPTOR, STORAGE_TEMPERATURE_INFO,
    };

    fn temperature_descriptor(entries: &[(u16, i16)]) -> Vec<u8> {
        let mut buffer = vec![0u8; TEMP_HEADER_LEN + entries.len() * TEMP_ENTRY_LEN];
        buffer[0..4].copy_from_slice(&1u32.to_le_bytes());
        let info_count = entries.len() as u16;
        buffer[12..14].copy_from_slice(&info_count.to_le_bytes());
        for (position, (index, temperature)) in entries.iter().enumerate() {
            let offset = TEMP_HEADER_LEN + position * TEMP_ENTRY_LEN;
            buffer[offset..offset + 2].copy_from_slice(&index.to_le_bytes());
            buffer[offset + 2..offset + 4].copy_from_slice(&temperature.to_le_bytes());
        }
        buffer
    }

    fn nvme_log(kelvin: u16) -> Vec<u8> {
        let mut buffer = vec![0u8; NVME_HEALTH_LOG_LEN];
        buffer[NVME_COMPOSITE_OFFSET..NVME_COMPOSITE_OFFSET + 2]
            .copy_from_slice(&kelvin.to_le_bytes());
        buffer
    }

    fn protocol_descriptor(log: &[u8], data_offset: usize, offset_field: u32) -> Vec<u8> {
        let mut buffer = vec![0u8; 600];
        buffer[0..4].copy_from_slice(&1u32.to_le_bytes());
        let descriptor_size = buffer.len() as u32;
        buffer[4..8].copy_from_slice(&descriptor_size.to_le_bytes());
        buffer[8..12].copy_from_slice(&(ProtocolTypeNvme.0 as u32).to_le_bytes());
        buffer[12..16].copy_from_slice(&(NVMeDataTypeLogPage.0 as u32).to_le_bytes());
        buffer[16..20].copy_from_slice(&0x02u32.to_le_bytes());
        buffer[24..28].copy_from_slice(&offset_field.to_le_bytes());
        buffer[28..32].copy_from_slice(&(log.len() as u32).to_le_bytes());
        buffer[data_offset..data_offset + log.len()].copy_from_slice(log);
        buffer
    }

    fn device_descriptor(bus_type: i32, vendor: Option<&str>, product: Option<&str>) -> Vec<u8> {
        let mut buffer = vec![0u8; 128];
        let mut cursor = DEVICE_DESCRIPTOR_HEADER_LEN;
        let mut vendor_offset = 0u32;
        let mut product_offset = 0u32;

        if let Some(vendor) = vendor {
            vendor_offset = cursor as u32;
            buffer[cursor..cursor + vendor.len()].copy_from_slice(vendor.as_bytes());
            cursor += vendor.len() + 1;
        }
        if let Some(product) = product {
            product_offset = cursor as u32;
            buffer[cursor..cursor + product.len()].copy_from_slice(product.as_bytes());
            cursor += product.len() + 1;
        }

        let descriptor_size = buffer.len() as u32;
        buffer[4..8].copy_from_slice(&descriptor_size.to_le_bytes());
        buffer[12..16].copy_from_slice(&vendor_offset.to_le_bytes());
        buffer[16..20].copy_from_slice(&product_offset.to_le_bytes());
        buffer[28..32].copy_from_slice(&bus_type.to_le_bytes());
        buffer[32..36]
            .copy_from_slice(&((cursor - DEVICE_DESCRIPTOR_HEADER_LEN) as u32).to_le_bytes());
        buffer
    }

    fn counter(name: &str, value: f64) -> CounterValue {
        CounterValue {
            name: name.to_string(),
            value,
        }
    }

    fn disk(
        index: u32,
        name: &str,
        is_nvme: bool,
        temp_c: Option<f32>,
        activity_pct: Option<f32>,
    ) -> DiskSample {
        DiskSample {
            name: name.to_string(),
            index: Some(index),
            is_nvme,
            activity_pct,
            temp_c,
            temp_source: if temp_c.is_some() {
                TempSource::Nvme
            } else {
                TempSource::None
            },
        }
    }

    #[test]
    fn storage_layouts_match_windows_bindings() {
        assert_eq!(size_of::<STORAGE_TEMPERATURE_INFO>(), TEMP_ENTRY_LEN);
        assert_eq!(std::mem::offset_of!(STORAGE_TEMPERATURE_INFO, Index), 0);
        assert_eq!(
            std::mem::offset_of!(STORAGE_TEMPERATURE_INFO, Temperature),
            2
        );
        assert_eq!(
            std::mem::offset_of!(STORAGE_TEMPERATURE_DATA_DESCRIPTOR, InfoCount),
            TEMP_COUNT_OFFSET
        );
        assert_eq!(
            std::mem::offset_of!(STORAGE_TEMPERATURE_DATA_DESCRIPTOR, TemperatureInfo),
            TEMP_HEADER_LEN
        );
        assert_eq!(size_of::<STORAGE_PROTOCOL_SPECIFIC_DATA>(), 40);
        assert_eq!(size_of::<STORAGE_PROTOCOL_DATA_DESCRIPTOR>(), 48);
        assert_eq!(PROTOCOL_INPUT_LEN, 48);
    }

    #[test]
    fn composite_sensor_wins() {
        let buffer = temperature_descriptor(&[(1, 55), (0, 41)]);
        assert_eq!(parse_temperature_descriptor(&buffer), Some(41.0));
    }

    #[test]
    fn implausible_composite_falls_back_to_next_sensor() {
        let buffer = temperature_descriptor(&[(0, 1000), (1, 45)]);
        assert_eq!(parse_temperature_descriptor(&buffer), Some(45.0));
    }

    #[test]
    fn first_valid_sensor_is_fallback() {
        let buffer = temperature_descriptor(&[(1, 500), (2, 47), (3, 49)]);
        assert_eq!(parse_temperature_descriptor(&buffer), Some(47.0));
    }

    #[test]
    fn multiple_entries_scan_until_the_composite_sensor() {
        let buffer = temperature_descriptor(&[(1, 55), (2, 60), (0, 41)]);
        assert_eq!(parse_temperature_descriptor(&buffer), Some(41.0));
    }

    #[test]
    fn implausible_temperatures_are_ignored() {
        let buffer = temperature_descriptor(&[(1, -1000), (2, 1000), (3, 0x7FFF)]);
        assert_eq!(parse_temperature_descriptor(&buffer), None);
    }

    #[test]
    fn info_count_is_clamped_to_the_buffer() {
        let mut buffer = temperature_descriptor(&[(0, 41)]);
        // Claim seven entries but leave room for only one.
        buffer[12..14].copy_from_slice(&7u16.to_le_bytes());
        assert_eq!(parse_temperature_descriptor(&buffer), Some(41.0));
    }

    #[test]
    fn truncated_or_empty_descriptor_is_ignored() {
        assert_eq!(parse_temperature_descriptor(&[]), None);
        assert_eq!(parse_temperature_descriptor(&[0u8; 20]), None);
        assert_eq!(parse_temperature_descriptor(&[0u8; TEMP_HEADER_LEN]), None);

        let mut buffer = temperature_descriptor(&[(0, 41)]);
        buffer.truncate(TEMP_HEADER_LEN + 8);
        // InfoCount says 1 but the entry is only half present.
        assert_eq!(parse_temperature_descriptor(&buffer), None);
    }

    #[test]
    fn nvme_composite_kelvin_is_converted() {
        let log = nvme_log(314);
        let temperature = parse_nvme_health_temperature(&log).expect("temperature");
        assert!((temperature - 40.85).abs() < 0.01, "{temperature}");
    }

    #[test]
    fn nvme_sensor_is_used_when_composite_is_missing() {
        let mut log = vec![0u8; NVME_HEALTH_LOG_LEN];
        log[NVME_SENSOR_OFFSET..NVME_SENSOR_OFFSET + 2].copy_from_slice(&310u16.to_le_bytes());

        let temperature = parse_nvme_health_temperature(&log).expect("sensor");
        assert!((temperature - 36.85).abs() < 0.01, "{temperature}");
    }

    #[test]
    fn nvme_implausible_or_short_log_is_ignored() {
        assert_eq!(parse_nvme_health_temperature(&[0u8; 2]), None);
        assert_eq!(parse_nvme_health_temperature(&nvme_log(0)), None);
        assert_eq!(parse_nvme_health_temperature(&nvme_log(1000)), None);
        assert_eq!(parse_nvme_health_temperature(&nvme_log(u16::MAX)), None);
    }

    #[test]
    fn protocol_descriptor_reads_the_log_page() {
        let buffer = protocol_descriptor(&nvme_log(314), 48, 40);
        let temperature = parse_protocol_descriptor_temperature(&buffer).expect("temperature");
        assert!((temperature - 40.85).abs() < 0.01, "{temperature}");
    }

    #[test]
    fn protocol_descriptor_tolerates_offset_relative_to_descriptor_start() {
        let buffer = protocol_descriptor(&nvme_log(314), 56, 48);
        let temperature = parse_protocol_descriptor_temperature(&buffer).expect("temperature");
        assert!((temperature - 40.85).abs() < 0.01, "{temperature}");
    }

    #[test]
    fn protocol_descriptor_rejects_short_buffers() {
        assert_eq!(parse_protocol_descriptor_temperature(&[]), None);
        assert_eq!(parse_protocol_descriptor_temperature(&[0u8; 24]), None);
        assert_eq!(parse_protocol_descriptor_temperature(&[0u8; 48]), None);
    }

    #[test]
    fn device_descriptor_maps_bus_types() {
        for (bus_type, is_nvme) in [
            (BusTypeNvme.0, true),
            (BusTypeSata.0, false),
            (BusTypeUsb.0, false),
            (BusTypeRAID.0, false),
        ] {
            let buffer = device_descriptor(bus_type, Some("Apacer"), Some("AS2280P4U 1TB"));
            let info = parse_device_descriptor(&buffer).expect("descriptor");
            assert_eq!(info.bus_type, bus_type);
            assert_eq!(info.is_nvme(), is_nvme);
            assert_eq!(info.name, "Apacer AS2280P4U 1TB");
        }
    }

    #[test]
    fn device_descriptor_bad_offsets_are_bounded() {
        let mut buffer = device_descriptor(BusTypeNvme.0, Some("Vendor"), Some("Product"));
        buffer[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        let beyond = buffer.len() as u32 + 100;
        buffer[16..20].copy_from_slice(&beyond.to_le_bytes());

        let info = parse_device_descriptor(&buffer).expect("descriptor");
        assert_eq!(info.name, "");
    }

    #[test]
    fn device_descriptor_accepts_non_nul_terminated_strings() {
        let mut buffer = vec![0u8; 48];
        buffer[4..8].copy_from_slice(&48u32.to_le_bytes());
        buffer[16..20].copy_from_slice(&40u32.to_le_bytes());
        buffer[28..32].copy_from_slice(&BusTypeNvme.0.to_le_bytes());
        buffer[32..36].copy_from_slice(&12u32.to_le_bytes());
        buffer[40..48].copy_from_slice(b"NO-NUL!!");

        let info = parse_device_descriptor(&buffer).expect("descriptor");
        assert_eq!(info.name, "NO-NUL!!");
    }

    #[test]
    fn short_device_descriptor_is_rejected() {
        assert!(parse_device_descriptor(&[0u8; 12]).is_none());
        assert!(parse_device_descriptor(&[0u8; 35]).is_none());
    }

    #[test]
    fn pdh_instance_names_map_to_disk_indices() {
        assert_eq!(parse_instance_index("0 C:"), Some(0));
        assert_eq!(parse_instance_index("1 D:"), Some(1));
        assert_eq!(parse_instance_index("7 E: F:"), Some(7));
        assert_eq!(parse_instance_index("12"), Some(12));
        assert_eq!(parse_instance_index(" 3 E:"), Some(3));
        assert_eq!(parse_instance_index("_Total"), None);
        assert_eq!(parse_instance_index(""), None);
        assert_eq!(parse_instance_index("C:"), None);
        assert_eq!(parse_instance_index("0C:"), None);
        assert_eq!(parse_instance_index("x 1"), None);
    }

    #[test]
    fn activity_is_clamped() {
        assert_eq!(activity_from_idle(100.0), Some(0.0));
        assert_eq!(activity_from_idle(0.0), Some(100.0));
        assert_eq!(activity_from_idle(120.0), Some(0.0));
        assert_eq!(activity_from_idle(-20.0), Some(100.0));
        assert_eq!(activity_from_idle(f64::NAN), None);

        let value = activity_from_idle(98.74).expect("value");
        assert!((value - 1.26).abs() < 0.0001, "{value}");
    }

    #[test]
    fn wildcard_instances_are_merged_per_disk() {
        let (by_index, total) = group_activity(vec![
            counter("0 C:", 90.0),
            counter("1 D:", 40.0),
            counter("2 E:", 120.0),
            counter("_Total", 80.0),
            counter("C:", 10.0),
            counter("0C:", 10.0),
        ]);

        assert_eq!(by_index.len(), 3);
        assert_eq!(by_index.get(&0), Some(&10.0));
        assert_eq!(by_index.get(&1), Some(&60.0));
        assert_eq!(by_index.get(&2), Some(&0.0));
        assert_eq!(total, Some(20.0));
    }

    #[test]
    fn explicit_disk_is_primary() {
        let disks = vec![
            disk(0, "nvme", true, Some(40.0), Some(5.0)),
            disk(1, "sata", false, None, Some(7.0)),
        ];

        let primary = select_primary(&disks, Some(1), Some(50.0));
        assert_eq!(primary.index, Some(1));
        assert_eq!(primary.activity_pct, Some(7.0));
        assert_eq!(primary.temp_c, None);
    }

    #[test]
    fn missing_explicit_disk_falls_back_to_nvme_with_temperature() {
        let disks = vec![
            disk(0, "nvme-no-temp", true, None, Some(1.0)),
            disk(1, "sata", false, None, Some(2.0)),
            disk(2, "nvme-temp", true, Some(41.0), Some(3.0)),
        ];

        let primary = select_primary(&disks, Some(9), None);
        assert_eq!(primary.index, Some(2));
    }

    #[test]
    fn first_disk_is_primary_when_no_nvme_temperature() {
        let disks = vec![
            disk(3, "sata", false, None, Some(1.0)),
            disk(4, "usb", false, None, None),
        ];

        let primary = select_primary(&disks, None, Some(42.0));
        assert_eq!(primary.index, Some(3));
        assert_eq!(primary.activity_pct, Some(1.0));
    }

    #[test]
    fn primary_activity_falls_back_to_total() {
        let disks = vec![disk(0, "nvme", true, Some(40.0), None)];
        let primary = select_primary(&disks, None, Some(12.5));
        assert_eq!(primary.activity_pct, Some(12.5));
    }

    #[test]
    fn empty_inventory_yields_total_sample() {
        let primary = select_primary(&[], None, Some(3.0));
        assert_eq!(primary.name, "_Total");
        assert_eq!(primary.index, None);
        assert_eq!(primary.activity_pct, Some(3.0));
        assert!(!primary.is_nvme);
        assert_eq!(primary.temp_source, TempSource::None);
    }

    #[test]
    fn per_disk_activities_are_applied_by_index() {
        let mut disks = vec![
            disk(0, "a", false, None, None),
            disk(1, "b", false, None, None),
            disk(2, "c", false, None, None),
        ];
        let mut activities = BTreeMap::new();
        activities.insert(0, 10.0);
        activities.insert(2, 90.0);

        apply_activities(&mut disks, &activities);
        assert_eq!(disks[0].activity_pct, Some(10.0));
        assert_eq!(disks[1].activity_pct, None);
        assert_eq!(disks[2].activity_pct, Some(90.0));
    }
}
