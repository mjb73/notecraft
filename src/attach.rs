//! Getting files and images into a page.
//!
//! Three routes, in order of how reliable they are on KDE Wayland:
//!
//! 1. **Clipboard** (`Ctrl+V`): the compositor hands us `image/png` when an image
//!    was copied, or a file list / `text/uri-list` when files were copied in
//!    Dolphin. This is the Wayland-native stand-in for dragging.
//! 2. **File chooser** (`Attach`): kdialog, falling back to zenity.
//! 3. **Drag and drop** — wired up in `app.rs`, but winit's Wayland backend
//!    implements no data-device protocol at all (no `WindowEvent::DroppedFile`),
//!    so on a Wayland session this route never fires. It works under X11/Windows.

use crate::vault::{ASSETS, Ops};
use image::RgbaImage;
use std::io;
use std::path::{Path, PathBuf};

/// Formats the bundled `image` crate decodes, i.e. what can actually be shown on a page.
const IMAGE_EXTS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];
/// Formats that are still *pictures* when they arrive from a browser or a drop, but that this build
/// cannot decode (no codec is compiled in). They are kept with their own extension — a `.bmp`
/// downloaded as `photo.bmp.png` was a lie about the bytes.
const IMAGE_EXTS_OTHER: [&str; 5] = ["bmp", "avif", "tiff", "svg", "ico"];
/// `IMAGE_EXTS` + `IMAGE_EXTS_OTHER`, for recognising an image URL.
const IMAGE_EXTS_EXT: [&str; 10] = [
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".avif", ".tiff", ".svg", ".ico",
];

