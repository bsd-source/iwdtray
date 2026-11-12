//! Watches iwd on the system bus and reduces its state to a [`Status`].
//!
//! Rather than tracking individual objects (the source of most lifetime bugs
//! in the C version), every change iwd reports triggers a full recompute
//! from the object manager's cache, which holds only a handful of objects.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gio::glib;
use gio::prelude::*;

use iwdtray_common::iwd::{
    self as shared, DEFAULT_TIMEOUT, IFACE_ADHOC, IFACE_AP, IFACE_DIAGNOSTIC, IFACE_SIGNAL_AGENT,
    IFACE_STATION, SIGNAL_THRESHOLDS, prop_bool, prop_str,
};

use crate::export::Exported;
use crate::status::Status;

/// Our signal level agent's object path on the system bus connection.
const SIGNAL_AGENT_PATH: &str = "/io/github/bsd_source/iwdtray/SignalLevelAgent";

const SIGNAL_AGENT_XML: &str = r#"
<node>
  <interface name="net.connman.iwd.SignalLevelAgent">
    <method name="Release"><arg name="device" type="o"/></method>
    <method name="Changed"><arg name="device" type="o"/><arg name="level" type="y"/></method>
  </interface>
</node>"#;

/// Keeps iwd under watch for as long as it is alive.
pub struct Monitor {
    inner: Rc<Inner>,
    _signal_agent: Exported,
}

struct Inner {
    manager: gio::DBusObjectManagerClient,
    on_change: Box<dyn Fn(&Status)>,
    last: RefCell<Option<Status>>,
    /// The station the icon follows, if any.
    followed: RefCell<Option<Followed>>,
    /// Numbers each diagnostics request, so a late reply for an earlier
    /// link is ignored.
    next_ask: Cell<u64>,
}

/// What we know about the followed station. Replaced when the station
/// changes or goes away, so one that reappears (iwd restart, mode change)
/// is registered with again.
struct Followed {
    path: String,
    /// The link's signal level, from the agent or the diagnostics.
    level: Option<u8>,
    /// The diagnostics request in flight for the link's starting level.
    asking: Option<u64>,
}

impl Monitor {
    /// Starts watching iwd. `on_change` is called immediately with the current
    /// status and again whenever it changes.
    pub fn new(on_change: impl Fn(&Status) + 'static) -> Result<Self, glib::Error> {
        Self::with_manager(shared::manager()?, on_change)
    }

    /// Like [`Monitor::new`], with a given object manager (tests use a fake
    /// iwd on a private bus).
    pub fn with_manager(
        manager: gio::DBusObjectManagerClient,
        on_change: impl Fn(&Status) + 'static,
    ) -> Result<Self, glib::Error> {
        let inner = Rc::new(Inner {
            manager,
            on_change: Box::new(on_change),
            last: RefCell::new(None),
            followed: RefCell::new(None),
            next_ask: Cell::new(0),
        });

        let signal_agent = export_signal_agent(&inner)?;
        let weak = Rc::downgrade(&inner);
        shared::on_any_change(&inner.manager, move || {
            if let Some(inner) = weak.upgrade() {
                inner.update();
            }
        });
        inner.update();

        Ok(Self {
            inner,
            _signal_agent: signal_agent,
        })
    }

    /// The object manager mirroring iwd, shared with the agent.
    pub fn manager(&self) -> &gio::DBusObjectManagerClient {
        &self.inner.manager
    }
}

fn export_signal_agent(inner: &Rc<Inner>) -> Result<Exported, glib::Error> {
    let weak: Weak<Inner> = Rc::downgrade(inner);
    Exported::new(
        &inner.manager.connection(),
        SIGNAL_AGENT_PATH,
        SIGNAL_AGENT_XML,
        IFACE_SIGNAL_AGENT,
        |b| {
            b.method_call(move |_, sender, _, _, method, params, invocation| {
                let Some(inner) = weak.upgrade() else {
                    return invocation.return_value(None);
                };
                // Only iwd itself may report signal levels.
                let Some(invocation) = shared::only_iwd(&inner.manager, sender, invocation) else {
                    return;
                };
                invocation.return_value(None);
                let station = params.child_value(0);
                let level = match method {
                    "Changed" => params.child_value(1).get::<u8>(),
                    // The station is going away; it is registered with
                    // again if it comes back.
                    _ => None,
                };
                inner.set_level(station.str().unwrap_or_default(), level);
            })
        },
    )
}

