//! Theme tokens, bundled fonts and the custom-painted widget vocabulary.
//!
//! Everything visual goes through this module so the four themes stay honest:
//! no widget in the app may hard-code a colour.

use egui::{
    Color32, CornerRadius, FontFamily, FontId, Margin, Response, Sense, Shadow, Stroke, StrokeKind,
    TextStyle, Ui, Vec2,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------- fonts

const CHAKRA_REG: &[u8] = include_bytes!("../assets/ChakraPetch-Regular.ttf");
const CHAKRA_MED: &[u8] = include_bytes!("../assets/ChakraPetch-Medium.ttf");
const CHAKRA_SEMI: &[u8] = include_bytes!("../assets/ChakraPetch-SemiBold.ttf");
const CHAKRA_BOLD: &[u8] = include_bytes!("../assets/ChakraPetch-Bold.ttf");
const JETBRAINS: &[u8] = include_bytes!("../assets/JetBrainsMono.ttf");

pub const F_REG: &str = "Reg";
pub const F_MED: &str = "Med";
pub const F_SEMI: &str = "Semi";
pub const F_BOLD: &str = "Bold";
pub const F_MONO: &str = "Mono";

pub fn fam(name: &str) -> FontFamily {
    FontFamily::Name(name.into())
}

/// The four weight faces of the house font, in `F_REG` `F_MED` `F_SEMI` `F_BOLD` order. The keys are
/// the family names too (`fam(F_REG)` and so on) — that is what the chrome is set in.
const HOUSE: [(&str, &[u8]); 4] = [
    (F_REG, CHAKRA_REG),
    (F_MED, CHAKRA_MED),
    (F_SEMI, CHAKRA_SEMI),
    (F_BOLD, CHAKRA_BOLD),
];

/// The note faces the Display menu offers beside the house face.
///
/// Each ships **four** weights, one family each, because egui has no synthetic bold: a single-face
/// font would flatten every heading and every `**bold**` run to regular. All four are the same
/// variable font instanced at 400/500/600/700 and subset to Latin + punctuation (the emoji faces and
/// Chakra Petch behind each family catch anything outside that). Licences ride along in
/// `assets/licenses/`.
const GROTESK: [(&str, &[u8]); 4] = [
    ("Grotesk Reg", include_bytes!("../assets/SpaceGrotesk-Regular.ttf")),
    ("Grotesk Med", include_bytes!("../assets/SpaceGrotesk-Medium.ttf")),
    ("Grotesk Semi", include_bytes!("../assets/SpaceGrotesk-SemiBold.ttf")),
    ("Grotesk Bold", include_bytes!("../assets/SpaceGrotesk-Bold.ttf")),
];
const FRAUNCES: [(&str, &[u8]); 4] = [
    ("Fraunces Reg", include_bytes!("../assets/Fraunces-Regular.ttf")),
    ("Fraunces Med", include_bytes!("../assets/Fraunces-Medium.ttf")),
    ("Fraunces Semi", include_bytes!("../assets/Fraunces-SemiBold.ttf")),
    ("Fraunces Bold", include_bytes!("../assets/Fraunces-Bold.ttf")),
];
const LITERATA: [(&str, &[u8]); 4] = [
    ("Literata Reg", include_bytes!("../assets/Literata-Regular.ttf")),
    ("Literata Med", include_bytes!("../assets/Literata-Medium.ttf")),
    ("Literata Semi", include_bytes!("../assets/Literata-SemiBold.ttf")),
    ("Literata Bold", include_bytes!("../assets/Literata-Bold.ttf")),
];
const CAVEAT: [(&str, &[u8]); 4] = [
    ("Caveat Reg", include_bytes!("../assets/Caveat-Regular.ttf")),
    ("Caveat Med", include_bytes!("../assets/Caveat-Medium.ttf")),
    ("Caveat Semi", include_bytes!("../assets/Caveat-SemiBold.ttf")),
    ("Caveat Bold", include_bytes!("../assets/Caveat-Bold.ttf")),
];

/// One face a note can be set in, from the Display menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoteFont {
    /// Chakra Petch — the house face, and the default.
    House,
    /// Space Grotesk — a geometric sans with a techy cut.
    Grotesk,
    /// Fraunces — a display serif with some wonk in the curves.
    Fraunces,
    /// Literata — a book serif, cut for reading on screen.
    Literata,
    /// Caveat — handwriting, for a note that is a scribble rather than a draft.
    Caveat,
}

/// The picker's order: the house face first, then the alternatives.
pub const NOTE_FONTS: [NoteFont; 5] = [
    NoteFont::House,
    NoteFont::Grotesk,
    NoteFont::Fraunces,
    NoteFont::Literata,
    NoteFont::Caveat,
];

