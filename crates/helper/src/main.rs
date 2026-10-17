//! Privileged backend for Time Portal.
//!
//! The GUI is unprivileged and shells out to this binary only through polkit
//! (`pkexec time-portal-helper <cmd>`), governed by
//! `org.timeportal.helper.*` actions. It never takes a passphrase on the
//! command line: `unlock` reads it from a 0600 file or from stdin.
//!
//! Run as root directly when already root (e.g. from a live USB).

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "time-portal-helper", version, about = "Privileged backend for Time Portal")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List candidate backup drives (LUKS containers) as JSON.
    Probe,
    /// Report state of a mounted target as JSON.
    Status {
        #[arg(long, default_value = "/mnt/backup/portal")]
        target: String,
    },
    /// List snapshot versions under a mounted target as JSON.
    List {
        #[arg(long, default_value = "/mnt/backup/portal")]
        target: String,
    },
    /// Write the self-describing manifest into a target directory.
    WriteManifest {
        #[arg(long, default_value = "/mnt/backup/portal")]
        target: String,
        #[arg(long)]
        host: Option<String>,
    },
    /// Open a LUKS device. Passphrase from --passphrase-file, else stdin.
    Unlock {
        device: String,
        #[arg(long, default_value = "backup")]
        name: String,
        #[arg(long)]
        passphrase_file: Option<String>,
    },
    /// Close a LUKS mapper by name.
    Lock {
        #[arg(long, default_value = "backup")]
        name: String,
    },
    /// Mount a device/mapper at a target directory.
    Mount { source: String, target: String },
    /// Unmount a directory.
    Umount { target: String },
    /// Restore a directory tree (source -> dest) preserving metadata.
    Restore {
        source: String,
        dest: String,
        #[arg(long)]
        delete: bool,
    },
    /// Start the backup unit (non-blocking).
    StartBackup,
    /// Run btrbk, writing live progress to /run/time-portal/progress.json.
    /// This is what the backup service runs (as root, so it can see bytes).
    RunBackup,
    /// Unlock and mount the backup drive (idempotent).
    MountDrive,
    /// Install the hotplug udev rule + mount service for the attached drive.
    InstallHooks,
    /// Stop the backup unit (non-blocking).
    StopBackup,
    /// Enable the hourly backup timer.
    EnableBackup,
    /// Disable the hourly backup timer.
    DisableBackup,
    /// Mirror /boot (the vfat ESP) next to the btrbk backups.
    BackupEsp {
        #[arg(long, default_value = "/boot")]
        source: String,
        #[arg(long, default_value = "/mnt/backup/portal/boot-esp")]
        dest: String,
    },
    /// List disks/partitions that could become a backup target, as JSON.
    Disks,
    /// Erase a device and set it up as an encrypted btrfs backup target.
    FormatDrive {
        device: String,
        #[arg(long)]
        passphrase_file: String,
        #[arg(long, default_value = "backup")]
        mapper: String,
        #[arg(long, default_value = "/mnt/backup")]
        mount_base: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        yes: bool,
    },
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Probe => probe(),
        Cmd::Status { target } => status(&target),
        Cmd::List { target } => list(&target),
        Cmd::WriteManifest { target, host } => write_manifest(&target, host),
        Cmd::Unlock { device, name, passphrase_file } => unlock(&device, &name, passphrase_file),
        Cmd::Lock { name } => run("cryptsetup", &["close", &name]),
        Cmd::Mount { source, target } => mount(&source, &target),
        Cmd::Umount { target } => run("umount", &[&target]),
        Cmd::Restore { source, dest, delete } => restore(&source, &dest, delete),
        Cmd::StartBackup => {
            // Mount the drive if it is attached but not mounted (a replug),
            // otherwise the service's mount-point conditions skip it silently.
            ensure_mounted().context("backup drive")?;
            run("systemctl", &["start", "--no-block", "btrbk.service"])
        }
        Cmd::RunBackup => run_backup(),
        Cmd::MountDrive => {
            ensure_mounted()?;
            println!("{}", serde_json::json!({"mounted": true}));
            Ok(())
        }
        Cmd::InstallHooks => install_hooks(),
        Cmd::StopBackup => run("systemctl", &["stop", "--no-block", "btrbk.service"]),
        Cmd::EnableBackup => run("systemctl", &["enable", "--now", "btrbk.timer"]),
        Cmd::DisableBackup => run("systemctl", &["disable", "--now", "btrbk.timer"]),
        Cmd::BackupEsp { source, dest } => backup_esp(&source, &dest),
        Cmd::Disks => disks(),
        Cmd::FormatDrive {
            device,
            passphrase_file,
            mapper,
            mount_base,
            host,
            yes,
        } => format_drive(&device, &passphrase_file, &mapper, &mount_base, host, yes),
    }
}

