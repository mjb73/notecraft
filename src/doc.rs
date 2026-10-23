//! A page as a **document**: blocks, and text runs with properties.
//!
//! The note is not text with markup. The app never sees or edits syntax — bold is a property of a
//! run, a heading is a block kind, a todo is a block with a `done` flag, a due date is a date — so
//! there is nothing that can leak, break, or land the caret inside. Markdown is only *read*, to
//! migrate the pages that already exist, and *written*, to check that migration is faithful and to
//! be able to export a page as plain text.

use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The file extension for a page stored as a document.
pub const EXT: &str = "note";

/// What kind of block a line of the note is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Para,
    /// `n` = 1..=6, the level.
    Heading(u8),
    Bullet,
    Todo,
    Quote,
    Code,
    /// A picture: `image` holds the reference, `runs[0]` its caption.
    Image,
    Rule,
}

/// Properties of one stretch of text inside a block.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Attrs {
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub strike: bool,
    #[serde(default)]
    pub code: bool,
    /// Link target, if this run is a link. The text is the label.
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub tag: bool,
    /// A due date, stored as a date — never as `@due(…)` text.
    #[serde(default)]
    pub due: Option<NaiveDate>,
    /// Text size step: `-1` small, `0` the block's own size, `+1` large. Relative, so it composes
    /// with a heading or a code block instead of fighting it.
    #[serde(default)]
    pub size: i8,
    /// Highlighter colour, 1-based into `theme::MARK_NAMES`; 0 is no mark. A marker is a property of
    /// the words, like bold — it never inserts characters, and markdown cannot express it (the HTML
    /// export and read mode can).
    #[serde(default)]
    pub mark: u8,
}

/// The multiplier a run's size step applies to its block's base size.
pub fn size_scale(step: i8) -> f32 {
    match step {
        i8::MIN..=-1 => 0.82,
        1..=i8::MAX => 1.32,
        _ => 1.0,
    }
}

/// A stretch of text with properties.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub text: String,
    #[serde(default)]
    pub a: Attrs,
}

impl Run {
    pub fn plain(text: impl Into<String>) -> Run {
        Run {
            text: text.into(),
            a: Attrs::default(),
        }
    }
}

/// A reversible crop: the part of a picture that is shown, as fractions of the source (0..1 each).
///
/// The file in `assets/` is never rewritten — a crop is four numbers on the image line, so it can be
/// adjusted again or taken off completely and the whole picture is still there. Fractions rather than
/// pixels so the crop survives the picture being replaced by a different resolution (an annotation, a
/// re-scan, a screenshot retaken at another size).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Crop {
    pub const FULL: Crop = Crop {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };

    /// The smallest crop the app will draw: an eighth of a side in each direction. Anything smaller is
    /// a handful of pixels once it reaches the screen, and a zero would make the picture vanish with
    /// no way to grab it again.
    pub const MIN: f32 = 0.02;

    /// Whether this is the whole picture (to within a thousandth of a pixel on each edge — a crop
    /// dragged back out by hand should still be taken off rather than kept as a no-op).
    pub fn is_full(self) -> bool {
        let near = |a: f32, b: f32| (a - b).abs() < 0.0005;
        near(self.x, 0.0) && near(self.y, 0.0) && near(self.w, 1.0) && near(self.h, 1.0)
    }

    /// Inside the picture, at least `MIN` on each side. A hand-edited or stale line can hold anything;
    /// this is the only place that decides what is drawable.
    pub fn clamped(self) -> Crop {
        let w = self.w.clamp(Crop::MIN, 1.0);
        let h = self.h.clamp(Crop::MIN, 1.0);
        Crop {
            x: self.x.clamp(0.0, 1.0 - w),
            y: self.y.clamp(0.0, 1.0 - h),
            w,
            h,
        }
    }

    /// The crop a pixel rect of a `src`-sized picture describes (`x, y, w, h` in source pixels).
    pub fn from_px(rect: (f32, f32, f32, f32), src: (f32, f32)) -> Crop {
        let (sx, sy) = (src.0.max(1.0), src.1.max(1.0));
        Crop {
            x: rect.0 / sx,
            y: rect.1 / sy,
            w: rect.2 / sx,
            h: rect.3 / sy,
        }
        .clamped()
    }

    /// The pixel rect this crop keeps, in the source's own pixels.
    pub fn px(self, src: (f32, f32)) -> (f32, f32, f32, f32) {
        let c = self.clamped();
        (
            c.x * src.0,
            c.y * src.1,
            c.w * src.0,
            c.h * src.1,
        )
    }

    /// The `crop="x,y,w,h"` attribute's value. Four decimals is a tenth of a pixel on a 1000-px
    /// picture: fine enough that a round trip does not drift, short enough to stay readable in a file.
    pub fn attr(self) -> String {
        let c = self.clamped();
        format!("{:.4},{:.4},{:.4},{:.4}", c.x, c.y, c.w, c.h)
    }

    /// Parse `crop="…"`. Anything malformed, out of range or degenerate is `None`, which means "the
    /// whole picture": a bad attribute must never make a picture unshowable.
    pub fn parse(v: &str) -> Option<Crop> {
        let parts: Vec<f32> = v
            .split(',')
            .map(|p| p.trim().parse::<f32>())
            .collect::<Result<_, _>>()
            .ok()?;
        if parts.len() != 4 || parts.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let c = Crop {
            x: parts[0],
            y: parts[1],
            w: parts[2],
            h: parts[3],
        }
        .clamped();
        (!c.is_full()).then_some(c)
    }
}

/// One block of a page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub kind: Kind,
    #[serde(default)]
    pub runs: Vec<Run>,
    /// Todo: ticked off.
    #[serde(default)]
    pub done: bool,
    /// Image: the asset reference (`assets/x.png`).
    #[serde(default)]
    pub image: String,
    /// Image: an explicit display width in points, once the user resizes it.
    #[serde(default)]
    pub width: Option<u32>,
    /// Image: the part of the file that is shown. `None` is the whole picture, which is also what a
    /// file written before cropping existed means.
    #[serde(default)]
    pub crop: Option<Crop>,
}

impl Block {
    pub fn new(kind: Kind) -> Block {
        Block {
            kind,
            runs: Vec::new(),
            done: false,
            image: String::new(),
            width: None,
            crop: None,
        }
    }

    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }

    pub fn plain(kind: Kind, text: impl Into<String>) -> Block {
        let mut b = Block::new(kind);
        b.runs.push(Run::plain(text));
        b
    }

}

/// A whole page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Doc {
    pub blocks: Vec<Block>,
}

impl Doc {
    /// A document always holds at least one block. The editor draws one `TextEdit` per block, so an
    /// empty document is a page with no caret and nowhere for keystrokes to land — a brand-new page
    /// was exactly that. An empty block exports as nothing, so the markdown stays empty too.
    pub fn ensure_block(&mut self) {
        if self.blocks.is_empty() {
            self.blocks.push(Block::new(Kind::Para));
        }
    }
}

// ---------------------------------------------------------------- markdown in

