use crate::audio;
use crate::crop::{self, Aspect, CropRect, Handle};
use crate::ffmpeg::{self, Bins, Job, Mode, VideoInfo};
use crate::settings;
use crate::timeline;
use crate::version_line;
use eframe::egui::{self, pos2, vec2, Color32, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, Vec2};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "3gp", "mpg", "mpeg", "ts", "mts", "m2ts", "ogv", "gif",
];

/// One press of the up or down arrow moves the volume this far, in percent.
const VOLUME_STEP: f32 = 5.0;

enum Msg {
    Bins(Result<Bins, String>),
    Probed(u64, Result<VideoInfo, String>),
    Frame(u64, Result<(usize, usize, Vec<u8>), String>),
    Keyframes(u64, Vec<f64>),
    Progress(f32),
    Done(Result<PathBuf, String>),
}

/// FFmpeg is prepared on a worker thread: on the first run the two packed
/// binaries have to be written to disk, and a frozen window would look broken.
enum BinsState {
    Preparing,
    Ready(Bins),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutFormat {
    Same,
    Mp4,
    Mkv,
    Mov,
    Gif,
    /// The sound alone, copied into the container its codec calls for.
    Audio,
    /// The sound alone, re-encoded to MP3.
    Mp3,
}

impl OutFormat {
    fn label(self) -> &'static str {
        match self {
            OutFormat::Same => "Same as source",
            OutFormat::Mp4 => "MP4",
            OutFormat::Mkv => "MKV",
            OutFormat::Mov => "MOV",
            OutFormat::Gif => "GIF",
            OutFormat::Audio => "Extract audio",
            OutFormat::Mp3 => "MP3",
        }
    }

    /// True for the two formats that write the sound and no picture.
    fn is_audio(self) -> bool {
        matches!(self, OutFormat::Audio | OutFormat::Mp3)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quality {
    High,
    Medium,
    Small,
}

impl Quality {
    fn label(self) -> &'static str {
        match self {
            Quality::High => "High quality",
            Quality::Medium => "Balanced",
            Quality::Small => "Smaller file",
        }
    }
    fn crf(self) -> u32 {
        match self {
            Quality::High => 18,
            Quality::Medium => 23,
            Quality::Small => 28,
        }
    }

    /// The same three steps as an MP3 bitrate: MP3 has no constant-quality
    /// mode worth exposing, and 320/192/128 covers "keep it", "normal" and
    /// "small enough to send".
    fn mp3_kbps(self) -> u32 {
        match self {
            Quality::High => 320,
            Quality::Medium => 192,
            Quality::Small => 128,
        }
    }

    /// GIF has no bitrate to turn down, so each quality steps the frame rate,
    /// the size and the palette together. `cw`/`ch` are the cropped picture; the
    /// long side is only ever made smaller, never blown up.
    fn gif_opts(self, cw: u32, ch: u32) -> ffmpeg::GifOpts {
        let (fps, max_side, colors) = match self {
            Quality::High => (15, 800.0, 256),
            Quality::Medium => (12, 640.0, 192),
            Quality::Small => (10, 480.0, 128),
        };
        let scale = (max_side / cw.max(ch).max(1) as f64).min(1.0);
        ffmpeg::GifOpts {
            fps,
            width: ((cw as f64 * scale).round() as u32).max(1),
            height: ((ch as f64 * scale).round() as u32).max(1),
            colors,
        }
    }
}

struct Exporting {
    progress: f32,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone)]
enum Notice {
    Info(String),
    Error(String),
    Saved(PathBuf),
}

#[derive(Clone)]
struct Plan {
    mode: Mode,
    cropped: bool,
    trim_changed: bool,
    can_save: bool,
    warn: Option<String>,
    out_ext: String,
    eff_start: f64,
    dur: f64,
    crop_px: Option<(u32, u32, u32, u32)>,
    /// Gain to apply to the sound, when the box under the controls asks for
    /// one. `None` leaves the audio as it is copied or converted.
    volume: Option<f64>,
    /// Geometry and frame rate, when the output is a GIF.
    gif: Option<ffmpeg::GifOpts>,
}

/// A crop-box drag in progress. The box is recomputed from the one it started
/// with every frame, so a slow drag is never rounded away by the snapping and
/// nothing accumulates while the pointer is held down.
#[derive(Clone, Copy)]
struct Drag {
    handle: Handle,
    origin: Pos2,
    rect: CropRect,
}

pub struct App {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    bins: BinsState,
    about_open: bool,
    /// What the title bar currently says, so it is only updated when it changes.
    title: String,
    gen: u64,
    loading: bool,
    info: Option<VideoInfo>,
    texture: Option<egui::TextureHandle>,
    frame_in_flight: bool,
    pending_t: Option<f64>,
    keyframes: Vec<f64>,
    crop: CropRect,
    aspect: Aspect,
    drag: Option<Drag>,
    /// Preview view: 1.0 is the picture fitted to the window, and the wheel
    /// scales it from there; `pan` shifts a zoomed picture.
    zoom: f32,
    pan: Vec2,
    /// Preview only, never the saved file: nearest-neighbour sampling, and the
    /// crop edges land on whole pixels.
    disable_smoothing: bool,
    pos: f64,
    start: f64,
    end: f64,
    keep_metadata: bool,
    keep_audio: bool,
    format: OutFormat,
    quality: Quality,
    export: Option<Exporting>,
    /// Playback, when it is running.
    player: Option<ffmpeg::Player>,
    /// The soundtrack of that playback. Kept apart from the player: sound is
    /// optional and can fail while the picture plays on.
    sound: Option<audio::Sound>,
    /// The *Sound* switch, which only affects the preview.
    sound_on: bool,
    /// The preview's loudness in percent, 0..=200. It is also the volume the
    /// save applies when *Apply current volume level to output* is ticked.
    volume: f32,
    /// Whether the saved file takes that volume.
    apply_volume: bool,
    /// Video time the current playback started at, and how many frames it has
    /// shown since: position follows the pictures, not the wall clock.
    play_from: f64,
    play_frames: u64,
    notice: Option<Notice>,
    /// Where the video is drawn on screen and the video-px -> screen scale (set every frame).
    disp: Rect,
    scale: f32,
}

fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    let ms = ((t - t.floor()) * 1000.0).round() as u64 % 1000;
    let s = t.floor() as u64;
    format!("{:02}:{:02}:{:02}.{:03}", s / 3600, (s / 60) % 60, s % 60, ms)
}

/// A gain as the whole percentage the controls show, for the sentences under
/// them: `1.5` reads as `150%`.
fn pct(gain: f64) -> String {
    format!("{}%", (gain * 100.0).round() as i64)
}

fn pick_video() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Video", VIDEO_EXTS)
        .add_filter("All files", &["*"])
        .pick_file()
}

fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        if let Some(p) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(p).spawn();
        }
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self::with_ctx(&cc.egui_ctx);
        // The one setting that is remembered from the last run. Tests build the
        // window through `with_ctx`, so they never touch the real file.
        app.sound_on = settings::Settings::load().sound_on;
        app
    }

    fn with_ctx(ctx: &egui::Context) -> Self {
        ctx.set_visuals(egui::Visuals::dark());
        let (tx, rx) = channel();
        let mut app = Self {
            tx,
            rx,
            bins: BinsState::Preparing,
            about_open: false,
            title: version_line(),
            gen: 0,
            loading: false,
            info: None,
            texture: None,
            frame_in_flight: false,
            pending_t: None,
            keyframes: Vec::new(),
            crop: CropRect::full(1.0, 1.0),
            aspect: Aspect::Free,
            drag: None,
            zoom: 1.0,
            pan: Vec2::ZERO,
            // Whole pixels by default: a crop is easier to place on the grid
            // the picture actually has, and the edges then land on it too.
            disable_smoothing: true,
            pos: 0.0,
            start: 0.0,
            end: 0.0,
            keep_metadata: true,
            keep_audio: true,
            format: OutFormat::Same,
            quality: Quality::High,
            export: None,
            player: None,
            sound: None,
            sound_on: true,
            volume: 100.0,
            apply_volume: false,
            play_from: 0.0,
            play_frames: 0,
            notice: None,
            disp: Rect::NOTHING,
            scale: 1.0,
        };
        app.prepare(ctx);
        app
    }

    /// Find the packed FFmpeg (writing it out on first use) without blocking the
    /// window for the second or two that takes.
    fn prepare(&mut self, ctx: &egui::Context) {
        self.bins = BinsState::Preparing;
        let (tx, ctx2) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Bins(ffmpeg::find_bins()));
            ctx2.request_repaint();
        });
    }

    /// The binaries, if they are ready.
    fn bins(&self) -> Option<Bins> {
        match &self.bins {
            BinsState::Ready(b) => Some(b.clone()),
            _ => None,
        }
    }

    /// Sampling for preview textures. Without smoothing the picture is drawn
    /// as nearest pixel, so a zoom shows the pixels the file actually has
    /// instead of a blurred guess at them. Saved files are never affected.
    fn tex_options(&self) -> egui::TextureOptions {
        if self.disable_smoothing {
            egui::TextureOptions::NEAREST
        } else {
            egui::TextureOptions::LINEAR
        }
    }

    /// How large a still preview should be grabbed. Zoomed in, it is grabbed
    /// above 1280 so that a video pixel is not softer than the screen pixel it
    /// lands on - that is what makes a crop to one pixel possible. The step
    /// doubles, so a turn of the wheel does not re-grab at a new size for
    /// every notch; it never goes past the picture's own size.
    fn still_size(&self, info: &VideoInfo) -> (usize, usize) {
        let level = if self.zoom > 1.0 { 2f32.powf(self.zoom.log2().floor()).max(1.0) } else { 1.0 };
        let long = (1280.0 * level).round() as u32;
        ffmpeg::fit_size(info.width, info.height, long.min(info.width.max(info.height).max(1)))
    }

    // ---------------------------------------------------------------- loading

    fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
        let Some(bins) = self.bins() else { return };
        self.stop_playback();
        self.gen += 1;
        self.loading = true;
        self.info = None;
        self.texture = None;
        self.keyframes.clear();
        self.pending_t = None;
        self.drag = None;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.notice = None;
        let (tx, ctx2, gen) = (self.tx.clone(), ctx.clone(), self.gen);
        std::thread::spawn(move || {
            let r = ffmpeg::probe(&bins, &path);
            let _ = tx.send(Msg::Probed(gen, r));
            ctx2.request_repaint();
        });
    }

    /// Put the window back to how it starts: nothing open, nothing drawn,
    /// nothing pending. The generation counter is what makes that final: a
    /// probe or a frame already on its way arrives with the old number and is
    /// ignored, so it cannot paint itself over the empty window afterwards.
    ///
    /// Output preferences (format, quality, metadata, audio)
    /// are deliberately left alone; clearing a file is not a reset of choices.
    fn clear(&mut self) {
        self.stop_playback();
        self.gen += 1;
        self.loading = false;
        self.info = None;
        self.texture = None;
        self.keyframes.clear();
        self.pending_t = None;
        self.drag = None;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.notice = None;
        self.crop = CropRect::full(1.0, 1.0);
        self.aspect = Aspect::Free;
        self.pos = 0.0;
        self.start = 0.0;
        self.end = 0.0;
    }

    fn handle_msgs(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Bins(res) => {
                    self.bins = match res {
                        Ok(b) => BinsState::Ready(b),
                        Err(e) => BinsState::Failed(e),
                    }
                }
                Msg::Probed(g, res) if g == self.gen => {
                    self.loading = false;
                    match res {
                        Ok(info) => {
                            self.crop = CropRect::full(info.width as f32, info.height as f32);
                            self.aspect = Aspect::Free;
                            self.start = 0.0;
                            self.end = info.duration;
                            // The playhead starts where the trim does. It used
                            // to jump ten percent in (three seconds at most)
                            // to skip a black opening frame, but that put the
                            // indicator somewhere the trim did not: the first
                            // preview was a frame nobody asked for, and Play
                            // began there instead of at the start.
                            self.pos = 0.0;
                            self.pending_t = Some(0.0);
                            if let Some(bins) = self.bins() {
                                let (tx, ctx2, gen, path) = (self.tx.clone(), ctx.clone(), self.gen, info.path.clone());
                                std::thread::spawn(move || {
                                    let k = ffmpeg::keyframes(&bins, &path).unwrap_or_default();
                                    let _ = tx.send(Msg::Keyframes(gen, k));
                                    ctx2.request_repaint();
                                });
                            }
                            self.info = Some(info);
                        }
                        Err(e) => self.notice = Some(Notice::Error(e)),
                    }
                }
                Msg::Frame(g, res) => {
                    self.frame_in_flight = false;
                    // A single-frame grab can land after playback has started;
                    // showing its older picture would make the video jump back.
                    if g == self.gen && !self.is_playing() {
                        match res {
                            Ok((w, h, data)) => {
                                let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &data);
                                let opts = self.tex_options();
                                self.texture = Some(ctx.load_texture("frame", img, opts));
                            }
                            Err(e) => self.notice = Some(Notice::Error(e)),
                        }
                    }
                }
                Msg::Keyframes(g, k) if g == self.gen => self.keyframes = k,
                Msg::Progress(p) => {
                    if let Some(e) = &mut self.export {
                        e.progress = p;
                    }
                }
                Msg::Done(res) => {
                    self.export = None;
                    self.notice = Some(match res {
                        Ok(p) => Notice::Saved(p),
                        Err(e) if e == ffmpeg::CANCELLED => Notice::Info("Export cancelled.".to_string()),
                        Err(e) => Notice::Error(e),
                    });
                }
                _ => {}
            }
        }
    }

    fn pump_frame(&mut self, ctx: &egui::Context) {
        if self.frame_in_flight || self.is_playing() {
            return;
        }
        let job = {
            let Some(bins) = self.bins() else {
                return;
            };
            let (Some(t), Some(info)) = (self.pending_t, self.info.as_ref()) else {
                return;
            };
            let size = self.still_size(info);
            (t, bins, info.path.clone(), size, info.duration)
        };
        self.pending_t = None;
        self.frame_in_flight = true;
        let (t, bins, path, (pw, ph), dur) = job;
        let (tx, ctx2, gen) = (self.tx.clone(), ctx.clone(), self.gen);
        std::thread::spawn(move || {
            let r = ffmpeg::grab_frame(&bins, &path, t.min(dur - 0.05).max(0.0), pw, ph);
            let _ = tx.send(Msg::Frame(gen, r));
            ctx2.request_repaint();
        });
    }

    // ---------------------------------------------------------------- playback

    /// True while the video is playing.
    fn is_playing(&self) -> bool {
        self.player.is_some()
    }

    /// Video time shown since playback started, counted from the frames that
    /// were actually drawn rather than from the clock: that keeps the playhead
    /// in step with the picture even when decoding is slower than real time.
    fn played(&self) -> f64 {
        match self.info.as_ref() {
            Some(info) if info.fps > 0.0 => self.play_frames as f64 / info.fps,
            _ => 0.0,
        }
    }

    fn toggle_play(&mut self, ctx: &egui::Context) {
        if self.is_playing() {
            self.stop_playback();
        } else {
            self.start_playback(ctx);
        }
    }

    fn start_playback(&mut self, ctx: &egui::Context) {
        if self.is_playing() || self.export.is_some() {
            return;
        }
        let (Some(bins), Some(info)) = (self.bins(), self.info.clone()) else {
            return;
        };
        // Play from the playhead, but if it sits at (or past) the end of the
        // trim, start the trim over instead of playing nothing.
        if self.pos >= self.end - 0.05 || self.pos < self.start {
            self.pos = self.start;
        }
        self.play_from = self.pos;
        self.play_frames = 0;
        self.pending_t = None;

        // A crop changes what playback is: the decoder is told to cut out just
        // that box, so the preview plays the selection instead of showing the
        // whole frame with the rest of it going unnoticed.
        let (fw, fh) = (info.width as f32, info.height as f32);
        let crop_px = if self.crop.is_full(fw, fh) { None } else { Some(self.crop.to_pixels(info.width, info.height)) };

        let ctx2 = ctx.clone();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx2.request_repaint());
        let started = ffmpeg::Player::start(
            &bins,
            &info.path,
            self.pos,
            self.end,
            info.width,
            info.height,
            info.fps,
            crop_px,
            repaint,
        );
        match started {
            Ok(p) => {
                self.player = Some(p);
                self.notice = None;
                self.start_sound(self.pos, false);
            }
            Err(e) => self.notice = Some(Notice::Error(e)),
        }
        ctx.request_repaint();
    }

    /// Starts the soundtrack at `from`, or leaves the preview silent when the
    /// switch is off or the file has no track. `now` is for the switch being
    /// ticked during playback: the picture is already running, so the sound
    /// has nothing to wait for.
    fn start_sound(&mut self, from: f64, now: bool) {
        self.sound = None;
        if !self.sound_on {
            return;
        }
        let (Some(bins), Some(info)) = (self.bins(), self.info.clone()) else { return };
        if info.a_codec.is_none() {
            return;
        }
        self.sound = audio::Sound::start(&bins, &info.path, from, self.end, self.volume / 100.0).ok();
        if now {
            if let Some(sound) = self.sound.as_ref() {
                sound.begin();
            }
        }
    }

    /// The volume box moved: pass it to the sound that is playing, if one is.
    /// The next buffers are ramped, so nothing clicks.
    fn volume_changed(&mut self) {
        if let Some(sound) = self.sound.as_ref() {
            sound.set_gain(self.volume / 100.0);
        }
    }

    /// Steps the volume by whole 5% increments, kept whole and inside the
    /// box's range. One call per key event, so holding an arrow key moves
    /// at the speed the keyboard repeats at, with nothing added on top.
    fn nudge_volume(&mut self, steps: f32) {
        let v = (self.volume + steps * VOLUME_STEP).round().clamp(0.0, 200.0);
        if v != self.volume {
            self.volume = v;
            self.volume_changed();
        }
    }

    /// Dropping the player kills the decoder and joins its reader thread.
    ///
    /// The last picture it drew is the crop, so a still of the whole frame has
    /// to be grabbed again before the preview goes back to showing one.
    ///
    /// The sound goes first and at once: a pause should be silent the moment it
    /// is asked for, not one buffer later.
    fn stop_playback(&mut self) {
        self.sound = None;
        let was = self.player.take();
        if was.is_some() && self.info.is_some() {
            self.pending_t = Some(self.pos);
        }
    }

    /// Playback reached the end of the trim by itself. The picture is done, but
    /// the sound still has a few buffers queued and cutting them would clip the
    /// last word; that thread stops when it runs out, and the next playback
    /// collects what is left of it.
    fn finish_playback(&mut self) {
        let was = self.player.take();
        if was.is_some() && self.info.is_some() {
            self.pending_t = Some(self.pos);
        }
    }

    /// Move the playhead by whole frames. Stepping stops playback: a frame
    /// cannot be looked at while the video is running past it.
    fn step_frame(&mut self, delta: i64) {
        let Some(info) = self.info.as_ref() else { return };
        let fps = if info.fps > 0.5 { info.fps } else { 30.0 };
        let last = (info.duration * fps).floor() as i64;
        let frame = ((self.pos * fps).round() as i64 + delta).clamp(0, last.max(0));
        self.stop_playback();
        self.pos = frame as f64 / fps;
        self.pending_t = Some(self.pos);
    }

    fn pump_playback(&mut self, ctx: &egui::Context) {
        let Some(player) = self.player.as_ref() else { return };

        if let Some((w, h, data)) = player.take_frame() {
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &data);
            let opts = self.tex_options();
            match &mut self.texture {
                Some(tex) if tex.size() == [w, h] => tex.set(img, opts),
                _ => {
                    self.texture = Some(ctx.load_texture("frame", img, opts));
                }
            }
        }

        // Follow the decoder, not the frames the window had time to draw.
        self.play_frames = player.decoded();
        if self.play_frames > 0 {
            self.pos = self.play_from + self.played();
            // The first picture is on screen, so the sound may follow it now.
            if let Some(sound) = self.sound.as_ref() {
                sound.begin();
            }
        }

        let ended = player.finished() || self.pos >= self.end - 0.001;
        if ended {
            self.finish_playback();
            self.pos = self.pos.clamp(self.start, self.end);
            ctx.request_repaint();
        } else {
            // A short delay rather than a busy loop: the reader thread asks for
            // a repaint as each frame arrives anyway.
            ctx.request_repaint_after(std::time::Duration::from_millis(8));
        }
    }

    // ---------------------------------------------------------------- planning

    fn plan(&self) -> Option<Plan> {
        let info = self.info.as_ref()?;
        let (fw, fh) = (info.width as f32, info.height as f32);
        let cropped = !self.crop.is_full(fw, fh);
        let trim_changed = self.start > 0.001 || self.end < info.duration - 0.001;
        let gif = self.format == OutFormat::Gif;
        let audio = self.format.is_audio();
        // The sound is turned up or down only when the box asks for it, the
        // file has a sound to turn, and that sound is being kept at all. 100%
        // is no change, so the box does nothing while it sits at 100%.
        let volume = if self.apply_volume
            && !gif
            && info.a_codec.is_some()
            && (audio || self.keep_audio)
            && (self.volume - 100.0).abs() >= 0.5
        {
            Some((self.volume / 100.0) as f64)
        } else {
            None
        };
        // Cutting re-encodes the picture. A stream copy can only begin on a
        // keyframe, so it cannot start where the trim says unless the trim
        // happens to land on one - and a cut that quietly becomes a different,
        // longer clip is worse than one that takes a moment longer to write.
        // The copy path is left for the one case that cannot go wrong: no cut
        // and no crop, only the tags being rewritten.
        let mode = if gif {
            Mode::Gif
        } else if audio {
            Mode::Audio
        } else if trim_changed || cropped {
            Mode::Video
        } else {
            Mode::Remux
        };

        let out_ext = match self.format {
            OutFormat::Gif => "gif".to_string(),
            OutFormat::Mp4 => "mp4".to_string(),
            OutFormat::Mkv => "mkv".to_string(),
            OutFormat::Mov => "mov".to_string(),
            OutFormat::Mp3 => "mp3".to_string(),
            // "Same as source" for the sound is the source codec's own
            // container. Without a codec there is nothing to extract and the
            // warning below stops the save, so the extension is a placeholder.
            OutFormat::Audio => {
                info.a_codec.as_deref().map(ffmpeg::audio_ext).unwrap_or("m4a").to_string()
            }
            OutFormat::Same => {
                if mode == Mode::Video && !matches!(info.ext.as_str(), "mp4" | "m4v" | "mov" | "mkv") {
                    "mp4".to_string()
                } else if volume.is_some() && !matches!(info.ext.as_str(), "mp4" | "m4v" | "mov" | "mkv") {
                    // A volume change re-encodes the sound, and the safe
                    // codec for that is AAC - which AVI and WMV cannot hold.
                    "mkv".to_string()
                } else {
                    info.ext.clone()
                }
            }
        };

        let eff_start = self.start;
        let dur = (self.end - eff_start).max(0.05);
        let warn = if audio && info.a_codec.is_none() {
            Some("This video has no audio track to extract.".to_string())
        } else if mode == Mode::Remux {
            ffmpeg::copy_compat(info, &out_ext)
        } else {
            None
        };
        // GIF and either audio format are a decision about the output itself,
        // so choosing one is always something to save.
        let can_save =
            gif || audio || cropped || trim_changed || !self.keep_metadata || !self.keep_audio || volume.is_some();
        // The picture is not written at all when only the sound is, so a box
        // drawn on it changes nothing.
        let crop_px = if cropped && !audio { Some(self.crop.to_pixels(info.width, info.height)) } else { None };
        let (gw, gh) = match crop_px {
            Some((w, h, _, _)) => (w, h),
            None => (info.width, info.height),
        };
        let gif = if gif { Some(self.quality.gif_opts(gw, gh)) } else { None };
        Some(Plan { mode, cropped, trim_changed, can_save, warn, out_ext, eff_start, dur, crop_px, volume, gif })
    }

    fn start_export(&mut self, ctx: &egui::Context) {
        let (Some(plan), Some(info)) = (self.plan(), self.info.clone()) else {
            return;
        };
        let stem = info.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "video".into());
        let suffix = if plan.mode == Mode::Audio {
            "_audio"
        } else if plan.cropped {
            "_cropped"
        } else if plan.mode == Mode::Gif {
            "_gif"
        } else if plan.trim_changed {
            "_cut"
        } else {
            "_clean"
        };
        let filter = match plan.mode {
            Mode::Gif => "GIF",
            Mode::Audio => "Audio",
            _ => "Video",
        };
        let mut dlg = rfd::FileDialog::new()
            .set_file_name(format!("{stem}{suffix}.{}", plan.out_ext))
            .add_filter(filter, &[plan.out_ext.as_str()]);
        if let Some(dir) = info.path.parent() {
            dlg = dlg.set_directory(dir);
        }
        let Some(mut out) = dlg.save_file() else { return };
        if out.extension().is_none() {
            out.set_extension(&plan.out_ext);
        }
        let same = out == info.path
            || matches!((out.canonicalize(), info.path.canonicalize()), (Ok(a), Ok(b)) if a == b);
        if same {
            self.notice = Some(Notice::Error("Choose a different file name; the original can't be overwritten.".into()));
            return;
        }
        self.begin_export(ctx, out);
    }

    fn begin_export(&mut self, ctx: &egui::Context, out: PathBuf) {
        let (Some(plan), Some(info), Some(bins)) = (self.plan(), self.info.clone(), self.bins()) else {
            return;
        };
        self.stop_playback();
        let job = Job {
            input: info.path.clone(),
            output: out,
            mode: plan.mode,
            start: plan.eff_start,
            dur: plan.dur,
            crop: plan.crop_px,
            keep_metadata: self.keep_metadata,
            keep_audio: self.keep_audio,
            crf: self.quality.crf(),
            // The audio is copied whenever it can be, except for two cases. One
            // is playback support: Opus and Vorbis in MP4. The other is
            // Matroska, and it is ffmpeg's doing - a copied stream there makes
            // it keep the input timestamps on the re-encoded video, so the
            // picture starts where the cut began while the sound starts at
            // zero, and the two sit however far the trim started from apart.
            audio_aac: self.keep_audio
                && plan.mode != Mode::Audio
                && (ffmpeg::audio_needs_aac(&info, &plan.out_ext)
                    || (plan.mode == Mode::Video && plan.out_ext == "mkv")),
            gif: plan.gif,
            audio_kbps: if plan.mode == Mode::Audio && self.format == OutFormat::Mp3 {
                Some(self.quality.mp3_kbps())
            } else {
                None
            },
            volume: plan.volume,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.export = Some(Exporting { progress: 0.0, cancel: cancel.clone() });
        self.notice = None;
        let (tx, ctx2) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let (tx2, c2) = (tx.clone(), ctx2.clone());
            let res = ffmpeg::export(&bins, &job, &cancel, &|p| {
                let _ = tx2.send(Msg::Progress(p));
                c2.request_repaint();
            });
            let _ = tx.send(Msg::Done(res.map(|_| job.output.clone())));
            ctx2.request_repaint();
        });
    }

    // ---------------------------------------------------------------- UI pieces

    fn ui_top(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let busy = self.export.is_some();
            let ready = self.bins().is_some();
            if ui.add_enabled(!busy && ready, egui::Button::new("Open video...")).clicked() {
                if let Some(p) = pick_video() {
                    self.open(ctx, p);
                }
            }
            // Clear sits to the right of the picker and is always laid out:
            // with no file it is greyed out rather than gone, so the row never
            // rearranges itself under the pointer. A file still being read
            // counts as open, so a slow or wrong pick can be undone.
            let has_file = self.info.is_some() || self.loading;
            if ui
                .add_enabled(!busy && has_file, egui::Button::new("Clear"))
                .on_hover_text("Close this file and go back to the drop area")
                .clicked()
            {
                self.clear();
            }
            if let Some(info) = &self.info {
                let name = info.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                ui.label(RichText::new(name).strong());
                ui.label(
                    RichText::new(format!(
                        "{}x{}   {}   {}{}",
                        info.width,
                        info.height,
                        fmt_time(info.duration),
                        info.v_codec,
                        info.a_codec.as_ref().map(|a| format!(" / {a}")).unwrap_or_default()
                    ))
                    .weak(),
                );
            } else if self.loading {
                ui.spinner();
                ui.label("Reading file...");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("About").clicked() {
                    self.about_open = true;
                }
            });
        });
        // The version stays visible even when a file is open, so the title bar
        // is still the place to read it from.
        let title = match &self.info {
            Some(info) => info
                .path
                .file_name()
                .map(|n| format!("{} - {}", n.to_string_lossy(), version_line())),
            None => None,
        }
        .unwrap_or_else(version_line);
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        ui.add_space(6.0);
    }

    fn ui_center(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if matches!(self.bins, BinsState::Preparing) {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.spinner();
                ui.add_space(8.0);
                ui.heading("Getting FFmpeg ready");
                ui.add_space(6.0);
                ui.label(RichText::new("This happens once, on the first run.").weak());
            });
            return;
        }
        if let BinsState::Failed(msg) = &self.bins {
            let msg = msg.clone();
            let mut retry = false;
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.heading("FFmpeg could not be prepared");
                ui.add_space(6.0);
                ui.label(msg);
                ui.add_space(10.0);
                if ui.button("Try again").clicked() {
                    retry = true;
                }
            });
            if retry {
                self.prepare(ctx);
            }
            return;
        }

        let Some(info) = self.info.clone() else {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.32);
                if self.loading {
                    ui.spinner();
                } else {
                    ui.heading("Drop a video here");
                    ui.add_space(6.0);
                    ui.label(RichText::new("or use Open video... above").weak());
                    if let Some(Notice::Error(e)) = &self.notice {
                        ui.add_space(12.0);
                        ui.colored_label(Color32::from_rgb(230, 100, 100), e);
                    }
                }
            });
            return;
        };

        let (fw, fh) = (info.width as f32, info.height as f32);
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let avail = area.shrink(8.0);
        let busy = self.export.is_some();
        let playing = self.is_playing();
        let cropped = !self.crop.is_full(fw, fh);

        // While a cropped clip plays, the decoder hands over just the crop, so
        // the preview *is* the selection - the picture the file will hold, and
        // nothing around it. Paused, the whole frame is shown, fitted to the
        // window and then zoomed by the wheel.
        let (cw, ch) = if playing && cropped {
            (self.crop.w().max(1.0), self.crop.h().max(1.0))
        } else {
            (fw, fh)
        };
        let fit = (avail.width() / cw).min(avail.height() / ch).max(0.01);
        let view = |zoom: f32, pan: Vec2| {
            let scale = fit * zoom;
            (scale, Rect::from_center_size(avail.center() + pan, vec2(cw * scale, ch * scale)))
        };
        let (mut scale, mut disp) =
            view(if playing { 1.0 } else { self.zoom }, if playing { Vec2::ZERO } else { self.pan });

        // The picture only answers the pointer where the panel can show it: a
        // zoomed one is larger than the panel, and must not take clicks from
        // the bar below.
        let resp = ui.interact(disp.expand(6.0).intersect(area), ui.id().with("crop_area"), Sense::click_and_drag());

        // The right button belongs to the picture, not to the box: it pans and
        // nothing else, even when it is pressed on a handle.
        let right_held = ui.input(|i| i.pointer.button_down(egui::PointerButton::Secondary));

        if !playing {
            // The wheel zooms around the pointer, so the pixel under it stays
            // under it while the picture grows and shrinks.
            let zoom_before = self.zoom;
            if resp.hovered() {
                let scroll = ui.input(|i| i.raw_scroll_delta.y);
                if scroll != 0.0 {
                    self.zoom = (self.zoom * (scroll * 0.0025).exp()).clamp(1.0, 8.0);
                    if let Some(p) = resp.hover_pos() {
                        let anchor = (p - disp.min) / scale.max(0.01);
                        let ns = fit * self.zoom;
                        self.pan = p - anchor * ns + vec2(cw, ch) * ns / 2.0 - avail.center();
                    }
                }
            }
            // Right-drag pans, the way a map does.
            let (delta, pointer) = ui.input(|i| (i.pointer.delta(), i.pointer.interact_pos()));
            if right_held && pointer.is_some_and(|p| area.contains(p)) && self.zoom > 1.0 {
                self.pan += delta;
            }
            // Keep the picture over the window: it cannot be dragged off into
            // the dark, and a fitted one cannot move at all.
            let (_, fitted) = view(self.zoom, self.pan);
            let room = (fitted.size() - avail.size()) / 2.0;
            self.pan = vec2(
                self.pan.x.clamp(-room.x.max(0.0), room.x.max(0.0)),
                self.pan.y.clamp(-room.y.max(0.0), room.y.max(0.0)),
            );
            let (s2, d2) = view(self.zoom, self.pan);
            scale = s2;
            disp = d2;

            // A zoomed picture is grabbed larger, so one video pixel keeps a
            // pixel of its own on screen.
            if self.zoom != zoom_before && !self.frame_in_flight && self.pending_t.is_none() {
                let size = self.still_size(&info);
                if self.texture.as_ref().map(|t| t.size()) != Some([size.0, size.1]) {
                    self.pending_t = Some(self.pos);
                }
            }
        }
        self.disp = disp;
        self.scale = scale;

        // Frame.
        let painter = ui.painter_at(area);
        painter.rect_filled(disp, 0.0, Color32::BLACK);
        match &self.texture {
            Some(tex) => {
                painter.image(tex.id(), disp, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                painter.text(
                    disp.center(),
                    egui::Align2::CENTER_CENTER,
                    "Loading preview...",
                    egui::FontId::proportional(16.0),
                    Color32::GRAY,
                );
            }
        }

        // Crop interaction.
        let to_screen = |c: &CropRect| CropRect {
            x0: disp.left() + c.x0 * scale,
            y0: disp.top() + c.y0 * scale,
            x1: disp.left() + c.x1 * scale,
            y1: disp.top() + c.y1 * scale,
        };
        // A click on the picture pauses, the way a player behaves.
        if playing && resp.clicked() {
            self.stop_playback();
        }
        if !busy && !playing {
            let sr = to_screen(&self.crop);
            let hover = resp.hover_pos().and_then(|p| crop::hit_test(&sr, p.x, p.y, 10.0));
            // Only the primary button drags the box. egui reports a drag for
            // every button, so without this a right-drag would move the box
            // while it panned the picture under it.
            let primary = egui::PointerButton::Primary;
            if resp.drag_started_by(primary) {
                let origin = ui.input(|i| i.pointer.press_origin());
                let rect = self.crop;
                self.drag = origin
                    .and_then(|p| crop::hit_test(&sr, p.x, p.y, 10.0).map(|handle| Drag { handle, origin: p, rect }));
            }
            if resp.dragged_by(primary) {
                if let (Some(d), Some(p)) = (self.drag, resp.interact_pointer_pos()) {
                    // Every frame starts from the box the drag began with, so
                    // a slow drag is never rounded away by the snapping.
                    let mut r = d.rect;
                    let delta = (p - d.origin) / scale.max(0.01);
                    crop::apply_drag(&mut r, d.handle, delta.x, delta.y, fw, fh, 16.0, self.aspect.ratio());
                    if self.disable_smoothing {
                        crop::snap(&mut r, fw, fh);
                    }
                    self.crop = r;
                }
            }
            if resp.drag_stopped() {
                self.drag = None;
            }
            if let Some(h) = self.drag.map(|d| d.handle).or(hover).filter(|_| !right_held) {
                ctx.set_cursor_icon(match h {
                    Handle::Move => CursorIcon::Move,
                    Handle::N | Handle::S => CursorIcon::ResizeVertical,
                    Handle::E | Handle::W => CursorIcon::ResizeHorizontal,
                    Handle::NE | Handle::SW => CursorIcon::ResizeNeSw,
                    Handle::NW | Handle::SE => CursorIcon::ResizeNwSe,
                });
            } else if right_held && self.zoom > 1.0 {
                ctx.set_cursor_icon(CursorIcon::Grabbing);
            }
        }

        // Overlay. Not while playing: there a dimmed crop box would just get in
        // the way of the thing the user is watching.
        if !self.is_playing() {
            let sr = to_screen(&self.crop);
            let dim = Color32::from_black_alpha(150);
            let r = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(pos2(x0, y0), pos2(x1, y1));
            painter.rect_filled(r(disp.left(), disp.top(), disp.right(), sr.y0), 0.0, dim);
            painter.rect_filled(r(disp.left(), sr.y1, disp.right(), disp.bottom()), 0.0, dim);
            painter.rect_filled(r(disp.left(), sr.y0, sr.x0, sr.y1), 0.0, dim);
            painter.rect_filled(r(sr.x1, sr.y0, disp.right(), sr.y1), 0.0, dim);
            let box_rect = r(sr.x0, sr.y0, sr.x1, sr.y1);
            let thin = Stroke::new(1.0_f32, Color32::from_white_alpha(80));
            for i in 1..3 {
                let f = i as f32 / 3.0;
                let x = sr.x0 + (sr.x1 - sr.x0) * f;
                let y = sr.y0 + (sr.y1 - sr.y0) * f;
                painter.line_segment([pos2(x, sr.y0), pos2(x, sr.y1)], thin);
                painter.line_segment([pos2(sr.x0, y), pos2(sr.x1, y)], thin);
            }
            painter.rect_stroke(box_rect, 0.0, Stroke::new(2.0_f32, Color32::WHITE));
            let (mx, my) = ((sr.x0 + sr.x1) / 2.0, (sr.y0 + sr.y1) / 2.0);
            for p in [
                pos2(sr.x0, sr.y0),
                pos2(sr.x1, sr.y0),
                pos2(sr.x0, sr.y1),
                pos2(sr.x1, sr.y1),
                pos2(mx, sr.y0),
                pos2(mx, sr.y1),
                pos2(sr.x0, my),
                pos2(sr.x1, my),
            ] {
                let hr = Rect::from_center_size(p, vec2(9.0, 9.0));
                painter.rect_filled(hr, 1.5, Color32::WHITE);
                painter.rect_stroke(hr, 1.5, Stroke::new(1.0_f32, Color32::from_gray(60)));
            }
        }

        // Drop highlight.
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            painter.rect_filled(area, 0.0, Color32::from_rgba_unmultiplied(40, 90, 160, 120));
            painter.text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                "Drop to open",
                egui::FontId::proportional(26.0),
                Color32::WHITE,
            );
        }
    }

    fn ui_bottom(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(info) = self.info.clone() else {
            return;
        };
        let plan = self.plan();
        let busy = self.export.is_some();
        let (fw, fh) = (info.width as f32, info.height as f32);
        ui.add_space(6.0);

        ui.add_enabled_ui(!busy, |ui| {
            // Timeline.
            if timeline::show(ui, info.duration, &mut self.pos, &mut self.start, &mut self.end, &self.keyframes) {
                self.stop_playback();
                self.pos = self.pos.clamp(0.0, (info.duration - 0.05).max(0.0));
                self.pending_t = Some(self.pos);
            }
            ui.horizontal(|ui| {
                let label = if self.is_playing() { "Pause" } else { "Play" };
                let play = ui
                    .add(egui::Button::new(label).min_size(vec2(60.0, 20.0)))
                    .on_hover_text("Play from the playhead to the end of the trim (Space)")
                    .clicked();
                if play {
                    self.toggle_play(ctx);
                }
                // The preview's own switch, not an output setting: what the
                // saved file does with its sound is *Keep audio*, further along.
                let has_sound = info.a_codec.is_some();
                let sound = ui
                    .add_enabled(has_sound, egui::Checkbox::new(&mut self.sound_on, "Sound"))
                    .on_hover_text("Plays the soundtrack with the preview. The switch is remembered.")
                    .on_disabled_hover_text("This video has no audio track.");
                if sound.changed() {
                    // The one switch that says how the program should behave
                    // before a file is open, so it outlives the session.
                    settings::Settings { sound_on: self.sound_on }.save();
                    if self.is_playing() {
                        // Ticking it mid-play starts the sound at the frame on
                        // screen; unticking it stops the sound there too.
                        self.start_sound(self.pos, true);
                    }
                }
                // The preview's loudness - and, with the box further along the
                // format row ticked, the saved file's too. One control: a box
                // that drags like a slider and can be clicked and typed into.
                // It moves at half the usual drag speed, so an odd percentage
                // is easy to land on.
                let typed = ui
                    .add_enabled(
                        has_sound,
                        egui::DragValue::new(&mut self.volume)
                            .range(0.0..=200.0)
                            .suffix("%")
                            .max_decimals(0)
                            .speed(0.5),
                    )
                    .on_hover_text(
                        "How loud the preview plays. Drag it, or click and type a number; tick \
                         *Apply current volume level to output* to use it in the saved file too.",
                    )
                    .on_disabled_hover_text("This video has no audio track.");
                if typed.changed() {
                    self.volume_changed();
                }
                // The drag moves in halves, which is what makes an odd
                // percentage easy to reach; the number snaps to whole once the
                // mouse is let go, so what is saved is the 137% that is shown.
                if typed.drag_stopped() || typed.lost_focus() {
                    self.volume = self.volume.round();
                    self.volume_changed();
                }
                ui.separator();
                if ui.button("Set start").on_hover_text("Trim start = current position").clicked() {
                    self.start = self.pos.min(self.end - 0.05).max(0.0);
                }
                ui.label(RichText::new(fmt_time(self.start)).monospace());
                ui.separator();
                ui.label(RichText::new(format!("Position {}", fmt_time(self.pos))).monospace().weak());
                ui.separator();
                ui.label(RichText::new(fmt_time(self.end)).monospace());
                if ui.button("Set end").on_hover_text("Trim end = current position").clicked() {
                    self.end = self.pos.max(self.start + 0.05).min(info.duration);
                }
                if ui.button("Reset trim").clicked() {
                    self.start = 0.0;
                    self.end = info.duration;
                }
                // A preview setting, not an output one, so it sits next to the
                // other trims rather than with the format choices.
                if ui
                    .checkbox(&mut self.disable_smoothing, "Disable video smoothing")
                    .on_hover_text(
                        "Preview only: draws whole pixels instead of a smoothed picture, and makes \
                         the crop edges land on whole pixels. The saved file is not affected.\n\
                         Scroll over the picture to zoom; right-drag pans a zoomed one.",
                    )
                    .changed()
                    && !self.is_playing()
                    && self.info.is_some()
                {
                    // The sampling is fixed when the texture is uploaded, so
                    // the frame has to be grabbed again to change it.
                    self.pending_t = Some(self.pos);
                }
            });

            ui.add_space(4.0);
            // Aspect presets.
            ui.horizontal_wrapped(|ui| {
                ui.label("Aspect:");
                for a in Aspect::ALL {
                    if ui.selectable_label(self.aspect == a, a.label()).clicked() {
                        self.aspect = a;
                        if let Some(ra) = a.ratio() {
                            self.crop = crop::fit_aspect(fw, fh, ra);
                            if self.disable_smoothing {
                                crop::snap(&mut self.crop, fw, fh);
                            }
                        }
                    }
                }
                if ui.button("Reset crop").clicked() {
                    self.crop = CropRect::full(fw, fh);
                    self.aspect = Aspect::Free;
                }
                let (cw, ch, cx, cy) = self.crop.to_pixels(info.width, info.height);
                ui.label(RichText::new(format!("   {cw} x {ch}  at ({cx}, {cy})")).monospace().weak());
            });

            ui.add_space(2.0);
            // Output options.
            ui.horizontal_wrapped(|ui| {
                ui.label("Format:");
                egui::ComboBox::from_id_salt("format").selected_text(self.format.label()).show_ui(ui, |ui| {
                    for f in [
                        OutFormat::Same,
                        OutFormat::Mp4,
                        OutFormat::Mkv,
                        OutFormat::Mov,
                        OutFormat::Gif,
                        OutFormat::Audio,
                        OutFormat::Mp3,
                    ] {
                        ui.selectable_value(&mut self.format, f, f.label());
                    }
                });
                let reencode = plan
                    .as_ref()
                    .map(|p| p.mode == Mode::Video || (p.mode == Mode::Audio && self.format == OutFormat::Mp3))
                    .unwrap_or(false);
                ui.add_enabled_ui(reencode, |ui| {
                    egui::ComboBox::from_id_salt("quality").selected_text(self.quality.label()).show_ui(ui, |ui| {
                        for q in [Quality::High, Quality::Medium, Quality::Small] {
                            ui.selectable_value(&mut self.quality, q, q.label());
                        }
                    });
                })
                .response
                .on_hover_text("The picture's frame rate and size, or the MP3 bitrate.");
                let gif = plan.as_ref().map(|p| p.mode == Mode::Gif).unwrap_or(false);
                let audio_only = plan.as_ref().map(|p| p.mode == Mode::Audio).unwrap_or(false);
                ui.add_enabled(
                    !gif,
                    egui::Checkbox::new(&mut self.keep_metadata, "Keep metadata"),
                )
                .on_hover_text(
                    "Off: removes tags such as title, creation date, GPS location and encoder info.\nVideo and audio are not re-encoded just for this.",
                )
                .on_disabled_hover_text("A GIF has no place to keep the tags.");
                ui.add_enabled(!gif && !audio_only, egui::Checkbox::new(&mut self.keep_audio, "Keep audio"))
                    .on_hover_text("Off: the sound is dropped and the file is written without an audio track.")
                    .on_disabled_hover_text("A GIF has no sound, and an audio export is the sound itself.");
                // The box's volume, used for the saved file. Off by default:
                // a save that only rewrites tags must not touch the sound.
                let gain_possible = !gif && info.a_codec.is_some() && (audio_only || self.keep_audio);
                ui.add_enabled(
                    gain_possible,
                    egui::Checkbox::new(&mut self.apply_volume, "Apply current volume level to output"),
                )
                .on_hover_text(
                    "Writes the saved file's sound at the volume above, from silence to twice as loud. \
                     The audio is re-encoded; the picture is not re-encoded for it.",
                )
                .on_disabled_hover_text(if gif {
                    "A GIF has no sound."
                } else if info.a_codec.is_none() {
                    "This video has no audio track."
                } else {
                    "Tick Keep audio to keep the sound."
                });
            });
        });

        ui.add_space(4.0);
        // GIF is the only mode with something worth announcing: its size, rate
        // and colour count are chosen by the quality step. A cut is just a cut.
        if let Some(p) = &plan {
            if let Mode::Gif = p.mode {
                let g = p.gif.unwrap_or_default();
                ui.colored_label(
                    Color32::from_rgb(240, 180, 90),
                    format!(
                        "GIF: {} x {} at {} fps, {} colours, silent, loops forever. Re-encoded in two passes.",
                        g.width, g.height, g.fps, g.colors
                    ),
                );
            }
            if p.mode == Mode::Audio {
                let codec = info.a_codec.as_deref().unwrap_or("the sound");
                let level = p.volume.map(|g| format!(", volume {}", pct(g))).unwrap_or_default();
                let text = if self.format == OutFormat::Mp3 {
                    format!(
                        "Audio only: {codec} converted to MP3 at {} kbps{level}. No picture is written.",
                        self.quality.mp3_kbps()
                    )
                } else if p.volume.is_some() {
                    format!("Audio only: {codec} re-encoded into .{}{level}. No picture is written.", p.out_ext)
                } else {
                    format!("Audio only: {codec} copied as it is into .{}. No picture is written.", p.out_ext)
                };
                ui.label(RichText::new(text).weak());
            }
            if let Some(g) = p.volume {
                if p.mode != Mode::Audio {
                    ui.label(
                        RichText::new(format!(
                            "Sound at {}: the audio is re-encoded, the picture is not.",
                            pct(g)
                        ))
                        .weak(),
                    );
                }
            }
            if p.trim_changed {
                ui.label(RichText::new(format!("Output length {}", fmt_time(p.dur))).weak());
            }
            if self.keep_audio && ffmpeg::audio_needs_aac(&info, &p.out_ext) {
                ui.label(
                    RichText::new(format!(
                        "Audio is converted to AAC: {} in .{} plays in few players.",
                        info.a_codec.as_deref().unwrap_or("this audio"),
                        p.out_ext
                    ))
                    .weak(),
                );
            }
            if let Some(w) = &p.warn {
                ui.colored_label(Color32::from_rgb(230, 100, 100), w);
            }
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let ok = plan.as_ref().map(|p| p.can_save && p.warn.is_none()).unwrap_or(false);
            if ui
                .add_enabled(ok && !busy, egui::Button::new("Save...").min_size(vec2(96.0, 28.0)))
                .on_disabled_hover_text("Crop the video, trim it, or turn off Keep metadata first.")
                .clicked()
            {
                self.start_export(ctx);
            }
            if let Some(e) = &self.export {
                ui.add(egui::ProgressBar::new(e.progress).desired_width(280.0).show_percentage());
                if ui.button("Cancel").clicked() {
                    e.cancel.store(true, Ordering::Relaxed);
                }
            } else if let Some(n) = self.notice.clone() {
                match n {
                    Notice::Info(s) => {
                        ui.label(s);
                    }
                    Notice::Error(s) => {
                        ui.colored_label(Color32::from_rgb(230, 100, 100), s);
                    }
                    Notice::Saved(p) => {
                        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                        ui.colored_label(Color32::from_rgb(110, 200, 140), format!("Saved {name}"));
                        if ui.button("Show in folder").clicked() {
                            reveal(&p);
                        }
                    }
                }
            }
        });
        ui.add_space(8.0);
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }
}

