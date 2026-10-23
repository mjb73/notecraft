//! Exporting the page you are looking at.
//!
//! HTML is the format that reproduces what the document actually models, so it is the primary one: one
//! self-contained file, the pictures inlined as data URIs, and CSS that follows the current theme (with a
//! print block that forces a light page). It is also what the PDF is made from — WeasyPrint,
//! wkhtmltopdf or a Chromium-family browser in headless mode; when none is installed the export
//! says so instead of writing nothing.
//! Plain text exists for grep, mail bodies and pasting.

use crate::doc::{self, Kind};
use crate::theme::Theme;
use crate::vault;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- html

/// The whole page as one HTML document.
pub fn html(d: &doc::Doc, title: &str, page: &Path, t: &Theme) -> String {
    let mut body = String::new();
    let mut i = 0usize;
    while i < d.blocks.len() {
        let kind = d.blocks[i].kind;
        match kind {
            // lists: consecutive bullets / todos become one list, which is what they are
            Kind::Bullet | Kind::Todo => {
                let todo = kind == Kind::Todo;
                body.push_str(if todo { "<ul class=\"todo\">\n" } else { "<ul>\n" });
                while i < d.blocks.len() && d.blocks[i].kind == kind {
                    let b = &d.blocks[i];
                    if todo {
                        body.push_str(&format!(
                            "<li class=\"{}\"><span class=\"box\">{}</span>{}</li>\n",
                            if b.done { "done" } else { "open" },
                            if b.done { "&#10003;" } else { "" },
                            runs_html(&b.runs)
                        ));
                    } else {
                        body.push_str(&format!("<li>{}</li>\n", runs_html(&b.runs)));
                    }
                    i += 1;
                }
                body.push_str("</ul>\n");
            }
            // consecutive code lines are one fence
            Kind::Code => {
                let mut lines: Vec<String> = Vec::new();
                while i < d.blocks.len() && d.blocks[i].kind == Kind::Code {
                    lines.push(escape(&d.blocks[i].text()));
                    i += 1;
                }
                body.push_str(&format!(
                    "<pre><code>{}</code></pre>\n",
                    lines.join("\n")
                ));
            }
            Kind::Image => {
                let b = &d.blocks[i];
                // Only the page's own `assets/` folder is inlined: a reference that climbs out of it
                // (`../..`, or an absolute path) used to be read and base64-inlined into the
                // export, which made any readable file on the box part of a shareable document.
                let abs = vault::contained_asset_path(page, &b.image);
                let caption = b.text();
                match abs
                    .as_ref()
                    .map(std::fs::read)
                    .and_then(|r| r.ok())
                {
                    Some(bytes) => {
                        let abs = abs.unwrap_or_default();
                        let mime = match abs
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.to_ascii_lowercase())
                            .as_deref()
                        {
                            Some("jpg") | Some("jpeg") => "image/jpeg",
                            Some("gif") => "image/gif",
                            Some("webp") => "image/webp",
                            Some("svg") => "image/svg+xml",
                            Some("bmp") => "image/bmp",
                            _ => "image/png",
                        };
                        // A cropped picture is exported *cropped*: the bytes are decoded and cut
                        // here, in memory, and the asset on disk is left whole. Baking beats CSS
                        // offsets here — the export has to look the same in a browser and in the PDF
                        // that is rendered from the same HTML, and only one of the two can be trusted
                        // to honour negative margins. If the decode fails the whole picture is
                        // exported rather than nothing.
                        let bytes = match b.crop.filter(|c| !c.is_full()) {
                            Some(crop) => crop_bytes(&bytes, crop).unwrap_or(bytes),
                            None => bytes,
                        };
                        let width = match b.width {
                            Some(w) => format!(" style=\"width:{w}px\""),
                            None => String::new(),
                        };
                        let cap = if caption.trim().is_empty() {
                            String::new()
                        } else {
                            format!("<figcaption>{}</figcaption>", escape(&caption))
                        };
                        body.push_str(&format!(
                            "<figure><img src=\"data:{mime};base64,{}\" alt=\"{}\"{width}>{cap}</figure>\n",
                            b64(&bytes),
                            escape_attr(&caption)
                        ));
                    }
                    None => body.push_str(&format!(
                        "<p class=\"missing\">missing picture: {}</p>\n",
                        escape(&b.image)
                    )),
                }
                i += 1;
            }
            Kind::Rule => {
                body.push_str("<hr>\n");
                i += 1;
            }
            Kind::Heading(n) => {
                let lvl = n.clamp(1, 6);
                body.push_str(&format!(
                    "<h{lvl}>{}</h{lvl}>\n",
                    runs_html(&d.blocks[i].runs)
                ));
                i += 1;
            }
            Kind::Quote => {
                body.push_str(&format!(
                    "<blockquote><p>{}</p></blockquote>\n",
                    runs_html(&d.blocks[i].runs)
                ));
                i += 1;
            }
            Kind::Para => {
                if !d.blocks[i].text().trim().is_empty() {
                    body.push_str(&format!("<p>{}</p>\n", runs_html(&d.blocks[i].runs)));
                }
                i += 1;
            }
        }
    }
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n<style>\n{css}</style>\n</head>\n<body>\n<main>\n\
         <h1 class=\"page-title\">{title}</h1>\n{body}</main>\n</body>\n</html>\n",
        title = escape(title),
        css = css(t),
        body = body
    )
}

