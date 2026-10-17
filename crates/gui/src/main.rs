//! Time Portal — the application.
//!
//! One GTK4/libadwaita window that does both jobs: shows backup state and
//! offers "Back up now", and restores from an encrypted backup drive (pick
//! drive → passphrase → version → restore). Privileged work is delegated to
//! `time-portal-helper` via polkit (`pkexec`); when already root (e.g. a live
//! USB) it runs the helper directly.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use adw::prelude::*;
use gtk4 as gtk;
use gtk::glib;
use gtk::glib::ControlFlow;
use gtk::prelude::*;
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{Icon, MenuItem, ToolTip, Tray};
use libadwaita as adw;

use timeportal_core::devices::BlockDevice;
use timeportal_core::Version;

const APP_ID: &str = "org.timeportal.TimePortal";
const MOUNT_BASE: &str = "/mnt/backup";
const MAPPER_PATH: &str = "/dev/mapper/backup";
const DEFAULT_TARGET: &str = "/mnt/backup/portal";
const FALLBACK_HELPER: &str = "/usr/lib/time-portal/time-portal-helper";

struct State {
    helper: String,
    target_root: String,
    versions: Vec<Version>,
    selected_device: Option<String>,
    selected_ts: Option<String>,
    source_user: String,
    setup_device: Option<String>,
}

#[derive(serde::Deserialize)]
struct Disk {
    path: String,
    name: String,
    size: Option<String>,
    #[serde(rename = "type")]
    dtype: String,
    fstype: Option<String>,
    label: Option<String>,
    mounted: bool,
    protected: bool,
}

/// Commands the tray sends to the GTK main thread.
#[derive(Debug)]
enum Cmd {
    Open,
    Start,
    Stop,
    Quit,
    SetKeep(bool),
}

struct TmTray {
    tx: mpsc::Sender<Cmd>,
    keep: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
    status: Arc<Mutex<String>>,
}

impl Tray for TmTray {
    fn id(&self) -> String {
        "time-portal".to_string()
    }
    fn icon_name(&self) -> String {
        "time-portal".to_string()
    }
    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![
            Icon {
                width: 64,
                height: 64,
                data: ICON64.clone(),
            },
            Icon {
                width: 128,
                height: 128,
                data: ICON128.clone(),
            },
        ]
    }
    fn title(&self) -> String {
        let s = self.status.lock().map(|g| g.clone()).unwrap_or_default();
        if s.is_empty() {
            "Time Portal".to_string()
        } else {
            format!("Time Portal — {s}")
        }
    }
    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: "time-portal".to_string(),
            icon_pixmap: Vec::new(),
            title: "Time Portal".to_string(),
            description: self.status.lock().map(|g| g.clone()).unwrap_or_default(),
        }
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let running = self.running.load(Ordering::Relaxed);
        vec![
            StandardItem {
                label: "Open Time Portal".to_string(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Open);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Back Up Now".to_string(),
                visible: !running,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Start);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Stop Backup".to_string(),
                visible: running,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Stop);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Keep in tray".to_string(),
                checked: self.keep.load(Ordering::Relaxed),
                activate: Box::new(|t: &mut Self| {
                    let v = !t.keep.load(Ordering::Relaxed);
                    t.keep.store(v, Ordering::Relaxed);
                    let _ = t.tx.send(Cmd::SetKeep(v));
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".to_string(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.send(Cmd::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.send(Cmd::Open);
    }
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    // Build once. A later launch only activates this instance; without the
    // guard, activate() would build a second window and a second tray item.
    let built: Rc<RefCell<Option<adw::ApplicationWindow>>> = Rc::new(RefCell::new(None));
    app.connect_activate(move |app| {
        if let Some(w) = built.borrow().as_ref() {
            w.present();
            return;
        }
        let w = build_ui(app);
        *built.borrow_mut() = Some(w);
    });
    app.run()
}

// --- plumbing ---------------------------------------------------------------

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn helper_path() -> String {
    if let Ok(p) = std::env::var("TIME_PORTAL_HELPER") {
        if !p.is_empty() {
            return p;
        }
    }
    if Path::new(FALLBACK_HELPER).exists() {
        return FALLBACK_HELPER.to_string();
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sib = dir.join("time-portal-helper");
            if sib.exists() {
                return sib.to_string_lossy().into_owned();
            }
        }
    }
    "/usr/bin/time-portal-helper".to_string()
}

fn current_user() -> String {
    std::env::var("USER")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("id")
                .arg("-un")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|| "user".to_string())
}

fn exec(helper: &str, args: &[String], elevate: bool) -> Result<String, String> {
    let mut cmd = if elevate && !is_root() {
        let mut c = Command::new("pkexec");
        c.arg(helper);
        c
    } else {
        Command::new(helper)
    };
    cmd.args(args);
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).to_string()),
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            Err(if e.is_empty() { "command failed".to_string() } else { e })
        }
        Err(e) => Err(format!("could not run helper: {e}")),
    }
}

fn write_secret(content: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let p = std::env::temp_dir().join(format!("tm-pass-{}", std::process::id()));
    std::fs::write(&p, content)?;
    let mut perm = std::fs::metadata(&p)?.permissions();
    perm.set_mode(0o600);
    std::fs::set_permissions(&p, perm)?;
    Ok(p)
}

fn toast(overlay: &adw::ToastOverlay, msg: &str) {
    overlay.add_toast(adw::Toast::new(msg));
}

