//! Coloured copies of the current icon theme's tray icons.
//!
//! We look up a symbolic icon in the user's icon theme, write a copy with its
//! colour fixed, and give the panel that copy's name plus our directory via
//! `IconThemePath`. The panel still draws the icon itself, so it stays sharp
//! and keeps the theme's shape.
//!
//! Themes draw each size separately (Papirus's 22px icons are padded, its
//! 16px ones are not), and the tray protocol doesn't tell us the size the
//! panel draws at. So every size the theme has is copied into a `hicolor`
//! layout and the panel picks the closest one, as it would for the original.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gio::glib::{self, Variant};
use gio::prelude::*;

/// Fixed-size directories the standard `hicolor` theme defines for status icons.
const HICOLOR_SIZES: [i32; 13] = [16, 22, 24, 32, 36, 48, 64, 72, 96, 128, 192, 256, 512];

pub struct Colorizer {
    dir: PathBuf,
    /// Generated icon name per (theme, colour, icon); `None` if not found.
    cache: HashMap<(String, String, String), Option<String>>,
}

impl Colorizer {
    pub fn new() -> Self {
        let dir = glib::user_runtime_dir().join("iwdtray").join("icons");
        // Drop icons from earlier runs, which may belong to another theme.
        if let Err(e) = std::fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("Cannot clear {}: {e}", dir.display());
        }
        Self {
            dir,
            cache: HashMap::new(),
        }
    }

    /// Directory to advertise as the tray item's `IconThemePath`.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Returns the name of a coloured copy of `icon` from `theme`, creating
    /// it if needed, or `None` if the icon can't be found or written.
    pub fn colored(&mut self, theme: &str, icon: &str, color: &str) -> Option<String> {
        let key = (theme.to_owned(), color.to_owned(), icon.to_owned());
        if let Some(name) = self.cache.get(&key) {
            return name.clone();
        }
        // Numbered by cache entry, so different colours never share a file.
        let name = format!(
            "iwdtray-{}-{}",
            self.cache.len(),
            // No "-symbolic" suffix, or the panel would recolour it back.
            icon.trim_end_matches("-symbolic")
        );
        let name = self.create(theme, icon, color, name);
        self.cache.insert(key, name.clone());
        name
    }

    fn create(&self, theme: &str, icon: &str, color: &str, name: String) -> Option<String> {
        let sources = find_icon(&icon_dirs(), theme, icon);
        if sources.is_empty() {
            eprintln!("Icon {icon} not found in theme {theme}; using it uncoloured");
            return None;
        }

        let mut written = 0;
        for (slot, source) in sources {
            let colored = std::fs::read_to_string(&source)
                .inspect_err(|e| eprintln!("Cannot read {}: {e}", source.display()))
                .ok()
                .and_then(|svg| recolor(&svg, color));
            let Some(colored) = colored else { continue };
            let path = self
                .dir
                .join("hicolor")
                .join(slot)
                .join("status")
                .join(format!("{name}.svg"));
            match write_atomic(&path, colored.as_bytes()) {
                Ok(()) => written += 1,
                Err(e) => eprintln!("Cannot write {}: {e}", path.display()),
            }
        }
        (written > 0).then_some(name)
    }
}

/// Forces every shape to `color`, much as GTK recolours symbolic icons,
/// plus themes' `ColorScheme-` classes (KDE, Papirus). The panel draws the
/// result, so the style keeps to selectors every SVG renderer supports;
/// a shape drawn only as an outline would be filled in. Opacity attributes
/// are kept, so two-tone icons still work.
fn recolor(svg: &str, color: &str) -> Option<String> {
    let open = svg.find("<svg")?;
    let insert_at = open + svg[open..].find('>')? + 1;
    let style = format!(
        "<style type=\"text/css\">rect,circle,ellipse,path,polygon,polyline{{fill:{color} !important}}\
         [class^=\"ColorScheme-\"]{{color:{color} !important}}</style>"
    );
    let mut out = String::with_capacity(svg.len() + style.len());
    out.push_str(&svg[..insert_at]);
    out.push_str(&style);
    out.push_str(&svg[insert_at..]);
    Some(out)
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("svg.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, path)
}

/// Base directories that may contain icon themes, in lookup order.
fn icon_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        glib::user_data_dir().join("icons"),
        glib::home_dir().join(".icons"),
    ];
    dirs.extend(glib::system_data_dirs().iter().map(|d| d.join("icons")));
    dirs
}

/// A symbolic icon's SVG files in `theme` or the first theme it inherits
/// from that has it, keyed by `hicolor` directory (`"16x16"`, `"scalable"`).
/// Falls back to shorter names as GTK does
/// (`a-b-c-symbolic` → `a-b-symbolic` → `a-symbolic`).
fn find_icon(bases: &[PathBuf], theme: &str, icon: &str) -> Vec<(String, PathBuf)> {
    let themes = theme_chain(bases, theme);
    let mut base = icon.strip_suffix("-symbolic").unwrap_or(icon);
    loop {
        let name = format!("{base}-symbolic");
        if let Some(found) = themes.iter().find_map(|(theme, index)| {
            let found = find_in_theme(bases, theme, index, &name);
            (!found.is_empty()).then_some(found)
        }) {
            return found;
        }
        match base.rfind('-') {
            Some(i) => base = &base[..i],
            None => return Vec::new(),
        }
    }
}

