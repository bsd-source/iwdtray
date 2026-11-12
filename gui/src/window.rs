//! The window: tabs, live link telemetry, and a table per tab.
//!
//! As in the tray, iwd's state is not tracked object by object: any change
//! queues one refresh that rebuilds the view from the object manager's
//! cache, plus the station's network lists, which iwd only gives on request.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;
use gtk::prelude::*;

use zeroize::Zeroizing;

use crate::actions;
use crate::arp;
use crate::config::{self, WindowConfig};
use crate::format;
use crate::hidden;
use crate::hotspot;
use crate::iwd::{self, Network, Saved};
use crate::saved;
use crate::style;
use crate::widgets::{self, TABLE_GAP, label};

const APP_ID: &str = "io.github.bsd_source.iwdtray";
const POLL_SECONDS: u32 = 2;
/// How often Saved's "5 min ago" times are brought up to date.
const SAVED_TIMES_SECONDS: u32 = 30;
const NEARBY_EMPTY: &str = "No networks found. Click Rescan to look again.";
/// What the Nearby and Hidden lists say in hotspot mode, where iwd can't scan.
const HOTSPOT_EMPTY: &str = "The hotspot has Wi-Fi to itself. Stop it to see networks again.";
/// What the Nearby and Hidden lists say while the device is off.
const DEVICE_OFF_EMPTY: &str = "Network interface is disabled or down.";
const RADIO_OFF_EMPTY: &str = "The wireless radio is switched off (rfkill). Unblock it to scan.";
/// How long Forget waits for its confirming second click.
const FORGET_CONFIRM_SECONDS: u32 = 4;
/// How long a finished message (an error, "Forgot …") stays on screen.
const STATUS_SECONDS: u32 = 8;

pub fn run() -> glib::ExitCode {
    let app = gtk::Application::builder().application_id(APP_ID).build();
    let failed = Rc::new(Cell::new(false));
    let f = Rc::clone(&failed);
    app.connect_activate(move |app| {
        // A second launch (e.g. another tray click) raises the open window.
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        match iwd::manager() {
            Ok(manager) => Console::open(app, manager),
            Err(e) => {
                eprintln!("Cannot reach iwd on the system bus: {e}");
                f.set(true);
            }
        }
    });
    // Our own arguments were already parsed in main.
    let code = app.run_with_args::<&str>(&[]);
    if failed.get() {
        glib::ExitCode::FAILURE
    } else {
        code
    }
}

/// The window's widgets, top to bottom.
struct Ui {
    window: gtk::ApplicationWindow,
    device: gtk::Label,
    power: gtk::Switch,
    theme: gtk::Button,
    tabs: Tabs,
    tele: Telemetry,
    table: Table,
    actions: Actions,
}

struct Tabs {
    nearby: gtk::ToggleButton,
    hidden: gtk::ToggleButton,
    saved: gtk::ToggleButton,
    hotspot: gtk::ToggleButton,
}

/// The network name, its state, and the link readouts.
struct Telemetry {
    ssid: gtk::Label,
    live: gtk::Label,
    readouts: [gtk::Label; format::READOUTS],
    /// The readouts' grid, shown only on Nearby while not in hotspot mode.
    grid: gtk::Grid,
    /// The name and state row above it, whose rule goes with the grid.
    top: gtk::Box,
}

/// The filter entry, one page per tab, and the status line.
struct Table {
    filter: gtk::SearchEntry,
    stack: gtk::Stack,
    nearby_list: gtk::ListBox,
    /// The message Nearby shows while it has no rows.
    nearby_empty: gtk::Label,
    nearby_columns: [gtk::SizeGroup; 4],
    hidden: hidden::Page,
    saved_list: gtk::ListBox,
    /// The message Saved shows while it has no rows.
    saved_empty: gtk::Label,
    saved_columns: saved::Columns,
    hotspot: hotspot::Page,
    status: gtk::Label,
}

/// The action buttons along the bottom.
struct Actions {
    connect: gtk::Button,
    disconnect: gtk::Button,
    rescan: gtk::Button,
    forget: gtk::Button,
    start: gtk::Button,
    stop: gtk::Button,
    filter: gtk::Button,
}

struct Console {
    manager: gio::DBusObjectManagerClient,
    ui: Ui,
    networks: RefCell<Vec<Network>>,
    saved: RefCell<Vec<Saved>>,
    /// Each saved row's LAST USED label, in list order.
    saved_times: RefCell<Vec<gtk::Label>>,
    /// Saved network whose Forget is waiting for the confirming click.
    forget_armed: RefCell<Option<String>>,
    /// Bumped on every status change, so a stale timer can't clear a newer one.
    status_serial: Cell<u64>,
    /// Bumped each time Forget is armed, likewise for its timer.
    forget_serial: Cell<u64>,
    /// Whether the device is in AP mode, and whether its hotspot is up.
    in_ap_mode: Cell<bool>,
    hotspot_up: Cell<bool>,
    /// Station path while connected, for the diagnostics poll.
    connected_station: RefCell<Option<String>>,
    refresh_queued: Cell<bool>,
    /// Whether the device is in station mode, and whether it is scanning.
    has_station: Cell<bool>,
    scanning: Cell<bool>,
    /// The device is on and its adapter can run an access point.
    can_host: Cell<bool>,
    /// The filter text, lowercased.
    filter_text: RefCell<String>,
    /// Set while the view updates the power switch, so it isn't sent back.
    syncing_power: Cell<bool>,
    css: gtk::CssProvider,
    /// The palettes and size from iwdtray.conf.
    config: WindowConfig,
    dark: Cell<bool>,
}

