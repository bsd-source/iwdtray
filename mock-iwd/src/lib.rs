//! A fake iwd for tests. It starts a private `dbus-daemon`, owns
//! `net.connman.iwd` on it and serves the objects of a typical laptop: an
//! adapter, `wlan0` in station mode connected to Lighthouse, CornerCafe in
//! range, one saved network and the agent
//! manager. It never touches the real system bus.
//!
//! The fake runs on its own thread with its own main loop, so code under test
//! may block on it. Tests drive it through [`MockIwd`]'s methods and read back
//! the calls it received with [`MockIwd::calls`].

// Test support: panicking is the failure report, and its API is for tests only.
#![allow(
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::doc_markdown
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gio::glib::{self, Variant, VariantTy};
use gio::prelude::*;

pub const ADAPTER: &str = "/net/connman/iwd/0";
pub const DEVICE: &str = "/net/connman/iwd/0/3";
pub const HOME: &str = "/net/connman/iwd/0/3/lighthouse_psk";
pub const CAFE: &str = "/net/connman/iwd/0/3/cornercafe_psk";
pub const KNOWN_HOME: &str = "/net/connman/iwd/lighthouse_psk";
pub const ROOT: &str = "/net/connman/iwd";
/// The phone the fake reports as joined to a running hotspot, in the
/// uppercase form iwd prints MAC addresses in.
pub const HOTSPOT_CLIENT: &str = "6A:1F:2B:3C:4D:5E";
/// A hidden access point the fake reports in range: (address, signal in
/// hundredths of a dBm, security type).
pub const HIDDEN_AP: (&str, i16, &str) = ("12:34:56:78:9A:BC", -6100, "psk");

const NAME: &str = "net.connman.iwd";
const PREFIX: &str = "net.connman.iwd.";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";

/// Introspection for the parts of iwd's API that iwdtray uses.
const XML: &str = r#"
<node>
  <interface name="org.freedesktop.DBus.ObjectManager">
    <method name="GetManagedObjects">
      <arg name="objects" type="a{oa{sa{sv}}}" direction="out"/>
    </method>
    <signal name="InterfacesAdded"><arg type="o"/><arg type="a{sa{sv}}"/></signal>
    <signal name="InterfacesRemoved"><arg type="o"/><arg type="as"/></signal>
  </interface>
  <interface name="net.connman.iwd.AgentManager">
    <method name="RegisterAgent"><arg name="path" type="o" direction="in"/></method>
    <method name="UnregisterAgent"><arg name="path" type="o" direction="in"/></method>
  </interface>
  <interface name="net.connman.iwd.Adapter">
    <property name="Powered" type="b" access="readwrite"/>
    <property name="Name" type="s" access="read"/>
    <property name="Model" type="s" access="read"/>
    <property name="Vendor" type="s" access="read"/>
    <property name="SupportedModes" type="as" access="read"/>
  </interface>
  <interface name="net.connman.iwd.Device">
    <property name="Name" type="s" access="read"/>
    <property name="Address" type="s" access="read"/>
    <property name="Powered" type="b" access="readwrite"/>
    <property name="Adapter" type="o" access="read"/>
    <property name="Mode" type="s" access="readwrite"/>
  </interface>
  <interface name="net.connman.iwd.Station">
    <method name="Scan"/>
    <method name="Disconnect"/>
    <method name="GetOrderedNetworks">
      <arg name="networks" type="a(on)" direction="out"/>
    </method>
    <method name="ConnectHiddenNetwork"><arg name="name" type="s" direction="in"/></method>
    <method name="GetHiddenAccessPoints">
      <arg name="aps" type="a(sns)" direction="out"/>
    </method>
    <method name="RegisterSignalLevelAgent">
      <arg name="path" type="o" direction="in"/>
      <arg name="levels" type="an" direction="in"/>
    </method>
    <property name="State" type="s" access="read"/>
    <property name="Scanning" type="b" access="read"/>
    <property name="ConnectedNetwork" type="o" access="read"/>
    <property name="ConnectedAccessPoint" type="o" access="read"/>
  </interface>
  <interface name="net.connman.iwd.BasicServiceSet">
    <property name="Address" type="s" access="read"/>
  </interface>
  <interface name="net.connman.iwd.StationDiagnostic">
    <method name="GetDiagnostics"><arg name="info" type="a{sv}" direction="out"/></method>
  </interface>
  <interface name="net.connman.iwd.Network">
    <method name="Connect"/>
    <property name="Name" type="s" access="read"/>
    <property name="Type" type="s" access="read"/>
    <property name="Connected" type="b" access="read"/>
    <property name="Device" type="o" access="read"/>
    <property name="KnownNetwork" type="o" access="read"/>
    <property name="ExtendedServiceSet" type="ao" access="read"/>
  </interface>
  <interface name="net.connman.iwd.KnownNetwork">
    <method name="Forget"/>
    <property name="Name" type="s" access="read"/>
    <property name="Type" type="s" access="read"/>
    <property name="Hidden" type="b" access="read"/>
    <property name="AutoConnect" type="b" access="readwrite"/>
    <property name="LastConnectedTime" type="s" access="read"/>
  </interface>
  <interface name="net.connman.iwd.AccessPointDiagnostic">
    <method name="GetDiagnostics"><arg name="clients" type="aa{sv}" direction="out"/></method>
  </interface>
  <interface name="net.connman.iwd.AccessPoint">
    <method name="Start">
      <arg name="ssid" type="s" direction="in"/>
      <arg name="psk" type="s" direction="in"/>
    </method>
    <method name="Stop"/>
    <property name="Started" type="b" access="read"/>
    <property name="Name" type="s" access="read"/>
  </interface>
