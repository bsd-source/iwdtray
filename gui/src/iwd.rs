//! The parts of iwd's D-Bus API the window uses.

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib::{self, Variant};

// Names, calls and read helpers shared with the tray; this module adds the
// calls and types only the window needs.
pub use iwdtray_common::iwd::*;

/// Sets a property through `org.freedesktop.DBus.Properties.Set`.
pub fn set_property(
    proxy: &gio::DBusProxy,
    name: &str,
    value: &Variant,
    done: impl FnOnce(Result<(), glib::Error>) + 'static,
) {
    let iface = proxy.interface_name();
    proxy.call(
        "org.freedesktop.DBus.Properties.Set",
        Some(&(iface.as_str(), name, value).to_variant()),
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        move |r| done(r.map(|_| ())),
    );
}

/// Whether `name` can be a network name: 1 to 32 bytes, Wi-Fi's limit.
pub fn valid_ssid(name: &str) -> bool {
    (1..=32).contains(&name.len())
}

/// One row of the network table.
#[derive(Clone, Debug, PartialEq)]
pub struct Network {
    pub path: String,
    pub ssid: String,
    pub dbm: i16,
    pub security: &'static str,
    pub connected: bool,
    pub known: bool,
    /// Radios broadcasting this network (iwd's `ExtendedServiceSet`):
    /// usually 1, more for dual-band routers and mesh systems.
    pub access_points: usize,
    /// MAC address shown under the name (lowercase): the radio the device
    /// is connected to, otherwise the network's first radio.
    pub address: Option<String>,
}

