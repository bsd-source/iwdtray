//! The Saved tab: networks iwd has a stored profile for.

use gtk::glib;
use gtk::prelude::*;

use crate::format;
use crate::iwd::Saved;
use crate::widgets;

/// Widths shared by the column headings and every row.
pub type Columns = [gtk::SizeGroup; 3];

/// Builds the tab's column headings and list, returning the page, the list,
/// the message it shows while empty, and the column size groups the rows join.
pub fn page() -> (gtk::Box, gtk::ListBox, gtk::Label, Columns) {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let (head, columns) = widgets::heading("SSID", ["LAST USED", "AUTO", "SEC"], 12);
    page.append(&head);
    let (list, empty) = widgets::list(
        gtk::SelectionMode::Single,
        "No saved networks yet. Networks you connect to appear here.",
    );
    page.append(&widgets::scrolled(&list));
    (page, list, empty, columns)
}

/// One saved network, and its LAST USED label for keeping current.
/// `on_auto_connect` runs when its switch is flipped.
pub fn row(
    s: &Saved,
    columns: &Columns,
    on_auto_connect: impl Fn(bool) + 'static,
) -> (gtk::ListBoxRow, gtk::Label) {
    let row = gtk::ListBoxRow::new();
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);

    let ssid = widgets::label(&s.ssid, "");
    ssid.set_hexpand(true);
    ssid.set_ellipsize(gtk::pango::EllipsizeMode::End);
    line.append(&ssid);

    let when = widgets::label(&last_connected(s.last_connected.as_deref()), "when");
    columns[0].add_widget(&when);
    line.append(&when);

    // Set before connecting the handler, so building the row sends nothing.
    let auto = gtk::Switch::builder()
        .active(s.auto_connect)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Start)
        .tooltip_text("Auto-connect on/off")
        .build();
    auto.connect_state_set(move |_, on| {
        on_auto_connect(on);
        glib::Propagation::Proceed
    });
    let auto_cell = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    auto_cell.append(&auto);
    columns[1].add_widget(&auto_cell);
    line.append(&auto_cell);

    line.append(&widgets::cell(s.security, 4, &columns[2]));

    row.set_child(Some(&line));
    (row, when)
}

/// `"2026-01-15T09:30:00Z"` → "4 min ago", or a date for older times.
pub fn last_connected(iso: Option<&str>) -> String {
    let Some(then) = iso.and_then(|t| glib::DateTime::from_iso8601(t, None).ok()) else {
        return "never".to_owned();
    };
    let Ok(now) = glib::DateTime::now_utc() else {
        return String::new();
    };
    let secs = now.difference(&then).as_seconds();
    format::ago(secs).unwrap_or_else(|| {
        then.to_local()
            .and_then(|t| t.format("%Y-%m-%d"))
            .map(Into::into)
            .unwrap_or_default()
    })
}