/// One run: its text with the properties it carries, as nested elements.
fn runs_html(runs: &[doc::Run]) -> String {
    let mut out = String::new();
    for r in runs {
        if r.text.is_empty() {
            continue;
        }
        let mut s = escape(&r.text);
        if r.a.mark > 0 {
            if let Some(c) = crate::theme::MARKS.get(r.a.mark as usize - 1) {
                // the marker goes inside the other properties: a highlighted bold word stays bold
                s = format!("<span class=\"mark\" style=\"background:{}\">{s}</span>", hex(*c));
            }
        }
        if r.a.code {
            s = format!("<code>{s}</code>");
        }
        if r.a.strike {
            s = format!("<s>{s}</s>");
        }
        if r.a.italic {
            s = format!("<em>{s}</em>");
        }
        if r.a.bold {
            s = format!("<strong>{s}</strong>");
        }
        if r.a.tag {
            s = format!("<span class=\"tag\">{s}</span>");
        }
        if r.a.due.is_some() {
            s = format!("<span class=\"due\">{s}</span>");
        }
        if r.a.size < 0 {
            s = format!("<span class=\"small\">{s}</span>");
        } else if r.a.size > 0 {
            s = format!("<span class=\"large\">{s}</span>");
        }
        if let Some(link) = &r.a.link {
            if link.starts_with("[[") {
                s = format!("<span class=\"wikilink\">{s}</span>");
            } else if let Some(href) = safe_href(link) {
                s = format!("<a href=\"{}\">{s}</a>", escape_attr(&href));
            } else {
                // A target whose scheme we do not render as a link. The parser stores whatever sat
                // between the brackets, so a note could carry `javascript:` — exporting it as an
                // href produced HTML that ran it when the reader clicked.
                s = format!("<span class=\"link\">{s}</span>");
            }
        }
        out.push_str(&s);
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    escape(s).replace('"', "&quot;")
}

/// The href to emit for a link target, or `None` when it is not a scheme we hand to a browser.
///
/// Escaping an attribute does not make a *scheme* safe: `javascript:…` and `data:…` are valid
/// attribute values, and they run in the page that opens the export.
fn safe_href(link: &str) -> Option<String> {
    let trimmed = link.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    match lower.find(':') {
        // no scheme at all: a relative target ("notes/other.html", "#anchor") is fine
        None => Some(trimmed.to_string()),
        Some(i) => {
            let head = &lower[..i];
            let looks_like_scheme = !head.is_empty()
                && head
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.');
            // a colon after something that is not a scheme (e.g. a path segment) is just a path
            if !looks_like_scheme {
                return Some(trimmed.to_string());
            }
            match head {
                "http" | "https" | "mailto" | "file" => Some(trimmed.to_string()),
                _ => None,
            }
        }
    }
}

fn hex(c: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn rgba(c: egui::Color32, a: f32) -> String {
    format!("rgba({},{},{},{a})", c.r(), c.g(), c.b())
}

fn css(t: &Theme) -> String {
    format!(
        r#"
:root {{ color-scheme: {}; }}
* {{ box-sizing: border-box; }}
body {{
  margin: 0; background: {bg}; color: {text};
  font: 15px/1.5 "Chakra Petch", system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  -webkit-font-smoothing: antialiased;
}}
main {{ max-width: 880px; margin: 0 auto; padding: 40px 24px 64px; }}
.page-title {{ font-size: 28px; line-height: 1.2; margin: 0 0 6px; color: {heading}; }}
h1, h2, h3, h4, h5, h6 {{ color: {heading}; line-height: 1.25; margin: 1.1em 0 0.35em; }}
h1 {{ font-size: 23px; }} h2 {{ font-size: 20px; }} h3 {{ font-size: 18px; }}
h4 {{ font-size: 16.5px; }} h5, h6 {{ font-size: 15px; }}
p {{ margin: 0.35em 0; }}
ul {{ margin: 0.35em 0; padding-left: 1.3em; }}
li {{ margin: 1px 0; }}
ul.todo {{ list-style: none; padding-left: 0; }}
ul.todo li {{ margin: 1px 0; }}
/* inline, not flex: weasyprint mis-positions an empty flex item (the box of an open todo was drawn
   below the list), and this is the layout every renderer agrees on */
.box {{
  display: inline-block; vertical-align: -2px; margin-right: 7px;
  width: 13px; height: 13px; border-radius: 3px;
  border: 1.4px solid {faint}; text-align: center; font-size: 10px; line-height: 12px;
}}
li.done .box {{ background: {done}; border-color: {done}; color: {bg}; }}
li.done {{ color: {dim}; text-decoration: line-through; }}
blockquote {{ margin: 0.7em 0; padding-left: 12px; border-left: 2px solid {border}; color: {dim}; }}
blockquote p {{ margin: 0; }}
pre {{
  background: {code_bg}; border-radius: 8px; padding: 10px 12px;
  font: 13px/1.45 "JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, monospace;
}}
code {{
  font-family: "JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px;
  background: {code_bg}; border-radius: 4px; padding: 0 3px; color: {accent};
}}
pre code {{ background: none; padding: 0; color: {text}; }}
figure {{ margin: 1em 0; }}
img {{ max-width: 100%; height: auto; border-radius: 6px; }}
figcaption {{ font: 11px "JetBrains Mono", ui-monospace, monospace; color: {faint}; margin-top: 4px; }}
.tag {{ background: {tag_bg}; color: {accent}; padding: 0 4px; border-radius: 4px; }}
.due {{ background: {due_bg}; color: {warn}; padding: 0 4px; border-radius: 4px; }}
.wikilink {{ color: {accent2}; text-decoration: underline dotted; }}
.mark {{ color: #181612; padding: 0 1px; border-radius: 2px; }}
@media print {{ .mark {{ -webkit-print-color-adjust: exact; print-color-adjust: exact; }} }}
.small {{ font-size: 0.82em; }}
.large {{ font-size: 1.32em; }}
a {{ color: {accent2}; }}
hr {{ border: 0; border-top: 1px solid {border}; margin: 1.4em 0; }}
.missing {{ color: {danger}; font: 12px ui-monospace, monospace; }}
/* browsers scroll a long code line; the PDF lays it out as it is (weasyprint has no overflow-x) */
@media screen {{ pre {{ overflow-x: auto; }} }}
@media print {{
  @page {{ size: A4; margin: 18mm 16mm; }}
  body {{ background: #fff; color: #14161a; }}
  main {{ max-width: none; padding: 0; }}
  .page-title, h1, h2, h3, h4, h5, h6 {{ color: #0b0d10; }}
  pre, code {{ background: #f2f2f4 !important; color: #14161a !important; }}
  .tag, .due {{ background: #eeeef1 !important; color: #333 !important; }}
  blockquote {{ color: #444; border-left-color: #ccc; }}
  li.done {{ color: #666; }}
  a {{ color: #14161a; text-decoration: underline; }}
  figure, pre, blockquote, li {{ break-inside: avoid; }}
  h1, h2, h3, h4 {{ break-after: avoid; }}
}}
"#,
        if t.dark { "dark" } else { "light" },
        bg = hex(t.bg),
        text = hex(t.text),
        dim = hex(t.dim),
        faint = hex(t.faint),
        border = hex(t.border),
        heading = hex(t.heading),
        accent = hex(t.accent),
        accent2 = hex(t.accent2),
        code_bg = hex(t.code_bg),
        done = hex(t.done),
        warn = hex(t.warn),
        danger = hex(t.danger),
        tag_bg = rgba(t.accent, 0.14),
        due_bg = rgba(t.warn, 0.14),
    )
}

/// RFC 4648 base64 — small enough to keep in-tree rather than add a dependency for it.
fn b64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (chunk[0] as u32) << 16 | b1 << 8 | b2;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------- plain text

/// The page as plain text: the words, with the lightest structure that keeps it readable.
pub fn plain(d: &doc::Doc, title: &str) -> String {
    let mut out = String::new();
    out.push_str(title.trim());
    out.push_str("\n\n");
    for b in &d.blocks {
        let text = b.text();
        match b.kind {
            Kind::Para => {
                if !text.trim().is_empty() {
                    out.push_str(&text);
                    out.push('\n');
                }
            }
            Kind::Heading(_) => {
                out.push_str(text.trim());
                out.push('\n');
            }
            Kind::Bullet => {
                out.push_str("• ");
                out.push_str(&text);
                out.push('\n');
            }
            Kind::Todo => {
                out.push_str(if b.done { "[x] " } else { "[ ] " });
                out.push_str(&text);
                out.push('\n');
            }
            Kind::Quote => {
                out.push_str("  ");
                out.push_str(&text);
                out.push('\n');
            }
            Kind::Code => {
                out.push_str("    ");
                out.push_str(&text);
                out.push('\n');
            }
            Kind::Rule => out.push_str("---\n"),
            Kind::Image => {
                out.push_str("[picture: ");
                if text.trim().is_empty() {
                    out.push_str(&b.image);
                } else {
                    out.push_str(text.trim());
                }
                out.push_str("]\n");
            }
        }
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------- pdf

/// Make a PDF from the HTML. The HTML is written beside the output so nothing has to resolve a
/// path, and removed again whatever happens.
///
/// Nothing here can prove the page came out right — the renderer is a separate program — so a
/// missing tool, a failed run and an empty file are all errors, and a half-written output is deleted
/// rather than left looking like a PDF that "did not save properly".
pub fn html_to_pdf(html: &str, out: &Path, work_dir: &Path) -> Result<(), String> {
    let tmp = work_dir.join(format!(
        ".notecraft-export-{}.html",
        chrono::Local::now().timestamp_millis()
    ));
    std::fs::write(&tmp, html).map_err(|e| format!("cannot write the temporary HTML: {e}"))?;

    let rendered = render_html(&tmp, out);
    let _ = std::fs::remove_file(&tmp);
    match rendered {
        Ok(()) => Ok(()),
        Err(e) => {
            // a renderer that died halfway can leave a zero-byte or truncated file behind
            let _ = std::fs::remove_file(out);
            Err(e)
        }
    }
}

/// One way of turning the export's HTML into a PDF: the program, the arguments it needs for
/// `in` and `out`, and the sentence shown when it is not installed.
struct PdfRenderer {
    label: &'static str,
    /// Every place the program may live. The first entry is the `PATH` name.
    bins: Vec<PathBuf>,
    args: fn(&Path, &Path) -> Vec<std::ffi::OsString>,
}

/// Run the first available renderer over the temporary HTML.
///
/// WeasyPrint first (its output honours the export's page CSS most faithfully), then wkhtmltopdf,
/// then a Chromium-family browser in headless mode — which is the only one that exists on Windows
/// out of the box (`msedge`), and is a common install on macOS and Linux. Only spawning is retried:
/// once a renderer actually runs, its verdict is the answer.
fn render_html(tmp: &Path, out: &Path) -> Result<(), String> {
    let mut tried: Vec<String> = Vec::new();
    for r in pdf_renderers() {
        for bin in &r.bins {
            let args = (r.args)(tmp, out);
            let res = std::process::Command::new(bin)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output();
            let output = match res {
                Ok(o) => o,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    tried.push(format!("{}: not installed", bin.display()));
                    continue;
                }
                Err(e) => return Err(format!("cannot run {}: {e}", r.label)),
            };
            if output.status.success() && out.is_file() && file_has_bytes(out) {
                return Ok(());
            }
            // A renderer that ran and failed is a real failure: report it rather than silently
            // trying the next one, which would hide a broken page behind a different engine's page.
            let err = String::from_utf8_lossy(&output.stderr);
            let msg = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no message");
            return Err(format!("{} failed: {msg}", r.label));
        }
    }
    Err(format!(
        "no HTML-to-PDF renderer found ({}) — install WeasyPrint (python-weasyprint) or \
         wkhtmltopdf, or use the HTML export, which is the same page",
        if tried.is_empty() { "none on PATH".to_string() } else { tried.join("; ") }
    ))
}

fn file_has_bytes(p: &Path) -> bool {
    std::fs::metadata(p).map(|m| m.len() > 512).unwrap_or(false)
}

/// The renderers to try, in order, with the paths each may live at on this platform.
fn pdf_renderers() -> Vec<PdfRenderer> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut weasy = Vec::new();
    if let Some(p) = env("NOTECRAFT_WEASYPRINT") {
        weasy.push(p);
    }
    weasy.extend(path_names(&[
        "weasyprint",
        "/usr/bin/weasyprint",
        "/usr/sbin/weasyprint",
        "/usr/local/bin/weasyprint",
        "/opt/homebrew/bin/weasyprint",
    ]));

    let mut wk = Vec::new();
    if let Some(p) = env("NOTECRAFT_WKHTMLTOPDF") {
        wk.push(p);
    }
    wk.extend(path_names(&[
        "wkhtmltopdf",
        "/usr/bin/wkhtmltopdf",
        "/usr/local/bin/wkhtmltopdf",
        "/opt/homebrew/bin/wkhtmltopdf",
        "C:\\Program Files\\wkhtmltopdf\\bin\\wkhtmltopdf.exe",
    ]));

    let mut chrome = Vec::new();
    if let Some(p) = env("NOTECRAFT_CHROME") {
        chrome.push(p);
    }
    chrome.extend(path_names(&[
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "msedge",
        "brave-browser",
        "vivaldi",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
        "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
    ]));

    vec![
        PdfRenderer {
            label: "weasyprint",
            bins: weasy,
            args: |input, output| {
                vec![input.as_os_str().to_owned(), output.as_os_str().to_owned()]
            },
        },
        PdfRenderer {
            label: "wkhtmltopdf",
            bins: wk,
            args: |input, output| {
                vec![
                    "--enable-local-file-access".into(),
                    "--quiet".into(),
                    input.as_os_str().to_owned(),
                    output.as_os_str().to_owned(),
                ]
            },
        },
        PdfRenderer {
            label: "chrome (headless)",
            bins: chrome,
            args: |input, output| {
                vec![
                    "--headless=new".into(),
                    "--disable-gpu".into(),
                    "--no-pdf-header-footer".into(),
                    // give the page a moment to lay out its fonts and inline images
                    "--virtual-time-budget=3000".into(),
                    format!("--print-to-pdf={}", output.display()).into(),
                    file_url(input).into(),
                ]
            },
        },
    ]
}

/// `PATH` names as-is, then the absolute paths that actually exist (a desktop session's `PATH` is
/// not always a shell's, and a tool that works in a terminal but not from the launcher is a failure
/// mode this app has hit before).
fn path_names(names: &[&str]) -> Vec<PathBuf> {
    let mut v = Vec::new();
    for n in names {
        let p = PathBuf::from(n);
        if p.is_absolute() {
            if p.is_file() {
                v.push(p);
            }
        } else {
            v.push(p);
        }
    }
    v
}

/// A `file://` URL the renderers accept: Chrome needs the absolute path with the platform's own
/// separators converted and the spaces escaped.
fn file_url(p: &Path) -> String {
    let s = p.display().to_string();
    let mut out = String::from("file://");
    #[cfg(windows)]
    {
        out.push('/');
        out.push_str(&s.replace('\\', "/"));
    }
    #[cfg(not(windows))]
    {
        out.push_str(&s);
    }
    out.replace(' ', "%20")
}

/// Cut an encoded picture down to `crop` and re-encode it as PNG, in memory.
///
/// `None` when the bytes are not an image this build can decode (or the crop is degenerate): the caller
/// then exports the whole picture, because a picture that is too big is a much smaller failure than a
/// figure with nothing in it.
fn crop_bytes(bytes: &[u8], crop: crate::doc::Crop) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (x, y, cw, ch) = crop.px((w, h));
    let (x, y) = (x.round().max(0.0) as u32, y.round().max(0.0) as u32);
    // at least one pixel of each side, and never past the picture's own edge
    let cw = (cw.round() as u32).clamp(1, img.width().saturating_sub(x).max(1));
    let ch = (ch.round() as u32).clamp(1, img.height().saturating_sub(y).max(1));
    let cut = img.crop_imm(x.min(img.width().saturating_sub(1)), y.min(img.height().saturating_sub(1)), cw, ch);
    let mut out = std::io::Cursor::new(Vec::new());
    cut.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::from_markdown;
    use crate::theme;

    fn tmpdir(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("notecraft-export-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn the_pdf_export_writes_a_real_pdf() {
        // This is the one export that shells out, so nothing in the Rust type system proves it
        // worked: the page could come back empty, or not at all. Skipped when the machine has no
        // renderer, so `cargo test` stays honest on a box without one.
        let have_tool = pdf_renderers()
            .iter()
            .any(|r| r.bins.iter().any(|b| !b.is_absolute() || b.is_file()));
        if !have_tool {
            eprintln!("no HTML-to-PDF renderer installed — PDF export test skipped");
            return;
        }
        let t = theme::themes()[0];
        let dir = tmpdir("pdf");
        let page = dir.join("P.md");
        std::fs::write(&page, "body").unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        // a real 2×2 PNG: weasyprint has to decode it, so a broken inline shows up as a failure
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        img.save_with_format(dir.join("assets/shot.png"), image::ImageFormat::Png)
            .unwrap();
        let d = from_markdown(
            "---\ntitle: Export\n---\n\n# Heading\n\nSome **bold** words and a `code` bit.\n\n![a shot](assets/shot.png)\n",
        );
        let html = html(&d, "Export", &page, &t);
        let out = dir.join("Export.pdf");
        html_to_pdf(&html, &out, &dir).expect("the PDF export failed");
        let bytes = std::fs::read(&out).unwrap();
        assert!(bytes.starts_with(b"%PDF"), "not a PDF: {:?}", &bytes[..8.min(bytes.len())]);
        assert!(bytes.len() > 1500, "a {}-byte PDF is not a page", bytes.len());
        // and the words really are in it (a PDF of an empty page is a valid PDF)
        let text = std::process::Command::new("pdftotext")
            .arg(&out)
            .arg("-")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        if !text.is_empty() {
            assert!(text.contains("Heading"), "the page's words are not in the PDF: {text:?}");
            assert!(text.contains("bold"), "the body text is not in the PDF: {text:?}");
        }
        // the temporary HTML beside the output is cleaned up
        assert!(
            !std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with(".notecraft-export-")),
            "the temporary HTML was left behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_link_target_is_only_emitted_when_its_scheme_is_safe() {
        // The document parser stores whatever sat between the brackets, so the export is the last
        // place that can refuse a scheme a browser would happily execute.
        for bad in [
            "javascript:alert(1)",
            "javascript:alert%28document.cookie%29",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:msgbox(1)",
            "JAVASCRIPT:alert(1)",
        ] {
            assert_eq!(super::safe_href(bad), None, "{bad:?} must not become an href");
        }
        for good in [
            "https://example.com/x",
            "http://example.com",
            "mailto:me@example.com",
            "file:///tmp/x.png",
            "notes/other.html",
            "#anchor",
            "page two",
            "./notes/2026:10/x.html", // a colon in the path, not a scheme
        ] {
            assert_eq!(
                super::safe_href(good).as_deref(),
                Some(good),
                "{good:?} is a link we do render"
            );
        }
        // end to end: the export of a page carrying a script link has no href for it
        let dir = tmpdir("xss");
        let page = dir.join("Note.md");
        let d = from_markdown("[click](javascript:alert(1)) words\n");
        let t = theme::themes()[0];
        let html = super::html(&d, "Note", &page, &t);
        assert!(!html.contains("javascript:"), "exported HTML carries the scheme: {html}");
        assert!(html.contains("click"), "the words are still there");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_outside_assets_is_not_inlined_into_the_export() {
        let dir = tmpdir("contain");
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("assets/ok.png"), b"png").unwrap();
        std::fs::write(dir.join("secret.txt"), b"not a picture").unwrap();
        let page = dir.join("Note.md");
        let t = theme::themes()[0];

        // a reference inside assets/ is inlined
        let inside = from_markdown("![shot](assets/ok.png)\n");
        let html = super::html(&inside, "Note", &page, &t);
        assert!(html.contains("data:image/png;base64,"), "{html}");

        // anything that climbs out, or is absolute, is reported as missing instead
        for reference in [
            "../secret.txt",
            "../../../../etc/hostname",
            "/etc/hostname",
            "assets/../secret.txt",
        ] {
            let d = from_markdown(&format!("![x]({reference})\n"));
            let html = super::html(&d, "Note", &page, &t);
            assert!(
                !html.contains("base64"),
                "{reference:?} was inlined into the export: {html}"
            );
            assert!(html.contains("missing picture"), "{reference:?}: {html}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (raw, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(b64(raw.as_bytes()), want, "{raw:?}");
        }
    }

    #[test]
    /// The export bakes the crop into the copy it embeds: the kept half of a 4x2 picture is a 2x2 PNG,
    /// and bytes that are not an image fall back to the original rather than to an empty figure.
    #[test]
    fn a_cropped_picture_is_exported_cropped() {
        let img = image::RgbaImage::from_fn(4, 2, |x, _| {
            image::Rgba([if x < 2 { 255 } else { 0 }, 0, 0, 255])
        });
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let out = crop_bytes(
            &png,
            crate::doc::Crop {
                x: 0.5,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            },
        )
        .expect("a real png crops");
        let cut = image::load_from_memory(&out).unwrap();
        assert_eq!((cut.width(), cut.height()), (2, 2));
        // x=0.5 keeps the *right* half, which is the zero (not the red) one — proof it cut rather
        // than rescaled
        assert_eq!(cut.to_rgba8().get_pixel(0, 0)[0], 0);
        let left = crop_bytes(
            &png,
            crate::doc::Crop {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            },
        )
        .expect("the left half crops too");
        assert_eq!(
            image::load_from_memory(&left).unwrap().to_rgba8().get_pixel(0, 0)[0],
            255
        );
        assert!(crop_bytes(b"not an image", crate::doc::Crop::FULL).is_none());
    }

    fn text_is_escaped_and_properties_become_elements() {
        let t = theme::themes()[0];
        let page = std::path::PathBuf::from("/tmp/x/P.md");
        let d = from_markdown("**bold** *it* `code` ~~gone~~ and <script>alert(\"x\") & co\n");
        let out = html(&d, "T", &page, &t);
        assert!(out.contains("<strong>bold</strong>"), "{out}");
        assert!(out.contains("<em>it</em>"));
        assert!(out.contains("<code>code</code>"));
        assert!(out.contains("<s>gone</s>"));
        // no markup from the note's own text
        assert!(!out.contains("<script>"), "a note's text must never become markup");
        assert!(out.contains("&lt;script&gt;"));
        assert!(out.contains("&amp; co"));
        // the title is escaped too
        let out = html(&d, "A <b> & c", &page, &t);
        assert!(out.contains("<title>A &lt;b&gt; &amp; c</title>"));
    }

    #[test]
    fn a_run_carrying_several_properties_keeps_them_all() {
        let t = theme::themes()[0];
        let mut runs = vec![doc::Run::plain("plain ")];
        runs.push(doc::Run {
            text: "both".into(),
            a: doc::Attrs {
                bold: true,
                italic: true,
                code: true,
                ..Default::default()
            },
        });
        let mut d = doc::Doc::default();
        let mut b = doc::Block::new(doc::Kind::Para);
        b.runs = runs;
        d.blocks.push(b);
        let out = html(&d, "T", std::path::Path::new("/tmp/x/P.md"), &t);
        // markdown cannot express this combination; HTML can, which is why it is the export format
        assert!(out.contains("<strong><em><code>both</code></em></strong>"), "{out}");
    }

    #[test]
    fn a_highlight_becomes_a_coloured_span() {
        let d = crate::doc::from_markdown("plain **marked** words\n");
        let mut d = d;
        crate::doc::set_attr(&mut d.blocks[0].runs, Some((6, 12)), |a| a.mark = 1);
        let html = html(&d, "T", std::path::Path::new("/tmp/M.md"), &theme::themes()[0]);
        let hex = hex(theme::MARKS[0]);
        assert!(
            html.contains(&format!("<span class=\"mark\" style=\"background:{hex}\">")),
            "the marker colour travels with the file: {html}"
        );
        // inside the bold, so the words stay bold as well
        assert!(html.contains("<strong><span class=\"mark\""), "{html}");
    }

    #[test]
    fn a_size_step_becomes_a_class_in_the_html() {
        let t = theme::themes()[0];
        let mut runs = vec![doc::Run::plain("base ")];
        runs.push(doc::Run {
            text: "big".into(),
            a: doc::Attrs {
                size: 1,
                ..Default::default()
            },
        });
        let mut d = doc::Doc::default();
        let mut b = doc::Block::new(doc::Kind::Para);
        b.runs = runs;
        d.blocks.push(b);
        let out = html(&d, "T", std::path::Path::new("/tmp/x/P.md"), &t);
        assert!(out.contains("<span class=\"large\">big</span>"), "{out}");
        assert!(out.contains(".large { font-size: 1.32em; }"), "the class is styled");
    }

    #[test]
    fn lists_code_and_todos_are_grouped() {
        let t = theme::themes()[0];
        let d = from_markdown(
            "- one\n- two\n- [ ] open\n- [x] done\n\n```\nfirst\nsecond\n```\n\n> quoted\n",
        );
        let out = html(&d, "T", std::path::Path::new("/tmp/x/P.md"), &t);
        assert_eq!(out.matches("<ul>").count(), 1, "{out}");
        assert_eq!(out.matches("<ul class=\"todo\">").count(), 1, "{out}");
        assert_eq!(out.matches("<pre>").count(), 1, "one fence, not two");
        assert!(out.contains("first\nsecond"));
        assert!(out.contains("<blockquote>"));
        assert!(out.contains("class=\"done\""));
    }

    #[test]
    fn a_picture_is_inlined_so_the_file_stands_alone() {
        let t = theme::themes()[0];
        let dir = tmpdir("img");
        let page = dir.join("P.md");
        std::fs::write(&page, "body").unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        let bytes: &[u8] = b"\x89PNG\r\n\x1a\nnot-really-a-png";
        std::fs::write(dir.join("assets/shot.png"), bytes).unwrap();
        let d = from_markdown("![a shot](assets/shot.png)\n");
        let out = html(&d, "T", &page, &t);
        assert!(out.contains("src=\"data:image/png;base64,"), "{out}");
        assert!(out.contains(&b64(bytes)), "the bytes are the file's own");
        assert!(out.contains("a shot"), "the caption travels with it");
        // a reference to a file that is gone is reported, not silently dropped
        let missing = from_markdown("![gone](assets/nope.png)\n");
        let out = html(&missing, "T", &page, &t);
        assert!(out.contains("missing picture: assets/nope.png"), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_text_keeps_the_words_and_the_task_state() {
        let d = from_markdown("# Head\n\nwords here\n\n- [ ] open\n- [x] done\n\n- bullet\n");
        let text = plain(&d, "Title");
        assert!(text.starts_with("Title\n\n"));
        assert!(text.contains("Head\n"));
        assert!(text.contains("words here"));
        assert!(text.contains("[ ] open"));
        assert!(text.contains("[x] done"));
        assert!(text.contains("• bullet"));
        assert!(!text.contains('#'), "no markup in plain text: {text:?}");
        assert!(!text.contains("**"));
    }
}
