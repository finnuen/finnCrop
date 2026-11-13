//! Build-time plumbing: the icon and the version block in the executable,
//! plus the version string the program shows.
//!
//! `Cargo.toml` holds the version and nothing else does. The number is
//! `MAJOR.MINOR.PATCH` because Cargo wants three parts; finnCrop publishes two,
//! so the trailing `.0` is trimmed here once and the rest of the program reads
//! `FINN_CROP_VERSION` (`1.5`) or `CARGO_PKG_VERSION` (`1.5.0`).

use std::path::PathBuf;

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let short = version.strip_suffix(".0").unwrap_or(&version).to_string();

    println!("cargo:rustc-env=FINN_CROP_VERSION={short}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/app.ico");
    println!("cargo:rerun-if-changed=src/icon-64.rgba");
    println!("cargo:rerun-if-changed=vendor/ffmpeg.exe");
    println!("cargo:rerun-if-changed=vendor/ffprobe.exe");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if !PathBuf::from("src/app.ico").exists() {
        println!("cargo:warning=src/app.ico is missing; run tools\\make-icon.ps1");
        return;
    }

    // Four numbers, straight from the version: 1.5.0 -> 1,5,0,0.
    let mut parts = version.split('.').map(|p| p.parse::<u16>().unwrap_or(0));
    let (major, minor, patch) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );

    let mut res = winresource::WindowsResource::new();
    res.set_icon("src/app.ico");
    res.set("FileDescription", "finnCrop - video cropper");
    res.set("ProductName", "finnCrop");
    res.set("OriginalFilename", "finnCrop.exe");
    res.set("InternalName", "finnCrop");
    res.set("LegalCopyright", "Copyright 2026 finnuen. Apache License 2.0.");
    res.set("Comments", "Lossless cutting and cropping for video files.");
    let packed = ((major as u64) << 48) | ((minor as u64) << 32) | ((patch as u64) << 16);
    res.set_version_info(winresource::VersionInfo::FILEVERSION, packed);
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, packed);
    // The strings are what Explorer's Details tab shows; the numeric fields
    // above are what other programs compare.
    res.set("FileVersion", &short);
    res.set("ProductVersion", &short);

    if let Err(e) = res.compile() {
        // A missing resource compiler must not cost the whole build: the app
        // still runs without its icon and version block.
        println!("cargo:warning=could not compile the Windows resources: {e}");
    }
}