/// The themes to search, in order: `theme` and its parents depth first,
/// then GTK's default theme and its parents, then `hicolor`, as GTK 3 does.
fn theme_chain(bases: &[PathBuf], theme: &str) -> Vec<(String, glib::KeyFile)> {
    fn visit(
        bases: &[PathBuf],
        theme: &str,
        seen: &mut HashSet<String>,
        out: &mut Vec<(String, glib::KeyFile)>,
    ) {
        if theme == "hicolor" || !seen.insert(theme.to_owned()) {
            return;
        }
        let Some(index) = load_index(bases, theme) else {
            return;
        };
        let parents = index
            .string_list("Icon Theme", "Inherits")
            .unwrap_or_default();
        out.push((theme.to_owned(), index));
        for parent in &parents {
            visit(bases, parent.trim(), seen, out);
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for theme in [theme, DEFAULT_THEME] {
        visit(bases, theme, &mut seen, &mut out);
    }
    if let Some(index) = load_index(bases, "hicolor") {
        out.push(("hicolor".to_owned(), index));
    }
    out
}

fn load_index(bases: &[PathBuf], theme: &str) -> Option<glib::KeyFile> {
    bases.iter().find_map(|base| {
        let index = glib::KeyFile::new();
        // Icon theme lists are comma-separated, not GKeyFile's default ';'.
        index.set_list_separator(glib::Char::from(b','));
        index
            .load_from_file(
                base.join(theme).join("index.theme"),
                glib::KeyFileFlags::NONE,
            )
            .ok()
            .map(|()| index)
    })
}

/// Every copy of `name.svg` in one theme, one per `hicolor` directory; the
/// first listed wins when a theme has several directories of the same size.
fn find_in_theme(
    bases: &[PathBuf],
    theme: &str,
    index: &glib::KeyFile,
    name: &str,
) -> Vec<(String, PathBuf)> {
    let Ok(dirs) = index.string_list("Icon Theme", "Directories") else {
        return Vec::new();
    };
    let file = format!("{name}.svg");
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for dir in &dirs {
        let Some(slot) = hicolor_slot(index, dir) else {
            continue;
        };
        if found.iter().any(|(s, _)| *s == slot) {
            continue;
        }
        if let Some(path) = bases
            .iter()
            .map(|base| base.join(theme).join(dir.as_str()).join(&file))
            .find(|p| p.is_file())
        {
            found.push((slot, path));
        }
    }
    found
}

/// The `hicolor` directory matching a theme directory's size, if any.
fn hicolor_slot(index: &glib::KeyFile, dir: &str) -> Option<String> {
    if index.string(dir, "Type").is_ok_and(|t| t == "Scalable") {
        return Some("scalable".to_owned());
    }
    let size = index.integer(dir, "Size").ok()?;
    HICOLOR_SIZES
        .contains(&size)
        .then(|| format!("{size}x{size}"))
}

/// GTK's own default icon theme, also its fallback before `hicolor`.
const DEFAULT_THEME: &str = "Adwaita";

/// The user's icon theme: XFCE's setting, then GTK's `settings.ini`, then
/// GTK's default.
pub fn current_theme(session: &gio::DBusConnection) -> String {
    xfconf_theme(session)
        .or_else(gtk_settings_theme)
        .unwrap_or_else(|| DEFAULT_THEME.to_owned())
}

const XFCONF_NAME: &str = "org.xfce.Xfconf";
const XFCONF_PATH: &str = "/org/xfce/Xfconf";
const XFCONF_CHANNEL: &str = "xsettings";
const XFCONF_PROPERTY: &str = "/Net/IconThemeName";

/// On XFCE, calls `on_change` with the new theme name whenever XFCE's icon
/// theme setting changes. Returns the subscription, which stays active until
/// dropped; elsewhere, `None`.
pub fn watch_xfce_theme(
    session: &gio::DBusConnection,
    on_change: impl Fn(String) + 'static,
) -> Option<gio::SignalSubscription> {
    if !on_xfce() {
        return None;
    }
    Some(session.subscribe_to_signal(
        Some(XFCONF_NAME),
        Some(XFCONF_NAME),
        Some("PropertyChanged"),
        Some(XFCONF_PATH),
        None,
        gio::DBusSignalFlags::NONE,
        move |signal| {
            // Nothing checks the signal's type for us: decode, don't index.
            if let Some((channel, property, theme)) =
                signal.parameters.get::<(String, String, Variant)>()
                && channel == XFCONF_CHANNEL
                && property == XFCONF_PROPERTY
                && let Some(theme) = theme.str()
            {
                on_change(theme.to_owned());
            }
        },
    ))
}

fn on_xfce() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|d| d.split(':').any(|d| d.eq_ignore_ascii_case("XFCE")))
}

