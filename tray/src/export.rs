//! Objects the tray exports on a D-Bus connection.

use gio::glib;

/// An exported object; dropping it removes it from the bus.
pub struct Exported {
    connection: gio::DBusConnection,
    id: Option<gio::RegistrationId>,
}

impl Exported {
    /// Exports interface `iface`, described in the introspection `xml`, at
    /// `path`. `handlers` adds the method call (and property) handlers.
    pub fn new(
        connection: &gio::DBusConnection,
        path: &str,
        xml: &str,
        iface: &str,
        handlers: impl for<'a> FnOnce(gio::RegistrationBuilder<'a>) -> gio::RegistrationBuilder<'a>,
    ) -> Result<Self, glib::Error> {
        let node = gio::DBusNodeInfo::for_xml(xml)?;
        let info = node
            .lookup_interface(iface)
            .expect("the interface is defined in its introspection XML");
        let id = handlers(connection.register_object(path, &info)).build()?;
        Ok(Self {
            connection: connection.clone(),
            id: Some(id),
        })
    }
}

impl Drop for Exported {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            let _ = self.connection.unregister_object(id);
        }
    }
}
