//! Text → `LayoutJob`.
//!
//! Two renderers live here and they no longer resemble each other:
//!
//! * `runs_job` lays out a *document* block — the words the block holds, styled from each
//!   run's attributes. Nothing is parsed, so no markup can be present, let alone leak.
//! * `render_line` lays out a line of markdown, consuming the markup as it goes. Its one caller is
//!   the **tasks view** (`app.rs`), which renders task lines read from the files. Read mode and the
//!   exports render the *document* through `runs_job`, not through this parser.
//!
//! The old editor renderer — markdown with the markers laid out invisibly so the caret still
//! lined up with the file — is gone with the markdown editor it served.

use crate::doc;
use crate::theme::{fam, note_fam, Theme, F_BOLD, F_MED, F_MONO, F_REG, F_SEMI};
use egui::text::LayoutJob;
use egui::{Color32, FontId, Stroke, TextFormat};

/// Inline styling flags accumulated by the parser.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct F {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    /// This run is a line of a **code block**, not an inline span: the block's panel is drawn behind
    /// the lines by the editor and read mode, so the run itself must not paint a chip of its own.
    pub block_code: bool,
    pub link: bool,
    pub dim: bool,
    pub tag: bool,
    pub due: bool,
    /// Highlighter colour (0 = none); the marker sits behind the words and keeps them dark.
    pub mark: u8,
    /// Heading text: uses the heading colour (and, for h3+, the semibold face) while still
    /// going through the inline pass, so `**bold**` inside a heading is styled, not shown.
    pub heading: bool,
    pub semi: bool,
}

fn format(size: f32, f: F, t: &Theme) -> TextFormat {
    // a marked run is a felt-tip highlight: opaque colour behind, dark ink on top
    let marked = f.mark > 0 && (f.mark as usize) <= t.marks.len();
    let color = if marked {
        t.mark_ink
    } else if f.heading {
        t.heading
    } else if f.due {
        t.warn
    } else if f.tag {
        t.accent
    } else if f.code {
        t.accent
    } else if f.link {
        t.accent2
    } else if f.dim {
        t.dim
    } else {
        t.text
    };
    let background = if marked {
        t.marks[f.mark as usize - 1]
    } else if f.code && !f.block_code {
        // an *inline* code span is a chip; a whole code block is a panel, which the editor and read
        // mode draw behind the lines. Letting the block's runs carry the chip too painted a code_bg
        // smear behind each line's own words — panels of different widths, one per line.
        t.code_bg
    } else if f.tag {
        t.accent.gamma_multiply(0.13)
    } else if f.due {
        t.warn.gamma_multiply(0.13)
    } else if f.link {
        // A link carries a wash of its own colour as well as the underline: a dotted underline would
        // say "link" better, but egui's underline is a stroke and cannot be dotted, and a bare 1 pt
        // rule under coloured words is what a typo looks like. The tag and due chips already read
        // this way, so a link now belongs to the same family.
        t.accent2.gamma_multiply(0.11)
    } else {
        Color32::TRANSPARENT
    };
    let family = if f.code {
        F_MONO
    } else if f.bold {
        F_BOLD
    } else if f.italic {
        F_MED
    } else if f.semi {
        F_SEMI
    } else {
        F_REG
    };
    TextFormat {
        // the note body follows the face the Display menu picked; a code run keeps JetBrains Mono,
        // because a note's code should line up whatever the prose is set in.
        font_id: FontId::new(
            if f.code { size - 1.5 } else { size },
            if f.code { fam(F_MONO) } else { note_fam(family) },
        ),
        color,
        background,
        italics: f.italic && !f.code,
        underline: if f.link {
            // the wash says "link" at a glance; the rule only has to finish the word
            Stroke::new(1.0_f32, t.accent2.gamma_multiply(0.7))
        } else {
            Stroke::NONE
        },
        strikethrough: if f.strike {
            Stroke::new(1.0_f32, t.dim)
        } else {
            Stroke::NONE
        },
        ..Default::default()
    }
}