impl Console {
    fn open(app: &gtk::Application, manager: gio::DBusObjectManagerClient) {
        let config = WindowConfig::load();
        let dark = config.dark;
        let css = style::install(config.palette(dark), dark);

        let ui = build_ui(app, &config);
        let console = Rc::new(Self {
            manager,
            ui,
            networks: RefCell::default(),
            saved: RefCell::default(),
            saved_times: RefCell::default(),
            forget_armed: RefCell::default(),
            status_serial: Cell::new(0),
            forget_serial: Cell::new(0),
            in_ap_mode: Cell::new(false),
            hotspot_up: Cell::new(false),
            connected_station: RefCell::default(),
            refresh_queued: Cell::new(false),
            has_station: Cell::new(false),
            scanning: Cell::new(false),
            can_host: Cell::new(false),
            filter_text: RefCell::default(),
            syncing_power: Cell::new(false),
            css,
            config,
            dark: Cell::new(dark),
        });
        console.apply_theme();
        console.connect();
        console.refresh();
        // iwd only lists networks from its latest scan, which may be minutes
        // old while connected, so fetch a fresh list for the user.
        console.rescan(true);
        console.ui.window.present();
        console.ui.table.nearby_list.grab_focus();
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        iwd::on_any_change(&self.manager, move || {
            if let Some(c) = weak.upgrade() {
                c.queue_refresh();
            }
        });

        let weak = Rc::downgrade(self);
        self.ui.power.connect_state_set(move |_, on| {
            let Some(c) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if c.syncing_power.get() {
                return glib::Propagation::Proceed;
            }
            // The switch's state follows iwd, set by the next refresh.
            c.set_power(on);
            glib::Propagation::Stop
        });

        let weak = Rc::downgrade(self);
        self.ui.table.nearby_list.set_filter_func(move |row| {
            weak.upgrade().is_none_or(|c| {
                c.passes_filter(
                    c.networks
                        .borrow()
                        .get(row_index(row))
                        .map(|n| n.ssid.as_str()),
                )
            })
        });
        let weak = Rc::downgrade(self);
        self.ui.table.saved_list.set_filter_func(move |row| {
            weak.upgrade().is_none_or(|c| {
                c.passes_filter(
                    c.saved
                        .borrow()
                        .get(row_index(row))
                        .map(|s| s.ssid.as_str()),
                )
            })
        });
        let weak = Rc::downgrade(self);
        self.ui.table.filter.connect_search_changed(move |entry| {
            if let Some(c) = weak.upgrade() {
                *c.filter_text.borrow_mut() = entry.text().to_lowercase();
                let t = &c.ui.table;
                refilter(&t.nearby_list, &t.nearby_empty);
                refilter(&t.saved_list, &t.saved_empty);
                c.update_actions();
            }
        });
        let weak = Rc::downgrade(self);
        self.ui.table.filter.connect_stop_search(move |entry| {
            entry.set_text("");
            entry.set_visible(false);
            if let Some(c) = weak.upgrade() {
                c.current_list().grab_focus();
            }
        });

        self.connect_actions();

        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(POLL_SECONDS, move || match weak.upgrade() {
            Some(c) => {
                c.poll_diagnostics();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(SAVED_TIMES_SECONDS, move || match weak.upgrade() {
            Some(c) => {
                c.update_saved_times();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });

        // The window owns the console; closing it drops everything above.
        let strong = RefCell::new(Some(Rc::clone(self)));
        self.ui.window.connect_close_request(move |_| {
            strong.borrow_mut().take();
            glib::Propagation::Proceed
        });
    }

    /// The action buttons, and keeping them greyed out when unusable.
    fn connect_actions(self: &Rc<Self>) {
        let a = &self.ui.actions;
        let weak = Rc::downgrade(self);
        a.connect.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade()
                && let Some(row) = c.ui.table.nearby_list.selected_row()
            {
                c.connect_row(&row);
            }
        });
        let weak = Rc::downgrade(self);
        a.disconnect.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.disconnect();
            }
        });
        let weak = Rc::downgrade(self);
        a.rescan.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.rescan(false);
            }
        });
        let weak = Rc::downgrade(self);
        a.filter.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.toggle_filter();
            }
        });
        let weak = Rc::downgrade(self);
        self.ui.theme.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.dark.set(!c.dark.get());
                c.apply_theme();
                config::save_theme(c.dark.get());
            }
        });
        let weak = Rc::downgrade(self);
        self.ui
            .table
            .nearby_list
            .connect_selected_rows_changed(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.update_actions();
                }
            });
        let weak = Rc::downgrade(self);
        self.ui
            .table
            .saved_list
            .connect_selected_rows_changed(move |_| {
                if let Some(c) = weak.upgrade() {
                    c.disarm_forget();
                    c.update_actions();
                }
            });
        let weak = Rc::downgrade(self);
        a.forget.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.forget_selected();
            }
        });
        self.connect_hidden_page();
        let tabs = &self.ui.tabs;
        for tab in [&tabs.nearby, &tabs.hidden, &tabs.saved, &tabs.hotspot] {
            let weak = Rc::downgrade(self);
            // Switching tabs untoggles one and toggles another; act once.
            tab.connect_toggled(move |tab| {
                if tab.is_active()
                    && let Some(c) = weak.upgrade()
                {
                    c.show_tab();
                }
            });
        }
        self.connect_hotspot();
    }

    fn show_tab(self: &Rc<Self>) {
        let tab = self.tab();
        let table = &self.ui.table;
        table.stack.set_visible_child_name(tab);
        // Only Nearby and Saved have a list to filter.
        if !matches!(tab, "nearby" | "saved") && table.filter.is_visible() {
            table.filter.set_text("");
            table.filter.set_visible(false);
        }
        self.disarm_forget();
        self.set_status("", false);
        self.update_actions();
        // Fresh readings rather than ones from before the tab was hidden.
        self.poll_diagnostics();
    }

    fn connect_hotspot(self: &Rc<Self>) {
        let page = &self.ui.table.hotspot;
        let weak = Rc::downgrade(self);
        page.name.connect_changed(move |_| {
            if let Some(c) = weak.upgrade() {
                c.update_actions();
            }
        });
        let weak = Rc::downgrade(self);
        page.password.connect_changed(move |_| {
            if let Some(c) = weak.upgrade() {
                c.update_actions();
            }
        });
        let weak = Rc::downgrade(self);
        self.ui.actions.start.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.confirm_hotspot();
            }
        });
        let weak = Rc::downgrade(self);
        self.ui.actions.stop.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.stop_hotspot();
            }
        });
    }

    /// Starting a hotspot drops the current Wi-Fi connection, so ask first
    /// while connected; with nothing to lose, start straight away.
    fn confirm_hotspot(self: &Rc<Self>) {
        if self.connected_station.borrow().is_none() {
            return self.start_hotspot();
        }
        // Non-breaking hyphens (U+2011) keep "Wi-Fi" on one line.
        let network = self.ui.tele.ssid.text();
        let device = self.ui.device.text();
        let weak = Rc::downgrade(self);
        confirm(
            &self.ui.window,
            "Start hotspot?",
            &format!(
                "Starting the hotspot disconnects {device} from {network}.\n\n\
                 iwd can't yet run a hotspot and a Wi\u{2011}Fi connection on the \
                 same card at the same time (station + access point concurrency), \
                 so Wi\u{2011}Fi stays off until you stop the hotspot."
            ),
            "Start hotspot",
            move || {
                if let Some(c) = weak.upgrade() {
                    c.start_hotspot();
                }
            },
        );
    }

    /// Switches the device to AP mode if needed and starts the hotspot.
    fn start_hotspot(self: &Rc<Self>) {
        let page = &self.ui.table.hotspot;
        let name = page.name.text().to_string();
        let password = Zeroizing::new(page.password.text().to_string());
        if !iwd::valid_ssid(&name) {
            return;
        }
        if !hotspot::valid_password(&password) {
            self.set_status(
                "Use only plain letters, digits and symbols in the password.",
                true,
            );
            return;
        }
        let Some(device) = iwd::first_device(&self.manager) else {
            return;
        };
        page.password.set_text("");
        let progress = self.set_status(&format!("Starting hotspot {name}…"), false);
        let weak = Rc::downgrade(self);
        actions::start_hotspot(
            &self.manager,
            &device.object_path(),
            name,
            password,
            move |r| {
                if let Some(c) = weak.upgrade() {
                    c.report(Some(progress), r, None);
                    c.queue_refresh();
                }
            },
        );
    }

    /// Stops the hotspot and returns the device to station mode, where iwd
    /// reconnects to a saved network by itself.
    fn stop_hotspot(self: &Rc<Self>) {
        let Some(device) = iwd::first_device(&self.manager) else {
            return;
        };
        self.set_status("Stopping the hotspot…", false);
        let weak = Rc::downgrade(self);
        actions::stop_hotspot(&self.manager, &device.object_path(), move |r| {
            if let Some(c) = weak.upgrade() {
                c.report(None, r, Some("Back on Wi-Fi."));
                c.queue_refresh();
            }
        });
    }

    /// Shows a device in AP mode.
    fn show_hotspot(&self, ap: &gio::DBusProxy) {
        let started = iwd::prop_bool(ap, "Started") == Some(true);
        let name = iwd::prop_str(ap, "Name");
        self.clear_station();
        self.set_ap_mode(true);
        self.hotspot_up.set(started);

        let title = match (started, name.as_deref()) {
            (true, Some(n)) => format!("Hotspot · {n}"),
            (true, None) => "Hotspot".to_owned(),
            (false, _) => "Hotspot mode".to_owned(),
        };
        self.ui.tele.ssid.set_text(&title);
        if started {
            self.set_live("HOTSPOT", false);
            self.ui
                .table
                .hotspot
                .show(Some(name.as_deref().unwrap_or("")));
        } else {
            self.set_live("STOPPED", true);
            self.ui.table.hotspot.show(None);
        }
        self.update_actions();
    }

    /// Records whether the device is in AP mode, and has the Nearby and
    /// Hidden lists explain why they are empty while it is.
    fn set_ap_mode(&self, on: bool) {
        self.in_ap_mode.set(on);
        self.explain_empty(on.then_some(HOTSPOT_EMPTY));
    }

    /// What the empty Nearby and Hidden lists say: `why` they can't list
    /// anything, or, with `None`, their usual hint to rescan.
    fn explain_empty(&self, why: Option<&str>) {
        let table = &self.ui.table;
        table.nearby_empty.set_text(why.unwrap_or(NEARBY_EMPTY));
        table.hidden.set_empty(why);
    }

    /// The state beside the network name; `off` greys it out.
    fn set_live(&self, text: &str, off: bool) {
        let live = &self.ui.tele.live;
        live.set_text(text);
        if off {
            live.add_css_class("off");
        } else {
            live.remove_css_class("off");
        }
    }

    /// Empties everything that describes a station: link, lists, readouts.
    fn clear_station(&self) {
        *self.connected_station.borrow_mut() = None;
        self.has_station.set(false);
        self.scanning.set(false);
        self.set_readouts(None);
        self.show_networks(Vec::new());
        self.ui.table.hidden.show(Vec::new());
    }

    fn queue_refresh(self: &Rc<Self>) {
        if self.refresh_queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(c) = weak.upgrade() {
                c.refresh_queued.set(false);
                c.refresh();
            }
        });
    }

    /// Rebuilds the whole view from iwd's current state.
    fn refresh(self: &Rc<Self>) {
        let ui = &self.ui;
        self.can_host.set(false);
        if self.manager.name_owner().is_none() {
            self.show_saved(Vec::new());
            return self.show_unavailable("iwd is not running", "iwd is not running.", true);
        }
        // Saved networks don't depend on the device, so they show even with
        // Wi-Fi off.
        self.show_saved(iwd::saved_networks(&self.manager));
        let Some(device) = iwd::first_device(&self.manager) else {
            return self.show_unavailable(
                "No wireless device found",
                "No wireless device found.",
                true,
            );
        };
        let name = iwd::prop_str(&device, "Name").unwrap_or_default();
        ui.device.set_text(&name);
        let adapter = iwd::adapter(&self.manager, &device);
        ui.device.set_tooltip_text(
            adapter
                .as_ref()
                .and_then(|a| iwd::prop_str(a, "Model"))
                .as_deref(),
        );

        // A radio switched off by rfkill: iwd can't turn the device on.
        let radio_on = adapter.as_ref().and_then(|a| iwd::prop_bool(a, "Powered")) != Some(false);
        let powered = iwd::prop_bool(&device, "Powered") == Some(true);
        let supports_ap = adapter
            .and_then(|a| a.cached_property("SupportedModes"))
            .and_then(|m| m.get::<Vec<String>>())
            .is_some_and(|m| m.iter().any(|m| m == "ap"));
        self.can_host.set(powered && supports_ap);
        self.syncing_power.set(true);
        ui.power.set_active(powered);
        ui.power.set_state(powered);
        ui.power.set_sensitive(radio_on);
        self.syncing_power.set(false);
        if !radio_on {
            return self.show_unavailable(
                &format!("{name}'s radio is switched off"),
                RADIO_OFF_EMPTY,
                false,
            );
        }
        if !powered {
            return self.show_unavailable(
                &format!("{name} is turned off"),
                DEVICE_OFF_EMPTY,
                false,
            );
        }

        let path = device.object_path().to_string();
        let Some(station) = iwd::proxy(&self.manager, &path, iwd::IFACE_STATION) else {
            if let Some(ap) = iwd::proxy(&self.manager, &path, iwd::IFACE_AP) {
                return self.show_hotspot(&ap);
            }
            let mode = iwd::prop_str(&device, "Mode").unwrap_or_default();
            let message = format!("{name} is in {mode} mode");
            return self.show_unavailable(&message, &format!("{message}."), false);
        };
        self.set_ap_mode(false);
        self.hotspot_up.set(false);

        let state = iwd::prop_str(&station, "State").unwrap_or_default();
        let scanning = iwd::prop_bool(&station, "Scanning") == Some(true);
        self.has_station.set(true);
        self.scanning.set(scanning);
        let current = iwd::connected_ssid(&self.manager, &station);

        ui.tele
            .ssid
            .set_text(current.as_deref().unwrap_or("Not connected"));
        ui.table.hotspot.show(None);
        let now_connected = iwd::is_connected(&state);
        if scanning {
            self.set_live("SCANNING", false);
        } else {
            let off = !now_connected && state != "connecting";
            self.set_live(&state.to_uppercase(), off);
        }

        let was_connected = self.connected_station.borrow().is_some();
        *self.connected_station.borrow_mut() = now_connected.then(|| path.clone());
        if now_connected && !was_connected {
            self.poll_diagnostics();
        } else if !now_connected {
            self.set_readouts(None);
        }
        self.update_actions();

        let weak = Rc::downgrade(self);
        // A reply that arrives after the station went away (Wi-Fi off,
        // hotspot started) is dropped rather than refilling the lists.
        iwd::hidden_access_points(&station, move |aps| {
            if let Some(c) = weak.upgrade()
                && c.has_station.get()
            {
                c.ui.table.hidden.show(aps);
            }
        });
        let weak = Rc::downgrade(self);
        iwd::ordered_networks(&self.manager, &station, move |networks| {
            if let Some(c) = weak.upgrade()
                && c.has_station.get()
            {
                c.show_networks(networks);
            }
        });
    }

    /// Shows `message` in place of the network name, and `why` in the
    /// emptied lists and on the Hotspot tab. `no_device` also clears the
    /// device's name and power switch.
    fn show_unavailable(&self, message: &str, why: &str, no_device: bool) {
        let ui = &self.ui;
        ui.tele.ssid.set_text(message);
        self.set_live("OFFLINE", true);
        self.clear_station();
        self.in_ap_mode.set(false);
        self.explain_empty(Some(why));
        ui.table.hotspot.show_unavailable(why);
        self.hotspot_up.set(false);
        if no_device {
            ui.device.set_text("");
            ui.device.set_tooltip_text(None);
            ui.power.set_sensitive(false);
        }
        self.update_actions();
    }

    /// Refreshes what the visible tab shows of the link: the readouts on
    /// Nearby, the client list on Hotspot. Other tabs need neither.
    fn poll_diagnostics(self: &Rc<Self>) {
        match (self.tab(), self.hotspot_up.get()) {
            ("hotspot", true) => return self.poll_hotspot_clients(),
            ("nearby", false) => {}
            _ => return,
        }
        let Some(path) = self.connected_station.borrow().clone() else {
            return;
        };
        let Some(diag) = iwd::proxy(&self.manager, &path, iwd::IFACE_DIAGNOSTIC) else {
            return;
        };
        let weak = Rc::downgrade(self);
        iwd::diagnostics(&diag, move |d| {
            let Some(c) = weak.upgrade() else { return };
            // Ignore a late reply after the link went down.
            if c.connected_station.borrow().is_none() {
                return;
            }
            c.set_readouts(d.as_ref());
        });
    }

    /// Refreshes the hotspot's client list, adding IPs from the ARP table.
    fn poll_hotspot_clients(self: &Rc<Self>) {
        let Some(device) = iwd::first_device(&self.manager) else {
            return;
        };
        let path = device.object_path().to_string();
        let Some(diag) = iwd::proxy(&self.manager, &path, iwd::IFACE_AP_DIAGNOSTIC) else {
            return;
        };
        let name = iwd::prop_str(&device, "Name").unwrap_or_default();
        let weak = Rc::downgrade(self);
        iwd::hotspot_clients(&diag, move |clients| {
            let Some(c) = weak.upgrade() else { return };
            if !c.hotspot_up.get() {
                return;
            }
            let ips = arp::ips_by_mac(&name);
            let rows: Vec<hotspot::ClientRow> = clients
                .iter()
                .map(|client| hotspot::ClientRow {
                    client,
                    ip: ips.get(&client.address).map(String::as_str),
                })
                .collect();
            c.ui.table.hotspot.show_clients(&rows);
        });
    }

    fn set_readouts(&self, d: Option<&iwd::Diagnostics>) {
        for (label, (_, value, unit)) in self.ui.tele.readouts.iter().zip(format::readouts(d)) {
            let value = glib::markup_escape_text(&value);
            if unit.is_empty() {
                label.set_markup(&value);
            } else {
                label.set_markup(&format!(
                    "{value} <span size=\"small\" alpha=\"65%\">{unit}</span>"
                ));
            }
        }
    }

    fn show_networks(&self, networks: Vec<Network>) {
        if *self.networks.borrow() == networks {
            return;
        }
        let list = &self.ui.table.nearby_list;
        let selected = list.selected_row().and_then(|r| {
            self.networks
                .borrow()
                .get(row_index(&r))
                .map(|n| n.path.clone())
        });
        let focused = list.focus_child().is_some();
        let columns = &self.ui.table.nearby_columns;
        widgets::fill(list, networks.iter().map(|n| network_row(n, columns)));
        if let Some(i) = selected.and_then(|p| networks.iter().position(|n| n.path == p)) {
            select(list, i, focused);
        }
        *self.networks.borrow_mut() = networks;
        // The filter ran on each append against the old list.
        refilter(list, &self.ui.table.nearby_empty);
        self.update_actions();
    }

    fn show_saved(self: &Rc<Self>, saved: Vec<Saved>) {
        if *self.saved.borrow() == saved {
            return;
        }
        let list = &self.ui.table.saved_list;
        let selected = list.selected_row().and_then(|r| {
            self.saved
                .borrow()
                .get(row_index(&r))
                .map(|s| s.path.clone())
        });
        let focused = list.focus_child().is_some();
        let mut times = Vec::with_capacity(saved.len());
        let rows = saved.iter().map(|s| {
            let weak = Rc::downgrade(self);
            let path = s.path.clone();
            let (row, when) = saved::row(s, &self.ui.table.saved_columns, move |on| {
                if let Some(c) = weak.upgrade() {
                    c.set_auto_connect(&path, on);
                }
            });
            times.push(when);
            row
        });
        widgets::fill(list, rows);
        *self.saved_times.borrow_mut() = times;
        // Keep the selection; failing that (first fill, after Forget), pick
        // the first row so Forget is ready.
        let i = selected
            .and_then(|p| saved.iter().position(|s| s.path == p))
            .unwrap_or(0);
        select(list, i, focused);
        *self.saved.borrow_mut() = saved;
        refilter(list, &self.ui.table.saved_empty);
        self.update_actions();
    }

    fn update_saved_times(&self) {
        for (label, s) in self
            .saved_times
            .borrow()
            .iter()
            .zip(self.saved.borrow().iter())
        {
            label.set_text(&saved::last_connected(s.last_connected.as_deref()));
        }
    }

    fn saved_at(&self, row: &gtk::ListBoxRow) -> Option<Saved> {
        self.saved.borrow().get(row_index(row)).cloned()
    }

    /// The visible tab's stack page name.
    fn tab(&self) -> &'static str {
        if self.ui.tabs.saved.is_active() {
            "saved"
        } else if self.ui.tabs.hidden.is_active() {
            "hidden"
        } else if self.ui.tabs.hotspot.is_active() {
            "hotspot"
        } else {
            "nearby"
        }
    }

    fn current_list(&self) -> &gtk::ListBox {
        if self.tab() == "saved" {
            &self.ui.table.saved_list
        } else {
            &self.ui.table.nearby_list
        }
    }

    fn set_auto_connect(self: &Rc<Self>, path: &str, on: bool) {
        let weak = Rc::downgrade(self);
        actions::set_auto_connect(&self.manager, path, on, move |r| {
            if let Some(c) = weak.upgrade() {
                // The switch already moved; on failure, rebuild the list so
                // it shows iwd's value again.
                if r.is_err() {
                    c.saved.borrow_mut().clear();
                }
                c.report(None, r, None);
                c.queue_refresh();
            }
        });
    }

    fn connect_hidden_page(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.ui.table.hidden.name.connect_changed(move |_| {
            if let Some(c) = weak.upgrade() {
                c.update_actions();
            }
        });
        let weak = Rc::downgrade(self);
        self.ui.table.hidden.join.connect_clicked(move |_| {
            if let Some(c) = weak.upgrade() {
                c.join_hidden();
            }
        });
    }

    /// Asks iwd to find and join a network that doesn't broadcast its name.
    /// iwd asks the tray's agent for the password if one is needed.
    fn join_hidden(self: &Rc<Self>) {
        let name = self.ui.table.hidden.name.text().to_string();
        let Some(device) = iwd::first_device(&self.manager) else {
            return;
        };
        if !iwd::valid_ssid(&name) {
            return;
        }
        self.ui.table.hidden.name.set_text("");
        let progress = self.set_status(&format!("Looking for {name}…"), false);
        let weak = Rc::downgrade(self);
        actions::join_hidden(&self.manager, &device.object_path(), &name, move |r| {
            if let Some(c) = weak.upgrade() {
                c.report_connect(progress, r);
            }
        });
    }

    /// First click arms Forget for the selected network; a second click on
    /// the same network within a few seconds removes its stored profile.
    fn forget_selected(self: &Rc<Self>) {
        let Some(s) = self
            .ui
            .table
            .saved_list
            .selected_row()
            .and_then(|r| self.saved_at(&r))
        else {
            return;
        };
        if self.forget_armed.borrow().as_deref() != Some(s.path.as_str()) {
            *self.forget_armed.borrow_mut() = Some(s.path);
            let serial = self.forget_serial.get() + 1;
            self.forget_serial.set(serial);
            let button = &self.ui.actions.forget;
            button.set_label("Confirm");
            button.add_css_class("confirm");
            self.set_status(
                &format!("Click Confirm to forget {} and its password.", s.ssid),
                false,
            );
            let weak = Rc::downgrade(self);
            glib::timeout_add_seconds_local_once(FORGET_CONFIRM_SECONDS, move || {
                if let Some(c) = weak.upgrade()
                    && c.forget_serial.get() == serial
                {
                    c.disarm_forget();
                }
            });
            return;
        }
        self.disarm_forget();
        let weak = Rc::downgrade(self);
        let ssid = s.ssid;
        actions::forget(&self.manager, &s.path, move |r| {
            if let Some(c) = weak.upgrade() {
                c.report(None, r, Some(&format!("Forgot {ssid}.")));
                c.queue_refresh();
            }
        });
    }

    /// Puts Forget back to normal, along with its "Click Confirm" prompt.
    fn disarm_forget(self: &Rc<Self>) {
        if self.forget_armed.borrow_mut().take().is_some() {
            let button = &self.ui.actions.forget;
            button.set_label("Forget");
            button.remove_css_class("confirm");
            self.set_status("", false);
        }
    }

    /// Greys out actions that can't do anything right now, and shows only
    /// the ones that belong to the current tab.
    fn update_actions(&self) {
        let a = &self.ui.actions;
        let tab = self.tab();
        let on_saved = tab == "saved";
        let on_nearby = tab == "nearby";
        let on_hotspot = tab == "hotspot";
        // The readouts describe the current link, so only Nearby shows them,
        // and not in hotspot mode, where there is no link.
        let readouts = on_nearby && !self.in_ap_mode.get();
        self.ui.tele.grid.set_visible(readouts);
        // Without readouts, the rule under the name would double up with the
        // section's bottom edge.
        if readouts {
            self.ui.tele.top.remove_css_class("no-rule");
        } else {
            self.ui.tele.top.add_css_class("no-rule");
        }
        for b in [&a.connect, &a.disconnect] {
            b.set_visible(on_nearby);
        }
        // Rescan also refreshes the hidden list.
        a.rescan.set_visible(on_nearby || tab == "hidden");
        a.forget.set_visible(on_saved);
        a.filter.set_visible(on_nearby || on_saved);
        a.start.set_visible(on_hotspot && !self.hotspot_up.get());
        a.stop.set_visible(on_hotspot && self.in_ap_mode.get());
        let page = &self.ui.table.hotspot;
        a.start.set_sensitive(
            self.can_host.get()
                && iwd::valid_ssid(&page.name.text())
                // Only the length here: the full check needs a copy of the
                // text, so it waits for Start.
                && hotspot::PASSWORD_LEN.contains(&widgets::secret_len(&page.password)),
        );
        let hidden = &self.ui.table.hidden;
        hidden
            .join
            .set_sensitive(self.has_station.get() && iwd::valid_ssid(&hidden.name.text()));
        a.forget
            .set_sensitive(self.ui.table.saved_list.selected_row().is_some());
        // Picking another network offers only Connect: iwd leaves the
        // current network by itself when asked to join a different one.
        let other_selected = self.ui.table.nearby_list.selected_row().is_some_and(|row| {
            self.networks
                .borrow()
                .get(row_index(&row))
                .is_some_and(|n| !n.connected)
        });
        a.connect.set_sensitive(other_selected);
        a.disconnect
            .set_sensitive(self.connected_station.borrow().is_some() && !other_selected);
        a.rescan
            .set_sensitive(self.has_station.get() && !self.scanning.get());
        let rows = if on_saved {
            !self.saved.borrow().is_empty()
        } else {
            !self.networks.borrow().is_empty()
        };
        a.filter
            .set_sensitive(rows || self.ui.table.filter.is_visible());
    }

    /// Loads the current palette and points the toggle at the other one.
    fn apply_theme(&self) {
        let dark = self.dark.get();
        style::apply(&self.css, self.config.palette(dark), dark);
        let theme = &self.ui.theme;
        if dark {
            theme.set_icon_name("weather-clear-symbolic");
            theme.set_tooltip_text(Some("Light"));
        } else {
            theme.set_icon_name("weather-clear-night-symbolic");
            theme.set_tooltip_text(Some("Dark"));
        }
    }

    fn toggle_filter(&self) {
        let filter = &self.ui.table.filter;
        if filter.is_visible() {
            filter.emit_by_name::<()>("stop-search", &[]);
        } else {
            filter.set_visible(true);
            filter.grab_focus();
        }
    }

    fn network_at(&self, row: &gtk::ListBoxRow) -> Option<Network> {
        self.networks.borrow().get(row_index(row)).cloned()
    }

    /// Whether a row named `name` shows under the current filter text.
    fn passes_filter(&self, name: Option<&str>) -> bool {
        let needle = self.filter_text.borrow();
        needle.is_empty() || name.is_some_and(|n| n.to_lowercase().contains(needle.as_str()))
    }

    fn station(&self) -> Option<gio::DBusProxy> {
        let device = iwd::first_device(&self.manager)?;
        iwd::proxy(&self.manager, &device.object_path(), iwd::IFACE_STATION)
    }

    fn connect_row(self: &Rc<Self>, row: &gtk::ListBoxRow) {
        let Some(network) = self.network_at(row) else {
            return;
        };
        if network.connected {
            return;
        }
        let progress = self.set_status(&format!("Connecting to {}…", network.ssid), false);
        let weak = Rc::downgrade(self);
        actions::connect(&self.manager, &network.path, move |r| {
            if let Some(c) = weak.upgrade() {
                c.report_connect(progress, r);
            }
        });
    }

    fn disconnect(self: &Rc<Self>) {
        let Some(device) = iwd::first_device(&self.manager) else {
            return;
        };
        let progress = self.set_status("Disconnecting…", false);
        let weak = Rc::downgrade(self);
        actions::disconnect(&self.manager, &device.object_path(), move |r| {
            if let Some(c) = weak.upgrade() {
                c.report(Some(progress), r, None);
            }
        });
    }

    /// A `quiet` scan reports nothing: iwd refuses one as Busy while it is
    /// connecting, which is no news to someone who just opened the window.
    fn rescan(self: &Rc<Self>, quiet: bool) {
        let Some(station) = self.station() else {
            return;
        };
        let weak = Rc::downgrade(self);
        iwd::call(
            &station,
            "Scan",
            None,
            iwd::DEFAULT_TIMEOUT,
            move |result| {
                if let Some(c) = weak.upgrade()
                    && !quiet
                {
                    c.report(None, result, None);
                }
            },
        );
    }

    fn set_power(self: &Rc<Self>, on: bool) {
        let Some(device) = iwd::first_device(&self.manager) else {
            return self.queue_refresh();
        };
        let weak = Rc::downgrade(self);
        iwd::set_property(&device, "Powered", &on.to_variant(), move |result| {
            if let Some(c) = weak.upgrade() {
                c.report(None, result, None);
                c.queue_refresh();
            }
        });
    }

    /// Shows what went wrong, or `done` on success. A success without
    /// `done` clears the action's own progress message (serial `progress`),
    /// leaving any newer message alone.
    fn report<T>(
        self: &Rc<Self>,
        progress: Option<u64>,
        result: Result<T, glib::Error>,
        done: Option<&str>,
    ) {
        match (result, done) {
            (Err(e), _) => {
                self.set_status(&iwd::describe_error(&e), true);
            }
            (Ok(_), Some(done)) => {
                self.set_status(done, false);
            }
            (Ok(_), None) => {
                if progress == Some(self.status_serial.get()) {
                    self.set_status("", false);
                }
            }
        }
    }

    /// As `report`, for joining a network, where "Failed" usually means a
    /// wrong password.
    fn report_connect(self: &Rc<Self>, progress: u64, result: Result<(), glib::Error>) {
        match result {
            Err(e) => {
                self.set_status(&iwd::describe_connect_error(&e), true);
            }
            Ok(()) => self.report(Some(progress), Ok(()), None),
        }
    }

    /// Shows a message under the list. Progress messages (ending in "…")
    /// stay until replaced; anything else clears itself after a few seconds.
    /// Returns the message's serial, for `report` to tell if it still shows.
    fn set_status(self: &Rc<Self>, text: &str, error: bool) -> u64 {
        let status = &self.ui.table.status;
        status.set_text(text);
        status.set_visible(!text.is_empty());
        if error {
            status.add_css_class("error");
        } else {
            status.remove_css_class("error");
        }
        let serial = self.status_serial.get() + 1;
        self.status_serial.set(serial);
        if text.is_empty() || text.ends_with('…') {
            return serial;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local_once(STATUS_SECONDS, move || {
            if let Some(c) = weak.upgrade()
                && c.status_serial.get() == serial
            {
                c.set_status("", false);
            }
        });
        serial
    }
}

