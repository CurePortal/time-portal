# Time Portal (alpha) — a Time Machine for Omarchy: browsable btrfs snapshots on an encrypted drive. Looking for testers.

*(Draft announcement for r/omarchy and r/linuxonmac.)*

Hi all,

I've been building **Time Portal**, a Time Machine–style backup and restore tool
for Arch/Omarchy, and I'm putting it out as an **alpha** to find people willing
to test it.

**What it is.** Every backup is a real, browsable btrfs snapshot on a
LUKS‑encrypted drive — not an opaque archive. It snapshots `@` and `@home`,
sends only the delta (`btrfs send -p`) to the drive, and keeps versions on Time
Machine's cadence (hourly, then `24h / 30d / 12m / 3y`). Because the versions
are real subvolumes, you can plug the drive into any Linux box and just `ls`/`cp`
files out — no special software, no repository format. Restoring a file is a
copy; restoring the machine is a subvolume swap.

There's a GTK4/libadwaita app with a system-tray icon, a one-shot "set up a new
backup drive" flow (encrypt + format + schedule), live progress, and
**pause/resume** for that first, multi-hour full backup.

**Where I need help.** It works on my machine — backups run, the tray/status/
pause all behave — but I need more people running it **with non-critical data**
so we can shake out the restore path. Please don't point it at the only copy of
anything you care about yet. Requirements: a btrfs root, a spare disk to become
the encrypted backup drive, and a polkit agent for the prompt.

**The big safety note — do NOT restore over the running system.** "Restore
System" writes a whole-machine version over a *target root*. If you aim that at
the `/` you're currently booted into, you can wreck the running system. It's
meant to be run **from a live USB, against a mounted target root** (or to
re-image the same machine from a live environment). "Restore My Files" (home) is
far safer — it can't touch the boot path — but still test carefully.

**Live media.** The tool is *designed* for exactly this: the privileged helper
runs directly as root from a live USB, so the ideal restore is to **boot a live
USB/CD**, plug in the encrypted backup drive, unlock it with your recovery
passphrase, and restore. That's why I think it'd be genuinely valuable for
**Omarchy to ship it on the live/install ISO** — boot the Omarchy media, plug in
your Time Portal drive, restore your machine. It's MIT, free to bundle, and
there's a section in the README aimed at that.

**Try it:**
- Repo: https://github.com/CurePortal/time-portal
- Landing page: https://cureportal.github.io/time-portal/
- Download: https://github.com/CurePortal/time-portal/releases/latest
- Install: `sudo pacman -U time-portal-0.1.0-1-x86_64.pkg.tar.zst` (deps come
  from the official repos), or build from source.

This is **alpha — still in testing phases, be careful.** If you try it: set up a
spare drive, let a few backups run, then attempt restores onto a throwaway disk
/ non-critical files, and tell me what breaks. Logs,
`journalctl -u btrbk.service` output, and errors are all welcome. Thanks!
