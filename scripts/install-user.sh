#!/usr/bin/env bash
# Per-user install of iwdtray, for trying it out without a package.
#
#   scripts/install-user.sh            build, install, start at login
#   scripts/install-user.sh uninstall  undo everything this script did
#
# Binaries go to ~/.local/lib/iwdtray, which is deliberately not on PATH. The C
# iwgtk indicator's system autostart entry is hidden for this user only, so
# the two don't both sit in the tray.
set -euo pipefail

prefix="$HOME/.local/lib/iwdtray"
autostart="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
tray_entry="$autostart/iwdtray.desktop"
c_override="$autostart/iwgtk-indicator.desktop"
marker="X-iwdtray-override=true"

ours() {
    [[ -f "$c_override" ]] && grep -qx "$marker" "$c_override"
}

uninstall() {
    rm -f "$tray_entry"
    rm -rf "$prefix"
    echo "Removed."
    # Only remove the override if we created it.
    if ours; then
        rm -f "$c_override"
        echo "The C indicator will autostart again from the next login."
    fi
}

do_install() {
    local root
    root="$(cd "$(dirname "$0")/.." && pwd)"
    cargo build --release --locked --manifest-path "$root/Cargo.toml"

    install -Dm755 "$root/target/release/iwdtray" "$prefix/iwdtray"
    install -Dm755 "$root/target/release/iwdtray-gui" "$prefix/iwdtray-gui"

    # The packaged autostart entry, pointed at this install.
    mkdir -p "$autostart"
    sed "s|^Exec=.*|Exec=\"$prefix/iwdtray\"|" "$root/data/iwdtray.desktop" > "$tray_entry"

    if [[ -f "$c_override" ]] && ! ours; then
        echo "Leaving existing $c_override alone (not created by this script)." >&2
    else
        cat > "$c_override" <<EOF
[Desktop Entry]
Type=Application
Name=iwgtk
Hidden=true
$marker
EOF
    fi
    echo "Installed to $prefix; iwdtray starts at login."
}

case "${1:-install}" in
    install) do_install ;;
    uninstall) uninstall ;;
    *) echo "usage: $0 [install|uninstall]" >&2; exit 2 ;;
esac
