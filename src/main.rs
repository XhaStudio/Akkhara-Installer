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

mod mmtext;
use mmtext::{contains_myanmar, Shaper};
use std::collections::HashMap;

const REPO: &str = "XhaStudio/Akkhara-Programming-Language";
const INSTALLER_REPO: &str = "XhaStudio/Akkhara-Installer";
const INSTALLER_VERSION: &str = env!("CARGO_PKG_VERSION");

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

fn download(url: &str, tx: &Sender<Msg>, from: f32, to: f32) -> Result<Vec<u8>, String> {
    let resp = ureq::get(url)
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
            let frac = (buf.len() as f32 / t as f32).min(1.0);
            let _ = tx.send(Msg::Progress(from + (to - from) * frac));
        }
    }
    Ok(buf)
}

/// Install the selected akk release: download its archive, unpack `akk`, add to PATH.
fn install(dir: PathBuf, add_path: bool, url: &str, asset: &str, tx: &Sender<Msg>) -> Result<(), String> {
    let prog = |v: f32| {
        let _ = tx.send(Msg::Progress(v));
    };
    prog(0.03);
    let buf = download(url, tx, 0.05, 0.8)?;
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

/// Name of this installer's own release asset for the current OS/CPU
/// (must match the names produced by .github/workflows/release.yml).
fn installer_asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("Akkhara-Installer-windows-x86_64.exe"),
        ("linux", "x86_64") => Some("Akkhara-Installer-linux-x86_64"),
        ("macos", "aarch64") => Some("Akkhara-Installer-macos-aarch64"),
        ("macos", "x86_64") => Some("Akkhara-Installer-macos-x86_64"),
        _ => None,
    }
}

/// Replace this running installer with the chosen installer release.
/// (A running exe can be renamed but not overwritten, so: current -> .old, new -> current.)
fn self_update(exe: PathBuf, url: &str, tx: &Sender<Msg>) -> Result<(), String> {
    let prog = |v: f32| {
        let _ = tx.send(Msg::Progress(v));
    };
    prog(0.03);
    let buf = download(url, tx, 0.05, 0.85)?;
    if buf.len() < 100_000 {
        return Err("Downloaded installer looks too small; aborting.".into());
    }

    let new_path = exe.with_extension("new");
    let old_path = exe.with_extension("old");
    fs::write(&new_path, &buf).map_err(|e| format!("Could not write the new installer: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&new_path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    prog(0.9);

    let _ = fs::remove_file(&old_path);
    fs::rename(&exe, &old_path).map_err(|e| format!("Could not replace the installer: {e}"))?;
    if let Err(e) = fs::rename(&new_path, &exe) {
        let _ = fs::rename(&old_path, &exe); // roll back
        return Err(format!("Could not replace the installer: {e}"));
    }
    prog(1.0);
    Ok(())
}

// ------------------------------------------------------ versions & releases

#[derive(Clone)]
enum Installed {
    None,            // no akk in the install folder
    Unknown,         // akk exists but its version could not be read
    Version(String), // e.g. "1.1.0"
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
    cmd.arg("--version").stdin(std::process::Stdio::null());
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

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Akkhara,
    Installer,
}

impl Target {
    fn idx(self) -> usize {
        match self {
            Target::Akkhara => 0,
            Target::Installer => 1,
        }
    }
    fn repo(self) -> &'static str {
        match self {
            Target::Akkhara => REPO,
            Target::Installer => INSTALLER_REPO,
        }
    }
    fn title(self) -> &'static str {
        match self {
            Target::Akkhara => "Akkhara Release",
            Target::Installer => "Installer release",
        }
    }
    fn asset(self) -> Option<&'static str> {
        match self {
            Target::Akkhara => asset_name(),
            Target::Installer => installer_asset_name(),
        }
    }
}

#[derive(Clone)]
struct Release {
    tag: String,
    prerelease: bool,
    latest: bool,
    published: Option<i64>, // unix seconds
    size: Option<u64>,      // bytes of the file for this OS/CPU
    url: Option<String>,    // download URL of that file
    body: String,           // release notes (markdown)
}

enum Load<T> {
    Idle,
    Loading,
    Done(T),
    Error,
}