</node>"#;

/// Properties of one interface, by name. Keys starting with `_` are the
/// fake's own bookkeeping and are never sent over D-Bus.
type Props = BTreeMap<String, Variant>;

/// What a test asks the fake's thread to do.
enum Cmd {
    Set(String, String, String, Variant),
    Unset(String, String, String),
    AddInterface(String, String, Props),
    RemoveInterface(String, String),
    StopIwd,
    StartIwd,
    SwitchMode(String),
    CallAgent(String, Variant, mpsc::Sender<Result<Variant, String>>),
    SignalLevel(u8),
    Quit,
}

pub struct MockIwd {
    address: String,
    daemon: Child,
    tx: mpsc::Sender<Cmd>,
    calls: Arc<Mutex<Vec<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl MockIwd {
    /// Starts a private bus and the fake iwd on it, ready to answer.
    pub fn start() -> Self {
        // The bus runs under a small shell that stops it as soon as our end
        // of its stdin closes. That happens when we drop the handle, and also
        // when this process crashes or is killed, so no bus is ever orphaned.
        let mut daemon = Command::new("sh")
            .args([
                "-c",
                "dbus-daemon --session --nofork --print-address=1 & bus=$!; \
                 read -r _; kill $bus",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("dbus-daemon must be installed to run these tests");
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().expect("piped stdout"))
            .read_line(&mut address)
            .expect("dbus-daemon prints its address");
        let address = address.trim().to_owned();
        assert!(
            !address.is_empty(),
            "dbus-daemon didn't start; it must be installed to run these tests"
        );

        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let thread_calls = Arc::clone(&calls);
        let thread_address = address.clone();
        let thread = std::thread::spawn(move || {
            run(&thread_address, &rx, &ready_tx, thread_calls);
        });
        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("fake iwd starts");
        Self {
            address,
            daemon,
            tx,
            calls,
            thread: Some(thread),
        }
    }

    /// The private bus's address, for pointing another process at it.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// A new client connection to the private bus, for code under test.
    /// Call it from the thread (and thread-default context) that uses it.
    pub fn connect(&self) -> gio::DBusConnection {
        gio::DBusConnection::for_address_sync(
            &self.address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            gio::Cancellable::NONE,
        )
        .expect("connect to the private bus")
    }

    /// Every call and property write the fake received, in order, as
    /// `Interface.Method(args)` or `Interface.Property=value`.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls lock").clone()
    }

    pub fn set(&self, path: &str, iface: &str, prop: &str, value: impl ToVariant) {
        self.send(Cmd::Set(
            path.into(),
            full(iface),
            prop.into(),
            value.to_variant(),
        ));
    }

    pub fn unset(&self, path: &str, iface: &str, prop: &str) {
        self.send(Cmd::Unset(path.into(), full(iface), prop.into()));
    }

    pub fn add_interface(&self, path: &str, iface: &str, props: &[(&str, Variant)]) {
        let props = props
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect();
        self.send(Cmd::AddInterface(path.into(), full(iface), props));
    }

    pub fn remove_interface(&self, path: &str, iface: &str) {
        self.send(Cmd::RemoveInterface(path.into(), full(iface)));
    }

    /// Releases `net.connman.iwd`, as when iwd exits.
    pub fn stop_iwd(&self) {
        self.send(Cmd::StopIwd);
    }

    /// Takes `net.connman.iwd` again after [`MockIwd::stop_iwd`], as a
    /// restarted iwd would, with no agents registered.
    pub fn start_iwd(&self) {
        self.send(Cmd::StartIwd);
    }

    /// Calls a method on the agent iwdtray registered, as iwd would. The reply
    /// arrives on the returned channel (`Err` holds the D-Bus error name).
    /// It doesn't block, because the agent under test usually runs on the
    /// caller's thread and needs it free; see [`wait_for`].
    pub fn call_agent(
        &self,
        method: &str,
        args: Variant,
    ) -> mpsc::Receiver<Result<Variant, String>> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(Cmd::CallAgent(method.into(), args, reply_tx));
        reply_rx
    }

