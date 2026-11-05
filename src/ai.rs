//! The page formatter and the summariser: the only part of NoteCraft that asks a language model
//! anything.
//!
//! **Where the model lives is the user's choice, not the app's.** `Settings` (saved in
//! `ai.json` beside `state.json`, mode 0600 because it may hold a key) names a provider, a base
//! URL, a model and how to find the key — or the key itself. Two shapes are spoken:
//!
//! * **Ollama** (`provider: "ollama"`, `POST {base}/api/chat`) — a local daemon on
//!   `http://127.0.0.1:11434` by default, and the same shape as ollama.com for hosted models,
//!   which needs a key.
//! * **OpenAI-compatible** (`provider: "openai"`, `POST {base}/v1/chat/completions`) — OpenAI,
//!   OpenRouter, Groq, Together, DeepSeek, Fireworks, LM Studio, llama.cpp's server, vLLM: all of
//!   them speak this, and Anthropic/Gemini are reachable through OpenRouter or their own
//!   compatible endpoints.
//!
//! Nothing is assumed about the address: `https://` is spoken too (TLS through `rustls`, bundled
//! roots, so no system packages and no OpenSSL), and a key is sent as `Authorization: Bearer …`
//! only when one is configured.
//!
//! **The formatter's contract is the point.** The model never retypes the article: Rust splits the
//! page into numbered units and the model only says how to *group* them into blocks. Its reply is a
//! list of inclusive `from..to` ranges, validated to cover every unit exactly once in order — a
//! reply with a gap, an overlap or an out-of-range index is refused outright, and because the words
//! that end up in the document are the unit strings themselves, no reply can change, shorten,
//! reorder or lose a word. That is what makes it safe to run on a page the user cares about.

use crate::doc::{Block, Doc, Kind, Run};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A local Ollama, and the model most people will have pulled. Both are editable in the app; these
/// are only what the fields start as.
pub const DEFAULT_URL: &str = "http://127.0.0.1:11434";
pub const DEFAULT_MODEL: &str = "llama3.2";
pub const OPENAI_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_WORDS: u32 = 200;
/// The file `Settings` lives in, beside `state.json`. Holds a key when one is typed in, so it is
/// written 0600 and never committed anywhere.
pub const SETTINGS_FILE: &str = "ai.json";

/// Units per request. A 3000-word article is ~150 sentences, so it goes out in two or three calls
/// that run one after another on the worker thread; the reply for one batch is validated on its own.
const BATCH: usize = 80;
/// The longest page the summariser will send in one go. Beyond this the honest answer is that it
/// does not fit, rather than a summary of the first half pretending to be the whole.
const SUMMARY_LIMIT: usize = 30_000;
/// A unit longer than this is split at the last space before the limit — a wall of text with no
/// full stops still has to arrive in pieces the model can refer to by number.
const MAX_UNIT: usize = 380;
/// A line of at most this many words that does not end in punctuation is one unit (a title, a
/// heading, a caption): splitting it into "sentences" would glue it to the next line's prose.
const HEADING_WORDS: usize = 14;

/// Which request shape the endpoint speaks. Two cover everything people actually run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// `POST {base}/api/chat` — Ollama on this machine, or ollama.com for hosted models.
    #[default]
    Ollama,
    /// `POST {base}/v1/chat/completions` — OpenAI, OpenRouter, Groq, Together, DeepSeek, LM Studio,
    /// llama.cpp's server, vLLM, and the compatible front doors of Anthropic and Google.
    OpenAi,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::Ollama => "Ollama",
            Provider::OpenAi => "OpenAI-compatible",
        }
    }

    /// What the server field starts as, and what it goes back to when the shape is switched.
    pub fn default_url(self) -> &'static str {
        match self {
            Provider::Ollama => DEFAULT_URL,
            Provider::OpenAi => OPENAI_URL,
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Ollama => DEFAULT_MODEL,
            Provider::OpenAi => "gpt-4o-mini",
        }
    }

    /// A key is optional for a local Ollama and required for everything else in practice.
    pub fn needs_key(self, url: &str) -> bool {
        match self {
            Provider::Ollama => !is_local(url),
            Provider::OpenAi => !is_local(url),
        }
    }
}

/// `127.0.0.1`, `localhost`, `::1`, or a bare host with no dot: a server on this machine, which
/// needs no key and is worth saying so about.
pub fn is_local(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let host = rest.split('/').next().unwrap_or(rest);
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host == "::1"
        || host.starts_with("127.")
        || host == "0.0.0.0"
        || (!host.contains('.') && !host.is_empty())
}

/// Where the model lives, which one to ask, and the key to ask with (empty for a local server).
#[derive(Clone, Debug, PartialEq)]
pub struct Cfg {
    pub provider: Provider,
    pub url: String,
    pub model: String,
    pub key: String,
    pub num_ctx: u32,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            provider: Provider::Ollama,
            url: DEFAULT_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            key: String::new(),
            num_ctx: 8192,
        }
    }
}

impl Cfg {
    /// The URL a chat request is actually posted to.
    ///
    /// The field holds a *base*, because that is what people paste: `http://127.0.0.1:11434`,
    /// `https://openrouter.ai/api/v1`, `http://localhost:1234/v1`. A full endpoint is accepted too
    /// and left alone, so pasting the URL out of a provider's docs cannot double the path.
    pub fn endpoint(&self) -> String {
        let base = self.url.trim().trim_end_matches('/');
        match self.provider {
            Provider::Ollama => {
                if base.ends_with("/api/chat") {
                    base.to_string()
                } else {
                    format!("{base}/api/chat")
                }
            }
            Provider::OpenAi => {
                if base.ends_with("/chat/completions") {
                    base.to_string()
                } else if base.ends_with("/v1") || base.ends_with("/v1beta") {
                    format!("{base}/chat/completions")
                } else {
                    format!("{base}/v1/chat/completions")
                }
            }
        }
    }

}

/// What is saved in `ai.json`: the connection, and nothing about the notes.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub provider: Provider,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub model: String,
    /// The key itself, when the user would rather type it than set a variable. The file is 0600.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// The name of an environment variable to read the key from — nothing secret on disk.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key_env: String,
    /// Ollama's context window. Absent means the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_ctx: Option<u32>,
}

