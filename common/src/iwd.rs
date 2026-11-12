//! iwd's D-Bus names and the small helpers both binaries use to read it.

use gio::glib;
use gio::prelude::*;

pub const IWD_NAME: &str = "net.connman.iwd";
pub const IWD_PATH: &str = "/net/connman/iwd";

pub const IFACE_ADAPTER: &str = "net.connman.iwd.Adapter";
pub const IFACE_DEVICE: &str = "net.connman.iwd.Device";
pub const IFACE_STATION: &str = "net.connman.iwd.Station";
pub const IFACE_DIAGNOSTIC: &str = "net.connman.iwd.StationDiagnostic";
pub const IFACE_NETWORK: &str = "net.connman.iwd.Network";
pub const IFACE_BSS: &str = "net.connman.iwd.BasicServiceSet";
pub const IFACE_KNOWN: &str = "net.connman.iwd.KnownNetwork";
pub const IFACE_AP: &str = "net.connman.iwd.AccessPoint";
pub const IFACE_ADHOC: &str = "net.connman.iwd.AdHoc";
pub const IFACE_AP_DIAGNOSTIC: &str = "net.connman.iwd.AccessPointDiagnostic";
pub const IFACE_AGENT_MANAGER: &str = "net.connman.iwd.AgentManager";
pub const IFACE_AGENT: &str = "net.connman.iwd.Agent";
pub const IFACE_SIGNAL_AGENT: &str = "net.connman.iwd.SignalLevelAgent";

/// Signal strength thresholds in dBm, strongest first. They define the tray
/// icon's five levels and the window's five signal dots, so both agree.
pub const SIGNAL_THRESHOLDS: [i16; 4] = [-60, -67, -74, -81];

/// Signal level for a reading: 0 (strongest, −60 dBm or more) to 4 (below
/// −81), the same levels iwd's signal agent reports for these thresholds.
#[must_use]
pub fn signal_level(dbm: i16) -> u8 {
    let below = SIGNAL_THRESHOLDS.iter().filter(|&&t| dbm < t).count();
    // At most four thresholds, so the fallback is never used.
    u8::try_from(below).unwrap_or(4)
}

/// Whether a station `State` means it has a link. Roaming moves between
/// radios of the same network without dropping it.
#[must_use]
pub fn is_connected(state: &str) -> bool {
    matches!(state, "connected" | "roaming")
}

/// The D-Bus default timeout, for calls iwd answers right away.
pub const DEFAULT_TIMEOUT: i32 = -1;
/// For calls that may wait on the user, such as typing a passphrase.
pub const NO_TIMEOUT: i32 = i32::MAX;

/// Calls `method` and hands back its reply.
pub fn call(
    proxy: &gio::DBusProxy,
    method: &str,
    args: Option<&glib::Variant>,
    timeout: i32,
    done: impl FnOnce(Result<glib::Variant, glib::Error>) + 'static,
) {
    proxy.call(
        method,
        args,
        gio::DBusCallFlags::NONE,
        timeout,
        gio::Cancellable::NONE,
        done,
    );
}

/// Calls a getter and hands back its single return value, or `None` (and a
/// line on stderr) if the call failed or the value isn't of type `ty`, such
/// as `"a{sv}"`. Proxies from the object manager carry no interface data,
/// so GIO doesn't check reply types; checking here keeps the value safe to
/// take apart.
pub fn fetch(
    proxy: &gio::DBusProxy,
    method: &'static str,
    ty: &'static str,
    done: impl FnOnce(Option<glib::Variant>) + 'static,
) {
    call(proxy, method, None, DEFAULT_TIMEOUT, move |result| {
        done(match result {
            Ok(reply) if reply.type_().as_str() == format!("({ty})") => Some(reply.child_value(0)),
            Ok(reply) => {
                eprintln!("{method} returned {}, not ({ty})", reply.type_());
                None
            }
            Err(e) => {
                eprintln!("{method} failed: {e}");
                None
            }
        });
    });
}

/// A client for iwd's object manager. It works while iwd is down: objects
/// appear once it starts.
///
/// # Errors
///
/// Fails if the system bus can't be reached.
pub fn manager() -> Result<gio::DBusObjectManagerClient, glib::Error> {
    manager_on(&gio::bus_get_sync(
        gio::BusType::System,
        gio::Cancellable::NONE,
    )?)
}

/// Like [`manager`], on a given connection; tests pass a private bus.
///
/// # Errors
///
/// Fails if the object manager can't be created on `connection`.
pub fn manager_on(
    connection: &gio::DBusConnection,
) -> Result<gio::DBusObjectManagerClient, glib::Error> {
    gio::DBusObjectManagerClient::new_sync(
        connection,
        gio::DBusObjectManagerClientFlags::DO_NOT_AUTO_START,
        Some(IWD_NAME),
        "/",
        gio::Cancellable::NONE,
    )
}

