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

## UI assets
- `assets/logo.png` is a **placeholder cropped from the design screenshot**. Replace it with your real (A + က) logo
  (square PNG, ~450x450 works well); keep the file name.
- `assets/msg1.png` ... `msg4.png` are the rotating messages on the installing page. egui cannot shape Burmese,
  so these were pre-rendered with Noto Sans Myanmar + DejaVu Sans Mono. To change a message, re-render the PNG.

## Screens
Main Menu -> "Akkhara Release" / "Installer release" -> release list (rows: version, age, size, Latest / pre-release)
-> click a row (Loading...) -> release page (notes + Install / Update + BACK).
- Akkhara release: Install -> install folder + PATH page -> install that exact version.
- Installer release: Install replaces this installer with that version, then Restart.
- Failed loads show the retry icon; click it to try again.
`assets/about.png` is the pre-rendered Burmese paragraph on the Main Menu.

## Release notes (markdown + Burmese)
Release notes are rendered by a small built-in markdown renderer: headings, **bold**, *italic*, `code`,
~~strike~~, links, bullet / numbered / nested lists, > quotes, --- rules, tables (as rows) and code blocks.
egui cannot shape Myanmar script, so any line containing Burmese is shaped with rustybuzz (`src/mmtext.rs`) and
drawn as an image. Bundled fonts (in `assets/`):
- Noto Sans Myanmar - SIL Open Font License 1.1 (see `assets/OFL.txt`)
- DejaVu Sans Mono - free license (Bitstream Vera / DejaVu), used for Latin text inside Burmese lines
