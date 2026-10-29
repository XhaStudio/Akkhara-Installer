# Akkhara Installer

Cross-platform GUI installer (Rust + egui) for `akk`.

What it does: finds the latest GitHub Release of XhaStudio/Akkhara-Programming-Language,
downloads the right `akk` binary for the OS/CPU, installs it per-user
(Windows: `%LOCALAPPDATA%\Akkhara\bin`, Mac/Linux: `~/.akkhara/bin`), and adds it to PATH.
It also has an Uninstall button.

## Setup (two repos)
**Installer repo** (this folder, e.g. XhaStudio/Akkhara-Installer): the repo root must contain
`Cargo.toml`, `src/main.rs`, `LICENSE`, `.gitignore` and `.github/workflows/release.yml`.
Tag a release (`git tag v0.1.0 && git push origin v0.1.0`) to publish `Akkhara-Installer-<platform>`.

**Main repo** (XhaStudio/Akkhara-Programming-Language): no changes needed. The installer downloads the
archives your existing releases already publish (`akk-x86_64-pc-windows-msvc.zip`,
`akk-x86_64-unknown-linux-gnu.tar.gz`, `akk-aarch64-apple-darwin.tar.gz`, `akk-x86_64-apple-darwin.tar.gz`)
and extracts `akk` / `akk.exe` from them.

## Local test
    cd installer && cargo run --release
(needs at least one published release with the asset names above)

## Notes
- Mac/Linux installers need `chmod +x` after download. macOS will warn (unsigned) until you sign/notarize.
- Asset names are defined in `asset_name()` in src/main.rs; keep them in sync with the main repo's release assets.