fn push(job: &mut LayoutJob, text: &str, f: F, t: &Theme, size: f32) {
    if text.is_empty() {
        return;
    }
    job.append(text, 0.0, format(size, f, t));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    #[test]
    fn a_size_step_scales_only_the_text_it_is_set_on() {
        let t = theme::themes()[0];
        let mut runs = vec![doc::Run::plain("base ")];
        runs.push(doc::Run {
            text: "large".into(),
            a: doc::Attrs {
                size: 1,
                ..Default::default()
            },
        });
        runs.push(doc::Run {
            text: " small".into(),
            a: doc::Attrs {
                size: -1,
                ..Default::default()
            },
        });
        let job = runs_job("base large small", &runs, &t, 15.0, doc::Kind::Para, false);
        let sizes: Vec<f32> = job.sections.iter().map(|s| s.format.font_id.size).collect();
        assert_eq!(sizes[0], 15.0, "the untouched run keeps the block's size");
        assert!(sizes[1] > 15.0, "large is laid out bigger: {sizes:?}");
        assert!(sizes[2] < 15.0, "small is laid out smaller: {sizes:?}");
    }

    #[test]
    fn a_highlighted_run_is_painted_with_a_marker_and_dark_ink() {
        let t = theme::themes()[0];
        let mut runs = vec![doc::Run::plain("plain ")];
        runs.push(doc::Run {
            text: "marked".into(),
            a: doc::Attrs {
                mark: 2,
                bold: true,
                ..Default::default()
            },
        });
        let job = runs_job("plain marked", &runs, &t, 15.0, doc::Kind::Para, false);
        assert_eq!(job.sections.len(), 2);
        let plain = &job.sections[0].format;
        let marked = &job.sections[1].format;
        assert_eq!(plain.background, egui::Color32::TRANSPARENT, "no mark, no wash");
        assert_eq!(marked.background, t.marks[1], "the marker is the run's own colour");
        assert_eq!(marked.color, t.mark_ink, "ink on a marker is dark, not themed text");
        assert!(marked.font_id.size == plain.font_id.size);
    }

    /// The guard that outlived the markdown editor: **read mode never shows a marker**. Read mode
    /// consumes the markup as it lays it out, so no construct may leak `**`, `](`, `@due(` or a
    /// bullet's `- `, and the words must survive. (There is no editor-side counterpart any more —
    /// the editor lays out a document block's own runs, so nothing is parsed and nothing can leak.)
    #[test]
    fn read_mode_never_shows_a_marker() {
        let t = theme::themes()[0];
        let markers = ["**", "~~", "`", "[[", "]]", "![", "](", "@due(", "# ", "- ", "> "];
        let cases = [
            "plain words #tag",
            "**bold** words",
            "*italic* words",
            "~~struck~~ words",
            "`code` words",
            "# heading words",
            "### heading three words",
            "- bulleted words",
            "- [ ] todo words",
            "> quoted words",
            "[label](https://example.com/path) words",
            "[[wiki page]] words",
            "@due(2026-10-24) words",
            "![alt text](assets/x.png) inline words",
        ];
        for line in cases {
            let read = render_line(line, &t, 15.0).text;
            for m in markers {
                assert!(!read.contains(m), "read mode shows {m:?} for {line:?}: {read:?}");
            }
            assert!(read.contains("words"), "read mode lost the words of {line:?}: {read:?}");
        }
        // `![` + a multi-byte alt character used to panic the Tasks view: the parser sliced at
        // byte 3, which is inside the first character of `![日本](…)` or `![📷](…)`.
        for line in [
            "- [ ] ![日本](assets/x.png) 参照 words",
            "- [ ] ![📷](assets/x.png) words",
            "- [ ] ![a] words",
            "- [ ] ![ words",
        ] {
            let out = render_line(line, &t, 15.0).text;
            assert!(out.contains("words") || line.contains("参照"), "lost the words: {line:?}");
        }

        // the styling is real, not merely dropped
        for line in ["**bold** words", "# **bold** heading", "> **bold** quote"] {
            let j = render_line(line, &t, 15.0);
            let at = j.text.find("bold").expect("word present");
            let bold = j.sections.iter().any(|s| {
                s.format.font_id.family == fam(theme::F_BOLD)
                    && s.byte_range.start <= at
                    && at < s.byte_range.end
            });
            assert!(bold, "not laid out with the bold face: {line:?} -> {:?}", j.text);
        }
    }

}

