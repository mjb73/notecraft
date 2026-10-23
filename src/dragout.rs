//! Dragging a picture **out** of NoteCraft: the XDND drag-source half, which winit does not have.
//!
//! winit implements only the destination side of XDND (its `x11/dnd.rs` answers `XdndEnter` and
//! turns a drop into `WindowEvent::DroppedFile`), so a picture could be dropped in and never dragged
//! out. This module is the other half, spoken by hand on its own X connection:
//!
//! 1. a 1×1 never-mapped window of ours owns the `XdndSelection` selection,
//! 2. the pointer is **polled** (`XQueryPointer`) rather than grabbed, because the press that started
//!    the drag is already an implicit grab held by winit: an explicit `XGrabPointer` from another
//!    client fails with `AlreadyGrabbed`. Polling also keeps winit's own view of the drag intact —
//!    it holds that grab, so it still sees the motion and the release, and egui is never left
//!    believing a button is still down,
//! 3. `XdndEnter` / `XdndPosition` / `XdndDrop` are sent at whatever app is under the pointer,
//! 4. the picture is served as `text/uri-list` (the path — what Dolphin, GIMP, Krita and browsers
//!    all accept) plus, for a small file, its own `image/png`/`image/jpeg` bytes, and `text/plain`.
//!
//! Three things cost real time to work out, so they are worth keeping:
//!
//! - **The selection must be owned by a window on this connection.** X delivers `SelectionRequest`
//!   to the client that *created* the owner window, so owning `XdndSelection` on NoteCraft's own
//!   winit window would send every data request into winit, which quietly drops it — and the target
//!   then reports "the source sent nothing".
//! - **The press is an implicit grab, held by winit.** `XGrabPointer` then fails with
//!   `AlreadyGrabbed` — verified on this box — so the source must not grab at all: it reads the
//!   pointer's position and button mask with `XQueryPointer`, which needs no grab. Grabbing would
//!   also have taken the release away from winit and left egui believing a button was still held.
//! - **`XdndDrop` and `SetSelectionOwner` both carry a timestamp**, and X has no "what time is it"
//!   request — hence the append-a-property trick in `server_time`.
//!
//! X11/XWayland only, by nature: under a native Wayland window there is no X11 window to drag from,
//! so `start` is never called (the app says why). NoteCraft defaults to the X11 backend anyway.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConnectionExt as _,
    CreateWindowAux, EventMask, KeyButMask, PropMode, SelectionNotifyEvent, SelectionRequestEvent,
    Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

x11rb::atom_manager! {
    pub Atoms: AtomsCookie {
        XdndSelection,
        XdndAware,
        XdndProxy,
        XdndEnter,
        XdndPosition,
        XdndStatus,
        XdndLeave,
        XdndDrop,
        XdndFinished,
        XdndActionCopy,
        Targets: b"TARGETS",
        UriList: b"text/uri-list",
        TextPlain: b"text/plain",
        Utf8String: b"UTF8_STRING",
        ImagePng: b"image/png",
        ImageJpeg: b"image/jpeg",
        TimeProbe: b"notecraft_drag_time",
        // ICCCM `TIMESTAMP`: a target may ask when the selection was taken before asking for data,
        // and refusing it can make it abort the transfer.
        Timestamp: b"TIMESTAMP",
    }
}

/// How long a target is given to fetch the picture after the mouse is released. GTK apps ask within
/// milliseconds; a browser writing the file to disk still fits easily.
const SERVE_TIMEOUT: Duration = Duration::from_secs(6);

/// The largest file handed over as image *bytes*; bigger pictures are offered by path only.
/// `ChangeProperty` has to carry the whole payload in one request, and every app that wants the
/// pixels can open the path instead.
const MAX_INLINE_IMAGE: u64 = 1 << 20;

/// Start dragging `file` out of the app. Returns the receiver of status lines for the status bar, or
/// `None` when the thread could not be started. `app_window` is NoteCraft's own X11 window, so a drag
/// that ends over NoteCraft can be recognised as a cancel instead of importing the picture again.
pub fn start(file: PathBuf, app_window: Option<u32>) -> Option<Receiver<String>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("notecraft-dragout".into())
        .spawn(move || {
            let msg = match drag(&file, app_window, &tx) {
                Ok(m) => m,
                Err(e) => format!("drag out failed: {e}"),
            };
            let _ = tx.send(msg);
        })
        .ok()?;
    Some(rx)
}