impl NoteFont {
    /// `state.json`'s spelling of the face.
    pub fn id(self) -> &'static str {
        match self {
            NoteFont::House => "house",
            NoteFont::Grotesk => "grotesk",
            NoteFont::Fraunces => "fraunces",
            NoteFont::Literata => "literata",
            NoteFont::Caveat => "caveat",
        }
    }

    /// The name the Display menu shows — set, in the menu, in the face it names.
    pub fn label(self) -> &'static str {
        match self {
            NoteFont::House => "Chakra Petch",
            NoteFont::Grotesk => "Space Grotesk",
            NoteFont::Fraunces => "Fraunces",
            NoteFont::Literata => "Literata",
            NoteFont::Caveat => "Caveat",
        }
    }

    /// A missing or unknown id is **the house face**: a `state.json` written before the picker
    /// existed has to open in the app's own type, not in whichever face happens to be listed first.
    pub fn from_id(s: Option<&str>) -> Self {
        NOTE_FONTS
            .iter()
            .copied()
            .find(|f| Some(f.id()) == s)
            .unwrap_or(NoteFont::House)
    }

    /// The four binaries, in `F_REG` `F_MED` `F_SEMI` `F_BOLD` order.
    fn faces(self) -> [(&'static str, &'static [u8]); 4] {
        match self {
            NoteFont::House => HOUSE,
            NoteFont::Grotesk => GROTESK,
            NoteFont::Fraunces => FRAUNCES,
            NoteFont::Literata => LITERATA,
            NoteFont::Caveat => CAVEAT,
        }
    }

    /// The family this face is set in at `role` (`F_REG`, `F_MED`, `F_SEMI`, `F_BOLD`). Any other
    /// role — a code run's `F_MONO` — is the regular face, but code never asks: it keeps JetBrains.
    pub fn fam(self, role: &str) -> FontFamily {
        let faces = self.faces();
        let i = match role {
            F_MED => 1,
            F_SEMI => 2,
            F_BOLD => 3,
            _ => 0,
        };
        fam(faces[i].0)
    }
}

/// The face the note body is set in — a process-wide switch, like the theme: the picker writes it and
/// every note the app lays out reads it, so no page has to carry it. `0` is the house face.
static NOTE_FONT: AtomicUsize = AtomicUsize::new(0);

pub fn set_note_font(face: NoteFont) {
    let i = NOTE_FONTS.iter().position(|f| *f == face).unwrap_or(0);
    NOTE_FONT.store(i, Ordering::Relaxed);
}

pub fn note_font() -> NoteFont {
    NOTE_FONTS[NOTE_FONT.load(Ordering::Relaxed).min(NOTE_FONTS.len() - 1)]
}

/// The family a **note body** is set in at `role`. The chrome keeps asking `fam(..)`, i.e. the house
/// face: a note in Fraunces must not turn the toolbar, the rail or the menus into a serif.
pub fn note_fam(role: &str) -> FontFamily {
    note_font().fam(role)
}

/// The page title: the note's own face, bold, at the size the old chrome "Title" style used. Read by
/// both `apply` (the text style) and the title `TextEdit` (which picks it up per frame, so switching
/// face in the Display menu re-sets the title in the same frame).
pub fn title_font() -> egui::FontId {
    egui::FontId::new(TITLE_SIZE, note_fam(F_BOLD))
}

/// The title at the size the Display menu's "text size" step asks for. The title is note text, so it
/// grows and shrinks with the rest of the note — a fixed 28 pt sat there unmoved at S and at L.
pub fn title_font_scaled(scale: f32) -> egui::FontId {
    egui::FontId::new(TITLE_SIZE * scale, note_fam(F_BOLD))
}

/// Base size of the page title at the middle "text size" step.
pub const TITLE_SIZE: f32 = 28.0;

/// Bundled house fonts (Chakra Petch for the chrome, JetBrains Mono for code) and every note face,
/// registered up front: the picker then swaps a family name instead of rebuilding the atlas.
pub fn install_fonts(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

/// The atlas's definition list, split out from `install_fonts` so a test can check that every family
/// the app names by string is actually bound — an unbound `FontFamily::Name(..)` panics inside
/// epaint's `Fonts::layout`, which kills the process on the first frame that asks for it.
fn font_definitions() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(egui::FontData::from_static(bytes)));
    };
    add("jbmono", JETBRAINS);

    // Keep egui's emoji/symbol faces as fallback so glyphs outside a note font (arrows, ticks, box
    // drawing) still rasterize instead of showing tofu; the house regular sits in front of them
    // because Chakra Petch is the one face guaranteed to cover the app's Latin text.
    let fallback = ["NotoEmoji-Regular", "emoji-icon-font"];

    for face in NOTE_FONTS {
        for (key, bytes) in face.faces() {
            add(key, bytes);
            let mut list = vec![key.to_owned()];
            if key != F_REG {
                list.push(F_REG.to_owned());
            }
            list.extend(fallback.iter().map(|s| s.to_string()));
            fonts.families.insert(fam(key), list);
        }
    }

    fonts.families.insert(
        FontFamily::Proportional,
        {
            let mut v = vec![F_REG.to_owned(), F_MED.to_owned()];
            v.extend(fallback.iter().map(|s| s.to_string()));
            v
        },
    );
    fonts.families.insert(
        FontFamily::Monospace,
        {
            let mut v = vec!["jbmono".to_owned(), F_REG.to_owned()];
            v.extend(fallback.iter().map(|s| s.to_string()));
            v
        },
    );
    // code keeps JetBrains Mono: `fam(F_MONO)` is asked for by every code run, and a note face must
    // not set code (it would stop lining up).
    fonts.families.insert(
        fam(F_MONO),
        {
            let mut v = vec!["jbmono".to_owned(), F_REG.to_owned()];
            v.extend(fallback.iter().map(|s| s.to_string()));
            v
        },
    );
    fonts
}