pub fn is_image(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .map(|e| IMAGE_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

/// Download an image named by a URL. Used when a browser drag offers the image's
/// URL instead of its bytes (the usual case for cross-origin images).
pub fn snippet_for_url(page_path: &Path, url: &str) -> io::Result<String> {
    let raw = url.split(['?', '#']).next().unwrap_or(url);
    let mut name: String = raw
        .rsplit('/')
        .next()
        .unwrap_or("image")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name.starts_with('.') {
        name = "image".to_string();
    }
    let ext = Path::new(&name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let known = !ext.is_empty()
        && (IMAGE_EXTS.contains(&ext.as_str()) || IMAGE_EXTS_OTHER.contains(&ext.as_str()));
    if !known {
        // no extension at all (a `download?id=7` URL): PNG is the likeliest body for an <img>
        name = format!("{name}.png");
    }
    let dir = page_path.parent().unwrap_or(Path::new(".")).join(ASSETS);
    std::fs::create_dir_all(&dir)?;
    let name = free_name(&dir, &name);
    let target = dir.join(&name);
    // In-process (rustls), so a Windows or macOS install needs no `wget` on PATH.
    let size = crate::platform::download_to_file(url, &target).map_err(|e| {
        let _ = std::fs::remove_file(&target);
        io::Error::other(format!("could not download {url}: {e}"))
    })?;
    if size > 0 {
        Ok(format!("![{name}]({ASSETS}/{name})"))
    } else {
        let _ = std::fs::remove_file(&target);
        Err(io::Error::other(format!("could not download {url}")))
    }
}

/// A free name in `dir` for `name`: `photo.png` becomes `photo 2.png` when the first one is taken.
/// Copying onto an existing name silently changed every earlier reference to that picture.
/// A file name that is safe to put in a markdown reference.
///
/// `![photo (1).png](assets/photo (1).png)` parses as a destination ending at the first `)` — this
/// app's own parser and every other one — so the picture vanished and the next save wrote the
/// truncated reference back to the `.md`. Brackets and whitespace are the other hazards.
fn safe_asset_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '(' | ')' | '[' | ']' | ' ' | '\t' | '\n' | '\r' | '#' | '?' | '%' => '-',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim_matches(['.', '-']).to_string();
    if trimmed.is_empty() {
        "image.png".to_string()
    } else {
        trimmed
    }
}

fn free_name(dir: &Path, name: &str) -> String {
    let name = &safe_asset_name(name);
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image".into());
    let ext = p.extension().map(|e| e.to_string_lossy().to_string());
    let mut cand = name.to_string();
    let mut n = 2usize;
    while dir.join(&cand).exists() {
        cand = match &ext {
            Some(e) => format!("{stem} {n}.{e}"),
            None => format!("{stem} {n}"),
        };
        n += 1;
    }
    cand
}

/// Take at most `len` bytes from `from`, trimmed back to a character boundary.
fn slice_bounded(s: &str, from: usize, len: usize) -> &str {
    let mut end = (from + len).min(s.len());
    while end > from && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(from..end).unwrap_or("")
}

/// Find an image URL in dropped HTML or plain text (`<img src=…>`, `og:image`,
/// or any http(s) URL that looks like an image).
pub fn find_image_url(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    // <img src="…"> (also srcset/og:image content="…")
    for key in ["<img", "og:image"] {
        let mut from = 0usize;
        while let Some(pos) = lower[from..].find(key) {
            let start = from + pos;
            let lw = slice_bounded(&lower, start, 600);
            for attr in ["src=\"", "src='", "content=\"", "content='"] {
                if let Some(a) = lw.find(attr) {
                    let q = &attr[attr.len() - 1..];
                    let s = start + a + attr.len();
                    if let Some(end) = text[s..].find(q) {
                        let url = text[s..s + end].trim();
                        if url.starts_with("http") {
                            return Some(url.to_string());
                        }
                    }
                }
            }
            from = start + key.len();
        }
    }
    // any bare URL that ends in an image extension
    for word in text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>') {
        let w = word.trim_end_matches([')', ',', ';']);
        if w.starts_with("http") {
            let path = w.split(['?', '#']).next().unwrap_or(w).to_ascii_lowercase();
            if IMAGE_EXTS_EXT.iter().any(|e| path.ends_with(e)) {
                return Some(w.to_string());
            }
        }
    }
    None
}

/// Markdown for text dropped from a browser: an image URL in it is downloaded and
/// attached; otherwise the text itself is inserted.
pub fn snippet_for_dropped_text(page_path: &Path, src: &Path) -> io::Result<String> {
    let text = std::fs::read_to_string(src).unwrap_or_default();
    let looks_like_html = text.contains('<') && text.contains('>');
    if let Some(url) = find_image_url(&text) {
        if let Ok(snippet) = snippet_for_url(page_path, &url) {
            return Ok(snippet);
        }
        // The image could not be fetched (some sites' URLs need cookies/referer).
        // For HTML payloads that is a failure, not something to paste into a note.
        if looks_like_html {
            return Err(io::Error::other("image URL could not be downloaded"));
        }
    }
    // Not an image: insert the text (tags stripped, blank lines dropped).
    let mut out = String::new();
    let mut in_tag = false;
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (at, ch) in chars.iter().enumerate() {
        match ch {
            '<' => {
                // Only a plausible tag opens one: `compare a < b` is text, and dropping the rest of
                // the line from the `<` on lost the sentence the user dragged in.
                let next = chars.get(at + 1).copied().unwrap_or(' ');
                in_tag = next.is_ascii_alphabetic() || next == '/' || next == '!';
                if !in_tag {
                    current.push('<');
                }
            }
            '>' if in_tag => {
                in_tag = false;
                current.push(' ');
            }
            c if !in_tag => current.push(*c),
            _ => {}
        }
    }
    for line in current.lines() {
        let l = line.trim();
        if !l.is_empty() {
            out.push_str(l);
            out.push('\n');
        }
    }
    if out.trim().is_empty() {
        return Err(io::Error::other("dropped text was empty"));
    }
    Ok(out.trim_end().to_string())
}

/// Copy a file into the page's `assets/` (images) or link it in place, and
/// return the markdown to insert.
pub fn snippet_for_file(page_path: &Path, src: &Path) -> io::Result<String> {
    let mut name = src
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());

    // Screen grabs written by a region grabber (spectacle -r) land in the temp
    // directory as notecraft-snip-<ms>.png: keep the reference meaningful.
    if name.starts_with("notecraft-snip-") {
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "png".into());
        name = format!("snip-{}.{ext}", chrono::Local::now().format("%Y%m%d-%H%M%S%3f"));
    }

    // Payloads from the patched drop target, named notecraft-drop-<ms>.<ext>:
    // .url is an image URL the browser offered; .txt/.html is dropped text or HTML
    // (which may contain the image URL).
    if name.starts_with("notecraft-drop-") {
        let ext = src
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if ext == "url" {
            let url = std::fs::read_to_string(src).unwrap_or_default().trim().to_string();
            if url.starts_with("http") {
                if let Ok(snippet) = snippet_for_url(page_path, &url) {
                    return Ok(snippet);
                }
                return Ok(format!("[{name}]({url})"));
            }
        }
        if ext == "txt" || ext == "html" {
            return snippet_for_dropped_text(page_path, src);
        }
        if is_image(src) {
            name = format!("dropped-{}.{ext}", chrono::Local::now().format("%Y%m%d-%H%M%S%3f"));
        }
    }

    if is_image(src) {
        let dir = page_path.parent().unwrap_or(Path::new(".")).join(ASSETS);
        std::fs::create_dir_all(&dir)?;
        let name = free_name(&dir, &name);
        let target = dir.join(&name);
        std::fs::copy(src, &target)?;
        Ok(format!("![{name}]({ASSETS}/{name})"))
    } else {
        // A file linked in place: the label is escaped for the parser, and the destination is
        // percent-encoded so a path with a `)` or a space does not end the link early.
        let label = safe_asset_name(&name);
        Ok(format!("[{label}]({})", link_destination(&src.display().to_string())))
    }
}

