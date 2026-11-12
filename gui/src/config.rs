//! The window's settings from `iwdtray.conf`: `[window size]`,
//! `[window theme]`, and `[dark theme colors]` / `[light theme colors]`.

use iwdtray_common::config::{self, Config};

use crate::style::{self, Palette, Rgb};

const THEME: &str = "window theme";
const SIZE: &str = "window size";
const DARK_COLORS: &str = "dark theme colors";
const LIGHT_COLORS: &str = "light theme colors";

/// Room for the four fixed-width action buttons; also the default width.
const MIN_WIDTH: i32 = 390;
/// Ends the Nearby list just above its fourth row's bottom rule, so the
/// action bar's top border takes that rule's place; also the default.
const MIN_HEIGHT: i32 = 539;

pub struct WindowConfig {
    pub width: i32,
    pub height: i32,
    pub dark: bool,
    pub dark_colors: Palette,
    pub light_colors: Palette,
}

impl WindowConfig {
    pub fn load() -> Self {
        Self::from_config(&Config::load())
    }

    fn from_config(config: &Config) -> Self {
        config.warn_unknown(THEME, &["theme"]);
        config.warn_unknown(SIZE, &["width", "height"]);
        let dark = match config.string(THEME, "theme").as_deref() {
            None | Some("dark") => true,
            Some("light") => false,
            Some(other) => {
                eprintln!("iwdtray.conf: [{THEME}] theme = {other} should be dark or light");
                true
            }
        };
        let size = |key: &str, min: i32| match config.string(SIZE, key) {
            None => min,
            Some(v) => match v.parse::<i32>() {
                Ok(n) if n >= min => n,
                _ => {
                    eprintln!(
                        "iwdtray.conf: [{SIZE}] {key} = {v} should be a number of pixels, at least {min}"
                    );
                    min
                }
            },
        };
        Self {
            width: size("width", MIN_WIDTH),
            height: size("height", MIN_HEIGHT),
            dark,
            dark_colors: palette(config, DARK_COLORS, style::DARK),
            light_colors: palette(config, LIGHT_COLORS, style::LIGHT),
        }
    }

    pub fn palette(&self, dark: bool) -> &Palette {
        if dark {
            &self.dark_colors
        } else {
            &self.light_colors
        }
    }
}

/// `defaults` with any colours `group` sets.
fn palette(config: &Config, group: &str, defaults: Palette) -> Palette {
    let mut p = defaults;
    config.warn_unknown(group, &p.named().map(|(key, _, _)| key));
    for (key, _, color) in p.named() {
        let Some(value) = config.string(group, key) else {
            continue;
        };
        match gtk::gdk::RGBA::parse(&value) {
            Ok(c) => *color = Rgb::from(c),
            Err(_) => {
                eprintln!("iwdtray.conf: [{group}] {key} = {value} is not a colour; ignoring it");
            }
        }
    }
    p
}

/// Saves the theme picked with the sun/moon button in the user's file.
pub fn save_theme(dark: bool) {
    let theme = if dark { "dark" } else { "light" };
    if let Err(e) = config::save_user(THEME, "theme", theme) {
        eprintln!("Cannot save the theme choice: {e}");
    }
}

#[cfg(test)]
mod tests {
    use iwdtray_common::config::{Config, TEMPLATE};

    use super::{DARK_COLORS, LIGHT_COLORS, WindowConfig};
    use crate::style;

    #[test]
    fn colours_and_size_from_the_file_are_used() {
        let c = WindowConfig::from_config(&Config::from_text(
            "[window theme]\ntheme = light\n[window size]\nwidth = 300\nheight = 700\n\
             [light theme colors]\nsignal-dots = orange\nbuttons = not-a-colour\n",
        ));
        assert!(!c.dark);
        // Too narrow is raised to the minimum; taller is fine.
        assert_eq!((c.width, c.height), (390, 700));
        assert_eq!(c.light_colors.neon, style::Rgb(255, 165, 0));
        // A bad colour keeps the default; the dark theme is untouched.
        assert_eq!(c.light_colors.action, style::LIGHT.action);
        assert_eq!(c.dark_colors, style::DARK);
    }

    #[test]
    fn the_shipped_file_lists_every_setting_at_its_default() {
        let config = Config::from_text(TEMPLATE);
        let from_file = WindowConfig::from_config(&config);
        let defaults = WindowConfig::from_config(&Config::from_files(&[]));
        assert_eq!(
            (from_file.width, from_file.height, from_file.dark),
            (defaults.width, defaults.height, defaults.dark)
        );
        assert_eq!(from_file.dark_colors, style::DARK);
        assert_eq!(from_file.light_colors, style::LIGHT);
        for group in [DARK_COLORS, LIGHT_COLORS] {
            let mut palette = style::DARK;
            for (key, _, _) in palette.named() {
                assert!(
                    config.string(group, key).is_some(),
                    "[{group}] {key} missing"
                );
            }
        }
    }
}
