//! The vault: plain markdown on disk, three levels (notebook → section → page).
//!
//! Nothing in this module is egui-aware. The layout is
//!
//! ```text
//! ~/Notes/<Notebook>/<Section>/<Page>.md
//! ~/Notes/<Notebook>/<Section>/assets/<pasted images>
//! ~/Notes/.trash/<timestamp>-<Page>.md      (deleted pages, never hard-deleted)
//! ```
//!
//! Each page carries a tiny YAML front-matter block (title/created/updated/order)
//! and is otherwise ordinary markdown, so the vault is greppable, syncable and
//! readable without this app.

use chrono::{DateTime, Local, NaiveDate};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const TRASH: &str = ".trash";
pub const ASSETS: &str = "assets";

/// The longest side a stored picture is allowed to have. The note view makes its display texture at
/// most 1024 px wide (`imgcache::MAX_TEX_DIM`), so this is twice that: enough detail for drawing on
/// a picture, far below what a 4K screenshot costs to decode.
const MAX_ASSET_DIM: u32 = 2048;

#[derive(Clone, Debug)]
pub struct Todo {
    pub page: PathBuf,
    pub page_title: String,
    pub notebook: String,
    pub section: String,
    pub line: usize,
    pub text: String,
    pub done: bool,
    pub due: Option<NaiveDate>,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Page {
    pub path: PathBuf,
    pub notebook: String,
    pub section: String,
    pub title: String,
    pub body: String,
    pub order: f32,
    pub created: Option<DateTime<Local>>,
    pub updated: Option<DateTime<Local>>,
    pub tags: Vec<String>,
    pub todos: Vec<Todo>,
    /// The file could not be read as UTF-8 text. The body is empty and the page must never be
    /// written back from that emptiness — the file holds something this app cannot edit.
    pub unreadable: bool,
}

#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    pub pages: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct Notebook {
    pub name: String,
    pub sections: Vec<Section>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum HitKind {
    Title,
    Tag,
    Body,
}

/// One page that matched, not one match: a page with the word in its title *and* twice in the body is
/// a single result that says so, instead of three rows for the same note.
#[derive(Clone, Debug)]
pub struct Hit {
    pub page: usize,
    /// The strongest of the terms' kinds on this page.
    pub kind: HitKind,
    pub score: i32,
    /// How many matches the query has here, over all terms.
    pub count: usize,
    /// The body line of the best match, when one came from the body.
    pub line: Option<usize>,
    /// The words under the title — the matched line, windowed around the hit.
    pub snippet: String,
    /// Where the matched term sits inside `snippet`, so the row can mark it.
    pub mark: Option<(usize, usize)>,
    /// The term itself, for landing on it when the row is clicked.
    pub needle: String,
}

#[derive(Clone, Debug, Default)]
pub struct Vault {
    pub root: PathBuf,
    pub pages: Vec<Page>,
    pub notebooks: Vec<Notebook>,
    /// Bumped by every `reload`. Anything that caches something *about* the pages (the find bar
    /// caches their texts while a search is open) compares this to know when its copy is stale.
    pub generation: u64,
}

// ------------------------------------------------------------------ text helpers

pub fn count_words(body: &str) -> usize {
    body.split_whitespace().count()
}

/// Extract `#tags` from a body, skipping headings, code spans and fenced code.
pub fn extract_tags(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_code = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        // Only a real ATX heading ("#" or "## " … ) suppresses tag scanning — a
        // line that merely starts with "#word" is a tag, not a heading.
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        let is_heading = (1..=6).contains(&hashes)
            && (trimmed.len() == hashes || trimmed.as_bytes()[hashes] == b' ');
        if in_code || is_heading {
            continue;
        }
        // Walk char boundaries: byte-wise stepping slices multi-byte characters
        // in half (an em dash in a tag-adjacent word used to panic here).
        let mut chars = line.char_indices().peekable();
        let mut in_inline_code = false;
        while let Some((i, c)) = chars.next() {
            if c == '`' {
                in_inline_code = !in_inline_code;
                continue;
            }
            if in_inline_code || c != '#' {
                continue;
            }
            let prev_ok = i == 0
                || line[..i]
                    .chars()
                    .next_back()
                    .map(|p| p == ' ' || p == '(')
                    .unwrap_or(true);
            if !prev_ok {
                continue;
            }
            let mut tag = String::new();
            while let Some((_, nc)) = chars.peek().copied() {
                if nc.is_ascii_alphanumeric() || nc == '-' || nc == '_' || nc == '/' {
                    tag.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            let tag = tag.trim_end_matches('/').to_string();
            if !tag.is_empty() && !out.contains(&tag) {
                out.push(tag);
            }
        }
    }
    out
}

/// One image reference in a page body, in either supported form:
/// `![alt](path)` or a sized `<img src="path" alt="alt" width="320">`.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageRef {
    pub alt: String,
    pub reference: String,
    pub width: Option<u32>,
    /// The kept region of the picture, when the line carries one (`crop="x,y,w,h"`, fractions).
    pub crop: Option<crate::doc::Crop>,
    /// The line exactly as it appears in the file (kept so a body round-trips).
    pub raw: String,
}

/// Serialise an image line. An untouched picture stays plain markdown; a resized **or cropped** one
/// becomes an HTML `<img>`, because neither a width nor a crop fits in `![alt](path)` and both have to
/// survive in every markdown renderer. Markdown has no crop attribute at all, so a cropped picture is
/// always the HTML form — that is also what tells a human reading the file that something was cut.
pub fn image_line(
    reference: &str,
    alt: &str,
    width: Option<u32>,
    crop: Option<crate::doc::Crop>,
) -> String {
    let crop = crop.filter(|c| !c.is_full());
    match (width, crop) {
        (None, None) => format!("![{alt}]({reference})"),
        (w, c) => {
            let mut tag = format!("<img src=\"{reference}\" alt=\"{alt}\"");
            if let Some(w) = w {
                tag.push_str(&format!(" width=\"{w}\""));
            }
            if let Some(c) = c {
                tag.push_str(&format!(" crop=\"{}\"", c.attr()));
            }
            tag.push('>');
            tag
        }
    }
}

/// Pull `src`/`alt`/`width` out of an `<img …>` tag (anywhere in the string).
fn img_attrs(s: &str) -> Option<(String, String, Option<u32>, Option<crate::doc::Crop>)> {
    let lower = s.to_ascii_lowercase();
    let attr = |name: &str| -> Option<String> {
        let key = format!("{name}=");
        let pos = lower.find(&key)?;
        let after = &s[pos + key.len()..];
        let quote = after.chars().next()?;
        if quote == '"' || quote == '\'' {
            let rest = &after[1..];
            let end = rest.find(quote)?;
            Some(rest[..end].to_string())
        } else {
            let end = after
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(after.len());
            Some(after[..end].to_string())
        }
    };
    let src = attr("src")?;
    let crop = attr("crop").and_then(|c| crate::doc::Crop::parse(&c));
    let width = attr("width").and_then(|w| {
        w.trim()
            .trim_end_matches("px")
            .trim_end_matches('%')
            .parse::<u32>()
            .ok()
    });
    Some((src, attr("alt").unwrap_or_default(), width, crop))
}

/// Parse a line that is nothing but an image reference (markdown or `<img>`).
pub fn parse_image_line(line: &str) -> Option<ImageRef> {
    let trimmed = line.trim();
    if trimmed.starts_with("![") {
        let close = trimmed.find("](")?;
        let end = close + trimmed[close..].find(')')?;
        let alt = trimmed[2..close].to_string();
        let reference = trimmed[close + 2..end].trim().to_string();
        if reference.is_empty() || !trimmed[end + 1..].trim().is_empty() {
            return None;
        }
        return Some(ImageRef {
            alt,
            reference,
            width: None,
            crop: None,
            raw: line.to_string(),
        });
    }
    if trimmed.to_ascii_lowercase().starts_with("<img") {
        let (reference, alt, width, crop) = img_attrs(trimmed)?;
        // nothing but the tag on this line?
        let tail = match trimmed.find('>') {
            Some(end) => trimmed[end + 1..].trim(),
            None => return None,
        };
        if !tail.is_empty() {
            return None;
        }
        return Some(ImageRef {
            alt,
            reference,
            width,
            crop,
            raw: line.to_string(),
        });
    }
    None
}

// ------------------------------------------------------------- line-level edits

fn parse_due(text: &str) -> Option<NaiveDate> {
    // @due(YYYY-MM-DD)
    if let Some(start) = text.find("@due(") {
        let rest = &text[start + 5..];
        if let Some(end) = rest.find(')') {
            if let Ok(d) = NaiveDate::parse_from_str(rest[..end].trim(), "%Y-%m-%d") {
                return Some(d);
            }
        }
    }
    // @today / @tomorrow
    if text.contains("@today") {
        return Some(Local::now().date_naive());
    }
    if text.contains("@tomorrow") {
        return Some(Local::now().date_naive() + chrono::Duration::days(1));
    }
    None
}

/// Parse one body line as a todo, if it is one.
pub fn parse_todo_line(line: &str) -> Option<(bool, String)> {
    let trimmed = line.trim_start();
    let rest = ["- ", "* ", "+ "]
        .iter()
        .find_map(|m| trimmed.strip_prefix(*m))?;
    if rest.len() < 3 || rest.as_bytes()[0] != b'[' || rest.as_bytes()[2] != b']' {
        return None;
    }
    let done = matches!(rest.as_bytes()[1], b'x' | b'X');
    let text = rest[3..].trim().to_string();
    Some((done, text))
}

// ------------------------------------------------------------------ front matter

/// Front matter parsing, exposed so the document migration can reuse it.
pub fn parse_front_matter_pub(text: &str) -> (BTreeMap<String, String>, &str) {
    parse_front_matter(text)
}

fn parse_front_matter(text: &str) -> (BTreeMap<String, String>, &str) {
    let mut map = BTreeMap::new();
    if !text.starts_with("---") {
        return (map, text);
    }
    let after = &text[3..];
    let after = after.strip_prefix('\n').unwrap_or(after);
    // Where `after` starts inside `text`. It is 4 after "---\n" but only 3 after "---\r\n":
    // a hard-coded 4 was right for LF files and ate the first character of the body on CRLF ones.
    let base = text.len() - after.len();
    let mut idx = 0usize;
    for line in after.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            // the body begins immediately after this whole line, not after its trimmed
            // content — getting this wrong leaks "---" into the page body.
            let end = (base + idx + line.len()).min(text.len());
            let body = text.get(end..).unwrap_or("");
            let body = body.trim_start_matches(['\n', '\r']);
            return (map, body);
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
        idx += line.len();
    }
    (map, text)
}

