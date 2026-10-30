// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{channel, Receiver, Sender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const REPO: &str = "XhaStudio/Akkhara-Programming-Language";

enum Msg {
    Progress(f32),
    Done,
    Fail(String),
}

// ---------------------------------------------------------------- platform

/// Name of the release asset to download for this OS/CPU.
/// Must match the asset names on the main repo's GitHub Releases.
fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("akk-x86_64-pc-windows-msvc.zip"),
        ("linux", "x86_64") => Some("akk-x86_64-unknown-linux-gnu.tar.gz"),
        ("macos", "aarch64") => Some("akk-aarch64-apple-darwin.tar.gz"),
        ("macos", "x86_64") => Some("akk-x86_64-apple-darwin.tar.gz"),
        _ => None,
    }
}

fn bin_name() -> &'static str {
    if cfg!(windows) {
        "akk.exe"
    } else {
        "akk"
    }
}

fn default_dir() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into()))
            .join("Akkhara")
            .join("bin")
    } else {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
            .join(".akkhara")
            .join("bin")
    }
}

// -------------------------------------------------------------------- PATH

#[cfg(windows)]
fn set_user_path(env: &winreg::RegKey, value: &str) -> Result<(), String> {
    use winreg::{enums::REG_EXPAND_SZ, RegValue};
    // Keep the value as REG_EXPAND_SZ so existing %VAR% entries keep working.
    let bytes: Vec<u8> = value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(|u| u.to_le_bytes())
        .collect();
    env.set_raw_value("Path", &RegValue { bytes, vtype: REG_EXPAND_SZ })
        .map_err(|e| format!("Could not update PATH: {e}"))
}

#[cfg(windows)]
fn add_to_path(dir: &Path) -> Result<bool, String> {
    use winreg::{enums::*, RegKey};
    let env = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
        .map_err(|e| e.to_string())?;
    let current: String = env.get_value("Path").unwrap_or_default();
    let d = dir.to_string_lossy().to_string();
    if current.split(';').any(|p| p.eq_ignore_ascii_case(&d)) {
        return Ok(false);
    }
    let new = if current.is_empty() {
        d
    } else {
        format!("{};{}", current.trim_end_matches(';'), d)
    };
    set_user_path(&env, &new)?;
    Ok(true)
}

#[cfg(windows)]
fn remove_from_path(dir: &Path) -> Result<(), String> {
    use winreg::{enums::*, RegKey};
    let env = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
        .map_err(|e| e.to_string())?;
    let current: String = env.get_value("Path").unwrap_or_default();
    let d = dir.to_string_lossy().to_string();
    let kept: Vec<&str> = current
        .split(';')
        .filter(|p| !p.is_empty() && !p.eq_ignore_ascii_case(&d))
        .collect();
    set_user_path(&env, &kept.join(";"))
}

#[cfg(not(windows))]
fn shell_rc_files() -> Vec<PathBuf> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let shell = std::env::var("SHELL").unwrap_or_default();
    let mut files: Vec<PathBuf> = [".zshrc", ".bashrc", ".profile"]
        .iter()
        .map(|f| home.join(f))
        .filter(|p| p.exists())
        .collect();
    // zsh (the macOS default) may not have a .zshrc yet.
    if shell.contains("zsh") && !files.iter().any(|p| p.ends_with(".zshrc")) {
        files.push(home.join(".zshrc"));
    }
    if files.is_empty() {
        files.push(home.join(".profile"));
    }
    files
}

#[cfg(not(windows))]
fn add_to_path(dir: &Path) -> Result<bool, String> {
    let line = format!("export PATH=\"{}:$PATH\"  # akkhara", dir.display());
    let mut changed = false;
    for f in shell_rc_files() {
        let mut text = fs::read_to_string(&f).unwrap_or_default();
        if text.contains("# akkhara") {
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&line);
        text.push('\n');
        fs::write(&f, text).map_err(|e| format!("Could not edit {}: {e}", f.display()))?;
        changed = true;
    }
    Ok(changed)
}

