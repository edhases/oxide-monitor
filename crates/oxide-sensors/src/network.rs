//! Network throughput from the IP Helper table (`GetIfTable2`/`GetIfEntry2`).
//!
//! The sampler selects one adapter and reports its 64-bit octet counters as
//! deltas over the real elapsed time. Adapter selection combines:
//!
//! * an explicit `config.general.adapter` alias/description (case-insensitive;
//!   selected even while disconnected, and may explicitly be a VPN),
//! * the IPv4 default route from `GetBestInterface`,
//! * a preference for real hardware over virtual switches/overlays,
//! * keyword exclusion for common virtual adapters (Hyper-V, WSL, Tailscale,
//!   VPN/TAP/TUN, VMware, VirtualBox, loopback, WAN Miniport, Bluetooth).
//!
//! Ethernet, Wi-Fi and USB/WWAN modems (`IF_TYPE_WWANPP`/`IF_TYPE_WWANPP2`) are
//! eligible for automatic selection. VPN/tunnel adapters are never picked
//! implicitly, because their traffic is already counted on the physical link.
//! Counters reset when an adapter is disabled/re-enabled or the machine
//! resumes; a decrease is treated as such a reset and one tick is skipped
//! instead of reporting a bogus spike.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use oxide_core::{Config, NetworkSample};
use tracing::{debug, warn};
use windows::Win32::Foundation::NO_ERROR;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetBestInterface, GetIfEntry2, GetIfTable2, IF_TYPE_ETHERNET_CSMACD,
    IF_TYPE_IEEE80211, IF_TYPE_PPP, IF_TYPE_PROP_VIRTUAL, IF_TYPE_SOFTWARE_LOOPBACK,
    IF_TYPE_TUNNEL, IF_TYPE_WWANPP, IF_TYPE_WWANPP2, MIB_IF_ROW2, MIB_IF_TABLE2,
};
use windows::Win32::NetworkManagement::Ndis::{
    IfOperStatusUp, MediaConnectStateConnected, NET_LUID_LH,
};

/// Enumerating the interface table is not free; cap a corrupted entry count.
const MAX_INTERFACES: usize = 4096;

/// Ignore sub-50 ms intervals so a tight poll loop cannot divide by ~zero.
const MIN_INTERVAL: Duration = Duration::from_millis(50);
/// Retry enumeration at this cadence while no adapter is available.
const RESELECT_INTERVAL: Duration = Duration::from_secs(2);
/// Reject a rate above 1 TB/s: counters are 64-bit but a corrupt delta or a
/// bogus interval must never reach the UI as a "huge" value.
const MAX_RATE_BYTES_PER_SEC: f64 = 1.0e12;

/// `MIB_IF_ROW2.InterfaceAndOperStatusFlags.HardwareInterface`.
const FLAG_HARDWARE: u8 = 0x01;
/// `MIB_IF_ROW2.InterfaceAndOperStatusFlags.FilterInterface`.
const FLAG_FILTER: u8 = 0x02;

/// Destination used only to ask Windows which interface carries the default
/// route. IPv4 8.8.8.8, in network byte order.
const ROUTE_PROBE: u32 = u32::from_be_bytes([8, 8, 8, 8]);

/// Name fragments used by common virtual adapters. `MIB_IF_ROW2`'s hardware
/// flag is not reliable for Hyper-V/vEthernet, so names are the last resort.
const VIRTUAL_NAME_HINTS: &[&str] = &[
    "vethernet",
    "hyper-v",
    "virtual",
    "wsl",
    "tailscale",
    "vpn",
    "wireguard",
    "openvpn",
    "zerotier",
    "tap-",
    "tun-",
    "vmware",
    "virtualbox",
    "loopback",
    "wan miniport",
    "bluetooth",
    "default switch",
];

/// Overlay/VPN/tunnel names. These are never auto-selected, not even as a last
/// resort, because that would double-count traffic or hide the physical link.
const VPN_NAME_HINTS: &[&str] = &[
    "tailscale",
    "wireguard",
    "openvpn",
    "zerotier",
    "vpn",
    "tap-",
    "tun-",
    "wsl",
];

