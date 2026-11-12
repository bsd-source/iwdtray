//! The Hotspot tab: run the wireless device as an access point.
//!
//! iwd runs a device in one mode at a time, so starting a hotspot takes the
//! device off its current network, and devices that join it get a local
//! network only (no internet) unless the laptop has another uplink and
//! forwarding/NAT are set up outside iwd.

use gtk::prelude::*;

use crate::format;
use crate::iwd::Client;
use crate::widgets::{self, TABLE_GAP};

pub struct Page {
    pub root: gtk::Box,
    pub name: gtk::Entry,
    pub password: gtk::PasswordEntry,
    form: gtk::Grid,
    /// "● RUNNING · name", shown while the hotspot is up.
    running: gtk::Box,
    running_name: gtk::Label,
    clients_box: gtk::Box,
    clients_head: gtk::Label,
    clients: gtk::ListBox,
    /// Why there is no hotspot to start, in place of the form.
    unavailable: gtk::Label,
    /// SIGNAL, dBm and TIME widths, shared by the headings and every row.
    columns: [gtk::SizeGroup; 3],
}

/// A connected device and, once known, the IP address it was given.
pub struct ClientRow<'a> {
    pub client: &'a Client,
    pub ip: Option<&'a str>,
}

impl Page {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.add_css_class("hotspot");

        let form = gtk::Grid::builder()
            .row_spacing(10)
            .column_spacing(12)
            .build();
        let name = gtk::Entry::builder().hexpand(true).build();
        let password = gtk::PasswordEntry::builder()
            .tooltip_text(format!(
                "{} to {} characters",
                PASSWORD_LEN.start(),
                PASSWORD_LEN.end()
            ))
            .show_peek_icon(true)
            .hexpand(true)
            .build();
        let fields = [
            ("NAME", name.upcast_ref::<gtk::Widget>()),
            ("PASSWORD", password.upcast_ref()),
        ];
        for ((text, widget), row) in fields.into_iter().zip(0..) {
            form.attach(&widgets::label(text, "readout-label"), 0, row, 1, 1);
            // Keep the field at its own height rather than the row's.
            widget.set_valign(gtk::Align::Center);
            form.attach(widget, 1, row, 1, 1);
        }
        root.append(&form);

        // Styled and placed like the Nearby list's empty message.
        let unavailable = widgets::message("");
        unavailable.set_vexpand(true);
        unavailable.set_visible(false);
        root.append(&unavailable);

        let running = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        running.set_visible(false);
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("dot");
        dot.add_css_class("lit");
        dot.set_valign(gtk::Align::Center);
        running.append(&dot);
        running.append(&widgets::label("RUNNING", "running-label"));
        let running_name = widgets::label("", "");
        running_name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        running.append(&running_name);
        root.append(&running);

        // The client table, laid out like the Nearby table.
        let clients_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        clients_box.set_visible(false);
        let clients_head = widgets::label("CLIENTS (0)", "readout-label");
        clients_head.add_css_class("clients-count");
        clients_box.append(&clients_head);
        let (head, columns) = widgets::heading("CLIENT", ["SIGNAL", "dBm", "TIME"], TABLE_GAP);
        head.add_css_class("flush");
        clients_box.append(&head);
        let (clients, _) = widgets::list(gtk::SelectionMode::None, "No devices connected yet.");
        clients.add_css_class("clients");
        clients_box.append(&clients);
        root.append(&clients_box);

        Self {
            root,
            name,
            password,
            form,
            running,
            running_name,
            clients_box,
            clients_head,
            clients,
            unavailable,
            columns,
        }
    }

    /// Shows the start form, or the running hotspot's name and clients.
    pub fn show(&self, running: Option<&str>) {
        self.unavailable.set_visible(false);
        self.form.set_visible(running.is_none());
        self.running.set_visible(running.is_some());
        self.clients_box.set_visible(running.is_some());
        match running {
            Some(name) => self.running_name.set_text(&format!("· {name}")),
            // Cleared once, not on every refresh while there is no hotspot.
            None if self.clients.row_at_index(0).is_some() => self.show_clients(&[]),
            None => {}
        }
    }

    /// Shows `why` there is no hotspot to start (the device is off, say)
    /// in place of the form, until the next `show`.
    pub fn show_unavailable(&self, why: &str) {
        self.show(None);
        self.form.set_visible(false);
        self.unavailable.set_text(why);
        self.unavailable.set_visible(true);
    }

    /// Lists the connected devices like Nearby lists networks: IP with the
    /// MAC beneath, then signal dots, dBm and connected time.
    pub fn show_clients(&self, rows: &[ClientRow]) {
        self.clients_head
            .set_text(&format!("CLIENTS ({})", rows.len()));
        widgets::fill(&self.clients, rows.iter().map(|r| self.client_row(r)));
    }

    fn client_row(&self, row: &ClientRow) -> gtk::ListBoxRow {
        let c = row.client;
        let line = gtk::Box::new(gtk::Orientation::Horizontal, TABLE_GAP);
        line.append(&widgets::name_and_mac(
            row.ip.unwrap_or("getting address…"),
            Some(&c.address),
            row.ip.is_none(),
        ));

        let signal: gtk::Widget = match c.rssi {
            Some(rssi) => widgets::signal_meter(rssi).upcast(),
            None => gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast(),
        };
        self.columns[0].add_widget(&signal);
        line.append(&signal);

        let dash = || format::DASH.to_owned();
        let dbm = c.rssi.map_or_else(dash, |v| format::signed(v.into()));
        line.append(&widgets::cell(&dbm, 3, &self.columns[1]));
        let time = c.connected_secs.map_or_else(dash, format::uptime);
        line.append(&widgets::cell(&time, 8, &self.columns[2]));

        let item = gtk::ListBoxRow::builder().activatable(false).build();
        item.set_child(Some(&line));
        item
    }
}

/// How long a WPA2 passphrase may be.
pub const PASSWORD_LEN: std::ops::RangeInclusive<usize> = 8..=63;

/// WPA2 passphrases are 8 to 63 printable ASCII characters.
pub fn valid_password(p: &str) -> bool {
    PASSWORD_LEN.contains(&p.len()) && p.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

#[cfg(test)]
mod tests {
    use super::valid_password;

    #[test]
    fn password_rules() {
        assert!(valid_password("hunter22"));
        assert!(!valid_password("short"));
        assert!(valid_password(&"x".repeat(63)));
        assert!(!valid_password(&"x".repeat(64)));
        assert!(!valid_password("pässwörd123"));
    }
}
