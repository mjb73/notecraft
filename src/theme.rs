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
/// The glyph fallbacks every family ends with (see `font_definitions`). No note face — and not
/// Chakra Petch either — carries arrows, maths, dingbats or emoji, so without these a note full of
/// them reads as rows of empty boxes.
///
/// * Noto Sans Symbols 2 — geometric shapes, dingbats, pictographs, half the technical marks.
/// * Noto Sans Symbols — the misc-technical marks Symbols 2 leaves out (⌘ ⏎ ⎋ ⏱), some arrows.
/// * Noto Emoji — the *full* monochrome emoji face. egui bundles a subset of this same font and
///   scales it 0.81; the subset is why 😀 rendered but 🧠 🧪 🧭 🗓 did not.
/// * DejaVu Sans — last catch-all: ⇒ ⇐ ⇢, letterlike ℃ ℕ, Greek and Cyrillic the note faces lack.
///
/// All four are outline fonts (epaint cannot draw colour emoji: it rasterises with `ab_glyph`, which
/// has no COLR/bitmap support), so emoji land as crisp monochrome glyphs in the text's own ink.
const NOTO_SYMBOLS2: &[u8] = include_bytes!("../assets/NotoSansSymbols2-Regular.ttf");
const NOTO_SYMBOLS: &[u8] = include_bytes!("../assets/NotoSansSymbols-Regular.ttf");
const NOTO_EMOJI: &[u8] = include_bytes!("../assets/NotoEmoji-Regular.ttf");
const DEJAVU: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");
/// Carlito: the free, metric-compatible Calibri. Two binaries only — regular and bold — so the
/// medium and semi roles point at the nearest cut. egui has no synthetic weight, so the four names
/// still have to exist; a ribbon label is regular and a heading is bold, exactly as Word draws them.
const CARLITO_REG: &[u8] = include_bytes!("../assets/Carlito-Regular.ttf");
const CARLITO_BOLD: &[u8] = include_bytes!("../assets/Carlito-Bold.ttf");
const CARLITO: [(&str, &[u8]); 4] = [
    ("Carlito Reg", CARLITO_REG),
    ("Carlito Med", CARLITO_REG),
    ("Carlito Semi", CARLITO_BOLD),
    ("Carlito Bold", CARLITO_BOLD),
];

pub const F_REG: &str = "Reg";
pub const F_MED: &str = "Med";
pub const F_SEMI: &str = "Semi";
pub const F_BOLD: &str = "Bold";
pub const F_MONO: &str = "Mono";

// ---------------------------------------------------------------- bar metrics
//
// One set of numbers for the whole chrome, in every theme: a bar whose labels are 12, 13 and 14 pt
// with three different gaps reads as three different apps stacked on top of each other, which is
// exactly what the first two-row pass looked like. Sizes and gaps live here so a theme can change
// colours but never the rhythm of the bar.

/// Height of the title row (wordmark, breadcrumb, search, theme switch).
pub const BAR_ROW1_H: f32 = 32.0;
/// Height of the tab strip under it.
pub const BAR_TABS_H: f32 = 26.0;
/// Height of the ribbon content row: one row of controls plus their captions, with 2 pt of air
/// under the captions so they do not sit on the band's own edge.
pub const BAR_CONTENT_H: f32 = 46.0;
/// Left and right padding — identical on every strip and in every theme.
pub const BAR_PAD: f32 = 12.0;
/// Every label in the bar: tabs, painted buttons, menus, the breadcrumb.
///
/// 11.5, not 13: WordCraft's menu line measures ~11.3 pt of ink at a 1200 pt window, and a bar a
/// size and a half larger than the one it is modelled on was the first thing that read as wrong.
pub const BAR_FONT: f32 = 11.5;
/// Secondary text in the bar: the search hint and the group captions (~9.5 in WordCraft).
pub const BAR_SMALL: f32 = 9.5;
/// Height of a row-1 control (the search box, the theme switch).
pub const BAR_CTRL_H: f32 = 24.0;
/// Height of a row-2 control (a tab, a painted button, a menu). 28, not 26: the picture buttons are
/// squares of this, and the icons in them were asked to be bigger twice — a 19 pt drawing needs the
/// room, and 2 pt came off the air under the captions (`BAR_CONTENT_H` 46) rather than off the icons.
pub const BAR_BTN_H: f32 = 28.0;
/// Gap between two controls inside one group.
pub const BAR_ITEM_GAP: f32 = 6.0;
/// Gap between two controls *inside* one ribbon group. Tighter than `BAR_ITEM_GAP`: a group is a
/// cluster of pictures that belong together, and the rules between groups (12 pt either side) are
/// what separates one job from the next. At 6 pt the icon-only row was 762 pt; at 4 it is ~730, which
/// is the difference between the highlighter swatches fitting a half-width portrait window or not.
pub const BAR_IN_GROUP_GAP: f32 = 4.0;
/// Gap either side of a group rule.
pub const BAR_GROUP_GAP: f32 = 12.0;
/// Height of the caption line under a group. An 11 pt mono line is ~14 pt tall (ascent + descender),
/// so a 12 pt box drew the caption into the band's edge and cut it in half.
pub const BAR_CAPTION_H: f32 = 14.0;
/// The folded panel strip's stacked label. Bold, and one size up from the 9.5 pt mono it was set at:
/// the strip is 26 pt wide and can carry it, and the letters are the only thing that says which
/// panel comes back, so they have to read without being hovered.
pub const FOLDED_LABEL_SIZE: f32 = 10.5;
/// The face that label is set in. The chrome's bold, not `F_MONO`: mono ships one weight, so "bold"
/// in mono is a wish, and the letters are stacked one per line so they need no fixed advance.
pub const FOLDED_LABEL_FONT: &str = F_BOLD;
/// Line step for it: 10.5 pt of bold ink is ~13 pt tall, so 13 pt per letter keeps them apart.
pub const FOLDED_LABEL_STEP: f32 = 13.0;

/// A family by its exact name — what `install_fonts` binds, and what a note face asks for by name.
/// It is deliberately *not* the chrome switch: a family name is a fact, `fam()` is a decision.
pub fn raw(name: &str) -> FontFamily {
    FontFamily::Name(name.into())
}

/// The chrome face a theme wears. WordCraft's is Carlito — the free Calibri-metric sans — so the
/// ribbon, the panels and the status bar read like the word processor it is modelled on; every
/// other theme keeps the house face, Chakra Petch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChromeFace {
    Chakra,
    Carlito,
}

impl ChromeFace {
    /// The four families this face is set in, in `F_REG` `F_MED` `F_SEMI` `F_BOLD` order.
    fn families(self) -> [(&'static str, &'static [u8]); 4] {
        match self {
            ChromeFace::Chakra => HOUSE,
            ChromeFace::Carlito => CARLITO,
        }
    }
}

/// The chrome face, a process-wide switch like the note face: `apply` writes it from the theme and
/// every existing `fam(..)` call site keeps working. `0` is the house face.
static CHROME_FACE: AtomicUsize = AtomicUsize::new(0);