// ---------------------------------------------------------------- themes

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Theme {
    pub id: &'static str,
    pub label: &'static str,
    pub dark: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub panel2: Color32,
    pub card: Color32,
    pub card_hover: Color32,
    pub border: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub faint: Color32,
    /// The quietest ink there is: a control that exists but is *inert* (Undo with nothing to undo,
    /// the arrow of a folded-away panel). It is deliberately below AA — nothing readable may use it,
    /// which is what `faint` is for now. Before this token existed, `faint` was 2.3:1 and carried every
    /// 10.5 pt label in the app.
    pub ghost: Color32,
    pub accent: Color32,
    pub accent2: Color32,
    pub heading: Color32,
    pub code_bg: Color32,
    pub done: Color32,
    pub warn: Color32,
    pub danger: Color32,
    /// Highlighter colours for a marked run — a felt-tip marker, not a translucent wash: the text on
    /// top is `mark_ink`, so the mark reads the same on a dark page as on paper.
    pub marks: [Color32; 5],
    /// The ink of highlighted words: dark on every theme, because a marker is opaque.
    pub mark_ink: Color32,
}

/// The highlighter names, in palette order (`Attrs::mark` is 1-based; 0 means no mark).
pub const MARK_NAMES: [&str; 5] = ["yellow", "green", "pink", "blue", "orange"];

/// The fluorescent palette: the colours a real highlighter leaves on paper.
pub const MARKS: [Color32; 5] = [
    rgb(255, 232, 92),  // yellow
    rgb(167, 240, 122), // green
    rgb(255, 156, 197), // pink
    rgb(140, 214, 255), // blue
    rgb(255, 190, 112), // orange
];

/// Dark ink for a marked run: readable on all five markers.
const MARK_INK: Color32 = rgb(24, 22, 18);

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub const VOID: Theme = Theme {
    id: "void",
    label: "Void",
    dark: true,
    bg: rgb(10, 13, 19),
    panel: rgb(16, 21, 30),
    panel2: rgb(21, 26, 38),
    card: rgb(26, 32, 46),
    card_hover: rgb(33, 41, 57),
    border: rgb(38, 46, 64),
    text: rgb(226, 231, 240),
    dim: rgb(152, 162, 184),
    faint: rgb(120, 137, 171),
    ghost: rgb(58, 66, 84),
    accent: rgb(79, 209, 197),
    accent2: rgb(124, 156, 255),
    heading: rgb(255, 255, 255),
    code_bg: rgb(33, 39, 54),
    done: rgb(88, 192, 138),
    warn: rgb(242, 180, 87),
    danger: rgb(242, 112, 106),
    marks: MARKS,
    mark_ink: MARK_INK,
};

pub const EMBER: Theme = Theme {
    id: "ember",
    label: "Ember",
    dark: true,
    bg: rgb(20, 15, 11),
    panel: rgb(28, 22, 16),
    panel2: rgb(36, 28, 21),
    card: rgb(43, 34, 25),
    card_hover: rgb(53, 42, 30),
    border: rgb(60, 47, 34),
    text: rgb(243, 236, 226),
    dim: rgb(182, 167, 147),
    faint: rgb(163, 135, 100),
    ghost: rgb(78, 66, 50),
    accent: rgb(233, 161, 59),
    accent2: rgb(242, 112, 106),
    heading: rgb(255, 246, 233),
    code_bg: rgb(46, 36, 27),
    done: rgb(143, 191, 106),
    warn: rgb(242, 180, 87),
    danger: rgb(239, 108, 95),
    marks: MARKS,
    mark_ink: MARK_INK,
};

pub const SLATE: Theme = Theme {
    id: "slate",
    label: "Slate",
    dark: true,
    bg: rgb(17, 21, 25),
    panel: rgb(23, 28, 33),
    panel2: rgb(29, 35, 41),
    card: rgb(35, 42, 49),
    card_hover: rgb(43, 51, 59),
    border: rgb(46, 54, 62),
    text: rgb(228, 233, 238),
    dim: rgb(154, 165, 177),
    faint: rgb(130, 148, 166),
    ghost: rgb(58, 68, 78),
    accent: rgb(104, 195, 232),
    accent2: rgb(214, 137, 190),
    heading: rgb(255, 255, 255),
    code_bg: rgb(40, 45, 54),
    done: rgb(126, 190, 140),
    warn: rgb(226, 187, 106),
    danger: rgb(232, 122, 118),
    marks: MARKS,
    mark_ink: MARK_INK,
};

