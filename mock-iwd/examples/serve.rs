//! Serves the fake iwd until killed, for looking at iwdtray against it:
//!
//!     cargo run -p mock-iwd --example serve [hotspot]
//!
//! It prints the private bus address; start iwdtray-gui with
//! `DBUS_SYSTEM_BUS_ADDRESS=<address>` to use it. With `hotspot`, the device
//! starts in AP mode with a hotspot running and one client joined. After
//! the address it prints each call the fake receives.

use mock_iwd::{DEVICE, MockIwd};

fn main() {
    let iwd = MockIwd::start();
    if std::env::args().any(|a| a == "hotspot") {
        iwd.switch_mode("ap");
        iwd.set(DEVICE, "AccessPoint", "Started", true);
        iwd.set(DEVICE, "AccessPoint", "Name", "MyHotspot");
        iwd.add_interface(DEVICE, "AccessPointDiagnostic", &[]);
    }
    println!("{}", iwd.address());
    // Then every call the fake receives, as it arrives.
    let mut seen = 0;
    loop {
        let calls = iwd.calls();
        for call in &calls[seen..] {
            println!("{call}");
        }
        seen = calls.len();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