/// The image formats a file may be offered as, decided by its **magic bytes**: an `.webp` or a
/// mislabelled `.png` must never be advertised as PNG.
#[derive(Clone, Copy, PartialEq)]
enum ImageKind {
    Png,
    Jpeg,
}

fn sniff_image(file: &Path) -> Option<ImageKind> {
    let mut head = [0u8; 8];
    let n = read_head(file, &mut head)?;
    let head = &head[..n];
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageKind::Png)
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageKind::Jpeg)
    } else {
        None
    }
}

fn read_head(file: &Path, buf: &mut [u8]) -> Option<usize> {
    use std::io::Read;
    let mut f = std::fs::File::open(file).ok()?;
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return None,
        }
    }
    Some(filled)
}

/// Every type this drag can serve, in the order a target is offered them. Three at most: that is
/// what `XdndEnter` can carry inline, so no `XdndTypeList` property is ever needed.
fn serveable_types(file: &Path, atoms: &Atoms) -> Vec<Atom> {
    // TIMESTAMP is not payload, but a target that asks for it and is refused can abort; ICCCM
    // lists it among the types a selection owner offers
    let mut types = vec![atoms.Timestamp, atoms.UriList, atoms.TextPlain];
    let small = std::fs::metadata(file)
        .map(|m| m.len() <= MAX_INLINE_IMAGE)
        .unwrap_or(false);
    if small {
        match sniff_image(file) {
            Some(ImageKind::Png) => types.push(atoms.ImagePng),
            Some(ImageKind::Jpeg) => types.push(atoms.ImageJpeg),
            None => {}
        }
    }
    types
}

/// The `text/uri-list` body for one file: `file:///path\r\n`, everything that is not an unreserved
/// URI character percent-encoded, so a space or an `é` in an asset name survives the trip.
fn uri_list(file: &Path) -> String {
    format!("file://{}\r\n", uri_path(&file.display().to_string()))
}

fn uri_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for &b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------- the drag itself