fn fm_get<'a>(fm: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    fm.get(key).map(|s| s.as_str())
}

fn parse_dt(s: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Local))
}

pub fn now_string() -> String {
    Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn render_page(title: &str, created: &str, updated: &str, order: f32, body: &str) -> String {
    format!(
        "---\ntitle: {}\ncreated: {}\nupdated: {}\norder: {}\n---\n\n{}\n",
        title.trim(),
        created,
        updated,
        order,
        body.trim_end()
    )
}

// ------------------------------------------------------------------ paths

pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            '\n' | '\r' | '\t' => ' ',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    let mut out: String = cleaned.chars().take(80).collect();
    out = out.trim_end().to_string();
    if out.is_empty() {
        out = "Untitled".to_string();
    }
    out
}

fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    let mut candidate = dir.join(format!("{stem}.md"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} {n}.md"));
        n += 1;
    }
    candidate
}

/// One thing sitting in the vault's `.trash`: a deleted page, or a whole section/notebook folder.
#[derive(Clone, Debug)]
pub struct TrashEntry {
    /// The `.md` file (a page) or the folder (a section/notebook) inside `.trash`.
    pub path: PathBuf,
    /// The name it had before it was deleted, without the timestamp prefix.
    pub label: String,
    /// When it was deleted, as `YYYY-MM-DD HH:MM`; empty when the name carries no stamp.
    pub when: String,
    pub is_dir: bool,
}

/// Split a trash file name into (the original name, when it was deleted).
///
/// `delete_page` and `dir_to_trash` both stamp what they move: `20261019-085754-Alpha.md`,
/// `20261019-085754-Personal`. Anything without that prefix is handed back whole, so a file a user
/// dropped into `.trash` themselves still lists.
pub fn split_trash_name(name: &str) -> (String, String) {
    let b = name.as_bytes();
    let stamped = b.len() > 16
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'-'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'-';
    if !stamped {
        return (name.to_string(), String::new());
    }
    let when = format!(
        "{}-{}-{} {}:{}",
        &name[0..4],
        &name[4..6],
        &name[6..8],
        &name[9..11],
        &name[11..13]
    );
    (name[16..].to_string(), when)
}

/// Absolute path for an asset reference, resolving `assets/x.png` against a page.
pub fn asset_path(page_path: &Path, reference: &str) -> PathBuf {
    let p = Path::new(reference);
    if p.is_absolute() {
        p.to_path_buf()
    } else if let Some(parent) = page_path.parent() {
        parent.join(p)
    } else {
        p.to_path_buf()
    }
}

/// The same, but only when the reference stays inside the page's own `assets/` folder.
///
/// `asset_path` resolves whatever it is given, absolute paths included, because the UI must be
/// able to *show* what a hand-written page asks for. Anything that reads the target and puts its
/// bytes somewhere else (the HTML export inlines pictures as `data:` URIs) has to use this instead:
/// `![x](../../../../etc/hostname)` must not become part of a document that leaves the machine.
pub fn contained_asset_path(page_path: &Path, reference: &str) -> Option<PathBuf> {
    let dir = page_path.parent()?.join(ASSETS);
    let resolved = asset_path(page_path, reference);
    // `canonicalize` so `assets/../../x` and a symlink pointing out are both caught; a picture
    // that does not exist yet still has to pass the lexical check, so fall back to comparing the
    // normalized components when it cannot be resolved.
    let base = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    match fs::canonicalize(&resolved) {
        Ok(real) => real.starts_with(&base).then_some(real),
        Err(_) => {
            let under_assets = resolved
                .components()
                .any(|c| c.as_os_str() == ASSETS)
                && !resolved
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir));
            under_assets.then_some(resolved)
        }
    }
}

// ------------------------------------------------------------------ vault

impl Vault {
    pub fn load(root: impl Into<PathBuf>) -> Self {
        let mut v = Vault {
            root: root.into(),
            ..Default::default()
        };
        let _ = fs::create_dir_all(&v.root);
        v.reload();
        v
    }

    pub fn reload(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pages.clear();
        self.notebooks.clear();

        let mut nb_names: Vec<String> = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.root) {
            for e in entries.flatten() {
                let path = e.path();
                if !path.is_dir() {
                    continue;
                }
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                nb_names.push(name);
            }
        }
        nb_names.sort_by_key(|a| a.to_lowercase());

        for nb in nb_names {
            let nb_dir = self.root.join(&nb);
            let mut sections: Vec<Section> = Vec::new();
            let mut sec_names: Vec<String> = Vec::new();
            if let Ok(entries) = fs::read_dir(&nb_dir) {
                for e in entries.flatten() {
                    let path = e.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') || name == ASSETS {
                        continue;
                    }
                    sec_names.push(name);
                }
            }
            sec_names.sort_by_key(|a| a.to_lowercase());

