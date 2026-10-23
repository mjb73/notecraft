use std::io;
use std::os::raw::*;
use std::path::{Path, PathBuf};
use std::str::Utf8Error;
use std::sync::Arc;

use percent_encoding::percent_decode;
use x11rb::protocol::xproto::{self, ConnectionExt};

use super::atoms::AtomName::None as DndNone;
use super::atoms::*;
use super::{util, CookieResultExt, X11Error, XConnection};

#[derive(Debug, Clone, Copy)]
pub enum DndState {
    Accepted,
    Rejected,
}

#[derive(Debug)]
pub enum DndDataParseError {
    EmptyData,
    InvalidUtf8(#[allow(dead_code)] Utf8Error),
    HostnameSpecified(#[allow(dead_code)] String),
    UnexpectedProtocol(#[allow(dead_code)] String),
    UnresolvablePath(#[allow(dead_code)] io::Error),
}

impl From<Utf8Error> for DndDataParseError {
    fn from(e: Utf8Error) -> Self {
        DndDataParseError::InvalidUtf8(e)
    }
}

impl From<io::Error> for DndDataParseError {
    fn from(e: io::Error) -> Self {
        DndDataParseError::UnresolvablePath(e)
    }
}

pub struct Dnd {
    xconn: Arc<XConnection>,
    // Populated by XdndEnter event handler
    pub version: Option<c_long>,
    pub type_list: Option<Vec<xproto::Atom>>,
    // Populated by XdndPosition event handler
    pub source_window: Option<xproto::Window>,
    // NOTE(notecraft patch): the payload kind chosen for this drag (text/uri-list
    // or an image MIME), with the file extension to save image bytes under.
    pub target: Option<(xproto::Atom, String)>,
    // NOTE(notecraft patch): the offered type names, resolved from the server, so
    // a drag can be matched on its real MIME string (browsers send types we do not
    // hardcode, e.g. image/avif or text/plain;charset=utf-8).
    pub type_names: Vec<String>,
    // Populated by SelectionNotify event handler (triggered by XdndPosition event handler)
    pub result: Option<Result<Vec<PathBuf>, DndDataParseError>>,
}

/// NOTE(notecraft patch): is this offered type a file path list? Accepts MIME
/// parameters and any letter case, because browsers do not always send the bare
/// `text/uri-list`.
pub fn is_uri_list(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "text/uri-list" || lower.starts_with("text/uri-list;")
}

/// NOTE(notecraft patch): is this a text payload? Chromium-based browsers drag an
/// image with `text/html` (containing the `<img src>`) plus `text/plain`, and no
/// uri-list or image bytes when the image is cross-origin.
pub fn is_text_payload(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("text/html") || lower.starts_with("text/plain")
}

/// NOTE(notecraft patch): human-readable name for the types we know, for logging
/// what a drag actually offered (this is what makes browser drags diagnosable).
pub fn atom_name(atoms: &Atoms, atom: xproto::Atom) -> &'static str {
    if atom == atoms[TextUriList] {
        "text/uri-list"
    } else if atom == atoms[ImagePng] {
        "image/png"
    } else if atom == atoms[ImageJpeg] {
        "image/jpeg"
    } else if atom == atoms[ImageJpg] {
        "image/jpg"
    } else if atom == atoms[ImageGif] {
        "image/gif"
    } else if atom == atoms[ImageWebp] {
        "image/webp"
    } else if atom == atoms[ImageBmp] {
        "image/bmp"
    } else {
        "other"
    }
}

impl Dnd {
    pub fn new(xconn: Arc<XConnection>) -> Result<Self, X11Error> {
        Ok(Dnd {
            xconn,
            version: None,
            type_list: None,
            source_window: None,
            target: None,
            type_names: Vec::new(),
            result: None,
        })
    }

    pub fn reset(&mut self) {
        self.version = None;
        self.type_list = None;
        self.source_window = None;
        self.target = None;
        self.type_names.clear();
        self.result = None;
    }

    /// NOTE(notecraft patch): the server's name for an atom, so drags can be
    /// matched on their real MIME string instead of a fixed list.
    pub fn resolve_atom(&self, atom: xproto::Atom) -> String {
        self.xconn
            .xcb_connection()
            .get_atom_name(atom)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .map(|reply| String::from_utf8_lossy(&reply.name).to_string())
            .unwrap_or_else(|| format!("atom#{atom}"))
    }

    /// NOTE(notecraft patch): extension to save an image payload under, from a
    /// MIME type such as `image/png`, `image/jpeg;q=1` or `image/avif`.
    pub fn image_ext_for(name: &str) -> Option<String> {
        let lower = name.to_ascii_lowercase();
        if !lower.starts_with("image/") {
            return None;
        }
        let sub = lower
            .split(';')
            .next()
            .unwrap_or(&lower)
            .split('/')
            .nth(1)
            .unwrap_or("png");
        let sub = sub.trim().trim_start_matches("x-");
        let ext: String = sub
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let ext = match ext.as_str() {
            "jpeg" => "jpg".to_string(),
            "svg+xml" | "" => "png".to_string(),
            other => other.to_string(),
        };
        Some(ext)
    }

    pub unsafe fn send_status(
        &self,
        this_window: xproto::Window,
        target_window: xproto::Window,
        state: DndState,
    ) -> Result<(), X11Error> {
        let atoms = self.xconn.atoms();
        let (accepted, action) = match state {
            DndState::Accepted => (1, atoms[XdndActionPrivate]),
            DndState::Rejected => (0, atoms[DndNone]),
        };
        self.xconn
            .send_client_msg(target_window, target_window, atoms[XdndStatus] as _, None, [
                this_window,
                accepted,
                0,
                0,
                action as _,
            ])?
            .ignore_error();

        Ok(())
    }

    pub unsafe fn send_finished(
        &self,
        this_window: xproto::Window,
        target_window: xproto::Window,
        state: DndState,
    ) -> Result<(), X11Error> {
        let atoms = self.xconn.atoms();
        let (accepted, action) = match state {
            DndState::Accepted => (1, atoms[XdndActionPrivate]),
            DndState::Rejected => (0, atoms[DndNone]),
        };
        self.xconn
            .send_client_msg(target_window, target_window, atoms[XdndFinished] as _, None, [
                this_window,
                accepted,
                action as _,
                0,
                0,
            ])?
            .ignore_error();

        Ok(())
    }

    pub unsafe fn get_type_list(
        &self,
        source_window: xproto::Window,
    ) -> Result<Vec<xproto::Atom>, util::GetPropertyError> {
        let atoms = self.xconn.atoms();
        self.xconn.get_property(
            source_window,
            atoms[XdndTypeList],
            xproto::Atom::from(xproto::AtomEnum::ATOM),
        )
    }

    pub unsafe fn convert_selection(&self, window: xproto::Window, time: xproto::Timestamp, target: xproto::Atom) {
        let atoms = self.xconn.atoms();
        self.xconn
            .xcb_connection()
            .convert_selection(
                window,
                atoms[XdndSelection],
                target,
                atoms[XdndSelection],
                time,
            )
            .expect_then_ignore_error("Failed to send XdndSelection event")
    }

    pub unsafe fn read_data(
        &self,
        window: xproto::Window,
        target: xproto::Atom,
    ) -> Result<Vec<c_uchar>, util::GetPropertyError> {
        let atoms = self.xconn.atoms();
        self.xconn.get_property(window, atoms[XdndSelection], target)
    }

    /// NOTE(notecraft patch): write a dropped image payload to a temp file so the
    /// application receives a path it can save as an attachment.
    pub fn save_bytes(&self, data: &[c_uchar], ext: &str) -> Option<PathBuf> {
        if data.is_empty() {
            return None;
        }
        let dir = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = dir.join(format!("notecraft-drop-{stamp}.{ext}"));
        std::fs::write(&path, data).ok()?;
        Some(path)
    }

    /// NOTE(notecraft patch): a browser puts an http(s) URL in text/uri-list.
    /// Hand it to the application as a `.url` file instead of discarding it.
    pub fn save_url(&self, data: &mut [c_uchar]) -> Result<Vec<PathBuf>, DndDataParseError> {
        let text = percent_decode(data).decode_utf8()?.into_owned();
        let url = text
            .split("\r\n")
            .map(|u| u.trim())
            .find(|u| u.starts_with("http://") || u.starts_with("https://"))
            .ok_or(DndDataParseError::EmptyData)?;
        let path = self
            .save_bytes(url.as_bytes(), "url")
            .ok_or(DndDataParseError::EmptyData)?;
        Ok(vec![path])
    }

    pub fn parse_data(&self, data: &mut [c_uchar]) -> Result<Vec<PathBuf>, DndDataParseError> {
        if !data.is_empty() {
            let mut path_list = Vec::new();
            let decoded = percent_decode(data).decode_utf8()?.into_owned();
            for uri in decoded.split("\r\n").filter(|u| !u.is_empty()) {
                // The format is specified as protocol://host/path
                // However, it's typically simply protocol:///path
                let path_str = if uri.starts_with("file://") {
                    let path_str = uri.replace("file://", "");
                    if !path_str.starts_with('/') {
                        // A hostname is specified
                        // Supporting this case is beyond the scope of my mental health
                        return Err(DndDataParseError::HostnameSpecified(path_str));
                    }
                    path_str
                } else {
                    // Only the file protocol is supported
                    return Err(DndDataParseError::UnexpectedProtocol(uri.to_owned()));
                };

                let path = Path::new(&path_str).canonicalize()?;
                path_list.push(path);
            }
            Ok(path_list)
        } else {
            Err(DndDataParseError::EmptyData)
        }
    }
}