#[must_use]
pub fn proxy(
    manager: &gio::DBusObjectManagerClient,
    path: &str,
    iface: &str,
) -> Option<gio::DBusProxy> {
    DBusObjectManagerExt::interface(manager, path, iface)?
        .downcast()
        .ok()
}

#[must_use]
pub fn prop_str(proxy: &gio::DBusProxy, name: &str) -> Option<String> {
    proxy.cached_property(name)?.str().map(str::to_owned)
}

#[must_use]
pub fn prop_bool(proxy: &gio::DBusProxy, name: &str) -> Option<bool> {
    proxy.cached_property(name)?.get()
}

/// The device iwdtray shows: the first one by object path.
#[must_use]
pub fn first_device(manager: &gio::DBusObjectManagerClient) -> Option<gio::DBusProxy> {
    manager
        .objects()
        .iter()
        .filter_map(|o| DBusObjectExt::interface(o, IFACE_DEVICE)?.downcast().ok())
        .min_by(|a: &gio::DBusProxy, b| a.object_path().cmp(&b.object_path()))
}

/// The adapter (radio) a device belongs to.
#[must_use]
pub fn adapter(
    manager: &gio::DBusObjectManagerClient,
    device: &gio::DBusProxy,
) -> Option<gio::DBusProxy> {
    proxy(manager, &prop_str(device, "Adapter")?, IFACE_ADAPTER)
}

/// The name of the network a station is connected or connecting to.
#[must_use]
pub fn connected_ssid(
    manager: &gio::DBusObjectManagerClient,
    station: &gio::DBusProxy,
) -> Option<String> {
    let network = proxy(
        manager,
        &prop_str(station, "ConnectedNetwork")?,
        IFACE_NETWORK,
    )?;
    prop_str(&network, "Name")
}

/// Hands back `invocation` if the call came from iwd itself, as calls to an
/// agent must; otherwise refuses it and returns `None`.
#[must_use]
pub fn only_iwd(
    manager: &gio::DBusObjectManagerClient,
    sender: Option<&str>,
    invocation: gio::DBusMethodInvocation,
) -> Option<gio::DBusMethodInvocation> {
    if sender.is_some() && sender == manager.name_owner().as_deref() {
        Some(invocation)
    } else {
        invocation.return_dbus_error(
            "org.freedesktop.DBus.Error.AccessDenied",
            "Only iwd may call this agent",
        );
        None
    }
}

/// Calls `f` whenever iwd starts or stops.
pub fn on_owner_change(
    manager: &gio::DBusObjectManagerClient,
    f: impl Fn() + 'static,
) -> glib::SignalHandlerId {
    // This binding demands Send + Sync even though the signal is delivered
    // on the main thread; ThreadGuard panics if that ever changes.
    let f = glib::thread_guard::ThreadGuard::new(f);
    manager.connect_name_owner_notify(move |_| (f.get_ref())())
}

/// Calls `f` whenever anything in iwd changes: objects or interfaces come
/// and go, a property changes, or iwd starts or stops. Callers rebuild their
/// view from the cache rather than tracking individual objects.
pub fn on_any_change(manager: &gio::DBusObjectManagerClient, f: impl Fn() + Clone + 'static) {
    let g = f.clone();
    manager.connect_object_added(move |_, _| g());
    let g = f.clone();
    manager.connect_object_removed(move |_, _| g());
    let g = f.clone();
    manager.connect_interface_added(move |_, _, _| g());
    let g = f.clone();
    manager.connect_interface_removed(move |_, _, _| g());
    // This binding demands Send + Sync even though the signal is delivered
    // on the main thread; ThreadGuard panics if that ever changes.
    let g = glib::thread_guard::ThreadGuard::new(f.clone());
    manager.connect_interface_proxy_properties_changed(move |_, _, _, _, _| (g.get_ref())());
    on_owner_change(manager, f);
}

#[cfg(test)]
mod tests {
    use super::signal_level;

    /// iwd picks the first threshold the reading reaches, so a reading on a
    /// threshold belongs to the stronger level.
    #[test]
    fn levels_follow_thresholds() {
        assert_eq!(signal_level(-37), 0);
        assert_eq!(signal_level(-60), 0);
        assert_eq!(signal_level(-61), 1);
        assert_eq!(signal_level(-67), 1);
        assert_eq!(signal_level(-74), 2);
        assert_eq!(signal_level(-81), 3);
        assert_eq!(signal_level(-82), 4);
        assert_eq!(signal_level(-120), 4);
    }
}
