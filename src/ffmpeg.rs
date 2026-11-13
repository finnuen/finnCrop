//! FFmpeg / ffprobe integration. No GUI types in here.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub type Res<T> = Result<T, String>;
pub const CANCELLED: &str = "Cancelled";

#[derive(Clone, Debug)]
pub struct Bins {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

fn bin_names() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("ffmpeg.exe", "ffprobe.exe")
    } else {
        ("ffmpeg", "ffprobe")
    }
}

/// Where the two programs come from, in order of preference:
///
///   1. the copies packed into this executable (unpacked on first use),
///   2. files sitting next to the executable,
///   3. ffmpeg and ffprobe on PATH.
pub fn find_bins() -> Res<Bins> {
    let (n1, n2) = bin_names();
    let mut why: Vec<String> = Vec::new();

    match crate::bundled::bins() {
        Ok(bins) => return Ok(bins),
        Err(e) => why.push(e),
    }

    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()));
    let find = |name: &str| -> Option<PathBuf> {
        if let Some(d) = &exe_dir {
            let p = d.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
        let paths = std::env::var_os("PATH")?;
        std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file())
    };
    match (find(n1), find(n2)) {
        (Some(ffmpeg), Some(ffprobe)) => Ok(Bins { ffmpeg, ffprobe }),
        _ => {
            why.push(format!("Also no {n1} and {n2} next to this program or on PATH."));
            Err(why.join("\n\n"))
        }
    }
}

fn command(bin: &Path) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(bin);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

#[derive(Clone, Debug)]
pub struct VideoInfo {
    pub path: PathBuf,
    /// Size as displayed (rotation already applied).
    pub width: u32,
    pub height: u32,
    pub duration: f64,
    #[allow(dead_code)]
    pub fps: f64,
    pub v_codec: String,
    pub a_codec: Option<String>,
    pub ext: String,
}

fn parse_f64(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        serde_json::Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn parse_rate(s: &str) -> Option<f64> {
    let mut it = s.split('/');
    let a: f64 = it.next()?.trim().parse().ok()?;
    let b: f64 = it.next().map(|x| x.trim().parse().ok()).unwrap_or(Some(1.0))?;
    if b == 0.0 {
        None
    } else {
        Some(a / b)
    }
}

pub fn probe(bins: &Bins, path: &Path) -> Res<VideoInfo> {
    let out = command(&bins.ffprobe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffprobe could not read this file:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    let streams = v["streams"].as_array().ok_or("No streams found in this file")?;
    let vs = streams
        .iter()
        .find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64().unwrap_or(0) == 0)
        .ok_or("This file has no video stream")?;
    let mut width = vs["width"].as_u64().ok_or("Video has no width")? as u32;
    let mut height = vs["height"].as_u64().ok_or("Video has no height")? as u32;

    // Rotation: display matrix side data (new) or "rotate" tag (old).
    let mut rot: Option<f64> = vs["tags"]["rotate"].as_str().and_then(|s| s.parse().ok());
    if let Some(list) = vs["side_data_list"].as_array() {
        for sd in list {
            if let Some(r) = parse_f64(&sd["rotation"]) {
                rot = Some(r);
            }
        }
    }
    if let Some(r) = rot {
        let r = (r.abs().round() as i64) % 360;
        if r == 90 || r == 270 {
            std::mem::swap(&mut width, &mut height);
        }
    }

    let duration = parse_f64(&vs["duration"])
        .filter(|d| *d > 0.0)
        .or_else(|| parse_f64(&v["format"]["duration"]))
        .filter(|d| *d > 0.0)
        .ok_or("Could not determine the video duration")?;
    let fps = vs["avg_frame_rate"]
        .as_str()
        .and_then(parse_rate)
        .filter(|f| *f > 0.0)
        .or_else(|| vs["r_frame_rate"].as_str().and_then(parse_rate))
        .filter(|f| *f > 0.0)
        .unwrap_or(30.0);
    let v_codec = vs["codec_name"].as_str().unwrap_or("unknown").to_string();
    let a_codec = streams
        .iter()
        .find(|s| s["codec_type"] == "audio")
        .and_then(|s| s["codec_name"].as_str())
        .map(|s| s.to_string());
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "mkv".to_string());

    Ok(VideoInfo { path: path.to_path_buf(), width, height, duration, fps, v_codec, a_codec, ext })
}