impl Settings {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join(SETTINGS_FILE)
    }

    /// Read the file, filling in whatever it does not say. A missing or unreadable file is not an
    /// error: it is the first run, and the defaults are a local Ollama.
    pub fn load(dir: &Path) -> Settings {
        let mut s: Settings = std::fs::read_to_string(Settings::path(dir))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if s.url.trim().is_empty() {
            s.url = s.provider.default_url().to_string();
        }
        if s.model.trim().is_empty() {
            s.model = s.provider.default_model().to_string();
        }
        s
    }

    /// Write the file, 0600 on unix because it may hold a key. The directory is created.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let path = Settings::path(dir);
        let body = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string());
        std::fs::write(&path, body)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// The key to send: the environment variable if one is named and set, otherwise the stored one.
    pub fn resolved_key(&self) -> String {
        let named = self.key_env.trim();
        if !named.is_empty() {
            if let Ok(v) = std::env::var(named) {
                if !v.trim().is_empty() {
                    return v.trim().to_string();
                }
            }
        }
        self.key.trim().to_string()
    }

    pub fn to_cfg(&self) -> Cfg {
        let mut cfg = Cfg::default();
        cfg.provider = self.provider;
        cfg.url = if self.url.trim().is_empty() {
            self.provider.default_url().to_string()
        } else {
            self.url.trim().trim_end_matches('/').to_string()
        };
        cfg.model = if self.model.trim().is_empty() {
            self.provider.default_model().to_string()
        } else {
            self.model.trim().to_string()
        };
        cfg.key = self.resolved_key();
        cfg.num_ctx = self.num_ctx.unwrap_or(8192);
        cfg
    }
}

/// One piece of text the model can refer to by number: a sentence, or a whole short line.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    pub text: String,
    /// This unit is the first of a line in the source, i.e. the page probably already starts a
    /// block here. Only a hint, passed to the model as `¶`.
    pub block_start: bool,
}

/// A block as the model described it: inclusive unit range `from..=to`, 1-based.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub kind: Kind,
    pub from: usize,
    pub to: usize,
    /// A heading whose words were pasted onto the front of the paragraph that follows it.
    pub head: Option<String>,
}

// ------------------------------------------------------------------ units

/// Split text into the numbered units the model groups. Nothing is dropped: every unit is a slice
/// of the input with its outer whitespace trimmed (and any leftover markup marker at the start of a
/// line removed), so the words of the page survive exactly.
pub fn units(text: &str) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::new();
    for raw in text.split('\n') {
        let flat = collapse(raw);
        let line = strip_leading_markers(&flat).to_string();
        if line.is_empty() {
            continue;
        }
        let words = line.split_whitespace().count();
        let pieces = if !ends_closed(&line) && words <= HEADING_WORDS {
            vec![line.clone()]
        } else {
            sentences(&line)
        };
        for (i, p) in pieces.iter().enumerate() {
            let text = p.trim();
            if text.is_empty() {
                continue;
            }
            out.push(Unit {
                text: text.to_string(),
                block_start: i == 0,
            });
        }
    }
    out
}

/// A line pasted out of a markdown-flavoured source arrives with its markers still on it ("## ",
/// "> "). The block's kind carries that meaning here, so the marker goes. A `#tag` keeps its `#`:
/// only a `#` run followed by a space is a heading marker.
fn strip_leading_markers(s: &str) -> &str {
    let mut t = s.trim_start();
    loop {
        if let Some(rest) = t.strip_prefix('>') {
            t = rest.trim_start();
            continue;
        }
        let hashes = t.len() - t.trim_start_matches('#').len();
        if hashes > 0 {
            let rest = &t[hashes..];
            if rest.starts_with(' ') {
                t = rest.trim_start();
                continue;
            }
        }
        return t;
    }
}

/// Runs of spaces and tabs become one space, and the ends are trimmed. The words are untouched.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() || c == '\u{a0}' {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

/// Does the line end in something a sentence ends with? A closing quote or bracket after the full
/// stop still counts.
fn ends_closed(s: &str) -> bool {
    for c in s.trim_end().chars().rev() {
        return match c {
            '.' | '!' | '?' | '…' | ':' | ';' => true,
            '"' | '\'' | '”' | '’' | ')' | ']' => continue,
            _ => false,
        };
    }
    false
}

const ABBREV: [&str; 16] = [
    "mr", "mrs", "ms", "dr", "prof", "st", "no", "e.g", "i.e", "etc", "vs", "fig", "approx",
    "inc", "ltd", "jr",
];

/// Split one line into sentences, keeping abbreviations ("Dr. Smith"), initials ("J. R. Tolkien")
/// and a lowercase word after the stop ("e.g. the") together.
fn sentences(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        if matches!(chars[i], '.' | '!' | '?' | '…') {
            // "?!", "..." and "…" are one end of a sentence
            let mut j = i;
            while j + 1 < chars.len() && matches!(chars[j + 1], '.' | '!' | '?') {
                j += 1;
            }
            // a closing quote or bracket belongs to the sentence that just ended
            let mut k = j + 1;
            while k < chars.len() && matches!(chars[k], '"' | '\'' | ')' | ']' | '”' | '’') {
                k += 1;
            }
            let mut m = k;
            while m < chars.len() && chars[m].is_whitespace() {
                m += 1;
            }
            let next_ok = m >= chars.len() || !chars[m].is_lowercase();
            let word = word_before(&chars, i);
            // A full stop with a digit on each side and no space between them is a decimal point, a
            // version or an address ("CUDA 13.4", "43.0/86.0", "615.78.08"), never the end of a
            // sentence. Splitting there puts a space inside the number when the units are joined
            // back together, which is the one way this module could still damage a page's words.
            let inside_number = m == k && m < chars.len() && chars[m].is_ascii_digit();
            if next_ok && !inside_number && !is_abbrev(&word) {
                out.push(chars[start..k].iter().collect());
                start = k;
            }
            i = k.max(i + 1);
            continue;
        }
        i += 1;
    }
    if start < chars.len() {
        out.push(chars[start..].iter().collect());
    }
    out.iter()
        .filter(|s| !s.trim().is_empty())
        .flat_map(|s| hard_split(s))
        .collect()
}

/// The word ending just before the punctuation at `at`, lowercased and stripped of quotes.
fn word_before(chars: &[char], at: usize) -> String {
    let mut end = at;
    while end > 0 && !chars[end - 1].is_alphanumeric() && chars[end - 1] != '.' {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '.') {
        start -= 1;
    }
    chars[start..end]
        .iter()
        .collect::<String>()
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '.')
        .to_lowercase()
}

fn is_abbrev(word: &str) -> bool {
    if word.chars().count() == 1 {
        // an initial: "J. R. Tolkien"
        return true;
    }
    ABBREV.contains(&word)
}

