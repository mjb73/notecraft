//! A Windows drop target that understands what a browser actually offers.
//!
//! The toolkit NoteCraft is built on (winit) asks the drag source for one format only: `CF_HDROP`,
//! a list of real files on disk. A browser dragged image is not a file on disk — it hands over a
//! *virtual file* (`FileGroupDescriptorW` + `FileContents`: a name, a size and a stream) or, for
//! some sources, the encoded bytes in a `PNG`/`image/png` format. None of those is `CF_HDROP`, so
//! winit produces no event at all and the window looks dead while a picture is dragged over it.
//!
//! This module registers its own `IDropTarget` on the same HWND (OLE allows exactly one per
//! window, so winit's is revoked first) and turns any of those payloads into something the app can
//! attach: either paths on disk, or names with bytes.
//!
//! Everything here is best-effort on purpose. A drop that cannot be read must never panic the app
//! or swallow the drag — it logs what the source offered and returns `DROPEFFECT_NONE`.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use windows::core::{implement, Result as WinResult, PCWSTR};
use windows::Win32::Foundation::{HGLOBAL, HWND, POINTL};
use windows::Win32::System::Com::{
    IDataObject, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL,
    TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop, RevokeDragDrop, CF_HDROP,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};

/// One thing a drop delivered.
#[derive(Debug, Clone)]
pub enum Dropped {
    /// Real files, as Explorer and Finder hand them over. Attached by path.
    Paths(Vec<PathBuf>),
    /// Virtual files: bytes that exist only inside the drag (browsers, some mail clients).
    Blobs(Vec<Blob>),
    /// A drag entered (true) or left (false) the window. winit's own hover events stop once its
    /// target is revoked, so the "release to attach" highlight has to come from here.
    Hover(bool),
    /// A drop arrived but nothing in it could be read. Carries the formats the source offered, which
    /// is the only way to find out why on a machine we cannot debug.
    Refused(String),
}

pub use crate::dropped::Blob;

/// Formats a source may offer. Registered names are looked up per drop because
/// `RegisterClipboardFormatW` ids are per-session and must not be cached across runs.
struct Formats {
    file_group: u16,
    file_contents: u16,
    image: Vec<(u16, &'static str)>,
}

fn formats() -> Formats {
    let reg = |name: &str| -> u16 {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) as u16 }
    };
    let mut image = Vec::new();
    for (name, ext) in [("PNG", "png"), ("image/png", "png"), ("image/jpeg", "jpg")] {
        let id = reg(name);
        if id != 0 {
            image.push((id, ext));
        }
    }
    Formats {
        file_group: reg("FileGroupDescriptorW"),
        file_contents: reg("FileContents"),
        image,
    }
}

/// The COM object. `#[implement]` generates the IUnknown plumbing (AddRef/Release/QueryInterface).
#[implement(IDropTarget)]
struct Target {
    tx: Sender<Dropped>,
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(
        &self,
        pdataobj: windows::core::Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        let effect = if can_take(pdataobj.ok().ok()) {
            let _ = self.tx.send(Dropped::Hover(true));
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        };
        unsafe { *pdweffect = effect };
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        // The effect was decided in DragEnter; OLE expects this call to repeat it.
        unsafe { *pdweffect = DROPEFFECT_COPY };
        Ok(())
    }

    fn DragLeave(&self) -> WinResult<()> {
        let _ = self.tx.send(Dropped::Hover(false));
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: windows::core::Ref<'_, IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        let Ok(obj) = pdataobj.ok() else {
            unsafe { *pdweffect = DROPEFFECT_NONE };
            return Ok(());
        };
        let _ = self.tx.send(Dropped::Hover(false));
        let (dropped, took) = match read_drop(&obj) {
            Some(d) => (d, true),
            None => (Dropped::Refused(describe(Some(&obj))), false),
        };
        // The receiver is the UI thread; a closed channel means the window is gone.
        let _ = self.tx.send(dropped);
        unsafe { *pdweffect = if took { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
        Ok(())
    }
}

/// True when we know how to read at least one of the offered formats.
fn can_take(obj: Option<&IDataObject>) -> bool {
    let Some(obj) = obj else { return false };
    let f = formats();
    has(obj, CF_HDROP.0, TYMED_HGLOBAL)
        || has(obj, f.file_group, TYMED_HGLOBAL)
        || f.image.iter().any(|(id, _)| {
            has(obj, *id, TYMED_HGLOBAL) || has(obj, *id, TYMED_ISTREAM)
        })
}

fn has(obj: &IDataObject, cf: u16, tymed: windows::Win32::System::Com::TYMED) -> bool {
    let fe = formatetc(cf, -1, tymed);
    unsafe { obj.QueryGetData(&fe) }.is_ok()
}

fn formatetc(cf: u16, index: i32, tymed: windows::Win32::System::Com::TYMED) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: index,
        tymed: tymed.0 as u32,
    }
}

