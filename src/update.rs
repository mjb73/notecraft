//! *Check for updates…*: ask GitHub for the newest NoteCraft release, offer it,
//! install it and restart into it.
//!
//! **Nothing here dials out on its own.** The check runs when the person picks the
//! menu row, on a worker thread, so the window never waits on a socket; the reply
//! comes back through a channel that `NoteCraft::update` polls each frame.
//!
//! The install is the released Linux tarball the project already publishes
//! (`.github/workflows/release.yml`: `notecraft-<version>-<arch>-linux.tar.gz` with
//! a `.sha256` beside it), checked against that checksum before anything on disk is
//! touched, and only then unpacked and renamed over the running binary. A rename
//! replaces a running executable; writing into it would fail with `ETXTBSY`, which
//! is why the new build is staged beside the old one and moved into place in one
//! step. Other platforms are told to use the release page — only the Linux tarball
//! is something this can install by itself.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

/// The public repository the releases are published to. The working copy pushes to
/// a private mirror; the downloads and the `releases/latest` answer live here.
pub const REPO: &str = "mjb73/notecraft";

/// The version this build reports, from `Cargo.toml` — the same number the release
/// tag carries and the About box shows.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// GitHub asks for a user agent and refuses requests without one.
const UA: &str = concat!("NoteCraft/", env!("CARGO_PKG_VERSION"));

/// No step may hang the worker forever: a check that never returns would leave the
/// menu row saying *checking…* for the rest of the session.
const TIMEOUT: Duration = Duration::from_secs(30);

/// A release download can be a few hundred kB or tens of MB; the default body limit
/// in ureq is 10 MB.
const MAX_BODY: u64 = 96 * 1024 * 1024;

/// One downloadable file of a release.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

/// What the release says about itself, and which of its files belongs to this
/// machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Without the tag's `v`: `1.3.1`.
    pub version: String,
    /// The release body, as written in `.github/release-notes.md`.
    pub notes: String,
    /// `notecraft-1.3.1-x86_64-linux.tar.gz`.
    pub tarball: String,
    pub tarball_url: String,
    /// `…tar.gz.sha256`, when the release has it. A download without it is refused
    /// rather than trusted.
    pub sha_url: Option<String>,
    /// The release page, for the person who would rather read it in a browser.
    pub page: String,
}

/// Where the check asks. `NOTECRAFT_UPDATE_ENDPOINT` points it somewhere else — at a
/// test server, or at a file on disk — without a rebuild; the reply must have
/// GitHub's release shape whatever the address.
pub fn endpoint() -> String {
    match std::env::var("NOTECRAFT_UPDATE_ENDPOINT") {
        Ok(url) if !url.trim().is_empty() => url.trim().to_string(),
        _ => format!("https://api.github.com/repos/{REPO}/releases/latest"),
    }
}

/// The architecture the release assets are named for: `x86_64` or `aarch64`.
pub fn arch() -> &'static str {
    std::env::consts::ARCH
}

/// `v1.3.1` and `1.3.1` both read as `[1, 3, 1]`; a part that is not a number reads
/// as 0, so a tag with a suffix (`2.0.0-beta`) still compares on its numbers rather
/// than crashing.
pub(crate) fn numbers(v: &str) -> Vec<u32> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u32>()
                .unwrap_or(0)
        })
        .collect()
}

/// Is `latest` a later version than `current`? Numeric, so `1.10.0` beats `1.9.0` —
/// string order would say the opposite.
pub fn newer(current: &str, latest: &str) -> bool {
    let mut a = numbers(current);
    let mut b = numbers(latest);
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    b > a
}

/// The first line of the release notes that is not a heading, for the one-line
/// message.
pub fn summary(notes: &str) -> String {
    notes
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("")
        .to_string()
}

