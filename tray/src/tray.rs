//! Ties iwd's status, the configured colours and the icon theme to the tray item.

use std::cell::RefCell;

use gio::glib;

use crate::config::Colors;
use crate::icons::{self, Colorizer};
use crate::sni::StatusNotifierItem;
use crate::status::Status;

pub struct Tray {
    item: StatusNotifierItem,
    colors: Colors,
    colorizer: RefCell<Colorizer>,
    theme: RefCell<String>,
    status: RefCell<Option<Status>>,
}

impl Tray {
    pub fn new(
        session: &gio::DBusConnection,
        on_activate: impl Fn() + 'static,
    ) -> Result<Self, glib::Error> {
        let colorizer = Colorizer::new();
        let item =
            StatusNotifierItem::new(session, &colorizer.dir().to_string_lossy(), on_activate)?;
        Ok(Self {
            item,
            colors: Colors::load(),
            colorizer: RefCell::new(colorizer),
            theme: RefCell::new(icons::current_theme(session)),
            status: RefCell::new(None),
        })
    }

    pub fn show(&self, status: &Status) {
        *self.status.borrow_mut() = Some(status.clone());
        self.render();
    }

    pub fn set_theme(&self, theme: String) {
        if *self.theme.borrow() != theme {
            *self.theme.borrow_mut() = theme;
            self.render();
        }
    }

    fn render(&self) {
        let status = self.status.borrow();
        let Some(status) = status.as_ref() else {
            return;
        };
        let base = status.icon_name();
        let icon = status
            .color(&self.colors)
            .and_then(|color| {
                self.colorizer
                    .borrow_mut()
                    .colored(&self.theme.borrow(), base, color)
            })
            .unwrap_or_else(|| base.to_owned());
        self.item.set(icon, status.title());
    }
}
