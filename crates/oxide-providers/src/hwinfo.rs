//! Read-only HWiNFO shared-memory provider.
//!
//! HWiNFO (7.0+, with *Shared Memory Support* enabled) publishes its sensors in the
//! pagefile-backed section `Global\HWiNFO_SENS_SM2`. Reading it needs no elevation: the
//! section is opened with `FILE_MAP_READ` only.
//!
//! The binary layout is taken from the public `hwisenssm2.h` specification, **not** from
//! HWiNFO sources. Strides and counts are always read from the header; nothing is
//! hard-coded except the minimum sizes and field offsets. The `Value` field sits at
//! `0x11C`, which is not 8-byte aligned, so it is read with `ptr::read_unaligned`.
//!
//! Everything in the mapping is treated as untrusted input: a hostile or broken publisher
//! must produce an error, never a panic or an out-of-bounds read.

use std::ffi::c_void;
use std::mem::size_of;
use std::time::{Duration, Instant};

use thiserror::Error;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Memory::{
    FILE_MAP_READ, MEMORY_BASIC_INFORMATION, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, UnmapViewOfFile, VirtualQuery,
};
use windows::core::PCWSTR;

use crate::ProviderReadings;
use crate::heuristics::TemperaturePicker;

const MAPPING_NAME: &str = "Global\\HWiNFO_SENS_SM2";

/// Little-endian DWORD from the bytes `"HWiS"`.
const SIGNATURE: u32 = 0x5369_5748;
const HEADER_LEN: usize = 48;

// Header offsets (`HWiNFO_SENSORS_SHARED_MEM2`, packed, 48 bytes).
const OFF_VERSION: usize = 0x04;
const OFF_REVISION: usize = 0x08;
const OFF_POLL_TIME: usize = 0x0C;
const OFF_SENSOR_SECTION: usize = 0x14;
const OFF_SENSOR_STRIDE: usize = 0x18;
const OFF_SENSOR_COUNT: usize = 0x1C;
const OFF_READING_SECTION: usize = 0x20;
const OFF_READING_STRIDE: usize = 0x24;
const OFF_READING_COUNT: usize = 0x28;

// Sensor element fields.
const SENSOR_NAME_ORIG: usize = 0x08;
const SENSOR_NAME_USER: usize = 0x88;
const SENSOR_NAME_UTF8: usize = 0x108;
const SENSOR_NAME_LEN: usize = 128;
const SENSOR_UTF8_END: usize = SENSOR_NAME_UTF8 + SENSOR_NAME_LEN; // 0x188, v2 layout

// Reading element fields.
const READING_LABEL_ORIG: usize = 0x0C;
const READING_LABEL_USER: usize = 0x8C;
const READING_LABEL_UTF8: usize = 0x13C;
const READING_LABEL_LEN: usize = 128;
const READING_UNIT: usize = 0x10C;
const READING_UNIT_UTF8: usize = 0x1BC;
const READING_UNIT_LEN: usize = 16;
const READING_VALUE: usize = 0x11C;
const READING_UTF8_LABEL_END: usize = READING_LABEL_UTF8 + READING_LABEL_LEN; // 0x1BC
const READING_UTF8_UNIT_END: usize = READING_UNIT_UTF8 + READING_UNIT_LEN; // 0x1CC

const MIN_SENSOR_STRIDE: u32 = 264;
const MIN_READING_STRIDE: u32 = 316;
const MAX_STRIDE: u32 = 4096;
const MAX_SENSORS: u32 = 4096;
const MAX_READINGS: u32 = 65_536;

/// `poll_time` older than this means HWiNFO stopped updating.
const MAX_AGE_SECS: i64 = 15;
/// Tolerated clock skew for `poll_time` in the future.
const MAX_FUTURE_SECS: i64 = 5;
/// Reopen periodically so HWiNFO can recreate the section after a sensor-set change.
const MAX_MAPPING_AGE: Duration = Duration::from_secs(300);

const READING_TYPE_TEMP: u32 = 1;