/// Paper is the one light theme, so its colours are chosen against a *light* background — the
/// dark themes can get away with mid-tone accents and very low-contrast hints because everything
/// is composited onto near-black, and this one cannot. Every text token here clears WCAG AA
/// (4.5:1) on `bg`, and the coloured ones also clear it on the pale tint of themselves that the
/// chips/tag runs draw behind them (`accent` and `warn` are the two that get that treatment);
/// `tests_light` pins those numbers so a future "prettier" value cannot quietly drop a label back
/// to unreadable. `faint` is the smallest text in the app (10.5 pt labels, meta, hints) and used
/// to be rgb(185,178,163) — 1.9:1 on this page, i.e. physically unreadable.
pub const PAPER: Theme = Theme {
    id: "paper",
    label: "Paper",
    dark: false,
    bg: rgb(244, 241, 234),
    panel: rgb(250, 248, 243),
    panel2: rgb(237, 233, 224),
    card: rgb(255, 255, 255),
    card_hover: rgb(242, 238, 229),
    border: rgb(208, 201, 186),
    text: rgb(32, 34, 39),
    dim: rgb(86, 91, 97),
    faint: rgb(112, 106, 92),
    ghost: rgb(176, 168, 152),
    accent: rgb(160, 60, 38),
    accent2: rgb(35, 95, 122),
    heading: rgb(17, 19, 23),
    code_bg: rgb(222, 215, 198),
    done: rgb(46, 108, 64),
    warn: rgb(124, 89, 16),
    danger: rgb(168, 48, 38),
    marks: MARKS,
    mark_ink: MARK_INK,
};

pub fn themes() -> [Theme; 4] {
    [VOID, EMBER, SLATE, PAPER]
}

/// Apply a theme to the whole context. Called on startup and on theme change.
pub fn apply(ctx: &egui::Context, t: &Theme) {
    let mut style = (*ctx.style()).clone();
    let mut v = if t.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.dark_mode = t.dark;
    v.panel_fill = t.panel;
    v.window_fill = t.card;
    v.window_stroke = Stroke::new(1.0_f32, t.border);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow {
        offset: [0, 12],
        blur: 30,
        spread: 0,
        color: Color32::from_black_alpha(if t.dark { 170 } else { 45 }),
    };
    v.popup_shadow = Shadow {
        offset: [0, 8],
        blur: 20,
        spread: 0,
        color: Color32::from_black_alpha(if t.dark { 150 } else { 35 }),
    };
    v.extreme_bg_color = t.code_bg;
    v.faint_bg_color = t.panel2;
    v.code_bg_color = t.code_bg;
    v.hyperlink_color = t.accent2;
    v.error_fg_color = t.danger;
    v.warn_fg_color = t.warn;
    v.override_text_color = Some(t.text);
    v.selection = egui::style::Selection {
        bg_fill: t.accent.gamma_multiply(0.32),
        stroke: Stroke::new(1.0_f32, t.accent),
    };

    let wv = |bg: Color32, weak: Color32, fg: Color32| egui::style::WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: weak,
        bg_stroke: Stroke::new(1.0_f32, t.border),
        fg_stroke: Stroke::new(1.0_f32, fg),
        corner_radius: CornerRadius::same(8),
        expansion: 0.0,
    };
    v.widgets.noninteractive = wv(t.panel, t.panel, t.text);
    v.widgets.inactive = wv(t.card, t.card, t.text);
    v.widgets.hovered = wv(t.card_hover, t.card_hover, t.heading);
    // A pressed widget is a filled accent chip with the page colour as its label. On a dark theme
    // the accent is softened to 85 % so it does not glare; on a light theme that alpha only washes
    // the fill toward white and drops the label to ~4.3:1, so the accent stays at full strength.
    let active_fill = if t.dark {
        t.accent.gamma_multiply(0.85)
    } else {
        t.accent
    };
    v.widgets.active = wv(active_fill, active_fill, t.bg);
    v.widgets.open = wv(t.card_hover, t.card_hover, t.heading);

    style.visuals = v;
    style.spacing.item_spacing = Vec2::new(8.0, 7.0);
    style.spacing.button_padding = Vec2::new(10.0, 5.0);
    style.spacing.window_margin = Margin::same(12);
    style.spacing.indent = 14.0;
    style.spacing.interact_size = Vec2::new(30.0, 26.0);
    style.spacing.scroll.floating = true;
    style.spacing.scroll.bar_width = 6.0;
    style.spacing.scroll.bar_inner_margin = 2.0;
    style.spacing.scroll.bar_outer_margin = 4.0;
    style.animation_time = 0.10;
    style.wrap_mode = Some(egui::TextWrapMode::Wrap);

    let mut ts: BTreeMap<TextStyle, FontId> = BTreeMap::new();
    macro_rules! ins {
        ($name:expr, $size:expr, $fam:expr) => {
            ts.insert(TextStyle::Name($name.into()), FontId::new($size, fam($fam)));
        };
    }
    ts.insert(TextStyle::Heading, FontId::new(23.0, fam(F_BOLD)));
    ts.insert(TextStyle::Body, FontId::new(15.0, fam(F_REG)));
    ts.insert(TextStyle::Button, FontId::new(14.0, fam(F_MED)));
    ts.insert(TextStyle::Small, FontId::new(12.5, fam(F_REG)));
    ts.insert(TextStyle::Monospace, FontId::new(13.5, fam(F_MONO)));
    // The page title is the one line that must be in the note's own face: it used to take the chrome
    // face, so a note set in Literata opened under a Chakra Petch title — two voices on one page, and
    // the mismatch was the first thing you saw. `title_font` is the single definition, so the style
    // and the TextEdit that draws the title can never disagree.
    ts.insert(TextStyle::Name("Title".into()), title_font());
    ins!("H1", 22.0, F_BOLD);
    ins!("H2", 18.5, F_SEMI);
    ins!("H3", 16.0, F_SEMI);
    ins!("Label", 12.0, F_MED);
    ins!("Tiny", 11.0, F_MED);
    ins!("MonoSmall", 12.0, F_MONO);
    style.text_styles = ts;

    ctx.set_style(style);
}