/// Parse `**bold**`, `*italic*`, `` `code` ``, `~~struck~~`, `[label](url)`, `[[wiki]]`, `#tag`,
/// `@due(2026-10-24)` / `@today` / `@tomorrow` into runs. Only used to bring existing pages over.
pub fn parse_inline(s: &str, base: Attrs) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut run = 0usize;
    let push = |out: &mut Vec<Run>, text: &str, a: Attrs| {
        if text.is_empty() {
            return;
        }
        // merge into the previous run when the properties match, so runs stay tidy
        if let Some(last) = out.last_mut() {
            if last.a == a {
                last.text.push_str(text);
                return;
            }
        }
        out.push(Run {
            text: text.to_string(),
            a,
        });
    };
    while i < b.len() {
        let rest = &s[i..];
        let mut matched = false;
        // inline code
        if b[i] == b'`' {
            if let Some(rel) = rest[1..].find('`') {
                let end = i + 1 + rel;
                push(&mut out, &s[run..i], base.clone());
                push(
                    &mut out,
                    &s[i + 1..end],
                    Attrs {
                        code: true,
                        ..base.clone()
                    },
                );
                i = end + 1;
                run = i;
                matched = true;
            }
        }
        // bold / italic / strikethrough
        if !matched {
            for (m, set) in [
                ("**", 0usize),
                ("__", 0),
                ("~~", 2),
                ("*", 1),
                ("_", 1),
            ] {
                if !rest.starts_with(m) {
                    continue;
                }
                // a single `*` right after another `*` is the tail of a bold marker
                if m.len() == 1 && (i > 0 && b[i - 1] == b[i]) {
                    continue;
                }
                if let Some(rel) = rest[m.len()..].find(m) {
                    let end = i + m.len() + rel;
                    if end > i + m.len() {
                        let mut a = base.clone();
                        match set {
                            0 => a.bold = true,
                            1 => a.italic = true,
                            _ => a.strike = true,
                        }
                        push(&mut out, &s[run..i], base.clone());
                        for r in parse_inline(&s[i + m.len()..end], a) {
                            push(&mut out, &r.text, r.a);
                        }
                        i = end + m.len();
                        run = i;
                        matched = true;
                        break;
                    }
                }
            }
        }
        // markdown link, wiki link, inline image
        if !matched && b[i] == b'[' {
            if rest.starts_with("[[") {
                if let Some(rel) = rest[2..].find("]]") {
                    let end = i + 2 + rel;
                    push(&mut out, &s[run..i], base.clone());
                    push(
                        &mut out,
                        &s[i + 2..end],
                        Attrs {
                            link: Some(format!("[[{}]]", &s[i + 2..end])),
                            ..base.clone()
                        },
                    );
                    i = end + 2;
                    run = i;
                    matched = true;
                }
            } else if let Some(rel) = rest[1..].find("](") {
                let close = i + 1 + rel;
                if let Some(rel2) = s[close + 2..].find(')') {
                    let end = close + 2 + rel2;
                    push(&mut out, &s[run..i], base.clone());
                    push(
                        &mut out,
                        &s[i + 1..close],
                        Attrs {
                            link: Some(s[close + 2..end].to_string()),
                            ..base.clone()
                        },
                    );
                    i = end + 1;
                    run = i;
                    matched = true;
                }
            }
        }
        // inline picture: `![alt](ref)`. The '![' must be followed by '](' **and** a closing
        // ')' — a line that has `![alt]` and no target is ordinary text, not a broken image, and
        // slicing it as one panicked ("byte range starts at 18 but ends at 15"), which took the
        // whole app down on open.
        if !matched && rest.starts_with("![") {
            if let Some(close) = rest.find("](") {
                if let Some(rel) = s[i + close + 2..].find(')') {
                    let end = i + close + 2 + rel;
                    push(&mut out, &s[run..i], base.clone());
                    push(
                        &mut out,
                        &s[i + 2..i + close],
                        Attrs {
                            // the reference sits between the '(' and the ')'
                            link: Some(s[i + close + 2..end].to_string()),
                            ..base.clone()
                        },
                    );
                    i = end + 1;
                    run = i;
                    matched = true;
                }
            }
        }
        // @due(…), @today, @tomorrow
        if !matched && b[i] == b'@' {
            let (due, end) = if rest.starts_with("@due(") {
                let inner_end = rest.find(')').map(|r| i + r).unwrap_or(i + 1);
                let date = rest
                    .get(5..inner_end - i)
                    .and_then(|d| NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").ok());
                (date, inner_end + 1)
            } else if rest.starts_with("@today") {
                (Some(Local::now().date_naive()), i + 6)
            } else if rest.starts_with("@tomorrow") {
                (Some(Local::now().date_naive() + chrono::Duration::days(1)), i + 9)
            } else {
                (None, i + 1)
            };
            if end > i + 1 {
                push(&mut out, &s[run..i], base.clone());
                if let Some(d) = due {
                    push(
                        &mut out,
                        &d.format("%d %b %Y").to_string(),
                        Attrs {
                            due: Some(d),
                            ..base.clone()
                        },
                    );
                }
                i = end;
                run = i;
                matched = true;
            }
        }
        // #tag (at the start of a word)
        if !matched && b[i] == b'#' && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
            let n = rest[1..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '/')
                .count();
            if n > 0 {
                push(&mut out, &s[run..i], base.clone());
                push(
                    &mut out,
                    &s[i..i + 1 + n],
                    Attrs {
                        tag: true,
                        ..base.clone()
                    },
                );
                i += 1 + n;
                run = i;
                matched = true;
            }
        }
        if !matched {
            i += s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        }
    }
    push(&mut out, &s[run..], base);
    out
}

