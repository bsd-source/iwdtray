//! Hotspot clients' IP addresses from the kernel's neighbour (ARP) table.
//!
//! iwd's DHCP server knows the leases but doesn't publish them over D-Bus
//! (as of iwd 3.12 its access-point code ignores the lease events). Every
//! client that got an address has talked to the laptop as its gateway, so
//! the kernel has its IP next to its MAC in `/proc/net/arp`, readable
//! without root.

use std::collections::HashMap;

const ARP_TABLE: &str = "/proc/net/arp";

/// `Flags` bit for an entry whose MAC address is known.
const ATF_COM: u32 = 0x2;

/// Client IPs on `device` by lowercase MAC address; empty if the table
/// can't be read.
pub fn ips_by_mac(device: &str) -> HashMap<String, String> {
    std::fs::read_to_string(ARP_TABLE)
        .map(|table| parse(&table, device))
        .unwrap_or_default()
}

/// Parses `/proc/net/arp`, whose lines are:
/// `IP address  HW type  Flags  HW address  Mask  Device`.
fn parse(table: &str, device: &str) -> HashMap<String, String> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let [ip, _hw_type, flags, mac, _mask, dev] = f.as_slice() else {
                return None;
            };
            let flags = u32::from_str_radix(flags.trim_start_matches("0x"), 16).ok()?;
            (*dev == device && flags & ATF_COM != 0)
                .then(|| (mac.to_ascii_lowercase(), (*ip).to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse;

    const TABLE: &str = "\
IP address       HW type     Flags       HW address            Mask     Device
192.168.42.2     0x1         0x2         6A:1F:2B:3C:4D:5E     *        wlan0
192.168.42.3     0x1         0x0         00:00:00:00:00:00     *        wlan0
192.168.1.1      0x1         0x2         02:00:00:00:01:01     *        eth0
";

    #[test]
    fn finds_complete_entries_on_the_hotspot_device() {
        let ips = parse(TABLE, "wlan0");
        assert_eq!(
            ips.get("6a:1f:2b:3c:4d:5e").map(String::as_str),
            Some("192.168.42.2")
        );
        // Incomplete entries (no MAC yet) and other devices are left out.
        assert_eq!(ips.len(), 1);
    }

    #[test]
    fn tolerates_empty_or_odd_tables() {
        assert!(parse("", "wlan0").is_empty());
        assert!(parse("IP address HW type\nnot a real line\n", "wlan0").is_empty());
    }
}