fn read_drop(obj: &IDataObject) -> Option<Dropped> {
    if let Some(paths) = hdrop_paths(obj) {
        if !paths.is_empty() {
            return Some(Dropped::Paths(paths));
        }
    }
    let blobs = virtual_files(obj);
    if !blobs.is_empty() {
        return Some(Dropped::Blobs(blobs));
    }
    let f = formats();
    for (id, ext) in &f.image {
        if let Some(bytes) = blob_of(obj, *id, -1) {
            if !bytes.is_empty() {
                return Some(Dropped::Blobs(vec![Blob {
                    name: format!("dropped.{ext}"),
                    bytes,
                }]));
            }
        }
    }
    None
}

/// `CF_HDROP`: the paths of files that really are on disk.
fn hdrop_paths(obj: &IDataObject) -> Option<Vec<PathBuf>> {
    let fe = formatetc(CF_HDROP.0, -1, TYMED_HGLOBAL);
    let stg = unsafe { obj.GetData(&fe) }.ok()?;
    let mut out = Vec::new();
    unsafe {
        let hdrop = HDROP(stg.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len + 1];
            let wrote = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            if wrote == 0 {
                continue;
            }
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..wrote])));
        }
        DragFinish(hdrop);
    }
    // DragFinish freed the medium, so it must not be released again.
    std::mem::forget(stg);
    Some(out)
}

/// One `FILEDESCRIPTORW` is 592 bytes; `cFileName` is 260 UTF-16 units at offset 72. The struct is
/// declared packed in the Windows headers, so it is read out of the buffer by offset rather than
/// through a reference to it.
const DESCRIPTOR_SIZE: usize = 592;
const CNAME_OFFSET: usize = 72;
const CNAME_BYTES: usize = 260 * 2;

/// Virtual files: `FileGroupDescriptorW` describes them, `FileContents` carries the bytes.
fn virtual_files(obj: &IDataObject) -> Vec<Blob> {
    let f = formats();
    let Some(group) = hglobal(obj, f.file_group, -1) else {
        return Vec::new();
    };
    if group.len() < 4 {
        return Vec::new();
    }
    let names = descriptor_names(&group);
    let mut out = Vec::new();
    for (i, name) in names.into_iter().enumerate() {
        let bytes = blob_of(obj, f.file_contents, i as i32).unwrap_or_default();
        if bytes.is_empty() {
            continue;
        }
        out.push(Blob { name, bytes });
    }
    out
}

/// The names in a `FileGroupDescriptorW` block: a `u32` count, then one 592-byte descriptor each.
///
/// The count is bounded by the bytes actually received — a source can claim any number, and a
/// claimed count larger than the buffer would otherwise walk off the end.
fn descriptor_names(group: &[u8]) -> Vec<String> {
    if group.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_ne_bytes([group[0], group[1], group[2], group[3]]) as usize;
    let count = count.min(group.len().saturating_sub(4) / DESCRIPTOR_SIZE);
    (0..count)
        .filter_map(|i| {
            let at = 4 + i * DESCRIPTOR_SIZE;
            group.get(at..at + DESCRIPTOR_SIZE).map(descriptor_name)
        })
        .collect()
}

fn descriptor_name(descriptor: &[u8]) -> String {
    let raw = descriptor
        .get(CNAME_OFFSET..CNAME_OFFSET + CNAME_BYTES)
        .unwrap_or_default();
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_ne_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|c| *c == 0).unwrap_or(units.len());
    let name = String::from_utf16_lossy(&units[..end]);
    if name.trim().is_empty() {
        "dropped".to_string()
    } else {
        name
    }
}

/// Bytes for one format, preferring a stream (what browsers use for `FileContents`) and falling
/// back to a global memory block.
fn blob_of(obj: &IDataObject, cf: u16, index: i32) -> Option<Vec<u8>> {
    stream(obj, cf, index).or_else(|| hglobal(obj, cf, index))
}

fn stream(obj: &IDataObject, cf: u16, index: i32) -> Option<Vec<u8>> {
    let fe = formatetc(cf, index, TYMED_ISTREAM);
    let mut stg = unsafe { obj.GetData(&fe) }.ok()?;
    let mut out = Vec::new();
    unsafe {
        let Some(stream) = stg.u.pstm.as_ref() else {
            return None;
        };
        let mut buf = [0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            let ok = stream
                .Read(buf.as_mut_ptr() as *mut c_void, buf.len() as u32, Some(&mut read))
                .is_ok();
            if !ok || read == 0 {
                break;
            }
            out.extend_from_slice(&buf[..read as usize]);
        }
        // Refuse to grow without bound: a 4 GB stream is not a note attachment.
        if out.len() > 64 * 1024 * 1024 {
            out.truncate(64 * 1024 * 1024);
        }
    }
    release(&mut stg);
    Some(out)
}