#[cfg(not(windows))]
fn remove_from_path(_dir: &Path) -> Result<(), String> {
    for f in shell_rc_files() {
        if let Ok(text) = fs::read_to_string(&f) {
            if text.contains("# akkhara") {
                let kept: Vec<&str> = text.lines().filter(|l| !l.contains("# akkhara")).collect();
                let mut out = kept.join("\n");
                out.push('\n');
                fs::write(&f, out).map_err(|e| format!("Could not edit {}: {e}", f.display()))?;
            }
        }
    }
    Ok(())
}

// ----------------------------------------------------------------- install

/// Pull the `akk` executable out of a downloaded .zip / .tar.gz (any folder depth).
fn extract_binary(asset: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let want = bin_name();
    if asset.ends_with(".zip") {
        let mut ar = zip::ZipArchive::new(std::io::Cursor::new(data))
            .map_err(|e| format!("Bad zip file: {e}"))?;
        for i in 0..ar.len() {
            let mut f = ar.by_index(i).map_err(|e| e.to_string())?;
            let is_match = f.is_file()
                && Path::new(f.name()).file_name().and_then(|n| n.to_str()) == Some(want);
            if is_match {
                let mut out = Vec::new();
                f.read_to_end(&mut out).map_err(|e| e.to_string())?;
                return Ok(out);
            }
        }
    } else if asset.ends_with(".tar.gz") {
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(data));
        for entry in ar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let is_match = entry
                .path()
                .ok()
                .and_then(|p| p.file_name().map(|n| n == want))
                .unwrap_or(false);
            if is_match {
                let mut out = Vec::new();
                entry.read_to_end(&mut out).map_err(|e| e.to_string())?;
                return Ok(out);
            }
        }
    } else {
        return Ok(data.to_vec());
    }
    Err(format!("`{want}` was not found inside {asset}"))
}

fn install(dir: PathBuf, add_path: bool, tx: &Sender<Msg>) -> Result<(), String> {
    let prog = |v: f32| {
        let _ = tx.send(Msg::Progress(v));
    };

    let asset = asset_name().ok_or("This platform is not supported yet.")?;
    prog(0.03);

    let release: serde_json::Value =
        ureq::get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
            .set("User-Agent", "akkhara-installer")
            .set("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| format!("Could not reach GitHub (is there a published release?): {e}"))?
            .into_json()
            .map_err(|e| e.to_string())?;
    prog(0.08);

    let tag = release["tag_name"].as_str().unwrap_or("unknown").to_string();
    let url = release["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"] == asset))
        .and_then(|x| x["browser_download_url"].as_str())
        .ok_or_else(|| format!("Release {tag} has no asset named {asset}"))?
        .to_string();

    let resp = ureq::get(&url)
        .set("User-Agent", "akkhara-installer")
        .call()
        .map_err(|e| format!("Download failed: {e}"))?;
    let total: Option<u64> = resp.header("Content-Length").and_then(|v| v.parse().ok());
    let mut reader = resp.into_reader().take(300_000_000);
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 32 * 1024];
    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| format!("Download failed: {e}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(t) = total.filter(|t| *t > 0) {
            prog(0.08 + 0.72 * (buf.len() as f32 / t as f32).min(1.0));
        }
    }
    if buf.len() < 1024 {
        return Err("Downloaded file looks too small; aborting.".into());
    }
    prog(0.82);

    let exe = extract_binary(asset, &buf)?;
    prog(0.88);

    fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let target = dir.join(bin_name());
    let tmp = dir.join(format!("{}.new", bin_name()));
    fs::write(&tmp, &exe).map_err(|e| format!("Could not write file: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, &target).map_err(|e| {
        format!("Could not replace {} (is akk running?): {e}", target.display())
    })?;
    prog(0.94);

    if add_path {
        add_to_path(&dir)?;
    }
    prog(1.0);
    Ok(())
}