pub fn fmt_secs(t: f64) -> String {
    format!("{:.3}", t.max(0.0))
}

/// Size of the preview frame (keeps aspect, max 1280 on the long side).
pub fn preview_size(w: u32, h: u32) -> (usize, usize) {
    let m = w.max(h).max(1) as f64;
    let s = (1280.0 / m).min(1.0);
    (((w as f64 * s).round() as usize).max(1), ((h as f64 * s).round() as usize).max(1))
}

/// One RGBA frame at time `t`, scaled for preview. FFmpeg auto-rotates, so it matches `VideoInfo` size.
pub fn grab_frame(bins: &Bins, path: &Path, t: f64, w: u32, h: u32) -> Res<(usize, usize, Vec<u8>)> {
    let (pw, ph) = preview_size(w, h);
    let out = command(&bins.ffmpeg)
        .args(["-v", "error", "-ss"])
        .arg(fmt_secs(t))
        .arg("-i")
        .arg(path)
        .args(["-frames:v", "1", "-an", "-sn", "-vf"])
        .arg(format!("scale={pw}:{ph}"))
        .args(["-pix_fmt", "rgba", "-f", "rawvideo", "-"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {e}"))?;
    let need = pw * ph * 4;
    if out.stdout.len() < need {
        return Err("Could not read a frame at this position".to_string());
    }
    let mut data = out.stdout;
    data.truncate(need);
    Ok((pw, ph, data))
}

/// Real-time playback of a slice of the video. Silent: there is no audio sink,
/// and previewing the picture is what this is for.
///
/// One ffmpeg process decodes `from`..`until` and writes raw RGBA frames to a
/// pipe, paced to the video's own frame rate by `-re`. A thread keeps only the
/// newest frame, so the window never waits on the decoder: frames that arrive
/// while the UI is busy are dropped, which is what playback should do anyway.
///
/// The counter of decoded frames is what tells the app where the picture is.
/// Counting *shown* frames instead would let the playhead fall behind the
/// picture whenever a frame is dropped, and playback would then run past the
/// end of the trim.
pub struct Player {
    frame: Arc<Mutex<Option<(usize, usize, Vec<u8>)>>>,
    decoded: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    child: Arc<Mutex<Option<std::process::Child>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Player {
    /// Starts decoding at `from` seconds and stops at `until`. `on_frame` is
    /// called after every frame (and once at the end) so the app can repaint.
    pub fn start(
        bins: &Bins,
        path: &Path,
        from: f64,
        until: f64,
        w: u32,
        h: u32,
        on_frame: Arc<dyn Fn() + Send + Sync>,
    ) -> Res<Self> {        let (pw, ph) = preview_size(w, h);
        let mut child = command(&bins.ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-re", "-ss"])
            .arg(fmt_secs(from))
            .arg("-i")
            .arg(path)
            .args(["-t"])
            .arg(fmt_secs((until - from).max(0.05)))
            .args(["-an", "-sn", "-vf"])
            .arg(format!("scale={pw}:{ph}"))
            .args(["-pix_fmt", "rgba", "-f", "rawvideo", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Could not start playback: {e}"))?;
        let stdout = child.stdout.take().ok_or("ffmpeg gave no picture to read")?;

        let frame = Arc::new(Mutex::new(None));
        let decoded = Arc::new(AtomicU64::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));

        let thread = {
            let (frame, decoded, cancel, done) =
                (frame.clone(), decoded.clone(), cancel.clone(), done.clone());
            std::thread::spawn(move || {
                let need = pw * ph * 4;
                let mut buf = vec![0u8; need];
                let mut reader = BufReader::with_capacity(need, stdout);
                while !cancel.load(Ordering::Relaxed) {
                    if reader.read_exact(&mut buf).is_err() {
                        break; // end of the slice, or ffmpeg was killed
                    }
                    decoded.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut slot) = frame.lock() {
                        *slot = Some((pw, ph, buf.clone()));
                    }
                    on_frame();
                }
                done.store(true, Ordering::Relaxed);
                on_frame();
            })
        };

        Ok(Self {
            frame,
            decoded,
            cancel,
            done,
            child: Arc::new(Mutex::new(Some(child))),
            thread: Some(thread),
        })
    }

    /// The newest frame, if one has arrived since the last call.
    pub fn take_frame(&self) -> Option<(usize, usize, Vec<u8>)> {
        self.frame.lock().ok().and_then(|mut slot| slot.take())
    }

    /// How many frames have been decoded since playback started. The last one
    /// is the one on screen, so this is the playhead in frames.
    pub fn decoded(&self) -> u64 {
        self.decoded.load(Ordering::Relaxed)
    }

    /// True once the decoder has reached the end of the slice.
    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    /// Stops decoding. Safe to call more than once; also runs on drop.
    ///
    /// The decoder has to be killed before the reader thread is joined: that
    /// thread is blocked on the pipe, and only a dead ffmpeg closes it.
    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Ok(mut slot) = self.child.lock() {
            if let Some(mut c) = slot.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Times (seconds) of all video keyframes. Reads packet headers only, so it is fast.
pub fn keyframes(bins: &Bins, path: &Path) -> Res<Vec<f64>> {
    let out = command(&bins.ffprobe)
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "packet=pts_time,flags", "-of", "csv=p=0"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err("Could not list keyframes".to_string());
    }
    let mut v: Vec<f64> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut t = None;
            let mut key = false;
            for part in line.split(',') {
                if part.contains('K') {
                    key = true;
                } else if let Ok(x) = part.trim().parse::<f64>() {
                    t = Some(x);
                }
            }
            if key {
                t
            } else {
                None
            }
        })
        .collect();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    Ok(v)
}

/// Is stream copy into this container likely to work? Returns a message if not.
/// Audio that the container allows but common players on Windows do not:
/// Opus and Vorbis inside .mp4/.mov. Copying them yields a file that finnCrop
/// plays and the system player may open to a still frame, so the audio is
/// converted to AAC instead.
pub fn audio_needs_aac(info: &VideoInfo, out_ext: &str) -> bool {
    let mov_family = matches!(out_ext, "mp4" | "m4v" | "mov");
    mov_family && matches!(info.a_codec.as_deref(), Some("opus") | Some("vorbis"))
}

pub fn copy_compat(info: &VideoInfo, out_ext: &str) -> Option<String> {
    let mov_family = matches!(out_ext, "mp4" | "m4v" | "mov");
    if mov_family {
        let v_ok = matches!(
            info.v_codec.as_str(),
            "h264" | "hevc" | "mpeg4" | "av1" | "vp9" | "mjpeg" | "mpeg2video"
        ) || (out_ext == "mov" && info.v_codec == "prores");
        let a_ok = match &info.a_codec {
            None => true,
            // Opus and Vorbis are not copied, they are converted to AAC (see
            // `audio_needs_aac`), so they belong here: a file that only needs
            // its audio converted is one this can still produce.
            Some(a) => {
                matches!(a.as_str(), "aac" | "mp3" | "mp2" | "ac3" | "eac3" | "alac" | "flac" | "opus" | "vorbis")
                    || (out_ext == "mov" && a.starts_with("pcm"))
            }
        };
        if !v_ok || !a_ok {
            return Some(format!(
                "Can't copy {}{} into .{out_ext} without re-encoding. Choose MKV or \"Same as source\".",
                info.v_codec,
                info.a_codec.as_ref().map(|a| format!("/{a}")).unwrap_or_default()
            ));
        }
    } else if out_ext == "webm" {
        let v_ok = matches!(info.v_codec.as_str(), "vp8" | "vp9" | "av1");
        let a_ok = info.a_codec.as_deref().map(|a| matches!(a, "vorbis" | "opus")).unwrap_or(true);
        if !v_ok || !a_ok {
            return Some("These codecs can't be copied into WebM. Choose MKV.".to_string());
        }
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Re-encode the picture, with an optional crop; the audio is copied.
    ///
    /// Every cut goes through here. A stream copy can only begin on a keyframe,
    /// so it cannot start where the trim says unless the trim happens to land
    /// on one; re-encoding can, and cutting where it was asked matters more
    /// than the time a copy would save.
    Video,
    /// Streams copied without seeking. Only for stripping metadata when the
    /// picture is otherwise untouched: no cut, no crop, so there is nothing to
    /// seek to and nothing that can land in the wrong place.
    Remux,
    /// Animated GIF: palette built in one pass, encoded in a second.
    Gif,
}

/// Geometry and frame rate for a GIF export. GIF has no audio and no 24-bit
/// colour, so the file size is decided by these three numbers rather than by a
/// bitrate.
#[derive(Clone, Copy, Debug)]
pub struct GifOpts {
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    /// Palette entries, 2..=256.
    pub colors: u16,
}

impl Default for GifOpts {
    fn default() -> Self {
        Self { fps: 15, width: 640, height: 360, colors: 256 }
    }
}

#[derive(Clone, Debug)]
pub struct Job {
    pub input: PathBuf,
    pub output: PathBuf,
    pub mode: Mode,
    pub start: f64,
    pub dur: f64,
    /// (w, h, x, y) in video pixels.
    pub crop: Option<(u32, u32, u32, u32)>,
    pub keep_metadata: bool,
    pub crf: u32,
    /// Re-encode audio to AAC instead of copying (used as a fallback).
    pub audio_aac: bool,
    /// Set for `Mode::Gif`.
    pub gif: Option<GifOpts>,
}

macro_rules! push_args {
    ($v:ident; $($x:expr),* $(,)?) => { $( $v.push(OsString::from($x)); )* };
}

pub fn build_args(job: &Job) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let ext = job.output.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mov_family = matches!(ext.as_str(), "mp4" | "m4v" | "mov");

    push_args!(a; "-hide_banner", "-loglevel", "error", "-y", "-ss", fmt_secs(job.start), "-i", job.input.clone());
    push_args!(a; "-t", fmt_secs(job.dur), "-map", "0:v:0", "-map", "0:a?");

    match job.mode {
        Mode::Remux => {
            push_args!(a; "-c", "copy");
            if job.audio_aac {
                push_args!(a; "-c:a", "aac", "-b:a", "192k");
            }
            push_args!(a; "-avoid_negative_ts", "make_zero");
        }
        Mode::Gif => unreachable!("GIF exports are built by gif_args()"),
        Mode::Video => {
            if let Some((w, h, x, y)) = job.crop {
                push_args!(a; "-vf", format!("crop={w}:{h}:{x}:{y}"));
            }
            push_args!(a; "-c:v", "libx264", "-crf", job.crf.to_string(), "-preset", "fast", "-pix_fmt", "yuv420p");
            if job.audio_aac {
                push_args!(a; "-c:a", "aac", "-b:a", "192k");
            } else {
                push_args!(a; "-c:a", "copy");
            }
        }
    }

    if job.keep_metadata {
        push_args!(a; "-map_metadata", "0", "-map_chapters", "0");
        if mov_family {
            push_args!(a; "-movflags", "+faststart+use_metadata_tags");
        }
    } else {
        push_args!(a; "-map_metadata", "-1", "-map_chapters", "-1");
        push_args!(a; "-map_metadata:s:v", "-1", "-map_metadata:s:a", "-1");
        push_args!(a; "-fflags", "+bitexact", "-flags:v", "+bitexact", "-flags:a", "+bitexact");
        if job.mode == Mode::Video {
            // FFmpeg 6 still writes an "encoder" stream tag; a blank value suppresses it.
            // Also drop the x264 settings SEI so the bitstream doesn't carry encoder info.
            push_args!(a; "-metadata:s:v:0", "encoder= ", "-bsf:v", "filter_units=remove_types=6");
        }
        if mov_family {
            push_args!(a; "-movflags", "+faststart");
        }
    }

    push_args!(a; "-progress", "pipe:1", "-nostats", job.output.clone());
    a
}

/// The two ffmpeg commands a GIF needs: one to build the palette from the
/// trimmed, cropped and scaled picture, one to encode with it.
///
/// The common one-liner (`split` → `palettegen` and `paletteuse` in a single
/// graph) has to hold every frame in memory until the palette is complete, which
/// on a long clip is gigabytes. Two passes decode twice but keep memory flat,
/// and the palette is built from the whole clip instead of its first second.
pub fn gif_args(job: &Job, palette: &Path) -> (Vec<OsString>, Vec<OsString>) {
    let g = job.gif.unwrap_or_default();
    let mut chain = String::new();
    if let Some((w, h, x, y)) = job.crop {
        chain.push_str(&format!("crop={w}:{h}:{x}:{y},"));
    }
    // fps first: fewer frames means less work and a smaller palette pass.
    chain.push_str(&format!("fps={},scale={}:{}:flags=lanczos,", g.fps, g.width, g.height));

    let mut pass1: Vec<OsString> = Vec::new();
    push_args!(pass1; "-hide_banner", "-loglevel", "error", "-y", "-ss", fmt_secs(job.start), "-i", job.input.clone());
    push_args!(pass1; "-t", fmt_secs(job.dur), "-an", "-vf");
    push_args!(pass1; format!("{chain}palettegen=max_colors={}:stats_mode=diff", g.colors));
    push_args!(pass1; "-frames:v", "1", "-progress", "pipe:1", "-nostats", palette.to_path_buf());

    let mut pass2: Vec<OsString> = Vec::new();
    push_args!(pass2; "-hide_banner", "-loglevel", "error", "-y", "-ss", fmt_secs(job.start), "-i", job.input.clone(), "-i", palette.to_path_buf());
    push_args!(pass2; "-t", fmt_secs(job.dur), "-an", "-lavfi");
    push_args!(pass2; format!("{chain}paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"));
    // -loop 0: an animated GIF that repeats forever.
    push_args!(pass2; "-loop", "0", "-progress", "pipe:1", "-nostats", job.output.clone());
    (pass1, pass2)
}

fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let n = lines.len().saturating_sub(6);
    let t = lines[n..].join("\n");
    if t.is_empty() {
        "ffmpeg failed (no details)".to_string()
    } else {
        t
    }
}

/// Runs one ffmpeg command and reports its `out_time` as a fraction of `dur`.
fn run_ffmpeg(bins: &Bins, args: Vec<OsString>, dur: f64, cancel: &AtomicBool, on_progress: &dyn Fn(f32)) -> Res<()> {
    let mut child = command(&bins.ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start ffmpeg: {e}"))?;

    let mut stderr = child.stderr.take().unwrap();
    let errbuf = Arc::new(Mutex::new(String::new()));
    let eb = errbuf.clone();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        *eb.lock().unwrap() = s;
    });

    let stdout = child.stdout.take().unwrap();
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Some(v) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")) {
            if let Ok(us) = v.trim().parse::<i64>() {
                on_progress((((us as f64) / 1e6) / dur.max(0.001)).clamp(0.0, 1.0) as f32);
            }
        }
    }
    if cancel.load(Ordering::Relaxed) {
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        return Err(CANCELLED.to_string());
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let _ = reader.join();
    if !status.success() {
        let e = errbuf.lock().unwrap().clone();
        return Err(tail(&e));
    }
    on_progress(1.0);
    Ok(())
}

