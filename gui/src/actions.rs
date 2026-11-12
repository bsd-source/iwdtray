//! What the window asks iwd to do, kept free of GTK so it can be tested
//! against a fake iwd. Each action reports once, through `done`.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib::{self, Variant};
use zeroize::Zeroizing;

use crate::iwd;

/// How long to wait for iwd to bring up the access point after a mode change.
const AP_TIMEOUT_SECONDS: u32 = 10;

type Done = Box<dyn FnOnce(Result<(), glib::Error>)>;
type Start = Box<dyn FnOnce(&gio::DBusProxy, Done)>;
/// A hotspot start waiting for the access point to appear.
type Waiting = Rc<RefCell<Option<(Done, Start)>>>;

fn unit(result: Result<Variant, glib::Error>) -> Result<(), glib::Error> {
    result.map(|_| ())
}

fn missing(what: &str) -> glib::Error {
    glib::Error::new(
        gio::IOErrorEnum::NotFound,
        &format!("{what} is not available"),
    )
}

pub fn connect(
    manager: &gio::DBusObjectManagerClient,
    network: &str,
    done: impl FnOnce(Result<(), glib::Error>) + 'static,
) {
    match iwd::proxy(manager, network, iwd::IFACE_NETWORK) {
        Some(net) => iwd::call(&net, "Connect", None, iwd::NO_TIMEOUT, move |r| {
            done(unit(r));
        }),
        None => done(Err(missing("That network"))),
    }
}

pub fn disconnect(
    manager: &gio::DBusObjectManagerClient,
    device: &str,
    done: impl FnOnce(Result<(), glib::Error>) + 'static,
) {
    match iwd::proxy(manager, device, iwd::IFACE_STATION) {
        Some(station) => iwd::call(
            &station,
            "Disconnect",
            None,
            iwd::DEFAULT_TIMEOUT,
            move |r| done(unit(r)),
        ),
        None => done(Err(missing("Wi-Fi"))),
    }
}

/// Finds and joins a network that doesn't broadcast its name. iwd asks the
/// tray's agent for a password if the network needs one.
pub fn join_hidden(
    manager: &gio::DBusObjectManagerClient,
    device: &str,
    name: &str,
    done: impl FnOnce(Result<(), glib::Error>) + 'static,
) {
    match iwd::proxy(manager, device, iwd::IFACE_STATION) {
        Some(station) => {
            let args = (name,).to_variant();
            iwd::call(
                &station,
                "ConnectHiddenNetwork",
                Some(&args),
                iwd::NO_TIMEOUT,
                move |r| {
                    done(unit(r));
                },
            );
        }
        None => done(Err(missing("Wi-Fi"))),
    }
}

/// Removes a saved network's profile, password included.
pub fn forget(
    manager: &gio::DBusObjectManagerClient,
    known: &str,
    done: impl FnOnce(Result<(), glib::Error>) + 'static,
) {
    match iwd::proxy(manager, known, iwd::IFACE_KNOWN) {
        Some(k) => iwd::call(&k, "Forget", None, iwd::DEFAULT_TIMEOUT, move |r| {
            done(unit(r));
        }),
        None => done(Err(missing("That saved network"))),
    }
}

pub fn set_auto_connect(
    manager: &gio::DBusObjectManagerClient,
    known: &str,
    on: bool,
    done: impl FnOnce(Result<(), glib::Error>) + 'static,
) {
    match iwd::proxy(manager, known, iwd::IFACE_KNOWN) {
        Some(k) => iwd::set_property(&k, "AutoConnect", &on.to_variant(), done),
        None => done(Err(missing("That saved network"))),
    }
}

/// Starts a hotspot on `device`: switches it to AP mode if needed, waits for
/// iwd to expose the access point, then starts it with `ssid` and `password`.
pub fn start_hotspot(
    manager: &gio::DBusObjectManagerClient,
    device: &str,
    ssid: String,
    password: Zeroizing<String>,
    done: impl FnOnce(Result<(), glib::Error>) + 'static,
) {
    start_hotspot_within(manager, device, ssid, password, AP_TIMEOUT_SECONDS, done);
}

