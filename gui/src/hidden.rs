//! The Hidden tab: join a network that doesn't broadcast its name, and see
//! which hidden access points are in range.

use std::cell::RefCell;

use gtk::prelude::*;

use crate::format;
use crate::iwd::HiddenAp;
use crate::widgets;

const EMPTY: &str = "No hidden networks in range. Click Rescan to look again.";

pub struct Page {
    pub root: gtk::Box,
    pub name: gtk::Entry,
    pub join: gtk::Button,
    list: gtk::ListBox,
    empty: gtk::Label,
    /// SIGNAL, dBm and SEC widths, shared by the headings and every row.
    columns: [gtk::SizeGroup; 3],
    /// What the list shows, so an unchanged refresh rebuilds nothing.
    shown: RefCell<Vec<HiddenAp>>,
}

impl Page {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Name and Join, sized like the action bar.
        let form = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        form.add_css_class("hidden-bar");
        let name = gtk::Entry::builder()
            .placeholder_text("Hidden network name")
            .hexpand(true)
            // Let the field shrink, so the row never forces the window wider.
            .width_chars(6)
            .valign(gtk::Align::Center)
            .build();
        let join = gtk::Button::builder()
            .label("Join")
            .sensitive(false)
            .valign(gtk::Align::Center)
            .focus_on_click(false)
            .build();
        join.add_css_class("action");
        form.append(&name);
        form.append(&join);
        root.append(&form);

        let (head, columns) = widgets::heading("ADDRESS", ["SIGNAL", "dBm", "SEC"], 12);
        root.append(&head);
        let (list, empty) = widgets::list(gtk::SelectionMode::None, EMPTY);
        root.append(&widgets::scrolled(&list));

        Self {
            root,
            name,
            join,
            list,
            empty,
            columns,
            shown: RefCell::default(),
        }
    }

    /// What the empty list says: why there is nothing, or the usual hint.
    pub fn set_empty(&self, why: Option<&str>) {
        self.empty.set_text(why.unwrap_or(EMPTY));
    }

    /// Lists the hidden access points in range, strongest first.
    pub fn show(&self, aps: Vec<HiddenAp>) {
        if *self.shown.borrow() == aps {
            return;
        }
        widgets::fill(&self.list, aps.iter().map(|ap| self.row(ap)));
        *self.shown.borrow_mut() = aps;
    }

    fn row(&self, ap: &HiddenAp) -> gtk::ListBoxRow {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let address = widgets::label(&format::mac(&ap.address), "");
        address.set_hexpand(true);
        address.set_ellipsize(gtk::pango::EllipsizeMode::End);
        line.append(&address);

        let signal = widgets::locked_meter(ap.dbm, ap.security);
        self.columns[0].add_widget(&signal);
        line.append(&signal);
        line.append(&widgets::cell(
            &format::signed(ap.dbm.into()),
            4,
            &self.columns[1],
        ));
        line.append(&widgets::cell(ap.security, 4, &self.columns[2]));

        let row = gtk::ListBoxRow::builder().activatable(false).build();
        row.set_child(Some(&line));
        row
    }
}
