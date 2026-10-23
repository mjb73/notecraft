//! The parts of NoteCraft that are not the same on every desktop: file choosers,
//! screen grabs, opening a folder, the backend readout, and the single-instance lock.
//!
//! Everything platform-specific lives here so the rest of the app reads as if there were
//! only one platform. Each function has one implementation per target, selected by `cfg`,
//! and every one of them degrades to a message rather than a panic: a missing helper tool
//! is a sentence in the status bar, not a crash.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The per-user configuration directory (`~/.config/notecraft`,
/// `~/Library/Application Support/notecraft`, `%APPDATA%\notecraft`).
pub fn config_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.config_dir().join("notecraft"))
}

/// Where the notes live on a fresh install: `Documents/NoteCraft` on every platform, so the
/// pages are somewhere a person will actually find them (they are plain markdown, and half the
/// point is that the folder can be backed up, synced or grep-ed). `NOTECRAFT_VAULT` overrides it.
pub fn vault_default() -> PathBuf {
    if let Ok(v) = std::env::var("NOTECRAFT_VAULT") {
        if !v.trim().is_empty() {
            return PathBuf::from(v);
        }
    }
    if let Some(docs) = directories::UserDirs::new().and_then(|u| u.document_dir().map(|d| d.to_path_buf())) {
        return docs.join("NoteCraft");
    }
    directories::BaseDirs::new()
        .map(|d| d.home_dir().join("NoteCraft"))
        .unwrap_or_else(|| PathBuf::from("NoteCraft"))
}

/// What the status bar calls the live windowing backend.
pub fn backend_label() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "Wayland"
        } else {
            "X11"
        }
    }
    #[cfg(windows)]
    {
        "Windows"
    }
    #[cfg(target_os = "macos")]
    {
        "macOS"
    }
}

/// Whether dragging files in and pictures out can work on this backend. Only X11 implements
/// XDND; winit's Wayland backend has no data-device code at all. macOS and Windows have it
/// (incoming drops; dragging *out* is Linux/X11 only, hence `can_drag_out`).
pub fn backend_drags() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("WAYLAND_DISPLAY").is_none()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

pub fn backend_hint() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            "Wayland: dragging files in is not supported by the toolkit — use Ctrl+V or Attach,\n\
             or start NoteCraft with --x11 (XWayland) for drag & drop."
        } else {
            "X11: dragging files in and pictures out works here."
        }
    }
    #[cfg(windows)]
    {
        "Windows: drop files in, or copy and press Ctrl+V."
    }
    #[cfg(target_os = "macos")]
    {
        "macOS: drop files in, or copy and press Ctrl+V."
    }
}

pub fn drag_out_hint() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "dragging out needs the X11 backend (this window is Wayland) — right-click the picture and Copy"
    }
    #[cfg(not(target_os = "linux"))]
    {
        "dragging a picture out is an X11 feature — right-click the picture and Copy instead"
    }
}

/// The keyboard layout chip talks to KDE's layout service over D-Bus, so it only exists on Linux.
pub fn has_keyboard_layout_toggle() -> bool {
    cfg!(target_os = "linux")
}

/// Open a folder or a file in whatever the desktop considers its handler.
pub fn open_path(p: &Path) -> bool {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("explorer");
        c.arg(p);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(p);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(p);
        c
    };
    cmd.spawn().map(|_| true).unwrap_or(false)
}

/// Ask for files with the platform's own chooser (`rfd`: the XDG desktop portal on Linux,
/// `IFileOpenDialog` on Windows, `NSOpenPanel` on macOS). An empty list means the user cancelled
/// or no chooser could be shown.
pub fn pick_files(images_only: bool) -> Vec<PathBuf> {
    let mut dlg = rfd::FileDialog::new();
    if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()) {
        dlg = dlg.set_directory(home);
    }
    dlg = if images_only {
        dlg.add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
            .set_title("Attach an image")
    } else {
        dlg.add_filter("All files", &["*"]).set_title("Attach files")
    };
    dlg.pick_files().unwrap_or_default()
}

/// Split the app's own `"*.png *.jpg|Images"` filter into what a native dialog wants: a label and
/// the extensions. `"*"` (every file) yields no extensions, which means no filter in the dialog.
fn parse_filter(filter: &str) -> (String, Vec<String>) {
    let (pattern, label) = match filter.split_once('|') {
        Some((p, l)) => (p.trim(), l.trim()),
        None => (filter.trim(), "Files"),
    };
    let exts = pattern
        .split([';', ' '])
        .filter_map(|p| p.trim().strip_prefix("*."))
        .map(|e| e.to_string())
        .collect();
    (label.to_string(), exts)
}

