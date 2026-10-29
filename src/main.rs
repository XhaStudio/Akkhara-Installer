// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{channel, Receiver, Sender},
    thread,
};

const REPO: &str = "XhaStudio/Akkhara-Programming-Language";

enum Msg {
    Log(String),
    Done(bool),
}

// ---------------------------------------------------------------- platform

/// Name of the release asset to download for this OS/CPU.
/// Must match the names produced by .github/workflows/release.yml.
fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("akk-windows-x86_64.exe"),
        ("linux", "x86_64") => Some("akk-linux-x86_64"),
        ("macos", "aarch64") => Some("akk-macos-aarch64"),
        ("macos", "x86_64") => Some("akk-macos-x86_64"),
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

fn install(dir: PathBuf, add_path: bool, tx: &Sender<Msg>) -> Result<(), String> {
    let log = |s: &str| {
        let _ = tx.send(Msg::Log(s.to_string()));
    };

    let asset = asset_name().ok_or("This platform is not supported yet.")?;

    log("Looking up the latest release...");
    let release: serde_json::Value =
        ureq::get(&format!("https://api.github.com/repos/{REPO}/releases/latest"))
            .set("User-Agent", "akkhara-installer")
            .set("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| format!("Could not reach GitHub (is there a published release?): {e}"))?
            .into_json()
            .map_err(|e| e.to_string())?;

    let tag = release["tag_name"].as_str().unwrap_or("unknown").to_string();
    let url = release["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"] == asset))
        .and_then(|x| x["browser_download_url"].as_str())
        .ok_or_else(|| format!("Release {tag} has no asset named {asset}"))?
        .to_string();

    log(&format!("Downloading akk {tag} ({asset})..."));
    let mut buf = Vec::new();
    ureq::get(&url)
        .set("User-Agent", "akkhara-installer")
        .call()
        .map_err(|e| format!("Download failed: {e}"))?
        .into_reader()
        .take(300_000_000)
        .read_to_end(&mut buf)
        .map_err(|e| format!("Download failed: {e}"))?;
    if buf.len() < 1024 {
        return Err("Downloaded file looks too small; aborting.".into());
    }

    fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let target = dir.join(bin_name());
    let tmp = dir.join(format!("{}.new", bin_name()));
    fs::write(&tmp, &buf).map_err(|e| format!("Could not write file: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, &target).map_err(|e| {
        format!("Could not replace {} (is akk running?): {e}", target.display())
    })?;
    log(&format!("Installed to {}", target.display()));

    if add_path {
        match add_to_path(&dir)? {
            true => log("Added to PATH. Open a NEW terminal to use `akk`."),
            false => log("Already on PATH."),
        }
    } else {
        log("Skipped PATH setup.");
    }
    log("Done! Try: akk myprogram.akk");
    Ok(())
}

fn uninstall(dir: PathBuf, tx: &Sender<Msg>) -> Result<(), String> {
    let log = |s: &str| {
        let _ = tx.send(Msg::Log(s.to_string()));
    };
    let target = dir.join(bin_name());
    if target.exists() {
        fs::remove_file(&target)
            .map_err(|e| format!("Could not remove {}: {e}", target.display()))?;
        log(&format!("Removed {}", target.display()));
    } else {
        log("akk was not found in that folder.");
    }
    remove_from_path(&dir)?;
    log("Removed PATH entry.");
    let _ = fs::remove_dir(&dir);
    if let Some(parent) = dir.parent() {
        let name = parent.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "Akkhara" || name == ".akkhara" {
            let _ = fs::remove_dir(parent); // only succeeds if empty
        }
    }
    log("Uninstalled.");
    Ok(())
}

// ---------------------------------------------------------------------- UI

struct InstallerApp {
    dir: String,
    add_path: bool,
    log: Vec<String>,
    busy: bool,
    rx: Option<Receiver<Msg>>,
}

impl Default for InstallerApp {
    fn default() -> Self {
        Self {
            dir: default_dir().to_string_lossy().to_string(),
            add_path: true,
            log: vec!["Ready.".into()],
            busy: false,
            rx: None,
        }
    }
}

impl InstallerApp {
    fn start(&mut self, remove: bool) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.busy = true;
        self.log.clear();
        let dir = PathBuf::from(self.dir.trim());
        let add_path = self.add_path;
        thread::spawn(move || {
            let result = if remove {
                uninstall(dir, &tx)
            } else {
                install(dir, add_path, &tx)
            };
            match result {
                Ok(()) => {
                    let _ = tx.send(Msg::Done(true));
                }
                Err(e) => {
                    let _ = tx.send(Msg::Log(format!("Error: {e}")));
                    let _ = tx.send(Msg::Done(false));
                }
            }
        });
    }
}

impl eframe::App for InstallerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.rx {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Log(s) => self.log.push(s),
                    Msg::Done(_) => self.busy = false,
                }
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Akkhara (အက္ခရာ) Installer");
            ui.label("Installs the `akk` interpreter for your user account. No admin rights needed.");
            ui.add_space(10.0);

            ui.label("Install folder");
            ui.add_enabled(!self.busy, egui::TextEdit::singleline(&mut self.dir).desired_width(f32::INFINITY));
            ui.add_enabled(
                !self.busy,
                egui::Checkbox::new(&mut self.add_path, "Add akk to PATH"),
            );
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy, egui::Button::new("Install / Update")).clicked() {
                    self.start(false);
                }
                if ui.add_enabled(!self.busy, egui::Button::new("Uninstall")).clicked() {
                    self.start(true);
                }
                if self.busy {
                    ui.spinner();
                }
            });
            ui.add_space(8.0);

            egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                for line in &self.log {
                    ui.monospace(line);
                }
            });
        });

        if self.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 400.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "Akkhara Installer",
        options,
        Box::new(|_cc| Ok(Box::new(InstallerApp::default()))),
    )
}