/// A small modal question in the window style: a heading, an explanation,
/// and Cancel beside a button that runs `on_confirm`.
fn confirm(
    parent: &gtk::ApplicationWindow,
    heading: &str,
    body: &str,
    action: &str,
    on_confirm: impl FnOnce() + 'static,
) {
    let dialog = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .destroy_with_parent(true)
        .default_width(340)
        .build();
    let text = gtk::Box::new(gtk::Orientation::Vertical, 10);
    text.add_css_class("prompt");
    text.append(&label(heading, "ssid"));
    let message = label(body, "confirm-body");
    message.set_wrap(true);
    message.set_max_width_chars(40);
    text.append(&message);
    let (cancel, go) = widgets::dialog(&dialog, &text, action);

    // Weak: a strong ref from its own buttons would keep the dialog alive.
    let d = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(d) = d.upgrade() {
            d.close();
        }
    });
    let d = dialog.downgrade();
    let on_confirm = RefCell::new(Some(on_confirm));
    go.connect_clicked(move |_| {
        if let Some(d) = d.upgrade() {
            d.close();
        }
        if let Some(f) = on_confirm.borrow_mut().take() {
            f();
        }
    });
    dialog.present();
}

fn build_ui(app: &gtk::Application, config: &WindowConfig) -> Ui {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("iwdtray")
        .default_width(config.width)
        .default_height(config.height)
        // Sized from iwdtray.conf when opened, then fixed: the layout is
        // designed for one size and the list scrolls.
        .resizable(false)
        .build();
    window.add_css_class("console");

    let header = gtk::HeaderBar::new();
    let title = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    title.append(&label("iwdtray", "title"));
    let device = label("", "dev");
    title.append(&device);
    header.set_title_widget(Some(&title));
    let power = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .tooltip_text("Wi-Fi on/off")
        .build();
    header.pack_end(&power);
    // Its icon and tooltip follow the theme; see `apply_theme`.
    let theme = gtk::Button::builder().valign(gtk::Align::Center).build();
    theme.add_css_class("theme-toggle");
    header.pack_start(&theme);
    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    window.set_child(Some(&root));
    // Tabs go above the connection summary, so they stay put when the
    // readouts hide (every tab but Nearby, and hotspot mode).
    let tabs = build_tabs(&root);
    let tele = build_telemetry(&root);
    let table = build_table(&root);
    let actions = build_actions(&root);
    Ui {
        window,
        device,
        power,
        theme,
        tabs,
        tele,
        table,
        actions,
    }
}

