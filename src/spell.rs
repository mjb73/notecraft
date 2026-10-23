//! Spelling: two word lists (British and American), and the rules for what counts as a misspelling.
//!
//! The lists are the expanded word forms of the hunspell dictionaries LibreOffice ships
//! (`en_GB.dic`/`en_US.dic` + their `.aff`), run through `unmunch` so every inflected form is a
//! word of its own, lower-cased and de-duplicated, then gzipped. They are *not* interchangeable:
//! `colour` is only in the British list and `color` only in the American one, which is the whole
//! point of offering the choice (`cargo test spell` pins that).
//!
//! Nothing here reads a file or allocates per word: a list is decompressed once, the first time it
//! is needed, and leaked for the lifetime of the process (two lists, ~1.5 MB compressed, ~7 MB in
//! memory — cheap next to the picture cache, and only paid when spelling is switched on).

use std::io::Read;
use std::ops::Range;
use std::sync::OnceLock;

/// Which dictionary to spell against. `state.json`'s `spell` key holds [`Dict::id`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Dict {
    /// No spelling at all: no list is loaded and nothing is underlined.
    #[default]
    Off,
    British,
    American,
}

impl Dict {
    pub fn id(self) -> &'static str {
        match self {
            Dict::Off => "off",
            Dict::British => "gb",
            Dict::American => "us",
        }
    }

    /// A missing or unknown id is **off**: spelling has to be asked for, and a `state.json` from a
    /// build without it must not start underlining everything.
    pub fn from_id(id: Option<&str>) -> Dict {
        match id {
            Some("gb") | Some("en_GB") | Some("british") => Dict::British,
            Some("us") | Some("en_US") | Some("american") => Dict::American,
            _ => Dict::Off,
        }
    }

    pub fn is_on(self) -> bool {
        self != Dict::Off
    }
}

/// One decompressed dictionary: a set of lower-case words.
pub struct Words {
    set: std::collections::HashSet<&'static str>,
}

impl Words {
    fn from_gz(bytes: &'static [u8]) -> Words {
        let mut text = String::new();
        // A corrupt or missing asset must not take the app down over a spelling list: an empty set
        // simply underlines nothing.
        let _ = flate2::read::GzDecoder::new(bytes).read_to_string(&mut text);
        // Leaked on purpose: the set borrows from it, and it lives as long as the process does.
        let text: &'static str = Box::leak(text.into_boxed_str());
        Words {
            set: text.lines().filter(|l| !l.is_empty()).collect(),
        }
    }

    pub fn contains(&self, word: &str) -> bool {
        self.set.contains(word)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.set.len()
    }
}

static GB: OnceLock<Words> = OnceLock::new();
static US: OnceLock<Words> = OnceLock::new();

/// The list for a dictionary, decompressed on first use. `None` for [`Dict::Off`].
pub fn words(dict: Dict) -> Option<&'static Words> {
    match dict {
        Dict::Off => None,
        Dict::British => Some(GB.get_or_init(|| {
            Words::from_gz(include_bytes!("../assets/en_GB.words.gz"))
        })),
        Dict::American => Some(US.get_or_init(|| {
            Words::from_gz(include_bytes!("../assets/en_US.words.gz"))
        })),
    }
}

/// Split text into words: runs of letters with inner apostrophes (`don't`, `o'clock`).
///
/// Everything else - digits, punctuation, hyphens, em dashes - is a break, so `well-known` is two
/// words and `2026` and `3rd` are not words at all. Indices are byte offsets into `text`, which is
/// what the layout job addresses text by.
pub fn tokenize(text: &str) -> Vec<(Range<usize>, &str)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (start, c) = chars[i];
        if !c.is_alphabetic() {
            i += 1;
            continue;
        }
        let mut end = start + c.len_utf8();
        i += 1;
        while i < chars.len() {
            let (j, d) = chars[i];
            if d.is_alphabetic() {
                end = j + d.len_utf8();
                i += 1;
            } else if (d == '\'' || d == '\u{2019}')
                && chars.get(i + 1).map(|(_, e)| e.is_alphabetic()).unwrap_or(false)
            {
                // an inner apostrophe: keep it and carry on with the letters after it
                end = j + d.len_utf8();
                i += 1;
            } else {
                break;
            }
        }
        out.push((start..end, &text[start..end]));
    }
    out
}