enum Check {
    Installed(String, Installed), // (folder that was checked, result)
    List(Target, Result<Vec<Release>, String>),
    Detail(Target, String, Result<Release, String>),
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "2026-11-15T10:20:30Z" -> unix seconds
fn iso_to_unix(s: &str) -> Option<i64> {
    let n = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (h, mi, se) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn ago(published: i64) -> String {
    let s = (now_secs() - published).max(0);
    let plural = |n: i64, unit: &str| format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" });
    match s {
        0..=59 => "Just now".to_string(),
        60..=3599 => plural(s / 60, "min"),
        3600..=86_399 => plural(s / 3600, "hour"),
        86_400..=172_799 => "Yesterday".to_string(),
        172_800..=604_799 => plural(s / 86_400, "day"),
        604_800..=1_209_599 => "Last week".to_string(),
        1_209_600..=2_591_999 => plural(s / 604_800, "week"),
        2_592_000..=5_183_999 => "Last month".to_string(),
        5_184_000..=31_535_999 => plural(s / 2_592_000, "month"),
        _ => plural(s / 31_536_000, "year"),
    }
}

fn size_label(bytes: u64) -> String {
    format!("{:.2}MB", bytes as f64 / 1_048_576.0)
}

fn parse_release(v: &serde_json::Value, asset: Option<&str>) -> Release {
    let a = asset.and_then(|n| {
        v["assets"]
            .as_array()
            .and_then(|arr| arr.iter().find(|x| x["name"] == n))
    });
    Release {
        tag: v["tag_name"].as_str().unwrap_or("?").to_string(),
        prerelease: v["prerelease"].as_bool().unwrap_or(false),
        latest: false,
        published: v["published_at"].as_str().and_then(iso_to_unix),
        size: a.and_then(|x| x["size"].as_u64()),
        url: a
            .and_then(|x| x["browser_download_url"].as_str())
            .map(|s| s.to_string()),
        body: v["body"].as_str().unwrap_or("").to_string(),
    }
}

fn github_get(path: &str) -> Result<serde_json::Value, String> {
    ureq::get(&format!("https://api.github.com/repos/{path}"))
        .set("User-Agent", "akkhara-installer")
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(12))
        .call()
        .map_err(|e| e.to_string())?
        .into_json()
        .map_err(|e| e.to_string())
}

fn load_releases(t: Target) -> Result<Vec<Release>, String> {
    let v = github_get(&format!("{}/releases?per_page=30", t.repo()))?;
    let arr = v.as_array().ok_or("unexpected response")?;
    let mut out: Vec<Release> = arr
        .iter()
        .filter(|x| !x["draft"].as_bool().unwrap_or(false))
        .map(|x| parse_release(x, t.asset()))
        .collect();
    if let Some(first) = out.iter_mut().find(|r| !r.prerelease) {
        first.latest = true;
    }
    Ok(out)
}

fn load_release(t: Target, tag: &str) -> Result<Release, String> {
    let v = github_get(&format!("{}/releases/tags/{}", t.repo(), tag))?;
    Ok(parse_release(&v, t.asset()))
}

// ---------------------------------------------------------------------- UI

const CARD: Color32 = Color32::from_rgb(118, 169, 199);
const LIGHT: Color32 = Color32::from_rgb(184, 229, 255);
const BLUE: Color32 = Color32::from_rgb(29, 91, 194);
const INK: Color32 = Color32::BLACK;
const ROW: Color32 = Color32::from_rgb(133, 205, 247);
const GREY: Color32 = Color32::from_rgb(85, 85, 85);
const PURPLE: Color32 = Color32::from_rgb(46, 14, 140);
const LILAC: Color32 = Color32::from_rgb(200, 172, 255);
const GREEN_BG: Color32 = Color32::from_rgb(38, 94, 14);
const GREEN_FG: Color32 = Color32::from_rgb(126, 206, 32);
const GOLD_BG: Color32 = Color32::from_rgb(172, 138, 1);
const GOLD_FG: Color32 = Color32::from_rgb(255, 230, 120);
const ERR_RED: Color32 = Color32::from_rgb(248, 101, 103);
const TRACK: Color32 = Color32::from_rgb(118, 169, 199);
const THUMB: Color32 = Color32::from_rgb(75, 125, 153);

const MENU_W: f32 = 700.0; // menu, release lists, release page
const MENU_H: f32 = 384.0;
const SETUP_W: f32 = 660.0; // install folder, progress, finished
const SETUP_H: f32 = 280.0;

const LOGO_PNG: &[u8] = include_bytes!("../assets/logo.png");
const ABOUT_PNG: &[u8] = include_bytes!("../assets/about.png");
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

#[derive(Clone)]
enum Page {
    Menu,           // main menu
    List,           // release list of `target`
    Open,           // loading / failed while opening one release
    Detail,         // one release: notes + Install
    Setup,          // install folder + PATH
    Working,        // progress
    Finished,
    Failed(String), // install / update error
}

struct InstallerApp {
    page: Page,
    target: Target,
    lists: [Load<Vec<Release>>; 2],
    detail: Load<Release>,
    sel_tag: String,
    chosen: Option<Release>,
    dir: String,
    add_path: bool,
    uninstalling: bool,
    updating: bool,
    self_updating: bool,
    rx: Option<Receiver<Msg>>,
    goal: f32,
    shown: f32,
    done: bool,
    started: Instant,
    msg_idx: usize,
    msg_at: Instant,
    logo: Tex,
    about: Tex,
    shaper: Option<Shaper>,
    md_cache: HashMap<String, Tex>,
    msgs: Vec<Tex>,
    check_tx: Sender<Check>,
    check_rx: Receiver<Check>,
    installed: Option<Installed>,
    checked_dir: String,
    exe_path: Option<PathBuf>,
    win_size: egui::Vec2,
}

impl InstallerApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        let (check_tx, check_rx) = channel();
        let mut app = Self {
            page: Page::Menu,
            target: Target::Akkhara,
            lists: [Load::Idle, Load::Idle],
            detail: Load::Idle,
            sel_tag: String::new(),
            chosen: None,
            dir: default_dir().to_string_lossy().to_string(),
            add_path: true,
            uninstalling: false,
            updating: false,
            self_updating: false,
            rx: None,
            goal: 0.0,
            shown: 0.0,
            done: false,
            started: Instant::now(),
            msg_idx: next_index(usize::MAX, MESSAGE_PNGS.len()),
            msg_at: Instant::now(),
            logo: load_tex(ctx, "logo", LOGO_PNG, 1.0),
            about: load_tex(ctx, "about", ABOUT_PNG, 2.0),
            shaper: Shaper::new(),
            md_cache: HashMap::new(),
            msgs: MESSAGE_PNGS
                .iter()
                .enumerate()
                .map(|(i, b)| load_tex(ctx, &format!("msg{i}"), b, 2.0))
                .collect(),
            check_tx,
            check_rx,
            installed: None,
            checked_dir: String::new(),
            exe_path: std::env::current_exe().ok(),
            win_size: vec2(MENU_W, MENU_H),
        };
        app.check_installed(ctx);
        app
    }

    fn page_size(&self) -> egui::Vec2 {
        match &self.page {
            Page::Menu | Page::List | Page::Open | Page::Detail => vec2(MENU_W, MENU_H),
            _ => vec2(SETUP_W, SETUP_H),
        }
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

    fn fetch_list(&mut self, ctx: &egui::Context, t: Target) {
        self.lists[t.idx()] = Load::Loading;
        let tx = self.check_tx.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = tx.send(Check::List(t, load_releases(t)));
            ctx.request_repaint();
        });
    }

    fn open_list(&mut self, ctx: &egui::Context, t: Target) {
        self.target = t;
        self.page = Page::List;
        if !matches!(self.lists[t.idx()], Load::Done(_)) {
            self.fetch_list(ctx, t);
        }
    }

    fn fetch_detail(&mut self, ctx: &egui::Context) {
        self.detail = Load::Loading;
        let (t, tag) = (self.target, self.sel_tag.clone());
        let tx = self.check_tx.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let r = load_release(t, &tag);
            let _ = tx.send(Check::Detail(t, tag, r));
            ctx.request_repaint();
        });
    }

    fn open_release(&mut self, ctx: &egui::Context, tag: String) {
        self.sel_tag = tag;
        self.page = Page::Open;
        self.fetch_detail(ctx);
    }

    /// "Update" when akk is installed and this release is newer than it, else "Install".
    fn akk_label(&self, tag: &str) -> &'static str {
        match &self.installed {
            Some(Installed::Version(v)) if is_newer(tag, v) => "Update",
            _ => "Install",
        }
    }

    fn reset_progress(&mut self) {
        self.page = Page::Working;
        self.goal = 0.0;
        self.shown = 0.0;
        self.done = false;
        self.started = Instant::now();
        self.msg_at = Instant::now();
        self.msg_idx = next_index(usize::MAX, self.msgs.len());
    }

    fn start(&mut self, remove: bool) {
        let source = if remove {
            None
        } else {
            match (self.chosen.as_ref().and_then(|r| r.url.clone()), asset_name()) {
                (Some(u), Some(a)) => Some((u, a.to_string())),
                _ => {
                    self.page = Page::Failed("This release has no download for your system.".into());
                    return;
                }
            }
        };
        let tag = self.chosen.as_ref().map(|r| r.tag.clone()).unwrap_or_default();
        self.updating = !remove && self.akk_label(&tag) == "Update";
        self.uninstalling = remove;
        self.self_updating = false;
        self.reset_progress();

        let (tx, rx) = channel();
        self.rx = Some(rx);
        let dir = PathBuf::from(self.dir.trim());
        let add_path = self.add_path;
        thread::spawn(move || {
            let result = match source {
                None => uninstall(dir, &tx),
                Some((url, asset)) => install(dir, add_path, &url, &asset, &tx),
            };
            let _ = match result {
                Ok(()) => tx.send(Msg::Done),
                Err(e) => tx.send(Msg::Fail(e)),
            };
        });
    }

    fn start_self_update(&mut self) {
        let url = self.chosen.as_ref().and_then(|r| r.url.clone());
        let (Some(exe), Some(url)) = (self.exe_path.clone(), url) else {
            self.page = Page::Failed("This installer release has no download for your system.".into());
            return;
        };
        self.self_updating = true;
        self.uninstalling = false;
        self.updating = false;
        self.reset_progress();
        let (tx, rx) = channel();
        self.rx = Some(rx);
        thread::spawn(move || {
            let _ = match self_update(exe, &url, &tx) {
                Ok(()) => tx.send(Msg::Done),
                Err(e) => tx.send(Msg::Fail(e)),
            };
        });
    }
}