pub fn set_chrome_face(face: ChromeFace) {
    CHROME_FACE.store(
        if face == ChromeFace::Carlito { 1 } else { 0 },
        Ordering::Relaxed,
    );
}

pub fn chrome_face() -> ChromeFace {
    if CHROME_FACE.load(Ordering::Relaxed) == 1 {
        ChromeFace::Carlito
    } else {
        ChromeFace::Chakra
    }
}

/// The chrome family for a role. Code keeps JetBrains Mono in every theme — `fam(F_MONO)` must not
/// follow the ribbon's face, or code stops lining up the moment the theme changes.
pub fn fam(role: &str) -> FontFamily {
    if role == F_MONO {
        return raw(F_MONO);
    }
    let i = match role {
        F_MED => 1,
        F_SEMI => 2,
        F_BOLD => 3,
        F_REG => 0,
        // not a weight role: a name the caller asked for by hand, left alone
        other => return raw(other),
    };
    raw(chrome_face().families()[i].0)
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
const INTER: [(&str, &[u8]); 4] = [
    ("Inter Reg", include_bytes!("../assets/Inter-Regular.ttf")),
    ("Inter Med", include_bytes!("../assets/Inter-Medium.ttf")),
    ("Inter Semi", include_bytes!("../assets/Inter-SemiBold.ttf")),
    ("Inter Bold", include_bytes!("../assets/Inter-Bold.ttf")),
];
const NUNITO: [(&str, &[u8]); 4] = [
    ("Nunito Reg", include_bytes!("../assets/Nunito-Regular.ttf")),
    ("Nunito Med", include_bytes!("../assets/Nunito-Medium.ttf")),
    ("Nunito Semi", include_bytes!("../assets/Nunito-SemiBold.ttf")),
    ("Nunito Bold", include_bytes!("../assets/Nunito-Bold.ttf")),
];
const MERRIWEATHER: [(&str, &[u8]); 4] = [
    ("Merriweather Reg", include_bytes!("../assets/Merriweather-Regular.ttf")),
    ("Merriweather Med", include_bytes!("../assets/Merriweather-Medium.ttf")),
    ("Merriweather Semi", include_bytes!("../assets/Merriweather-SemiBold.ttf")),
    ("Merriweather Bold", include_bytes!("../assets/Merriweather-Bold.ttf")),
];
const GARAMOND: [(&str, &[u8]); 4] = [
    ("Garamond Reg", include_bytes!("../assets/EBGaramond-Regular.ttf")),
    ("Garamond Med", include_bytes!("../assets/EBGaramond-Medium.ttf")),
    ("Garamond Semi", include_bytes!("../assets/EBGaramond-SemiBold.ttf")),
    ("Garamond Bold", include_bytes!("../assets/EBGaramond-Bold.ttf")),
];
const BITTER: [(&str, &[u8]); 4] = [
    ("Bitter Reg", include_bytes!("../assets/Bitter-Regular.ttf")),
    ("Bitter Med", include_bytes!("../assets/Bitter-Medium.ttf")),
    ("Bitter Semi", include_bytes!("../assets/Bitter-SemiBold.ttf")),
    ("Bitter Bold", include_bytes!("../assets/Bitter-Bold.ttf")),
];

/// One face a note can be set in, from the Display menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoteFont {
    /// Chakra Petch — the house face, and the default.
    House,
    /// Space Grotesk — a geometric sans with a techy cut.
    Grotesk,
    /// Inter — the neutral UI sans; a note in it reads like an app, not like a page.
    Inter,
    /// Nunito — a rounded sans, soft without being handwriting.
    Nunito,
    /// Fraunces — a display serif with some wonk in the curves.
    Fraunces,
    /// Literata — a book serif, cut for reading on screen.
    Literata,
    /// Merriweather — a screen serif with a large x-height; kind at long paragraphs.
    Merriweather,
    /// EB Garamond — an old-style serif with calligraphic roots.
    Garamond,
    /// Bitter — a slab serif, the sturdy middle ground between serif and mono.
    Bitter,
    /// Caveat — handwriting, for a note that is a scribble rather than a draft.
    Caveat,
    /// Carlito — Calibri's free metric twin; the WordCraft theme's own default.
    Carlito,
}

/// The picker's order: the house face first, then the alternatives, grouped sans → serif → hand.
pub const NOTE_FONTS: [NoteFont; 11] = [
    NoteFont::House,
    NoteFont::Grotesk,
    NoteFont::Inter,
    NoteFont::Nunito,
    NoteFont::Fraunces,
    NoteFont::Literata,
    NoteFont::Merriweather,
    NoteFont::Garamond,
    NoteFont::Bitter,
    NoteFont::Caveat,
    NoteFont::Carlito,
];