fn run_export(bins: &Bins, job: &Job, cancel: &AtomicBool, on_progress: &dyn Fn(f32)) -> Res<()> {
    match run_ffmpeg(bins, build_args(job), job.dur, cancel, on_progress) {
        Ok(()) => Ok(()),
        Err(e) => {
            // A cancelled or failed encode leaves a half-written file behind.
            let _ = std::fs::remove_file(&job.output);
            Err(e)
        }
    }
}

/// GIF in two passes: the palette takes the first 35 % of the progress bar, the
/// encode the rest.
///
/// The palette is written beside the output as `<name>.gif.palette.png` and
/// removed again: the output folder is the one place we know is writable, and
/// the name makes it obvious what the file is if the program is killed before
/// it can clean up.
fn run_gif(bins: &Bins, job: &Job, cancel: &AtomicBool, on_progress: &dyn Fn(f32)) -> Res<()> {
    let name = job
        .output
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "out.gif".to_string());
    let palette = job.output.with_file_name(format!("{name}.palette.png"));
    let (pass1, pass2) = gif_args(job, &palette);

    let result = match run_ffmpeg(bins, pass1, job.dur, cancel, &|f| on_progress(f * 0.35)) {
        Ok(()) => run_ffmpeg(bins, pass2, job.dur, cancel, &|f| on_progress(0.35 + f * 0.65)),
        Err(e) => Err(e),
    };

    let _ = std::fs::remove_file(&palette);
    if result.is_err() {
        let _ = std::fs::remove_file(&job.output);
    }
    result
}