/// Fetches the station's networks: the connected one first, then the
/// rest strongest first.
pub fn ordered_networks(
    manager: &gio::DBusObjectManagerClient,
    station: &gio::DBusProxy,
    done: impl FnOnce(Vec<Network>) + 'static,
) {
    let manager = manager.clone();
    let connected_ap = prop_str(station, "ConnectedAccessPoint");
    fetch(station, "GetOrderedNetworks", "a(on)", move |list| {
        let Some(list) = list else {
            return done(Vec::new());
        };
        let networks = list
            .iter()
            .filter_map(|entry| {
                let path = entry.child_value(0).str()?.to_owned();
                // iwd reports signal strength in hundredths of a dBm.
                let dbm = entry.child_value(1).get::<i16>()? / 100;
                let net = proxy(&manager, &path, IFACE_NETWORK)?;
                let radios: Vec<String> = net
                    .cached_property("ExtendedServiceSet")
                    .map(|ess| {
                        ess.iter()
                            .filter_map(|b| b.str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                // The radio we're on if it belongs to this network, else the first.
                let shown = connected_ap
                    .as_ref()
                    .filter(|ap| radios.contains(ap))
                    .or_else(|| radios.first());
                let address = shown
                    .and_then(|bss| proxy(&manager, bss, IFACE_BSS))
                    .and_then(|bss| prop_str(&bss, "Address"))
                    .map(|a| a.to_ascii_lowercase());
                Some(Network {
                    ssid: prop_str(&net, "Name").unwrap_or_else(|| path.clone()),
                    security: security_label(prop_str(&net, "Type").as_deref()),
                    connected: prop_bool(&net, "Connected") == Some(true),
                    known: net.cached_property("KnownNetwork").is_some(),
                    access_points: radios.len(),
                    address,
                    path,
                    dbm,
                })
            })
            .collect();
        done(pin_connected(networks));
    });
}

/// Moves the connected network to the top, keeping the rest in iwd's order
/// (strongest first). The sort is stable, so ties keep their order.
fn pin_connected(mut networks: Vec<Network>) -> Vec<Network> {
    networks.sort_by_key(|n| !n.connected);
    networks
}

fn security_label(kind: Option<&str>) -> &'static str {
    match kind {
        Some("psk") => "PSK",
        Some("8021x") => "EAP",
        Some("wep") => "WEP",
        Some("open") => "OPEN",
        _ => "?",
    }
}

/// A network iwd has a stored profile for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub path: String,
    pub ssid: String,
    pub security: &'static str,
    pub auto_connect: bool,
    /// ISO 8601 in UTC, e.g. `2026-01-15T09:30:00Z`; `None` if never used.
    pub last_connected: Option<String>,
}

/// Saved networks from the object manager's cache, most recently used first.
pub fn saved_networks(manager: &gio::DBusObjectManagerClient) -> Vec<Saved> {
    let mut saved: Vec<Saved> = manager
        .objects()
        .iter()
        .filter_map(|o| {
            let known: gio::DBusProxy =
                DBusObjectExt::interface(o, IFACE_KNOWN)?.downcast().ok()?;
            let path = o.object_path().to_string();
            Some(Saved {
                ssid: prop_str(&known, "Name").unwrap_or_else(|| path.clone()),
                security: security_label(prop_str(&known, "Type").as_deref()),
                auto_connect: prop_bool(&known, "AutoConnect") != Some(false),
                last_connected: prop_str(&known, "LastConnectedTime"),
                path,
            })
        })
        .collect();
    sort_saved(&mut saved);
    saved
}

/// Most recent first; never-used profiles last, by name.
fn sort_saved(saved: &mut [Saved]) {
    // UTC ISO 8601 timestamps sort correctly as plain strings.
    saved.sort_by_cached_key(|s| {
        (
            std::cmp::Reverse(s.last_connected.clone()),
            s.ssid.to_lowercase(),
        )
    });
}

/// Link readings from `StationDiagnostic.GetDiagnostics`. iwd omits keys it
/// doesn't know, so every field is optional.
#[derive(Debug, Default, PartialEq)]
pub struct Diagnostics {
    pub rssi: Option<i16>,
    pub average_rssi: Option<i16>,
    /// Receive and transmit bitrates in units of 100 kbit/s, as iwd reports.
    pub rx_bitrate: Option<u32>,
    pub tx_bitrate: Option<u32>,
    pub frequency: Option<u32>,
    pub channel: Option<u16>,
    pub security: Option<String>,
    pub cipher: Option<String>,
    pub connected_secs: Option<u32>,
}

impl Diagnostics {
    /// From an `a{sv}` value; `VariantDict` panics on any other type.
    pub fn from_variant(dict: &Variant) -> Self {
        let dict = glib::VariantDict::new(Some(dict));
        let get = |key: &str| dict.lookup_value(key, None);
        Self {
            rssi: get("RSSI").and_then(|v| v.get()),
            average_rssi: get("AverageRSSI").and_then(|v| v.get()),
            rx_bitrate: get("RxBitrate").and_then(|v| v.get()),
            tx_bitrate: get("TxBitrate").and_then(|v| v.get()),
            frequency: get("Frequency").and_then(|v| v.get()),
            channel: get("Channel").and_then(|v| v.get()),
            security: get("Security").and_then(|v| v.str().map(str::to_owned)),
            cipher: get("PairwiseCipher").and_then(|v| v.str().map(str::to_owned)),
            connected_secs: get("ConnectedTime").and_then(|v| v.get()),
        }
    }
}

pub fn diagnostics(proxy: &gio::DBusProxy, done: impl FnOnce(Option<Diagnostics>) + 'static) {
    fetch(proxy, "GetDiagnostics", "a{sv}", move |dict| {
        done(dict.map(|d| Diagnostics::from_variant(&d)));
    });
}

/// An access point in range that doesn't broadcast its name.
#[derive(Debug, PartialEq, Eq)]
pub struct HiddenAp {
    pub address: String,
    pub dbm: i16,
    pub security: &'static str,
}

/// Hidden access points from the latest scan, strongest first.
pub fn hidden_access_points(station: &gio::DBusProxy, done: impl FnOnce(Vec<HiddenAp>) + 'static) {
    fetch(station, "GetHiddenAccessPoints", "a(sns)", move |list| {
        let mut aps: Vec<HiddenAp> = list
            .map(|list| {
                list.iter()
                    .filter_map(|entry| {
                        Some(HiddenAp {
                            address: entry.child_value(0).str()?.to_ascii_lowercase(),
                            // Hundredths of a dBm, as for named networks.
                            dbm: entry.child_value(1).get::<i16>()? / 100,
                            security: security_label(entry.child_value(2).str()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        aps.sort_by_key(|ap| std::cmp::Reverse(ap.dbm));
        done(aps);
    });
}

/// A device connected to the hotspot, from `AccessPointDiagnostic`.
#[derive(Debug, PartialEq, Eq)]
pub struct Client {
    /// MAC address, lowercase. Phones often use a random private one.
    pub address: String,
    pub rssi: Option<i16>,
    pub connected_secs: Option<u32>,
}

impl Client {
    /// From an `a{sv}` value; `VariantDict` panics on any other type.
    fn from_variant(dict: &Variant) -> Option<Self> {
        let dict = glib::VariantDict::new(Some(dict));
        Some(Self {
            address: dict
                .lookup_value("Address", None)?
                .str()?
                .to_ascii_lowercase(),
            rssi: dict.lookup_value("RSSI", None).and_then(|v| v.get()),
            connected_secs: dict
                .lookup_value("ConnectedTime", None)
                .and_then(|v| v.get()),
        })
    }
}

/// The hotspot's connected clients; empty if iwd can't say.
pub fn hotspot_clients(diagnostic: &gio::DBusProxy, done: impl FnOnce(Vec<Client>) + 'static) {
    fetch(diagnostic, "GetDiagnostics", "aa{sv}", move |list| {
        done(list.map_or_else(Vec::new, |list| {
            list.iter()
                .filter_map(|c| Client::from_variant(&c))
                .collect()
        }));
    });
}

/// A short, user-facing explanation of a failed iwd call.
pub fn describe_error(e: &glib::Error) -> String {
    let message = match gio::DBusError::remote_error(e).as_deref() {
        Some("net.connman.iwd.Failed") => "iwd couldn't do that. Try again in a moment.",
        Some("net.connman.iwd.Aborted") => "Canceled.",
        Some("net.connman.iwd.Busy") => "iwd is busy. Try again in a moment.",
        Some("net.connman.iwd.InProgress") => "Already in progress.",
        Some("net.connman.iwd.NoAgent") => {
            "No passphrase agent is running. Start iwdtray and try again."
        }
        Some("net.connman.iwd.NotConnected") => "Not connected.",
        Some("net.connman.iwd.NotAvailable") => "Not available right now.",
        Some("net.connman.iwd.NotSupported") => "This device doesn't support that.",
        Some("net.connman.iwd.AlreadyProvisioned") => {
            "That network is already saved. Pick it from the list instead."
        }
        _ => {
            let mut e = e.clone();
            gio::DBusError::strip_remote_error(&mut e);
            return e.message().to_owned();
        }
    };
    message.to_owned()
}

/// As `describe_error`, for a failed attempt to join a network.
pub fn describe_connect_error(e: &glib::Error) -> String {
    match gio::DBusError::remote_error(e).as_deref() {
        Some("net.connman.iwd.Failed") => "Connection failed. Check the password and try again.",
        Some("net.connman.iwd.NotFound") => {
            "No hidden network with that name is in range. Check the name and try again."
        }
        Some("net.connman.iwd.ServiceSetOverlap") => {
            "More than one hidden network answered to that name."
        }
        _ => return describe_error(e),
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_diagnostics_from_this_laptop() {
        // Captured from this laptop's iwd (Intel AX201) via GetDiagnostics.
        let dict = glib::VariantDict::new(None);
        dict.insert_value("RSSI", &(-46i16).to_variant());
        dict.insert_value("AverageRSSI", &(-44i16).to_variant());
        dict.insert_value("RxBitrate", &9607u32.to_variant());
        dict.insert_value("Frequency", &5785u32.to_variant());
        dict.insert_value("Channel", &157u16.to_variant());
        dict.insert_value("TxBitrate", &7206u32.to_variant());
        dict.insert_value("Security", &"WPA3-Personal".to_variant());
        dict.insert_value("PairwiseCipher", &"CCMP-128".to_variant());
        dict.insert_value("ConnectedTime", &856u32.to_variant());
        assert_eq!(
            Diagnostics::from_variant(&dict.end()),
            Diagnostics {
                rssi: Some(-46),
                average_rssi: Some(-44),
                rx_bitrate: Some(9607),
                tx_bitrate: Some(7206),
                frequency: Some(5785),
                channel: Some(157),
                security: Some("WPA3-Personal".into()),
                cipher: Some("CCMP-128".into()),
                connected_secs: Some(856),
            }
        );
    }

    #[test]
    fn connected_network_is_pinned_first() {
        let net = |ssid: &str, dbm, connected| Network {
            path: format!("/{ssid}"),
            ssid: ssid.into(),
            dbm,
            security: "PSK",
            connected,
            known: false,
            access_points: 1,
            address: None,
        };
        // Strongest first, as iwd returns them; ours is not the strongest.
        let list = vec![
            net("Sam’s Phone", -37, false),
            net("Lighthouse", -46, true),
            net("CornerCafe", -74, false),
        ];
        let order: Vec<_> = pin_connected(list).into_iter().map(|n| n.ssid).collect();
        assert_eq!(order, ["Lighthouse", "Sam’s Phone", "CornerCafe"]);
    }

    #[test]
    fn saved_networks_sort_most_recent_first() {
        let s = |ssid: &str, when: Option<&str>| Saved {
            path: format!("/{ssid}"),
            ssid: ssid.into(),
            security: "PSK",
            auto_connect: true,
            last_connected: when.map(Into::into),
        };
        let mut list = vec![
            s("never", None),
            s("old", Some("2025-01-02T10:00:00Z")),
            s("Lighthouse", Some("2026-01-15T09:30:00Z")),
        ];
        sort_saved(&mut list);
        let order: Vec<_> = list.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(order, ["Lighthouse", "old", "never"]);
    }

    #[test]
    fn ssid_length_limits() {
        assert!(valid_ssid("PineStar13"));
        assert!(!valid_ssid(""));
        assert!(valid_ssid(&"a".repeat(32)));
        assert!(!valid_ssid(&"a".repeat(33)));
        // 32 bytes, not characters: 11 three-byte characters is 33 bytes.
        assert!(!valid_ssid(&"\u{2605}".repeat(11)));
    }

    #[test]
    fn missing_keys_stay_empty() {
        let d = Diagnostics::from_variant(&glib::VariantDict::new(None).end());
        assert_eq!(d, Diagnostics::default());
    }
}
