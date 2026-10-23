//! Windows only: stamp the icon, the version resource and the DPI-awareness manifest into the exe.
//!
//! Kept best-effort on purpose. Building for Windows *from* Linux (a `cargo check
//! --target x86_64-pc-windows-gnu` sanity pass, say) has no resource compiler, and that must not
//! turn into a build failure — the icons are cosmetic, the app is not. On a real Windows machine
//! (and in CI) `winresource` finds `rc.exe` and this succeeds.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=packaging/windows/notecraft.manifest");
    println!("cargo:rerun-if-changed=packaging/windows/notecraft.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var_os("NOTECRAFT_SKIP_WINRES").is_some() {
        return;
    }
    if let Err(e) = windows_resources() {
        // A warning, not a failure: the binary is complete without a resource section.
        println!("cargo:warning=could not embed the Windows icon/version resource: {e}");
    }
}

fn windows_resources() -> Result<(), Box<dyn std::error::Error>> {
    let mut res = winresource::WindowsResource::new();
    let icon = std::path::Path::new("packaging/windows/notecraft.ico");
    if icon.is_file() {
        res.set_icon(&icon.display().to_string());
    }
    let manifest = std::path::Path::new("packaging/windows/notecraft.manifest");
    if manifest.is_file() {
        res.set_manifest_file(&manifest.display().to_string());
    }
    res.set("ProductName", "NoteCraft");
    res.set("FileDescription", "NoteCraft — notes, todos, ideas");
    res.set("CompanyName", "Michael Bayes");
    res.set("LegalCopyright", "Copyright (c) 2026 Michael Bayes — MIT licence");
    res.set("OriginalFilename", "notecraft.exe");
    res.compile()?;
    Ok(())
}