/// Run the export. If a re-encode fails (often audio codec vs container), retry once with AAC audio.
pub fn export(bins: &Bins, job: &Job, cancel: &AtomicBool, on_progress: &dyn Fn(f32)) -> Res<()> {
    if job.mode == Mode::Gif {
        return run_gif(bins, job, cancel, on_progress);
    }
    match run_export(bins, job, cancel, on_progress) {
        Err(e) if e != CANCELLED && job.mode == Mode::Video && !job.audio_aac => {
            let mut j2 = job.clone();
            j2.audio_aac = true;
            run_export(bins, &j2, cancel, on_progress)
        }
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn bins() -> Bins {
        find_bins().expect("ffmpeg/ffprobe needed for tests")
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join("vc_tests");
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    /// 12 s, 640x360 @30fps, keyframe every 2 s, AAC audio, with identifying metadata.
    ///
    /// Built once per test process and shared: tests run in parallel, and two of
    /// them writing the same file at the same time leaves the loser reading a
    /// half-written MP4.
    fn make_video() -> PathBuf {
        static SRC: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        SRC.get_or_init(|| {
            let p = tmp("src.mp4");
            let b = bins();
            let st = Command::new(&b.ffmpeg)
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30", "-f", "lavfi", "-i", "sine=frequency=440"])
                .args(["-t", "12", "-c:v", "libx264", "-g", "60", "-keyint_min", "60", "-sc_threshold", "0", "-pix_fmt", "yuv420p", "-c:a", "aac"])
                .args(["-metadata", "title=SecretTitle", "-metadata", "comment=SecretComment", "-metadata", "location=+12.3456+065.4321/"])
                .args(["-metadata:s:v:0", "title=StreamSecret", "-metadata:s:v:0", "creation_time=2019-03-04T05:06:07Z"])
                .args(["-movflags", "+use_metadata_tags"])
                .arg(&p)
                .status()
                .unwrap();
            assert!(st.success());
            p
        })
        .clone()
    }

    fn dump(b: &Bins, p: &Path) -> String {
        let o = Command::new(&b.ffprobe)
            .args(["-v", "error", "-show_format", "-show_streams", "-of", "default"])
            .arg(p)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).to_string()
    }

    fn job(input: &Path, out: &Path, mode: Mode, start: f64, dur: f64, crop: Option<(u32, u32, u32, u32)>, keep: bool) -> Job {
        Job {
            input: input.to_path_buf(),
            output: out.to_path_buf(),
            mode,
            start,
            dur,
            crop,
            keep_metadata: keep,
            crf: 23,
            audio_aac: false,
            gif: None,
        }
    }

    /// The scratch palette a GIF export leaves beside its output, if cleanup failed.
    fn palette_for(out: &Path) -> PathBuf {
        let name = out.file_name().unwrap().to_string_lossy();
        out.with_file_name(format!("{name}.palette.png"))
    }

    #[test]
    fn gif_export_has_the_asked_size_rate_and_no_audio() {
        let b = bins();
        let src = make_video();
        let out = tmp("clip.gif");
        let _ = std::fs::remove_file(&out);
        let c = AtomicBool::new(false);
        let mut j = job(&src, &out, Mode::Gif, 3.0, 2.0, Some((400, 300, 100, 50)), true);
        j.gif = Some(GifOpts { fps: 12, width: 320, height: 180, colors: 128 });

        let seen = Mutex::new((0.0f32, 0u32));
        export(&b, &j, &c, &|p| {
            let mut s = seen.lock().unwrap();
            s.0 = p;
            s.1 += 1;
        })
        .unwrap();
        let (last, calls) = *seen.lock().unwrap();
        assert_eq!(last, 1.0);
        assert!(calls > 1, "progress was reported {calls} time(s)");

        let info = probe(&b, &out).unwrap();
        assert_eq!(info.v_codec, "gif");
        assert_eq!((info.width, info.height), (320, 180));
        assert!(info.a_codec.is_none(), "a GIF has no audio: {:?}", info.a_codec);
        assert!((info.duration - 2.0).abs() < 0.2, "duration {}", info.duration);
        assert!((info.fps - 12.0).abs() < 0.6, "fps {}", info.fps);
        assert!(std::fs::metadata(&out).unwrap().len() > 1000, "the GIF is suspiciously small");
        assert!(!palette_for(&out).exists(), "the palette scratch file was left behind");
    }

    #[test]
    fn cancelled_gif_leaves_nothing_behind() {
        let b = bins();
        let src = make_video();
        let out = tmp("cancelled.gif");
        let _ = std::fs::remove_file(&out);
        let mut j = job(&src, &out, Mode::Gif, 0.0, 10.0, None, true);
        j.gif = Some(GifOpts { fps: 24, width: 640, height: 360, colors: 256 });

        let c = AtomicBool::new(true);
        assert_eq!(export(&b, &j, &c, &|_| {}).unwrap_err(), CANCELLED);
        assert!(!out.exists(), "a cancelled GIF left a partial file behind");
        assert!(!palette_for(&out).exists(), "a cancelled GIF left its palette behind");
    }

    #[test]
    fn probe_frame_keyframes() {
        let b = bins();
        let src = make_video();
        let info = probe(&b, &src).unwrap();
        assert_eq!((info.width, info.height), (640, 360));
        assert!((info.duration - 12.0).abs() < 0.2, "{}", info.duration);
        assert_eq!(info.v_codec, "h264");
        assert_eq!(info.a_codec.as_deref(), Some("aac"));
        let (w, h, data) = grab_frame(&b, &src, 3.0, info.width, info.height).unwrap();
        assert_eq!((w, h), (640, 360));
        assert_eq!(data.len(), 640 * 360 * 4);
        assert!(data.iter().any(|&x| x != 0));
        let kf = keyframes(&b, &src).unwrap();
        assert!(kf.len() >= 6 && kf[0].abs() < 0.01 && (kf[1] - 2.0).abs() < 0.05, "{kf:?}");
    }

    #[test]
    fn player_delivers_frames() {
        let b = bins();
        let src = make_video();
        let mut p = Player::start(&b, &src, 2.0, 4.0, 640, 360, Arc::new(|| {})).unwrap();

        let wait = std::time::Duration::from_secs(15);
        let mut first = None;
        let end = std::time::Instant::now() + wait;
        while first.is_none() && std::time::Instant::now() < end {
            if let Some(f) = p.take_frame() {
                first = Some(f);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (w, h, data) = first.expect("no frame arrived from the player");
        assert_eq!((w, h), (640, 360));
        assert_eq!(data.len(), w * h * 4);
        assert!(data.iter().any(|&x| x != 0), "the frame is entirely black");

        // A second frame shows the stream keeps running past the first one.
        let mut second = false;
        let end = std::time::Instant::now() + wait;
        while !second && std::time::Instant::now() < end {
            if p.take_frame().is_some() {
                second = true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(second, "playback stopped after a single frame");
        p.stop();
    }

    #[test]
    fn a_cut_is_exactly_the_range_that_was_asked_for() {
        let b = bins();
        let src = make_video();
        let out = tmp("cut.mp4");
        let c = AtomicBool::new(false);
        // 4.0 to 9.0 on a file whose keyframes sit every 2 s: the start is
        // nowhere near one, and the cut must still be exactly five seconds.
        export(&b, &job(&src, &out, Mode::Video, 4.0, 5.0, None, true), &c, &|_| {}).unwrap();
        let info = probe(&b, &out).unwrap();
        assert!((info.duration - 5.0).abs() < 0.15, "dur {}", info.duration);
        assert_eq!(info.v_codec, "h264");
        assert_eq!((info.width, info.height), (640, 360));
        let d = dump(&b, &out);
        assert!(d.contains("SecretTitle"), "metadata should be kept:\n{d}");
    }

    #[test]
    fn metadata_off_strips_tags() {
        let b = bins();
        let src = make_video();
        let c = AtomicBool::new(false);
        for (name, mode, crop) in [
            ("strip_copy.mp4", Mode::Remux, None),
            ("strip_crop.mp4", Mode::Video, Some((320u32, 180u32, 10u32, 10u32))),
            ("strip_copy.mkv", Mode::Remux, None),
        ] {
            let out = tmp(name);
            export(&b, &job(&src, &out, mode, 0.0, 12.0, crop, false), &c, &|_| {}).unwrap();
            let d = dump(&b, &out);
            let tags: String = d.lines().filter(|l| l.to_ascii_lowercase().starts_with("tag:")).collect::<Vec<_>>().join("\n");
            for needle in ["secrettitle", "secretcomment", "12.3456", "streamsecret", "2019-03-04", "lavf5", "lavf6", "lavf7", "lavc", "libx264"] {
                assert!(!tags.to_ascii_lowercase().contains(needle), "{name}: found {needle} in tags:\n{tags}");
            }
            if mode == Mode::Video {
                let bytes = std::fs::read(&out).unwrap();
                assert!(!bytes.windows(11).any(|w| w == b"x264 - core"), "{name}: x264 SEI still present");
            }
        }
    }

    #[test]
    fn crop_reencodes_to_exact_size_and_keeps_audio() {
        let b = bins();
        let src = make_video();
        let out = tmp("crop.mp4");
        let c = AtomicBool::new(false);
        export(&b, &job(&src, &out, Mode::Video, 1.0, 4.0, Some((320, 180, 100, 50)), true), &c, &|_| {}).unwrap();
        let info = probe(&b, &out).unwrap();
        assert_eq!((info.width, info.height), (320, 180));
        assert!((info.duration - 4.0).abs() < 0.2, "dur {}", info.duration);
        assert_eq!(info.a_codec.as_deref(), Some("aac"));
    }

    #[test]
    fn cancel_removes_partial_output() {
        let b = bins();
        let src = make_video();
        let out = tmp("cancel.mp4");
        let c = AtomicBool::new(true);
        let r = export(&b, &job(&src, &out, Mode::Video, 0.0, 12.0, Some((320, 180, 0, 0)), true), &c, &|_| {});
        assert_eq!(r.unwrap_err(), CANCELLED);
        assert!(!out.exists());
    }

    #[test]
    fn progress_reaches_one() {
        let b = bins();
        let src = make_video();
        let out = tmp("prog.mp4");
        let c = AtomicBool::new(false);
        let last = Mutex::new(0.0f32);
        export(&b, &job(&src, &out, Mode::Video, 0.0, 12.0, Some((320, 180, 0, 0)), true), &c, &|p| *last.lock().unwrap() = p).unwrap();
        assert!(*last.lock().unwrap() > 0.99);
    }

    #[test]
    fn copy_compat_rules() {
        let mut i = VideoInfo {
            path: PathBuf::from("a.avi"),
            width: 10,
            height: 10,
            duration: 1.0,
            fps: 30.0,
            v_codec: "msmpeg4v3".into(),
            a_codec: Some("mp3".into()),
            ext: "avi".into(),
        };
        assert!(copy_compat(&i, "mp4").is_some());
        assert!(copy_compat(&i, "mkv").is_none());
        i.v_codec = "h264".into();
        assert!(copy_compat(&i, "mp4").is_none());

        // AV1 with Opus is the pair that made a file look uncopyable: the video
        // is fine as it is, and the audio is converted to AAC, so nothing here
        // should stop the save.
        i.v_codec = "av1".into();
        i.a_codec = Some("opus".into());
        assert!(copy_compat(&i, "mp4").is_none(), "av1/opus into mp4 is handled");
        assert!(audio_needs_aac(&i, "mp4"), "and that is what handles it");
        assert!(!audio_needs_aac(&i, "mkv"), "MKV keeps Opus as it is");

        // Audio nothing can rescue still has to be refused.
        i.a_codec = Some("wmav2".into());
        assert!(copy_compat(&i, "mp4").is_some());
    }
}