impl NoteFont {
    /// `state.json`'s spelling of the face.
    pub fn id(self) -> &'static str {
        match self {
            NoteFont::House => "house",
            NoteFont::Grotesk => "grotesk",
            NoteFont::Inter => "inter",
            NoteFont::Nunito => "nunito",
            NoteFont::Fraunces => "fraunces",
            NoteFont::Literata => "literata",
            NoteFont::Merriweather => "merriweather",
            NoteFont::Garamond => "garamond",
            NoteFont::Bitter => "bitter",
            NoteFont::Caveat => "caveat",
            NoteFont::Carlito => "carlito",
        }
    }

    /// The name the Display menu shows — set, in the menu, in the face it names.
    pub fn label(self) -> &'static str {
        match self {
            NoteFont::House => "Chakra Petch",
            NoteFont::Grotesk => "Space Grotesk",
            NoteFont::Inter => "Inter",
            NoteFont::Nunito => "Nunito",
            NoteFont::Fraunces => "Fraunces",
            NoteFont::Literata => "Literata",
            NoteFont::Merriweather => "Merriweather",
            NoteFont::Garamond => "EB Garamond",
            NoteFont::Bitter => "Bitter",
            NoteFont::Caveat => "Caveat",
            NoteFont::Carlito => "Carlito",
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
            NoteFont::Inter => INTER,
            NoteFont::Nunito => NUNITO,
            NoteFont::Fraunces => FRAUNCES,
            NoteFont::Literata => LITERATA,
            NoteFont::Merriweather => MERRIWEATHER,
            NoteFont::Garamond => GARAMOND,
            NoteFont::Bitter => BITTER,
            NoteFont::Caveat => CAVEAT,
            NoteFont::Carlito => CARLITO,
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
        raw(faces[i].0)
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
    // Emoji sit on the baseline as pictures, not letters: a hair under the type they follow reads as
    // one line. (egui's own copy of this font is scaled 0.81; the difference that mattered was the
    // subset, not the scale.) Inserted before the `add` closure exists, because a closure holding
    // `fonts` mutably is what makes the rest of this function read as one list.
    fonts.font_data.insert(
        "notoemoji".to_owned(),
        Arc::new(egui::FontData::from_static(NOTO_EMOJI).tweak(egui::FontTweak {
            scale: 0.94,
            ..Default::default()
        })),
    );
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(egui::FontData::from_static(bytes)));
    };
    add("jbmono", JETBRAINS);
    // The glyph fallbacks, registered once and shared by every family (see the constants).
    add("notosym2", NOTO_SYMBOLS2);
    add("notosym", NOTO_SYMBOLS);
    add("dejavu", DEJAVU);
    // both chrome faces, whichever one a theme happens to wear: a theme switch must never land on a
    // family the atlas has not bound (the note-face loop below covers them too; re-binding the same
    // key to the same bytes is a no-op and keeps this independent of NOTE_FONTS' contents)
    for face in [ChromeFace::Chakra, ChromeFace::Carlito] {
        for (key, bytes) in face.families() {
            add(key, bytes);
        }
    }

    // Keep egui's emoji/symbol faces as the tail of the fallback chain so glyphs outside a note font
    // (arrows, ticks, box drawing) still rasterize instead of showing tofu; the house regular sits in
    // front of them because Chakra Petch is the one face guaranteed to cover the app's Latin text.
    // The full symbol and emoji faces go *before* them: egui's Noto Emoji is a subset, and its
    // `emoji-icon-font` draws dozens of emoji as thin line art (a target that looks like a circle, a
    // snake that looks like a squiggle) — which is exactly what "the emoji are not right" meant. The
    // icon font only answers for characters nothing else has, and it stays for the few box-drawing
    // marks the app's own labels use.
    let fallback = [
        "notosym2",
        "notosym",
        "notoemoji",
        "dejavu",
        "NotoEmoji-Regular",
        "emoji-icon-font",
    ];

    for face in NOTE_FONTS {
        for (key, bytes) in face.faces() {
            add(key, bytes);
            let mut list = vec![key.to_owned()];
            if key != F_REG {
                list.push(F_REG.to_owned());
            }
            list.extend(fallback.iter().map(|s| s.to_string()));
            // `raw`, not `fam`: the chrome switch may be on Carlito by the time this runs, and a
            // family's own name is exactly what it is being bound to here.
            fonts.families.insert(raw(key), list);
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
    /// The face the chrome is set in for this theme (`fam()` reads it through the process switch).
    pub chrome: ChromeFace,
    /// Draw the note on a page: a white sheet with a hairline edge and a shadow, on the canvas
    /// colour. Word's document view — the one part of the look a palette alone cannot fake.
    pub sheet: bool,
    /// Corner radius for painted controls. Word's ribbon is nearly square; the house themes are
    /// deliberately rounder.
    pub radius: u8,
    /// The note face this theme opens a note in. The Display menu still overrides it for the
    /// session; picking a theme is picking its type.
    pub default_note: NoteFont,
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
    chrome: ChromeFace::Chakra,
    sheet: false,
    radius: 8,
    default_note: NoteFont::House,
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
    chrome: ChromeFace::Chakra,
    sheet: false,
    radius: 8,
    default_note: NoteFont::House,
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
    chrome: ChromeFace::Chakra,
    sheet: false,
    radius: 8,
    default_note: NoteFont::House,
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
    chrome: ChromeFace::Chakra,
    sheet: false,
    radius: 8,
    default_note: NoteFont::House,
};

/// WordCraft: the ribbon look of the word processor this app is modelled on, measured off its own
/// window rather than guessed — #F0F0F0 chrome bands, #F9F9F9 ribbon, #E6E6E6 canvas, a white page
/// and its #3B5BDB indigo. The indigo is deepened to #2F4BC4 because a tag or a due chip paints its
/// colour at 14 % behind its own text, and the raw #3B5BDB only manages 3.8:1 on that wash.
pub const WORDCRAFT: Theme = Theme {
    id: "wordcraft",
    label: "WordCraft",
    dark: false,
    bg: rgb(230, 230, 230),
    panel: rgb(240, 240, 240),
    panel2: rgb(249, 249, 249),
    card: rgb(255, 255, 255),
    card_hover: rgb(237, 237, 237),
    border: rgb(212, 212, 212),
    text: rgb(36, 36, 36),
    dim: rgb(92, 92, 92),
    faint: rgb(102, 102, 102),
    ghost: rgb(180, 180, 180),
    accent: rgb(47, 75, 196),
    accent2: rgb(11, 90, 158),
    heading: rgb(0, 0, 0),
    code_bg: rgb(204, 204, 204),
    done: rgb(14, 107, 58),
    warn: rgb(122, 73, 0),
    danger: rgb(176, 36, 24),
    marks: MARKS,
    mark_ink: MARK_INK,
    chrome: ChromeFace::Carlito,
    sheet: true,
    radius: 4,
    default_note: NoteFont::Carlito,
};

pub fn themes() -> [Theme; 5] {
    [VOID, EMBER, SLATE, PAPER, WORDCRAFT]
}

/// Apply a theme to the whole context. Called on startup and on theme change.
pub fn apply(ctx: &egui::Context, t: &Theme) {
    // The chrome face first: every text style below is built through `fam()`, which reads this
    // switch, so setting it after the styles would leave the bar in the previous theme's type.
    set_chrome_face(t.chrome);
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
    // Round for the house themes, near-square for WordCraft, whose ribbon buttons have a 2–3 pt cut
    v.window_corner_radius = CornerRadius::same(t.radius + 6);
    v.menu_corner_radius = CornerRadius::same(t.radius + 2);
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
        corner_radius: CornerRadius::same(t.radius),
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
    // A ribbon is denser than the house bar: WordCraft's small radius is also its density switch.
    let dense = t.radius <= 4;
    style.spacing.item_spacing = if dense {
        Vec2::new(6.0, 5.0)
    } else {
        Vec2::new(8.0, 7.0)
    };
    style.spacing.button_padding = if dense {
        Vec2::new(8.0, 3.0)
    } else {
        Vec2::new(10.0, 5.0)
    };
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
        p.rect_filled(rect, CornerRadius::same(t.radius), t.card_hover);
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
        ui.painter()
            .rect_filled(rect, CornerRadius::same(t.radius), t.card_hover);
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
        p.rect_filled(rect, CornerRadius::same(t.radius), t.accent.gamma_multiply(0.16));
        p.rect_filled(
            egui::Rect::from_min_size(rect.min + Vec2::new(0.0, 5.0), Vec2::new(2.5, rect.height() - 10.0)),
            CornerRadius::same(2),
            t.accent,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(t.radius), t.card_hover);
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

/// A row in the rail: the name wrapped at word boundaries over as many lines as it needs, and its
/// count on the right.
///
/// `nav_row` sets its label on one line and, when the name is too long, lays out a *wrapped* galley in
/// a 27 pt row — text spilling onto the row beneath it, which is worse than the clipping it was meant
/// to avoid. This one measures first and allocates the height it needs, the way `page_row` does for
/// the page list: a long notebook name makes a taller row instead of a shorter name.
pub fn rail_row(
    ui: &mut Ui,
    label: &str,
    selected: bool,
    badge: Option<String>,
    indent: f32,
    t: &Theme,
) -> Response {
    let width = ui.available_width();
    let badge_g = badge.map(|b| {
        ui.fonts_mut(|f| f.layout_no_wrap(b, FontId::new(11.0, fam(F_MONO)), t.faint))
    });
    let badge_w = badge_g.as_ref().map(|g| g.size().x + 10.0).unwrap_or(0.0);
    // `t.text`, not `t.dim`: the page list sets its titles in `t.text` and those are what these are
    // being matched to. A dim name in the same face still reads a weight lighter than a bright one —
    // anti-aliasing on a dark panel thins every stroke — so the weight alone did not make them twins.
    let text_color = if selected { t.heading } else { t.text };
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = (width - 18.0 - indent - badge_w).max(24.0);
    job.wrap.break_anywhere = false;
    job.append(
        label,
        0.0,
        egui::TextFormat {
            // Semi at every weight, like the page list's titles: the rail's names are headings for the
            // lists beside them, not the body text they used to be set as.
            font_id: FontId::new(15.5, fam(F_SEMI)),
            color: text_color,
            ..Default::default()
        },
    );
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    // measured before the galley is handed to the painter, which takes it by value
    let text_h = galley.size().y;
    let first_line = galley
        .rows
        .first()
        .map(|r| r.height())
        .unwrap_or(text_h);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, text_h + 12.0), Sense::click());
    {
        let p = ui.painter();
        if selected {
            p.rect_filled(rect, CornerRadius::same(t.radius), t.accent.gamma_multiply(0.16));
            p.rect_filled(
                egui::Rect::from_min_size(
                    rect.min + Vec2::new(0.0, 5.0),
                    Vec2::new(2.5, rect.height() - 10.0),
                ),
                CornerRadius::same(2),
                t.accent,
            );
        } else if resp.hovered() {
            p.rect_filled(rect, CornerRadius::same(t.radius), t.card_hover);
        }
    }
    let text_color = if selected { t.heading } else { t.text };
    let at = egui::pos2(rect.min.x + 10.0 + indent, rect.center().y - text_h / 2.0);
    ui.painter().galley(at, galley, text_color);
    if let Some(g) = badge_g {
        // Level with the *first* line, not the middle of the block: beside a four-line notebook name a
        // centred count floats away from the name it belongs to.
        ui.painter().galley(
            egui::pos2(
                rect.max.x - g.size().x - 8.0,
                rect.min.y + 6.0 + (first_line - g.size().y) * 0.5,
            ),
            g,
            t.faint,
        );
    }
    resp
}