fn ts_when(ts: &str) -> String {
    if let Some(e) = timeportal_core::parse_ts(ts) {
        if let Ok(o) = Command::new("date")
            .args(["-d", &format!("@{e}"), "+%a %d %b %H:%M"])
            .output()
        {
            return String::from_utf8_lossy(&o.stdout).trim().to_string();
        }
    }
    ts.to_string()
}

/// A drive that actually holds our backups: the LUKS label we set, or a btrfs
/// child labelled BACKUP. This keeps the internal system disk out of the
/// restore list.
fn looks_like_backup(d: &BlockDevice) -> bool {
    if d.label.as_deref() == Some("backup-crypt") {
        return true;
    }
    fn has_backup_btrfs(x: &BlockDevice) -> bool {
        (x.fstype.as_deref() == Some("btrfs") && x.label.as_deref() == Some("BACKUP"))
            || x.children.iter().any(has_backup_btrfs)
    }
    has_backup_btrfs(d)
}

fn elapsed_human(s: i64) -> String {
    if s <= 0 {
        return String::new();
    }
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m} min");
    }
    format!("{} h {} min", m / 60, m % 60)
}

fn human_bytes(n: u64) -> String {
    let mut v = n as f64;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut i = 0;
    while v >= 1024.0 && i < units.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if v >= 100.0 {
        format!("{:.0} {}", v, units[i])
    } else {
        format!("{:.1} {}", v, units[i])
    }
}

// The tray icon: the same fa-history glyph the Time Machine widget used,
// embedded as ARGB32 (network byte order) so it renders whatever the host is.
fn rgba_to_argb(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        out.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
    }
    out
}

static ICON64: LazyLock<Vec<u8>> =
    LazyLock::new(|| rgba_to_argb(include_bytes!("../../../data/icons/time-portal-64.rgba")));
static ICON128: LazyLock<Vec<u8>> =
    LazyLock::new(|| rgba_to_argb(include_bytes!("../../../data/icons/time-portal-128.rgba")));

fn probe_devices(helper: &str) -> Vec<BlockDevice> {
    exec(helper, &["probe".to_string()], false)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<BlockDevice>>(&s).ok())
        .unwrap_or_default()
}

fn probe_disks(helper: &str) -> Vec<Disk> {
    exec(helper, &["disks".to_string()], false)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Disk>>(&s).ok())
        .unwrap_or_default()
}

/// Rebuild the "Restore from a backup drive" rows.
fn fill_restore_group(
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<gtk::Widget>>>,
    luks: &[BlockDevice],
    state: &Rc<RefCell<State>>,
    stack: &gtk::Stack,
) {
    for r in rows.borrow_mut().drain(..) {
        group.remove(&r);
    }
    if luks.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No backup drives found")
            .subtitle("Plug in your Time Portal drive")
            .build();
        group.add(&row);
        rows.borrow_mut().push(row.upcast());
        return;
    }
    for d in luks {
        let title = d.label.clone().unwrap_or_else(|| d.name.clone());
        let sub = format!("{} · {}", d.path, d.size.clone().unwrap_or_default());
        let row = adw::ActionRow::builder()
            .title(&title)
            .subtitle(&sub)
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("drive-harddisk-symbolic"));
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let state = state.clone();
        let stack = stack.clone();
        let device = d.path.clone();
        row.connect_activated(move |_| {
            state.borrow_mut().selected_device = Some(device.clone());
            stack.set_visible_child_name("passphrase");
        });
        group.add(&row);
        rows.borrow_mut().push(row.upcast());
    }
}

/// Rebuild the "Set up a new backup drive" rows.
fn fill_setup_group(
    group: &adw::PreferencesGroup,
    rows: &Rc<RefCell<Vec<gtk::Widget>>>,
    disks: &[Disk],
    state: &Rc<RefCell<State>>,
    stack: &gtk::Stack,
    label: &gtk::Label,
) {
    for r in rows.borrow_mut().drain(..) {
        group.remove(&r);
    }
    if disks.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No spare drives found")
            .subtitle("Plug in a drive (and unmount it) to set one up")
            .build();
        group.add(&row);
        rows.borrow_mut().push(row.upcast());
        return;
    }
    for d in disks {
        let title = format!("{} ({})", d.name, d.size.clone().unwrap_or_default());
        let row = adw::ActionRow::builder()
            .title(&title)
            .subtitle(&d.path)
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("drive-harddisk-symbolic"));
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let state = state.clone();
        let stack = stack.clone();
        let label = label.clone();
        let path = d.path.clone();
        row.connect_activated(move |_| {
            state.borrow_mut().setup_device = Some(path.clone());
            label.set_text(&format!("Drive: {path}"));
            stack.set_visible_child_name("format");
        });
        group.add(&row);
        rows.borrow_mut().push(row.upcast());
    }
}

/// Find the directory under the mount base that holds our backups.
fn find_target_root() -> String {
    let mut best = None;
    if let Ok(rd) = std::fs::read_dir(MOUNT_BASE) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            if p.join(timeportal_core::manifest::MANIFEST_NAME).exists()
                || p.join(".timemachine.json").exists()
            {
                return p.to_string_lossy().into_owned();
            }
            if let Ok(inner) = std::fs::read_dir(&p) {
                if inner
                    .flatten()
                    .any(|x| x.file_name().to_string_lossy().starts_with('@'))
                {
                    best = Some(p.to_string_lossy().into_owned());
                }
            }
        }
    }
    best.unwrap_or_else(|| DEFAULT_TARGET.to_string())
}