fn build_tabs(root: &gtk::Box) -> Tabs {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    bar.add_css_class("tabs");
    let nearby = gtk::ToggleButton::builder()
        .label("NEARBY")
        .active(true)
        .build();
    let tab = |text: &str| {
        gtk::ToggleButton::builder()
            .label(text)
            .group(&nearby)
            .build()
    };
    let tabs = Tabs {
        hidden: tab("HIDDEN"),
        saved: tab("SAVED"),
        hotspot: tab("HOTSPOT"),
        nearby: nearby.clone(),
    };
    // HIDDEN sits next to NEARBY: both are about what's in range.
    for tab in [&tabs.nearby, &tabs.hidden, &tabs.saved, &tabs.hotspot] {
        tab.add_css_class("tab");
        tab.set_focus_on_click(false);
        bar.append(tab);
    }
    root.append(&bar);
    tabs
}

fn build_telemetry(root: &gtk::Box) -> Telemetry {
    let tele = gtk::Box::new(gtk::Orientation::Vertical, 8);
    tele.add_css_class("tele");
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    top.add_css_class("tele-top");
    let ssid = label("", "ssid");
    ssid.set_hexpand(true);
    ssid.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let live = label("", "live");
    top.append(&ssid);
    top.append(&live);
    tele.append(&top);

    let grid = gtk::Grid::builder()
        .column_spacing(12)
        .row_spacing(8)
        .column_homogeneous(true)
        .build();
    let readouts = format::readouts(None).map(|(name, _, _)| {
        let cell = gtk::Box::new(gtk::Orientation::Vertical, 1);
        cell.append(&label(name, "readout-label"));
        let value = label("", "readout-value");
        value.set_ellipsize(gtk::pango::EllipsizeMode::End);
        cell.append(&value);
        (cell, value)
    });
    for ((cell, _), i) in readouts.iter().zip(0..) {
        grid.attach(cell, i % 3, i / 3, 1, 1);
    }
    tele.append(&grid);
    root.append(&tele);
    Telemetry {
        ssid,
        live,
        readouts: readouts.map(|(_, value)| value),
        grid,
        top,
    }
}