            for sec in sec_names {
                let sec_dir = nb_dir.join(&sec);
                let mut page_idx: Vec<usize> = Vec::new();
                let mut files: Vec<PathBuf> = Vec::new();
                if let Ok(entries) = fs::read_dir(&sec_dir) {
                    for e in entries.flatten() {
                        let path = e.path();
                        if path.is_file()
                            && path
                                .extension()
                                .map(|x| x.eq_ignore_ascii_case("md") || x.eq_ignore_ascii_case("markdown"))
                                .unwrap_or(false)
                        {
                            files.push(path);
                        }
                    }
                }
                files.sort();
                for f in files {
                    let page = Page::read(&f, &nb, &sec);
                    page_idx.push(self.pages.len());
                    self.pages.push(page);
                }
                // manual order (front matter), then title
                page_idx.sort_by(|a, b| {
                    let pa = &self.pages[*a];
                    let pb = &self.pages[*b];
                    pa.order
                        .partial_cmp(&pb.order)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| pa.title.to_lowercase().cmp(&pb.title.to_lowercase()))
                });
                sections.push(Section {
                    name: sec,
                    pages: page_idx,
                });
            }
            self.notebooks.push(Notebook {
                name: nb,
                sections,
            });
        }
    }

    pub fn page_index(&self, path: &Path) -> Option<usize> {
        self.pages.iter().position(|p| p.path == path)
    }

    pub fn notebook(&self, name: &str) -> Option<&Notebook> {
        self.notebooks.iter().find(|n| n.name == name)
    }

    /// Pages of a section, in manual order.
    pub fn section_pages(&self, nb: &str, sec: &str) -> Vec<usize> {
        self.notebook(nb)
            .and_then(|n| n.sections.iter().find(|s| s.name == sec))
            .map(|s| s.pages.clone())
            .unwrap_or_default()
    }

    pub fn all_todos(&self) -> Vec<Todo> {
        let mut out: Vec<Todo> = self
            .pages
            .iter()
            .flat_map(|p| p.todos.iter().cloned())
            .collect();
        out.sort_by(|a, b| match (a.due, b.due) {
            (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.text.cmp(&b.text)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.text.cmp(&b.text),
        });
        out
    }

    pub fn all_tags(&self) -> Vec<(String, usize)> {
        let mut map: BTreeMap<String, usize> = BTreeMap::new();
        for p in &self.pages {
            for t in &p.tags {
                *map.entry(t.clone()).or_insert(0) += 1;
            }
        }
        let mut out: Vec<(String, usize)> = map.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }

    /// Search the vault. Every whitespace-separated term must match somewhere on the page (a tag, the
    /// title, or the body), so "compost heap" finds the line where both words appear however far
    /// apart. `#tag` is a tag term and only looks at tags. One hit per page — with how many matches it
    /// has, the line the best body match sits on, and that line's words with the term marked — so a
    /// click can land on it. Returns at most `limit` hits and how many pages matched in total.
    pub fn search(&self, query: &str, limit: usize) -> (Vec<Hit>, usize) {
        let terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        if terms.is_empty() {
            return (Vec::new(), 0);
        }
        let mut hits: Vec<Hit> = self
            .pages
            .iter()
            .enumerate()
            .filter_map(|(i, p)| page_hit(i, p, &terms))
            .collect();
        let total = hits.len();
        // score first, then the page order so equal scores do not shuffle between frames
        hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.page.cmp(&b.page)));
        hits.truncate(limit);
        (hits, total)
    }
}

/// The words a line shows: a due date is `@due(2026-10-19)` in the file and "19 Oct 2026" on screen,
/// so searching for the date the user can read has to look at the second one.
fn shown_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find("@due(") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 5..];
        match after.find(')') {
            Some(close) => {
                let tok = &after[..close];
                let words = chrono::NaiveDate::parse_from_str(tok.trim(), "%Y-%m-%d")
                    .ok()
                    .map(|d| d.format("%d %b %Y").to_string())
                    .unwrap_or_else(|| tok.to_string());
                out.push_str(&words);
                rest = &after[close + 1..];
            }
            None => {
                out.push_str("@due(");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Case-insensitive occurrence search over an already-lowercased needle; returns byte offsets into
/// `hay` for every occurrence.
fn occurrences(hay: &str, needle_l: &str) -> Vec<usize> {
    if needle_l.is_empty() {
        return Vec::new();
    }
    let lowered = hay.to_lowercase();
    // only trust the offsets when lowering kept the byte layout (true for ASCII and most text)
    if lowered.len() != hay.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(at) = lowered[from..].find(needle_l) {
        let abs = from + at;
        if hay.is_char_boundary(abs) && hay.is_char_boundary(abs + needle_l.len()) {
            out.push(abs);
        }
        from = abs + needle_l.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        if from >= lowered.len() {
            break;
        }
    }
    out
}

/// One page's result, or `None` when a term matches nowhere on it.
fn page_hit(index: usize, p: &Page, terms: &[String]) -> Option<Hit> {
    let title_l = p.title.to_lowercase();
    let tags_l: Vec<String> = p.tags.iter().map(|t| t.to_lowercase()).collect();
    let lines: Vec<String> = p.body.split('\n').map(shown_line).collect();
    // every occurrence of every term, per line, so the marked words can be the whole phrase
    let mut found = vec![false; terms.len()];
    let mut hits: Vec<Vec<(usize, usize)>> = vec![Vec::new(); lines.len()];
    let mut score = 0i32;
    let mut count = 0usize;
    let mut kind: Option<HitKind> = None;

    for (ti, term) in terms.iter().enumerate() {
        let tag_only = term.starts_with('#');
        let needle = term.trim_start_matches('#');
        if needle.is_empty() {
            return None;
        }
        if let Some(tag) = tags_l.iter().find(|t| t.starts_with(needle)) {
            found[ti] = true;
            score += if tag == needle { 84 } else { 80 };
            count += 1;
            if kind != Some(HitKind::Title) {
                kind = Some(HitKind::Tag);
            }
        }
        if tag_only {
            if !found[ti] {
                return None;
            }
            continue;
        }
        if title_l.contains(needle) {
            found[ti] = true;
            score += if title_l.starts_with(needle) { 120 } else { 90 };
            count += 1;
            if kind != Some(HitKind::Tag) {
                kind = Some(HitKind::Title);
            }
        }
        for (n, line) in lines.iter().enumerate() {
            for at in occurrences(line, needle) {
                found[ti] = true;
                count += 1;
                score += 40;
                hits[n].push((at, at + needle.len()));
                if kind.is_none() {
                    kind = Some(HitKind::Body);
                }
            }
        }
        if !found[ti] {
            return None;
        }
    }

    let kind = kind?;
    // the line to show: the one carrying the most of the query, earliest on a tie
    let mut best: Option<(usize, usize)> = None;
    for (n, hs) in hits.iter().enumerate() {
        if hs.is_empty() {
            continue;
        }
        match best {
            Some((_, b)) if hs.len() <= b => {}
            _ => best = Some((n, hs.len())),
        }
    }
    let (snippet, mark, line, needle) = match best {
        Some((n, _)) => {
            let hs = &hits[n];
            let from = hs.iter().map(|h| h.0).min().unwrap_or(0);
            let to = hs.iter().map(|h| h.1).max().unwrap_or(from);
            let (snippet, mark) = window_snippet(&lines[n], (from, to));
            // the words to land on: the whole marked span, so a phrase query marks the phrase
            let needle = lines[n][from..to].to_string();
            (snippet, mark, Some(n), needle)
        }
        None => match kind {
            HitKind::Tag => (
                p.tags
                    .iter()
                    .map(|t| format!("#{t}"))
                    .collect::<Vec<_>>()
                    .join(" "),
                None,
                None,
                terms
                    .iter()
                    .find(|t| t.starts_with('#'))
                    .map(|t| t.trim_start_matches('#').to_string())
                    .unwrap_or_default(),
            ),
            _ => (p.title.clone(), None, None, terms[0].clone()),
        },
    };
    Some(Hit {
        page: index,
        kind,
        score,
        count,
        line,
        snippet,
        mark,
        needle,
    })
}

/// The marked words, shortened around the match so the marked part is never the part that gets cut.
fn window_snippet(text: &str, mark: (usize, usize)) -> (String, Option<(usize, usize)>) {
    const WINDOW: usize = 90;
    let trimmed = text.trim();
    let lead = text.len() - text.trim_start().len();
    let (s, e) = (mark.0.saturating_sub(lead), mark.1.saturating_sub(lead));
    let chars: Vec<char> = trimmed.chars().collect();
    let to_char = |bytes: usize| trimmed[..bytes.min(trimmed.len())].chars().count();
    let (s_c, e_c) = (to_char(s), to_char(e));
    if chars.len() <= WINDOW {
        return (trimmed.to_string(), Some((s_c, e_c)));
    }
    let start = s_c.saturating_sub(WINDOW / 3);
    let end = (start + WINDOW).min(chars.len());
    let mut out: String = chars[start..end].iter().collect();
    let mut ms = s_c - start;
    let mut me = e_c - start;
    if start > 0 {
        out.insert(0, '…');
        ms += 1;
        me += 1;
    }
    if end < chars.len() {
        out.push('…');
    }
    (out.trim_end().to_string(), Some((ms, me.min(out.chars().count()))))
}

impl Page {
    pub fn read(path: &Path, notebook: &str, section: &str) -> Page {
        let title = || {
            path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "Untitled".into())
        };
        let raw = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => {
                // Not text (or not readable): keep the page in the list, but never pretend it is
                // empty — a save would write that emptiness back over whatever is in the file.
                return Page {
                    path: path.to_path_buf(),
                    notebook: notebook.to_string(),
                    section: section.to_string(),
                    title: title(),
                    body: String::new(),
                    order: 0.0,
                    created: None,
                    updated: None,
                    tags: Vec::new(),
                    todos: Vec::new(),
                    unreadable: true,
                };
            }
        };
        let (fm, body) = parse_front_matter(&raw);
        let title = fm_get(&fm, "title")
            .map(|s| s.to_string())
            .unwrap_or_else(title);
        let order = fm_get(&fm, "order")
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.0);
        let created = fm_get(&fm, "created").and_then(parse_dt);
        let updated = fm_get(&fm, "updated").and_then(parse_dt);
        let tags = extract_tags(body);
        let todos: Vec<Todo> = body
            .split('\n')
            .enumerate()
            .filter_map(|(i, line)| {
                parse_todo_line(line).map(|(done, text)| Todo {
                    page: path.to_path_buf(),
                    page_title: title.clone(),
                    notebook: notebook.to_string(),
                    section: section.to_string(),
                    line: i,
                    due: parse_due(&text),
                    // the same scanner the page bodies use: a tag in inline code is not a tag on
                    // a task row either, and `#tag/` trims the same way in both places
                    tags: extract_tags(&text),
                    done,
                    text: text.trim().to_string(),
                })
            })
            .collect();
        Page {
            path: path.to_path_buf(),
            notebook: notebook.to_string(),
            section: section.to_string(),
            title,
            body: body.trim_end().to_string(),
            order,
            created,
            updated,
            tags,
            todos,
            unreadable: false,
        }
    }
}