#[derive(Clone, Debug, PartialEq)]
struct AdapterInfo {
    luid: u64,
    index: u32,
    alias: String,
    description: String,
    link_speed: u64,
    if_type: u32,
    up: bool,
    connected: bool,
    hardware: bool,
    filter: bool,
    has_mac: bool,
}

impl AdapterInfo {
    fn name(&self) -> String {
        if self.alias.is_empty() {
            self.description.clone()
        } else {
            self.alias.clone()
        }
    }

    fn lower_name(&self) -> String {
        format!("{} {}", self.alias, self.description).to_lowercase()
    }

    fn is_modem(&self) -> bool {
        matches!(self.if_type, IF_TYPE_WWANPP | IF_TYPE_WWANPP2)
    }

    fn has_virtual_name(&self) -> bool {
        let name = self.lower_name();
        VIRTUAL_NAME_HINTS.iter().any(|hint| name.contains(hint))
    }

    fn has_vpn_name(&self) -> bool {
        let name = self.lower_name();
        VPN_NAME_HINTS.iter().any(|hint| name.contains(hint))
    }

    /// Physical, connected Ethernet/Wi-Fi/WWAN links are preferred for
    /// auto-selection. VPN/TAP/loopback/filter interfaces are deliberately not
    /// in this set, which avoids counting the same traffic twice.
    fn is_auto_candidate(&self) -> bool {
        self.up
            && self.connected
            && !self.filter
            && !self.has_vpn_name()
            && matches!(
                self.if_type,
                IF_TYPE_ETHERNET_CSMACD | IF_TYPE_IEEE80211 | IF_TYPE_WWANPP | IF_TYPE_WWANPP2
            )
    }

    fn is_virtual(&self) -> bool {
        self.filter
            || self.has_virtual_name()
            || (!self.is_modem() && !self.has_mac)
            || matches!(
                self.if_type,
                IF_TYPE_TUNNEL | IF_TYPE_SOFTWARE_LOOPBACK | IF_TYPE_PPP | IF_TYPE_PROP_VIRTUAL
            )
    }
}

/// Convert an octet-counter delta into bytes per second. Returns 0 for a
/// counter reset, a non-positive/non-finite interval, or an implausibly large
/// rate instead of propagating a negative or huge value.
fn counter_rate(current: u64, previous: u64, elapsed_secs: f64) -> f64 {
    if !elapsed_secs.is_finite() || elapsed_secs <= 0.0 || current < previous {
        return 0.0;
    }
    let rate = (current - previous) as f64 / elapsed_secs;
    if rate.is_finite() && rate <= MAX_RATE_BYTES_PER_SEC {
        rate
    } else {
        0.0
    }
}

pub(crate) struct NetworkSampler {
    requested: Option<String>,
    luid: Option<u64>,
    adapter: String,
    last: Option<(u64, u64, Instant)>,
    next_reselect: Option<Instant>,
    warned_loss: bool,
    warned_missing: bool,
}

impl NetworkSampler {
    pub(crate) fn new(config: &Config) -> Self {
        let requested = config
            .general
            .adapter
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        let mut sampler = Self {
            requested,
            luid: None,
            adapter: String::new(),
            last: None,
            next_reselect: None,
            warned_loss: false,
            warned_missing: false,
        };
        sampler.reselect();
        sampler
    }

