//! Pictures on a page: their intrinsic size, the texture cache, and the decode pool.
//!
//! Two things made a page with a lot of screenshots slow enough to look broken:
//!
//! 1. the cache dropped **everything** once it reached its cap, so a page with more pictures than
//!    the cap re-decoded all of them on every frame — 34 screenshots is ~500 ms of PNG decoding
//!    per frame, i.e. about 2 fps, forever;
//! 2. decoding ran on the UI thread, so even with a correct cache the first paint of a picture-heavy
//!    page froze the window for half a second, and any eviction froze it again.
//!
//! So: an LRU with a **byte** budget that only evicts entries the current frame did not draw, and
//! decoding on a small worker pool. The UI thread reads just the file header (microseconds) to learn
//! a picture's size, which is all the layout needs; the pixels arrive a frame or two later and the
//! entry repaints itself then. No frame ever waits on a PNG.
//!
//! The header read is also what removes the old per-frame `exists()` + `metadata()` pair: one stat
//! per entry, revalidated at most once a second.

use eframe::egui::{self, Vec2};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The widest a display texture is made: the note column is at most 880 pt wide, so 1024 px covers
/// every size the app can draw and a picture is never upscaled on screen. The file on disk is never
/// touched — this is a display copy, so the stored picture keeps its detail.
pub const MAX_TEX_DIM: u32 = 1024;

/// A texture is never made narrower than this, however small the picture is drawn: below it the
/// scaled-up blur is worse than the memory saved.
const MIN_TEX_DIM: u32 = 64;

/// How much VRAM decoded pictures may hold before the least recently used ones are dropped. A long
/// page of screenshots at 745×1024 is ~3 MB each, so this is ~85 pictures — comfortably more than
/// one page holds, which is the point: an eviction here should be rare, and cheap when it happens.
const BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// Decode threads. One screenshot takes ~30 ms; three at once fill a 34-picture page in a few frames
/// instead of blocking the window for half a second.
const WORKERS: usize = 3;

/// How often an entry is stat'ed again to notice that the file was replaced. Every frame would be a
/// syscall per picture per frame, for a file that changes about never.
const REVALIDATE: Duration = Duration::from_secs(1);

/// Re-decode when the wanted width leaves this band around the texture we already have, so dragging
/// a picture's corner does not queue a decode for every frame of the drag.
const REGROW_HI: f32 = 1.5;
const REGROW_LO: f32 = 0.6;

/// What is known about a picture's file without decoding it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Source {
    /// Readable, with its own pixel size.
    Known(Vec2),
    /// No such file (the page's reference is stale, or the asset was deleted).
    Missing,
    /// Present but not something the bundled decoders can size — the page says so.
    Unreadable,
}

/// What the view can draw this frame.
pub enum Preview {
    Ready(egui::TextureHandle),
    /// Decoding; ask again after the next frame (`ctx.request_repaint()` is already coming).
    Pending,
    /// The header read succeeded but the pixels did not decode.
    Failed,
}

#[derive(Clone)]
struct Tex {
    handle: egui::TextureHandle,
    /// Pixel width the texture was made at, which is what "close enough" is judged against.
    w: u32,
    bytes: usize,
}

struct Entry {
    mtime: u64,
    checked: Instant,
    source: Source,
    tex: Option<Tex>,
    /// The width a job is in flight for, so a picture is never queued twice for the same size.
    want: Option<u32>,
    failed: bool,
    /// The frame this picture was last drawn in. Eviction never touches the current frame's entries:
    /// dropping a texture that a shape in this frame's paint list refers to would draw a hole.
    used: u64,
}

impl Entry {
    fn new(path: &Path) -> Self {
        Self {
            mtime: mtime_of(path),
            checked: Instant::now(),
            source: probe_size(path),
            tex: None,
            want: None,
            failed: false,
            used: 0,
        }
    }
}

struct Job {
    path: PathBuf,
    mtime: u64,
    want_w: u32,
    /// Held so the worker can wake the UI when the pixels are ready, instead of the app polling
    /// (which would mean repainting forever while a picture decodes).
    ctx: egui::Context,
}

struct Done {
    path: PathBuf,
    mtime: u64,
    want_w: u32,
    image: Option<(egui::ColorImage, u32, u32)>,
}