/// Read a GitHub release reply into the release this machine can install.
///
/// Pure, so the shape is checked against a fixture without a socket. A release with
/// no build for this architecture is an answer, not a crash: the error says which
/// architecture was missing.
pub fn parse_release(json: &str) -> Result<Release, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("the reply was not a release: {e}"))?;
    let tag = v["tag_name"]
        .as_str()
        .ok_or("the reply has no release tag")?
        .to_string();
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    if version.is_empty() {
        return Err("the release tag is empty".into());
    }
    let notes = v["body"].as_str().unwrap_or("").to_string();
    let page = v["html_url"].as_str().unwrap_or("").to_string();
    let want = format!("notecraft-{version}-{}-linux.tar.gz", arch());
    let assets: Vec<Asset> = v["assets"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some(Asset {
                        name: a["name"].as_str()?.to_string(),
                        url: a["browser_download_url"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let tarball = assets
        .iter()
        .find(|a| a.name == want)
        .ok_or_else(|| format!("release {tag} has no build for {} ({want})", arch()))?;
    let sha = format!("{}.sha256", tarball.name);
    let sha_url = assets.iter().find(|a| a.name == sha).map(|a| a.url.clone());
    Ok(Release {
        version,
        notes,
        tarball: tarball.name.clone(),
        tarball_url: tarball.url.clone(),
        sha_url,
        page,
    })
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .new_agent()
}

/// One request, to bytes. A failure carries the reason in words the status line can
/// show: a status code, a TLS refusal, a timeout.
fn get(url: &str) -> Result<Vec<u8>, String> {
    let mut res = agent()
        .get(url)
        .header("User-Agent", UA)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("could not reach GitHub ({e})"))?;
    res.body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_vec()
        .map_err(|e| format!("the download stopped ({e})"))
}

fn get_text(url: &str) -> Result<String, String> {
    let mut res = agent()
        .get(url)
        .header("User-Agent", UA)
        .call()
        .map_err(|e| format!("could not reach GitHub ({e})"))?;
    res.body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("the reply stopped ({e})"))
}

/// What a check found.
pub enum Found {
    /// A later release, ready to offer.
    Newer(Box<Release>),
    /// The newest release is the one running.
    Same(String),
}

/// Ask GitHub for the newest release and compare it with the running build.
pub fn check() -> Result<Found, String> {
    let text = get_text(&endpoint())?;
    let rel = parse_release(&text)?;
    if newer(VERSION, &rel.version) {
        Ok(Found::Newer(Box::new(rel)))
    } else {
        Ok(Found::Same(rel.version))
    }
}

/// The hex digest of some bytes, the form a `.sha256` file carries.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// The digest named by a `.sha256` file: `sha256sum` writes the hex first, then two
/// spaces and the file name.
pub(crate) fn checksum_from_file(text: &str) -> Option<String> {
    let hex = text.split_whitespace().next()?;
    if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(hex.to_ascii_lowercase())
    } else {
        None
    }
}