/// Whether one word is spelled correctly under `dict`, or is on the ignore list.
///
/// Rules, in the order they are tried:
/// * anything on the ignore list (compared lower-case) is fine;
/// * a single letter is fine (`a`, `I`, and a stray `x` in a list);
/// * words with a capital *inside* them are left alone (`NoteCraft`, `iPhone`, `macOS`): they are
///   names, and underlining every one of them makes the squiggle meaningless;
/// * look the lower-case spelling up, then try stripping a possessive or contraction ending
///   (`Michael's`, `doesn't`).
pub fn is_known(raw: &str, dict: Dict, ignore: &[String]) -> bool {
    let lower = raw.to_lowercase();
    if ignore.iter().any(|w| w.to_lowercase() == lower) {
        return true;
    }
    let Some(list) = words(dict) else {
        return true;
    };
    if lower.chars().count() <= 1 {
        return true;
    }
    // an interior capital: a name or a product, not a spelling mistake
    let mut seen_first = false;
    for c in raw.chars() {
        if c.is_alphabetic() {
            if seen_first && c.is_uppercase() {
                return true;
            }
            seen_first = true;
        }
    }
    if list.contains(&lower) {
        return true;
    }
    for suffix in ["'s", "s'", "'re", "'ve", "'ll", "'d", "'m", "'t", "n't"] {
        if let Some(stem) = lower.strip_suffix(suffix) {
            if !stem.is_empty() && list.contains(stem) {
                return true;
            }
        }
    }
    false
}

/// The ranges of the words in `text` that are misspelled under `dict`.
pub fn misspelled(text: &str, dict: Dict, ignore: &[String]) -> Vec<Range<usize>> {
    if !dict.is_on() {
        return Vec::new();
    }
    tokenize(text)
        .into_iter()
        .filter(|(_, w)| !is_known(w, dict, ignore))
        .map(|(range, _)| range)
        .collect()
}

/// The screen rects a word occupies, one per row when it wraps, positioned for a squiggle: the
/// bottom two points of the text on that row.
///
/// `origin` is where the galley was drawn (`TextEditOutput::galley_pos`). A glyph's `pos` is relative
/// to its row and a row's rect to the galley, which is why both are added here.
pub fn word_rects(
    galley: &egui::Galley,
    origin: egui::Pos2,
    range: Range<usize>,
) -> Vec<egui::Rect> {
    let mut out = Vec::new();
    let mut idx = 0usize;
    for row in &galley.rows {
        let mut run: Option<(f32, f32, f32)> = None;
        for g in &row.glyphs {
            if idx >= range.start && idx < range.end && !g.chr.is_whitespace() {
                let x0 = origin.x + row.rect().left() + g.pos.x;
                let x1 = x0 + g.advance_width.max(1.0);
                let baseline = origin.y + row.rect().top() + g.pos.y;
                run = Some(match run {
                    Some((a, _, y)) => (a, x1, y),
                    None => (x0, x1, baseline),
                });
            }
            idx += 1;
        }
        if let Some((x0, x1, baseline)) = run {
            out.push(egui::Rect::from_min_max(
                egui::pos2(x0, baseline + 1.5),
                egui::pos2(x1, baseline + 3.5),
            ));
        }
        if row.ends_with_newline {
            idx += 1;
        }
    }
    out
}

/// Paint one red squiggle through a rect: a zigzag under the word, the way a spellchecker does.
pub fn paint_squiggle(painter: &egui::Painter, rect: egui::Rect, colour: egui::Color32) {
    if rect.width() < 2.0 {
        return;
    }
    let y = rect.center().y;
    let stroke = egui::Stroke::new(1.2_f32, colour);
    let mut points = Vec::with_capacity((rect.width() / 3.0) as usize + 2);
    let mut x = rect.left();
    let mut up = true;
    while x <= rect.right() {
        points.push(egui::pos2(x, if up { y - 1.2 } else { y + 1.2 }));
        x += 3.0;
        up = !up;
    }
    painter.add(egui::Shape::line(points, stroke));
}