#[derive(Clone, Debug, Error)]
pub(crate) enum HwinfoError {
    #[error("HWiNFO shared memory not found")]
    NotRunning,
    #[error("access denied to HWiNFO shared memory")]
    AccessDenied,
    #[error("HWiNFO shared memory I/O error: {0}")]
    Io(String),
    #[error("malformed HWiNFO shared memory: {0}")]
    Malformed(&'static str),
    #[error("HWiNFO shared memory layout changed")]
    Rebuild,
    #[error("HWiNFO shared memory is stale")]
    Stale,
}

/// Parsed, validated header. All offsets are relative to the start of the mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    version: u32,
    revision: u32,
    poll_time: i64,
    sensor_offset: u32,
    sensor_stride: u32,
    sensor_count: u32,
    reading_offset: u32,
    reading_stride: u32,
    reading_count: u32,
}

impl Header {
    fn parse(data: &[u8]) -> Result<Self, HwinfoError> {
        if data.len() < HEADER_LEN {
            return Err(HwinfoError::Malformed("header truncated"));
        }

        let signature = read_u32(data, 0x00).ok_or(HwinfoError::Malformed("header truncated"))?;
        if signature != SIGNATURE {
            return Err(HwinfoError::Malformed("bad signature"));
        }

        let version = read_u32(data, OFF_VERSION).ok_or(HwinfoError::Malformed("no version"))?;
        if version != 1 && version != 2 {
            return Err(HwinfoError::Malformed("unsupported version"));
        }

        let revision = read_u32(data, OFF_REVISION).ok_or(HwinfoError::Malformed("no revision"))?;
        if revision > 1 {
            return Err(HwinfoError::Malformed("unsupported revision"));
        }

        let poll_time =
            read_i64(data, OFF_POLL_TIME).ok_or(HwinfoError::Malformed("no poll time"))?;
        let sensor_offset =
            read_u32(data, OFF_SENSOR_SECTION).ok_or(HwinfoError::Malformed("no sensor offset"))?;
        let sensor_stride =
            read_u32(data, OFF_SENSOR_STRIDE).ok_or(HwinfoError::Malformed("no sensor stride"))?;
        let sensor_count =
            read_u32(data, OFF_SENSOR_COUNT).ok_or(HwinfoError::Malformed("no sensor count"))?;
        let reading_offset = read_u32(data, OFF_READING_SECTION)
            .ok_or(HwinfoError::Malformed("no reading offset"))?;
        let reading_stride = read_u32(data, OFF_READING_STRIDE)
            .ok_or(HwinfoError::Malformed("no reading stride"))?;
        let reading_count =
            read_u32(data, OFF_READING_COUNT).ok_or(HwinfoError::Malformed("no reading count"))?;

        if sensor_count > MAX_SENSORS {
            return Err(HwinfoError::Malformed("too many sensors"));
        }
        if reading_count > MAX_READINGS {
            return Err(HwinfoError::Malformed("too many readings"));
        }
        if !(MIN_SENSOR_STRIDE..=MAX_STRIDE).contains(&sensor_stride) {
            return Err(HwinfoError::Malformed("sensor element stride out of range"));
        }
        if !(MIN_READING_STRIDE..=MAX_STRIDE).contains(&reading_stride) {
            return Err(HwinfoError::Malformed(
                "reading element stride out of range",
            ));
        }
        if sensor_count == 0 && reading_count > 0 {
            return Err(HwinfoError::Malformed("readings without sensors"));
        }

        let mapped = data.len() as u64;
        check_region(
            sensor_offset,
            sensor_stride,
            sensor_count,
            mapped,
            "sensor section out of bounds",
        )?;
        check_region(
            reading_offset,
            reading_stride,
            reading_count,
            mapped,
            "reading section out of bounds",
        )?;

        Ok(Self {
            version,
            revision,
            poll_time,
            sensor_offset,
            sensor_stride,
            sensor_count,
            reading_offset,
            reading_stride,
            reading_count,
        })
    }
}

fn check_region(
    offset: u32,
    stride: u32,
    count: u32,
    mapped: u64,
    what: &'static str,
) -> Result<(), HwinfoError> {
    let length = (stride as u64)
        .checked_mul(count as u64)
        .ok_or(HwinfoError::Malformed(what))?;
    let end = (offset as u64)
        .checked_add(length)
        .ok_or(HwinfoError::Malformed(what))?;
    if end > mapped {
        return Err(HwinfoError::Malformed(what));
    }
    Ok(())
}

/// Parse and validate one snapshot from raw mapped bytes. `now_unix` is the current Unix
/// time in seconds; it is a parameter so the freshness rule is testable.
#[cfg(test)]
pub(crate) fn snapshot(data: &[u8], now_unix: i64) -> Result<ProviderReadings, HwinfoError> {
    let header = Header::parse(data)?;
    read_readings(data, &header, now_unix)
}

