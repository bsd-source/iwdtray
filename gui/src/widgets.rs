//! Small building blocks shared by the tabs' tables.

use gtk::prelude::*;

use crate::format;

/// Gap between table columns; tight enough to leave room for long network
/// names next to Nearby's APs column.
pub const TABLE_GAP: i32 = 10;

/// The length in characters of a password entry's text, read without
/// copying the text out of GTK's wiped buffer, as `text()` would.
pub fn secret_len(entry: &gtk::PasswordEntry) -> usize {
    entry
        .delegate()
        .and_downcast::<gtk::Text>()
        .map_or(0, |t| usize::from(t.text_length()))
}

/// A left-aligned label, with a CSS class unless `class` is empty.
pub fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::builder().label(text).xalign(0.0).build();
    if !class.is_empty() {
        l.add_css_class(class);
    }
    l
}

/// A small window over the main one in the window's style: `body` above
/// an equal-width bar of Cancel and the `action` button, which it returns.
/// `window` should be built with its size and modality already set.
pub fn dialog(
    window: &gtk::Window,
    body: &impl IsA<gtk::Widget>,
    action: &str,
) -> (gtk::Button, gtk::Button) {
    window.set_title(Some("iwdtray"));
    window.set_resizable(false);
    window.add_css_class("console");
    let header = gtk::HeaderBar::new();
    header.set_title_widget(Some(&label("iwdtray", "title")));
    window.set_titlebar(Some(&header));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(body);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_homogeneous(true);
    buttons.add_css_class("actions");
    let cancel = gtk::Button::with_label("Cancel");
    let go = gtk::Button::with_label(action);
    for b in [&cancel, &go] {
        b.add_css_class("action");
        b.set_focus_on_click(false);
        buttons.append(b);
    }
    root.append(&buttons);
    window.set_child(Some(&root));
    (cancel, go)
}

/// A column heading row: `first` takes the spare width, and each of
/// `columns` gets a size group that rows join so their cells line up.
pub fn heading<const N: usize>(
    first: &str,
    columns: [&str; N],
    gap: i32,
) -> (gtk::Box, [gtk::SizeGroup; N]) {
    let head = gtk::Box::new(gtk::Orientation::Horizontal, gap);
    head.add_css_class("colhead");
    let first = label(first, "");
    first.set_hexpand(true);
    head.append(&first);
    let groups = columns.map(|text| {
        let group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let l = label(text, "");
        group.add_widget(&l);
        head.append(&l);
        group
    });
    (head, groups)
}

/// A table body showing `empty` while it has no rows, and that message's
/// label, for changing it later (GTK doesn't show a placeholder swapped in
/// while the list is already empty).
pub fn list(selection: gtk::SelectionMode, empty: &str) -> (gtk::ListBox, gtk::Label) {
    let list = gtk::ListBox::builder().selection_mode(selection).build();
    list.add_css_class("networks");
    let empty = message(empty);
    list.set_placeholder(Some(&empty));
    (list, empty)
}

/// A centred message in place of content, such as why a list is empty.
pub fn message(text: &str) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .justify(gtk::Justification::Center)
        // Wrap, or a one-line message sets the window's minimum width.
        .wrap(true)
        .build();
    l.add_css_class("empty-list");
    l
}

/// Replaces `list`'s rows with `rows`, marking the last one so it gets a
/// bottom rule. (CSS `:last-child` can't do that: the placeholder is a child
/// of the list too, after the rows.)
pub fn fill(list: &gtk::ListBox, rows: impl IntoIterator<Item = gtk::ListBoxRow>) {
    // Not ListBox::remove_all, which also drops the placeholder, so an
    // emptied list would show nothing at all.
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }
    let mut final_row = None;
    for row in rows {
        list.append(&row);
        final_row = Some(row);
    }
    if let Some(row) = final_row {
        row.add_css_class("last");
    }
}

/// `list` in a vertical scroller that takes the page's spare height.
pub fn scrolled(list: &gtk::ListBox) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(list)
        .build()
}

/// A table cell of at least `chars` characters, joined to `group`.
pub fn cell(text: &str, chars: i32, group: &gtk::SizeGroup) -> gtk::Label {
    let l = label(text, "");
    l.set_width_chars(chars);
    group.add_widget(&l);
    l
}

/// The five-dot signal meter.
pub fn signal_meter(dbm: i16) -> gtk::Box {
    let lit = format::signal_dots(dbm);
    let meter = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    meter.set_valign(gtk::Align::Center);
    for i in 0..format::DOTS {
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("dot");
        if i < lit {
            dot.add_css_class("lit");
        }
        meter.append(&dot);
    }
    meter
}

/// A padlock for networks that need a password, then the signal meter.
/// Open networks keep an invisible padlock so every meter lines up.
pub fn locked_meter(dbm: i16, security: &str) -> gtk::Box {
    let signal = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let lock = gtk::Image::builder()
        .icon_name("changes-prevent-symbolic")
        .pixel_size(12)
        .valign(gtk::Align::Center)
        .build();
    lock.add_css_class("lock");
    if security == "OPEN" {
        lock.set_opacity(0.0);
    }
    signal.append(&lock);
    signal.append(&signal_meter(dbm));
    signal
}

/// A name with a MAC address in small print beneath it, for a row's first
/// column. `quiet` greys the name (a placeholder rather than a value).
pub fn name_and_mac(name: &str, mac: Option<&str>, quiet: bool) -> gtk::Box {
    let names = gtk::Box::new(gtk::Orientation::Vertical, 1);
    names.set_hexpand(true);
    names.set_valign(gtk::Align::Center);
    let first = label(name, if quiet { "when" } else { "" });
    first.set_ellipsize(gtk::pango::EllipsizeMode::End);
    names.append(&first);
    if let Some(mac) = mac {
        let mac = label(&format::mac(mac), "row-mac");
        mac.set_ellipsize(gtk::pango::EllipsizeMode::End);
        names.append(&mac);
    }
    names
}