impl Inner {
    fn update(self: &Rc<Self>) {
        let status = self.compute();
        if self.last.borrow().as_ref() != Some(&status) {
            self.last.replace(Some(status.clone()));
            (self.on_change)(&status);
        }
    }

    fn compute(self: &Rc<Self>) -> Status {
        match self.station() {
            Ok(station) => self.station_status(&station),
            Err(status) => {
                self.followed.replace(None);
                status
            }
        }
    }

    /// The followed device's station, or the status when it has none.
    fn station(&self) -> Result<gio::DBusProxy, Status> {
        if self.manager.name_owner().is_none() {
            return Err(Status::IwdNotRunning);
        }
        // One tray icon for now: follow the first device, ordered by path.
        let device = shared::first_device(&self.manager).ok_or(Status::NoDevice)?;
        let adapter_powered =
            shared::adapter(&self.manager, &device).and_then(|a| prop_bool(&a, "Powered"));
        if adapter_powered == Some(false) {
            return Err(Status::AdapterOff);
        }
        if prop_bool(&device, "Powered") == Some(false) {
            return Err(Status::DeviceOff);
        }
        let path = device.object_path();
        if let Some(station) = self.proxy(&path, IFACE_STATION) {
            return Ok(station);
        }
        if let Some(ap) = self.proxy(&path, IFACE_AP) {
            return Err(Status::AccessPoint {
                started: prop_bool(&ap, "Started") == Some(true),
            });
        }
        if let Some(adhoc) = self.proxy(&path, IFACE_ADHOC) {
            return Err(Status::AdHoc {
                started: prop_bool(&adhoc, "Started") == Some(true),
            });
        }
        // Mode is switching; the next interface-added event fills this in.
        Err(Status::Disconnected)
    }

    fn station_status(self: &Rc<Self>, station: &gio::DBusProxy) -> Status {
        let path = station.object_path();
        let new = self
            .followed
            .borrow()
            .as_ref()
            .is_none_or(|f| f.path != path.as_str());
        if new {
            self.followed.replace(Some(Followed {
                path: path.to_string(),
                level: None,
                asking: None,
            }));
            register_signal_agent(station);
        }
        let ssid = || shared::connected_ssid(&self.manager, station);
        let state = prop_str(station, "State").unwrap_or_default();
        if shared::is_connected(&state) {
            let level = self.followed.borrow().as_ref().and_then(|f| f.level);
            if level.is_none() {
                self.ask_level(station);
            }
            return Status::Connected {
                ssid: ssid(),
                level,
            };
        }
        // A level belongs to one link. iwd reports one only when the
        // signal crosses a threshold, so the next link's starting level is
        // read from its diagnostics instead.
        if let Some(f) = self.followed.borrow_mut().as_mut() {
            f.level = None;
            f.asking = None;
        }
        if state == "connecting" {
            Status::Connecting { ssid: ssid() }
        } else {
            Status::Disconnected
        }
    }

    fn proxy(&self, path: &str, iface: &str) -> Option<gio::DBusProxy> {
        shared::proxy(&self.manager, path, iface)
    }

    /// Records the followed station's signal level (`None` to forget it)
    /// and shows it.
    fn set_level(self: &Rc<Self>, station: &str, level: Option<u8>) {
        let changed = match self.followed.borrow_mut().as_mut() {
            Some(f) if f.path == station => {
                f.level = level;
                f.asking = None;
                true
            }
            _ => false,
        };
        if changed {
            self.update();
        }
    }

