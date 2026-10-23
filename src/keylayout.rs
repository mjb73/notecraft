//! The keyboard layout, as KDE knows it.
//!
//! NoteCraft does not own a keymap — the compositor does. The toggle in the status bar therefore
//! asks KWin's layout service (`org.kde.keyboard`, `/Layouts`, interface `org.kde.KeyboardLayouts`)
//! which layouts exist and which one is live, and switches between the British and the US one.
//!
//! KWin reads its layout list from `~/.config/kxkbrc` **at start-up only**: a layout added while
//! the session is running is not in `getLayoutsList` until the next login, so the toggle reports
//! that instead of looking like a click that did nothing.
//!
//! No D-Bus crate: the whole conversation is three method calls, and `qdbus6`/`qdbus` already
//! speak it. A call costs a few milliseconds, which is why the result is cached and re-read on a
//! timer rather than every frame.

use std::process::Command;

/// One layout as KWin lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// xkb code — `gb`, `us`, … (this is what the pair is decided on).
    pub code: String,
    /// The name KDE shows, e.g. `English (UK)`.
    pub name: String,
}

/// What KWin says about the keyboard: every layout it has, and which one is live.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyLayout {
    pub layouts: Vec<Layout>,
    pub current: usize,
}

impl KeyLayout {
    /// The live layout, if the index KWin reported still names one.
    pub fn live(&self) -> Option<&Layout> {
        self.layouts.get(self.current)
    }

    /// The short label for the status bar: `UK`, `US` — the pair this toggle is about — or the
    /// code in capitals for anything else.
    pub fn short(&self) -> String {
        short_label(self.live().map(|l| l.code.as_str()).unwrap_or(""))
    }

    /// The layout a click would switch to.
    ///
    /// British and US are the pair the toggle is named for, so each prefers the other. A third
    /// layout is still reachable (a German keyboard is a legitimate second layout); it is simply
    /// taken in KWin's own order.
    pub fn other(&self) -> Option<(usize, &Layout)> {
        let live = self.live()?;
        if self.layouts.len() < 2 {
            return None;
        }
        let want = match live.code.as_str() {
            "gb" => Some("us"),
            "us" => Some("gb"),
            _ => None,
        };
        let idx = want
            .and_then(|code| self.layouts.iter().position(|l| l.code == code))
            .unwrap_or((self.current + 1) % self.layouts.len());
        self.layouts.get(idx).map(|l| (idx, l))
    }
}

/// `UK`/`US` for the two layouts the toggle names, the code uppercased for anything else.
pub fn short_label(code: &str) -> String {
    match code {
        "gb" => "UK".to_string(),
        "us" => "US".to_string(),
        "" => "—".to_string(),
        other => other.to_uppercase(),
    }
}

/// The xkb keymap the X11 server (XWayland) is running — the layout **NoteCraft's own typing** goes
/// through, because the app is an X11 client. This is the immediate fallback when KWin has not got
/// the second layout: `setxkbmap` on the X display changes what the app types at once, while KWin's
/// Wayland keymap (and therefore every other app) keeps the layout it built at login.
///
/// Measured on this box: `setxkbmap -layout us` against XWayland takes effect immediately and
/// survives `org.kde.KWin.reconfigure`, so it does not need re-applying on every frame.
pub fn x11_layout() -> Option<Layout> {
    let name = x11_group_name()?;
    Some(Layout {
        code: code_from_name(&name),
        name,
    })
}

