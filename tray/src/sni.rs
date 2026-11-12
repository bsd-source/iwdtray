//! Minimal `StatusNotifierItem` (KDE/freedesktop tray protocol) over `GDBus`.
//!
//! The panel draws the icon; we only export properties, answer clicks, and
//! (re-)register with the `StatusNotifierWatcher` whenever one appears.

use std::cell::RefCell;
use std::rc::Rc;

use gio::glib::{self, Variant};
use gio::prelude::*;

use crate::export::Exported;

const ITEM_PATH: &str = "/StatusNotifierItem";
const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";

const INTROSPECTION: &str = r#"
<node>
  <interface name="org.kde.StatusNotifierItem">
    <method name="ContextMenu"><arg name="x" type="i"/><arg name="y" type="i"/></method>
    <method name="Activate"><arg name="x" type="i"/><arg name="y" type="i"/></method>
    <method name="SecondaryActivate"><arg name="x" type="i"/><arg name="y" type="i"/></method>
    <method name="Scroll"><arg name="delta" type="i"/><arg name="orientation" type="s"/></method>
    <signal name="NewTitle"/>
    <signal name="NewIcon"/>
    <signal name="NewToolTip"/>
    <signal name="NewStatus"><arg name="status" type="s"/></signal>
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="WindowId" type="i" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconPixmap" type="a(iiay)" access="read"/>
    <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="IconThemePath" type="s" access="read"/>
  </interface>
</node>"#;

const IFACE: &str = "org.kde.StatusNotifierItem";

/// What the panel currently shows.
struct Display {
    icon: String,
    title: String,
    theme_path: String,
}

/// A registered tray item. Dropping it removes the icon from the tray.
pub struct StatusNotifierItem {
    connection: gio::DBusConnection,
    display: Rc<RefCell<Display>>,
    _exported: Exported,
    owner: Option<gio::OwnerId>,
    // gio exports two types named `WatcherId`; the bus-name one can't be
    // named, so its release is captured in a closure instead.
    unwatch: Option<Box<dyn FnOnce()>>,
}

impl StatusNotifierItem {
    /// Exports the item on `connection` and registers it with the tray host.
    /// `on_activate` runs when the icon is clicked, with either button.
    /// Icon names are also looked up in `icon_theme_path`.
    pub fn new(
        connection: &gio::DBusConnection,
        icon_theme_path: &str,
        on_activate: impl Fn() + 'static,
    ) -> Result<Self, glib::Error> {
        let display = Rc::new(RefCell::new(Display {
            icon: "network-wireless-symbolic".to_owned(),
            title: "iwdtray".to_owned(),
            theme_path: icon_theme_path.to_owned(),
        }));
        let shown = Rc::clone(&display);

        let exported = Exported::new(connection, ITEM_PATH, INTROSPECTION, IFACE, |b| {
            b.method_call(move |_, _, _, _, method, _, invocation| match method {
                "Activate" | "ContextMenu" => {
                    on_activate();
                    invocation.return_value(None);
                }
                "SecondaryActivate" | "Scroll" => invocation.return_value(None),
                // GDBus validates names against the introspection data first.
                _ => unreachable!("unknown method {method}"),
            })
            .property(move |_, _, _, _, name| property(&shown.borrow(), name))
        })?;

        // Spec-recommended well-known name: org.kde.StatusNotifierItem-<pid>-<n>.
        let bus_name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        let owner = gio::bus_own_name_on_connection(
            connection,
            &bus_name,
            gio::BusNameOwnerFlags::NONE,
            |_, _| {},
            |_, name| eprintln!("Lost bus name {name}"),
        );

        // Register now if a tray host is running, and again whenever one
        // (re)starts, e.g. after a panel restart. The bus processes our
        // RequestName before this later Register call, so the name is
        // already ours when the watcher looks it up.
        let watcher = gio::bus_watch_name_on_connection(
            connection,
            WATCHER_NAME,
            gio::BusNameWatcherFlags::NONE,
            move |conn, _, _| register_with_watcher(&conn, &bus_name),
            |_, _| {},
        );

        Ok(Self {
            connection: connection.clone(),
            display,
            _exported: exported,
            owner: Some(owner),
            unwatch: Some(Box::new(move || gio::bus_unwatch_name(watcher))),
        })
    }

    /// Changes the icon and title, notifying the panel only if they differ.
    pub fn set(&self, icon: String, title: String) {
        {
            let mut d = self.display.borrow_mut();
            if d.icon == icon && d.title == title {
                return;
            }
            d.icon = icon;
            d.title = title;
        }
        for signal in ["NewIcon", "NewTitle", "NewToolTip"] {
            if let Err(e) = self
                .connection
                .emit_signal(None, ITEM_PATH, IFACE, signal, None)
            {
                eprintln!("Failed to emit {signal}: {e}");
            }
        }
    }
}

impl Drop for StatusNotifierItem {
    fn drop(&mut self) {
        if let Some(unwatch) = self.unwatch.take() {
            unwatch();
        }
        if let Some(id) = self.owner.take() {
            gio::bus_unown_name(id);
        }
    }
}

fn property(d: &Display, name: &str) -> Variant {
    match name {
        "Category" => "Hardware".to_variant(),
        "Id" => "iwdtray".to_variant(),
        "Title" => d.title.to_variant(),
        "Status" => "Active".to_variant(),
        "WindowId" => 0i32.to_variant(),
        "IconName" => d.icon.to_variant(),
        "IconThemePath" => d.theme_path.to_variant(),
        "IconPixmap" => no_pixmaps().to_variant(),
        // The pixmaps go in unwrapped: a Variant inside a tuple would be
        // sent boxed, as `v`, giving (svss) where the spec says (sa(iiay)ss).
        "ToolTip" => (d.icon.as_str(), no_pixmaps(), d.title.as_str(), "").to_variant(),
        "ItemIsMenu" => false.to_variant(),
        // GDBus validates names against the introspection data first.
        _ => unreachable!("unknown property {name}"),
    }
}

fn no_pixmaps() -> Vec<(i32, i32, Vec<u8>)> {
    Vec::new()
}

fn register_with_watcher(connection: &gio::DBusConnection, bus_name: &str) {
    connection.call(
        Some(WATCHER_NAME),
        WATCHER_PATH,
        WATCHER_NAME,
        "RegisterStatusNotifierItem",
        Some(&(bus_name,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
        |result| {
            if let Err(e) = result {
                eprintln!("Failed to register with StatusNotifierWatcher: {e}");
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{Display, property};

    #[test]
    fn properties_have_the_types_the_spec_gives() {
        let d = Display {
            icon: "network-wireless-symbolic".into(),
            title: "Lighthouse".into(),
            theme_path: String::new(),
        };
        assert_eq!(property(&d, "IconPixmap").type_().as_str(), "a(iiay)");
        assert_eq!(property(&d, "ToolTip").type_().as_str(), "(sa(iiay)ss)");
    }
}
