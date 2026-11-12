#!/usr/bin/env bash
# Runs inside the container started by scripts/build-packages.sh, from the
# source tree it unpacked to /build/iwdtray: builds and tests it, then writes
# the package to /out.
set -euo pipefail

kind="$1"
stage=/build/stage

case "$kind" in
    deb)
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends \
            ca-certificates curl gcc libc6-dev pkg-config libgtk-4-dev \
            dbus-daemon dpkg-dev >/dev/null
        ;;
    rpm)
        # Move on from a slow mirror instead of waiting on it.
        printf 'minrate=100k\ntimeout=20\nfastestmirror=True\n' >> /etc/dnf/dnf.conf
        dnf -y -q install gcc pkgconf-pkg-config gtk4-devel dbus-daemon rpm-build
        ;;
esac

if [[ ! -x /root/.cargo/bin/cargo ]]; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --no-modify-path
fi
export PATH="/root/.cargo/bin:$PATH"
rustup -q update stable

cd /build/iwdtray
cargo build --locked --release
cargo test --locked --release

install -Dm755 -t "$stage/usr/bin" target/release/iwdtray target/release/iwdtray-gui
install -Dm644 data/iwdtray.conf "$stage/etc/iwdtray/iwdtray.conf"
install -Dm644 data/iwdtray.desktop "$stage/etc/xdg/autostart/iwdtray.desktop"
install -Dm644 -t "$stage/usr/share/applications" data/io.github.bsd_source.iwdtray.desktop

case "$kind" in
    deb)
        install -Dm644 COPYING "$stage/usr/share/doc/iwdtray/copyright"
        arch="$(dpkg --print-architecture)"
        # dpkg-shlibdeps wants a debian/control to exist.
        mkdir -p debian && echo "Source: iwdtray" > debian/control
        shlibs="$(dpkg-shlibdeps -O "$stage"/usr/bin/* | sed 's/^shlibs:Depends=//')"
        mkdir -p "$stage/DEBIAN"
        # Every file under /etc, so dpkg keeps the admin's edits.
        printf '%s\n' /etc/iwdtray/iwdtray.conf /etc/xdg/autostart/iwdtray.desktop \
            > "$stage/DEBIAN/conffiles"
        cat > "$stage/DEBIAN/control" <<EOF
Package: iwdtray
Version: $VERSION-1
Architecture: $arch
Maintainer: bsd-source <git@null.net>
Homepage: https://github.com/bsd-source/iwdtray
Installed-Size: $(du -sk --exclude=DEBIAN "$stage" | cut -f1)
Depends: iwd, $shlibs
Conflicts: iwgtk
Section: net
Priority: optional
Description: Tray icon and GUI front-end for iwd
 iwdtray shows Wi-Fi status in the system tray and answers iwd's passphrase
 requests. iwdtray-gui manages networks, saved networks and the hotspot.
EOF
        dpkg-deb --root-owner-group -Zxz --build "$stage" "/out/iwdtray_${VERSION}-1_${arch}.deb"
        ;;
    rpm)
        rpmbuild -bb -D "_topdir /build/rpm" -D "pkgver $VERSION" -D "stage $stage" \
            -D "license_file /build/iwdtray/COPYING" packaging/rpm/iwdtray.spec
        cp /build/rpm/RPMS/*/*.rpm /out/
        ;;
esac