// ---------------------------------------------------------------- widgets
//
// Custom-painted vocabulary. Every one of these is a single `allocate_exact_size`
// plus painter calls, so hover/selected states are identical across the app.

/// Rounded checkbox with a painted tick. Returns the click response.
pub fn checkbox(ui: &mut Ui, checked: bool, t: &Theme) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::click());
    paint_checkbox(ui, rect, checked, resp.hovered(), t);
    resp
}

/// Same checkbox at an explicit rect (used by the editor's todo gutter, where
/// the box must line up with a row of the text galley).
pub fn paint_checkbox(ui: &mut Ui, rect: egui::Rect, checked: bool, hovered: bool, t: &Theme) {
    let p = ui.painter();
    let r = rect.shrink(1.0);
    if checked {
        p.rect_filled(r, CornerRadius::same(4), t.done);
        p.rect_stroke(
            r,
            CornerRadius::same(4),
            Stroke::new(1.0_f32, t.done),
            StrokeKind::Inside,
        );
        let c = r.center();
        p.line_segment(
            [c + Vec2::new(-4.0, 0.0), c + Vec2::new(-1.0, 3.0)],
            Stroke::new(2.0_f32, t.bg),
        );
        p.line_segment(
            [c + Vec2::new(-1.0, 3.0), c + Vec2::new(4.0, -3.0)],
            Stroke::new(2.0_f32, t.bg),
        );
    } else {
        let stroke = if hovered {
            Stroke::new(1.6_f32, t.accent)
        } else {
            Stroke::new(1.4_f32, t.faint)
        };
        p.rect_stroke(r, CornerRadius::same(4), stroke, StrokeKind::Inside);
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

/// Pill with a tinted background — used for tags, due dates and status.
pub fn chip(ui: &mut Ui, text: &str, color: Color32, t: &Theme) -> Response {
    let font = FontId::new(12.0, fam(F_MED));
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font.clone(), color));
    let pad = Vec2::new(8.0, 3.0);
    let size = galley.size() + pad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let fill = color.gamma_multiply(if t.dark { 0.20 } else { 0.14 });
    p.rect_filled(rect, CornerRadius::same(99), fill);
    let stroke = if resp.hovered() {
        Stroke::new(1.2_f32, color)
    } else {
        Stroke::new(1.0_f32, color.gamma_multiply(0.45))
    };
    p.rect_stroke(rect, CornerRadius::same(99), stroke, StrokeKind::Inside);
    p.galley(rect.min + pad, galley, color);
    resp
}

/// The status bar's compact chip. `chip`'s 12 pt label plus its 3 pt padding is taller than the
/// 26 pt bar it would sit in, so this one is built for the line: same pill, smaller type and
/// padding.
pub fn chip_small(ui: &mut Ui, text: &str, color: Color32, t: &Theme) -> Response {
    let font = FontId::new(10.5, fam(F_MED));
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font.clone(), color));
    let pad = Vec2::new(6.0, 1.0);
    let size = galley.size() + pad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    let fill = color.gamma_multiply(if t.dark { 0.20 } else { 0.14 });
    p.rect_filled(rect, CornerRadius::same(99), fill);
    let stroke = if resp.hovered() {
        Stroke::new(1.2_f32, color)
    } else {
        Stroke::new(1.0_f32, color.gamma_multiply(0.45))
    };
    p.rect_stroke(rect, CornerRadius::same(99), stroke, StrokeKind::Inside);
    p.galley(rect.min + pad, galley, color);
    resp
}

/// The size a chip's label needs, so a caller can place a chip itself (the editor puts `+ due` at the
/// end of a task's words, not at the column edge).
pub fn chip_size(ui: &mut Ui, text: &str) -> Vec2 {
    let font = FontId::new(12.0, fam(F_MED));
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER));
    galley.size() + Vec2::new(8.0, 3.0) * 2.0
}

/// Paint a chip into an exact rect and take its clicks. Same look as `chip`, but the caller decides
/// where it goes.
pub fn chip_at(ui: &mut Ui, rect: egui::Rect, text: &str, color: Color32, t: &Theme, id: egui::Id) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    let font = FontId::new(12.0, fam(F_MED));
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, color));
    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same(99),
        color.gamma_multiply(if t.dark { 0.20 } else { 0.14 }),
    );
    let stroke = if resp.hovered() {
        Stroke::new(1.2_f32, color)
    } else {
        Stroke::new(1.0_f32, color.gamma_multiply(0.45))
    };
    p.rect_stroke(rect, CornerRadius::same(99), stroke, StrokeKind::Inside);
    let pad = Vec2::new(8.0, 3.0);
    let pos = rect.min + Vec2::new(pad.x, (rect.height() - galley.size().y) * 0.5);
    p.galley(pos, galley, color);
    resp
}

/// A chip that has to shout: a stronger tint and a solid border, for an overdue date.
pub fn chip_strong(ui: &mut Ui, text: &str, color: Color32, t: &Theme) -> Response {
    let font = FontId::new(12.0, fam(F_MED));
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font.clone(), color));
    let pad = Vec2::new(8.0, 3.0);
    let size = galley.size() + pad * 2.0;
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same(99),
        color.gamma_multiply(if t.dark { 0.34 } else { 0.26 }),
    );
    p.rect_stroke(
        rect,
        CornerRadius::same(99),
        Stroke::new(1.2_f32, color),
        StrokeKind::Inside,
    );
    p.galley(rect.min + pad, galley, color);
    resp
}

