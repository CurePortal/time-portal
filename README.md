# Time Portal

Time Machine for Arch/Omarchy — backups that are **real, browsable filesystem
snapshots you restore in place**, not an opaque archive.

Every backup is a btrfs subvolume on an encrypted drive that looks exactly like
your machine at that moment: ownership, permissions, ACLs, xattrs, symlinks,
device nodes — all of it. Restoring a file is a copy; restoring the machine is a
subvolume swap. That is the whole point, and it is the one thing file-level
tools (restic, borg, duplicity) deliberately do not give you.

## Why it is Time Machine, on Linux

Apple's Time Machine is four ideas:

1. a copy-on-write filesystem that can freeze a consistent point in time for
   free (APFS snapshots),
2. deltas — hourly, incremental — to an external drive,
3. tiered retention (`24h / 30d / 12m / 3y`),
4. versions you can actually *see* and pull individual files out of.

Time Portal is those same four ideas with btrfs in APFS's seat:

| Time Machine | Time Portal |
| --- | --- |
| APFS snapshots | btrfs read-only subvolume snapshots |
| incremental backup to a drive | `btrfs send -p` deltas over the wire |
| the external drive holds real versions | the drive **is** a btrfs filesystem: `portal/@home.<ts>/…` |
| restore a file by dragging it out | restore a file by `cp`; restore the machine by a subvolume swap |
| `24h / 30d / 12m / 3y` retention | `24h / 30d / 12m / 3y` retention |
| FileVault-encrypted external | LUKS-encrypted external |

The important difference from a restic/borg repository is the **medium**. A
restic repo is opaque: to read one file you need restic, the repository, the
password, and a working `restic`. A Time Portal drive is an ordinary btrfs
filesystem — plug it into any Linux machine, mount it, and browse the versions
with `ls` and `cp`. No special software, no repository format, nothing to go
stale.

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

## How it compares

The Linux backup space is crowded, and the honest answer is that these tools
solve **different problems**. Time Portal is not a replacement for snapper or
restic; it is the one piece of the Time Machine puzzle they don't cover.

| | Time Portal | snapper + Limine | omarchy-time-machine (restic) | Timeshift | Back In Time |
| --- | --- | --- | --- | --- | --- |
| The job | Whole-machine backup to a drive | Instant rollback | Off-site / cloud archive | System restore points | File backup |
| Engine | btrfs send/receive (btrbk) | btrfs snapshots | restic | rsync / btrfs | rsync |
| Medium is a browsable filesystem | **yes** | yes (local only) | no — opaque repo | partial | yes |
| Survives disk failure / theft | **yes** | no | yes | partial | yes |
| Encrypted at rest | **LUKS** | no | yes | no | optional |
| Preserves ACLs, xattrs, device nodes | **yes** | yes | userspace copy | partial | partial |
| Restore a whole machine onto a new box | **subvolume swap** | no | extract a tree | no | partial |
| Restore one file without special tools | **yes (`cp`)** | local only | no (needs restic) | partial | yes |
| Network / NAS / S3 targets | no (drive only) | no | **yes** | no | no |
| GUI | GTK4 app + tray | CLI / boot menu | Quickshell panel | GTK | GTK/Qt |

**snapper + Limine** (what Omarchy runs by default) takes btrfs snapshots on
every package transaction and makes them bootable. It is the best possible
*rollback* — seconds to undo a bad update — and it is **not a backup**: the
snapshots live on the same disk, so a dead disk, a stolen laptop, or a mistaken
`rm` of the snapshot tree takes the history with it. Time Portal is the
off-machine half that snapper intentionally leaves out.

**omarchy-time-machine** (the `jankeesvw.time-machine` plugin) drives **restic**
repositories on a local disk, a NAS (`rest:http://…`), or S3, with
content-defined deduplication, encryption, `forget`/`prune` retention, and a
Quickshell panel. It is excellent when you want dedup and a *remote or cloud*
target, and a repository you can verify with `restic check`. Its trade-off is
the repository itself: reading one file means having restic, the repo, and the
password; ACLs, xattrs and device nodes are reconstructed by restic rather than
preserved as filesystem state; and "restore the machine" means extracting a tree,
not swapping a subvolume. Time Portal gives you the browsable-medium and
in-place-restore properties restic gives up; restic gives you the network targets
Time Portal gives up. Run both if you have a NAS.

**Timeshift** is a system-restore tool (rsync hardlinks or btrfs snapshots) built
for rolling back a broken system. By its own documentation it is not for your
personal data, it is not encrypted, and its external-drive story is awkward.

**Back In Time** is the closest in *feel* — an rsync snapshot tool with a
Time-Machine-like browsing UI — but it makes userspace copies (slower, less
metadata fidelity), does not do a whole-machine subvolume swap, and does not
encrypt by default.

**borg / restic / duplicity generally** are superb deduplicating archives for
remote targets. They are opaque repositories by design; that is a feature for the
cloud and a liability for "open the drive and grab last Tuesday's file".

So: keep **snapper** for rollback, keep **restic** for the cloud/NAS copy, and
use **Time Portal** for the Time-Machine-style, encrypted, browsable,
whole-machine backup on the drive in your bag — the one you can hand to a new
computer and boot from.

## Requirements

`gtk4`, `libadwaita`, `btrbk`, `cryptsetup`, `btrfs-progs`, `rsync`, `polkit`.
For the app to prompt for a passphrase you need a **polkit agent** in your
session (`hyprpolkitagent` on Hyprland; Omarchy's shell ships one). The source
filesystem must be **btrfs**.

## Install

```
yay -S time-portal
```

Everything ships in one package: the **app** and its **privileged helper**
(driven by polkit), plus the **btrbk systemd units** for the scheduled backups.
Launching the app also puts an icon in the **system tray**; closing the window
keeps it there (toggle *Keep running in the tray* under **Settings** or in the
tray menu).

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

You do not strictly need this app to read a backup: the drive is plain
LUKS + btrfs, so `cryptsetup open`, mount, and browse the `@home.<timestamp>`
subvolume directly on any Linux box.

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

## Pausing a long backup

The first backup of a full disk can run for many hours, so a run can be paused
and resumed without starting over. **Pause** freezes the running `btrfs
send`/`receive` pipeline in place (SIGSTOP); the in-flight subvolume and its
pipe buffers are untouched, and **Resume** (SIGCONT) continues from the same
byte. Use it from the app's buttons or the **tray menu**, which shows the live
status — phase, percent, bytes sent — on hover and at the top of the menu, with
*Pause*/*Resume*/*Stop* to match the current state.

Caveats, because the pause is a frozen process and not a checkpoint:

- A paused run must stay paused on the **same boot**: a reboot, an unplug, or a
  crash loses the in-flight subvolume (the next run re-sends it). Nothing else
  on the machine is affected — only the backup is frozen.
- The scheduled hourly timer still fires while paused, but btrbk's lock keeps a
  second run from starting on top of the paused one.

## Architecture

```
crates/
  core/     parse_ts, snapshot versions, the manifest, lsblk probing, restore plans  (pure, tested)
  helper/   the privileged backend: probe/list/unlock/lock/mount/restore/clean/
            backup-esp/disks/format-drive/write-manifest/start-stop/pause-resume/enable  (polkit)
  gui/      the GTK4/libadwaita application (window + system tray)
data/       .desktop, AppStream metainfo, polkit .policy, icon, btrbk.conf, systemd units
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

To install a local build system-wide (app, helper, desktop file, AppStream
metadata, polkit policy, btrbk units):

```bash
cargo build --release
sudo ./dev-install.sh
```

## Licence

MIT.