/// Break a unit that is longer than [`MAX_UNIT`] at the last space that fits.
fn hard_split(s: &str) -> Vec<String> {
    let n = s.chars().count();
    if n <= MAX_UNIT {
        return vec![s.to_string()];
    }
    let chars: Vec<char> = s.chars().collect();
    let mut cut = MAX_UNIT;
    while cut > 0 && !chars[cut].is_whitespace() {
        cut -= 1;
    }
    if cut == 0 {
        cut = MAX_UNIT; // one enormous token: cut it anyway
    }
    let head: String = chars[..cut].iter().collect();
    let tail: String = chars[cut..].iter().collect();
    let mut out = vec![head.trim().to_string()];
    out.extend(hard_split(tail.trim()));
    out
}

// ------------------------------------------------------------------ the model's plan

/// Read the model's reply into groups, and refuse anything that is not a clean cover of 1..=n.
pub fn parse_plan(reply: &str, n: usize) -> Result<Vec<Group>, String> {
    let body = strip_fence(reply);
    let v: serde_json::Value = serde_json::from_str(body.trim())
        .map_err(|e| format!("the model's reply was not JSON ({e})"))?;
    let arr = match &v {
        serde_json::Value::Array(a) => a.clone(),
        _ => v
            .get("blocks")
            .and_then(|b| b.as_array())
            .cloned()
            .ok_or_else(|| "the model's reply had no \"blocks\" list".to_string())?,
    };
    let mut out = Vec::new();
    for item in &arr {
        let Some(from) = num(item, "from").or_else(|| first_of(item, "sentences")) else {
            continue;
        };
        let to = num(item, "to")
            .or_else(|| last_of(item, "sentences"))
            .unwrap_or(from);
        let kind = item
            .get("kind")
            .and_then(|k| k.as_str())
            .map(kind_of)
            .unwrap_or(Kind::Para);
        let head = item
            .get("head")
            .or_else(|| item.get("heading"))
            .and_then(|h| h.as_str())
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty());
        out.push(Group { kind, from, to, head });
    }
    // The guard. Not "best effort": a reply that says `1..3, 6..9` would silently drop units 4 and
    // 5 from the page, and one that says `1..5, 3..9` would print words twice. Refusing keeps the
    // note exactly as it was, which is the only safe answer.
    let mut want = 1usize;
    for g in &out {
        if g.from != want {
            return Err(format!(
                "the model's grouping skipped or repeated units (expected {} at {}, got {})",
                want, want, g.from
            ));
        }
        if g.to < g.from {
            return Err(format!("the model's grouping reversed a range ({}..{})", g.from, g.to));
        }
        if g.to > n {
            return Err(format!(
                "the model's grouping ran past the last unit ({} of {})",
                g.to, n
            ));
        }
        want = g.to + 1;
    }
    if want != n + 1 {
        return Err(format!(
            "the model's grouping stopped at unit {} of {}",
            want - 1,
            n
        ));
    }
    Ok(out)
}

/// Models wrap JSON in a fence now and then; the fence is not part of the answer.
fn strip_fence(s: &str) -> &str {
    let t = s.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    t.strip_suffix("```").unwrap_or(t)
}

fn num(item: &serde_json::Value, key: &str) -> Option<usize> {
    let v = item.get(key)?;
    if let Some(n) = v.as_u64() {
        return Some(n as usize);
    }
    v.as_str()?.trim().parse().ok()
}

fn first_of(item: &serde_json::Value, key: &str) -> Option<usize> {
    item.get(key)?.as_array()?.first()?.as_u64().map(|n| n as usize)
}

fn last_of(item: &serde_json::Value, key: &str) -> Option<usize> {
    item.get(key)?.as_array()?.last()?.as_u64().map(|n| n as usize)
}

/// The kind words the prompt asks for, plus the near-misses models actually produce (`h2/h3`,
/// `heading`, `paragraph`, `list`).
fn kind_of(s: &str) -> Kind {
    let raw = s.trim().to_lowercase();
    // "h2/h3" and "h2 or h3": take the first level the model named
    let k = raw.split(['/', ',', '|', ' ']).next().unwrap_or("").trim();
    match k {
        "h1" | "heading1" | "heading 1" | "title" => Kind::Heading(1),
        "h2" | "heading2" | "heading 2" | "subtitle" => Kind::Heading(2),
        "h3" | "heading3" | "heading 3" => Kind::Heading(3),
        "h4" => Kind::Heading(4),
        "h5" => Kind::Heading(5),
        "h6" => Kind::Heading(6),
        "bullet" | "bullets" | "list" | "ul" | "li" => Kind::Bullet,
        "quote" | "blockquote" | "quotation" => Kind::Quote,
        _ => Kind::Para,
    }
}

// ------------------------------------------------------------------ building blocks

/// Turn the plan into document blocks. The words come from the units, never from the reply.
pub fn build(groups: &[Group], units: &[Unit]) -> Vec<Block> {
    let mut out = Vec::new();
    for g in groups {
        let from = g.from.max(1);
        let to = g.to.min(units.len());
        if from > to || from > units.len() {
            continue;
        }
        let text = units[from - 1..to]
            .iter()
            .map(|u| {
                // a pasted bullet arrives with its marker on every line; the kind carries the
                // meaning, so each unit's marker goes
                if matches!(g.kind, Kind::Bullet | Kind::Quote) {
                    marker_stripped(&u.text).to_string()
                } else {
                    u.text.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let heading = matches!(g.kind, Kind::Heading(_));
        let (head, body) = match (&g.head, heading) {
            (Some(h), true) => split_head(&text, h),
            _ => (None, text),
        };
        if let Some(h) = head {
            out.push(block_of(g.kind, &h));
        } else if heading {
            // The model's words for the heading did not match the page word for word, so there is
            // nowhere honest to cut: the block keeps the model's kind and every word it was given.
            out.push(block_of(g.kind, &body));
            continue;
        }
        if !body.trim().is_empty() {
            // A heading whose words were glued to its first sentence leaves the rest as prose —
            // never as a second heading.
            let kind = if heading { Kind::Para } else { g.kind };
            out.push(block_of(kind, &body));
        }
    }
    out
}

/// Split `text` where the model says the heading's words end. Only a word-for-word prefix counts:
/// if the heading was retyped differently the split is refused and the block stays whole, so no
/// words can be invented or dropped.
fn split_head(text: &str, head: &str) -> (Option<String>, String) {
    let words: Vec<&str> = text.split_whitespace().collect();
    let want: Vec<&str> = head.split_whitespace().collect();
    if want.is_empty() || want.len() >= words.len() {
        return (None, text.to_string());
    }
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    };
    for (a, b) in want.iter().zip(words.iter()) {
        if norm(a) != norm(b) {
            return (None, text.to_string());
        }
    }
    (
        Some(words[..want.len()].join(" ")),
        words[want.len()..].join(" "),
    )
}

fn block_of(kind: Kind, text: &str) -> Block {
    let mut b = Block::new(kind);
    b.runs.push(Run::plain(marker_stripped(text)));
    b
}

/// The highlighter's slot for yellow (`theme::MARK_NAMES[0]`, 1-based as `Attrs::mark` is). A heading
/// the formatter decided on wears it, so the structure the model read out of a wall of text can be
/// seen at a glance instead of hunted for.
const HEAD_MARK: u8 = 1;

/// What every heading in a formatted segment gets, once the whole segment is known: a **blank line in
/// front of it** (a heading glued to the paragraph above reads as part of that paragraph) and the
/// **yellow marker** on its words. A heading that opens a segment gets no blank line — there is
/// nothing above it to separate it from. Everything the words are is untouched: this adds a block and
/// an attribute, never a character.
fn headings_stand_out(blocks: Vec<Block>) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len() + 4);
    for mut b in blocks {
        if matches!(b.kind, Kind::Heading(_)) {
            let already_blank = out
                .last()
                .map(|p| p.kind == Kind::Para && p.text().trim().is_empty())
                .unwrap_or(true);
            if !already_blank {
                out.push(Block::new(Kind::Para));
            }
            for r in &mut b.runs {
                r.a.mark = HEAD_MARK;
            }
        }
        out.push(b);
    }
    out
}