    /// Re-enumerate the interface table and pick an adapter.
    fn reselect(&mut self) {
        let adapters = enumerate_adapters();
        // The default-route interface is the best signal for "the link that
        // actually carries traffic"; link speed alone favours Hyper-V/vEthernet.
        let best_index = best_interface_index();
        let selected = select_adapter(&adapters, self.requested.as_deref(), best_index);
        let now = Instant::now();

        match selected {
            Some(adapter) => {
                self.luid = Some(adapter.luid);
                self.adapter = adapter.name();
                self.last = None;
                self.next_reselect = None;
                self.warned_loss = false;
                self.warned_missing = false;
                debug!(
                    adapter = %self.adapter,
                    link_speed = adapter.link_speed,
                    "network adapter selected"
                );
            }
            None => {
                self.luid = None;
                self.last = None;
                self.next_reselect = Some(now + RESELECT_INTERVAL);
                if let Some(requested) = &self.requested {
                    self.adapter = requested.clone();
                    if !self.warned_missing {
                        warn!(adapter = %requested, "configured network adapter not found");
                        self.warned_missing = true;
                    }
                } else {
                    self.adapter.clear();
                    if !self.warned_missing {
                        warn!("no connected Ethernet/Wi-Fi/WWAN adapter found");
                        self.warned_missing = true;
                    }
                }
            }
        }
    }

    fn reselect_due(&self, now: Instant) -> bool {
        match self.next_reselect {
            Some(at) => now >= at,
            None => true,
        }
    }

    pub(crate) fn poll(&mut self) -> NetworkSample {
        let now = Instant::now();
        if self.luid.is_none() && self.reselect_due(now) {
            self.reselect();
        }

        let Some(luid) = self.luid else {
            return NetworkSample {
                adapter: self.adapter.clone(),
                connected: false,
                ..Default::default()
            };
        };

        let mut row = MIB_IF_ROW2 {
            InterfaceLuid: NET_LUID_LH { Value: luid },
            ..Default::default()
        };
        let status = unsafe { GetIfEntry2(&mut row) };
        if status != NO_ERROR {
            if !self.warned_loss {
                warn!(
                    adapter = %self.adapter,
                    code = status.0,
                    "network adapter lost; will re-enumerate"
                );
                self.warned_loss = true;
            }
            self.luid = None;
            self.last = None;
            self.next_reselect = Some(now + RESELECT_INTERVAL);
            return NetworkSample {
                adapter: self.adapter.clone(),
                connected: false,
                ..Default::default()
            };
        }

        let connected =
            row.OperStatus == IfOperStatusUp && row.MediaConnectState == MediaConnectStateConnected;

        // An auto-selected link went down (cable, Wi-Fi roam, VPN drop): drop
        // the selection so the next poll can fail over to another adapter. The
        // delta baseline goes with it, so no rate is computed across the gap.
        if !connected && self.requested.is_none() {
            self.luid = None;
            self.last = None;
            self.next_reselect = Some(now);
            return NetworkSample {
                adapter: self.adapter.clone(),
                connected: false,
                ..Default::default()
            };
        }

        let (mut down_bps, mut up_bps) = (0.0, 0.0);

        if let Some((prev_in, prev_out, prev_at)) = self.last {
            let elapsed = now.saturating_duration_since(prev_at).as_secs_f64();
            let counters_reset = row.InOctets < prev_in || row.OutOctets < prev_out;

            if connected && elapsed >= MIN_INTERVAL.as_secs_f64() {
                if counters_reset {
                    // Adapter reset or resume: discard this tick, keep the new
                    // baseline so the next poll reports a sane delta.
                    debug!(adapter = %self.adapter, "network counters reset; skipping a tick");
                } else {
                    down_bps = counter_rate(row.InOctets, prev_in, elapsed);
                    up_bps = counter_rate(row.OutOctets, prev_out, elapsed);
                }
            }
        }

        self.last = Some((row.InOctets, row.OutOctets, now));

        NetworkSample {
            adapter: self.adapter.clone(),
            up_bps,
            down_bps,
            connected,
        }
    }
}

