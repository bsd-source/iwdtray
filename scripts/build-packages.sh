#!/usr/bin/env bash
# Builds .deb and .rpm packages of the committed HEAD inside containers, so
# the binaries link against the oldest glibc and GTK they support instead of
# the host's.
#
#   scripts/build-packages.sh          build all into packaging/out
#   scripts/build-packages.sh deb rpm  build only those
#
# deb: built on Ubuntu 24.04, installs on Ubuntu 24.04+ and Debian 13+.
# rpm: built on Fedora 44, installs on Fedora 44+. RHEL 10 ships no iwd, and
# RHEL 9's Pango is too old for gtk4-rs.
# Needs podman. Cargo and rustup caches persist in podman volumes.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/packaging/out"
# From HEAD too, so the version always matches the source packaged.
version="$(git -C "$root" show HEAD:Cargo.toml | sed -n 's/^version = "\(.*\)"/\1/p' | head -n1)"

declare -A image=([deb]=docker.io/library/ubuntu:24.04 [rpm]=registry.fedoraproject.org/fedora:44)

mkdir -p "$out"
trap 'rm -f "$out/src.tar"' EXIT
git -C "$root" archive --prefix=iwdtray/ HEAD > "$out/src.tar"

(( $# )) || set -- deb rpm
for k in "$@"; do
    [[ -n "${image[$k]:-}" ]] || { echo "unknown package type: $k" >&2; exit 2; }
    # The packaging scripts come from the archive too, so they always match
    # the source they build.
    podman run --rm \
        -v "$out:/out:Z" \
        -v "iwdtray-$k-rustup:/root/.rustup" \
        -v "iwdtray-$k-cargo:/root/.cargo" \
        -e VERSION="$version" \
        "${image[$k]}" bash -c 'mkdir -p /build && tar -xf /out/src.tar -C /build &&
            exec bash /build/iwdtray/packaging/container-build.sh "$0"' "$k"
done
ls -1 "$out"/*.deb "$out"/*.rpm 2>/dev/null || true