fn read_readings(
    data: &[u8],
    header: &Header,
    now_unix: i64,
) -> Result<ProviderReadings, HwinfoError> {
    check_freshness(header.poll_time, now_unix)?;

    if header.reading_count == 0 {
        // Normal during a republish, but not usable data.
        return Err(HwinfoError::Malformed("no readings"));
    }

    let mut sensor_names = Vec::with_capacity(header.sensor_count as usize);
    for index in 0..header.sensor_count as usize {
        let base = header.sensor_offset as usize + index * header.sensor_stride as usize;
        sensor_names.push(read_sensor_name(data, header, base)?);
    }

    let mut picker = TemperaturePicker::new();
    for index in 0..header.reading_count as usize {
        let base = header.reading_offset as usize + index * header.reading_stride as usize;

        let Some(reading_type) = read_u32(data, base) else {
            continue;
        };
        let Some(sensor_index) = read_u32(data, base + 0x04) else {
            continue;
        };
        let Some(value) = read_f64(data, base + READING_VALUE) else {
            continue;
        };
        let Some(sensor_name) = sensor_names.get(sensor_index as usize) else {
            // Stale index during a republish; ignore the row.
            continue;
        };

        let unit = read_reading_unit(data, header, base)?;
        // `tReading == 1` is TEMP; the unit check keeps publishers that leave the type
        // field at zero usable without trusting unrelated sensors.
        if reading_type != READING_TYPE_TEMP && !unit_looks_celsius(&unit) {
            continue;
        }

        let label = read_reading_label(data, header, base)?;
        picker.offer(sensor_name, &label, value as f32);
    }

    if picker.is_empty() {
        return Err(HwinfoError::Malformed("no temperature readings"));
    }
    Ok(picker.finish())
}

fn check_freshness(poll_time: i64, now_unix: i64) -> Result<(), HwinfoError> {
    if poll_time <= 0 {
        return Err(HwinfoError::Stale);
    }
    if poll_time > now_unix.saturating_add(MAX_FUTURE_SECS) {
        return Err(HwinfoError::Stale);
    }
    if now_unix.saturating_sub(poll_time) > MAX_AGE_SECS {
        return Err(HwinfoError::Stale);
    }
    Ok(())
}

fn read_sensor_name(data: &[u8], header: &Header, base: usize) -> Result<String, HwinfoError> {
    let orig = read_text(data, base + SENSOR_NAME_ORIG, SENSOR_NAME_LEN)?;
    let user = read_text(data, base + SENSOR_NAME_USER, SENSOR_NAME_LEN)?;
    let utf8 = if header.version >= 2 && header.sensor_stride as usize >= SENSOR_UTF8_END {
        read_text(data, base + SENSOR_NAME_UTF8, SENSOR_NAME_LEN)?
    } else {
        String::new()
    };
    // v2 UTF-8 user name > v1 user name > original name.
    Ok([utf8, user, orig]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or_default())
}

fn read_reading_label(data: &[u8], header: &Header, base: usize) -> Result<String, HwinfoError> {
    let orig = read_text(data, base + READING_LABEL_ORIG, READING_LABEL_LEN)?;
    let user = read_text(data, base + READING_LABEL_USER, READING_LABEL_LEN)?;
    let utf8 = if header.version >= 2 && header.reading_stride as usize >= READING_UTF8_LABEL_END {
        read_text(data, base + READING_LABEL_UTF8, READING_LABEL_LEN)?
    } else {
        String::new()
    };
    Ok([utf8, user, orig]
        .into_iter()
        .find(|label| !label.is_empty())
        .unwrap_or_default())
}

fn read_reading_unit(data: &[u8], header: &Header, base: usize) -> Result<String, HwinfoError> {
    let ansi = read_text(data, base + READING_UNIT, READING_UNIT_LEN)?;
    if header.version >= 2 && header.reading_stride as usize >= READING_UTF8_UNIT_END {
        let utf8 = read_text(data, base + READING_UNIT_UTF8, READING_UNIT_LEN)?;
        if !utf8.is_empty() {
            return Ok(utf8);
        }
    }
    Ok(ansi)
}

fn unit_looks_celsius(unit: &str) -> bool {
    unit.contains('C') || unit.contains('c') || unit.contains('°')
}