fn hglobal(obj: &IDataObject, cf: u16, index: i32) -> Option<Vec<u8>> {
    if cf == 0 {
        return None;
    }
    let fe = formatetc(cf, index, TYMED_HGLOBAL);
    let mut stg = unsafe { obj.GetData(&fe) }.ok()?;
    let bytes = unsafe {
        let h: HGLOBAL = stg.u.hGlobal;
        let len = GlobalSize(h);
        let p = GlobalLock(h) as *const u8;
        if p.is_null() || len == 0 {
            release(&mut stg);
            return None;
        }
        let v = std::slice::from_raw_parts(p, len).to_vec();
        let _ = GlobalUnlock(h);
        v
    };
    release(&mut stg);
    Some(bytes)
}

fn release(stg: &mut STGMEDIUM) {
    unsafe {
        windows::Win32::System::Ole::ReleaseStgMedium(stg as *mut STGMEDIUM);
    }
    // ReleaseStgMedium zeroes what it released; make sure Drop of the wrapper does not repeat.
    stg.tymed = 0;
    stg.pUnkForRelease = std::mem::ManuallyDrop::new(None);
}

/// Take the window's drop target away from the toolkit and register ours.
///
/// OLE allows one target per HWND, so winit's is revoked first. Returns false (and leaves the
/// toolkit's own handling alone as far as it can) when OLE refuses.
#[cfg(test)]
fn descriptor_block(names: &[&str]) -> Vec<u8> {
    let mut out = (names.len() as u32).to_ne_bytes().to_vec();
    for name in names {
        let mut d = vec![0u8; DESCRIPTOR_SIZE];
        let units: Vec<u16> = name.encode_utf16().take(259).collect();
        for (i, u) in units.iter().enumerate() {
            d[CNAME_OFFSET + i * 2] = (u & 0xff) as u8;
            d[CNAME_OFFSET + i * 2 + 1] = (u >> 8) as u8;
        }
        out.extend_from_slice(&d);
    }
    out
}

pub fn install(hwnd: isize, tx: Sender<Dropped>) -> bool {
    unsafe {
        // S_FALSE means "already initialised on this thread", which is fine.
        let _ = OleInitialize(None);
        let hwnd = HWND(hwnd as *mut c_void);
        let _ = RevokeDragDrop(hwnd);
        let target: IDropTarget = Target { tx }.into();
        // The window keeps its own reference for as long as it exists; ours is dropped here.
        RegisterDragDrop(hwnd, &target).is_ok()
    }
}

/// The names a source offered, for the log. Cheap enough to call on a failed drop only.
pub fn describe(obj: Option<&IDataObject>) -> String {
    let Some(obj) = obj else {
        return "no data object".into();
    };
    let mut names = Vec::new();
    unsafe {
        if let Ok(enumfmt) = obj.EnumFormatEtc(DATADIR_GET.0 as u32) {
            loop {
                let mut fe = [FORMATETC::default()];
                let mut got = 0u32;
                if enumfmt.Next(&mut fe, Some(&mut got)).is_err() || got == 0 {
                    break;
                }
                names.push(format!("cf#{}", fe[0].cfFormat));
                if names.len() >= 16 {
                    break;
                }
            }
        }
    }
    if names.is_empty() {
        "no formats".into()
    } else {
        names.join(", ")
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_block_yields_one_name_per_file() {
        let block = descriptor_block(&["image.png", "photo.jpg"]);
        assert_eq!(descriptor_names(&block), vec!["image.png", "photo.jpg"]);
    }

    #[test]
    fn a_claimed_count_larger_than_the_buffer_does_not_read_past_it() {
        // A hostile or buggy source can say 1000 files and send one descriptor.
        let mut block = 1000u32.to_ne_bytes().to_vec();
        block.extend(descriptor_block(&["only-one.png"])[4..].to_vec());
        let names = descriptor_names(&block);
        assert_eq!(names.len(), 1, "{names:?}");
        assert_eq!(names[0], "only-one.png");
    }

    #[test]
    fn a_truncated_descriptor_is_skipped_rather_than_guessed() {
        let mut block = 2u32.to_ne_bytes().to_vec();
        block.extend(descriptor_block(&["complete.png"])[4..].to_vec());
        block.extend_from_slice(&[0u8; 40]); // second descriptor cut short
        assert_eq!(descriptor_names(&block), vec!["complete.png"]);
        assert!(descriptor_names(&[]).is_empty());
    }

    #[test]
    fn an_unnamed_descriptor_still_produces_a_usable_name() {
        let mut block = 1u32.to_ne_bytes().to_vec();
        block.extend_from_slice(&vec![0u8; DESCRIPTOR_SIZE]);
        assert_eq!(descriptor_names(&block), vec!["dropped"]);
    }
}