fn enumerate_adapters() -> Vec<AdapterInfo> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    let status = unsafe { GetIfTable2(&mut table) };
    if status != NO_ERROR || table.is_null() {
        return Vec::new();
    }

    let mut adapters = Vec::new();
    // SAFETY: `GetIfTable2` returns a system-allocated table with `NumEntries`
    // rows; it stays valid until `FreeMibTable`.
    unsafe {
        let count = ((*table).NumEntries as usize).min(MAX_INTERFACES);
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), count);
        adapters.reserve(count);
        for row in rows {
            let flags = row.InterfaceAndOperStatusFlags._bitfield;
            adapters.push(AdapterInfo {
                luid: row.InterfaceLuid.Value,
                index: row.InterfaceIndex,
                alias: wide_array_to_string(&row.Alias),
                description: wide_array_to_string(&row.Description),
                link_speed: row.ReceiveLinkSpeed.max(row.TransmitLinkSpeed),
                if_type: row.Type,
                up: row.OperStatus == IfOperStatusUp,
                connected: row.MediaConnectState == MediaConnectStateConnected,
                hardware: flags & FLAG_HARDWARE != 0,
                filter: flags & FLAG_FILTER != 0,
                has_mac: row.PhysicalAddressLength > 0,
            });
        }
        FreeMibTable(table.cast::<c_void>());
    }
    adapters
}

fn wide_array_to_string(value: &[u16]) -> String {
    let len = value.iter().position(|&ch| ch == 0).unwrap_or(value.len());
    String::from_utf16_lossy(&value[..len]).trim().to_string()
}

/// Ask the IP Helper stack which interface carries the IPv4 default route.
fn best_interface_index() -> Option<u32> {
    let mut index = 0u32;
    let status = unsafe { GetBestInterface(ROUTE_PROBE, &mut index) };
    (status == NO_ERROR.0 && index != 0).then_some(index)
}

fn pick_adapter<'a>(pool: &[&'a AdapterInfo], best_index: Option<u32>) -> Option<&'a AdapterInfo> {
    pool.iter().copied().max_by_key(|adapter| {
        (
            best_index == Some(adapter.index),
            adapter.hardware,
            adapter.link_speed,
        )
    })
}