fn scroll(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

/// Run a closure off the main thread; spin `spinner` until it returns, then
/// call `on_done` back on the main thread (so it may touch widgets).
fn run_async<F, G>(spinner: gtk::Spinner, status: gtk::Label, work: F, on_done: G)
where
    F: FnOnce() -> Result<String, String> + Send + 'static,
    G: FnOnce(Result<String, String>) + 'static,
{
    spinner.set_visible(true);
    spinner.set_spinning(true);
    status.set_text("Working…");
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    let rx = Rc::new(RefCell::new(rx));
    let on_done = Rc::new(RefCell::new(Some(on_done)));
    glib::timeout_add_local(Duration::from_millis(150), move || {
        match rx.borrow().try_recv() {
            Ok(res) => {
                spinner.set_spinning(false);
                spinner.set_visible(false);
                match &res {
                    Ok(msg) => status.set_text(msg),
                    Err(err) => status.set_markup(&format!(
                        "<span foreground='red'>{}</span>",
                        glib::markup_escape_text(err)
                    )),
                }
                if let Some(cb) = on_done.borrow_mut().take() {
                    cb(res);
                }
                ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                spinner.set_spinning(false);
                spinner.set_visible(false);
                ControlFlow::Break
            }
        }
    });
}

// --- the wizard -------------------------------------------------------------

fn build_ui(app: &adw::Application) -> adw::ApplicationWindow {
    let state = Rc::new(RefCell::new(State {
        helper: helper_path(),
        target_root: DEFAULT_TARGET.to_string(),
        versions: Vec::new(),
        selected_device: None,
        selected_ts: None,
        source_user: current_user(),
        setup_device: None,
    }));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Time Portal")
        .default_width(820)
        .default_height(640)
        .build();
    let header = adw::HeaderBar::new();
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::SlideLeft);

    // The window's *direct* content must be the ToolbarView so libadwaita
    // treats the HeaderBar as the title bar and draws the window controls
    // (close/minimise). The ToastOverlay goes inside, over the content.
    let overlay = adw::ToastOverlay::new();
    overlay.set_child(Some(&stack));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&overlay));
    window.set_content(Some(&toolbar));

    // Always-quit affordances, independent of window decorations.
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let app_for_quit = app.clone();
    quit.connect_activate(move |_, _| app_for_quit.quit());
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Control>q"]);

    window.present();

    let helper = state.borrow().helper.clone();

    // --- system tray (StatusNotifierItem) ----------------------------------
    // Launching the app brings up the tray; closing the window hides to it.
    let (tray_tx, tray_rx) = mpsc::channel::<Cmd>();
    let keep_in_tray = Arc::new(AtomicBool::new(true));
    let tray_running = Arc::new(AtomicBool::new(false));
    let tray_status = Arc::new(Mutex::new(String::new()));
    let bar_determinate = Arc::new(AtomicBool::new(false));
    let tray_handle = TmTray {
        tx: tray_tx,
        keep: keep_in_tray.clone(),
        running: tray_running.clone(),
        status: tray_status.clone(),
    }
    .spawn()
    .ok();

    {
        let rx = Rc::new(RefCell::new(tray_rx));
        let window_c = window.clone();
        let app_c = app.clone();
        let helper_c = helper.clone();
        glib::timeout_add_local(Duration::from_millis(250), move || {
            while let Ok(cmd) = rx.borrow().try_recv() {
                match cmd {
                    Cmd::Open => window_c.present(),
                    Cmd::Start => {
                        let h = helper_c.clone();
                        std::thread::spawn(move || {
                            let _ = exec(&h, &["start-backup".to_string()], true);
                        });
                    }
                    Cmd::Stop => {
                        let h = helper_c.clone();
                        std::thread::spawn(move || {
                            let _ = exec(&h, &["stop-backup".to_string()], true);
                        });
                    }
                    Cmd::Quit => {
                        app_c.quit();
                        return ControlFlow::Break;
                    }
                    Cmd::SetKeep(_) => {}
                }
            }
            ControlFlow::Continue
        });
    }

    {
        let keep = keep_in_tray.clone();
        window.connect_close_request(move |w| {
            if keep.load(Ordering::Relaxed) {
                w.set_visible(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }

    // --- page: welcome ------------------------------------------------------
    let welcome = gtk::Box::new(gtk::Orientation::Vertical, 18);
    welcome.set_margin_top(18);
    welcome.set_margin_bottom(18);
    welcome.set_margin_start(18);
    welcome.set_margin_end(18);

    let status_group = adw::PreferencesGroup::new();
    status_group.set_title("Backup");
    let status_row = adw::ActionRow::builder().title("Time Portal").build();
    status_group.add(&status_row);
    let mut timer_enabled = false;
    if let Ok(s) = exec(
        &helper,
        &[
            "status".to_string(),
            "--target".to_string(),
            state.borrow().target_root.clone(),
        ],
        false,
    ) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            let mounted = v["mounted"].as_bool().unwrap_or(false);
            let count = v["version_count"].as_u64().unwrap_or(0);
            let running = v["running"].as_bool().unwrap_or(false);
            timer_enabled = v["timer_enabled"].as_bool().unwrap_or(false);
            let sub = if running {
                "Backing up now…".to_string()
            } else if mounted && count > 0 {
                format!("Drive connected · {count} version(s)")
            } else if mounted {
                "Drive connected · no backups yet".to_string()
            } else {
                "No backup drive connected".to_string()
            };
            status_row.set_subtitle(&sub);
        }
    }
    welcome.append(&status_group);

    // Live progress, shown only while a backup is running.
    let progress_bar = gtk::ProgressBar::new();
    progress_bar.set_pulse_step(0.08);
    let progress_label = gtk::Label::new(None);
    progress_label.set_halign(gtk::Align::Start);
    progress_label.add_css_class("dim-label");
    let progress_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
    progress_box.append(&progress_bar);
    progress_box.append(&progress_label);
    progress_box.set_visible(false);
    welcome.append(&progress_box);

    let sched_group = adw::PreferencesGroup::new();
    sched_group.set_title("Schedule");
    let sched_row = adw::ActionRow::builder()
        .title("Hourly backups")
        .subtitle("Snapshot every hour and send the delta to the drive")
        .build();
    let sched_switch = gtk::Switch::new();
    sched_switch.set_active(timer_enabled);
    sched_switch.set_valign(gtk::Align::Center);
    sched_row.add_suffix(&sched_switch);
    {
        let helper = helper.clone();
        let overlay = overlay.clone();
        sched_switch.connect_state_set(move |_, active| {
            let cmd = if active { "enable-backup" } else { "disable-backup" };
            match exec(&helper, &[cmd.to_string()], true) {
                Ok(_) => glib::Propagation::Proceed,
                Err(e) => {
                    toast(&overlay, &format!("Could not change the schedule: {e}"));
                    glib::Propagation::Stop
                }
            }
        });
    }
    sched_group.add(&sched_row);
    welcome.append(&sched_group);

    let app_group = adw::PreferencesGroup::new();
    app_group.set_title("Application");
    let keep_row = adw::ActionRow::builder()
        .title("Keep running in the tray")
        .subtitle("Closing the window keeps Time Portal in the tray")
        .build();
    let keep_switch = gtk::Switch::new();
    keep_switch.set_active(true);
    keep_switch.set_valign(gtk::Align::Center);
    keep_row.add_suffix(&keep_switch);
    {
        let keep = keep_in_tray.clone();
        let handle = tray_handle.clone();
        keep_switch.connect_state_set(move |_, active| {
            keep.store(active, Ordering::Relaxed);
            if let Some(h) = &handle {
                h.update(|t: &mut TmTray| t.keep.store(active, Ordering::Relaxed));
            }
            glib::Propagation::Proceed
        });
    }
    app_group.add(&keep_row);
    welcome.append(&app_group);

    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let backup_btn = gtk::Button::with_label("Back Up Now");
    backup_btn.add_css_class("suggested-action");
    {
        let helper = helper.clone();
        let overlay = overlay.clone();
        backup_btn.connect_clicked(move |_| {
            match exec(&helper, &["start-backup".to_string()], true) {
                Ok(_) => toast(&overlay, "Backup started"),
                Err(e) => toast(&overlay, &format!("Could not start backup: {e}")),
            }
        });
    }
    let stop_btn = gtk::Button::with_label("Stop Backup");
    {
        let helper = helper.clone();
        let overlay = overlay.clone();
        stop_btn.connect_clicked(move |_| {
            match exec(&helper, &["stop-backup".to_string()], true) {
                Ok(_) => toast(&overlay, "Backup stopped"),
                Err(e) => toast(&overlay, &format!("Could not stop backup: {e}")),
            }
        });
    }
    btn_row.append(&backup_btn);
    btn_row.append(&stop_btn);
    stop_btn.set_visible(false);
    welcome.append(&btn_row);

    let restore_group = adw::PreferencesGroup::new();
    restore_group.set_title("Restore from a backup drive");
    restore_group.set_description(Some("Choose the encrypted drive, then unlock it."));
    let restore_rows: Rc<RefCell<Vec<gtk::Widget>>> = Rc::new(RefCell::new(Vec::new()));
    let initial_luks: Vec<BlockDevice> = probe_devices(&helper)
        .into_iter()
        .filter(looks_like_backup)
        .collect();
    fill_restore_group(&restore_group, &restore_rows, &initial_luks, &state, &stack);
    welcome.append(&restore_group);

    // Filled once the format page exists (it needs that page's label).
    let setup_group = adw::PreferencesGroup::new();
    setup_group.set_title("Set up a new backup drive");
    setup_group.set_description(Some("Erase a spare disk and format it for Time Portal."));
    let setup_rows: Rc<RefCell<Vec<gtk::Widget>>> = Rc::new(RefCell::new(Vec::new()));
    welcome.append(&setup_group);

    stack.add_named(&scroll(&welcome), Some("welcome"));

    // --- page: passphrase ---------------------------------------------------
    let pass_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    pass_page.set_margin_top(18);
    pass_page.set_margin_bottom(18);
    pass_page.set_margin_start(18);
    pass_page.set_margin_end(18);
    let pass_title = gtk::Label::new(Some("Unlock the backup drive"));
    pass_title.add_css_class("title-2");
    pass_title.set_halign(gtk::Align::Start);
    pass_page.append(&pass_title);
    let pass_hint = gtk::Label::new(Some("Enter the recovery passphrase you set when the drive was created."));
    pass_hint.add_css_class("dim-label");
    pass_hint.set_wrap(true);
    pass_hint.set_halign(gtk::Align::Start);
    pass_page.append(&pass_hint);
    let pass_entry = gtk::PasswordEntry::new();
    pass_entry.set_show_peek_icon(true);
    pass_page.append(&pass_entry);
    let unlock_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let unlock_btn = gtk::Button::with_label("Unlock");
    unlock_btn.add_css_class("suggested-action");
    let pass_spinner = gtk::Spinner::new();
    unlock_row.append(&unlock_btn);
    unlock_row.append(&pass_spinner);
    pass_page.append(&unlock_row);
    let pass_back = gtk::Button::with_label("Back");
    pass_back.set_halign(gtk::Align::Start);
    {
        let stack = stack.clone();
        pass_back.connect_clicked(move |_| stack.set_visible_child_name("welcome"));
    }
    pass_page.append(&pass_back);
    stack.add_named(&scroll(&pass_page), Some("passphrase"));

    // --- page: versions -----------------------------------------------------
    let versions_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    versions_page.set_margin_top(18);
    versions_page.set_margin_bottom(18);
    versions_page.set_margin_start(18);
    versions_page.set_margin_end(18);
    let versions_title = gtk::Label::new(Some("Choose a version"));
    versions_title.add_css_class("title-2");
    versions_title.set_halign(gtk::Align::Start);
    versions_page.append(&versions_title);
    let versions_list = gtk::ListBox::new();
    versions_list.add_css_class("boxed-list");
    versions_page.append(&versions_list);
    let versions_back = gtk::Button::with_label("Back");
    versions_back.set_halign(gtk::Align::Start);
    {
        let stack = stack.clone();
        versions_back.connect_clicked(move |_| stack.set_visible_child_name("welcome"));
    }
    versions_page.append(&versions_back);
    stack.add_named(&scroll(&versions_page), Some("versions"));

    // --- page: restore ------------------------------------------------------
    let restore_page = gtk::Box::new(gtk::Orientation::Vertical, 18);
    restore_page.set_margin_top(18);
    restore_page.set_margin_bottom(18);
    restore_page.set_margin_start(18);
    restore_page.set_margin_end(18);
    let restore_title = gtk::Label::new(Some("Restore"));
    restore_title.add_css_class("title-2");
    restore_title.set_halign(gtk::Align::Start);
    restore_page.append(&restore_title);
    let restore_version = gtk::Label::new(None);
    restore_version.add_css_class("dim-label");
    restore_version.set_halign(gtk::Align::Start);
    restore_page.append(&restore_version);

    let user_group = adw::PreferencesGroup::new();
    user_group.set_title("My files");
    let user_row = adw::EntryRow::builder()
        .title("User in the backup")
        .text(&state.borrow().source_user)
        .build();
    user_group.add(&user_row);
    let home_row = adw::EntryRow::builder()
        .title("Restore into")
        .text(&std::env::var("HOME").unwrap_or_else(|_| "/root".to_string()))
        .build();
    user_group.add(&home_row);
    restore_page.append(&user_group);
    let home_btn = gtk::Button::with_label("Restore My Files");
    home_btn.add_css_class("suggested-action");
    home_btn.set_halign(gtk::Align::Start);
    restore_page.append(&home_btn);

    let sys_group = adw::PreferencesGroup::new();
    sys_group.set_title("The whole system");
    sys_group.set_description(Some("Advanced. Use from a live USB; this overwrites system files."));
    let sys_row = adw::EntryRow::builder()
        .title("Restore into (mounted root)")
        .text("/")
        .build();
    sys_group.add(&sys_row);
    restore_page.append(&sys_group);
    let sys_btn = gtk::Button::with_label("Restore System…");
    sys_btn.add_css_class("destructive-action");
    sys_btn.set_halign(gtk::Align::Start);
    restore_page.append(&sys_btn);

    let prog_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let restore_spinner = gtk::Spinner::new();
    restore_spinner.set_visible(false);
    let restore_status = gtk::Label::new(None);
    restore_status.set_halign(gtk::Align::Start);
    prog_row.append(&restore_spinner);
    prog_row.append(&restore_status);
    restore_page.append(&prog_row);

    let restore_back = gtk::Button::with_label("Back");
    restore_back.set_halign(gtk::Align::Start);
    {
        let stack = stack.clone();
        restore_back.connect_clicked(move |_| stack.set_visible_child_name("versions"));
    }
    restore_page.append(&restore_back);
    stack.add_named(&scroll(&restore_page), Some("restore"));

    // --- page: format a new drive ------------------------------------------
    let format_page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    format_page.set_margin_top(18);
    format_page.set_margin_bottom(18);
    format_page.set_margin_start(18);
    format_page.set_margin_end(18);
    let format_title = gtk::Label::new(Some("Set up a backup drive"));
    format_title.add_css_class("title-2");
    format_title.set_halign(gtk::Align::Start);
    format_page.append(&format_title);
    let format_device_label = gtk::Label::new(None);
    format_device_label.add_css_class("dim-label");
    format_device_label.set_halign(gtk::Align::Start);
    format_page.append(&format_device_label);
    let format_warn = gtk::Label::new(Some(
        "Everything on this drive will be permanently erased and replaced with an encrypted btrfs filesystem.",
    ));
    format_warn.set_wrap(true);
    format_warn.add_css_class("error");
    format_warn.set_halign(gtk::Align::Start);
    format_page.append(&format_warn);
    let fmt_pass = gtk::PasswordEntry::new();
    fmt_pass.set_show_peek_icon(true);
    fmt_pass.set_placeholder_text(Some("New recovery passphrase"));
    format_page.append(&fmt_pass);
    let fmt_confirm = gtk::PasswordEntry::new();
    fmt_confirm.set_show_peek_icon(true);
    fmt_confirm.set_placeholder_text(Some("Repeat passphrase"));
    format_page.append(&fmt_confirm);
    let fmt_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let fmt_btn = gtk::Button::with_label("Erase and Set Up");
    fmt_btn.add_css_class("destructive-action");
    let fmt_spinner = gtk::Spinner::new();
    fmt_spinner.set_visible(false);
    fmt_row.append(&fmt_btn);
    fmt_row.append(&fmt_spinner);
    format_page.append(&fmt_row);
    let fmt_status = gtk::Label::new(None);
    fmt_status.set_halign(gtk::Align::Start);
    fmt_status.set_wrap(true);
    format_page.append(&fmt_status);
    let fmt_back = gtk::Button::with_label("Back");
    fmt_back.set_halign(gtk::Align::Start);
    {
        let stack = stack.clone();
        fmt_back.connect_clicked(move |_| stack.set_visible_child_name("welcome"));
    }
    format_page.append(&fmt_back);
    stack.add_named(&scroll(&format_page), Some("format"));

    // --- wiring: unpack the drive ------------------------------------------
    {
        let state = state.clone();
        let stack = stack.clone();
        let overlay = overlay.clone();
        let helper = helper.clone();
        let versions_list = versions_list.clone();
        let restore_version = restore_version.clone();
        let pass_spinner = pass_spinner.clone();
        unlock_btn.connect_clicked(move |_| {
            let device = state.borrow().selected_device.clone().unwrap_or_default();
            let pw = pass_entry.text().to_string();
            if device.is_empty() || pw.is_empty() {
                toast(&overlay, "Enter the passphrase");
                return;
            }
            let secret = match write_secret(&pw) {
                Ok(p) => p,
                Err(e) => {
                    toast(&overlay, &format!("Could not prepare passphrase: {e}"));
                    return;
                }
            };
            let helper2 = helper.clone();
            let state2 = state.clone();
            let stack2 = stack.clone();
            let overlay2 = overlay.clone();
            let list2 = versions_list.clone();
            let ver_label = restore_version.clone();
            let helper_work = helper.clone();
            let device_work = device.clone();
            let secret_work = secret.clone();
            let work = move || -> Result<String, String> {
                let r = exec(
                    &helper_work,
                    &[
                        "unlock".to_string(),
                        device_work,
                        "--passphrase-file".to_string(),
                        secret_work.to_string_lossy().into_owned(),
                    ],
                    true,
                );
                let _ = std::fs::remove_file(&secret_work);
                r?;
                exec(
                    &helper_work,
                    &[
                        "mount".to_string(),
                        MAPPER_PATH.to_string(),
                        MOUNT_BASE.to_string(),
                    ],
                    true,
                )?;
                Ok("Unlocked".to_string())
            };
            let on_done = move |res: Result<String, String>| match res {
                Ok(_) => {
                    let target = find_target_root();
                    state2.borrow_mut().target_root = target.clone();
                    populate_versions(&state2, &list2, &stack2, &overlay2, &ver_label, &helper2);
                    stack2.set_visible_child_name("versions");
                }
                Err(e) => toast(&overlay2, &e),
            };
            run_async(pass_spinner.clone(), gtk::Label::new(None), work, on_done);
        });
    }

    // --- wiring: restore my files ------------------------------------------
    {
        let state = state.clone();
        let overlay = overlay.clone();
        let spinner = restore_spinner.clone();
        let status = restore_status.clone();
        let user_row = user_row.clone();
        let home_row = home_row.clone();
        let helper = helper.clone();
        home_btn.connect_clicked(move |_| {
            let (ts, target) = selected(&state);
            if ts.is_empty() {
                toast(&overlay, "Pick a version first");
                return;
            }
            let user = user_row.text().to_string();
            let dest = home_row.text().to_string();
            let source = format!("{target}/@home.{ts}/{user}");
            let helper = helper.clone();
            let spinner = spinner.clone();
            let status = status.clone();
            let overlay2 = overlay.clone();
            let work = move || {
                exec(&helper, &["restore".to_string(), source, dest], true)
                    .map(|_| "Restore complete".to_string())
            };
            let on_done = move |res: Result<String, String>| {
                if let Err(e) = res {
                    toast(&overlay2, &e);
                }
            };
            run_async(spinner, status, work, on_done);
        });
    }

    // --- wiring: restore system (with confirmation) -------------------------
    {
        let state = state.clone();
        let overlay = overlay.clone();
        let spinner = restore_spinner.clone();
        let status = restore_status.clone();
        let sys_row = sys_row.clone();
        let helper = helper.clone();
        let window = window.clone();
        sys_btn.connect_clicked(move |_| {
            let (ts, target) = selected(&state);
            if ts.is_empty() {
                toast(&overlay, "Pick a version first");
                return;
            }
            let dest = sys_row.text().to_string();
            let source = format!("{target}/@.{ts}");
            let dialog = adw::MessageDialog::builder()
                .transient_for(&window)
                .heading("Restore the whole system?")
                .body("This writes the snapshot over the target root, including system files. Only do this from a live environment.")
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("ok", "Restore");
            dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
            let spinner = spinner.clone();
            let status = status.clone();
            let helper = helper.clone();
            let overlay_for_dialog = overlay.clone();
            dialog.connect_response(None, move |dlg: &adw::MessageDialog, resp: &str| {
                dlg.close();
                if resp != "ok" {
                    return;
                }
                let (source, dest) = (source.clone(), dest.clone());
                let (helper, spinner, status) = (helper.clone(), spinner.clone(), status.clone());
                let overlay2 = overlay_for_dialog.clone();
                let work = move || {
                    exec(
                        &helper,
                        &["restore".to_string(), source, dest, "--delete".to_string()],
                        true,
                    )
                    .map(|_| "System restore complete".to_string())
                };
                let on_done = move |res: Result<String, String>| {
                    if let Err(e) = res {
                        toast(&overlay2, &e);
                    }
                };
                run_async(spinner, status, work, on_done);
            });
            dialog.present();
        });
    }

    // --- wiring: set up a new drive ----------------------------------------
    {
        let candidates: Vec<Disk> = probe_disks(&helper)
            .into_iter()
            .filter(|d| !d.protected)
            .collect();
        fill_setup_group(
            &setup_group,
            &setup_rows,
            &candidates,
            &state,
            &stack,
            &format_device_label,
        );
    }

    {
        let state = state.clone();
        let helper = helper.clone();
        let overlay = overlay.clone();
        let window = window.clone();
        let spinner = fmt_spinner.clone();
        let status = fmt_status.clone();
        let pass = fmt_pass.clone();
        let confirm = fmt_confirm.clone();
        fmt_btn.connect_clicked(move |_| {
            let device = state.borrow().setup_device.clone().unwrap_or_default();
            let p = pass.text().to_string();
            let c = confirm.text().to_string();
            if device.is_empty() {
                toast(&overlay, "Choose a drive first");
                return;
            }
            if p.is_empty() || p != c {
                toast(&overlay, "The passphrases do not match");
                return;
            }
            let dialog = adw::MessageDialog::builder()
                .transient_for(&window)
                .heading("Erase this drive?")
                .body(format!(
                    "Everything on {device} will be destroyed and replaced with an encrypted backup filesystem."
                ))
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("ok", "Erase");
            dialog.set_response_appearance("ok", adw::ResponseAppearance::Destructive);
            let device2 = device.clone();
            let pass2 = p.clone();
            let helper2 = helper.clone();
            let overlay2 = overlay.clone();
            let spinner2 = spinner.clone();
            let status2 = status.clone();
            dialog.connect_response(None, move |dlg: &adw::MessageDialog, resp: &str| {
                dlg.close();
                if resp != "ok" {
                    return;
                }
                let secret = match write_secret(&pass2) {
                    Ok(s) => s,
                    Err(e) => {
                        toast(&overlay2, &format!("{e}"));
                        return;
                    }
                };
                let helper3 = helper2.clone();
                let device3 = device2.clone();
                let work = move || -> Result<String, String> {
                    let r = exec(
                        &helper3,
                        &[
                            "format-drive".to_string(),
                            device3.clone(),
                            "--yes".to_string(),
                            "--passphrase-file".to_string(),
                            secret.to_string_lossy().into_owned(),
                        ],
                        true,
                    );
                    let _ = std::fs::remove_file(&secret);
                    r.map(|_| format!("Set up {device3} — backups will start on schedule"))
                };
                let overlay3 = overlay2.clone();
                let on_done = move |res: Result<String, String>| match res {
                    Ok(m) => toast(&overlay3, &m),
                    Err(e) => toast(&overlay3, &e),
                };
                run_async(spinner2.clone(), status2.clone(), work, on_done);
            });
            dialog.present();
        });
    }

    // --- live backup status (poll) -----------------------------------------
    // Shows phase + elapsed in the app and the tray, and swaps the button for
    // Stop only while a backup is actually running.
    {
        let helper = helper.clone();
        let target = state.borrow().target_root.clone();
        let busy = Rc::new(Cell::new(false));
        let (tx, rx) = mpsc::channel::<String>();
        let rx = Rc::new(RefCell::new(rx));
        let status_row = status_row.clone();
        let progress_box = progress_box.clone();
        let progress_bar = progress_bar.clone();
        let progress_label = progress_label.clone();
        let backup_btn = backup_btn.clone();
        let stop_btn = stop_btn.clone();
        let handle = tray_handle.clone();
        let tray_running = tray_running.clone();
        let tray_status = tray_status.clone();
        let bar_determinate = bar_determinate.clone();
        let restore_group = restore_group.clone();
        let restore_rows = restore_rows.clone();
        let setup_group = setup_group.clone();
        let setup_rows = setup_rows.clone();
        let format_device_label = format_device_label.clone();
        let state = state.clone();
        let stack = stack.clone();
        let dev_sig: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

        glib::timeout_add_local(Duration::from_millis(1500), move || {
            if !busy.get() {
                busy.set(true);
                let h = helper.clone();
                let t = target.clone();
                let tx = tx.clone();
                std::thread::spawn(move || {
                    let st = exec(&h, &["status".to_string(), "--target".to_string(), t], false)
                        .unwrap_or_default();
                    let lu = exec(&h, &["probe".to_string()], false).unwrap_or_else(|_| "[]".to_string());
                    let dk = exec(&h, &["disks".to_string()], false).unwrap_or_else(|_| "[]".to_string());
                    let payload = format!(
                        "{{\"status\":{},\"luks\":{},\"disks\":{}}}",
                        if st.trim().is_empty() { "{}" } else { st.trim() },
                        if lu.trim().is_empty() { "[]" } else { lu.trim() },
                        if dk.trim().is_empty() { "[]" } else { dk.trim() },
                    );
                    let _ = tx.send(payload);
                });
            }
            if let Ok(s) = rx.borrow().try_recv() {
                busy.set(false);
                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&s) {
                    // Rebuild the drive lists only when the device set changes.
                    let luks: Vec<BlockDevice> = payload["luks"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| serde_json::from_value::<BlockDevice>(x.clone()).ok())
                                .filter(looks_like_backup)
                                .collect()
                        })
                        .unwrap_or_default();
                    let disks: Vec<Disk> = payload["disks"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| serde_json::from_value::<Disk>(x.clone()).ok())
                                .filter(|d| !d.protected)
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut sig = String::new();
                    for d in &luks {
                        sig.push_str(&d.path);
                        sig.push(':');
                        sig.push_str(d.label.as_deref().unwrap_or(""));
                        sig.push(';');
                    }
                    sig.push('|');
                    for d in &disks {
                        sig.push_str(&d.path);
                        sig.push(':');
                        sig.push_str(d.size.as_deref().unwrap_or(""));
                        sig.push(';');
                    }
                    if *dev_sig.borrow() != sig {
                        *dev_sig.borrow_mut() = sig;
                        fill_restore_group(&restore_group, &restore_rows, &luks, &state, &stack);
                        fill_setup_group(
                            &setup_group,
                            &setup_rows,
                            &disks,
                            &state,
                            &stack,
                            &format_device_label,
                        );
                    }

                    let v = &payload["status"];
                    let running = v["running"].as_bool().unwrap_or(false);
                    let mounted = v["mounted"].as_bool().unwrap_or(false);
                    let drive_present = v["drive_present"].as_bool().unwrap_or(false);
                    let count = v["version_count"].as_u64().unwrap_or(0);
                    let secs = v["run_seconds"].as_i64().unwrap_or(0);
                    let percent = v["percent"].as_f64();
                    let done = v["done_bytes"].as_u64();
                    let total = v["total_bytes"].as_u64();
                    let line = if running {
                        // One bar for the whole backup: never narrate which
                        // internal subvolume is in flight.
                        let mut parts = vec!["Backing up".to_string()];
                        if let (Some(pc), Some(t)) = (percent, total) {
                            parts.push(format!(
                                "{}% of {}",
                                (pc * 100.0).round() as i64,
                                human_bytes(t)
                            ));
                        }
                        if let Some(d) = done {
                            parts.push(format!("{} sent", human_bytes(d)));
                        }
                        let el = elapsed_human(secs);
                        if !el.is_empty() {
                            parts.push(el);
                        }
                        parts.join(" · ")
                    } else if mounted && count > 0 {
                        format!("Drive connected · {count} version(s)")
                    } else if mounted {
                        "Drive connected · no backups yet".to_string()
                    } else if drive_present {
                        "Backup drive detected · not mounted — Back Up Now will connect it"
                            .to_string()
                    } else {
                        "No backup drive connected".to_string()
                    };
                    status_row.set_subtitle(&line);
                    progress_box.set_visible(running);
                    progress_label.set_text(if running { line.as_str() } else { "" });
                    match percent {
                        Some(pc) if running => {
                            bar_determinate.store(true, Ordering::Relaxed);
                            progress_bar.set_fraction(pc.clamp(0.0, 1.0));
                        }
                        _ => bar_determinate.store(false, Ordering::Relaxed),
                    }
                    backup_btn.set_visible(!running);
                    stop_btn.set_visible(running);

                    let shown = if running { line.clone() } else { String::new() };
                    tray_running.store(running, Ordering::Relaxed);
                    if let Ok(mut g) = tray_status.lock() {
                        *g = shown.clone();
                    }
                    if let Some(h) = &handle {
                        let _ = h.update(move |t: &mut TmTray| {
                            t.running.store(running, Ordering::Relaxed);
                            if let Ok(mut g) = t.status.lock() {
                                *g = shown.clone();
                            }
                        });
                    }
                }
            }
            ControlFlow::Continue
        });
    }

    {
        let bar = progress_bar.clone();
        let running = tray_running.clone();
        let determinate = bar_determinate.clone();
        glib::timeout_add_local(Duration::from_millis(150), move || {
            if running.load(Ordering::Relaxed) && !determinate.load(Ordering::Relaxed) {
                bar.pulse();
            }
            ControlFlow::Continue
        });
    }

    window
}

