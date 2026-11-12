//! The tray icon's colours, from `[tray colors]` in `iwdtray.conf`.

use iwdtray_common::config::Config;

const GROUP: &str = "tray colors";
/// Leaves an icon in the icon theme's own colour.
const THEME: &str = "theme";

/// Tray icon colours as CSS colour strings (names, `#hex`, `rgb()`, `rgba()`);
/// `None` keeps the icon theme's colour.
pub struct Colors {
    pub connected: Option<String>,
    pub connecting: Option<String>,
    pub disconnected: Option<String>,
    pub wifi_off: Option<String>,
    pub no_wifi_hardware: Option<String>,
    pub hotspot_on: Option<String>,
    pub hotspot_off: Option<String>,
}

impl Colors {
    pub fn load() -> Self {
        Self::from_config(&Config::load())
    }

    fn from_config(config: &Config) -> Self {
        let color = |key: &str, default: &str| {
            let value = config
                .string(GROUP, key)
                .unwrap_or_else(|| default.to_owned());
            if value == THEME {
                None
            } else if is_safe_color(&value) {
                Some(value)
            } else {
                eprintln!(
                    "iwdtray.conf: [{GROUP}] {key} = {value} is not a colour; using {default}"
                );
                Some(default.to_owned())
            }
        };
        let colors = Self {
            connected: color("connected", "#00ff00"),
            connecting: color("connecting", "#ffff00"),
            disconnected: color("disconnected", "#808080"),
            wifi_off: color("wifi-off", "#808080"),
            no_wifi_hardware: color("no-wifi-hardware", "#808080"),
            hotspot_on: color("hotspot-on", "#00ff00"),
            hotspot_off: color("hotspot-off", "#808080"),
        };
        config.warn_unknown(GROUP, &KEYS);
        colors
    }
}

const KEYS: [&str; 7] = [
    "connected",
    "connecting",
    "disconnected",
    "wifi-off",
    "no-wifi-hardware",
    "hotspot-on",
    "hotspot-off",
];

/// Colours are pasted into generated SVG, so allow only characters that CSS
/// colour syntax needs. Validity itself is left to the SVG renderer.
fn is_safe_color(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "#(),.% ".contains(c))
}

#[cfg(test)]
mod tests {
    use iwdtray_common::config::{Config, TEMPLATE};

    use super::{Colors, is_safe_color};

    #[test]
    fn accepts_css_colors() {
        for c in [
            "lime",
            "#0f0",
            "#00ff0080",
            "rgb(0, 255, 0)",
            "rgba(0,100%,0,0.5)",
        ] {
            assert!(is_safe_color(c), "{c}");
        }
    }

    #[test]
    fn rejects_markup() {
        for c in ["", "red}</style><script>", "red;fill:blue", "\"red\""] {
            assert!(!is_safe_color(c), "{c}");
        }
    }

    #[test]
    fn the_shipped_file_lists_every_colour_at_its_default() {
        let config = Config::from_text(TEMPLATE);
        let from_file = Colors::from_config(&config);
        let defaults = Colors::from_config(&Config::from_files(&[]));
        let all = |c: &Colors| {
            [
                c.connected.clone(),
                c.connecting.clone(),
                c.disconnected.clone(),
                c.wifi_off.clone(),
                c.no_wifi_hardware.clone(),
                c.hotspot_on.clone(),
                c.hotspot_off.clone(),
            ]
        };
        assert_eq!(all(&from_file), all(&defaults));
        for key in super::KEYS {
            assert!(config.string(super::GROUP, key).is_some(), "{key} missing");
        }
    }
}