/// Bring a markdown body over to a document. Blank lines carry no information in a document (the
/// layout spaces blocks), so they are dropped; fenced code keeps its lines, without the fences.
pub fn from_markdown(body: &str) -> Doc {
    let mut doc = Doc::default();
    let mut in_code = false;
    for line in body.split('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            doc.blocks
                .push(Block::plain(Kind::Code, line.trim_end_matches(['\r'])));
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if let Some(img) = crate::vault::parse_image_line(line) {
            let mut b = Block::new(Kind::Image);
            b.image = img.reference.clone();
            b.width = img.width;
            b.crop = img.crop;
            b.runs.push(Run::plain(if img.alt.is_empty() {
                img.reference
            } else {
                img.alt
            }));
            doc.blocks.push(b);
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && (trimmed.len() == hashes || trimmed.as_bytes()[hashes] == b' ')
        {
            let mut b = Block::new(Kind::Heading(hashes as u8));
            b.runs = parse_inline(trimmed[hashes..].trim_start(), Attrs::default());
            doc.blocks.push(b);
            continue;
        }
        if trimmed.starts_with('>') {
            let mut b = Block::new(Kind::Quote);
            b.runs = parse_inline(trimmed[1..].trim_start(), Attrs::default());
            doc.blocks.push(b);
            continue;
        }
        if let Some((done, text)) = crate::vault::parse_todo_line(trimmed) {
            let mut b = Block::new(Kind::Todo);
            b.done = done;
            b.runs = parse_inline(&text, Attrs::default());
            doc.blocks.push(b);
            continue;
        }
        let bullet = ["- ", "* ", "+ "]
            .iter()
            .find_map(|m| trimmed.strip_prefix(*m));
        if let Some(rest) = bullet {
            let mut b = Block::new(Kind::Bullet);
            b.runs = parse_inline(rest, Attrs::default());
            doc.blocks.push(b);
            continue;
        }
        // A numbered line is **left alone**. It used to be turned into a bullet, and `to_markdown`
        // writes a bullet as "- ", so "1. rinse" became "- rinse" on the first save: the numbers were
        // gone from the file as well as from the page, which no renderer can undo. A document has no
        // numbered kind, so the honest migration is to keep the line's own words — the digits *are*
        // the words — and let it round-trip unchanged.
        if trimmed.len() >= 3 {
            let c0 = trimmed.chars().next().unwrap_or(' ');
            if (c0 == '-' || c0 == '*' || c0 == '_')
                && trimmed.chars().all(|c| c == c0 || c == ' ')
                && trimmed.chars().filter(|c| *c == c0).count() >= 3
            {
                doc.blocks.push(Block::new(Kind::Rule));
                continue;
            }
        }
        let mut b = Block::new(Kind::Para);
        b.runs = parse_inline(line.trim_end_matches(['\r']), Attrs::default());
        doc.blocks.push(b);
    }
    doc
}

// ---------------------------------------------------------------- markdown out

/// Write the document back as markdown: a faithful check that the migration kept everything, and a
/// way to export a page as plain text. Not how the app stores or edits it.
pub fn to_markdown(doc: &Doc) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for b in &doc.blocks {
        if b.kind == Kind::Code {
            // `from_markdown` makes one block per line of a fence, so consecutive code blocks are
            // one fence: wrapping each block on its own turned a three-line block into three
            if !in_fence {
                out.push_str("```\n");
                in_fence = true;
            }
            for r in &b.runs {
                out.push_str(&r.text);
            }
            if !b.text().ends_with('\n') {
                out.push('\n');
            }
            continue;
        }
        if in_fence {
            out.push_str("```\n");
            in_fence = false;
        }
        // the empty block that keeps a new page typeable exports as nothing, so the page file stays
        // empty rather than gaining a line
        if b.kind == Kind::Para && b.text().is_empty() {
            continue;
        }
        match b.kind {
            Kind::Heading(n) => {
                out.push_str(&"#".repeat(n.clamp(1, 6) as usize));
                out.push(' ');
            }
            Kind::Bullet => out.push_str("- "),
            Kind::Todo => out.push_str(if b.done { "- [x] " } else { "- [ ] " }),
            Kind::Quote => out.push_str("> "),
            Kind::Image => {
                out.push_str(&crate::vault::image_line(&b.image, &b.text(), b.width, b.crop));
                out.push('\n');
                continue;
            }
            Kind::Rule => {
                out.push_str("---\n");
                continue;
            }
            Kind::Para => {}
            // unreachable: the fence is opened and closed above, before this match
            Kind::Code => {}
        }
        for r in &b.runs {
            let t = &r.text;
            if let Some(d) = r.a.due {
                out.push_str(&format!("@due({})", d.format("%Y-%m-%d")));
            } else if r.a.code {
                out.push('`');
                out.push_str(t);
                out.push('`');
            } else if r.a.bold && r.a.italic {
                out.push_str(&format!("***{t}***"));
            } else if r.a.bold {
                out.push_str(&format!("**{t}**"));
            } else if r.a.italic {
                out.push_str(&format!("*{t}*"));
            } else if r.a.strike {
                out.push_str(&format!("~~{t}~~"));
            } else if let Some(link) = &r.a.link {
                if link.starts_with("[[") {
                    out.push_str(link);
                } else {
                    out.push_str(&format!("[{t}]({link})"));
                }
            } else if r.a.tag {
                out.push_str(t);
            } else {
                out.push_str(t);
            }
        }
        out.push('\n');
    }
    if in_fence {
        out.push_str("```\n");
    }
    out
}

// ---------------------------------------------------------------- store

/// The document file that belongs to a page.
pub fn doc_path(page: &Path) -> PathBuf {
    page.with_extension(EXT)
}

/// Take a page's document along when the page itself moves, is renamed or deleted: the document's
/// file name is derived from the page's, so one left behind is inherited by whatever page takes
/// that name next — which is how a new page came up holding a deleted page's text.
pub fn move_doc(from_page: &Path, to_page: &Path) {
    let (from, to) = (doc_path(from_page), doc_path(to_page));
    if from == to || !from.is_file() {
        return;
    }
    let _ = std::fs::rename(&from, &to);
}

/// Load a page's document, if it has one.
pub fn load(page: &Path) -> Option<Doc> {
    let text = std::fs::read_to_string(doc_path(page)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write a page's document (atomic: temporary file, then rename).
pub fn save(page: &Path, doc: &Doc) -> std::io::Result<()> {
    let path = doc_path(page);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(doc).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("note.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)
}

/// What a migration run did.
#[derive(Debug, Default, PartialEq)]
pub struct MigrateReport {
    pub converted: usize,
    /// Pages that already had a document.
    pub already: usize,
    /// Pages where the round trip did not reproduce the document (nothing was written for those).
    pub refused: Vec<PathBuf>,
    /// Documents with no page beside them: a page renamed or deleted by an older build left its
    /// document behind, and whatever page takes that name next would inherit it. Nothing is done to
    /// them here — they are reported so they can be removed deliberately.
    pub orphans: Vec<PathBuf>,
}

/// Bring every page in a vault over to the document store, leaving the markdown files in place as the
/// backup. A page is only written when parsing it back from the markdown we just generated gives the
/// same document, so a conversion that would lose something is refused rather than saved.
pub fn migrate_vault(root: &Path) -> MigrateReport {
    let mut report = MigrateReport::default();
    let vault = crate::vault::Vault::load(root);
    for page in vault.pages.iter() {
        if doc_path(&page.path).is_file() {
            report.already += 1;
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&page.path) else {
            report.refused.push(page.path.clone());
            continue;
        };
        let (_, body) = crate::vault::parse_front_matter_pub(&text);
        let doc = from_markdown(body);
        if from_markdown(&to_markdown(&doc)) != doc {
            report.refused.push(page.path.clone());
            continue;
        }
        match save(&page.path, &doc) {
            Ok(()) => report.converted += 1,
            Err(_) => report.refused.push(page.path.clone()),
        }
    }
    // walk the section directories again: every `.note` should have its `.md`
    for page in vault.pages.iter() {
        if let Some(dir) = page.path.parent() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.extension().map(|x| x == EXT).unwrap_or(false) && !p.with_extension("md").is_file() {
                        report.orphans.push(p);
                    }
                }
            }
        }
    }
    report.orphans.sort();
    report.orphans.dedup();
    report
}

// ---------------------------------------------------------------- editing a document

/// Divide runs at every newline: one set per line, the newlines themselves dropped. `Enter` gives two
/// pieces and a pasted chunk gives as many as it has lines. A block never holds a `'\n'` — the
/// document has no line breaks *inside* a block, which is why typing one has to make a new block.
pub fn split_runs_on_newlines(runs: &[Run]) -> Vec<Vec<Run>> {
    let mut out: Vec<Vec<Run>> = Vec::new();
    let mut cur: Vec<Run> = Vec::new();
    for r in runs {
        let mut piece = String::new();
        for c in r.text.chars() {
            if c == '\n' {
                if !piece.is_empty() {
                    cur.push(Run {
                        text: std::mem::take(&mut piece),
                        a: r.a.clone(),
                    });
                }
                out.push(std::mem::take(&mut cur));
            } else {
                piece.push(c);
            }
        }
        if !piece.is_empty() {
            cur.push(Run {
                text: piece,
                a: r.a.clone(),
            });
        }
    }
    out.push(cur);
    out
}

/// Put `text` into a block's runs at `range` (an empty range inserts, `None` appends at the end),
/// keeping the properties around it — `remap_runs` gives the pasted characters the attributes of the
/// run they land in, so pasting into a bold word stays bold — and split the result on line breaks:
/// a pasted chunk with `\n` is several blocks, the same rule typing follows. Always at least one
/// piece. The caller places the pieces and the caret; `tail` (the chars after `range`) is what makes
/// the caret land after the pasted text rather than at the end of the line.
pub fn paste_runs(
    runs: &[Run],
    old: &str,
    range: Option<(usize, usize)>,
    text: &str,
) -> Vec<Vec<Run>> {
    let chars: Vec<char> = old.chars().collect();
    let (s, e) = range.unwrap_or((chars.len(), chars.len()));
    let (s, e) = (s.min(chars.len()), e.min(chars.len()));
    let (s, e) = (s, e.max(s));
    let mut new = String::new();
    new.extend(chars[..s].iter());
    new.push_str(text);
    new.extend(chars[e..].iter());
    split_runs_on_newlines(&remap_runs(runs, old, &new))
}

