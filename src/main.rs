//! NoteCraft — notes, todos and idea capture.
//!
//! `notecraft`            open the notes window
//! `notecraft new TEXT`   append to today's Inbox page, print the path
//! `notecraft tasks`      list open todos (scriptable)
//! `notecraft search Q`   find pages matching Q
//! `notecraft path`       print the vault root
//!
//! Built for Linux, macOS and Windows; the only platform-specific code is in `platform.rs`.

// A windowed executable on Windows: double-clicking it must not leave a console window behind.
// The CLI verbs still print, because `main` attaches to the terminal that started it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod attach;
mod doc;
#[cfg(all(unix, not(target_os = "macos")))]
mod dragout;
mod export;
mod highlight;
mod imgcache;
mod keylayout;
mod platform;
mod spell;
mod theme;
mod vault;

use std::path::PathBuf;

/// Minimal stderr logger: egui's image loaders report failures through `log`,
/// and without a logger installed those messages vanish silently (which is how
/// a blank image looked like a crash).
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        // Only our own messages plus warnings/errors from dependencies. Without
        // this filter, winit/eframe emit per-frame TRACE lines, which floods the
        // log file with hundreds of thousands of lines and slows the UI thread.
        meta.level() <= log::Level::Warn
            || meta.target().starts_with("notecraft")
            // winit's DnD diagnostics (drag type lists, transfer results) — rare,
            // and the only way to tell a rejected drag from one that never arrived.
            // Capped at info: winit also logs per-frame DEBUG (cursor visibility,
            // IME area) which floods the log from the UI thread.
            || (meta.target().starts_with("winit") && meta.level() <= log::Level::Info)
    }
    fn log(&self, record: &log::Record) {
        eprintln!("[{} {}] {}", record.level(), record.target(), record.args());
    }
    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

/// On Windows the binary is a windowed app, so a CLI verb started from `cmd.exe` has no console of
/// its own. Attaching to the parent's console makes `notecraft tasks` print where the user typed
/// it, while a double-click still opens no console at all (there is no parent to attach to).
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: called once, first thing in `main`, before any thread exists and before std's
    // stdout/stderr handles are created (they are initialised lazily on first use).
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// winit 0.30 removed `WINIT_UNIX_BACKEND` and picks the backend purely from the
/// environment: with `WAYLAND_DISPLAY` set it uses Wayland, otherwise X11. Only
/// the X11 backend implements drag-and-drop (winit's Wayland backend has no
/// data-device support at all), so NoteCraft prefers X11 **when an X server is
/// actually reachable** (XWayland under a Wayland session) and uses the native
/// backend otherwise — a Wayland-only box must not be told to open `:0`.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Debug)]
enum Backend {
    X11,
    Wayland,
}

fn backend_choice() -> Backend {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--wayland") {
        return Backend::Wayland;
    }
    if args.iter().any(|a| a == "--x11") {
        return Backend::X11;
    }
    match std::env::var("NOTECRAFT_BACKEND")
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_default()
        .as_str()
    {
        "wayland" => Backend::Wayland,
        "x11" => Backend::X11,
        _ => {
            #[cfg(target_os = "linux")]
            {
                let has_x11 = std::env::var("DISPLAY")
                    .map(|d| !d.trim().is_empty())
                    .unwrap_or(false);
                if has_x11 { Backend::X11 } else { Backend::Wayland }
            }
            #[cfg(not(target_os = "linux"))]
            {
                Backend::X11
            }
        }
    }
}

/// `--software` / `NOTECRAFT_SOFTWARE=1`: render with a software rasteriser (llvmpipe on Linux,
/// WARP on Windows). Slower, but immune to GPU-side start-up failures on a machine that is nearly
/// out of VRAM.
fn software_requested() -> bool {
    std::env::args().any(|a| a == "--software")
        || std::env::var("NOTECRAFT_SOFTWARE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

fn apply_backend(choice: Backend) {
    #[cfg(not(target_os = "linux"))]
    {
        // Only X11 and Wayland are distinguished; Windows and macOS have one backend each.
        let _ = choice;
    }
    #[cfg(target_os = "linux")]
    {
        match choice {
            Backend::X11 => {
                // SAFETY: called before any thread is created, so no other thread can
                // be reading the environment concurrently.
                unsafe {
                    std::env::remove_var("WAYLAND_DISPLAY");
                    std::env::remove_var("WAYLAND_SOCKET");
                }
                eprintln!("notecraft: X11/XWayland backend (drag & drop works here)");
            }
            Backend::Wayland => {
                eprintln!("notecraft: Wayland backend — winit has no drag & drop support on Wayland");
            }
        }
        // Publish the live backend. /proc/<pid>/environ cannot be used for this: it
        // shows the exec-time environment, so removing WAYLAND_DISPLAY in-process
        // leaves it still reporting Wayland — a launcher wrapper reads this file.
        if let Some(dir) = platform::config_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let name = match choice {
                Backend::X11 => "x11",
                Backend::Wayland => "wayland",
            };
            let _ = std::fs::write(dir.join("backend"), name);
        }
    }
}

fn vault_root() -> PathBuf {
    platform::vault_default()
}

/// The window manager icon (taskbar, window switcher). The Linux desktop entry carries its own;
/// Windows and macOS have none without this.
fn app_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../assets/icon-256.png");
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    Some(egui::IconData {
        rgba: img.as_raw().clone(),
        width: img.width(),
        height: img.height(),
    })
}

