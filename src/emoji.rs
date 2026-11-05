//! Colour emoji, drawn as pictures over the monochrome glyph.
//!
//! `theme.rs` gives every family a monochrome emoji face because that is **all epaint can draw from a
//! font**: it rasterises with `ab_glyph`, which speaks outline fonts only — no COLR, no CBDT/CBLC, no
//! sbix. So a colour emoji cannot come from a font in this stack, and never will without swapping the
//! rasteriser.
//!
//! What it *can* do is paint. The character stays in the text (it is what the `.md` carries and what
//! the caret, the spell check and the search see) and its monochrome glyph reserves the cell; this
//! module draws the Twemoji picture into that cell, on top of the glyph. `assets/emoji.pack` holds the
//! pictures (see `scripts/build-emoji-pack.py` in the skill): one blob, `include_bytes!`, so the
//! binary carries its emoji the way it carries its fonts.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The pack: `NCEMOJI1 | u32 count | count × (u16 key_len, key, u32 off, u32 len) | blobs`.
static PACK: &[u8] = include_bytes!("../assets/emoji.pack");

fn index() -> &'static HashMap<&'static str, (&'static [u8], usize, usize)> {
    static INDEX: OnceLock<HashMap<&'static str, (&'static [u8], usize, usize)>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut out = HashMap::new();
        if PACK.len() < 12 || &PACK[..8] != b"NCEMOJI1" {
            return out;
        }
        let count = u32::from_le_bytes(PACK[8..12].try_into().unwrap()) as usize;
        let mut at = 12usize;
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            if at + 2 > PACK.len() {
                break;
            }
            let key_len = u16::from_le_bytes(PACK[at..at + 2].try_into().unwrap()) as usize;
            at += 2;
            if at + key_len + 8 > PACK.len() {
                break;
            }
            let key = std::str::from_utf8(&PACK[at..at + key_len]).ok();
            at += key_len;
            let off = u32::from_le_bytes(PACK[at..at + 4].try_into().unwrap()) as usize;
            let len = u32::from_le_bytes(PACK[at + 4..at + 8].try_into().unwrap()) as usize;
            at += 8;
            if let Some(key) = key {
                records.push((key, off, len));
            }
        }
        // the blob starts after the index
        let blob = at;
        for (key, off, len) in records {
            let start = blob + off;
            if start + len <= PACK.len() {
                out.insert(key, (PACK, start, len));
            }
        }
        out
    })
}

/// Whether this cluster has a colour picture.
pub fn has(cluster: &str) -> bool {
    index().contains_key(cluster)
}

/// The picture of one cluster, as a texture. Decoded on first use and kept, so a page full of the same
/// emoji costs one texture; the cache is trimmed to the most recently used so a long note cannot grow
/// it without limit.
pub fn texture(ctx: &egui::Context, cluster: &str) -> Option<egui::TextureHandle> {
    static CACHE: OnceLock<Mutex<(HashMap<String, (egui::TextureHandle, u64)>, u64)>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new((HashMap::new(), 0)));
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    let frame = {
        guard.1 = guard.1.wrapping_add(1);
        guard.1
    };
    if let Some((tex, used)) = guard.0.get_mut(cluster) {
        *used = frame;
        return Some(tex.clone());
    }
    let (_, start, len) = *index().get(cluster)?;
    let bytes = &PACK[start..start + len];
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    let texture = ctx.load_texture(
        format!("emoji:{cluster}"),
        egui::ColorImage::from_rgba_unmultiplied(size, &pixels),
        egui::TextureOptions::LINEAR,
    );
    guard.0.insert(cluster.to_string(), (texture.clone(), frame));
    if guard.0.len() > TEXTURE_BUDGET {
        // drop the coldest half in one go rather than one texture every frame
        let mut ages: Vec<(u64, String)> = guard.0.iter().map(|(k, (_, f))| (*f, k.clone())).collect();
        ages.sort();
        for (_, k) in ages.into_iter().take(guard.0.len() / 2) {
            guard.0.remove(&k);
        }
    }
    Some(texture)
}

/// How many emoji pictures stay resident. 256 textures of 72×72 RGBA is ~5 MB of atlas, and a note
/// showing more than that many *different* emoji at once is a different problem.
const TEXTURE_BUDGET: usize = 256;

/// The clusters of a string: base + variation selector, skin tone or ZWJ continuation. Returns
/// `(char_start, char_end, slice)` so a caller can turn the ends into cursor positions.
fn clusters(text: &str) -> Vec<(usize, usize, &str)> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let start_byte = chars[i].0;
        let mut end = i + 1;
        // a variation selector or a skin tone belongs to this cluster
        while end < chars.len() {
            let c = chars[end].1;
            let joins = matches!(c, '\u{FE0F}' | '\u{FE0E}')
                || ('\u{1F3FB}'..='\u{1F3FF}').contains(&c);
            if joins {
                end += 1;
            } else {
                break;
            }
        }
        // two regional indicators are one flag (a single one draws as a boxed letter)
        let ri = |c: char| ('\u{1F1E6}'..='\u{1F1FF}').contains(&c);
        if ri(chars[i].1) && end < chars.len() && ri(chars[end].1) {
            end += 1;
        }
        // and a ZWJ pulls the next base (and its own selector) in
        while end + 1 < chars.len() && chars[end].1 == '\u{200D}' {
            let mut after = end + 2;
            while after < chars.len() {
                let c = chars[after].1;
                if matches!(c, '\u{FE0F}' | '\u{FE0E}') || ('\u{1F3FB}'..='\u{1F3FF}').contains(&c) {
                    after += 1;
                } else {
                    break;
                }
            }
            end = after;
        }
        let end_byte = chars.get(end).map(|(b, _)| *b).unwrap_or(text.len());
        out.push((i, end, &text[start_byte..end_byte]));
        i = end;
    }
    out
}