    /// Reads a newly connected station's signal from its diagnostics, for
    /// the icon until iwd's agent reports a change.
    fn ask_level(self: &Rc<Self>, station: &gio::DBusProxy) {
        let path = station.object_path().to_string();
        // The interface can lag the connection; the next update retries.
        let Some(diagnostic) = self.proxy(&path, IFACE_DIAGNOSTIC) else {
            return;
        };
        let id = self.next_ask.get() + 1;
        {
            let mut followed = self.followed.borrow_mut();
            let Some(f) = followed.as_mut().filter(|f| f.asking.is_none()) else {
                return;
            };
            f.asking = Some(id);
        }
        self.next_ask.set(id);
        let weak = Rc::downgrade(self);
        shared::fetch(&diagnostic, "GetDiagnostics", "a{sv}", move |dict| {
            let rssi = dict.and_then(|d| {
                glib::VariantDict::new(Some(&d))
                    .lookup::<i16>("RSSI")
                    .ok()
                    .flatten()
            });
            let Some(inner) = weak.upgrade() else { return };
            // Only for the request still in flight: the link may have
            // dropped, or the agent reported a level, meanwhile.
            let current = inner
                .followed
                .borrow()
                .as_ref()
                .is_some_and(|f| f.asking == Some(id));
            if !current {
                return;
            }
            match rssi {
                Some(rssi) => inner.set_level(&path, Some(shared::signal_level(rssi))),
                // Failed: ask again on the next update, or the icon would
                // show no level until iwd's agent reports a change.
                None => {
                    if let Some(f) = inner.followed.borrow_mut().as_mut() {
                        f.asking = None;
                    }
                }
            }
        });
    }
}

/// Asks `station` to report signal level changes to our agent.
fn register_signal_agent(station: &gio::DBusProxy) {
    let path = station.object_path();
    let args = (
        glib::variant::ObjectPath::try_from(SIGNAL_AGENT_PATH).expect("valid path"),
        SIGNAL_THRESHOLDS.to_vec(),
    );
    shared::call(
        station,
        "RegisterSignalLevelAgent",
        Some(&args.to_variant()),
        DEFAULT_TIMEOUT,
        move |result| {
            // Without an agent the icon still shows the connection state,
            // just not the signal strength. Not retried until the station
            // reappears, to avoid hammering iwd.
            if let Err(e) = result {
                eprintln!("Signal level agent not registered for {path}: {e}");
            }
        },
    );
}