fn uninstall(dir: PathBuf, tx: &Sender<Msg>) -> Result<(), String> {
    let prog = |v: f32| {
        let _ = tx.send(Msg::Progress(v));
    };
    prog(0.1);
    let target = dir.join(bin_name());
    if target.exists() {
        fs::remove_file(&target)
            .map_err(|e| format!("Could not remove {}: {e}", target.display()))?;
    }
    prog(0.5);
    remove_from_path(&dir)?;
    prog(0.8);
    let _ = fs::remove_dir(&dir);
    if let Some(parent) = dir.parent() {
        let name = parent.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "Akkhara" || name == ".akkhara" {
            let _ = fs::remove_dir(parent); // only succeeds if empty
        }
    }
    prog(1.0);
    Ok(())
}

// ---------------------------------------------------------------------- UI

const CARD: Color32 = Color32::from_rgb(118, 169, 199);
const LIGHT: Color32 = Color32::from_rgb(184, 229, 255);
const BLUE: Color32 = Color32::from_rgb(29, 91, 194);
const INK: Color32 = Color32::BLACK;

const WIN_W: f32 = 660.0;
const WIN_H: f32 = 280.0;

const LOGO_PNG: &[u8] = include_bytes!("../assets/logo.png");
// Pre-rendered with proper Myanmar text shaping (egui cannot shape Burmese itself).
const MESSAGE_PNGS: [&[u8]; 4] = [
    include_bytes!("../assets/msg1.png"),
    include_bytes!("../assets/msg2.png"),
    include_bytes!("../assets/msg3.png"),
    include_bytes!("../assets/msg4.png"),
];

struct Tex {
    handle: egui::TextureHandle,
    size: egui::Vec2,
}

fn load_tex(ctx: &egui::Context, name: &str, bytes: &[u8], scale: f32) -> Tex {
    let img = image::load_from_memory(bytes)
        .expect("bad embedded image")
        .to_rgba8();
    let (w, h) = img.dimensions();
    let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    Tex {
        handle: ctx.load_texture(name, ci, egui::TextureOptions::LINEAR),
        size: vec2(w as f32 / scale, h as f32 / scale),
    }
}

fn next_index(cur: usize, n: usize) -> usize {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as usize;
    let mut i = nanos % n;
    if i == cur {
        i = (i + 1) % n;
    }
    i
}

// ------------------------------------------------------- update checking

#[derive(Clone)]
enum Installed {
    None,            // no akk in the install folder
    Unknown,         // akk exists but its version could not be read
    Version(String), // e.g. "1.1.0"
}

enum Check {
    Latest(Option<String>),
    Installed(String, Installed), // (folder that was checked, result)
}

fn parse_version(text: &str) -> Option<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|t| t.trim_matches('.'))
        .find(|t| {
            t.contains('.') && t.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(|t| t.to_string())
}

fn version_nums(v: &str) -> Vec<u64> {
    v.trim_start_matches('v')
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

fn is_newer(latest: &str, installed: &str) -> bool {
    let (a, b) = (version_nums(latest), version_nums(installed));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (*a.get(i).unwrap_or(&0), *b.get(i).unwrap_or(&0));
        if x != y {
            return x > y;
        }
    }
    false
}

/// Runs `<dir>/akk --version` and reads the version number from its output.
fn installed_version(dir: &Path) -> Installed {
    let bin = dir.join(bin_name());
    if !bin.exists() {
        return Installed::None;
    }
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash
    }
    match cmd.output() {
        Ok(o) => {
            let text = format!(
                "{} {}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            match parse_version(&text) {
                Some(v) => Installed::Version(v),
                None => Installed::Unknown,
            }
        }
        Err(_) => Installed::Unknown,
    }
}