/// A pasted bullet arrives with its marker ("- ", "• ", "* "); the kind carries that meaning, so
/// the marker is the one thing the formatter may take away.
fn marker_stripped(s: &str) -> &str {
    let t = s.trim();
    for m in ["- ", "– ", "— ", "• ", "* ", "· "] {
        if let Some(r) = t.strip_prefix(m) {
            return r.trim_start();
        }
    }
    t
}

/// The page's words, as one text: paragraph blocks one per line, headings and bullets kept, and
/// pictures / code / rules skipped — the summariser is asked about the article, not about markup.
pub fn page_text(doc: &Doc) -> String {
    let mut out: Vec<String> = Vec::new();
    for b in &doc.blocks {
        let text = b.text();
        let t = text.trim();
        if t.is_empty() {
            continue;
        }
        match b.kind {
            Kind::Para | Kind::Heading(_) | Kind::Quote => out.push(t.to_string()),
            Kind::Bullet | Kind::Todo => out.push(format!("- {t}")),
            _ => {}
        }
    }
    out.join("\n")
}

/// The summary, as blocks: one paragraph per blank-line-separated chunk, and a run of lines that
/// all start with a bullet marker becomes real bullets.
pub fn summary_blocks(text: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    for chunk in text.split("\n\n") {
        let lines: Vec<&str> = chunk
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();
        if lines.is_empty() {
            continue;
        }
        let bulleted = lines.len() > 1
            && lines
                .iter()
                .all(|l| marker_stripped(l).len() != l.trim_start().len());
        if bulleted {
            for l in lines {
                out.push(block_of(Kind::Bullet, l));
            }
        } else {
            let joined = lines.join(" ");
            // a model that answers with markdown bold would otherwise print the asterisks
            out.push(block_of(Kind::Para, &joined.replace("**", "")));
        }
    }
    out
}

// ------------------------------------------------------------------ talking to Ollama

// ------------------------------------------------------------------ the wire

/// POST a JSON body to the configured endpoint and hand back the reply body.
///
/// A non-2xx reply is **not** thrown away: providers put the reason in the body (`{"error":
/// {"message": "invalid api key"}}`, `{"error": "model not found"}`), and that sentence is what the
/// status bar should say, so the body is read and mined before an error is returned.
fn post_json(cfg: &Cfg, body: &str, timeout: Duration) -> Result<String, String> {
    let url = cfg.endpoint();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into();
    let mut req = agent
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "application/json");
    if !cfg.key.is_empty() {
        req = req.header("authorization", &format!("Bearer {}", cfg.key));
    }
    let mut res = req
        .send(body.as_bytes())
        .map_err(|e| format!("cannot reach the model server — {e}"))?;
    let status = res.status().as_u16();
    let text = res
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("the model server's reply could not be read — {e}"))?;
    if !(200..300).contains(&status) {
        return Err(server_error(&url, status, &text));
    }
    Ok(text)
}

/// The sentence to show for a refused request: whatever the provider said, or the status alone.
fn server_error(url: &str, status: u16, body: &str) -> String {
    let said = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| {
                    e.get("message")
                        .and_then(|m| m.as_str())
                        .or_else(|| e.as_str())
                })
                .or_else(|| v.get("message").and_then(|m| m.as_str()))
                .map(|m| m.trim().to_string())
        })
        .filter(|m| !m.is_empty());
    let host = url.split('/').nth(2).unwrap_or(url);
    match said {
        Some(m) => format!("{host} refused it ({status}): {m}"),
        None => {
            let brief: String = body.trim().chars().take(200).collect();
            if brief.is_empty() {
                format!("{host} answered {status}")
            } else {
                format!("{host} answered {status}: {brief}")
            }
        }
    }
}

/// One chat turn. `json` asks for a JSON object where the provider supports that natively; the
/// OpenAI-compatible shape does not, because `response_format` is the one field half the servers
/// that speak it refuse — the prompts ask for JSON in words instead, and the reply is parsed
/// leniently.
fn chat(
    cfg: &Cfg,
    system: &str,
    user: &str,
    json: bool,
    timeout: Duration,
    num_ctx: u32,
) -> Result<String, String> {
    let messages = serde_json::json!([
        { "role": "system", "content": system },
        { "role": "user", "content": user },
    ]);
    let payload = match cfg.provider {
        Provider::Ollama => {
            let mut p = serde_json::json!({
                "model": cfg.model,
                "stream": false,
                "options": { "temperature": 0, "num_ctx": num_ctx },
                "messages": messages,
            });
            if json {
                p["format"] = serde_json::json!("json");
            }
            p
        }
        Provider::OpenAi => serde_json::json!({
            "model": cfg.model,
            "temperature": 0,
            "stream": false,
            "messages": messages,
        }),
    };
    let reply = post_json(cfg, &payload.to_string(), timeout)?;
    let v: serde_json::Value = serde_json::from_str(&reply)
        .map_err(|e| format!("the model server's reply was not JSON ({e})"))?;
    if let Some(err) = v
        .get("error")
        .and_then(|e| e.get("message").and_then(|m| m.as_str()).or_else(|| e.as_str()))
    {
        return Err(format!("{} — {err}", cfg.model));
    }
    let content = match cfg.provider {
        Provider::Ollama => v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        Provider::OpenAi => v
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
    };
    let content = content.ok_or_else(|| {
        format!(
            "{} answered without any text — check that the model name exists",
            cfg.model
        )
    })?;
    Ok(content.to_string())
}