/// A small header above a group of rows — `Notebooks`, `Chapters`, `Pages`, `Commands`.
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
    let font = FontId::new(BAR_FONT, fam(F_MED));
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), t.text));
    let size = Vec2::new(galley.size().x + 20.0, BAR_BTN_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, CornerRadius::same(t.radius), t.accent.gamma_multiply(0.20));
        p.rect_stroke(
            rect,
            CornerRadius::same(t.radius),
            Stroke::new(1.0_f32, t.accent.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(t.radius), t.card_hover);
    }
    let color = if active {
        t.accent
    } else if resp.hovered() {
        t.heading
    } else {
        t.dim
    };
    let g = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), color));
    p.galley(
        egui::Pos2::new(rect.min.x + 10.0, rect.center().y - g.size().y * 0.5),
        g,
        color,
    );
    resp
}

/// The little glyph at the left of a ribbon label.
///
/// WordCraft draws every ribbon command as line art — a clipboard before "Paste", scissors before
/// "Cut" — and the picture is what makes a row of words into a toolbar. These are drawn, not images:
/// one stroke width, the theme's own ink, and a fixed box so every label in the row starts at the
/// same offset. Letters (H, B, I, S, </>) are set in the chrome face, because a hand-drawn "B" looks
/// like a bug and Word uses the letter too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Paste,
    Cut,
    Copy,
    Attach,
    Snip,
    Heading,
    Task,
    Bullet,
    Plain,
    Bold,
    Italic,
    Code,
    Strike,
    /// The two ends of the find walk: a chevron each, the same marks as the `‹ ›` in the status bar's
    /// find strip, so the ribbon offers the step without a key.
    FindPrev,
    FindNext,
    /// Symbols and emoji: a smiling face, which is what the picker is for (the smiley is a glyph, not
    /// a drawing — Chakra Petch has none, so it comes from the emoji fallback every family ends with).
    Symbol,
}

impl Icon {
    /// The ones that are a letter rather than a drawing.
    ///
    /// `</>` is **not** here: three characters of text are ~25 pt wide at the size the other letters
    /// are set, which made its button 34 pt in a row of 28 pt squares. It is drawn instead (two
    /// chevrons and a slash), which is square by construction and reads the same at any size.
    fn glyph(self) -> Option<&'static str> {
        match self {
            Icon::Heading => Some("H"),
            Icon::Bold => Some("B"),
            Icon::Italic => Some("I"),
            Icon::Strike => Some("S"),
            Icon::Symbol => Some("\u{263A}"),
            _ => None,
        }
    }
}

/// The square a ribbon icon is drawn in, and the air around it inside the button.
///
/// 19, not 13: the ribbon's buttons carry no label any more, so the picture is the whole command
/// and it has to read at arm's length. `BAR_BTN_PAD` is set so the button stays the square
/// `BAR_ICON` + 2 × 4.5 = `BAR_BTN_H`.
pub const BAR_ICON: f32 = 19.0;
/// The box the icon paths below are *written* in. `paint_icon` scales that box to `BAR_ICON`, so a
/// bigger `BAR_ICON` enlarges the drawing instead of leaving a small picture in a big frame.
const BAR_ICON_UNIT: f32 = 13.0;
const BAR_BTN_PAD: f32 = 4.5;