/// Switch the X11 keymap (`setxkbmap -layout <code>`).
pub fn set_x11_layout(code: &str) -> Result<(), String> {
    let out = Command::new("setxkbmap")
        .args(["-layout", code])
        .output()
        .map_err(|e| format!("setxkbmap is not there: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// `English (US)` → `us`, `English (UK)` → `gb`, anything else → its own lowercased name.
pub fn code_from_name(name: &str) -> String {
    if name.contains("(US)") {
        "us".to_string()
    } else if name.contains("(UK)") {
        "gb".to_string()
    } else {
        name.to_lowercase()
    }
}

fn x11_group_name() -> Option<String> {
    let display = std::env::var("DISPLAY").ok()?;
    let out = Command::new("xkbcomp").args(["-xkb", &display, "-"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_group_name(&String::from_utf8_lossy(&out.stdout))
}

/// The first `name[group1]="English (UK)";` of a dumped keymap.
pub fn parse_group_name(keymap: &str) -> Option<String> {
    let at = keymap.find("name[group1]")?;
    let rest = &keymap[at..];
    let start = rest.find('"')? + 1;
    let end = rest[start..].find('"')? + start;
    Some(rest[start..end].to_string())
}

/// Ask KWin for the layout list and the live index. `None` when the layout service is not there
/// (no KDE session, or a different desktop) — the status bar then draws no toggle at all.
pub fn query() -> Option<KeyLayout> {
    let raw = call(&["--literal", "org.kde.keyboard", "/Layouts", "getLayoutsList"])?;
    let layouts = parse_layouts(&raw);
    if layouts.is_empty() {
        return None;
    }
    let current = call(&["org.kde.keyboard", "/Layouts", "getLayout"])
        .and_then(|s| parse_current(&s))
        .unwrap_or(0)
        .min(layouts.len() - 1);
    Some(KeyLayout { layouts, current })
}

/// Switch the whole desktop to `index`. This is KWin's own layout, so every window follows —
/// NoteCraft is only the switch, not the owner.
pub fn switch_to(index: usize) -> Result<(), String> {
    let idx = index.to_string();
    let out = call(&["org.kde.keyboard", "/Layouts", "setLayout", &idx]).ok_or(
        "KDE's keyboard service did not answer (is KWin running?)",
    )?;
    if out.trim() == "false" {
        return Err("KWin refused the layout change".to_string());
    }
    Ok(())
}

/// The layouts `kxkbrc` names — what System Settings shows. `[Layout]` carries `LayoutList` (codes)
/// and `DisplayNames`, both comma-separated.
pub fn configured_layouts() -> Vec<Layout> {
    let Some(dir) = directories::BaseDirs::new().map(|d| d.config_dir().to_path_buf()) else {
        return Vec::new();
    };
    match std::fs::read_to_string(dir.join("kxkbrc")) {
        Ok(text) => layouts_from_ini(&text),
        Err(_) => Vec::new(),
    }
}

/// Configured layouts KWin is **not** running, by display name.
///
/// This is the state that reads as a contradiction: System Settings lists a layout (it reads
/// `kxkbrc`), while the switch says there is nothing to switch to (KWin built its keymap when it
/// started). KWin only re-reads the list at start-up — measured on this box: writing the file,
/// `kwriteconfig6`, `org.kde.KWin.reconfigure`, the `reloadConfig` signal and a keyboard hotplug
/// all left `getLayoutsList` unchanged — so the remedy is a login, and the toggle says so.
pub fn configured_missing(live: &[Layout]) -> Vec<String> {
    configured_layouts()
        .into_iter()
        .filter(|c| !live.iter().any(|l| l.code == c.code))
        .map(|c| c.name)
        .collect()
}

/// A layout is configured but KWin is not running it.
pub fn pending_restart(live: &[Layout]) -> bool {
    !configured_missing(live).is_empty()
}

/// Parse a `kxkbrc` into layouts. Only `[Layout]` counts; a value of `\0` is how KConfig writes an
/// empty string and is not a layout.
pub fn layouts_from_ini(ini: &str) -> Vec<Layout> {
    let mut in_layout = false;
    let mut codes: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for line in ini.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_layout = line.eq_ignore_ascii_case("[Layout]");
            continue;
        }
        if !in_layout {
            continue;
        }
        if let Some(v) = line.strip_prefix("LayoutList=") {
            codes = split_list(v);
        } else if let Some(v) = line.strip_prefix("DisplayNames=") {
            names = split_list(v);
        }
    }
    codes
        .into_iter()
        .enumerate()
        .map(|(i, code)| Layout {
            name: names.get(i).cloned().unwrap_or_else(|| code.clone()),
            code,
        })
        .collect()
}

/// Split a KConfig list value on commas.
///
/// KConfig escapes a comma inside a value as `\,` — a layout name like
/// `English (US, intl.)` is written `English (US\, intl.)`, and splitting on every comma turned
/// two layouts into three names (one of them `English (US\`), so the status bar showed a fragment.
fn split_list(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&',') => {
                chars.next();
                cur.push(',');
            }
            ',' => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            c => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out.into_iter()
        .filter(|s| !s.is_empty() && s != r"\0")
        .collect()
}

/// Run one D-Bus method through whichever `qdbus` this Qt has, returning stdout on success.
fn call(args: &[&str]) -> Option<String> {
    for bin in ["qdbus6", "qdbus"] {
        if let Ok(out) = Command::new(bin).args(args).output() {
            if out.status.success() {
                return Some(String::from_utf8_lossy(&out.stdout).into_owned());
            }
        }
    }
    None
}

/// KWin's answer is a list of `(code, variant, display name)` triples. qdbus prints
/// `[Argument: a(sss) {[Argument: (sss) "gb", "", "English (UK)"], …}]`, gdbus prints
/// `([('gb', '', 'English (UK)')],)`: both quote the strings with one character, so the parser
/// only has to know the quotes — every three quoted strings are one layout.
pub fn parse_layouts(raw: &str) -> Vec<Layout> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in raw.chars() {
        match quote {
            Some(q) => {
                if escaped {
                    cur.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                    parts.push(std::mem::take(&mut cur));
                } else {
                    cur.push(c);
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    cur.clear();
                }
            }
        }
    }
    parts
        .chunks(3)
        .filter(|c| c.len() == 3)
        .map(|c| Layout {
            code: c[0].clone(),
            name: c[2].clone(),
        })
        .collect()
}

/// The live index. `getLayout` answers `0` (qdbus) or `(uint32 0,)` (gdbus), so the *last* run of
/// digits is the number — taking every digit would read the `32` of `uint32` as part of it.
pub fn parse_current(raw: &str) -> Option<usize> {
    raw.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .next_back()
        .and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const QDBUS: &str = r#"[Argument: a(sss) {[Argument: (sss) "gb", "", "English (UK)"], [Argument: (sss) "us", "", "English (US)"]}]"#;

    #[test]
    fn qdbus_layout_list_parses() {
        let got = parse_layouts(QDBUS);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].code, "gb");
        assert_eq!(got[0].name, "English (UK)");
        assert_eq!(got[1].code, "us");
        assert_eq!(got[1].name, "English (US)");
    }

    #[test]
    fn gdbus_layout_list_parses() {
        let got = parse_layouts("([('gb', '', 'English (UK)'), ('de', '', 'German')],)");
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].code, "de");
        assert_eq!(got[1].name, "German");
    }

    #[test]
    fn a_layout_name_with_a_comma_is_one_field() {
        let got = parse_layouts(r#"[Argument: (sss) "us", "", "English (US, intl.)"]"#);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "English (US, intl.)");
    }

    #[test]
    fn current_index_ignores_the_uint32() {
        assert_eq!(parse_current("0\n"), Some(0));
        assert_eq!(parse_current("(uint32 1,)"), Some(1));
        assert_eq!(parse_current("nonsense"), None);
    }

    #[test]
    fn the_toggle_is_the_british_us_pair() {
        let kl = KeyLayout {
            layouts: parse_layouts(QDBUS),
            current: 0,
        };
        assert_eq!(kl.short(), "UK");
        let (i, other) = kl.other().unwrap();
        assert_eq!(i, 1);
        assert_eq!(other.code, "us");
        // and back again from the US one
        let kl = KeyLayout {
            layouts: parse_layouts(QDBUS),
            current: 1,
        };
        assert_eq!(kl.short(), "US");
        assert_eq!(kl.other().unwrap().1.code, "gb");
    }

    #[test]
    fn one_layout_means_nothing_to_switch_to() {
        let kl = KeyLayout {
            layouts: parse_layouts(r#"[Argument: (sss) "gb", "", "English (UK)"]"#),
            current: 0,
        };
        assert!(kl.other().is_none());
    }

    #[test]
    fn a_third_layout_is_still_reachable() {
        let kl = KeyLayout {
            layouts: parse_layouts(
                r#"[Argument: (sss) "gb", "", "English (UK)"], [Argument: (sss) "de", "", "German"]"#,
            ),
            current: 0,
        };
        assert_eq!(kl.other().unwrap().1.code, "de");
    }

    #[test]
    fn an_unlisted_index_does_not_panic() {
        let kl = KeyLayout {
            layouts: parse_layouts(QDBUS),
            current: 9,
        };
        assert_eq!(kl.short(), "—");
        assert!(kl.other().is_none());
    }

    #[test]
    fn kxkbrc_layout_list_is_counted() {
        let got = layouts_from_ini(
            "[Layout]\nDisplayNames=English (UK),English (US)\nLayoutList=gb,us\nUse=true\n",
        );
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].code, "us");
        assert_eq!(got[1].name, "English (US)");
        // a comma inside a name is escaped by KConfig as `\,`: it must not split the list
        let got = layouts_from_ini(
            "[Layout]\nDisplayNames=English (UK),English (US\\, intl.)\nLayoutList=gb,us\nUse=true\n",
        );
        assert_eq!(got.len(), 2, "escaped comma split the names: {got:?}");
        assert_eq!(got[1].name, "English (US, intl.)");
        // names are optional — a code is better than nothing
        let got = layouts_from_ini("[Layout]\nLayoutList=gb,us\n");
        assert_eq!(got[1].name, "us");
        // an empty list is KDE not configuring layouts; `\0` is how KConfig writes an empty string
        assert!(layouts_from_ini("[Layout]\nLayoutList=\n").is_empty());
        assert!(layouts_from_ini("[Layout]\nLayoutList=\\0\n").is_empty());
        // another group's key is not a layout list
        assert!(layouts_from_ini("[Other]\nLayoutList=gb,us,de\n").is_empty());
    }

    #[test]
    fn the_x11_keymap_group_name_is_read() {
        let dump = "xkb_keymap {\n    xkb_keycodes \"evdev_aliases(qwerty)\" {\n...\n\
                    xkb_symbols \"pc_gb_inet(evdev)\" {\n    name[group1]=\"English (UK)\";\n";
        assert_eq!(parse_group_name(dump).as_deref(), Some("English (UK)"));
        assert_eq!(
            parse_group_name("xkb_symbols \"pc_us\" { name[group1]= \"English (US)\"; }").as_deref(),
            Some("English (US)")
        );
        assert_eq!(parse_group_name("nothing here"), None);
    }

    #[test]
    fn a_keymap_name_becomes_the_layout_code_the_pair_uses() {
        assert_eq!(code_from_name("English (US)"), "us");
        assert_eq!(code_from_name("English (UK)"), "gb");
        assert_eq!(code_from_name("German"), "german");
    }

    #[test]
    fn a_configured_but_absent_layout_is_named() {
        let live = vec![Layout {
            code: "gb".to_string(),
            name: "English (UK)".to_string(),
        }];
        let ini = "[Layout]\nDisplayNames=English (UK),English (US)\nLayoutList=gb,us\n";
        let configured = layouts_from_ini(ini);
        // the rule the status bar uses, without touching the real kxkbrc
        let missing: Vec<String> = configured
            .into_iter()
            .filter(|c| !live.iter().any(|l| l.code == c.code))
            .map(|c| c.name)
            .collect();
        assert_eq!(missing, vec!["English (US)".to_string()]);
        // nothing missing once KWin is actually running both
        let live = parse_layouts(QDBUS);
        let missing: Vec<String> = layouts_from_ini(ini)
            .into_iter()
            .filter(|c| !live.iter().any(|l| l.code == c.code))
            .map(|c| c.name)
            .collect();
        assert!(missing.is_empty());
    }
}