// ---- drawing helpers

fn box_with(p: &egui::Painter, r: Rect, radius: f32, fill: Color32, w: f32, stroke: Color32) {
    p.rect(r.shrink(w / 2.0), radius, fill, Stroke::new(w, stroke));
}

fn thick_box(p: &egui::Painter, r: Rect, radius: f32, fill: Color32, w: f32) {
    box_with(p, r, radius, fill, w, INK);
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

/// Grey-bordered blue-text button (menu, BACK, Install on the release page).
fn menu_button(ui: &mut egui::Ui, p: &egui::Painter, r: Rect, label: &str, size: f32) -> bool {
    let resp = ui.interact(r, ui.id().with(("mb", label)), Sense::click());
    let fill = if resp.is_pointer_button_down_on() {
        Color32::from_rgb(110, 188, 236)
    } else if resp.hovered() {
        Color32::from_rgb(160, 220, 252)
    } else {
        ROW
    };
    box_with(p, r, 16.0, fill, 5.0, GREY);
    p.text(r.center(), Align2::CENTER_CENTER, label, FontId::monospace(size), BLUE);
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

/// Underlined page title.
fn title(p: &egui::Painter, pos: Pos2, text: &str, size: f32) {
    let r = p.text(pos, Align2::LEFT_CENTER, text, FontId::monospace(size), INK);
    p.line_segment(
        [r.left_bottom() + vec2(0.0, 1.0), r.right_bottom() + vec2(0.0, 1.0)],
        Stroke::new(2.0_f32, INK),
    );
}

fn pill_width(p: &egui::Painter, text: &str, size: f32) -> f32 {
    p.layout_no_wrap(text.to_string(), FontId::monospace(size), INK).size().x + 22.0
}

/// Rounded label starting at `pos` (vertically centred on pos.y). Returns its width.
fn pill(p: &egui::Painter, pos: Pos2, text: &str, bg: Color32, fg: Color32, size: f32) -> f32 {
    let w = pill_width(p, text, size);
    let r = Rect::from_min_size(pos2(pos.x, pos.y - 14.0), vec2(w, 28.0));
    p.rect_filled(r, 10.0, bg);
    p.text(r.center(), Align2::CENTER_CENTER, text, FontId::monospace(size), fg);
    w
}

/// White text with a dark outline (the size label in the release rows).
fn outlined_text(p: &egui::Painter, pos: Pos2, text: &str, size: f32) {
    let font = FontId::monospace(size);
    let dark = Color32::from_rgb(80, 80, 90);
    for d in [vec2(-1.5, 0.0), vec2(1.5, 0.0), vec2(0.0, -1.5), vec2(0.0, 1.5), vec2(1.5, 1.5)] {
        p.text(pos + d, Align2::LEFT_CENTER, text, font.clone(), dark);
    }
    p.text(pos, Align2::LEFT_CENTER, text, font, Color32::WHITE);
}

/// One release row: version, age, size and Latest / pre-release badge.
fn draw_release_row(p: &egui::Painter, rect: Rect, r: &Release, hover: bool) {
    let fill = if hover { Color32::from_rgb(160, 220, 252) } else { ROW };
    box_with(p, rect, 14.0, fill, 5.0, GREY);
    let cy = rect.center().y;
    let vr = p.text(pos2(rect.min.x + 22.0, cy), Align2::LEFT_CENTER, &r.tag, FontId::monospace(17.0), BLUE);
    let mut x = vr.max.x + 14.0;
    if let Some(ts) = r.published {
        x += pill(p, pos2(x, cy), &ago(ts), PURPLE, LILAC, 13.0) + 20.0;
    }
    let size = r.size.map(size_label).unwrap_or_else(|| "-".to_string());
    outlined_text(p, pos2(x, cy), &size, 16.0);

    let badge = if r.latest {
        Some(("Latest", GREEN_BG, GREEN_FG))
    } else if r.prerelease {
        Some(("pre-release", GOLD_BG, GOLD_FG))
    } else {
        None
    };
    if let Some((t, bg, fg)) = badge {
        let w = pill_width(p, t, 13.0);
        pill(p, pos2(rect.max.x - 16.0 - w, cy), t, bg, fg, 13.0);
    }
}

fn scroll_thumb<R>(p: &egui::Painter, panel: Rect, out: &egui::scroll_area::ScrollAreaOutput<R>) {
    let view_h = out.inner_rect.height();
    let content_h = out.content_size.y;
    if content_h <= view_h + 1.0 {
        return;
    }
    let track = Rect::from_min_max(
        pos2(panel.max.x - 30.0, panel.min.y + 14.0),
        pos2(panel.max.x - 17.0, panel.max.y - 14.0),
    );
    p.rect_filled(track, 7.0, TRACK);
    let th = (track.height() * view_h / content_h).clamp(36.0, track.height());
    let t = (out.state.offset.y / (content_h - view_h)).clamp(0.0, 1.0);
    let y = track.min.y + t * (track.height() - th);
    p.rect_filled(Rect::from_min_size(pos2(track.min.x, y), vec2(track.width(), th)), 7.0, THUMB);
}

/// Scrollable list of release rows inside `panel`. Returns the tag that was clicked.
fn release_list(ui: &mut egui::Ui, p: &egui::Painter, panel: Rect, rels: &[Release], key: usize) -> Option<String> {
    let view = Rect::from_min_max(
        panel.min + vec2(12.0, 12.0),
        pos2(panel.max.x - 40.0, panel.max.y - 12.0),
    );
    let mut clicked = None;
    let out = ui
        .allocate_new_ui(egui::UiBuilder::new().max_rect(view), |ui| {
            ui.push_id(("rel_list", key), |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 12.0;
                        for r in rels {
                            let (rect, resp) = ui.allocate_exact_size(vec2(view.width(), 61.0), Sense::click());
                            draw_release_row(ui.painter(), rect, r, resp.hovered());
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            }
                            if resp.clicked() {
                                clicked = Some(r.tag.clone());
                            }
                        }
                    })
            })
            .inner
        })
        .inner;
    scroll_thumb(p, panel, &out);
    clicked
}