/// Reads a NUL-terminated UTF-8-ish field. A field without a terminator is malformed;
/// non-UTF-8 bytes are replaced (lossy) rather than rejected, because HWiNFO writes ANSI
/// strings and a publisher may use a legacy code page.
fn read_text(data: &[u8], offset: usize, len: usize) -> Result<String, HwinfoError> {
    let end = offset
        .checked_add(len)
        .ok_or(HwinfoError::Malformed("string out of bounds"))?;
    let field = data
        .get(offset..end)
        .ok_or(HwinfoError::Malformed("string out of bounds"))?;
    let terminator = field
        .iter()
        .position(|&byte| byte == 0)
        .ok_or(HwinfoError::Malformed("unterminated string"))?;
    let bytes = &field[..terminator];
    if bytes.is_empty() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(bytes).trim().to_string())
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let field = field(data, offset, 4)?;
    Some(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
}

fn read_i64(data: &[u8], offset: usize) -> Option<i64> {
    let field = field(data, offset, 8)?;
    let bytes: [u8; 8] = field.try_into().ok()?;
    Some(i64::from_le_bytes(bytes))
}

/// `Value` lives at `0x11C`, i.e. 4 mod 8, inside a packed struct. A reference would be
/// UB (and faults on strict-alignment targets), so the bytes are read unaligned.
fn read_f64(data: &[u8], offset: usize) -> Option<f64> {
    let field = field(data, offset, 8)?;
    // SAFETY: `field` is exactly 8 bytes inside `data`; `read_unaligned` has no alignment
    // requirement.
    Some(unsafe { std::ptr::read_unaligned(field.as_ptr() as *const f64) })
}

fn field(data: &[u8], offset: usize, len: usize) -> Option<&[u8]> {
    let end = offset.checked_add(len)?;
    data.get(offset..end)
}

/// Owns the section handle and the mapped view.
struct Mapping {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    handle: HANDLE,
    size: usize,
}

