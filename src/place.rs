//! Places: where the machine is, told by the network it is on.
//!
//! A place is the router a default route goes through: its IPv4 address
//! *and* its hardware (MAC) address, because 192.168.1.1 is a router in half
//! the homes and offices there are, while its MAC address is its own. The
//! kind of link (wifi or ethernet) and the wifi name can narrow it further,
//! so a desk's cable and the same building's wifi can be two places.
//!
//! This file holds the pure part (parsing what the kernel says, choosing a
//! place); `niructl place` reads the files, acts and watches.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    Wifi,
    Ethernet,
    Other,
}

impl Link {
    pub fn name(self) -> &'static str {
        match self {
            Link::Wifi => "wifi",
            Link::Ethernet => "ethernet",
            Link::Other => "other",
        }
    }
}

/// One way out of the machine: a default route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Network {
    pub interface: String,
    pub link: Link,
    pub gateway: String,
    pub gateway_mac: Option<String>,
    pub ssid: Option<String>,
    pub metric: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Place {
    pub name: String,
    /// The router's IPv4 address.
    pub gateway: String,
    /// The router's MAC address; what really tells two networks apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_mac: Option<String>,
    /// "wifi" or "ethernet"; any link when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<Link>,
    /// The wifi name; any when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String>,

    /// Do not disturb while here (on or off); left as it is when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dnd: Option<bool>,
    /// A NIRU Display profile to apply on arrival.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_profile: Option<String>,
    /// An audio output (`niructl audio outputs`) to switch to on arrival.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_output: Option<String>,
    /// Shell commands run on arrival and on leaving, with NIRUX_PLACE set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_enter: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on_leave: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaceFile {
    #[serde(default, rename = "place", skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<Place>,
}

/// A little-endian hex IPv4 address from /proc/net/route.
fn route_ip(hex: &str) -> Option<String> {
    let value = u32::from_str_radix(hex, 16).ok()?;
    let b = value.to_le_bytes();
    Some(format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3]))
}

/// The default routes in /proc/net/route: (interface, gateway, metric),
/// best (lowest metric) first.
pub fn parse_routes(text: &str) -> Vec<(String, String, u32)> {
    const RTF_UP: u32 = 0x1;
    const RTF_GATEWAY: u32 = 0x2;
    let mut routes: Vec<(String, String, u32)> = text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 8 {
                return None;
            }
            let flags = u32::from_str_radix(f[3], 16).ok()?;
            if f[1] != "00000000"
                || f[7] != "00000000"
                || flags & (RTF_UP | RTF_GATEWAY) != RTF_UP | RTF_GATEWAY
            {
                return None;
            }
            let gateway = route_ip(f[2])?;
            let metric = f[6].parse().ok()?;
            Some((f[0].to_string(), gateway, metric))
        })
        .collect();
    routes.sort_by_key(|(_, _, metric)| *metric);
    routes.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    routes
}

/// Complete entries of /proc/net/arp: (ip, interface) -> lower-case MAC.
pub fn parse_arp(text: &str) -> HashMap<(String, String), String> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 6 {
                return None;
            }
            let flags = u32::from_str_radix(f[2].trim_start_matches("0x"), 16).ok()?;
            let mac = f[3].to_ascii_lowercase();
            if flags & 0x2 == 0 || mac == "00:00:00:00:00:00" {
                return None;
            }
            Some(((f[0].to_string(), f[5].to_string()), mac))
        })
        .collect()
}

/// The wifi name from `iw dev IF link`.
pub fn parse_iw_ssid(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("SSID: "))
        .map(|ssid| ssid.to_string())
        .filter(|ssid| !ssid.is_empty())
}

pub fn same_mac(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Whether `place` is the network `net`. A place saved with a MAC address
/// needs that router; without one (written by hand) the address is enough.
pub fn matches(place: &Place, net: &Network) -> bool {
    if place.gateway.trim() != net.gateway {
        return false;
    }
    if let Some(mac) = &place.gateway_mac {
        match &net.gateway_mac {
            Some(seen) if same_mac(mac, seen) => {}
            _ => return false,
        }
    }
    if place.link.is_some_and(|link| link != net.link) {
        return false;
    }
    if let Some(ssid) = &place.ssid {
        if net.ssid.as_deref() != Some(ssid.as_str()) {
            return false;
        }
    }
    true
}

/// The place for these networks: the first network (best route first) that
/// some place matches, and for it the first such place in the file.
pub fn choose<'a>(places: &'a [Place], networks: &[Network]) -> Option<&'a Place> {
    networks
        .iter()
        .find_map(|net| places.iter().find(|place| matches(place, net)))
}

/// A new place from the network now in use (the best route).
pub fn from_network(name: &str, net: &Network) -> Place {
    Place {
        name: name.to_string(),
        gateway: net.gateway.clone(),
        gateway_mac: net.gateway_mac.clone(),
        link: (net.link != Link::Other).then_some(net.link),
        ssid: net.ssid.clone(),
        ..Place::default()
    }
}

/// How long nothing has to match before the place counts as left: a wifi
/// that drops for a moment, or a cable moved to another socket, does not
/// run leave and enter again.
pub const LEAVE_AFTER_SECS: u64 = 20;

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE: &str =
        "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
wlp0s20f3\t00000000\t0132A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
enp0s20f0u1u4\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
enp0s20f0u1u4\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0
tailscale0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0
";

    const ARP: &str =
        "IP address       HW type     Flags       HW address            Mask     Device
