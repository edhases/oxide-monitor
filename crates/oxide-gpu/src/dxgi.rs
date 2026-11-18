//! Vendor-neutral adapter enumeration through DXGI.
//!
//! DXGI is present on every Windows 10/11 installation, including RDP sessions and
//! VMs, and requires neither elevation nor a vendor SDK. `CreateDXGIFactory1` /
//! `EnumAdapters1` gives the adapter description and — crucially — the adapter
//! LUID that ties the adapter to the PDH `GPU Engine` counters.
//!
//! Some environments (Server Core without the graphics stack, broken display
//! drivers) fail to create a factory or enumerate any adapter. That is a normal
//! degradation: the sampler falls back to vendor data or to an empty list.

use std::sync::atomic::{AtomicBool, Ordering};

use oxide_core::GpuVendor;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_DESC1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1,
};

/// PCI vendor IDs used by the adapters Windows actually exposes.
const VENDOR_ID_NVIDIA: u32 = 0x10DE;
const VENDOR_ID_AMD: u32 = 0x1002;
const VENDOR_ID_INTEL: u32 = 0x8086;
const VENDOR_ID_MICROSOFT: u32 = 0x1414;
const VENDOR_ID_QUALCOMM: u32 = 0x5143;

/// Defensive cap; real machines expose a handful of adapters.
const MAX_ADAPTERS: u32 = 64;

/// One DXGI adapter in the vendor-neutral form the sampler needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdapterInfo {
    pub name: String,
    pub vendor: GpuVendor,
    /// Combined `LUID`: `high << 32 | low`. The same value appears in PDH instance
    /// names, which is how load is attached to the adapter.
    pub luid: u64,
    pub dedicated_vram_bytes: Option<u64>,
    /// DXGI software adapter (Microsoft Basic Render, RDP, some VMs).
    pub software: bool,
}

/// Map a PCI vendor ID from `DXGI_ADAPTER_DESC1::VendorId`.
pub(crate) fn vendor_from_pci_id(vendor_id: u32) -> GpuVendor {
    match vendor_id {
        VENDOR_ID_NVIDIA => GpuVendor::Nvidia,
        VENDOR_ID_AMD => GpuVendor::Amd,
        VENDOR_ID_INTEL => GpuVendor::Intel,
        VENDOR_ID_MICROSOFT => GpuVendor::Microsoft,
        VENDOR_ID_QUALCOMM => GpuVendor::Qualcomm,
        _ => GpuVendor::Unknown,
    }
}

/// Combine a Windows `LUID` (`HighPart` is a signed `LONG`) into the unsigned
/// 64-bit value used by PDH instance names.
pub(crate) fn luid_to_u64(high_part: i32, low_part: u32) -> u64 {
    ((high_part as u32 as u64) << 32) | low_part as u64
}

/// Decode the fixed-size UTF-16 description. DXGI NUL-terminates, but some
/// drivers leave garbage after the terminator, so stop at the first NUL.
fn description_to_string(description: &[u16; 128]) -> String {
    let len = description
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(description.len());
    let name = String::from_utf16_lossy(&description[..len]);
    let name = name.trim();
    if name.is_empty() {
        "GPU".to_owned()
    } else {
        name.to_owned()
    }
}

/// Enumerate all DXGI adapters, deduplicated by LUID and in DXGI order.
pub(crate) fn enumerate() -> Vec<AdapterInfo> {
    static WARNED: AtomicBool = AtomicBool::new(false);

    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(factory) => factory,
        Err(err) => {
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::debug!(
                    error = %err,
                    "DXGI unavailable; falling back to vendor-only GPU data"
                );
            }
            return Vec::new();
        }
    };

    let mut adapters: Vec<AdapterInfo> = Vec::new();
    for index in 0..MAX_ADAPTERS {
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            // DXGI_ERROR_NOT_FOUND is the normal end of enumeration; any other
            // error means the adapter list is unusable from here on.
            Err(_) => break,
        };

        let desc: DXGI_ADAPTER_DESC1 = match unsafe { adapter.GetDesc1() } {
            Ok(desc) => desc,
            Err(_) => continue,
        };

        let luid = luid_to_u64(desc.AdapterLuid.HighPart, desc.AdapterLuid.LowPart);
        if adapters.iter().any(|existing| existing.luid == luid) {
            continue;
        }

        let dedicated_vram_bytes =
            (desc.DedicatedVideoMemory > 0).then_some(desc.DedicatedVideoMemory as u64);

        adapters.push(AdapterInfo {
            name: description_to_string(&desc.Description),
            vendor: vendor_from_pci_id(desc.VendorId),
            luid,
            dedicated_vram_bytes,
            software: desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
        });
    }

    adapters
}

#[cfg(test)]
mod tests {
    use super::{AdapterInfo, description_to_string, luid_to_u64, vendor_from_pci_id};
    use oxide_core::GpuVendor;

    #[test]
    fn maps_pci_vendor_ids() {
        assert_eq!(vendor_from_pci_id(0x10DE), GpuVendor::Nvidia);
        assert_eq!(vendor_from_pci_id(0x1002), GpuVendor::Amd);
        assert_eq!(vendor_from_pci_id(0x8086), GpuVendor::Intel);
        assert_eq!(vendor_from_pci_id(0x1414), GpuVendor::Microsoft);
        assert_eq!(vendor_from_pci_id(0x5143), GpuVendor::Qualcomm);
        assert_eq!(vendor_from_pci_id(0x0000), GpuVendor::Unknown);
        assert_eq!(vendor_from_pci_id(0x5333), GpuVendor::Unknown);
    }

    #[test]
    fn combines_luid_parts_without_sign_extension() {
        assert_eq!(luid_to_u64(0, 0x0001_04FB), 0x0001_04FB);
        assert_eq!(luid_to_u64(1, 2), 0x0000_0001_0000_0002);
        // HighPart is a signed LONG; -1 must become the high 32 bits of all ones.
        assert_eq!(luid_to_u64(-1, 0xFFFF_FFFF), u64::MAX);
        assert_eq!(luid_to_u64(i32::MIN, 0), 0x8000_0000_0000_0000);
    }

    #[test]
    fn decodes_adapter_description_until_the_first_nul() {
        let mut desc = [0u16; 128];
        for (index, unit) in "NVIDIA GeForce GTX 1650".encode_utf16().enumerate() {
            desc[index] = unit;
        }
        desc[24] = b'!' as u16; // garbage after the terminator must be ignored
        assert_eq!(description_to_string(&desc), "NVIDIA GeForce GTX 1650");

        let empty = [0u16; 128];
        assert_eq!(description_to_string(&empty), "GPU");
    }

    #[test]
    fn adapter_info_is_plain_data_and_comparable() {
        let adapter = AdapterInfo {
            name: "AMD Radeon(TM) Graphics".to_owned(),
            vendor: GpuVendor::Amd,
            luid: 0x1_04FB,
            dedicated_vram_bytes: None,
            software: false,
        };
        assert_eq!(adapter.clone(), adapter);
    }
}