/// Small square icon button drawn from a unicode glyph.
pub fn icon_button(ui: &mut Ui, glyph: &str, tip: &str, t: &Theme) -> Response {
    debug_assert!(
        ICON_GLYPHS.contains(glyph),
        "glyph {glyph:?} has no face in the bundled fonts — it will render as a tofu box"
    );
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(8), t.card_hover);
    }
    let color = if resp.hovered() { t.heading } else { t.dim };
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        FontId::new(15.0, fam(F_MED)),
        color,
    );
    resp.on_hover_text(tip)
}

/// An icon button whose mark is **painted** by the caller.
///
/// Some verbs have no glyph in the bundled faces — "crop" is not in Chakra Petch, and a tofu box reads
/// as a broken button, so the shape is drawn with the painter instead. Same box, hover and tooltip as
/// `icon_button`, so the two are interchangeable in a row.
pub fn icon_button_painted(
    ui: &mut Ui,
    tip: &str,
    t: &Theme,
    paint: impl FnOnce(&egui::Painter, egui::Rect, Color32),
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(8), t.card_hover);
    }
    let color = if resp.hovered() { t.heading } else { t.dim };
    paint(ui.painter(), rect.shrink(8.0), color);
    resp.on_hover_text(tip)
}

/// Glyphs the bundled faces can actually rasterise — checked against the cmap of every embedded
/// face (Chakra Petch ×2 weights, JetBrains Mono, NotoEmoji, emoji-icon), not assumed from a
/// lookalike. A symbol outside this set renders as a tofu box — `✎` (U+270E) and `◐` (U+25D0) both
/// did, which is how a square appeared where a pen was meant. `✓` (U+2713) and `◦` (U+25E6) were in
/// this list for a while and are in no face at all: a set that lists a glyph the atlas cannot draw
/// is worse than no set, because `icon_button`'s debug assert then passes for exactly the character
/// it was written to catch.
pub const ICON_GLYPHS: &str = "←↑→↓×•✏◑◆■↔+";

/// Full-width navigation row with an accent bar when selected.
pub fn nav_row(
    ui: &mut Ui,
    label: &str,
    selected: bool,
    badge: Option<String>,
    indent: f32,
    t: &Theme,
) -> Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 27.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, CornerRadius::same(7), t.accent.gamma_multiply(0.16));
        p.rect_filled(
            egui::Rect::from_min_size(rect.min + Vec2::new(0.0, 5.0), Vec2::new(2.5, rect.height() - 10.0)),
            CornerRadius::same(2),
            t.accent,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(7), t.card_hover);
    }
    let text_color = if selected {
        t.heading
    } else if resp.hovered() {
        t.text
    } else {
        t.dim
    };
    let galley = ui.fonts_mut(|f| {
        f.layout_no_wrap(
            label.to_owned(),
            FontId::new(13.5, fam(if selected { F_SEMI } else { F_MED })),
            text_color,
        )
    });
    let y = rect.center().y - galley.size().y / 2.0;
    let clipped = galley.size().x > rect.width() - 16.0 - indent;
    ui.painter().galley(
        egui::Pos2::new(rect.min.x + 10.0 + indent, y),
        if clipped {
            ui.fonts_mut(|f| {
                f.layout(
                    label.to_owned(),
                    FontId::new(13.5, fam(F_MED)),
                    text_color,
                    rect.width() - 16.0 - indent,
                )
            })
        } else {
            galley
        },
        text_color,
    );
    if let Some(b) = badge {
        let g = ui.fonts_mut(|f| {
            f.layout_no_wrap(b, FontId::new(11.0, fam(F_MONO)), t.faint)
        });
        ui.painter().galley(
            egui::Pos2::new(rect.max.x - g.size().x - 8.0, rect.center().y - g.size().y / 2.0),
            g,
            t.faint,
        );
    }
    resp
}

/// A small header above a group of rows — `Notebooks`, `Sections`, `Pages`, `Commands`.
///
/// It is set exactly like the small labels in the Display menu (`row_label`, `menu_fonts`): JetBrains
/// Mono, 11 pt, `faint`. It used to be Chakra Petch, uppercased and tracked out with thin spaces, so
/// the same job — "this is a heading for the rows below" — was done in two different voices two
/// panels apart: the panel said `N O T E B O O K S` in a geometric sans and the menu directly above
/// it said `note font` in mono. The uppercase and the tracking are gone with the face: tracking a
/// monospaced font only adds gaps to spacing that is already even.
pub fn section_label(ui: &mut Ui, text: &str, t: &Theme) {
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(text)
            .family(fam(F_MONO))
            .size(11.0)
            .color(t.faint),
    );
}

/// Toolbar button: text, optional accent tint, flat until hovered.
pub fn toolbar_button(ui: &mut Ui, label: &str, active: bool, t: &Theme) -> Response {
    let font = FontId::new(13.0, fam(F_MED));
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), t.text));
    let size = galley.size() + Vec2::new(18.0, 9.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, CornerRadius::same(8), t.accent.gamma_multiply(0.20));
        p.rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.0_f32, t.accent.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(8), t.card_hover);
    }
    let color = if active {
        t.accent
    } else if resp.hovered() {
        t.heading
    } else {
        t.dim
    };
    let g = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), color));
    p.galley(rect.min + Vec2::new(9.0, 4.5), g, color);
    resp
}