/// Ask the server one trivial question, so the settings window can say whether the address, the
/// key and the model name all work — before the user finds out by formatting a page.
pub fn probe(cfg: &Cfg) -> Result<String, String> {
    if cfg.model.trim().is_empty() {
        return Err("no model name is set".to_string());
    }
    let reply = chat(
        cfg,
        "You are a connection test. Answer with one short word.",
        "Reply with the word: ready",
        false,
        Duration::from_secs(60),
        cfg.num_ctx.min(2048),
    )?;
    let said: String = reply.trim().replace('\n', " ").chars().take(60).collect();
    if said.is_empty() {
        Err("the model answered with nothing".to_string())
    } else {
        Ok(said)
    }
}

// ------------------------------------------------------------------ formatting a page

const FORMAT_SYSTEM: &str = "You tidy text that was pasted without its formatting. \
You never rewrite, shorten, translate, expand or reorder a single word. \
You only decide where the text breaks into blocks, and what kind each block is.";

const SUMMARY_SYSTEM: &str = "You summarise an article faithfully for the person who saved it. \
You keep its facts, names, numbers and dates, and you add nothing that is not in the text. \
You answer with the summary itself and nothing else.";

/// Ask for one batch of units and turn the answer into groups.
fn plan(cfg: &Cfg, units: &[Unit]) -> Result<Vec<Group>, String> {
    let mut listing = String::new();
    for (i, u) in units.iter().enumerate() {
        let mark = if u.block_start { "¶ " } else { "" };
        listing.push_str(&format!("[{}] {mark}{}\n", i + 1, u.text));
    }
    let user = format!(
        "Text, one unit per line, numbered. A ¶ before the number means the page already starts a \
new block there.\n\n{listing}\n\
Group consecutive numbers into blocks for a readable article. Kinds: \"h1\" (the document's own \
title), \"h2\" and \"h3\" (section headings), \"para\", \"bullet\", \"quote\".\n\
Rules:\n\
- every number 1..{n} appears in exactly one block, in order, with no gaps and no overlaps\n\
- \"from\" and \"to\" are inclusive unit numbers\n\
- a heading block holds only the heading's own words and nothing else\n\
- if a heading was pasted onto the front of the paragraph under it, put the heading's words in \
\"head\" (copied exactly) and still cover those units with from/to\n\
- a short capitalised phrase at the start of a long unit is often a section heading that lost its \
line break (\"Etymology The name was first recorded in 1309\"); treat it as a heading with \"head\"\n\
- ordinary prose is \"para\"; never split one unit across two blocks\n\
- if there is no title, do not invent one\n\n\
Reply with JSON only: {{\"blocks\":[{{\"kind\":\"h2\",\"from\":3,\"to\":3}}]}}",
        n = units.len()
    );
    let reply = chat(
        cfg,
        FORMAT_SYSTEM,
        &user,
        true,
        Duration::from_secs(180),
        cfg.num_ctx,
    )?;
    parse_plan(&reply, units.len())
}

/// Reformat a document: every run of text blocks is grouped into paragraphs, headings and bullets;
/// pictures, code, rules, tables and tasks are left exactly where they are and are never sent.
pub fn tidy_page(
    page: &Doc,
    cfg: &Cfg,
    note: &mut dyn FnMut(&str),
) -> Result<Doc, String> {
    let mut out: Vec<Block> = Vec::new();
    let mut seg: Vec<Block> = Vec::new();
    for b in &page.blocks {
        if is_text(b.kind) {
            seg.push(b.clone());
        } else {
            if !seg.is_empty() {
                out.extend(tidy_segment(&seg, cfg, note)?);
                seg.clear();
            }
            out.push(b.clone());
        }
    }
    if !seg.is_empty() {
        out.extend(tidy_segment(&seg, cfg, note)?);
    }
    let mut doc = Doc { blocks: out };
    doc.ensure_block();
    Ok(doc)
}

fn is_text(kind: Kind) -> bool {
    matches!(kind, Kind::Para | Kind::Heading(_) | Kind::Bullet | Kind::Quote)
}

/// One run of text blocks, batched so a long article does not arrive as one enormous prompt.
fn tidy_segment(
    seg: &[Block],
    cfg: &Cfg,
    note: &mut dyn FnMut(&str),
) -> Result<Vec<Block>, String> {
    let text = seg
        .iter()
        .map(|b| b.text())
        .collect::<Vec<_>>()
        .join("\n");
    let units = units(&text);
    if units.len() < 2 {
        // one unit is already one line: there is no structure to add
        return Ok(seg.to_vec());
    }
    let batches = units.len().div_ceil(BATCH);
    let mut built: Vec<Block> = Vec::new();
    for (i, chunk) in units.chunks(BATCH).enumerate() {
        if batches > 1 {
            note(&format!("formatting {} of {}…", i + 1, batches));
        }
        let groups = plan(cfg, chunk)?;
        built.extend(build(&groups, chunk));
    }
    if built.is_empty() {
        return Ok(seg.to_vec());
    }
    Ok(headings_stand_out(built))
}

