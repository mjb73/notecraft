//! The main window: three-pane OneNote layout, live-styled markdown editor,
//! aggregated tasks view, tags view and the Ctrl+K command palette.

use crate::attach;
use crate::doc;
#[cfg(all(unix, not(target_os = "macos")))]
use crate::dragout;
use crate::export;
use crate::highlight;
use crate::imgcache;
use crate::keylayout;
use crate::spell;
use crate::theme::{self, Theme};
use crate::vault::{self, HitKind, Ops, Page, Vault};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Margin, RichText, Sense, Stroke, StrokeKind, Vec2,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Our own X11 window id, when the window really is an X11 one (winit's X11/XWayland backend).
///
/// Dragging a picture *out* is a hand-written XDND source and needs that id; every other backend
/// (native Wayland, Windows, macOS) has no X11 window and no XDND to speak.
#[cfg(all(unix, not(target_os = "macos")))]
fn x11_window_of(frame: &eframe::Frame) -> Option<u32> {
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    frame.window_handle().ok().and_then(|h| match h.as_raw() {
        RawWindowHandle::Xlib(h) => Some(h.window as u32),
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        _ => None,
    })
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn x11_window_of(_frame: &eframe::Frame) -> Option<u32> {
    None
}

/// Consume a `Ctrl` shortcut.
///
/// The UI labels every shortcut `Ctrl+…` and that is true on every platform, but a Mac user
/// reaches for Command: on macOS both are accepted, so the label stays honest and the Mac habit
/// still works. Elsewhere `Modifiers::CTRL` is already the one modifier that exists.
fn ctrl_key(i: &mut egui::InputState, mods: egui::Modifiers, key: egui::Key) -> bool {
    if i.consume_key(mods, key) {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        if mods.ctrl && !mods.command {
            let alt = egui::Modifiers {
                ctrl: false,
                command: true,
                mac_cmd: false,
                ..mods
            };
            return i.consume_key(alt, key);
        }
    }
    false
}

const GUTTER: f32 = 26.0;
/// What the right-hand cluster of the top bar costs, measured: `Find` plus the mode, view, action
/// and display buttons and their gaps. The breadcrumb is given `available - this`, so it can never
/// grow a slot the buttons then have to overflow. It is a constant because the buttons' own widths
/// are; if a label is added to the bar, re-measure it.
const TOOLBAR_TAIL_W: f32 = 660.0;
/// Height of a picture's caption row. It is the height of `theme::icon_button` (30 pt), because the
/// file name, the picture's resolution and those buttons share one centre line — a shorter row would
/// leave the text floating above the buttons it sits next to.
const PICTURE_ROW_H: f32 = 30.0;
/// Room kept at the end of a task line with no date for its "+ due" chip.
const DUE_CHIP_W: f32 = 62.0;
/// How many search results the page panel draws before saying how many it is holding back.
const SEARCH_ROWS: usize = 60;

#[derive(Clone, Copy, PartialEq)]
enum View {
    Pages,
    Tasks,
    Tags,
}

/// The note's text size, the app-wide step under every block's own size. Large is the default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum NoteTextSize {
    Small,
    Medium,
    Large,
}

impl NoteTextSize {
    /// The same multipliers the editor's per-run `size S M L` uses, so "large note" and "large
    /// words" mean the same amount of larger.
    fn scale(self) -> f32 {
        match self {
            NoteTextSize::Small => doc::size_scale(-1),
            NoteTextSize::Medium => doc::size_scale(0),
            NoteTextSize::Large => doc::size_scale(1),
        }
    }

    fn id(self) -> &'static str {
        match self {
            NoteTextSize::Small => "s",
            NoteTextSize::Medium => "m",
            NoteTextSize::Large => "l",
        }
    }

    /// A missing or unknown value is **large**, not medium: the setting's default is large, and a
    /// state file written before the setting existed has to start there too.
    fn from_id(s: Option<&str>) -> Self {
        match s {
            Some("s") => NoteTextSize::Small,
            Some("m") => NoteTextSize::Medium,
            _ => NoteTextSize::Large,
        }
    }
}

/// How round a picture's corners are, when rounded corners are switched on. A fraction of the
/// picture's shorter side, so the same step looks the same on a thumbnail and on a full-width photo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CornerStep {
    Small,
    Medium,
    Large,
}

impl CornerStep {
    fn fraction(self) -> f32 {
        match self {
            CornerStep::Small => 0.03,
            CornerStep::Medium => 0.06,
            CornerStep::Large => 0.12,
        }
    }

    fn id(self) -> &'static str {
        match self {
            CornerStep::Small => "s",
            CornerStep::Medium => "m",
            CornerStep::Large => "l",
        }
    }

    /// Medium — the radius pictures were drawn with before the step was configurable.
    fn from_id(s: Option<&str>) -> Self {
        match s {
            Some("s") => CornerStep::Small,
            Some("l") => CornerStep::Large,
            _ => CornerStep::Medium,
        }
    }
}

#[derive(Clone, PartialEq)]
enum Confirm {
    DeletePage,
    DeleteNotebook(String),
    DeleteSection(String, String),
}

#[derive(Clone, PartialEq)]
enum Action {
    /// Open the page and land on this term (a search result).
    OpenPageAt(PathBuf, String, Option<usize>),
    OpenSection(String, String),
    Command(&'static str),
    Tag(String),
}

struct PalItem {
    label: String,
    hint: String,
    /// Which block of the list it belongs to ("Pages", "Tags", …). The list used to be one
    /// undifferentiated column of rows whose only clue was a faint "command" repeated on the right of
    /// every command; a heading per group says what the next rows *are*.
    group: &'static str,
    action: Action,
}

struct Palette {
    query: String,
    sel: usize,
    focus: bool,
}

/// The find bar's counter: `3 / 7`, and the page the match is on when it is not the open one.
fn find_label(current: usize, total: usize, other_page: Option<&str>) -> String {
    if total == 0 {
        return "no matches".to_string();
    }
    let n = current.min(total - 1) + 1;
    match other_page {
        Some(title) => format!("{n} / {total} · {title}"),
        None => format!("{n} / {total}"),
    }
}

/// Whether a page belongs in `scope`, anchored on the notebook and section that are open.
fn page_in_scope(
    notebook: &str,
    section: &str,
    scope: FindScope,
    nb: Option<&str>,
    sec: Option<&str>,
) -> bool {
    match scope {
        FindScope::Section => Some(notebook) == nb && Some(section) == sec,
        FindScope::Notebook => Some(notebook) == nb,
        FindScope::Vault => true,
    }
}

/// A page title cut to `max` characters with an ellipsis, for a label that has no room.
fn short_title(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let mut out: String = title.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Where `needle` sits in a list of block texts: `(block, char range within the block)`.
///
/// Offsets are counted in *characters*, which is what the editor's selection wants. A block whose
/// lowercase form is a different length (some scripts change byte length when lowercased) is
/// skipped rather than sliced at an offset that no longer lines up.
fn find_in_texts(texts: &[String], needle: &str) -> Vec<(usize, (usize, usize))> {
    let needle_l = needle.trim().to_lowercase();
    let mut out = Vec::new();
    if needle_l.is_empty() {
        return out;
    }
    let len = needle_l.chars().count();
    for (i, text) in texts.iter().enumerate() {
        let lowered = text.to_lowercase();
        if lowered.len() != text.len() {
            continue;
        }
        let mut from = 0usize;
        while let Some(at) = lowered[from..].find(&needle_l) {
            let abs = from + at;
            if text.is_char_boundary(abs) && text.is_char_boundary(abs + needle_l.len()) {
                let start = text[..abs].chars().count();
                out.push((i, (start, start + len)));
            }
            from = abs + 1;
        }
    }
    out
}

/// How far a search reaches: the open section, its whole notebook, or every page in the vault.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FindScope {
    Section,
    Notebook,
    Vault,
}

impl FindScope {
    fn label(self) -> &'static str {
        match self {
            FindScope::Section => "Section",
            FindScope::Notebook => "Notebook",
            FindScope::Vault => "All",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            FindScope::Section => "find in this section",
            FindScope::Notebook => "find in this notebook",
            FindScope::Vault => "find in the whole vault",
        }
    }
}

/// One place the query matched. Matches are collected across the whole scope, so the bar can walk
/// from page to page the way `Ctrl+F` does in an editor with a project-wide search.
#[derive(Clone, Debug)]
struct FindHit {
    page: PathBuf,
    page_title: String,
    block: usize,
    range: (usize, usize),
}

/// The texts of the pages in the current scope, read once per (scope, vault state).
///
/// Reading every `.note` in the vault on each keystroke would stall the bar on a big vault; the
/// block texts are kept instead of the documents (a fraction of the memory) because landing on a
/// hit reopens the page anyway.
struct FindCache {
    scope: FindScope,
    /// `notebook/section` the scope was computed for, so moving the scope's anchor rebuilds it.
    anchor: String,
    generation: u64,
    pages: Vec<(PathBuf, String, Vec<String>)>,
}

/// Find: a small bar over the note, with a scope toggle, the match count and next/previous.
#[derive(Clone)]
struct FindBar {
    query: String,
    scope: FindScope,
    /// Claim the field on the next frame.
    focus: bool,
    /// Which match is showing: an index into the hits for the scope *as a whole*, in vault order,
    /// so walking them is stable even as the open page changes under it.
    current: usize,
    /// Whether Enter has already jumped somewhere for this query. Without it the first Enter after
    /// typing skipped match 1 and landed on match 2.
    landed: bool,
}

pub struct NoteCraft {
    vault: Vault,
    theme: Theme,
    theme_idx: usize,
    /// The theme whose tokens have been pushed into egui's `Visuals`. Everything the app paints
    /// itself reads `self.theme`, but egui's own widgets (the title's `TextEdit`, menus, tooltips,
    /// the search field, scroll bars) read the `Visuals` that `theme::apply` writes — so a theme
    /// change has to be pushed there too, not just into the app's own colours.
    applied_theme: Option<&'static str>,
    view: View,
    nb: Option<String>,
    sec: Option<String>,
    page: Option<PathBuf>,
    title: String,
    body: String,
    created: Option<String>,
    order: f32,
    dirty: bool,
    last_edit: Option<Instant>,
    saved_at: Option<String>,
    read_mode: bool,
    /// The mode to go back to once a page that is not UTF-8 text (shown read-only) is left: the
    /// guard used to be a one-way switch, so every page opened afterwards stayed read-only.
    read_mode_before_unreadable: Option<bool>,
    /// The notebooks rail and the page list can each be folded away, so the note gets the whole
    /// window width. Remembered in state.json (`rail_open` / `pages_open`), defaulting to open.
    rail_open: bool,
    pages_open: bool,
    /// How pictures are drawn: centred in the note column, and with rounded corners. Both are
    /// display preferences (state.json), not properties of a page — the document stays portable.
    img_center: bool,
    img_rounded: bool,
    /// How round those corners are: S/M/L, a fraction of the picture's shorter side.
    img_radius: CornerStep,
    /// The note's text size, a step on the same scale the editor's `size S M L` row uses. Large is
    /// the default: the note is the thing being read, so it is set for reading, not for fitting.
    note_text: NoteTextSize,
    /// The face the note body is set in. The chrome keeps the house face: a note in Fraunces must not
    /// turn the toolbar into a serif. A display preference (state.json), like the text size.
    note_font: theme::NoteFont,
    /// Which dictionary misspellings are checked against, and the words the user has told it to leave
    /// alone (lower-case, from the context menu). Both are display preferences in `state.json`.
    spell: spell::Dict,
    spell_ignore: Vec<String>,
    /// The word under the pointer when a block's context menu was opened, so the menu can offer to
    /// correct or ignore it. Cleared whenever a menu opens on no word.
    spell_menu_word: Option<String>,
    /// Where that word sits in the block's text — the range a suggestion replaces.
    spell_menu_range: Option<std::ops::Range<usize>>,
    /// The corrections to offer for it, worked out **once** as the menu opens: [`spell::suggest`] can
    /// cost milliseconds (its distance-two fallback) and the menu is redrawn every frame it is open.
    spell_menu_fixes: Vec<String>,
    search: String,
    palette: Option<Palette>,
    confirm: Option<Confirm>,
    focus_title: bool,
    /// Put the caret in the vault search field on the next frame (Ctrl+F).
    status: String,
    status_at: Option<Instant>,
    last_focused: bool,
    drop_hover: bool,
    /// Decoded pictures, keyed by path: sizes from file headers, an LRU of textures and the pool
    /// that decodes them off the UI thread (see `imgcache`).
    image_cache: crate::imgcache::ImageCache,
    /// Block index of the picture being edited (drawn with a selection border).
    selected_image: Option<usize>,
    /// Lines from a picture being dragged *out* to another app (see `dragout`), drained into the
    /// status bar. `Some` while a drag is in flight, which is also what stops a second one starting.
    drag_rx: Option<std::sync::mpsc::Receiver<String>>,
    /// NoteCraft's own X11 window, when it is an X11 client: the drag-out refuses to drop the picture
    /// back onto this app, and uses it for the synthetic button release that ends the drag for egui.
    x11_window: Option<u32>,
    /// The picture whose corner handle is being dragged: `(block, how far inside its right edge the
    /// pointer grabbed the handle, the anchor the width is measured from)`. The anchor is the
    /// picture's left edge, or the column's middle when pictures are centred (the width is then twice
    /// the pointer's distance from it). The live width is measured from the pointer against those
    /// numbers, so the size follows the corner exactly and nothing accumulates from frame to frame —
    /// an `old width + drag delta` form re-adds last frame's growth and runs away from the pointer.
    resizing: Option<(usize, f32, f32)>,
    /// Drops from our own Windows drop target (see `windows_dnd`), which is the only way a browser
    /// image can reach the app there: the toolkit accepts real files only.
    #[cfg(windows)]
    win_dnd: Option<std::sync::mpsc::Receiver<crate::windows_dnd::Dropped>>,
    /// Whether registering that target was attempted — OLE refuses on some systems and it must not
    /// be retried every frame.
    #[cfg(windows)]
    win_dnd_tried: bool,
    /// Screen grab asked for by a button or a shortcut, started on the next frame (when the
    /// `egui::Context` is at hand to hide and restore the window).
    snip_request: bool,
    /// Screen grab in flight, attached when it lands.
    pending_snip: Option<PendingSnip>,
    /// Grab being cropped in the note: drag a box, Enter keeps all of it, Esc drops it.
    snip: Option<SnipShot>,
    /// Image being drawn on (its own fullscreen overlay viewport).
    annotate: Option<Annotate>,
    /// Set while a picture is being cropped: the overlay owns the window until Apply or Cancel.
    crop_edit: Option<CropEdit>,
    /// The page's document: blocks of text runs. The editor works on this; the markdown body is kept
    /// in step as its export, so the page list, tasks, tags and the CLI keep working unchanged.
    doc: doc::Doc,
    /// The block the menu acts on, and the character range marked inside it.
    selected_block: Option<usize>,
    block_selection: Option<(usize, usize)>,
    /// The last non-empty marking in each block. egui collapses a selection on *any* button press,
    /// so this is what lets a right-click keep the marked words (and the menu act on them).
    last_sel: std::collections::HashMap<usize, (usize, usize)>,
    /// A marking that spans blocks (inclusive range). Each block is its own TextEdit, so egui can
    /// only mark inside one of them; a drag that leaves its block or a Shift+click selects whole
    /// blocks here, and every menu action, Delete and Copy apply to the range.
    block_sel: Option<(usize, usize)>,
    /// Set while annotating a picture: the block to repoint at the annotated copy.
    annotate_from_block: Option<usize>,
    rename_for: Option<PathBuf>,
    rename_buf: String,
    move_for: Option<PathBuf>,
    /// The month the due-date calendar is showing. One picker at a time, so one month is enough.
    due_month: chrono::NaiveDate,
    /// The due-date picker, when one is open.
    due_pick: Option<DuePick>,
    /// The find-in-page bar, when one is open (Ctrl+F).
    find: Option<FindBar>,
    /// Page texts for the open search scope (see `FindCache`).
    find_cache: Option<FindCache>,
    /// The window title last sent to the compositor, so the breadcrumb is not re-sent every frame.
    title_sent: String,
    /// The "restore from .trash" dialog is showing.
    trash_open: bool,
    /// The tasks view's filters: what state a task is in, and when it is due.
    task_status: TaskStatus,
    task_due: TaskDue,
    /// Undo/redo over the open page's document.
    history: History,
    /// A block to bring into view once (a search result landing on its match).
    scroll_to_block: Option<usize>,
    /// Bring the foot of the note into view on the next frame. A picture dropped, pasted or snipped
    /// into the page is appended at the end, so the view follows it down; without this the new
    /// picture sat below the fold and looked as if nothing had happened.
    scroll_to_bottom: bool,
    /// Read the note from its top on the next frame. The page's scroll area keeps its offset across
    /// a page change, so a note opened after reading another one to its end would otherwise open at
    /// that note's depth.
    scroll_top: bool,
    /// Where the page's scroll area is and how tall it and the note are, as the last frame drew them:
    /// what Home / End / PgUp / PgDn measure against.
    page_scroll: Option<ScrollMetrics>,
    /// A vertical offset to put the note's scroll area at on the next frame (a scroll key's target or
    /// the progress rail's thumb). One-shot: after it is applied the wheel is the user's again.
    pending_scroll: Option<f32>,
    /// Screen y of every block, in document order, as the last frame drew them. One block is one line
    /// of the note (the document splits on newlines), so this is where the status bar's line number
    /// and the rail's heading ticks come from.
    block_tops: Vec<f32>,
    /// Screen y of the heading blocks from that same frame: the rail's outline ticks.
    heading_tops: Vec<f32>,
    /// `(first visible line, lines in the note)`, counted from the last frame's block tops.
    view_line: Option<(usize, usize)>,
    /// How wide each picture's caption row came out last frame, by block: what centres it under the
    /// picture (see the caption row in `edit_doc`).
    caption_rows: Vec<f32>,
    /// The block and the words to focus and mark on the *next* frame: a click hands focus back at the
    /// end of its own frame, so claiming it in that frame is lost.
    pending_land: Option<(usize, (usize, usize))>,
    /// What KDE says about the keyboard layout (which ones exist, which is live). KWin owns the
    /// keymap — the status bar's toggle switches *its* layout, so this is re-read on a timer and
    /// whenever the window comes back, and is never written into state.json.
    keylayout: Option<keylayout::KeyLayout>,
    /// The X11 keymap, when KWin has only one layout: that is what NoteCraft itself types through,
    /// so the toggle switches it directly (immediate, this app only) until a login gives KWin both.
    x11_layout: Option<keylayout::Layout>,
    /// The X11 layout the user asked for from the toggle, re-applied if something pushes KWin's
    /// keymap over it. Memory only: after a restart the live keymap is the truth again.
    want_x11: Option<String>,
    keylayout_at: Option<Instant>,
}

/// A click on the keyboard chip: what it would do.
#[derive(Clone)]
enum KbSwitch {
    /// KWin has every layout, so the switch moves the whole desktop.
    KWin(usize, String),
    /// KWin is missing a layout: switch the X11 keymap, which is what NoteCraft types through.
    X11(String, String),
}

/// What the keyboard chip should say and do, worked out once per frame.
struct KbChip {
    label: String,
    tip: String,
    /// `None` when there is nothing to switch to; the chip is then dim and `hint` explains why.
    switch: Option<KbSwitch>,
    hint: String,
}

/// A screen grab in flight: the capture thread writes `path` and reports how it went.
struct PendingSnip {
    path: PathBuf,
    /// Size seen last frame, and when it last changed (the file can be seen
    /// half-written, so it is only attached once it stops growing).
    len: u64,
    last_change: Instant,
    started: Instant,
    /// `Err` once the capture has failed, so the wait ends with a sentence instead of a timeout.
    outcome: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
}

/// How long the window is given to leave the screen before the grab is taken
/// (compositors animate a hide), otherwise NoteCraft photographs itself.
const SNIP_HIDE_SETTLE: Duration = Duration::from_millis(500);

/// Which kind of mark an annotation stroke is.
#[derive(Clone, Copy, PartialEq, Debug)]
enum AnnoTool {
    /// Freehand pen.
    Pen,
    /// Straight line (an underline is a line held flat).
    Line,
    /// Ellipse inscribed in the dragged box — a circle when the box is square.
    Circle,
}

/// One mark drawn on an image. Points are normalised to the image (0..1), so a mark keeps
/// its place whatever size the picture is shown at, and `width` is a fraction of the
/// image's shorter side.
#[derive(Clone, Debug)]
struct AnnoStroke {
    tool: AnnoTool,
    colour: egui::Color32,
    width: f32,
    points: Vec<(f32, f32)>,
}

/// A document edit picked from the editor's right-click menu: change what a block *is*, or what
/// a run carries. No characters are involved.
#[derive(Clone, Debug)]
enum Edit {
    Kind(doc::Kind),
    /// 0 bold, 1 italic, 2 code, 3 strike — toggled over the marked range.
    Attr(u8),
    /// Highlighter colour for the marked words (0 clears it).
    Mark(u8),
    Due(chrono::NaiveDate),
    DueClear,
    Tag(String),
    /// -1 small, 0 the block's own size, +1 large.
    Size(i8),
}

/// What "Export" writes.
#[derive(Clone, Copy, PartialEq)]
enum ExFmt {
    Html,
    Pdf,
    Text,
    CopyMd,
}

/// Picture actions offered by the same menu.
#[derive(Clone, Copy, PartialEq)]
enum PicAction {
    Attach,
    Snip,
    Paste,
}

/// The clipboard rows of the editor's menu. They act on the marking, not on a block's kind, so
/// they are plain actions rather than the toggles the rest of the menu is made of.
#[derive(Clone, Copy, PartialEq)]
enum ClipAction {
    Copy,
    Cut,
    Paste,
}

/// An image being annotated: full-resolution pixels for the result, a downscaled copy for
/// the screen, and the strokes drawn so far.
struct Annotate {
    img: image::RgbaImage,
    tex: Option<egui::TextureHandle>,
    /// Where the picture sits in the overlay this frame (for pointer → image mapping).
    rect: Option<egui::Rect>,
    tool: AnnoTool,
    colour: egui::Color32,
    brush: f32,
    strokes: Vec<AnnoStroke>,
    current: Option<AnnoStroke>,
}

/// A picture being cropped, in the overlay that owns the window while it is open.
///
/// Nothing is written until Apply: the block keeps the crop it had, so Cancel is free and the note
/// behind the overlay never shows a half-dragged crop.
struct CropEdit {
    block: usize,
    reference: String,
    abs: PathBuf,
    /// The picture's own size, from its header.
    src: Vec2,
    /// The region being kept, in source pixels — dragging is in the picture's own coordinates, so the
    /// same numbers do not change meaning when the window is resized.
    rect: egui::Rect,
    aspect: CropAspect,
    /// Where the picture was drawn this frame, and its scale — set by the overlay, read by the drags.
    view: egui::Rect,
}

/// The ratio lock above the crop. `Free` is the freehand crop; the others hold the frame to a shape
/// while a corner is dragged.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CropAspect {
    Free,
    Square,
    FourThree,
    ThreeTwo,
    SixteenNine,
}

impl CropAspect {
    const ALL: [CropAspect; 5] = [
        CropAspect::Free,
        CropAspect::Square,
        CropAspect::FourThree,
        CropAspect::ThreeTwo,
        CropAspect::SixteenNine,
    ];

    fn label(self) -> &'static str {
        match self {
            CropAspect::Free => "Free",
            CropAspect::Square => "1:1",
            CropAspect::FourThree => "4:3",
            CropAspect::ThreeTwo => "3:2",
            CropAspect::SixteenNine => "16:9",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            CropAspect::Free => "freehand: drag any handle to any shape",
            CropAspect::Square => "hold the frame to 1:1 while a corner is dragged",
            CropAspect::FourThree => "hold the frame to 4:3",
            CropAspect::ThreeTwo => "hold the frame to 3:2",
            CropAspect::SixteenNine => "hold the frame to 16:9",
        }
    }

    /// width / height, or `None` when the crop is freehand.
    fn ratio(self) -> Option<f32> {
        match self {
            CropAspect::Free => None,
            CropAspect::Square => Some(1.0),
            CropAspect::FourThree => Some(4.0 / 3.0),
            CropAspect::ThreeTwo => Some(3.0 / 2.0),
            CropAspect::SixteenNine => Some(16.0 / 9.0),
        }
    }
}

/// One of the eight handles around a crop. Moving the frame is the frame's own drag, not a handle: it
/// has no grab point to hit, and the frame's body is the whole kept region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CropHandle {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
}

impl CropHandle {
    /// The four corners, which are the only handles a locked ratio uses (an edge drag would have to
    /// invent the other dimension).
    const CORNERS: [CropHandle; 4] = [CropHandle::Nw, CropHandle::Ne, CropHandle::Se, CropHandle::Sw];
    const EDGES: [CropHandle; 4] = [CropHandle::N, CropHandle::E, CropHandle::S, CropHandle::W];

    fn all() -> Vec<CropHandle> {
        Self::CORNERS.into_iter().chain(Self::EDGES).collect()
    }

    /// The point the drag must not move: the corner (or edge's far side) opposite this handle.
    fn anchor(self, r: egui::Rect) -> egui::Pos2 {
        match self {
            CropHandle::Nw => r.max,
            CropHandle::Ne => egui::pos2(r.left(), r.bottom()),
            CropHandle::Se => r.min,
            CropHandle::Sw => egui::pos2(r.right(), r.top()),
            CropHandle::N => egui::pos2(r.center().x, r.bottom()),
            CropHandle::S => egui::pos2(r.center().x, r.top()),
            CropHandle::W => egui::pos2(r.right(), r.center().y),
            CropHandle::E => egui::pos2(r.left(), r.center().y),
        }
    }

    fn cursor(self) -> egui::CursorIcon {
        match self {
            CropHandle::Nw | CropHandle::Se => egui::CursorIcon::ResizeNwSe,
            CropHandle::Ne | CropHandle::Sw => egui::CursorIcon::ResizeNeSw,
            CropHandle::N | CropHandle::S => egui::CursorIcon::ResizeVertical,
            CropHandle::E | CropHandle::W => egui::CursorIcon::ResizeHorizontal,
        }
    }
}

/// Pen colours offered in the annotator (opaque, so they survive premultiplication).
const ANNO_COLOURS: [(egui::Color32, &str); 7] = [
    (egui::Color32::from_rgb(255, 59, 48), "red"),
    (egui::Color32::from_rgb(255, 149, 0), "orange"),
    (egui::Color32::from_rgb(255, 214, 10), "yellow"),
    (egui::Color32::from_rgb(52, 199, 89), "green"),
    (egui::Color32::from_rgb(64, 156, 255), "blue"),
    (egui::Color32::WHITE, "white"),
    (egui::Color32::from_rgb(20, 20, 24), "black"),
];

/// Brush widths as a fraction of the image's shorter side.
const ANNO_BRUSHES: [f32; 3] = [0.005, 0.012, 0.024];

/// Paint a filled disc, clipped to the image.
fn stamp_disc(img: &mut image::RgbaImage, cx: f32, cy: f32, r: f32, rgba: [u8; 4]) {
    let (w, h) = (img.width() as f32, img.height() as f32);
    if cx + r < 0.0 || cy + r < 0.0 || cx - r > w || cy - r > h {
        return;
    }
    let x0 = (cx - r).floor().max(0.0) as u32;
    let y0 = (cy - r).floor().max(0.0) as u32;
    let x1 = ((cx + r).ceil().max(0.0) as u32).min(img.width().saturating_sub(1));
    let y1 = ((cy + r).ceil().max(0.0) as u32).min(img.height().saturating_sub(1));
    let r2 = r * r;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if dx * dx + dy * dy <= r2 {
                img.put_pixel(x, y, image::Rgba(rgba));
            }
        }
    }
}

/// Stamp discs along a segment: the brush-stamping way to draw a thick, round-capped line
/// without pulling in a drawing crate.
fn stamp_segment(img: &mut image::RgbaImage, a: (f32, f32), b: (f32, f32), r: f32, rgba: [u8; 4]) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    let step = (r * 0.4).max(0.7);
    let n = ((len / step).ceil() as i32).max(1);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        stamp_disc(img, a.0 + dx * t, a.1 + dy * t, r, rgba);
    }
}

/// A stroke as a polyline in image pixels (the ellipse is sampled).
fn stroke_points(s: &AnnoStroke, w: f32, h: f32) -> Vec<(f32, f32)> {
    let pts: Vec<(f32, f32)> = s.points.iter().map(|(x, y)| (x * w, y * h)).collect();
    match s.tool {
        AnnoTool::Circle if pts.len() >= 2 => {
            let (x0, y0) = pts[0];
            let (x1, y1) = pts[1];
            let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
            let (rx, ry) = ((x1 - x0).abs() * 0.5, (y1 - y0).abs() * 0.5);
            let steps = ((rx.max(ry) * 2.0) as i32).clamp(24, 720);
            (0..=steps)
                .map(|i| {
                    let t = i as f32 / steps as f32 * std::f32::consts::TAU;
                    (cx + rx * t.cos(), cy + ry * t.sin())
                })
                .collect()
        }
        _ => pts,
    }
}

/// Flatten every stroke onto a copy of the image, at full resolution.
fn draw_strokes(img: &image::RgbaImage, strokes: &[AnnoStroke]) -> image::RgbaImage {
    let mut out = img.clone();
    let (w, h) = (out.width() as f32, out.height() as f32);
    let short = w.min(h);
    for s in strokes {
        let r = (s.width * short).max(1.0) * 0.5;
        let rgba = s.colour.to_array();
        let pts = stroke_points(s, w, h);
        if s.tool == AnnoTool::Circle {
            for p in pts {
                stamp_disc(&mut out, p.0, p.1, r, rgba);
            }
        } else {
            for pair in pts.windows(2) {
                stamp_segment(&mut out, pair[0], pair[1], r, rgba);
            }
            if pts.len() == 1 {
                stamp_disc(&mut out, pts[0].0, pts[0].1, r, rgba);
            }
        }
    }
    out
}

/// A full-screen grab waiting to be cropped in the note.
struct SnipShot {
    /// Full-resolution pixels: the crop is taken from these, never from the preview.
    img: image::RgbaImage,
    /// Downscaled display copy, uploaded on first draw.
    tex: Option<egui::TextureHandle>,
    /// Where the whole grab is drawn in the note, and the box being dragged.
    rect: Option<egui::Rect>,
    from: Option<egui::Pos2>,
    to: Option<egui::Pos2>,
    /// The box actually being kept (inside the picture, big enough to be a box).
    sel: Option<egui::Rect>,
    temp: PathBuf,
}

/// A grab is complete once the file has content and has stopped growing — or as
/// soon as the grabber exits, whichever comes first.
fn snip_ready(len: u64, since_change: Duration, exited: bool) -> bool {
    len > 0 && (since_change >= Duration::from_millis(350) || exited)
}

/// Load a whole-screen grab: full pixels for the crop, a downscaled copy for the
/// texture (uploading 8 megapixels of RGBA would be 33 MB of VRAM for a preview).
fn open_snip(path: &Path) -> Result<SnipShot, String> {
    let img = image::open(path)
        .map_err(|e| e.to_string())?
        .to_rgba8();
    Ok(SnipShot {
        img,
        tex: None,
        rect: None,
        from: None,
        to: None,
        sel: None,
        temp: path.to_path_buf(),
    })
}

/// Map a selection dragged in the note back to pixels of the grab, clamped to the
/// image: the note is smaller than the screen, so the box is scaled up.
fn snip_crop(img: (u32, u32), drawn: egui::Rect, sel: egui::Rect) -> (u32, u32, u32, u32) {
    let (iw, ih) = (img.0.max(1) as f32, img.1.max(1) as f32);
    let sx = iw / drawn.width().max(1.0);
    let sy = ih / drawn.height().max(1.0);
    let x0 = ((sel.min.x - drawn.min.x) * sx).clamp(0.0, iw - 1.0);
    let y0 = ((sel.min.y - drawn.min.y) * sy).clamp(0.0, ih - 1.0);
    let x1 = ((sel.max.x - drawn.min.x) * sx).clamp(x0 + 1.0, iw);
    let y1 = ((sel.max.y - drawn.min.y) * sy).clamp(y0 + 1.0, ih);
    (
        x0 as u32,
        y0 as u32,
        (x1 - x0).max(1.0) as u32,
        (y1 - y0).max(1.0) as u32,
    )
}

#[cfg(test)]
mod tests_snip {
    use super::{default_size, snip_ready};
    use crate::imgcache::{self, MAX_TEX_DIM};
    use std::time::Duration;

    #[test]
    fn a_new_picture_arrives_as_wide_as_the_column() {
        // Dropping a picture into a note gives it the note's own width, at the file's aspect…
        let drawn = default_size(egui::vec2(1024.0, 1408.0), 840.0);
        assert_eq!(drawn, egui::vec2(840.0, 1155.0));
        // …including a tall portrait, which is as tall as its aspect makes it (no height cap: the
        // old 640 pt fit is what made a screenshot arrive a third of the page wide)
        let tall = default_size(egui::vec2(800.0, 4000.0), 840.0);
        assert_eq!(tall, egui::vec2(800.0, 4000.0));
        // and a picture narrower than the column keeps its own pixels rather than being blown up
        assert_eq!(
            default_size(egui::vec2(300.0, 200.0), 840.0),
            egui::vec2(300.0, 200.0)
        );
    }

    #[test]
    fn a_drawn_picture_is_never_wider_than_its_texture() {
        // The texture is made for the width the picture is actually drawn at, so no picture is ever
        // stretched past its own pixels on screen.
        let col_w = 880.0f32;
        for (w, h) in [
            (1000u32, 600u32),
            (4000, 3000),
            (3000, 1200),
            (800, 4000),
            (200, 100),
        ] {
            let intrinsic = egui::vec2(w as f32, h as f32);
            let drawn = default_size(intrinsic, col_w);
            let tex = imgcache::tex_size(w, h, drawn.x.round() as u32);
            let tex_size = egui::vec2(tex.0 as f32, tex.1 as f32);
            let drawn_again = default_size(tex_size, col_w);
            assert!(
                drawn_again.x <= drawn.x + 0.01,
                "{w}x{h}: texture {tex:?} would be stretched to {drawn_again:?}"
            );
            assert!(tex.0 <= MAX_TEX_DIM && tex.1 <= MAX_TEX_DIM);
            // aspect is kept, so the picture does not squash
            let ratio_in = intrinsic.y / intrinsic.x;
            let ratio_out = drawn.y / drawn.x;
            assert!((ratio_in - ratio_out).abs() < 0.01, "{w}x{h} changed aspect");
        }
    }

    #[test]
    fn a_thumbnail_is_not_decoded_at_full_size() {
        // a picture drawn 300 pt wide costs a 300 px texture, not a 1024 px one: a page of
        // thumbnails must not hold full-size copies of every screenshot
        assert_eq!(imgcache::tex_size(1024, 1408, 300), (300, 413));
        assert_eq!(imgcache::tex_size(1024, 1408, 640), (640, 880));
    }

    #[test]
    fn a_grab_is_attached_only_once_it_stopped_growing() {
        // empty file: the grabber has not written anything yet
        assert!(!snip_ready(0, Duration::from_secs(10), false));
        // still growing: wait, or a half-written PNG would be attached
        assert!(!snip_ready(1024, Duration::from_millis(50), false));
        // size settled -> attach, even if the grabber is still alive
        assert!(snip_ready(1024, Duration::from_millis(400), false));
        // grabber gone with content -> attach straight away
        assert!(snip_ready(1024, Duration::from_millis(0), true));
        // grabber gone with nothing written -> cancelled, never attach
        assert!(!snip_ready(0, Duration::from_secs(3), true));
    }

    #[test]
    fn crop_maps_the_dragged_box_back_to_image_pixels() {
        use super::snip_crop;
        use egui::{pos2, vec2, Rect};
        // a 1000x500 grab drawn at half size in the note
        let drawn = Rect::from_min_size(pos2(100.0, 200.0), vec2(500.0, 250.0));
        // a box over the middle quarter of that drawing -> (200,100) 500x250 of pixels
        let sel = Rect::from_min_size(pos2(200.0, 250.0), vec2(250.0, 125.0));
        assert_eq!(snip_crop((1000, 500), drawn, sel), (200, 100, 500, 250));
        // a box hanging off every edge cannot address pixels outside the image
        let out = Rect::from_min_size(pos2(-500.0, -500.0), vec2(2000.0, 2000.0));
        assert_eq!(snip_crop((1000, 500), drawn, out), (0, 0, 1000, 500));
        // a degenerate box still yields a drawable 1x1 crop
        let dot = Rect::from_min_size(pos2(300.0, 300.0), vec2(0.0, 0.0));
        assert_eq!(snip_crop((1000, 500), drawn, dot), (400, 200, 1, 1));
    }
}

/// Tick the todo on body line `line` of a page that may not be the open one (the tasks view acts
/// on the whole vault). The document is the store: flip the block, write both files. `Ok(false)`
/// means the markdown and the document disagree about that line, so nothing was touched.
fn toggle_todo_in_page(path: &Path, line: usize) -> std::io::Result<bool> {
    let (title, body, created, order, _) = vault::read_page_file(path);
    let Some((_, text)) = body.split('\n').nth(line).and_then(vault::parse_todo_line) else {
        return Ok(false);
    };
    let mut d = doc::load(path).unwrap_or_else(|| doc::from_markdown(&body));
    let Some(i) = doc::todo_block_at_line(&d, &body, line, &text) else {
        return Ok(false);
    };
    d.blocks[i].done = !d.blocks[i].done;
    vault::write_page(path, &title, created.as_deref(), order, &doc::to_markdown(&d))?;
    doc::save(path, &d)?;
    Ok(true)
}

/// What `strip_done_tasks` did with one page.
enum StripOutcome {
    /// The done tasks' words, and the page rewritten without them.
    Removed(String, Vec<String>),
    Nothing,
    /// The markdown and the document disagree about how many tasks are ticked: left alone, because
    /// guessing which side is right while deleting could take words with it.
    OutOfStep,
}

/// Remove every done task block from one page, writing the markdown and the document together.
fn strip_done_tasks(path: &Path) -> std::io::Result<StripOutcome> {
    let (title, body, created, order, _) = vault::read_page_file(path);
    let mut d = doc::load(path).unwrap_or_else(|| doc::from_markdown(&body));
    if d.blocks.is_empty() {
        return Ok(StripOutcome::Nothing);
    }
    let md_done = body
        .split('\n')
        .filter(|l| matches!(vault::parse_todo_line(l), Some((true, _))))
        .count();
    let doc_done = d
        .blocks
        .iter()
        .filter(|b| b.kind == doc::Kind::Todo && b.done)
        .count();
    if doc_done == 0 {
        return Ok(StripOutcome::Nothing);
    }
    if md_done != doc_done {
        return Ok(StripOutcome::OutOfStep);
    }
    let mut removed = Vec::with_capacity(doc_done);
    let mut keep = Vec::with_capacity(d.blocks.len());
    for b in d.blocks.drain(..) {
        if b.kind == doc::Kind::Todo && b.done {
            removed.push(b.text());
        } else {
            keep.push(b);
        }
    }
    d.blocks = keep;
    d.ensure_block();
    vault::write_page(path, &title, created.as_deref(), order, &doc::to_markdown(&d))?;
    doc::save(path, &d)?;
    Ok(StripOutcome::Removed(title, removed))
}

/// Set (or clear, with `None`) the due date of the todo on body line `line`, in a page that may not
/// be open — the tasks view is the only place a due date can be set, and it acts on the whole vault.
/// The document is the store: put the date on that block's runs and write both files.
fn set_todo_due_in_page(
    path: &Path,
    line: usize,
    due: Option<chrono::NaiveDate>,
) -> std::io::Result<bool> {
    let (title, body, created, order, _) = vault::read_page_file(path);
    let Some((_, text)) = body.split('\n').nth(line).and_then(vault::parse_todo_line) else {
        return Ok(false);
    };
    let mut d = doc::load(path).unwrap_or_else(|| doc::from_markdown(&body));
    let Some(i) = doc::todo_block_at_line(&d, &body, line, &text) else {
        return Ok(false);
    };
    match due {
        Some(day) => doc::set_due(&mut d.blocks[i].runs, day),
        None => doc::clear_due(&mut d.blocks[i].runs),
    }
    vault::write_page(path, &title, created.as_deref(), order, &doc::to_markdown(&d))?;
    doc::save(path, &d)?;
    Ok(true)
}

#[cfg(test)]
mod tests_todo {
    use super::*;

    #[test]
    fn ticking_a_todo_writes_the_document_and_the_markdown_together() {
        let dir = std::env::temp_dir().join(format!("notecraft-tick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("P.md");
        vault::write_page(
            &page,
            "P",
            None,
            0.0,
            "- [ ] water the plants #home\n- [ ] call mum\n",
        )
        .unwrap();
        // the state every page has after the migration: a document beside the markdown
        doc::save(&page, &doc::from_markdown("- [ ] water the plants #home\n- [ ] call mum\n")).unwrap();

        assert!(toggle_todo_in_page(&page, 1).unwrap(), "the second todo ticks");
        assert_eq!(doc::load(&page).unwrap().blocks[1].done, true);
        let (_, body, _, _, _) = vault::read_page_file(&page);
        assert!(body.contains("- [x] call mum"), "{body:?}");
        assert!(body.contains("- [ ] water"), "{body:?}");
        // and back
        assert!(toggle_todo_in_page(&page, 1).unwrap());
        assert_eq!(doc::load(&page).unwrap().blocks[1].done, false);
        // a line the document does not know about is refused, not guessed at
        std::fs::write(&page, "---\ntitle: P\norder: 0\n---\n\n- [ ] a hand-typed one\n").unwrap();
        assert!(!toggle_todo_in_page(&page, 0).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests_due {
    use super::*;

    fn todo_page(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("T.md");
        vault::write_page(&p, "T", None, 0.0, body).unwrap();
        doc::save(&p, &doc::from_markdown(body)).unwrap();
        p
    }

    #[test]
    fn the_tasks_view_sets_and_clears_a_due_date_in_both_files() {
        let dir = std::env::temp_dir().join(format!("notecraft-due-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = todo_page(&dir, "- [ ] water the plants\n- [x] call mum\n");
        let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();

        assert!(set_todo_due_in_page(&p, 0, Some(day)).unwrap());
        let (_, body, _, _, _) = vault::read_page_file(&p);
        assert!(body.contains("- [ ] water the plants @due(2026-10-24)"), "{body:?}");
        let d = doc::load(&p).unwrap();
        assert_eq!(d.blocks[0].runs.iter().find_map(|r| r.a.due), Some(day));
        // the other task is untouched
        assert!(d.blocks[1].runs.iter().all(|r| r.a.due.is_none()));

        // clearing takes the date and its own words with it
        assert!(set_todo_due_in_page(&p, 0, None).unwrap());
        let (_, body, _, _, _) = vault::read_page_file(&p);
        assert!(body.contains("- [ ] water the plants\n"), "{body:?}");
        assert!(!body.contains("@due"), "{body:?}");

        // a line the document does not know about is refused, not guessed at
        std::fs::write(&p, "---\ntitle: T\norder: 0\n---\n\n- [ ] a hand-typed one\n").unwrap();
        assert!(!set_todo_due_in_page(&p, 0, Some(day)).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests_calendar {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn the_calendar_walks_months_and_counts_days() {
        assert_eq!(month_of(d(2026, 10, 18)), d(2026, 10, 1));
        assert_eq!(shift_month(d(2026, 10, 1), 1), d(2026, 11, 1));
        assert_eq!(shift_month(d(2026, 1, 1), -1), d(2025, 12, 1));
        assert_eq!(shift_month(d(2026, 12, 1), 1), d(2027, 1, 1));
        assert_eq!(days_in_month(d(2026, 2, 1)), 28);
        assert_eq!(days_in_month(d(2028, 2, 1)), 29, "a leap year");
        assert_eq!(days_in_month(d(2026, 10, 1)), 31);
        assert_eq!(days_in_month(d(2026, 4, 1)), 30);
    }
}

#[cfg(test)]
mod tests_done_cleanup {
    use super::*;

    fn page(dir: &Path, body: &str) -> PathBuf {
        let p = dir.join("T.md");
        vault::write_page(&p, "T", None, 0.0, body).unwrap();
        doc::save(&p, &doc::from_markdown(body)).unwrap();
        p
    }

    #[test]
    fn deleting_done_tasks_removes_the_lines_from_both_files() {
        let dir = std::env::temp_dir().join(format!("notecraft-done-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = page(&dir, "- [ ] buy compost\n- [x] water the plants\n- [x] ring the plumber\n");

        match strip_done_tasks(&p).unwrap() {
            StripOutcome::Removed(title, lines) => {
                assert_eq!(title, "T");
                assert_eq!(lines, vec!["water the plants", "ring the plumber"]);
            }
            _ => panic!("the two done tasks should have gone"),
        }
        let (_, body, _, _, _) = vault::read_page_file(&p);
        assert_eq!(body.trim(), "- [ ] buy compost", "{body:?}");
        let d = doc::load(&p).unwrap();
        assert_eq!(d.blocks.len(), 1);
        assert_eq!(d.blocks[0].text(), "buy compost");
        // nothing left to delete the second time
        assert!(matches!(strip_done_tasks(&p).unwrap(), StripOutcome::Nothing));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_page_that_is_all_done_tasks_keeps_one_block_to_type_in() {
        let dir = std::env::temp_dir().join(format!("notecraft-done2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = page(&dir, "- [x] water the plants\n");
        assert!(matches!(strip_done_tasks(&p).unwrap(), StripOutcome::Removed(..)));
        let d = doc::load(&p).unwrap();
        assert_eq!(d.blocks.len(), 1, "a page always keeps a block to type in");
        assert_eq!(d.blocks[0].text(), "");
        let (_, body, _, _, _) = vault::read_page_file(&p);
        assert_eq!(body.trim(), "", "{body:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_page_whose_document_disagrees_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("notecraft-done3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = page(&dir, "- [x] water the plants\n");
        // someone ticked it in the file, not through the app
        std::fs::write(&p, "---\ntitle: T\norder: 0\n---\n\n- [ ] water the plants\n").unwrap();
        assert!(matches!(strip_done_tasks(&p).unwrap(), StripOutcome::OutOfStep));
        let (_, body, _, _, _) = vault::read_page_file(&p);
        assert!(body.contains("- [ ] water the plants"), "untouched: {body:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests_filters {
    use super::*;

    #[test]
    fn the_task_filters_pick_their_buckets_and_dates() {
        // status decides which buckets exist …
        assert!(bucket_shown(TaskStatus::All, 0));
        assert!(bucket_shown(TaskStatus::All, 6), "done shows unless a status hides it");
        assert!(!bucket_shown(TaskStatus::Open, 6), "Open hides Done");
        assert!(!bucket_shown(TaskStatus::Done, 0), "Done hides the open buckets");
        assert!(bucket_shown(TaskStatus::Done, 6));

        // … and the date filter is per task, so it works in the Done group too
        let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
        let d = |n: i64| Some(today + chrono::Duration::days(n));
        assert!(due_matches(d(-1), TaskDue::Overdue, today));
        assert!(!due_matches(d(0), TaskDue::Overdue, today), "today is not overdue");
        assert!(due_matches(d(0), TaskDue::Today, today));
        assert!(due_matches(d(7), TaskDue::Week, today), "a week includes its last day");
        assert!(!due_matches(d(8), TaskDue::Week, today));
        assert!(due_matches(d(8), TaskDue::Later, today));
        assert!(due_matches(None, TaskDue::None, today));
        assert!(!due_matches(None, TaskDue::Overdue, today));
        assert!(due_matches(None, TaskDue::Any, today));
    }
}

#[cfg(test)]
mod tests_history {
    use super::*;

    fn doc_with(text: &str) -> doc::Doc {
        doc::from_markdown(text)
    }

    #[test]
    fn undo_and_redo_walk_the_documents_and_a_new_edit_drops_the_redo() {
        let mut h = History::new();
        let v1 = doc_with("one\n");
        let v2 = doc_with("one two\n");
        let v3 = doc_with("one two three\n");
        // nothing to undo yet
        assert!(!h.can_undo());
        let mut current = v1.clone();
        h.push(&current);
        current = v2.clone();
        h.push(&current);
        current = v3.clone();

        current = h.undo(&current).expect("one step back");
        assert_eq!(current.blocks[0].text(), "one two");
        current = h.undo(&current).expect("two steps back");
        assert_eq!(current.blocks[0].text(), "one");
        assert!(h.undo(&current).is_none(), "the start of the history");

        current = h.redo(&current).expect("forward again");
        assert_eq!(current.blocks[0].text(), "one two");
        // a new edit after an undo throws the redo branch away
        h.push(&current);
        current = doc_with("one two and a new line\n");
        assert!(h.redo(&current).is_none(), "the old redo branch is gone");
        assert_eq!(h.undo(&current).unwrap().blocks[0].text(), "one two");
    }

    #[test]
    fn typing_is_one_step_per_burst() {
        let mut h = History::new();
        let mut d = doc_with("a\n");
        h.push_typing(&d);
        // a second keystroke a moment later is the same step
        d = doc_with("ab\n");
        h.push_typing(&d);
        assert_eq!(h.undo.len(), 1);
        assert_eq!(h.undo(&d).unwrap().blocks[0].text(), "a");
    }
}

#[cfg(test)]
mod tests_panels {
    use super::*;

    /// Home / End / PgUp / PgDn move through the open note, and `End` really is its end: the offset
    /// from which the note's last line is on screen (`content - view`), not the top of the last
    /// screenful. A page is one window with a little overlap so nothing is skipped at the fold.
    #[test]
    fn the_scroll_keys_land_on_the_notes_own_top_and_end() {
        // a 1000 pt note in a 400 pt window: the furthest offset is 600
        assert_eq!(scroll_target(0.0, 400.0, 1000.0, ScrollTo::Top), 0.0);
        assert_eq!(scroll_target(300.0, 400.0, 1000.0, ScrollTo::Bottom), 600.0);
        assert_eq!(scroll_target(0.0, 400.0, 1000.0, ScrollTo::PageDown), 368.0);
        assert_eq!(scroll_target(368.0, 400.0, 1000.0, ScrollTo::PageDown), 600.0);
        // the last screen stays put instead of drifting past the content
        assert_eq!(scroll_target(600.0, 400.0, 1000.0, ScrollTo::PageDown), 600.0);
        assert_eq!(scroll_target(600.0, 400.0, 1000.0, ScrollTo::PageUp), 232.0);
        assert_eq!(scroll_target(100.0, 400.0, 1000.0, ScrollTo::PageUp), 0.0);
        // a note shorter than the window has nowhere to go
        assert_eq!(scroll_target(0.0, 400.0, 200.0, ScrollTo::PageDown), 0.0);
        assert_eq!(scroll_target(0.0, 400.0, 200.0, ScrollTo::Bottom), 0.0);
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// The panel toggles are Ctrl+1 / Ctrl+2, and a plain `1` typed into a note must still be a
    /// character: `consume_key(CTRL, Num1)` only matches with Ctrl held, and the binding is taken
    /// before the focused editor draws (it is, `shortcuts()` runs first in `update`).
    #[test]
    fn the_counter_says_where_the_match_is() {
        assert_eq!(find_label(0, 1, None), "1 / 1");
        assert_eq!(find_label(2, 7, None), "3 / 7");
        // a match on another page names it, because the bar is the only place that shows where the
        // next Enter will land
        assert_eq!(find_label(0, 3, Some("Four")), "1 / 3 · Four");
        // an index left over from a longer query cannot print past the total
        assert_eq!(find_label(9, 7, None), "7 / 7");
        assert_eq!(find_label(0, 0, None), "no matches");
    }

    #[test]
    fn a_scope_reaches_exactly_its_own_pages() {
        // the three-way toggle in the find bar
        assert!(page_in_scope("NB", "Sec", FindScope::Section, Some("NB"), Some("Sec")));
        assert!(!page_in_scope("NB", "Other", FindScope::Section, Some("NB"), Some("Sec")));
        assert!(!page_in_scope("Elsewhere", "Sec", FindScope::Section, Some("NB"), Some("Sec")));

        assert!(page_in_scope("NB", "Other", FindScope::Notebook, Some("NB"), Some("Sec")));
        assert!(!page_in_scope("Elsewhere", "Sec", FindScope::Notebook, Some("NB"), Some("Sec")));

        assert!(page_in_scope("Elsewhere", "Sec", FindScope::Vault, Some("NB"), Some("Sec")));
        // nothing open: only "the whole vault" still means something
        assert!(!page_in_scope("NB", "Sec", FindScope::Section, None, None));
        assert!(page_in_scope("NB", "Sec", FindScope::Vault, None, None));
    }

    #[test]
    fn the_match_walk_uses_chars_and_finds_every_occurrence() {
        let texts = vec![
            "The lantern is here.".to_string(),
            "nothing".to_string(),
            "Two lanterns, lantern twice — and a café lantern.".to_string(),
        ];
        let hits = find_in_texts(&texts, "lantern");
        // one in the first block, three in the third ("lanterns" holds one too)
        assert_eq!(hits.len(), 4, "{hits:?}");
        assert_eq!(hits[0], (0, (4, 11)));
        assert_eq!(hits[1], (2, (4, 11)));
        assert_eq!(hits[2], (2, (14, 21)));
        // ranges are char offsets into the block text, so the em dash and the `é` before the last
        // match shift them but never split a character
        let last = hits[3].1;
        let block = &texts[2];
        let piece: String = block.chars().skip(last.0).take(last.1 - last.0).collect();
        assert_eq!(piece.to_lowercase(), "lantern");

        // case-insensitive, and a query of only spaces finds nothing
        assert_eq!(find_in_texts(&texts, "LANTERN").len(), 4);
        assert!(find_in_texts(&texts, "   ").is_empty());
        // overlapping occurrences are each a hit, so stepping never skips one
        let doubled = vec!["aaaa".to_string()];
        assert_eq!(find_in_texts(&doubled, "aa").len(), 3);
    }

    #[test]
    fn a_short_title_keeps_the_label_readable() {
        assert_eq!(short_title("Note", 18), "Note");
        assert_eq!(short_title("A very long page title indeed", 10), "A very lo…");
    }

    #[test]
    fn only_the_ctrl_combination_is_the_panel_shortcut() {
        let press = |ev: egui::Event| {
            let ctx = egui::Context::default();
            let mut raw = egui::RawInput::default();
            raw.events.push(ev);
            ctx.begin_pass(raw);
            let one = ctx.input_mut(|i| ctrl_key(i, egui::Modifiers::CTRL, egui::Key::Num1));
            let two = ctx.input_mut(|i| ctrl_key(i, egui::Modifiers::CTRL, egui::Key::Num2));
            let _ = ctx.end_pass();
            (one, two)
        };
        assert_eq!(press(key(egui::Key::Num1, egui::Modifiers::CTRL)), (true, false));
        assert_eq!(press(key(egui::Key::Num2, egui::Modifiers::CTRL)), (false, true));
        assert_eq!(
            press(key(egui::Key::Num1, egui::Modifiers::NONE)),
            (false, false),
            "a plain 1 is text, not a toggle"
        );
    }

    /// Every colour `tag_color` can pick has to stay readable on the light theme, where a tag chip
    /// is that colour at 14 % *behind its own text*: the lavender chosen for the dark themes is
    /// 2:1 there and made the one tag that hashed onto it unreadable.
    #[test]
    fn tag_colours_are_readable_on_paper() {
        let t = theme::PAPER;
        let mut seen = std::collections::HashSet::new();
        for i in 0..40 {
            let tag = format!("tag{i}");
            let c = tag_color(&t, &tag);
            seen.insert((c.r(), c.g(), c.b()));
            let wash = theme::over(c, 0.14, t.bg);
            let on_wash = theme::contrast(c, wash);
            let on_page = theme::contrast(c, t.bg);
            assert!(on_wash >= 4.0, "{tag}: {on_wash:.2}:1 on its chip");
            assert!(on_page >= 4.5, "{tag}: {on_page:.2}:1 on the page");
        }
        assert_eq!(seen.len(), 6, "all six palette colours should show up");
    }

    /// A state.json written by a build that had no panel flags must open both panels; only a
    /// stored `false` folds one away.
    #[test]
    fn an_older_state_file_leaves_both_panels_open() {
        let s: State =
            serde_json::from_str(r#"{"theme":"void","read_mode":false,"view":"pages"}"#).unwrap();
        assert_eq!(s.rail_open.unwrap_or(true), true);
        assert_eq!(s.pages_open.unwrap_or(true), true);

        let s: State = serde_json::from_str(r#"{"rail_open":false,"pages_open":true}"#).unwrap();
        assert_eq!(s.rail_open.unwrap_or(true), false);
        assert_eq!(s.pages_open.unwrap_or(true), true);
    }
}

impl NoteCraft {
    pub fn new(cc: &eframe::CreationContext<'_>, vault_root: PathBuf) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let state = load_state();
        let theme_idx = state
            .theme
            .and_then(|id| theme::themes().iter().position(|t| t.id == id))
            .unwrap_or(0);
        let t = theme::themes()[theme_idx];
        theme::apply(&cc.egui_ctx, &t);

        let vault = Vault::load(vault_root);
        dnd_log(&format!(
            "app started (backend {})",
            if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                "wayland"
            } else {
                "x11"
            }
        ));
        let last = state.last_page.map(PathBuf::from);
        let start_view = match state.view.as_deref() {
            Some("tasks") => View::Tasks,
            Some("tags") => View::Tags,
            _ => View::Pages,
        };
        let mut app = NoteCraft {
            vault,
            theme: t,
            theme_idx,
            applied_theme: Some(t.id),
            view: start_view,
            nb: None,
            sec: None,
            page: None,
            title: String::new(),
            body: String::new(),
            created: None,
            order: 0.0,
            dirty: false,
            last_edit: None,
            saved_at: None,
            read_mode: state.read_mode,
            read_mode_before_unreadable: None,
            rail_open: state.rail_open.unwrap_or(true),
            pages_open: state.pages_open.unwrap_or(true),
            img_center: state.img_center.unwrap_or(false),
            img_rounded: state.img_rounded.unwrap_or(false),
            img_radius: CornerStep::from_id(state.img_radius.as_deref()),
            note_text: NoteTextSize::from_id(state.note_text.as_deref()),
            note_font: theme::NoteFont::from_id(state.note_font.as_deref()),
            spell: spell::Dict::from_id(state.spell.as_deref()),
            spell_ignore: state.spell_ignore.unwrap_or_default(),
            spell_menu_word: None,
            spell_menu_range: None,
            spell_menu_fixes: Vec::new(),
            search: String::new(),
            palette: None,
            confirm: None,
            focus_title: false,
            status: String::new(),
            status_at: None,
            last_focused: true,
            drop_hover: false,
            image_cache: crate::imgcache::ImageCache::default(),
            selected_image: None,
            resizing: None,
            drag_rx: None,
            x11_window: None,
            #[cfg(windows)]
            win_dnd: None,
            #[cfg(windows)]
            win_dnd_tried: false,
            pending_snip: None,
            snip_request: false,
            snip: None,
            annotate: None,
            crop_edit: None,
            doc: doc::Doc::default(),
            selected_block: None,
            block_selection: None,
            last_sel: std::collections::HashMap::new(),
            block_sel: None,
            annotate_from_block: None,
            rename_for: None,
            rename_buf: String::new(),
            move_for: None,
            due_month: {
                let now = chrono::Local::now().date_naive();
                chrono::NaiveDate::from_ymd_opt(chrono::Datelike::year(&now), chrono::Datelike::month(&now), 1)
                    .unwrap_or(now)
            },
            due_pick: None,
            find: None,
            find_cache: None,
            title_sent: "NoteCraft".to_owned(),
            trash_open: false,
            task_status: TaskStatus::default(),
            task_due: TaskDue::default(),
            history: History::new(),
            scroll_to_block: None,
            scroll_to_bottom: false,
            scroll_top: false,
            page_scroll: None,
            pending_scroll: None,
            block_tops: Vec::new(),
            heading_tops: Vec::new(),
            view_line: None,
            caption_rows: Vec::new(),
            pending_land: None,
            keylayout: None,
            x11_layout: None,
            want_x11: None,
            keylayout_at: Some(Instant::now()),
        };
        // The note face is a process-wide switch read while a note is laid out: set it from the saved
        // state before the first frame, not only when the Display menu is used.
        theme::set_note_font(app.note_font);
        // the chip needs both halves at once: KWin's list and, if KWin has one layout, the X11 keymap
        app.refresh_keyboard();
        // restore last page, else first page in the vault
        let start = last
            .filter(|p| p.exists())
            .or_else(|| app.vault.pages.first().map(|p| p.path.clone()));
        if let Some(p) = start {
            // do not stomp the remembered view (tasks/tags) at startup
            app.open_page(&p, false);
        } else {
            app.ensure_default_vault();
        }
        app
    }

    fn ensure_default_vault(&mut self) {
        // A brand new vault gets a Welcome page so the window is never blank.
        if self.vault.pages.is_empty() {
            let _ = Ops::create_notebook(&self.vault.root, "Personal");
            let _ = Ops::create_section(&self.vault.root, "Personal", "Notes");
            if let Ok(path) = Ops::create_page(&self.vault.root, "Personal", "Notes", "Welcome") {
                let body = "Welcome to NoteCraft.\n\n## Getting around\n\n- **Ctrl+K** — command palette / search everything\n- **Ctrl+N** — new page in this section\n- **Ctrl+T** — tasks view (every open checkbox in the vault)\n- **Ctrl+E** — toggle read / edit\n- **Ctrl+Shift+V** — paste an image into the page\n- **Ctrl+Shift+T** — cycle theme\n\n## Todos work like this\n\n- [ ] write it as a checkbox anywhere\n- [ ] add a #tag in the line to group it\n- [ ] set a due date in the tasks view: click the date chip on the task\n- [x] tick it here or in the tasks view\n\nPages are plain markdown under the vault folder, so grep, backup and sync\nwork on them exactly as they do on any other file.\n";
                let _ = vault::write_page(&path, "Welcome", None, -100.0, body);
                self.vault.reload();
                // Open it: created-but-not-opened left the window saying "no notebook" and the page
                // list saying "No pages here yet" while the Welcome page sat in the vault.
                self.open_page(&path, false);
            }
        }
    }

    // ------------------------------------------------------------ page buffer

    fn open_page(&mut self, path: &Path, reset_view: bool) {
        self.selected_block = None;
        self.block_selection = None;
        self.last_sel.clear();
        self.block_sel = None;
        self.annotate_from_block = None;
        // Which page the history belongs to. The undo stack holds whole documents, so carrying it
        // across a page switch meant Ctrl+Z on the page you had just opened wrote the *previous*
        // page's words into this one (and the next autosave made that permanent).
        let previous_page = self.page.clone();
        if self.dirty {
            self.save();
        }
        if let Some(i) = self.vault.page_index(path) {
            let p = &self.vault.pages[i];
            let unreadable = p.unreadable;
            self.title = p.title.clone();
            self.body = p.body.clone();
            // the editor works on the document; a page without one is brought over from its markdown
            let mut d = doc::load(&path).unwrap_or_else(|| doc::from_markdown(&self.body));
            d.ensure_block();
            self.doc = d;
            if previous_page.as_deref() != Some(path) {
                self.history = History::new();
                // a note opens at its own top: the scroll area keeps its offset across a page
                // change, so reading one note to its end and opening another landed at that depth
                self.scroll_top = true;
            }
            self.created = p.created.map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            self.order = p.order;
            self.nb = Some(p.notebook.clone());
            self.sec = Some(p.section.clone());
            self.page = Some(p.path.clone());
            self.dirty = false;
            self.last_edit = None;
            // the time it was last written, or nothing: a placeholder word here read
            // "saved saved" in the page's meta row and in the status bar
            self.saved_at = p.updated.map(|d| d.format("%H:%M:%S").to_string());
            if unreadable {
                // shown, not edited: the file holds something this app cannot represent
                if self.read_mode_before_unreadable.is_none() {
                    self.read_mode_before_unreadable = Some(self.read_mode);
                }
                self.read_mode = true;
                dnd_log(&format!("open: {} is not UTF-8 text — read-only", path.display()));
                self.flash("this file is not UTF-8 text — shown read-only");
            } else if let Some(was) = self.read_mode_before_unreadable.take() {
                // the read-only page forced read mode on; a normal page gets the user's own mode back
                self.read_mode = was;
            }
            if reset_view {
                self.view = View::Pages;
            }
        } else {
            self.page = None;
            self.history = History::new();
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.last_edit = Some(Instant::now());
        self.saved_at = None;
    }

    /// Write the open page out. Returns whether it actually wrote, so Ctrl+S can report the truth
    /// instead of flashing "saved" over a failure message.
    fn save(&mut self) -> bool {
        let Some(path) = self.page.clone() else {
            return false;
        };
        // a page whose file is not text was loaded as empty: writing it back would destroy it
        if self
            .vault
            .page_index(&path)
            .map(|i| self.vault.pages[i].unreadable)
            .unwrap_or(false)
        {
            self.dirty = false;
            self.flash("this page is not UTF-8 text — not saving over it");
            return false;
        }
        if let Err(e) = vault::write_page(&path, &self.title, self.created.as_deref(), self.order, &self.body)
        {
            self.flash(format!("save failed: {e}"));
            return false;
        }
        // and the page's document, which is what the editor reads back
        let mut wrote_doc = true;
        if let Err(e) = doc::save(&path, &self.doc) {
            wrote_doc = false;
            self.flash(format!("document save failed: {e}"));
        }
        // keep the file name in step with the title
        let want = format!("{}.md", vault::sanitize(&self.title));
        let mut path = path;
        let mut renamed = false;
        if path.file_name().map(|f| f.to_string_lossy().to_string()) != Some(want.clone()) {
            let target = path.with_file_name(&want);
            if !target.exists() {
                if std::fs::rename(&path, &target).is_ok() {
                    doc::move_doc(&path, &target);
                    path = target;
                    self.page = Some(path.clone());
                    renamed = true;
                }
            }
        }
        if renamed {
            // Every cached path in the vault still names the old file. Without this rescan the page
            // list kept the old title in the row and, worse, that row still pointed at the old path:
            // opening it showed the page from the stale index and the next keystroke rebuilt the .md
            // under the old name, leaving two files that then drifted apart.
            self.vault.reload();
        }
        // refresh just this page in the index (no full vault rescan)
        if let Some(i) = self.vault.page_index(&path) {
            let (nb, sec) = (
                self.vault.pages[i].notebook.clone(),
                self.vault.pages[i].section.clone(),
            );
            self.vault.pages[i] = Page::read(&path, &nb, &sec);
            self.vault.pages[i].order = self.order;
            // re-sort the section order cache
            if let Some(nb_ref) = self
                .vault
                .notebooks
                .iter_mut()
                .find(|n| n.name == nb)
                .and_then(|n| n.sections.iter_mut().find(|s| s.name == sec))
            {
                nb_ref.pages.sort_by(|a, b| {
                    let pa = &self.vault.pages[*a];
                    let pb = &self.vault.pages[*b];
                    pa.order
                        .partial_cmp(&pb.order)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| pa.title.to_lowercase().cmp(&pb.title.to_lowercase()))
                });
            }
        }
        self.dirty = false;
        self.saved_at = Some(chrono::Local::now().format("%H:%M:%S").to_string());
        wrote_doc
    }

    /// Carry out a confirmed delete. Everything goes to `.trash` (never removed), and the open page is
    /// closed when it lived inside what was deleted.
    fn delete_confirmed(&mut self, what: &Confirm) {
        match what {
            Confirm::DeletePage => match self.move_for.clone() {
                Some(p) => match Ops::delete_page(&self.vault.root, &p) {
                    Ok(_) => {
                        if self.page.as_deref() == Some(p.as_path()) {
                            self.close_page();
                        }
                        self.vault.reload();
                        self.flash("page moved to .trash");
                    }
                    Err(e) => self.flash(format!("delete failed: {e}")),
                },
                // saying so beats a silent no-op: the dialog used to close with nothing deleted
                None => self.flash("nothing to delete — pick Delete again"),
            },
            Confirm::DeleteNotebook(name) => {
                let dir = self.vault.root.join(vault::sanitize(name));
                let inside = self.open_page_is_under(&dir);
                match Ops::delete_notebook(&self.vault.root, name) {
                    Ok(_) => {
                        if inside {
                            self.close_page();
                        }
                        self.vault.reload();
                        if self.nb.as_deref() == Some(name.as_str()) {
                            self.nb = self.vault.notebooks.first().map(|n| n.name.clone());
                            let mut first_sec = None;
                            if let Some(n) = self.nb.clone() {
                                if let Some(nbv) = self.vault.notebook(&n) {
                                    first_sec = nbv.sections.first().map(|s| s.name.clone());
                                }
                            }
                            self.sec = first_sec;
                        }
                        self.flash(format!("notebook “{name}” moved to .trash"));
                    }
                    Err(e) => self.flash(format!("delete failed: {e}")),
                }
            }
            Confirm::DeleteSection(nb, sec) => {
                let dir = self
                    .vault
                    .root
                    .join(vault::sanitize(nb))
                    .join(vault::sanitize(sec));
                let inside = self.open_page_is_under(&dir);
                match Ops::delete_section(&self.vault.root, nb, sec) {
                    Ok(_) => {
                        if inside {
                            self.close_page();
                        }
                        self.vault.reload();
                        if self.nb.as_deref() == Some(nb.as_str())
                            && self.sec.as_deref() == Some(sec.as_str())
                        {
                            let mut first_sec = None;
                            if let Some(nbv) = self.vault.notebook(nb) {
                                first_sec = nbv.sections.first().map(|s| s.name.clone());
                            }
                            self.sec = first_sec;
                        }
                        self.flash(format!("section “{sec}” moved to .trash"));
                    }
                    Err(e) => self.flash(format!("delete failed: {e}")),
                }
            }
        }
    }

    /// Drop the open page from the editor (its file is gone).
    fn close_page(&mut self) {
        self.page = None;
        self.title.clear();
        self.body.clear();
        self.doc = doc::Doc::default();
        self.selected_block = None;
        self.block_selection = None;
        self.last_sel.clear();
        self.block_sel = None;
        self.history = History::new();
        self.dirty = false;
    }

    /// The due-date picker: presets, a month calendar and Clear, for the task or blocks it was opened
    /// from. A small floating window rather than a menu, because egui closes a menu on any click
    /// inside it — a calendar in a menu shut itself the moment "<" was pressed.
    fn due_picker(&mut self, ctx: &egui::Context) {
        let Some(pick) = self.due_pick.clone() else {
            return;
        };
        let t = self.theme;
        let mut month = self.due_month;
        let mut chosen: Option<DueChoice> = None;
        let shown = egui::Window::new("due_picker")
            .id(egui::Id::new("due_picker"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .fixed_pos(pick.at)
            .show(ctx, |ui| {
                if let Some(choice) = due_rows(ui, pick.current, &mut month, &t) {
                    chosen = Some(choice);
                }
            });
        self.due_month = month;
        if let Some(p) = self.due_pick.as_mut() {
            p.fresh = false;
        }
        let mut close = chosen.is_some();
        if let Some(r) = shown {
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            if !pick.fresh && ctx.input(|i| i.pointer.any_click()) {
                let inside = ctx
                    .input(|i| i.pointer.interact_pos())
                    .map(|p| r.response.rect.contains(p))
                    .unwrap_or(false);
                if !inside {
                    close = true;
                }
            }
        }
        if let Some(choice) = chosen {
            self.apply_due(pick.target.clone(), choice);
        }
        if close {
            self.due_pick = None;
        }
    }

    /// Delete every done task in the vault. The lines as they were are copied into
    /// `<vault>/.trash/deleted-done-tasks-<stamp>.md` first, so a mis-click is recoverable — there is
    /// no other undo for a line.
    fn delete_done_tasks(&mut self) {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let mut removed: Vec<(String, String)> = Vec::new();
        let mut touched: Vec<PathBuf> = Vec::new();
        let mut skipped = 0usize;
        let pages: Vec<(PathBuf, String)> = self
            .vault
            .pages
            .iter()
            .map(|p| (p.path.clone(), format!("{} / {}", p.notebook, p.section)))
            .collect();
        for (path, where_) in pages {
            // the writer below reads the file, so an unsaved buffer would be rewritten away
            self.flush_open_page(&path);
            match strip_done_tasks(&path) {
                Ok(StripOutcome::Removed(title, lines)) => {
                    for text in lines {
                        removed.push((format!("{where_} / {title}"), text));
                    }
                    touched.push(path);
                }
                Ok(StripOutcome::OutOfStep) => skipped += 1,
                Ok(StripOutcome::Nothing) => {}
                Err(e) => {
                    self.flash(format!("could not tidy {}: {e}", path.display()));
                    return;
                }
            }
        }

        if removed.is_empty() {
            self.flash(if skipped > 0 {
                "no done tasks removed — their pages are out of step with their documents"
            } else {
                "no done tasks to remove"
            });
            return;
        }

        // the record, beside the pages deleted earlier: the vault's own .trash
        let trash = self.vault.root.join(".trash");
        let n = removed.len();
        let mut record = format!(
            "# done tasks deleted {}\n\n",
            chrono::Local::now().format("%d %b %Y %H:%M")
        );
        let mut last_page = String::new();
        for (page, text) in &removed {
            if *page != last_page {
                record.push_str(&format!("\n## {page}\n"));
                last_page = page.clone();
            }
            record.push_str(&format!("- [x] {text}\n"));
        }
        if std::fs::create_dir_all(&trash).is_ok() {
            let _ = std::fs::write(trash.join(format!("deleted-done-tasks-{stamp}.md")), record);
        }

        for path in touched {
            self.reload_page_from_disk(&path);
        }
        let plural = if n == 1 { "" } else { "s" };
        let skipped_note = if skipped > 0 {
            format!(
                ", {skipped} page{} left alone (document out of step)",
                if skipped == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        };
        self.flash(format!(
            "deleted {n} done task{plural} (kept in .trash){skipped_note}"
        ));
    }

    /// Put a due date on what the picker was opened for, or take it off — the tasks view's task, or
    /// the blocks the editor's menu was acting on.
    fn apply_due(&mut self, target: DueTarget, choice: DueChoice) {
        match target {
            DueTarget::Task { page, line } => {
                let due = match choice {
                    DueChoice::Set(d) => Some(d),
                    DueChoice::Clear => None,
                };
                // the task write reads the file; flush the open page so its buffer is not replaced
                self.flush_open_page(&page);
                match set_todo_due_in_page(&page, line, due) {
                    Ok(true) => self.reload_page_from_disk(&page),
                    Ok(false) => {
                        self.flash("that task is not in the page's document (edited by hand?)")
                    }
                    Err(e) => self.flash(format!("could not set the due date: {e}")),
                }
            }
            DueTarget::Blocks(blocks) => {
                if !blocks.is_empty() {
                    self.snapshot();
                }
                for i in blocks {
                    if let Some(b) = self.doc.blocks.get_mut(i) {
                        match choice {
                            DueChoice::Set(d) => doc::set_due(&mut b.runs, d),
                            DueChoice::Clear => doc::clear_due(&mut b.runs),
                        }
                        self.body = doc::to_markdown(&self.doc);
                        self.mark_dirty();
                    }
                }
            }
        }
    }

    /// Reload one page from disk after the tasks view changed it behind the app's back: the page
    /// list entry, and the buffers when it is the open page (one writer, two files).
    ///
    /// Callers that also *write* the page flush it first (`flush_open_page`): these writers read the
    /// file, so an unsaved buffer would otherwise be replaced by the stale disk copy.
    fn reload_page_from_disk(&mut self, path: &Path) {
        if let Some(i) = self.vault.page_index(path) {
            let (nb, sec) = (
                self.vault.pages[i].notebook.clone(),
                self.vault.pages[i].section.clone(),
            );
            self.vault.pages[i] = Page::read(path, &nb, &sec);
            if self.page.as_deref() == Some(path) {
                let body = self.vault.pages[i].body.clone();
                self.body = body.clone();
                self.doc = doc::load(path).unwrap_or_else(|| doc::from_markdown(&body));
                // the undo stack holds whole documents; after a reload from disk they are history
                self.history = History::new();
                self.dirty = false;
                self.last_edit = None;
            }
        }
    }

    /// Remember the document as it is, before a change is applied. The whole document is the step:
    /// blocks, kinds, runs and every property on them, so an undo restores all of it at once.
    fn snapshot(&mut self) {
        self.history.push(&self.doc);
    }

    /// Write the open page out if the path is the one on screen and there are unsaved changes.
    ///
    /// The tasks view and the due picker write straight into files, so anything they are about to
    /// touch has to be on disk first — otherwise their rewrite becomes the page's content and the
    /// buffer is silently thrown away.
    fn flush_open_page(&mut self, path: &Path) {
        if self.page.as_deref() == Some(path) && self.dirty {
            self.save();
        }
    }

    /// Typing is one step per burst, not one per keystroke: a snapshot is taken when the last one is
    /// older than `TYPING_STEP`, so Ctrl+Z after a sentence goes back a sentence.
    fn snapshot_typing(&mut self) {
        self.history.push_typing(&self.doc);
    }

    fn undo_doc(&mut self) {
        match self.history.undo(&self.doc) {
            Some(prev) => {
                self.doc = prev;
                self.after_undo();
            }
            None => self.flash("nothing to undo"),
        }
    }

    fn redo_doc(&mut self) {
        match self.history.redo(&self.doc) {
            Some(next) => {
                self.doc = next;
                self.after_undo();
            }
            None => self.flash("nothing to redo"),
        }
    }

    /// An undo replaces the document wholesale, so the marking and the caret no longer mean anything:
    /// drop them and let the page be saved as the restored version.
    fn after_undo(&mut self) {
        self.body = doc::to_markdown(&self.doc);
        self.block_selection = None;
        self.block_sel = None;
        self.last_sel.clear();
        self.selected_block = None;
        self.mark_dirty();
    }

    /// Open a page and land on the term that matched: the block that contains it becomes the
    /// selection, the words are marked, the caret goes there and the block is scrolled into view.
    /// This is what "open the result" has to mean — without it the words are somewhere in a long
    /// note and the reader hunts for them.
    fn land_on_hit(&mut self, ctx: &egui::Context, path: &Path, needle: &str, line: Option<usize>) {
        self.open_page(path, true);
        // a page that is not UTF-8 text is shown read-only; do not hand it the editor, where every
        // keystroke would land in an empty buffer and be refused at save time
        let unreadable = self
            .vault
            .page_index(path)
            .map(|i| self.vault.pages[i].unreadable)
            .unwrap_or(false);
        if !unreadable {
            self.read_mode = false;
        }
        let needle_l = needle.trim().to_lowercase();
        if needle_l.is_empty() {
            return;
        }
        // which occurrence of the word this hit is: the same word can sit on several lines, and the
        // row was built from one particular line of the body
        let mut nth = 0usize;
        if let Some(target) = line {
            for l in self.body.split('\n').take(target) {
                nth += l.to_lowercase().matches(&needle_l).count();
            }
        }
        let mut found: Option<(usize, (usize, usize))> = None;
        if let Some(occ) = self.find_in_doc(&needle_l).get(nth) {
            found = Some(*occ);
        }
        let Some((i, range)) = found else {
            self.flash(format!("opened the page — \"{needle}\" is in the title or a tag"));
            return;
        };
        let _ = ctx;
        self.land_on_block_range(i, range);
    }

    /// Every occurrence of `needle` in the open document, in reading order, as
    /// `(block, char range)`.
    ///
    /// The range is in *characters*, which is what egui's cursor wants. Offsets are found in the
    /// lowercased text and only used when lowering did not change the length, otherwise a page with
    /// a character such as `İ` would report ranges that point into the middle of another character.
    /// Where `needle` sits in the document that is open: `(block, char range within the block)`.
    ///
    /// Matching runs on the block's *text*, not on the markdown, so a search for `#` or `-` cannot
    /// land on a marker the reader cannot see.
    fn find_in_doc(&self, needle: &str) -> Vec<(usize, (usize, usize))> {
        let texts: Vec<String> = self.doc.blocks.iter().map(|b| b.text()).collect();
        find_in_texts(&texts, needle)
    }

    /// Every match in the scope, in vault order (section order inside a section).
    ///
    /// The order is the vault's, not "the open page first": walking the hits reopens pages, and a
    /// list that reordered itself around whichever page was open made Enter jump somewhere new
    /// every time. The open page contributes its matches from the live document, so unsaved words
    /// are searchable; the rest come from `scope_page_texts`.
    fn find_in_scope(&mut self, scope: FindScope, needle: &str) -> Vec<FindHit> {
        let mut out = Vec::new();
        if needle.trim().is_empty() {
            return out;
        }
        self.ensure_find_cache(scope);
        let open = self.page.clone();
        let live = self.find_in_doc(needle);
        let Some(cache) = self.find_cache.as_ref() else {
            return out;
        };
        for (path, title, texts) in &cache.pages {
            let ranges = if Some(path.as_path()) == open.as_deref() {
                live.clone()
            } else {
                find_in_texts(texts, needle)
            };
            for (block, range) in ranges {
                out.push(FindHit {
                    page: path.clone(),
                    page_title: title.clone(),
                    block,
                    range,
                });
            }
        }
        out
    }

    /// Fill `find_cache` with the block texts of the pages in `scope`, unless the copy in hand is
    /// still good for it (same scope, same anchor, same vault generation).
    fn ensure_find_cache(&mut self, scope: FindScope) {
        let anchor = format!("{:?}/{:?}", self.nb, self.sec);
        let fresh = self
            .find_cache
            .as_ref()
            .map(|c| {
                c.scope == scope && c.anchor == anchor && c.generation == self.vault.generation
            })
            .unwrap_or(false);
        if fresh {
            return;
        }
        let mut pages = Vec::new();
        for p in &self.vault.pages {
            if p.unreadable {
                continue;
            }
            if !page_in_scope(&p.notebook, &p.section, scope, self.nb.as_deref(), self.sec.as_deref())
            {
                continue;
            }
            // the document, not the markdown: the same matching rule as the page that is open, and
            // landing needs the document's own block indices
            let d = doc::load(&p.path).unwrap_or_else(|| doc::from_markdown(&p.body));
            let texts: Vec<String> = d.blocks.iter().map(|b| b.text()).collect();
            pages.push((p.path.clone(), p.title.clone(), texts));
        }
        self.find_cache = Some(FindCache {
            scope,
            anchor,
            generation: self.vault.generation,
            pages,
        });
    }

    /// Jump to a hit, opening its page first when it is somewhere else.
    fn land_on_find_hit(&mut self, hit: &FindHit) {
        if self.page.as_deref() != Some(hit.page.as_path()) {
            self.open_page(&hit.page, false);
        }
        // the page may have changed since the hit list was built: never index past its blocks
        let last = self.doc.blocks.len().saturating_sub(1);
        self.land_on_block_range(hit.block.min(last), hit.range);
    }

    /// Put the caret on `range` in block `i` and bring that block into view.
    fn land_on_block_range(&mut self, i: usize, range: (usize, usize)) {
        self.pending_land = Some((i, range));
        self.selected_block = Some(i);
        self.block_sel = Some((i, i));
        self.block_selection = Some(range);
        self.last_sel.insert(i, range);
        self.scroll_to_block = Some(i);
    }

    /// Whether the open page lives under `dir`.
    fn open_page_is_under(&self, dir: &Path) -> bool {
        self.page.as_ref().map(|p| p.starts_with(dir)).unwrap_or(false)
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.status = msg.into();
        self.status_at = Some(Instant::now());
    }

    // ------------------------------------------------------------ actions

    fn new_page(&mut self) {
        let (nb, sec) = match (self.nb.clone(), self.sec.clone()) {
            (Some(n), Some(s)) => (n, s),
            _ => {
                let _ = Ops::create_notebook(&self.vault.root, "Personal");
                let _ = Ops::create_section(&self.vault.root, "Personal", "Notes");
                ("Personal".to_string(), "Notes".to_string())
            }
        };
        let max_order = self
            .vault
            .section_pages(&nb, &sec)
            .iter()
            .filter_map(|i| Some(self.vault.pages[*i].order))
            .fold(0.0f32, f32::max);
        match Ops::create_page(&self.vault.root, &nb, &sec, "Untitled page") {
            Ok(path) => {
                let _ = Ops::set_order(&path, max_order + 1.0);
                self.vault.reload();
                self.open_page(&path, false);
                self.focus_title = true;
                self.flash("new page");
            }
            Err(e) => self.flash(format!("could not create page: {e}")),
        }
    }

    fn new_section(&mut self) {
        let nb = self.nb.clone().unwrap_or_else(|| "Personal".into());
        let _ = Ops::create_notebook(&self.vault.root, &nb);
        let name = format!("Section {}", self.vault.notebook(&nb).map(|n| n.sections.len() + 1).unwrap_or(1));
        let _ = Ops::create_section(&self.vault.root, &nb, &name);
        self.vault.reload();
        self.sec = Some(name);
        self.flash("new section");
    }

    fn run_command(&mut self, cmd: &str) {
        match cmd {
            "New page" => self.new_page(),
            "New section" => self.new_section(),
            "New notebook" => {
                let name = format!("Notebook {}", self.vault.notebooks.len() + 1);
                let _ = Ops::create_notebook(&self.vault.root, &name);
                let _ = Ops::create_section(&self.vault.root, &name, "Notes");
                self.vault.reload();
                self.nb = Some(name);
                self.flash("new notebook");
            }
            "Tasks view" => self.view = View::Tasks,
            "Tags view" => self.view = View::Tags,
            "Pages view" => self.view = View::Pages,
            "Toggle read / edit" => self.read_mode = !self.read_mode,
            "Cycle theme" => self.cycle_theme(),
            "Reload vault" => {
                self.vault.reload();
                self.flash("vault reloaded");
            }
            "Save page" => {
                if self.save() {
                    self.flash("saved");
                }
            }
            "Toggle notebooks panel" => self.rail_open = !self.rail_open,
            "Toggle page list" => self.pages_open = !self.pages_open,
            "Find in page" => self.open_find_bar(),
            "Duplicate page" => match self.page.clone() {
                Some(p) => match Ops::duplicate_page(&p) {
                    Ok(newp) => {
                        self.vault.reload();
                        self.open_page(&newp, false);
                        self.focus_title = true;
                        self.flash("duplicated — rename the copy if you like");
                    }
                    Err(e) => self.flash(format!("duplicate failed: {e}")),
                },
                None => self.flash("open a page first"),
            },
            "Restore deleted…" => self.trash_open = true,
            "Open vault folder" => {
                let _ = crate::platform::open_path(&self.vault.root);
            }
            _ => {}
        }
    }

    fn cycle_theme(&mut self) {
        self.theme_idx = (self.theme_idx + 1) % theme::themes().len();
        self.theme = theme::themes()[self.theme_idx];
        self.flash(format!("theme: {}", self.theme.label));
    }

    /// Write state.json as soon as a display preference changes, so it survives a crash as well as
    /// a clean exit. Only cheap, rarely-pressed settings go through this — never anything per frame.
    fn save_state_soon(&self) {
        save_state(self);
    }

    /// Push the current theme into egui's style whenever it has actually changed. `theme::apply`
    /// is the only thing that writes `Visuals`, and egui's own widgets read those rather than the
    /// `Theme` tokens — so without this a cycle only repainted what the app draws itself, and on
    /// Paper the one thing left in Void's colours was the note title's `TextEdit`: pale text on a
    /// light page, the most prominent text in the window. Guarded rather than called from each
    /// switch so a future path that sets `self.theme` cannot forget it.
    fn sync_theme(&mut self, ctx: &egui::Context) {
        if self.applied_theme != Some(self.theme.id) {
            theme::apply(ctx, &self.theme);
            self.applied_theme = Some(self.theme.id);
        }
    }

    // ------------------------------------------------------------ frame

    /// Global attachment intake: Ctrl+V (an image or files on the clipboard) and
    /// dropped files, in every view and in read mode.
    ///
    /// Drag and drop only fires where winit implements it: its Wayland backend
    /// has no data-device code at all, so on a Wayland session the Ctrl+V route
    /// is the one that can work. The drop handler stays for X11/Windows.
    fn global_attachments(&mut self, ctx: &egui::Context) {
        // Highlight the drop target while a file is held over the window. Only
        // X11/Windows deliver these events (winit's Wayland backend has no
        // data-device support), so on Wayland this stays off.
        let hovered: Vec<String> = ctx.input(|i| {
            i.raw
                .hovered_files
                .iter()
                .map(|f| {
                    f.path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| f.mime.clone())
                })
                .collect()
        });
        let was_hovering = self.drop_hover;
        self.drop_hover = !hovered.is_empty();
        if self.drop_hover && !was_hovering {
            // winit only emits HoveredFile once it has fetched the uri-list, so
            // seeing this means the drag really reached us with usable data.
            let msg = format!("hover: {} item(s) {}", hovered.len(), hovered.join(", "));
            dnd_log(&msg);
            self.flash("drag detected — release to attach");
        } else if was_hovering && !self.drop_hover {
            dnd_log("hover cancelled (drag left the window, or the drop was rejected)");
        }

        let dropped: Vec<PathBuf> = ctx
            .input_mut(|i| std::mem::take(&mut i.raw.dropped_files))
            .into_iter()
            .filter_map(|f| f.path)
            .collect();
        if !dropped.is_empty() {
            dnd_log(&format!(
                "drop: {} file(s): {}",
                dropped.len(),
                dropped
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            self.attach_paths(&dropped);
        }

        let paste = ctx.input(|i| {
            (i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(egui::Key::V)
        });
        if paste && self.paste_from_clipboard() {
            // swallow the key so the text editor does not paste as well
            ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::V);
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::V);
            });
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        // Shift combinations are consumed FIRST: `consume_key(CTRL, Key::S)` also
        // matches Ctrl+Shift+S, so a plain binding listed before its shifted variant
        // swallows it (Ctrl+Shift+S was saving the page instead of grabbing a region).
        let (shift_z, undo, redo, shift_a, shift_c, shift_t, shift_s, k, n, t, e, f, s, one, two) =
            ctx.input_mut(|i| {
            (
                ctrl_key(
                    i,
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::Z,
                ),
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::Z),
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::Y),
                ctrl_key(
                    i,
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::A,
                ),
                ctrl_key(
                    i,
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::X,
                ),
                ctrl_key(
                    i,
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::T,
                ),
                ctrl_key(
                    i,
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::S,
                ),
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::K),
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::N),
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::T),
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::E),
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::F),
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::S),
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::Num1),
                ctrl_key(i, egui::Modifiers::CTRL, egui::Key::Num2),
            )
        });
        if undo && !shift_z {
            self.undo_doc();
        }
        if shift_z || redo {
            self.redo_doc();
        }
        if one {
            self.rail_open = !self.rail_open;
        }
        if two {
            self.pages_open = !self.pages_open;
        }
        if t && !shift_t {
            self.view = View::Tasks;
        }
        if shift_t {
            // theme only: cycling while triaging tasks used to throw the user back to Pages
            self.cycle_theme();
        }
        if k {
            self.palette = Some(Palette {
                query: self.search.clone(),
                sel: 0,
                focus: true,
            });
        }
        if n {
            self.new_page();
        }
        if e {
            self.read_mode = !self.read_mode;
        }
        if shift_s {
            self.snip_region();
        }
        if shift_a && self.annotate.is_none() {
            // the selected picture, else the first one in the page — blocks, like the editor
            let selected = self
                .selected_image
                .filter(|i| self.doc.blocks.get(*i).map(|b| b.kind == doc::Kind::Image).unwrap_or(false));
            match selected.or_else(|| self.doc.blocks.iter().position(|b| b.kind == doc::Kind::Image)) {
                Some(i) => {
                    let reference = self.doc.blocks[i].image.clone();
                    self.open_annotate_block(i, &reference);
                }
                None => self.flash("no picture in this note to draw on"),
            }
        }
        if shift_c && self.crop_edit.is_none() {
            // the same rule as annotate: the selected picture, else the first one in the page
            let selected = self
                .selected_image
                .filter(|i| self.doc.blocks.get(*i).map(|b| b.kind == doc::Kind::Image).unwrap_or(false));
            match selected.or_else(|| self.doc.blocks.iter().position(|b| b.kind == doc::Kind::Image)) {
                Some(i) => {
                    let reference = self.doc.blocks[i].image.clone();
                    self.open_crop(i, &reference);
                }
                None => self.flash("no picture in this note to crop"),
            }
        }
        if s && !shift_s {
            // save() flashes its own failure; "saved" over it was a lie the status bar contradicted
            if self.save() {
                self.flash("saved");
            }
        }
        if f {
            // Find means the open page when there is one — that is what Ctrl+F does in every
            // editor. With no page open it falls back to the section filter, which is where it
            // used to always go.
            // Ctrl+F with the bar already open only takes the query back: clearing what was typed
            // would make "search for this other word" mean retyping it from nothing
            self.open_find_bar();
        }
    }

    /// Home / End / PgUp / PgDn move through the open note: its top, its end, and a screen at a
    /// time — the keys every editor has, and they never leave the note (they are not the page
    /// list's). They are taken before any widget draws, so a caret in a block does not keep `End`
    /// for the end of its line (`consume_key` removes the event, so egui's TextEdit never sees it).
    /// The short fields of their own — the page filter, the title, the find bar, a rename — keep
    /// them, and a modal keeps everything.
    fn page_scroll_keys(&mut self, ctx: &egui::Context) {
        if self.view != View::Pages || !self.scroll_keys_free(ctx) {
            return;
        }
        let (up, down, top, bottom) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Home),
                i.consume_key(egui::Modifiers::NONE, egui::Key::End),
            )
        });
        let Some(what) = [
            (top, ScrollTo::Top),
            (bottom, ScrollTo::Bottom),
            (up, ScrollTo::PageUp),
            (down, ScrollTo::PageDown),
        ]
        .into_iter()
        .find_map(|(down, what)| down.then_some(what))
        else {
            return;
        };
        // The metrics are what the *last* frame drew; before the first draw there is nothing to
        // measure, and the note has not moved anyway.
        let Some(m) = self.page_scroll else {
            return;
        };
        let target = scroll_target(m.offset, m.view, m.content, what);
        if (target - m.offset).abs() < 1.0 {
            // Already there: say so at the ends, stay quiet in the middle (a note shorter than the
            // window has nowhere to go, which is what every editor does too).
            let max = (m.content - m.view).max(0.0);
            if max > 1.0 {
                if target <= 0.5 {
                    self.flash("top of the note");
                } else if target >= max - 0.5 {
                    self.flash("end of the note");
                }
            }
            return;
        }
        self.pending_scroll = Some(target);
    }

    /// Whether the scroll keys may take Home/End/PgUp/PgDn this frame: nothing modal is open, and
    /// the focused widget is not a short field of its own.
    fn scroll_keys_free(&self, ctx: &egui::Context) -> bool {
        if self.palette.is_some()
            || self.confirm.is_some()
            || self.due_pick.is_some()
            || self.snip.is_some()
            || self.annotate.is_some()
            || self.crop_edit.is_some()
            || self.trash_open
            || self.rename_for.is_some()
            || self.move_for.is_some()
            || self.page.is_none()
        {
            return false;
        }
        match ctx.memory(|m| m.focused()) {
            None => true,
            // a note block belongs to the note; anything else focused owns the keys for its own line
            Some(id) => self
                .doc
                .blocks
                .iter()
                .enumerate()
                .any(|(i, _)| egui::Id::new(("block", i)) == id),
        }
    }

    /// Typing while the page is read-only is a dead end: `read_body()` draws no
    /// text edit, so there is no caret to aim at and every keystroke vanishes —
    /// which is read as "the cursor is missing, typing does not work". Treat a
    /// keystroke that no widget wanted as "I mean to edit this note".
    fn typing_wants_edit(&mut self, ctx: &egui::Context) {
        if !self.read_mode
            || self.palette.is_some()
            || self.confirm.is_some()
            || self.rename_for.is_some()
            || self.move_for.is_some()
        {
            return;
        }
        // Only when nothing has keyboard focus — a keystroke into the search field
        // or the title is not a request to start editing. The stray text is taken
        // out of the event queue and kept: no widget wanted it, and losing the
        // character that woke the editor up looks exactly like "typing is broken".
        let mut stray_text = String::new();
        let stray = ctx.memory(|m| m.focused()).is_none()
            && ctx.input_mut(|i| {
                let mut got = false;
                i.events.retain(|e| match e {
                    egui::Event::Text(t) if t.chars().any(|c| !c.is_control()) => {
                        stray_text.push_str(t);
                        got = true;
                        false
                    }
                    egui::Event::Paste(s) => {
                        stray_text.push_str(s);
                        got = true;
                        false
                    }
                    _ => true,
                });
                got
            });
        if stray {
            self.read_mode = false;
            if !stray_text.is_empty() {
                // The character belongs to the document: a keystroke appended to the markdown
                // export alone was dropped the next time the export was regenerated from the
                // document — which is what made typing in read mode look half-broken.
                //
                // Only a *text* block can take it. A trailing picture swallowed the character into
                // its caption (`![alt+chars](…)`), and a rule discards its runs entirely — accepted,
                // saved, and never shown anywhere.
                let takes_text = self
                    .doc
                    .blocks
                    .last()
                    .map(|b| !matches!(b.kind, doc::Kind::Image | doc::Kind::Rule | doc::Kind::Code))
                    .unwrap_or(false);
                if takes_text {
                    if let Some(b) = self.doc.blocks.last_mut() {
                        b.runs.push(doc::Run::plain(stray_text.clone()));
                    }
                } else {
                    // a fresh paragraph after the picture/rule, so the words have somewhere to go
                    self.doc
                        .blocks
                        .push(doc::Block::plain(doc::Kind::Para, stray_text.clone()));
                }
                self.body = doc::to_markdown(&self.doc);
                self.mark_dirty();
            }
            // Claim the caret *before* the body is drawn, so the next keystroke goes straight in:
            // a TextEdit reads text events only while it has focus, and focus requested after it
            // drew arrives a frame too late. Blocks, not the editor segments that no longer exist.
            if !self.doc.blocks.is_empty() {
                let last = self.doc.blocks.len() - 1;
                ctx.memory_mut(|m| m.request_focus(egui::Id::new(("block", last))));
            }
            dnd_log("read mode off: a keystroke arrived with nothing focused");
            self.flash("editing — read mode was on (Ctrl+E switches)");
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        // Cheap heartbeat: repaint once a second even while idle. Seen on this
        // box (X11/XWayland, VRAM ~96% full): the GL surface can end up black or
        // stale and, because an idle egui app only repaints on events, the window
        // then stays black until something else forces a frame. A 1 Hz repaint
        // lets it heal itself.
        ctx.request_repaint_after(Duration::from_secs(1));
        // A grab in flight is polled here: tick() runs every frame, and the 1 Hz
        // heartbeat means a snip still lands while the window is unfocused.
        self.poll_snip();

        let focused = ctx.input(|i| i.focused);
        if focused && !self.last_focused {
            // returning to the window: flush, then pick up outside edits
            if self.dirty {
                self.save();
            }
            self.vault.reload();
            if let Some(p) = self.page.clone() {
                if !self.dirty {
                    self.open_page(&p, false);
                }
            }
            // the layout can have been changed while we were away (KDE's own shortcut, the panel
            // applet), and the status bar says which one is live
            self.refresh_keyboard();
        }
        self.last_focused = focused;

        // KWin can switch the layout from anywhere, so the readout is re-read now and then. It costs
        // a `qdbus6` process (~20 ms), which at 3 s was 0.8 % of a core — so it is 5 s, only while
        // the window has focus, and immediately whenever focus arrives or the toggle is clicked.
        let stale = self
            .keylayout_at
            .map(|t| t.elapsed() > Duration::from_secs(5))
            .unwrap_or(true);
        if stale && crate::platform::has_keyboard_layout_toggle() && (focused || self.keylayout.is_none()) {
            self.refresh_keyboard();
        }

        if self.dirty {
            let quiet = self
                .last_edit
                .map(|t| t.elapsed() > Duration::from_millis(900))
                .unwrap_or(true);
            if quiet {
                self.save();
            } else {
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }
        // The drag-out runs on its own thread; what it has to say lands in the status bar, and a
        // closed channel means it has finished.
        if let Some(rx) = self.drag_rx.take() {
            let mut done = false;
            loop {
                match rx.try_recv() {
                    Ok(msg) => {
                        if !msg.is_empty() {
                            dnd_log(&format!("drag out: {msg}"));
                            self.flash(msg);
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
            }
            if !done {
                self.drag_rx = Some(rx);
            }
        }
        if self
            .status_at
            .map(|t| t.elapsed() > Duration::from_secs(6))
            .unwrap_or(false)
        {
            self.status.clear();
            self.status_at = None;
        }
    }

    /// The find-scope row, always on screen under the toolbar.
    ///
    /// The same three-way toggle lives inside the find bar, but a control that only exists while a
    /// popup is open is a control half the users never find — this one is part of the window, and
    /// clicking a scope opens the bar in that scope so the row explains itself.
    fn find_scope_row(&mut self, ctx: &egui::Context) {
        // Find mode is the row *and* the bar: the toolbar's Find button turns both on and off, so
        // the note gets the 32px back the moment you stop searching.
        let Some(scope_now) = self.find.as_ref().map(|f| f.scope) else {
            return;
        };
        let t = self.theme;
        let mut picked: Option<FindScope> = None;
        let current = scope_now;
        egui::TopBottomPanel::top("findscope")
            .exact_height(32.0)
            .frame(
                egui::Frame::new()
                    .fill(t.panel)
                    .inner_margin(Margin::symmetric(14, 3)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("find in").size(11.0).color(t.faint));
                    egui::Frame::new()
                        .fill(t.panel2)
                        .stroke(Stroke::new(1.0_f32, t.border))
                        .corner_radius(CornerRadius::same(9))
                        .inner_margin(Margin::symmetric(3, 1))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                for s in
                                    // widest first — All · Notebook · Section — as the user asked
                                    [FindScope::Vault, FindScope::Notebook, FindScope::Section]
                                {
                                    if theme::segment_button(ui, s.label(), current == s, &t)
                                        .on_hover_text(s.hint())
                                        .clicked()
                                    {
                                        picked = Some(s);
                                    }
                                    ui.add_space(1.0);
                                }
                            });
                        });
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new("Enter next · Shift+Enter back · Esc closes")
                            .size(11.0)
                            .color(t.faint),
                    );
                });
            });
        if let Some(scope) = picked {
            // a new scope is a new search: reset the walk and hand the keyboard back to the field
            if let Some(f) = self.find.as_mut() {
                f.scope = scope;
                f.focus = true;
                f.current = 0;
                f.landed = false;
            }
        }
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        let t = self.theme;
        let mut attach_clicked = false;
        let mut paste_clicked = false;
        let mut snip_clicked = false;
        let mut find_clicked = false;
        let mut export: Option<ExFmt> = None;
        egui::TopBottomPanel::top("top")
            .exact_height(52.0)
            .frame(
                egui::Frame::new()
                    .fill(t.panel)
                    .inner_margin(Margin::symmetric(14, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // The app's own title first, as asked: the wordmark and the place you are in.
                    let mark = egui::Frame::new()
                        .fill(t.accent.gamma_multiply(0.18))
                        .corner_radius(CornerRadius::same(9))
                        .inner_margin(Margin::symmetric(8, 4));
                    mark.show(ui, |ui| {
                        ui.label(
                            RichText::new("NOTE")
                                .family(theme::fam(theme::F_BOLD))
                                .size(13.0)
                                .color(t.accent),
                        );
                    });
                    ui.add_space(4.0);
                    let crumb = match (&self.nb, &self.sec) {
                        (Some(n), Some(s)) => format!("{n}  /  {s}"),
                        (Some(n), None) => n.clone(),
                        _ => "no notebook".into(),
                    };
                    // The breadcrumb is the widest and least essential thing in the bar — it is also
                    // in the window title — and it used to be an unbounded label drawn *before* the
                    // button cluster, which is laid out into whatever is left: at 780 pt the cluster
                    // overflowed leftwards and "Personal / Notes" printed through `Edit` and `Find`.
                    // It gets a bounded slot, and no slot at all when the window is narrow.
                    let room = (ui.available_width() - TOOLBAR_TAIL_W).max(0.0);
                    // `Find` is a second copy of Ctrl+F, so it is the first thing to go when the bar
                    // cannot hold the button cluster *and* it: at 780 pt the cluster overflowed
                    // leftwards and printed through the word "Find" itself.
                    let find_room = room > 50.0;
                    let crumb_room = (room - if find_room { 70.0 } else { 8.0 }).max(0.0);
                    let crumb_w = text_w(ui, &crumb, 13.0, false).min(crumb_room).min(320.0);
                    if crumb_w > 40.0 {
                        ui.add_sized(
                            [crumb_w, 22.0],
                            egui::Label::new(
                                RichText::new(crumb)
                                    .family(theme::fam(theme::F_MED))
                                    .size(13.0)
                                    .color(t.dim),
                            )
                            .truncate(),
                        );
                    }
                    ui.add_space(10.0);
                    // then the find toggle, then the rest
                    let find_on = self.find.is_some();
                    if find_room
                        && theme::segment_button(ui, "Find", find_on, &t)
                            .on_hover_text(
                                "show / hide the find row: scope, field and counter (Ctrl+F, Esc)",
                            )
                            .clicked()
                    {
                        find_clicked = true;
                    }
                    ui.add_space(8.0);

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_button(ui, "◑", "cycle theme (Ctrl+Shift+T)", &t).clicked() {
                            self.cycle_theme();
                        }
                        // How the note is drawn: pictures centred / with rounded corners, and the
                        // size of its text. These are display preferences, so they live in the bar
                        // and survive a restart — a page never carries them.
                        // `CloseOnClickOutside`, not the default `CloseOnClick`: the two picture
                        // toggles are independent, so turning on both must not mean opening the
                        // menu twice. It stays up until the pointer goes elsewhere (or Esc).
                        let _ = egui::containers::menu::MenuButton::new("Display")
                            .config(
                                egui::containers::menu::MenuConfig::new()
                                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                            )
                            .ui(ui, |ui| {
                            // A fixed width, not a minimum: every row in here is as wide as the menu
                            // (`ui.available_width()`), so a minimum let the popup grow into whatever
                            // the screen had left — the note-font rows made that visibly wrong.
                            ui.set_width(196.0);
                            if menu_toggle(ui, self.img_center, "Centre pictures", &t) {
                                self.img_center = !self.img_center;
                                self.save_state_soon();
                            }
                            if menu_toggle(ui, self.img_rounded, "Rounded corners", &t) {
                                self.img_rounded = !self.img_rounded;
                                self.save_state_soon();
                            }
                            // How round the corners are is only a question while rounding is on, so
                            // the row appears with the toggle and goes with it.
                            if self.img_rounded {
                                if let Some(step) = step_row(
                                    ui,
                                    "corners",
                                    self.img_radius,
                                    [
                                        ("S", CornerStep::Small, "small corner radius"),
                                        ("M", CornerStep::Medium, "medium corner radius"),
                                        ("L", CornerStep::Large, "large corner radius"),
                                    ],
                                    &t,
                                ) {
                                    self.img_radius = step;
                                    self.save_state_soon();
                                }
                            }
                            ui.separator();
                            if let Some(size) = step_row(
                                ui,
                                "text size",
                                self.note_text,
                                [
                                    ("S", NoteTextSize::Small, "small note text"),
                                    ("M", NoteTextSize::Medium, "medium note text"),
                                    ("L", NoteTextSize::Large, "large note text (the default)"),
                                ],
                                &t,
                            ) {
                                self.note_text = size;
                                self.save_state_soon();
                            }
                            ui.separator();
                            if let Some(face) = menu_fonts(ui, self.note_font, &t) {
                                self.note_font = face;
                                // every note laid out from here on, this frame included, reads the
                                // switch — so the note behind the menu re-sets itself as it is picked
                                theme::set_note_font(face);
                                self.save_state_soon();
                            }
                            ui.separator();
                            // Spelling: off by default, and either side of the Atlantic. The list a
                            // note is checked against is a display preference like the face — it says
                            // nothing about the note itself, so it lives in state.json, not the file.
                            // The label sits on its own line and the chips below it: "British" and
                            // "American" together are wider than the menu, and squeezing them in
                            // beside a label wrapped "American" onto two lines.
                            ui.label(
                                RichText::new("spelling")
                                    .family(theme::fam(theme::F_MONO))
                                    .size(11.0)
                                    .color(t.faint),
                            );
                            ui.horizontal(|ui| {
                                for (label, dict, tip) in [
                                    ("Off", spell::Dict::Off, "no spellchecking"),
                                    (
                                        "British",
                                        spell::Dict::British,
                                        "British English: colour, realise, centre",
                                    ),
                                    (
                                        "American",
                                        spell::Dict::American,
                                        "American English: color, realize, center",
                                    ),
                                ] {
                                    let on = self.spell == dict;
                                    if theme::segment_button(ui, label, on, &t)
                                        .on_hover_text(tip)
                                        .clicked()
                                        && !on
                                    {
                                        self.spell = dict;
                                        self.save_state_soon();
                                    }
                                }
                            });
                            });
                        ui.menu_button("Export", |ui| {
                            if ui.button("HTML (single file)…").clicked() {
                                export = Some(ExFmt::Html);
                                ui.close();
                            }
                            if ui.button("PDF…").clicked() {
                                export = Some(ExFmt::Pdf);
                                ui.close();
                            }
                            if ui.button("Plain text…").clicked() {
                                export = Some(ExFmt::Text);
                                ui.close();
                            }
                            if ui.button("Copy as Markdown").clicked() {
                                export = Some(ExFmt::CopyMd);
                                ui.close();
                            }
                        });
                        bar_rule(ui, &t);
                        if theme::toolbar_button(ui, "Snip", false, &t)
                            .on_hover_text("drag a region of the screen into this note (Ctrl+Shift+S)")
                            .clicked()
                        {
                            // handled after the panel: the grabber is a separate process
                            snip_clicked = true;
                        }
                        if theme::toolbar_button(ui, "Attach", false, &t).clicked() {
                            // handled after the panel: the chooser blocks
                            attach_clicked = true;
                        }
                        if theme::toolbar_button(ui, "Paste", false, &t).clicked() {
                            paste_clicked = true;
                        }
                        bar_rule(ui, &t);
                        let (pages_active, tasks_active, tags_active) = (
                            self.view == View::Pages,
                            self.view == View::Tasks,
                            self.view == View::Tags,
                        );
                        if theme::toolbar_button(ui, "Tags", tags_active, &t).clicked() {
                            self.view = View::Tags;
                        }
                        if theme::toolbar_button(ui, "Tasks", tasks_active, &t).clicked() {
                            self.view = View::Tasks;
                        }
                        if theme::toolbar_button(ui, "Pages", pages_active, &t).clicked() {
                            self.view = View::Pages;
                        }
                        bar_rule(ui, &t);
                        if theme::toolbar_button(
                            ui,
                            if self.read_mode { "Edit" } else { "Read" },
                            self.read_mode,
                            &t,
                        )
                        .clicked()
                        {
                            self.read_mode = !self.read_mode;
                        }
                    });
                });
            });
        if attach_clicked {
            self.attach_dialog();
        }
        if snip_clicked {
            self.snip_region();
        }
        if paste_clicked && !self.paste_from_clipboard() {
            self.flash("no image or file on the clipboard");
        }
        if find_clicked {
            if self.find.is_some() {
                self.find = None;
                self.find_cache = None;
            } else {
                self.open_find_bar();
            }
        }
        if let Some(how) = export {
            self.export_page(ctx, how);
        }
    }

    /// Export the open page: one self-contained HTML file, a PDF made from it, plain text, or the
    /// markdown on the clipboard.
    fn export_page(&mut self, ctx: &egui::Context, how: ExFmt) {
        let Some(page) = self.page.clone() else {
            self.flash("open a page first");
            return;
        };
        let title = {
            let t = self.title.trim();
            if t.is_empty() {
                "Untitled".to_string()
            } else {
                t.to_string()
            }
        };
        if matches!(how, ExFmt::CopyMd) {
            ctx.copy_text(doc::to_markdown(&self.doc));
            self.flash("markdown copied");
            return;
        }
        let (ext, filter) = match how {
            ExFmt::Html => ("html", "*.html|HTML files"),
            ExFmt::Pdf => ("pdf", "*.pdf|PDF files"),
            _ => ("txt", "*.txt|Text files"),
        };
        let default = page.with_file_name(format!("{}.{ext}", vault::sanitize(&title)));
        let Some(out) = attach::choose_save(&default, filter) else {
            return; // cancelled
        };
        let result = match how {
            ExFmt::Html => std::fs::write(&out, export::html(&self.doc, &title, &page, &self.theme))
                .map_err(|e| e.to_string()),
            ExFmt::Pdf => {
                let html = export::html(&self.doc, &title, &page, &self.theme);
                let dir = out
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."));
                export::html_to_pdf(&html, &out, &dir)
            }
            _ => std::fs::write(&out, export::plain(&self.doc, &title)).map_err(|e| e.to_string()),
        };
        match result {
            Ok(()) => self.flash(format!("exported {}", out.display())),
            Err(e) => self.flash(format!("export failed: {e}")),
        }
    }

    /// Re-read what the keyboard chip is driving: KWin's layout list, and — when KWin is running only
    /// one layout — the X11 keymap, which is the one this app's typing goes through.
    ///
    /// Linux only: elsewhere there is no KWin to ask and no `setxkbmap` to fall back on, so the chip
    /// stays absent rather than spawning a subprocess on a timer that can never succeed.
    fn refresh_keyboard(&mut self) {
        if !crate::platform::has_keyboard_layout_toggle() {
            self.keylayout = None;
            return;
        }
        self.keylayout = keylayout::query();
        self.keylayout_at = Some(Instant::now());
        let kwin_can_switch = self
            .keylayout
            .as_ref()
            .map(|k| k.other().is_some())
            .unwrap_or(false);
        if kwin_can_switch {
            // the desktop-wide switch works: nothing to hold at the X11 level
            self.x11_layout = None;
            self.want_x11 = None;
            return;
        }
        // KWin is missing a layout (it has not re-read kxkbrc since login), so keep the user's choice
        // in the X11 keymap instead — that changes what NoteCraft types immediately.
        if let Some(code) = self.want_x11.clone() {
            if keylayout::x11_layout().map(|l| l.code != code).unwrap_or(false) {
                let _ = keylayout::set_x11_layout(&code);
            }
        }
        self.x11_layout = keylayout::x11_layout();
    }

    /// What the keyboard chip says, and what a click would do.
    ///
    /// KWin comes first — that is the desktop-wide switch the toggle exists for. When KWin is running
    /// only one layout the X11 keymap takes over: `setxkbmap` changes what NoteCraft itself types at
    /// once, while every other app keeps KWin's layout until the next login.
    fn kb_chip(&self) -> Option<KbChip> {
        if let Some(kl) = &self.keylayout {
            if let (Some(live), Some((index, other))) = (kl.live(), kl.other()) {
                return Some(KbChip {
                    label: kl.short(),
                    tip: format!(
                        "keyboard layout: {} — click to switch the desktop to {}",
                        live.name, other.name
                    ),
                    switch: Some(KbSwitch::KWin(index, other.name.clone())),
                    hint: String::new(),
                });
            }
        }
        if let Some(x) = &self.x11_layout {
            let pair = match x.code.as_str() {
                "gb" => Some(("us", "English (US)")),
                "us" => Some(("gb", "English (UK)")),
                _ => None,
            };
            return Some(match pair {
                Some((code, name)) => KbChip {
                    label: keylayout::short_label(&x.code),
                    tip: format!(
                        "keyboard layout: {} — click to type {name} in NoteCraft.\n\
                         KWin has only one layout loaded (it reads its list at login), so this \
                         changes what this app types now, and the whole desktop after the next login.",
                        x.name
                    ),
                    switch: Some(KbSwitch::X11(code.to_string(), name.to_string())),
                    hint: String::new(),
                },
                None => KbChip {
                    label: keylayout::short_label(&x.code),
                    tip: format!("keyboard layout: {}", x.name),
                    switch: None,
                    hint: "only the British and US layouts can be switched here".to_string(),
                },
            });
        }
        // KWin has one layout and there is no X11 display to move (a Wayland-native build): nothing
        // this app can switch, so say why instead of looking broken.
        let kl = self.keylayout.as_ref()?;
        let live = kl.live()?;
        let (tip, hint) = if keylayout::pending_restart(&kl.layouts) {
            (
                format!(
                    "keyboard layout: {}\n{} is set up in System Settings, but KWin builds its keymap \
                     when it starts and never re-reads it — the switch works from your next login.",
                    live.name,
                    keylayout::configured_missing(&kl.layouts).join(", ")
                ),
                "log out and back in once — KWin reads layouts at login".to_string(),
            )
        } else {
            (
                format!(
                    "keyboard layout: {}\nno second layout is set up — add one in System Settings → Keyboard.",
                    live.name
                ),
                "add a second layout in System Settings → Keyboard".to_string(),
            )
        };
        Some(KbChip {
            label: kl.short(),
            tip,
            switch: None,
            hint,
        })
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let t = self.theme;
        egui::TopBottomPanel::bottom("status")
            .exact_height(26.0)
            .frame(
                egui::Frame::new()
                    .fill(t.bg)
                    .inner_margin(Margin::symmetric(14, 3)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // The bar is one row and it has no second line to wrap onto: a segment that does
                    // not fit is *dropped*, right to left, in order of importance (saved state, word
                    // count, open todos, line position). Before this, a 780 pt window pushed the date
                    // into three stacked fragments and printed the left cluster through it.
                    let right_w = clock_slot_w(ui) + 168.0;
                    let budget = (ui.available_width() - right_w).max(0.0);
                    let mut used = 0.0f32;
                    let (state_text, state_colour) = if self.dirty {
                        ("• unsaved".to_string(), t.warn)
                    } else {
                        (
                            format!("saved {}", self.saved_at.clone().unwrap_or_else(|| "—".into())),
                            t.done,
                        )
                    };
                    ui.label(
                        RichText::new(&state_text)
                            .family(theme::fam(theme::F_MED))
                            .size(11.0)
                            .color(state_colour),
                    );
                    used += seg_w(ui, &state_text, 11.0, false);
                    let words = format!("{} words", vault::count_words(&self.body));
                    status_seg(ui, &words, true, 11.0, t.faint, &mut used, budget);
                    let open = self
                        .vault
                        .pages
                        .iter()
                        .flat_map(|p| p.todos.iter())
                        .filter(|x| !x.done)
                        .count();
                    status_seg(
                        ui,
                        &format!("{open} open todos"),
                        true,
                        11.0,
                        t.faint,
                        &mut used,
                        budget,
                    );
                    // Where the note is, while there is more of it than fits: the line being read
                    // (one block is one line) and how far through it that is.
                    // only for the note view: the tasks and tags views have their own scroll area,
                    // and a line count left over from the note would be a lie there
                    if let (Some((line, total)), Some(m)) = (self.view_line, self.page_scroll)
                        && self.view == View::Pages
                    {
                        let max = (m.content - m.view).max(0.0);
                        if max > 1.0 {
                            let pct = (m.offset / max * 100.0).round().clamp(0.0, 100.0) as i32;
                            status_seg(
                                ui,
                                &format!("line {line} / {total} · {pct}%"),
                                true,
                                11.0,
                                t.faint,
                                &mut used,
                                budget,
                            );
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // The flash message is the widest variable thing here, and the leftmost in a
                        // right-to-left cluster: it gets the room that is left, truncated, instead of
                        // pushing the clock and the backend readout off the end.
                        if !self.status.is_empty() {
                            let room = (ui.available_width() - 4.0).max(0.0);
                            ui.add_sized(
                                [room, 16.0],
                                egui::Label::new(
                                    RichText::new(self.status.clone())
                                        .family(theme::fam(theme::F_MED))
                                        .size(11.0)
                                        .color(t.accent),
                                )
                                .truncate(),
                            );
                        }
                        // The vault's own path used to sit here — the longest, least useful label in
                        // the window, and the first thing a narrow bar clipped. It is in the window
                        // title's place, in `~/.config/notecraft/state.json`, and in the tooltip of
                        // the page list. The bar says what the note is doing instead.
                        // The keyboard layout toggle. KWin owns the keymap, so the desktop-wide
                        // switch is a call into it; when KWin is running only one layout the chip
                        // falls back to the X11 keymap, which is what this app types through.
                        // Added before the backend label because this layout is right-to-left: the
                        // chip then draws immediately to the right of "X11 · ".
                        if let Some(kb) = self.kb_chip() {
                            let colour = if kb.switch.is_some() { t.accent } else { t.faint };
                            let chip = theme::chip_small(ui, &kb.label, colour, &t).on_hover_text(&kb.tip);
                            if chip.clicked() {
                                match kb.switch.clone() {
                                    Some(KbSwitch::KWin(index, name)) => {
                                        match keylayout::switch_to(index) {
                                            Ok(()) => {
                                                self.refresh_keyboard();
                                                self.flash(format!("keyboard: {name}"));
                                            }
                                            Err(e) => self.flash(format!("keyboard layout: {e}")),
                                        }
                                    }
                                    Some(KbSwitch::X11(code, name)) => {
                                        self.want_x11 = Some(code.clone());
                                        match keylayout::set_x11_layout(&code) {
                                            Ok(()) => {
                                                self.refresh_keyboard();
                                                self.flash(format!("NoteCraft typing: {name}"));
                                            }
                                            Err(e) => self.flash(format!("keyboard layout: {e}")),
                                        }
                                    }
                                    None => self.flash(kb.hint.clone()),
                                }
                            }
                        }
                        let backend = crate::platform::backend_label();
                        ui.label(
                            RichText::new(format!("{backend} · "))
                                .family(theme::fam(theme::F_MONO))
                                .size(10.5)
                                .color(if crate::platform::backend_drags() { t.done } else { t.faint }),
                        )
                        .on_hover_text(crate::platform::backend_hint());
                        // The clock is drawn last, which in this right-to-left cluster puts it to the
                        // left of `X11 · ` — next to the backend readout without pushing the keyboard
                        // chip away from the ` · ` that label already ends with. It is rebuilt from
                        // `Local::now()` every frame and `tick()` asks for a repaint every second, so
                        // the minute turns over by itself: no timer thread, no stored state.
                        // The slot is measured from the string itself, so the date can never wrap onto
                        // a second line — which is exactly what it did at 780 pt ("Wedn / esda / y").
                        let clock = format!("{} · ", clock_label(chrono::Local::now()));
                        ui.add_sized(
                            [clock_slot_w(ui), 16.0],
                            egui::Label::new(
                                RichText::new(&clock)
                                    .family(theme::fam(theme::F_MONO))
                                    .size(10.5)
                                    .color(t.dim),
                            )
                            .truncate(),
                        )
                        .on_hover_text("now, on this machine's local clock");
                    });
                });
            });
    }

    fn left_rail(&mut self, ctx: &egui::Context) {
        let t = self.theme;
        let mut click_nb: Option<String> = None;
        let mut click_sec: Option<(String, String)> = None;
        let mut new_section = false;
        let mut new_notebook = false;
        let mut kill_nb: Option<String> = None;
        let mut kill_sec: Option<(String, String)> = None;

        egui::SidePanel::left("rail")
            .exact_width(206.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(t.panel)
                    .inner_margin(Margin::symmetric(8, 10)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("rail_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            theme::section_label(ui, "Notebooks", &t);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if theme::icon_button(
                                        ui,
                                        "←",
                                        "fold the notebooks panel away (Ctrl+1)",
                                        &t,
                                    )
                                    .clicked()
                                    {
                                        self.rail_open = false;
                                    }
                                },
                            );
                        });
                        let notebooks = self.vault.notebooks.clone();
                        for nb in &notebooks {
                            let pages: usize = nb
                                .sections
                                .iter()
                                .map(|s| s.pages.len())
                                .sum();
                            let selected = self.nb.as_deref() == Some(nb.name.as_str());
                            let row = theme::nav_row(
                                ui,
                                &nb.name,
                                selected,
                                Some(pages.to_string()),
                                0.0,
                                &t,
                            );
                            if row.clicked() {
                                click_nb = Some(nb.name.clone());
                            }
                            row.context_menu(|ui| {
                                if ui.button("Delete notebook…").clicked() {
                                    kill_nb = Some(nb.name.clone());
                                    ui.close();
                                }
                            });
                        }
                        ui.add_space(10.0);
                        if let Some(nb) = self.vault.notebook(&self.nb.clone().unwrap_or_default()) {
                            theme::section_label(ui, "Sections", &t);
                            for sec in &nb.sections {
                                let selected = self.sec.as_deref() == Some(sec.name.as_str());
                                let row = theme::nav_row(
                                    ui,
                                    &sec.name,
                                    selected,
                                    Some(sec.pages.len().to_string()),
                                    8.0,
                                    &t,
                                );
                                if row.clicked() {
                                    click_sec = Some((nb.name.clone(), sec.name.clone()));
                                }
                                row.context_menu(|ui| {
                                    if ui.button("Delete section…").clicked() {
                                        kill_sec = Some((nb.name.clone(), sec.name.clone()));
                                        ui.close();
                                    }
                                });
                            }
                        }
                        ui.add_space(8.0);
                        if theme::nav_row(ui, "+ Section", false, None, 8.0, &t).clicked() {
                            new_section = true;
                        }
                        if theme::nav_row(ui, "+ Notebook", false, None, 0.0, &t).clicked() {
                            new_notebook = true;
                        }
                    });
            });

        if let Some(n) = click_nb {
            self.nb = Some(n);
            self.sec = self
                .vault
                .notebook(self.nb.as_deref().unwrap_or_default())
                .and_then(|nb| nb.sections.first().map(|s| s.name.clone()));
            self.view = View::Pages;
        }
        if let Some((n, s)) = click_sec {
            self.nb = Some(n);
            self.sec = Some(s);
            self.view = View::Pages;
        }
        if new_section {
            self.new_section();
        }
        if new_notebook {
            self.run_command("New notebook");
        }
        if let Some(name) = kill_nb {
            self.confirm = Some(Confirm::DeleteNotebook(name));
        }
        if let Some((nb, sec)) = kill_sec {
            self.confirm = Some(Confirm::DeleteSection(nb, sec));
        }
    }

    /// A slim strip left where a folded side panel used to be: the whole strip is the target, so
    /// bringing the panel back never depends on finding a small control. Keyboard: Ctrl+1 / Ctrl+2.
    fn panel_handle(&mut self, ctx: &egui::Context, id: &'static str, rail: bool, tip: &str) {
        let t = self.theme;
        let mut open = false;
        egui::SidePanel::left(egui::Id::new(id))
            .exact_width(26.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(t.panel)
                    .inner_margin(Margin::symmetric(3, 8)),
            )
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                let resp = ui.interact(rect, egui::Id::new((id, "handle")), Sense::click());
                if resp.hovered() {
                    ui.painter()
                        .rect_filled(rect, CornerRadius::same(6), t.card_hover);
                }
                let color = if resp.hovered() { t.heading } else { t.faint };
                ui.painter().text(
                    egui::Pos2::new(rect.center().x, rect.top() + 5.0),
                    egui::Align2::CENTER_TOP,
                    "→",
                    FontId::new(13.0, theme::fam(theme::F_MED)),
                    color,
                );
                // What the strip *was*, one letter per line: two identical arrows at the window edge
                // said nothing about which panel came back. egui cannot rotate text, and a 26 pt strip
                // cannot hold a word, so the label is stacked — the same trick `section_label` uses
                // for tracking, only vertical.
                let name = if rail { "NOTEBOOKS" } else { "PAGES" };
                let ink = if resp.hovered() { t.dim } else { t.ghost };
                for (n, ch) in name.chars().enumerate() {
                    ui.painter().text(
                        egui::Pos2::new(rect.center().x, rect.top() + 28.0 + n as f32 * 12.0),
                        egui::Align2::CENTER_TOP,
                        ch,
                        FontId::new(9.5, theme::fam(theme::F_MONO)),
                        ink,
                    );
                }
                if resp.clicked() {
                    open = true;
                }
                resp.on_hover_text(tip);
            });
        if open {
            if rail {
                self.rail_open = true;
            } else {
                self.pages_open = true;
            }
        }
    }

    /// Filter the page list by a tag. The field that shows the query lives in the page panel, so
    /// the panel has to be open or the user would set a filter they cannot see.
    fn filter_by_tag(&mut self, tag: &str) {
        self.search = format!("#{tag}");
        self.view = View::Pages;
        self.pages_open = true;
    }

    fn page_panel(&mut self, ctx: &egui::Context) {
        let t = self.theme;
        let searching = !self.search.trim().is_empty();
        let mut open: Option<PathBuf> = None;
        let mut land: Option<(PathBuf, String, Option<usize>)> = None;
        let mut search_all: Option<String> = None;
        let mut action: Option<(&'static str, PathBuf)> = None;

        egui::SidePanel::left("pages")
            .exact_width(268.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(t.bg)
                    .inner_margin(Margin::symmetric(8, 10)),
            )
            .show(ctx, |ui| {
                // The field filters the section you are in — the vault-wide search is Ctrl+K. Every
                // matching page is one row, however many times the words appear on it.
                let (all_hits, _total) = if searching {
                    self.vault.search(&self.search, 1000)
                } else {
                    (Vec::new(), 0)
                };
                let scope = match (&self.nb, &self.sec) {
                    (Some(n), Some(s)) => Some((n.clone(), s.clone())),
                    _ => None,
                };
                let mut hits: Vec<&vault::Hit> = all_hits
                    .iter()
                    .filter(|h| match &scope {
                        Some((n, s)) => {
                            let p = &self.vault.pages[h.page];
                            &p.notebook == n && &p.section == s
                        }
                        None => true,
                    })
                    .collect();
                let elsewhere = all_hits.len().saturating_sub(hits.len());
                let found = hits.len();
                hits.truncate(SEARCH_ROWS);
                ui.horizontal(|ui| {
                    let label = if searching {
                        match found {
                            0 => "Search · no pages here".to_string(),
                            n if n > hits.len() => format!("Search · {} pages (showing {})", n, hits.len()),
                            n => format!("Search · {n} page{}", if n == 1 { "" } else { "s" }),
                        }
                    } else {
                        self.sec.clone().unwrap_or_else(|| "Pages".into())
                    };
                    ui.label(
                        RichText::new(label)
                            .family(theme::fam(theme::F_SEMI))
                            .size(13.5)
                            .color(t.heading),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::icon_button(ui, "+", "new page (Ctrl+N)", &t).clicked() {
                            action = Some(("new", PathBuf::new()));
                        }
                        if theme::icon_button(
                            ui,
                            "←",
                            "fold the page list away (Ctrl+2)",
                            &t,
                        )
                        .clicked()
                        {
                            self.pages_open = false;
                        }
                    });
                });
                // The filter lives on the panel it filters. In the toolbar it read as "filter the
                // note", which is what the find row is for.
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let clear = !self.search.trim().is_empty();
                    let w = (ui.available_width() - if clear { 24.0 } else { 0.0 }).max(60.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .desired_width(w)
                            .hint_text("filter pages…")
                            .font(egui::TextStyle::Small),
                    );
                    if clear && theme::icon_button(ui, "×", "clear the filter", &t).clicked() {
                        self.search.clear();
                    }
                });
                ui.add_space(2.0);

                egui::ScrollArea::vertical()
                    .id_salt("page_list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if searching {
                            for h in &hits {
                                let p = &self.vault.pages[h.page];
                                let selected = self.page.as_deref() == Some(p.path.as_path());
                                let kind = match h.kind {
                                    HitKind::Title => "title",
                                    HitKind::Tag => "tag",
                                    HitKind::Body => "body",
                                };
                                let badge = if h.count > 1 {
                                    format!("{kind} · {} matches", h.count)
                                } else {
                                    kind.to_string()
                                };
                                let resp =
                                    page_row(ui, &p.title, &h.snippet, &badge, h.mark, selected, &t);
                                if resp.clicked() {
                                    land = Some((p.path.clone(), h.needle.clone(), h.line));
                                }
                                resp.on_hover_text("open this page at the match");
                            }
                            if elsewhere > 0 {
                                ui.add_space(6.0);
                                let resp = page_row(
                                    ui,
                                    &format!("{elsewhere} more outside this section"),
                                    "search the whole vault (Ctrl+K)",
                                    "",
                                    None,
                                    false,
                                    &t,
                                );
                                if resp.clicked() {
                                    search_all = Some(self.search.clone());
                                }
                            }
                        } else {
                            let sec_pages = match (&self.nb, &self.sec) {
                                (Some(n), Some(s)) => self.vault.section_pages(n, s),
                                _ => Vec::new(),
                            };
                            for (pos, idx) in sec_pages.iter().enumerate() {
                                let p = &self.vault.pages[*idx];
                                let selected = self.page.as_deref() == Some(p.path.as_path());
                                let meta = page_meta_line(p);
                                let resp = page_row(ui, &p.title, &meta, "", None, selected, &t);
                                if resp.clicked() {
                                    open = Some(p.path.clone());
                                }
                                resp.context_menu(|ui| {
                                    if ui.button("Rename").clicked() {
                                        action = Some(("rename", p.path.clone()));
                                        ui.close();
                                    }
                                    if ui.button("Duplicate").clicked() {
                                        action = Some(("duplicate", p.path.clone()));
                                        ui.close();
                                    }
                                    if ui.button("Move up").clicked() {
                                        action = Some(("up", p.path.clone()));
                                        ui.close();
                                    }
                                    if ui.button("Move down").clicked() {
                                        action = Some(("down", p.path.clone()));
                                        ui.close();
                                    }
                                    if ui.button("Delete").clicked() {
                                        action = Some(("delete", p.path.clone()));
                                        ui.close();
                                    }
                                    ui.menu_button("Move to…", |ui| {
                                        let nbs = self.vault.notebooks.clone();
                                        for nb in nbs {
                                            for sec in nb.sections {
                                                let label = format!("{} / {}", nb.name, sec.name);
                                                if ui.button(label).clicked() {
                                                    action = Some(("move", p.path.clone()));
                                                    self.nb = Some(nb.name.clone());
                                                    self.sec = Some(sec.name.clone());
                                                    ui.close();
                                                }
                                            }
                                        }
                                    });
                                    let _ = pos;
                                });
                            }
                            if sec_pages.is_empty() {
                                ui.add_space(16.0);
                                ui.label(
                                    RichText::new("No pages here yet.\nCtrl+N creates one.")
                                        .family(theme::fam(theme::F_REG))
                                        .size(12.5)
                                        .color(t.faint),
                                );
                            }
                        }
                    });
            });

        if let Some((path, needle, line)) = land {
            self.land_on_hit(ctx, &path, &needle, line);
        }
        if let Some(q) = search_all {
            self.palette = Some(Palette {
                query: q,
                sel: 0,
                focus: true,
            });
        }
        if let Some(p) = open {
            self.open_page(&p, false);
        }
        if let Some((what, path)) = action {
            match what {
                "new" => self.new_page(),
                "rename" => {
                    self.rename_buf = self
                        .vault
                        .page_index(&path)
                        .map(|i| self.vault.pages[i].title.clone())
                        .unwrap_or_default();
                    self.rename_for = Some(path);
                }
                "delete" => {
                    // the target lives in `move_for` until the confirm dialog is done with it
                    self.move_for = Some(path.clone());
                    self.confirm = Some(Confirm::DeletePage);
                }
                "duplicate" => match Ops::duplicate_page(&path) {
                    Ok(newp) => {
                        self.vault.reload();
                        self.open_page(&newp, false);
                        // the name is the first thing to change on a copy; put the caret there
                        self.focus_title = true;
                        self.flash("duplicated — rename the copy if you like");
                    }
                    Err(e) => self.flash(format!("duplicate failed: {e}")),
                },
                "up" | "down" => {
                    if let (Some(n), Some(s)) = (self.nb.clone(), self.sec.clone()) {
                        let list = self.vault.section_pages(&n, &s);
                        if let Some(pos) = list.iter().position(|i| self.vault.pages[*i].path == path)
                        {
                            let other = if what == "up" {
                                if pos == 0 { None } else { Some(list[pos - 1]) }
                            } else if pos + 1 >= list.len() {
                                None
                            } else {
                                Some(list[pos + 1])
                            };
                            if let Some(o) = other {
                                let _ = Ops::reorder(&path, &self.vault.pages[o].path);
                                self.vault.reload();
                                // reorder rewrote this page's `order` on disk; the buffer's copy is
                                // stale, and the next save would put the old value back and undo
                                // the move
                                if let Some(i) = self
                                    .page
                                    .clone()
                                    .and_then(|p| self.vault.page_index(&p))
                                {
                                    self.order = self.vault.pages[i].order;
                                }
                            }
                        }
                    }
                }
                "move" => {
                    if let (Some(n), Some(s)) = (self.nb.clone(), self.sec.clone()) {
                        // Write the buffer out *before* the move: the page's path is about to change,
                        // and a save on the way into the new file would otherwise recreate the old
                        // path (two copies, drifting apart) — the rename path guards exactly this.
                        self.flush_open_page(&path);
                        if let Ok(newp) = Ops::move_page(&path, &self.vault.root, &n, &s) {
                            self.vault.reload();
                            // claim the new path before open_page can flush anything
                            self.page = Some(newp.clone());
                            self.open_page(&newp, false);
                            self.flash(format!("moved to {n} / {s}"));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn central(&mut self, ctx: &egui::Context) {
        let t = self.theme;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(t.bg)
                    .inner_margin(Margin::symmetric(18, 12)),
            )
            .show(ctx, |ui| {
                match self.view {
                    View::Pages => self.page_view(ui),
                    View::Tasks => self.tasks_view(ui),
                    View::Tags => self.tags_view(ui),
                }
                // Drop-target feedback belongs to the whole window, not just the
                // page canvas — otherwise dragging while the Tasks or Tags view
                // is open looks like nothing is happening.
                self.drop_overlay(ui);
            });
    }

    /// Accent border + "drop to attach" chip while a file is held over the
    /// window. Only the X11/Windows backends deliver hover events, so on Wayland
    /// (and for drags whose types winit rejects, e.g. image/png out of a browser)
    /// this never appears.
    fn drop_overlay(&self, ui: &egui::Ui) {
        if !self.drop_hover {
            return;
        }
        let t = self.theme;
        let r = ui.max_rect().shrink(5.0);
        let p = ui.painter();
        p.rect_stroke(
            r,
            CornerRadius::same(12),
            Stroke::new(2.0_f32, t.accent),
            StrokeKind::Inside,
        );
        let g = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                "drop to attach".to_owned(),
                FontId::new(12.0, theme::fam(theme::F_MED)),
                t.accent,
            )
        });
        let size = g.size() + Vec2::new(18.0, 8.0);
        let chip = egui::Rect::from_min_size(
            egui::Pos2::new(r.center().x - size.x / 2.0, r.min.y + 8.0),
            size,
        );
        p.rect_filled(chip, CornerRadius::same(99), t.panel2);
        p.rect_stroke(
            chip,
            CornerRadius::same(99),
            Stroke::new(1.0_f32, t.accent.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
        p.galley(chip.min + Vec2::new(9.0, 4.0), g, t.accent);
    }

    fn page_view(&mut self, ui: &mut egui::Ui) {
        let t = self.theme;
        let Some(path) = self.page.clone() else {
            // nothing open: no line count, no rail, and no stale numbers in the status bar
            self.view_line = None;
            self.page_scroll = None;
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new("Ctrl+N to start a page\nCtrl+K to search everything")
                        .family(theme::fam(theme::F_REG))
                        .size(14.0)
                        .color(t.faint),
                );
            });
            return;
        };
        let _ = path;
        let mut area = egui::ScrollArea::vertical()
            .id_salt("page_scroll")
            .auto_shrink([false, false])
            // Dragging the contents must not scroll the page: a picture is dragged and a
            // selection is swept with the mouse, and a scroll that follows the pointer makes
            // both impossible. The wheel still scrolls.
            .scroll_source(egui::containers::scroll_area::ScrollSource {
                drag: false,
                ..Default::default()
            })
            // the app draws its own rail (`note_rail`): the share of the note read, the headings, and
            // a target you can press. egui's bar would sit on top of it and say the same thing twice.
            .scroll_bar_visibility(
                egui::containers::scroll_area::ScrollBarVisibility::AlwaysHidden,
            );
        // Home / End / PgUp / PgDn asked for a place in this note (worked out from the metrics the
        // last frame left behind); a fresh note starts at its top. One-shot: the wheel is the
        // user's again on the next frame. A landing on a search match wins over the top (it scrolls
        // the block into view inside the area).
        let jump = self.pending_scroll.take().or_else(|| {
            (self.scroll_to_block.is_none() && std::mem::take(&mut self.scroll_top)).then_some(0.0)
        });
        if let Some(y) = jump {
            area = area.vertical_scroll_offset(y);
        }
        let out = area.show(ui, |ui| {
                // 880 pt is the reading measure, but the column can never be wider than the space
                // that is actually there: `set_max_width` *sets* the region's right edge (it does
                // not clamp), so a fixed 880 inside a narrower window laid every line out past the
                // panel — the text wrapped on a 880 pt measure and the right-hand end of each line
                // was clipped away, which reads as "wordwrap is off".
                let canvas = ui.available_width() - PAGE_SCROLLBAR_ALLOWANCE;
                let column = canvas.min(880.0).max(240.0);
                // When the canvas is wider than the measure, the column is *centred* in it. It used
                // to be pinned to the left edge, so with both side panels folded the note sat in the
                // first 880 of a 1210 pt page with every bit of the slack stacked on the right — a
                // writing app with the text shoved into a corner. The indent is a horizontal
                // `add_space`, which needs a horizontal layout to mean anything: in a vertical one
                // it would push the note *down*.
                let slack = (canvas - column).max(0.0);
                let inset = if slack > 40.0 { slack * 0.5 } else { 0.0 };
                ui.horizontal_top(|ui| {
                    ui.add_space(inset);
                    ui.vertical(|ui| {
                ui.set_max_width(column);
                // ------- title
                let title_resp = ui.add(
                    egui::TextEdit::singleline(&mut self.title)
                        .id(egui::Id::new("title"))
                        // the note's own face, not the chrome face: the title and the body are one
                        // page, and a serif body under a geometric sans title read as two. It also
                        // follows the Display menu's text-size step, because it is note text.
                        .font(theme::title_font_scaled(self.note_text.scale()))
                        .frame(false)
                        .hint_text("Untitled page")
                        .desired_width(ui.available_width()),
                );
                if self.focus_title {
                    title_resp.request_focus();
                    self.focus_title = false;
                }
                if title_resp.changed() {
                    self.mark_dirty();
                }
                ui.add_space(2.0);

                // ------- meta row: tags + dates
                // In read mode the chrome steps back: the note is what is being read, and this row
                // (tags, dates, Undo/Redo) was competing with its first line for attention.
                if self.read_mode {
                    ui.scope(|ui| {
                        ui.set_opacity(0.45);
                        self.note_meta_row(ui);
                    });
                } else {
                    self.note_meta_row(ui);
                }

                ui.add_space(6.0);
                let rule = ui.available_width();
                let (r, _) = ui.allocate_exact_size(Vec2::new(rule, 1.0), Sense::hover());
                ui.painter()
                    .rect_filled(r, CornerRadius::ZERO, t.border);
                ui.add_space(8.0);

                if self.read_mode {
                    self.read_body(ui);
                } else {
                    self.edit_doc(ui);
                }

                ui.add_space(20.0);
                    });
                });
            });
        // What the scroll keys measure against next frame: where the view is, how tall it is, and
        // how tall the note is (End means the note's own end, not the last block that fitted).
        self.page_scroll = Some(ScrollMetrics {
            offset: out.state.offset.y,
            view: out.inner_rect.height(),
            content: out.content_size.y,
        });
        // Which line is at the top of the window, for the status bar. One block is one line of the
        // document (a newline divides it), so the last block starting above the fold is the line
        // being read.
        let total = self.block_tops.len();
        let first = self
            .block_tops
            .iter()
            .rposition(|y| *y <= out.inner_rect.top() + 1.0)
            .unwrap_or(0);
        self.view_line = (total > 0).then_some((first + 1, total));
        self.note_rail(ui, out.inner_rect, out.content_size.y, out.state.offset.y);
    }

    /// The row under the title: the note's tags, when it was made and last saved, and Undo/Redo. Its
    /// own method so read mode can draw it at 45% opacity (`ui.set_opacity` on a scope) without a
    /// second copy of the layout.
    fn note_meta_row(&mut self, ui: &mut egui::Ui) {
        let t = self.theme;
        // Tags get their own line, above the dates. They are *content* — the note's place in the
        // vault, and buttons that filter by it — while everything beside them is provenance; on one
        // line a note with three tags started with a row of chips and the date was squeezed out to
        // the right of them.
        let tags = self
            .vault
            .page_index(&self.page.clone().unwrap_or_default())
            .map(|i| self.vault.pages[i].tags.clone())
            .unwrap_or_default();
        if !tags.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for tag in tags {
                    if theme::chip(ui, &format!("#{tag}"), t.accent, &t).clicked() {
                        self.filter_by_tag(&tag);
                    }
                }
            });
            ui.add_space(3.0);
        }
        ui.horizontal_wrapped(|ui| {
            let created = self
                .created
                .as_deref()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.format("%d %b %Y").to_string());
            let mut meta = String::new();
            if let Some(c) = created {
                meta.push_str(&format!("created {c}"));
            }
            if let Some(s) = &self.saved_at {
                if !meta.is_empty() {
                    meta.push_str("   ·   ");
                }
                meta.push_str(&format!("saved {s}"));
            }
            if !meta.is_empty() {
                ui.label(
                    RichText::new(meta)
                        .family(theme::fam(theme::F_MONO))
                        .size(10.5)
                        .color(t.faint),
                );
            }
            // undo / redo sit at the top of the page, where the change was made; a button with
            // nothing behind it greys out instead of lying
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let can_redo = self.history.can_redo();
                let can_undo = self.history.can_undo();
                let redo_t = if can_redo { t } else { Theme { dim: t.ghost, ..t } };
                let undo_t = if can_undo { t } else { Theme { dim: t.ghost, ..t } };
                if theme::toolbar_button(ui, "Redo", false, &redo_t)
                    .on_hover_text("redo the last undo (Ctrl+Shift+Z)")
                    .clicked()
                {
                    self.redo_doc();
                }
                if theme::toolbar_button(ui, "Undo", false, &undo_t)
                    .on_hover_text("undo the last change (Ctrl+Z)")
                    .clicked()
                {
                    self.undo_doc();
                }
            });
        });
    }

    /// A thin rail where the scroll bar used to be, down the right of the note column: the thumb says
    /// how far through the note you are, one tick marks each heading (the note's outline, with no
    /// panel to open), and pressing or dragging anywhere on it puts the view there. It only appears
    /// when the note is longer than the window, so a short note stays quiet.
    fn note_rail(&mut self, ui: &mut egui::Ui, view: egui::Rect, content: f32, offset: f32) {
        let t = self.theme;
        let max = (content - view.height()).max(0.0);
        if max <= 1.0 || view.height() < 40.0 {
            return;
        }
        // Wide enough to see and to hit: 8 pt of groove, a 22 pt strip to grab, and heading ticks
        // that step out past it on both sides.
        let track_h = (view.height() - 8.0).max(24.0);
        let track_top = view.top() + 4.0;
        let x = ui.max_rect().right() - 11.0;
        let hit = egui::Rect::from_min_max(
            egui::pos2(x - 11.0, track_top),
            egui::pos2(x + 11.0, track_top + track_h),
        );
        let resp = ui.interact(hit, egui::Id::new("note_rail"), Sense::click_and_drag());
        let hot = resp.hovered() || resp.dragged();
        let width = if hot { 9.0 } else { 6.0 };
        let p = ui.painter();
        // the groove: nearly invisible until the pointer comes near
        p.rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(x, track_top + track_h / 2.0),
                Vec2::new(width, track_h),
            ),
            CornerRadius::same(2),
            t.dim.gamma_multiply(if hot { 0.32 } else { 0.16 }),
        );
        // the thumb: the visible share of the note
        let thumb_h = (track_h * view.height() / content).clamp(30.0, track_h);
        let thumb_y = track_top + (offset / max) * (track_h - thumb_h);
        p.rect_filled(
            egui::Rect::from_min_size(egui::pos2(x - width / 2.0, thumb_y), Vec2::new(width, thumb_h)),
            CornerRadius::same(2),
            t.accent.gamma_multiply(if hot { 0.8 } else { 0.5 }),
        );
        // one tick per heading, in the note's own coordinates
        let origin = view.top() - offset;
        for y in &self.heading_tops {
            let fy = ((y - origin) / content.max(1.0)).clamp(0.0, 1.0);
            p.rect_filled(
                egui::Rect::from_center_size(
                    egui::pos2(x, track_top + fy * track_h),
                    Vec2::new(16.0, 3.0),
                ),
                CornerRadius::same(1),
                t.accent2.gamma_multiply(0.9),
            );
        }
        // pressing or dragging on the rail scrolls to the pointer: the same thing the bar did
        if let Some(pos) = resp.interact_pointer_pos() {
            let frac = ((pos.y - track_top) / track_h).clamp(0.0, 1.0);
            self.pending_scroll = Some(frac * max);
        }
    }

    /// Full-screen snip overlay: the frozen grab, a dotted "snip is active" frame
    /// around the edges, and the box being dragged. It runs in its own fullscreen
    /// viewport so the user sees the whole screen at the size they photographed it.
    fn snip_viewport(&mut self, ctx: &egui::Context) {
        let id = egui::ViewportId::from_hash_of("notecraft-snip");
        let builder = egui::ViewportBuilder::default()
            .with_title("NoteCraft snip")
            .with_decorations(false)
            .with_fullscreen(true)
            .with_always_on_top()
            .with_active(true)
            .with_resizable(false);
        ctx.show_viewport_immediate(id, builder, |ctx, _class| {
            self.snip_ui(ctx);
        });
    }

    fn snip_ui(&mut self, ctx: &egui::Context) {
        let t = self.theme;
        let Some(shot) = self.snip.as_mut() else {
            return;
        };
        // Downscaled display copy, uploaded once.
        if shot.tex.is_none() {
            let (iw, ih) = (shot.img.width(), shot.img.height());
            let scale = (2400.0 / iw.max(1) as f32).min(1.0);
            let small = image::imageops::resize(
                &shot.img,
                ((iw as f32 * scale) as u32).max(1),
                ((ih as f32 * scale) as u32).max(1),
                image::imageops::FilterType::Triangle,
            );
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [small.width() as usize, small.height() as usize],
                small.as_raw(),
            );
            shot.tex = Some(ctx.load_texture(
                "snip-preview",
                color,
                egui::TextureOptions::LINEAR,
            ));
        }
        let tex = shot.tex.as_ref().map(|h| h.id());
        let mut released = false;
        let mut whole = false;
        let mut cancel = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::from_rgb(6, 6, 8)))
            .show(ctx, |ui| {
                let area = ui.max_rect();
                // Dotted frame right around the overlay: the sign that a snip is armed.
                let inset = area.shrink(7.0);
                let stroke = Stroke::new(2.0_f32, t.accent);
                for (a, b) in [
                    (inset.left_top(), inset.right_top()),
                    (inset.right_top(), inset.right_bottom()),
                    (inset.right_bottom(), inset.left_bottom()),
                    (inset.left_bottom(), inset.left_top()),
                ] {
                    for s in egui::Shape::dashed_line(&[a, b], stroke, 7.0, 6.0) {
                        ui.painter().add(s);
                    }
                }
                // The frozen screen, scaled to fit and centred (never stretched).
                let inner = area.shrink(24.0);
                let (iw, ih) = (shot.img.width().max(1) as f32, shot.img.height().max(1) as f32);
                let scale = (inner.width() / iw).min(inner.height() / ih);
                let draw = egui::Rect::from_center_size(
                    inner.center(),
                    Vec2::new(iw * scale, ih * scale),
                );
                shot.rect = Some(draw);
                if let Some(id) = tex {
                    ui.painter().image(
                        id,
                        draw,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                // Drag anywhere on the overlay to pick the region.
                let resp = ui.interact(area, ui.id().with("snip-drag"), Sense::click_and_drag());
                if resp.dragged() {
                    if shot.from.is_none() {
                        shot.from = resp.interact_pointer_pos();
                    }
                    shot.to = resp.interact_pointer_pos();
                }
                if resp.drag_stopped() {
                    released = true;
                }
                // A box that starts outside the picture (in the letterbox margin) is not
                // a selection — a sliver at the edge would crop a line of pixels.
                let sel = match (shot.from, shot.to) {
                    (Some(a), Some(b)) => {
                        let raw = egui::Rect::from_two_pos(a, b).intersect(draw);
                        (raw.width() > 8.0 && raw.height() > 8.0).then_some(raw)
                    }
                    _ => None,
                };
                shot.sel = sel;
                if let Some(sel) = sel {
                    // Dim everything that is not being kept.
                    let dim = egui::Color32::from_black_alpha(140);
                    for r in [
                        egui::Rect::from_min_max(area.left_top(), egui::pos2(area.right(), sel.top())),
                        egui::Rect::from_min_max(egui::pos2(area.left(), sel.bottom()), area.right_bottom()),
                        egui::Rect::from_min_max(
                            egui::pos2(area.left(), sel.top()),
                            egui::pos2(sel.left(), sel.bottom()),
                        ),
                        egui::Rect::from_min_max(
                            egui::pos2(sel.right(), sel.top()),
                            egui::pos2(area.right(), sel.bottom()),
                        ),
                    ] {
                        ui.painter().rect_filled(r, CornerRadius::ZERO, dim);
                    }
                    ui.painter().rect_stroke(
                        sel,
                        CornerRadius::same(2),
                        Stroke::new(2.0_f32, t.accent),
                        StrokeKind::Inside,
                    );
                }
                // With several monitors the frozen desktop is wider than this screen, so
                // it is scaled to fit: say so instead of leaving the margin unexplained.
                let letterboxed = draw.width() < area.width() - 1.0 || draw.height() < area.height() - 1.0;
                let hint = if letterboxed {
                    "drag a box over the screen   ·   Enter = whole screen   ·   Esc = cancel   ·   (desktop scaled to fit)"
                } else {
                    "drag a box over the screen   ·   Enter = whole screen   ·   Esc = cancel"
                };
                ui.painter().text(
                    egui::pos2(area.center().x, area.top() + 40.0),
                    egui::Align2::CENTER_CENTER,
                    hint,
                    egui::FontId::new(16.0, theme::fam(theme::F_MONO)),
                    t.dim,
                );
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape))
            || ctx.input(|i| i.viewport().close_requested())
        {
            cancel = true;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            whole = true;
        }

        if cancel {
            let Some(shot) = self.snip.take() else { return };
            let _ = std::fs::remove_file(&shot.temp);
            dnd_log("snip: cancelled at the crop step");
            self.flash("snip cancelled");
            self.focus_main_window(ctx);
            return;
        }
        // Dragging outside the picture is not a crop: leave the overlay up to retry.
        if !whole && !(released && self.snip.as_ref().and_then(|s| s.sel).is_some()) {
            return;
        }
        let Some(shot) = self.snip.take() else { return };
        let Some(drawn) = shot.rect else { return };
        let sel = match (shot.sel, whole) {
            // Enter keeps the whole grab
            (_, true) => drawn,
            (Some(sel), _) => sel,
            _ => return,
        };
        let (x, y, w, h) = snip_crop((shot.img.width(), shot.img.height()), drawn, sel);
        let cropped = image::imageops::crop_imm(&shot.img, x, y, w, h).to_image();
        let _ = std::fs::remove_file(&shot.temp);
        let Some(page) = self.page.clone() else { return };
        match attach::snippet_for_snip(&page, &cropped) {
            Ok(snippet) => {
                dnd_log(&format!("snip: attached {w}x{h} from {x},{y}"));
                self.append_block(&[snippet]);
                self.flash(format!("snip {w}×{h} added to the note"));
            }
            Err(e) => self.flash(format!("cannot save the snip: {e}")),
        }
        self.focus_main_window(ctx);
    }

    /// Hand focus back to the notes window after an overlay closes.
    ///
    /// Overlays (crop, annotate) are drawn inside the notes window itself, so no window manager
    /// call is involved any more: this only asks the toolkit to raise and focus us, which works on
    /// X11 and Wayland alike.
    fn focus_main_window(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    /// Open the annotator for the picture `reference` (the block is remembered in
    /// `annotate_from_block`, which is what the page repoints when the marks are saved).
    fn open_annotate(&mut self, reference: &str) {
        let Some(page) = self.page.clone() else {
            self.flash("open a page first");
            return;
        };
        let abs = vault::asset_path(&page, reference);
        let Ok(img) = image::open(&abs).map(|i| i.to_rgba8()) else {
            self.flash(format!("cannot open {} to draw on it", abs.display()));
            return;
        };
        dnd_log(&format!("annotate: opened {reference} ({}x{})", img.width(), img.height()));
        self.annotate = Some(Annotate {
            img,
            tex: None,
            rect: None,
            tool: AnnoTool::Pen,
            colour: ANNO_COLOURS[0].0,
            brush: ANNO_BRUSHES[1],
            strokes: Vec::new(),
            current: None,
        });
    }

    /// The annotator's own fullscreen overlay, sized like the image being drawn on.
    fn annotate_viewport(&mut self, ctx: &egui::Context) {
        let id = egui::ViewportId::from_hash_of("notecraft-annotate");
        let builder = egui::ViewportBuilder::default()
            .with_title("NoteCraft annotate")
            .with_decorations(false)
            .with_fullscreen(true)
            .with_always_on_top()
            .with_active(true)
            .with_resizable(false);
        ctx.show_viewport_immediate(id, builder, |ctx, _class| {
            self.annotate_ui(ctx);
        });
    }

    fn annotate_ui(&mut self, ctx: &egui::Context) {
        let t = self.theme;
        let Some(a) = self.annotate.as_mut() else {
            return;
        };
        if a.tex.is_none() {
            let (iw, ih) = (a.img.width(), a.img.height());
            let scale = (2400.0 / iw.max(1) as f32).min(1.0);
            let small = image::imageops::resize(
                &a.img,
                ((iw as f32 * scale) as u32).max(1),
                ((ih as f32 * scale) as u32).max(1),
                image::imageops::FilterType::Triangle,
            );
            let color = egui::ColorImage::from_rgba_unmultiplied(
                [small.width() as usize, small.height() as usize],
                small.as_raw(),
            );
            a.tex = Some(ctx.load_texture("anno-preview", color, egui::TextureOptions::LINEAR));
        }
        let tex = a.tex.as_ref().map(|h| h.id());
        let mut save = false;
        let mut cancel = false;
        let mut undo = false;
        let mut clear = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(egui::Color32::from_rgb(6, 6, 8)))
            .show(ctx, |ui| {
                let area = ui.max_rect();
                // ------- tool strip
                let bar = egui::Rect::from_min_size(area.left_top(), Vec2::new(area.width(), 56.0));
                ui.painter().rect_filled(bar, CornerRadius::ZERO, t.bg);
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(bar.shrink2(Vec2::new(16.0, 12.0))),
                    |ui| {
                        ui.horizontal_centered(|ui| {
                            if theme::toolbar_button(ui, "Pen", a.tool == AnnoTool::Pen, &t).clicked()
                            {
                                a.tool = AnnoTool::Pen;
                            }
                            if theme::toolbar_button(ui, "Line", a.tool == AnnoTool::Line, &t)
                                .on_hover_text("straight line — hold it flat to underline")
                                .clicked()
                            {
                                a.tool = AnnoTool::Line;
                            }
                            if theme::toolbar_button(ui, "Circle", a.tool == AnnoTool::Circle, &t)
                                .on_hover_text("ellipse over the dragged box")
                                .clicked()
                            {
                                a.tool = AnnoTool::Circle;
                            }
                            ui.add_space(10.0);
                            for (i, (c, name)) in ANNO_COLOURS.iter().enumerate() {
                                let (r, resp) =
                                    ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                                ui.painter().rect_filled(r, CornerRadius::same(5), *c);
                                if a.colour == *c {
                                    ui.painter().rect_stroke(
                                        r.expand(1.5),
                                        CornerRadius::same(6),
                                        Stroke::new(2.0_f32, t.accent),
                                        StrokeKind::Outside,
                                    );
                                }
                                if resp.clicked() {
                                    a.colour = *c;
                                }
                                resp.on_hover_text(*name);
                                let _ = i;
                            }
                            ui.add_space(10.0);
                            for (i, w) in ANNO_BRUSHES.iter().enumerate() {
                                let label = ["thin", "medium", "thick"][i];
                                if theme::toolbar_button(ui, label, a.brush == *w, &t).clicked() {
                                    a.brush = *w;
                                }
                            }
                            ui.add_space(10.0);
                            if theme::toolbar_button(ui, "Undo", false, &t).clicked() {
                                undo = true;
                            }
                            if theme::toolbar_button(ui, "Clear", false, &t).clicked() {
                                clear = true;
                            }
                            ui.add_space(10.0);
                            if theme::toolbar_button(ui, "Cancel (Esc)", false, &t).clicked() {
                                cancel = true;
                            }
                            if theme::toolbar_button(ui, "Save", true, &t).clicked() {
                                save = true;
                            }
                        });
                    },
                );

                // ------- the picture, fitted
                let inner = egui::Rect::from_min_max(
                    area.min + Vec2::new(24.0, 78.0),
                    area.max - Vec2::new(24.0, 64.0),
                );
                let (iw, ih) = (a.img.width().max(1) as f32, a.img.height().max(1) as f32);
                let scale = (inner.width() / iw).min(inner.height() / ih);
                let draw = egui::Rect::from_center_size(inner.center(), Vec2::new(iw * scale, ih * scale));
                a.rect = Some(draw);
                if let Some(id) = tex {
                    ui.painter().image(
                        id,
                        draw,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                ui.painter().rect_stroke(
                    draw,
                    CornerRadius::same(2),
                    Stroke::new(1.0_f32, t.border),
                    StrokeKind::Outside,
                );

                // ------- drag to draw
                let resp = ui.interact(draw, ui.id().with("anno-draw"), Sense::click_and_drag());
                let to_img = |p: egui::Pos2| {
                    (
                        ((p.x - draw.min.x) / draw.width().max(1.0)).clamp(0.0, 1.0),
                        ((p.y - draw.min.y) / draw.height().max(1.0)).clamp(0.0, 1.0),
                    )
                };
                if resp.dragged() {
                    let p = resp.interact_pointer_pos().map(to_img);
                    match (&mut a.current, p) {
                        (None, Some(p)) => {
                            a.current = Some(AnnoStroke {
                                tool: a.tool,
                                colour: a.colour,
                                width: a.brush,
                                points: vec![p],
                            })
                        }
                        (Some(st), Some(p)) => {
                            // freehand keeps every point; line and circle just move their end
                            if st.tool == AnnoTool::Pen {
                                if st.points.last().map(|l| (l.0 - p.0).abs() + (l.1 - p.1).abs()).unwrap_or(1.0) > 0.002 {
                                    st.points.push(p);
                                }
                            } else {
                                st.points.truncate(1);
                                st.points.push(p);
                            }
                        }
                        _ => {}
                    }
                }
                if resp.drag_stopped() {
                    if let Some(st) = a.current.take() {
                        a.strokes.push(st);
                    }
                }

                // ------- paint the strokes
                let px = |p: (f32, f32)| {
                    egui::pos2(draw.min.x + p.0 * draw.width(), draw.min.y + p.1 * draw.height())
                };
                for st in a.strokes.iter().chain(a.current.iter()) {
                    let pts: Vec<egui::Pos2> =
                        stroke_points(st, 1.0, 1.0).iter().map(|p| px(*p)).collect();
                    if pts.len() >= 2 {
                        ui.painter().add(egui::Shape::line(
                            pts,
                            Stroke::new((st.width * draw.height().min(draw.width())).max(2.0), st.colour),
                        ));
                    } else if let Some(p) = pts.first() {
                        ui.painter().circle_filled(
                            *p,
                            (st.width * draw.height().min(draw.width())).max(2.0) * 0.5,
                            st.colour,
                        );
                    }
                }
                ui.painter().text(
                    egui::pos2(area.center().x, area.bottom() - 26.0),
                    egui::Align2::CENTER_CENTER,
                    "draw on the picture   ·   Save keeps it in the note (the original file stays)",
                    egui::FontId::new(14.0, theme::fam(theme::F_MONO)),
                    t.dim,
                );
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape))
            || ctx.input(|i| i.viewport().close_requested())
        {
            cancel = true;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
            save = true;
        }

        if undo || clear {
            if let Some(a) = self.annotate.as_mut() {
                if clear {
                    a.strokes.clear();
                } else {
                    a.strokes.pop();
                }
                a.current = None;
            }
        }
        if cancel {
            self.annotate = None;
            self.annotate_from_block = None;
            dnd_log("annotate: cancelled");
            self.flash("annotations discarded");
            self.focus_main_window(ctx);
            return;
        }
        if !save {
            return;
        }
        let Some(a) = self.annotate.take() else { return };
        if a.strokes.is_empty() {
            dnd_log("annotate: nothing drawn, left alone");
            self.flash("nothing drawn — image unchanged");
            self.focus_main_window(ctx);
            return;
        }
        let (n, size) = (a.strokes.len(), (a.img.width(), a.img.height()));
        let annotated = draw_strokes(&a.img, &a.strokes);
        let Some(page) = self.page.clone() else { return };
        match vault::Ops::save_asset_named(&page, &annotated, "annotated") {
            Ok(reference) => {
                // the picture is a block, so the page repoints the block: this used to rewrite
                // body line 0, which left the mark on no picture at all unless the note happened
                // to start with one
                let target = self.annotate_from_block.take();
                let pointed = target
                    .and_then(|i| self.doc.blocks.get_mut(i))
                    .map(|b| b.image = reference.clone())
                    .is_some();
                if pointed {
                    self.body = doc::to_markdown(&self.doc);
                    self.mark_dirty();
                    self.save();
                    dnd_log(&format!(
                        "annotate: {n} mark(s) saved onto {}x{} -> {reference}",
                        size.0, size.1
                    ));
                    self.flash(format!("{n} mark(s) added to the picture"));
                } else {
                    self.flash("could not point the note at the annotated picture");
                }
            }
            Err(e) => self.flash(format!("cannot save the annotated picture: {e}")),
        }
        self.focus_main_window(ctx);
    }

    /// The editor, working on the page's **document**: one block per row, its text edit holding only
    /// the words. Properties are toggled through the right-click menu, never typed as characters, so
    /// there is no syntax to hide and nothing to resurface.
    fn edit_doc(&mut self, ui: &mut egui::Ui) {
        let t = self.theme;
        // A one-line block must sit a line-height under the one above it. The theme's row metrics
        // are for widgets in panels (26 pt tall rows, 7 pt apart) and applied here they added
        // ~15 pt of gap to every paragraph, so text read as double-spaced.
        ui.spacing_mut().interact_size.y = 0.0;
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut changed = false;
        let mut cmd: Option<Edit> = None;
        // a word the context menu was asked to add to the spelling ignore list
        let mut ignore_word: Option<String> = None;
        // a misspelled word the context menu was asked to correct: block, range in its text, and the
        // suggestion to put there
        let mut fix_word: Option<(usize, std::ops::Range<usize>, String)> = None;
        let mut pic: Option<PicAction> = None;
        let mut clip: Option<ClipAction> = None;
        let mut annotate: Option<(usize, String)> = None;
        // a picture asked to be cropped: handled after the panel, like annotate (the overlay needs the
        // source size, which the cache has by then)
        let mut crop_open: Option<(usize, String)> = None;
        let mut move_block: Option<(usize, i32)> = None;
        let mut drop_block: Option<usize> = None;
        let mut toggle_todo: Option<usize> = None;
        // Enter / Backspace at a block edge are document operations, settled after the loop
        let mut join_block: Option<usize> = None;
        let mut split_block: Option<(usize, Vec<Vec<doc::Run>>)> = None;
        let mut caret_to: Option<(usize, usize)> = None;
        // a marking that spans blocks: the keys that act on it are taken before any TextEdit can
        // see them, or the focused block would act on its own little selection first
        let mut drop_blocks: Option<(usize, usize)> = None;
        let mut typed_over: Option<((usize, usize), String)> = None;
        let mut block_rects: Vec<(usize, egui::Rect)> = Vec::new();
        let mut dragged_block: Option<usize> = None;
        // a picture whose corner grip was released this frame
        let mut resize_commit: Option<(usize, u32)> = None;
        // a picture dragged off its block: handed to the XDND drag source after the loop
        let mut drag_out: Option<PathBuf> = None;
        // Shift+Up / Shift+Down marks whole lines from the block the caret is in: every line is its
        // own TextEdit, so this is the keyboard way to mark across them (and the keys are taken
        // before the focused editor can turn them into a one-line selection)
        {
            let (up, down) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowUp),
                    i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowDown),
                )
            });
            if up || down {
                if let Some(anchor) = self.selected_block {
                    let (lo, hi) = self.block_sel.unwrap_or((anchor, anchor));
                    let (lo, hi) = if down {
                        (lo, hi + 1)
                    } else {
                        (lo.saturating_sub(1), hi)
                    };
                    let hi = hi.min(self.doc.blocks.len().saturating_sub(1));
                    if hi > lo {
                        self.block_sel = Some((lo, hi));
                        caret_to = Some((if down { hi } else { lo }, 0));
                    }
                }
            }
        }
        if let Some((a, b)) = self.block_sel {
            let (del, copy, cut) = ui.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
                        || i.consume_key(egui::Modifiers::NONE, egui::Key::Delete),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::C),
                    i.consume_key(egui::Modifiers::COMMAND, egui::Key::X),
                )
            });
            if del {
                drop_blocks = Some((a, b));
            }
            if copy || cut {
                let text: Vec<String> = (a..=b)
                    .filter_map(|i| self.doc.blocks.get(i).map(|blk| blk.text()))
                    .collect();
                ui.ctx().copy_text(text.join("\n"));
                self.flash(if cut {
                    format!("cut {} lines", text.len())
                } else {
                    format!("copied {} lines", text.len())
                });
            }
            if cut {
                // the same key that copies a one-line marking cuts it (the menu's Cut does too)
                drop_blocks = Some((a, b));
            }
            // typing replaces the marking, as in any editor
            let typed = ui.input_mut(|i| {
                let mut t = String::new();
                i.events.retain(|e| match e {
                    egui::Event::Text(s) if s.chars().any(|c| !c.is_control()) => {
                        t.push_str(s);
                        false
                    }
                    _ => true,
                });
                t
            });
            if !typed.is_empty() {
                typed_over = Some(((a, b), typed));
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.block_sel = None;
            }
        }
        let all_tags: Vec<String> = self
            .vault
            .all_tags()
            .into_iter()
            .take(16)
            .map(|(t, _)| t)
            .collect();
        let n = self.doc.blocks.len();
        // where each line starts this frame (status bar) and where the headings are (the rail's
        // outline ticks)
        let mut tops: Vec<f32> = Vec::with_capacity(n);
        let mut heads: Vec<f32> = Vec::new();
        // how wide each picture's caption row came out, so the next frame can centre it under the
        // picture (zero for blocks that are not pictures, and until a picture has been drawn once)
        let mut rows: Vec<f32> = vec![0.0; n];
        // "Pick a date…" or the line's own "+ due" chip opens the floating picker
        let mut pick_due: Option<Option<chrono::NaiveDate>> = None;
        let mut pick_blocks: Option<Vec<usize>> = None;
        let mut pick_at: Option<egui::Pos2> = None;

        for i in 0..n {
            let kind = self.doc.blocks[i].kind;
            // where this line starts on screen: the status bar counts lines from these, the rail
            // marks headings with them
            let line_top = ui.cursor().top();
            tops.push(line_top);
            if matches!(kind, doc::Kind::Heading(_)) {
                heads.push(line_top);
            }
            // ---------------- a picture
            if kind == doc::Kind::Image {
                let reference = self.doc.blocks[i].image.clone();
                let caption = self.doc.blocks[i].text();
                let width = self.doc.blocks[i].width;
                let crop = self.doc.blocks[i].crop;
                let abs_path = self
                    .page
                    .as_ref()
                    .map(|p| vault::asset_path(p, &reference))
                    .unwrap_or_else(|| PathBuf::from(&reference));
                // the width this picture was drawn at, for the size readout below (`None` when it
                // is auto-sized, or when the file is missing and nothing was drawn), and how far it
                // was indented so the caption can sit under its left edge
                // `dims` is the picture's own pixel size (from its header), which the caption row
                // shows next to the file name; it is `None` when there is nothing to show.
                let (shown_width, dims, pic_w, pic_left): (Option<u32>, Option<Vec2>, f32, f32) =
                    ui.horizontal_top(|ui| {
                    ui.allocate_exact_size(Vec2::new(GUTTER - 10.0, 20.0), Sense::hover());
                    // The picture's own size comes from its file header, which costs microseconds:
                    // the layout is exact before any pixels exist, because the pixels are decoded
                    // on a worker thread and land a frame or two later (`imgcache`).
                    let intrinsic = match self.image_cache.source(&abs_path) {
                        imgcache::Source::Known(size) => size,
                        imgcache::Source::Missing => {
                            ui.label(
                                RichText::new(format!("missing picture: {reference}"))
                                    .family(theme::fam(theme::F_MONO))
                                    .size(11.5)
                                    .color(t.danger),
                            );
                            return (None, None, 0.0, 0.0);
                        }
                        imgcache::Source::Unreadable => {
                            ui.label(
                                RichText::new(format!("cannot display {reference}"))
                                    .family(theme::fam(theme::F_MONO))
                                    .size(11.5)
                                    .color(t.warn),
                            );
                            return (None, None, 0.0, 0.0);
                        }
                    };
                    let box_w = (ui.available_width() - 40.0).max(120.0);
                    // While the corner is dragged the picture takes its width from the pointer, not
                    // from the block: the stored width is written only when the drag ends, so a drag
                    // that is abandoned leaves the picture as it was.
                    // A centred picture grows about the column's middle, so the pointer's distance
                    // from that middle is *half* its width — measured from the pointer every frame
                    // either way, so a drag cannot accumulate
                    let grow = if self.img_center { 2.0 } else { 1.0 };
                    let live = self
                        .resizing
                        .filter(|(b, _, _)| *b == i)
                        .and_then(|(_, grab, anchor)| {
                            ui.input(|inp| inp.pointer.interact_pos()).map(|p| {
                                resize_width_from(p.x, grab, anchor, box_w, grow)
                            })
                        });
                    let shown = live.or(width);
                    // The picture's *visible* pixels: a crop narrows the height (and so the height the
                    // note lays out), never the width the user set.
                    let visible = crop_size(crop, intrinsic);
                    let disp = picture_disp(visible, shown, box_w);
                    // Centred when the display setting says so: the picture keeps its own width and
                    // the space to its left is what moves, so a narrow picture sits in the middle of
                    // the column and a full-width one does not move at all.
                    let indent = centre_indent(box_w, disp.x, self.img_center);
                    ui.add_space(indent);
                    let (rect, resp) = ui.allocate_exact_size(disp, Sense::click_and_drag());
                    // Only pictures near the viewport are decoded. A page can hold far more pictures
                    // than it can show, and each one costs a decode and a texture; the ones off
                    // screen reserve their space and nothing else until they scroll in.
                    if near_viewport(ui, rect) {
                        let ppp = ui.ctx().pixels_per_point();
                        // While the corner is dragged the width changes every frame, and a texture
                        // asked for at each of those widths is a decode queued per frame: the picture
                        // then churns through textures as fast as the pointer moves. Ask at 64-pixel
                        // steps and let the GPU stretch the texture we already have in between.
                        let want_pt = if live.is_some() {
                            (disp.x / 64.0).round() * 64.0
                        } else {
                            disp.x
                        };
                        match self
                            .image_cache
                            .preview(ui.ctx(), &abs_path, (want_pt * ppp).round() as u32)
                        {
                            imgcache::Preview::Ready(tex) => paint_picture(
                                ui.painter(),
                                rect,
                                tex.id(),
                                crop_uv(crop),
                                self.img_rounded.then_some(self.img_radius),
                            ),
                            imgcache::Preview::Pending => {
                                picture_pending(ui, rect, &t, None);
                            }
                            imgcache::Preview::Failed => picture_pending(
                                ui,
                                rect,
                                &t,
                                Some(&format!("cannot display {reference}")),
                            ),
                        }
                    }
                    let selected = self.selected_image == Some(i);
                    // The drag running on this picture, if any: the width follows the pointer, so the
                    // stored width is written only when the drag ends and an abandoned drag leaves the
                    // picture as it was.
                    let resizing_this = self.resizing.filter(|(b, _, _)| *b == i);
                    // The corner grip is registered on **every** frame, for every picture, lit or not.
                    // egui decides what a press hit from the *previous* frame's widgets, so a grip that
                    // only exists while the picture is hovered is not the widget the first press
                    // reaches: that press went to the picture's own response and started a drag-out
                    // instead of a resize, which is why grabbing the corner took several attempts.
                    let grip = egui::Rect::from_center_size(
                        rect.max - Vec2::splat(GRIP_CENTRE),
                        Vec2::splat(GRIP_HIT),
                    );
                    let hresp = ui
                        .interact(grip, ui.id().with(("img_resize", i)), Sense::click_and_drag())
                        .on_hover_cursor(egui::CursorIcon::ResizeNwSe);
                    // Hovering the grip counts as hovering the picture. The grip hangs over the corner,
                    // so without this the border blinked off exactly as the pointer arrived, and the
                    // grip vanished from under it.
                    let engaged = selected
                        || resp.hovered()
                        || resp.dragged()
                        || hresp.hovered()
                        || hresp.dragged()
                        || resizing_this.is_some();
                    if engaged {
                        ui.painter().rect_stroke(
                            rect.expand(2.0),
                            CornerRadius::same(6),
                            Stroke::new(1.5_f32, t.accent),
                            StrokeKind::Outside,
                        );
                        // The knob, drawn well inside the square that catches the pointer: the target
                        // is generous, the mark is not clumsy. Bright only when it is the thing under
                        // the pointer.
                        let hot = hresp.hovered() || hresp.dragged();
                        let knob = egui::Rect::from_center_size(
                            grip.center(),
                            Vec2::splat(if hot { 15.0 } else { 13.0 }),
                        );
                        ui.painter().rect_filled(
                            knob,
                            CornerRadius::same(4),
                            if hot { t.accent } else { t.accent.gamma_multiply(0.7) },
                        );
                        ui.painter().line_segment(
                            [
                                knob.min + Vec2::new(3.5, 9.0),
                                knob.max - Vec2::new(3.5, 3.5),
                            ],
                            Stroke::new(1.5_f32, t.bg),
                        );
                    }
                    // Dragging the grip, outside `engaged`: a press that lands on the handle is a
                    // resize even if the frame that decided the hover said otherwise.
                    if hresp.drag_started() {
                        // Where the button actually went down, and the edge the width is measured
                        // from, so not one frame of the drag jumps: the left edge, or the middle of a
                        // centred picture. `press_origin` (not the pointer now) because egui only
                        // calls a drag a drag after the pointer has travelled a few points — measuring
                        // from the pointer at that moment threw the picture out by that distance.
                        let grab = ui
                            .input(|inp| inp.pointer.press_origin().or(inp.pointer.interact_pos()))
                            .map(|p| rect.right() - p.x)
                            .unwrap_or(GRIP_CENTRE);
                        let anchor = if self.img_center {
                            rect.center().x
                        } else {
                            rect.left()
                        };
                        self.resizing = Some((i, grab, anchor));
                        self.selected_image = Some(i);
                    } else if hresp.drag_stopped() {
                        if let Some(w) = live {
                            resize_commit = Some((i, w));
                        }
                        self.resizing = None;
                    }
                    if hresp.clicked() || resp.clicked() {
                        self.selected_image = Some(i);
                    }
                    // A drag that is not on the corner grip is the picture being taken somewhere:
                    // the gesture that means "give this to another app", and the one the drop-target
                    // side has always answered in the other direction. The position is checked as well
                    // as the responses, so a press on the handle can never leave as a dragged file.
                    if resp.drag_started()
                        && !hresp.dragged()
                        && !ui
                            .input(|inp| inp.pointer.interact_pos())
                            .is_some_and(|p| grip.expand(2.0).contains(p))
                    {
                        drag_out = Some(abs_path.clone());
                    }
                    (shown, Some(intrinsic), disp.x, rect.left())
                })
                .inner;
                if let Some(r) = block_rects.last_mut() {
                    if r.0 == i {
                        r.1 = r.1.union(ui.min_rect());
                    }
                }
                // The caption row: file name, the picture's own resolution and the buttons all on the
                // icons' centre line, and the row as a whole centred under the picture rather than hung
                // from its left edge — a full-width picture left its name and buttons stranded far to
                // the left of what they describe. egui can only centre a row whose width it knows, and
                // this row's width is its text's, so the row is measured as it is drawn and the offset
                // comes from that measurement on the next frame (the same one-frame pattern the note's
                // own height uses). A row wider than the picture keeps the picture's left edge: shifting
                // it would hang it into the gutter, outside the column.
                let shift = self
                    .caption_rows
                    .get(i)
                    .copied()
                    .filter(|w| *w > 1.0)
                    .map(|w| {
                        // The row ends in an icon button, whose glyph is centred in a 30 pt box: that
                        // half-empty box is not part of what the eye reads as the row, so it comes off
                        // before the centring or the caption looks a few points left of centre.
                        let glyph = ui
                            .painter()
                            .layout_no_wrap(
                                "×".to_owned(),
                                egui::FontId::new(15.0, theme::fam(theme::F_MED)),
                                t.dim,
                            )
                            .size()
                            .x;
                        // A row wider than the picture is still centred under it, as far as the
                        // column's left edge allows (a narrow picture near the gutter keeps it there
                        // rather than hanging its caption off the column).
                        let inset = pic_left - ui.max_rect().left();
                        ((pic_w - (w - (30.0 - glyph) * 0.5)) * 0.5).max(-inset)
                    })
                    .unwrap_or(0.0);
                let lead = if pic_w > 1.0 {
                    (pic_left - ui.max_rect().left()) + shift
                } else {
                    GUTTER - 10.0
                };
                let row_w = ui
                    .allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), PICTURE_ROW_H),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.add_space(lead);
                            ui.label(
                                RichText::new(caption)
                                    .family(theme::fam(theme::F_MONO))
                                    .size(10.5)
                                    .color(t.faint),
                            );
                            // the file's own pixels, which is what the picture is, not what it is drawn
                            if let Some(d) = dims {
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new(format!(
                                        "{} × {}",
                                        d.x.round() as u32,
                                        d.y.round() as u32
                                    ))
                                    .family(theme::fam(theme::F_MONO))
                                    .size(10.5)
                                    .color(t.dim),
                                );
                            }
                            // the width the picture is drawn at, live while its corner is dragged
                            if self.selected_image == Some(i) {
                                if let Some(w) = shown_width {
                                    ui.add_space(2.0);
                                    ui.label(
                                        RichText::new(format!("{w} px wide"))
                                            .family(theme::fam(theme::F_MONO))
                                            .size(10.5)
                                            .color(t.accent),
                                    );
                                }
                            }
                            // What is kept, while a crop is on: the size of the crop, not the file's.
                            if let Some(c) = crop {
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new(format!(
                                        "crop {}×{}%",
                                        (c.w * 100.0).round() as u32,
                                        (c.h * 100.0).round() as u32
                                    ))
                                    .family(theme::fam(theme::F_MONO))
                                    .size(10.5)
                                    .color(t.dim),
                                );
                            }
                            if theme::icon_button_painted(
                                ui,
                                "crop this picture — reversible, the file is never cut",
                                &t,
                                paint_crop_icon,
                            )
                            .clicked()
                            {
                                crop_open = Some((i, reference.clone()));
                            }
                            if theme::icon_button(ui, "✏", "draw on this picture", &t).clicked() {
                                annotate = Some((i, reference.clone()));
                            }
                            if theme::icon_button(ui, "↑", "move this picture up", &t).clicked() {
                                move_block = Some((i, -1));
                            }
                            if theme::icon_button(ui, "↓", "move this picture down", &t).clicked() {
                                move_block = Some((i, 1));
                            }
                            if theme::icon_button(ui, "×", "remove this picture", &t).clicked() {
                                drop_block = Some(i);
                            }
                            // what the row actually took, for the centring above
                            (ui.min_rect().width() - lead).max(0.0)
                        },
                    )
                    .inner;
                rows[i] = row_w;
                ui.add_space(4.0);
                // A search hit on a picture's caption lands here; without this the page never
                // scrolled to it (and the flag stayed armed for some later text block).
                if self.scroll_to_block == Some(i) {
                    ui.scroll_to_rect(ui.min_rect(), Some(egui::Align::Center));
                    self.scroll_to_block = None;
                }
                continue;
            }
            // ---------------- a rule
            if kind == doc::Kind::Rule {
                ui.horizontal(|ui| {
                    ui.add_space(GUTTER - 10.0);
                    let (r, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width() - 10.0, 9.0),
                        Sense::hover(),
                    );
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(r.center(), Vec2::new(r.width(), 1.0)),
                        CornerRadius::ZERO,
                        t.border,
                    );
                });
                ui.add_space(2.0);
                if self.scroll_to_block == Some(i) {
                    ui.scroll_to_rect(ui.min_rect(), Some(egui::Align::Center));
                    self.scroll_to_block = None;
                }
                continue;
            }

            // ---------------- a text block
            let size = block_size(kind) * self.note_text.scale();
            let runs = self.doc.blocks[i].runs.clone();
            let mut buf: String = self.doc.blocks[i].text();
            let old = buf.clone();
            let done = self.doc.blocks[i].done;
            ui.horizontal_top(|ui| {
                ui.allocate_exact_size(Vec2::new(GUTTER - 10.0, 20.0), Sense::hover());
                // the markers of a document are drawn, not typed: a bullet, a quote bar, a checkbox
                match kind {
                    doc::Kind::Bullet => {
                        ui.label(
                            RichText::new("•")
                                .family(theme::fam(theme::F_BOLD))
                                .size(size)
                                .color(t.accent),
                        );
                    }
                    doc::Kind::Quote => {
                        // A quote's mark is a bar down its side, **painted**. `▏` (U+258F) is in no
                        // bundled face, so drawing it as text produced the missing-glyph box — a
                        // quote looked like an unticked task, and the bar that *is* the quote was
                        // the one thing missing.
                        let (r, _) =
                            ui.allocate_exact_size(Vec2::new(3.0, size * 1.15), Sense::hover());
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(r.left() + 1.0, r.top()),
                                Vec2::new(3.0, r.height()),
                            ),
                            CornerRadius::same(2),
                            t.dim,
                        );
                    }
                    doc::Kind::Todo => {
                        let resp = theme::checkbox(ui, done, &t);
                        if resp.clicked() {
                            toggle_todo = Some(i);
                        }
                    }
                    _ => {}
                }
                let t2 = t;
                let job_runs = runs.clone();
                let mut layouter = |ui: &egui::Ui, b: &dyn egui::TextBuffer, wrap: f32| {
                    let mut job =
                        highlight::runs_job(b.as_str(), &job_runs, &t2, size, kind, done);
                    job.wrap.max_width = wrap;
                    ui.fonts_mut(|f| f.layout_job(job))
                };
                // Backspace at the very start of a block joins it to the one above: a block holds
                // no line break, and without this there is no way to remove one at all. Decided
                // from the TextEdit's own state before the editor handles this frame's keys.
                let block_id = egui::Id::new(("block", i));
                if i > 0 && ui.memory(|m| m.has_focus(block_id)) {
                    let at_start = egui::text_edit::TextEditState::load(ui.ctx(), block_id)
                        .and_then(|st| st.cursor.char_range())
                        .map(|r| r.primary.index == 0 && r.secondary.index == 0)
                        .unwrap_or(false);
                    if at_start && ui.input(|inp| inp.key_pressed(egui::Key::Backspace)) {
                        join_block = Some(i);
                    }
                }
                // A task with no due date carries the same "+ due" chip the tasks view shows, at the
                // end of the line where its date would appear. The text edit gives up that width so
                // the chip cannot sit on top of the words.
                let has_due = runs.iter().any(|r| r.a.due.is_some());
                let due_btn = kind == doc::Kind::Todo && !has_due;
                let text_w = if due_btn {
                    (ui.available_width() - DUE_CHIP_W).max(80.0)
                } else {
                    f32::INFINITY
                };
                // A code line carries the block's panel as its *own* background: each line is its own
                // TextEdit, so a full-width fill on each of them stacks into one block (the 0.5 pt
                // spacing between code lines closes the seams). That is why the run itself no longer
                // paints a chip — see `highlight::F::block_code`.
                let is_code = kind == doc::Kind::Code;
                let is_code_first = is_code && (i == 0 || self.doc.blocks[i - 1].kind != doc::Kind::Code);
                let is_code_last = is_code && (i + 1 >= n || self.doc.blocks[i + 1].kind != doc::Kind::Code);
                if is_code {
                    ui.visuals_mut().extreme_bg_color = t.code_bg;
                    // The run of code lines is one panel, so only the outermost corners are round: a
                    // radius on every line would scallop the block's edge.
                    let r = 8u8;
                    let corners = CornerRadius {
                        nw: if is_code_first { r } else { 0 },
                        ne: if is_code_first { r } else { 0 },
                        sw: if is_code_last { r } else { 0 },
                        se: if is_code_last { r } else { 0 },
                    };
                    ui.visuals_mut().widgets.inactive.corner_radius = corners;
                    ui.visuals_mut().widgets.hovered.corner_radius = corners;
                    ui.visuals_mut().widgets.active.corner_radius = corners;
                }
                // the panel stops where read mode's frame does, not at the canvas edge
                let code_w = (ui.available_width() - 26.0).max(80.0);
                let out = egui::TextEdit::multiline(&mut buf)
                    .id(block_id)
                    .frame(is_code)
                    // no vertical margin: the default 2 pt top+bottom on every block added to the
                    // line pitch and made a note read double-spaced. A code panel is the exception:
                    // its first and last lines carry the block's own padding.
                    .margin(if is_code {
                        Margin {
                            left: 8,
                            right: 8,
                            top: if is_code_first { 5 } else { 1 },
                            bottom: if is_code_last { 5 } else { 1 },
                        }
                    } else {
                        Margin::symmetric(2, 0)
                    })
                    .desired_width(if is_code { code_w } else { text_w })
                    .desired_rows(1)
                    .layouter(&mut layouter)
                    .show(ui);
                // Spelling: a red squiggle under every word this dictionary does not know. Painted
                // after the edit (the text is already drawn) but under the baseline, so it reads as an
                // annotation rather than as part of the glyph, and clipped to the block so a squiggle
                // never bleeds into the line below.
                if self.spell.is_on() {
                    let bad = spell::misspelled(&buf, self.spell, &self.spell_ignore);
                    if !bad.is_empty() {
                        let clip = ui.clip_rect().intersect(out.response.rect.expand(3.0));
                        let painter = ui.painter().with_clip_rect(clip);
                        for range in bad {
                            for r in spell::word_rects(&out.galley, out.galley_pos, range) {
                                spell::paint_squiggle(&painter, r, t.danger);
                            }
                        }
                    }
                }
                if due_btn {
                    // at the end of the task's words, not at the column edge: the laid-out last line
                    // says where the text ends (a long task wraps, and the chip follows the last row)
                    let size = theme::chip_size(ui, "+ due");
                    let end = out.galley.rows.last().map(|r| r.rect());
                    let (x, cy) = match end {
                        Some(r) => (
                            out.galley_pos.x + r.right() + 8.0,
                            out.galley_pos.y + r.center().y,
                        ),
                        None => (out.response.rect.left() + 8.0, out.response.rect.center().y),
                    };
                    let rect = egui::Rect::from_min_size(egui::pos2(x, cy - size.y * 0.5), size);
                    let id = egui::Id::new(("due_chip", i));
                    let chip = theme::chip_at(ui, rect, "+ due", t.faint, &t, id);
                    if chip.clicked() {
                        pick_due = Some(None);
                        pick_blocks = Some(vec![i]);
                        pick_at = Some(rect.left_bottom() + Vec2::new(0.0, 2.0));
                    }
                    chip.on_hover_text("set this task's due date");
                }
                // the lines this menu will act on: the marked range when it covers this block,
                // otherwise this block alone
                let menu_idx: Vec<usize> = match self.block_sel {
                    Some((a, b)) if b > a && a <= i && i <= b => (a..=b).collect(),
                    _ => vec![i],
                };
                let menu_range = if menu_idx.len() > 1 {
                    None
                } else {
                    self.block_selection
                };
                let ms = menu_state(&self.doc, &menu_idx, menu_range);
                // The word the right-click landed on, if it is one this dictionary flags: the menu
                // offers to ignore it, the way it offers to make the line bold. Captured as the menu
                // opens — by the next frame the pointer is over the menu itself.
                if out.response.secondary_clicked() {
                    // The pointer's position on the frame the right-click lands: `interact_pos` while
                    // the button is down, `latest_pos` once it has been released (which is the frame
                    // the menu actually opens on).
                    let at = out
                        .response
                        .interact_pointer_pos()
                        .or_else(|| ui.ctx().input(|i| i.pointer.latest_pos()));
                    self.spell_menu_range =
                        at.and_then(|p| spell::word_at(&out.galley, out.galley_pos, p, &buf));
                    self.spell_menu_word = self
                        .spell_menu_range
                        .as_ref()
                        .map(|r| buf[r.clone()].to_string())
                        .filter(|w| !spell::is_known(w, self.spell, &self.spell_ignore));
                    if self.spell_menu_word.is_none() {
                        self.spell_menu_range = None;
                    }
                    self.spell_menu_fixes = match &self.spell_menu_word {
                        Some(w) => spell::suggest(w, self.spell, 6),
                        None => Vec::new(),
                    };
                }
                let spell_word = self.spell_menu_word.clone();
                let spell_range = self.spell_menu_range.clone();
                // the corrections, and the one the user picks this frame: applied after the loop,
                // through the same run remapping typing uses, so formatting rides along
                let spell_fixes = if spell_word.is_some() {
                    self.spell_menu_fixes.clone()
                } else {
                    Vec::new()
                };
                out.response.context_menu(|ui| {
                    ui.set_min_width(210.0);
                    // Clipboard rows first: they act on the marking, so they are plain actions and
                    // get no checkbox (a tick would claim a state Copy does not have).
                    if menu_action(ui, "Copy", "Ctrl+C", &t) {
                        clip = Some(ClipAction::Copy);
                        ui.close();
                    }
                    if menu_action(ui, "Cut", "Ctrl+X", &t) {
                        clip = Some(ClipAction::Cut);
                        ui.close();
                    }
                    if menu_action(ui, "Paste", "Ctrl+V", &t) {
                        clip = Some(ClipAction::Paste);
                        ui.close();
                    }
                    // Spelling, on the word the click landed on: the only place a word can be
                    // taught without touching the file. Its own section, so it reads as a different
                    // kind of action from the formatting below.
                    if let Some(w) = &spell_word {
                        ui.separator();
                        ui.label(
                            RichText::new(format!("“{w}”"))
                                .family(theme::fam(theme::F_REG))
                                .size(12.0)
                                .color(t.faint),
                        );
                        // the corrections, the likeliest first; pressed, the word is replaced in
                        // place and the squiggle goes with it
                        for fix in &spell_fixes {
                            if menu_action(ui, &spell::match_case(w, fix), "", &t) {
                                if let Some(range) = spell_range.clone() {
                                    fix_word = Some((i, range, spell::match_case(w, fix)));
                                }
                                ui.close();
                            }
                        }
                        if spell_fixes.is_empty() {
                            ui.label(
                                RichText::new("no suggestion close enough")
                                    .family(theme::fam(theme::F_MONO))
                                    .size(10.0)
                                    .color(t.faint),
                            );
                        }
                        ui.separator();
                        if menu_action(ui, &format!("Ignore “{w}”"), "", &t) {
                            ignore_word = Some(w.to_lowercase());
                            ui.close();
                        }
                    }
                    ui.separator();
                    // every row below is a toggle: ticked means the marked lines already are that,
                    // and pressing it again takes it off
                    let h1 = ms.kind == Some(doc::Kind::Heading(1));
                    if menu_toggle(ui, h1, "Heading", &t) {
                        cmd = Some(Edit::Kind(doc::Kind::Heading(1)));
                        ui.close();
                    }
                    if menu_toggle(ui, ms.kind == Some(doc::Kind::Todo), "Task", &t) {
                        cmd = Some(Edit::Kind(doc::Kind::Todo));
                        ui.close();
                    }
                    if menu_toggle(ui, ms.kind == Some(doc::Kind::Bullet), "Bullet", &t) {
                        cmd = Some(Edit::Kind(doc::Kind::Bullet));
                        ui.close();
                    }
                    if menu_toggle(ui, ms.kind == Some(doc::Kind::Quote), "Quote", &t) {
                        cmd = Some(Edit::Kind(doc::Kind::Quote));
                        ui.close();
                    }
                    if menu_toggle(ui, ms.kind == Some(doc::Kind::Para), "Plain text", &t) {
                        cmd = Some(Edit::Kind(doc::Kind::Para));
                        ui.close();
                    }
                    ui.separator();
                    for (label, which, on) in [
                        ("Bold", 0, ms.bold),
                        ("Italic", 1, ms.italic),
                        ("Code", 2, ms.code),
                        ("Strike", 3, ms.strike),
                    ] {
                        if menu_toggle(ui, on, label, &t) {
                            cmd = Some(Edit::Attr(which));
                            ui.close();
                        }
                    }
                    // The highlighter colours as swatches on this level, like `size S M L`: a colour
                    // you can see, one click away, instead of a submenu that hides all five. Pressing
                    // the colour the marked words already carry takes the marker off, as every other
                    // toggle here does.
                    if let Some(mark) = menu_marks_row(ui, &ms.marks, &t) {
                        cmd = Some(Edit::Mark(mark));
                        ui.close();
                    }
                    // text size: S / M / L, the current one filled in; pressing the one already on
                    // returns the text to M
                    ui.horizontal(|ui| {
                        row_label(ui, "size", &t);
                        for (label, step) in [("S", -1i8), ("M", 0), ("L", 1)] {
                            let on = ms.size == Some(step);
                            let text = if on {
                                RichText::new(label).size(13.0).color(t.accent)
                            } else {
                                RichText::new(label).size(13.0)
                            };
                            if ui
                                .add(egui::Button::new(text).min_size(Vec2::new(26.0, 20.0)).selected(on))
                                .clicked()
                            {
                                cmd = Some(Edit::Size(step));
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    // A due date belongs to a task, so the row only exists when every line the menu
                    // will act on is a todo; on a paragraph there is nothing a date could mean.
                    let all_todos = !menu_idx.is_empty()
                        && menu_idx.iter().all(|i| {
                            self.doc
                                .blocks
                                .get(*i)
                                .map(|b| b.kind == doc::Kind::Todo)
                                .unwrap_or(false)
                        });
                    let current_due = menu_idx.iter().find_map(|i| {
                        self.doc
                            .blocks
                            .get(*i)
                            .and_then(|b| b.runs.iter().find_map(|r| r.a.due))
                    });
                    if all_todos {
                        ui.menu_button("Due date", |ui| {
                            let today = chrono::Local::now().date_naive();
                            for (label, days) in [("Today", 0i64), ("Tomorrow", 1), ("In a week", 7)]
                            {
                                let d = today + chrono::Duration::days(days);
                                if menu_action(ui, label, &d.format("%d %b").to_string(), &t) {
                                    cmd = Some(Edit::Due(d));
                                    ui.close();
                                }
                            }
                            ui.separator();
                            // the calendar is a floating picker, not a menu item: a menu closes on
                            // the first click inside it, so it could not be paged
                            if menu_action(ui, "Pick a date…", "", &t) {
                                pick_due = Some(current_due);
                                pick_blocks = Some(menu_idx.clone());
                                ui.close();
                            }
                            if current_due.is_some() && menu_action(ui, "Clear due date", "", &t) {
                                cmd = Some(Edit::DueClear);
                                ui.close();
                            }
                        });
                    }
                    ui.menu_button("Tag", |ui| {
                        if all_tags.is_empty() {
                            ui.label("no tags in the vault yet");
                        }
                        for tag in all_tags.iter() {
                            if menu_toggle(ui, ms.tags.contains(tag), &format!("#{tag}"), &t) {
                                cmd = Some(Edit::Tag(tag.clone()));
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    ui.menu_button("Picture", |ui| {
                        if ui.button("Attach a file…").clicked() {
                            pic = Some(PicAction::Attach);
                            ui.close();
                        }
                        if ui.button("Snip a region of the screen").clicked() {
                            pic = Some(PicAction::Snip);
                            ui.close();
                        }
                        if ui.button("Paste from the clipboard").clicked() {
                            pic = Some(PicAction::Paste);
                            ui.close();
                        }
                    });
                });
                if self.scroll_to_block == Some(i) {
                    ui.scroll_to_rect(out.response.rect, Some(egui::Align::Center));
                    self.scroll_to_block = None;
                }
                block_rects.push((i, out.response.rect));
                if out.response.dragged() {
                    dragged_block = Some(i);
                }
                if out.response.clicked() {
                    // a plain click starts a new marking; a drag builds its own range
                    self.block_sel = None;
                    self.selected_block = Some(i);
                }
                let live: Option<(usize, usize)> = egui::text_edit::TextEditState::load(ui.ctx(), block_id)
                    .and_then(|st| st.cursor.char_range())
                    .map(|r| r.as_sorted_char_range())
                    .map(|r| (r.start, r.end));
                if out.response.has_focus() {
                    self.selected_block = Some(i);
                    self.block_selection = live.filter(|(s, e)| e > s);
                    if let Some((s, e)) = self.block_selection {
                        self.last_sel.insert(i, (s, e));
                    }
                }
                // A right-click inside marked words collapses the marking (egui drops the selection
                // on any button press) — the highlight vanished and the menu had nothing to act on.
                // Put the range back into the editor's state so it paints again, and hand it to the
                // menu.
                if out.response.secondary_clicked() {
                    // The menu's own list above falls back to this block when the click is outside
                    // the marked range, but every command it sets is applied to `self.block_sel`.
                    // Left set, right-clicking line 7 while lines 2-4 were marked made the menu
                    // *show* line 7's state and *edit* lines 2-4.
                    if let Some((a, b)) = self.block_sel {
                        if i < a || i > b {
                            self.block_sel = None;
                        }
                    }
                    let remembered = self.last_sel.get(&i).copied();
                    // collapsed by the click, so the range's start is where the caret landed
                    let caret = live.map(|(s, _)| s);
                    self.selected_block = Some(i);
                    if let (Some((s, e)), Some(at)) = (remembered, caret) {
                        if at >= s && at <= e && e > s {
                            self.block_selection = Some((s, e));
                            let mut st = egui::text_edit::TextEditState::load(ui.ctx(), block_id)
                                .unwrap_or_default();
                            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                                egui::text::CCursor::new(s),
                                egui::text::CCursor::new(e),
                            )));
                            st.store(ui.ctx(), block_id);
                            ui.ctx().memory_mut(|m| m.request_focus(block_id));
                        }
                    }
                }
                if out.response.changed() {
                    // one undo step per burst of typing, taken before the document changes
                    self.snapshot_typing();
                    // fit the existing properties to what was typed: typing inside bold stays bold
                    self.doc.blocks[i].runs = doc::remap_runs(&job_runs, &old, &buf);
                    if buf.contains('\n') {
                        // Enter (or a pasted chunk with line breaks) divides the block — one block
                        // per line, the newline belonging to no block. The edit is applied after
                        // the loop so this frame's drawing is not disturbed by inserted blocks.
                        let runs = self.doc.blocks[i].runs.clone();
                        split_block = Some((i, doc::split_runs_on_newlines(&runs)));
                    }
                    changed = true;
                }
            });
            ui.add_space(space_after(kind));
        }

        // ---------------- a marking that left its block
        if let Some(d) = dragged_block {
            if let Some(py) = ui.input(|i| i.pointer.interact_pos()).map(|p| p.y) {
                let own = block_rects.iter().find(|(i, _)| *i == d).map(|(_, r)| *r);
                if let Some(own) = own {
                    if py < own.top() - 2.0 || py > own.bottom() + 2.0 {
                        // the block under the pointer; whole blocks are marked, which is what a
                        // drag across lines means here
                        let target = block_rects
                            .iter()
                            .filter(|(i, _)| *i != d)
                            .min_by(|a, b| {
                                let da = (a.1.center().y - py).abs();
                                let db = (b.1.center().y - py).abs();
                                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .map(|(i, _)| *i);
                        if let Some(t) = target {
                            self.block_sel = Some((d.min(t), d.max(t)));
                        }
                    }
                }
            }
        }
        // A resize whose button came up without the grip reporting it (a grab lost to another
        // widget) must not leave the picture chasing the pointer for the rest of the session.
        if self.resizing.is_some() && !ui.input(|i| i.pointer.any_down()) {
            self.resizing = None;
        }
        // A brand-new page is one empty paragraph, and it used to be a black rectangle: no caret
        // (nothing had focus), no hint, nothing to say the page was ready. The caret is still claimed
        // by the title — naming the page is the first thing you do — so the body has to say so itself.
        if self.doc.blocks.len() == 1
            && self.doc.blocks[0].kind == doc::Kind::Para
            && self.doc.blocks[0].text().is_empty()
        {
            if let Some(top) = tops.first() {
                ui.painter().text(
                    egui::pos2(ui.max_rect().left() + GUTTER, *top + 1.0),
                    egui::Align2::LEFT_TOP,
                    "start typing — right-click for headings, lists, a picture",
                    FontId::new(
                        block_size(doc::Kind::Para) * self.note_text.scale() * 0.86,
                        theme::note_fam(theme::F_REG),
                    ),
                    t.ghost,
                );
            }
        }
        // this frame's line map, for the status bar and the rail (they draw after the note)
        self.block_tops = tops;
        self.heading_tops = heads;
        self.caption_rows = rows;

        if let Some((a, b)) = self.block_sel {
            if b > a {
                for (i, r) in &block_rects {
                    if *i >= a && *i <= b {
                        ui.painter().rect_filled(
                            r.expand2(Vec2::new(4.0, 2.0)),
                            CornerRadius::same(4),
                            t.accent.gamma_multiply(0.13),
                        );
                    }
                }
            }
        }

        // ---------------- a picture dragged out to another app
        if let Some(file) = drag_out {
            self.start_drag_out(file);
        }

        // ---------------- a picture whose corner was just released
        // A drag that ended without its grip reporting a release (the pointer left the window, the
        // page was switched under it) must not leave the picture following the mouse.
        if self.resizing.is_some() && !ui.input(|i| i.pointer.any_down()) {
            self.resizing = None;
        }

        // ---------------- the menu's edits, applied to the document
        // one undo step for the whole action, taken before any of it lands
        let structural = cmd.is_some()
            || toggle_todo.is_some()
            || drop_block.is_some()
            || move_block.is_some()
            || drop_blocks.is_some()
            || typed_over.is_some()
            || split_block.is_some()
            || join_block.is_some()
            || resize_commit.is_some()
            || matches!(clip, Some(ClipAction::Cut));
        if structural {
            self.snapshot();
        }
        if let (Some(current), Some(blocks)) = (pick_due, pick_blocks) {
            let at = pick_at
                .or_else(|| ui.ctx().input(|i| i.pointer.interact_pos()))
                .unwrap_or_else(|| ui.ctx().content_rect().center());
            self.due_month = month_of(current.unwrap_or_else(|| chrono::Local::now().date_naive()));
            self.due_pick = Some(DuePick {
                at: at + Vec2::new(6.0, 6.0),
                current,
                target: DueTarget::Blocks(blocks),
                fresh: true,
            });
        }
        let targets: Vec<usize> = match self.block_sel {
            Some((a, b)) if b > a => (a..=b).filter(|i| *i < self.doc.blocks.len()).collect(),
            _ => self.selected_block.into_iter().collect(),
        };
        if let Some(edit) = cmd {
            match edit {
                Edit::Kind(k) => {
                    for i in &targets {
                        if let Some(b) = self.doc.blocks.get_mut(*i) {
                            // picking what a line already is turns it back into plain text
                            b.kind = if b.kind == k && k != doc::Kind::Para {
                                doc::Kind::Para
                            } else {
                                k
                            };
                            changed = true;
                        }
                    }
                }
                Edit::Attr(which) => {
                    // inside one block the menu honours the marked words; across blocks it applies
                    // to every block in the range (a marking of whole lines has no single char range)
                    let range = if targets.len() > 1 {
                        None
                    } else {
                        self.block_selection
                    };
                    let on = targets.iter().all(|i| {
                        self.doc
                            .blocks
                            .get(*i)
                            .map(|b| doc::attr_all(&b.runs, range, |a| match which {
                                0 => a.bold,
                                1 => a.italic,
                                2 => a.code,
                                _ => a.strike,
                            }))
                            .unwrap_or(false)
                    });
                    for i in &targets {
                        if let Some(b) = self.doc.blocks.get_mut(*i) {
                            doc::set_attr(&mut b.runs, range, |a| {
                                let v = !on;
                                match which {
                                    0 => a.bold = v,
                                    1 => a.italic = v,
                                    2 => a.code = v,
                                    _ => a.strike = v,
                                }
                            });
                            changed = true;
                        }
                    }
                }
                Edit::Mark(v) => {
                    let range = if targets.len() > 1 {
                        None
                    } else {
                        self.block_selection
                    };
                    for i in &targets {
                        if let Some(b) = self.doc.blocks.get_mut(*i) {
                            doc::set_attr(&mut b.runs, range, |a| a.mark = v);
                            changed = true;
                        }
                    }
                }
                Edit::Due(d) => {
                    // the editor sets a date on a task line exactly as the tasks view does; it never
                    // takes one off (only "Clear due date" does, so a stray press cannot delete)
                    for i in &targets {
                        if let Some(b) = self.doc.blocks.get_mut(*i) {
                            doc::set_due(&mut b.runs, d);
                            changed = true;
                        }
                    }
                }
                Edit::DueClear => {
                    for i in &targets {
                        if let Some(b) = self.doc.blocks.get_mut(*i) {
                            doc::clear_due(&mut b.runs);
                            changed = true;
                        }
                    }
                }
                Edit::Size(step) => {
                    let range = if targets.len() > 1 {
                        None
                    } else {
                        self.block_selection
                    };
                    // the step is a toggle like the rest: pressing the size the text already has
                    // returns it to the block's own size
                    let on = targets.iter().all(|i| {
                        self.doc
                            .blocks
                            .get(*i)
                            .map(|b| doc::attr_all(&b.runs, range, |a| a.size == step))
                            .unwrap_or(false)
                    });
                    let v = if on && step != 0 { 0 } else { step };
                    for i in &targets {
                        if let Some(b) = self.doc.blocks.get_mut(*i) {
                            doc::set_attr(&mut b.runs, range, |a| a.size = v);
                            changed = true;
                        }
                    }
                }
                Edit::Tag(tag) => {
                    for i in &targets {
                        if let Some(b) = self.doc.blocks.get_mut(*i) {
                            let has = b
                                .runs
                                .iter()
                                .any(|r| r.a.tag && r.text.trim_start_matches('#') == tag);
                            if has {
                                // the tag is already there: the menu toggles it off
                                b.runs
                                    .retain(|r| !(r.a.tag && r.text.trim_start_matches('#') == tag));
                            } else {
                                b.runs.push(doc::Run {
                                    text: format!("#{tag}"),
                                    a: doc::Attrs {
                                        tag: true,
                                        ..Default::default()
                                    },
                                });
                            }
                            changed = true;
                        }
                    }
                }
            }
        }
        if let Some(i) = toggle_todo {
            if let Some(b) = self.doc.blocks.get_mut(i) {
                b.done = !b.done;
                changed = true;
            }
        }
        if let Some(i) = drop_block {
            self.doc.blocks.remove(i);
            self.selected_image = None;
            changed = true;
        }
        if let Some((i, w)) = resize_commit {
            // the width belongs to the block, so `to_markdown` writes it out as
            // `<img … width="…">` and the round trip keeps it (the `.md` export, the page list and a
            // plain markdown viewer all read the same number)
            if let Some(b) = self.doc.blocks.get_mut(i) {
                b.width = Some(w);
                changed = true;
                self.flash(format!("picture {w}px wide"));
            }
        }
        if let Some((i, dir)) = move_block {
            let to = (i as i32 + dir).clamp(0, self.doc.blocks.len() as i32 - 1) as usize;
            if to != i {
                let b = self.doc.blocks.remove(i);
                self.doc.blocks.insert(to, b);
                changed = true;
                // the border/grip follow the picture to its new index, not the old one
                if self.selected_image == Some(i) {
                    self.selected_image = Some(to);
                }
            }
        }
        if let Some((i, reference)) = annotate {
            self.open_annotate_block(i, &reference);
        }
        if let Some((i, reference)) = crop_open {
            self.open_crop(i, &reference);
        }
        if let Some(action) = pic {
            match action {
                PicAction::Attach => self.attach_dialog(),
                PicAction::Snip => self.snip_region(),
                PicAction::Paste => {
                    if !self.paste_from_clipboard() {
                        self.flash("no image or file on the clipboard");
                    }
                }
            }
        }
        if let Some(action) = clip {
            match action {
                ClipAction::Copy => match self.editor_selection() {
                    Some((blocks, range, text)) if !text.is_empty() => {
                        ui.ctx().copy_text(text);
                        self.flash(format!("copied {}", clip_summary(blocks.len(), range)));
                    }
                    _ => self.flash("nothing to copy"),
                },
                ClipAction::Cut => {
                    if let Some((blocks, range, text)) = self.editor_selection() {
                        if text.is_empty() {
                            self.flash("nothing to cut");
                        } else {
                            ui.ctx().copy_text(text);
                            caret_to = self.cut_selection(blocks, range);
                            changed = true;
                        }
                    }
                }
                ClipAction::Paste => {
                    // An image or a file on the clipboard still becomes a picture/link (the row in
                    // the Picture submenu does the same); only plain text comes into the line.
                    if !self.paste_from_clipboard() {
                        match attach::clipboard_text() {
                            Some(text) if !text.is_empty() => {
                                let caret = self
                                    .selected_block
                                    .and_then(|i| self.caret_in(ui.ctx(), i));
                                caret_to = self.paste_text(&text, caret);
                                changed = true;
                            }
                            _ => self.flash("nothing on the clipboard to paste"),
                        }
                    }
                }
            }
        }
        if let Some(i) = join_block {
            if i > 0 && i < self.doc.blocks.len() {
                self.last_sel.clear();
                let right = self.doc.blocks.remove(i);
                let at = doc::run_len(&self.doc.blocks[i - 1].runs);
                let left = self.doc.blocks[i - 1].runs.clone();
                self.doc.blocks[i - 1].runs = doc::join_runs(&left, &right.runs);
                caret_to = Some((i - 1, at));
                changed = true;
            }
        }
        if let Some((i, pieces)) = split_block {
            if i < self.doc.blocks.len() {
                self.last_sel.clear();
                let kind = self.doc.blocks[i].kind;
                let mut pieces = pieces.into_iter();
                if let Some(first) = pieces.next() {
                    self.doc.blocks[i].runs = first;
                }
                let mut at = i + 1;
                for p in pieces {
                    let mut b = doc::Block::new(kind);
                    b.runs = p;
                    // a new line starts open even when it continues a ticked todo
                    self.doc.blocks.insert(at, b);
                    at += 1;
                }
                // the text that was pushed down starts the next line, so the caret goes there
                caret_to = Some((i + 1, 0));
                changed = true;
            }
        }
        if let Some((a, b)) = drop_blocks {
            let target = self.remove_blocks(a, b);
            caret_to = Some((target, 0));
            changed = true;
        }
        if let Some(((a, b), typed)) = typed_over {
            let target = self.remove_blocks(a, b);
            let at = typed.chars().count();
            if self
                .doc
                .blocks
                .get(target)
                .map(|b| b.text().is_empty())
                .unwrap_or(false)
            {
                self.doc.blocks[target].runs = vec![doc::Run::plain(typed)];
            } else {
                let kind = self.doc.blocks[target].kind;
                self.doc.blocks.insert(target, doc::Block::plain(kind, typed));
            }
            caret_to = Some((target, at));
            changed = true;
        }
        if let Some((i, range, fix)) = fix_word {
            if let Some(block) = self.doc.blocks.get(i) {
                let old_text = block.text();
                // the range came from this block's text in this frame; anything else is stale
                if range.end <= old_text.len() && old_text.is_char_boundary(range.start) {
                    self.snapshot();
                    let mut new_text = old_text.clone();
                    new_text.replace_range(range, &fix);
                    if let Some(block) = self.doc.blocks.get_mut(i) {
                        // the same remap typing uses: the properties around the word stay put
                        block.runs = doc::remap_runs(&block.runs, &old_text, &new_text);
                    }
                    changed = true;
                    dnd_log(&format!("spell: corrected a word to {fix}"));
                }
            }
        }
        if let Some(word) = ignore_word {
            // Sorted and de-duplicated, so state.json stays diffable and the check stays a linear
            // scan with no duplicates to wade through.
            if !self.spell_ignore.iter().any(|w| *w == word) {
                self.spell_ignore.push(word.clone());
                self.spell_ignore.sort();
            }
            self.save_state_soon();
            self.flash(format!("“{word}” will not be flagged again"));
            dnd_log(&format!("spell: ignored {word}"));
        }
        if changed {
            self.body = doc::to_markdown(&self.doc);
            self.mark_dirty();
        }
        if let Some((bi, at)) = caret_to {
            // claim the caret before the block is drawn next frame (egui reads text events only
            // while a widget has focus, so a request made after it drew arrives a frame late)
            let id = egui::Id::new(("block", bi));
            ui.ctx().memory_mut(|m| m.request_focus(id));
            let mut st = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(at),
            )));
            st.store(ui.ctx(), id);
        }

        // A picture dropped, pasted or snipped into the page is appended at the end, so the note
        // follows it down to its own bottom — otherwise the new picture arrives below the fold and
        // the drop looks like it did nothing. One frame only: the flag is taken here.
        if std::mem::take(&mut self.scroll_to_bottom) {
            ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
        }

        // No legend line under the note: the right-click menu is where those constructs live now,
        // and the row was permanent clutter at the foot of every page.
    }

    /// Hand `file` to the XDND drag source: the mouse is already down on the picture, and a
    /// background thread takes over the pointer from here (see `dragout`). Only one drag at a time —
    /// a second `XdndSelection` owner would be a second cursor on the same job.
    fn start_drag_out(&mut self, file: PathBuf) {
        dnd_log(&format!(
            "drag out: asked for {} (x11_window={:?}, running={})",
            file.display(),
            self.x11_window,
            self.drag_rx.is_some()
        ));
        if self.drag_rx.is_some() {
            return;
        }
        if self.x11_window.is_none() {
            self.flash(crate::platform::drag_out_hint());
            return;
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        match dragout::start(file.clone(), self.x11_window) {
            Some(rx) => {
                self.drag_rx = Some(rx);
                let name = file
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| file.display().to_string());
                self.flash(format!("dragging {name} — drop it on another app"));
            }
            None => self.flash("could not start the drag"),
        }
        // Only the X11/XWayland build has an XDND source; the early return above already covers
        // every other backend, so this arm exists to keep the function total on Windows/macOS.
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        self.flash(crate::platform::drag_out_hint());
    }

    /// Drop blocks `a..=b` (a marking that spans lines) and return the index the caret should land
    /// on. A document always keeps one block, so removing everything leaves one empty line.
    fn remove_blocks(&mut self, a: usize, b: usize) -> usize {
        let n = self.doc.blocks.len();
        if n == 0 {
            return 0;
        }
        let (a, b) = (a.min(n - 1), b.min(n - 1));
        if a > b {
            return a;
        }
        self.doc.blocks.drain(a..=b);
        self.doc.ensure_block();
        self.last_sel.clear();
        self.block_sel = None;
        self.selected_block = None;
        self.block_selection = None;
        a.min(self.doc.blocks.len() - 1)
    }

    // ------------------------------------------------------------ clipboard

    /// What the menu's clipboard rows act on: `(blocks, marked chars inside that block, text)`.
    /// A marking that left its block covers whole lines — each block is its own `TextEdit`, so
    /// egui can only mark inside one of them — otherwise it is the marked words of the block,
    /// otherwise the block's whole line.
    fn editor_selection(&self) -> Option<(Vec<usize>, Option<(usize, usize)>, String)> {
        if let Some((a, b)) = self.block_sel {
            if b > a {
                let blocks: Vec<usize> = (a..=b).filter(|i| *i < self.doc.blocks.len()).collect();
                if blocks.is_empty() {
                    return None;
                }
                let text = blocks
                    .iter()
                    .filter_map(|i| self.doc.blocks.get(*i).map(|blk| blk.text()))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Some((blocks, None, text));
            }
        }
        let i = self.selected_block?;
        let block = self.doc.blocks.get(i)?;
        let text = block.text();
        let n = text.chars().count();
        let range = self
            .block_selection
            .map(|(s, e)| (s.min(n), e.min(n)))
            .filter(|(s, e)| e > s);
        let text = match range {
            Some((s, e)) => text.chars().skip(s).take(e - s).collect(),
            None => text,
        };
        Some((vec![i], range, text))
    }

    /// The caret position (char index) the editor last left in block `i`.
    fn caret_in(&self, ctx: &egui::Context, i: usize) -> Option<usize> {
        egui::text_edit::TextEditState::load(ctx, egui::Id::new(("block", i)))
            .and_then(|st| st.cursor.char_range())
            .map(|r| r.primary.index)
    }

    /// Remove what Copy just took: the marked lines, the marked words, or the whole line. A document
    /// keeps at least one block, so the last line is emptied rather than removed.
    fn cut_selection(
        &mut self,
        blocks: Vec<usize>,
        range: Option<(usize, usize)>,
    ) -> Option<(usize, usize)> {
        match range {
            Some((s, e)) => {
                let i = *blocks.first()?;
                let block = self.doc.blocks.get(i)?;
                let old = block.text();
                let chars: Vec<char> = old.chars().collect();
                let (s, e) = (s.min(chars.len()), e.min(chars.len()));
                let mut new = String::new();
                new.extend(chars[..s].iter());
                new.extend(chars[e..].iter());
                self.doc.blocks[i].runs = doc::remap_runs(&block.runs, &old, &new);
                self.block_selection = None;
                self.last_sel.remove(&i);
                Some((i, s))
            }
            None if blocks.len() > 1 => {
                let at = self.remove_blocks(blocks[0], *blocks.last()?);
                Some((at, 0))
            }
            None => {
                let i = *blocks.first()?;
                if self.doc.blocks.len() > 1 {
                    let at = self.remove_blocks(i, i);
                    Some((at, 0))
                } else {
                    self.doc.blocks[i].runs.clear();
                    self.block_selection = None;
                    self.last_sel.remove(&i);
                    Some((i, 0))
                }
            }
        }
    }

    /// Paste plain text where the marking is: over the marked lines, over the marked words, or at
    /// the caret. Line breaks in the text make several blocks — the same rule typing follows — and
    /// `remap_runs` keeps the properties around the paste, so pasting into a bold word stays bold.
    fn paste_text(&mut self, text: &str, caret: Option<usize>) -> Option<(usize, usize)> {
        let (blocks, range, _) = self.editor_selection()?;
        if blocks.len() > 1 {
            // the marked lines are replaced by the pasted ones
            let at = self.remove_blocks(blocks[0], *blocks.last()?);
            let mut caret = (at, 0);
            for (n, line) in text.split('\n').enumerate() {
                let idx = at + n;
                if n == 0 && self.doc.blocks.get(idx).map(|b| b.text().is_empty()).unwrap_or(false)
                {
                    self.doc.blocks[idx].runs = vec![doc::Run::plain(line)];
                } else {
                    self.doc.blocks.insert(idx, doc::Block::plain(doc::Kind::Para, line));
                }
                caret = (idx, line.chars().count());
            }
            self.block_sel = None;
            self.block_selection = None;
            return Some(caret);
        }
        let i = blocks[0];
        let (old, kind, runs) = {
            let b = self.doc.blocks.get(i)?;
            (b.text(), b.kind, b.runs.clone())
        };
        let chars = old.chars().count();
        let (s, e) = match range {
            Some((s, e)) => (s.min(chars), e.min(chars)),
            None => {
                let at = caret.unwrap_or(chars).min(chars);
                (at, at)
            }
        };
        let pieces = doc::paste_runs(&runs, &old, Some((s, e)), text);
        let tail = chars - e;
        let mut pieces = pieces.into_iter();
        self.doc.blocks[i].runs = pieces.next().unwrap_or_default();
        let (mut at, mut last) = (i + 1, i);
        for p in pieces {
            let mut b = doc::Block::new(kind);
            b.runs = p;
            self.doc.blocks.insert(at, b);
            last = at;
            at += 1;
        }
        // the caret sits where the pasted text ends, not at the end of the line it landed in
        let caret_at = doc::run_len(&self.doc.blocks[last].runs).saturating_sub(tail);
        self.block_selection = None;
        self.last_sel.remove(&i);
        Some((last, caret_at))
    }

    /// Annotate the picture in block `i`.
    /// Open the crop overlay for a picture. The source size comes from the file's header, which costs
    /// microseconds, so the frame is the right shape before any pixels have been decoded.
    ///
    /// A crop that is already on the picture opens as the frame it describes — cropping again adjusts
    /// it rather than starting from the whole picture, and `Reset` is what takes it back to the whole.
    fn open_crop(&mut self, i: usize, reference: &str) {
        let Some(page) = self.page.clone() else { return };
        let abs = vault::asset_path(&page, reference);
        let existing = self.doc.blocks.get(i).and_then(|b| b.crop);
        let src = match self.image_cache.source(&abs) {
            imgcache::Source::Known(size) => size,
            imgcache::Source::Missing => {
                self.flash(format!("cannot crop {reference}: the file is not there"));
                return;
            }
            imgcache::Source::Unreadable => {
                self.flash(format!("cannot crop {reference}: the file cannot be read"));
                return;
            }
        };
        self.crop_edit = Some(CropEdit {
            block: i,
            reference: reference.to_string(),
            abs,
            src,
            rect: crop_px(existing, src),
            aspect: CropAspect::Free,
            view: egui::Rect::NOTHING,
        });
        dnd_log(&format!(
            "crop: opened {reference} ({}x{}) {}",
            src.x,
            src.y,
            if existing.is_some() {
                "with its existing crop"
            } else {
                "whole"
            }
        ));
    }

    /// Commit the overlay's frame to the block: a fraction rect on the image line, or nothing at all
    /// when the frame is the whole picture. The asset is never touched, so this is the only write a
    /// crop ever makes — which is also why any crop can be taken back off.
    fn apply_crop(&mut self, c: CropEdit) {
        let whole = c.rect.left().abs() < 0.5
            && c.rect.top().abs() < 0.5
            && (c.rect.right() - c.src.x).abs() < 0.5
            && (c.rect.bottom() - c.src.y).abs() < 0.5;
        let new = if whole {
            None
        } else {
            Some(doc::Crop::from_px(
                (c.rect.left(), c.rect.top(), c.rect.width(), c.rect.height()),
                (c.src.x, c.src.y),
            ))
        };
        let Some(b) = self.doc.blocks.get(c.block) else {
            return;
        };
        if b.crop == new {
            return;
        }
        self.snapshot();
        if let Some(b) = self.doc.blocks.get_mut(c.block) {
            b.crop = new;
        }
        self.body = doc::to_markdown(&self.doc);
        self.mark_dirty();
        let _ = self.save();
        dnd_log(&format!(
            "crop: {} kept {:.0}x{:.0} of {:.0}x{:.0} ({})",
            c.reference,
            c.rect.width(),
            c.rect.height(),
            c.src.x,
            c.src.y,
            match new {
                Some(n) => format!("{}", n.attr()),
                None => "whole picture again".to_string(),
            }
        ));
    }

    /// The crop overlay: the picture whole, the kept region bright, handles on it, a ratio lock and
    /// three verbs. It owns the window while it is open — `update` returns before the editor draws, so
    /// no keystroke reaches the note behind it.
    fn crop_ui(&mut self, ctx: &egui::Context) {
        let Some(mut c) = self.crop_edit.take() else {
            return;
        };
        let t = self.theme;
        let mut apply = false;
        let mut cancel = false;
        let mut reset = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(t.bg))
            .show(ctx, |ui| {
                // ---- bar: what this is, the ratio lock, and the verbs
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.label(
                        RichText::new("crop")
                            .family(theme::fam(theme::F_MED))
                            .size(14.0)
                            .color(t.heading),
                    );
                    ui.add_space(12.0);
                    for a in CropAspect::ALL {
                        let on = c.aspect == a;
                        if theme::segment_button(ui, a.label(), on, &t)
                            .on_hover_text(a.tip())
                            .clicked()
                            && !on
                        {
                            c.aspect = a;
                            // refit now, so picking 1:1 shows 1:1 instead of waiting for a drag
                            c.rect = fit_ratio(c.rect, a.ratio(), c.src);
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme::toolbar_button(ui, "Cancel", false, &t)
                            .on_hover_text("Esc — leave the picture as it was")
                            .clicked()
                        {
                            cancel = true;
                        }
                        if theme::toolbar_button(ui, "Apply", true, &t)
                            .on_hover_text("Enter — keep the frame")
                            .clicked()
                        {
                            apply = true;
                        }
                        if theme::toolbar_button(ui, "Reset", false, &t)
                            .on_hover_text("back to the whole picture")
                            .clicked()
                        {
                            reset = true;
                        }
                    });
                });

                let stage = ui.available_rect_before_wrap().shrink2(Vec2::new(24.0, 16.0));
                if stage.width() < 40.0 || stage.height() < 40.0 {
                    return;
                }
                // Fit the picture into the stage, never stretched: the frame's numbers stay in source
                // pixels, so nothing here has to be recomputed when the window changes size.
                let scale = (stage.width() / c.src.x.max(1.0))
                    .min(stage.height() / c.src.y.max(1.0))
                    .max(0.0001);
                let view = egui::Rect::from_center_size(stage.center(), c.src * scale);
                c.view = view;
                let to_source = |p: egui::Pos2| -> egui::Pos2 {
                    egui::pos2((p.x - view.min.x) / scale, (p.y - view.min.y) / scale)
                };

                // the picture itself, whole: the scrim is what hides the part being cut
                let want = (view.width() * ui.ctx().pixels_per_point()).round() as u32;
                match self
                    .image_cache
                    .preview(ui.ctx(), &c.abs, want.max(64))
                {
                    imgcache::Preview::Ready(tex) => paint_picture(
                        ui.painter(),
                        view,
                        tex.id(),
                        crop_uv(None),
                        None,
                    ),
                    imgcache::Preview::Pending => picture_pending(ui, view, &t, None),
                    imgcache::Preview::Failed => picture_pending(
                        ui,
                        view,
                        &t,
                        Some(&format!("cannot display {}", c.reference)),
                    ),
                }

                let frame = egui::Rect::from_min_size(
                    view.min + c.rect.min.to_vec2() * scale,
                    c.rect.size() * scale,
                );
                let frame = frame.intersect(view);
                // everything outside the frame is dimmed, so what is kept reads as the picture
                let dim = egui::Color32::from_black_alpha(150);
                for r in [
                    egui::Rect::from_min_max(view.left_top(), egui::pos2(view.right(), frame.top())),
                    egui::Rect::from_min_max(egui::pos2(view.left(), frame.bottom()), view.right_bottom()),
                    egui::Rect::from_min_max(
                        egui::pos2(view.left(), frame.top()),
                        egui::pos2(frame.left(), frame.bottom()),
                    ),
                    egui::Rect::from_min_max(
                        egui::pos2(frame.right(), frame.top()),
                        egui::pos2(view.right(), frame.bottom()),
                    ),
                ] {
                    if r.width() > 0.5 && r.height() > 0.5 {
                        ui.painter().rect_filled(r, CornerRadius::ZERO, dim);
                    }
                }

                // the move: the frame's body drags it (registered first, so the handles sit on top)
                let mv = ui
                    .interact(frame, ui.id().with("crop-move"), Sense::click_and_drag())
                    .on_hover_cursor(egui::CursorIcon::Move);
                if mv.dragged() {
                    c.rect = move_crop(c.rect, mv.drag_delta() / scale, c.src);
                }

                // the handles: eight in freehand, the four corners when a ratio is locked
                let handles: Vec<CropHandle> = if c.aspect.ratio().is_some() {
                    CropHandle::CORNERS.to_vec()
                } else {
                    CropHandle::all()
                };
                for h in handles {
                    let centre = handle_point(frame, h);
                    let r = egui::Rect::from_center_size(centre, Vec2::splat(14.0));
                    let resp = ui
                        .interact(
                            r.expand(4.0),
                            ui.id().with(("crop-handle", h as u8)),
                            Sense::click_and_drag(),
                        )
                        .on_hover_cursor(h.cursor());
                    if resp.dragged() {
                        if let Some(p) = resp.interact_pointer_pos() {
                            c.rect = drag_crop(c.rect, h, to_source(p), c.src, c.aspect.ratio());
                        }
                    }
                    let hot = resp.hovered() || resp.dragged();
                    ui.painter().rect_filled(
                        r,
                        CornerRadius::same(3),
                        if hot { t.heading } else { t.panel },
                    );
                    ui.painter().rect_stroke(
                        r,
                        CornerRadius::same(3),
                        Stroke::new(1.2_f32, t.accent),
                        egui::StrokeKind::Inside,
                    );
                }

                // frame, then the thirds: a crop that lands on them is usually the crop people wanted
                ui.painter().rect_stroke(
                    frame,
                    CornerRadius::ZERO,
                    Stroke::new(1.5_f32, t.accent),
                    egui::StrokeKind::Outside,
                );
                for f in [1.0 / 3.0, 2.0 / 3.0] {
                    let stroke = Stroke::new(1.0_f32, t.accent.gamma_multiply(0.35));
                    let x = frame.left() + frame.width() * f;
                    let y = frame.top() + frame.height() * f;
                    ui.painter()
                        .line_segment([egui::pos2(x, frame.top()), egui::pos2(x, frame.bottom())], stroke);
                    ui.painter()
                        .line_segment([egui::pos2(frame.left(), y), egui::pos2(frame.right(), y)], stroke);
                }

                // what is kept, in the picture's own pixels, and the keys that end this
                let kept = c.rect;
                let hint = format!(
                    "kept {:.0} × {:.0} of {:.0} × {:.0} px      drag the frame to move · {} · Enter applies · Esc cancels",
                    kept.width(),
                    kept.height(),
                    c.src.x,
                    c.src.y,
                    if c.aspect.ratio().is_some() {
                        "a corner resizes, the ratio holds"
                    } else {
                        "handles resize"
                    }
                );
                ui.painter().text(
                    egui::pos2(stage.left(), stage.bottom() + 2.0),
                    egui::Align2::LEFT_TOP,
                    hint,
                    egui::FontId::new(11.0, theme::fam(theme::F_MONO)),
                    t.dim,
                );
            });

        let (enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if cancel || esc {
            dnd_log("crop: cancelled");
            return;
        }
        if reset {
            c.rect = egui::Rect::from_min_size(egui::Pos2::ZERO, c.src);
            c.aspect = CropAspect::Free;
        }
        if apply || enter {
            self.apply_crop(c);
            return;
        }
        self.crop_edit = Some(c);
    }

    fn open_annotate_block(&mut self, i: usize, reference: &str) {
        self.annotate_from_block = Some(i);
        self.open_annotate(reference);
    }

    // ------------------------------------------------------------ attachments

    /// Append markdown for already-resolved snippets to this page and save now
    /// (attachments must not sit in the autosave debounce).
    fn append_block(&mut self, snippets: &[String]) {
        if snippets.is_empty() {
            return;
        }
        self.snapshot();
        let mut body = self.body.trim_end().to_string();
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        for s in snippets {
            body.push_str(s);
            body.push('\n');
        }
        self.body = body;
        // and into the document, which is what the editor shows
        for s in snippets {
            self.doc.blocks.extend(doc::from_markdown(s).blocks);
        }
        self.mark_dirty();
        self.save();
        // and follow it down: what was just attached is at the foot of the page
        self.scroll_to_bottom = true;
    }

    /// Attach files that already exist on disk (drop, paste, file chooser).
    fn attach_paths(&mut self, paths: &[PathBuf]) {
        let Some(page) = self.page.clone() else {
            self.flash("open a page first");
            return;
        };
        let mut snippets = Vec::new();
        let mut failed = 0usize;
        for p in paths {
            match attach::snippet_for_file(&page, p) {
                Ok(s) => snippets.push(s),
                Err(_) => failed += 1,
            }
        }
        if snippets.is_empty() {
            self.flash(if failed > 0 {
                "nothing could be attached"
            } else {
                "nothing to attach"
            });
            return;
        }
        let n = snippets.len();
        self.append_block(&snippets);
        dnd_log(&format!(
            "attached {n} file(s): {}",
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let msg = if n == 1 {
            "attached 1 file".to_string()
        } else {
            format!("attached {n} files")
        };
        self.flash(msg);
    }

    /// Ctrl+V and the toolbar Paste button: an image or a list of files on the
    /// clipboard becomes an attachment. Returns true when something was attached.
    /// Every attempt is logged: a silent failure here is indistinguishable from
    /// "the key never arrived", which is what made this hard to pin down.
    /// Register our own OLE drop target on the window, once, and attach anything it delivered.
    ///
    /// Only Windows needs this: `winit` asks a drag source for `CF_HDROP` alone, and a browser offers
    /// a dragged image as a virtual file instead, so without this the drag never becomes an event.
    #[cfg(windows)]
    fn windows_drops(&mut self, frame: &mut eframe::Frame) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if !self.win_dnd_tried {
            self.win_dnd_tried = true;
            if let Ok(handle) = frame.window_handle() {
                if let RawWindowHandle::Win32(w) = handle.as_raw() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    if crate::windows_dnd::install(w.hwnd.get() as isize, tx) {
                        self.win_dnd = Some(rx);
                        dnd_log("windows drop target installed");
                    } else {
                        dnd_log("windows drop target refused by OLE; the toolkit's own handler stays");
                    }
                }
            }
        }
        let Some(rx) = &self.win_dnd else { return };
        let mut items = Vec::new();
        while let Ok(item) = rx.try_recv() {
            items.push(item);
        }
        for item in items {
            {
                match item {
                    crate::windows_dnd::Dropped::Paths(paths) => {
                        dnd_log(&format!("windows drop: {} real file(s)", paths.len()));
                        self.attach_paths(&paths);
                    }
                    crate::windows_dnd::Dropped::Hover(on) => {
                        // The toolkit's own hover events stopped when we took the drop target over.
                        if on && !self.drop_hover {
                            self.flash("drag detected — release to attach");
                        }
                        self.drop_hover = on;
                    }
                    crate::windows_dnd::Dropped::Refused(formats) => {
                        dnd_log(&format!("windows drop: nothing readable, source offered {formats}"));
                        self.flash("that drop held nothing NoteCraft can read");
                    }
                    crate::windows_dnd::Dropped::Blobs(blobs) => {
                        dnd_log(&format!("windows drop: {} virtual file(s)", blobs.len()));
                        let paths = blob_files(&blobs);
                        if paths.is_empty() {
                            self.flash("could not read what was dropped");
                        } else {
                            self.attach_paths(&paths);
                        }
                    }
                }
            }
        }
    }

    fn paste_from_clipboard(&mut self) -> bool {
        if self.page.is_none() {
            dnd_log("paste: no page open");
            return false;
        }
        if let Some(img) = attach::clipboard_image() {
            let (w, h) = (img.width(), img.height());
            let Some(page) = self.page.clone() else {
                return false;
            };
            match attach::snippet_for_image(&page, &img) {
                Ok(snippet) => {
                    dnd_log(&format!("paste: image {w}x{h}"));
                    self.append_block(&[snippet]);
                    self.flash("image pasted into assets/");
                }
                Err(e) => {
                    dnd_log(&format!("paste: image save failed: {e}"));
                    self.flash(format!("could not save image: {e}"));
                }
            }
            return true;
        }
        let files = attach::clipboard_files();
        if !files.is_empty() {
            dnd_log(&format!(
                "paste: {} file(s): {}",
                files.len(),
                files
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            self.attach_paths(&files);
            return true;
        }
        let text_len = arboard::Clipboard::new()
            .ok()
            .and_then(|mut c| c.get_text().ok())
            .map(|t| t.chars().count())
            .unwrap_or(0);
        dnd_log(&format!(
            "paste: nothing usable on the clipboard (image/files absent, text chars={text_len})"
        ));
        false
    }

    /// Toolbar "Snip" button / Ctrl+Shift+S: grab the screen and put a piece of it in the note.
    ///
    /// The work happens on the next frame in `begin_snip`, which has the `egui::Context` needed to
    /// hide and restore the window; this only records the request so every caller stays a one-liner.
    fn snip_region(&mut self) {
        if self.page.is_none() {
            self.flash("open a page first");
            return;
        }
        if self.pending_snip.is_some() {
            self.flash("a snip is already in progress");
            return;
        }
        self.snip_request = true;
    }

    /// Hide the window, capture the screen behind it, and hand the file to `poll_snip`.
    ///
    /// The window hides itself (`ViewportCommand::Visible`, which every backend implements), a
    /// worker thread waits for the hide to finish and captures the whole screen through
    /// `platform::capture_screen`, then asks for the window back and reports. The region is chosen
    /// afterwards in the app's own crop overlay — no compositor-specific region tool, and the same
    /// code path on X11, Wayland, Windows and macOS.
    fn begin_snip(&mut self, ctx: &egui::Context) {
        dnd_log("snip: requested");
        let path = std::env::temp_dir().join(format!(
            "notecraft-snip-{}.png",
            chrono::Local::now().timestamp_millis()
        ));
        let _ = std::fs::remove_file(&path);
        let (tx, rx) = std::sync::mpsc::channel();
        // egui's Context is Send + Sync: the worker can ask for the window back itself, which is
        // what makes a hidden (and therefore unpainted) window come back without a compositor tool.
        let ctx2 = ctx.clone();
        let path2 = path.clone();
        std::thread::spawn(move || {
            std::thread::sleep(SNIP_HIDE_SETTLE);
            let result = crate::platform::capture_screen(&path2);
            if let Err(e) = &result {
                dnd_log(&format!("snip: capture failed: {e}"));
            }
            ctx2.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx2.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx2.request_repaint();
            let _ = tx.send(result);
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        dnd_log("snip: window hidden, capture running");
        self.pending_snip = Some(PendingSnip {
            path,
            len: 0,
            last_change: Instant::now(),
            started: Instant::now(),
            outcome: Some(rx),
        });
        self.flash("NoteCraft hidden — grabbing the screen behind it");
    }

    /// Attach a finished grab, or give up on one that was cancelled.
    fn poll_snip(&mut self) {
        // The capture thread reports its verdict; a failure ends the wait at once instead of after
        // a timeout, and says why (no tool, no permission, no display).
        let mut failure: Option<String> = None;
        if let Some(p) = self.pending_snip.as_mut() {
            match p.outcome.take() {
                Some(rx) => match rx.try_recv() {
                    Ok(Err(e)) => failure = Some(e),
                    Ok(Ok(())) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
                    Err(std::sync::mpsc::TryRecvError::Empty) => p.outcome = Some(rx),
                },
                None => {}
            }
        }
        if let Some(e) = failure {
            let msg = format!("cannot grab the screen: {e}");
            dnd_log(&format!("snip: {msg}"));
            self.pending_snip = None;
            self.flash(msg);
            return;
        }
        let Some(p) = self.pending_snip.as_mut() else {
            return;
        };
        let len = std::fs::metadata(&p.path).map(|m| m.len()).unwrap_or(0);
        if len != p.len {
            p.len = len;
            p.last_change = Instant::now();
        }
        let stable = p.last_change.elapsed();
        if snip_ready(len, stable, false) {
            let file = p.path.clone();
            self.pending_snip = None;
            dnd_log(&format!("snip: {len} bytes captured -> {file:?}"));
            match open_snip(&file) {
                Ok(shot) => {
                    self.flash("drag a box over the screen grab — Enter uses all of it, Esc cancels");
                    self.snip = Some(shot);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&file);
                    dnd_log(&format!("snip: cannot read the grab: {e}"));
                    self.flash(format!("cannot read the screen grab: {e}"));
                }
            }
            return;
        }
        if p.started.elapsed() > Duration::from_secs(180) {
            let _ = std::fs::remove_file(&p.path);
            self.pending_snip = None;
            dnd_log("snip: cancelled");
            self.flash("snip cancelled");
        }
    }

    /// Toolbar "Attach" button.
    fn attach_dialog(&mut self) {
        let files = attach::choose_files(false);
        if files.is_empty() {
            return; // cancelled
        }
        self.attach_paths(&files);
    }

    /// Read mode: the **document** rendered — the same blocks, kinds, runs and properties the editor
    /// shows, with no text edits and working checkboxes.
    ///
    /// It used to render the markdown export, which cannot show anything markdown cannot express (a run
    /// that is bold *and* italic *and* code, a text size, the tag and due chips) and had to be
    /// regenerated before it agreed with the document.
    fn read_body(&mut self, ui: &mut egui::Ui) {
        let t = self.theme;
        let page = self.page.clone();
        let mut toggles: Vec<usize> = Vec::new();
        let mut i = 0usize;
        let mut tops: Vec<f32> = Vec::with_capacity(self.doc.blocks.len());
        let mut heads: Vec<f32> = Vec::new();
        while i < self.doc.blocks.len() {
            let kind = self.doc.blocks[i].kind;
            // same line map as the editor: blocks are lines, headings are the outline
            let line_top = ui.cursor().top();
            tops.push(line_top);
            if matches!(kind, doc::Kind::Heading(_)) {
                heads.push(line_top);
            }
            let size = block_size(kind) * self.note_text.scale();
            match kind {
                doc::Kind::Rule => {
                    let (r, _) =
                        ui.allocate_exact_size(Vec2::new(ui.available_width(), 9.0), Sense::hover());
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(r.center(), Vec2::new(r.width(), 1.0)),
                        CornerRadius::ZERO,
                        t.border,
                    );
                }
                doc::Kind::Image => {
                    let b = &self.doc.blocks[i];
                    let reference = b.image.clone();
                    let caption = b.text();
                    let width = b.width;
                    let abs = page
                        .as_ref()
                        .map(|p| vault::asset_path(p, &reference))
                        .unwrap_or_else(|| PathBuf::from(&reference));
                    // size from the header, pixels from the cache if they have landed (same rules as
                    // the editor: one document, two views of it)
                    match self.image_cache.source(&abs) {
                        imgcache::Source::Missing => {
                            ui.label(
                                RichText::new(format!("missing picture: {reference}"))
                                    .family(theme::fam(theme::F_MONO))
                                    .size(11.0)
                                    .color(t.danger),
                            );
                        }
                        imgcache::Source::Unreadable => {
                            ui.label(
                                RichText::new(format!("cannot display {reference}"))
                                    .family(theme::fam(theme::F_MONO))
                                    .size(11.0)
                                    .color(t.warn),
                            );
                        }
                        imgcache::Source::Known(intrinsic) => {
                            let box_w = (ui.available_width() - 40.0).max(120.0);
                            // same rule as the editor: the crop changes the height, not the width
                            let disp = picture_disp(crop_size(b.crop, intrinsic), width, box_w);
                            // Same picture, same placement rules as the editor: read mode is a
                            // different view of one document, so a centred or rounded picture
                            // must not change shape when the mode changes.
                            let indent = centre_indent(box_w, disp.x, self.img_center);
                            let (rect, _) = ui.allocate_exact_size(disp, Sense::hover());
                            let rect = rect.translate(Vec2::new(indent, 0.0));
                            if near_viewport(ui, rect) {
                                let ppp = ui.ctx().pixels_per_point();
                                match self
                                    .image_cache
                                    .preview(ui.ctx(), &abs, (disp.x * ppp).round() as u32)
                                {
                                    imgcache::Preview::Ready(tex) => paint_picture(
                                        ui.painter(),
                                        rect,
                                        tex.id(),
                                        crop_uv(b.crop),
                                        self.img_rounded.then_some(self.img_radius),
                                    ),
                                    imgcache::Preview::Pending => {
                                        picture_pending(ui, rect, &t, None);
                                    }
                                    imgcache::Preview::Failed => picture_pending(
                                        ui,
                                        rect,
                                        &t,
                                        Some(&format!("cannot display {reference}")),
                                    ),
                                }
                            }
                            if !caption.trim().is_empty() {
                                ui.horizontal(|ui| {
                                    ui.add_space(indent);
                                    ui.label(
                                        RichText::new(caption)
                                            .family(theme::fam(theme::F_MONO))
                                            .size(10.5)
                                            .color(t.faint),
                                    );
                                });
                            }
                        }
                    }
                }
                doc::Kind::Code => {
                    // consecutive code lines are one panel, like one fence in the export
                    let mut jobs: Vec<egui::text::LayoutJob> = Vec::new();
                    while i < self.doc.blocks.len() && self.doc.blocks[i].kind == doc::Kind::Code {
                        let b = &self.doc.blocks[i];
                        let mut job = highlight::runs_job(&b.text(), &b.runs, &t, size, kind, false);
                        job.wrap.max_width = (ui.available_width() - 26.0).max(80.0);
                        jobs.push(job);
                        i += 1;
                    }
                    egui::Frame::new()
                        .fill(t.code_bg)
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            for job in jobs {
                                ui.label(job);
                            }
                        });
                    ui.add_space(space_after(kind));
                    continue;
                }
                _ => {
                    let text = self.doc.blocks[i].text();
                    let runs = self.doc.blocks[i].runs.clone();
                    let done = self.doc.blocks[i].done;
                    ui.horizontal_top(|ui| {
                        ui.allocate_exact_size(Vec2::new(GUTTER - 10.0, 20.0), Sense::hover());
                        match kind {
                            doc::Kind::Bullet => {
                                ui.label(
                                    RichText::new("•")
                                        .family(theme::fam(theme::F_BOLD))
                                        .size(size)
                                        .color(t.accent),
                                );
                            }
                            doc::Kind::Quote => {
                                // same painted bar in read mode; markdown cannot carry it, so the
                                // words are left plain and the bar is drawn by the reader
                                let (r, _) = ui
                                    .allocate_exact_size(Vec2::new(3.0, size * 1.15), Sense::hover());
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_size(
                                        egui::pos2(r.left() + 1.0, r.top()),
                                        Vec2::new(3.0, r.height()),
                                    ),
                                    CornerRadius::same(2),
                                    t.dim,
                                );
                            }
                            doc::Kind::Todo => {
                                if theme::checkbox(ui, done, &t).clicked() {
                                    toggles.push(i);
                                }
                            }
                            _ => {}
                        }
                        // the strike-through is in the text's own format now, not a painted line
                        let mut job = highlight::runs_job(&text, &runs, &t, size, kind, done);
                        job.wrap.max_width = ui.available_width();
                        ui.label(job);
                    });
                }
            }
            ui.add_space(space_after(kind));
            i += 1;
        }
        self.block_tops = tops;
        self.heading_tops = heads;
        // same rule in read mode: an attachment appended at the end of the page is brought into view
        if std::mem::take(&mut self.scroll_to_bottom) {
            ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
        }
        // the tick goes to the document; the markdown export follows
        if !toggles.is_empty() {
            self.snapshot();
        }
        for i in toggles {
            if let Some(b) = self.doc.blocks.get_mut(i) {
                b.done = !b.done;
                self.body = doc::to_markdown(&self.doc);
                self.mark_dirty();
            }
        }
    }

    fn tasks_view(&mut self, ui: &mut egui::Ui) {
        let t = self.theme;
        let today = chrono::Local::now().date_naive();
        let todos = self.vault.all_todos();
        let mut toggle: Option<(PathBuf, usize)> = None;
        let mut delete_done = false;
        // where the due-date picker should open, when a chip was clicked
        let mut due_pick_at: Option<(egui::Pos2, Option<chrono::NaiveDate>, PathBuf, usize)> = None;
        let mut open: Option<PathBuf> = None;

        let mut groups: Vec<(&str, Vec<&vault::Todo>)> = vec![
            ("Overdue", vec![]),
            ("Today", vec![]),
            ("Tomorrow", vec![]),
            ("Next 7 days", vec![]),
            ("Later", vec![]),
            ("Someday / no date", vec![]),
            ("Done", vec![]),
        ];
        for td in &todos {
            let bucket = if td.done {
                6
            } else {
                match td.due {
                    None => 5,
                    Some(d) if d < today => 0,
                    Some(d) if d == today => 1,
                    Some(d) if d == today + chrono::Duration::days(1) => 2,
                    Some(d) if d <= today + chrono::Duration::days(7) => 3,
                    Some(_) => 4,
                }
            };
            groups[bucket].1.push(td);
        }

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Tasks")
                    .family(theme::fam(theme::F_BOLD))
                    .size(22.0)
                    .color(t.heading),
            );
            ui.label(
                RichText::new(format!(
                    "{} open · {} done",
                    todos.iter().filter(|x| !x.done).count(),
                    todos.iter().filter(|x| x.done).count()
                ))
                .family(theme::fam(theme::F_MONO))
                .size(11.5)
                .color(t.faint),
            );
        });
        ui.add_space(6.0);

        // filters: state, then when it is due. Chips, like the rest of the app — no dialog, no menu.
        let t2 = t;
        let mut status = self.task_status;
        let mut due = self.task_due;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new("status")
                    .family(theme::fam(theme::F_MONO))
                    .size(10.5)
                    .color(t2.faint),
            );
            for (label, v) in [
                ("All", TaskStatus::All),
                ("Open", TaskStatus::Open),
                ("Done", TaskStatus::Done),
            ] {
                if filter_chip(ui, label, status == v, &t2) {
                    status = v;
                }
            }
            ui.add_space(10.0);
            ui.label(
                RichText::new("due")
                    .family(theme::fam(theme::F_MONO))
                    .size(10.5)
                    .color(t2.faint),
            );
            for (label, v) in [
                ("Any", TaskDue::Any),
                ("Overdue", TaskDue::Overdue),
                ("Today", TaskDue::Today),
                ("This week", TaskDue::Week),
                ("Later", TaskDue::Later),
                ("No date", TaskDue::None),
            ] {
                if filter_chip(ui, label, due == v, &t2) {
                    due = v;
                }
            }
        });
        self.task_status = status;
        self.task_due = due;
        ui.add_space(6.0);

        let mut shown = 0usize;
        egui::ScrollArea::vertical()
            .id_salt("tasks_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (bucket, (title, items)) in groups.iter().enumerate() {
                    if !bucket_shown(self.task_status, bucket) {
                        continue;
                    }
                    let items: Vec<&vault::Todo> = items
                        .iter()
                        .copied()
                        .filter(|td| due_matches(td.due, self.task_due, today))
                        .collect();
                    if items.is_empty() {
                        continue;
                    }
                    shown += items.len();
                    let color = if *title == "Overdue" { t.danger } else { t.faint };
                    ui.horizontal(|ui| {
                        theme::section_label(ui, title, &theme::Theme { faint: color, ..t });
                        ui.label(
                            RichText::new(items.len().to_string())
                                .family(theme::fam(theme::F_MONO))
                                .size(10.5)
                                .color(t.faint),
                        );
                    });
                    for td in &items {
                        ui.horizontal(|ui| {
                            let resp = theme::checkbox(ui, td.done, &t);
                            if resp.clicked() {
                                toggle = Some((td.page.clone(), td.line));
                            }
                            // the date is the chip on the right, not words in the line: the markdown
                            // token would render as "due 2026-10-19" beside its own chip. An overdue
                            // task reads in red, words and chip, so it cannot be missed.
                            let overdue = !td.done && td.due.map(|d| d < today).unwrap_or(false);
                            let row_t = if overdue {
                                Theme { text: t.danger, ..t }
                            } else {
                                t
                            };
                            let mut job = highlight::render_line(
                                &format!(
                                    "- [{}] {}",
                                    if td.done { "x" } else { " " },
                                    doc::strip_due_tokens(&td.text)
                                ),
                                &row_t,
                                14.0,
                            );
                            job.wrap.max_width = (ui.available_width() - 300.0).max(140.0);
                            let resp = ui.label(job);
                            if resp.clicked() {
                                open = Some(td.page.clone());
                            }
                            resp.on_hover_text("open the page this lives on");
                            // The due date is a task's property, so this row is the only place it
                            // can be set or cleared: the note editor has no Due date menu any more
                            // (a date on a paragraph made no sense).
                            let (label, col) = match td.due {
                                Some(d) if td.done => (d.format("%d %b").to_string(), t.faint),
                                Some(d) if d < today => {
                                    (format!("overdue {}", d.format("%d %b")), t.danger)
                                }
                                Some(d) if d == today => ("today".to_string(), t.warn),
                                Some(d) => (d.format("%d %b").to_string(), t.dim),
                                None => ("+ due".to_string(), t.faint),
                            };
                            let resp = if overdue {
                                theme::chip_strong(ui, &label, col, &t)
                            } else {
                                theme::chip(ui, &label, col, &t)
                            };
                            let resp = resp.on_hover_text("set this task's due date");
                            if resp.clicked() {
                                due_pick_at = Some((
                                    resp.rect.left_bottom() + Vec2::new(0.0, 2.0),
                                    td.due,
                                    td.page.clone(),
                                    td.line,
                                ));
                            }
                            for tag in &td.tags {
                                if theme::chip(ui, &format!("#{tag}"), t.accent, &t).clicked() {
                                    self.filter_by_tag(&tag);
                                }
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("{} / {}", td.notebook, td.section))
                                            .family(theme::fam(theme::F_MONO))
                                            .size(10.0)
                                            .color(t.faint),
                                    );
                                },
                            );
                        });
                    }
                    ui.add_space(10.0);
                }
                if shown == 0 {
                    ui.label(
                        RichText::new("no tasks match these filters")
                            .family(theme::fam(theme::F_REG))
                            .size(12.5)
                            .color(t.faint),
                    );
                }
                // at the foot of the list: every done task in the vault, whatever the filters say
                let done_total = todos.iter().filter(|x| x.done).count();
                if done_total > 0 {
                    ui.add_space(14.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let label = if done_total == 1 {
                            "Delete 1 done task".to_string()
                        } else {
                            format!("Delete {done_total} done tasks")
                        };
                        if theme::chip(ui, &label, t.danger, &t).clicked() {
                            delete_done = true;
                        }
                        ui.label(
                            RichText::new("the lines are copied into the vault's .trash first")
                                .family(theme::fam(theme::F_MONO))
                                .size(10.0)
                                .color(t.faint),
                        );
                    });
                }
            });

        if let Some((path, line)) = toggle {
            // flush first: the tick rewrites both files from what is on disk
            self.flush_open_page(&path);
            match toggle_todo_in_page(&path, line) {
                Ok(ticked) => {
                    self.reload_page_from_disk(&path);
                    if !ticked {
                        self.flash("that todo is not in the page's document (edited by hand?)");
                    }
                }
                Err(e) => self.flash(format!("could not tick it: {e}")),
            }
        }
        if delete_done {
            self.delete_done_tasks();
        }
        if let Some((at, current, page, line)) = due_pick_at {
            self.due_month = month_of(current.unwrap_or(today));
            self.due_pick = Some(DuePick {
                at,
                current,
                target: DueTarget::Task { page, line },
                fresh: true,
            });
        }
        if let Some(p) = open {
            self.open_page(&p, false);
            self.view = View::Pages;
        }
    }

    fn tags_view(&mut self, ui: &mut egui::Ui) {
        let t = self.theme;
        let tags = self.vault.all_tags();
        ui.label(
            RichText::new("Tags")
                .family(theme::fam(theme::F_BOLD))
                .size(22.0)
                .color(t.heading),
        );
        ui.add_space(6.0);
        if tags.is_empty() {
            ui.label(
                RichText::new("No #tags yet — add one anywhere in a page body.")
                    .family(theme::fam(theme::F_REG))
                    .size(13.0)
                    .color(t.faint),
            );
            return;
        }
        let mut pick: Option<String> = None;
        let mut open: Option<PathBuf> = None;
        egui::ScrollArea::vertical()
            .id_salt("tags_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (tag, count) in &tags {
                        let color = tag_color(&t, tag);
                        if theme::chip(ui, &format!("#{tag}  {count}"), color, &t).clicked() {
                            pick = Some(tag.clone());
                        }
                    }
                });
                ui.add_space(14.0);
                theme::section_label(ui, "Where they live", &t);
                let rows: Vec<(String, PathBuf, usize)> = self
                    .vault
                    .pages
                    .iter()
                    .filter(|p| !p.tags.is_empty())
                    .map(|p| {
                        (
                            format!("{}   ·   {} / {}", p.title, p.notebook, p.section),
                            p.path.clone(),
                            p.tags.len(),
                        )
                    })
                    .collect();
                for (label, path, count) in rows {
                    let resp = theme::nav_row(ui, &label, false, Some(count.to_string()), 0.0, &t);
                    if resp.clicked() {
                        open = Some(path);
                    }
                }
            });
        if let Some(tag) = pick {
            self.filter_by_tag(&tag);
        }
        if let Some(p) = open {
            self.open_page(&p, false);
            self.view = View::Pages;
        }
    }

    fn palette(&mut self, ctx: &egui::Context) {
        let t = self.theme;
        let Some(pal) = self.palette.as_mut() else { return };
        let mut close = false;
        let mut run: Option<Action> = None;
        let items = build_palette(&self.vault, &pal.query);

        egui::Window::new("palette")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 84.0))
            .min_width(620.0)
            .max_width(620.0)
            .frame(
                egui::Frame::new()
                    .fill(t.panel2)
                    .stroke(Stroke::new(1.0_f32, t.border))
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(Margin::same(10)),
            )
            .show(ctx, |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut pal.query)
                        .desired_width(f32::INFINITY)
                        .hint_text("Jump to a page, run a command, find a tag…")
                        .font(egui::TextStyle::Name("H2".into())),
                );
                if pal.focus {
                    resp.request_focus();
                    pal.focus = false;
                }
                if resp.changed() {
                    pal.sel = 0;
                }
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    close = true;
                }
                let down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
                let up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
                if down {
                    pal.sel = (pal.sel + 1).min(items.len().saturating_sub(1));
                }
                if up {
                    pal.sel = pal.sel.saturating_sub(1);
                }
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(6.0);
                if items.is_empty() {
                    ui.label(
                        RichText::new("nothing matches")
                            .family(theme::fam(theme::F_REG))
                            .size(13.0)
                            .color(t.faint),
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt("palette_list")
                    .max_height(340.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // a heading per group: pages, tags, sections and commands are four different
                        // kinds of row, and the list used to say so only with a faint "command"
                        let mut last_group = "";
                        // the first heading sits at the very top of the scroll area, where
                        // `section_label`'s own leading space is clipped by the viewport
                        ui.add_space(3.0);
                        for (i, it) in items.iter().enumerate() {
                            if it.group != last_group {
                                if !last_group.is_empty() {
                                    ui.add_space(4.0);
                                }
                                theme::section_label(ui, it.group, &t);
                                last_group = it.group;
                            }
                            let selected = i == pal.sel;
                            let resp = theme::nav_row(ui, &it.label, selected, None, 0.0, &t);
                            if !it.hint.is_empty() {
                                let hint_galley = ui.fonts_mut(|f| {
                                    f.layout_no_wrap(
                                        it.hint.clone(),
                                        FontId::new(11.0, theme::fam(theme::F_MONO)),
                                        t.faint,
                                    )
                                });
                                let rect = resp.rect;
                                ui.painter().galley(
                                    egui::Pos2::new(
                                        rect.right() - 10.0 - hint_galley.size().x,
                                        rect.center().y - hint_galley.size().y / 2.0,
                                    ),
                                    hint_galley,
                                    t.faint,
                                );
                            }
                            if resp.clicked() {
                                run = Some(it.action.clone());
                                close = true;
                            }
                        }
                    });
                if enter {
                    if let Some(it) = items.get(pal.sel) {
                        run = Some(it.action.clone());
                        close = true;
                    }
                }
            });

        if let Some(a) = run {
            match a {
                Action::OpenPageAt(p, needle, line) => self.land_on_hit(ctx, &p, &needle, line),
                Action::OpenSection(nb, sec) => {
                    self.nb = Some(nb);
                    self.sec = Some(sec);
                    self.view = View::Pages;
                }
                Action::Command(c) => self.run_command(c),
                Action::Tag(tag) => self.filter_by_tag(&tag),
            }
        }
        if close {
            self.palette = None;
        }
    }

    /// Find in the open page (Ctrl+F): a hit count, and Enter / Shift+Enter to walk the matches.
    ///
    /// It searches the document's blocks, not the markdown, so a match cannot be found in a marker
    /// the user cannot see (`**`, `@due(...)`) and the range it lands on is the range egui's caret
    /// wants. Read mode has no caret to put anywhere, so opening find switches to the editor.
    /// Open the find bar, or take the keyboard back to it if it is already up.
    ///
    /// With no page open the bar still comes up, searching the whole vault: Ctrl+F that sometimes
    /// did nothing (it used to focus the section filter instead) is Ctrl+F the user stops trusting.
    fn open_find_bar(&mut self) {
        if self.read_mode {
            self.read_mode = false;
        }
        let scope = if self.page.is_some() {
            FindScope::Section
        } else {
            FindScope::Vault
        };
        match self.find.as_mut() {
            Some(f) => f.focus = true,
            None => {
                self.flash("find: Section · Notebook · All — Enter next, Esc closes");
                self.find = Some(FindBar {
                    query: String::new(),
                    scope,
                    focus: true,
                    current: 0,
                    landed: false,
                });
                // the scope's page texts are read on the first keystroke, not here
                self.find_cache = None;
            }
        }
    }

    fn find_bar(&mut self, ctx: &egui::Context) {
        // taken out of `self` so the handlers below can call `&mut self` methods; put back at the end
        let Some(mut f) = self.find.take() else {
            return;
        };
        let t = self.theme;
        let mut close = false;
        // Some(true) = next match, Some(false) = previous
        let mut step: Option<bool> = None;

        let hits = self.find_in_scope(f.scope, &f.query);
        // The bar is anchored to the top-right corner, so anything it does not fit into falls off
        // the *left* edge — where the scope toggle lives. Clamping it to the window and letting the
        // row wrap keeps every control on screen from a narrow window to a large UI scale.
        let bar_w = (ctx.content_rect().width() - 48.0).clamp(240.0, 620.0);

        // Stepping has to work from wherever the keyboard is: after a jump the caret sits in the
        // *note* (that is what shows the match), so Enter there used to insert a newline instead of
        // moving on. The bar therefore owns Enter while it is up and the keys are either in its
        // field or on a match it put the caret on — and hands the key back everywhere else (a
        // palette, a confirm dialog, or any other block the user clicked into).
        let modal = self.palette.is_some() || self.confirm.is_some();
        let mut keys_theirs = false;
        if !modal {
            let focus = ctx.memory(|m| m.focused());
            let field = egui::Id::new("find_field");
            let match_id = hits
                .get(f.current.min(hits.len().saturating_sub(1)))
                .map(|h| egui::Id::new(("block", h.block)));
            keys_theirs = focus == Some(field) || (f.landed && focus.is_some() && focus == match_id);
        }
        if keys_theirs {
            let enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
            let shift_enter =
                ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter));
            if enter {
                step = Some(true);
            } else if shift_enter {
                step = Some(false);
            }
        }
        // F3 / Shift+F3 are find-only keys: they mean "again" with the bar up, whatever has focus
        if !modal {
            let f3 = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F3));
            let shift_f3 = ctx.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT, egui::Key::F3));
            if step.is_none() {
                if f3 {
                    step = Some(true);
                } else if shift_f3 {
                    step = Some(false);
                }
            }
        }
        if !modal && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            close = true;
        }
        egui::Window::new("find")
            .id(egui::Id::new("find_bar"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(Align2::RIGHT_TOP, Vec2::new(-16.0, 94.0))
            .max_width(bar_w)
            .frame(
                egui::Frame::new()
                    .fill(t.panel2)
                    .stroke(Stroke::new(1.0_f32, t.accent.gamma_multiply(0.45)))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(8)),
            )
            .show(ctx, |ui| {
                ui.set_max_width(bar_w - 16.0);
                ui.horizontal_wrapped(|ui| {
                    // No toggle here: the permanent `find in …` row sits directly above this bar and
                    // owns the scope, so the same three buttons would only crowd the field.
                    let field = egui::Id::new("find_field");
                    let resp = ui
                        .add(
                            egui::TextEdit::singleline(&mut f.query)
                                .id(field)
                                .hint_text("find")
                                .desired_width(150.0),
                        )
                        .on_hover_text(
                            "Enter next · Shift+Enter previous · F3 / Shift+F3 · Esc closes",
                        );
                    if f.focus {
                        resp.request_focus();
                        f.focus = false;
                    }
                    if resp.changed() {
                        f.current = 0;
                        f.landed = false;
                    }
                    let (label, col) = if f.query.trim().is_empty() {
                        ("type to find".to_string(), t.faint)
                    } else if hits.is_empty() {
                        ("no matches".to_string(), t.warn)
                    } else {
                        // on another page, say which: the bar is the only thing that shows it
                        let other = hits
                            .get(f.current.min(hits.len() - 1))
                            .filter(|h| Some(h.page.as_path()) != self.page.as_deref())
                            .map(|h| short_title(&h.page_title, 16));
                        (find_label(f.current, hits.len(), other.as_deref()), t.dim)
                    };
                    // A fixed-width box: the count, and the page name after it, would otherwise
                    // resize the bar on every keystroke — which moved the scope buttons out from
                    // under the pointer while the user was typing or stepping.
                    ui.add_sized(
                        [92.0, 18.0],
                        egui::Label::new(
                            RichText::new(label)
                                .family(theme::fam(theme::F_MONO))
                                .size(11.0)
                                .color(col),
                        )
                        .truncate(),
                    );
                    if theme::icon_button(ui, "↑", "previous match (Shift+Enter, Shift+F3)", &t)
                        .clicked()
                    {
                        step = Some(false);
                    }
                    if theme::icon_button(ui, "↓", "next match (Enter, F3)", &t).clicked() {
                        step = Some(true);
                    }
                    if theme::icon_button(ui, "×", "close (Esc)", &t).clicked() {
                        close = true;
                    }
                });
            });

        if close {
            self.find = None;
            self.find_cache = None;
            return;
        }
        if let Some(next) = step {
            if !hits.is_empty() {
                let len = hits.len();
                // the first Enter after typing lands on match 1 rather than skipping it
                f.current = if !f.landed {
                    0
                } else if next {
                    (f.current + 1) % len
                } else {
                    (f.current + len - 1) % len
                };
                f.landed = true;
                let hit = hits[f.current.min(len - 1)].clone();
                // stepping off the end is a real jump on a multi-page scope; say so
                if len > 1 && next && f.current == 0 {
                    self.flash(format!("wrapped to the first of {len} matches"));
                }
                self.land_on_find_hit(&hit);
            } else if !f.query.trim().is_empty() {
                let where_ = match f.scope {
                    FindScope::Section => "this section",
                    FindScope::Notebook => "this notebook",
                    FindScope::Vault => "the vault",
                };
                self.flash(format!("no matches in {where_}"));
            }
        }
        self.find = Some(f);
    }

    /// The "restore from .trash" dialog. Deletes are never hard deletes, so this is the way back
    /// without leaving the app for a file manager.
    fn trash_dialog(&mut self, ctx: &egui::Context) {
        if !self.trash_open {
            return;
        }
        let t = self.theme;
        let mut close = false;
        let entries = Ops::trash_entries(&self.vault.root);
        let (nb, sec) = match (self.nb.clone(), self.sec.clone()) {
            (Some(n), Some(s)) => (n, s),
            _ => ("Personal".to_string(), "Notes".to_string()),
        };
        let mut restore: Option<vault::TrashEntry> = None;
        egui::Window::new("trash")
            .id(egui::Id::new("trash_dialog"))
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .min_width(560.0)
            .max_width(560.0)
            .frame(
                egui::Frame::new()
                    .fill(t.panel2)
                    .stroke(Stroke::new(1.0_f32, t.border))
                    .corner_radius(CornerRadius::same(14))
                    .inner_margin(Margin::same(14)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Deleted pages")
                        .family(theme::fam(theme::F_SEMI))
                        .size(15.0)
                        .color(t.heading),
                );
                ui.label(
                    RichText::new(format!(
                        "Pages go back into “{nb} / {sec}” — the section you have open. \
                         Notebook and section folders go back to the top of the vault."
                    ))
                    .family(theme::fam(theme::F_REG))
                    .size(12.0)
                    .color(t.dim),
                );
                ui.add_space(8.0);
                if entries.is_empty() {
                    ui.label(
                        RichText::new("Nothing in the trash.")
                            .family(theme::fam(theme::F_REG))
                            .size(12.5)
                            .color(t.faint),
                    );
                }
                egui::ScrollArea::vertical()
                    .id_salt("trash_scroll")
                    .max_height(320.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for e in &entries {
                            ui.horizontal(|ui| {
                                let kind = if e.is_dir { "folder" } else { "page" };
                                ui.label(
                                    RichText::new(format!("{kind} · {}", e.label))
                                        .family(theme::fam(theme::F_MED))
                                        .size(12.5)
                                        .color(t.text),
                                );
                                ui.label(
                                    RichText::new(e.when.clone())
                                        .family(theme::fam(theme::F_MONO))
                                        .size(10.5)
                                        .color(t.faint),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if theme::toolbar_button(ui, "Restore", false, &t).clicked()
                                        {
                                            restore = Some(e.clone());
                                        }
                                    },
                                );
                            });
                        }
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if theme::toolbar_button(ui, "Close", false, &t).clicked() {
                        close = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
            });
        if let Some(e) = restore {
            let result = if e.is_dir {
                Ops::restore_folder(&self.vault.root, &e.path)
            } else {
                Ops::restore_page(&self.vault.root, &e.path, &nb, &sec)
            };
            match result {
                Ok(p) => {
                    let name = p
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.vault.reload();
                    self.flash(format!("restored {name}"));
                    if !e.is_dir {
                        self.open_page(&p, false);
                    }
                    close = true;
                }
                Err(err) => self.flash(format!("restore failed: {err}")),
            }
        }
        if close {
            self.trash_open = false;
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let t = self.theme;
        // rename page
        if let Some(path) = self.rename_for.clone() {
            let mut close = false;
            let mut commit = false;
            egui::Window::new("rename")
                .title_bar(false)
                .resizable(false)
                .collapsible(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .min_width(420.0)
                .max_width(420.0)
                .frame(
                    egui::Frame::new()
                        .fill(t.panel2)
                        .stroke(Stroke::new(1.0_f32, t.border))
                        .corner_radius(CornerRadius::same(14))
                        .inner_margin(Margin::same(14)),
                )
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new("Rename page")
                            .family(theme::fam(theme::F_SEMI))
                            .size(15.0)
                            .color(t.heading),
                    );
                    // Claim the field *before* it is drawn, and mark the whole title: a TextEdit
                    // only reads Text events while it has focus, so a request made after the draw
                    // arrives a frame late — the first keystroke of the new name was dropped (or
                    // went to the note behind the dialog) and the rest was appended to the old
                    // title, which read as "I change the name but it will not save it".
                    let field = egui::Id::new("rename_field");
                    if !ui.memory(|m| m.has_focus(field)) {
                        let all = egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(self.rename_buf.chars().count()),
                        );
                        let mut st = egui::text_edit::TextEditState::load(ui.ctx(), field)
                            .unwrap_or_default();
                        st.cursor.set_char_range(Some(all));
                        st.store(ui.ctx(), field);
                        ui.memory_mut(|m| m.request_focus(field));
                    }
                    ui.add(
                        egui::TextEdit::singleline(&mut self.rename_buf)
                            .id(field)
                            .desired_width(f32::INFINITY)
                            .font(egui::TextStyle::Name("H3".into())),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if theme::toolbar_button(ui, "Rename", true, &t).clicked() {
                            commit = true;
                        }
                        if theme::toolbar_button(ui, "Cancel", false, &t).clicked() {
                            close = true;
                        }
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        commit = true;
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
            if commit {
                // Anything still in the buffer belongs in the file *before* it is renamed: the
                // rename rewrites the page from what is on disk, and the page-level save that
                // `open_page` runs on the way to the new name would then write the old path again —
                // recreating the old page beside the renamed one (the buffer's words landing in the
                // resurrected file, the rename apparently doing nothing).
                let mut from = path.clone();
                if self.page.as_deref() == Some(path.as_path()) && self.dirty {
                    self.save();
                    if let Some(p) = self.page.clone() {
                        from = p;
                    }
                }
                match Ops::rename_page(&from, &self.rename_buf) {
                    Ok(newp) => {
                        self.vault.reload();
                        if self.page.as_deref() == Some(from.as_path()) {
                            // the buffer was flushed above and the file now has another name:
                            // claim the new path first, so no later save can touch the old one
                            self.dirty = false;
                            self.last_edit = None;
                            self.page = Some(newp.clone());
                            self.open_page(&newp, false);
                        }
                        self.flash("renamed");
                        close = true;
                    }
                    Err(e) => self.flash(format!("rename failed: {e}")),
                }
            }
            if close {
                self.rename_for = None;
            }
        }

        // delete confirm — a page, a notebook or a section
        if let Some(what) = self.confirm.clone() {
            let (heading, detail) = match &what {
                Confirm::DeletePage => (
                    "Delete this page?".to_string(),
                    "It moves to <vault>/.trash — never hard-deleted.".to_string(),
                ),
                Confirm::DeleteNotebook(name) => (
                    format!("Delete the notebook “{name}”?"),
                    "Its sections, pages, pictures and documents move into <vault>/.trash together, \
                     so the whole notebook can be recovered."
                        .to_string(),
                ),
                Confirm::DeleteSection(nb, sec) => (
                    format!("Delete the section “{sec}”?"),
                    format!("Every page in “{nb} / {sec}” moves to <vault>/.trash — never hard-deleted."),
                ),
            };
            let mut close = false;
            egui::Window::new("confirm")
                .title_bar(false)
                .resizable(false)
                .collapsible(false)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .min_width(440.0)
                .max_width(440.0)
                .frame(
                    egui::Frame::new()
                        .fill(t.panel2)
                        .stroke(Stroke::new(1.0_f32, t.danger))
                        .corner_radius(CornerRadius::same(14))
                        .inner_margin(Margin::same(14)),
                )
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new(heading)
                            .family(theme::fam(theme::F_SEMI))
                            .size(15.0)
                            .color(t.heading),
                    );
                    ui.label(
                        RichText::new(detail)
                            .family(theme::fam(theme::F_REG))
                            .size(12.0)
                            .color(t.dim),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if theme::toolbar_button(ui, "Move to trash", true, &t).clicked() {
                            self.delete_confirmed(&what);
                            close = true;
                        }
                        if theme::toolbar_button(ui, "Cancel", false, &t).clicked() {
                            close = true;
                        }
                    });
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        close = true;
                    }
                });
            if close {
                self.confirm = None;
                self.move_for = None;
            }
        }
    }
}

impl eframe::App for NoteCraft {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Pictures first: collect the decodes that finished since the last frame and drop textures
        // over budget, so the rest of the frame only ever reads sizes and draws what is ready.
        self.image_cache.begin_frame(ctx);
        // A picture can only be dragged out through XDND, which needs an X11 window of our own: on a
        // native Wayland window there is none, and the drag says so instead of doing nothing.
        self.x11_window = x11_window_of(frame);
        // Windows: take the window's drop target for ourselves so browser drags (virtual files,
        // PNG bytes) arrive at all, and drain whatever landed since the last frame.
        #[cfg(windows)]
        self.windows_drops(frame);
        // A theme change has to reach egui's own widgets, not only the app's painted ones.
        self.sync_theme(ctx);
        self.tick(ctx);
        // A snip asked for by a button or a shortcut starts here, where the context is available.
        if std::mem::take(&mut self.snip_request) {
            self.begin_snip(ctx);
        }
        // The crop overlay owns the window while a picture is being cropped: no key or click may reach
        // the note behind it (Enter applies, Esc cancels, and a Ctrl+N must not create a page behind a
        // modal), and the overlay draws the whole window itself.
        if self.crop_edit.is_some() {
            self.crop_ui(ctx);
            return;
        }
        // Attachments first: they must not depend on which view is showing.
        self.global_attachments(ctx);
        self.shortcuts(ctx);
        self.page_scroll_keys(ctx);
        // claim the search result's words before anything draws: the request made in the frame the
        // row was clicked is cleared by that click
        if let Some((i, range)) = self.pending_land.take() {
            let id = egui::Id::new(("block", i));
            let mut st = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(range.0),
                egui::text::CCursor::new(range.1),
            )));
            st.store(ctx, id);
            ctx.memory_mut(|m| m.request_focus(id));
        }
        self.typing_wants_edit(ctx);
        // The snip overlay owns the screen while a grab is being cropped.
        if self.snip.is_some() {
            self.snip_viewport(ctx);
        }
        if self.annotate.is_some() {
            self.annotate_viewport(ctx);
        }
        // The breadcrumb left the toolbar (its slot is the find toggle now), so the window title
        // carries the place instead: it costs no in-app space and is visible while switching apps.
        let want_title = match (&self.nb, &self.sec) {
            (Some(n), Some(s)) => format!("{n} / {s} — NoteCraft"),
            (Some(n), None) => format!("{n} — NoteCraft"),
            _ => "NoteCraft".to_owned(),
        };
        if want_title != self.title_sent {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(want_title.clone()));
            self.title_sent = want_title;
        }
        self.top_bar(ctx);
        self.find_scope_row(ctx);
        self.status_bar(ctx);
        if self.rail_open {
            self.left_rail(ctx);
        } else {
            self.panel_handle(ctx, "rail_handle", true, "open the notebooks panel (Ctrl+1)");
        }
        if self.pages_open {
            self.page_panel(ctx);
        } else {
            self.panel_handle(ctx, "pages_handle", false, "open the page list (Ctrl+2)");
        }
        self.find_bar(ctx);
        self.central(ctx);
        self.due_picker(ctx);
        self.dialogs(ctx);
        self.trash_dialog(ctx);
        self.palette(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.dirty {
            self.save();
        }
        save_state(self);
    }
}

// ------------------------------------------------------------------ helpers

/// Width kept free at the right of the note column for the note's rail: the rail is drawn in that
/// strip, so without this the last characters of a line on a narrow window sit underneath it. It is
/// wider than egui's own scroll bar was: a 14 pt rail with a 2 pt groove was hard to see, let alone
/// aim at.
const PAGE_SCROLLBAR_ALLOWANCE: f32 = 22.0;

/// Is a picture close enough to the viewport to be worth decoding?
///
/// A page can hold far more pictures than it can show at once, and every one that is decoded costs
/// a PNG inflate and a texture; egui lays out every block whether or not it is on screen. A screen
/// of margin each way keeps scrolling from landing on a placeholder. Culling is only a hint for the
/// work — the space is reserved either way, so the layout does not move when a picture loads.
fn near_viewport(ui: &egui::Ui, rect: egui::Rect) -> bool {
    let clip = ui.clip_rect();
    let margin = clip.height().max(clip.width());
    clip.expand(margin).intersects(rect)
}

/// What stands in for a picture whose pixels are not ready. The space is already reserved by the
/// time this is drawn, so it only has to say that the picture is coming — or why it never will.
fn picture_pending(ui: &egui::Ui, rect: egui::Rect, t: &Theme, message: Option<&str>) {
    let radius = CornerRadius::same(6);
    ui.painter().rect_filled(rect, radius, t.panel2);
    if let Some(msg) = message {
        ui.painter()
            .rect_stroke(rect, radius, Stroke::new(1.0_f32, t.warn), StrokeKind::Inside);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            msg,
            FontId::new(11.0, theme::fam(theme::F_MONO)),
            t.warn,
        );
    }
}

/// The size an unsized picture is drawn at: the **full width** of the column it sits in, at the
/// file's own aspect ratio.
///
/// This is what "drop a picture into the note" means — it arrives as wide as the page, not shrunk to
/// a 640 pt box. Two limits stay: the picture is never drawn wider than its own pixels (a small icon
/// stays small instead of being blown up into a blur), and there is no height cap, so a tall portrait
/// screenshot is as tall as its aspect makes it. The corner grip is there to make one smaller.
fn default_size(intrinsic: Vec2, box_w: f32) -> Vec2 {
    let w = box_w.min(intrinsic.x.max(1.0)).max(1.0);
    Vec2::new(w, (intrinsic.y * (w / intrinsic.x.max(1.0))).max(1.0))
}

/// How far a picture is pushed right to sit in the middle of the column it is measured against.
/// `box_w` is the width available to the picture (which already leaves room for the resize grip),
/// so a picture that fills the column is not moved at all and a narrow one gets equal margins.
fn centre_indent(box_w: f32, picture_w: f32, centre: bool) -> f32 {
    if !centre {
        return 0.0;
    }
    ((box_w - picture_w) * 0.5).max(0.0)
}

/// The corner radius a rounded picture is drawn with: the step's fraction of its shorter side, so a
/// thumbnail keeps a corner rather than turning into a pill and a big photo is still rounded the same
/// way. The clamps only keep the rounding visible on something tiny and sane on something huge.
fn picture_radius(rect: egui::Rect, step: CornerStep) -> u8 {
    (rect.width().min(rect.height()) * step.fraction())
        .clamp(2.0, 44.0)
        .round() as u8
}

/// The part of the picture that is shown: the crop's rect in source pixels.
fn crop_px(crop: Option<doc::Crop>, src: Vec2) -> egui::Rect {
    let c = crop.unwrap_or(doc::Crop::FULL).clamped();
    egui::Rect::from_min_size(
        egui::pos2(c.x * src.x, c.y * src.y),
        Vec2::new(c.w * src.x, c.h * src.y),
    )
}

/// The size the *kept* part of a picture has in its own pixels — the picture's visible aspect, which is
/// what the note lays out. Cropping narrows the picture's height, never its width.
fn crop_size(crop: Option<doc::Crop>, src: Vec2) -> Vec2 {
    let r = crop_px(crop, src);
    Vec2::new(r.width().max(1.0), r.height().max(1.0))
}

/// The texture coordinates of the kept region: what a cropped draw hands `Brush`/`Painter::image`.
fn crop_uv(crop: Option<doc::Crop>) -> egui::Rect {
    let c = crop.unwrap_or(doc::Crop::FULL).clamped();
    egui::Rect::from_min_size(egui::pos2(c.x, c.y), Vec2::new(c.w, c.h))
}

/// The size a picture is drawn at: its stored width (or as much of the column as it needs), scaled from
/// the *visible* pixels, so a crop changes the height and leaves the width alone.
fn picture_disp(visible: Vec2, width: Option<u32>, box_w: f32) -> Vec2 {
    match width {
        Some(w) => {
            let w = (w as f32).min(box_w).max(MIN_IMAGE_W);
            let scale = w / visible.x.max(1.0);
            Vec2::new(w, (visible.y * scale).max(1.0))
        }
        None => default_size(visible, box_w),
    }
}

/// Draw a picture into an exact rect, with rounded corners at the given step when there is one.
///
/// The rounded case is a *textured* rounded rect: `RectShape` carries a `Brush`, and the tessellator
/// clips the texture's own UVs to the corner radius. That is why no mask texture (and no clip rect,
/// which egui cannot round) is needed here.
///
/// `uv` is the part of the texture to draw: the whole picture, or the kept region of a cropped one. A
/// crop is a UV rect and nothing else — the asset is never re-encoded, which is what makes it
/// reversible.
fn paint_picture(
    painter: &egui::Painter,
    rect: egui::Rect,
    tex: egui::TextureId,
    uv: egui::Rect,
    radius: Option<CornerStep>,
) {
    let Some(step) = radius else {
        painter.image(tex, rect, uv, egui::Color32::WHITE);
        return;
    };
    painter.add(egui::Shape::Rect(egui::epaint::RectShape {
        rect,
        corner_radius: CornerRadius::same(picture_radius(rect, step)),
        fill: egui::Color32::WHITE,
        stroke: Stroke::NONE,
        stroke_kind: StrokeKind::Inside,
        round_to_pixels: None,
        blur_width: 0.0,
        brush: Some(std::sync::Arc::new(egui::epaint::Brush {
            fill_texture_id: tex,
            uv,
        })),
    }));
}

/// Where a crop handle sits on the frame that is drawn for it (screen space).
fn handle_point(frame: egui::Rect, h: CropHandle) -> egui::Pos2 {
    let (c, l, r, t, b) = (
        frame.center(),
        frame.left(),
        frame.right(),
        frame.top(),
        frame.bottom(),
    );
    match h {
        CropHandle::Nw => egui::pos2(l, t),
        CropHandle::N => egui::pos2(c.x, t),
        CropHandle::Ne => egui::pos2(r, t),
        CropHandle::E => egui::pos2(r, c.y),
        CropHandle::Se => egui::pos2(r, b),
        CropHandle::S => egui::pos2(c.x, b),
        CropHandle::Sw => egui::pos2(l, b),
        CropHandle::W => egui::pos2(l, c.y),
    }
}

/// Carry a crop's frame with the pointer, kept inside the picture. Used by the frame's body: a move
/// never changes the frame's size, so the clamps only stop it leaving the picture.
fn move_crop(rect: egui::Rect, delta: Vec2, src: Vec2) -> egui::Rect {
    let dx = delta.x.clamp(-rect.left(), src.x - rect.right());
    let dy = delta.y.clamp(-rect.top(), src.y - rect.bottom());
    rect.translate(Vec2::new(dx, dy))
}

/// Drag one handle to `to` (source pixels), staying inside the picture and never smaller than
/// `Crop::MIN` on a side.
///
/// Freehand the moving edges follow the pointer and the opposite edges stay put; a handle dragged past
/// its opposite edge stops at the minimum instead of inverting the frame. With a ratio locked the
/// pointer's distance from the **anchor** (the opposite corner) sets the size, the shape follows the
/// ratio, and the frame is cut down to whatever room the picture leaves on that side — so a locked drag
/// cannot leave the picture or shear out of shape.
fn drag_crop(
    rect: egui::Rect,
    handle: CropHandle,
    to: egui::Pos2,
    src: Vec2,
    ratio: Option<f32>,
) -> egui::Rect {
    let min = Vec2::new(src.x * doc::Crop::MIN, src.y * doc::Crop::MIN);
    let to = egui::pos2(to.x.clamp(0.0, src.x), to.y.clamp(0.0, src.y));
    let west = matches!(handle, CropHandle::Nw | CropHandle::W | CropHandle::Sw);
    let east = matches!(handle, CropHandle::Ne | CropHandle::E | CropHandle::Se);
    let north = matches!(handle, CropHandle::Nw | CropHandle::N | CropHandle::Ne);
    let south = matches!(handle, CropHandle::Sw | CropHandle::S | CropHandle::Se);

    match ratio {
        None => {
            let mut r = rect;
            if west {
                r.min.x = to.x;
            }
            if east {
                r.max.x = to.x;
            }
            if north {
                r.min.y = to.y;
            }
            if south {
                r.max.y = to.y;
            }
            if r.width() < min.x {
                if west {
                    r.min.x = r.max.x - min.x;
                } else {
                    r.max.x = r.min.x + min.x;
                }
            }
            if r.height() < min.y {
                if north {
                    r.min.y = r.max.y - min.y;
                } else {
                    r.max.y = r.min.y + min.y;
                }
            }
            // a pointer outside the picture can push the far edge out; pull the frame back inside
            let shift = Vec2::new(
                if r.left() < 0.0 {
                    -r.left()
                } else if r.right() > src.x {
                    src.x - r.right()
                } else {
                    0.0
                },
                if r.top() < 0.0 {
                    -r.top()
                } else if r.bottom() > src.y {
                    src.y - r.bottom()
                } else {
                    0.0
                },
            );
            r.translate(shift)
        }
        Some(ratio) => {
            let anchor = handle.anchor(rect);
            // the room on the side the pointer is on; at least the minimum, so a locked frame can still
            // be made small against an edge
            let room_x = if to.x < anchor.x {
                anchor.x
            } else {
                src.x - anchor.x
            }
            .max(min.x);
            let room_y = if to.y < anchor.y {
                anchor.y
            } else {
                src.y - anchor.y
            }
            .max(min.y);
            let dx = (to.x - anchor.x).abs().max(min.x);
            let dy = (to.y - anchor.y).abs().max(min.y);
            // the pointer's dominant axis decides the size, then the shape follows the ratio
            let mut w = if dx / ratio >= dy { dx } else { dy * ratio };
            w = w.clamp(min.x, room_x);
            let mut h = w / ratio;
            if h > room_y {
                h = room_y;
                w = h * ratio;
            }
            if h < min.y {
                h = min.y;
                w = h * ratio;
            }
            let sx = if to.x < anchor.x { -1.0 } else { 1.0 };
            let sy = if to.y < anchor.y { -1.0 } else { 1.0 };
            egui::Rect::from_two_pos(anchor, egui::pos2(anchor.x + sx * w, anchor.y + sy * h))
        }
    }
}

/// Fit a frame to a ratio (when one is locked) around its own centre, kept inside the picture. Used when
/// a ratio is picked: the frame changes shape where it is instead of jumping, and the corners stay
/// grabbable afterwards.
fn fit_ratio(rect: egui::Rect, ratio: Option<f32>, src: Vec2) -> egui::Rect {
    let Some(ratio) = ratio else {
        return rect;
    };
    let min = Vec2::new(src.x * doc::Crop::MIN, src.y * doc::Crop::MIN);
    // the biggest frame of this shape that fits both what is there now and the picture
    let mut w = rect.width().min(rect.height() * ratio).min(src.x).min(src.y * ratio);
    w = w.max(min.x);
    let mut h = (w / ratio).max(min.y);
    w = (h * ratio).max(min.x);
    if w > src.x {
        w = src.x;
        h = w / ratio;
    }
    if h > src.y {
        h = src.y;
        w = h * ratio;
    }
    let r = egui::Rect::from_center_size(rect.center(), Vec2::new(w, h));
    let dx = if r.left() < 0.0 {
        -r.left()
    } else if r.right() > src.x {
        src.x - r.right()
    } else {
        0.0
    };
    let dy = if r.top() < 0.0 {
        -r.top()
    } else if r.bottom() > src.y {
        src.y - r.bottom()
    } else {
        0.0
    };
    r.translate(Vec2::new(dx, dy))
}

/// The narrowest a picture can be dragged to: under this the corner grip is wider than the picture,
/// so it would cover the thing it is resizing.
const MIN_IMAGE_W: f32 = 40.0;

/// The corner grip's hit square, and how far its centre sits inside the picture's corner. The square
/// is nearly twice the knob it draws: a 13 pt handle is one you miss, and a miss at the corner used to
/// become a drag-out of the file rather than a resize.
const GRIP_HIT: f32 = 26.0;
const GRIP_CENTRE: f32 = 6.0;

/// The width a picture takes while its corner grip is dragged: the pointer's distance from the
/// picture's left edge, plus the offset at which the button went down inside the grip, held between
/// the minimum and what the column can show. It is measured from the pointer every frame rather than
/// from the previous frame's size, so dragging out and back returns the picture to where it started.
///
/// A left-aligned picture measures from its left edge (factor 1), a centred one from the column's
/// middle, where its width is twice the pointer's distance (factor 2).
fn resize_width_from(pointer_x: f32, grab: f32, anchor: f32, box_w: f32, grow: f32) -> u32 {
    ((pointer_x + grab - anchor) * grow)
        .clamp(MIN_IMAGE_W, box_w.max(MIN_IMAGE_W))
        .round() as u32
}

/// Append a line to `~/.config/notecraft/dnd.log`. Drag-and-drop failures are
/// otherwise invisible (the compositor can swallow a drag before any app code
/// runs), so every hover/drop/attach step is recorded for diagnosis — including
/// the outgoing half of a drag, which lives in `dragout`.
/// Write dropped bytes to a temporary file so the existing path-based attachment code can take it.
///
/// The name comes from the drag source and may have no extension at all (browsers often send none),
/// so the content is sniffed and an extension added: `attach::snippet_for_file` decides image-versus
/// file from the extension, and a PNG named `image` would otherwise be attached as an opaque blob.
#[cfg(any(windows, test))]
#[cfg(any(windows, test))]
pub(crate) fn blob_files(blobs: &[crate::dropped::Blob]) -> Vec<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("notecraft-drop");
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    blobs
        .iter()
        .enumerate()
        .filter_map(|(i, blob)| {
            let mut name = blob
                .name
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("dropped")
                .trim()
                .to_string();
            if name.is_empty() {
                name = format!("dropped-{i}");
            }
            if std::path::Path::new(&name).extension().is_none() {
                name.push_str(guess_ext(&blob.bytes));
            }
            let path = dir.join(format!("{i}-{name}"));
            std::fs::write(&path, &blob.bytes).ok()?;
            Some(path)
        })
        .collect()
}

/// The extension for bytes with no name, from their magic number.
#[cfg(any(windows, test))]
pub(crate) fn guess_ext(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        ".png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ".jpg"
    } else if bytes.starts_with(b"GIF8") {
        ".gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        ".webp"
    } else if bytes.starts_with(b"BM") {
        ".bmp"
    } else {
        ".bin"
    }
}

pub(crate) fn dnd_log(msg: &str) {
    let Some(dir) = directories::BaseDirs::new().map(|d| d.config_dir().join("notecraft")) else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("dnd.log"))
    {
        use std::io::Write;
        let _ = writeln!(f, "{} {msg}", chrono::Local::now().format("%H:%M:%S%.3f"));
    }
}

// Local checkbox painting lives in `theme::paint_checkbox` so the block editor, the
// tasks view and the read view all draw the same control.

/// The second line of a page row: the date only.
///
/// It used to append a snippet of the body and a todo count, which wrapped past the
/// row's fixed height and spilled over the next row (a first line like
/// `![alt](assets/x.png)` has no spaces to break on at all).
/// The base text size of a block, by kind — shared by the editor and read mode so the two views cannot
/// drift apart.
fn block_size(kind: doc::Kind) -> f32 {
    match kind {
        doc::Kind::Heading(1) => 23.5,
        doc::Kind::Heading(2) => 20.0,
        doc::Kind::Heading(3) => 18.0,
        doc::Kind::Heading(4) => 16.5,
        doc::Kind::Heading(_) => 15.0,
        doc::Kind::Code => 13.5,
        _ => 15.0,
    }
}

/// Space after a block, by kind: paragraphs keep a line's rhythm, headings and rules get room, code
/// lines stay tight so a fence reads as one block.
fn space_after(kind: doc::Kind) -> f32 {
    match kind {
        doc::Kind::Heading(1) => 10.0,
        doc::Kind::Heading(_) => 8.0,
        doc::Kind::Rule => 4.0,
        doc::Kind::Code => 0.5,
        _ => 2.5,
    }
}

/// A context-menu row that reads as a toggle: the app's own painted checkbox (visible outline when
/// off, filled tick when on — no font glyph needed, "✓" is not in Chakra Petch) beside the label, and
/// the whole row clickable like a menu item.
fn menu_toggle(ui: &mut egui::Ui, on: bool, label: &str, t: &Theme) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), t.card_hover);
    }
    let box_rect = egui::Rect::from_min_size(
        egui::Pos2::new(rect.min.x + 7.0, rect.center().y - 7.0),
        Vec2::splat(14.0),
    );
    theme::paint_checkbox(ui, box_rect, on, resp.hovered(), t);
    ui.painter().text(
        egui::Pos2::new(box_rect.right() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::new(13.5, theme::fam(theme::F_MED)),
        if on { t.accent } else { t.text },
    );
    resp.clicked()
}

/// The crop mark: a frame with the two corner ticks that mean "cut this out". Painted, not typed —
/// there is no crop glyph in the bundled faces (`ICON_GLYPHS` lists what is safe).
fn paint_crop_icon(p: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = Stroke::new(1.3_f32, color);
    let inner = rect.shrink(2.0);
    // the frame that is kept
    p.rect_stroke(
        inner,
        CornerRadius::same(2),
        stroke,
        egui::StrokeKind::Inside,
    );
    // crop marks: each corner's tick runs out past the frame
    let tick = 3.5_f32;
    for (corner, dx, dy) in [
        (inner.left_top(), -1.0, 0.0),
        (inner.left_top(), 0.0, -1.0),
        (inner.right_bottom(), 1.0, 0.0),
        (inner.right_bottom(), 0.0, 1.0),
    ] {
        p.line_segment(
            [corner, corner + Vec2::new(dx * tick, dy * tick)],
            stroke,
        );
    }
}

/// The note-font picker: one row per face, each name set in the face it names, so the choice is made
/// by looking at the type rather than at a word for it. A short accent bar in the gutter marks the
/// face in use; answers the row that was pressed, and only when it is not the one already on.
fn menu_fonts(ui: &mut egui::Ui, current: theme::NoteFont, t: &Theme) -> Option<theme::NoteFont> {
    let mut picked = None;
    ui.label(
        RichText::new("note font")
            .family(theme::fam(theme::F_MONO))
            .size(11.0)
            .color(t.faint),
    );
    ui.add_space(2.0);
    for face in theme::NOTE_FONTS {
        let on = current == face;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 21.0), Sense::click());
        if resp.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(6), t.card_hover);
        }
        // the marker is a bar, not a glyph: "✓" is not in every face, and a dot reads as a bullet
        if on {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    egui::Pos2::new(rect.min.x + 3.0, rect.center().y - 6.0),
                    Vec2::new(2.0, 12.0),
                ),
                CornerRadius::same(1),
                t.accent,
            );
        }
        // the sample *is* the label: set in the face it names, in that face's own regular weight
        ui.painter().text(
            egui::Pos2::new(rect.min.x + 12.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            face.label(),
            egui::FontId::new(14.0, face.fam(theme::F_REG)),
            if on { t.accent } else { t.text },
        );
        if resp.clicked() && !on {
            picked = Some(face);
        }
    }
    picked
}

/// A row of `S M L` buttons under a quiet label, the current step filled in and the others plain.
/// Answers the step that was pressed, and only when it is not the one already on.
fn step_row<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    label: &str,
    current: T,
    steps: [(&'static str, T, &'static str); 3],
    t: &Theme,
) -> Option<T> {
    let mut picked = None;
    ui.horizontal(|ui| {
        row_label(ui, label, t);
        for (text, value, hint) in steps {
            let on = current == value;
            let rich = if on {
                RichText::new(text).size(13.0).color(t.accent)
            } else {
                RichText::new(text).size(13.0)
            };
            if ui
                .add(
                    egui::Button::new(rich)
                        .min_size(Vec2::new(26.0, 20.0))
                        .selected(on),
                )
                .on_hover_text(hint)
                .clicked()
                && !on
            {
                picked = Some(value);
            }
        }
    });
    picked
}

/// Width of a menu row's leading label, so the controls beside it line up between rows (`highlight`
/// and `size` sit one above the other).
const ROW_LABEL_W: f32 = 64.0;

/// The quiet mono label that opens a row of controls in the editor's menu, padded to `ROW_LABEL_W`.
fn row_label(ui: &mut egui::Ui, text: &str, t: &Theme) {
    let font = egui::FontId::new(11.0, theme::fam(theme::F_MONO));
    let w = ui
        .fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, t.faint).size().x);
    ui.label(
        RichText::new(text)
            .family(theme::fam(theme::F_MONO))
            .size(11.0)
            .color(t.faint),
    );
    ui.add_space((ROW_LABEL_W - w).max(0.0) + 4.0);
}

/// The highlighter palette as a row of swatches on the menu's first level, beside its label — the
/// same shape as `size S M L`, because the marker's colour is the thing being chosen and a submenu
/// hid all five behind a word. A ring marks the colour the marked words already carry; pressing that
/// one answers `Edit::Mark(0)`, which takes the marker off.
fn menu_marks_row(ui: &mut egui::Ui, current: &[bool; 5], t: &Theme) -> Option<u8> {
    let mut picked = None;
    ui.horizontal(|ui| {
        row_label(ui, "highlight", t);
        for (i, on) in current.iter().enumerate() {
            let colour = t.marks.get(i).copied().unwrap_or(t.faint);
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
            let swatch = egui::Rect::from_center_size(rect.center(), Vec2::splat(16.0));
            let name = theme::MARK_NAMES.get(i).copied().unwrap_or("highlight");
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            ui.painter()
                .rect_filled(swatch, CornerRadius::same(4), colour);
            // the swatch is the colour itself, so the state has to be drawn around it: a ring for
            // "these words already are that colour", a hover ring for "this is the one you are on"
            if *on {
                ui.painter().rect_stroke(
                    swatch.expand(2.0),
                    CornerRadius::same(6),
                    Stroke::new(1.4_f32, t.text),
                    StrokeKind::Middle,
                );
            } else if resp.hovered() {
                ui.painter().rect_stroke(
                    swatch.expand(2.0),
                    CornerRadius::same(6),
                    Stroke::new(1.0_f32, t.accent),
                    StrokeKind::Middle,
                );
            }
            // clicked() before the tooltip: on_hover_text takes the Response by value
            let clicked = resp.clicked();
            resp.on_hover_text(if *on {
                format!("{name} — click again to remove")
            } else {
                name.to_string()
            });
            if clicked {
                picked = Some(if *on { 0 } else { i as u8 + 1 });
            }
        }
    });
    picked
}

/// A menu row that *does* something rather than showing a state: no checkbox, and the shortcut
/// hint dim on the right. Copy/Cut/Paste are actions, so a tick would claim a state they do not
/// have — the toggles below them answer "is this line already like that", these do not.
/// A filter pill for the tasks view: accent-tinted when it is the live filter, quiet when it is not.
fn filter_chip(ui: &mut egui::Ui, label: &str, active: bool, t: &Theme) -> bool {
    let color = if active { t.accent } else { t.faint };
    theme::chip(ui, label, color, t).clicked()
}

fn menu_action(ui: &mut egui::Ui, label: &str, hint: &str, t: &Theme) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 21.0), Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(6), t.card_hover);
    }
    ui.painter().text(
        egui::Pos2::new(rect.min.x + 9.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::new(13.5, theme::fam(theme::F_MED)),
        if resp.hovered() { t.heading } else { t.text },
    );
    if !hint.is_empty() {
        ui.painter().text(
            egui::Pos2::new(rect.max.x - 9.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            hint,
            egui::FontId::new(10.5, theme::fam(theme::F_MONO)),
            t.faint,
        );
    }
    resp.clicked()
}

/// How a Copy/Cut is described in the status line: "3 lines", "the marked words", "the line".
fn clip_summary(lines: usize, range: Option<(usize, usize)>) -> String {
    if lines > 1 {
        format!("{lines} lines")
    } else if range.is_some() {
        "the marked words".to_string()
    } else {
        "the line".to_string()
    }
}

/// Where a scroll key lands in the open note, in the note's own coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScrollTo {
    Top,
    Bottom,
    PageUp,
    PageDown,
}

/// What the page's scroll area measured on the last frame: where the view is (`offset`), how tall the
/// window onto the note is (`view`), and how tall the note is (`content`).
#[derive(Clone, Copy)]
struct ScrollMetrics {
    offset: f32,
    view: f32,
    content: f32,
}

/// The vertical offset a scroll key asks for.
///
/// `End` means the end of the *note*: the last offset from which the whole foot of the note is
/// visible (`content - view`), not the top of the last screenful. A page is one window with a little
/// overlap (8%), so the line that was at the fold is still on screen after the jump, and both ends
/// clamp — PgDn on the last screen stays put instead of drifting past the content.
fn scroll_target(offset: f32, view: f32, content: f32, what: ScrollTo) -> f32 {
    let max = (content - view).max(0.0);
    let step = (view * 0.92).max(40.0);
    match what {
        ScrollTo::Top => 0.0,
        ScrollTo::Bottom => max,
        ScrollTo::PageDown => (offset + step).min(max),
        ScrollTo::PageUp => (offset - step).max(0.0),
    }
}

/// Undo/redo over whole documents. One step is one document as it was, which is why nothing has to be
/// inverted: `undo` swaps the current document for the remembered one and keeps the other for `redo`.
struct History {
    undo: Vec<doc::Doc>,
    redo: Vec<doc::Doc>,
    at: Option<Instant>,
}

impl History {
    fn new() -> History {
        History {
            undo: Vec::new(),
            redo: Vec::new(),
            at: None,
        }
    }

    fn push(&mut self, current: &doc::Doc) {
        const LIMIT: usize = 200;
        self.undo.push(current.clone());
        if self.undo.len() > LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.at = Some(Instant::now());
    }

    /// A burst of typing is one step: pushed only when the last one is older than 700 ms.
    fn push_typing(&mut self, current: &doc::Doc) {
        let stale = self
            .at
            .map(|t| t.elapsed() > Duration::from_millis(700))
            .unwrap_or(true);
        if stale {
            self.push(current);
        }
    }

    fn undo(&mut self, current: &doc::Doc) -> Option<doc::Doc> {
        let prev = self.undo.pop()?;
        self.redo.push(current.clone());
        self.at = None;
        Some(prev)
    }

    fn redo(&mut self, current: &doc::Doc) -> Option<doc::Doc> {
        let next = self.redo.pop()?;
        self.undo.push(current.clone());
        self.at = None;
        Some(next)
    }

    fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}

/// What a due-date control chose.
#[derive(Clone, Copy, PartialEq)]
enum DueChoice {
    Set(chrono::NaiveDate),
    Clear,
}

/// What state of task the tasks view shows.
#[derive(Clone, Copy, PartialEq)]
enum TaskStatus {
    All,
    Open,
    Done,
}

/// When the tasks view shows a task as due.
#[derive(Clone, Copy, PartialEq)]
enum TaskDue {
    Any,
    Overdue,
    Today,
    /// Today through the next 7 days.
    Week,
    Later,
    None,
}

impl Default for TaskStatus {
    fn default() -> Self {
        TaskStatus::All
    }
}

impl Default for TaskDue {
    fn default() -> Self {
        TaskDue::Any
    }
}

/// Whether the tasks view's bucket `bucket` (0 Overdue … 5 Someday/no date, 6 Done) survives the
/// status filter. The date filter is per task (`due_matches`), so the two filters really are
/// independent: "Done + Overdue" means tasks finished whose date had already passed.
fn bucket_shown(status: TaskStatus, bucket: usize) -> bool {
    match status {
        TaskStatus::All => true,
        TaskStatus::Open => bucket != 6,
        TaskStatus::Done => bucket == 6,
    }
}

/// Whether a task's own date passes the due filter.
fn due_matches(due: Option<chrono::NaiveDate>, f: TaskDue, today: chrono::NaiveDate) -> bool {
    match f {
        TaskDue::Any => true,
        TaskDue::Overdue => due.map(|d| d < today).unwrap_or(false),
        TaskDue::Today => due == Some(today),
        TaskDue::Week => due.map(|d| d >= today && d <= today + chrono::Duration::days(7)).unwrap_or(false),
        TaskDue::Later => due.map(|d| d > today + chrono::Duration::days(7)).unwrap_or(false),
        TaskDue::None => due.is_none(),
    }
}

/// The suffix on the day of the month: `19` → `th`. The teens are the exception (`11th`, not
/// `11st`), which is why the last *two* digits are looked at before the last one.
fn ordinal(day: u32) -> &'static str {
    match (day % 10, day % 100) {
        (1, 11) | (2, 12) | (3, 13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    }
}

/// The status bar's clock, in the order it is read out: `Monday 19th October 2026 · 14:32`.
/// chrono's `%d` pads the day and has no ordinal formatter, so the day is spliced in by hand.
fn clock_label(now: chrono::DateTime<chrono::Local>) -> String {
    use chrono::Datelike;
    format!(
        "{} {}{} {} · {}",
        now.format("%A"),
        now.day(),
        ordinal(now.day()),
        now.format("%B %Y"),
        now.format("%H:%M")
    )
}

/// Where a due date is being set from the picker.
#[derive(Clone)]
enum DueTarget {
    /// A task in the tasks view: the page it lives on and its body line.
    Task { page: PathBuf, line: usize },
    /// Blocks of the open page, from the editor's right-click menu.
    Blocks(Vec<usize>),
}

/// The open due-date picker: what it points at, what it currently shows and where it sits.
#[derive(Clone)]
struct DuePick {
    at: egui::Pos2,
    current: Option<chrono::NaiveDate>,
    target: DueTarget,
    /// The picker opens on the same frame as the click that asked for it, so that click must not
    /// count as "clicked outside" and close it again.
    fresh: bool,
}

/// The first day of the month `d` is in.
fn month_of(d: chrono::NaiveDate) -> chrono::NaiveDate {
    use chrono::Datelike;
    chrono::NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap_or(d)
}

fn shift_month(m: chrono::NaiveDate, by: i32) -> chrono::NaiveDate {
    use chrono::Datelike;
    let mut year = m.year();
    let mut month = m.month() as i32 + by;
    while month < 1 {
        month += 12;
        year -= 1;
    }
    while month > 12 {
        month -= 12;
        year += 1;
    }
    chrono::NaiveDate::from_ymd_opt(year, month as u32, 1).unwrap_or(m)
}

fn days_in_month(m: chrono::NaiveDate) -> u32 {
    (shift_month(m, 1) - m).num_days() as u32
}

/// One month, drawn with the app's own buttons, weeks starting Monday. Returns the day picked.
fn month_grid(
    ui: &mut egui::Ui,
    month: &mut chrono::NaiveDate,
    current: Option<chrono::NaiveDate>,
    t: &Theme,
) -> Option<chrono::NaiveDate> {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    let mut picked = None;
    let step = |ui: &mut egui::Ui, glyph: &str, by: i32, month: &mut chrono::NaiveDate| {
        if ui
            .add(egui::Button::new(RichText::new(glyph).size(12.0)).min_size(Vec2::new(20.0, 18.0)))
            .clicked()
        {
            *month = shift_month(*month, by);
        }
    };
    ui.horizontal(|ui| {
        step(ui, "<", -1, month);
        ui.label(
            RichText::new(month.format("%b %Y").to_string())
                .family(theme::fam(theme::F_MED))
                .size(12.0)
                .color(t.heading),
        );
        step(ui, ">", 1, month);
    });
    let first = *month;
    let lead = first.weekday().num_days_from_monday() as usize;
    let total = days_in_month(first);
    egui::Grid::new(("due_days", first.to_string()))
        .num_columns(7)
        .spacing([2.0, 1.0])
        .show(ui, |ui| {
            for d in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
                ui.label(
                    RichText::new(d)
                        .family(theme::fam(theme::F_MONO))
                        .size(9.0)
                        .color(t.faint),
                );
            }
            ui.end_row();
            let mut col = 0usize;
            for _ in 0..lead {
                ui.label("");
                col += 1;
            }
            for day in 1..=total {
                let date = first + chrono::Duration::days((day - 1) as i64);
                let mut text = RichText::new(day.to_string())
                    .family(theme::fam(theme::F_MONO))
                    .size(11.0);
                text = if current == Some(date) {
                    text.color(t.bg).background_color(t.accent)
                } else if date == today {
                    text.color(t.accent)
                } else {
                    text.color(t.text)
                };
                let cell = ui.add(
                    egui::Button::new(text)
                        .min_size(Vec2::new(20.0, 17.0))
                        .frame(false),
                );
                if cell.clicked() {
                    picked = Some(date);
                }
                col += 1;
                if col % 7 == 0 {
                    ui.end_row();
                }
            }
            if col % 7 != 0 {
                ui.end_row();
            }
        });
    picked
}

/// The due-date rows shared by the tasks view's chip popup and the editor's right-click submenu:
/// three presets, a real calendar for any other day, and Clear when there is a date to clear.
fn due_rows(
    ui: &mut egui::Ui,
    current: Option<chrono::NaiveDate>,
    month: &mut chrono::NaiveDate,
    t: &Theme,
) -> Option<DueChoice> {
    let today = chrono::Local::now().date_naive();
    let mut out = None;
    ui.set_min_width(196.0);
    for (label, days) in [("Today", 0i64), ("Tomorrow", 1), ("In a week", 7)] {
        if menu_action(ui, label, &(today + chrono::Duration::days(days)).format("%d %b").to_string(), t) {
            out = Some(DueChoice::Set(today + chrono::Duration::days(days)));
        }
    }
    ui.separator();
    if let Some(d) = month_grid(ui, month, current, t) {
        out = Some(DueChoice::Set(d));
    }
    if current.is_some() {
        ui.separator();
        if menu_action(ui, "Clear due date", "", t) {
            out = Some(DueChoice::Clear);
        }
    }
    out
}

/// What the editor's menu shows as already on, for the lines it will act on.
struct MenuState {
    bold: bool,
    italic: bool,
    code: bool,
    strike: bool,
    /// The common kind, when every target is the same kind.
    kind: Option<doc::Kind>,
    /// The size step every target shares, when they agree.
    size: Option<i8>,
    /// Tags every target carries.
    tags: Vec<String>,
    /// Highlighter colours every run of the marked range carries (one bool per palette entry).
    marks: [bool; 5],
}

fn menu_state(doc: &doc::Doc, idx: &[usize], range: Option<(usize, usize)>) -> MenuState {
    let idx: Vec<usize> = idx.iter().copied().filter(|i| *i < doc.blocks.len()).collect();
    let all = |f: &dyn Fn(&doc::Block) -> bool| !idx.is_empty() && idx.iter().all(|i| f(&doc.blocks[*i]));
    let attr = |range: Option<(usize, usize)>, f: &dyn Fn(&doc::Attrs) -> bool| {
        all(&|b| doc::attr_all(&b.runs, range, f))
    };
    let kind = idx
        .first()
        .map(|i| doc.blocks[*i].kind)
        .filter(|k| idx.iter().all(|i| doc.blocks[*i].kind == *k));
    let mut marks = [false; 5];
    for (i, m) in marks.iter_mut().enumerate() {
        *m = attr(range, &move |a: &doc::Attrs| a.mark == (i as u8 + 1));
    }
    let mut tags: Vec<String> = Vec::new();
    if let Some(first) = idx.first() {
        for r in &doc.blocks[*first].runs {
            if !r.a.tag {
                continue;
            }
            let name = r.text.trim_start_matches('#').to_string();
            if idx
                .iter()
                .all(|i| doc.blocks[*i].runs.iter().any(|q| q.a.tag && q.text.trim_start_matches('#') == name))
                && !tags.contains(&name)
            {
                tags.push(name);
            }
        }
    }
    // the size of the first run the *range* covers — taking the block's first run showed "M" for a
    // large word that had plain text before it
    let size = idx
        .first()
        .map(|i| {
            let b = &doc.blocks[*i];
            let start = range.map(|(s, _)| s).unwrap_or(0);
            let mut at = 0usize;
            for r in &b.runs {
                let n = r.text.chars().count();
                if at + n > start {
                    return r.a.size;
                }
                at += n;
            }
            0
        })
        .filter(|s| {
            idx.iter()
                .all(|i| doc::attr_all(&doc.blocks[*i].runs, range, |a| a.size == *s))
        });
    MenuState {
        bold: attr(range, &|a| a.bold),
        italic: attr(range, &|a| a.italic),
        code: attr(range, &|a| a.code),
        strike: attr(range, &|a| a.strike),
        kind,
        size,
        tags,
        marks,
    }
}

/// "1 page" / "4 pages" — the palette printed "1 pages", which reads as a bug in a list of one.
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The width `seg_w` measures — or, for a label already drawn, the width it took.
fn text_w(ui: &egui::Ui, text: &str, size: f32, mono: bool) -> f32 {
    let family = if mono {
        theme::fam(theme::F_MONO)
    } else {
        theme::fam(theme::F_MED)
    };
    ui.fonts_mut(|f| {
        f.layout_no_wrap(
            text.to_owned(),
            FontId::new(size, family),
            egui::Color32::WHITE,
        )
        .size()
        .x
    })
}

/// The clock's own width, measured from the string it will draw this frame. Giving the date a slot
/// of its measured width is what stops it wrapping: a label in an `add_sized` box wraps inside the
/// box, and a two-line clock in a 26 pt bar prints over the panel edge.
fn clock_slot_w(ui: &egui::Ui) -> f32 {
    text_w(
        ui,
        &format!("{} · ", clock_label(chrono::Local::now())),
        10.5,
        true,
    ) + 2.0
}

/// How wide a status segment is, including the gap in front of it.
fn seg_w(ui: &egui::Ui, text: &str, size: f32, mono: bool) -> f32 {
    text_w(ui, text, size, mono) + 14.0
}

/// Draw one status-bar segment **if there is room for it**, and say whether it was drawn. A segment
/// that does not fit is dropped rather than allowed to wrap or run into the right-hand cluster —
/// the bar is one row, and the order the segments are offered in is their order of importance.
fn status_seg(
    ui: &mut egui::Ui,
    text: &str,
    mono: bool,
    size: f32,
    color: egui::Color32,
    used: &mut f32,
    budget: f32,
) -> bool {
    let w = seg_w(ui, text, size, mono);
    if *used + w > budget {
        return false;
    }
    *used += w;
    ui.label(
        RichText::new(text)
            .family(if mono {
                theme::fam(theme::F_MONO)
            } else {
                theme::fam(theme::F_MED)
            })
            .size(size)
            .color(color),
    );
    true
}

/// A hairline between two groups of top-bar controls. Ten buttons of equal weight, mixing modes,
/// views, actions and settings, read as one undifferentiated strip; the rule says where one job ends.
/// It is painted rather than `ui.separator()` because egui's own rule spans the whole interact height
/// and takes its colour from `Visuals`, which on Paper is very nearly the panel itself.
fn bar_rule(ui: &mut egui::Ui, t: &Theme) {
    ui.add_space(8.0);
    let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, 18.0), Sense::hover());
    ui.painter()
        .rect_filled(r, CornerRadius::same(1), t.border);
    ui.add_space(8.0);
}

fn page_meta_line(p: &Page) -> String {
    p.created
        .or(p.updated)
        .map(|d| d.format("%d %b %Y").to_string())
        .unwrap_or_default()
}

/// The meta line of a row: plain faint words, with the matched term of a search result tinted, so the
/// row shows *why* the page matched. Shortened like `one_line`, keeping the marked words inside what
/// survives.
fn meta_galley(
    ui: &egui::Ui,
    text: &str,
    mark: Option<(usize, usize)>,
    max_w: f32,
    t: &Theme,
) -> std::sync::Arc<egui::Galley> {
    let font = FontId::new(11.0, theme::fam(theme::F_REG));
    let build = |s: &str, mark: Option<(usize, usize)>| {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = f32::INFINITY;
        let plain = egui::TextFormat {
            font_id: font.clone(),
            color: t.faint,
            ..Default::default()
        };
        let chars: Vec<usize> = s.char_indices().map(|(i, _)| i).collect();
        let at = |i: usize| if i >= chars.len() { s.len() } else { chars[i] };
        match mark {
            Some((a, b)) if a < b && a < chars.len() => {
                let (ba, bb) = (at(a), at(b));
                job.append(&s[..ba], 0.0, plain.clone());
                job.append(
                    &s[ba..bb],
                    0.0,
                    egui::TextFormat {
                        font_id: font.clone(),
                        color: t.accent,
                        background: t.accent.gamma_multiply(0.18),
                        ..Default::default()
                    },
                );
                job.append(&s[bb..], 0.0, plain);
            }
            _ => job.append(s, 0.0, plain),
        }
        job
    };
    let mut s = text.to_owned();
    let mut mark = mark;
    let mut g = ui.fonts_mut(|f| f.layout_job(build(&s, mark)));
    while g.size().x > max_w && !s.is_empty() {
        let keep = s.chars().count().saturating_sub(4);
        let kept: String = s.chars().take(keep).collect();
        let m = mark
            .map(|(a, b)| (a.min(keep), b.min(keep)))
            .filter(|(a, b)| a < b);
        s = format!("{kept}…");
        mark = m;
        g = ui.fonts_mut(|f| f.layout_job(build(&s, mark)));
    }
    g
}

/// Lay text out on a single line, shortening it with an ellipsis when it does not fit,
/// so a long title cannot wrap out of its row.
fn one_line(ui: &egui::Ui, text: &str, font: egui::FontId, color: Color32, max_w: f32) -> std::sync::Arc<egui::Galley> {
    let mut s = text.to_owned();
    let mut g = ui.fonts_mut(|f| f.layout_no_wrap(s.clone(), font.clone(), color));
    if g.size().x > max_w && !s.is_empty() {
        // drop a few characters at a time: relayouting per character is wasteful on
        // a long page list, and a glyph or two of slack does not matter here
        while !s.is_empty() && g.size().x > max_w {
            let keep = s.chars().count().saturating_sub(4);
            s = s.chars().take(keep).collect();
            g = ui.fonts_mut(|f| f.layout_no_wrap(format!("{s}…"), font.clone(), color));
        }
    }
    g
}

fn page_row(
    ui: &mut egui::Ui,
    title: &str,
    meta: &str,
    badge: &str,
    mark: Option<(usize, usize)>,
    selected: bool,
    t: &Theme,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 46.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, CornerRadius::same(9), t.accent.gamma_multiply(0.15));
        p.rect_filled(
            egui::Rect::from_min_size(
                rect.min + Vec2::new(0.0, 7.0),
                Vec2::new(2.5, rect.height() - 14.0),
            ),
            CornerRadius::same(2),
            t.accent,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(9), t.card_hover);
    }
    let title_color = if selected { t.heading } else { t.text };
    let title_g = one_line(
        ui,
        title,
        FontId::new(13.5, theme::fam(theme::F_SEMI)),
        title_color,
        rect.width() - 20.0,
    );
    let meta_g = meta_galley(ui, meta, mark, rect.width() - 20.0, t);
    let x = rect.min.x + 10.0;
    ui.painter()
        .galley(egui::Pos2::new(x, rect.min.y + 7.0), title_g, title_color);
    ui.painter()
        .galley(egui::Pos2::new(x, rect.min.y + 25.0), meta_g, t.faint);
    if !badge.is_empty() {
        let g = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                badge.to_owned(),
                FontId::new(10.0, theme::fam(theme::F_MONO)),
                t.accent,
            )
        });
        ui.painter().galley(
            egui::Pos2::new(rect.right() - g.size().x - 8.0, rect.min.y + 8.0),
            g,
            t.accent,
        );
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

fn tag_color(t: &Theme, tag: &str) -> Color32 {
    // stable hash → palette so a tag keeps its colour between sessions
    let mut h: u32 = 2166136261;
    for b in tag.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    // The sixth entry is a colour no theme token provides, so it has to be picked per theme: the
    // lavender that reads on near-black is 2:1 on Paper, where a tag chip is that colour drawn at
    // 14 % behind its own text — a tag landing on it was the one label you could not read.
    let extra = if t.dark {
        Color32::from_rgb(168, 155, 240)
    } else {
        Color32::from_rgb(92, 74, 160)
    };
    let palette = [t.accent, t.accent2, t.done, t.warn, t.danger, extra];
    palette[(h as usize) % palette.len()]
}

fn build_palette(vault: &Vault, query: &str) -> Vec<PalItem> {
    const COMMANDS: [&str; 16] = [
        "New page",
        "New section",
        "New notebook",
        "Find in page",
        "Duplicate page",
        "Restore deleted…",
        "Tasks view",
        "Tags view",
        "Pages view",
        "Toggle read / edit",
        "Cycle theme",
        "Reload vault",
        "Save page",
        "Open vault folder",
        "Toggle notebooks panel",
        "Toggle page list",
    ];
    const PAGES: usize = 8;
    const TAGS: usize = 6;
    const SECTIONS: usize = 6;
    const CMDS: usize = 8;
    let q = query.trim().to_lowercase();
    let mut items: Vec<PalItem> = Vec::new();
    let matches = |hay: &str| q.is_empty() || hay.to_lowercase().contains(&q);

    // Pages first when there is something to find: they are what a search is usually after, and
    // appending them last used to let commands and tags push every page out of the list.
    if !q.is_empty() {
        let (hits, total) = vault.search(query, PAGES);
        for h in &hits {
            let p = &vault.pages[h.page];
            let kind = match h.kind {
                HitKind::Title => "title",
                HitKind::Tag => "tag",
                HitKind::Body => "body",
            };
            let snippet = if h.snippet.is_empty() {
                String::new()
            } else {
                format!(" — {}", h.snippet)
            };
            items.push(PalItem {
                label: p.title.clone(),
                hint: if h.count > 1 {
                    format!("{kind} · {} matches", h.count)
                } else {
                    format!("{kind} · {} / {}", p.notebook, p.section)
                },
                group: "Pages",
                action: Action::OpenPageAt(p.path.clone(), h.needle.clone(), h.line),
            });
            let _ = snippet;
        }
        if total > hits.len() {
            items.push(PalItem {
                label: format!("{} more pages", total - hits.len()),
                hint: "type a longer query".into(),
                group: "Pages",
                action: Action::Command("Pages view"),
            });
        }
    }

    for (tag, count) in vault.all_tags().into_iter().take(TAGS) {
        let label = format!("#{tag}");
        if matches(&label) {
            items.push(PalItem {
                label,
                hint: plural(count, "page", "pages"),
                group: "Tags",
                action: Action::Tag(tag),
            });
        }
    }
    let mut secs = 0usize;
    for nb in &vault.notebooks {
        for sec in &nb.sections {
            let label = format!("{} / {}", nb.name, sec.name);
            if secs < SECTIONS && matches(&label) {
                secs += 1;
                items.push(PalItem {
                    label,
                    hint: plural(sec.pages.len(), "page", "pages"),
                    group: "Sections",
                    action: Action::OpenSection(nb.name.clone(), sec.name.clone()),
                });
            }
        }
    }
    let mut cmds = 0usize;
    for c in COMMANDS {
        if cmds < CMDS && matches(c) {
            cmds += 1;
            items.push(PalItem {
                label: c.to_string(),
                // no hint: "command" on every command row was six identical words saying what the
                // heading above them now says once
                hint: String::new(),
                group: "Commands",
                action: Action::Command(c),
            });
        }
    }
    items
}

// ------------------------------------------------------------------ persisted state

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct State {
    theme: Option<String>,
    last_page: Option<String>,
    #[serde(default)]
    read_mode: bool,
    #[serde(default)]
    view: Option<String>,
    /// `None` means the preference was never touched, so the panel opens (a state.json from an
    /// older build has no such key and must not start with the panels folded away).
    #[serde(default)]
    rail_open: Option<bool>,
    #[serde(default)]
    pages_open: Option<bool>,
    /// Picture display (centred / rounded) and the note's text size. Absent means the default,
    /// which for the text size is **large**.
    #[serde(default)]
    img_center: Option<bool>,
    #[serde(default)]
    img_rounded: Option<bool>,
    #[serde(default)]
    img_radius: Option<String>,
    #[serde(default)]
    note_text: Option<String>,
    /// The face the note body is set in. Absent means the house face (Chakra Petch).
    #[serde(default)]
    note_font: Option<String>,
    /// `off` / `gb` / `us`. Absent means off: spelling is asked for, never assumed.
    #[serde(default)]
    spell: Option<String>,
    /// Words the user has added to the ignore list, lower-case.
    #[serde(default)]
    spell_ignore: Option<Vec<String>>,
}

fn state_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.config_dir().join("notecraft").join("state.json"))
}

fn load_state() -> State {
    state_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_state(app: &NoteCraft) {
    let Some(path) = state_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let state = State {
        theme: Some(app.theme.id.to_string()),
        last_page: app.page.as_ref().map(|p| p.display().to_string()),
        read_mode: app.read_mode,
        view: Some(
            match app.view {
                View::Pages => "pages",
                View::Tasks => "tasks",
                View::Tags => "tags",
            }
            .to_string(),
        ),
        rail_open: Some(app.rail_open),
        pages_open: Some(app.pages_open),
        img_center: Some(app.img_center),
        img_rounded: Some(app.img_rounded),
        img_radius: Some(app.img_radius.id().to_string()),
        note_text: Some(app.note_text.id().to_string()),
        note_font: Some(app.note_font.id().to_string()),
        spell: Some(app.spell.id().to_string()),
        spell_ignore: Some(app.spell_ignore.clone()),
    };
    if let Ok(s) = serde_json::to_string_pretty(&state) {
        let _ = std::fs::write(path, s);
    }
}

#[cfg(test)]
mod tests_clock {
    use super::{clock_label, ordinal};
    use chrono::TimeZone;

    #[test]
    fn the_day_of_the_month_carries_its_own_suffix() {
        assert_eq!(ordinal(1), "st");
        assert_eq!(ordinal(2), "nd");
        assert_eq!(ordinal(3), "rd");
        assert_eq!(ordinal(4), "th");
        // the teens take no part in the 1/2/3 rule
        assert_eq!(ordinal(11), "th");
        assert_eq!(ordinal(12), "th");
        assert_eq!(ordinal(13), "th");
        assert_eq!(ordinal(21), "st");
        assert_eq!(ordinal(22), "nd");
        assert_eq!(ordinal(23), "rd");
        assert_eq!(ordinal(30), "th");
        assert_eq!(ordinal(31), "st");
    }

    #[test]
    fn the_clock_reads_weekday_day_month_year_then_the_time() {
        // the shape the user asked for, and a single-digit minute keeps its zero
        let now = chrono::Local.with_ymd_and_hms(2026, 10, 19, 14, 5, 0).unwrap();
        assert_eq!(clock_label(now), "Monday 19th October 2026 · 14:05");
    }
}

#[cfg(test)]
mod tests_resize {
    use super::{resize_width_from, MIN_IMAGE_W};

    #[test]
    fn the_corner_follows_the_pointer() {
        // pointer exactly on the corner it grabbed, 200 px from the picture's left edge
        assert_eq!(resize_width_from(500.0, 0.0, 300.0, 900.0, 1.0), 200);
        // 100 px further right is 100 px wider
        assert_eq!(resize_width_from(600.0, 0.0, 300.0, 900.0, 1.0), 300);
    }

    /// The frame a drag begins must not move the picture. The grab offset is measured from the
    /// pointer at that moment and the width is measured from that same pointer, so the first width
    /// is the width the picture already had — for a left-aligned picture and for a centred one.
    #[test]
    fn the_first_frame_of_a_drag_keeps_the_width_it_found() {
        // a picture 200 wide at x 100..300, the pointer landing 6 pt inside its right edge
        assert_eq!(resize_width_from(294.0, 6.0, 100.0, 900.0, 1.0), 200);
        // the same picture centred at x 200: its width is twice the distance from the middle
        assert_eq!(resize_width_from(294.0, 6.0, 200.0, 900.0, 2.0), 200);
        // and a press a few points *outside* the corner (the handle hangs over it) does not jump
        assert_eq!(resize_width_from(303.0, -3.0, 100.0, 900.0, 1.0), 200);
    }

    #[test]
    fn grabbing_the_grip_off_centre_does_not_jump_the_picture() {
        // the grip sits 7 px inside the corner, so the pointer lands there: same width, not 7 less
        assert_eq!(resize_width_from(493.0, 7.0, 300.0, 900.0, 1.0), 200);
    }

    #[test]
    fn a_drag_is_held_between_the_minimum_and_the_column() {
        // dragged past the left edge
        assert_eq!(resize_width_from(200.0, 0.0, 300.0, 900.0, 1.0), MIN_IMAGE_W as u32);
        // dragged past what the column can show
        assert_eq!(resize_width_from(5000.0, 0.0, 300.0, 640.0, 1.0), 640);
        // a column narrower than the minimum still leaves a picture, never a negative width
        assert_eq!(resize_width_from(5000.0, 0.0, 300.0, 20.0, 1.0), MIN_IMAGE_W as u32);
    }

    #[test]
    fn a_centred_picture_grows_about_the_middle() {
        // the pointer is on the grip 3 px inside a 400 wide picture centred on 500
        let w = |x: f32| resize_width_from(x, 3.0, 500.0, 900.0, 2.0);
        assert_eq!(w(700.0), 406); // 2 * (703 - 500)
        assert!(w(750.0) > w(700.0));
        // and a round trip back to the grip returns the width it started at
        assert_eq!(w(700.0), w(700.0));
        // the minimum and the column still hold
        assert_eq!(w(400.0), MIN_IMAGE_W as u32);
        assert_eq!(resize_width_from(9000.0, 3.0, 500.0, 640.0, 2.0), 640);
    }

    #[test]
    fn dragging_out_and_back_lands_on_the_same_width() {
        // measured from the pointer, so a round trip cannot drift (the old form re-added its own
        // growth every frame and ran away from the pointer)
        let start = resize_width_from(700.0, 3.0, 300.0, 900.0, 1.0);
        let out = resize_width_from(800.0, 3.0, 300.0, 900.0, 1.0);
        let back = resize_width_from(700.0, 3.0, 300.0, 900.0, 1.0);
        assert!(out > start);
        assert_eq!(back, start);
    }
}

#[cfg(test)]
mod tests_display {
    use super::{centre_indent, picture_radius, CornerStep, NoteTextSize, State};

    #[test]
    fn the_note_text_defaults_to_large() {
        // a state file with no such key is the state of every existing install
        let fresh: State = serde_json::from_str("{\"theme\": \"void\"}").unwrap();
        assert_eq!(NoteTextSize::from_id(fresh.note_text.as_deref()), NoteTextSize::Large);
        // and an unknown word does not silently fall back to medium either
        assert_eq!(NoteTextSize::from_id(Some("xl")), NoteTextSize::Large);
        assert_eq!(NoteTextSize::from_id(Some("m")), NoteTextSize::Medium);
        assert_eq!(NoteTextSize::from_id(Some("s")), NoteTextSize::Small);
    }

    #[test]
    fn large_text_is_bigger_than_medium() {
        assert!(NoteTextSize::Large.scale() > NoteTextSize::Medium.scale());
        assert_eq!(NoteTextSize::Medium.scale(), 1.0);
    }

    #[test]
    fn a_centred_picture_gets_equal_margins() {
        // 600 wide in a 700 box: 50 either side
        assert_eq!(centre_indent(700.0, 600.0, true), 50.0);
        assert_eq!(centre_indent(700.0, 300.0, true), 200.0);
        // nothing to centre: full width, and the setting off
        assert_eq!(centre_indent(700.0, 700.0, true), 0.0);
        assert_eq!(centre_indent(700.0, 300.0, false), 0.0);
        // a picture wider than the box (a stale width, a squeezed window) is not pushed off it
        assert_eq!(centre_indent(300.0, 700.0, true), 0.0);
    }

    #[test]
    fn the_corner_radius_follows_the_step_and_the_picture() {
        let rect = |w: f32, h: f32| egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
        // S < M < L on the same picture
        let (s, m, l) = (
            picture_radius(rect(400.0, 300.0), CornerStep::Small),
            picture_radius(rect(400.0, 300.0), CornerStep::Medium),
            picture_radius(rect(400.0, 300.0), CornerStep::Large),
        );
        assert!(s < m && m < l, "{s} {m} {l}");
        // the shorter side is what is measured: a wide letterbox is rounded like a small picture
        assert_eq!(
            picture_radius(rect(4000.0, 60.0), CornerStep::Medium),
            picture_radius(rect(200.0, 60.0), CornerStep::Medium)
        );
        // a thumbnail still shows a corner, and a giant picture does not become a circle
        assert!(picture_radius(rect(20.0, 20.0), CornerStep::Small) >= 2);
        assert!(picture_radius(rect(4000.0, 4000.0), CornerStep::Large) <= 44);
        // medium is where the radius was before it was configurable: 6 % of the shorter side
        assert_eq!(picture_radius(rect(400.0, 300.0), CornerStep::Medium), 18);
        // a state file from the boolean-only build has no radius key: medium, as it was drawn
        let old: State = serde_json::from_str("{\"img_rounded\": true}").unwrap();
        assert_eq!(CornerStep::from_id(old.img_radius.as_deref()), CornerStep::Medium);
    }
}

#[cfg(test)]
mod tests_crop {
    use super::*;

    fn src() -> Vec2 {
        Vec2::new(1000.0, 800.0)
    }

    /// The crop decides the picture's *visible* pixels: the shape the note lays out is the kept region,
    /// which is why cropping changes the height and never the width the user set.
    #[test]
    fn a_crop_changes_the_height_and_leaves_the_width_alone() {
        assert_eq!(crop_size(None, src()), Vec2::new(1000.0, 800.0));
        let half = doc::Crop {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 0.5,
        };
        assert_eq!(crop_size(Some(half), src()), Vec2::new(1000.0, 400.0));
        let cropped = picture_disp(crop_size(Some(half), src()), Some(300), 700.0);
        assert_eq!(cropped.x, 300.0);
        assert!((cropped.y - 120.0).abs() < 0.01, "{cropped:?}");
        // no stored width: the crop is fitted to the column by its own aspect
        let auto = picture_disp(crop_size(Some(half), src()), None, 700.0);
        assert_eq!(auto.x, 700.0);
        assert!((auto.y - 280.0).abs() < 0.01, "{auto:?}");
    }

    #[test]
    fn the_uv_rect_is_exactly_the_kept_region() {
        let c = doc::Crop {
            x: 0.25,
            y: 0.1,
            w: 0.5,
            h: 0.6,
        };
        let uv = crop_uv(Some(c));
        assert!((uv.min.x - 0.25).abs() < 1e-6 && (uv.min.y - 0.1).abs() < 1e-6);
        assert!((uv.max.x - 0.75).abs() < 1e-6 && (uv.max.y - 0.7).abs() < 1e-6);
        assert_eq!(
            crop_uv(None),
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
        );
        // in source pixels, the same region
        let px = crop_px(Some(c), src());
        assert!((px.min.x - 250.0).abs() < 0.01 && (px.width() - 500.0).abs() < 0.01);
        assert!((px.min.y - 80.0).abs() < 0.01 && (px.height() - 480.0).abs() < 0.01);
    }

    #[test]
    fn a_free_drag_moves_only_the_edges_it_holds() {
        let r = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(500.0, 400.0));
        let e = drag_crop(r, CropHandle::E, egui::pos2(700.0, 999.0), src(), None);
        assert_eq!((e.left(), e.right()), (100.0, 700.0));
        assert_eq!((e.top(), e.bottom()), (100.0, 400.0));
        // a handle dragged past its opposite edge stops at the minimum instead of inverting the frame
        let tiny = drag_crop(r, CropHandle::E, egui::pos2(0.0, 0.0), src(), None);
        assert!(
            tiny.width() >= 1000.0 * doc::Crop::MIN - 0.01,
            "{tiny:?}"
        );
        // and nothing leaves the picture
        let far = drag_crop(r, CropHandle::W, egui::pos2(-500.0, 0.0), src(), None);
        assert!(far.left() >= -0.01, "{far:?}");
    }

    #[test]
    fn a_locked_drag_holds_the_ratio_and_the_anchor() {
        let r = egui::Rect::from_min_max(egui::pos2(200.0, 200.0), egui::pos2(600.0, 500.0));
        let s = drag_crop(r, CropHandle::Se, egui::pos2(700.0, 520.0), src(), Some(1.0));
        assert_eq!(s.min, egui::pos2(200.0, 200.0));
        assert!((s.width() - s.height()).abs() < 0.01, "1:1 stayed 1:1: {s:?}");
        let w = drag_crop(r, CropHandle::Nw, egui::pos2(0.0, 0.0), src(), Some(16.0 / 9.0));
        assert!(
            (w.width() / w.height() - 16.0 / 9.0).abs() < 0.01,
            "16:9 stayed 16:9: {w:?}"
        );
        assert!(w.left() >= -0.01 && w.top() >= -0.01, "{w:?}");
        assert_eq!(w.max, r.max, "the anchor corner did not move");
    }

    #[test]
    fn picking_a_ratio_reshapes_the_frame_where_it_is() {
        let r = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(900.0, 500.0));
        let sq = fit_ratio(r, Some(1.0), src());
        assert!((sq.width() - sq.height()).abs() < 0.01, "{sq:?}");
        assert!(sq.center().distance(r.center()) < 0.01, "it stayed put");
        assert_eq!(fit_ratio(r, None, src()), r);
        // even a frame against the edge comes back inside
        let edge = egui::Rect::from_min_max(egui::pos2(990.0, 10.0), egui::pos2(1000.0, 700.0));
        let fitted = fit_ratio(edge, Some(1.0), src());
        assert!(fitted.right() <= 1000.0 + 0.01 && fitted.left() >= -0.01, "{fitted:?}");
    }

    #[test]
    fn moving_the_frame_keeps_it_inside_the_picture_at_its_own_size() {
        let r = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), Vec2::new(200.0, 100.0));
        let m = move_crop(r, Vec2::new(-100.0, 20.0), src());
        assert_eq!(m.min.x, 0.0, "stopped at the left edge");
        assert_eq!(m.min.y, 30.0, "free to move down");
        assert_eq!(m.size(), r.size(), "a move never resizes");
        let far = move_crop(r, Vec2::new(5000.0, 5000.0), src());
        assert!((far.right() - 1000.0).abs() < 0.01 && (far.bottom() - 800.0).abs() < 0.01);
    }
}

#[cfg(test)]
mod tests_dropped_blobs {
    use super::*;

    #[test]
    fn an_extension_is_read_from_the_bytes_when_the_source_forgot_one() {
        // Browsers often send a virtual file named "image" with no extension at all; the attachment
        // code decides image-versus-file from the name, so the magic number has to supply it.
        assert_eq!(guess_ext(&[0x89, b'P', b'N', b'G', 0x0d]), ".png");
        assert_eq!(guess_ext(&[0xFF, 0xD8, 0xFF, 0xE0]), ".jpg");
        assert_eq!(guess_ext(b"GIF89a"), ".gif");
        assert_eq!(guess_ext(b"RIFF\x24\x00\x00\x00WEBPVP8 "), ".webp");
        assert_eq!(guess_ext(b"BM\x36\x00"), ".bmp");
        assert_eq!(guess_ext(b"not an image at all"), ".bin");
    }

    #[test]
    fn a_dropped_blob_becomes_a_file_the_attachment_code_can_take() {
        let blob = crate::dropped::Blob {
            name: "image".into(),
            bytes: vec![0x89, b'P', b'N', b'G', 1, 2, 3],
        };
        let paths = blob_files(&[blob]);
        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths[0].extension().and_then(|e| e.to_str()),
            Some("png"),
            "the sniffed extension has to reach the file name: {:?}",
            paths[0]
        );
        assert_eq!(std::fs::read(&paths[0]).unwrap().len(), 7);
    }

    #[test]
    fn a_name_carrying_a_path_cannot_escape_the_drop_directory() {
        // The name comes from another process; it must not be able to name a path of its choosing.
        let blob = crate::dropped::Blob {
            name: "../../etc/passwd".into(),
            bytes: vec![0u8; 4],
        };
        let paths = blob_files(&[blob]);
        assert_eq!(paths.len(), 1);
        let file = paths[0].file_name().unwrap().to_string_lossy().to_string();
        assert!(!file.contains(".."), "{file}");
        assert!(!file.contains('/'), "{file}");
        assert!(paths[0].starts_with(std::env::temp_dir().join("notecraft-drop")));
    }

    #[test]
    fn a_nameless_blob_still_gets_a_file() {
        let paths = blob_files(&[crate::dropped::Blob {
            name: String::new(),
            bytes: b"GIF89a".to_vec(),
        }]);
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].extension().and_then(|e| e.to_str()), Some("gif"));
    }
}
