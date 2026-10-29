# Akkhara Installer

Cross-platform GUI installer (Rust + egui) for `akk`.

What it does: finds the latest GitHub Release of XhaStudio/Akkhara-Programming-Language,
downloads the right `akk` binary for the OS/CPU, installs it per-user
(Windows: `%LOCALAPPDATA%\Akkhara\bin`, Mac/Linux: `~/.akkhara/bin`), and adds it to PATH.
It also has an Uninstall button.

## Setup
1. Copy this folder into the Akkhara repo as `installer/`.
2. Put `.github/workflows/release.yml` into the repo's `.github/workflows/`.
3. Tag a release: `git tag v1.0.0 && git push origin v1.0.0`.
4. Releases will contain `akk-<platform>` binaries and `Akkhara-Installer-<platform>` installers.

## Local test
    cd installer && cargo run --release
(needs at least one published release with the asset names above)

## Notes
- Mac/Linux installers need `chmod +x` after download. macOS will warn (unsigned) until you sign/notarize.
- Asset names are defined in `asset_name()` in src/main.rs; keep them in sync with release.yml.
