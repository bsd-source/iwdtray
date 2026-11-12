//! Code shared by `iwdtray` and `iwdtray-gui`. It depends only on `gio`, so the
//! tray can use it without loading GTK.

pub mod config;
pub mod iwd;