/// Summarise a page's words at about `words` words.
pub fn summarise(text: &str, words: u32, cfg: &Cfg) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("this page has no words to summarise".to_string());
    }
    let n = text.chars().count();
    if n > SUMMARY_LIMIT {
        return Err(format!(
            "this page is too long to summarise in one go ({n} characters; the limit is {SUMMARY_LIMIT})"
        ));
    }
    let user = format!(
        "Summarise the article below in about {words} words, as plain prose in two to five short \
paragraphs separated by blank lines. Keep the names, numbers and dates that matter, and drop \
detail that does not change the meaning. Do not describe the summary, and do not add a heading.\n\n\
ARTICLE:\n{text}"
    );
    let reply = chat(
        cfg,
        SUMMARY_SYSTEM,
        &user,
        false,
        Duration::from_secs(300),
        cfg.num_ctx.max(16384),
    )?;
    let reply = reply.trim().to_string();
    if reply.is_empty() {
        return Err("the model returned an empty summary".to_string());
    }
    Ok(reply)
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(|w| w.to_lowercase()).collect()
    }

    /// The property the whole design rests on: splitting a page into units loses no word, whatever
    /// the page throws at it. Only a leftover markup marker at the start of a line may go.
    #[test]
    fn units_keep_every_word() {
        let page = "Kielder Water is a large man-made reservoir.\n\n\
It was planned in the late 1960s. Dr. Smith disagreed! Who won? The scheme won, 3-1.\n\
\n\
## Etymology\nThe name was first recorded in 1309 as Keldre. It may come from Brittonic.\n\
> a quoted line goes here";
        let us = units(page);
        assert!(us.len() >= 6, "{us:?}");
        let rebuilt = us
            .iter()
            .map(|u| u.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        // the same page with its markers taken off is what the units must add up to
        let want: String = page
            .split('\n')
            .map(|l| strip_leading_markers(&collapse(l)).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(words(&rebuilt), words(&want), "a word was lost or added");
    }

    #[test]
    fn a_heading_marker_goes_and_a_tag_keeps_its_hash() {
        let us = units("### Construction\n#final notes about the dam");
        assert_eq!(us[0].text, "Construction");
        assert_eq!(us[1].text, "#final notes about the dam");
    }

    #[test]
    fn a_short_line_without_a_full_stop_is_one_unit() {
        let us = units("Kielder Water\nThe name was first recorded in 1309 as Keldre.");
        assert_eq!(us.len(), 2);
        assert_eq!(us[0].text, "Kielder Water");
        assert!(us[0].block_start && us[1].block_start);
    }

    #[test]
    fn abbreviations_and_initials_do_not_end_a_sentence() {
        let us = units("Dr. Smith met J. R. Tolkien at 5 p.m. and they talked about the dam, e.g. its size. It was big.");
        assert_eq!(us.len(), 2, "{us:?}");
        assert!(us[0].text.contains("e.g. its size."));
    }

    /// A number is one unit, whatever it looks like: a decimal point is not the end of a sentence, and
    /// joining the units back with a space must never turn "13.4" into "13. 4". This was real damage
    /// to a page of measurements — every decimal and every version string in it was split.
    #[test]
    fn numbers_are_never_split_at_their_decimal_point() {
        let line = "Pinned for sm_89 with CUDA 13.4 and driver 615.78.08: 43.0/86.0 = 0.500, TF32 21.6, 5.9 watts. It was measured twice.";
        let us = units(line);
        let joined = us.iter().map(|u| u.text.as_str()).collect::<Vec<_>>().join(" ");
        for n in ["13.4", "615.78.08", "43.0", "86.0", "0.500", "21.6", "5.9"] {
            assert!(joined.contains(n), "{n} was broken: {joined}");
            assert!(!joined.contains(&n.replace('.', ". ")), "{n} gained a space: {joined}");
        }
        // the sentence still ends at the full stop that is followed by a space
        assert!(us.len() >= 2, "{us:?}");
    }

    #[test]
    fn a_wall_of_text_is_split_somewhere() {
        let long = "word ".repeat(400);
        let us = units(&long);
        assert!(us.len() > 1);
        assert!(us.iter().all(|u| u.text.chars().count() <= MAX_UNIT));
    }

    #[test]
    fn a_plan_must_cover_every_unit_once() {
        let ok = r#"{"blocks":[{"kind":"h1","from":1,"to":1},{"kind":"para","from":2,"to":3}]}"#;
        assert_eq!(parse_plan(ok, 3).unwrap().len(), 2);

        let gap = r#"{"blocks":[{"kind":"para","from":1,"to":1},{"kind":"para","from":3,"to":3}]}"#;
        assert!(parse_plan(gap, 3).is_err());

        let twice = r#"{"blocks":[{"kind":"para","from":1,"to":2},{"kind":"para","from":2,"to":3}]}"#;
        assert!(parse_plan(twice, 3).is_err());

        let short = r#"{"blocks":[{"kind":"para","from":1,"to":2}]}"#;
        assert!(parse_plan(short, 3).is_err());

        let past = r#"{"blocks":[{"kind":"para","from":1,"to":9}]}"#;
        assert!(parse_plan(past, 3).is_err());

        let not_json = "I think this text is about a reservoir.";
        assert!(parse_plan(not_json, 3).is_err());
    }

    #[test]
    fn a_fenced_reply_and_loose_kind_words_are_read() {
        let reply = "```json\n{\"blocks\":[{\"kind\":\"h2/h3\",\"from\":1,\"to\":1},{\"kind\":\"paragraph\",\"from\":2,\"to\":2}]}\n```";
        let g = parse_plan(reply, 2).unwrap();
        assert_eq!(g[0].kind, Kind::Heading(2));
        assert_eq!(g[1].kind, Kind::Para);
    }

    #[test]
    fn a_heading_glued_to_its_paragraph_is_split_on_the_models_words() {
        let us = units("Etymology The name was first recorded in 1309 as Keldre.");
        assert_eq!(us.len(), 1);
        let g = vec![Group {
            kind: Kind::Heading(2),
            from: 1,
            to: 1,
            head: Some("Etymology".into()),
        }];
        let b = build(&g, &us);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].kind, Kind::Heading(2));
        assert_eq!(b[0].text(), "Etymology");
        assert_eq!(b[1].kind, Kind::Para);
        assert_eq!(b[1].text(), "The name was first recorded in 1309 as Keldre.");
    }

    /// A heading the model *retyped* differently must not be cut anywhere: there is nowhere
    /// honest to cut, so the block keeps the heading kind and every word it was given.
    #[test]
    fn a_heading_that_does_not_match_keeps_every_word() {
        let us = units("The name Kielder was first recorded in 1309.");
        let g = vec![Group {
            kind: Kind::Heading(2),
            from: 1,
            to: 1,
            head: Some("Etymology".into()),
        }];
        let b = build(&g, &us);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, Kind::Heading(2));
        assert_eq!(b[0].text(), "The name Kielder was first recorded in 1309.");
    }

    #[test]
    fn a_pasted_bullet_loses_only_its_marker() {
        let us = units("- one\n- two");
        let g = vec![Group { kind: Kind::Bullet, from: 1, to: 2, head: None }];
        let b = build(&g, &us);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, Kind::Bullet);
        assert_eq!(b[0].text(), "one two");
    }

    /// The shape the user asked for: a heading the formatter identifies gets a blank line in front of
    /// it and the yellow highlighter on its words — and the paragraph above keeps exactly its own
    /// words, so nothing about the guarantee changes.
    #[test]
    fn a_heading_gets_a_line_before_it_and_the_yellow_marker() {
        let page = "Kielder Water is a reservoir in Northumberland. It was planned in the 1960s.\n\
                    Etymology\nThe name was first recorded in 1309.";
        let us = units(page);
        let g = vec![
            Group { kind: Kind::Para, from: 1, to: 2, head: None },
            Group { kind: Kind::Heading(2), from: 3, to: 4, head: Some("Etymology".into()) },
        ];
        let out = headings_stand_out(build(&g, &us));
        assert_eq!(out.len(), 4, "{:?}", out.iter().map(|b| (&b.kind, b.text())).collect::<Vec<_>>());
        assert_eq!(out[0].kind, Kind::Para);
        assert_eq!(out[1].kind, Kind::Para);
        assert!(out[1].text().trim().is_empty(), "the line before the heading is empty");
        assert_eq!(out[2].kind, Kind::Heading(2));
        assert_eq!(out[2].text(), "Etymology");
        assert!(out[2].runs.iter().all(|r| r.a.mark == HEAD_MARK), "the heading wears the marker");
        assert_eq!(out[3].kind, Kind::Para);
        assert_eq!(out[3].text(), "The name was first recorded in 1309.");

        // the body is untouched: only the heading is marked
        assert!(out[0].runs.iter().all(|r| r.a.mark == 0));
        assert!(out[3].runs.iter().all(|r| r.a.mark == 0));
        // yellow is the first marker in the highlighter's palette
        assert_eq!(crate::theme::MARK_NAMES[HEAD_MARK as usize - 1], "yellow");
    }

    /// A page that opens with a heading does not start with a blank line: there is nothing above it.
    #[test]
    fn a_heading_at_the_top_gets_no_blank_line() {
        let us = units("Etymology\nThe name was first recorded in 1309.");
        let g = vec![
            Group { kind: Kind::Heading(1), from: 1, to: 1, head: Some("Etymology".into()) },
            Group { kind: Kind::Para, from: 2, to: 2, head: None },
        ];
        let out = headings_stand_out(build(&g, &us));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, Kind::Heading(1));
        assert_eq!(out[0].runs[0].a.mark, HEAD_MARK);
    }

    /// Running the pass twice changes nothing: re-formatting a page that already has its blank lines
    /// must not stack them up.
    #[test]
    fn the_blank_line_is_not_doubled() {
        let us = units("One sentence here. Etymology\nA name from 1309.");
        let g = vec![
            Group { kind: Kind::Para, from: 1, to: 1, head: None },
            Group { kind: Kind::Heading(2), from: 2, to: 3, head: Some("Etymology".into()) },
        ];
        let once = headings_stand_out(build(&g, &us));
        let twice = headings_stand_out(once.clone());
        assert_eq!(once, twice);
        assert_eq!(once.iter().filter(|b| b.text().trim().is_empty()).count(), 1);
    }

    #[test]
    fn a_summary_becomes_paragraphs_and_bullets() {
        let b = summary_blocks("First para line one\nstill line one.\n\n- a\n- b\n\nLast one.");
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].kind, Kind::Para);
        assert_eq!(b[1].kind, Kind::Bullet);
        assert_eq!(b[3].text(), "Last one.");
    }

    #[test]
    fn page_text_reads_the_words_and_skips_the_pictures() {
        let mut doc = Doc::default();
        doc.blocks.push(block_of(Kind::Heading(2), "Etymology"));
        doc.blocks.push(block_of(Kind::Para, "The name was recorded in 1309."));
        let mut img = Block::new(Kind::Image);
        img.image = "assets/x.png".into();
        doc.blocks.push(img);
        doc.blocks.push(block_of(Kind::Bullet, "one"));
        assert_eq!(
            page_text(&doc),
            "Etymology\nThe name was recorded in 1309.\n- one"
        );
    }

    /// A page's task block is words too — a summary of a to-do page should mention them.
    #[test]
    fn a_task_is_read_as_a_bullet() {
        let mut doc = Doc::default();
        let mut b = block_of(Kind::Todo, "call the plumber");
        b.done = true;
        doc.blocks.push(b);
        assert_eq!(page_text(&doc), "- call the plumber");
    }

    /// The live path, against the Ollama on this box: `cargo test live_ -- --ignored --nocapture`.
    /// It is `#[ignore]`d because it needs a model server, not because it is a weaker check — this
    /// is the one test that exercises units → prompt → HTTP → plan → blocks end to end, and the
    /// property it holds is the one that matters: a tidied page keeps every word of the original.
    #[test]
    #[ignore]
    fn live_tidy_and_summarise() {
        let blob = "Kielder Water\nKielder Water is a large man-made reservoir in Northumberland in \
North East England. It is the largest artificial lake in the United Kingdom by capacity of water and \
it is surrounded by Kielder Forest, one of the biggest man-made woodlands in Europe.\n\
The scheme was planned in the late 1960s to satisfy an expected rise in demand for water. But the \
decline of traditional heavy industry served to undermine the original justification for the \
reservoir.\nEtymology\nThe name Kielder was first recorded in 1309 as Keldre. Originating as a river \
name, Kielder may have the same origin as the various rivers named Calder.\nConstruction\nAfter the \
scheme was approved by Parliament in 1974, work to build the reservoir began in 1975. The design \
meant the loss of numerous farms and a school.";
        let mut doc = Doc::default();
        doc.blocks.push(block_of(Kind::Para, blob));
        let cfg = Cfg::default();
        let mut said = String::new();
        let out = tidy_page(&doc, &cfg, &mut |m| said = m.to_string()).expect("tidy");
        for b in &out.blocks {
            println!("{:?}: {}", b.kind, b.text());
        }
        assert_eq!(words(&page_text(&out)), words(blob), "tidying lost or added a word");
        assert!(
            out.blocks.iter().filter(|b| matches!(b.kind, Kind::Heading(_))).count() >= 2,
            "the two section headings were not found: {said}"
        );
        assert!(out.blocks.len() > doc.blocks.len());

        let text = page_text(&doc);
        let summary = summarise(&text, 60, &cfg).expect("summarise");
        println!("--- summary ---\n{summary}");
        let n = summary.split_whitespace().count();
        assert!((20..160).contains(&n), "asked for 60 words, got {n}");
    }
    // ---------------------------------------------------------------- the connection

    /// The field holds what people paste. A base becomes the endpoint; a full endpoint is left
    /// exactly as pasted, so copying a URL out of a provider's docs cannot double the path.
    #[test]
    fn the_endpoint_follows_from_the_base_people_paste() {
        let ollama = |url: &str| Cfg {
            provider: Provider::Ollama,
            url: url.to_string(),
            ..Cfg::default()
        }
        .endpoint();
        assert_eq!(ollama("http://127.0.0.1:11434"), "http://127.0.0.1:11434/api/chat");
        assert_eq!(ollama("http://127.0.0.1:11434/"), "http://127.0.0.1:11434/api/chat");
        assert_eq!(ollama("http://box:11434/api/chat"), "http://box:11434/api/chat");
        assert_eq!(ollama("https://ollama.com"), "https://ollama.com/api/chat");

        let openai = |url: &str| Cfg {
            provider: Provider::OpenAi,
            url: url.to_string(),
            ..Cfg::default()
        }
        .endpoint();
        assert_eq!(
            openai("https://api.openai.com/v1"),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            openai("https://openrouter.ai/api/v1/"),
            "https://openrouter.ai/api/v1/chat/completions"
        );
        assert_eq!(
            openai("http://localhost:1234"),
            "http://localhost:1234/v1/chat/completions"
        );
        assert_eq!(
            openai("http://localhost:8080/v1/chat/completions"),
            "http://localhost:8080/v1/chat/completions"
        );
    }

    #[test]
    fn a_local_address_is_recognised_and_needs_no_key() {
        for local in [
            "http://127.0.0.1:11434",
            "http://localhost:1234/v1",
            "http://[::1]:11434",
            "https://box:11434",
            "http://0.0.0.0:8080",
        ] {
            assert!(is_local(local), "{local} should be local");
        }
        for remote in ["https://api.openai.com/v1", "https://ollama.com", "http://192.168.1.9:11434"]
        {
            assert!(!is_local(remote), "{remote} should be remote");
        }
    }

    /// `ai.json` holds the key when the user types one in, so it must be private on disk — and a
    /// file that only says part of it still opens with the rest filled from the defaults.
    #[test]
    fn settings_round_trip_and_keep_the_key_private() {
        let dir = std::env::temp_dir().join(format!("nc-ai-settings-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let s = Settings {
            provider: Provider::OpenAi,
            url: "https://openrouter.ai/api/v1".into(),
            model: "anthropic/claude-3.5-sonnet".into(),
            key: "sk-or-secret".into(),
            key_env: String::new(),
            num_ctx: None,
        };
        s.save(&dir).unwrap();
        let back = Settings::load(&dir);
        assert_eq!(back, s);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(Settings::path(&dir)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "ai.json must not be readable by anyone else");
        }

        // a partial file: the blanks are filled from the shape's defaults, not left empty
        std::fs::write(
            Settings::path(&dir),
            r#"{"provider":"openai","model":"gpt-4o-mini"}"#,
        )
        .unwrap();
        let partial = Settings::load(&dir);
        assert_eq!(partial.url, OPENAI_URL);
        assert!(partial.to_cfg().key.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_key_can_come_from_an_environment_variable() {
        let s = Settings {
            provider: Provider::OpenAi,
            url: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
            key: "typed-in".into(),
            key_env: "NOTECRAFT_TEST_KEY".into(),
            num_ctx: None,
        };
        // unsafe: the test sets a variable for its own process only, and reads it straight back
        unsafe { std::env::set_var("NOTECRAFT_TEST_KEY", "from-env") };
        assert_eq!(s.to_cfg().key, "from-env", "a named variable wins over a stored key");
        unsafe { std::env::remove_var("NOTECRAFT_TEST_KEY") };
        assert_eq!(s.to_cfg().key, "typed-in", "with no variable set the stored key is used");
    }

    /// A fake server that answers one request the way a provider would, so both shapes are proved
    /// end to end: the path asked, the header sent, and the reply read. Nothing leaves the machine.
    fn serve_once(status: &str, body: &str) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let reply = body.to_string();
        let head = status.to_string();
        std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                // read the head, then exactly Content-Length more
                loop {
                    let n = sock.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&buf[..n]);
                    if let Some(split) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&raw[..split]).to_lowercase();
                        let want = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if raw.len() >= split + 4 + want {
                            break;
                        }
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&raw).to_string());
                let msg = format!(
                    "HTTP/1.1 {head}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = sock.write_all(msg.as_bytes());
            }
        });
        (format!("http://{}", addr), rx)
    }

    #[test]
    fn an_ollama_reply_is_read_and_the_key_is_sent_as_a_bearer() {
        let (base, seen) = serve_once(
            "200 OK",
            r#"{"message":{"role":"assistant","content":"ready"},"done":true}"#,
        );
        let cfg = Cfg {
            provider: Provider::Ollama,
            url: base,
            model: "llama3.2".into(),
            key: "secret-key".into(),
            num_ctx: 4096,
        };
        let said = chat(&cfg, "sys", "user", false, Duration::from_secs(10), 4096).unwrap();
        assert_eq!(said, "ready");

        let req = seen.recv().unwrap();
        assert!(req.starts_with("POST /api/chat "), "asked {}", &req[..40.min(req.len())]);
        let lower = req.to_lowercase();
        assert!(lower.contains("authorization: bearer secret-key"));
        assert!(lower.contains(r#""model":"llama3.2""#));
        assert!(lower.contains(r#""stream":false"#));
    }

    #[test]
    fn an_openai_reply_is_read_from_choices() {
        let (base, seen) = serve_once(
            "200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":"ready"},"finish_reason":"stop"}]}"#,
        );
        let cfg = Cfg {
            provider: Provider::OpenAi,
            url: format!("{base}/v1"),
            model: "gpt-4o-mini".into(),
            key: String::new(),
            num_ctx: 8192,
        };
        let said = chat(&cfg, "sys", "user", false, Duration::from_secs(10), 8192).unwrap();
        assert_eq!(said, "ready");

        let req = seen.recv().unwrap();
        assert!(req.starts_with("POST /v1/chat/completions "), "asked {}", &req[..40.min(req.len())]);
        let lower = req.to_lowercase();
        assert!(!lower.contains("authorization:"), "no key configured, no header");
        assert!(lower.contains(r#""temperature":0"#));
    }

    /// The provider's own sentence is the useful one: a refused key has to say so, not "error".
    #[test]
    fn a_refused_request_says_what_the_server_said() {
        let (base, _seen) = serve_once(
            "401 Unauthorized",
            r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#,
        );
        let cfg = Cfg {
            provider: Provider::OpenAi,
            url: format!("{base}/v1"),
            model: "gpt-4o-mini".into(),
            key: "wrong".into(),
            num_ctx: 8192,
        };
        let err = chat(&cfg, "sys", "user", false, Duration::from_secs(10), 8192).unwrap_err();
        assert!(err.contains("401"), "{err}");
        assert!(err.contains("Incorrect API key provided"), "{err}");
    }

}