fn fetch_latest_tag() -> Option<String> {
    let v: serde_json::Value = ureq::get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .set("User-Agent", "akkhara-installer")
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .call()
        .ok()?
        .into_json()
        .ok()?;
    v["tag_name"].as_str().map(|s| s.to_string())
}

#[derive(Clone)]
enum Page {
    Setup,
    Working,
    Finished,
    Failed(String),
}

struct InstallerApp {
    page: Page,
    dir: String,
    add_path: bool,
    uninstalling: bool,
    rx: Option<Receiver<Msg>>,
    target: f32,
    shown: f32,
    done: bool,
    started: Instant,
    msg_idx: usize,
    msg_at: Instant,
    logo: Tex,
    msgs: Vec<Tex>,
    check_tx: Sender<Check>,
    check_rx: Receiver<Check>,
    latest: Option<String>,
    installed: Option<Installed>,
    checked_dir: String,
    updating: bool,
}

impl InstallerApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        let (check_tx, check_rx) = channel();
        let mut app = Self {
            check_tx,
            check_rx,
            latest: None,
            installed: None,
            checked_dir: String::new(),
            updating: false,
            page: Page::Setup,
            dir: default_dir().to_string_lossy().to_string(),
            add_path: true,
            uninstalling: false,
            rx: None,
            target: 0.0,
            shown: 0.0,
            done: false,
            started: Instant::now(),
            msg_idx: next_index(usize::MAX, MESSAGE_PNGS.len()),
            msg_at: Instant::now(),
            logo: load_tex(ctx, "logo", LOGO_PNG, 1.0),
            msgs: MESSAGE_PNGS
                .iter()
                .enumerate()
                .map(|(i, b)| load_tex(ctx, &format!("msg{i}"), b, 2.0))
                .collect(),
        };
        app.check_installed(ctx);
        app.check_latest(ctx);
        app
    }

    /// Look at the install folder in the background (is akk there, and which version?).
    fn check_installed(&mut self, ctx: &egui::Context) {
        self.installed = None;
        self.checked_dir = self.dir.trim().to_string();
        let dir = PathBuf::from(&self.checked_dir);
        let key = self.checked_dir.clone();
        let tx = self.check_tx.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(Check::Installed(key, installed_version(&dir)));
            ctx.request_repaint();
        });
    }

    /// Ask GitHub for the newest release tag in the background.
    fn check_latest(&self, ctx: &egui::Context) {
        let tx = self.check_tx.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(Check::Latest(fetch_latest_tag()));
            ctx.request_repaint();
        });
    }

    /// True only when akk is already installed and a newer release exists.
    fn update_available(&self) -> bool {
        match (&self.installed, &self.latest) {
            (Some(Installed::Version(v)), Some(l)) => is_newer(l, v),
            (Some(Installed::Unknown), Some(_)) => true,
            _ => false,
        }
    }

    fn start(&mut self, remove: bool) {
        self.updating = !remove && self.update_available();
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.uninstalling = remove;
        self.page = Page::Working;
        self.target = 0.0;
        self.shown = 0.0;
        self.done = false;
        self.started = Instant::now();
        self.msg_at = Instant::now();
        self.msg_idx = next_index(usize::MAX, self.msgs.len());
        let dir = PathBuf::from(self.dir.trim());
        let add_path = self.add_path;
        thread::spawn(move || {
            let result = if remove {
                uninstall(dir, &tx)
            } else {
                install(dir, add_path, &tx)
            };
            let _ = match result {
                Ok(()) => tx.send(Msg::Done),
                Err(e) => tx.send(Msg::Fail(e)),
            };
        });
    }
}

// ---- drawing helpers

fn thick_box(p: &egui::Painter, r: Rect, radius: f32, fill: Color32, w: f32) {
    p.rect(r.shrink(w / 2.0), radius, fill, Stroke::new(w, INK));
}

