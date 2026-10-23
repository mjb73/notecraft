//! What a drop can hand the app, in a form that does not depend on which platform delivered it.
//!
//! Only Windows produces `Blob` today (see `windows_dnd`), but the type lives here so the code that
//! turns bytes into an attachment is compiled and tested on every platform, not only on Windows.

// Only the Windows drop target builds one; the attachment code that consumes it is compiled
// everywhere so it can be tested everywhere.
#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Debug, Clone)]
pub struct Blob {
    /// Name the source suggested, e.g. `image.png`. May have no extension at all.
    pub name: String,
    pub bytes: Vec<u8>,
}