fn backup_esp(source: &str, dest: &str) -> Result<()> {
    if !mounted("/mnt/backup") {
        return Ok(());
    }
    std::fs::create_dir_all(dest).with_context(|| format!("creating {dest}"))?;
    run_str(
        "rsync",
        &[
            "-aH".into(),
            "--delete".into(),
            format!("{}/", source.trim_end_matches('/')),
            format!("{}/", dest.trim_end_matches('/')),
        ],
    )
}

fn probe() -> Result<()> {
    let out = Command::new("lsblk")
        .args(["-J", "-o", "NAME,PATH,TYPE,FSTYPE,LABEL,UUID,MOUNTPOINTS,SIZE"])
        .output()
        .context("running lsblk")?;
    let text = String::from_utf8(out.stdout).context("lsblk output not UTF-8")?;
    let devs = timeportal_core::devices::parse_lsblk(&text).context("parsing lsblk JSON")?;
    let luks = timeportal_core::devices::luks_devices(&devs);
    println!("{}", serde_json::to_string_pretty(&luks)?);
    Ok(())
}

fn mounted(target: &str) -> bool {
    std::fs::read_to_string("/proc/mounts")
        .map(|m| m.lines().any(|l| l.split_whitespace().nth(1) == Some(target)))
        .unwrap_or(false)
}