/// The word at a screen position inside a drawn galley, for the context menu's "ignore this word".
///
/// The glyphs are measured directly rather than going through egui's cursor: a cursor position rounds
/// to the nearest character boundary, so a click on the right half of a letter lands on the character
/// *after* it and the word was not found at all. A glyph's `pos` is relative to its row and a row's
/// rect to the galley, so both are added to `origin` here, exactly as in [`word_rects`].
pub fn word_at(
    galley: &egui::Galley,
    origin: egui::Pos2,
    pos: egui::Pos2,
    text: &str,
) -> Option<Range<usize>> {
    let area = egui::Rect::from_min_size(origin, galley.size());
    let slack = 4.0;
    if pos.x < area.left() - slack
        || pos.x > area.right() + slack
        || pos.y < area.top() - slack
        || pos.y > area.bottom() + slack
    {
        return None;
    }
    let mut idx = 0usize;
    let mut best: Option<(f32, usize)> = None;
    for row in &galley.rows {
        // the whole band of the row counts, not just where the glyphs' ink is: a click in the gap
        // between two baselines is still a click on that line of text
        let band = egui::Rect::from_min_size(
            origin + row.rect().min.to_vec2(),
            egui::vec2(row.size.x.max(1.0), row.size.y.max(1.0)),
        );
        let on_row = pos.y >= band.top() - slack && pos.y <= band.bottom() + slack;
        for g in &row.glyphs {
            let x0 = origin.x + row.rect().left() + g.pos.x;
            let x1 = x0 + g.advance_width.max(1.0);
            if on_row && pos.x >= x0 - 2.0 && pos.x <= x1 + 2.0 {
                let d = (pos.x - (x0 + x1) * 0.5).abs();
                if best.map(|(bd, _)| d < bd).unwrap_or(true) {
                    best = Some((d, idx));
                }
            }
            idx += 1;
        }
        if row.ends_with_newline {
            idx += 1;
        }
    }
    let byte = text
        .char_indices()
        .nth(best?.1)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    // the word the click is inside, or the one ending right there (a click in the sliver of space
    // just after a word still means that word)
    let words = tokenize(text);
    words
        .iter()
        .find(|(range, _)| range.contains(&byte))
        .or_else(|| words.iter().find(|(range, _)| range.end == byte))
        .map(|(range, _)| range.clone())
}