fn drag(file: &Path, app_window: Option<u32>, tx: &Sender<String>) -> Result<String, String> {
    let (conn, screen_num) = x11rb::connect(None).map_err(|e| format!("no X display ({e})"))?;
    let atoms = Atoms::new(&conn)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    let screen = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or("the X server reports no screen")?
        .clone();
    let root = screen.root;

    // our own window: it owns the selection and holds properties, and is never mapped
    let src = conn.generate_id().map_err(|e| e.to_string())?;
    conn.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        src,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new().override_redirect(1),
    )
    .map_err(|e| e.to_string())?;
    conn.change_window_attributes(
        src,
        &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )
    .map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())?;

    let types = serveable_types(file, &atoms);
    let mut pending: VecDeque<Event> = VecDeque::new();
    let ts = server_time(&conn, src, atoms.TimeProbe, &mut pending)?;
    conn.set_selection_owner(src, atoms.XdndSelection, ts)
        .map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())?;
    let owner = conn
        .get_selection_owner(atoms.XdndSelection)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    if owner.owner != src {
        cleanup(&conn, src, &atoms, ts);
        return Err("another drag is already running".into());
    }

    // No grab: the press that started the drag already gave winit an implicit grab, and a second
    // client cannot take the pointer (`XGrabPointer` answers AlreadyGrabbed). The pointer's position
    // and button mask are read instead, which needs no grab and leaves winit's drag untouched.
    let mut current: Option<Window> = None;
    // What the target under the pointer last said about taking the picture. An `XdndStatus` with
    // the accept bit clear means "not here": sending `XdndDrop` regardless told the user the
    // picture was handed over when the target had refused it.
    let mut accepted = true;
    let mut status_seen = false;
    let mut last_position = Instant::now() - Duration::from_secs(1);
    let mut dropped: Option<Window> = None;
    let mut serve_deadline: Option<Instant> = None;
    let mut handed = false;
    let give_up = Instant::now() + Duration::from_secs(120);
    // the server's timestamp, refreshed lazily: every `XdndPosition` carries one, but it only has to
    // be recent for a target to consider the drag alive
    let mut stamp = ts;
    let mut stamp_at = Instant::now() - Duration::from_secs(1);

    loop {
        if Instant::now() >= give_up {
            break;
        }
        if serve_deadline.map(|d| Instant::now() >= d).unwrap_or(false) {
            break;
        }

        // The target may ask for the picture at any time: winit converts the selection on every
        // `XdndPosition`, long before the drop. Requests must therefore be served while the drag is
        // still moving — only answering them after the button comes up loses them all.
        let mut taken_away = false;
        let mut finished = false;
        loop {
            let ev = match pending.pop_front() {
                Some(ev) => ev,
                None => match conn.poll_for_event().map_err(|e| e.to_string())? {
                    Some(ev) => ev,
                    None => break,
                },
            };
            match ev {
                Event::SelectionRequest(req) => {
                    serve_one(&conn, &atoms, file, &types, &req)?;
                    handed = true;
                    let _ = conn.flush();
                }
                Event::ClientMessage(m) => {
                    if m.type_ == atoms.XdndFinished {
                        finished = true;
                    } else if m.type_ == atoms.XdndStatus {
                        // data32: [0] the target window, [1] flags (bit 0 = will accept)
                        let d = m.data.as_data32();
                        if Some(d[0]) == current {
                            status_seen = true;
                            accepted = (d[1] & 1) == 1;
                        }
                    }
                }
                Event::SelectionClear(e) if e.selection == atoms.XdndSelection => taken_away = true,
                _ => {}
            }
        }
        if taken_away || finished {
            break;
        }

        if dropped.is_some() {
            // the drop is out; all that is left is the transfer itself
            std::thread::sleep(Duration::from_millis(4));
            continue;
        }

        let Some((under, held)) = pointer_state(&conn, root) else {
            break;
        };
        let target = under
            .filter(|w| !is_own(&conn, *w, app_window))
            .and_then(|w| xdnd_target(&conn, &atoms, w).ok().flatten());
        if target != current {
            if let Some(old) = current {
                crate::app::dnd_log(&format!("drag out: leave {old}"));
                let _ = send_client(&conn, old, atoms.XdndLeave, [src, 0, 0, 0, 0]);
            }
            if let Some(new) = target {
                let mut data = [src, 5u32 << 24, 0, 0, 0];
                for (i, t) in types.iter().take(3).enumerate() {
                    data[2 + i] = *t;
                }
                crate::app::dnd_log(&format!("drag out: enter {new}"));
                let _ = send_client(&conn, new, atoms.XdndEnter, data);
                // naming the app is what tells the user the drag is live and which window will take
                // the picture
                if let Some(name) = app_name(&conn, new) {
                    let _ = tx.send(format!("release to give the picture to {name}"));
                }
            }
            current = target;
            last_position = Instant::now() - Duration::from_secs(1);
        }
        if let Some(t) = current {
            // the pointer can report at 100 Hz; a target only needs to know where we are
            if last_position.elapsed() >= Duration::from_millis(20) {
                if let Some((x, y)) = pointer_xy(&conn, root) {
                    let packed = ((x as u16 as u32) << 16) | (y as u16 as u32);
                    if stamp_at.elapsed() >= Duration::from_millis(200) {
                        stamp = server_time(&conn, src, atoms.TimeProbe, &mut pending)?;
                        stamp_at = Instant::now();
                    }
                    let _ = send_client(
                        &conn,
                        t,
                        atoms.XdndPosition,
                        [src, 0, packed, stamp, atoms.XdndActionCopy],
                    );
                }
                last_position = Instant::now();
            }
        }

        if !held {
            match current {
                // A target that answered "no" does not get a drop. One that never answered at all
                // is still offered it: some clients ignore XdndStatus, and refusing them would
                // break drags that used to work.
                Some(t) if status_seen && !accepted => {
                    crate::app::dnd_log(&format!("drag out: {t} will not take it — cancelled"));
                    let _ = tx.send("that app will not take the picture".to_string());
                    break;
                }
                Some(t) => {
                    // the drop's timestamp has to be the one of the release, so it is fetched fresh
                    let drop_ts = server_time(&conn, src, atoms.TimeProbe, &mut pending)?;
                    crate::app::dnd_log(&format!("drag out: drop on {t}"));
                    let _ = send_client(&conn, t, atoms.XdndDrop, [src, 0, drop_ts, 0, 0]);
                    dropped = Some(t);
                    serve_deadline = Some(Instant::now() + SERVE_TIMEOUT);
                }
                None => break, // let go over nothing that takes a picture
            }
        }
        std::thread::sleep(Duration::from_millis(12));
    }

    // the target fetches the picture now; the selection stays owned until it says it is done
    let outcome = if dropped.is_none() {
        "drag cancelled".to_string()
    } else if handed {
        "picture handed over".to_string()
    } else {
        "the app asked for nothing".to_string()
    };

    cleanup(&conn, src, &atoms, ts);
    Ok(outcome)
}