fn run_seconds() -> Option<i64> {
    let ts = Command::new("systemctl")
        .args(["show", "btrbk.service", "-p", "ExecMainStartTimestamp", "--value"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())?;
    let start = Command::new("date")
        .args(["-d", &ts, "+%s"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<i64>().ok())?;
    let now = Command::new("date")
        .arg("+%s")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<i64>().ok())?;
    Some(now - start)
}

fn status(target: &str) -> Result<()> {
    let exists = Path::new(target).is_dir();
    let versions = timeportal_core::scan_versions(Path::new(target)).unwrap_or_default();
    let running = Command::new("systemctl")
        .args(["is-active", "btrbk.service"])
        .output()
        .map(|o| {
            let s = String::from_utf8_lossy(&o.stdout);
            matches!(s.trim(), "active" | "activating" | "deactivating" | "reloading")
        })
        .unwrap_or(false);
    let timer_enabled = Command::new("systemctl")
        .args(["is-enabled", "btrbk.timer"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("enabled"))
        .unwrap_or(false);
    // The target is a directory *inside* the mounted filesystem, so check its
    // parent as well as the path itself.
    let is_mounted = mounted(target)
        || Path::new(target)
            .parent()
            .map(|p| mounted(&p.to_string_lossy()))
            .unwrap_or(false);
    let drive_present = backup_luks_device().is_some() || Path::new("/dev/mapper/backup").exists();
    let prog = read_progress();
    let phase = if running {
        prog.as_ref()
            .and_then(|p| p["phase"].as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(detect_phase)
    } else {
        String::new()
    };
    let secs = if running { run_seconds() } else { None };
    let percent = prog.as_ref().and_then(|p| p["percent"].as_f64());
    let done_bytes = prog.as_ref().and_then(|p| p["done_bytes"].as_u64());
    let total_bytes = prog.as_ref().and_then(|p| p["total_bytes"].as_u64());
    let obj = serde_json::json!({
        "target": target,
        "exists": exists,
        "mounted": is_mounted,
        "drive_present": drive_present,
        "running": running,
        "timer_enabled": timer_enabled,
        "phase": phase,
        "run_seconds": secs,
        "percent": percent,
        "done_bytes": done_bytes,
        "total_bytes": total_bytes,
        "version_count": versions.len(),
        "latest_ts": versions.last().map(|v| v.ts.clone()),
    });
    println!("{}", serde_json::to_string_pretty(&obj)?);
    Ok(())
}

fn list(target: &str) -> Result<()> {
    let versions = timeportal_core::scan_versions(Path::new(target))
        .with_context(|| format!("scanning {target}"))?;
    println!("{}", serde_json::to_string_pretty(&versions)?);
    Ok(())
}

fn write_manifest(target: &str, host: Option<String>) -> Result<()> {
    let host = host
        .or_else(|| {
            Command::new("hostname")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());
    let created = Command::new("date")
        .arg("-Iseconds")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let mut m = timeportal_core::Manifest::new(&host);
    m.created = created;
    m.save(Path::new(target))
        .with_context(|| format!("writing manifest into {target}"))?;
    println!("{}", m.describe());
    Ok(())
}

fn unlock(device: &str, name: &str, passphrase_file: Option<String>) -> Result<()> {
    let pass = match passphrase_file {
        Some(p) => std::fs::read_to_string(&p).context("reading passphrase file")?,
        None => {
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).context("reading passphrase from stdin")?;
            s
        }
    };
    if pass.is_empty() {
        bail!("no passphrase supplied");
    }
    let mut child = Command::new("cryptsetup")
        .args(["open", "--type", "luks2", "--key-file", "-", device, name])
        .stdin(Stdio::piped())
        .spawn()
        .context("spawning cryptsetup")?;
    child
        .stdin
        .take()
        .context("cryptsetup stdin")?
        .write_all(pass.as_bytes())
        .context("writing passphrase to cryptsetup")?;
    let status = child.wait().context("waiting for cryptsetup")?;
    if !status.success() {
        bail!("could not open {device} (wrong passphrase?)");
    }
    Ok(())
}

/// The attached Time Portal drive, identified by its LUKS label.
fn backup_luks_device() -> Option<String> {
    let out = Command::new("lsblk")
        .args(["-J", "-o", "NAME,PATH,TYPE,FSTYPE,LABEL,UUID,MOUNTPOINTS,SIZE"])
        .output()
        .ok()?;
    let devs =
        timeportal_core::devices::parse_lsblk(&String::from_utf8_lossy(&out.stdout)).ok()?;
    let mut flat = Vec::new();
    for d in &devs {
        d.flatten(&mut flat);
    }
    flat.into_iter()
        .find(|d| {
            d.fstype.as_deref() == Some("crypto_LUKS")
                && d.label.as_deref() == Some("backup-crypt")
        })
        .map(|d| d.path.clone())
}

/// Unlock (with the keyfile) and mount the backup drive. Idempotent.
fn ensure_mounted() -> Result<()> {
    let mapper = "/dev/mapper/backup";
    if !Path::new(mapper).exists() {
        let dev = backup_luks_device()
            .context("no Time Portal drive (LUKS label 'backup-crypt') is attached")?;
        let key = "/etc/luks/backup.key";
        if !Path::new(key).exists() {
            bail!("missing keyfile {key}");
        }
        run("cryptsetup", &["open", "--key-file", key, &dev, "backup"])
            .context("unlocking the backup drive")?;
    }
    std::fs::create_dir_all("/mnt/backup")?;
    if !mounted("/mnt/backup") {
        run("mount", &[mapper, "/mnt/backup"]).context("mounting /mnt/backup")?;
    }
    std::fs::create_dir_all("/mnt/btrfs")?;
    if !mounted("/mnt/btrfs") {
        let _ = run("mount", &["/mnt/btrfs"]);
    }
    Ok(())
}

/// Install the hotplug rule + mount service so a replugged drive is unlocked
/// and mounted automatically (no prompt), which the scheduled backup needs.
fn install_hooks() -> Result<()> {
    let uuid = backup_luks_device().as_deref().and_then(blkid_uuid);
    if let Some(u) = &uuid {
        let rule = format!(
            "ACTION==\"add|change\", SUBSYSTEM==\"block\", ENV{{ID_FS_UUID}}==\"{u}\", ENV{{UDISKS_IGNORE}}=\"1\", TAG+=\"systemd\", ENV{{SYSTEMD_WANTS}}=\"time-portal-mount.service\"\n"
        );
        std::fs::write("/etc/udev/rules.d/90-time-portal.rules", rule)
            .context("writing the udev rule")?;
    }
    std::fs::write(
        "/etc/systemd/system/time-portal-mount.service",
        "[Unit]\nDescription=Unlock and mount the Time Portal backup drive\nAfter=local-fs.target\n\n[Service]\nType=oneshot\nExecStart=/usr/lib/time-portal/time-portal-helper mount-drive\n",
    )
    .context("writing time-portal-mount.service")?;
    let _ = Command::new("udevadm").args(["control", "--reload-rules"]).status();
    let _ = Command::new("udevadm").args(["trigger", "--subsystem-match=block"]).status();
    let _ = Command::new("systemctl").arg("daemon-reload").status();
    let _ = Command::new("systemctl").args(["start", "time-portal-mount.service"]).status();
    println!("{}", serde_json::json!({"ok": true, "luks_uuid": uuid}));
    Ok(())
}

fn mount(source: &str, target: &str) -> Result<()> {
    std::fs::create_dir_all(target).with_context(|| format!("creating {target}"))?;
    run("mount", &[source, target])
}

fn restore(source: &str, dest: &str, delete: bool) -> Result<()> {
    let mut args: Vec<String> = vec!["-aAXH".into(), "--info=progress2".into()];
    if delete {
        args.push("--delete".into());
    }
    args.push(format!("{}/", source.trim_end_matches('/')));
    args.push(format!("{}/", dest.trim_end_matches('/')));
    run_str("rsync", &args)
}

// --- disk selection + provisioning -----------------------------------------

fn findmnt_field(target: &str, field: &str) -> Option<String> {
    Command::new("findmnt")
        .args(["-no", field, "--target", target])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn blkid_uuid(device: &str) -> Option<String> {
    Command::new("blkid")
        .args(["-s", "UUID", "-o", "value", device])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn hostname() -> Option<String> {
    Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn mount_sources() -> Vec<String> {
    std::fs::read_to_string("/proc/mounts")
        .map(|m| {
            m.lines()
                .filter_map(|l| l.split_whitespace().next().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// The device and all its backing devices (dm/partition/disk), as bare names,
/// from `lsblk --inverse`. Works for LUKS/dm where PKNAME is empty.
fn chain_names(path: &str) -> Vec<String> {
    Command::new("lsblk")
        .args(["-s", "-r", "-n", "-o", "NAME", path])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn device_mounted(device: &str) -> bool {
    let dev = basename(device);
    for s in mount_sources() {
        if s == device || s.starts_with(device) {
            return true;
        }
        if chain_names(&s).iter().any(|n| n == &dev) {
            return true;
        }
    }
    false
}

/// True if the device backs the running system (or is a disk that contains it).
fn is_protected(device: &str) -> bool {
    let root_source = match findmnt_field("/", "SOURCE") {
        Some(s) => s.split('[').next().unwrap_or("").to_string(),
        None => return true,
    };
    if root_source.is_empty() {
        return true;
    }
    if device == root_source || root_source.starts_with(device) {
        return true;
    }
    let dev = basename(device);
    chain_names(&root_source).iter().any(|n| n == &dev)
}

fn append_line(file: &str, line: &str) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .with_context(|| format!("opening {file}"))?;
    writeln!(f, "{line}").with_context(|| format!("writing {file}"))?;
    Ok(())
}

fn disks() -> Result<()> {
    let out = Command::new("lsblk")
        .args(["-J", "-o", "NAME,PATH,TYPE,FSTYPE,LABEL,UUID,MOUNTPOINTS,SIZE"])
        .output()
        .context("running lsblk")?;
    let devs = timeportal_core::devices::parse_lsblk(&String::from_utf8_lossy(&out.stdout))
        .context("parsing lsblk")?;
    let mut flat = Vec::new();
    for d in &devs {
        d.flatten(&mut flat);
    }
    let list: Vec<serde_json::Value> = flat
        .iter()
        .filter(|d| d.dtype == "disk" || d.dtype == "part")
        .filter(|d| !(d.name.starts_with("zram") || d.name.starts_with("loop") || d.name.starts_with("ram")))
        .map(|d| {
            serde_json::json!({
                "name": d.name,
                "path": d.path,
                "type": d.dtype,
                "size": d.size,
                "fstype": d.fstype,
                "label": d.label,
                "mounted": device_mounted(&d.path),
                "protected": is_protected(&d.path) || device_mounted(&d.path),
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&list)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn format_drive(
    device: &str,
    passphrase_file: &str,
    mapper: &str,
    mount_base: &str,
    host: Option<String>,
    yes: bool,
) -> Result<()> {
    if !yes {
        bail!("refusing to erase {device} without --yes");
    }
    if !Path::new(device).exists() {
        bail!("{device} does not exist");
    }
    if is_protected(device) {
        bail!("refusing to touch {device}: it backs the running system");
    }
    if device_mounted(device) {
        bail!("{device} is mounted; unmount it first");
    }
    if findmnt_field("/", "FSTYPE").as_deref() != Some("btrfs") {
        bail!("your root filesystem is not btrfs; btrbk needs btrfs on the source");
    }
    let root_uuid = findmnt_field("/", "UUID").context("reading the root btrfs UUID")?;
    let pass = std::fs::read_to_string(passphrase_file).context("reading passphrase file")?;
    if pass.trim().is_empty() {
        bail!("empty passphrase");
    }

    let mapper_path = format!("/dev/mapper/{mapper}");
    let keyfile = "/etc/luks/backup.key";

    // 1. LUKS2, the passphrase is the recovery key
    run_str(
        "cryptsetup",
        &[
            "luksFormat".into(),
            "--batch-mode".into(),
            "--type".into(),
            "luks2".into(),
            "--label".into(),
            "backup-crypt".into(),
            "--key-file".into(),
            passphrase_file.into(),
            device.into(),
        ],
    )
    .context("luksFormat failed")?;

    // 2. a keyfile slot for automatic unlock (the passphrase survives if the
    //    machine does not)
    std::fs::create_dir_all("/etc/luks").ok();
    if !Path::new(keyfile).exists() {
        run_str(
            "dd",
            &[
                "if=/dev/urandom".into(),
                format!("of={keyfile}"),
                "bs=512".into(),
                "count=4".into(),
                "status=none".into(),
            ],
        )?;
        let _ = Command::new("chmod").args(["600", keyfile]).status();
    }
    run_str(
        "cryptsetup",
        &[
            "luksAddKey".into(),
            device.into(),
            keyfile.into(),
            "--key-file".into(),
            passphrase_file.into(),
        ],
    )
    .context("luksAddKey failed")?;

    // 3. open, mkfs, close
    run_str("cryptsetup", &["open".into(), "--key-file".into(), keyfile.into(), device.into(), mapper.into()])?;
    run_str("mkfs.btrfs", &["-L".into(), "BACKUP".into(), mapper_path.clone()])?;
    let _ = Command::new("cryptsetup").args(["close", mapper]).status();

    // 4. persist unlock + mounts
    let luks_uuid = blkid_uuid(device).context("reading the LUKS UUID")?;
    append_line("/etc/crypttab", &format!("{mapper} UUID={luks_uuid} {keyfile} luks,nofail"))?;
    append_line(
        "/etc/fstab",
        &format!("/dev/mapper/{mapper} {mount_base} btrfs defaults,noatime,compress=zstd:3,nofail 0 0"),
    )?;
    append_line(
        "/etc/fstab",
        &format!("UUID={root_uuid} /mnt/btrfs btrfs subvolid=5,noatime,compress=zstd:3,nofail 0 0"),
    )?;

    // 5. keep udisks2 off the LUKS device, and auto-unlock/mount it on hotplug
    std::fs::write(
        "/etc/udev/rules.d/90-time-portal.rules",
        format!(
            "ACTION==\"add|change\", SUBSYSTEM==\"block\", ENV{{ID_FS_UUID}}==\"{luks_uuid}\", ENV{{UDISKS_IGNORE}}=\"1\", TAG+=\"systemd\", ENV{{SYSTEMD_WANTS}}=\"time-portal-mount.service\"\n"
        ),
    )?;
    std::fs::write(
        "/etc/systemd/system/time-portal-mount.service",
        "[Unit]\nDescription=Unlock and mount the Time Portal backup drive\nAfter=local-fs.target\n\n[Service]\nType=oneshot\nExecStart=/usr/lib/time-portal/time-portal-helper mount-drive\n",
    )?;
    let _ = Command::new("udevadm").args(["control", "--reload-rules"]).status();
    let _ = Command::new("udevadm").args(["trigger", "--subsystem-match=block"]).status();
    let _ = Command::new("systemctl").arg("daemon-reload").status();

    // 6. open + mount both sides
    run_str("cryptsetup", &["open".into(), "--key-file".into(), keyfile.into(), device.into(), mapper.into()])?;
    std::fs::create_dir_all(mount_base)?;
    run_str("mount", &[mapper_path.clone(), mount_base.into()])?;
    std::fs::create_dir_all("/mnt/btrfs")?;
    run_str("mount", &["/mnt/btrfs".into()])?;

    // 7. layout + manifest, and switch on the schedule
    let host = host.or_else(hostname).unwrap_or_else(|| "unknown".to_string());
    let target = format!("{mount_base}/{host}");
    std::fs::create_dir_all(&target)?;
    std::fs::create_dir_all("/mnt/btrfs/btrbk_snapshots")?;
    let mut m = timeportal_core::Manifest::new(&host);
    m.created = Command::new("date")
        .arg("-Iseconds")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    m.save(Path::new(&target))?;
    let _ = Command::new("systemctl").args(["enable", "--now", "btrbk.timer"]).status();

    println!(
        "{}",
        serde_json::json!({
            "ok": true, "device": device, "luks_uuid": luks_uuid,
            "mapper": mapper, "target": target
        })
    );
    Ok(())
}

// --- live backup progress ---------------------------------------------------

const PROGRESS_PATH: &str = "/run/time-portal/progress.json";

fn epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Processes whose executable name is exactly `name` (e.g. the real `btrfs`
/// binary, not the `sh -c '... | btrfs ...'` wrapper). Returns (pid, argv-tail).
fn pgrep_exact(name: &str) -> Vec<(u32, String)> {
    Command::new("pgrep")
        .args(["-x", "-a", name])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| {
                    let mut it = l.trim().splitn(2, ' ');
                    let pid = it.next()?.parse::<u32>().ok()?;
                    let args = it.next().unwrap_or("").to_string();
                    Some((pid, args))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn detect_phase() -> String {
    if Command::new("pgrep")
        .args(["-af", "rsync .*/boot/"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return "Copying /boot".to_string();
    }
    if let Some((p, _)) = send_info() {
        if p.contains("@home.") {
            return "Sending home".to_string();
        }
        if p.contains("/@.") {
            return "Sending system".to_string();
        }
        return "Sending".to_string();
    }
    if Command::new("pgrep")
        .args(["-f", "btrbk run"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return "Preparing".to_string();
    }
    String::new()
}

/// The snapshot being sent and whether it is a full send (no `-p` parent).
fn send_info() -> Option<(String, bool)> {
    for (_pid, args) in pgrep_exact("btrfs") {
        if let Some(rest) = args.strip_prefix("btrfs send ") {
            let toks: Vec<&str> = rest.split_whitespace().collect();
            let full = !toks.contains(&"-p");
            if let Some(p) = toks
                .iter()
                .rev()
                .map(|t| t.trim_matches('\''))
                .find(|t| t.starts_with('/'))
            {
                return Some((p.to_string(), full));
            }
        }
    }
    None
}

fn recv_pid() -> Option<u32> {
    for (pid, args) in pgrep_exact("btrfs") {
        if args.starts_with("btrfs receive") {
            return Some(pid);
        }
    }
    None
}

/// Overall progress across every subvolume in this run's snapshot set, so the
/// UI can show one bar: the sizes of the sibling snapshots (`@` and `@home`,
/// same timestamp) sum to the whole backup, and any already sent are counted
/// as done. Returns (grand_total, completed_before_current).
fn run_totals(
    snap: &str,
    totals: &mut HashMap<String, u64>,
) -> (u64, u64) {
    let path = Path::new(snap);
    let Some(base) = path.file_name().map(|s| s.to_string_lossy().to_string()) else {
        return (0, 0);
    };
    let Some((cur, ts)) = base.split_once('.') else {
        return (0, 0);
    };
    let Some(dir) = path.parent() else {
        return (0, 0);
    };
    let order = ["@", "@home"];
    let cur_i = order.iter().position(|n| *n == cur).unwrap_or(0);
    let mut grand = 0u64;
    let mut completed = 0u64;
    for (i, name) in order.iter().enumerate() {
        let p = dir.join(format!("{name}.{ts}"));
        if !p.exists() {
            continue;
        }
        let ps = p.to_string_lossy().to_string();
        let sz = *totals
            .entry(ps.clone())
            .or_insert_with(|| btrfs_du_total(&ps).unwrap_or(0));
        grand += sz;
        if i < cur_i {
            completed += sz;
        }
    }
    (grand, completed)
}

fn proc_write_bytes(pid: u32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("write_bytes:") {
            return v.trim().parse::<u64>().ok();
        }
    }
    None
}

fn btrfs_du_total(path: &str) -> Option<u64> {
    let out = Command::new("btrfs")
        .args(["filesystem", "du", "-s", "--raw", "--", path])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let last = text.lines().filter(|l| !l.trim().is_empty()).last()?;
    last.split_whitespace().next()?.parse::<u64>().ok()
}

fn write_progress(json: &str) {
    use std::os::unix::fs::PermissionsExt;
    let tmp = format!("{PROGRESS_PATH}.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::rename(&tmp, PROGRESS_PATH);
    }
}

fn monitor(totals: &mut HashMap<String, u64>) {
    let phase = detect_phase();
    let info = send_info();
    let snap = info.as_ref().map(|(p, _)| p.clone());
    let full = info.as_ref().map(|(_, f)| *f).unwrap_or(false);
    let mut done = None;
    let mut total = None;
    if let Some(p) = &snap {
        if let Some(pid) = recv_pid() {
            done = proc_write_bytes(pid);
        }
        if full {
            // One bar for the whole run: current bytes plus the siblings
            // already sent, over the summed size of every sibling snapshot.
            let (grand, completed) = run_totals(p, totals);
            if grand > 0 {
                total = Some(grand);
                done = Some(completed + done.unwrap_or(0));
            }
        }
    }
    let percent = match (done, total) {
        (Some(d), Some(t)) if t > 0 => Some((d as f64 / t as f64).min(1.0)),
        _ => None,
    };
    let json = serde_json::json!({
        "phase": phase,
        "snapshot": snap,
        "full": full,
        "done_bytes": done,
        "total_bytes": total,
        "percent": percent,
        "updated_epoch": epoch(),
    });
    write_progress(&json.to_string());
}

/// Runs btrbk and publishes progress. Runs as root (the backup service), so it
/// can read the receive process's byte counts and the snapshot sizes.
fn run_backup() -> Result<()> {
    std::fs::create_dir_all("/run/time-portal").ok();
    let mut totals: HashMap<String, u64> = HashMap::new();
    write_progress(&serde_json::json!({"phase": "Starting", "updated_epoch": epoch()}).to_string());

    let mut child = Command::new("btrbk").arg("run").spawn().context("spawning btrbk")?;
    loop {
        match child.try_wait().context("waiting for btrbk")? {
            Some(status) => {
                let _ = std::fs::remove_file(PROGRESS_PATH);
                std::process::exit(status.code().unwrap_or(1));
            }
            None => {
                monitor(&mut totals);
                std::thread::sleep(Duration::from_millis(1000));
            }
        }
    }
}

fn read_progress() -> Option<serde_json::Value> {
    let s = std::fs::read_to_string(PROGRESS_PATH).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    let u = v["updated_epoch"].as_u64().unwrap_or(0);
    if epoch().saturating_sub(u) > 20 {
        return None;
    }
    Some(v)
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program).args(args).status().with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} failed ({status})");
    }
    Ok(())
}

fn run_str(program: &str, args: &[String]) -> Result<()> {
    let status = Command::new(program).args(args).status().with_context(|| format!("running {program}"))?;
    if !status.success() {
        bail!("{program} failed ({status})");
    }
    Ok(())
}