/// Percent-encode the characters that would otherwise end (or split) a markdown destination.
fn link_destination(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            '(' | ')' | ' ' | '[' | ']' | '%' | '<' | '>' => {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// Save clipboard image data into the page's `assets/` and return the markdown.
pub fn snippet_for_image(page_path: &Path, img: &RgbaImage) -> io::Result<String> {
    let reference = Ops::save_asset(page_path, img)?;
    let name = reference.rsplit('/').next().unwrap_or("image.png");
    Ok(format!("![{name}]({reference})"))
}

/// Save the crop of a screen grab into the page's `assets/` and return the markdown.
pub fn snippet_for_snip(page_path: &Path, img: &RgbaImage) -> io::Result<String> {
    let reference = Ops::save_asset_named(page_path, img, "snip")?;
    let name = reference.rsplit('/').next().unwrap_or("snip.png");
    Ok(format!("![{name}]({reference})"))
}

/// Ask the user for files, with the platform's own chooser (`platform::pick_files`).
///
/// The chooser is native: the XDG desktop portal on Linux, `IFileOpenDialog` on Windows,
/// `NSOpenPanel` on macOS. An empty list means cancelled.
pub fn choose_files(images_only: bool) -> Vec<PathBuf> {
    crate::platform::pick_files(images_only)
}

/// Ask where to save a file: the export destination. `None` means the user cancelled.
pub fn choose_save(default_path: &Path, filter: &str) -> Option<PathBuf> {
    crate::platform::pick_save(default_path, filter)
}

/// Plain text on the clipboard. The editor's Paste row needs it: egui hands a paste to the
/// *focused* widget as an `Event::Paste`, and a menu action has no focused widget to deliver it to.
pub fn clipboard_text() -> Option<String> {
    let mut cb = arboard::Clipboard::new().ok()?;
    cb.get_text().ok()
}

/// Image on the clipboard, if any (Wayland: `image/png`; X11: image targets).
pub fn clipboard_image() -> Option<RgbaImage> {
    let mut cb = arboard::Clipboard::new().ok()?;
    let img = cb.get_image().ok()?;
    RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())
}

/// Files on the clipboard: a real file list where the platform offers one, else
/// `text/uri-list` or a pasted absolute path.
pub fn clipboard_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if let Ok(list) = cb.get().file_list() {
            files.extend(list);
        }
    }
    if files.is_empty() {
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if let Ok(text) = cb.get_text() {
                files.extend(paths_from_text(&text));
            }
        }
    }
    files.retain(|p| p.exists());
    files.dedup();
    files
}