192.168.1.1      0x1         0x2         AA:BB:CC:00:11:22     *        enp0s20f0u1u4
192.168.50.1     0x1         0x2         10:7c:61:aa:bb:cc     *        wlp0s20f3
192.168.50.9     0x1         0x0         00:00:00:00:00:00     *        wlp0s20f3
";

    fn net(
        interface: &str,
        link: Link,
        gateway: &str,
        mac: Option<&str>,
        ssid: Option<&str>,
    ) -> Network {
        Network {
            interface: interface.into(),
            link,
            gateway: gateway.into(),
            gateway_mac: mac.map(str::to_string),
            ssid: ssid.map(str::to_string),
            metric: 100,
        }
    }

    #[test]
    fn default_routes_come_best_first_without_onlink_or_local_ones() {
        assert_eq!(
            parse_routes(ROUTE),
            vec![
                ("enp0s20f0u1u4".to_string(), "192.168.1.1".to_string(), 100),
                ("wlp0s20f3".to_string(), "192.168.50.1".to_string(), 600),
            ]
        );
        assert!(parse_routes("").is_empty());
    }

    #[test]
    fn only_complete_arp_entries_count_and_macs_are_lower_case() {
        let arp = parse_arp(ARP);
        assert_eq!(arp.len(), 2);
        assert_eq!(
            arp[&("192.168.1.1".to_string(), "enp0s20f0u1u4".to_string())],
            "aa:bb:cc:00:11:22"
        );
    }

    #[test]
    fn the_ssid_comes_from_iw() {
        let text =
            "Connected to 10:7c:61:aa:bb:cc (on wlp0s20f3)\n\tSSID: Hemma 5G\n\tfreq: 5180\n";
        assert_eq!(parse_iw_ssid(text).as_deref(), Some("Hemma 5G"));
        assert_eq!(parse_iw_ssid("Not connected.\n"), None);
    }

    #[test]
    fn the_same_address_behind_another_router_is_another_place() {
        let home = from_network(
            "Home",
            &net(
                "wlp",
                Link::Wifi,
                "192.168.1.1",
                Some("aa:bb:cc:00:11:22"),
                Some("Hemma"),
            ),
        );
        assert!(matches(
            &home,
            &net(
                "wlp",
                Link::Wifi,
                "192.168.1.1",
                Some("AA:BB:CC:00:11:22"),
                Some("Hemma")
            )
        ));
        assert!(!matches(
            &home,
            &net(
                "wlp",
                Link::Wifi,
                "192.168.1.1",
                Some("de:ad:be:ef:00:01"),
                Some("Hemma")
            )
        ));
        assert!(!matches(
            &home,
            &net("wlp", Link::Wifi, "192.168.1.1", None, Some("Hemma"))
        ));
        assert!(!matches(
            &home,
            &net(
                "eth",
                Link::Ethernet,
                "192.168.1.1",
                Some("aa:bb:cc:00:11:22"),
                None
            )
        ));
    }

    #[test]
    fn a_hand_written_place_needs_only_what_it_names() {
        let office = Place {
            name: "Office".into(),
            gateway: "10.0.0.1".into(),
            ..Place::default()
        };
        assert!(matches(
            &office,
            &net("eth", Link::Ethernet, "10.0.0.1", None, None)
        ));
        assert!(matches(
            &office,
            &net(
                "wlp",
                Link::Wifi,
                "10.0.0.1",
                Some("aa:aa:aa:aa:aa:aa"),
                Some("Guest")
            )
        ));
        assert!(!matches(
            &office,
            &net("eth", Link::Ethernet, "10.0.0.254", None, None)
        ));
    }

    #[test]
    fn the_best_route_decides_and_then_the_order_in_the_file() {
        let desk = Place {
            name: "Desk".into(),
            gateway: "192.168.1.1".into(),
            link: Some(Link::Ethernet),
            ..Place::default()
        };
        let home = Place {
            name: "Home".into(),
            gateway: "192.168.50.1".into(),
            ..Place::default()
        };
        let home2 = Place {
            name: "Home again".into(),
            gateway: "192.168.50.1".into(),
            ..Place::default()
        };
        let places = vec![home.clone(), desk.clone(), home2];
        let both = vec![
            net("enp", Link::Ethernet, "192.168.1.1", None, None),
            net("wlp", Link::Wifi, "192.168.50.1", None, None),
        ];
        assert_eq!(
            choose(&places, &both).map(|p| p.name.as_str()),
            Some("Desk")
        );
        assert_eq!(
            choose(&places, &both[1..]).map(|p| p.name.as_str()),
            Some("Home")
        );
        assert_eq!(choose(&places, &[]), None);
    }

    #[test]
    fn the_file_round_trips_and_rejects_unknown_keys() {
        let text = "[[place]]\nname = \"Home\"\ngateway = \"192.168.50.1\"\ngateway_mac = \"10:7c:61:aa:bb:cc\"\nlink = \"wifi\"\nssid = \"Hemma\"\ndnd = false\ndisplay_profile = \"Two screens\"\non_enter = [\"echo hi\"]\n";
        let file: PlaceFile = toml::from_str(text).unwrap();
        assert_eq!(file.places[0].link, Some(Link::Wifi));
        assert_eq!(file.places[0].on_enter, vec!["echo hi".to_string()]);
        let again: PlaceFile = toml::from_str(&toml::to_string_pretty(&file).unwrap()).unwrap();
        assert_eq!(again, file);
        assert!(toml::from_str::<PlaceFile>(
            "[[place]]\nname = \"x\"\ngateway = \"1.1.1.1\"\ncolour = 1\n"
        )
        .is_err());
    }
}