/// Ask where to save an export. `filter` is the app's own `"*.pdf|PDF files"` form, converted to a
/// native filter. `None` means cancelled.
pub fn pick_save(default_path: &Path, filter: &str) -> Option<PathBuf> {
    let (label, exts) = parse_filter(filter);
    let mut dlg = rfd::FileDialog::new();
    if let Some(dir) = default_path.parent() {
        if dir.is_dir() {
            dlg = dlg.set_directory(dir);
        }
    }
    if let Some(name) = default_path.file_name().and_then(|n| n.to_str()) {
        dlg = dlg.set_file_name(name);
    }
    dlg = if exts.is_empty() {
        dlg
    } else {
        let refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
        dlg.add_filter(label, &refs)
    };
    dlg.save_file()
}

/// A message the user has to see even when the app has no window of its own (a second launch, a
/// fatal start-up error). Blocks until dismissed.
pub fn message(level: MsgLevel, title: &str, body: &str) {
    let level = match level {
        MsgLevel::Info => rfd::MessageLevel::Info,
        MsgLevel::Error => rfd::MessageLevel::Error,
    };
    let _ = rfd::MessageDialog::new()
        .set_level(level)
        .set_title(title)
        .set_description(body)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[derive(Clone, Copy)]
pub enum MsgLevel {
    Info,
    Error,
}

/// Grab the whole screen into `path`.
///
/// The app hides its own window first (`app::NoteCraft::snip_region`) and crops the result in a
/// window overlay, so this only has to produce one full-screen PNG. Every platform has a house
/// tool for that and no two agree on flags, so each is tried in turn and the failures are
/// reported together — a missing screenshot tool should read as a sentence, not as silence.
pub fn capture_screen(path: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(path);
    // Escape hatch for anyone whose compositor needs something else:
    //   NOTECRAFT_SCREENSHOT_CMD='my-grabber --out {out}'
    if let Ok(tpl) = std::env::var("NOTECRAFT_SCREENSHOT_CMD") {
        if !tpl.trim().is_empty() {
            let cmdline = tpl.replace("{out}", &shell_quote(&path.display().to_string()));
            #[cfg(windows)]
            let status = Command::new("cmd").args(["/C", &cmdline]).status();
            #[cfg(not(windows))]
            let status = Command::new("sh").args(["-c", &cmdline]).status();
            let ok = status.map(|s| s.success()).unwrap_or(false);
            if ok && file_ready(path) {
                return Ok(());
            }
            return Err(format!("NOTECRAFT_SCREENSHOT_CMD did not write {path:?}", path = path.display()));
        }
    }
    let mut failures: Vec<String> = Vec::new();
    for (bin, args) in capture_candidates(path) {
        match Command::new(bin).args(&args).status() {
            Ok(s) if s.success() && file_ready(path) => return Ok(()),
            Ok(_) => failures.push(format!("{bin}: no image")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => failures.push(format!("{bin}: not installed")),
            Err(e) => failures.push(format!("{bin}: {e}")),
        }
        let _ = std::fs::remove_file(path);
    }
    Err(format!("could not grab the screen ({})", failures.join("; ")))
}

fn file_ready(path: &Path) -> bool {
    std::fs::metadata(path).map(|m| m.len() > 1024).unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn capture_candidates(path: &Path) -> Vec<(&'static str, Vec<std::ffi::OsString>)> {
    let out = || vec![path.as_os_str().to_os_string()];
    let mut v: Vec<(&'static str, Vec<std::ffi::OsString>)> = Vec::new();
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    if wayland {
        // wlroots compositors: grim writes every output into one image
        v.push(("grim", out()));
        v.push((
            "spectacle",
            ["-b", "-n", "-f", "-o"].iter().map(|s| s.into()).chain(out()).collect(),
        ));
        v.push(("gnome-screenshot", ["-f"].iter().map(|s| s.into()).chain(out()).collect()));
    } else {
        // X11: ImageMagick's import needs no extra packages on most boxes
        v.push((
            "import",
            ["-silent", "-window", "root"].iter().map(|s| s.into()).chain(out()).collect(),
        ));
        v.push((
            "spectacle",
            ["-b", "-n", "-f", "-o"].iter().map(|s| s.into()).chain(out()).collect(),
        ));
        v.push(("scrot", ["-o"].iter().map(|s| s.into()).chain(out()).collect()));
    }
    v
}

#[cfg(windows)]
fn capture_candidates(path: &Path) -> Vec<(&'static str, Vec<std::ffi::OsString>)> {
    // No screenshot tool is guaranteed on Windows, but PowerShell and .NET always are.
    let p = path.display().to_string().replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
         $b=[System.Windows.Forms.SystemInformation]::VirtualScreen; \
         $bmp=New-Object System.Drawing.Bitmap $b.Width,$b.Height; \
         $g=[System.Drawing.Graphics]::FromImage($bmp); \
         $g.CopyFromScreen($b.Left,$b.Top,0,0,$bmp.Size); \
         $bmp.Save('{p}',[System.Drawing.Imaging.ImageFormat]::Png); \
         $g.Dispose(); $bmp.Dispose()"
    );
    let args = [
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-WindowStyle",
        "Hidden",
        "-Command",
        &script,
    ]
    .iter()
    .map(|s| s.into())
    .collect();
    vec![("powershell", args)]
}

#[cfg(target_os = "macos")]
fn capture_candidates(path: &Path) -> Vec<(&'static str, Vec<std::ffi::OsString>)> {
    // -x: no shutter sound. The main display; a region choice happens in the app's crop overlay.
    let args = ["-x", "-t", "png"].iter().map(|s| s.into()).chain(vec![path.as_os_str().to_os_string()]).collect();
    vec![("screencapture", args)]
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
fn capture_candidates(_path: &Path) -> Vec<(&'static str, Vec<std::ffi::OsString>)> {
    Vec::new()
}

fn shell_quote(s: &str) -> String {
    if cfg!(windows) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Fetch `url` to `dest`. Used for a picture dragged out of a browser as a URL rather than bytes.
///
/// Deliberately the platform's own downloader rather than a TLS stack in-tree: this is a fallback
/// path for one kind of drag, and `curl` ships with Windows 10+, macOS and most Linux desktops.
/// The alternative (rustls + ring) adds a C build dependency to every platform for one seldom-used
/// feature, which is exactly the kind of thing that stops a notes app from building on someone
/// else's machine.
pub fn download_to_file(url: &str, dest: &Path) -> Result<u64, String> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(format!("not an http(s) URL: {url}"));
    }
    let dest_s = dest.display().to_string();
    let attempts: Vec<(&str, Vec<std::ffi::OsString>)> = download_commands(url, &dest_s);
    let mut failures: Vec<String> = Vec::new();
    for (bin, args) in attempts {
        match Command::new(bin).args(&args).output() {
            Ok(out) if out.status.success() => {
                let size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
                if size > 0 {
                    return Ok(size);
                }
                failures.push(format!("{bin}: wrote nothing"));
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                let msg = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed");
                failures.push(format!("{bin}: {msg}"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                failures.push(format!("{bin}: not installed"))
            }
            Err(e) => failures.push(format!("{bin}: {e}")),
        }
        let _ = std::fs::remove_file(dest);
    }
    Err(format!("download failed ({})", failures.join("; ")))
}

#[cfg(windows)]
fn download_commands(url: &str, dest: &str) -> Vec<(&'static str, Vec<std::ffi::OsString>)> {
    // PowerShell is present on every supported Windows; curl.exe on Windows 10 1803+.
    let ps = format!(
        "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -Uri '{}' -OutFile '{}'",
        url.replace('\'', "''"),
        dest.replace('\'', "''")
    );
    vec![
        (
            "powershell",
            ["-NoProfile", "-NonInteractive", "-Command", ps.as_str()]
                .iter()
                .map(|s| s.into())
                .collect(),
        ),
        (
            "curl",
            ["-fsSL", "--max-time", "60", "-o", dest, url]
                .iter()
                .map(|s| s.into())
                .collect(),
        ),
    ]
}

#[cfg(not(windows))]
fn download_commands(url: &str, dest: &str) -> Vec<(&'static str, Vec<std::ffi::OsString>)> {
    let wget = [
        "-q",
        "--timeout=60",
        "--tries=2",
        "-O",
        dest,
        url,
    ];
    vec![
        (
            "curl",
            ["-fsSL", "--max-time", "60", "-o", dest, url]
                .iter()
                .map(|s| s.into())
                .collect(),
        ),
        ("wget", wget.iter().map(|s| s.into()).collect()),
    ]
}

/// Take the single-instance lock for the window. `None` means another NoteCraft already holds it.
///
/// Held for as long as the file stays open: the OS releases it when the process dies, so a crash
/// never leaves a stale lock behind (a `state.json` flag would). The CLI verbs deliberately do not
/// take it — `notecraft new "…"` is meant to work while the window is open.
pub fn window_lock() -> Option<std::fs::File> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("window.lock"))
        .ok()?;
    // `File::try_lock` is std's own (Rust 1.89+): flock(2) on Unix, LockFileEx on Windows.
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_export_filter_parser_takes_the_apps_own_form() {
        // The app builds these strings itself (see `ExFmt` in app.rs); this is what the native
        // dialog gets out of them.
        assert_eq!(
            parse_filter("*.pdf|PDF files"),
            ("PDF files".to_string(), vec!["pdf".to_string()])
        );
        assert_eq!(
            parse_filter("*.png *.jpg|Images"),
            ("Images".to_string(), vec!["png".to_string(), "jpg".to_string()])
        );
        // every file: no extension filter at all
        assert_eq!(parse_filter("*|All files"), ("All files".to_string(), Vec::new()));
        // and a filter with no label must not panic or lose the extension
        assert_eq!(parse_filter("*.txt"), ("Files".to_string(), vec!["txt".to_string()]));
    }

    #[test]
    fn the_default_vault_is_a_folder_the_user_can_find() {
        let v = vault_default();
        assert!(v.is_absolute() || std::env::var("NOTECRAFT_VAULT").is_ok());
        assert!(v.file_name().is_some());
    }

    #[test]
    fn the_backend_label_is_never_empty() {
        assert!(!backend_label().is_empty());
        assert!(!backend_hint().is_empty());
    }
}