/// Unpack the released tarball and put its binary where this one runs from.
///
/// `exe` is `current_exe()`: the `.desktop` entry and install.sh both point at a real
/// file, so replacing that file is what "installed" means. The archive is unpacked in
/// a scratch folder, its member copied beside `exe` as `<name>.new`, made executable,
/// and then renamed over `exe` — one atomic step the running process cannot lose, and
/// one a running executable allows.
pub fn install_archive(tar: &[u8], exe: &Path) -> Result<(), String> {
    let work = std::env::temp_dir().join(format!("notecraft-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| format!("no scratch folder ({e})"))?;
    let cleanup = |work: &Path| {
        let _ = std::fs::remove_dir_all(work);
    };
    let archive = work.join("release.tar.gz");
    std::fs::write(&archive, tar).map_err(|e| {
        cleanup(&work);
        format!("the download could not be saved ({e})")
    })?;
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&work)
        .status()
        .map_err(|e| {
            cleanup(&work);
            format!("tar could not be run ({e})")
        })?;
    if !status.success() {
        cleanup(&work);
        return Err("the downloaded archive would not unpack".into());
    }
    // The tarball holds one folder, `notecraft-<version>-<arch>-linux/`, and the
    // binary in it is called `notecraft`.
    let mut found: Option<PathBuf> = None;
    if let Ok(entries) = std::fs::read_dir(&work) {
        for entry in entries.flatten() {
            let candidate = entry.path().join("notecraft");
            if candidate.is_file() {
                found = Some(candidate);
                break;
            }
        }
    }
    let Some(binary) = found else {
        cleanup(&work);
        return Err("the archive holds no notecraft binary".into());
    };
    let name = exe
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "notecraft".to_string());
    let staged = exe.with_file_name(format!("{name}.new"));
    if let Err(e) = std::fs::copy(&binary, &staged) {
        cleanup(&work);
        return Err(format!("the new build could not be put in place ({e})"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
    }
    if let Err(e) = std::fs::rename(&staged, exe) {
        let _ = std::fs::remove_file(&staged);
        cleanup(&work);
        return Err(format!("the new build could not replace this one ({e})"));
    }
    cleanup(&work);
    Ok(())
}

/// Download the release, verify it, install it. The caller has already decided the
/// person asked for it.
pub fn download_and_install(rel: &Release) -> Result<PathBuf, String> {
    if std::env::consts::OS != "linux" {
        return Err(format!(
            "updates install themselves on Linux; this {} build should take the release from {}",
            std::env::consts::OS,
            if rel.page.is_empty() {
                "its GitHub page"
            } else {
                rel.page.as_str()
            }
        ));
    }
    let sha_url = rel
        .sha_url
        .clone()
        .ok_or("the release has no checksum file, so the download cannot be checked")?;
    let tar = get(&rel.tarball_url)?;
    let sum_text = get_text(&sha_url)?;
    let want = checksum_from_file(&sum_text).ok_or("the checksum file is not a sha256 sum")?;
    let got = sha256_hex(&tar);
    if got != want {
        return Err(format!(
            "the download does not match its checksum ({} for {}, expected {})",
            &got[..12],
            rel.tarball,
            &want[..12]
        ));
    }
    let exe = std::env::current_exe().map_err(|e| format!("this build has no path ({e})"))?;
    install_archive(&tar, &exe)?;
    Ok(exe)
}

/// What one page-turn of `NoteCraft::update` should do about the update.
pub enum Poll {
    /// Nothing finished this frame.
    Idle,
    /// Say this in the status line.
    Say(String),
    /// The new build is in place at this path: save the state and restart into it.
    /// The path is carried rather than looked up at restart time: `current_exe()`
    /// after the rename answers `/…/notecraft (deleted)`, because the running
    /// process's own inode is the one that was replaced.
    Restart(PathBuf),
}

/// Messages from the worker thread.
enum Msg {
    Checked(Result<Found, String>),
    Installed(Result<PathBuf, String>),
}

/// The update state the window holds: whether a job is running, what to offer, and
/// what went wrong.
#[derive(Default)]
pub struct Update {
    pub checking: bool,
    pub installing: bool,
    /// The newer release, once one has been found.
    pub release: Option<Release>,
    /// The newest version seen, so "up to date" can name it.
    pub latest: String,
    /// The last failure, shown in its own small window until dismissed.
    pub error: Option<String>,
    rx: Option<Receiver<Msg>>,
}

impl Update {
    /// Start asking GitHub, on a thread.
    pub fn start_check(&mut self) {
        if self.checking || self.installing {
            return;
        }
        self.checking = true;
        self.error = None;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Checked(check()));
        });
    }

    /// Start downloading and installing the offered release, on a thread.
    pub fn start_install(&mut self) {
        let Some(rel) = self.release.clone() else {
            return;
        };
        if self.checking || self.installing {
            return;
        }
        self.installing = true;
        self.error = None;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Installed(download_and_install(&rel)));
        });
    }

    /// Put the offer away without installing it.
    pub fn dismiss(&mut self) {
        self.release = None;
        self.error = None;
    }

    /// Collect a finished job. Called once a frame; returns at most one action.
    pub fn poll(&mut self) -> Poll {
        let Some(rx) = self.rx.as_ref() else {
            return Poll::Idle;
        };
        let Ok(msg) = rx.try_recv() else {
            return Poll::Idle;
        };
        self.rx = None;
        self.checking = false;
        self.installing = false;
        match msg {
            Msg::Checked(Ok(Found::Newer(rel))) => {
                self.latest = rel.version.clone();
                let said = summary(&rel.notes);
                self.release = Some(*rel);
                Poll::Say(if said.is_empty() {
                    format!("NoteCraft {} is available.", self.latest)
                } else {
                    format!("NoteCraft {} is available — {said}", self.latest)
                })
            }
            Msg::Checked(Ok(Found::Same(latest))) => {
                self.latest = latest.clone();
                Poll::Say(format!(
                    "NoteCraft {latest} is the latest version — this is {}.",
                    VERSION
                ))
            }
            Msg::Checked(Err(e)) => {
                self.error = Some(e.clone());
                Poll::Say(format!("Could not check for updates: {e}"))
            }
            Msg::Installed(Ok(exe)) => Poll::Restart(exe),
            Msg::Installed(Err(e)) => {
                // The offer goes away as well: the window then shows the reason
                // instead of the same *Install and restart* button that just failed.
                self.release = None;
                self.error = Some(e.clone());
                Poll::Say(format!("The update did not install: {e}"))
            }
        }
    }

    /// Is a job running (so the menu row can say so and do nothing)?
    pub fn busy(&self) -> bool {
        self.checking || self.installing
    }
}

