# Changelog

All notable changes to Time Portal are documented here. The format is loosely
[Keep a Changelog](https://keepachangelog.com/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [0.1.0] — 2026-10-17

First release.

### Added
- GTK4/libadwaita app with a system-tray icon, plus a polkit-driven privileged
  helper (`time-portal-helper`) for the steps that need root.
- Encrypted (LUKS) + btrfs backup drive set up from the app: erase, format,
  mount, and schedule in one flow.
- Hourly btrfs snapshot + incremental `btrfs send -p` backups to the drive,
  with Time Machine's retention cadence (`24h / 30d / 12m / 3y`).
- Restore wizard: restore your home from a version, or rebuild the whole system
  (from a live environment).
- Pause and resume a long backup in place (SIGSTOP/SIGCONT of the send/receive
  pipeline); live status with phase, percent and bytes shown in the app and the
  tray menu.
- AppStream metadata and a reverse-DNS desktop entry for software centres.
- `dev-install.sh` for installing a local build; `packaging/PKGBUILD` for the
  AUR package.