fn loading_view(p: &egui::Painter, panel: Rect) {
    p.text(panel.center(), Align2::CENTER_CENTER, "Loading...", FontId::monospace(28.0), INK);
}

/// Circular-arrow retry icon + red message. Returns true when the icon is clicked.
fn error_view(ui: &mut egui::Ui, p: &egui::Painter, panel: Rect) -> bool {
    let c = panel.center() + vec2(0.0, -28.0);
    let resp = ui.interact(
        Rect::from_center_size(c, vec2(84.0, 84.0)),
        ui.id().with("retry"),
        Sense::click(),
    );
    let col = if resp.hovered() { BLUE } else { INK };
    let (radius, sweep, n) = (32.0_f32, 300.0_f32, 40);
    let pts: Vec<Pos2> = (0..=n)
        .map(|i| {
            let a = (sweep * i as f32 / n as f32).to_radians();
            c + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    let end = pts[pts.len() - 1];
    p.add(egui::Shape::line(pts, Stroke::new(5.0_f32, col)));
    let a = sweep.to_radians();
    let tan = vec2(-a.sin(), a.cos()); // clockwise direction at the arc's end
    for ang in [150.0_f32, -150.0] {
        let r = ang.to_radians();
        let d = vec2(tan.x * r.cos() - tan.y * r.sin(), tan.x * r.sin() + tan.y * r.cos());
        p.line_segment([end, end + d * 13.0], Stroke::new(5.0_f32, col));
    }
    p.text(panel.center() + vec2(0.0, 40.0), Align2::CENTER_CENTER, "Something went wrong!!", FontId::monospace(14.0), ERR_RED);
    p.text(panel.center() + vec2(0.0, 62.0), Align2::CENTER_CENTER, "please try again..", FontId::monospace(14.0), ERR_RED);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    resp.clicked()
}

// ---- release notes: markdown + Burmese text

struct MdCtx<'a> {
    shaper: Option<&'a Shaper>,
    cache: &'a mut HashMap<String, Tex>,
    ctx: &'a egui::Context,
}

const CODE_BG: Color32 = Color32::from_rgb(160, 215, 245);
const BOLD_INK: Color32 = Color32::from_rgb(8, 36, 100);

/// Plain text of a markdown line (markers removed) - used for Burmese lines drawn as images.
fn clean_inline(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' || c == '`' || c == '~' {
            i += 1;
            continue;
        }
        if c == '_' && chars.get(i + 1) == Some(&'_') {
            i += 2;
            continue;
        }
        if c == '[' {
            if let Some(close) = chars[i..].iter().position(|&x| x == ']') {
                let j = i + close;
                if chars.get(j + 1) == Some(&'(') {
                    if let Some(pc) = chars[j..].iter().position(|&x| x == ')') {
                        out.extend(chars[i + 1..j].iter());
                        i = j + pc + 1;
                        continue;
                    }
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn push_seg(
    job: &mut egui::text::LayoutJob,
    s: &str,
    size: f32,
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    link: bool,
) {
    if s.is_empty() {
        return;
    }
    let color = if link {
        BLUE
    } else if bold {
        BOLD_INK
    } else {
        INK
    };
    let fmt = egui::TextFormat {
        font_id: FontId::monospace(size),
        color,
        italics: italic,
        background: if code { CODE_BG } else { Color32::TRANSPARENT },
        strikethrough: if strike { Stroke::new(1.5_f32, INK) } else { Stroke::NONE },
        underline: if link { Stroke::new(1.0_f32, BLUE) } else { Stroke::NONE },
        ..Default::default()
    };
    job.append(s, 0.0, fmt);
}

/// **bold**, *italic*, `code`, ~~strike~~ and [links](url) -> styled text.
fn inline_job(text: &str, size: f32, bold_all: bool) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let chars: Vec<char> = text.chars().collect();
    let (mut bold, mut italic, mut code, mut strike) = (bold_all, false, false, false);
    let mut buf = String::new();
    macro_rules! flush {
        () => {
            push_seg(&mut job, &buf, size, bold, italic, code, strike, false);
            buf.clear();
        };
    }
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if code && c != '`' {
            buf.push(c);
            i += 1;
            continue;
        }
        if (c == '*' && next == Some('*')) || (c == '_' && next == Some('_')) {
            flush!();
            bold = !bold;
            i += 2;
        } else if c == '~' && next == Some('~') {
            flush!();
            strike = !strike;
            i += 2;
        } else if c == '*' {
            flush!();
            italic = !italic;
            i += 1;
        } else if c == '`' {
            flush!();
            code = !code;
            i += 1;
        } else if c == '[' {
            let close = chars[i..].iter().position(|&x| x == ']').map(|k| i + k);
            let paren = close
                .filter(|&j| chars.get(j + 1) == Some(&'('))
                .and_then(|j| chars[j..].iter().position(|&x| x == ')').map(|k| (j, j + k)));
            if let Some((j, end)) = paren {
                flush!();
                let label: String = chars[i + 1..j].iter().collect();
                push_seg(&mut job, &label, size, bold, italic, false, strike, true);
                i = end + 1;
            } else {
                buf.push(c);
                i += 1;
            }
        } else {
            buf.push(c);
            i += 1;
        }
    }
    flush!();
    job
}