/// Select one adapter. An explicit config name wins even when it is currently
/// disconnected (and even when it is a VPN/tunnel).
fn select_adapter(
    adapters: &[AdapterInfo],
    requested: Option<&str>,
    best_index: Option<u32>,
) -> Option<AdapterInfo> {
    if let Some(requested) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        let wanted = requested.to_lowercase();
        let matches = |adapter: &AdapterInfo| {
            adapter.alias.to_lowercase() == wanted || adapter.description.to_lowercase() == wanted
        };
        let fuzzy = |adapter: &AdapterInfo| {
            adapter.alias.to_lowercase().contains(&wanted)
                || adapter.description.to_lowercase().contains(&wanted)
        };

        return adapters
            .iter()
            .filter(|adapter| matches(adapter))
            .max_by_key(|adapter| adapter.up && adapter.connected)
            .or_else(|| {
                adapters
                    .iter()
                    .filter(|adapter| fuzzy(adapter))
                    .max_by_key(|adapter| adapter.up && adapter.connected)
            })
            .cloned();
    }

    let candidates: Vec<&AdapterInfo> = adapters
        .iter()
        .filter(|adapter| adapter.is_auto_candidate())
        .collect();
    if candidates.is_empty() {
        return None;
    }

    // Prefer real hardware/MAC-backed links over virtual Ethernet (Hyper-V,
    // Tailscale, Bluetooth PAN). The default route wins over raw link speed so
    // a 10 Gbps virtual switch does not shadow the real Wi-Fi link.
    let physical: Vec<&AdapterInfo> = candidates
        .iter()
        .copied()
        .filter(|adapter| !adapter.is_virtual())
        .collect();
    if let Some(selected) = pick_adapter(&physical, best_index) {
        return Some(selected.clone());
    }

    // A VM's synthetic NIC often carries a "Hyper-V"/"VirtualBox" name but is
    // the only real uplink. Fall back to it, but never to a VPN/tunnel.
    let fallback: Vec<&AdapterInfo> = candidates
        .into_iter()
        .filter(|adapter| !adapter.has_vpn_name())
        .collect();
    pick_adapter(&fallback, best_index).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(
        index: u32,
        alias: &str,
        if_type: u32,
        speed: u64,
        up: bool,
        connected: bool,
    ) -> AdapterInfo {
        AdapterInfo {
            luid: index as u64,
            index,
            alias: alias.to_string(),
            description: format!("{alias} driver"),
            link_speed: speed,
            if_type,
            up,
            connected,
            hardware: true,
            filter: false,
            has_mac: true,
        }
    }

    #[test]
    fn auto_picks_fastest_physical_connected_adapter() {
        let adapters = vec![
            adapter(
                2,
                "Ethernet",
                IF_TYPE_ETHERNET_CSMACD,
                1_000_000_000,
                true,
                true,
            ),
            adapter(14, "Wi-Fi", IF_TYPE_IEEE80211, 2_400_000_000, true, true),
            adapter(
                4,
                "Disconnected",
                IF_TYPE_ETHERNET_CSMACD,
                10_000_000_000,
                false,
                false,
            ),
        ];
        let selected = select_adapter(&adapters, None, None).expect("adapter");
        assert_eq!(selected.alias, "Wi-Fi");
    }

    #[test]
    fn auto_skips_vethernet_tailscale_wsl_and_bluetooth() {
        let adapters = vec![
            adapter(14, "Wi-Fi", IF_TYPE_IEEE80211, 866_000_000, true, true),
            adapter(
                32,
                "vEthernet (Default Switch)",
                IF_TYPE_ETHERNET_CSMACD,
                10_000_000_000,
                true,
                true,
            ),
            adapter(
                33,
                "vEthernet (WSL)",
                IF_TYPE_ETHERNET_CSMACD,
                10_000_000_000,
                true,
                true,
            ),
            adapter(
                27,
                "Tailscale",
                IF_TYPE_ETHERNET_CSMACD,
                100_000_000_000,
                true,
                true,
            ),
            adapter(
                16,
                "Bluetooth Network Connection",
                IF_TYPE_ETHERNET_CSMACD,
                3_000_000,
                true,
                true,
            ),
        ];
        let selected = select_adapter(&adapters, None, None).expect("adapter");
        assert_eq!(selected.alias, "Wi-Fi");
    }

    #[test]
    fn auto_skips_vpn_when_a_physical_link_exists() {
        let mut vpn = adapter(
            27,
            "WireGuard",
            IF_TYPE_ETHERNET_CSMACD,
            10_000_000_000,
            true,
            true,
        );
        vpn.has_mac = false;
        vpn.hardware = false;
        let adapters = vec![
            adapter(14, "Wi-Fi", IF_TYPE_IEEE80211, 866_000_000, true, true),
            vpn,
        ];
        let selected = select_adapter(&adapters, None, None).expect("adapter");
        assert_eq!(selected.alias, "Wi-Fi");
    }

    #[test]
    fn vpn_only_is_not_auto_selected() {
        let adapters = vec![adapter(
            27,
            "WireGuard",
            IF_TYPE_ETHERNET_CSMACD,
            10_000_000_000,
            true,
            true,
        )];
        assert_eq!(select_adapter(&adapters, None, None), None);
    }

    #[test]
    fn explicit_vpn_is_selected() {
        let adapters = vec![
            adapter(14, "Wi-Fi", IF_TYPE_IEEE80211, 866_000_000, true, true),
            adapter(
                27,
                "WireGuard",
                IF_TYPE_ETHERNET_CSMACD,
                10_000_000_000,
                true,
                true,
            ),
        ];
        let selected = select_adapter(&adapters, Some("WIREGUARD"), None).expect("adapter");
        assert_eq!(selected.alias, "WireGuard");
    }

    #[test]
    fn usb_wwan_modem_is_auto_selected() {
        let mut modem = adapter(
            30,
            "Mobile Broadband",
            IF_TYPE_WWANPP,
            150_000_000,
            true,
            true,
        );
        // USB/WWAN modems frequently report no MAC and no hardware flag.
        modem.has_mac = false;
        modem.hardware = false;
        let adapters = vec![
            modem,
            adapter(
                32,
                "vEthernet (Default Switch)",
                IF_TYPE_ETHERNET_CSMACD,
                10_000_000_000,
                true,
                true,
            ),
        ];
        let selected = select_adapter(&adapters, None, None).expect("adapter");
        assert_eq!(selected.alias, "Mobile Broadband");
    }

    #[test]
    fn vm_synthetic_nic_is_a_last_resort() {
        let mut vm = adapter(
            5,
            "Microsoft Hyper-V Network Adapter",
            IF_TYPE_ETHERNET_CSMACD,
            10_000_000_000,
            true,
            true,
        );
        vm.hardware = false;
        let adapters = vec![vm];
        let selected = select_adapter(&adapters, None, None).expect("adapter");
        assert_eq!(selected.alias, "Microsoft Hyper-V Network Adapter");
    }

    #[test]
    fn no_adapters_yields_nothing() {
        assert_eq!(select_adapter(&[], None, None), None);
        assert_eq!(select_adapter(&[], Some("Wi-Fi"), None), None);
    }

    #[test]
    fn default_route_beats_link_speed() {
        let adapters = vec![
            adapter(
                2,
                "Ethernet",
                IF_TYPE_ETHERNET_CSMACD,
                2_500_000_000,
                true,
                true,
            ),
            adapter(14, "Wi-Fi", IF_TYPE_IEEE80211, 866_000_000, true, true),
        ];
        assert_eq!(
            select_adapter(&adapters, None, Some(14)).map(|a| a.alias),
            Some("Wi-Fi".into())
        );
        assert_eq!(
            select_adapter(&adapters, None, None).map(|a| a.alias),
            Some("Ethernet".into())
        );
    }

    #[test]
    fn explicit_name_matches_alias_or_description_case_insensitively() {
        let adapters = vec![
            adapter(
                2,
                "Ethernet",
                IF_TYPE_ETHERNET_CSMACD,
                1_000_000_000,
                true,
                true,
            ),
            adapter(14, "Wi-Fi", IF_TYPE_IEEE80211, 866_000_000, true, true),
        ];
        assert_eq!(
            select_adapter(&adapters, Some("wi-fi"), None).map(|a| a.alias),
            Some("Wi-Fi".into())
        );
        assert_eq!(
            select_adapter(&adapters, Some("ethernet driver"), None).map(|a| a.alias),
            Some("Ethernet".into())
        );
    }

    #[test]
    fn explicit_disconnected_adapter_is_still_selected() {
        let adapters = vec![
            adapter(
                2,
                "Ethernet",
                IF_TYPE_ETHERNET_CSMACD,
                1_000_000_000,
                true,
                true,
            ),
            adapter(14, "Wi-Fi", IF_TYPE_IEEE80211, 866_000_000, false, false),
        ];
        let selected = select_adapter(&adapters, Some("Wi-Fi"), None).expect("adapter");
        assert_eq!(selected.alias, "Wi-Fi");
    }

    #[test]
    fn counter_rate_handles_resets_and_garbage() {
        assert_eq!(counter_rate(2_000, 1_000, 1.0), 1_000.0);
        assert_eq!(counter_rate(1_000, 2_000, 1.0), 0.0);
        assert_eq!(counter_rate(2_000, 1_000, 0.0), 0.0);
        assert_eq!(counter_rate(2_000, 1_000, f64::NAN), 0.0);
        assert_eq!(counter_rate(2_000, 1_000, f64::INFINITY), 0.0);
        // A corrupt 64-bit delta over a 50 ms tick is rejected, not shown.
        assert_eq!(counter_rate(u64::MAX, 0, 0.05), 0.0);
    }

    #[test]
    fn wide_array_stops_at_nul() {
        let wide = [
            b'W' as u16,
            b'i' as u16,
            b'-' as u16,
            b'F' as u16,
            b'i' as u16,
            0,
            b'x' as u16,
        ];
        assert_eq!(wide_array_to_string(&wide), "Wi-Fi");
    }
}
