//! FFmpeg and ffprobe, packed inside finnCrop.exe.
//!
//! Windows cannot run a program out of a resource, so the two binaries are
//! written to `%LOCALAPPDATA%\finnCrop\ffmpeg\<version>\` the first time they
//! are needed and used from there afterwards. That is the whole of "installing":
//! nothing is registered, and deleting that folder only makes finnCrop write it
//! again on the next start.
//!
//! A debug build does not carry the two 100 MB blobs - they would be relinked
//! on every change - and reads the copies in `vendor\` instead. That is also
//! what the tests use.

use crate::ffmpeg::{Bins, Res};
use std::path::PathBuf;
#[cfg(not(debug_assertions))]
use std::path::Path;

#[cfg(windows)]
const FFMPEG_NAME: &str = "ffmpeg.exe";
#[cfg(windows)]
const FFPROBE_NAME: &str = "ffprobe.exe";
#[cfg(not(windows))]
const FFMPEG_NAME: &str = "ffmpeg";
#[cfg(not(windows))]
const FFPROBE_NAME: &str = "ffprobe";

#[cfg(not(debug_assertions))]
const FFMPEG: &[u8] = include_bytes!("../vendor/ffmpeg.exe");
#[cfg(not(debug_assertions))]
const FFPROBE: &[u8] = include_bytes!("../vendor/ffprobe.exe");
/// The GPL text, unpacked beside the two programs: FFmpeg is not Apache-licensed
/// and the licence has to travel with the binary.
///
/// It is read from `third-party/`, which is in the repository, and not from
/// `vendor/`, which is a download folder the build fills and git ignores.  A
/// clone has to be able to compile this line without fetching anything first.
#[cfg(not(debug_assertions))]
const FFMPEG_LICENSE: &[u8] = include_bytes!("../third-party/GPL-3.0.txt");

/// The two programs this executable carries.
pub fn bins() -> Res<Bins> {
    #[cfg(debug_assertions)]
    {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor");
        let ffmpeg = dir.join(FFMPEG_NAME);
        let ffprobe = dir.join(FFPROBE_NAME);
        if ffmpeg.is_file() && ffprobe.is_file() {
            Ok(Bins { ffmpeg, ffprobe })
        } else {
            Err(format!(
                "The development copies in {} are missing; run build.ps1 once to fetch them.",
                dir.display()
            ))
        }
    }
    #[cfg(not(debug_assertions))]
    {
        unpack_into(&unpack_root())
    }
}

/// One folder per version, so an update never has to overwrite a file that a
/// running ffmpeg may still hold open.
#[cfg(not(debug_assertions))]
fn unpack_root() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    base.join("finnCrop").join("ffmpeg").join(env!("FINN_CROP_VERSION"))
}

/// Writes both binaries into `dir` unless they are already there, and returns
/// their paths.
#[cfg(not(debug_assertions))]
pub fn unpack_into(dir: &Path) -> Res<Bins> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let ffmpeg = write_once(&dir.join(FFMPEG_NAME), FFMPEG)?;
    let ffprobe = write_once(&dir.join(FFPROBE_NAME), FFPROBE)?;
    write_once(&dir.join("FFmpeg-LICENSE.txt"), FFMPEG_LICENSE)?;
    Ok(Bins { ffmpeg, ffprobe })
}

/// Writes `data` to `dest` unless a file of exactly that size is already there.
#[cfg(not(debug_assertions))]
fn write_once(dest: &Path, data: &[u8]) -> Res<PathBuf> {
    if has_size(dest, data.len()) {
        return Ok(dest.to_path_buf());
    }

    let tmp = dest.with_extension("tmp");
    std::fs::write(&tmp, data).map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    match std::fs::rename(&tmp, dest) {
        Ok(()) => Ok(dest.to_path_buf()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            // Either another copy of finnCrop wrote the file a moment ago, or a
            // running ffmpeg is holding it open. A file of the right size is
            // what we came for, whether we wrote it or not.
            if has_size(dest, data.len()) {
                Ok(dest.to_path_buf())
            } else {
                Err(format!("Could not place {}: {e}", dest.display()))
            }
        }
    }
}

#[cfg(not(debug_assertions))]
fn has_size(path: &Path, len: usize) -> bool {
    std::fs::metadata(path).map(|m| m.len() == len as u64).unwrap_or(false)
}