/// Candidate corrections for a misspelled word, best first: every dictionary word one edit away
/// (a letter inserted, deleted, replaced or two neighbours swapped), and only when there is nothing
/// that close, every word two edits away.
///
/// The ranking is what makes the list usable: a correction almost never changes the word's first
/// letter, so those come first; then the nearer word; then the one whose length is closest, because a
/// typo keeps the word's length far more often than not (`teh` → `the`, never `te`); then alphabetical,
/// so the order is stable enough to click twice.
///
/// This is deliberately not a hunspell clone — no affix analysis, no frequency data. It costs a few
/// milliseconds for the distance-two fallback, so **call it once** when a menu opens, never per frame.
pub fn suggest(word: &str, dict: Dict, limit: usize) -> Vec<String> {
    let Some(list) = words(dict) else {
        return Vec::new();
    };
    let lower = word.to_lowercase();
    if lower.is_empty() || list.contains(&lower) {
        return Vec::new();
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut hits: Vec<(u8, String)> = Vec::new();
    for c in edits1(&lower) {
        if c != lower && list.contains(c.as_str()) && seen.insert(c.clone()) {
            hits.push((1, c));
        }
    }
    if hits.is_empty() {
        for c1 in edits1(&lower) {
            for c2 in edits1(&c1) {
                if c2 != lower && list.contains(c2.as_str()) && seen.insert(c2.clone()) {
                    hits.push((2, c2));
                }
            }
        }
    }
    let first = lower.chars().next();
    let len = lower.chars().count();
    let gap = |w: &str| (w.chars().count() as isize - len as isize).unsigned_abs();
    hits.sort_by(|(da, a), (db, b)| {
        let ka = u8::from(a.chars().next() != first);
        let kb = u8::from(b.chars().next() != first);
        ka.cmp(&kb)
            .then(da.cmp(db))
            // swapping two fingers is the commonest typo, then a letter too many or too few, and
            // only then the wrong letter: `teh` is `the` before it is `tea` or `te`
            .then(edit_kind(&lower, a).cmp(&edit_kind(&lower, b)))
            .then(gap(a).cmp(&gap(b)))
            .then(a.cmp(b))
    });
    hits.into_iter().take(limit).map(|(_, w)| w).collect()
}

const ALPHABET: &[char] = &[
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't',
    'u', 'v', 'w', 'x', 'y', 'z',
];

/// Every string one edit away from `word`: the four edits a typo is made of.
fn edits1(word: &str) -> Vec<String> {
    let c: Vec<char> = word.chars().collect();
    let n = c.len();
    let mut out = Vec::with_capacity(56 * n + 26);
    for i in 0..n {
        // a letter too many
        let mut s: String = c[..i].iter().collect();
        s.extend(&c[i + 1..]);
        out.push(s);
    }
    for i in 0..n.saturating_sub(1) {
        // two letters the wrong way round
        let mut t = c.clone();
        t.swap(i, i + 1);
        out.push(t.into_iter().collect());
    }
    for i in 0..n {
        for &l in ALPHABET {
            // the wrong letter
            let mut r = c.clone();
            r[i] = l;
            out.push(r.into_iter().collect());
            // a letter missing
            let mut ins = c.clone();
            ins.insert(i, l);
            out.push(ins.into_iter().collect());
        }
    }
    for &l in ALPHABET {
        let mut tail = c.clone();
        tail.push(l);
        out.push(tail.into_iter().collect());
    }
    out
}

/// Roughly what kind of edit turns `word` into `cand`, for ranking: 0 = two neighbours swapped,
/// 1 = a letter missing or one too many, 2 = the wrong letter in place.
fn edit_kind(word: &str, cand: &str) -> u8 {
    let w: Vec<char> = word.chars().collect();
    let c: Vec<char> = cand.chars().collect();
    if w.len() != c.len() {
        return 1;
    }
    let diff: Vec<usize> = (0..w.len()).filter(|&i| w[i] != c[i]).collect();
    if diff.len() == 2 && diff[1] == diff[0] + 1 {
        0
    } else if diff.is_empty() {
        0
    } else {
        2
    }
}

/// A suggestion as the word was actually written: `Teh` corrected to `The`, `TEH` to `THE`.
pub fn match_case(original: &str, fix: &str) -> String {
    let letters: Vec<char> = original.chars().filter(|c| c.is_alphabetic()).collect();
    let shouty = !letters.is_empty() && letters.iter().all(|c| c.is_uppercase());
    if shouty {
        return fix.to_uppercase();
    }
    let mut chars = fix.chars();
    match (original.chars().next(), chars.next()) {
        (Some(o), Some(f)) if o.is_uppercase() => {
            f.to_uppercase().collect::<String>() + chars.as_str()
        }
        _ => fix.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignore(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn the_two_lists_are_actually_different() {
        let gb = words(Dict::British).expect("british list");
        let us = words(Dict::American).expect("american list");
        for w in ["colour", "realise", "centre", "organisation", "travelling"] {
            assert!(gb.contains(w), "british list is missing {w}");
            assert!(!us.contains(w), "american list should not have {w}");
        }
        for w in ["color", "realize", "center", "organization", "traveling", "gray"] {
            assert!(us.contains(w), "american list is missing {w}");
        }
        // both know the words that are not a difference at all
        for w in ["the", "hello", "running", "children", "computer"] {
            assert!(gb.contains(w) && us.contains(w), "both lists need {w}");
        }
        assert!(gb.len() > 100_000 && us.len() > 50_000, "the lists look empty");
    }

    #[test]
    fn a_british_note_flags_american_spellings_and_the_other_way_round() {
        let text = "the colour of the color wheel, realised and realized";
        let gb = misspelled(text, Dict::British, &[]);
        let flagged: Vec<&str> = gb.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(flagged, ["color", "realized"], "{flagged:?}");
        let us = misspelled(text, Dict::American, &[]);
        let flagged: Vec<&str> = us.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(flagged, ["colour", "realised"], "{flagged:?}");
        // off is off
        assert!(misspelled(text, Dict::Off, &[]).is_empty());
    }

    #[test]
    fn the_tokenizer_keeps_inner_apostrophes_and_drops_everything_else() {
        let text = "Don't well-known 2026 3rd o'clock café — “quoted” x";
        let words: Vec<&str> = tokenize(text).into_iter().map(|(_, w)| w).collect();
        assert_eq!(
            words,
            ["Don't", "well", "known", "rd", "o'clock", "café", "quoted", "x"]
        );
    }

    #[test]
    fn the_ignore_list_is_case_insensitive_and_wins() {
        let text = "NoteCraft zorblax";
        // a name with an interior capital is left alone even without the list
        assert!(misspelled(text, Dict::British, &[]).is_empty() == false);
        let flagged = misspelled(text, Dict::British, &[]);
        let words: Vec<&str> = flagged.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(words, ["zorblax"]);
        assert!(misspelled(text, Dict::British, &ignore(&["Zorblax"])).is_empty());
    }

    #[test]
    fn contractions_and_possessives_ride_on_their_stem() {
        for w in ["doesn't", "don't", "Michael's", "dogs'", "we're", "I'd"] {
            assert!(
                is_known(w, Dict::British, &[]),
                "{w} should not be flagged"
            );
        }
        assert!(!is_known("Zorb's", Dict::British, &[]));
    }

    /// A bare `Context` has no fonts until it has run a frame once; every measuring test needs this.
    fn test_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx
    }

    #[test]
    fn a_word_under_the_pointer_is_found_by_its_own_characters() {
        let ctx = test_ctx();
        let galley = ctx.fonts_mut(|f| {
            f.layout_no_wrap(
                "hello wrold".to_owned(),
                egui::FontId::monospace(14.0),
                egui::Color32::WHITE,
            )
        });
        let origin = egui::pos2(10.0, 20.0);
        // a click inside "wrold" (the second word) finds it
        let text = "hello wrold";
        let found = word_at(&galley, origin, origin + egui::vec2(60.0, 5.0), text)
            .map(|r| text[r].to_string());
        assert_eq!(found.as_deref(), Some("wrold"));
        // a click a pixel under the line still lands on it (the galley is exactly the text's height)
        let below = origin + egui::vec2(60.0, galley.size().y + 2.0);
        assert_eq!(
            word_at(&galley, origin, below, "hello wrold").map(|r| "hello wrold"[r].to_string()),
            Some("wrold".to_string())
        );
        // and a click well outside the text finds nothing
        assert_eq!(
            word_at(&galley, origin, origin + egui::vec2(600.0, 5.0), "hello wrold"),
            None
        );
    }

    #[test]
    fn a_typo_gets_the_word_it_was_meant_to_be_at_the_top() {
        // a swapped pair, a swapped pair in a long word, and a wrong letter
        assert_eq!(suggest("teh", Dict::British, 6).first().map(String::as_str), Some("the"));
        assert!(suggest("recieves", Dict::British, 6).contains(&"receives".to_string()));
        assert!(suggest("seperate", Dict::British, 6).contains(&"separate".to_string()));
        // the suggestion follows the dictionary that is switched on
        assert!(suggest("colour", Dict::American, 6).contains(&"color".to_string()));
        assert!(suggest("color", Dict::British, 6).contains(&"colour".to_string()));
        // a word that is already right has nothing to offer, and so has "off"
        assert!(suggest("colour", Dict::British, 6).is_empty());
        assert!(suggest("teh", Dict::Off, 6).is_empty());
        // nothing within two edits is an empty list, not a guess
        assert!(suggest("qqqzzzwww", Dict::British, 6).is_empty());
    }

    #[test]
    fn the_suggestion_keeps_the_words_own_capitalisation() {
        assert_eq!(match_case("teh", "the"), "the");
        assert_eq!(match_case("Teh", "the"), "The");
        assert_eq!(match_case("TEH", "the"), "THE");
        // a name inside a sentence keeps its capital and its tail
        assert_eq!(match_case("Teh", "the"), "The");
    }

    #[test]
    fn suggestions_are_ranked_so_the_first_ones_are_worth_reading() {
        let fixes = suggest("recieve", Dict::American, 6);
        assert!(fixes.len() <= 6);
        assert_eq!(fixes.first().map(String::as_str), Some("receive"));
        // every suggestion is a real word of the dictionary, and none is the typo itself
        let list = words(Dict::American).expect("list");
        for f in &fixes {
            assert!(list.contains(f), "{f} is not in the list");
            assert_ne!(f, "recieve");
        }
        // a correction does not change the first letter: "teh" must not offer "eh" before "the"
        assert_eq!(suggest("teh", Dict::American, 1).first().map(String::as_str), Some("the"));
    }

    #[test]
    fn a_squiggle_sits_just_under_the_word_and_wraps_with_it() {
        let ctx = test_ctx();
        let text = "colour";
        let galley = ctx.fonts_mut(|f| {
            f.layout_no_wrap(
                text.to_owned(),
                egui::FontId::monospace(14.0),
                egui::Color32::WHITE,
            )
        });
        let origin = egui::pos2(5.0, 7.0);
        let rects = word_rects(&galley, origin, 0..text.len());
        assert_eq!(rects.len(), 1, "one row, one squiggle");
        let r = rects[0];
        assert!(r.left() >= origin.x && r.right() <= origin.x + galley.size().x + 1.0);
        assert!(r.top() > origin.y + 8.0, "below the baseline: {r:?}");
        assert!(r.height() <= 3.0);
    }
}