/// The filter entry, the tabs' pages, and the status line.
fn build_table(root: &gtk::Box) -> Table {
    let filter = gtk::SearchEntry::builder()
        .placeholder_text("Filter networks")
        .visible(false)
        .build();
    filter.add_css_class("filter");
    root.append(&filter);

    let nearby = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let (head, nearby_columns) =
        widgets::heading("SSID", ["SIGNAL", "dBm", "SEC", "APs"], TABLE_GAP);
    nearby.append(&head);
    let (nearby_list, nearby_empty) = widgets::list(gtk::SelectionMode::Single, NEARBY_EMPTY);
    nearby.append(&widgets::scrolled(&nearby_list));

    let (saved_page, saved_list, saved_empty, saved_columns) = saved::page();
    let hidden = hidden::Page::new();
    let hotspot = hotspot::Page::new();
    let stack = gtk::Stack::builder().vexpand(true).build();
    stack.add_named(&nearby, Some("nearby"));
    stack.add_named(&hidden.root, Some("hidden"));
    stack.add_named(&saved_page, Some("saved"));
    stack.add_named(&hotspot.root, Some("hotspot"));
    root.append(&stack);

    let status = label("", "status");
    status.set_wrap(true);
    status.set_visible(false);
    root.append(&status);

    Table {
        filter,
        stack,
        nearby_list,
        nearby_empty,
        nearby_columns,
        hidden,
        saved_list,
        saved_empty,
        saved_columns,
        hotspot,
        status,
    }
}