const LICENSE_TEXT: &str = include_str!("../LICENSE");

impl App {
    fn ui_about(&mut self, ctx: &egui::Context) {
        let mut open = self.about_open;
        egui::Window::new("About")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(vec2(660.0, 560.0))
            .show(ctx, |ui| {
                ui.heading(version_line());
                ui.add_space(2.0);
                ui.label("Cutting and cropping for video files.");
                ui.label(RichText::new("Copyright 2026 finnuen. Apache License 2.0.").weak());

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(RichText::new("FFmpeg").strong());
                ui.label(
                    "FFmpeg and ffprobe are packed inside this program and started as separate \
                     processes, which is what keeps finnCrop's own terms Apache 2.0. FFmpeg is \
                     licensed under the GNU General Public License v3; the version this build \
                     carries, and the source corresponding to it, are named in THIRD-PARTY.md, \
                     and the licence text is written beside the binaries in the folder below.",
                );
                // Selectable, so the address can be copied out of the window by
                // someone who has only the exe and cannot see the file itself.
                ui.add(
                    egui::Label::new(
                        RichText::new("github.com/finnuen/finnCrop/blob/main/THIRD-PARTY.md")
                            .monospace()
                            .weak(),
                    )
                    .selectable(true),
                );
                if let Some(bins) = self.bins() {
                    let dir = bins
                        .ffmpeg
                        .parent()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    ui.label(RichText::new(dir).monospace().weak());
                }

                ui.add_space(10.0);
                ui.collapsing("Apache License 2.0 (full text)", |ui| {
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        ui.label(RichText::new(LICENSE_TEXT).monospace().size(11.0));
                    });
                });
            });
        self.about_open = open;
    }

    fn frame(&mut self, ctx: &egui::Context) {
        self.handle_msgs(ctx);

        // Space plays and pauses. The event is consumed here so that a button
        // holding keyboard focus does not act on the same press.
        if self.info.is_some()
            && self.export.is_none()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space))
        {
            self.toggle_play(ctx);
        }

        // Left and right step one frame at a time, which is the only way to
        // land on an exact frame by hand.
        if self.info.is_some() && self.export.is_none() {
            let (back, forward) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                )
            });
            if back {
                self.step_frame(-1);
            }
            if forward {
                self.step_frame(1);
            }
        }

        // Up and down move the volume in whole steps of 5% - the same number
        // the box shows. The keyboard's own repeat drives this:
        // every repeat event is one step, so a held key runs at a steady speed
        // and never accelerates, and the ends of the range simply stop it.
        let has_sound = self.info.as_ref().is_some_and(|i| i.a_codec.is_some());
        if has_sound && self.export.is_none() {
            let (up, down) = ctx.input_mut(|i| {
                (
                    i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                )
            });
            if up != down {
                self.nudge_volume(up as f32 - down as f32);
            }
        }

        // Dropped files.
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).next());
        if let Some(p) = dropped {
            if self.export.is_none() {
                self.open(ctx, p);
            }
        }

        self.pump_playback(ctx);
        self.pump_frame(ctx);

        egui::TopBottomPanel::top("top").show(ctx, |ui| self.ui_top(ui, ctx));
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| self.ui_bottom(ui, ctx));
        egui::CentralPanel::default().show(ctx, |ui| self.ui_center(ui, ctx));
        self.ui_about(ctx);

        // Make sure a pending frame request gets picked up promptly.
        if self.pending_t.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, RawInput};
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn step(app: &mut App, ctx: &egui::Context, t: &mut f64, events: Vec<Event>) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1100.0, 780.0))),
            time: Some(*t),
            events,
            ..Default::default()
        };
        *t += 0.05;
        let _ = ctx.run(input, |c| app.frame(c));
    }

    fn wait_until(app: &mut App, ctx: &egui::Context, t: &mut f64, what: &str, mut cond: impl FnMut(&App) -> bool) {
        let end = Instant::now() + Duration::from_secs(40);
        while !cond(app) {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(40));
            step(app, ctx, t, vec![]);
        }
    }

    fn make_src() -> PathBuf {
        static SRC: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        SRC.get_or_init(|| {
            let dir = std::env::temp_dir().join("vc_ui_tests");
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join("ui_src.mp4");
            let b = ffmpeg::find_bins().unwrap();
            let st = Command::new(&b.ffmpeg)
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30", "-f", "lavfi", "-i", "sine=frequency=440"])
                .args(["-t", "8", "-c:v", "libx264", "-g", "30", "-keyint_min", "30", "-sc_threshold", "0", "-pix_fmt", "yuv420p", "-c:a", "aac"])
                .arg(&p)
                .status()
                .unwrap();
            assert!(st.success());
            p
        })
        .clone()
    }

    fn make_silent_src() -> PathBuf {
        static SRC: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        SRC.get_or_init(|| {
            let dir = std::env::temp_dir().join("vc_ui_tests");
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join("ui_silent.mp4");
            let b = ffmpeg::find_bins().unwrap();
            let st = Command::new(&b.ffmpeg)
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30"])
                .args(["-t", "2", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-an"])
                .arg(&p)
                .status()
                .unwrap();
            assert!(st.success());
            p
        })
        .clone()
    }

    /// A source in a container finnCrop would not write itself: asking for a
    /// volume change on it has to move the file somewhere the re-encoded
    /// sound fits.
    fn make_avi() -> PathBuf {
        static SRC: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        SRC.get_or_init(|| {
            let dir = std::env::temp_dir().join("vc_ui_tests");
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join("ui_src.avi");
            let b = ffmpeg::find_bins().unwrap();
            let st = Command::new(&b.ffmpeg)
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30", "-f", "lavfi", "-i", "sine=frequency=440"])
                .args(["-t", "2", "-c:v", "mpeg4", "-c:a", "libmp3lame"])
                .arg(&p)
                .status()
                .unwrap();
            assert!(st.success());
            p
        })
        .clone()
    }

    /// ffprobe's stream text, for the audio exports `probe` cannot read back:
    /// it insists on a video stream, which these files deliberately lack.
    fn probe_raw(p: &Path) -> String {
        let b = ffmpeg::find_bins().unwrap();
        let o = Command::new(&b.ffprobe)
            .args(["-v", "error", "-show_streams", "-show_format", "-of", "default"])
            .arg(p)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).to_string()
    }

    #[test]
    fn empty_and_missing_ffmpeg_states_render() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        app.bins = BinsState::Failed("not found".into());
        step(&mut app, &ctx, &mut t, vec![]);
    }

    #[test]
    fn playback_advances_the_playhead_and_stops_at_the_trim_end() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        let src = make_src();
        app.open(&ctx, src);
        wait_until(&mut app, &ctx, &mut t, "preview + keyframes", |a| a.texture.is_some() && !a.keyframes.is_empty());

        // A one-second trim keeps the test short: playback must stop at its end.
        app.start = 1.0;
        app.end = 2.0;
        app.pos = 1.0;
        app.start_playback(&ctx);
        assert!(app.is_playing());
        assert!(!app.frame_in_flight, "single-frame grabbing must stay off during playback");

        wait_until(&mut app, &ctx, &mut t, "playback to finish", |a| !a.is_playing());
        assert!(app.play_frames > 10, "only {} frames were shown", app.play_frames);
        assert!((app.pos - 2.0).abs() < 0.2, "stopped at {}", app.pos);

        // Play from the end: the trim starts over rather than playing nothing.
        app.start_playback(&ctx);
        assert!(app.is_playing());
        assert!((app.pos - 1.0).abs() < 0.001, "restarted at {}", app.pos);
        app.stop_playback();
        assert!(!app.is_playing());
    }

    #[test]
    fn a_file_opens_with_the_playhead_at_the_start() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_src());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.info.is_some() && a.texture.is_some());

        assert_eq!(app.pos, 0.0, "the playhead should sit at the start, not ten percent in");
        assert_eq!(app.start, 0.0);
        assert!(app.end > 1.0);
    }

    #[test]
    fn arrow_keys_step_exactly_one_frame() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_src());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.info.is_some() && a.texture.is_some());
        let fps = app.info.as_ref().unwrap().fps;
        assert!((fps - 30.0).abs() < 0.01, "fps {fps}");
        assert_eq!(app.pos, 0.0);

        let key = |k| Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        step(&mut app, &ctx, &mut t, vec![key(egui::Key::ArrowRight)]);
        assert!((app.pos - 1.0 / 30.0).abs() < 1e-9, "pos {}", app.pos);
        step(&mut app, &ctx, &mut t, vec![key(egui::Key::ArrowRight)]);
        assert!((app.pos - 2.0 / 30.0).abs() < 1e-9, "pos {}", app.pos);
        step(&mut app, &ctx, &mut t, vec![key(egui::Key::ArrowLeft)]);
        assert!((app.pos - 1.0 / 30.0).abs() < 1e-9, "pos {}", app.pos);

        // It cannot step back past the first frame.
        for _ in 0..4 {
            step(&mut app, &ctx, &mut t, vec![key(egui::Key::ArrowLeft)]);
        }
        assert_eq!(app.pos, 0.0);
    }

    /// The keyboard's own repeat is what carries a held arrow key: each repeat
    /// is one step, so the speed stays the same however long it is held.
    #[test]
    fn the_up_and_down_arrows_step_the_volume() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_src());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.info.is_some() && a.texture.is_some());
        assert_eq!(app.volume, 100.0);

        let key = |k, repeat| Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: Modifiers::NONE,
        };
        let (up, down) = (egui::Key::ArrowUp, egui::Key::ArrowDown);

        step(&mut app, &ctx, &mut t, vec![key(up, false)]);
        assert_eq!(app.volume, 105.0);
        step(&mut app, &ctx, &mut t, vec![key(up, true)]);
        assert_eq!(app.volume, 110.0, "a repeat is a step like any other");
        // Two repeats caught in one frame are two steps, not one.
        step(&mut app, &ctx, &mut t, vec![key(up, true), key(up, true)]);
        assert_eq!(app.volume, 120.0);
        step(&mut app, &ctx, &mut t, vec![key(down, false)]);
        assert_eq!(app.volume, 115.0);

        // The ends of the range stop it, and the number stays whole.
        for _ in 0..30 {
            step(&mut app, &ctx, &mut t, vec![key(up, true)]);
        }
        assert_eq!(app.volume, 200.0);
        for _ in 0..50 {
            step(&mut app, &ctx, &mut t, vec![key(down, true)]);
        }
        assert_eq!(app.volume, 0.0);

        // The keys move the shared number only: they do not opt the save in.
        assert!(!app.apply_volume, "the arrows must not tick the format row box");
        assert!(app.plan().unwrap().volume.is_none());
    }

    #[test]
    fn the_wheel_zooms_the_preview() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_src());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.info.is_some() && a.texture.is_some());
        assert_eq!(app.zoom, 1.0);

        let wheel = |dy: f32| Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, dy),
            modifiers: Modifiers::NONE,
        };
        // The pointer has to be over the picture: the wheel is only a zoom there.
        let centre = app.disp.center();
        step(&mut app, &ctx, &mut t, vec![Event::PointerMoved(centre)]);
        step(&mut app, &ctx, &mut t, vec![wheel(60.0)]);
        assert!(app.zoom > 1.0, "the wheel did not zoom: {}", app.zoom);
        let zoomed = app.zoom;

        // Zooming out stops at the fitted picture, with nothing panned away.
        for _ in 0..40 {
            step(&mut app, &ctx, &mut t, vec![wheel(-60.0)]);
        }
        assert_eq!(app.zoom, 1.0, "zoom {}", app.zoom);
        assert_eq!(app.pan, Vec2::ZERO, "a fitted picture cannot be off centre");
        assert!(zoomed > 1.1);
    }

    /// egui reports a drag for every button, so a right-drag over the box used
    /// to move the box while it panned. The right button is the picture's.
    #[test]
    fn a_right_drag_pans_without_moving_the_box() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_src());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.info.is_some() && a.texture.is_some());

        app.zoom = 2.0;
        step(&mut app, &ctx, &mut t, vec![]);
        let before = app.crop;
        let m = Modifiers::NONE;
        let start = app.disp.center();
        let end = start + vec2(30.0, 20.0);
        step(&mut app, &ctx, &mut t, vec![Event::PointerMoved(start)]);
        step(
            &mut app,
            &ctx,
            &mut t,
            vec![Event::PointerButton { pos: start, button: PointerButton::Secondary, pressed: true, modifiers: m }],
        );
        step(&mut app, &ctx, &mut t, vec![Event::PointerMoved(end)]);
        step(
            &mut app,
            &ctx,
            &mut t,
            vec![Event::PointerButton { pos: end, button: PointerButton::Secondary, pressed: false, modifiers: m }],
        );

        assert_eq!(app.crop, before, "the right button moved the crop box");
        assert!(app.pan.x > 0.0 && app.pan.y > 0.0, "the picture did not pan: {:?}", app.pan);
    }

    /// Whole pixels to start with: the picture is drawn as it is, and the crop
    /// edges land on the grid.
    #[test]
    fn the_pixelated_preview_is_the_default() {
        let ctx = egui::Context::default();
        assert!(App::with_ctx(&ctx).disable_smoothing);
    }

    /// The player is told to cut out the crop, so the picture on screen while
    /// a cropped clip plays is the selection and nothing else.
    #[test]
    fn playing_a_cropped_clip_shows_only_the_crop() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_src());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.info.is_some() && a.texture.is_some());

        app.crop = CropRect { x0: 100.0, y0: 50.0, x1: 420.0, y1: 230.0 };
        app.end = 6.0;
        app.pos = 0.0;
        app.start_playback(&ctx);
        wait_until(&mut app, &ctx, &mut t, "cropped playback", |a| {
            a.texture.as_ref().is_some_and(|t| t.size() == [320, 180])
        });

        // Pausing goes back to a still of the whole frame.
        app.stop_playback();
        wait_until(&mut app, &ctx, &mut t, "the whole frame again", |a| {
            a.texture.as_ref().is_some_and(|t| t.size() == [640, 360])
        });
    }

    /// Drags the bottom-right crop handle by a screen-space offset.
    fn drag_se(app: &mut App, ctx: &egui::Context, t: &mut f64, by: Vec2) {
        let m = Modifiers::default();
        let corner = pos2(app.disp.left() + app.crop.x1 * app.scale - 1.0, app.disp.top() + app.crop.y1 * app.scale - 1.0);
        step(app, ctx, t, vec![Event::PointerMoved(corner)]);
        step(
            app,
            ctx,
            t,
            vec![Event::PointerButton { pos: corner, button: PointerButton::Primary, pressed: true, modifiers: m }],
        );
        step(app, ctx, t, vec![Event::PointerMoved(corner + by)]);
        step(
            app,
            ctx,
            t,
            vec![Event::PointerButton { pos: corner + by, button: PointerButton::Primary, pressed: false, modifiers: m }],
        );
        step(app, ctx, t, vec![]);
    }

    #[test]
    fn without_smoothing_the_crop_snaps_to_whole_pixels() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_src());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.info.is_some() && a.texture.is_some());

        app.disable_smoothing = true;
        drag_se(&mut app, &ctx, &mut t, vec2(-97.3, -61.7));
        assert_eq!(app.crop.x1, app.crop.x1.round(), "x1 {}", app.crop.x1);
        assert_eq!(app.crop.y1, app.crop.y1.round(), "y1 {}", app.crop.y1);
        assert_eq!(app.crop.x0, 0.0);
        assert!(app.crop.x1 < 600.0 && app.crop.x1 > 300.0, "x1 {}", app.crop.x1);

        // With smoothing kept on the box is continuous, not snapped.
        app.crop = CropRect::full(640.0, 360.0);
        app.disable_smoothing = false;
        drag_se(&mut app, &ctx, &mut t, vec2(-97.3, -61.7));
        assert!(
            app.crop.x1.fract() != 0.0 || app.crop.y1.fract() != 0.0,
            "a smoothed preview should not snap: {:?}",
            app.crop
        );
    }

    #[test]
    fn clear_returns_the_window_to_the_drop_area() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        let src = make_src();
        app.open(&ctx, src.clone());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.info.is_some() && a.texture.is_some());

        // Something to clear: a crop, a trim, a playhead.
        app.crop = CropRect { x0: 10.0, y0: 10.0, x1: 320.0, y1: 180.0 };
        app.start = 1.0;
        app.end = 2.0;
        app.pos = 1.5;

        app.clear();
        step(&mut app, &ctx, &mut t, vec![]);
        assert!(app.info.is_none(), "the file is gone");
        assert!(app.texture.is_none(), "the picture is gone");
        assert!(!app.loading, "clearing is not a load");
        assert!(app.keyframes.is_empty());
        assert!(app.pending_t.is_none());
        assert!(app.plan().is_none(), "with no file there is nothing to export");
        assert_eq!(app.title, version_line(), "the title bar drops the file name");

        // Clearing must leave nothing stuck behind: opening again still works.
        app.open(&ctx, src);
        wait_until(&mut app, &ctx, &mut t, "preview after reopening", |a| a.info.is_some() && a.texture.is_some());
    }

    #[test]
    fn clear_while_loading_drops_the_late_probe() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_src());
        assert!(app.loading);
        app.clear();

        // The probe started before Clear is still running; its answer must be
        // thrown away when it arrives rather than reopening the file.
        std::thread::sleep(Duration::from_secs(3));
        for _ in 0..20 {
            step(&mut app, &ctx, &mut t, vec![]);
            assert!(!app.loading, "a cleared window must not show a spinner");
        }
        assert!(app.info.is_none(), "a probe from before Clear reopened the file");
        assert!(app.texture.is_none(), "a frame from before Clear was drawn");
    }

    #[test]
    fn about_window_renders() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        app.about_open = true;
        step(&mut app, &ctx, &mut t, vec![]);
        step(&mut app, &ctx, &mut t, vec![]);
        assert!(app.about_open);
    }

    #[test]
    fn gif_plan_is_always_saveable_and_capped() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        let src = make_src();
        app.open(&ctx, src);
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.texture.is_some());

        app.format = OutFormat::Gif;
        let p = app.plan().unwrap();
        assert_eq!(p.mode, Mode::Gif);
        assert_eq!(p.out_ext, "gif");
        assert!(p.can_save, "choosing GIF is itself a change, so Save must be enabled");
        let g = p.gif.unwrap();
        assert_eq!((g.width, g.height), (640, 360), "a picture under the cap is not scaled");
        assert_eq!((g.fps, g.colors), (15, 256));

        app.quality = Quality::Small;
        let g = app.plan().unwrap().gif.unwrap();
        assert_eq!((g.width, g.height), (480, 270), "the smaller quality steps the size down");
        assert_eq!((g.fps, g.colors), (10, 128));

        // And it really comes out as a GIF.
        app.quality = Quality::Medium;
        app.start = 1.0;
        app.end = 3.0;
        let out = std::env::temp_dir().join("vc_ui_tests").join("ui_anim.gif");
        let _ = std::fs::remove_file(&out);
        app.begin_export(&ctx, out.clone());
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "gif export", |a| a.export.is_none());
        assert!(
            matches!(app.notice, Some(Notice::Saved(_))),
            "notice: {:?}",
            match &app.notice {
                Some(Notice::Error(e)) => e.clone(),
                _ => String::new(),
            }
        );
        assert!(out.exists());
    }

    #[test]
    fn the_audio_formats_write_the_sound_and_no_picture() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_src());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.texture.is_some());

        // A box on the picture means nothing to the sound, and picking an audio
        // format is itself something to save.
        app.crop = crop::fit_aspect(640.0, 360.0, 1.0);
        app.format = OutFormat::Audio;
        let p = app.plan().unwrap();
        assert_eq!(p.mode, Mode::Audio);
        assert_eq!(p.out_ext, "m4a", "AAC is copied into .m4a");
        assert!(p.crop_px.is_none(), "a crop must not follow the sound out");
        assert!(p.can_save, "choosing an audio format is a change");
        assert!(p.warn.is_none(), "{:?}", p.warn);

        app.start = 1.0;
        app.end = 3.0;
        let out = std::env::temp_dir().join("vc_ui_tests").join("ui_sound.m4a");
        let _ = std::fs::remove_file(&out);
        app.begin_export(&ctx, out.clone());
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "audio export", |a| a.export.is_none());
        assert!(
            matches!(app.notice, Some(Notice::Saved(_))),
            "notice: {:?}",
            match &app.notice {
                Some(Notice::Error(e)) => e.clone(),
                _ => String::new(),
            }
        );
        let d = probe_raw(&out);
        assert!(d.contains("codec_name=aac"), "{d}");
        assert!(d.contains("codec_type=audio"), "{d}");
        assert!(!d.contains("codec_type=video"), "the export kept the picture:\n{d}");

        // MP3 re-encodes, and the quality step is its bitrate.
        app.format = OutFormat::Mp3;
        app.quality = Quality::Small;
        let p = app.plan().unwrap();
        assert_eq!((p.mode, p.out_ext.as_str()), (Mode::Audio, "mp3"));
        let out2 = std::env::temp_dir().join("vc_ui_tests").join("ui_sound.mp3");
        let _ = std::fs::remove_file(&out2);
        app.begin_export(&ctx, out2.clone());
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "mp3 export", |a| a.export.is_none());
        let d = probe_raw(&out2);
        assert!(d.contains("codec_name=mp3"), "{d}");
        assert!(!d.contains("codec_type=video"), "{d}");
    }

    #[test]
    fn extracting_audio_from_a_silent_video_is_refused() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_silent_src());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.info.is_some() && a.texture.is_some());
        assert!(app.info.as_ref().unwrap().a_codec.is_none());

        app.format = OutFormat::Audio;
        let p = app.plan().unwrap();
        assert!(!(p.can_save && p.warn.is_none()), "there is no sound to save");
        assert_eq!(p.warn.as_deref(), Some("This video has no audio track to extract."));
    }

    #[test]
    fn full_flow_drag_crop_export_and_cut() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        let src = make_src();
        app.open(&ctx, src.clone());
        wait_until(&mut app, &ctx, &mut t, "preview + keyframes", |a| a.texture.is_some() && !a.keyframes.is_empty());
        let info = app.info.clone().unwrap();
        assert_eq!((info.width, info.height), (640, 360));
        assert!(app.disp.width() > 100.0 && app.scale > 0.0);

        // Nothing changed: nothing to save (metadata kept).
        let p = app.plan().unwrap();
        assert_eq!(p.mode, Mode::Remux, "with no cut and no crop only the tags could change");
        assert!(!p.can_save);
        app.keep_metadata = false;
        assert!(app.plan().unwrap().can_save, "stripping metadata alone is a valid save");
        assert_eq!(app.plan().unwrap().mode, Mode::Remux, "stripping tags does not need a re-encode");
        app.keep_metadata = true;

        // Drag the bottom-right crop handle up-left with the real pointer events.
        let (disp, scale) = (app.disp, app.scale);
        let corner = pos2(disp.left() + app.crop.x1 * scale - 1.0, disp.top() + app.crop.y1 * scale - 1.0);
        let m = Modifiers::default();
        step(&mut app, &ctx, &mut t, vec![Event::PointerMoved(corner)]);
        step(&mut app, &ctx, &mut t, vec![Event::PointerButton { pos: corner, button: PointerButton::Primary, pressed: true, modifiers: m }]);
        for d in [20.0, 60.0, 100.0, 140.0] {
            step(&mut app, &ctx, &mut t, vec![Event::PointerMoved(corner - vec2(d, d * 0.6))]);
        }
        step(&mut app, &ctx, &mut t, vec![Event::PointerButton { pos: corner - vec2(140.0, 84.0), button: PointerButton::Primary, pressed: false, modifiers: m }]);
        step(&mut app, &ctx, &mut t, vec![]);
        assert!(app.crop.x1 < 640.0 - 100.0 / scale * 0.9, "crop did not move: {:?}", app.crop);
        assert!(app.crop.y1 < 360.0 - 50.0 / scale * 0.9, "crop did not move: {:?}", app.crop);
        assert!(app.crop.x0 == 0.0 && app.crop.y0 == 0.0);
        let p = app.plan().unwrap();
        assert_eq!(p.mode, Mode::Video);
        let (cw, ch, _, _) = p.crop_px.unwrap();

        // Aspect preset snaps the box (then put the dragged box back).
        let dragged = app.crop;
        app.aspect = Aspect::Square;
        app.crop = crop::fit_aspect(640.0, 360.0, 1.0);
        let (sw, sh, _, _) = app.plan().unwrap().crop_px.unwrap();
        assert_eq!((sw, sh), (360, 360));
        app.aspect = Aspect::Free;
        app.crop = dragged;

        // Export the dragged crop (re-encode).
        let out = std::env::temp_dir().join("vc_ui_tests").join("ui_cropped.mp4");
        let _ = std::fs::remove_file(&out);
        app.begin_export(&ctx, out.clone());
        assert!(app.export.is_some());
        step(&mut app, &ctx, &mut t, vec![]); // renders progress UI
        wait_until(&mut app, &ctx, &mut t, "crop export", |a| a.export.is_none());
        assert!(matches!(app.notice, Some(Notice::Saved(_))), "notice: {:?}", match &app.notice { Some(Notice::Error(e)) => e.clone(), _ => String::new() });
        let b = ffmpeg::find_bins().unwrap();
        let oi = ffmpeg::probe(&b, &out).unwrap();
        assert_eq!((oi.width, oi.height), (cw, ch));
        step(&mut app, &ctx, &mut t, vec![]); // renders the "Saved" notice

        // Reset crop and trim 2.4 .. 6.0. A cut always re-encodes, and it starts
        // exactly where the trim says - no keyframe shifting.
        app.crop = CropRect::full(640.0, 360.0);
        app.start = 2.4;
        app.end = 6.0;
        let p = app.plan().unwrap();
        assert_eq!(p.mode, Mode::Video);
        assert!((p.eff_start - 2.4).abs() < 0.001, "eff_start {}", p.eff_start);
        assert!((p.dur - 3.6).abs() < 0.01, "dur {}", p.dur);
        let out2 = std::env::temp_dir().join("vc_ui_tests").join("ui_cut.mp4");
        let _ = std::fs::remove_file(&out2);
        app.begin_export(&ctx, out2.clone());
        wait_until(&mut app, &ctx, &mut t, "cut export", |a| a.export.is_none());
        let oi = ffmpeg::probe(&b, &out2).unwrap();
        assert_eq!((oi.width, oi.height, oi.v_codec.as_str()), (640, 360, "h264"));
        assert!((oi.duration - 3.6).abs() < 0.2, "dur {}", oi.duration);

        // Opening a second file mid-session resets state.
        app.open(&ctx, src);
        wait_until(&mut app, &ctx, &mut t, "reopen", |a| a.texture.is_some() && a.info.is_some());
        assert!(app.crop.is_full(640.0, 360.0) && app.start == 0.0);
    }

    /// The output volume is opted into, and only for a sound that is kept:
    /// unticked, at 100%, with the track dropped, or for a GIF it changes
    /// nothing at all.
    #[test]
    fn the_output_volume_is_opted_into_and_only_for_kept_sound() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx);
        let mut t = 0.0;
        step(&mut app, &ctx, &mut t, vec![]);
        wait_until(&mut app, &ctx, &mut t, "ffmpeg", |a| a.bins().is_some());

        app.open(&ctx, make_src());
        wait_until(&mut app, &ctx, &mut t, "preview", |a| a.info.is_some() && a.texture.is_some());

        app.volume = 150.0;
        assert!(!app.apply_volume, "the box starts off");
        assert!(app.plan().unwrap().volume.is_none(), "unticked, the save leaves the sound alone");
        assert!(!app.plan().unwrap().can_save, "and nothing else changed either");

        app.apply_volume = true;
        let p = app.plan().unwrap();
        assert_eq!(p.volume, Some(1.5));
        assert!(p.can_save, "a volume change alone is something to save");
        assert_eq!(p.mode, Mode::Remux, "the picture is copied, only the sound is re-encoded");
        assert_eq!(p.out_ext, "mp4");

        app.volume = 100.0;
        assert!(app.plan().unwrap().volume.is_none(), "100% is no change");
        app.volume = 150.0;

        app.keep_audio = false;
        assert!(app.plan().unwrap().volume.is_none(), "a dropped track has no volume");
        app.keep_audio = true;

        app.format = OutFormat::Audio;
        assert_eq!(app.plan().unwrap().volume, Some(1.5), "an audio export takes it too");

        app.format = OutFormat::Gif;
        assert!(app.plan().unwrap().volume.is_none(), "a GIF has no sound to turn");

        // A cut carries it too, and the sound is encoded into the video file.
        app.format = OutFormat::Same;
        app.start = 1.0;
        app.end = 2.0;
        let p = app.plan().unwrap();
        assert_eq!((p.mode, p.volume), (Mode::Video, Some(1.5)));

        // A container that cannot hold AAC becomes MKV rather than failing.
        app.open(&ctx, make_avi());
        wait_until(&mut app, &ctx, &mut t, "avi preview", |a| {
            a.info.as_ref().map(|i| i.ext == "avi").unwrap_or(false) && a.texture.is_some()
        });
        let p = app.plan().unwrap();
        assert_eq!((p.mode, p.out_ext.as_str()), (Mode::Remux, "mkv"), "AAC does not fit in AVI");
        assert!(p.warn.is_none(), "{:?}", p.warn);
        assert!(p.can_save, "the volume is the change being saved");
    }
}
