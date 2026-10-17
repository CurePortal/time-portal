# Time Portal

Time Machine for Arch/Omarchy — backups that are **real, browsable filesystem
snapshots you restore in place**, not an opaque archive.

Every backup is a btrfs subvolume on an encrypted drive that looks exactly like
your machine at that moment: ownership, permissions, ACLs, xattrs, symlinks,
device nodes — all of it. Restoring a file is a copy; restoring the machine is a
subvolume swap. That is the whole point, and it is what file-level tools
(restic, borg) cannot give you.

## How it works

```
  your machine (btrfs, subvols @ and @home, LUKS-encrypted)
        │  btrbk: read-only snapshot  →  btrfs send -p (delta)
        ▼
  backup drive (LUKS + btrfs)
        └─ portal/@.<timestamp>        full tree of the root filesystem
           portal/@home.<timestamp>    full tree of /home
           portal/boot-esp/            the vfat ESP, mirrored
           portal/.timeportal.json     self-describing manifest
```

- **btrbk** takes hourly read-only snapshots and sends only the delta to the
  drive. Versions are real subvolumes that share extents (COW), so many versions
  stay small.
- The drive is **LUKS + btrfs**. LUKS is what encrypts the backup — `btrfs send`
  streams are plaintext, so this matters.
- Retention is Time Machine's: `24h / 30d / 12m / 3y`.

## Requirements

`gtk4`, `libadwaita`, `btrbk`, `cryptsetup`, `btrfs-progs`, `rsync`, `polkit`.
For the app to prompt for a passphrase you need a **polkit agent** in your
session (`hyprpolkitagent` on Hyprland; Omarchy's shell ships one).

## Install

```
yay -S time-portal
```

Everything ships in one package: the **app** and its **privileged helper**
(driven by polkit), plus the **btrbk systemd units** for the scheduled backups.
Launching the app also puts an icon in the **system tray**; closing the window
keeps it there (toggle *Keep running in the tray* in the app or the tray menu).

## Restoring on a new machine

1. `yay -S time-portal`
2. Plug in the backup drive. Launch **Time Portal**.
3. Pick the drive, type the **recovery passphrase** (the one you stored
   off-machine), choose a version.
4. **Restore My Files** copies your home back (correct ownership); **Restore
   System** rebuilds the whole tree, best run from a live USB.

The privileged steps (`cryptsetup open`, `mount`, `rsync`) run through
`pkexec`/polkit — the passphrase is passed via a 0600 file, never on a command
line.

## Setting up backups

The package installs the btrbk config and units. In the app, **Set up a new
backup drive** lists a spare disk, asks for a passphrase, and does the rest
(encrypt, format, mount, schedule). By hand it is:

```bash
sudo cryptsetup luksFormat --type luks2 --label backup-crypt /dev/sdX1
sudo cryptsetup open --key-file /etc/luks/backup.key /dev/sdX1 backup
sudo mkfs.btrfs -L BACKUP /dev/mapper/backup
mount /dev/mapper/backup /mnt/backup
mount -o subvolid=5 /dev/mapper/<root> /mnt/btrfs   # source top-level
time-portal-helper enable-backup                    # hourly schedule
```

Keep the **passphrase** somewhere other than this laptop: the keyfile unlocks
the drive automatically here, but the passphrase is what opens it on a machine
that has never seen your keys.

## Architecture

```
crates/
  core/     parse_ts, snapshot versions, the manifest, lsblk probing, restore plans  (pure, tested)
  helper/   the privileged backend: probe/list/unlock/lock/mount/restore/
            backup-esp/disks/format-drive/write-manifest/start-stop/enable        (polkit)
  gui/      the GTK4/libadwaita application (window + system tray)
data/       .desktop, polkit .policy, icon, btrbk.conf, systemd units
packaging/  PKGBUILD
```

The GUI never does anything privileged itself; it shells out to
`time-portal-helper`, which polkit authorizes via `org.timeportal.helper.run`.
The helper is also runnable directly when already root (a live USB).

## Building from source

```bash
cargo build --release
cargo test
# binaries: target/release/time-portal, target/release/time-portal-helper
```

For a dev run without installing, point the app at the helper you just built:

```bash
TIME_PORTAL_HELPER=$PWD/target/debug/time-portal-helper ./target/debug/time-portal
```

## Licence

MIT.
