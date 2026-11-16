//! The choices that outlive a session.
//!
//! Kept in `%LOCALAPPDATA%\finnCrop\settings.txt` as `key=value` lines, one per
//! setting, so a later version can add a line without disturbing the ones
//! already in the file. A file that is missing, unreadable, or written by a
//! newer version is not an error: the lines this version understands are used
//! and everything else falls back to the defaults.

use std::path::PathBuf;

/// Everything remembered between runs. Everything else in the window belongs
/// to the file that is open, and starts fresh with the next one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    /// The *Sound* switch. Off starts the next run silent; what is saved is
    /// never affected either way.
    pub sound_on: bool,
}

impl Default for Settings {
    fn default() -> Self {
        // On, because a video that is played should sound like one.
        Self { sound_on: true }
    }
}

impl Settings {
    /// What this machine has, or the defaults when nothing is saved yet.
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(_) => Self::default(),
        }
    }

    /// Writes them back. Failing to save must not get in the way of using the
    /// program - a read-only profile is no reason to refuse to play a video -
    /// so errors are dropped here.
    pub fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, self.text());
    }

    fn text(&self) -> String {
        format!("sound={}\n", if self.sound_on { "on" } else { "off" })
    }

    /// Reads what [`Settings::text`] writes, ignoring comments, blank lines,
    /// and keys this version does not know.
    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() == "sound" {
                s.sound_on = !matches!(value.trim(), "off" | "false" | "0");
            }
        }
        s
    }
}

/// `%LOCALAPPDATA%\finnCrop\settings.txt`, in the folder the unpacked FFmpeg
/// already lives under.
fn path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .map(|p| p.join("finnCrop").join("settings.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sound_switch_survives_a_round_trip() {
        for sound_on in [true, false] {
            let s = Settings { sound_on };
            assert_eq!(Settings::parse(&s.text()), s, "sound_on {sound_on}");
        }
    }

    #[test]
    fn a_missing_or_odd_file_falls_back_to_the_defaults() {
        assert!(Settings::default().sound_on, "a fresh install plays sound");
        assert_eq!(Settings::parse(""), Settings::default());
        // Junk, a comment, and a key from a newer version: none of them is an
        // error, and none of them changes what this version understands.
        assert_eq!(Settings::parse("nonsense\nfuture=1\n"), Settings::default());
        assert_eq!(Settings::parse("# sound=off\n"), Settings::default());
        assert!(!Settings::parse("sound = false\n").sound_on);
        assert!(!Settings::parse("sound=0\n").sound_on);
        assert!(Settings::parse("sound=on\n").sound_on);
    }
}