/// Pull file paths out of clipboard text: `file://` URIs (one per line, as
/// Dolphin writes them) or plain absolute paths.
pub fn paths_from_text(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim().trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("file://") {
            let decoded = percent_decode(rest);
            out.push(PathBuf::from(decoded));
        } else if line.starts_with('/') {
            out.push(PathBuf::from(line));
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("notecraft-attach-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn image_urls_are_found_in_dropped_html_and_text() {
        let html = r#"<html><body><img src="https://example.com/pic.png" width="800"></body></html>"#;
        assert_eq!(
            find_image_url(html).as_deref(),
            Some("https://example.com/pic.png")
        );
        let og = r#"<meta property="og:image" content="https://cdn.example.org/photo.jpg?w=900">"#;
        assert_eq!(
            find_image_url(og).as_deref(),
            Some("https://cdn.example.org/photo.jpg?w=900")
        );
        let plain = "have a look https://example.net/a/b/cat.jpeg and tell me";
        assert_eq!(
            find_image_url(plain).as_deref(),
            Some("https://example.net/a/b/cat.jpeg")
        );
        // a page link is not an image
        assert!(find_image_url("https://example.com/article").is_none());
        assert!(find_image_url("just some words").is_none());
        // multi-byte text must not panic the bounded slice
        assert!(find_image_url("— — — no image here — — —").is_none());
        assert!(find_image_url("— — see <img src=\"http://x/y.png\"> — —").is_some());
    }

    #[test]
    fn image_files_are_copied_into_assets_and_linked_relative() {
        let dir = tmpdir("img");
        let page = dir.join("Page.md");
        std::fs::write(&page, "body").unwrap();
        let src = dir.join("photo.png");
        let img = RgbaImage::from_pixel(4, 4, image::Rgba([1, 2, 3, 255]));
        img.save(&src).unwrap();

        let snippet = snippet_for_file(&page, &src).unwrap();
        assert_eq!(snippet, "![photo.png](assets/photo.png)");
        assert!(dir.join("assets/photo.png").exists());
        // stored at native resolution: the file is copied, never re-encoded or resized
        assert_eq!(
            std::fs::read(&src).unwrap(),
            std::fs::read(dir.join("assets/photo.png")).unwrap(),
            "the asset must be byte-for-byte the original file"
        );
        // a second file of the same name must not land on the first one: every earlier reference
        // to that picture would start showing the new image
        let other = dir.join("sub").join("photo.png");
        std::fs::create_dir_all(other.parent().unwrap()).unwrap();
        RgbaImage::from_pixel(4, 4, image::Rgba([9, 8, 7, 255])).save(&other).unwrap();
        let second = snippet_for_file(&page, &other).unwrap();
        assert_eq!(second, "![photo 2.png](assets/photo 2.png)");
        assert_ne!(
            std::fs::read(dir.join("assets/photo.png")).unwrap(),
            std::fs::read(dir.join("assets/photo 2.png")).unwrap(),
            "the first picture is untouched"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_image_files_are_linked_in_place() {
        let dir = tmpdir("doc");
        let page = dir.join("Page.md");
        std::fs::write(&page, "body").unwrap();
        let src = dir.join("notes.txt");
        std::fs::write(&src, "hello").unwrap();
        let snippet = snippet_for_file(&page, &src).unwrap();
        assert!(snippet.starts_with("[notes.txt]("));
        assert!(snippet.contains("notes.txt"));
        assert!(!dir.join("assets").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clipboard_images_become_paste_assets() {
        let dir = tmpdir("paste");
        let page = dir.join("Page.md");
        std::fs::write(&page, "body").unwrap();
        let img = RgbaImage::from_pixel(3, 2, image::Rgba([9, 9, 9, 255]));
        let snippet = snippet_for_image(&page, &img).unwrap();
        assert!(snippet.starts_with("![paste-"));
        assert!(snippet.contains("](assets/paste-"));
        // the stamp carries milliseconds, so two pastes in the same second cannot collide
        let stamp = snippet
            .trim_start_matches("![paste-")
            .split('.')
            .next()
            .unwrap();
        assert_eq!(stamp.len(), 8 + 1 + 6 + 3, "{stamp}");
        assert!(dir.join("assets").is_dir());
        assert_eq!(std::fs::read_dir(dir.join("assets")).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_screen_grab_crop_is_saved_as_a_snip_asset() {
        let dir = tmpdir("snip");
        let page = dir.join("Page.md");
        std::fs::write(&page, "body").unwrap();
        let img = RgbaImage::from_pixel(5, 4, image::Rgba([3, 4, 5, 255]));
        let snippet = snippet_for_snip(&page, &img).unwrap();
        assert!(snippet.starts_with("![snip-"), "{snippet}");
        assert!(snippet.contains("](assets/snip-"), "{snippet}");
        let name = snippet
            .rsplit("(assets/")
            .next()
            .unwrap()
            .trim_end_matches(')');
        assert!(dir.join("assets").join(name).is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uri_lists_and_plain_paths_parse() {
        let text = "file:///home/mjbayes/Pictures/holiday%20photo.png\r\n/abs/other.jpg\n\nnot-a-path\n";
        let paths = paths_from_text(text);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], PathBuf::from("/home/mjbayes/Pictures/holiday photo.png"));
        assert_eq!(paths[1], PathBuf::from("/abs/other.jpg"));
        assert!(!is_image(Path::new("/tmp/x.txt")));
        assert!(is_image(Path::new("/tmp/X.PNG")));
    }

    #[test]
    fn an_attached_picture_parses_back_into_a_picture() {
        // The markdown this module writes is read back by the document parser, which ends a
        // destination at the first `)`. A browser-download name like `Screenshot (3).png` used to
        // produce `![Screenshot (3).png](assets/Screenshot (3).png)`, which parsed as a paragraph
        // with a truncated link and no image at all.
        let dir = std::env::temp_dir().join(format!("notecraft-attach-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        let page = dir.join("Note.md");
        for name in ["Screenshot (3).png", "photo one.png", "a]b[c.png", "plain.png"] {
            let src = std::env::temp_dir().join(name);
            std::fs::write(&src, b"\x89PNG").unwrap();
            let snippet = snippet_for_file(&page, &src).unwrap();
            let d = crate::doc::from_markdown(&format!("{snippet}\n"));
            assert!(
                d.blocks.iter().any(|b| b.kind == crate::doc::Kind::Image),
                "{name:?} produced no picture block: {snippet:?}"
            );
            let _ = std::fs::remove_file(&src);
        }
        // a file that is not a picture is linked, with a destination the parser can read back
        let src = std::env::temp_dir().join("report (final).pdf");
        std::fs::write(&src, b"%PDF").unwrap();
        let snippet = snippet_for_file(&page, &src).unwrap();
        assert!(!snippet.contains(' '), "destination still has a space: {snippet:?}");
        let d = crate::doc::from_markdown(&format!("{snippet}\n"));
        assert_eq!(
            d.blocks[0].runs[0].a.link.as_deref(),
            Some(link_destination(&src.display().to_string()).as_str())
        );
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bare_less_than_does_not_swallow_the_rest_of_dropped_text() {
        let dir = std::env::temp_dir().join(format!("notecraft-drop-text-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("Note.md");
        let src = dir.join("payload.txt");
        std::fs::write(&src, "compare a < b and c > d\n").unwrap();
        let text = snippet_for_dropped_text(&page, &src).unwrap();
        assert!(text.contains("compare a < b"), "lost the text after a bare <: {text:?}");
        // a real tag is still stripped
        std::fs::write(&src, "<p>hello <b>world</b></p>\n").unwrap();
        let text = snippet_for_dropped_text(&page, &src).unwrap();
        assert!(!text.contains("<"), "tags survived: {text:?}");
        assert!(text.contains("hello") && text.contains("world"), "{text:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