    /// Switches the device's mode as iwd does (Station and AccessPoint come
    /// and go), without logging a call.
    pub fn switch_mode(&self, mode: &str) {
        self.send(Cmd::SwitchMode(mode.into()));
    }

    /// Sends a signal level (0 strongest, 4 weakest) to the registered
    /// signal level agent.
    pub fn signal_level(&self, level: u8) {
        self.send(Cmd::SignalLevel(level));
    }

    fn send(&self, cmd: Cmd) {
        self.tx.send(cmd).expect("fake iwd thread is running");
    }
}

impl Drop for MockIwd {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // Closing stdin tells the shell to stop the bus, then it exits.
        drop(self.daemon.stdin.take());
        let _ = self.daemon.wait();
    }
}

fn full(iface: &str) -> String {
    if iface.contains('.') {
        iface.to_owned()
    } else {
        format!("{PREFIX}{iface}")
    }
}

fn not_connected() -> Fail {
    (
        "net.connman.iwd.NotConnected".into(),
        "Not connected".into(),
    )
}

fn short(iface: &str) -> &str {
    iface.strip_prefix(PREFIX).unwrap_or(iface)
}

/// Iterates `ctx` until `cond` holds or `timeout` passes; returns `cond()`.
/// For tests' own (thread-default) context.
pub fn run_until(
    ctx: &glib::MainContext,
    timeout: Duration,
    mut cond: impl FnMut() -> bool,
) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if cond() {
            return true;
        }
        if !ctx.iteration(false) {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    cond()
}

/// Iterates `ctx` until `rx` delivers a value, or `timeout` passes.
pub fn wait_for<T>(
    ctx: &glib::MainContext,
    timeout: Duration,
    rx: &mpsc::Receiver<T>,
) -> Option<T> {
    let mut got = None;
    run_until(ctx, timeout, || {
        if got.is_none() {
            got = rx.try_recv().ok();
        }
        got.is_some()
    });
    got
}

/// Runs `test` with a fresh thread-default main context, which every gio
/// object it creates will use for callbacks.
pub fn with_context(test: impl FnOnce(&glib::MainContext)) {
    let ctx = glib::MainContext::new();
    ctx.with_thread_default(|| test(&ctx))
        .expect("acquire a fresh main context");
}

// ---------------------------------------------------------------------------
// The fake's thread.

struct Fake {
    conn: gio::DBusConnection,
    node: gio::DBusNodeInfo,
    objects: RefCell<BTreeMap<String, BTreeMap<String, Props>>>,
    registrations: RefCell<HashMap<(String, String), gio::RegistrationId>>,
    calls: Arc<Mutex<Vec<String>>>,
    /// (bus name, object path) of the registered agent and signal agent.
    agent: RefCell<Option<(String, String)>>,
    signal_agent: RefCell<Option<(String, String)>>,
}