/// [`start_hotspot`], giving up if the access point hasn't appeared after
/// `timeout` seconds.
fn start_hotspot_within(
    manager: &gio::DBusObjectManagerClient,
    device: &str,
    ssid: String,
    password: Zeroizing<String>,
    timeout: u32,
    done: impl FnOnce(Result<(), glib::Error>) + 'static,
) {
    let start = move |ap: &gio::DBusProxy, done: Done| {
        let args = (ssid.as_str(), password.as_str()).to_variant();
        iwd::call(ap, "Start", Some(&args), iwd::DEFAULT_TIMEOUT, move |r| {
            done(unit(r));
        });
    };
    if let Some(ap) = iwd::proxy(manager, device, iwd::IFACE_AP) {
        return start(&ap, Box::new(done));
    }
    let Some(dev) = iwd::proxy(manager, device, iwd::IFACE_DEVICE) else {
        return done(Err(missing("Wi-Fi")));
    };

    // `done` and the waiting state are shared by three paths (access point
    // appears, mode change fails, timeout); whichever comes first wins.
    let waiting: Waiting = Rc::new(RefCell::new(Some((Box::new(done), Box::new(start)))));
    let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
    let finish = {
        let (manager, handler) = (manager.clone(), Rc::clone(&handler));
        move || {
            if let Some(id) = handler.borrow_mut().take() {
                manager.disconnect(id);
            }
        }
    };

    let id = manager.connect_interface_added({
        let (waiting, finish, device) = (Rc::clone(&waiting), finish.clone(), device.to_owned());
        move |_, object, iface| {
            let Ok(ap) = iface.clone().downcast::<gio::DBusProxy>() else {
                return;
            };
            if object.object_path() != device || ap.interface_name() != iwd::IFACE_AP {
                return;
            }
            if let Some((done, start)) = waiting.borrow_mut().take() {
                finish();
                start(&ap, done);
            }
        }
    });
    *handler.borrow_mut() = Some(id);

    // On the caller's own (thread-default) main context, like the D-Bus
    // replies. glib::timeout_add_*_local attach to the global default
    // context instead, which made parallel tests fight over it.
    glib::MainContext::ref_thread_default().spawn_local({
        let (waiting, finish) = (Rc::clone(&waiting), finish.clone());
        async move {
            glib::timeout_future_seconds(timeout).await;
            if let Some((done, _)) = waiting.borrow_mut().take() {
                finish();
                done(Err(glib::Error::new(
                    gio::IOErrorEnum::TimedOut,
                    "The device didn't switch to hotspot mode.",
                )));
            }
        }
    });

    iwd::set_property(&dev, "Mode", &"ap".to_variant(), move |result| {
        if let Err(e) = result
            && let Some((done, _)) = waiting.borrow_mut().take()
        {
            finish();
            done(Err(e));
        }
    });
}