/// Against a fake iwd on a private bus (see the `mock-iwd` crate).
#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use gio::glib;
    use gio::prelude::*;
    use mock_iwd::{ADAPTER, DEVICE, HOME, MockIwd, run_until, with_context};

    use super::Monitor;
    use crate::status::Status;

    const WAIT: Duration = Duration::from_secs(5);

    type Seen = Rc<RefCell<Vec<Status>>>;

    fn watch(iwd: &MockIwd) -> (Monitor, Seen) {
        let seen: Seen = Rc::default();
        let s = Rc::clone(&seen);
        let manager = iwdtray_common::iwd::manager_on(&iwd.connect()).expect("object manager");
        let monitor =
            Monitor::with_manager(manager, move |status| s.borrow_mut().push(status.clone()))
                .expect("monitor starts");
        (monitor, seen)
    }

    fn reaches(ctx: &glib::MainContext, seen: &Seen, want: &Status) -> bool {
        run_until(ctx, WAIT, || seen.borrow().last() == Some(want))
    }

    fn connected(level: Option<u8>) -> Status {
        Status::Connected {
            ssid: Some("Lighthouse".into()),
            level,
        }
    }

    #[test]
    fn shows_the_connection_and_its_signal_level() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let (_monitor, seen) = watch(&iwd);
            // The starting level comes from the diagnostics (-46 dBm).
            assert!(
                reaches(ctx, &seen, &connected(Some(0))),
                "got {:?}",
                seen.borrow()
            );

            // The tray registers for signal levels, then follows them.
            assert!(run_until(ctx, WAIT, || iwd
                .calls()
                .iter()
                .any(|c| c.starts_with("Station.RegisterSignalLevelAgent"))));
            iwd.signal_level(1);
            assert!(
                reaches(ctx, &seen, &connected(Some(1))),
                "got {:?}",
                seen.borrow()
            );
        });
    }

    #[test]
    fn follows_connecting_and_disconnecting() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let (_monitor, seen) = watch(&iwd);
            assert!(reaches(ctx, &seen, &connected(Some(0))));
            assert!(run_until(ctx, WAIT, || iwd
                .calls()
                .iter()
                .any(|c| c.starts_with("Station.RegisterSignalLevelAgent"))));
            iwd.signal_level(1);
            assert!(reaches(ctx, &seen, &connected(Some(1))));
            iwd.set(DEVICE, "Station", "State", "connecting");
            let connecting = Status::Connecting {
                ssid: Some("Lighthouse".into()),
            };
            assert!(reaches(ctx, &seen, &connecting), "got {:?}", seen.borrow());
            iwd.set(DEVICE, "Station", "State", "disconnected");
            assert!(reaches(ctx, &seen, &Status::Disconnected));
            // The next link's level is read afresh, not carried over.
            iwd.set(DEVICE, "Station", "State", "connected");
            assert!(
                reaches(ctx, &seen, &connected(Some(0))),
                "got {:?}",
                seen.borrow()
            );
        });
    }

    #[test]
    fn reports_the_device_and_adapter_turned_off() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let (_monitor, seen) = watch(&iwd);
            iwd.set(DEVICE, "Device", "Powered", false);
            assert!(
                reaches(ctx, &seen, &Status::DeviceOff),
                "got {:?}",
                seen.borrow()
            );
            iwd.set(ADAPTER, "Adapter", "Powered", false);
            assert!(
                reaches(ctx, &seen, &Status::AdapterOff),
                "got {:?}",
                seen.borrow()
            );
        });
    }

    #[test]
    fn shows_a_running_hotspot() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let (_monitor, seen) = watch(&iwd);
            assert!(reaches(ctx, &seen, &connected(Some(0))));
            iwd.remove_interface(DEVICE, "Station");
            iwd.add_interface(DEVICE, "AccessPoint", &[("Started", true.to_variant())]);
            assert!(
                reaches(ctx, &seen, &Status::AccessPoint { started: true }),
                "got {:?}",
                seen.borrow()
            );
        });
    }

    #[test]
    fn follows_the_signal_again_after_a_hotspot() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let (_monitor, seen) = watch(&iwd);
            let registrations = || {
                iwd.calls()
                    .iter()
                    .filter(|c| c.starts_with("Station.RegisterSignalLevelAgent"))
                    .count()
            };
            assert!(reaches(ctx, &seen, &connected(Some(0))));
            assert!(run_until(ctx, WAIT, || registrations() == 1));
            // iwd drops the signal agent along with the station.
            iwd.switch_mode("ap");
            assert!(reaches(ctx, &seen, &Status::AccessPoint { started: false }));
            iwd.switch_mode("station");
            iwd.set(
                DEVICE,
                "Station",
                "ConnectedNetwork",
                glib::variant::ObjectPath::try_from(HOME).expect("valid path"),
            );
            iwd.set(DEVICE, "Station", "State", "connected");
            assert!(
                reaches(ctx, &seen, &connected(Some(0))),
                "got {:?}",
                seen.borrow()
            );
            assert!(run_until(ctx, WAIT, || registrations() == 2));
            iwd.signal_level(3);
            assert!(reaches(ctx, &seen, &connected(Some(3))));
        });
    }

    #[test]
    fn notices_iwd_stopping() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let (_monitor, seen) = watch(&iwd);
            assert!(reaches(ctx, &seen, &connected(Some(0))));
            iwd.stop_iwd();
            assert!(
                reaches(ctx, &seen, &Status::IwdNotRunning),
                "got {:?}",
                seen.borrow()
            );
            // Back again: the status returns and the signal agent is
            // registered with the new iwd.
            iwd.start_iwd();
            assert!(
                reaches(ctx, &seen, &connected(Some(0))),
                "got {:?}",
                seen.borrow()
            );
            let registrations = || {
                iwd.calls()
                    .iter()
                    .filter(|c| c.starts_with("Station.RegisterSignalLevelAgent"))
                    .count()
            };
            assert!(run_until(ctx, WAIT, || registrations() == 2));
        });
    }
}