/// The cache. One per app; call [`ImageCache::begin_frame`] once at the top of `update`.
#[derive(Default)]
pub struct ImageCache {
    entries: HashMap<PathBuf, Entry>,
    frame: u64,
    tx: Option<Sender<Job>>,
    done_rx: Option<Receiver<Done>>,
}

impl ImageCache {
    /// Start a frame: collect finished decodes and drop textures over budget.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.frame = self.frame.wrapping_add(1);
        self.drain(ctx);
        self.trim();
    }

    /// The picture's own size, read from the file header the first time it is asked for (and again
    /// only when the file changes). Cheap enough to call per picture per frame.
    pub fn source(&mut self, path: &Path) -> Source {
        let entry = self
            .entries
            .entry(path.to_path_buf())
            .or_insert_with(|| Entry::new(path));
        if entry.checked.elapsed() >= REVALIDATE {
            entry.checked = Instant::now();
            let mtime = mtime_of(path);
            if mtime != entry.mtime {
                // Replaced on disk: everything learned about the old file is stale.
                entry.mtime = mtime;
                entry.source = probe_size(path);
                entry.tex = None;
                entry.want = None;
                entry.failed = false;
            }
        }
        entry.source
    }

    /// The texture for a picture drawn `want_w` device pixels wide, queueing a decode when there is
    /// nothing suitable to draw. Call [`ImageCache::source`] first (the layout needs the size
    /// anyway); this assumes the entry exists.
    pub fn preview(&mut self, ctx: &egui::Context, path: &Path, want_w: u32) -> Preview {
        let frame = self.frame;
        let mut job: Option<Job> = None;
        let out = {
            let Some(entry) = self.entries.get_mut(path) else {
                return Preview::Failed;
            };
            entry.used = frame;
            let Source::Known(src) = entry.source else {
                return Preview::Failed;
            };
            // never wider than the file's own pixels: there is nothing to gain above that
            let want = want_w
                .clamp(MIN_TEX_DIM, MAX_TEX_DIM)
                .min(src.x.max(1.0) as u32)
                .max(1);
            match entry.tex.clone() {
                Some(tex) => {
                    // A texture at a wildly different size is replaced in the background; until it
                    // lands, the one we have is drawn stretched, which looks better than a hole.
                    if !close_enough(tex.w, want) && entry.want != Some(want) {
                        entry.want = Some(want);
                        job = Some(job_for(path, entry, want, ctx));
                    }
                    Preview::Ready(tex.handle)
                }
                None if entry.failed => Preview::Failed,
                None => {
                    if entry.want != Some(want) {
                        entry.want = Some(want);
                        job = Some(job_for(path, entry, want, ctx));
                    }
                    Preview::Pending
                }
            }
        };
        if let Some(job) = job {
            self.dispatch(job);
        }
        out
    }

    /// Forget a picture, so the next frame decodes it again. Kept for call sites that change the file
    /// behind the cache's back inside the one-second revalidation window.
    #[allow(dead_code)]
    pub fn forget(&mut self, path: &Path) {
        self.entries.remove(path);
    }

    fn drain(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.done_rx else { return };
        let mut done: Vec<Done> = Vec::new();
        while let Ok(d) = rx.try_recv() {
            done.push(d);
        }
        for d in done {
            let Some(entry) = self.entries.get_mut(&d.path) else {
                continue;
            };
            if entry.mtime != d.mtime {
                // the file changed while this job was decoding: the result is for the old picture
                entry.want = None;
                continue;
            }
            entry.want = None;
            match d.image {
                Some((color, w, h)) => {
                    let name = format!("nc-img:{}:{}", d.path.display(), d.want_w);
                    let handle = ctx.load_texture(name, color, egui::TextureOptions::LINEAR);
                    entry.tex = Some(Tex {
                        handle,
                        w,
                        bytes: (w as usize) * (h as usize) * 4,
                    });
                    entry.failed = false;
                }
                None => {
                    log::debug!(
                        "notecraft: cannot decode {} at {}px",
                        d.path.display(),
                        d.want_w
                    );
                    entry.failed = true;
                }
            }
        }
    }

    fn trim(&mut self) {
        self.trim_to(BUDGET_BYTES);
    }

    /// Drop least-recently-drawn textures until the cache fits `budget`. Entries drawn in the
    /// current frame are never dropped (their texture ids are already in this frame's paint list).
    fn trim_to(&mut self, budget: usize) {
        let mut total: usize = self
            .entries
            .values()
            .filter_map(|e| e.tex.as_ref().map(|t| t.bytes))
            .sum();
        if total <= budget {
            return;
        }
        let mut victims: Vec<(u64, usize, PathBuf)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.used < self.frame)
            .filter_map(|(p, e)| e.tex.as_ref().map(|t| (e.used, t.bytes, p.clone())))
            .collect();
        victims.sort_by_key(|v| v.0);
        for (_, bytes, path) in victims {
            if total <= budget {
                break;
            }
            if let Some(entry) = self.entries.get_mut(&path) {
                entry.tex = None;
                entry.want = None;
                total = total.saturating_sub(bytes);
            }
        }
    }

    fn dispatch(&mut self, job: Job) {
        let tx = self.pool();
        if tx.send(job).is_err() {
            // every worker is gone; the next request builds a new pool
            self.tx = None;
            self.done_rx = None;
        }
    }

    /// The worker queue, started on the first picture that needs decoding.
    fn pool(&mut self) -> Sender<Job> {
        if let Some(tx) = &self.tx {
            return tx.clone();
        }
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        let queue = Arc::new(Mutex::new(job_rx));
        for n in 0..WORKERS {
            let queue = Arc::clone(&queue);
            let done_tx = done_tx.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("nc-decode-{n}"))
                .spawn(move || loop {
                    // the lock is held only to take a job, never while decoding one
                    let job = {
                        let Ok(queue) = queue.lock() else { return };
                        match queue.recv() {
                            Ok(job) => job,
                            Err(_) => return,
                        }
                    };
                    let image = decode(&job);
                    let ctx = job.ctx.clone();
                    if done_tx
                        .send(Done {
                            path: job.path,
                            mtime: job.mtime,
                            want_w: job.want_w,
                            image,
                        })
                        .is_err()
                    {
                        return;
                    }
                    ctx.request_repaint();
                });
            if spawned.is_err() {
                log::warn!("notecraft: could not start decode thread {n}");
            }
        }
        self.done_rx = Some(done_rx);
        self.tx = Some(job_tx.clone());
        job_tx
    }
}

