//! The window's two palettes, turned into GTK CSS colour definitions.

use std::fmt::Write;

use gtk::glib;
use gtk::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl From<gtk::gdk::RGBA> for Rgb {
    /// Drops the alpha: every palette colour is opaque.
    fn from(c: gtk::gdk::RGBA) -> Self {
        // Each channel is clamped to 0..=255 first, so the cast is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Self(byte(c.red()), byte(c.green()), byte(c.blue()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: Rgb,
    pub panel: Rgb,
    pub line: Rgb,
    pub ink: Rgb,
    pub soft: Rgb,
    pub faint: Rgb,
    /// Highlights: the connected row's tint, the selection, focus rings.
    pub ice: Rgb,
    /// Neon orange: status label, warnings, errors.
    pub amber: Rgb,
    /// Live link.
    pub ok: Rgb,
    /// The signal dots.
    pub neon: Rgb,
    /// Action buttons that can be used.
    pub action: Rgb,
    /// Switches that are on.
    pub switch: Rgb,
}

pub const DARK: Palette = Palette {
    bg: Rgb(0x0c, 0x11, 0x18),
    panel: Rgb(0x12, 0x1a, 0x24),
    line: Rgb(0x22, 0x30, 0x42),
    ink: Rgb(0xd7, 0xe1, 0xee),
    soft: Rgb(0x87, 0x98, 0xae),
    faint: Rgb(0x56, 0x67, 0x7e),
    ice: Rgb(0x7f, 0xd3, 0xff),
    amber: Rgb(0xff, 0x5f, 0x1f),
    ok: Rgb(0x5b, 0xe3, 0xa1),
    neon: Rgb(0x39, 0xff, 0x14),
    action: Rgb(0xff, 0x5f, 0x1f),
    switch: Rgb(0xff, 0x5f, 0x1f),
};

impl Palette {
    /// Each colour as (name in iwdtray.conf, name in style.css as `@k_<name>`).
    pub fn named(&mut self) -> [(&'static str, &'static str, &mut Rgb); 12] {
        [
            ("background", "bg", &mut self.bg),
            ("fields", "panel", &mut self.panel),
            ("lines", "line", &mut self.line),
            ("text", "ink", &mut self.ink),
            ("dim-text", "soft", &mut self.soft),
            ("headings", "faint", &mut self.faint),
            ("highlight", "ice", &mut self.ice),
            ("status", "amber", &mut self.amber),
            ("connected-mark", "ok", &mut self.ok),
            ("signal-dots", "neon", &mut self.neon),
            ("buttons", "action", &mut self.action),
            ("switches", "switch", &mut self.switch),
        ]
    }
}

/// Cool paper ground; accents darkened to keep contrast on white.
pub const LIGHT: Palette = Palette {
    bg: Rgb(0xf7, 0xf9, 0xfc),
    panel: Rgb(0xed, 0xf1, 0xf6),
    line: Rgb(0xd6, 0xdd, 0xe7),
    ink: Rgb(0x15, 0x20, 0x2d),
    soft: Rgb(0x4d, 0x5c, 0x70),
    faint: Rgb(0x7a, 0x88, 0x9c),
    ice: Rgb(0x0a, 0x6f, 0xb0),
    amber: Rgb(0xff, 0x5f, 0x1f),
    ok: Rgb(0x0e, 0x8c, 0x56),
    neon: Rgb(0x39, 0xff, 0x14),
    // Orange looked wrong on the paper ground; the light accent blue instead.
    action: Rgb(0x0a, 0x6f, 0xb0),
    switch: Rgb(0x0a, 0x6f, 0xb0),
};

/// Adds the window stylesheet to the display in the given palette. The
/// returned provider can be passed to [`apply`] to switch palettes later.
pub fn install(palette: &Palette, dark: bool) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    apply(&provider, palette, dark);
    provider
}

/// Loads `palette` into `provider`; `dark` says which kind it is.
pub fn apply(provider: &gtk::CssProvider, palette: &Palette, dark: bool) {
    provider.load_from_string(&css(palette));
    set_color_scheme(dark);
}

/// Makes native controls (switch, entry, scrollbars) follow the palette.
pub fn set_color_scheme(dark: bool) {
    if let Some(settings) = gtk::Settings::default() {
        // GTK 4.20 replaced the prefer-dark flag with a colour scheme setting.
        // It's looked up by name so the window still runs on GTK 4.12.
        let scheme = glib::Type::from_name("GtkInterfaceColorScheme")
            .and_then(glib::EnumClass::with_type)
            .and_then(|class| class.to_value_by_nick(if dark { "dark" } else { "light" }));
        match scheme {
            Some(value) => settings.set_property_from_value("gtk-interface-color-scheme", &value),
            None => settings.set_gtk_application_prefer_dark_theme(dark),
        }
    }
}

/// The full stylesheet: palette colour definitions followed by style.css.
pub fn css(p: &Palette) -> String {
    let mut p = *p;
    let mut out = String::new();
    for (_, name, c) in p.named() {
        let _ = writeln!(
            out,
            "@define-color k_{name} #{:02x}{:02x}{:02x};",
            c.0, c.1, c.2
        );
    }
    out.push_str(include_str!("style.css"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_defines_every_colour_it_uses() {
        let css = css(&DARK);
        let mut palette = DARK;
        for (_, name, _) in palette.named() {
            assert!(css.contains(&format!("@define-color k_{name} #")), "{name}");
        }
        for used in css.match_indices("@k_").map(|(i, _)| &css[i + 1..]) {
            let name: String = used
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            assert!(
                css.contains(&format!("@define-color {name} ")),
                "{name} is used but not defined"
            );
        }
    }
}
