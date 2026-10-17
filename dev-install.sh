#!/bin/bash
# Install Time Portal locally from a source build (before the AUR package exists).
#   sudo ~/time-portal/dev-install.sh
set -euo pipefail

[ "$(id -u)" = 0 ] || { echo "Run with sudo." >&2; exit 1; }
cd "$(dirname -- "${BASH_SOURCE[0]}")"

# clean up any pre-rename install
rm -f /usr/bin/btrfs-timemachine /usr/local/bin/btrfs-timemachine
rm -f /usr/lib/btrfs-timemachine/timemachine-helper
rm -f /usr/share/applications/btrfs-timemachine.desktop
rm -f /usr/share/icons/hicolor/scalable/apps/btrfs-timemachine.svg
rm -f /usr/share/polkit-1/actions/org.btrfstimemachine.policy
rmdir /usr/lib/btrfs-timemachine 2>/dev/null || true

install -Dm755 target/release/time-portal /usr/local/bin/time-portal
install -Dm755 target/release/time-portal-helper \
  /usr/lib/time-portal/time-portal-helper

install -Dm644 data/time-portal.desktop /usr/share/applications/time-portal.desktop
install -Dm644 data/time-portal.svg \
  /usr/share/icons/hicolor/scalable/apps/time-portal.svg
for s in 48 64 128 256; do
  install -Dm644 "data/icons/time-portal-$s.png" \
    "/usr/share/icons/hicolor/${s}x${s}/apps/time-portal.png"
done
install -Dm644 data/org.timeportal.policy \
  /usr/share/polkit-1/actions/org.timeportal.policy

# btrbk config + units (the backup half)
install -Dm644 data/btrbk.conf /etc/btrbk/btrbk.conf
install -Dm644 data/systemd/btrbk.service.d/override.conf \
  /etc/systemd/system/btrbk.service.d/override.conf
install -Dm644 data/systemd/btrbk.timer.d/override.conf \
  /etc/systemd/system/btrbk.timer.d/override.conf
install -Dm644 data/systemd/time-portal-mount.service \
  /usr/lib/systemd/system/time-portal-mount.service
systemctl daemon-reload

# hotplug automount for the attached drive, and mount it now
/usr/lib/time-portal/time-portal-helper install-hooks 2>/dev/null || true

update-desktop-database /usr/share/applications 2>/dev/null || true
gtk-update-icon-cache -f /usr/share/icons/hicolor 2>/dev/null || true

echo "Installed. Launch 'Time Portal' from the launcher, or run: time-portal"
