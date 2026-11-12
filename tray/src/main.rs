//! iwdtray tray indicator and iwd agent (Rust rewrite). Deliberately GTK-free:
//! anything needing a window runs as a separate `iwdtray-gui` process.

mod agent;
mod config;
mod export;
mod icons;
mod iwd;
mod sni;
mod status;
mod tray;

use std::rc::Rc;

use gio::glib;
use gio::prelude::*;

// POSIX signal numbers, the same on every Unix.
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;

fn main() -> glib::ExitCode {
    let session = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Cannot connect to the session bus: {e}");
            return glib::ExitCode::FAILURE;
        }
    };
    if !first_instance(&session) {
        eprintln!("iwdtray is already running");
        return glib::ExitCode::SUCCESS;
    }

    // The GTK side (window and passphrase prompt) is the `iwdtray-gui`
    // binary installed next to us.
    let gui = std::env::current_exe()
        .map(|exe| exe.with_file_name("iwdtray-gui"))
        .ok()
        .filter(|p| p.is_file());
    if gui.is_none() {
        eprintln!("iwdtray-gui not found next to iwdtray; no window or passphrase prompt");
    }

    let window = gui.clone();
    let tray = match tray::Tray::new(&session, move || {
        // The window is single-instance: a second click raises it.
        let Some(window) = &window else { return };
        if let Err(e) = gio::Subprocess::newv(&[window.as_os_str()], gio::SubprocessFlags::NONE) {
            eprintln!("Failed to open the iwdtray window: {e}");
        }
    }) {
        Ok(tray) => Rc::new(tray),
        Err(e) => {
            eprintln!("Failed to create tray item: {e}");
            return glib::ExitCode::FAILURE;
        }
    };

    let t = Rc::clone(&tray);
    let _theme_watch = icons::watch_xfce_theme(&session, move |theme| t.set_theme(theme));

    let t = Rc::clone(&tray);
    let monitor = match iwd::Monitor::new(move |status| t.show(status)) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Cannot watch iwd on the system bus: {e}");
            return glib::ExitCode::FAILURE;
        }
    };

    // Without the prompt, stay out of iwd's way so another agent can answer.
    let agent = gui.as_deref().and_then(|prompt| {
        agent::Agent::new(monitor.manager(), prompt)
            .inspect_err(|e| eprintln!("Cannot export the iwd agent: {e}"))
            .ok()
    });

    // Quit on logout or Ctrl-C, so the agent closes any open prompt and
    // unregisters from iwd on the way out.
    let main_loop = glib::MainLoop::new(None, false);
    for signal in [SIGINT, SIGTERM] {
        let l = main_loop.clone();
        glib_unix::unix_signal_add_local_once(signal, move || l.quit());
    }
    main_loop.run();

    drop(agent);
    // Send UnregisterAgent before the process ends.
    let _ = monitor
        .manager()
        .connection()
        .flush_sync(gio::Cancellable::NONE);
    glib::ExitCode::SUCCESS
}

/// Claims the tray's well-known bus name, held until exit. A second copy
/// gets `false`: two would share one icon cache and both answer iwd.
fn first_instance(session: &gio::DBusConnection) -> bool {
    const NAME: &str = "io.github.bsd_source.iwdtray.tray";
    const DO_NOT_QUEUE: u32 = 4;
    const PRIMARY_OWNER: u32 = 1;
    session
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(NAME, DO_NOT_QUEUE).to_variant()),
            Some(glib::VariantTy::new("(u)").expect("valid type")),
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.get::<(u32,)>())
        // If the bus won't say, carry on rather than show no tray at all.
        .is_none_or(|(reply,)| reply == PRIMARY_OWNER)
}

#[cfg(test)]
mod tests {
    use mock_iwd::MockIwd;

    #[test]
    fn a_second_copy_is_turned_away() {
        let iwd = MockIwd::start();
        let (first, second) = (iwd.connect(), iwd.connect());
        assert!(super::first_instance(&first));
        assert!(!super::first_instance(&second));
    }
}
