//! `iwdtray.conf`. The system-wide file (`/etc/iwdtray/iwdtray.conf`) holds
//! the defaults for everyone; `~/.config/iwdtray/iwdtray.conf` overrides them
//! one setting at a time, so it only needs the lines a user changes.

use std::path::{Path, PathBuf};

use gio::glib;

const SYSCONFDIR: &str = match option_env!("SYSCONFDIR") {
    Some(dir) => dir,
    None => "/etc",
};
const DIR_NAME: &str = "iwdtray";
const FILE_NAME: &str = "iwdtray.conf";
/// Every group the file can have, across both binaries.
const GROUPS: [&str; 5] = [
    "window size",
    "window theme",
    "dark theme colors",
    "light theme colors",
    "tray colors",
];

/// The settings in effect: the user's file, then the system-wide one.
pub struct Config {
    files: Vec<glib::KeyFile>,
}

impl Config {
    #[must_use]
    pub fn load() -> Self {
        let config = Self::from_files(&[
            user_path(),
            Path::new(SYSCONFDIR).join(DIR_NAME).join(FILE_NAME),
        ]);
        for file in &config.files {
            for group in file.groups() {
                if !GROUPS.contains(&group.as_str()) {
                    eprintln!("iwdtray.conf: there is no [{group}] group; ignoring it");
                }
            }
        }
        config
    }

    /// Reads a file's contents rather than a file; tests use it.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let file = glib::KeyFile::new();
        match file.load_from_data(text, glib::KeyFileFlags::NONE) {
            Ok(()) => Self { files: vec![file] },
            Err(e) => {
                eprintln!("Cannot parse settings: {e}");
                Self { files: Vec::new() }
            }
        }
    }

    /// Reads `paths`, earlier ones winning; missing files are skipped.
    #[must_use]
    pub fn from_files(paths: &[PathBuf]) -> Self {
        let files = paths
            .iter()
            .filter_map(|path| {
                let file = glib::KeyFile::new();
                match file.load_from_file(path, glib::KeyFileFlags::NONE) {
                    Ok(()) => Some(file),
                    Err(e) => {
                        if !e.matches(glib::FileError::Noent) {
                            eprintln!("Cannot read {}: {e}", path.display());
                        }
                        None
                    }
                }
            })
            .collect();
        Self { files }
    }

    /// A setting's value, trimmed, from the first file that has it.
    #[must_use]
    pub fn string(&self, group: &str, key: &str) -> Option<String> {
        self.files
            .iter()
            .find_map(|f| f.string(group, key).ok())
            .map(|v| v.trim().to_owned())
    }

    /// Reports settings in `group` that aren't in `known`, which are most
    /// likely typos.
    pub fn warn_unknown(&self, group: &str, known: &[&str]) {
        for file in &self.files {
            for key in file.keys(group).unwrap_or_default() {
                if !known.contains(&key.as_str()) {
                    eprintln!("iwdtray.conf: [{group}] has no setting called {key}; ignoring it");
                }
            }
        }
    }
}

/// `~/.config/iwdtray/iwdtray.conf`.
fn user_path() -> PathBuf {
    glib::user_config_dir().join(DIR_NAME).join(FILE_NAME)
}

/// The file shipped as `/etc/iwdtray/iwdtray.conf`: every setting at its
/// default. A new user file starts as a copy with each setting commented out.
pub const TEMPLATE: &str = include_str!("../../data/iwdtray.conf");

/// Sets one setting in the user's file, editing just that line so the
/// rest of the file (comments, spacing, order) stays as it was. A missing
/// file starts as [`TEMPLATE`] with every setting commented out, so it
/// overrides only what the user changes and still lists the rest.
///
/// # Errors
/// If the file exists but can't be read, or can't be written.
pub fn save_user(group: &str, key: &str, value: &str) -> Result<(), String> {
    save(&user_path(), group, key, value)
}

fn save(path: &Path, group: &str, key: &str, value: &str) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    // Write through a symlink (a dotfile manager's, say) rather than
    // replacing it with a plain file.
    let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let (text, perms) = match std::fs::read_to_string(path) {
        Ok(text) => (text, std::fs::metadata(path).ok().map(|m| m.permissions())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (commented_out(TEMPLATE), None),
        // Don't overwrite a file we couldn't read, or its contents are lost.
        Err(e) => return Err(fail(e)),
    };
    let text = set_line(&text, group, key, value);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    // A temporary file per process, synced before it replaces the old one,
    // so a crash or a second window saving at once never leaves a torn file.
    let tmp = path.with_extension(format!("conf.{}.tmp", std::process::id()));
    let written = write_synced(&tmp, &text, perms).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map_err(fail)
}

