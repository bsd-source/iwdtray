//! Text formatting for readings, kept free of GTK so it can be unit tested.

use crate::iwd::Diagnostics;

/// `-46` → `"−46"`, with a real minus sign so columns line up.
pub fn signed(v: i64) -> String {
    if v < 0 {
        format!("\u{2212}{}", v.unsigned_abs())
    } else {
        v.to_string()
    }
}

/// How long ago something happened, for times under a month:
/// "just now", "5 min ago", "3 h ago", "yesterday", "12 days ago".
/// `None` for older times (shown as a date instead) or future ones; up to
/// a minute ahead counts as now, since clocks and rounding drift.
pub fn ago(secs: i64) -> Option<String> {
    const MIN: i64 = 60;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    const TWO_DAYS: i64 = 2 * DAY;
    const MONTH: i64 = 30 * DAY;
    const AHEAD: i64 = -MIN;
    Some(match secs {
        ..AHEAD => return None,
        AHEAD..MIN => "just now".to_owned(),
        MIN..HOUR => format!("{} min ago", secs / MIN),
        HOUR..DAY => format!("{} h ago", secs / HOUR),
        DAY..TWO_DAYS => "yesterday".to_owned(),
        TWO_DAYS..=MONTH => format!("{} days ago", secs / DAY),
        _ => return None,
    })
}

/// Number of dots in a signal meter.
pub const DOTS: u8 = 5;

/// How many of the meter's dots are lit: 5 at −60 dBm or more, one fewer
/// for each shared threshold the signal is below, down to 1. Uses the same
/// levels as the tray icon.
pub fn signal_dots(dbm: i16) -> u8 {
    DOTS - iwdtray_common::iwd::signal_level(dbm)
}

/// `9607` (units of 100 kbit/s) → `"960.7"` Mbit/s.
pub fn megabits(rate: u32) -> String {
    format!("{}.{}", rate / 10, rate % 10)
}

/// `856` → `"00:14:16"`.
pub fn uptime(secs: u32) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// `"WPA3-Personal"` → `"WPA3-PSK"`, which fits the readout cell.
pub fn security(s: &str) -> String {
    s.replace("-Personal", "-PSK")
        .replace("-Enterprise", "-EAP")
}

/// MAC addresses are shown in uppercase; they're kept lowercase internally
/// to match the kernel's ARP table.
pub fn mac(address: &str) -> String {
    address.to_ascii_uppercase()
}

/// Shown for a value iwd didn't report.
pub const DASH: &str = "\u{2014}";

/// Number of readouts, shown three per row.
pub const READOUTS: usize = 9;

/// The readouts as (label, value, unit), in display order.
pub fn readouts(d: Option<&Diagnostics>) -> [(&'static str, String, &'static str); READOUTS] {
    let dash = || DASH.to_owned();
    let none = Diagnostics::default();
    let d = d.unwrap_or(&none);
    let dbm = |v: Option<i16>| v.map_or_else(dash, |v| signed(v.into()));
    [
        ("SIGNAL", dbm(d.rssi), "dBm"),
        ("AVERAGE", dbm(d.average_rssi), "dBm"),
        ("UP TIME", d.connected_secs.map_or_else(dash, uptime), ""),
        (
            "DOWNLOAD",
            d.rx_bitrate.map_or_else(dash, megabits),
            "Mbit/s",
        ),
        ("UPLOAD", d.tx_bitrate.map_or_else(dash, megabits), "Mbit/s"),
        (
            "FREQUENCY",
            d.frequency.map_or_else(dash, |f| f.to_string()),
            "MHz",
        ),
        (
            "CHANNEL",
            d.channel.map_or_else(dash, |c| c.to_string()),
            "",
        ),
        (
            "SECURITY",
            d.security.as_deref().map_or_else(dash, security),
            "",
        ),
        ("ENCRYPTION", d.cipher.clone().unwrap_or_else(dash), ""),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_this_laptops_link() {
        assert_eq!(signed(-46), "\u{2212}46");
        assert_eq!(megabits(9607), "960.7");
        assert_eq!(uptime(856), "00:14:16");
        assert_eq!(security("WPA3-Personal"), "WPA3-PSK");
    }

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(5).as_deref(), Some("just now"));
        assert_eq!(ago(4 * 60 + 59).as_deref(), Some("4 min ago"));
        assert_eq!(ago(3 * 3600).as_deref(), Some("3 h ago"));
        assert_eq!(ago(30 * 3600).as_deref(), Some("yesterday"));
        assert_eq!(ago(12 * 86_400).as_deref(), Some("12 days ago"));
        assert_eq!(ago(40 * 86_400), None);
        assert_eq!(ago(-10).as_deref(), Some("just now"));
        assert_eq!(ago(-3600), None);
    }

    #[test]
    fn signal_dots_count_down_from_full() {
        // The thresholds themselves are tested in iwdtray-common.
        assert_eq!(signal_dots(-37), DOTS);
        assert_eq!(signal_dots(-120), 1);
    }

    #[test]
    fn readouts_without_diagnostics_are_dashes() {
        assert!(readouts(None).iter().all(|(_, v, _)| v == DASH));
    }
}