/// A scope segment: [toolbar_button] with the inactive labels at body-text brightness.
///
/// The scope toggle is the primary control of its row, not a secondary action tucked into a
/// toolbar, and `dim` made the two unchosen scopes read as a caption rather than as buttons.
pub fn segment_button(ui: &mut Ui, label: &str, active: bool, t: &Theme) -> Response {
    let font = FontId::new(13.0, fam(F_MED));
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), t.text));
    let size = galley.size() + Vec2::new(18.0, 9.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, CornerRadius::same(8), t.accent.gamma_multiply(0.20));
        p.rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.0_f32, t.accent.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(8), t.card_hover);
    }
    let color = if active {
        t.accent
    } else if resp.hovered() {
        t.heading
    } else {
        t.text
    };
    let g = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), color));
    p.galley(rect.min + Vec2::new(9.0, 4.5), g, color);
    resp
}

// ---------------------------------------------------------------- contrast guards

/// WCAG relative luminance of a colour (sRGB, 0..1).
#[cfg(test)]
fn rel_lum(c: Color32) -> f32 {
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
}

/// WCAG contrast ratio, 1.0 (identical) … 21.0 (black on white). 4.5 is AA for body text.
#[cfg(test)]
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (rel_lum(a), rel_lum(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `fg` at `alpha` composited onto `bg` — what a chip's pale wash actually looks like on screen.
#[cfg(test)]
pub fn over(fg: Color32, alpha: f32, bg: Color32) -> Color32 {
    let mix = |f: u8, b: u8| (f as f32 * alpha + b as f32 * (1.0 - alpha)).round() as u8;
    Color32::from_rgb(mix(fg.r(), bg.r()), mix(fg.g(), bg.g()), mix(fg.b(), bg.b()))
}

#[cfg(test)]
mod tests_light {
    use super::*;

    /// Paper is the only theme drawn on a light background, so it is the only one where a mid-tone
    /// accent or a "quiet" hint colour can become invisible. Every text token has to clear AA on
    /// every surface it can sit on — this is the guard that keeps `faint` (10.5 pt labels, page
    /// meta, the legend, the status bar) from drifting back to a 1.9:1 whisper.
    #[test]
    fn every_paper_text_token_clears_aa() {
        let t = PAPER;
        let tokens = [
            ("text", t.text),
            ("dim", t.dim),
            ("faint", t.faint),
            ("heading", t.heading),
            ("accent", t.accent),
            ("accent2", t.accent2),
            ("done", t.done),
            ("warn", t.warn),
            ("danger", t.danger),
        ];
        for (name, c) in tokens {
            for (surface, bg) in [("bg", t.bg), ("panel", t.panel), ("card", t.card)] {
                let r = contrast(c, bg);
                assert!(r >= 4.5, "{name} on {surface} is {r:.2}:1, under AA");
            }
        }
        // a tag/due run paints the colour over a 13–14 % wash of itself, and a chip does the same
        for (name, c) in [("accent", t.accent), ("warn", t.warn)] {
            let wash = over(c, 0.14, t.bg);
            let r = contrast(c, wash);
            assert!(r >= 4.5, "{name} on its own chip wash is {r:.2}:1, under AA");
        }
    }

    /// The dark themes are allowed quiet hints, but body text and headings must stay loud.
    #[test]
    fn dark_ink_reads_on_every_highlighter() {
        for theme in themes() {
            for (i, c) in theme.marks.iter().enumerate() {
                let ratio = contrast(theme.mark_ink, *c);
                assert!(
                    ratio >= 4.5,
                    "{}: {} on the {i} marker is {ratio:.1}:1",
                    theme.id,
                    MARK_NAMES[i]
                );
            }
        }
    }

    #[test]
    fn body_text_is_loud_in_every_theme() {
        for t in themes() {
            for (name, c) in [("text", t.text), ("heading", t.heading)] {
                let r = contrast(c, t.bg);
                assert!(r >= 7.0, "{}: {name} is {r:.2}:1", t.label);
            }
            let r = contrast(t.dim, t.bg);
            assert!(r >= 4.0, "{}: dim is {r:.2}:1", t.label);
        }
    }

    /// `faint` is the colour of every hint the app draws: section labels, page meta, the status
    /// bar's counters, the tasks view's `status` / `due` labels, the palette's row hints, the folded
    /// panel's arrow. On the three dark themes it used to be 2.3–2.4:1 — labels that read as disabled
    /// controls — while Paper had already been measured and fixed. This is the same guard for all
    /// four, on every surface a hint can sit on, and it keeps `ghost` (the genuinely inert ink) from
    /// being used where the user has to read.
    #[test]
    fn every_hint_token_clears_aa_in_every_theme() {
        for t in themes() {
            for (surface, bg) in [("bg", t.bg), ("panel", t.panel), ("card", t.card)] {
                let r = contrast(t.faint, bg);
                assert!(r >= 4.5, "{}: faint on {surface} is {r:.2}:1, under AA", t.label);
            }
            // `ghost` is for things that cannot be used, so it is allowed to be quiet — but not
            // invisible: a dead control still has to look like a control.
            let g = contrast(t.ghost, t.bg);
            assert!(
                (1.5..4.5).contains(&g),
                "{}: ghost on bg is {g:.2}:1 (wants 1.5–4.5)",
                t.label
            );
            // and there has to be a visible step between the three inks, or the ladder is a lie
            assert!(
                contrast(t.dim, t.bg) > contrast(t.faint, t.bg),
                "{}: dim is not brighter than faint",
                t.label
            );
        }
    }

    /// A code block is a surface, not loose monospace: `code_bg` used to sit at 1.02–1.03:1 against
    /// `bg`, so the frame the editor draws was invisible on all three dark themes and the block read
    /// as prose in another font. 1.25:1 is the point at which a fill reads as a panel rather than as
    /// a compression artefact.
    #[test]
    fn the_code_panel_reads_as_a_surface() {
        for t in themes() {
            let fill = contrast(t.code_bg, t.bg);
            assert!(fill >= 1.25, "{}: code_bg vs bg is {fill:.2}:1", t.label);
            // and the code itself has to stay readable on that fill
            let ink = contrast(t.text, t.code_bg);
            assert!(ink >= 7.0, "{}: text on code_bg is {ink:.2}:1", t.label);
        }
    }
}

#[cfg(test)]
mod tests_fonts {
    use super::*;

    /// egui has no synthetic bold, so a face that ships one binary would flatten every heading and
    /// every `**bold**` run to regular. Each face therefore has to own four *distinct* families.
    #[test]
    fn every_face_keeps_its_own_weights() {
        for face in NOTE_FONTS {
            let keys = face.faces().map(|(k, _)| k);
            for (i, k) in keys.iter().enumerate() {
                assert!(
                    !keys[..i].contains(k),
                    "{}: {k} is registered twice",
                    face.id()
                );
            }
            assert_eq!(face.fam(F_REG), fam(keys[0]));
            assert_eq!(face.fam(F_MED), fam(keys[1]));
            assert_eq!(face.fam(F_SEMI), fam(keys[2]));
            assert_eq!(face.fam(F_BOLD), fam(keys[3]));
            assert_ne!(face.fam(F_REG), face.fam(F_BOLD), "{}", face.id());
        }
    }

    /// A `state.json` with no `note_font` (i.e. one written before the picker existed) has to draw
    /// exactly what the build before it drew: the house face *is* the chrome's own families.
    #[test]
    fn the_house_face_is_the_chrome() {
        for role in [F_REG, F_MED, F_SEMI, F_BOLD] {
            assert_eq!(NoteFont::House.fam(role), fam(role));
        }
        assert_eq!(note_font(), NoteFont::House);
    }

    #[test]
    fn an_unknown_or_missing_font_id_is_the_house_face() {
        assert_eq!(NoteFont::from_id(None), NoteFont::House);
        assert_eq!(NoteFont::from_id(Some("nonesuch")), NoteFont::House);
        for face in NOTE_FONTS {
            assert_eq!(NoteFont::from_id(Some(face.id())), face, "{}", face.id());
        }
        // the ids `save_state` writes are the ones `from_id` reads, and no two faces share one
        let ids: Vec<&str> = NOTE_FONTS.iter().map(|f| f.id()).collect();
        for (i, id) in ids.iter().enumerate() {
            assert!(!ids[..i].contains(id), "{id} is not unique");
        }
    }

    /// Every family the app names by string has to be bound, and to binaries that exist: epaint
    /// panics on an unbound `FontFamily::Name`, and the panic lands on the first frame that lays out
    /// a run in it (this is how a missing `fam(F_MONO)` killed the process rather than a test).
    #[test]
    fn every_family_the_app_asks_for_is_bound() {
        let fonts = font_definitions();
        let mut keys: Vec<&str> = vec![F_REG, F_MED, F_SEMI, F_BOLD, F_MONO];
        for face in NOTE_FONTS {
            for (key, _) in face.faces() {
                keys.push(key);
            }
        }
        for key in keys {
            let list = fonts
                .families
                .get(&fam(key))
                .unwrap_or_else(|| panic!("{key} is not bound to any font"));
            assert!(!list.is_empty(), "{key} has an empty fallback chain");
            assert!(
                fonts.font_data.contains_key(&list[0]),
                "{key} asks for missing data {}",
                list[0]
            );
        }
        assert!(fonts.families.contains_key(&FontFamily::Proportional));
        assert!(fonts.families.contains_key(&FontFamily::Monospace));
    }

    /// The binaries are embedded with `include_bytes!`, which only checks that a file exists: a
    /// truncated or accidentally-substituted asset would still compile and then panic inside egui's
    /// atlas. Check the TrueType magic and a sane size instead.
    #[test]
    fn every_embedded_face_is_a_real_truetype_file() {
        for face in NOTE_FONTS {
            for (key, bytes) in face.faces() {
                assert!(bytes.len() > 10_000, "{}: {key} is too small", face.id());
                let magic = u32::from_be_bytes(bytes[..4].try_into().unwrap());
                assert!(
                    magic == 0x0001_0000 || magic == 0x7472_7565,
                    "{}: {key} is not a ttf",
                    face.id()
                );
            }
        }
    }
}