/// Stops the hotspot if it is running, then returns `device` to station
/// mode, where iwd reconnects to a saved network by itself.
pub fn stop_hotspot(
    manager: &gio::DBusObjectManagerClient,
    device: &str,
    done: impl FnOnce(Result<(), glib::Error>) + 'static,
) {
    let Some(dev) = iwd::proxy(manager, device, iwd::IFACE_DEVICE) else {
        return done(Err(missing("Wi-Fi")));
    };
    let to_station = move |done: Done| {
        iwd::set_property(&dev, "Mode", &"station".to_variant(), done);
    };
    let ap = iwd::proxy(manager, device, iwd::IFACE_AP)
        .filter(|ap| iwd::prop_bool(ap, "Started") == Some(true));
    match ap {
        // Return to station mode even if Stop fails, so Wi-Fi comes back.
        Some(ap) => iwd::call(&ap, "Stop", None, iwd::DEFAULT_TIMEOUT, move |stopped| {
            to_station(Box::new(move |r| done(unit(stopped).and(r))));
        }),
        None => to_station(Box::new(done)),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use gtk::gio;
    use gtk::glib;
    use gtk::prelude::*;
    use mock_iwd::{CAFE, DEVICE, KNOWN_HOME, MockIwd, run_until, with_context};
    use zeroize::Zeroizing;

    use crate::iwd;

    const WAIT: Duration = Duration::from_secs(5);

    type Outcome = Rc<RefCell<Option<Result<(), glib::Error>>>>;

    fn outcome() -> (Outcome, impl FnOnce(Result<(), glib::Error>) + 'static) {
        let o: Outcome = Rc::default();
        let set = Rc::clone(&o);
        (o, move |r| *set.borrow_mut() = Some(r))
    }

    fn finished(ctx: &glib::MainContext, o: &Outcome) -> Result<(), glib::Error> {
        assert!(
            run_until(ctx, WAIT, || o.borrow().is_some()),
            "action never finished"
        );
        o.borrow_mut().take().expect("outcome")
    }

    fn manager(iwd: &MockIwd) -> gio::DBusObjectManagerClient {
        iwd::manager_on(&iwd.connect()).expect("object manager")
    }

    /// The calls the fake received, from the first one matching `from`.
    fn calls_from(iwd: &MockIwd, from: &str) -> Vec<String> {
        let calls = iwd.calls();
        let start = calls
            .iter()
            .position(|c| c.starts_with(from))
            .unwrap_or(calls.len());
        calls[start..].to_vec()
    }

    #[test]
    fn starting_a_hotspot_switches_mode_then_starts_it() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);
            let (o, done) = outcome();
            super::start_hotspot(
                &m,
                DEVICE,
                "MyHotspot".into(),
                Zeroizing::new("hunter22".into()),
                done,
            );
            finished(ctx, &o).expect("hotspot starts");
            assert_eq!(
                calls_from(&iwd, "Device.Mode"),
                [
                    "Device.Mode='ap'",
                    "AccessPoint.Start('MyHotspot', 'hunter22')"
                ]
            );
            let ap = iwd::proxy(&m, DEVICE, iwd::IFACE_AP).expect("access point");
            assert!(run_until(ctx, WAIT, || iwd::prop_bool(&ap, "Started") == Some(true)));
            assert_eq!(iwd::prop_str(&ap, "Name").as_deref(), Some("MyHotspot"));
        });
    }

    #[test]
    fn a_failed_mode_change_reports_once() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);
            iwd.set(DEVICE, "Device", "Powered", false);
            let device = iwd::proxy(&m, DEVICE, iwd::IFACE_DEVICE).expect("device");
            assert!(run_until(ctx, WAIT, || iwd::prop_bool(&device, "Powered")
                == Some(false)));
            let reports = Rc::new(std::cell::Cell::new(0));
            let (o, done) = outcome();
            let r = Rc::clone(&reports);
            super::start_hotspot(
                &m,
                DEVICE,
                "MyHotspot".into(),
                Zeroizing::new("hunter22".into()),
                move |result| {
                    r.set(r.get() + 1);
                    done(result);
                },
            );
            finished(ctx, &o).expect_err("device is off");
            assert!(!iwd.calls().iter().any(|c| c.starts_with("AccessPoint.")));
            // Nothing else (a late access point, the timeout's guard) reports.
            run_until(ctx, Duration::from_millis(300), || false);
            assert_eq!(reports.get(), 1);
        });
    }

    #[test]
    fn gives_up_when_the_access_point_never_appears() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);
            // Mode already reads "ap" but no AccessPoint exists, so setting
            // it changes nothing and nothing appears.
            iwd.set(DEVICE, "Device", "Mode", "ap");
            let device = iwd::proxy(&m, DEVICE, iwd::IFACE_DEVICE).expect("device");
            assert!(run_until(ctx, WAIT, || iwd::prop_str(&device, "Mode")
                .as_deref()
                == Some("ap")));
            let (o, done) = outcome();
            super::start_hotspot_within(
                &m,
                DEVICE,
                "MyHotspot".into(),
                Zeroizing::new("hunter22".into()),
                1,
                done,
            );
            let err = finished(ctx, &o).expect_err("times out");
            assert!(err.matches(gio::IOErrorEnum::TimedOut), "got {err}");
            assert!(!iwd.calls().iter().any(|c| c.starts_with("AccessPoint.")));
        });
    }

    // The window's D-Bus readers in iwd.rs, against the same fake.

    #[test]
    fn networks_report_their_access_points_and_mac() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);
            let station = iwd::proxy(&m, DEVICE, iwd::IFACE_STATION).expect("station");
            let got: Rc<RefCell<Option<Vec<iwd::Network>>>> = Rc::default();
            let g = Rc::clone(&got);
            iwd::ordered_networks(&m, &station, move |n| *g.borrow_mut() = Some(n));
            assert!(run_until(ctx, WAIT, || got.borrow().is_some()));
            let seen: Vec<(String, usize, Option<String>)> = got
                .borrow_mut()
                .take()
                .expect("networks")
                .into_iter()
                .map(|n| (n.ssid, n.access_points, n.address))
                .collect();
            assert_eq!(
                seen,
                [
                    // The radio we're connected to, not just the first one.
                    (
                        "Lighthouse".to_owned(),
                        2,
                        Some("02:00:00:00:01:02".to_owned())
                    ),
                    (
                        "CornerCafe".to_owned(),
                        1,
                        Some("c0:ff:ee:00:00:01".to_owned())
                    ),
                ]
            );
        });
    }

    #[test]
    fn hidden_access_points_are_listed() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);
            let station = iwd::proxy(&m, DEVICE, iwd::IFACE_STATION).expect("station");
            let got: Rc<RefCell<Option<Vec<iwd::HiddenAp>>>> = Rc::default();
            let g = Rc::clone(&got);
            iwd::hidden_access_points(&station, move |aps| *g.borrow_mut() = Some(aps));
            assert!(run_until(ctx, WAIT, || got.borrow().is_some()));
            let (address, signal, _) = mock_iwd::HIDDEN_AP;
            assert_eq!(
                got.borrow_mut().take().expect("list"),
                [iwd::HiddenAp {
                    address: address.to_ascii_lowercase(),
                    dbm: signal / 100,
                    security: "PSK",
                }]
            );
        });
    }

    #[test]
    fn a_running_hotspot_lists_its_clients() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);
            let (o, done) = outcome();
            super::start_hotspot(
                &m,
                DEVICE,
                "MyHotspot".into(),
                Zeroizing::new("hunter22".into()),
                done,
            );
            finished(ctx, &o).expect("hotspot starts");
            assert!(run_until(ctx, WAIT, || iwd::proxy(
                &m,
                DEVICE,
                iwd::IFACE_AP_DIAGNOSTIC
            )
            .is_some()));

            let diag = iwd::proxy(&m, DEVICE, iwd::IFACE_AP_DIAGNOSTIC).expect("diagnostics");
            let got: Rc<RefCell<Option<Vec<iwd::Client>>>> = Rc::default();
            let g = Rc::clone(&got);
            iwd::hotspot_clients(&diag, move |c| *g.borrow_mut() = Some(c));
            assert!(run_until(ctx, WAIT, || got.borrow().is_some()));
            let clients = got.borrow_mut().take().expect("clients");
            assert_eq!(
                clients,
                [iwd::Client {
                    // Lowercased, to match the ARP table's form.
                    address: mock_iwd::HOTSPOT_CLIENT.to_ascii_lowercase(),
                    rssi: Some(-52),
                    connected_secs: Some(42),
                }]
            );
        });
    }

    #[test]
    fn stopping_a_hotspot_returns_to_wi_fi() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);
            let (o, done) = outcome();
            super::start_hotspot(
                &m,
                DEVICE,
                "MyHotspot".into(),
                Zeroizing::new("hunter22".into()),
                done,
            );
            finished(ctx, &o).expect("hotspot starts");
            let ap = iwd::proxy(&m, DEVICE, iwd::IFACE_AP).expect("access point");
            assert!(run_until(ctx, WAIT, || iwd::prop_bool(&ap, "Started") == Some(true)));

            let (o, done) = outcome();
            super::stop_hotspot(&m, DEVICE, done);
            finished(ctx, &o).expect("hotspot stops");
            assert_eq!(
                calls_from(&iwd, "AccessPoint.Stop"),
                ["AccessPoint.Stop()", "Device.Mode='station'"]
            );
            assert!(run_until(ctx, WAIT, || iwd::proxy(
                &m,
                DEVICE,
                iwd::IFACE_STATION
            )
            .is_some()));
            assert!(iwd::proxy(&m, DEVICE, iwd::IFACE_AP).is_none());
        });
    }

    #[test]
    fn stop_still_returns_to_wi_fi_after_a_failed_start() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);
            // Stuck in AP mode with nothing running, e.g. after Start failed.
            iwd.set(DEVICE, "Device", "Mode", "ap");
            iwd.remove_interface(DEVICE, "Station");
            iwd.add_interface(DEVICE, "AccessPoint", &[("Started", false.to_variant())]);
            assert!(run_until(ctx, WAIT, || iwd::proxy(
                &m,
                DEVICE,
                iwd::IFACE_AP
            )
            .is_some()));

            let (o, done) = outcome();
            super::stop_hotspot(&m, DEVICE, done);
            finished(ctx, &o).expect("back to station");
            assert_eq!(calls_from(&iwd, "AccessPoint.Stop"), Vec::<String>::new());
            assert_eq!(calls_from(&iwd, "Device.Mode"), ["Device.Mode='station'"]);
        });
    }

    #[test]
    fn forgetting_removes_the_saved_network() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);
            assert!(iwd::proxy(&m, KNOWN_HOME, iwd::IFACE_KNOWN).is_some());
            let (o, done) = outcome();
            super::forget(&m, KNOWN_HOME, done);
            finished(ctx, &o).expect("forgotten");
            assert!(iwd.calls().contains(&"KnownNetwork.Forget()".to_owned()));
        });
    }

    #[test]
    fn the_auto_connect_switch_sets_auto_connect() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);
            let (o, done) = outcome();
            super::set_auto_connect(&m, KNOWN_HOME, false, done);
            finished(ctx, &o).expect("set");
            assert!(
                iwd.calls()
                    .contains(&"KnownNetwork.AutoConnect=false".to_owned())
            );
        });
    }

    #[test]
    fn connect_disconnect_and_hidden_networks() {
        with_context(|ctx| {
            let iwd = MockIwd::start();
            let m = manager(&iwd);

            let (o, done) = outcome();
            super::connect(&m, CAFE, done);
            finished(ctx, &o).expect("connect");
            assert!(iwd.calls().contains(&"Network.Connect()".to_owned()));

            let (o, done) = outcome();
            super::disconnect(&m, DEVICE, done);
            finished(ctx, &o).expect("disconnect");
            let station = iwd::proxy(&m, DEVICE, iwd::IFACE_STATION).expect("station");
            assert!(run_until(ctx, WAIT, || iwd::prop_str(&station, "State")
                .as_deref()
                == Some("disconnected")));

            let (o, done) = outcome();
            super::join_hidden(&m, DEVICE, "iwdtray-test-no-such-network", done);
            let err = finished(ctx, &o).expect_err("not found");
            assert_eq!(
                iwd::describe_connect_error(&err),
                "No hidden network with that name is in range. Check the name and try again."
            );
        });
    }
}
