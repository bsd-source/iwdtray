//! What the tray shows: a snapshot of iwd's state, and its icon and title.

use crate::config::Colors;

use iwdtray_common::iwd::SIGNAL_THRESHOLDS;

/// Icons by iwd signal level, strongest first (see `signal_level`).
const SIGNAL_ICONS: [&str; SIGNAL_THRESHOLDS.len() + 1] = [
    "network-wireless-signal-excellent-symbolic",
    "network-wireless-signal-good-symbolic",
    "network-wireless-signal-ok-symbolic",
    "network-wireless-signal-weak-symbolic",
    "network-wireless-signal-none-symbolic",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    IwdNotRunning,
    NoDevice,
    AdapterOff,
    DeviceOff,
    Disconnected,
    Connecting {
        ssid: Option<String>,
    },
    Connected {
        ssid: Option<String>,
        level: Option<u8>,
    },
    AccessPoint {
        started: bool,
    },
    AdHoc {
        started: bool,
    },
}

impl Status {
    pub fn icon_name(&self) -> &'static str {
        match self {
            Self::IwdNotRunning => "network-wireless-error-symbolic",
            Self::NoDevice | Self::AdapterOff => "network-wireless-hardware-disabled-symbolic",
            Self::DeviceOff => "network-wireless-disabled-symbolic",
            Self::Disconnected => "network-wireless-offline-symbolic",
            Self::Connecting { .. } => "network-wireless-acquiring-symbolic",
            Self::Connected { level: None, .. } => "network-wireless-connected-symbolic",
            Self::Connected { level: Some(l), .. } => {
                SIGNAL_ICONS[usize::from(*l).min(SIGNAL_ICONS.len() - 1)]
            }
            Self::AccessPoint { .. } | Self::AdHoc { .. } => "network-wireless-hotspot-symbolic",
        }
    }

    /// The configured colour for this state, or `None` to use the theme's.
    pub fn color<'a>(&self, c: &'a Colors) -> Option<&'a str> {
        match self {
            Self::IwdNotRunning => None,
            Self::NoDevice | Self::AdapterOff => c.no_wifi_hardware.as_deref(),
            Self::DeviceOff => c.wifi_off.as_deref(),
            Self::Disconnected => c.disconnected.as_deref(),
            Self::Connecting { .. } => c.connecting.as_deref(),
            Self::Connected { .. } => c.connected.as_deref(),
            // Ad-hoc is a rarely used, deprecated mode; it shares the
            // hotspot's colours.
            Self::AccessPoint { started: true } | Self::AdHoc { started: true } => {
                c.hotspot_on.as_deref()
            }
            Self::AccessPoint { started: false } | Self::AdHoc { started: false } => {
                c.hotspot_off.as_deref()
            }
        }
    }

    pub fn title(&self) -> String {
        let network = |ssid: &Option<String>| ssid.as_deref().unwrap_or("a network").to_owned();
        match self {
            Self::IwdNotRunning => "iwd is not running".into(),
            Self::NoDevice => "No wireless device found".into(),
            Self::AdapterOff => "Wireless hardware is disabled".into(),
            Self::DeviceOff => "Wireless interface is disabled".into(),
            Self::Disconnected => "Not connected to any wireless network".into(),
            Self::Connecting { ssid } => format!("Connecting to {}", network(ssid)),
            Self::Connected { ssid, .. } => format!("Connected to {}", network(ssid)),
            Self::AccessPoint { started: true } => "Access point is up".into(),
            Self::AccessPoint { started: false } => "Access point is down".into(),
            Self::AdHoc { started: true } => "Ad-hoc node is up".into(),
            Self::AdHoc { started: false } => "Ad-hoc node is down".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_level_picks_icon() {
        let s = Status::Connected {
            ssid: Some("home".into()),
            level: Some(0),
        };
        assert_eq!(s.icon_name(), "network-wireless-signal-excellent-symbolic");
        assert_eq!(s.title(), "Connected to home");
    }

    #[test]
    fn out_of_range_level_is_clamped() {
        let s = Status::Connected {
            ssid: None,
            level: Some(200),
        };
        assert_eq!(s.icon_name(), "network-wireless-signal-none-symbolic");
    }

    #[test]
    fn unknown_level_uses_generic_icons() {
        assert_eq!(
            Status::Connecting { ssid: None }.icon_name(),
            "network-wireless-acquiring-symbolic"
        );
        assert_eq!(
            Status::Connected {
                ssid: None,
                level: None
            }
            .title(),
            "Connected to a network"
        );
    }
}