/// A button for each action.
fn build_actions(root: &gtk::Box) -> Actions {
    // Buttons have one fixed width (style.css) and sit on the left. Four of
    // them with 6 px gaps fit the 390 px window.
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("actions");
    let button = |what: &str| {
        let b = gtk::Button::builder()
            .label(what)
            .focus_on_click(false)
            .build();
        b.add_css_class("action");
        bar.append(&b);
        b
    };
    let actions = Actions {
        connect: button("Connect"),
        disconnect: button("Disconnect"),
        rescan: button("Rescan"),
        forget: button("Forget"),
        start: button("Start"),
        stop: button("Stop"),
        filter: button("Filter"),
    };
    root.append(&bar);
    actions
}

/// A row's position in its list's model.
/// Selects row `i`, giving it the keyboard focus if the list had it before
/// its rows were replaced.
fn select(list: &gtk::ListBox, i: usize, focus: bool) {
    let row = list.row_at_index(i32::try_from(i).unwrap_or(0));
    list.select_row(row.as_ref());
    if focus && let Some(row) = row {
        row.grab_focus();
    }
}

/// Re-runs `list`'s filter, dropping a selection it hid, so an action
/// never applies to a row the user can't see. `empty`, the list's
/// placeholder, explains only a list with no rows at all: when the filter
/// hides every row, the list shows nothing rather than claiming there are
/// no networks.
fn refilter(list: &gtk::ListBox, empty: &gtk::Label) {
    list.invalidate_filter();
    if list.selected_row().is_some_and(|r| !r.is_child_visible()) {
        list.unselect_all();
    }
    empty.set_visible(list.row_at_index(0).is_none());
}

