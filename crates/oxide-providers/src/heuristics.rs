//! Shared heuristics for mapping vendor sensor names to Oxide temperature slots.
//!
//! Both external providers expose flat lists of labelled sensors, and there is no stable
//! machine-readable identifier for "the CPU temperature". The rules below are deliberately
//! conservative: it is better to leave a slot empty (`—` in the UI) than to show a
//! plausible-looking but wrong number.
//!
//! The preference order mirrors `ARCHITECTURE.md` §5.4:
//! CPU: `CPU Package` → `Tctl`/`Tdie` → `Core Max` → generic `CPU`.
//! `Distance to TjMax` is never treated as a temperature (it does not contain any of the
//! labels above, so it simply does not match).

use crate::ProviderReadings;

/// Plausible PC temperature range in °C; used to reject sentinels such as `FLT_MAX`.
const MIN_C: f32 = -40.0;
const MAX_C: f32 = 150.0;

/// Per-category match strength; `0` means "does not match".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Scores {
    pub cpu: u8,
    pub gpu: u8,
    pub disk: u8,
    pub mainboard: u8,
}

pub(crate) fn classify(device: &str, label: &str) -> Scores {
    let device = device.to_ascii_lowercase();
    let label = label.to_ascii_lowercase();

    let cpu = if label.contains("cpu package") {
        100
    } else if label.contains("tctl") {
        96
    } else if label.contains("tdie") {
        92
    } else if label.contains("core max") {
        88
    } else if label.contains("cpu") {
        72
    } else if looks_like_cpu(&device)
        && (label.contains("package") || label.contains("total") || label.contains("temperature"))
    {
        58
    } else {
        0
    };

    let gpu = if label.contains("gpu core") {
        100
    } else if label.contains("gpu") {
        84
    } else if looks_like_gpu(&device)
        && (label.contains("temperature") || label.contains("core") || label.contains("hot spot"))
    {
        66
    } else {
        0
    };

    let disk_device = looks_like_disk(&device);
    let disk = if disk_device && (label.contains("temperature") || label.contains("composite")) {
        92
    } else if (label == "temperature" || label == "composite")
        && !looks_like_cpu(&device)
        && !looks_like_gpu(&device)
        && !looks_like_board(&device)
    {
        // Plain "Temperature" outside CPU/GPU/board sensors is usually a drive.
        42
    } else {
        0
    };

    let mainboard = if label.contains("motherboard") || label.contains("mainboard") {
        100
    } else if label.contains("vrm") {
        92
    } else if label.contains("system") {
        76
    } else if looks_like_board(&device) && label.contains("temperature") {
        68
    } else {
        0
    };

    Scores {
        cpu,
        gpu,
        disk,
        mainboard,
    }
}

pub(crate) fn is_plausible(value: f32) -> bool {
    // 0 °C is a very common "sensor not present" placeholder in HWiNFO and LHM.
    value.is_finite() && value != 0.0 && (MIN_C..=MAX_C).contains(&value)
}

fn looks_like_cpu(device: &str) -> bool {
    [
        "cpu",
        "ryzen",
        "threadripper",
        "xeon",
        "epyc",
        "core i",
        "intel",
    ]
    .iter()
    .any(|needle| device.contains(needle))
}

fn looks_like_gpu(device: &str) -> bool {
    [
        "gpu",
        "nvidia",
        "geforce",
        "radeon",
        "intel graphics",
        "uhd graphics",
        "iris",
        "arc a",
        "dgpu",
    ]
    .iter()
    .any(|needle| device.contains(needle))
}

fn looks_like_disk(device: &str) -> bool {
    [
        "ssd",
        "hdd",
        "nvme",
        "drive",
        "disk",
        "/storage",
        "hard disk",
    ]
    .iter()
    .any(|needle| device.contains(needle))
}

fn looks_like_board(device: &str) -> bool {
    ["motherboard", "mainboard", "vrm", "/lpc"]
        .iter()
        .any(|needle| device.contains(needle))
}

#[derive(Clone, Copy, Debug)]
struct Candidate {
    score: u8,
    value: f32,
}

/// Collects labelled readings and keeps the best candidate per slot.
#[derive(Debug, Default)]
pub(crate) struct TemperaturePicker {
    cpu: Option<Candidate>,
    gpu: Option<Candidate>,
    disk: Option<Candidate>,
    mainboard: Option<Candidate>,
}

impl TemperaturePicker {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Offer a reading with the owning device name (HWiNFO sensor name or LHM hardware id)
    /// and the reading label. Values outside a plausible range are ignored.
    pub(crate) fn offer(&mut self, device: &str, label: &str, value: f32) {
        if !is_plausible(value) {
            return;
        }
        let scores = classify(device, label);
        offer_slot(&mut self.cpu, scores.cpu, value);
        offer_slot(&mut self.gpu, scores.gpu, value);
        offer_slot(&mut self.disk, scores.disk, value);
        offer_slot(&mut self.mainboard, scores.mainboard, value);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.cpu.is_none() && self.gpu.is_none() && self.disk.is_none() && self.mainboard.is_none()
    }

    pub(crate) fn finish(self) -> ProviderReadings {
        ProviderReadings {
            cpu_temp_c: self.cpu.map(|c| c.value),
            gpu_temp_c: self.gpu.map(|c| c.value),
            disk_temp_c: self.disk.map(|c| c.value),
            mainboard_temp_c: self.mainboard.map(|c| c.value),
        }
    }
}

fn offer_slot(slot: &mut Option<Candidate>, score: u8, value: f32) {
    if score == 0 {
        return;
    }
    match slot {
        Some(current) if current.score >= score => {}
        _ => *slot = Some(Candidate { score, value }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_cpu_package_over_core_max() {
        let mut picker = TemperaturePicker::new();
        picker.offer("AMD Ryzen 7 7800X3D", "Core Max", 70.0);
        picker.offer("AMD Ryzen 7 7800X3D", "CPU Package", 60.0);
        assert_eq!(picker.finish().cpu_temp_c, Some(60.0));
    }

    #[test]
    fn distance_to_tjmax_is_not_a_temperature() {
        let mut picker = TemperaturePicker::new();
        picker.offer("Intel Core i7", "Core Max", 70.0);
        picker.offer("Intel Core i7", "Distance to TjMax", 30.0);
        assert_eq!(picker.finish().cpu_temp_c, Some(70.0));
    }

    #[test]
    fn plausible_range_and_zero() {
        assert!(!is_plausible(0.0));
        assert!(!is_plausible(f32::NAN));
        assert!(!is_plausible(151.0));
        assert!(!is_plausible(-41.0));
        assert!(is_plausible(41.0));
        assert!(is_plausible(-40.0));
        assert!(is_plausible(150.0));
    }
}