/// Concatenate two blocks' runs — Backspace at the start of a block joins it to the one above.
/// Neighbours carrying the same properties are merged, so the run list stays tidy.
pub fn join_runs(a: &[Run], b: &[Run]) -> Vec<Run> {
    let mut out: Vec<Run> = a.to_vec();
    for r in b {
        if r.text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.a == r.a => last.text.push_str(&r.text),
            _ => out.push(r.clone()),
        }
    }
    out
}

/// Total characters in a block's runs.
pub fn run_len(runs: &[Run]) -> usize {
    runs.iter().map(|r| r.text.chars().count()).sum()
}

/// Apply `f` to the properties of every run covered by the character range `range` (clamped to the
/// runs), splitting runs at the boundaries so only the covered text is affected. `None` means the
/// whole block — what the line-level menu items do. This is what "Bold" does to a marked word: it
/// sets a property, it never inserts characters.
pub fn set_attr(runs: &mut Vec<Run>, range: Option<(usize, usize)>, f: impl Fn(&mut Attrs)) {
    let total = run_len(runs);
    let (start, end) = match range {
        Some((s, e)) => (s.min(e).min(total), s.max(e).min(total)),
        None => (0, total),
    };
    let mut out: Vec<Run> = Vec::with_capacity(runs.len() + 2);
    let mut at = 0usize;
    for r in runs.iter() {
        let n = r.text.chars().count();
        let (rs, re) = (at, at + n);
        at = re;
        if re <= start || rs >= end {
            out.push(r.clone());
            continue;
        }
        let chars: Vec<char> = r.text.chars().collect();
        let head = start.saturating_sub(rs).min(n);
        let tail = end.saturating_sub(rs).min(n);
        if head > 0 {
            out.push(Run {
                text: chars[..head].iter().collect(),
                a: r.a.clone(),
            });
        }
        let mut mid = r.a.clone();
        f(&mut mid);
        if tail > head {
            out.push(Run {
                text: chars[head..tail].iter().collect(),
                a: mid,
            });
        }
        if tail < n {
            out.push(Run {
                text: chars[tail..].iter().collect(),
                a: r.a.clone(),
            });
        }
    }
    *runs = out;
}

/// True when the whole range already carries the property (so the menu item turns it off).
pub fn attr_all(runs: &[Run], range: Option<(usize, usize)>, f: impl Fn(&Attrs) -> bool) -> bool {
    let total = run_len(runs);
    let (start, end) = match range {
        Some((s, e)) => (s.min(e).min(total), s.max(e).min(total)),
        None => (0, total),
    };
    if start >= end {
        return false;
    }
    let mut at = 0usize;
    let mut all = true;
    for r in runs {
        let n = r.text.chars().count();
        let (rs, re) = (at, at + n);
        at = re;
        let overlap = rs.max(start) < re.min(end);
        if overlap {
            all &= f(&r.a);
        }
    }
    all
}

/// Set a block's due date. The date is a run carrying its own words, so the run that already holds a
/// date is rewritten **in place** — appending a second one made a line read "24 Oct 2026 25 Oct
/// 2026", and a second date in a hand-edited document is folded into the first (one date per line).
/// A line with no date gets one at the end, spaced away from the words so the chip cannot glue
/// itself to them.
pub fn set_due(runs: &mut Vec<Run>, d: NaiveDate) {
    let words = d.format("%d %b %Y").to_string();
    let mut placed = false;
    let mut out: Vec<Run> = Vec::with_capacity(runs.len() + 1);
    for r in runs.drain(..) {
        if r.a.due.is_some() {
            if !placed {
                out.push(Run {
                    text: words.clone(),
                    a: Attrs {
                        due: Some(d),
                        ..r.a
                    },
                });
                placed = true;
            }
            continue;
        }
        out.push(r);
    }
    if !placed {
        // the chip is its own run and `to_markdown` writes `@due(…)` for it, so the space has to
        // live in a *text* run in front of it — a space inside the chip's own words never reaches
        // the export, and the page then re-read "tomorrow18 Oct 2026"
        let glue = out
            .last()
            .map(|r| r.text.chars().last().map(|c| !c.is_whitespace()).unwrap_or(false))
            .unwrap_or(false);
        if glue {
            out.push(Run::plain(" "));
        }
        out.push(Run {
            text: words,
            a: Attrs {
                due: Some(d),
                ..Default::default()
            },
        });
    }
    *runs = out;
}

/// Take a block's due date off — **and the date's own words with it**. Clearing just the flag left
/// "24 Oct 2026" sitting in the note as plain text, which looks exactly like "Clear due date" doing
/// nothing: the chip colour went, the date stayed.
pub fn clear_due(runs: &mut Vec<Run>) {
    if runs.iter().all(|r| r.a.due.is_none()) {
        return;
    }
    let was_last = runs.last().map(|r| r.a.due.is_some()).unwrap_or(false);
    let mut out: Vec<Run> = Vec::with_capacity(runs.len());
    for r in runs.drain(..) {
        if r.a.due.is_none() {
            out.push(r);
        }
    }
    if was_last {
        if let Some(last) = out.last_mut() {
            let keep = last.text.trim_end().len();
            last.text.truncate(keep);
        }
    }
    // the date sat between two words: do not leave the two spaces that held it apart behind
    let mut i = 0;
    while i + 1 < out.len() {
        if out[i].text.ends_with(' ') && out[i + 1].text.starts_with(' ') {
            out[i + 1].text.remove(0);
            if out[i + 1].text.is_empty() {
                out.remove(i + 1);
                continue;
            }
        }
        i += 1;
    }
    out.retain(|r| !r.text.is_empty());
    *runs = out;
}

/// Fit a block's runs to edited text: the parts the user did not touch keep their properties, and
/// text typed inside a run takes that run's properties (typing inside bold stays bold). `old` is the
/// text the runs described; `new` is the text the editor now holds.
pub fn remap_runs(runs: &[Run], old: &str, new: &str) -> Vec<Run> {
    if old == new {
        return runs.to_vec();
    }
    let oc: Vec<char> = old.chars().collect();
    let nc: Vec<char> = new.chars().collect();
    // common prefix and suffix
    let mut pre = 0usize;
    while pre < oc.len() && pre < nc.len() && oc[pre] == nc[pre] {
        pre += 1;
    }
    let mut suf = 0usize;
    while suf < oc.len() - pre && suf < nc.len() - pre && oc[oc.len() - 1 - suf] == nc[nc.len() - 1 - suf]
    {
        suf += 1;
    }
    let tail_from = oc.len() - suf; // first char index of the untouched tail, in both
    let inserted = nc.len() - pre - suf; // characters added (or, if negative, removed)
    let mut out: Vec<Run> = Vec::new();
    let mut at = 0usize;
    let push = |out: &mut Vec<Run>, text: String, a: Attrs| {
        if text.is_empty() {
            return;
        }
        if let Some(last) = out.last_mut() {
            if last.a == a {
                last.text.push_str(&text);
                return;
            }
        }
        out.push(Run { text, a });
    };
    for r in runs {
        let n = r.text.chars().count();
        let (rs, re) = (at, at + n);
        at = re;
        let is_last = rs > tail_from || re >= oc.len();
        // the part before the change
        if rs < pre {
            let keep = r.text.chars().take(pre.min(re) - rs).collect::<String>();
            push(&mut out, keep, r.a.clone());
        }
        // the part the change landed in keeps this run's properties for what was typed there
        if rs < pre && re > pre {
            if inserted > 0 {
                let typed: String = nc[pre..pre + inserted].iter().collect();
                push(&mut out, typed, r.a.clone());
            }
        } else if rs == pre && inserted > 0 {
            let typed: String = nc[pre..pre + inserted].iter().collect();
            push(&mut out, typed, r.a.clone());
        }
        // the part after the change
        if re > tail_from && !is_last {
            let from = tail_from.max(rs);
            let skip = from - rs;
            let keep: String = r.text.chars().skip(skip).collect();
            push(&mut out, keep, r.a.clone());
        }
        if re >= tail_from && is_last {
            let from = tail_from.max(rs);
            let skip = from - rs;
            let keep: String = r.text.chars().skip(skip).collect();
            push(&mut out, keep, r.a.clone());
        }
    }
    // a change at the very end, or in an empty block: keep the last run's properties
    if out.iter().map(|r| r.text.chars().count()).sum::<usize>() < nc.len() {
        let have: usize = out.iter().map(|r| r.text.chars().count()).sum();
        let rest: String = nc[have..].iter().collect();
        let a = runs
            .last()
            .map(|r| r.a.clone())
            .unwrap_or_default();
        push(&mut out, rest, a);
    }
    // never lose text: if the mapping came up short, fall back to one plain run
    if out.iter().map(|r| r.text.chars().count()).sum::<usize>() != nc.len() {
        return vec![Run::plain(new)];
    }
    out
}