fn selected(state: &Rc<RefCell<State>>) -> (String, String) {
    let s = state.borrow();
    (
        s.selected_ts.clone().unwrap_or_default(),
        s.target_root.clone(),
    )
}

fn populate_versions(
    state: &Rc<RefCell<State>>,
    list: &gtk::ListBox,
    stack: &gtk::Stack,
    overlay: &adw::ToastOverlay,
    version_label: &gtk::Label,
    helper: &str,
) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let target = state.borrow().target_root.clone();
    let versions = match exec(helper, &["list".to_string(), "--target".to_string(), target], false) {
        Ok(s) => serde_json::from_str::<Vec<Version>>(&s).unwrap_or_default(),
        Err(e) => {
            toast(overlay, &format!("Could not read versions: {e}"));
            Vec::new()
        }
    };
    state.borrow_mut().versions = versions.clone();

    if versions.is_empty() {
        list.append(
            &adw::ActionRow::builder()
                .title("No versions on this drive")
                .build(),
        );
        return;
    }

    for v in versions.iter().rev() {
        let kind = match (v.system, v.home) {
            (true, true) => "system and home",
            (true, false) => "system",
            (false, true) => "home",
            _ => "unknown",
        };
        let row = adw::ActionRow::builder()
            .title(&ts_when(&v.ts))
            .subtitle(kind)
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let state = state.clone();
        let stack = stack.clone();
        let version_label = version_label.clone();
        let ts = v.ts.clone();
        let when = ts_when(&v.ts);
        row.connect_activated(move |_| {
            state.borrow_mut().selected_ts = Some(ts.clone());
            version_label.set_text(&format!("Version: {when}"));
            stack.set_visible_child_name("restore");
        });
        list.append(&row);
    }
}