fn xfconf_theme(session: &gio::DBusConnection) -> Option<String> {
    // xfconfd is started on demand and exits when idle, so it must be allowed
    // to start; but only on XFCE, never launch it on other desktops.
    if !on_xfce() {
        return None;
    }
    let reply = session
        .call_sync(
            Some(XFCONF_NAME),
            XFCONF_PATH,
            XFCONF_NAME,
            "GetProperty",
            Some(&(XFCONF_CHANNEL, XFCONF_PROPERTY).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            // Generous, since this may have to start xfconfd first.
            5000,
            gio::Cancellable::NONE,
        )
        .ok()?;
    // Reply is (v); the variant holds the theme name.
    let (theme,) = reply.get::<(Variant,)>()?;
    theme.str().map(str::to_owned)
}

fn gtk_settings_theme() -> Option<String> {
    let file = glib::KeyFile::new();
    let path = glib::user_config_dir().join("gtk-3.0").join("settings.ini");
    file.load_from_file(path, glib::KeyFileFlags::NONE).ok()?;
    file.string("Settings", "gtk-icon-theme-name")
        .ok()
        .map(Into::into)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write;
    use std::path::{Path, PathBuf};

    use super::{find_icon, recolor};

    /// A throwaway icon theme tree; removed when dropped.
    struct Themes(PathBuf);

    impl Themes {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("iwdtray-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        /// A theme with `inherits` and one directory per (dir, size) pair,
        /// holding `icons`.
        fn theme(&self, name: &str, inherits: &str, dirs: &[(&str, &str)], icons: &[&str]) {
            let root = self.0.join(name);
            let list: Vec<&str> = dirs.iter().map(|(d, _)| *d).collect();
            let mut index = format!(
                "[Icon Theme]\nName={name}\nInherits={inherits}\nDirectories={}\n",
                list.join(",")
            );
            for (dir, size) in dirs {
                let (size, kind) = match *size {
                    "scalable" => ("16", "Scalable"),
                    size => (size, "Fixed"),
                };
                writeln!(index, "[{dir}]\nSize={size}\nType={kind}").unwrap();
                std::fs::create_dir_all(root.join(dir)).unwrap();
                for icon in icons {
                    std::fs::write(root.join(dir).join(format!("{icon}.svg")), "<svg/>").unwrap();
                }
            }
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("index.theme"), index).unwrap();
        }

        fn found(&self, theme: &str, icon: &str) -> Vec<(String, String)> {
            let strip = |p: &Path| p.strip_prefix(&self.0).unwrap().display().to_string();
            find_icon(std::slice::from_ref(&self.0), theme, icon)
                .into_iter()
                .map(|(slot, path)| (slot, strip(&path)))
                .collect()
        }
    }

    impl Drop for Themes {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const ICON: &str = "network-wireless-signal-good-symbolic";

    #[test]
    fn parents_are_searched_depth_first_with_hicolor_last() {
        let t = Themes::new("depth");
        // A inherits B then C; B inherits D. D and C both have the icon,
        // and D comes first; hicolor's copy is the last resort.
        t.theme("A", "B,C", &[("16", "16")], &[]);
        t.theme("B", "D", &[("16", "16")], &[]);
        t.theme("C", "hicolor", &[("16", "16")], &[ICON]);
        t.theme("D", "", &[("16", "16")], &[ICON]);
        t.theme("hicolor", "", &[("16", "16")], &[ICON]);
        assert_eq!(
            t.found("A", ICON),
            [("16x16".into(), format!("D/16/{ICON}.svg"))]
        );
    }

    #[test]
    fn sizes_map_to_hicolor_directories() {
        let t = Themes::new("sizes");
        t.theme(
            "P",
            "",
            &[
                ("16", "16"),
                ("22", "22"),
                ("odd", "17"),
                ("sc", "scalable"),
            ],
            &[ICON],
        );
        let slots: Vec<String> = t.found("P", ICON).into_iter().map(|(s, _)| s).collect();
        // 17 px has no hicolor directory, so that copy is skipped.
        assert_eq!(slots, ["16x16", "22x22", "scalable"]);
    }

    #[test]
    fn missing_icons_fall_back_to_shorter_names() {
        let t = Themes::new("short");
        t.theme("P", "", &[("16", "16")], &["network-wireless-symbolic"]);
        assert_eq!(
            t.found("P", ICON),
            [("16x16".into(), "P/16/network-wireless-symbolic.svg".into())]
        );
        // A theme with no index.theme still reaches hicolor.
        t.theme("hicolor", "", &[("16", "16")], &[ICON]);
        assert_eq!(t.found("NoSuchTheme", ICON).len(), 1);
    }

    #[test]
    fn recolor_inserts_style_after_root_tag() {
        let svg = r#"<?xml version="1.0"?><svg width="16"><path d="M0"/></svg>"#;
        let out = recolor(svg, "lime").unwrap();
        assert!(out.starts_with(r#"<?xml version="1.0"?><svg width="16"><style"#));
        assert!(out.contains("fill:lime !important"));
        assert!(out.ends_with(r#"<path d="M0"/></svg>"#));
    }

    #[test]
    fn recolor_rejects_non_svg() {
        assert!(recolor("not an svg", "lime").is_none());
    }
}