/// A half-turn of the paperclip's wire, in the icon box's own units: `(cx, cy)` is the centre of the
/// turn, `from`/`to` are radians with y pointing *down*. Eight chords per half-circle are well under a
/// pixel of error at `BAR_ICON`, and the shape stays one polyline (a stroked curve would need a
/// different colour per segment, which `Shape::line` gives us for free).
fn icon_turn(cx: f32, cy: f32, r: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
    const STEPS: usize = 8;
    (0..=STEPS)
        .map(|i| {
            let a = from + (to - from) * i as f32 / STEPS as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// Paint one icon into `rect` (a `BAR_ICON` square, already centred on the button).
pub fn paint_icon(p: &egui::Painter, rect: egui::Rect, icon: Icon, colour: Color32, hot: Color32) {
    // Everything below is written in a `BAR_ICON_UNIT` box and scaled to the rect it is given, so a
    // larger icon is the same drawing larger — strokes included, or a bigger icon reads as a
    // lighter one instead of as a bigger one.
    let sc = rect.width() / BAR_ICON_UNIT;
    let sw = |w: f32, c: Color32| Stroke::new(w * sc, c);
    let s = sw(1.15, colour);
    let a = rect.min;
    let x = |fx: f32| a.x + fx * rect.width() / BAR_ICON_UNIT;
    let y = |fy: f32| a.y + fy * rect.height() / BAR_ICON_UNIT;
    let pt = |fx: f32, fy: f32| egui::pos2(x(fx), y(fy));
    match icon {
        // a sheet with a clip across its top
        Icon::Paste => {
            p.rect_stroke(
                egui::Rect::from_min_max(pt(2.0, 3.0), pt(11.0, 12.2)),
                CornerRadius::same(1),
                s,
                StrokeKind::Inside,
            );
            // filled, not stroked: a stroked tab merged with the sheet's own top edge and the
            // whole icon read as an empty box. Filled in the accent, the way WordCraft paints the
            // page on its clipboard — one coloured detail is what makes a row of icons look drawn
            // rather than printed.
            p.rect_filled(
                egui::Rect::from_min_max(pt(4.0, 1.2), pt(9.0, 4.2)),
                CornerRadius::same(1),
                hot,
            );
        }
        // paperclip: the real one, drawn as one wire — long run down the right, a half-turn along the
        // bottom, up the middle, a half-turn over the top, and a short run down the left that ends
        // free. Two rounded turns are what distinguishes a clip from the folded box that read as a
        // microphone; three parallel runs spaced 3 units apart are what keep it from reading as a
        // spring at 19 pt. The free end is the coloured detail, as one part of every icon is.
        Icon::Attach => {
            // A Gem clip, one wire: down the right, a wide turn round the bottom, up the left, over
            // the *top* and back down the middle, where it ends free. That combination — a big outer
            // U opening upward and a smaller inner loop turning the other way — is the whole of what
            // makes a paperclip a paperclip, and it is the only version of the four tried here that
            // survives at 19 pt. What failed, in order: a folded box (a microphone), two full-width
            // turns (a squiggle, "nU"), and the same wire tilted 45° (a paperclip in a vector view,
            // grey mush in the bar — thin diagonals anti-alias into nothing at this size). Axis
            // aligned, three strands 3.6 units apart, 2 px of stroke, and the inner end in the
            // accent. Every candidate was compared at 1:1 before this one was chosen.
            const PI: f32 = std::f32::consts::PI;
            const LEFT: f32 = 3.6;
            const RIGHT: f32 = 10.8;
            const BOT: f32 = 9.2;
            const TOP: f32 = 3.2;
            const R_IN: f32 = 1.8;
            let mut wire = vec![pt(RIGHT, 1.6), pt(RIGHT, BOT)];
            for (x, y) in icon_turn((LEFT + RIGHT) * 0.5, BOT, (RIGHT - LEFT) * 0.5, 0.0, PI) {
                wire.push(pt(x, y));
            }
            wire.push(pt(LEFT, TOP));
            for (x, y) in icon_turn(LEFT + R_IN, TOP, R_IN, PI, 2.0 * PI) {
                wire.push(pt(x, y));
            }
            wire.push(pt(LEFT + 2.0 * R_IN, 8.2));
            p.add(egui::Shape::line(wire, sw(1.35, colour)));
            // the inner end in the accent, starting a hair past the turn so the break is clean
            p.line_segment([pt(LEFT + 2.0 * R_IN, 5.0), pt(LEFT + 2.0 * R_IN, 8.2)], sw(1.35, hot));
        }
        // scissors: two blades crossing over two rings
        Icon::Cut => {
            p.line_segment([pt(3.0, 1.2), pt(9.4, 8.8)], s);
            p.line_segment([pt(10.0, 1.2), pt(3.6, 8.8)], s);
            p.circle_stroke(pt(3.2, 10.6), 1.7 * sc, sw(1.15, hot));
            p.circle_stroke(pt(9.8, 10.6), 1.7 * sc, sw(1.15, hot));
        }
        // two sheets, the back one offset and filled: copy
        Icon::Copy => {
            p.rect_filled(
                egui::Rect::from_min_max(pt(1.2, 1.2), pt(8.4, 9.4)),
                CornerRadius::same(1),
                hot.gamma_multiply(0.62),
            );
            p.rect_stroke(
                egui::Rect::from_min_max(pt(1.2, 1.2), pt(8.4, 9.4)),
                CornerRadius::same(1),
                s,
                StrokeKind::Inside,
            );
            p.rect_stroke(
                egui::Rect::from_min_max(pt(4.6, 3.8), pt(11.8, 12.0)),
                CornerRadius::same(1),
                s,
                StrokeKind::Inside,
            );
        }
        // a crop frame with its corner marks: what a snip takes
        Icon::Snip => {
            let (x0, y0, x1, y1) = (2.0_f32, 2.0_f32, 11.0_f32, 11.0_f32);
            p.line_segment([pt(x0, y0), pt(x0 + 3.0, y0)], s);
            p.line_segment([pt(x0, y0), pt(x0, y0 + 3.0)], s);
            p.line_segment([pt(x1, y0), pt(x1 - 3.0, y0)], s);
            p.line_segment([pt(x1, y0), pt(x1, y0 + 3.0)], s);
            p.line_segment([pt(x0, y1), pt(x0 + 3.0, y1)], s);
            p.line_segment([pt(x0, y1), pt(x0, y1 - 3.0)], s);
            p.line_segment([pt(x1, y1), pt(x1 - 3.0, y1)], s);
            p.line_segment([pt(x1, y1), pt(x1, y1 - 3.0)], s);
            // the frame's own centre, so the snip reads as a region and not as a broken box
            p.rect_stroke(
                egui::Rect::from_min_max(pt(4.6, 4.6), pt(8.4, 8.4)),
                CornerRadius::same(1),
                sw(1.0, hot),
                StrokeKind::Inside,
            );
        }
        // a ticked box
        Icon::Task => {
            p.rect_stroke(
                egui::Rect::from_min_max(pt(1.6, 1.6), pt(11.4, 11.4)),
                CornerRadius::same(2),
                s,
                StrokeKind::Inside,
            );
            p.add(egui::Shape::line(
                vec![pt(3.8, 6.4), pt(5.8, 8.6), pt(9.4, 3.9)],
                sw(1.6, hot),
            ));
        }
        // three dots and three lines
        Icon::Bullet => {
            for i in 0..3 {
                let fy = 2.6 + i as f32 * 3.9;
                p.circle_filled(pt(1.9, fy), 1.1 * sc, hot);
                p.line_segment([pt(4.6, fy), pt(11.8, fy)], s);
            }
        }
        // the same three lines, no dots: the line as it is written
        // Previous / next match: one chevron each, pointing the way the walk goes. A magnifier with a
        // chevron inside it was tried first — at 19 pt the two shapes merge into a blob, and the
        // chevron alone is the same mark the status bar's find strip uses.
        Icon::FindPrev => {
            p.add(egui::Shape::line(
                vec![pt(8.4, 2.4), pt(3.6, 6.5), pt(8.4, 10.6)],
                sw(1.5, colour),
            ));
        }
        Icon::FindNext => {
            p.add(egui::Shape::line(
                vec![pt(4.6, 2.4), pt(9.4, 6.5), pt(4.6, 10.6)],
                sw(1.5, colour),
            ));
        }
        Icon::Plain => {
            for i in 0..3 {
                let fy = 2.6 + i as f32 * 3.9;
                p.line_segment([pt(1.2, fy), pt(11.8, fy)], s);
            }
        }
        // the code mark: two chevrons and a slash — the slash is the one coloured part, the same
        // rule the other icons follow
        Icon::Code => {
            // The chevrons are wide apart and full height, and the slash is the *same weight* as
            // them: at 1.5 units in a 13-unit box, with the chevrons 2.6 apart, the slash filled the
            // gap and the mark read as a solid diamond (caught by upscaling the real button 6×).
            p.add(egui::Shape::line(
                vec![pt(4.6, 3.2), pt(1.6, 6.5), pt(4.6, 9.8)],
                s,
            ));
            p.add(egui::Shape::line(
                vec![pt(8.4, 3.2), pt(11.4, 6.5), pt(8.4, 9.8)],
                s,
            ));
            p.add(egui::Shape::line(
                vec![pt(8.3, 2.5), pt(4.9, 10.5)],
                sw(1.15, hot),
            ));
        }
        // the letters (Heading, Bold, Italic, Strike) never reach here
        _ => {}
    }
}

/// [toolbar_button] as a picture: the ribbon's command button.
///
/// **Icon only — the label is the tooltip, not the face of the button.** A labelled row needed
/// ~1050 pt for the Pages tab and dropped whatever did not fit, so on a portrait window the
/// highlighter swatches were simply not there. Square pictures need 740 pt and fit. The group
/// captions under them still name each cluster, and hovering says what the picture means.
///
/// The dialog buttons keep `toolbar_button` — an icon before "Cancel" would be decoration.
pub fn toolbar_icon_button(ui: &mut Ui, icon: Icon, tip: &str, active: bool, t: &Theme) -> Response {
    // Letters (H, B, I, S) are set in the chrome face, as Word sets them: a hand-drawn "B" reads as a
    // bug. They are sized off the icon box, so they grow with the drawings, and every one of them is
    // narrower than that box — which is what keeps every button in the row the same square.
    let glyph_font = FontId::new(BAR_ICON - 1.5, fam(F_SEMI));
    let glyph = icon.glyph().map(|g| {
        ui.fonts_mut(|f| f.layout_no_wrap(g.to_owned(), glyph_font.clone(), t.text))
    });
    let icon_w = glyph
        .as_ref()
        .map(|g| g.size().x.max(BAR_ICON))
        .unwrap_or(BAR_ICON);
    // `BAR_BTN_PAD` is half the difference between the icon box and the button height, so every
    // picture button is a square of `BAR_BTN_H` — a row of them reads as a toolbar, a row of
    // ragged boxes does not.
    let size = Vec2::new(BAR_BTN_PAD * 2.0 + icon_w, BAR_BTN_H);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    {
        let p = ui.painter();
        if active {
            p.rect_filled(rect, CornerRadius::same(t.radius), t.accent.gamma_multiply(0.20));
            p.rect_stroke(
                rect,
                CornerRadius::same(t.radius),
                Stroke::new(1.0_f32, t.accent.gamma_multiply(0.6)),
                StrokeKind::Inside,
            );
        } else if resp.hovered() {
            p.rect_filled(rect, CornerRadius::same(t.radius), t.card_hover);
        }
    }
    let color = if active {
        t.accent
    } else if resp.hovered() {
        t.heading
    } else {
        t.dim
    };
    // WordCraft's trick, kept to one detail per icon: the outline is ink, one filled part is
    // colour. Which colour is a property of the command — a tick is green whether or not the
    // theme's accent is, and a heading takes the accent because that *is* the structure colour.
    let hot = match icon {
        Icon::Task => t.done,
        Icon::Strike => t.warn,
        _ => t.accent,
    };
    let icon_rect = egui::Rect::from_center_size(rect.center(), Vec2::splat(BAR_ICON));
    match &glyph {
        Some(g) => {
            let glyph_ink = match icon {
                Icon::Heading | Icon::Code => hot,
                _ => color,
            };
            let g = ui.fonts_mut(|f| {
                f.layout_no_wrap(g.text().to_owned(), glyph_font.clone(), glyph_ink)
            });
            let at = egui::pos2(
                icon_rect.center().x - g.size().x * 0.5,
                icon_rect.center().y - g.size().y * 0.5,
            );
            let width = g.size().x;
            let p = ui.painter();
            p.galley(at, g, glyph_ink);
            // a strikethrough is an S with a line through it, exactly as Word draws it
            if icon == Icon::Strike {
                // sized to the letter, not to the icon box: a line wider than the S read as a dash
                // that happened to sit behind a letter
                p.line_segment(
                    [
                        egui::pos2(at.x - 0.5, icon_rect.center().y),
                        egui::pos2(at.x + width + 0.5, icon_rect.center().y),
                    ],
                    Stroke::new(1.4_f32, hot),
                );
            }
        }
        None => paint_icon(ui.painter(), icon_rect, icon, color, hot),
    }
    // The words are still here, one hover away: a picture toolbar without tooltips is a puzzle.
    resp.on_hover_text(tip)
}

/// A scope segment: [toolbar_button] with the inactive labels at body-text brightness.
///
/// The scope toggle is the primary control of its row, not a secondary action tucked into a
/// toolbar, and `dim` made the two unchosen scopes read as a caption rather than as buttons.
pub fn segment_button(ui: &mut Ui, label: &str, active: bool, t: &Theme) -> Response {
    let font = FontId::new(13.0, fam(F_MED));
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), t.text));
    let pad = if t.radius <= 4 {
        Vec2::new(14.0, 8.0)
    } else {
        Vec2::new(18.0, 9.0)
    };
    let size = galley.size() + pad;
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if active {
        p.rect_filled(rect, CornerRadius::same(t.radius), t.accent.gamma_multiply(0.20));
        p.rect_stroke(
            rect,
            CornerRadius::same(t.radius),
            Stroke::new(1.0_f32, t.accent.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(t.radius), t.card_hover);
    }
    let color = if active {
        t.accent
    } else if resp.hovered() {
        t.heading
    } else {
        t.text
    };
    let g = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), color));
    p.galley(rect.min + pad * 0.5, g, color);
    resp
}