/// Drop the selection and destroy the window. Safe on every path out of `drag`.
fn cleanup(conn: &RustConnection, src: Window, atoms: &Atoms, ts: u32) {
    let _ = conn.set_selection_owner(x11rb::NONE, atoms.XdndSelection, ts);
    let _ = conn.destroy_window(src);
    let _ = conn.flush();
}

// ---------------------------------------------------------------- serving the data

/// Hand one type to the requesting app: its property is filled in, then it is told where to look.
fn serve_one(
    conn: &RustConnection,
    atoms: &Atoms,
    file: &Path,
    types: &[Atom],
    req: &SelectionRequestEvent,
) -> Result<(), String> {
    // an obsolete client leaves `property` empty and expects the target atom back
    let prop = if req.property == x11rb::NONE {
        req.target
    } else {
        req.property
    };
    let ok = if req.target == atoms.Targets {
        conn.change_property32(PropMode::REPLACE, req.requestor, prop, AtomEnum::ATOM, types)
            .is_ok()
    } else if req.target == atoms.Timestamp {
        // the moment the selection was taken, as ICCCM specifies
        conn.change_property32(PropMode::REPLACE, req.requestor, prop, AtomEnum::INTEGER, &[req.time])
            .is_ok()
    } else if req.target == atoms.UriList {
        conn.change_property8(
            PropMode::REPLACE,
            req.requestor,
            prop,
            req.target,
            uri_list(file).as_bytes(),
        )
        .is_ok()
    } else if req.target == atoms.TextPlain || req.target == atoms.Utf8String {
        conn.change_property8(
            PropMode::REPLACE,
            req.requestor,
            prop,
            req.target,
            file.display().to_string().as_bytes(),
        )
        .is_ok()
    } else if (req.target == atoms.ImagePng || req.target == atoms.ImageJpeg)
        && types.contains(&req.target)
    {
        match std::fs::read(file) {
            Ok(bytes) => conn
                .change_property8(PropMode::REPLACE, req.requestor, prop, req.target, &bytes)
                .is_ok(),
            Err(_) => false,
        }
    } else {
        false
    };
    let notify = SelectionNotifyEvent {
        response_type: x11rb::protocol::xproto::SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: req.time,
        requestor: req.requestor,
        selection: req.selection,
        target: req.target,
        property: if ok { prop } else { x11rb::NONE },
    };
    conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify)
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------- X plumbing