/// A Burmese line as a texture (cached by text + size).
fn mm_texture(md: &mut MdCtx, line: &str, px: f32) -> Option<(egui::TextureId, egui::Vec2)> {
    let sh = md.shaper?;
    let key = format!("{px}|{line}");
    if !md.cache.contains_key(&key) {
        let (w, h, pix) = sh.render(line, px * 2.0, [0, 0, 0])?;
        let ci = egui::ColorImage::from_rgba_unmultiplied([w, h], &pix);
        let name = format!("md{}", md.cache.len());
        let handle = md.ctx.load_texture(name, ci, egui::TextureOptions::LINEAR);
        md.cache.insert(
            key.clone(),
            Tex {
                handle,
                size: vec2(w as f32 / 2.0, h as f32 / 2.0),
            },
        );
    }
    let t = md.cache.get(&key)?;
    Some((t.handle.id(), t.size))
}

/// One paragraph / heading / list item. Burmese is shaped and drawn as images,
/// everything else is normal styled text.
fn md_block(ui: &mut egui::Ui, md: &mut MdCtx, text: &str, size: f32, bold: bool, indent: f32) {
    if contains_myanmar(text) && md.shaper.is_some() {
        let plain = clean_inline(text);
        let px = size + 1.5;
        let avail = (ui.available_width() - indent).max(80.0);
        let lines = md
            .shaper
            .map(|s| s.wrap(&plain, px * 2.0, avail * 2.0))
            .unwrap_or_default();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(indent);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for l in &lines {
                    if let Some((id, sz)) = mm_texture(md, l, px) {
                        ui.image((id, sz));
                    }
                }
            });
        });
    } else {
        let job = inline_job(text, size, bold);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(indent);
            ui.vertical(|ui| {
                ui.label(job);
            });
        });
    }
}