/// A ribbon tab: the label, and — when it is the one you are on — a painted accent underline along
/// the bottom of the tab, which is how Word says "you are here". No fill on the active tab: the
/// underline is the mark, and a tinted box would read as a button sitting in a row of tabs.
pub fn ribbon_tab(ui: &mut Ui, label: &str, active: bool, t: &Theme, height: f32) -> Response {
    // A menu label is a name, not a caption: one step up from the ribbon's own labels and set in
    // the bold face, which is what WordCraft's menu line is. `BAR_FONT + 1.0` here matches the
    // ribbon buttons, so the two rows read as one bar.
    let font = FontId::new(BAR_FONT + 1.0, fam(F_BOLD));
    let galley = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), t.text));
    // the same 10 pt side padding a painted button uses, so the tab strip's first label lines up
    // with the ribbon's first label instead of starting 4 pt further right
    let size = Vec2::new(galley.size().x + 20.0, height);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if resp.hovered() && !active {
        p.rect_filled(
            rect,
            CornerRadius {
                nw: t.radius,
                ne: t.radius,
                sw: 0,
                se: 0,
            },
            t.card_hover,
        );
    }
    // WordCraft marks the current tab with the accent colour *and* the underline; only the underline
    // made it read as a hover state rather than a tab.
    let color = if active {
        t.accent
    } else if resp.hovered() {
        t.text
    } else {
        t.dim
    };
    let g = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font, color));
    p.galley(
        egui::Pos2::new(
            rect.center().x - g.size().x * 0.5,
            rect.center().y - g.size().y * 0.5,
        ),
        g,
        color,
    );
    if active {
        // the mark runs along the band's bottom edge, which is where Word puts it: the tab is as
        // tall as the ribbon, so the underline lands under the label and not through the middle
        p.rect_filled(
            egui::Rect::from_min_size(
                egui::Pos2::new(rect.min.x, rect.max.y - 2.25),
                Vec2::new(rect.width(), 2.25),
            ),
            CornerRadius::same(0),
            t.accent,
        );
    }
    resp
}