fn main() -> eframe::Result<()> {
    attach_parent_console();
    let _ = log::set_logger(&LOGGER);
    // Debug so our own messages get through; dependencies are filtered in
    // StderrLogger::enabled down to warnings and errors.
    log::set_max_level(log::LevelFilter::Debug);
    // Only the flags *before* the verb are options. Filtering the whole vector meant
    // `notecraft search --software` searched for nothing (and switched on llvmpipe), and any note
    // text containing a flag-like word lost it.
    let mut flags_done = false;
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| {
            if flags_done {
                return true;
            }
            match a.as_str() {
                "--x11" | "--wayland" | "--software" => false,
                _ => {
                    flags_done = true;
                    true
                }
            }
        })
        .collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("gui");
    let root = vault_root();

    match cmd {
        "new" | "add" => {
            let text = args[1..].join(" ");
            if text.trim().is_empty() {
                eprintln!("usage: notecraft new \"something to remember\"");
                std::process::exit(2);
            }
            match vault::Ops::capture(&root, &text) {
                Ok(path) => println!("{}", path.display()),
                Err(e) => {
                    eprintln!("notecraft: {e}");
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        "tasks" => {
            let v = vault::Vault::load(&root);
            let (mut n, mut done) = (0usize, 0usize);
            for t in v.all_todos() {
                if t.done {
                    done += 1;
                    continue;
                }
                n += 1;
                let due = t
                    .due
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "          ".into());
                println!("{due}  {}  [{}]", t.text, t.page_title);
            }
            eprintln!("{n} open · {done} done · vault {}", root.display());
            Ok(())
        }
        "search" => {
            let q = args[1..].join(" ");
            if q.trim().is_empty() {
                // An empty query "found nothing" and exited 0, which scripts cannot tell apart
                // from a search that ran.
                eprintln!("usage: notecraft search \"words to find\"");
                std::process::exit(2);
            }
            let v = vault::Vault::load(&root);
            let (hits, total) = v.search(&q, 40);
            for h in &hits {
                let p = &v.pages[h.page];
                println!("{} / {} :: {}", p.notebook, p.section, p.title);
                if !h.snippet.is_empty() {
                    println!("    {}", h.snippet.replace('\n', " "));
                }
            }
            if total > hits.len() {
                eprintln!("{} of {total} pages · vault {}", hits.len(), root.display());
            } else {
                eprintln!("{total} pages · vault {}", root.display());
            }
            Ok(())
        }
        "docs-migrate" => {
            let report = doc::migrate_vault(&root);
            println!(
                "converted {} page(s) to .{} documents ({} already had one)",
                report.converted,
                doc::EXT,
                report.already
            );
            for p in &report.refused {
                println!("  refused (the round trip lost something): {}", p.display());
            }
            for p in &report.orphans {
                println!(
                    "  orphaned document (its page is gone, safe to delete): {}",
                    p.display()
                );
            }
            println!("the .md files were left in place as the backup");
            Ok(())
        }
        "path" => {
            println!("{}", root.display());
            Ok(())
        }
        "version" | "--version" | "-V" => {
            println!("notecraft {}", env!("CARGO_PKG_VERSION"));
            println!("vault: {}", root.display());
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!(
                "notecraft            open notes\n\
                 notecraft new TEXT   append to today's Inbox\n\
                 notecraft tasks      list open todos\n\
                 notecraft search Q   search pages\n\
                 notecraft path       print vault root ({})\n\
                 notecraft --version  print the version\n\
                 --software           render on the CPU (no GPU needed)\n\
                 --x11 / --wayland    choose the windowing backend (Linux only)",
                root.display()
            );
            Ok(())
        }
        // An unknown verb is a mistake, not a request for the window: `notecraft capture` used to be a
        // verb, and a stale desktop entry or script must fail loudly instead of starting a second app.
        other if other != "gui" => {
            eprintln!("notecraft: unknown command {other:?} — try `notecraft help`");
            std::process::exit(2);
        }
        _ => run_gui(root),
    }
}

fn run_gui(root: PathBuf) -> eframe::Result<()> {
    // Backend choice and software rendering belong to the window and nowhere else: a CLI verb like
    // `notecraft tasks` must print the todos, not which windowing system it would have used.
    apply_backend(backend_choice());
    if software_requested() {
        // Safe: still single-threaded, before eframe initialises the GL context.
        unsafe { std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1") };
        eprintln!("notecraft: software rendering");
    }
    // One window per vault. Two would be two writers racing over the same pages, and the second
    // launch would simply look like nothing happened. The lock is released by the OS on exit.
    let Some(_lock) = platform::window_lock() else {
        let msg = format!(
            "NoteCraft is already open.\n\nIts notes are in {}",
            root.display()
        );
        eprintln!("notecraft: already running (vault {})", root.display());
        platform::message(platform::MsgLevel::Info, "NoteCraft", &msg);
        return Ok(());
    };
    // A consumer install starts with no folder at all. If the folder cannot be made (a read-only
    // home, a vault path on an unmounted drive) that is worth a dialog, not a blank window.
    if let Err(e) = std::fs::create_dir_all(&root) {
        let msg = format!(
            "NoteCraft cannot use its notes folder:\n\n{}\n\n{e}\n\n\
             Set NOTECRAFT_VAULT to a folder you can write to.",
            root.display()
        );
        eprintln!("notecraft: {msg}");
        platform::message(platform::MsgLevel::Error, "NoteCraft", &msg);
        return Ok(());
    }
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("NoteCraft")
        .with_app_id("notecraft")
        .with_inner_size([1300.0, 840.0])
        .with_min_inner_size([940.0, 600.0]);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        "NoteCraft",
        options,
        Box::new(move |cc| Ok(Box::new(app::NoteCraft::new(cc, root)))),
    )
}
