//! iwdtray-gui, the GTK front end: the window, and the passphrase prompt that
//! `iwdtray` starts when iwd asks for credentials.

mod actions;
mod arp;
mod config;
mod format;
mod hidden;
mod hotspot;
mod iwd;
mod prompt;
mod saved;
mod style;
mod widgets;
mod window;

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

const USAGE: &str = "\
Usage: iwdtray-gui
       iwdtray-gui --prompt <kind>

Without arguments, opens the window.

With --prompt, asks for credentials for the network named in $IWDTRAY_SSID
(and, for `password`, the username in $IWDTRAY_USER) and writes them to stdout,
separated by NUL bytes. Exits with status 1 if the user cancels. <kind> is one of:
  passphrase       pre-shared key passphrase
  private-key      passphrase for an 802.1X private key
  user-password    username and password
  password         password for the given username";

fn main() -> ExitCode {
    use_software_rendering();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let env = |name| std::env::var(name).ok();
    let (ssid, user) = (env("IWDTRAY_SSID"), env("IWDTRAY_USER"));

    let request = match (args.as_slice(), ssid.as_deref(), user.as_deref()) {
        ([], ..) => return window::run().into(),
        (["--prompt", "passphrase"], Some(ssid), _) => prompt::Request::Passphrase { ssid },
        (["--prompt", "private-key"], Some(ssid), _) => prompt::Request::PrivateKey { ssid },
        (["--prompt", "user-password"], Some(ssid), _) => prompt::Request::UserPassword { ssid },
        (["--prompt", "password"], Some(ssid), Some(user)) => {
            prompt::Request::Password { ssid, user }
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    prompt::run(&request)
}

/// Restarts this process with GTK's GPU rendering turned off, unless the user
/// chose a renderer. Otherwise GTK 4 loads the GPU driver (on Mesa, LLVM and
/// gallium: about 120 MB) just to draw a small window; measured here, the
/// window drops from 210 MB to 64 MB resident. GTK reads these variables
/// only at startup, and changing our own environment would need `unsafe`.
fn use_software_rendering() {
    const MARKER: &str = "IWDTRAY_SOFTWARE_RENDERING";
    let set = |name: &str| std::env::var_os(name).is_some();
    if set(MARKER) || set("GSK_RENDERER") || set("GDK_DISABLE") {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let error = Command::new(exe)
        .args(args)
        .env(MARKER, "1")
        .env("GSK_RENDERER", "cairo")
        .env("GDK_DISABLE", "gl,vulkan")
        .exec();
    // exec only returns on failure; carry on with GPU rendering.
    eprintln!("Cannot restart with software rendering: {error}");
}
