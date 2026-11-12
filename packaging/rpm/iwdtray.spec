# Packages binaries already built by packaging/container-build.sh; run
# scripts/build-packages.sh rpm rather than rpmbuild directly.
%global debug_package %{nil}
%global source_date_epoch_from_changelog 0

Name:           iwdtray
Version:        %{pkgver}
Release:        1%{?dist}
Summary:        Tray icon and GUI front-end for iwd
License:        GPL-3.0-or-later
URL:            https://github.com/bsd-source/iwdtray
Requires:       iwd
Requires:       gtk4 >= 4.12
Conflicts:      iwgtk

%description
iwdtray shows Wi-Fi status in the system tray and answers iwd's passphrase
requests. iwdtray-gui manages networks, saved networks and the hotspot.

%install
cp -a %{stage}/. %{buildroot}/
install -Dm644 %{license_file} %{buildroot}%{_licensedir}/%{name}/COPYING

%files
%license %{_licensedir}/%{name}/COPYING
%{_bindir}/iwdtray
%{_bindir}/iwdtray-gui
%dir %{_sysconfdir}/iwdtray
%config(noreplace) %{_sysconfdir}/iwdtray/iwdtray.conf
%config(noreplace) %{_sysconfdir}/xdg/autostart/iwdtray.desktop
%{_datadir}/applications/io.github.bsd_source.iwdtray.desktop