/// One ribbon group: the controls, then the caption Word puts under them (`Clipboard`, `Font`, …).
///
/// The caption is measured against the controls actually laid out, not against the width the group
/// was given: a group holding one short button must not centre its caption in a field of empty
/// space, which is what `vertical_centered` over the full row would do.
pub fn ribbon_group(ui: &mut Ui, caption: &str, t: &Theme, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        // every group is the same two rows: the controls on the bar's button height, then the
        // caption — so captions across the band sit on one line whatever the group holds
        ui.spacing_mut().item_spacing = Vec2::new(BAR_IN_GROUP_GAP, 0.0);
        ui.add_space((BAR_CONTENT_H - BAR_BTN_H - BAR_CAPTION_H) * 0.5);
        add(ui);
        let width = ui.min_rect().width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, BAR_CAPTION_H), Sense::hover());
        let g = ui.fonts_mut(|f| {
            f.layout_no_wrap(caption.to_owned(), FontId::new(BAR_SMALL, fam(F_MONO)), t.faint)
        });
        ui.painter().galley(
            egui::Pos2::new(
                rect.center().x - g.size().x * 0.5,
                rect.center().y - g.size().y * 0.5,
            ),
            g,
            t.faint,
        );
    });
}

/// The rule between two ribbon groups, with its air.
///
/// Ten buttons of equal weight, mixing modes, views, actions and settings, read as one
/// undifferentiated strip; the rule says where one job ends. It is painted rather than
/// `ui.separator()` because egui's own rule spans the whole interact height and takes its colour
/// from `Visuals`, which on Paper is very nearly the panel itself.
///
/// The air either side is part of the *one* allocation, not two `add_space` calls around it: the
/// row's own item spacing (6 pt) lands on each side of every item, so the old three-item version
/// cost 25 pt for a 1 pt rule. One allocation costs `2 × side + 1` = 13 and the visible gaps are the
/// same `BAR_GROUP_GAP`. Five rules at 25 pt were 60 pt of the portrait budget.
pub fn group_rule(ui: &mut Ui, t: &Theme) {
    let side = (BAR_GROUP_GAP - BAR_ITEM_GAP).max(0.0);
    let (r, _) = ui.allocate_exact_size(
        Vec2::new(1.0 + side * 2.0, BAR_CONTENT_H - 14.0),
        Sense::hover(),
    );
    let bar = egui::Rect::from_center_size(r.center(), Vec2::new(1.0, r.height()));
    ui.painter().rect_filled(bar, CornerRadius::same(1), t.border);
}

/// A menu on the ribbon: the same label metrics as a painted button, so "Display" and "Export" sit
/// in the same voice as "Paste" instead of being the only 14 pt boxes in the band.
pub fn ribbon_menu(ui: &mut Ui, label: &str, t: &Theme, close_on_click_outside: bool, add: impl FnOnce(&mut Ui)) {
    let text = egui::RichText::new(label)
        .family(fam(F_BOLD))
        .size(BAR_FONT + 1.0)
        .color(t.dim);
    let button = egui::Button::new(text).min_size(Vec2::new(0.0, BAR_BTN_H));
    let mut menu = egui::containers::menu::MenuButton::from_button(button);
    if close_on_click_outside {
        menu = menu.config(
            egui::containers::menu::MenuConfig::new()
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
        );
    }
    let _ = menu.ui(ui, add);
}

/// A row-1 control button: 24 pt square, so the theme switch and the search box share a height.
pub fn icon_button_bar(ui: &mut Ui, glyph: &str, tip: &str, t: &Theme) -> Response {
    debug_assert!(
        ICON_GLYPHS.contains(glyph),
        "glyph {glyph:?} has no face in the bundled fonts — it will render as a tofu box"
    );
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(BAR_CTRL_H), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(t.radius), t.card_hover);
    }
    let color = if resp.hovered() { t.heading } else { t.dim };
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        FontId::new(BAR_FONT, fam(F_MED)),
        color,
    );
    resp.on_hover_text(tip)
}