pub fn render_line(line: &str, t: &Theme, base: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let f0 = F::default();
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return job;
    }

    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && (trimmed.len() == hashes || trimmed.as_bytes()[hashes] == b' ')
    {
        let size = match hashes {
            1 => base + 8.5,
            2 => base + 5.0,
            3 => base + 3.0,
            4 => base + 1.5,
            _ => base,
        };
        // Through the inline pass as well, so a heading's `**bold**` is styled, not printed.
        render_inline(
            &mut job,
            trimmed[hashes..].trim_start(),
            F {
                bold: hashes <= 2,
                semi: hashes > 2,
                heading: true,
                ..f0
            },
            t,
            size,
        );
        return job;
    }
    if trimmed.starts_with('>') {
        // No bar is drawn into the words: `▏` (U+258F) is in none of the bundled faces and came out
        // as the missing-glyph box, so the caller paints one in the gutter instead. The words keep
        // their quiet styling and nothing else.
        render_inline(
            &mut job,
            trimmed[1..].trim_start(),
            F {
                dim: true,
                italic: true,
                ..f0
            },
            t,
            base,
        );
        return job;
    }

    for m in ["- ", "* ", "+ "] {
        if trimmed.starts_with(m) {
            if let Some((done, text)) = crate::vault::parse_todo_line(trimmed) {
                let mut f = f0;
                if done {
                    f.strike = true;
                    f.dim = true;
                }
                render_inline(&mut job, &text, f, t, base);
                return job;
            }
            push(
                &mut job,
                "•  ",
                F {
                    dim: true,
                    ..f0
                },
                t,
                base,
            );
            render_inline(&mut job, &trimmed[m.len()..], f0, t, base);
            return job;
        }
    }

    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0
        && trimmed.len() > digits + 1
        && trimmed.as_bytes()[digits] == b'.'
        && trimmed.as_bytes()[digits + 1] == b' '
    {
        push(
            &mut job,
            &format!("{}. ", &trimmed[..digits]),
            F {
                dim: true,
                ..f0
            },
            t,
            base,
        );
        render_inline(&mut job, &trimmed[digits + 2..], f0, t, base);
        return job;
    }

    render_inline(&mut job, trimmed, f0, t, base);
    job
}

/// Lay out one block's runs.
///
/// The text laid out is exactly the text the document holds — the words — and every property comes
/// from the run's attributes, so nothing is parsed, nothing can leak, and the caret lines up with
/// the text by construction.
pub fn runs_job(
    text: &str,
    runs: &[doc::Run],
    t: &Theme,
    size: f32,
    kind: doc::Kind,
    done: bool,
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let heading = matches!(kind, doc::Kind::Heading(_));
    let semi = matches!(kind, doc::Kind::Heading(n) if n > 2);
    let mut at = 0usize;
    for r in runs {
        let n = r.text.chars().count();
        if n == 0 {
            continue;
        }
        // a run's own size step scales the block's base size
        let size = size * doc::size_scale(r.a.size);
        let f = F {
            bold: r.a.bold || (heading && !semi),
            italic: r.a.italic,
            // a ticked task reads struck through and dimmed, exactly as it does in the tasks view —
            // a painted line over the row (or along its top edge) was not that
            strike: r.a.strike || done,
            code: r.a.code || kind == doc::Kind::Code,
            block_code: kind == doc::Kind::Code,
            link: r.a.link.is_some(),
            dim: done || kind == doc::Kind::Quote || kind == doc::Kind::Code,
            tag: r.a.tag,
            due: r.a.due.is_some(),
            mark: r.a.mark,
            heading,
            semi,
        };
        job.append(&r.text, 0.0, format(size, f, t));
        at += n;
    }
    // a document should never have text outside its runs; if it somehow does, show it plainly rather
    // than dropping it
    let total = text.chars().count();
    if at < total {
        let rest: String = text.chars().skip(at).collect();
        job.append(&rest, 0.0, format(size, F::default(), t));
    }
    job
}

/// Marker-free rendering for read mode: the markup is dropped, since read mode has no caret to
/// keep in step with the file.
fn render_inline(job: &mut LayoutJob, s: &str, f: F, t: &Theme, size: f32) {
    inline_pass(job, s, f, t, size);
}

