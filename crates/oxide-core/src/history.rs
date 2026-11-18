//! Fixed-capacity, allocation-free ring buffers for 60-second sparkline history.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::metrics::Snapshot;

/// Default number of 1 Hz samples stored per metric series (60 seconds).
pub const HISTORY_LEN: usize = 60;

/// Fixed-capacity ring buffer with zero heap allocations after construction.
#[derive(Clone, Debug)]
pub struct RingBuffer<T: Copy + Default, const N: usize = HISTORY_LEN> {
    data: [T; N],
    head: usize,
    len: usize,
}

impl<T: Copy + Default, const N: usize> Default for RingBuffer<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Default, const N: usize> RingBuffer<T, N> {
    pub fn new() -> Self {
        Self {
            data: [T::default(); N],
            head: 0,
            len: 0,
        }
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    pub fn push(&mut self, value: T) {
        if N == 0 {
            return;
        }
        self.data[self.head] = value;
        self.head = (self.head + 1) % N;
        if self.len < N {
            self.len += 1;
        }
    }

    pub fn latest(&self) -> Option<T> {
        if self.len == 0 || N == 0 {
            return None;
        }
        let idx = (self.head + N - 1) % N;
        Some(self.data[idx])
    }

    /// Iterate oldest-to-newest.
    pub fn iter(&self) -> RingIter<'_, T, N> {
        let start = if self.len == N { self.head } else { 0 };
        RingIter {
            buf: self,
            pos: start,
            remaining: self.len,
        }
    }

    /// Copy the last `max_points` samples (oldest-to-newest) into `out`.
    pub fn copy_tail(&self, max_points: usize, out: &mut Vec<T>) {
        out.clear();
        if self.len == 0 || max_points == 0 {
            return;
        }
        let take = self.len.min(max_points);
        let skip = self.len - take;
        out.extend(self.iter().skip(skip));
    }
}

pub struct RingIter<'a, T: Copy + Default, const N: usize> {
    buf: &'a RingBuffer<T, N>,
    pos: usize,
    remaining: usize,
}

impl<'a, T: Copy + Default, const N: usize> Iterator for RingIter<'a, T, N> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let val = self.buf.data[self.pos];
        self.pos = (self.pos + 1) % N;
        self.remaining -= 1;
        Some(val)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<const N: usize> RingBuffer<f32, N> {
    /// Maximum finite sample value in the buffer, clamped to at least `floor`.
    pub fn max_or(&self, floor: f32) -> f32 {
        let mut peak = floor;
        for v in self.iter() {
            if v.is_finite() && v > peak {
                peak = v;
            }
        }
        peak
    }
}

/// Identifies a metric history series for sparklines and Flyout graphs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SeriesKey {
    NetUp,
    NetDown,
    NetTotal,
    CpuUsage,
    CpuFreq,
    CpuTemp,
    MemoryUsage,
    GpuUsage { index: usize },
    GpuTemp { index: usize },
    DiskActivity { index: usize },
    DiskTemp { index: usize },
    External { name: String },
}

/// Stores 60-second ring buffers for all primary and indexed metrics.
#[derive(Clone, Debug, Default)]
pub struct HistoryStore {
    pub net_up: RingBuffer<f32, HISTORY_LEN>,
    pub net_down: RingBuffer<f32, HISTORY_LEN>,
    pub net_total: RingBuffer<f32, HISTORY_LEN>,
    pub cpu_usage: RingBuffer<f32, HISTORY_LEN>,
    pub cpu_freq: RingBuffer<f32, HISTORY_LEN>,
    pub cpu_temp: RingBuffer<f32, HISTORY_LEN>,
    pub memory_usage: RingBuffer<f32, HISTORY_LEN>,
    pub gpu_usage: Vec<RingBuffer<f32, HISTORY_LEN>>,
    pub gpu_temp: Vec<RingBuffer<f32, HISTORY_LEN>>,
    pub disk_activity: Vec<RingBuffer<f32, HISTORY_LEN>>,
    pub disk_temp: Vec<RingBuffer<f32, HISTORY_LEN>>,
    pub external: BTreeMap<String, RingBuffer<f32, HISTORY_LEN>>,
    /// Smoothly decaying peak for network auto-scaling (bytes/sec).
    net_peak_bps: f32,
}