impl Mapping {
    fn open() -> Result<Self, HwinfoError> {
        let wide: Vec<u16> = MAPPING_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call.
        let handle = unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, PCWSTR(wide.as_ptr())) }
            .map_err(|err| map_open_error(&err))?;

        // SAFETY: the handle is valid and opened for read access; mapping the whole
        // section is the documented usage.
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ, 0, 0, 0) };
        if view.Value.is_null() {
            // SAFETY: `handle` was returned by OpenFileMappingW and is not owned elsewhere.
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Err(HwinfoError::Io("MapViewOfFile failed".to_string()));
        }

        // SAFETY: `view` is a valid mapped range.
        let size = unsafe { mapped_size(view.Value) };
        if size < HEADER_LEN {
            // SAFETY: both resources belong to this partially constructed Mapping.
            unsafe {
                let _ = UnmapViewOfFile(view);
                let _ = CloseHandle(handle);
            }
            return Err(HwinfoError::Malformed("mapping too small"));
        }

        Ok(Self { view, handle, size })
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: `view`/`size` come from a successful MapViewOfFile and stay valid until
        // `Drop` unmaps them.
        unsafe { std::slice::from_raw_parts(self.view.Value as *const u8, self.size) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: both values were produced by the matching open calls and are released
        // exactly once, here.
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Ask the kernel for the size of the mapped region starting at `base`.
unsafe fn mapped_size(base: *mut c_void) -> usize {
    let mut info = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: `base` points into a live mapping; `info` is a valid out-param.
    let written = unsafe {
        VirtualQuery(
            Some(base as *const c_void),
            &mut info,
            size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written == 0 { 0 } else { info.RegionSize }
}

fn map_open_error(err: &windows::core::Error) -> HwinfoError {
    // HRESULT_FROM_WIN32 for the two states that are not really errors for us.
    match err.code().0 as u32 {
        0x8007_0002 => HwinfoError::NotRunning, // ERROR_FILE_NOT_FOUND
        0x8007_0005 => HwinfoError::AccessDenied, // ERROR_ACCESS_DENIED
        _ => HwinfoError::Io(err.to_string()),
    }
}

/// Stateful reader: owns the mapping, reopens it when the layout changes, becomes stale,
/// or after [`MAX_MAPPING_AGE`] so a running HWiNFO can recreate the section.
pub(crate) struct HwinfoReader {
    mapping: Option<Mapping>,
    opened_at: Option<Instant>,
    counts: Option<(u32, u32, u32, u32)>,
}

impl HwinfoReader {
    pub(crate) fn new() -> Self {
        Self {
            mapping: None,
            opened_at: None,
            counts: None,
        }
    }

    pub(crate) fn poll(&mut self, now_unix: i64) -> Result<ProviderReadings, HwinfoError> {
        if self
            .opened_at
            .is_some_and(|opened| opened.elapsed() > MAX_MAPPING_AGE)
        {
            self.reset();
        }

        let mut refreshed = false;
        loop {
            if self.mapping.is_none() {
                self.mapping = Some(Mapping::open()?);
                self.opened_at = Some(Instant::now());
                self.counts = None;
            }

            match self.read_once(now_unix) {
                Ok(readings) => return Ok(readings),
                Err(HwinfoError::Rebuild) if !refreshed => {
                    // HWiNFO republished with a different sensor set: reopen and retry once.
                    refreshed = true;
                    self.reset();
                }
                Err(err) => {
                    self.reset();
                    return Err(err);
                }
            }
        }
    }

    fn read_once(&mut self, now_unix: i64) -> Result<ProviderReadings, HwinfoError> {
        let header = {
            let mapping = self.mapping.as_ref().ok_or(HwinfoError::NotRunning)?;
            Header::parse(mapping.bytes())?
        };

        let counts = (
            header.sensor_count,
            header.reading_count,
            header.reading_stride,
            header.version,
        );
        if self.counts.is_some_and(|previous| previous != counts) {
            return Err(HwinfoError::Rebuild);
        }
        self.counts = Some(counts);

        let mapping = self.mapping.as_ref().ok_or(HwinfoError::NotRunning)?;
        read_readings(mapping.bytes(), &header, now_unix)
    }

    fn reset(&mut self) {
        self.mapping = None;
        self.opened_at = None;
        self.counts = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    struct Row<'a> {
        sensor: u32,
        kind: u32,
        label: &'a str,
        unit: &'a str,
        value: f64,
    }

    fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
        buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_i64(buf: &mut [u8], offset: usize, value: i64) {
        buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn put_f64(buf: &mut [u8], offset: usize, value: f64) {
        buf[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn put_text(buf: &mut [u8], offset: usize, len: usize, text: &str) {
        let bytes = text.as_bytes();
        let n = bytes.len().min(len - 1);
        buf[offset..offset + n].copy_from_slice(&bytes[..n]);
    }

    fn build(version: u32, poll_time: i64, sensors: &[&str], rows: &[Row<'_>]) -> Vec<u8> {
        let (sensor_stride, reading_stride): (u32, u32) =
            if version >= 2 { (392, 460) } else { (264, 316) };
        let sensor_offset = HEADER_LEN as u32;
        let reading_offset = sensor_offset + sensor_stride * sensors.len() as u32;
        let total = reading_offset as usize + reading_stride as usize * rows.len();
        let mut buf = vec![0u8; total];

        put_u32(&mut buf, 0x00, SIGNATURE);
        put_u32(&mut buf, OFF_VERSION, version);
        put_u32(&mut buf, OFF_REVISION, 1);
        put_i64(&mut buf, OFF_POLL_TIME, poll_time);
        put_u32(&mut buf, OFF_SENSOR_SECTION, sensor_offset);
        put_u32(&mut buf, OFF_SENSOR_STRIDE, sensor_stride);
        put_u32(&mut buf, OFF_SENSOR_COUNT, sensors.len() as u32);
        put_u32(&mut buf, OFF_READING_SECTION, reading_offset);
        put_u32(&mut buf, OFF_READING_STRIDE, reading_stride);
        put_u32(&mut buf, OFF_READING_COUNT, rows.len() as u32);
        put_u32(&mut buf, 0x2C, 1000);

        for (index, name) in sensors.iter().enumerate() {
            let base = sensor_offset as usize + index * sensor_stride as usize;
            put_text(&mut buf, base + SENSOR_NAME_ORIG, SENSOR_NAME_LEN, name);
            put_text(&mut buf, base + SENSOR_NAME_USER, SENSOR_NAME_LEN, name);
        }

        for (index, row) in rows.iter().enumerate() {
            let base = reading_offset as usize + index * reading_stride as usize;
            put_u32(&mut buf, base, row.kind);
            put_u32(&mut buf, base + 0x04, row.sensor);
            put_u32(&mut buf, base + 0x08, index as u32);
            put_text(
                &mut buf,
                base + READING_LABEL_ORIG,
                READING_LABEL_LEN,
                row.label,
            );
            put_text(
                &mut buf,
                base + READING_LABEL_USER,
                READING_LABEL_LEN,
                row.label,
            );
            put_text(&mut buf, base + READING_UNIT, READING_UNIT_LEN, row.unit);
            put_f64(&mut buf, base + READING_VALUE, row.value);
        }

        buf
    }

    fn standard_rows() -> Vec<Row<'static>> {
        vec![
            Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "CPU Package",
                unit: "°C",
                value: 55.5,
            },
            Row {
                sensor: 1,
                kind: READING_TYPE_TEMP,
                label: "GPU Core",
                unit: "°C",
                value: 47.0,
            },
            Row {
                sensor: 2,
                kind: READING_TYPE_TEMP,
                label: "Temperature",
                unit: "°C",
                value: 41.0,
            },
            Row {
                sensor: 3,
                kind: READING_TYPE_TEMP,
                label: "System",
                unit: "°C",
                value: 35.5,
            },
        ]
    }

    fn standard_sensors() -> Vec<&'static str> {
        vec![
            "AMD Ryzen 7 7800X3D",
            "NVIDIA GeForce GTX 1650",
            "Samsung SSD 990 PRO 1TB",
            "ASUS ROG STRIX B650E-F GAMING",
        ]
    }

    #[test]
    fn parses_v1_snapshot() {
        let data = build(1, NOW, &standard_sensors(), &standard_rows());
        let readings = snapshot(&data, NOW).expect("valid v1 snapshot");
        assert_eq!(readings.cpu_temp_c, Some(55.5));
        assert_eq!(readings.gpu_temp_c, Some(47.0));
        assert_eq!(readings.disk_temp_c, Some(41.0));
        assert_eq!(readings.mainboard_temp_c, Some(35.5));
    }

    #[test]
    fn parses_v2_snapshot() {
        let data = build(2, NOW, &standard_sensors(), &standard_rows());
        let readings = snapshot(&data, NOW).expect("valid v2 snapshot");
        assert_eq!(readings.cpu_temp_c, Some(55.5));
        assert_eq!(readings.mainboard_temp_c, Some(35.5));
    }

    #[test]
    fn prefers_utf8_v2_label() {
        let mut data = build(
            2,
            NOW,
            &["AMD Ryzen 7 7800X3D"],
            &[Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "Renamed by user",
                unit: "°C",
                value: 61.0,
            }],
        );
        // v2 layout: UTF-8 label at 0x13C wins over the ANSI fields.
        put_text(
            &mut data,
            HEADER_LEN + 392 + READING_LABEL_UTF8,
            READING_LABEL_LEN,
            "CPU Package",
        );
        let readings = snapshot(&data, NOW).expect("valid v2 snapshot");
        assert_eq!(readings.cpu_temp_c, Some(61.0));
    }

    #[test]
    fn picks_cpu_package_over_other_cpu_labels() {
        let rows = [
            Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "Core Max",
                unit: "°C",
                value: 70.0,
            },
            Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "CPU (Tctl/Tdie)",
                unit: "°C",
                value: 50.0,
            },
            Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "CPU Package",
                unit: "°C",
                value: 60.0,
            },
        ];
        let data = build(2, NOW, &["AMD Ryzen 7 7800X3D"], &rows);
        let readings = snapshot(&data, NOW).expect("valid snapshot");
        assert_eq!(readings.cpu_temp_c, Some(60.0));
    }

    #[test]
    fn rejects_bad_signature() {
        let mut data = build(1, NOW, &["CPU"], &standard_rows());
        data[0] = b'X';
        assert!(matches!(
            snapshot(&data, NOW),
            Err(HwinfoError::Malformed("bad signature"))
        ));
    }

    #[test]
    fn rejects_unknown_version_and_revision() {
        let mut version = build(1, NOW, &["CPU"], &standard_rows());
        put_u32(&mut version, OFF_VERSION, 3);
        assert!(matches!(
            snapshot(&version, NOW),
            Err(HwinfoError::Malformed("unsupported version"))
        ));

        let mut revision = build(1, NOW, &["CPU"], &standard_rows());
        put_u32(&mut revision, OFF_REVISION, 7);
        assert!(matches!(
            snapshot(&revision, NOW),
            Err(HwinfoError::Malformed("unsupported revision"))
        ));
    }

    #[test]
    fn rejects_short_header_and_short_strides() {
        let data = build(1, NOW, &["CPU"], &standard_rows());
        assert!(snapshot(&data[..HEADER_LEN - 1], NOW).is_err());

        let mut short_sensor = data.clone();
        put_u32(&mut short_sensor, OFF_SENSOR_STRIDE, 200);
        assert!(snapshot(&short_sensor, NOW).is_err());

        let mut short_reading = data;
        put_u32(&mut short_reading, OFF_READING_STRIDE, 200);
        assert!(snapshot(&short_reading, NOW).is_err());
    }

    #[test]
    fn rejects_oversized_counts_and_offsets() {
        let data = build(1, NOW, &["CPU"], &standard_rows());

        let mut counts = data.clone();
        put_u32(&mut counts, OFF_SENSOR_COUNT, 5000);
        assert!(snapshot(&counts, NOW).is_err());

        let mut readings = data.clone();
        put_u32(&mut readings, OFF_READING_COUNT, 70_000);
        assert!(snapshot(&readings, NOW).is_err());

        let mut offset = data.clone();
        let length = offset.len() as u32;
        put_u32(&mut offset, OFF_READING_SECTION, length);
        assert!(snapshot(&offset, NOW).is_err());

        let mut overflow = data;
        put_u32(&mut overflow, OFF_READING_SECTION, u32::MAX);
        put_u32(&mut overflow, OFF_READING_COUNT, 4096);
        assert!(snapshot(&overflow, NOW).is_err());
    }

    #[test]
    fn rejects_stale_and_future_poll_time() {
        let stale = build(1, NOW - 60, &["CPU"], &standard_rows());
        assert!(matches!(snapshot(&stale, NOW), Err(HwinfoError::Stale)));

        let zero = build(1, 0, &["CPU"], &standard_rows());
        assert!(matches!(snapshot(&zero, NOW), Err(HwinfoError::Stale)));

        let future = build(1, NOW + 60, &["CPU"], &standard_rows());
        assert!(matches!(snapshot(&future, NOW), Err(HwinfoError::Stale)));
    }

    #[test]
    fn rejects_unterminated_strings() {
        let mut data = build(
            1,
            NOW,
            &["CPU"],
            &[Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "CPU Package",
                unit: "°C",
                value: 50.0,
            }],
        );
        let base = HEADER_LEN + 264;
        data[base + READING_LABEL_ORIG..base + READING_LABEL_ORIG + READING_LABEL_LEN].fill(b'A');
        assert!(snapshot(&data, NOW).is_err());
    }

    #[test]
    fn skips_non_finite_and_out_of_range_values() {
        let rows = [
            Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "CPU Package",
                unit: "°C",
                value: f64::NAN,
            },
            Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "CPU Core",
                unit: "°C",
                value: 300.0,
            },
            Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "CPU Total",
                unit: "°C",
                value: f64::INFINITY,
            },
            Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "CPU Package",
                unit: "°C",
                value: 0.0,
            },
        ];
        let data = build(1, NOW, &["CPU"], &rows);
        assert!(matches!(
            snapshot(&data, NOW),
            Err(HwinfoError::Malformed("no temperature readings"))
        ));
    }

    #[test]
    fn zero_readings_is_not_ready_not_a_crash() {
        let data = build(1, NOW, &["CPU"], &[]);
        assert!(matches!(
            snapshot(&data, NOW),
            Err(HwinfoError::Malformed("no readings"))
        ));
    }

    #[test]
    fn ignores_non_temperature_readings() {
        let rows = [
            Row {
                sensor: 0,
                kind: 3, // FAN
                label: "CPU Fan",
                unit: "RPM",
                value: 1200.0,
            },
            Row {
                sensor: 0,
                kind: READING_TYPE_TEMP,
                label: "CPU Package",
                unit: "°C",
                value: 44.0,
            },
        ];
        let data = build(1, NOW, &["CPU"], &rows);
        assert_eq!(snapshot(&data, NOW).unwrap().cpu_temp_c, Some(44.0));
    }

    #[test]
    fn arbitrary_bytes_do_not_panic() {
        let mut seed = 0x1234_5678u32;
        for len in [0usize, 1, 8, 47, 48, 49, 64, 512, 4096] {
            let mut buf = vec![0u8; len];
            for byte in &mut buf {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *byte = (seed >> 16) as u8;
            }
            let _ = snapshot(&buf, NOW);
        }
    }
}