fn run(
    address: &str,
    rx: &mpsc::Receiver<Cmd>,
    ready: &mpsc::Sender<()>,
    calls: Arc<Mutex<Vec<String>>>,
) {
    let ctx = glib::MainContext::new();
    ctx.with_thread_default(|| {
        let conn = gio::DBusConnection::for_address_sync(
            address,
            gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
                | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
            None,
            gio::Cancellable::NONE,
        )
        .expect("fake iwd connects");
        let fake = Rc::new(Fake {
            node: gio::DBusNodeInfo::for_xml(XML).expect("valid XML"),
            conn,
            objects: RefCell::new(fixture()),
            registrations: RefCell::default(),
            calls,
            agent: RefCell::default(),
            signal_agent: RefCell::default(),
        });
        fake.register_object_manager();
        let paths: Vec<(String, String)> = fake
            .objects
            .borrow()
            .iter()
            .flat_map(|(p, ifaces)| ifaces.keys().map(|i| (p.clone(), i.clone())))
            .collect();
        for (path, iface) in paths {
            fake.register(&path, &iface);
        }
        fake.request_name();
        ready.send(()).expect("test is waiting");

        loop {
            while ctx.iteration(false) {}
            match rx.recv_timeout(Duration::from_millis(2)) {
                Ok(Cmd::Quit) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(cmd) => fake.apply(cmd),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    })
    .expect("fake iwd main context");
}

/// The starting state: a typical laptop connected at home.
fn fixture() -> BTreeMap<String, BTreeMap<String, Props>> {
    let o = |p: &str| {
        glib::variant::ObjectPath::try_from(p)
            .expect("valid path")
            .to_variant()
    };
    let s = |v: &str| v.to_variant();
    let bss = |network: &str, id: &str| {
        glib::variant::ObjectPath::try_from(format!("{network}/{id}")).expect("valid path")
    };
    let mut objects: BTreeMap<String, BTreeMap<String, Props>> = BTreeMap::new();
    let mut add = |path: &str, iface: &str, props: Vec<(&str, Variant)>| {
        objects.entry(path.to_owned()).or_default().insert(
            full(iface),
            props.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        );
    };
    add(ROOT, "AgentManager", vec![]);
    add(
        ADAPTER,
        "Adapter",
        vec![
            ("Powered", true.to_variant()),
            ("Name", s("phy0")),
            ("Model", s("AX201")),
            ("Vendor", s("Intel Corporation")),
            (
                "SupportedModes",
                vec!["station", "ap", "ad-hoc"].to_variant(),
            ),
        ],
    );
    add(
        DEVICE,
        "Device",
        vec![
            ("Name", s("wlan0")),
            ("Address", s("02:00:00:00:00:10")),
            ("Powered", true.to_variant()),
            ("Adapter", o(ADAPTER)),
            ("Mode", s("station")),
        ],
    );
    add(DEVICE, "Station", station_props("connected", Some(HOME)));
    add(DEVICE, "StationDiagnostic", vec![]);
    add(
        HOME,
        "Network",
        vec![
            ("Name", s("Lighthouse")),
            ("Type", s("psk")),
            ("Connected", true.to_variant()),
            ("Device", o(DEVICE)),
            ("KnownNetwork", o(KNOWN_HOME)),
            // Two radios, like a dual-band router.
            (
                "ExtendedServiceSet",
                vec![bss(HOME, "020000000101"), bss(HOME, "020000000102")].to_variant(),
            ),
            ("_signal", (-4600i16).to_variant()),
        ],
    );
    add(
        CAFE,
        "Network",
        vec![
            ("Name", s("CornerCafe")),
            ("Type", s("psk")),
            ("Connected", false.to_variant()),
            ("Device", o(DEVICE)),
            (
                "ExtendedServiceSet",
                vec![bss(CAFE, "c0ffee000001")].to_variant(),
            ),
            ("_signal", (-7400i16).to_variant()),
        ],
    );
    add(
        KNOWN_HOME,
        "KnownNetwork",
        vec![
            ("Name", s("Lighthouse")),
            ("Type", s("psk")),
            ("Hidden", false.to_variant()),
            ("AutoConnect", true.to_variant()),
            ("LastConnectedTime", s("2026-01-15T09:30:00Z")),
        ],
    );
    add_radios(&mut objects);
    objects
}

/// The networks' radios (BSS objects), and which one the station is on: the
/// second of Lighthouse's two, so tests can tell "the radio we're on"
/// from "the first radio".
fn add_radios(objects: &mut BTreeMap<String, BTreeMap<String, Props>>) {
    let o = |p: &str| {
        glib::variant::ObjectPath::try_from(p)
            .expect("valid path")
            .to_variant()
    };
    if let Some(station) = objects
        .get_mut(DEVICE)
        .and_then(|o| o.get_mut(&full("Station")))
    {
        station.insert(
            "ConnectedAccessPoint".into(),
            o(&format!("{HOME}/020000000102")),
        );
    }
    for (network, id, address) in [
        (HOME, "020000000101", "02:00:00:00:01:01"),
        (HOME, "020000000102", "02:00:00:00:01:02"),
        (CAFE, "c0ffee000001", "C0:FF:EE:00:00:01"),
    ] {
        objects
            .entry(format!("{network}/{id}"))
            .or_default()
            .insert(
                full("BasicServiceSet"),
                [("Address".to_owned(), address.to_variant())].into(),
            );
    }
}

fn station_props(state: &str, connected: Option<&str>) -> Vec<(&'static str, Variant)> {
    let mut props = vec![
        ("State", state.to_variant()),
        ("Scanning", false.to_variant()),
    ];
    if let Some(path) = connected {
        props.push((
            "ConnectedNetwork",
            glib::variant::ObjectPath::try_from(path)
                .expect("valid path")
                .to_variant(),
        ));
    }
    props
}

/// A D-Bus error to return: (error name, message).
type Fail = (String, String);

impl Fake {
    fn log(&self, entry: String) {
        self.calls.lock().expect("calls lock").push(entry);
    }

    fn request_name(&self) {
        self.conn
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "RequestName",
                Some(&(NAME, 0u32).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
            )
            .expect("own net.connman.iwd");
    }

    fn register_object_manager(self: &Rc<Self>) {
        let info = self.node.lookup_interface(OBJECT_MANAGER).expect("in XML");
        let weak = Rc::downgrade(self);
        self.conn
            .register_object("/", &info)
            .method_call(move |_, _, _, _, _, _, invocation| {
                if let Some(fake) = weak.upgrade() {
                    let objects = fake.managed_objects();
                    invocation.return_value(Some(&Variant::tuple_from_iter([objects])));
                }
            })
            .build()
            .expect("register ObjectManager");
    }

    fn register(self: &Rc<Self>, path: &str, iface: &str) {
        let info = self.node.lookup_interface(iface).expect("interface in XML");
        let (get, set, call) = (
            Rc::downgrade(self),
            Rc::downgrade(self),
            Rc::downgrade(self),
        );
        let id = self
            .conn
            .register_object(path, &info)
            .method_call(move |_, sender, path, iface, method, params, invocation| {
                let Some(fake) = call.upgrade() else { return };
                let iface = iface.unwrap_or_default();
                match fake.handle(sender.unwrap_or_default(), path, iface, method, &params) {
                    Ok(reply) => invocation.return_value(reply.as_ref()),
                    Err((name, msg)) => invocation.return_dbus_error(&name, &msg),
                }
            })
            .property(move |_, _, path, iface, prop| {
                get.upgrade()
                    .and_then(|fake| fake.prop(path, iface, prop))
                    .unwrap_or_else(|| empty_value(property_type(prop)))
            })
            .set_property(move |_, _, path, iface, prop, value| {
                set.upgrade()
                    .is_some_and(|fake| fake.write(path, iface, prop, value))
            })
            .build()
            .expect("register interface");
        self.registrations
            .borrow_mut()
            .insert((path.to_owned(), iface.to_owned()), id);
    }

    fn unregister(&self, path: &str, iface: &str) {
        if let Some(id) = self
            .registrations
            .borrow_mut()
            .remove(&(path.to_owned(), iface.to_owned()))
        {
            let _ = self.conn.unregister_object(id);
        }
    }

    fn prop(&self, path: &str, iface: &str, prop: &str) -> Option<Variant> {
        self.objects
            .borrow()
            .get(path)?
            .get(iface)?
            .get(prop)
            .cloned()
    }

    fn public(props: &Props) -> HashMap<String, Variant> {
        props
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn managed_objects(&self) -> Variant {
        let entries: Vec<Variant> = self
            .objects
            .borrow()
            .iter()
            .map(|(path, ifaces)| {
                let ifaces: HashMap<String, HashMap<String, Variant>> = ifaces
                    .iter()
                    .map(|(i, props)| (i.clone(), Self::public(props)))
                    .collect();
                let path = glib::variant::ObjectPath::try_from(path.as_str()).expect("valid path");
                glib::variant::DictEntry::new(path, ifaces).to_variant()
            })
            .collect();
        Variant::array_from_iter_with_type(
            VariantTy::new("{oa{sa{sv}}}").expect("valid type"),
            entries,
        )
    }

    fn emit(&self, path: &str, iface: &str, signal: &str, args: &Variant) {
        let _ = self.conn.emit_signal(None, path, iface, signal, Some(args));
    }

    /// Sets a property and, if that changed it, tells clients, as iwd does.
    fn set(&self, path: &str, iface: &str, prop: &str, value: Variant) {
        let mut objects = self.objects.borrow_mut();
        let Some(props) = objects.get_mut(path).and_then(|o| o.get_mut(iface)) else {
            return;
        };
        if props.insert(prop.to_owned(), value.clone()).as_ref() == Some(&value) {
            return;
        }
        drop(objects);
        let changed: HashMap<String, Variant> = [(prop.to_owned(), value)].into();
        self.emit(
            path,
            PROPERTIES,
            "PropertiesChanged",
            &(iface, changed, Vec::<String>::new()).to_variant(),
        );
    }

    fn unset(&self, path: &str, iface: &str, prop: &str) {
        if let Some(props) = self
            .objects
            .borrow_mut()
            .get_mut(path)
            .and_then(|o| o.get_mut(iface))
        {
            props.remove(prop);
        }
        self.emit(
            path,
            PROPERTIES,
            "PropertiesChanged",
            &(
                iface,
                HashMap::<String, Variant>::new(),
                vec![prop.to_owned()],
            )
                .to_variant(),
        );
    }

    fn add_interface(self: &Rc<Self>, path: &str, iface: &str, props: Props) {
        // Replaced, not exported twice, which GDBus refuses.
        self.remove_interface(path, iface);
        let public = Self::public(&props);
        self.objects
            .borrow_mut()
            .entry(path.to_owned())
            .or_default()
            .insert(iface.to_owned(), props);
        self.register(path, iface);
        let added: HashMap<String, HashMap<String, Variant>> = [(iface.to_owned(), public)].into();
        let path_v = glib::variant::ObjectPath::try_from(path).expect("valid path");
        self.emit(
            "/",
            OBJECT_MANAGER,
            "InterfacesAdded",
            &(path_v, added).to_variant(),
        );
    }

    fn remove_interface(&self, path: &str, iface: &str) {
        let removed = {
            let mut objects = self.objects.borrow_mut();
            let removed = objects
                .get_mut(path)
                .and_then(|o| o.remove(iface))
                .is_some();
            if objects.get(path).is_some_and(BTreeMap::is_empty) {
                objects.remove(path);
            }
            removed
        };
        if !removed {
            return;
        }
        self.unregister(path, iface);
        let path_v = glib::variant::ObjectPath::try_from(path).expect("valid path");
        self.emit(
            "/",
            OBJECT_MANAGER,
            "InterfacesRemoved",
            &(path_v, vec![iface.to_owned()]).to_variant(),
        );
    }

    fn apply(self: &Rc<Self>, cmd: Cmd) {
        match cmd {
            Cmd::Set(p, i, k, v) => self.set(&p, &i, &k, v),
            Cmd::Unset(p, i, k) => self.unset(&p, &i, &k),
            Cmd::AddInterface(p, i, props) => self.add_interface(&p, &i, props),
            Cmd::RemoveInterface(p, i) => self.remove_interface(&p, &i),
            Cmd::StartIwd => self.request_name(),
            Cmd::SwitchMode(mode) => {
                self.set(DEVICE, &full("Device"), "Mode", mode.to_variant());
                self.switch_mode(DEVICE, &mode);
            }
            Cmd::StopIwd => {
                // A new iwd knows nothing of the old one's agents.
                self.agent.borrow_mut().take();
                self.signal_agent.borrow_mut().take();
                let _ = self.conn.call_sync(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    "org.freedesktop.DBus",
                    "ReleaseName",
                    Some(&(NAME,).to_variant()),
                    None,
                    gio::DBusCallFlags::NONE,
                    -1,
                    gio::Cancellable::NONE,
                );
            }
            Cmd::CallAgent(method, args, reply) => {
                let Some((name, path)) = self.agent.borrow().clone() else {
                    let _ = reply.send(Err("no agent registered".into()));
                    return;
                };
                self.conn.call(
                    Some(&name),
                    &path,
                    "net.connman.iwd.Agent",
                    &method,
                    Some(&args),
                    None,
                    gio::DBusCallFlags::NONE,
                    // Long enough for a prompt; tests choose their own wait.
                    60_000,
                    gio::Cancellable::NONE,
                    move |result| {
                        let _ = reply.send(result.map_err(|e| {
                            gio::DBusError::remote_error(&e)
                                .map_or_else(|| e.to_string(), Into::into)
                        }));
                    },
                );
            }
            Cmd::SignalLevel(level) => {
                if let Some((name, path)) = self.signal_agent.borrow().clone() {
                    let device = glib::variant::ObjectPath::try_from(DEVICE).expect("valid path");
                    self.conn.call(
                        Some(&name),
                        &path,
                        "net.connman.iwd.SignalLevelAgent",
                        "Changed",
                        Some(&(device, level).to_variant()),
                        None,
                        gio::DBusCallFlags::NONE,
                        -1,
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                }
            }
            Cmd::Quit => {}
        }
    }

    fn write(self: &Rc<Self>, path: &str, iface: &str, prop: &str, value: Variant) -> bool {
        self.log(format!("{}.{prop}={}", short(iface), value.print(false)));
        match (short(iface), prop) {
            ("Device", "Mode") => {
                // Like iwd, a device that is off can't change mode.
                if self.prop(path, iface, "Powered").and_then(|p| p.get()) == Some(false) {
                    return false;
                }
                let mode = value.str().unwrap_or_default().to_owned();
                let current = self.prop(path, iface, prop);
                self.set(path, iface, prop, value);
                // Like iwd, setting the mode it is already in changes nothing.
                if current.as_ref().and_then(Variant::str) != Some(mode.as_str()) {
                    self.switch_mode(path, &mode);
                }
            }
            ("Device", "Powered") => {
                // Like iwd, writing the current value changes nothing.
                if self.prop(path, iface, prop).as_ref() == Some(&value) {
                    return true;
                }
                let on = value.get::<bool>() == Some(true);
                self.set(path, iface, prop, value);
                // Like iwd, the mode's interfaces exist only while powered.
                if on {
                    let mode = self.prop(path, iface, "Mode");
                    self.switch_mode(
                        path,
                        mode.as_ref().and_then(Variant::str).unwrap_or_default(),
                    );
                } else {
                    self.switch_mode(path, "");
                }
            }
            _ => self.set(path, iface, prop, value),
        }
        true
    }

    /// iwd replaces Station with AccessPoint (and back) when Mode changes,
    /// with their diagnostics; any other `mode` (powered off) leaves neither.
    /// A station's signal-level agent goes with it.
    fn switch_mode(self: &Rc<Self>, path: &str, mode: &str) {
        for iface in [
            "Station",
            "StationDiagnostic",
            "AccessPoint",
            "AccessPointDiagnostic",
        ] {
            self.remove_interface(path, &full(iface));
        }
        self.signal_agent.borrow_mut().take();
        // Without its station, no network is connected any more.
        let network = full("Network");
        let networks: Vec<String> = self
            .objects
            .borrow()
            .iter()
            .filter(|(_, ifaces)| ifaces.contains_key(&network))
            .map(|(p, _)| p.clone())
            .collect();
        for p in networks {
            self.set(&p, &network, "Connected", false.to_variant());
        }
        match mode {
            "ap" => self.add_interface(
                path,
                &full("AccessPoint"),
                [("Started".to_owned(), false.to_variant())].into(),
            ),
            "station" => {
                let props = station_props("disconnected", None)
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v))
                    .collect();
                self.add_interface(path, &full("Station"), props);
                self.add_interface(path, &full("StationDiagnostic"), Props::new());
            }
            _ => {}
        }
    }

    /// Drops the station's link; `false` if it had none.
    fn disconnect(&self, path: &str) -> bool {
        let station = full("Station");
        let Some(network) = self.prop(path, &station, "ConnectedNetwork") else {
            return false;
        };
        if let Some(network) = network.str() {
            self.set(network, &full("Network"), "Connected", false.to_variant());
        }
        self.set(path, &station, "State", "disconnected".to_variant());
        self.unset(path, &station, "ConnectedNetwork");
        self.unset(path, &station, "ConnectedAccessPoint");
        true
    }

    fn start_ap(self: &Rc<Self>, path: &str, iface: &str, params: &Variant) -> Result<(), Fail> {
        if self.prop(path, iface, "Started").and_then(|v| v.get()) == Some(true) {
            return Err((
                "net.connman.iwd.AlreadyExists".into(),
                "Already started".into(),
            ));
        }
        let ssid = params.child_value(0).str().unwrap_or_default().to_owned();
        let psk_len = params.child_value(1).str().map_or(0, str::len);
        if !(8..=63).contains(&psk_len) {
            return Err((
                "net.connman.iwd.InvalidArguments".into(),
                "Bad passphrase".into(),
            ));
        }
        self.set(path, iface, "Started", true.to_variant());
        self.set(path, iface, "Name", ssid.to_variant());
        // iwd adds the diagnostics interface once the AP is up.
        self.add_interface(path, &full("AccessPointDiagnostic"), Props::new());
        Ok(())
    }

    fn forget(self: &Rc<Self>, path: &str, iface: &str) {
        self.remove_interface(path, iface);
        // Like iwd, networks it described stop pointing at it.
        let network = full("Network");
        let described: Vec<String> = self
            .objects
            .borrow()
            .iter()
            .filter(|(_, ifaces)| {
                ifaces
                    .get(&network)
                    .and_then(|p| p.get("KnownNetwork"))
                    .and_then(Variant::str)
                    == Some(path)
            })
            .map(|(p, _)| p.clone())
            .collect();
        for p in described {
            self.unset(&p, &network, "KnownNetwork");
            // Like iwd, forgetting the network in use disconnects.
            if self.prop(&p, &network, "Connected").and_then(|v| v.get()) == Some(true) {
                self.disconnect(DEVICE);
            }
        }
    }

    fn handle(
        self: &Rc<Self>,
        sender: &str,
        path: &str,
        iface: &str,
        method: &str,
        params: &Variant,
    ) -> Result<Option<Variant>, Fail> {
        let args: Vec<String> = params.iter().map(|v| v.print(false).to_string()).collect();
        self.log(format!("{}.{method}({})", short(iface), args.join(", ")));
        let path_arg = || params.child_value(0).str().unwrap_or_default().to_owned();
        match (short(iface), method) {
            ("AgentManager", "RegisterAgent") => {
                *self.agent.borrow_mut() = Some((sender.to_owned(), path_arg()));
            }
            ("AgentManager", "UnregisterAgent") => {
                self.agent.borrow_mut().take();
            }
            ("Station", "RegisterSignalLevelAgent") => {
                // One per station, as in iwd.
                if self.signal_agent.borrow().is_some() {
                    return Err((
                        "net.connman.iwd.AlreadyExists".into(),
                        "Already exists".into(),
                    ));
                }
                *self.signal_agent.borrow_mut() = Some((sender.to_owned(), path_arg()));
            }
            ("Station", "GetOrderedNetworks") => return Ok(Some(self.ordered_networks(path))),
            ("Station", "GetHiddenAccessPoints") => {
                let (address, signal, kind) = HIDDEN_AP;
                return Ok(Some((vec![(address, signal, kind)],).to_variant()));
            }
            ("Station", "Disconnect") => {
                if !self.disconnect(path) {
                    return Err(not_connected());
                }
            }
            ("Station", "ConnectHiddenNetwork") => {
                return Err((
                    "net.connman.iwd.NotFound".into(),
                    "No hidden network with that name".into(),
                ));
            }
            ("StationDiagnostic", "GetDiagnostics") => {
                if self
                    .prop(path, &full("Station"), "ConnectedNetwork")
                    .is_none()
                {
                    return Err(not_connected());
                }
                let d: HashMap<String, Variant> = [
                    ("RSSI".to_owned(), (-46i16).to_variant()),
                    ("Frequency".to_owned(), 5785u32.to_variant()),
                    ("Channel".to_owned(), 157u16.to_variant()),
                ]
                .into();
                return Ok(Some((d,).to_variant()));
            }
            ("AccessPoint", "Start") => self.start_ap(path, iface, params)?,
            ("AccessPoint", "Stop") => {
                self.remove_interface(path, &full("AccessPointDiagnostic"));
                self.set(path, iface, "Started", false.to_variant());
                self.unset(path, iface, "Name");
            }
            ("AccessPointDiagnostic", "GetDiagnostics") => {
                let client: HashMap<String, Variant> = [
                    ("Address".to_owned(), HOTSPOT_CLIENT.to_variant()),
                    ("RSSI".to_owned(), (-52i16).to_variant()),
                    ("RxBitrate".to_owned(), 1300u32.to_variant()),
                    ("ConnectedTime".to_owned(), 42u32.to_variant()),
                ]
                .into();
                return Ok(Some((vec![client],).to_variant()));
            }
            ("KnownNetwork", "Forget") => self.forget(path, iface),
            _ => {}
        }
        Ok(None)
    }

    fn ordered_networks(&self, device: &str) -> Variant {
        let objects = self.objects.borrow();
        let mut networks: Vec<(String, i16)> = objects
            .iter()
            .filter(|(p, _)| p.starts_with(&format!("{device}/")))
            .filter_map(|(p, ifaces)| {
                let signal = ifaces.get(&full("Network"))?.get("_signal")?.get::<i16>()?;
                Some((p.clone(), signal))
            })
            .collect();
        networks.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
        let entries: Vec<(glib::variant::ObjectPath, i16)> = networks
            .into_iter()
            .map(|(p, s)| {
                (
                    glib::variant::ObjectPath::try_from(p.as_str()).expect("valid path"),
                    s,
                )
            })
            .collect();
        (entries,).to_variant()
    }
}

/// A property's D-Bus type, from `XML` (no two interfaces there give one
/// property name different types).
fn property_type(name: &str) -> &'static str {
    let tag = format!("<property name=\"{name}\" type=\"");
    XML.find(&tag)
        .and_then(|i| {
            let rest = &XML[i + tag.len()..];
            rest.find('"').map(|end| &rest[..end])
        })
        .unwrap_or("s")
}

/// A value of D-Bus type `signature` for a property that isn't set, so
/// `Get` and `GetAll` still pass GDBus's type check.
fn empty_value(signature: &str) -> Variant {
    match signature {
        "b" => false.to_variant(),
        "o" => glib::variant::ObjectPath::try_from("/")
            .expect("valid path")
            .to_variant(),
        "as" => Vec::<String>::new().to_variant(),
        "ao" => Vec::<glib::variant::ObjectPath>::new().to_variant(),
        _ => "".to_variant(),
    }
}

#[cfg(test)]
mod tests {
    use super::{empty_value, property_type};

    #[test]
    fn unset_properties_keep_their_type() {
        assert_eq!(property_type("ConnectedNetwork"), "o");
        assert_eq!(property_type("Powered"), "b");
        assert_eq!(empty_value("o").type_().as_str(), "o");
        assert_eq!(empty_value(property_type("Name")).str(), Some(""));
    }
}