fn heading_of(t: &str) -> Option<(usize, &str)> {
    let n = t.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&n) && t[n..].starts_with(' ') {
        Some((n, t[n..].trim()))
    } else {
        None
    }
}

fn is_hr(t: &str) -> bool {
    let s: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3
        && (s.chars().all(|c| c == '-') || s.chars().all(|c| c == '*') || s.chars().all(|c| c == '_'))
}

fn numbered_of(t: &str) -> Option<(&str, &str)> {
    let d = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if (1..=3).contains(&d) {
        let rest = &t[d..];
        if rest.starts_with(". ") || rest.starts_with(") ") {
            return Some((&t[..d], rest[2..].trim_start()));
        }
    }
    None
}

fn render_markdown(ui: &mut egui::Ui, text: &str, md: &mut MdCtx) {
    if text.trim().is_empty() {
        ui.label(egui::RichText::new("No release notes.").monospace().size(14.0).color(INK));
        return;
    }
    ui.spacing_mut().item_spacing.y = 3.0;
    let mut in_code = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        let tt = line.trim_start();
        if tt.starts_with("```") || tt.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            ui.label(
                egui::RichText::new(line)
                    .monospace()
                    .size(12.0)
                    .color(INK)
                    .background_color(CODE_BG),
            );
            continue;
        }
        if tt.is_empty() {
            ui.add_space(5.0);
            continue;
        }
        if is_hr(tt) {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 12.0), Sense::hover());
            ui.painter()
                .hline(r.x_range(), r.center().y, Stroke::new(2.0_f32, Color32::from_rgb(90, 120, 150)));
            continue;
        }
        let lead: usize = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .map(|c| if c == '\t' { 4 } else { 1 })
            .sum();
        let level = (lead / 2).min(4);
        let indent = level as f32 * 14.0;

        if let Some((n, h)) = heading_of(tt) {
            let size = match n {
                1 => 20.0,
                2 => 17.0,
                _ => 15.0,
            };
            if n <= 2 {
                ui.add_space(4.0);
            }
            md_block(ui, md, h, size, true, 0.0);
        } else if let Some(q) = tt.strip_prefix('>') {
            let y0 = ui.cursor().min.y;
            let x0 = ui.cursor().min.x;
            md_block(ui, md, q.trim_start_matches('>').trim(), 13.0, false, 16.0 + indent);
            let y1 = ui.min_rect().max.y;
            ui.painter().rect_filled(
                Rect::from_min_max(pos2(x0 + 3.0, y0), pos2(x0 + 6.0, y1)),
                1.0,
                GREY,
            );
        } else if tt.starts_with('|') {
            if tt.chars().all(|c| "|-: \t".contains(c)) {
                continue; // table separator row
            }
            let row = tt
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim())
                .collect::<Vec<_>>()
                .join("  |  ");
            md_block(ui, md, &row, 12.5, false, indent);
        } else if let Some(rest) = tt
            .strip_prefix("- ")
            .or_else(|| tt.strip_prefix("* "))
            .or_else(|| tt.strip_prefix("+ "))
        {
            let mark = if level == 0 { "•" } else { "–" };
            md_block(ui, md, &format!("{mark} {rest}"), 13.0, false, indent);
        } else if let Some((n, rest)) = numbered_of(tt) {
            md_block(ui, md, &format!("{n}. {rest}"), 13.0, false, indent);
        } else {
            md_block(ui, md, tt, 13.0, false, 0.0);
        }
    }
}