/// The page: a white sheet with a hairline edge and a soft shadow, drawn on the canvas colour.
///
/// This is Word's document view and the one piece of its look a palette cannot fake — a note that
/// bleeds to the edges of the window reads as a text field, not as a document. Only a theme with
/// `sheet` draws it; the other four keep the full-bleed canvas their design was measured against.
pub fn paint_page_sheet(ui: &Ui, rect: egui::Rect, t: &Theme) {
    let shadow = Shadow {
        offset: [0, 2],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(38),
    };
    let p = ui.painter();
    p.add(shadow.as_shape(rect, CornerRadius::same(1)));
    p.rect_filled(rect, CornerRadius::same(1), t.card);
    p.rect_stroke(
        rect,
        CornerRadius::same(1),
        Stroke::new(1.0_f32, t.border),
        StrokeKind::Inside,
    );
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
    fn every_light_text_token_clears_aa() {
        // Every theme drawn on a light background (Paper and WordCraft): a mid-tone accent or a
        // "quiet" hint colour that works on near-black is invisible here, and WordCraft's indigo was
        // measured off the real ribbon at 3.8:1 on its own chip wash before it was deepened.
        for t in themes().into_iter().filter(|t| !t.dark) {
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
                    assert!(r >= 4.5, "{}: {name} on {surface} is {r:.2}:1, under AA", t.label);
                }
            }
            // a tag/due run paints the colour over a 13–14 % wash of itself, and a chip does the same
            for (name, c) in [("accent", t.accent), ("warn", t.warn)] {
                let wash = over(c, 0.14, t.bg);
                let r = contrast(c, wash);
                assert!(
                    r >= 4.5,
                    "{}: {name} on its own chip wash is {r:.2}:1, under AA",
                    t.label
                );
            }
        }
    }

    /// WordCraft is the one theme drawn as a document: Carlito chrome, the page sheet, and a note
    /// face that belongs to it. A copy of its palette with the house type on top would be the
    /// halfway house this test exists to prevent.
    #[test]
    fn wordcraft_is_a_document_theme() {
        let sheeted: Vec<&str> = themes()
            .iter()
            .filter(|t| t.sheet)
            .map(|t| t.id)
            .collect();
        assert_eq!(sheeted, vec!["wordcraft"], "only WordCraft draws a page sheet");
        assert_eq!(WORDCRAFT.chrome, ChromeFace::Carlito);
        assert_eq!(WORDCRAFT.default_note, NoteFont::Carlito);
        // every theme's own default note face has to be one the picker offers, or the theme would
        // open a note in a face the Display menu cannot return to
        for t in themes() {
            assert!(
                NOTE_FONTS.contains(&t.default_note),
                "{}: default note face is not in NOTE_FONTS",
                t.label
            );
        }
        assert!(themes().iter().any(|t| t.id == "wordcraft"));
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
            assert_eq!(face.fam(F_REG), raw(keys[0]));
            assert_eq!(face.fam(F_MED), raw(keys[1]));
            assert_eq!(face.fam(F_SEMI), raw(keys[2]));
            assert_eq!(face.fam(F_BOLD), raw(keys[3]));
            assert_ne!(face.fam(F_REG), face.fam(F_BOLD), "{}", face.id());
        }
    }

    /// A `state.json` with no `note_font` (i.e. one written before the picker existed) has to draw
    /// exactly what the build before it drew: the house face *is* the chrome's own families.
    #[test]
    fn the_house_face_is_the_chrome() {
        // the chrome switch is process-wide, so pin it: a test that runs after another one has
        // applied the WordCraft theme would otherwise see Carlito here
        set_chrome_face(ChromeFace::Chakra);
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
                .get(&raw(key))
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

/// The ribbon's geometry, measured rather than estimated: a bar that does not fit its window is the
/// one bug here the eye only catches on someone else's monitor.
#[cfg(test)]
mod tests_bar {
    use super::*;

    /// Lay out the Pages command row exactly as `top_bar` does — the same groups, the same rules,
    /// the same gaps, the same order — and return the row's width. Nothing is estimated: the rules
    /// are `group_rule` itself, so the number this measures is the number the bar needs.
    fn pages_row_groups(t: &Theme) -> f32 {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let mut row = egui::Rect::NOTHING;
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 900.0),
        ));
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let r = ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(BAR_ITEM_GAP, 0.0);
                    ribbon_group(ui, "clipboard", t, |ui| {
                        ui.horizontal(|ui| {
                            for icon in [Icon::Cut, Icon::Copy, Icon::Paste] {
                                toolbar_icon_button(ui, icon, "cut", false, t);
                            }
                        });
                    });
                    group_rule(ui, t);
                    ribbon_group(ui, "insert", t, |ui| {
                        ui.horizontal(|ui| {
                            for icon in [Icon::Attach, Icon::Snip] {
                                toolbar_icon_button(ui, icon, "attach", false, t);
                            }
                        });
                    });
                    group_rule(ui, t);
                    ribbon_group(ui, "style", t, |ui| {
                        ui.horizontal(|ui| {
                            for icon in [Icon::Heading, Icon::Task, Icon::Bullet, Icon::Plain] {
                                toolbar_icon_button(ui, icon, "style", false, t);
                            }
                        });
                    });
                    group_rule(ui, t);
                    ribbon_group(ui, "text", t, |ui| {
                        ui.horizontal(|ui| {
                            for icon in [Icon::Bold, Icon::Italic, Icon::Code, Icon::Strike] {
                                toolbar_icon_button(ui, icon, "text", false, t);
                            }
                        });
                    });
                    group_rule(ui, t);
                    ribbon_group(ui, "size", t, |ui| {
                        ui.horizontal(|ui| {
                            for label in ["S", "M", "L"] {
                                toolbar_button(ui, label, false, t);
                            }
                        });
                    });
                    group_rule(ui, t);
                    ribbon_group(ui, "mark", t, |ui| {
                        let none = [false; 5];
                        ribbon_swatches(ui, &none, t);
                    });
                });
                row = r.response.rect;
            });
        });
        row.width()
    }

    /// The five highlighter swatches at the bar's own size (`ribbon_marks` in `app` draws these; the
    /// sizes are repeated here because that function also acts on clicks).
    fn ribbon_swatches(ui: &mut Ui, current: &[bool; 5], t: &Theme) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (i, _on) in current.iter().enumerate() {
                let colour = t.marks.get(i).copied().unwrap_or(t.faint);
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                ui.painter().rect_filled(rect, CornerRadius::same(3), colour);
            }
        });
    }

    /// The whole Pages row has to fit the width a portrait window offers. The labelled row needed
    /// ~1050 pt, so at 820 pt the bar dropped every group past `style` — including `mark`, the
    /// highlighter colours — and on a portrait monitor that is the width the user had. Pictures
    /// without labels bring the row to 741 pt, rules included, so nothing drops in a portrait window
    /// and the guards in `top_bar` are only there for a genuinely tiny one.
    #[test]
    fn the_pages_row_fits_a_portrait_window() {
        for t in themes() {
            let total = pages_row_groups(&t);
            assert!(
                total <= 780.0,
                "{}: the Pages row is {total:.0} pt, and an 820 pt window leaves 796",
                t.label
            );
            // a floor too: a layout that measured nothing would pass the ceiling above
            assert!(
                total > 600.0,
                "{}: the Pages row measured {total:.0} pt, which is too little to be the whole row",
                t.label
            );
        }
    }

    /// The command buttons are square and icon-only: a picture is the whole face of the button, and
    /// its words are the tooltip. A labelled button was 40–60 pt wide; a square one is `BAR_BTN_H`.
    #[test]
    fn a_command_button_is_a_square_picture() {
        let t = VOID;
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let mut sizes: Vec<Vec2> = Vec::new();
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 900.0),
        ));
        let _ = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for icon in [Icon::Cut, Icon::Attach, Icon::Heading, Icon::Bold, Icon::Code] {
                    let r = toolbar_icon_button(ui, icon, "tip", false, &t);
                    sizes.push(r.rect.size());
                }
            });
        });
        for s in sizes {
            assert_eq!(s.y, BAR_BTN_H, "a ribbon button is BAR_BTN_H tall");
            assert!(
                (s.x - BAR_BTN_H).abs() <= 6.0,
                "a ribbon button is a square, not {:.0} × {:.0}",
                s.x,
                s.y
            );
        }
    }

    /// The collapsed panel strips stack their label one letter per line. It has to be bold and a size
    /// up from the 9.5 pt mono it used to be (the user's own request): at 26 pt wide the strip can
    /// carry it, and a hairline of letters said nothing.
    #[test]
    fn the_folded_panel_label_is_bold_and_a_size_up() {
        assert!(FOLDED_LABEL_SIZE > BAR_SMALL, "the folded label must be bigger than a caption");
        assert_eq!(FOLDED_LABEL_SIZE, 10.5);
        assert_eq!(FOLDED_LABEL_FONT, F_BOLD);
    }
}