fn row_index(row: &gtk::ListBoxRow) -> usize {
    usize::try_from(row.index()).unwrap_or(usize::MAX)
}

fn network_row(n: &Network, columns: &[gtk::SizeGroup; 4]) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    if n.connected {
        row.add_css_class("connected");
    }

    let line = gtk::Box::new(gtk::Orientation::Horizontal, TABLE_GAP);
    // The name, with its access point's MAC address beneath it.
    line.append(&widgets::name_and_mac(&n.ssid, n.address.as_deref(), false));

    let signal = widgets::locked_meter(n.dbm, n.security);
    let mark = label(
        // Not dots, so they can't be mistaken for part of the meter.
        if n.connected {
            "✓"
        } else if n.known {
            "★"
        } else {
            " "
        },
        if n.connected { "link" } else { "known" },
    );
    signal.append(&mark);
    columns[0].add_widget(&signal);
    line.append(&signal);

    line.append(&widgets::cell(
        &format::signed(n.dbm.into()),
        3,
        &columns[1],
    ));
    line.append(&widgets::cell(n.security, 4, &columns[2]));
    let aps = match n.access_points {
        0 => format::DASH.to_owned(),
        n => n.to_string(),
    };
    line.append(&widgets::cell(&aps, 2, &columns[3]));

    row.set_child(Some(&line));
    row
}