impl HistoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one 1 Hz snapshot into all series.
    pub fn record_snapshot(&mut self, snapshot: &Snapshot) {
        let up = snapshot.network.up_bps.max(0.0) as f32;
        let down = snapshot.network.down_bps.max(0.0) as f32;
        let total = up + down;
        self.net_up.push(up);
        self.net_down.push(down);
        self.net_total.push(total);

        // Smooth decay for network auto-scaling (min floor: 125 KB/s = 1 Mb/s).
        let instant_peak = up.max(down);
        self.net_peak_bps = (self.net_peak_bps * 0.96).max(instant_peak).max(125_000.0);

        self.cpu_usage
            .push(snapshot.cpu.usage_pct.clamp(0.0, 100.0));
        if let Some(freq) = snapshot.cpu.freq_mhz.filter(|v| v.is_finite()) {
            self.cpu_freq.push(freq.max(0.0));
        }
        if let Some(temp) = snapshot.cpu.temp_c.filter(|v| v.is_finite()) {
            self.cpu_temp.push(temp);
        }
        self.memory_usage
            .push(snapshot.memory.usage_pct.clamp(0.0, 100.0));

        // Indexed GPUs (or fallback to primary GPU at slot 0 if `gpus` is empty).
        let gpu_count = snapshot.gpus.len().max(1);
        if self.gpu_usage.len() < gpu_count {
            self.gpu_usage.resize_with(gpu_count, RingBuffer::new);
            self.gpu_temp.resize_with(gpu_count, RingBuffer::new);
        }
        if snapshot.gpus.is_empty() {
            if let Some(u) = snapshot.gpu.usage_pct.filter(|v| v.is_finite()) {
                self.gpu_usage[0].push(u.clamp(0.0, 100.0));
            }
            if let Some(t) = snapshot.gpu.temp_c.filter(|v| v.is_finite()) {
                self.gpu_temp[0].push(t);
            }
        } else {
            for (idx, gpu) in snapshot.gpus.iter().enumerate() {
                if let Some(u) = gpu.usage_pct.filter(|v| v.is_finite()) {
                    self.gpu_usage[idx].push(u.clamp(0.0, 100.0));
                }
                if let Some(t) = gpu.temp_c.filter(|v| v.is_finite()) {
                    self.gpu_temp[idx].push(t);
                }
            }
        }

        // Indexed disks (or fallback to primary disk at slot 0 if `disks` is empty).
        let disk_count = snapshot.disks.len().max(1);
        if self.disk_activity.len() < disk_count {
            self.disk_activity.resize_with(disk_count, RingBuffer::new);
            self.disk_temp.resize_with(disk_count, RingBuffer::new);
        }
        if snapshot.disks.is_empty() {
            if let Some(a) = snapshot.disk.activity_pct.filter(|v| v.is_finite()) {
                self.disk_activity[0].push(a.clamp(0.0, 100.0));
            }
            if let Some(t) = snapshot.disk.temp_c.filter(|v| v.is_finite()) {
                self.disk_temp[0].push(t);
            }
        } else {
            for (idx, disk) in snapshot.disks.iter().enumerate() {
                if let Some(a) = disk.activity_pct.filter(|v| v.is_finite()) {
                    self.disk_activity[idx].push(a.clamp(0.0, 100.0));
                }
                if let Some(t) = disk.temp_c.filter(|v| v.is_finite()) {
                    self.disk_temp[idx].push(t);
                }
            }
        }
    }

    pub fn record_external(&mut self, name: &str, value: f32) {
        if !value.is_finite() {
            return;
        }
        if let Some(buf) = self.external.get_mut(name) {
            buf.push(value);
        } else {
            let mut buf = RingBuffer::new();
            buf.push(value);
            self.external.insert(name.to_string(), buf);
        }
    }

    pub fn series(&self, key: &SeriesKey) -> Option<&RingBuffer<f32, HISTORY_LEN>> {
        match key {
            SeriesKey::NetUp => Some(&self.net_up),
            SeriesKey::NetDown => Some(&self.net_down),
            SeriesKey::NetTotal => Some(&self.net_total),
            SeriesKey::CpuUsage => Some(&self.cpu_usage),
            SeriesKey::CpuFreq => Some(&self.cpu_freq),
            SeriesKey::CpuTemp => Some(&self.cpu_temp),
            SeriesKey::MemoryUsage => Some(&self.memory_usage),
            SeriesKey::GpuUsage { index } => self.gpu_usage.get(*index),
            SeriesKey::GpuTemp { index } => self.gpu_temp.get(*index),
            SeriesKey::DiskActivity { index } => self.disk_activity.get(*index),
            SeriesKey::DiskTemp { index } => self.disk_temp.get(*index),
            SeriesKey::External { name } => self.external.get(name),
        }
    }

    pub fn net_auto_scale_peak_bps(&self) -> f32 {
        self.net_peak_bps.max(125_000.0)
    }

    /// Export a compact serializable slice for IPC to `oxide-settings.exe`.
    pub fn to_dto(&self) -> HistorySliceDto {
        let collect = |buf: &RingBuffer<f32, HISTORY_LEN>| buf.iter().collect::<Vec<f32>>();
        HistorySliceDto {
            net_up: collect(&self.net_up),
            net_down: collect(&self.net_down),
            cpu_usage: collect(&self.cpu_usage),
            memory_usage: collect(&self.memory_usage),
            gpu_usage: self.gpu_usage.iter().map(collect).collect(),
            disk_activity: self.disk_activity.iter().map(collect).collect(),
            net_peak_bps: self.net_auto_scale_peak_bps(),
        }
    }
}