/// Start the build that is now on disk, with the arguments this one was given, so the
/// window comes back on the same note with the same vault.
pub fn restart(exe: &Path) -> Result<(), String> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    std::process::Command::new(exe)
        .args(&args)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("the new build would not start ({e})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(newer("1.3.0", "1.3.1"));
        assert!(newer("1.9.0", "1.10.0"));
        assert!(!newer("1.3.0", "1.3.0"));
        assert!(!newer("1.3.0", "v1.2.9"));
        assert!(newer("1.3", "v2"));
    }

    #[test]
    fn a_release_reply_names_the_linux_build_for_this_arch() {
        let json = r###"{
            "tag_name": "v1.3.1",
            "html_url": "https://github.com/mjb73/notecraft/releases/tag/v1.3.1",
            "body": "## What's new in 1.3.1\n\nFaster search.",
            "assets": [
                {"name": "notecraft-1.3.1-x86_64-linux.tar.gz", "browser_download_url": "https://example.invalid/x86.tar.gz"},
                {"name": "notecraft-1.3.1-x86_64-linux.tar.gz.sha256", "browser_download_url": "https://example.invalid/x86.tar.gz.sha256"},
                {"name": "NoteCraft-1.3.1-x86_64.dmg", "browser_download_url": "https://example.invalid/mac.dmg"}
            ]
        }"###;
        let rel = parse_release(json).expect("a real reply was refused");
        assert_eq!(rel.version, "1.3.1");
        assert_eq!(
            rel.tarball,
            format!("notecraft-1.3.1-{}-linux.tar.gz", arch())
        );
        assert!(rel.sha_url.expect("checksum link").ends_with(".sha256"));
        assert!(summary(&rel.notes).contains("Faster search"));
        assert!(parse_release(r###"{"tag_name":"v1.3.1","assets":[]}"###).is_err());
    }

    #[test]
    fn a_checksum_file_reads_back_and_a_bad_line_does_not() {
        let hex = sha256_hex(b"hello");
        assert_eq!(checksum_from_file(&format!("{hex}  x.tar.gz\n")).as_deref(), Some(hex.as_str()));
        assert!(checksum_from_file("not a checksum").is_none());
        assert_eq!(hex.len(), 64);
    }

    #[test]
    fn a_release_archive_installs_over_a_running_binary_and_leaves_no_staging_file() {
        let root = std::env::temp_dir().join(format!("notecraft-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let member = "notecraft-9.9.9-x86_64-linux";
        let pack = root.join(member);
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("notecraft"), b"NEW BUILD\n").unwrap();
        let archive = root.join(format!("{member}.tar.gz"));
        let ok = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&root)
            .arg(member)
            .status()
            .unwrap();
        assert!(ok.success(), "tar would not build the fixture");
        let bytes = std::fs::read(&archive).unwrap();
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("notecraft");
        std::fs::write(&exe, b"OLD BUILD\n").unwrap();
        install_archive(&bytes, &exe).expect("install");
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "NEW BUILD\n");
        assert!(!bin.join("notecraft.new").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o111 != 0);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