/// The inline pass, for markdown: `**bold**` renders bold, `[label](url)` renders just the label,
/// a wiki link renders its target, an image line renders its alt text and `@due(x)` becomes
/// "due x". The markup itself is consumed — nothing lays it out — so no marker can be seen in read
/// mode, in the tasks and tags views, or in the markdown export.
fn inline_pass(job: &mut LayoutJob, s: &str, f: F, t: &Theme, size: f32) {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut run = 0usize;
    while i < b.len() {
        let rest = &s[i..];
        if b[i] == b'`' {
            if let Some(rel) = rest[1..].find('`') {
                let end = i + 1 + rel;
                push(job, &s[run..i], f, t, size);
                push(
                    job,
                    &s[i + 1..end],
                    F {
                        code: true,
                        ..f
                    },
                    t,
                    size,
                );
                i = end + 1;
                run = i;
                continue;
            }
        }
        if rest.starts_with("**") || rest.starts_with("__") {
            let m = &rest[..2];
            if let Some(rel) = rest[2..].find(m) {
                let end = i + 2 + rel;
                if end > i + 2 {
                    push(job, &s[run..i], f, t, size);
                    inline_pass(
                        job,
                        &s[i + 2..end],
                        F {
                            bold: true,
                            ..f
                        },
                        t,
                        size,
                    );
                    i = end + 2;
                    run = i;
                    continue;
                }
            }
        }
        if rest.starts_with("~~") {
            if let Some(rel) = rest[2..].find("~~") {
                let end = i + 2 + rel;
                if end > i + 2 {
                    push(job, &s[run..i], f, t, size);
                    inline_pass(
                        job,
                        &s[i + 2..end],
                        F {
                            strike: true,
                            dim: true,
                            ..f
                        },
                        t,
                        size,
                    );
                    i = end + 2;
                    run = i;
                    continue;
                }
            }
        }
        if b[i] == b'*' || b[i] == b'_' {
            let c = b[i];
            let prev_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
            let next_ok = i + 1 < b.len() && b[i + 1] != c && b[i + 1] != b' ';
            if prev_ok && next_ok {
                if let Some(rel) = rest[1..].find(c as char) {
                    let end = i + 1 + rel;
                    if end > i + 1 {
                        push(job, &s[run..i], f, t, size);
                        inline_pass(
                            job,
                            &s[i + 1..end],
                            F {
                                italic: true,
                                ..f
                            },
                            t,
                            size,
                        );
                        i = end + 1;
                        run = i;
                        continue;
                    }
                }
            }
        }
        // wiki link: show the target only
        if rest.starts_with("[[") {
            if let Some(rel) = rest[2..].find("]]") {
                let end = i + 2 + rel;
                push(job, &s[run..i], f, t, size);
                push(
                    job,
                    &s[i + 2..end],
                    F {
                        link: true,
                        ..f
                    },
                    t,
                    size,
                );
                i = end + 2;
                run = i;
                continue;
            }
        }
        // markdown link: show the label only
        if b[i] == b'[' {
            if let Some(rel) = rest[1..].find("](") {
                let close = i + 1 + rel;
                if let Some(rel2) = s[close + 2..].find(')') {
                    let end = close + 2 + rel2;
                    push(job, &s[run..i], f, t, size);
                    push(
                        job,
                        &s[i + 1..close],
                        F {
                            link: true,
                            ..f
                        },
                        t,
                        size,
                    );
                    i = end + 1;
                    run = i;
                    continue;
                }
            }
        }
        // image: alt text only
        //
        // `![` is two bytes, so the search for `]` starts *after* it (`rest.get(2..)`), never at
        // byte 3: with `![日本](x.png)` byte 3 is the middle of `日`, and the slice panicked the
        // Tasks view ("byte index 3 is not a char boundary") — the markdown renderer's twin of the
        // old inline-picture panic in `doc::parse_inline`.
        if let Some(after) = rest.strip_prefix("![") {
            if let Some(rel) = after.find(']') {
                let end = i + 2 + rel;
                push(job, &s[run..i], f, t, size);
                push(
                    job,
                    &s[i + 2..end],
                    F {
                        dim: true,
                        ..f
                    },
                    t,
                    size,
                );
                // the `]` itself is part of the syntax, whether or not a `(...)` follows
                let skip = s
                    .get(end..)
                    .and_then(|r| r.find(')'))
                    .map(|r| end + r + 1)
                    .unwrap_or(end + 1);
                i = skip;
                run = i;
                continue;
            }
        }
        // due dates read as a date: `@due(2026-10-24)` becomes "due 2026-10-24"
        if rest.starts_with("@due(") {
            if let Some(rel) = rest.find(')') {
                let end = i + rel + 1;
                push(job, &s[run..i], f, t, size);
                push(
                    job,
                    &format!("due {}", &s[i + 5..i + rel]),
                    F {
                        due: true,
                        ..f
                    },
                    t,
                    size,
                );
                i = end;
                run = i;
                continue;
            }
        }
        i += s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    push(job, &s[run..], f, t, size);
}