fn job_for(path: &Path, entry: &Entry, want: u32, ctx: &egui::Context) -> Job {
    Job {
        path: path.to_path_buf(),
        mtime: entry.mtime,
        want_w: want,
        ctx: ctx.clone(),
    }
}

/// Is a texture made at `have` px good enough for a picture drawn `want` px wide?
fn close_enough(have: u32, want: u32) -> bool {
    let have = have.max(1) as f32;
    let want = want.max(1) as f32;
    want <= have * REGROW_HI && want >= have * REGROW_LO
}

fn mtime_of(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The size of a picture, from its header alone — no pixels are decoded.
fn probe_size(path: &Path) -> Source {
    let reader = match image::ImageReader::open(path).and_then(|r| r.with_guessed_format()) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Source::Missing,
        Err(_) => return Source::Unreadable,
    };
    match reader.into_dimensions() {
        Ok((w, h)) if w > 0 && h > 0 => Source::Known(Vec2::new(w as f32, h as f32)),
        _ => Source::Unreadable,
    }
}

/// The display texture's size for a `w`×`h` picture wanted `want_w` px wide: never an upscale, never
/// over the cap, and never narrower than `MIN_TEX_DIM`.
pub(crate) fn tex_size(w: u32, h: u32, want_w: u32) -> (u32, u32) {
    let w = w.max(1) as f32;
    let h = h.max(1) as f32;
    let scale = (want_w.max(MIN_TEX_DIM) as f32 / w)
        .min(MAX_TEX_DIM as f32 / w.max(h))
        .min(1.0);
    (
        ((w * scale).round()).max(1.0) as u32,
        ((h * scale).round()).max(1.0) as u32,
    )
}