fn write_synced(
    path: &Path,
    text: &str,
    perms: Option<std::fs::Permissions>,
) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path)?;
    if let Some(perms) = perms {
        file.set_permissions(perms)?;
    }
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// `text` with each `key = value` line turned into `#key = value`.
fn commented_out(text: &str) -> String {
    text.lines()
        .map(|l| {
            let t = l.trim_start();
            if t.is_empty() || t.starts_with(['#', '[']) {
                l.to_owned()
            } else {
                format!("#{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// `text` with `key = value` in `[group]`: replacing the key's line if it
/// is set, else switching on its commented-out line (`#key = …`), else
/// adding it at the end of the group, or a new group at the end. When a
/// group or key appears twice, the last one is edited, as it is the one
/// that takes effect.
fn set_line(text: &str, group: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let new = format!("{key} = {value}");
    let header = format!("[{group}]");
    let line_key = |line: &str| line.split_once('=').map(|(k, _)| k.trim().to_owned());

    let Some(start) = lines.iter().rposition(|l| l.trim() == header) else {
        if lines.last().is_some_and(|l| !l.trim().is_empty()) {
            lines.push(String::new());
        }
        lines.extend([header, new]);
        return lines.join("\n") + "\n";
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
    let section = start + 1..end;

    let set = section.clone().rfind(|&i| {
        !lines[i].trim_start().starts_with('#') && line_key(&lines[i]).as_deref() == Some(key)
    });
    let commented = || {
        section.clone().find(|&i| {
            lines[i]
                .trim_start()
                .strip_prefix('#')
                .and_then(line_key)
                .as_deref()
                == Some(key)
        })
    };
    if let Some(i) = set.or_else(commented) {
        lines[i] = new;
    } else {
        let last = section
            .rev()
            .find(|&i| !lines[i].trim().is_empty())
            .unwrap_or(start);
        lines.insert(last + 1, new);
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Config, save, set_line};

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("iwdtray-conf-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str, text: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_user_file_overrides_one_setting_at_a_time() {
        let d = Dir::new("merge");
        let user = d.file("user.conf", "[tray colors]\nconnected = orange\n");
        let system = d.file(
            "system.conf",
            "[tray colors]\nconnected = lime\nconnecting = yellow\n",
        );
        let c = Config::from_files(&[user, system, d.0.join("missing.conf")]);
        assert_eq!(
            c.string("tray colors", "connected").as_deref(),
            Some("orange")
        );
        assert_eq!(
            c.string("tray colors", "connecting").as_deref(),
            Some("yellow")
        );
        assert_eq!(c.string("tray colors", "disconnected"), None);
    }

    #[test]
    fn saving_edits_only_the_settings_line() {
        let text = "# mine\n[window theme]\n# dark or light\n#theme = dark\n\n\
                    [tray colors]\nconnected = orange\n";
        // A commented-out setting is switched on where it is.
        let on = set_line(text, "window theme", "theme", "light");
        assert_eq!(
            on,
            "# mine\n[window theme]\n# dark or light\ntheme = light\n\n\
             [tray colors]\nconnected = orange\n"
        );
        // A set one is replaced, not duplicated.
        assert_eq!(
            set_line(&on, "window theme", "theme", "dark"),
            on.replace("theme = light", "theme = dark")
        );
        // A setting the group lacks goes after its last line.
        assert_eq!(
            set_line(text, "tray colors", "connecting", "red"),
            format!("{text}connecting = red\n")
        );
        // A missing group is added at the end.
        assert_eq!(
            set_line(
                "[tray colors]\nconnected = orange\n",
                "window theme",
                "theme",
                "dark"
            ),
            "[tray colors]\nconnected = orange\n\n[window theme]\ntheme = dark\n"
        );
    }

    #[test]
    fn a_new_user_file_sets_only_the_changed_setting() {
        let d = Dir::new("save");
        let path = d.0.join("sub").join("iwdtray.conf");
        save(&path, "window theme", "theme", "light").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // The rest of the template is there, but commented out, so the
        // system-wide file still decides those settings.
        assert!(text.contains("[tray colors]") && text.contains("#connected = #00ff00"));
        let c = Config::from_files(std::slice::from_ref(&path));
        assert_eq!(c.string("window theme", "theme").as_deref(), Some("light"));
        assert_eq!(c.string("tray colors", "connected"), None);
    }

    #[test]
    fn the_last_copy_of_a_setting_is_edited() {
        let text = "[window theme]\ntheme = dark\n[window theme]\ntheme = dark\n";
        let saved = set_line(text, "window theme", "theme", "light");
        assert_eq!(
            saved,
            "[window theme]\ntheme = dark\n[window theme]\ntheme = light\n"
        );
        let c = Config::from_text(&saved);
        assert_eq!(c.string("window theme", "theme").as_deref(), Some("light"));
    }

    #[test]
    fn saving_keeps_a_symlink_and_the_file_mode() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let d = Dir::new("link");
        let real = d.file("real.conf", "[window theme]\ntheme = dark\n");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.0.join("iwdtray.conf");
        symlink(&real, &link).unwrap();
        save(&link, "window theme", "theme", "light").unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "[window theme]\ntheme = light\n"
        );
        let mode = std::fs::metadata(&real).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            std::fs::read_dir(&d.0).unwrap().count(),
            2,
            "no temporary file left"
        );
    }

    #[test]
    fn the_template_has_every_group() {
        let file = gio::glib::KeyFile::new();
        file.load_from_data(super::TEMPLATE, gio::glib::KeyFileFlags::NONE)
            .unwrap();
        let groups: Vec<_> = file.groups().iter().map(ToString::to_string).collect();
        assert_eq!(groups, super::GROUPS);
    }
}