fn rounded_path(r: Rect, rad: f32) -> Vec<Pos2> {
    let corners = [
        (r.right_top() + vec2(-rad, rad), -90.0_f32),
        (r.right_bottom() + vec2(-rad, -rad), 0.0),
        (r.left_bottom() + vec2(rad, -rad), 90.0),
        (r.left_top() + vec2(rad, rad), 180.0),
    ];
    let mut pts = Vec::new();
    for (c, start) in corners {
        for i in 0..=8 {
            let a = (start + i as f32 * 90.0 / 8.0).to_radians();
            pts.push(c + vec2(a.cos(), a.sin()) * rad);
        }
    }
    pts.push(pts[0]);
    pts
}

fn draw_logo(p: &egui::Painter, logo: &Tex, r: Rect) {
    p.rect_filled(r, 26.0, LIGHT);
    p.image(
        logo.handle.id(),
        r.shrink(10.0),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    p.rect_stroke(r.shrink(4.0), 26.0, Stroke::new(8.0_f32, BLUE));
}

fn big_button(ui: &mut egui::Ui, p: &egui::Painter, r: Rect, label: &str) -> bool {
    let resp = ui.interact(r, ui.id().with(label), Sense::click());
    let fill = if resp.is_pointer_button_down_on() {
        Color32::from_rgb(146, 208, 244)
    } else if resp.hovered() {
        Color32::from_rgb(204, 238, 255)
    } else {
        LIGHT
    };
    thick_box(p, r, 24.0, fill, 6.0);
    p.text(r.center(), Align2::CENTER_CENTER, label, FontId::monospace(20.0), INK);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    resp.clicked()
}

fn heading(p: &egui::Painter, origin: Pos2, text: &str) {
    p.text(
        origin + vec2(207.0, 0.0),
        Align2::LEFT_CENTER,
        text,
        FontId::monospace(22.0),
        INK,
    );
}

impl eframe::App for InstallerApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ---- results of the version checks
        while let Ok(m) = self.check_rx.try_recv() {
            match m {
                Check::Latest(tag) => self.latest = tag,
                Check::Installed(dir, state) => {
                    if dir == self.checked_dir {
                        self.installed = Some(state);
                    }
                }
            }
        }

        // ---- messages from the worker thread
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Progress(v) => self.target = self.target.max(v),
                    Msg::Done => {
                        self.target = 1.0;
                        self.done = true;
                    }
                    Msg::Fail(e) => self.page = Page::Failed(e),
                }
            }
        }

        // ---- animation
        if matches!(self.page, Page::Working) {
            self.shown += (self.target - self.shown) * 0.12;
            if self.done && self.shown >= 0.995 {
                self.shown = 1.0;
                self.page = Page::Finished;
            }
            if self.msg_at.elapsed() >= Duration::from_secs(3) {
                self.msg_idx = next_index(self.msg_idx, self.msgs.len());
                self.msg_at = Instant::now();
            }
            ctx.request_repaint_after(Duration::from_millis(16));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                let full = ui.max_rect();
                let o = full.min; // origin: design coordinates are relative to the card
                let p = ui.painter().clone();

                // drag the frameless window by its background
                let drag = ui.interact(full, ui.id().with("drag"), Sense::drag());
                if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                // card
                thick_box(&p, full.shrink(0.0), 26.0, CARD, 8.0);

                // close (x) button
                let cx = Rect::from_center_size(pos2(full.max.x - 30.0, o.y + 30.0), vec2(24.0, 24.0));
                let cr = ui.interact(cx, ui.id().with("close"), Sense::click());
                let col = if cr.hovered() { BLUE } else { INK };
                let c = cx.center();
                p.line_segment([c + vec2(-6.0, -6.0), c + vec2(6.0, 6.0)], Stroke::new(3.0_f32, col));
                p.line_segment([c + vec2(-6.0, 6.0), c + vec2(6.0, -6.0)], Stroke::new(3.0_f32, col));
                if cr.hovered() {
                    ctx.set_cursor_icon(CursorIcon::PointingHand);
                }
                if cr.clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }

                match self.page.clone() {
                    // ------------------------------------------------ setup (screenshot 1)
                    Page::Setup => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(22.0, 20.0), vec2(167.0, 167.0)));
                        p.text(o + vec2(206.0, 62.0), Align2::LEFT_CENTER, "Install folder", FontId::monospace(20.0), INK);

                        let field = Rect::from_min_size(o + vec2(205.0, 88.0), vec2(411.0, 42.0));
                        p.rect_filled(field.shrink(2.0), 14.0, LIGHT);
                        p.extend(egui::Shape::dashed_line(
                            &rounded_path(field.shrink(2.0), 14.0),
                            Stroke::new(3.0_f32, INK),
                            9.0,
                            6.0,
                        ));
                        let field_resp = ui.put(
                            field.shrink2(vec2(14.0, 4.0)),
                            egui::TextEdit::singleline(&mut self.dir)
                                .frame(false)
                                .font(FontId::monospace(14.0))
                                .text_color(INK)
                                .vertical_align(Align::Center)
                                .desired_width(f32::INFINITY),
                        );
                        if field_resp.lost_focus() && self.dir.trim() != self.checked_dir {
                            self.check_installed(ctx);
                        }

                        // "Add akk to PATH" (round check)
                        let row = Rect::from_min_size(o + vec2(206.0, 154.0), vec2(260.0, 28.0));
                        let rr = ui.interact(row, ui.id().with("path"), Sense::click());
                        if rr.clicked() {
                            self.add_path = !self.add_path;
                        }
                        let center = o + vec2(218.0, 168.0);
                        p.circle(center, 11.0, Color32::TRANSPARENT, Stroke::new(3.0_f32, INK));
                        if self.add_path {
                            p.circle_filled(center, 6.0, BLUE);
                        }
                        p.text(o + vec2(245.0, 168.0), Align2::LEFT_CENTER, "Add akk to PATH", FontId::monospace(16.0), INK);

                        let btn_label = if self.update_available() { "Update" } else { "Install" };
                        if big_button(ui, &p, Rect::from_min_size(o + vec2(197.0, 197.0), vec2(383.0, 62.0)), btn_label)
                            && !self.dir.trim().is_empty()
                        {
                            self.start(false);
                        }

                        // small uninstall link
                        let ur = Rect::from_center_size(o + vec2(616.0, 228.0), vec2(70.0, 20.0));
                        let uresp = ui.interact(ur, ui.id().with("uninstall"), Sense::click());
                        let ucol = if uresp.hovered() { BLUE } else { Color32::from_rgb(20, 60, 100) };
                        p.text(ur.center(), Align2::CENTER_CENTER, "Uninstall", FontId::monospace(12.0), ucol);
                        if uresp.hovered() {
                            ctx.set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if uresp.clicked() && !self.dir.trim().is_empty() {
                            self.start(true);
                        }
                    }

                    // --------------------------------------------- installing (screenshot 2)
                    Page::Working => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(22.0, 17.0), vec2(168.0, 168.0)));
                        heading(&p, o + vec2(0.0, 53.0), if self.uninstalling { "Uninstalling..." } else if self.updating { "Updating..." } else { "Installing..." });

                        let bar = Rect::from_min_size(o + vec2(207.0, 81.0), vec2(407.0, 47.0));
                        thick_box(&p, bar, 18.0, LIGHT, 6.0);
                        if self.shown > 0.0 {
                            let inner = bar.shrink(6.0);
                            let w = (inner.width() * self.shown).max(30.0);
                            thick_box(&p, Rect::from_min_size(inner.min, vec2(w, inner.height())), 13.0, BLUE, 5.0);
                        }
                        let pct_col = if self.shown > 0.55 { Color32::WHITE } else { INK };
                        p.text(
                            bar.center(),
                            Align2::CENTER_CENTER,
                            format!("{:.0}%", self.shown * 100.0),
                            FontId::monospace(18.0),
                            pct_col,
                        );

                        let left = if self.shown > 0.08 {
                            let rem = self.started.elapsed().as_secs_f32() * (1.0 - self.shown) / self.shown;
                            if rem < 45.0 {
                                "Less than a minute left".to_string()
                            } else {
                                let n = (rem / 60.0).ceil() as u32;
                                if n <= 1 { "About 1 minute left".to_string() } else { format!("{n} minutes left") }
                            }
                        } else {
                            "Calculating...".to_string()
                        };
                        p.text(o + vec2(217.0, 153.0), Align2::LEFT_CENTER, left, FontId::monospace(15.0), INK);

                        let t = &self.msgs[self.msg_idx];
                        let rect = Rect::from_center_size(pos2(full.center().x, o.y + 222.0), t.size);
                        p.image(
                            t.handle.id(),
                            rect,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }

                    // ------------------------------------------------------- finished
                    Page::Finished => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(22.0, 17.0), vec2(168.0, 168.0)));
                        let title = if self.uninstalling {
                            "Uninstall complete!"
                        } else if self.updating {
                            "Update complete!"
                        } else {
                            "Installation complete!"
                        };
                        heading(&p, o + vec2(0.0, 48.0), title);

                        if self.uninstalling {
                            p.text(o + vec2(207.0, 96.0), Align2::LEFT_CENTER, "akk was removed from:", FontId::monospace(14.0), INK);
                            p.text(o + vec2(207.0, 122.0), Align2::LEFT_CENTER, self.dir.trim(), FontId::monospace(13.0), INK);
                        } else {
                            p.text(o + vec2(207.0, 88.0), Align2::LEFT_CENTER, "Open a NEW terminal, then try:", FontId::monospace(14.0), INK);
                            let code = Rect::from_min_size(o + vec2(207.0, 104.0), vec2(407.0, 62.0));
                            thick_box(&p, code, 14.0, LIGHT, 4.0);
                            p.text(code.min + vec2(18.0, 20.0), Align2::LEFT_CENTER, "akk --version", FontId::monospace(15.0), INK);
                            p.text(code.min + vec2(18.0, 43.0), Align2::LEFT_CENTER, "akk <file_name>", FontId::monospace(15.0), INK);
                            p.text(o + vec2(207.0, 182.0), Align2::LEFT_CENTER, format!("Installed in {}", self.dir.trim()), FontId::monospace(11.0), INK);
                        }

                        if big_button(ui, &p, Rect::from_min_size(o + vec2(207.0, 208.0), vec2(407.0, 50.0)), "Finish") {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }

                    // --------------------------------------------------------- failed
                    Page::Failed(err) => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(22.0, 17.0), vec2(168.0, 168.0)));
                        heading(&p, o + vec2(0.0, 48.0), "Something went wrong");
                        let mut text: String = err.chars().take(260).collect();
                        if err.chars().count() > 260 {
                            text.push_str("...");
                        }
                        let galley = p.layout(text, FontId::monospace(13.0), INK, 407.0);
                        p.galley(o + vec2(207.0, 78.0), galley, INK);
                        if big_button(ui, &p, Rect::from_min_size(o + vec2(207.0, 208.0), vec2(407.0, 50.0)), "Back") {
                            self.page = Page::Setup;
                            self.check_installed(ctx);
                        }
                    }
                }
            });
    }
}

// Window / taskbar icon. Put your icon at `favicon.png` in the repo root (square PNG, e.g. 256x256).
const FAVICON_PNG: &[u8] = include_bytes!("../favicon.png");

fn window_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(FAVICON_PNG).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Akkhara Installer")
        .with_inner_size([WIN_W, WIN_H])
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false);
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "Akkhara Installer",
        options,
        Box::new(|cc| Ok(Box::new(InstallerApp::new(cc)))),
    )
}