/// Which block of `doc` the todo on body line `line` is: the same occurrence of the same text,
/// counted in file order. `None` when the markdown and the document disagree (a page written by
/// hand, or a document left behind by a rename) — better no tick at all than the wrong one.
pub fn todo_block_at_line(doc: &Doc, body: &str, line: usize, text: &str) -> Option<usize> {
    // A due date is a `@due(…)` token in the markdown and a run in the document, so neither side may
    // keep it for the match: with the token compared literally, a task that had a date could not be
    // ticked or re-dated from the tasks view ("that todo is not in the page's document").
    let text = strip_due_tokens(text);
    let nth = body
        .split('\n')
        .take(line)
        .filter_map(crate::vault::parse_todo_line)
        .filter(|(_, t)| strip_due_tokens(t) == text)
        .count();
    doc.blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind == Kind::Todo && block_words(b) == text)
        .nth(nth)
        .map(|(i, _)| i)
}

/// A todo's words without the date token: `@due(YYYY-MM-DD)`, `@today` and `@tomorrow` are text in
/// the markdown and a property on a run in the document (whose own words are "24 Oct 2026").
pub fn strip_due_tokens(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text.trim();
    while let Some(i) = rest.find('@') {
        let (before, after) = rest.split_at(i);
        let consumed = if let Some(r) = after.strip_prefix("@due(") {
            r.find(')').map(|j| 5 + j + 1)
        } else if after.starts_with("@today") {
            Some(6)
        } else if after.starts_with("@tomorrow") {
            Some(9)
        } else {
            None
        };
        match consumed {
            Some(n) if n <= after.len() => {
                out.push_str(before);
                rest = &after[n..];
            }
            _ => {
                out.push_str(before);
                out.push('@');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// The words a todo block would have in the markdown: its runs minus the due date's own words.
fn block_words(b: &Block) -> String {
    let joined: String = b
        .runs
        .iter()
        .filter(|r| r.a.due.is_none())
        .map(|r| r.text.as_str())
        .collect();
    joined.trim().to_string()
}

#[cfg(test)]
mod mark_tests {
    use super::*;

    #[test]
    fn a_highlight_survives_the_json_store() {
        let dir = std::env::temp_dir().join(format!("notecraft-mark-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("M.md");
        let mut d = from_markdown("one two three\n");
        set_attr(&mut d.blocks[0].runs, Some((4, 7)), |a| a.mark = 3);
        std::fs::write(&page, "---\ntitle: M\norder: 0\n---\n\n").unwrap();
        save(&page, &d).unwrap();
        let back = load(&page).unwrap();
        assert_eq!(back.blocks[0].runs.len(), 3, "the range was split out: {back:?}");
        assert_eq!(back.blocks[0].runs[1].text, "two");
        assert_eq!(back.blocks[0].runs[1].a.mark, 3);
        assert_eq!(back.blocks[0].runs[0].a.mark, 0);
        // markdown cannot carry it: the export keeps the words, the document keeps the colour
        let md = to_markdown(&back);
        assert!(md.contains("one two three"), "{md}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod bridge_tests {
    use super::*;

    #[test]
    fn a_dated_todo_is_found_from_its_markdown_line() {
        // the file keeps `@due(…)` as text, the document keeps it as a run with its own words;
        // comparing them literally meant a dated task could not be ticked or re-dated
        let body = "- [ ] water the plants @due(2026-10-24)\n- [ ] water the plants\n";
        let d = from_markdown(body);
        assert_eq!(d.blocks[0].text(), "water the plants 24 Oct 2026");
        assert_eq!(
            todo_block_at_line(&d, body, 0, "water the plants @due(2026-10-24)"),
            Some(0)
        );
        assert_eq!(todo_block_at_line(&d, body, 1, "water the plants"), Some(1));
        // and a date in the middle of the words does not stop the match either
        let body = "- [ ] order 2 bags @due(2026-10-24) of compost\n";
        let d = from_markdown(body);
        assert_eq!(
            todo_block_at_line(&d, body, 0, "order 2 bags @due(2026-10-24) of compost"),
            Some(0)
        );
    }
}

#[cfg(test)]
mod edit_tests {
    use super::*;

    fn bold(text: &str) -> Run {
        Run {
            text: text.to_string(),
            a: Attrs {
                bold: true,
                ..Attrs::default()
            },
        }
    }

    #[test]
    fn enter_divides_a_block_and_the_newline_belongs_to_neither_half() {
        let cases: &[(&str, &[&str])] = &[
            ("abc\ndef", &["abc", "def"]),
            ("abc\n", &["abc", ""]),
            ("\nabc", &["", "abc"]),
            ("\n", &["", ""]),
            ("a\n\nb", &["a", "", "b"]),
            ("one line", &["one line"]),
        ];
        for (text, want) in cases {
            let runs = vec![Run::plain(*text)];
            let pieces: Vec<String> = split_runs_on_newlines(&runs)
                .iter()
                .map(|p| p.iter().map(|r| r.text.as_str()).collect::<String>())
                .collect();
            assert_eq!(&pieces, want, "for {text:?}");
            // no piece may carry the newline
            assert!(split_runs_on_newlines(&runs)
                .iter()
                .all(|p| !p.iter().any(|r| r.text.contains('\n'))));
        }
        // the properties travel with their text
        let runs = parse_inline("**bold**\nplain", Attrs::default());
        let pieces = split_runs_on_newlines(&runs);
        assert!(pieces[0].iter().any(|r| r.a.bold && r.text == "bold"));
        assert!(pieces[1].iter().any(|r| r.text == "plain" && !r.a.bold));
    }

    #[test]
    fn backspace_joins_two_blocks_without_losing_anything() {
        let a = from_markdown("**bold**").blocks[0].runs.clone();
        let b = vec![Run::plain(" tail")];
        let joined = join_runs(&a, &b);
        assert_eq!(joined.iter().map(|r| r.text.as_str()).collect::<String>(), "bold tail");
        assert!(joined[0].a.bold, "the first run keeps its properties");
        // same-property neighbours merge into one run
        let joined = join_runs(&[Run::plain("one")], &[Run::plain(" two")]);
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].text, "one two");
    }

    #[test]
    fn setting_a_property_splits_runs_and_leaves_the_text_alone() {
        let mut runs = vec![Run::plain("hello world")];
        set_attr(&mut runs, Some((6, 11)), |a| a.bold = true);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "hello ");
        assert!(!runs[0].a.bold);
        assert_eq!(runs[1].text, "world");
        assert!(runs[1].a.bold);
        // the text is untouched: properties never change the words
        assert_eq!(runs.iter().map(|r| r.text.as_str()).collect::<String>(), "hello world");
        // applying it again over the same range is a no-op in shape
        set_attr(&mut runs, Some((6, 11)), |a| a.bold = true);
        assert_eq!(runs.len(), 2);
        assert!(runs[1].a.bold);
        // the whole block: every run carries it
        set_attr(&mut runs, None, |a| a.italic = true);
        assert!(runs.iter().all(|r| r.a.italic));
    }

    #[test]
    fn attr_all_reports_whether_a_range_is_already_marked() {
        let runs = vec![bold("bold"), Run::plain(" plain")];
        assert!(attr_all(&runs, Some((0, 4)), |a| a.bold));
        assert!(!attr_all(&runs, None, |a| a.bold));
        assert!(!attr_all(&runs, Some((0, 0)), |a| a.bold));
    }

    #[test]
    fn clearing_a_due_date_takes_the_dates_words_with_it() {
        // "Clear due date" only cleared the flag, so the line kept "24 Oct 2026" as plain text:
        // the chip colour went and the date stayed, which reads as "the menu item does nothing"
        let doc = from_markdown("- [ ] ring the plumber @due(2026-10-24) today\n");
        let mut runs = doc.blocks[0].runs.clone();
        assert!(runs.iter().any(|r| r.a.due.is_some() && r.text == "24 Oct 2026"));
        clear_due(&mut runs);
        assert!(!runs.iter().any(|r| r.a.due.is_some()));
        assert_eq!(
            runs.iter().map(|r| r.text.as_str()).collect::<String>(),
            "ring the plumber today",
            "the date's words go, and the space that held them does not double: {runs:?}"
        );
        let mut d = doc.clone();
        d.blocks[0].runs = runs;
        let md = to_markdown(&d);
        assert!(!md.contains("@due"), "{md}");
        assert!(!md.contains("Oct"), "{md}");
    }

    #[test]
    fn a_due_date_is_rewritten_in_place_and_never_doubles() {
        let mut runs = from_markdown("- [ ] x @due(2026-10-24) and more\n").blocks[0].runs.clone();
        set_due(&mut runs, NaiveDate::from_ymd_opt(2026, 10, 25).unwrap());
        let text: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "x 25 Oct 2026 and more", "one date, where it was: {runs:?}");
        assert_eq!(runs.iter().filter(|r| r.a.due.is_some()).count(), 1);
        // a line with no date gets one at the end, spaced away from its words
        let day = NaiveDate::from_ymd_opt(2026, 10, 24).unwrap();
        let mut block = Block::plain(Kind::Para, "call the plumber");
        set_due(&mut block.runs, day);
        let text: String = block.runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "call the plumber 24 Oct 2026");
        assert!(block.runs.last().unwrap().a.due.is_some());
        // the space has to reach the export too, or the page re-reads "plumber18 Oct 2026"
        let mut d = Doc::default();
        d.blocks.push(block.clone());
        let md = to_markdown(&d);
        assert!(md.contains("call the plumber @due(2026-10-24)"), "{md}");
        assert_eq!(from_markdown(&md).blocks[0].text(), "call the plumber 24 Oct 2026");
        // a second date folds into the first: a line never carries two chips
        let mut runs = block.runs.clone();
        set_due(&mut runs, day + chrono::Duration::days(1));
        assert_eq!(runs.iter().filter(|r| r.a.due.is_some()).count(), 1);
        // and clearing a date the menu put at the end leaves no trailing space
        clear_due(&mut runs);
        let text: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "call the plumber");
    }

    #[test]
    fn editing_inside_a_run_keeps_the_properties_and_the_text() {
        // typing inside the bold word
        let runs = vec![Run::plain("a "), bold("bold"), Run::plain(" z")];
        let old = "a bold z";
        let new = "a bolderr z";
        let out = remap_runs(&runs, old, new);
        assert_eq!(out.iter().map(|r| r.text.as_str()).collect::<String>(), new);
        let mid = &out[1];
        assert!(mid.a.bold, "the typed text took the run's properties: {out:?}");
        assert!(mid.text.starts_with("bold"));
    }

    #[test]
    fn editing_at_the_edges_keeps_the_other_runs_intact() {
        let runs = vec![Run::plain("plain "), bold("bold")];
        let out = remap_runs(&runs, "plain bold", "plain bold!");
        assert_eq!(out.iter().map(|r| r.text.as_str()).collect::<String>(), "plain bold!");
        assert!(out[0].text.starts_with("plain"));
        assert!(out.iter().any(|r| r.a.bold && r.text == "bold!"));
        // deleting the whole of the bold run leaves the rest alone
        let out = remap_runs(&runs, "plain bold", "plain ");
        assert_eq!(out.iter().map(|r| r.text.as_str()).collect::<String>(), "plain ");
        assert!(!out.iter().any(|r| r.a.bold));
    }

    #[test]
    fn remapping_never_loses_text_in_awkward_edits() {
        let cases: &[(&str, &str)] = &[
            ("", "typed"),
            ("typed", ""),
            ("abc", "abc"),
            ("abc", "axc"),
            ("abc", "xabc"),
            ("abc", "abcx"),
            ("a b c", "ab c"),
            ("— dash —", "— dash — more"),
        ];
        for (old, new) in cases {
            let runs = from_markdown(old).blocks.first().map(|b| b.runs.clone()).unwrap_or_default();
            let out = remap_runs(&runs, old, new);
            let text: String = out.iter().map(|r| r.text.as_str()).collect();
            assert_eq!(text, *new, "lost text for {old:?} -> {new:?} ({out:?})");
        }
        // a bold word: the block's text is just "x" (the boldness is a property), and typing "y"
        // after it must keep the property and the text
        let runs = from_markdown("**x**").blocks[0].runs.clone();
        let out = remap_runs(&runs, "x", "xy");
        assert_eq!(out.iter().map(|r| r.text.as_str()).collect::<String>(), "xy");
        assert!(out.iter().all(|r| r.a.bold), "{out:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(runs: &[Run]) -> String {
        runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// The editor's Paste row goes through `paste_runs`: the pasted characters take the properties
    /// of the run they land in, a break in the pasted text makes another block (the same rule typing
    /// follows), and a marked range is replaced rather than added to.
    #[test]
    fn a_paste_keeps_the_properties_it_lands_in_and_splits_on_its_breaks() {
        let runs = from_markdown("**bold** plain\n").blocks[0].runs.clone();
        let old = text_of(&runs);
        assert_eq!(old, "bold plain");

        // at the end of the line: one piece, nothing lost
        let pieces = paste_runs(&runs, &old, None, " tail");
        assert_eq!(pieces.len(), 1);
        assert_eq!(text_of(&pieces[0]), "bold plain tail");

        // a line break in the pasted text is a block boundary, and the newline belongs to neither
        let pieces = paste_runs(&runs, &old, None, "\none two");
        assert_eq!(pieces.len(), 2);
        assert_eq!(text_of(&pieces[0]), "bold plain");
        assert_eq!(text_of(&pieces[1]), "one two");

        // inside the bold run the pasted characters stay bold
        let pieces = paste_runs(&runs, &old, Some((2, 2)), "XX");
        assert_eq!(text_of(&pieces[0]), "boXXld plain");
        let mid = pieces[0]
            .iter()
            .find(|r| r.text.contains("XX"))
            .expect("the pasted characters are in a run");
        assert!(mid.a.bold, "a paste inside bold stays bold");

        // a marked range is replaced
        let pieces = paste_runs(&runs, &old, Some((5, 10)), "!");
        assert_eq!(text_of(&pieces[0]), "bold !");

        // and an empty block takes the text without losing it
        let empty: Vec<Run> = Vec::new();
        let pieces = paste_runs(&empty, "", None, "fresh");
        assert_eq!(text_of(&pieces[0]), "fresh");
    }

    #[test]
    fn migrating_a_vault_writes_documents_and_keeps_the_markdown() {
        let dir = std::env::temp_dir().join(format!("notecraft-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let notes = dir.join("Notebook").join("Notes");
        std::fs::create_dir_all(&notes).unwrap();
        let page = notes.join("Test.md");
        let md = "---\ntitle: Test\ncreated: 2026-10-17T10:00:00+01:00\nupdated: 2026-10-17T10:00:00+01:00\norder: 1\n---\n\n# heading\n\n**bold** and - [ ] todo @due(2026-10-24) #work\n";
        std::fs::write(&page, md).unwrap();
        let report = migrate_vault(&dir);
        assert_eq!(report.converted, 1, "{report:?}");
        assert!(report.refused.is_empty(), "{report:?}");
        assert!(doc_path(&page).is_file(), "a document was written next to the page");
        // the markdown is untouched, and a second run converts nothing
        assert_eq!(std::fs::read_to_string(&page).unwrap(), md);
        let again = migrate_vault(&dir);
        assert_eq!(again.converted, 0);
        assert_eq!(again.already, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn markdown_becomes_blocks_and_runs() {
        let md = "\
# Heading here

Some **bold** and *italic* and `code` and ~~gone~~ words.

- a bullet
- [ ] a todo @due(2026-10-24) #work
- [x] a done todo

> quoted words

1. numbered

---

![alt text](assets/x.png)

```
code line
```
";
        let doc = from_markdown(md);
        let kinds: Vec<Kind> = doc.blocks.iter().map(|b| b.kind).collect();
        assert_eq!(
            kinds,
            vec![
                Kind::Heading(1),
                Kind::Para,
                Kind::Bullet,
                Kind::Todo,
                Kind::Todo,
                Kind::Quote,
                // "1. numbered" is a numbered line, not a bullet: it keeps its digits
                Kind::Para,
                Kind::Rule,
                Kind::Image,
                Kind::Code,
            ],
            "blocks: {:?}",
            doc.blocks
        );
        // properties, not syntax: the words are bare text
        let para = &doc.blocks[1];
        assert_eq!(para.text(), "Some bold and italic and code and gone words.");
        assert!(para.runs.iter().any(|r| r.a.bold && r.text == "bold"));
        assert!(para.runs.iter().any(|r| r.a.italic && r.text == "italic"));
        assert!(para.runs.iter().any(|r| r.a.code && r.text == "code"));
        assert!(para.runs.iter().any(|r| r.a.strike && r.text == "gone"));
        // a due date is a date, a tag is a tag: both are properties on the runs, never text
        let todo = &doc.blocks[3];
        assert!(!todo.done);
        assert_eq!(
            todo.runs.iter().find_map(|r| r.a.due),
            Some(NaiveDate::from_ymd_opt(2026, 10, 24).unwrap())
        );
        assert!(todo
            .runs
            .iter()
            .any(|r| r.a.tag && r.text.trim_start_matches('#') == "work"));
        assert!(doc.blocks[4].done);
        // the picture keeps its reference and caption
        assert_eq!(doc.blocks[8].image, "assets/x.png");
        assert_eq!(doc.blocks[8].text(), "alt text");
        assert_eq!(doc.blocks[9].text(), "code line");
    }

    #[test]
    fn an_inline_picture_keeps_its_reference() {
        let doc = from_markdown("text before ![alt text](assets/x.png) text after\n");
        let runs = &doc.blocks[0].runs;
        let pic = runs.iter().find(|r| r.text == "alt text").unwrap();
        assert_eq!(pic.a.link.as_deref(), Some("assets/x.png"));
        assert_eq!(doc.blocks[0].text(), "text before alt text text after");
    }

    #[test]
    fn links_and_wiki_links_keep_their_targets() {
        let doc = from_markdown("see [the label](https://example.com/x) and [[wiki page]] here\n");
        let runs = &doc.blocks[0].runs;
        let link = runs.iter().find(|r| r.text == "the label").unwrap();
        assert_eq!(link.a.link.as_deref(), Some("https://example.com/x"));
        let wiki = runs.iter().find(|r| r.text == "wiki page").unwrap();
        assert_eq!(wiki.a.link.as_deref(), Some("[[wiki page]]"));
    }

    #[test]
    fn every_document_has_a_block_to_type_in() {
        // an empty body parses to no blocks, and a page with no block has no caret at all
        let mut doc = from_markdown("");
        assert!(doc.blocks.is_empty());
        doc.ensure_block();
        assert_eq!(doc.blocks.len(), 1);
        // and the block is invisible in the export, so the page file stays empty
        assert_eq!(to_markdown(&doc), "");
        // a document that has blocks is left alone
        let mut doc = from_markdown("words\n");
        doc.ensure_block();
        assert_eq!(doc.blocks.len(), 1);
        assert_eq!(doc.blocks[0].text(), "words");
    }

    #[test]
    fn a_size_step_survives_the_store_and_scales_the_render() {
        // the step is relative: -1 small, 0 the block's own size, +1 large
        assert!(size_scale(-1) < 1.0 && size_scale(0) == 1.0 && size_scale(1) > 1.0);
        let mut runs = vec![Run::plain("plain ")];
        runs.push(Run {
            text: "big".into(),
            a: Attrs {
                size: 1,
                ..Default::default()
            },
        });
        runs.push(Run {
            text: " small".into(),
            a: Attrs {
                size: -1,
                ..Default::default()
            },
        });
        let mut d = Doc::default();
        let mut b = Block::new(Kind::Para);
        b.runs = runs;
        d.blocks.push(b);
        // through the JSON store and back
        let dir = std::env::temp_dir().join(format!("notecraft-size-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("P.md");
        save(&page, &d).unwrap();
        let back = load(&page).unwrap();
        assert_eq!(back, d, "the step is part of the document");
        let _ = std::fs::remove_dir_all(&dir);
        // a document written before the property existed still loads (serde default = 0)
        let old = r#"{"blocks":[{"kind":"Para","runs":[{"text":"x","a":{}}]}]}"#;
        let parsed: Doc = serde_json::from_str(old).expect("old documents still parse");
        assert_eq!(parsed.blocks[0].runs[0].a.size, 0);
    }

    #[test]
    fn a_document_survives_the_json_store() {
        let md = "# t\n\n**b** *i* `c` #tag @due(2026-10-24)\n\n- [ ] x\n\n![a](assets/y.png)\n";
        let doc = from_markdown(md);
        let dir = std::env::temp_dir().join(format!("notecraft-doc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("Page.md");
        save(&page, &doc).unwrap();
        assert!(doc_path(&page).is_file());
        assert_eq!(load(&page).unwrap(), doc);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn markdown_round_trip_is_stable() {
        // doc -> markdown -> doc must be the same document: the migration loses nothing, and the
        // export is faithful. (Byte-identity with the *input* is not required: a document has no
        // blank lines and re-numbers lists.)
        for md in [
            "# h\n\ntext **b** *i* `c` ~~s~~ [l](https://e.com) [[w]] #t @due(2026-10-24)\n\n- a\n- [ ] b\n- [x] c\n\n> q\n\n---\n\n![alt](assets/i.png)\n\n```\ncode\n```\n",
            "plain words\nanother line\n",
            "— em dash and unicode ✏ words\n",
            "",
        ] {
            let once = from_markdown(md);
            let twice = from_markdown(&to_markdown(&once));
            assert_eq!(once, twice, "round trip changed the document for {md:?}");
        }
        // a fence with several lines is one fence on the way out: each code line is its own block,
        // and wrapping each block separately turned a three-line block into three fences (which is
        // what read mode then drew — one framed box per line)
        let md = "```\nfirst\nsecond\nthird\n```\n";
        let doc = from_markdown(md);
        assert_eq!(doc.blocks.len(), 3);
        assert_eq!(to_markdown(&doc), md);
    }

    /// A numbered line keeps its numbers. It used to be parsed into a bullet, and the export writes a
    /// bullet as "- ", so opening a page holding "1. rinse" and letting it save rewrote the file as
    /// "- rinse" — the numbering was gone from the store, not just from the view.
    #[test]
    fn a_numbered_line_keeps_its_numbers() {
        let md = "1. rinse\n2. repeat\n";
        let doc = from_markdown(md);
        assert!(doc.blocks.iter().all(|b| b.kind == Kind::Para), "became a bullet");
        assert_eq!(doc.blocks[0].text(), "1. rinse");
        assert_eq!(to_markdown(&doc), md);
        // a bullet still is one, and a dotted line that is not a list stays prose
        assert_eq!(from_markdown("- a\n").blocks[0].kind, Kind::Bullet);
        assert_eq!(
            from_markdown("3.14 is pi\n").blocks[0].kind,
            Kind::Para,
            "a decimal is not a list"
        );
    }

    /// Regression: `see ![screenshot] below` sliced the reference backwards
    /// ("byte range starts at 18 but ends at 15") and took the whole app down on open — the
    /// migration and every page without a document go through `from_markdown`.
    #[test]
    fn a_bracketed_picture_without_a_target_does_not_panic() {
        for line in ["see ![screenshot] below", "look ![shot](", "![a] and ) later", "![x]"] {
            let doc = from_markdown(line);
            assert_eq!(doc.blocks.len(), 1, "{line:?}");
            // no `](`, so it is not a picture: the words stay exactly as they were typed
            assert_eq!(doc.blocks[0].text(), line, "{line:?}");
        }
        // a real one consumes its own markup and keeps the reference
        let doc = from_markdown("![alt text](assets/x.png) after");
        assert_eq!(doc.blocks[0].text(), "alt text after");
        let pic = doc.blocks[0].runs.iter().find(|r| r.text == "alt text").unwrap();
        assert_eq!(pic.a.link.as_deref(), Some("assets/x.png"));
        // an empty alt on a line of its own is still a picture line (its caption falls back to
        // the reference)
        let doc = from_markdown("![](assets/x.png)");
        assert_eq!(doc.blocks[0].kind, Kind::Image);
        assert_eq!(doc.blocks[0].image, "assets/x.png");
    }

    #[test]
    fn multibyte_text_does_not_panic_the_parser() {
        for md in [
            "— dash — **bold** — dash —\n",
            "✏ #—tag @due(2026-10-24) —\n",
            "*—* — *—* —\n",
        ] {
            let _ = from_markdown(md);
        }
    }
}

#[cfg(test)]
mod tests_crop {
    use super::*;

    #[test]
    fn a_crop_is_clamped_inside_the_picture() {
        let c = Crop {
            x: 0.9,
            y: -0.5,
            w: 0.4,
            h: 2.0,
        }
        .clamped();
        assert!(c.x + c.w <= 1.0 + 1e-6, "{c:?}");
        assert!(c.y >= 0.0 && c.h <= 1.0, "{c:?}");
        assert!(c.w >= Crop::MIN && c.h >= Crop::MIN);
        // a degenerate one still has something to grab
        let zero = Crop {
            x: 0.5,
            y: 0.5,
            w: 0.0,
            h: 0.0,
        }
        .clamped();
        assert!(zero.w >= Crop::MIN && zero.h >= Crop::MIN, "{zero:?}");
        assert!(zero.x + zero.w <= 1.0 + 1e-6 && zero.y + zero.h <= 1.0 + 1e-6);
    }

    /// The whole picture is *not* a crop: a frame dragged back out has to come off the line, not stay as
    /// a no-op that says "cropped" in the file and in the caption.
    #[test]
    fn the_whole_picture_is_not_a_crop() {
        assert!(Crop::FULL.is_full());
        assert!(Crop {
            x: 0.0001,
            y: 0.0,
            w: 0.9999,
            h: 1.0
        }
        .is_full());
        assert!(!Crop {
            x: 0.05,
            y: 0.0,
            w: 0.9,
            h: 1.0
        }
        .is_full());
    }

    #[test]
    fn pixels_and_fractions_round_trip() {
        let c = Crop::from_px((100.0, 50.0, 400.0, 300.0), (1000.0, 800.0));
        assert!((c.x - 0.1).abs() < 1e-6 && (c.y - 0.0625).abs() < 1e-6);
        assert!((c.w - 0.4).abs() < 1e-6 && (c.h - 0.375).abs() < 1e-6);
        let (x, y, w, h) = c.px((1000.0, 800.0));
        assert!((x - 100.0).abs() < 0.01 && (y - 50.0).abs() < 0.01);
        assert!((w - 400.0).abs() < 0.01 && (h - 300.0).abs() < 0.01);
    }

    #[test]
    fn the_attribute_round_trips_and_junk_means_the_whole_picture() {
        let c = Crop {
            x: 0.1234,
            y: 0.5,
            w: 0.25,
            h: 0.4,
        };
        assert_eq!(Crop::parse(&c.attr()), Some(c));
        assert_eq!(
            Crop::parse("0.1,0.2,0.5,0.6"),
            Some(Crop {
                x: 0.1,
                y: 0.2,
                w: 0.5,
                h: 0.6
            })
        );
        for junk in [
            "1,2",
            "a,b,c,d",
            "0,0,1,1",        // the whole picture
            "nan,0,1,1",      // not a number
            "0.1,0.1,0,0",    // degenerate, then clamped — a valid tiny crop
            "",
        ] {
            let parsed = Crop::parse(junk);
            assert!(
                junk == "0.1,0.1,0,0" || parsed.is_none(),
                "{junk:?} should not be a crop, got {parsed:?}"
            );
            assert!(parsed.map(|c| !c.is_full()).unwrap_or(true));
        }
    }

    /// The line is the storage: a crop written to a page has to come back identical, with the width and
    /// the caption it had.
    #[test]
    fn a_cropped_image_line_survives_a_body_round_trip() {
        let body = "<img src=\"assets/p.png\" alt=\"p\" width=\"320\" crop=\"0.1000,0.2000,0.5000,0.6000\">\n";
        let doc = from_markdown(body);
        assert_eq!(doc.blocks.len(), 1);
        let b = &doc.blocks[0];
        assert_eq!(b.kind, Kind::Image);
        assert_eq!(b.width, Some(320));
        assert_eq!(
            b.crop,
            Some(Crop {
                x: 0.1,
                y: 0.2,
                w: 0.5,
                h: 0.6
            })
        );
        assert_eq!(to_markdown(&doc), body);
        // and a picture with no crop is still plain markdown
        let plain = from_markdown("![p](assets/p.png)\n");
        assert_eq!(plain.blocks[0].crop, None);
        assert_eq!(to_markdown(&plain), "![p](assets/p.png)\n");
    }
}