/// Paint every emoji in `text` into its cell of `galley`, which was drawn at `origin`.
///
/// `painter` should already be clipped to the block (an emoji must not bleed into the line below).
/// A cluster the pack has no picture for falls back to its first character — so a ZWJ sequence whose
/// composed picture is missing still shows its parts in colour rather than dropping back to outlines.
pub fn paint(
    painter: &egui::Painter,
    text: &str,
    galley: &egui::Galley,
    origin: egui::Pos2,
    ctx: &egui::Context,
    behind: egui::Color32,
) {
    for (start, end, cluster) in clusters(text) {
        let key = if has(cluster) {
            cluster
        } else {
            let first = cluster.chars().next();
            match first {
                Some(c) if has(&c.to_string()) => {
                    // paint the base on its own, at its own advance
                    let key = c.to_string();
                    let (cell, rect) = match cell_of(galley, origin, start, start + 1) {
                        Some(v) => v,
                        None => continue,
                    };
                    if let Some(tex) = texture(ctx, &key) {
                        cover(painter, cell, behind);
                        painter.image(
                            tex.id(),
                            rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                    continue;
                }
                _ => continue,
            }
        };
        let Some((cell, rect)) = cell_of(galley, origin, start, end) else {
            continue;
        };
        if let Some(tex) = texture(ctx, key) {
            cover(painter, cell, behind);
            painter.image(
                tex.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
}

/// Paint the text's own background over the cells an emoji is about to cover.
///
/// A single emoji occupies about one advance and its picture fills it, but a ZWJ family or a flag is
/// several advances wide in a monochrome face (there is no ligature for it) while the picture is one
/// square: without this, the faces the sequence is made of would stand either side of the picture.
fn cover(painter: &egui::Painter, cell: egui::Rect, behind: egui::Color32) {
    if cell.width() > cell.height() * 1.05 {
        painter.rect_filled(cell, 0.0, behind);
    }
}

/// The square to paint a cluster into: the caret rects at its two ends give the cell, and the picture
/// is a square of the line's height centred in it (a glyph's advance is not square, a picture is).
fn cell_of(
    galley: &egui::Galley,
    origin: egui::Pos2,
    start: usize,
    end: usize,
) -> Option<(egui::Rect, egui::Rect)> {
    let a = galley.pos_from_cursor(egui::text::CCursor::new(start));
    let b = galley.pos_from_cursor(egui::text::CCursor::new(end));
    // a cluster that wrapped leaves b on the next row; then the cell is the rest of the row
    let (left, right) = if (b.center().y - a.center().y).abs() < 1.0 {
        (a.left(), b.left())
    } else {
        (a.left(), a.left() + a.height())
    };
    let width = (right - left).max(1.0);
    let cell = egui::Rect::from_min_size(
        egui::pos2(origin.x + left, origin.y + a.top()),
        egui::vec2(width, a.height()),
    );
    // the picture is a square of the line height, centred in the cell
    let side = width.min(a.height()) * 0.98;
    let rect = egui::Rect::from_center_size(cell.center(), egui::vec2(side, side));
    Some((cell, rect))
}

/// One picture, centred in `rect` — the picker's grid cell, where there is no text to sit behind.
pub fn paint_in_rect(
    painter: &egui::Painter,
    rect: egui::Rect,
    ch: char,
    ctx: &egui::Context,
) -> bool {
    let key = ch.to_string();
    let Some(tex) = texture(ctx, &key) else {
        return false;
    };
    let side = rect.width().min(rect.height());
    let dest = egui::Rect::from_center_size(rect.center(), egui::vec2(side, side));
    painter.image(
        tex.id(),
        dest,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pack has to parse, and the emoji people actually type have to be in it.
    #[test]
    fn the_pack_carries_the_everyday_emoji() {
        assert!(index().len() > 1500, "only {} pictures in the pack", index().len());
        for s in ["😀", "🔥", "🧠", "🌍", "✔", "👨‍👩‍👧", "🇬🇧"] {
            assert!(has(s), "{s} has no picture");
        }
        // a plain letter is not an emoji and must not be in the pack; nor is ✓, which has no
        // emoji presentation (the coloured one is ✔ U+2714)
        assert!(!has("a"));
        assert!(!has("✓"));
        assert!(has("✔"));
    }

    /// Clustering: a family is one cluster, a base with a variation selector is one cluster.
    #[test]
    fn a_cluster_takes_its_selectors_and_zwj_with_it() {
        let fam = "a👨‍👩‍👧b";
        let c: Vec<&str> = clusters(fam).into_iter().map(|(_, _, s)| s).collect();
        assert_eq!(c, vec!["a", "👨‍👩‍👧", "b"], "{c:?}");
        let vs = "★\u{FE0F}!";
        let c: Vec<&str> = clusters(vs).into_iter().map(|(_, _, s)| s).collect();
        assert_eq!(c, vec!["★\u{FE0F}", "!"], "{c:?}");
        // a flag is two regional indicators, and stays one cluster
        let flag = "\u{1F1EC}\u{1F1E7}!";
        let c: Vec<&str> = clusters(flag).into_iter().map(|(_, _, s)| s).collect();
        assert_eq!(c, vec!["\u{1F1EC}\u{1F1E7}", "!"], "{c:?}");
    }
}