/// Decode a job's picture at its display size. Runs on a worker thread.
fn decode(job: &Job) -> Option<(egui::ColorImage, u32, u32)> {
    let img = image::ImageReader::open(&job.path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let (w, h) = (img.width(), img.height());
    let (nw, nh) = tex_size(w, h, job.want_w);
    let rgba = img.to_rgba8();
    let rgba = if (nw, nh) != (w, h) {
        image::imageops::resize(&rgba, nw, nh, image::imageops::FilterType::Triangle)
    } else {
        rgba
    };
    let color = egui::ColorImage::from_rgba_unmultiplied([nw as usize, nh as usize], rgba.as_raw());
    Some((color, nw, nh))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG on disk of the given size, in its own directory so tests cannot collide.
    fn test_png(tag: &str, w: u32, h: u32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("notecraft-imgcache-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pic.png");
        let img = image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        img.save_with_format(&path, image::ImageFormat::Png).unwrap();
        path
    }

    #[test]
    fn the_header_gives_the_size_without_decoding() {
        let path = test_png("header", 900, 600);
        let mut cache = ImageCache::default();
        assert_eq!(cache.source(&path), Source::Known(Vec2::new(900.0, 600.0)));
        // a second ask is a map lookup, not another stat
        assert_eq!(cache.source(&path), Source::Known(Vec2::new(900.0, 600.0)));
    }

    #[test]
    fn a_missing_file_says_so_instead_of_failing_silently() {
        let mut cache = ImageCache::default();
        let gone = std::env::temp_dir().join("notecraft-imgcache-not-here.png");
        assert_eq!(cache.source(&gone), Source::Missing);
    }

    #[test]
    fn textures_are_capped_and_never_upscaled() {
        // a big screenshot is brought down to the display width, not to 1024 by default
        assert_eq!(tex_size(1024, 1408, 640), (640, 880));
        // a picture smaller than the request keeps its own pixels
        assert_eq!(tex_size(300, 200, 800), (300, 200));
        // never over the cap, whatever is asked for
        let (w, h) = tex_size(4000, 3000, 4000);
        assert_eq!((w, h), (1024, 768));
        // and never below the floor
        let (w, _) = tex_size(4000, 3000, 8);
        assert_eq!(w, 64);
    }

    #[test]
    fn decode_lands_in_the_cache_and_is_reused() {
        let path = test_png("decode", 400, 300);
        let ctx = egui::Context::default();
        let mut cache = ImageCache::default();
        cache.begin_frame(&ctx);
        assert_eq!(cache.source(&path), Source::Known(Vec2::new(400.0, 300.0)));
        assert!(matches!(cache.preview(&ctx, &path, 400), Preview::Pending));
        // the worker finishes off this thread; a few frames of polling is what the app does too
        let mut ready = false;
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            cache.begin_frame(&ctx);
            if matches!(cache.preview(&ctx, &path, 400), Preview::Ready(_)) {
                ready = true;
                break;
            }
        }
        assert!(ready, "the decode never arrived");
        let bytes = cache.entries[&path].tex.as_ref().unwrap().bytes;
        assert_eq!(bytes, 400 * 300 * 4);
    }

    #[test]
    fn eviction_skips_the_frame_that_is_being_drawn() {
        let path = test_png("evict", 100, 100);
        let ctx = egui::Context::default();
        let mut cache = ImageCache::default();
        cache.begin_frame(&ctx);
        let _ = cache.source(&path);
        // a texture standing in for a decoded one, without waiting on a worker
        let color = egui::ColorImage::filled([100, 100], egui::Color32::RED);
        let handle = ctx.load_texture("t", color, egui::TextureOptions::LINEAR);
        cache.entries.get_mut(&path).unwrap().tex = Some(Tex {
            handle,
            w: 100,
            bytes: 100 * 100 * 4,
        });
        let _ = cache.preview(&ctx, &path, 100); // marks it used this frame
        cache.trim_to(0);
        assert!(
            cache.entries[&path].tex.is_some(),
            "a drawn picture lost its texture mid-frame"
        );

        // next frame it is not drawn, so a zero budget may take it
        cache.begin_frame(&ctx);
        cache.trim_to(0);
        assert!(cache.entries[&path].tex.is_none(), "an old picture was kept");
    }
}