/// The server's current time. X has no request for it, so a property is written — the server stamps
/// that change with `PropertyNotify.time` — and the stamp is read back here. `REPLACE` rather than
/// `APPEND` so the property does not grow with every refresh.
///
/// Anything else that arrives meanwhile goes back on `pending` instead of being dropped: a
/// `SelectionRequest` lost here would be a target that never got its picture.
fn server_time(
    conn: &RustConnection,
    win: Window,
    prop: Atom,
    pending: &mut VecDeque<Event>,
) -> Result<u32, String> {
    conn.change_property8(PropMode::REPLACE, win, prop, AtomEnum::STRING, &[0u8])
        .map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match conn.poll_for_event().map_err(|e| e.to_string())? {
            Some(Event::PropertyNotify(e)) if e.window == win && e.atom == prop => return Ok(e.time),
            Some(other) => pending.push_back(other),
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    Err("the X server did not say what time it is".into())
}

/// The deepest window under the pointer, and whether button 1 is still held. `None` when the pointer
/// has left the screen.
fn pointer_state(conn: &RustConnection, root: Window) -> Option<(Option<Window>, bool)> {
    let first = conn.query_pointer(root).ok()?.reply().ok()?;
    if !first.same_screen {
        return Some((None, false));
    }
    let held = first.mask.contains(KeyButMask::BUTTON1);
    let mut win = root;
    let mut child = first.child;
    for _ in 0..32 {
        if child == x11rb::NONE {
            break;
        }
        win = child;
        let q = conn.query_pointer(win).ok()?.reply().ok()?;
        if !q.same_screen {
            break;
        }
        child = q.child;
    }
    Some((Some(win), held))
}

fn pointer_xy(conn: &RustConnection, root: Window) -> Option<(i16, i16)> {
    let q = conn.query_pointer(root).ok()?.reply().ok()?;
    Some((q.root_x, q.root_y))
}

/// Walk up from the window under the pointer to the first one that takes XDND drops (`XdndAware`),
/// following an `XdndProxy` when a toolkit stands in for the real window.
fn xdnd_target(conn: &RustConnection, atoms: &Atoms, start: Window) -> Result<Option<Window>, String> {
    let mut w = start;
    for _ in 0..64 {
        if w == x11rb::NONE {
            return Ok(None);
        }
        if let Some(proxy) = xdnd_proxy(conn, atoms, w)? {
            if proxy != w {
                w = proxy;
                continue;
            }
        }
        if xdnd_aware(conn, atoms, w)? {
            return Ok(Some(w));
        }
        let tree = conn
            .query_tree(w)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        if w == tree.root {
            return Ok(None);
        }
        w = tree.parent;
    }
    Ok(None)
}

fn xdnd_aware(conn: &RustConnection, atoms: &Atoms, w: Window) -> Result<bool, String> {
    let reply = conn
        .get_property(false, w, atoms.XdndAware, AtomEnum::ANY, 0, 1)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    // the value is a CARD32 holding the XDND version the window speaks
    Ok(reply
        .value32()
        .and_then(|mut v| v.next())
        .map(|v| v >= 3)
        .unwrap_or(false))
}

fn xdnd_proxy(conn: &RustConnection, atoms: &Atoms, w: Window) -> Result<Option<Window>, String> {
    let reply = conn
        .get_property(false, w, atoms.XdndProxy, AtomEnum::ANY, 0, 1)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    Ok(reply
        .value32()
        .and_then(|mut v| v.next())
        .filter(|v| *v != 0))
}

/// The name the target calls itself in: `WM_CLASS` (the class half, e.g. `Dolphin`) or, failing
/// that, its `_NET_WM_NAME`/`WM_NAME`. Only used for the status-bar hint, so any answer will do.
fn app_name(conn: &RustConnection, w: Window) -> Option<String> {
    let raw = conn
        .get_property(false, w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 64)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| r.value)
        .filter(|v| !v.is_empty());
    if let Some(raw) = raw {
        // "instance\0Class\0": the class is the half a user recognises
        let mut parts = raw.split(|b| *b == 0).filter(|p| !p.is_empty());
        let instance = parts.next().map(|p| String::from_utf8_lossy(p).to_string());
        let class = parts.next().map(|p| String::from_utf8_lossy(p).to_string());
        if let Some(name) = class.or(instance) {
            return Some(name);
        }
    }
    None
}

/// Whether `w` is inside NoteCraft's own window: a drag that came out of the app must not be dropped
/// back onto it, which would import the same picture a second time.
fn is_own(conn: &RustConnection, w: Window, app: Option<u32>) -> bool {
    let Some(app) = app else { return false };
    let mut cur = w;
    for _ in 0..64 {
        if cur == app {
            return true;
        }
        let tree = match conn.query_tree(cur) {
            Ok(cookie) => match cookie.reply() {
                Ok(tree) => tree,
                Err(_) => return false,
            },
            Err(_) => return false,
        };
        if cur == tree.root || tree.parent == x11rb::NONE {
            return false;
        }
        cur = tree.parent;
    }
    false
}

fn send_client(conn: &RustConnection, target: Window, atom: Atom, data: [u32; 5]) -> Result<(), String> {
    let ev = ClientMessageEvent::new(32, target, atom, data);
    conn.send_event(false, target, EventMask::NO_EVENT, ev)
        .map_err(|e| e.to_string())?;
    conn.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::uri_list;
    use std::path::Path;

    #[test]
    fn a_uri_list_line_is_a_file_url_a_target_can_open() {
        assert_eq!(
            uri_list(Path::new("/home/mj/Notes/Shots/assets/pic.png")),
            "file:///home/mj/Notes/Shots/assets/pic.png\r\n"
        );
    }

    #[test]
    fn a_space_or_a_non_ascii_letter_in_an_asset_name_is_percent_encoded() {
        // a raw space ends the URI there, and a target that opens the truncated path says the drop
        // did nothing
        assert_eq!(
            uri_list(Path::new("/home/mj/Notes/my shots/pic 1.png")),
            "file:///home/mj/Notes/my%20shots/pic%201.png\r\n"
        );
        assert_eq!(
            uri_list(Path::new("/home/mj/Notes/café.png")),
            "file:///home/mj/Notes/caf%C3%A9.png\r\n"
        );
    }

    #[test]
    fn the_unreserved_characters_are_left_alone() {
        assert_eq!(
            uri_list(Path::new("/a/AZaz09-._~/b.png")),
            "file:///a/AZaz09-._~/b.png\r\n"
        );
    }
}