/// Serializable history payload sent over IPC to `oxide-settings.exe` for Live Preview.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistorySliceDto {
    pub net_up: Vec<f32>,
    pub net_down: Vec<f32>,
    pub cpu_usage: Vec<f32>,
    pub memory_usage: Vec<f32>,
    pub gpu_usage: Vec<Vec<f32>>,
    pub disk_activity: Vec<Vec<f32>>,
    pub net_peak_bps: f32,
}

impl HistoryStore {
    /// Hydrate a `HistoryStore` from an IPC `HistorySliceDto` (used inside `oxide-settings.exe`).
    pub fn from_dto(dto: &HistorySliceDto) -> Self {
        let mut store = Self::new();
        for &v in &dto.net_up {
            store.net_up.push(v);
        }
        for &v in &dto.net_down {
            store.net_down.push(v);
        }
        for (&u, &d) in dto.net_up.iter().zip(dto.net_down.iter()) {
            store.net_total.push(u + d);
        }
        for &v in &dto.cpu_usage {
            store.cpu_usage.push(v);
        }
        for &v in &dto.memory_usage {
            store.memory_usage.push(v);
        }
        for series in &dto.gpu_usage {
            let mut rb = RingBuffer::new();
            for &v in series {
                rb.push(v);
            }
            store.gpu_usage.push(rb);
        }
        for series in &dto.disk_activity {
            let mut rb = RingBuffer::new();
            for &v in series {
                rb.push(v);
            }
            store.disk_activity.push(rb);
        }
        store.net_peak_bps = dto.net_peak_bps.max(125_000.0);
        store
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_wraps_oldest_to_newest() {
        let mut rb = RingBuffer::<u32, 4>::new();
        for v in 1..=6 {
            rb.push(v);
        }
        assert_eq!(rb.len(), 4);
        assert_eq!(rb.latest(), Some(6));
        let items: Vec<u32> = rb.iter().collect();
        assert_eq!(items, vec![3, 4, 5, 6]);

        let mut tail = Vec::new();
        rb.copy_tail(2, &mut tail);
        assert_eq!(tail, vec![5, 6]);
    }

    #[test]
    fn history_store_records_and_roundtrips_dto() {
        let mut store = HistoryStore::new();
        let mut snap = Snapshot::default();
        snap.cpu.usage_pct = 42.5;
        snap.network.down_bps = 500_000.0;
        snap.memory.usage_pct = 68.0;
        store.record_snapshot(&snap);

        assert_eq!(store.cpu_usage.latest(), Some(42.5));
        assert!(store.net_auto_scale_peak_bps() >= 500_000.0);

        let dto = store.to_dto();
        let hydrated = HistoryStore::from_dto(&dto);
        assert_eq!(hydrated.cpu_usage.latest(), Some(42.5));
        assert_eq!(hydrated.memory_usage.latest(), Some(68.0));
    }
}