fn markdown_view(ui: &mut egui::Ui, p: &egui::Painter, panel: Rect, body: &str, key: &str, md: &mut MdCtx) {
    let view = Rect::from_min_max(
        panel.min + vec2(16.0, 12.0),
        pos2(panel.max.x - 40.0, panel.max.y - 12.0),
    );
    let out = ui
        .allocate_new_ui(egui::UiBuilder::new().max_rect(view), |ui| {
            ui.push_id(("notes", key), |ui| {
                ui.style_mut().visuals.override_text_color = Some(INK);
                ui.style_mut().interaction.selectable_labels = false;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .show(ui, |ui| {
                        ui.set_width(view.width());
                        render_markdown(ui, body, md);
                    })
            })
            .inner
        })
        .inner;
    scroll_thumb(p, panel, &out);
}

impl eframe::App for InstallerApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ---- window size follows the page (the menu/release pages are bigger)
        let want = self.page_size();
        if (self.win_size - want).length() > 0.5 {
            self.win_size = want;
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(vec2(100.0, 100.0)));
            ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(vec2(2000.0, 2000.0)));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(want));
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(want));
            ctx.send_viewport_cmd(egui::ViewportCommand::MaxInnerSize(want));
        }

        // ---- results of background lookups
        while let Ok(m) = self.check_rx.try_recv() {
            match m {
                Check::Installed(dir, state) => {
                    if dir == self.checked_dir {
                        self.installed = Some(state);
                    }
                }
                Check::List(t, r) => {
                    self.lists[t.idx()] = match r {
                        Ok(v) => Load::Done(v),
                        Err(_) => Load::Error,
                    };
                }
                Check::Detail(t, tag, r) => {
                    if t == self.target && tag == self.sel_tag && matches!(self.page, Page::Open) {
                        match r {
                            Ok(mut rel) => {
                                if let Load::Done(list) = &self.lists[t.idx()] {
                                    rel.latest = list.iter().any(|x| x.tag == rel.tag && x.latest);
                                }
                                self.detail = Load::Done(rel);
                                self.page = Page::Detail;
                            }
                            Err(_) => self.detail = Load::Error,
                        }
                    }
                }
            }
        }

        // ---- messages from the install worker thread
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Progress(v) => self.goal = self.goal.max(v),
                    Msg::Done => {
                        self.goal = 1.0;
                        self.done = true;
                    }
                    Msg::Fail(e) => self.page = Page::Failed(e),
                }
            }
        }

        // ---- animation
        if matches!(self.page, Page::Working) {
            self.shown += (self.goal - self.shown) * 0.12;
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
                let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));

                // drag the frameless window by its background
                let drag = ui.interact(full, ui.id().with("drag"), Sense::drag());
                if drag.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                // card
                thick_box(&p, full, 26.0, CARD, 8.0);

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
                    // ------------------------------------------------------ main menu
                    Page::Menu => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(26.0, 32.0), vec2(125.0, 125.0)));
                        title(&p, o + vec2(254.0, 38.0), "Main Menu", 22.0);
                        let panel = Rect::from_min_size(o + vec2(254.0, 67.0), vec2(426.0, 297.0));
                        thick_box(&p, panel, 20.0, LIGHT, 8.0);
                        p.image(
                            self.about.handle.id(),
                            Rect::from_center_size(panel.center(), self.about.size),
                            uv,
                            Color32::WHITE,
                        );
                        if menu_button(ui, &p, Rect::from_min_size(o + vec2(30.0, 194.0), vec2(199.0, 65.0)), "Akkhara Release", 16.0) {
                            self.open_list(ctx, Target::Akkhara);
                        }
                        if menu_button(ui, &p, Rect::from_min_size(o + vec2(30.0, 271.0), vec2(199.0, 65.0)), "Installer release", 16.0) {
                            self.open_list(ctx, Target::Installer);
                        }
                    }

                    // ------------------------------------------------- release list
                    Page::List => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(18.0, 15.0), vec2(126.0, 126.0)));
                        title(&p, o + vec2(170.0, 39.0), self.target.title(), 17.0);
                        let panel = Rect::from_min_size(o + vec2(166.0, 70.0), vec2(518.0, 289.0));
                        thick_box(&p, panel, 18.0, LIGHT, 7.0);
                        if menu_button(ui, &p, Rect::from_min_size(o + vec2(21.0, 300.0), vec2(119.0, 59.0)), "BACK", 15.0) {
                            self.page = Page::Menu;
                        }

                        let t = self.target;
                        let mut clicked: Option<String> = None;
                        let mut retry = false;
                        match &self.lists[t.idx()] {
                            Load::Done(rels) => {
                                if rels.is_empty() {
                                    p.text(panel.center(), Align2::CENTER_CENTER, "No releases yet.", FontId::monospace(18.0), INK);
                                } else {
                                    clicked = release_list(ui, &p, panel, rels, t.idx());
                                }
                            }
                            Load::Error => retry = error_view(ui, &p, panel),
                            _ => loading_view(&p, panel),
                        }
                        if retry {
                            self.fetch_list(ctx, t);
                        }
                        if let Some(tag) = clicked {
                            self.open_release(ctx, tag);
                        }
                    }

                    // ----------------------------- opening one release (loading / failed)
                    Page::Open => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(18.0, 15.0), vec2(126.0, 126.0)));
                        title(&p, o + vec2(170.0, 39.0), self.target.title(), 17.0);
                        let panel = Rect::from_min_size(o + vec2(166.0, 70.0), vec2(518.0, 289.0));
                        thick_box(&p, panel, 18.0, LIGHT, 7.0);
                        if menu_button(ui, &p, Rect::from_min_size(o + vec2(21.0, 300.0), vec2(119.0, 59.0)), "BACK", 15.0) {
                            self.page = Page::List;
                        }
                        if matches!(self.detail, Load::Error) {
                            if error_view(ui, &p, panel) {
                                self.fetch_detail(ctx);
                            }
                        } else {
                            loading_view(&p, panel);
                        }
                    }

                    // ----------------------------------------------- one release page
                    Page::Detail => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(17.0, 13.0), vec2(123.0, 123.0)));
                        let rel = match &self.detail {
                            Load::Done(r) => Some(r.clone()),
                            _ => None,
                        };
                        let bar = Rect::from_min_size(o + vec2(165.0, 13.0), vec2(521.0, 62.0));
                        let panel = Rect::from_min_size(o + vec2(165.0, 88.0), vec2(521.0, 271.0));
                        thick_box(&p, panel, 18.0, LIGHT, 7.0);

                        if let Some(rel) = rel {
                            draw_release_row(&p, bar, &rel, false);
                            let mut md = MdCtx {
                                shaper: self.shaper.as_ref(),
                                cache: &mut self.md_cache,
                                ctx,
                            };
                            markdown_view(ui, &p, panel, &rel.body, &rel.tag, &mut md);

                            let label = match self.target {
                                Target::Akkhara => self.akk_label(&rel.tag),
                                Target::Installer => {
                                    if is_newer(&rel.tag, INSTALLER_VERSION) { "Update" } else { "Install" }
                                }
                            };
                            if menu_button(ui, &p, Rect::from_min_size(o + vec2(19.0, 233.0), vec2(119.0, 56.0)), label, 15.0) {
                                self.chosen = Some(rel.clone());
                                match self.target {
                                    Target::Akkhara => self.page = Page::Setup,
                                    Target::Installer => self.start_self_update(),
                                }
                            }
                        }
                        if menu_button(ui, &p, Rect::from_min_size(o + vec2(19.0, 301.0), vec2(119.0, 56.0)), "BACK", 15.0) {
                            self.page = Page::List;
                        }
                    }

                    // ----------------------------------------- install folder + PATH
                    Page::Setup => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(22.0, 20.0), vec2(167.0, 167.0)));
                        let tag = self.chosen.as_ref().map(|r| r.tag.clone()).unwrap_or_default();
                        p.text(o + vec2(105.0, 208.0), Align2::CENTER_CENTER, &tag, FontId::monospace(16.0), BLUE);
                        let bk = Rect::from_center_size(o + vec2(105.0, 240.0), vec2(80.0, 22.0));
                        let br = ui.interact(bk, ui.id().with("setup_back"), Sense::click());
                        p.text(bk.center(), Align2::CENTER_CENTER, "< Back", FontId::monospace(13.0), if br.hovered() { BLUE } else { Color32::from_rgb(20, 60, 100) });
                        if br.hovered() {
                            ctx.set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if br.clicked() {
                            self.page = Page::Detail;
                        }

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

                        let btn_label = self.akk_label(&tag);
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

                    // ------------------------------------------------------ installing
                    Page::Working => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(22.0, 17.0), vec2(168.0, 168.0)));
                        heading(
                            &p,
                            o + vec2(0.0, 53.0),
                            if self.self_updating {
                                "Updating installer..."
                            } else if self.uninstalling {
                                "Uninstalling..."
                            } else if self.updating {
                                "Updating..."
                            } else {
                                "Installing..."
                            },
                        );

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
                        p.image(t.handle.id(), rect, uv, Color32::WHITE);
                    }

                    // ------------------------------------------------------- finished
                    Page::Finished => {
                        draw_logo(&p, &self.logo, Rect::from_min_size(o + vec2(22.0, 17.0), vec2(168.0, 168.0)));
                        let title_text = if self.self_updating {
                            "Installer updated!"
                        } else if self.uninstalling {
                            "Uninstall complete!"
                        } else if self.updating {
                            "Update complete!"
                        } else {
                            "Installation complete!"
                        };
                        heading(&p, o + vec2(0.0, 48.0), title_text);

                        if self.self_updating {
                            p.text(o + vec2(207.0, 96.0), Align2::LEFT_CENTER, "Restart to use the new version.", FontId::monospace(14.0), INK);
                        } else if self.uninstalling {
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

                        if big_button(ui, &p, Rect::from_min_size(o + vec2(207.0, 208.0), vec2(407.0, 50.0)), if self.self_updating { "Restart" } else { "Finish" }) {
                            if self.self_updating {
                                if let Some(exe) = &self.exe_path {
                                    let _ = std::process::Command::new(exe).spawn();
                                }
                            }
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
                            if self.self_updating {
                                self.page = Page::Detail;
                            } else {
                                self.page = Page::Setup;
                                self.check_installed(ctx);
                            }
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
    // remove the previous installer left behind by a self-update (best effort)
    if let Ok(exe) = std::env::current_exe() {
        let _ = fs::remove_file(exe.with_extension("old"));
    }
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Akkhara Installer")
        .with_inner_size([MENU_W, MENU_H])
        .with_min_inner_size([MENU_W, MENU_H])
        .with_max_inner_size([MENU_W, MENU_H])
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(true);
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