// ------------------------------------------------------------------ mutations

pub fn read_page_file(path: &Path) -> (String, String, Option<String>, f32, String) {
    let raw = fs::read_to_string(path).unwrap_or_default();
    let (fm, body) = parse_front_matter(&raw);
    (
        fm_get(&fm, "title")
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "Untitled".into())
            }),
        body.trim_end().to_string(),
        fm_get(&fm, "created").map(|s| s.to_string()),
        fm_get(&fm, "order")
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.0),
        fm_get(&fm, "updated").map(|s| s.to_string()).unwrap_or_default(),
    )
}

/// Refuse to touch a page file the app cannot read as UTF-8 text. `read_page_file` hands back an
/// empty body for such a file, and every writer here rebuilds the whole file from what it read —
/// so without this check one keystroke replaced the original with emptiness.
pub fn page_is_text(path: &Path) -> io::Result<()> {
    match fs::read_to_string(path) {
        Ok(_) => Ok(()),
        Err(e) => Err(io::Error::other(format!(
            "{} is not UTF-8 text ({e}) — left alone",
            path.display()
        ))),
    }
}

/// Atomic write (temp file + rename) so a crash or sync never truncates a page.
pub fn write_page(
    path: &Path,
    title: &str,
    created: Option<&str>,
    order: f32,
    body: &str,
) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let created = created
        .map(|s| s.to_string())
        .unwrap_or_else(now_string);
    let text = render_page(title, &created, &now_string(), order, body);
    let tmp = path.with_extension("md.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Move a directory into the vault's `.trash`, stamped so two deletes never collide.
fn dir_to_trash(root: &Path, dir: &Path, label: &str) -> io::Result<PathBuf> {
    if !dir.is_dir() {
        return Err(io::Error::other(format!("no such folder: {}", dir.display())));
    }
    let trash = root.join(TRASH);
    fs::create_dir_all(&trash)?;
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let stem = sanitize(label);
    let mut target = trash.join(format!("{stamp}-{stem}"));
    let mut n = 2;
    while target.exists() {
        target = trash.join(format!("{stamp}-{stem} {n}"));
        n += 1;
    }
    fs::rename(dir, &target)?;
    Ok(target)
}

pub struct Ops;

impl Ops {
    pub fn create_notebook(root: &Path, name: &str) -> io::Result<PathBuf> {
        let dir = root.join(sanitize(name));
        fs::create_dir_all(dir.join("Notes"))?;
        Ok(dir)
    }

    pub fn create_section(root: &Path, nb: &str, name: &str) -> io::Result<PathBuf> {
        let dir = root.join(sanitize(nb)).join(sanitize(name));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn create_page(root: &Path, nb: &str, sec: &str, title: &str) -> io::Result<PathBuf> {
        let dir = root.join(sanitize(nb)).join(sanitize(sec));
        fs::create_dir_all(&dir)?;
        let path = unique_path(&dir, &sanitize(title));
        write_page(&path, title, None, 0.0, "")?;
        Ok(path)
    }

    /// Rename: rewrites the title in place and renames the file to match, document and all.
    pub fn rename_page(path: &Path, new_title: &str) -> io::Result<PathBuf> {
        page_is_text(path)?;
        let (_, body, created, order, _) = read_page_file(path);
        write_page(path, new_title, created.as_deref(), order, &body)?;
        if let Some(dir) = path.parent() {
            let target = dir.join(format!("{}.md", sanitize(new_title)));
            if target != path {
                if target.exists() {
                    let alt = unique_path(dir, &sanitize(new_title));
                    fs::rename(path, &alt)?;
                    crate::doc::move_doc(path, &alt);
                    return Ok(alt);
                }
                fs::rename(path, &target)?;
                crate::doc::move_doc(path, &target);
                return Ok(target);
            }
        }
        Ok(path.to_path_buf())
    }

    pub fn delete_page(root: &Path, path: &Path) -> io::Result<PathBuf> {
        let trash = root.join(TRASH);
        fs::create_dir_all(&trash)?;
        let stamp = Local::now().format("%Y%m%d-%H%M%S");
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "page.md".into());
        // A name already in the trash would be *overwritten* by `rename`: delete the same page
        // twice inside one second (delete, restore, delete again) and the first copy — with its
        // `.note` — was silently replaced by the second.
        // The counter goes before the extension, not before the whole name: the trash entry has to
        // keep the `<stamp>-<label>` shape `split_trash_name` reads (and the `.note` keeps the stem).
        let mut target = trash.join(format!("{stamp}-{name}"));
        let stem = name.trim_end_matches(".md").to_string();
        let mut n = 2;
        while target.exists() || target.with_extension("note").exists() {
            target = trash.join(format!("{stamp}-{stem} {n}.md"));
            n += 1;
        }
        fs::rename(path, &target)?;
        // the document goes with it: left behind, it would be picked up by the next page that
        // takes this name (the trash name keeps the page's stem, so the pair stays recognisable)
        crate::doc::move_doc(path, &target);
        Ok(target)
    }

    /// Delete a whole notebook: its directory (pages, pictures, documents) moves into `.trash`.
    /// Nothing is removed, so the notebook stays recoverable as a unit.
    pub fn delete_notebook(root: &Path, name: &str) -> io::Result<PathBuf> {
        dir_to_trash(root, &root.join(sanitize(name)), name)
    }

    /// Delete one section of a notebook, the same way.
    pub fn delete_section(root: &Path, nb: &str, sec: &str) -> io::Result<PathBuf> {
        dir_to_trash(root, &root.join(sanitize(nb)).join(sanitize(sec)), sec)
    }

    pub fn move_page(path: &Path, root: &Path, nb: &str, sec: &str) -> io::Result<PathBuf> {
        let dir = root.join(sanitize(nb)).join(sanitize(sec));
        fs::create_dir_all(&dir)?;
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "page.md".into());
        let mut target = dir.join(&name);
        if target.exists() {
            target = unique_path(&dir, &name.trim_end_matches(".md"));
        }
        fs::rename(path, &target)?;
        crate::doc::move_doc(path, &target);
        Ok(target)
    }

    /// Copy a page beside itself: a new file, the same words, its own document.
    ///
    /// The copy is named "<title> copy" (then "<title> copy 2") and lands straight after the
    /// original in the section's order, so it appears where the original does instead of at the
    /// bottom of a long list. `created` is inherited — the words are as old as the page they came
    /// from — while `updated` is now, because the copy is new.
    pub fn duplicate_page(path: &Path) -> io::Result<PathBuf> {
        page_is_text(path)?;
        let (title, body, created, order, _) = read_page_file(path);
        let dir = path.parent().unwrap_or(Path::new("."));
        let new_title = format!("{title} copy");
        let target = unique_path(dir, &sanitize(&new_title));
        // Half a step after the original: sorts directly below it, and still leaves room for a
        // second copy between them.
        write_page(&target, &new_title, created.as_deref(), order + 0.5, &body)?;
        // The document is what the editor reads back; copying only the markdown would drop every
        // run property the export cannot express (bold, marks, picture widths).
        if let Some(d) = crate::doc::load(path) {
            // failing here would leave a copy that has lost every run property the markdown cannot
            // express, so the error is reported rather than swallowed
            crate::doc::save(&target, &d)?;
        }
        Ok(target)
    }

    /// Everything sitting in `.trash`, newest first.
    pub fn trash_entries(root: &Path) -> Vec<TrashEntry> {
        let trash = root.join(TRASH);
        let mut out = Vec::new();
        let Ok(read) = fs::read_dir(&trash) else {
            return out;
        };
        for e in read.flatten() {
            let path = e.path();
            let Some(name) = path.file_name().map(|s| s.to_string_lossy().to_string()) else {
                continue;
            };
            // `.note` documents travel with their page, and `assets` directories travel inside a
            // deleted section: neither is a thing the user deleted, so neither is offered back.
            if name.ends_with(".note") || name == ASSETS {
                continue;
            }
            let is_dir = path.is_dir();
            if !is_dir && !name.ends_with(".md") {
                continue;
            }
            let (label, when) = split_trash_name(&name);
            out.push(TrashEntry {
                path,
                label,
                when,
                is_dir,
            });
        }
        // newest first: the stamp sorts lexicographically, and an entry without one falls back to
        // its label so the order is still stable
        out.sort_by(|a, b| b.when.cmp(&a.when).then_with(|| a.label.cmp(&b.label)));
        out
    }

    /// Put a deleted page back, into `nb / sec`, under its own name (or a free variant of it).
    ///
    /// The trash keeps the page's stem but not where it came from, so the caller says where it
    /// should land. The `.note` document comes back with it: restoring only the markdown would
    /// hand the page to `from_markdown` on open and lose every styled run.
    pub fn restore_page(root: &Path, trashed: &Path, nb: &str, sec: &str) -> io::Result<PathBuf> {
        let dir = root.join(sanitize(nb)).join(sanitize(sec));
        fs::create_dir_all(&dir)?;
        let name = trashed
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "page.md".into());
        let (label, _) = split_trash_name(&name);
        let stem = label.trim_end_matches(".md").trim();
        let stem = if stem.is_empty() { "Restored page" } else { stem };
        let mut target = dir.join(format!("{stem}.md"));
        if target.exists() {
            target = unique_path(&dir, stem);
        }
        fs::rename(trashed, &target)?;
        crate::doc::move_doc(trashed, &target);
        Ok(target)
    }

    /// Put a deleted notebook or section back at the top of the vault, under its own name (or a
    /// free variant of it). Returns where it landed.
    pub fn restore_folder(root: &Path, trashed: &Path) -> io::Result<PathBuf> {
        let name = trashed
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let (label, _) = split_trash_name(&name);
        let label = sanitize(if label.trim().is_empty() { "Restored" } else { label.trim() });
        let mut target = root.join(&label);
        let mut n = 2;
        while target.exists() {
            target = root.join(format!("{label} {n}"));
            n += 1;
        }
        fs::rename(trashed, &target)?;
        Ok(target)
    }

    /// Reorder inside a section by swapping the `order` value with a neighbour.
    pub fn reorder(path: &Path, neighbour: &Path) -> io::Result<()> {
        page_is_text(path)?;
        page_is_text(neighbour)?;
        let (t1, b1, c1, o1, _) = read_page_file(path);
        let (t2, b2, c2, o2, _) = read_page_file(neighbour);
        let mid = (o1 + o2) / 2.0;
        let (n1, n2) = if (o1 - o2).abs() < f32::EPSILON {
            (mid - 0.5, mid + 0.5)
        } else if o1 < o2 {
            (o2, o1.min(mid))
        } else {
            (o2, o1.max(mid))
        };
        write_page(path, &t1, c1.as_deref(), n1, &b1)?;
        write_page(neighbour, &t2, c2.as_deref(), n2, &b2)?;
        Ok(())
    }

    pub fn set_order(path: &Path, order: f32) -> io::Result<()> {
        page_is_text(path)?;
        let (t, b, c, _, _) = read_page_file(path);
        write_page(path, &t, c.as_deref(), order, &b)
    }

    /// Quick capture: append a timestamped bullet to today's Inbox page.
    pub fn capture(root: &Path, text: &str) -> io::Result<PathBuf> {
        let day = Local::now().format("%Y-%m-%d").to_string();
        let dir = root.join("Inbox").join("Inbox");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{day}.md"));
        let (_, mut body, created, order, _) = if path.exists() {
            page_is_text(&path)?;
            read_page_file(&path)
        } else {
            (
                day.clone(),
                format!("## Captured ideas\n"),
                None,
                -1_000_000_000.0,
                String::new(),
            )
        };
        let stamp = Local::now().format("%H:%M");
        if !body.ends_with('\n') {
            body.push('\n');
        }
        // A captured line that starts with a checkbox marker is a task, not an
        // idea: write it as a real markdown checkbox so it shows up in the
        // tasks view instead of as a bullet.
        let text = text.trim();
        let as_task = ["[ ]", "[]", "[x]", "[X]"]
            .iter()
            .find_map(|m| text.strip_prefix(*m).map(|r| r.trim().to_string()));
        match as_task {
            Some(rest) => body.push_str(&format!("- [ ] {rest}\n")),
            None => body.push_str(&format!("- {stamp} {text}\n")),
        }
        write_page(&path, &day, created.as_deref(), order, &body)?;
        Ok(path)
    }

    /// Save a pasted image next to the page and return the markdown reference.
    pub fn save_asset(page_path: &Path, image: &image::RgbaImage) -> io::Result<String> {
        Self::save_asset_named(page_path, image, "paste")
    }

    /// Same, with a prefix that says where the picture came from (`paste`, `snip`).
    pub fn save_asset_named(
        page_path: &Path,
        image: &image::RgbaImage,
        prefix: &str,
    ) -> io::Result<String> {
        let dir = page_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(ASSETS);
        fs::create_dir_all(&dir)?;
        // milliseconds: two pastes inside the same second must not land on one name
        let stamp = Local::now().format("%Y%m%d-%H%M%S%3f");
        let name = format!("{prefix}-{stamp}.png");
        let target = dir.join(&name);
        // A pasted picture is stored at a sane size rather than at whatever the clipboard held: the
        // note view never draws one wider than `imgcache::MAX_TEX_DIM`, and a 3072² screenshot is a
        // 13 MB PNG that costs every later decode. 2× the display cap keeps the detail zooming and
        // drawing on a picture needs.
        let scaled;
        let image = if image.width().max(image.height()) > MAX_ASSET_DIM {
            let (w, h) = (image.width(), image.height());
            let scale = MAX_ASSET_DIM as f32 / w.max(h) as f32;
            let (nw, nh) = (
                ((w as f32 * scale).round()).max(1.0) as u32,
                ((h as f32 * scale).round()).max(1.0) as u32,
            );
            scaled = image::imageops::resize(image, nw, nh, image::imageops::FilterType::CatmullRom);
            &scaled
        } else {
            image
        };
        image
            .save_with_format(&target, image::ImageFormat::Png)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(format!("{ASSETS}/{name}"))
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use std::fs;

    fn page(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(
            &p,
            format!("---\ntitle: {}\norder: 0\n---\n\n{body}", name.trim_end_matches(".md")),
        )
        .unwrap();
        p
    }

    fn vault_with(pages: &[(&str, &str)]) -> Vault {
        // a name per test: two tests sharing one directory wiped each other under `cargo test`
        let dir = std::env::temp_dir().join(format!(
            "notecraft-search-{}-{}-{}",
            std::process::id(),
            pages.len(),
            pages.first().map(|p| p.0).unwrap_or("empty")
        ));
        let _ = fs::remove_dir_all(&dir);
        let sec = dir.join("Work").join("Notes");
        fs::create_dir_all(&sec).unwrap();
        for (name, body) in pages {
            page(&sec, name, body);
        }
        Vault::load(&dir)
    }

    #[test]
    fn a_page_is_one_result_however_many_times_it_matches() {
        let v = vault_with(&[
            ("compost plan.md", "The compost heap needs turning.\n"),
            ("garden notes.md", "Spread the compost in spring.\n"),
        ]);
        let (hits, total) = v.search("compost", 60);
        assert_eq!(total, 2, "two pages — not three hits for one of them");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].page, 0, "the title match ranks first");
        assert_eq!(hits[0].kind, HitKind::Title);
        assert_eq!(hits[0].count, 2, "title + body on the same page");
        let second = &hits[1];
        assert_eq!(second.kind, HitKind::Body);
        assert!(second.snippet.contains("Spread the compost"));
        assert_eq!(second.line, Some(0));
        // the marked words are the words that matched
        let (a, b) = second.mark.expect("a body hit marks its term");
        let chars: Vec<char> = second.snippet.chars().collect();
        assert_eq!(chars[a..b].iter().collect::<String>(), "compost");
    }

    #[test]
    fn every_term_has_to_match_and_tag_terms_only_look_at_tags() {
        let v = vault_with(&[
            ("beds.md", "compost for the beds\n#garden\n"),
            ("heap.md", "compost for the heap\n"),
        ]);
        let (hits, total) = v.search("compost beds", 60);
        assert_eq!(total, 1, "both words have to be on the page");
        assert_eq!(hits.len(), 1);
        // a tag term is tags only: `#garden` matches no body
        let (hits, total) = v.search("#garden", 60);
        assert_eq!(total, 1);
        assert_eq!(hits[0].kind, HitKind::Tag);
        // …and a tag term that is not there finds nothing, even though the word is in the body
        let (_, total) = v.search("#compost", 60);
        assert_eq!(total, 0);
    }

    #[test]
    fn the_date_a_note_shows_is_searchable_not_just_the_file_token() {
        let v = vault_with(&[("due.md", "- [ ] ring the plumber @due(2026-10-19)\n")]);
        let (hits, total) = v.search("19 Oct 2026", 60);
        assert_eq!(total, 1, "the chip the reader sees is what they search for");
        assert_eq!(hits[0].kind, HitKind::Body);
        assert!(hits[0].snippet.contains("19 Oct 2026"), "{:?}", hits[0].snippet);
        let chars: Vec<char> = hits[0].snippet.chars().collect();
        let (a, b) = hits[0].mark.unwrap();
        assert_eq!(chars[a..b].iter().collect::<String>(), "19 Oct 2026");
    }

    #[test]
    fn the_total_survives_the_limit() {
        let pages: Vec<(&str, &str)> = (0..5).map(|i| ("x.md", "needle here\n")).collect();
        let _ = pages;
        let dir = std::env::temp_dir().join(format!("notecraft-search-many-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let sec = dir.join("Work").join("Notes");
        fs::create_dir_all(&sec).unwrap();
        for i in 0..5 {
            page(&sec, &format!("p{i}.md"), "needle here\n");
        }
        let v = Vault::load(&dir);
        let (hits, total) = v.search("needle", 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(total, 5, "the caller can say how many it is holding back");
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("notecraft-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn a_pasted_picture_is_stored_at_a_sane_size() {
        // A clipboard can hold a 4K screenshot; storing it as-is makes a page that costs 13 MB and
        // a couple of hundred milliseconds to decode every time it is opened. The view never draws
        // wider than 1024 px, so pastes are written at twice that.
        let dir = tmpdir("asset-big");
        let page = dir.join("Page.md");
        fs::write(&page, "---\ntitle: Page\n---\n\n").unwrap();
        let big = image::RgbaImage::from_pixel(3072, 2400, image::Rgba([1, 2, 3, 255]));
        let reference = Ops::save_asset(&page, &big).unwrap();
        let stored = image::open(dir.join(&reference)).unwrap();
        assert_eq!((stored.width(), stored.height()), (2048, 1600));

        // a picture already inside the limit keeps every pixel
        let small = image::RgbaImage::from_pixel(640, 480, image::Rgba([1, 2, 3, 255]));
        let reference = Ops::save_asset(&page, &small).unwrap();
        let stored = image::open(dir.join(&reference)).unwrap();
        assert_eq!((stored.width(), stored.height()), (640, 480));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn multibyte_lines_do_not_break_the_image_parser() {
        // Regression: the image scan used to slice by bytes, so a line starting with a multi-byte
        // character killed the app ("not a char boundary"). The page itself is the one that scans
        // now (read mode renders the document), so this pins the parser those lines reach.
        for line in [
            "— an idea — with dashes",
            "— another ![a](p1.png) inline",
            "<img src=\"p2.png\">",
            "![— shot —](assets/x.png)",
        ] {
            let _ = parse_image_line(line);
            let _ = parse_todo_line(line);
            let _ = extract_tags(line);
        }
        // and the two real forms still parse
        assert!(parse_image_line("![a](p1.png)").is_some());
        assert!(parse_image_line("<img src=\"p2.png\">").is_some());
    }

    #[test]
    fn image_lines_parse_and_resize_round_trip() {
        // a resized image is written as <img … width="…"> and parses back
        let sized = image_line("assets/shot.png", "shot", Some(320), None);
        assert_eq!(sized, "<img src=\"assets/shot.png\" alt=\"shot\" width=\"320\">");
        let parsed = parse_image_line(&sized).expect("sized image parses");
        assert_eq!(parsed.reference, "assets/shot.png");
        assert_eq!(parsed.alt, "shot");
        assert_eq!(parsed.width, Some(320));
        // single quotes and no-width also parse
        let q = parse_image_line("<img alt='x' src='/tmp/a.png'>").unwrap();
        assert_eq!(q.reference, "/tmp/a.png");
        assert_eq!(q.width, None);
        // an inline picture on a text line is still a picture line's business, not this one's
        assert!(parse_image_line("text ![a](p1.png)").is_none());

    /// A crop rides on the line as `crop="x,y,w,h"` and comes back as fractions. It is enough on its own
    /// to make the line an `<img>` — markdown has nowhere to put a crop — and the whole picture stays
    /// plain markdown.
    #[test]
    fn a_crop_round_trips_on_the_image_line() {
        let crop = crate::doc::Crop {
            x: 0.25,
            y: 0.1,
            w: 0.5,
            h: 0.6,
        };
        let line = image_line("assets/p.png", "p", None, Some(crop));
        assert_eq!(
            line,
            "<img src=\"assets/p.png\" alt=\"p\" crop=\"0.2500,0.1000,0.5000,0.6000\">"
        );
        let parsed = parse_image_line(&line).expect("a cropped image parses");
        assert_eq!(parsed.crop, Some(crop));
        assert_eq!(parsed.width, None);
        // width and crop together
        let both = image_line("assets/p.png", "p", Some(300), Some(crop));
        let parsed = parse_image_line(&both).expect("a sized, cropped image parses");
        assert_eq!((parsed.width, parsed.crop), (Some(300), Some(crop)));
        // the whole picture is not a crop
        assert_eq!(
            image_line("assets/p.png", "p", None, Some(crate::doc::Crop::FULL)),
            "![p](assets/p.png)"
        );
        // a malformed or full-range attribute is the whole picture, never an unshowable one
        assert_eq!(
            parse_image_line("<img src=\"a.png\" crop=\"1,2\">")
                .unwrap()
                .crop,
            None
        );
        assert_eq!(
            parse_image_line("<img src=\"a.png\" crop=\"0,0,1,1\">")
                .unwrap()
                .crop,
            None
        );
        assert!(parse_image_line("<img src=\"a.png\" crop=\"0.1,0.1,0.5,0.5\">")
            .unwrap()
            .crop
            .is_some());
    }

    }

    #[test]
    fn a_crlf_page_keeps_the_first_character_of_its_body() {
        // the body offset was `4 + idx + line.len()`, which only holds when the file starts with
        // "---\n": on a CRLF file the first character of the body was eaten on every read
        let text = "---\r\ntitle: T\r\norder: 1\r\n---\r\nfirst line\r\n";
        let (fm, body) = parse_front_matter(text);
        assert_eq!(fm.get("title").map(|s| s.as_str()), Some("T"));
        assert_eq!(body, "first line\r\n");
        // and the LF form is unchanged, with or without a blank line after the fence
        assert_eq!(parse_front_matter("---\ntitle: T\n---\nfirst line\n").1, "first line\n");
        assert_eq!(parse_front_matter("---\ntitle: T\n---\n\nfirst line\n").1, "first line\n");
    }

    #[test]
    fn front_matter_round_trip_does_not_leak_the_fence() {
        let body = "## Captured ideas\n- 15:16 a thought — with an em dash\n";
        let dir = tmpdir("fm");
        let page = dir.join("Page.md");
        write_page(&page, "Page", None, 3.0, body).unwrap();
        let (title, read_back, created, order, _) = read_page_file(&page);
        assert_eq!(title, "Page");
        assert_eq!(read_back, body.trim_end());
        assert_eq!(order, 3.0);
        assert!(created.is_some());
        // no stray fence, no front matter in the body
        assert!(!read_back.contains("---"));
        assert!(!read_back.contains("title:"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_todos_due_dates_and_tags() {
        let body = "- [ ] ring the plumber @due(2026-10-20) #home #urgent\n- [x] done thing\n* [ ] star bullet @tomorrow\nplain text\n";
        let todos: Vec<(bool, String)> = body
            .split('\n')
            .filter_map(parse_todo_line)
            .collect();
        assert_eq!(todos.len(), 3);
        assert!(!todos[0].0);
        assert!(todos[1].0);
        assert_eq!(
            parse_due("- [ ] x @due(2026-10-20)"),
            NaiveDate::from_ymd_opt(2026, 10, 20)
        );
        assert_eq!(parse_due("- [ ] x @today"), Some(Local::now().date_naive()));
        let tags = extract_tags(body);
        assert_eq!(tags, vec!["home".to_string(), "urgent".to_string()]);
        // a task row is scanned by the same scanner: a tag inside inline code is not a tag
        let (_, text) = parse_todo_line("- [ ] see `#notatag` and #real\n").unwrap();
        assert_eq!(extract_tags(&text), vec!["real".to_string()]);
    }

    #[test]
    fn tags_survive_multibyte_text_and_skip_code() {
        let body = "an idea — see #project/alpha\n`#notatag`\n```\n#notatag either\n```\n#final";
        let tags = extract_tags(body);
        assert_eq!(tags, vec!["project/alpha".to_string(), "final".to_string()]);
    }

    #[test]
    fn capture_writes_an_idea_as_a_bullet_and_a_task_as_a_checkbox() {
        let dir = tmpdir("capture");
        let root = dir.join("vault");
        fs::create_dir_all(&root).unwrap();
        let path = Ops::capture(&root, "an idea").unwrap();
        let (_, body, _, _, _) = read_page_file(&path);
        assert!(body.contains("an idea"));
        assert!(body.lines().any(|l| l.starts_with("- ")));
        assert!(!body.lines().any(|l| l.starts_with("- [")));

        Ops::capture(&root, "[ ] a task @due(2026-11-01)").unwrap();
        let (_, body, _, _, _) = read_page_file(&path);
        assert!(body.contains("- [ ] a task @due(2026-11-01)"));
        let v = Vault::load(&root);
        let all = v.all_todos();
        let open: Vec<&Todo> = all.iter().filter(|t| !t.done).collect();
        assert_eq!(open.len(), 1);
        assert_eq!(
            open[0].due,
            NaiveDate::from_ymd_opt(2026, 11, 1)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn vault_scans_three_levels_and_searches() {
        let dir = tmpdir("vault");
        let root = dir.join("vault");
        Ops::create_notebook(&root, "Personal").unwrap();
        Ops::create_section(&root, "Personal", "Notes").unwrap();
        let p = Ops::create_page(&root, "Personal", "Notes", "Shopping").unwrap();
        write_page(&p, "Shopping", None, 1.0, "milk and #groceries").unwrap();
        let v = Vault::load(&root);
        assert_eq!(v.notebooks.len(), 1);
        assert_eq!(v.pages.len(), 1);
        assert_eq!(v.pages[0].title, "Shopping");
        assert_eq!(v.pages[0].tags, vec!["groceries".to_string()]);
        let (hits, total) = v.search("milk", 10);
        assert_eq!(total, 1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, HitKind::Body);
        let (tag_hits, tag_total) = v.search("#groceries", 10);
        assert_eq!(tag_total, 1);
        assert_eq!(tag_hits.len(), 1);
        assert_eq!(tag_hits[0].kind, HitKind::Tag);
        // delete must move to .trash, never unlink
        let trashed = Ops::delete_page(&root, &p).unwrap();
        assert!(trashed.exists());
        assert!(trashed.starts_with(root.join(TRASH)));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_moves_the_file_with_the_title() {
        let dir = tmpdir("rename");
        let root = dir.join("vault");
        Ops::create_notebook(&root, "N").unwrap();
        Ops::create_section(&root, "N", "S").unwrap();
        let p = Ops::create_page(&root, "N", "S", "Old name").unwrap();
        // give the page a document, as every migrated page has one
        crate::doc::save(&p, &crate::doc::from_markdown("- [ ] a task\n")).unwrap();
        let moved = Ops::rename_page(&p, "New name").unwrap();
        assert_eq!(moved.file_name().unwrap(), "New name.md");
        let (title, _, _, _, _) = read_page_file(&moved);
        assert_eq!(title, "New name");
        assert!(!p.exists());
        // the document moved with it: left behind, a later page of that name would inherit it
        assert!(!crate::doc::doc_path(&p).exists(), "the old document must not stay behind");
        assert!(crate::doc::load(&moved).is_some(), "the document must arrive under the new name");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_page_that_is_not_utf8_text_is_listed_but_never_rewritten() {
        let dir = tmpdir("binary");
        let root = dir.join("vault");
        Ops::create_notebook(&root, "N").unwrap();
        Ops::create_section(&root, "N", "S").unwrap();
        let p = root.join("N").join("S").join("Binary.md");
        let raw: &[u8] = b"---\ntitle: Binary\norder: 1\n---\n\xff\xfe not text\n";
        fs::write(&p, raw).unwrap();

        let page = Page::read(&p, "N", "S");
        assert!(page.unreadable, "the page says it could not be read");
        assert!(page.body.is_empty(), "and its body is empty — but it is not really empty");
        assert_eq!(page.title, "Binary");
        // every writer refuses it, so the emptiness never reaches the file
        assert!(page_is_text(&p).is_err());
        assert!(Ops::set_order(&p, 2.0).is_err());
        assert!(Ops::rename_page(&p, "Renamed").is_err());
        assert_eq!(fs::read(&p).unwrap(), raw, "the file is untouched");
        // `capture` appends to today's Inbox page — never to one it cannot read
        assert!(Ops::capture(&root, "an idea").is_ok(), "a new page is fine");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_a_notebook_or_a_section_moves_the_whole_folder_to_the_trash() {
        let dir = tmpdir("nbdelete");
        let root = dir.join("vault");
        Ops::create_notebook(&root, "Doomed").unwrap();
        Ops::create_section(&root, "Doomed", "Notes").unwrap();
        let p = Ops::create_page(&root, "Doomed", "Notes", "Inside").unwrap();
        write_page(&p, "Inside", None, 1.0, "words\n").unwrap();
        crate::doc::save(&p, &crate::doc::from_markdown("words\n")).unwrap();
        fs::create_dir_all(p.parent().unwrap().join(ASSETS)).unwrap();
        fs::write(p.parent().unwrap().join(ASSETS).join("x.png"), b"png").unwrap();

        let trashed = Ops::delete_notebook(&root, "Doomed").unwrap();
        assert!(trashed.is_dir() && trashed.starts_with(root.join(TRASH)));
        assert!(!root.join("Doomed").exists(), "gone from the vault");
        // page, markdown, document and picture travelled together
        assert!(trashed.join("Notes").join("Inside.md").is_file());
        assert!(trashed.join("Notes").join("Inside.note").is_file());
        assert!(trashed.join("Notes").join(ASSETS).join("x.png").is_file());

        // a section on its own, leaving its notebook behind
        Ops::create_notebook(&root, "Keep").unwrap();
        Ops::create_section(&root, "Keep", "Gone").unwrap();
        Ops::create_page(&root, "Keep", "Gone", "P").unwrap();
        let first = Ops::delete_section(&root, "Keep", "Gone").unwrap();
        assert!(first.starts_with(root.join(TRASH)));
        assert!(!root.join("Keep").join("Gone").exists());
        assert!(root.join("Keep").is_dir(), "the notebook survives");
        // two deletes of the same name inside one second must not collide
        Ops::create_section(&root, "Keep", "Gone").unwrap();
        let second = Ops::delete_section(&root, "Keep", "Gone").unwrap();
        assert_ne!(second, first);
        assert!(second.is_dir());
        // deleting something that is not there is an error, not a silent success
        assert!(Ops::delete_notebook(&root, "Never existed").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deleted_page_takes_its_document_to_the_trash() {
        let dir = tmpdir("delete");
        let root = dir.join("vault");
        Ops::create_notebook(&root, "N").unwrap();
        Ops::create_section(&root, "N", "S").unwrap();
        let p = Ops::create_page(&root, "N", "S", "Doomed").unwrap();
        crate::doc::save(&p, &crate::doc::from_markdown("goodbye\n")).unwrap();
        let trashed = Ops::delete_page(&root, &p).unwrap();
        assert!(trashed.exists());
        assert!(!crate::doc::doc_path(&p).exists(), "the document must not stay behind to be inherited");
        let doc_in_trash = trashed.with_extension(crate::doc::EXT);
        assert!(doc_in_trash.exists(), "it is in the trash beside its page: {doc_in_trash:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_duplicate_is_a_second_page_with_the_same_words_and_its_own_document() {
        let dir = tmpdir("dup");
        let root = dir.join("vault");
        Ops::create_notebook(&root, "N").unwrap();
        Ops::create_section(&root, "N", "S").unwrap();
        let p = Ops::create_page(&root, "N", "S", "Recipe").unwrap();
        write_page(&p, "Recipe", None, 3.0, "flour\n").unwrap();
        crate::doc::save(&p, &crate::doc::from_markdown("flour\n")).unwrap();

        let copy = Ops::duplicate_page(&p).unwrap();
        assert_eq!(copy.file_name().unwrap().to_string_lossy(), "Recipe copy.md");
        let (title, body, _, order, _) = read_page_file(&copy);
        assert_eq!(title, "Recipe copy");
        assert_eq!(body, "flour");
        assert_eq!(order, 3.5, "the copy sorts right below the original");
        assert!(crate::doc::doc_path(&copy).exists(), "the document comes with it");
        // the original is untouched
        assert_eq!(read_page_file(&p).0, "Recipe");
        // a second copy takes a free name rather than overwriting the first
        let second = Ops::duplicate_page(&p).unwrap();
        assert_eq!(second.file_name().unwrap().to_string_lossy(), "Recipe copy 2.md");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deleted_page_can_be_found_in_the_trash_and_put_back() {
        let dir = tmpdir("restore");
        let root = dir.join("vault");
        Ops::create_notebook(&root, "N").unwrap();
        Ops::create_section(&root, "N", "S").unwrap();
        let p = Ops::create_page(&root, "N", "S", "Lost then found").unwrap();
        write_page(&p, "Lost then found", None, 0.0, "the words\n").unwrap();
        crate::doc::save(&p, &crate::doc::from_markdown("the words\n")).unwrap();
        Ops::delete_page(&root, &p).unwrap();

        let entries = Ops::trash_entries(&root);
        assert_eq!(entries.len(), 1, "one page, and never its .note: {entries:?}");
        assert_eq!(entries[0].label, "Lost then found.md");
        assert!(!entries[0].is_dir);
        assert_eq!(entries[0].when.len(), 16, "YYYY-MM-DD HH:MM: {:?}", entries[0].when);

        let back = Ops::restore_page(&root, &entries[0].path, "N", "S").unwrap();
        assert_eq!(back.file_name().unwrap().to_string_lossy(), "Lost then found.md");
        assert_eq!(read_page_file(&back).1, "the words");
        assert!(crate::doc::doc_path(&back).exists(), "the document comes back too");
        assert!(Ops::trash_entries(&root).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deleted_section_can_be_put_back_as_a_folder() {
        let dir = tmpdir("restore-dir");
        let root = dir.join("vault");
        Ops::create_notebook(&root, "N").unwrap();
        Ops::create_section(&root, "N", "Gone").unwrap();
        Ops::create_page(&root, "N", "Gone", "Inner").unwrap();
        Ops::delete_section(&root, "N", "Gone").unwrap();
        let entries = Ops::trash_entries(&root);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].label, "Gone");
        let back = Ops::restore_folder(&root, &entries[0].path).unwrap();
        assert_eq!(back.file_name().unwrap().to_string_lossy(), "Gone");
        assert!(back.join("Inner.md").exists());
        // a second folder of the same name is not allowed to overwrite the first one
        Ops::create_section(&root, "N", "Gone").unwrap();
        Ops::create_page(&root, "N", "Gone", "Second").unwrap();
        Ops::delete_section(&root, "N", "Gone").unwrap();
        let again = Ops::trash_entries(&root);
        assert_eq!(again.len(), 1);
        let back2 = Ops::restore_folder(&root, &again[0].path).unwrap();
        assert_eq!(back2.file_name().unwrap().to_string_lossy(), "Gone 2");
        assert!(back2.join("Second.md").exists());
        assert!(back.join("Inner.md").exists(), "the first restore is untouched");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_the_same_page_twice_inside_one_second_keeps_both_copies() {
        let root = std::env::temp_dir().join(format!("notecraft-trash-twice-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("NB/Sec")).unwrap();
        let path = root.join("NB/Sec/Note.md");
        crate::vault::write_page(&path, "Note", None, 0.0, "first words\n").unwrap();

        // Two sections with a page of the same name — the ordinary case, and both deletes fall in
        // the same stamp second.
        fs::create_dir_all(root.join("NB/Other")).unwrap();
        let other = root.join("NB/Other/Note.md");
        crate::vault::write_page(&other, "Note", None, 0.0, "second words\n").unwrap();
        let a = Ops::delete_page(&root, &path).unwrap();
        let b = Ops::delete_page(&root, &other).unwrap();
        assert_ne!(a, b, "the second delete overwrote the first: {a:?} {b:?}");
        assert!(fs::read_to_string(&a).unwrap().contains("first words"), "{a:?}");
        assert!(fs::read_to_string(&b).unwrap().contains("second words"), "{b:?}");
        // and both are offered back, under their own stamp-stripped names
        let entries = Ops::trash_entries(&root);
        assert_eq!(entries.len(), 2, "{entries:?}");
        for e in &entries {
            assert!(e.label.starts_with("Note"), "{e:?}");
            assert!(!e.when.is_empty(), "stamp was not read back: {e:?}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_trash_name_is_split_into_its_label_and_its_stamp() {
        assert_eq!(
            split_trash_name("20261019-085754-Alpha.md"),
            ("Alpha.md".to_string(), "2026-10-19 08:57".to_string())
        );
        assert_eq!(
            split_trash_name("20261019-085754-Personal"),
            ("Personal".to_string(), "2026-10-19 08:57".to_string())
        );
        // something a user dropped in by hand still lists, with no date
        assert_eq!(
            split_trash_name("notes.md"),
            ("notes.md".to_string(), String::new())
        );
        // a name that merely looks like a stamp prefix is not chopped
        assert_eq!(
            split_trash_name("2026-10-19-x.md"),
            ("2026-10-19-x.md".to_string(), String::new())
        );
    }
}
