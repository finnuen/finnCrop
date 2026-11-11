use crate::crop::{self, Aspect, CropRect, Handle};
use crate::ffmpeg::{self, Bins, Job, Mode, VideoInfo};
use crate::timeline;
use crate::version_line;
use eframe::egui::{self, pos2, vec2, Color32, CursorIcon, Rect, RichText, Sense, Stroke};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "3gp", "mpg", "mpeg", "ts", "mts", "m2ts", "ogv", "gif",
];

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
}

impl OutFormat {
    fn label(self) -> &'static str {
        match self {
            OutFormat::Same => "Same as source",
            OutFormat::Mp4 => "MP4",
            OutFormat::Mkv => "MKV",
            OutFormat::Mov => "MOV",
            OutFormat::Gif => "GIF",
        }
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
    /// Geometry and frame rate, when the output is a GIF.
    gif: Option<ffmpeg::GifOpts>,
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
    drag: Option<Handle>,
    pos: f64,
    start: f64,
    end: f64,
    snap_start: bool,
    precise: bool,
    keep_metadata: bool,
    format: OutFormat,
    quality: Quality,
    export: Option<Exporting>,
    /// Playback, when it is running.
    player: Option<ffmpeg::Player>,
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
        Self::with_ctx(&cc.egui_ctx)
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
            pos: 0.0,
            start: 0.0,
            end: 0.0,
            snap_start: true,
            precise: false,
            keep_metadata: true,
            format: OutFormat::Same,
            quality: Quality::High,
            export: None,
            player: None,
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
    /// Output preferences (format, quality, precise cut, metadata, snapping)
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
                            self.pos = if info.duration > 1.0 { (info.duration * 0.1).min(3.0) } else { 0.0 };
                            self.pending_t = Some(self.pos);
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
                                self.texture = Some(ctx.load_texture("frame", img, egui::TextureOptions::LINEAR));
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
            (t, bins, info.path.clone(), info.width, info.height, info.duration)
        };
        self.pending_t = None;
        self.frame_in_flight = true;
        let (t, bins, path, w, h, dur) = job;
        let (tx, ctx2, gen) = (self.tx.clone(), ctx.clone(), self.gen);
        std::thread::spawn(move || {
            let r = ffmpeg::grab_frame(&bins, &path, t.min(dur - 0.05).max(0.0), w, h);
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

        let ctx2 = ctx.clone();
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx2.request_repaint());
        match ffmpeg::Player::start(&bins, &info.path, self.pos, self.end, info.width, info.height, repaint) {
            Ok(p) => {
                self.player = Some(p);
                self.notice = None;
            }
            Err(e) => self.notice = Some(Notice::Error(e)),
        }
        ctx.request_repaint();
    }

    /// Dropping the player kills the decoder and joins its reader thread.
    fn stop_playback(&mut self) {
        self.player = None;
    }

    fn pump_playback(&mut self, ctx: &egui::Context) {
        let Some(player) = self.player.as_ref() else { return };

        if let Some((w, h, data)) = player.take_frame() {
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &data);
            match &mut self.texture {
                Some(tex) if tex.size() == [w, h] => tex.set(img, egui::TextureOptions::LINEAR),
                _ => {
                    self.texture = Some(ctx.load_texture("frame", img, egui::TextureOptions::LINEAR));
                }
            }
        }

        // Follow the decoder, not the frames the window had time to draw.
        self.play_frames = player.decoded();
        if self.play_frames > 0 {
            self.pos = self.play_from + self.played();
        }

        let ended = player.finished() || self.pos >= self.end - 0.001;
        if ended {
            self.stop_playback();
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
        let mode = if gif {
            Mode::Gif
        } else if cropped {
            Mode::Crop
        } else if self.precise && trim_changed {
            Mode::Precise
        } else {
            Mode::FastCut
        };

        let out_ext = match self.format {
            OutFormat::Gif => "gif".to_string(),
            OutFormat::Mp4 => "mp4".to_string(),
            OutFormat::Mkv => "mkv".to_string(),
            OutFormat::Mov => "mov".to_string(),
            OutFormat::Same => {
                if mode != Mode::FastCut && !matches!(info.ext.as_str(), "mp4" | "m4v" | "mov" | "mkv") {
                    "mp4".to_string()
                } else {
                    info.ext.clone()
                }
            }
        };

        let mut eff_start = self.start;
        if mode == Mode::FastCut && self.snap_start {
            if let Some(k) = self.keyframes.iter().rev().find(|&&k| k <= self.start + 0.001) {
                eff_start = *k;
            }
        }
        let dur = (self.end - eff_start).max(0.05);
        let warn = if mode == Mode::FastCut { ffmpeg::copy_compat(info, &out_ext) } else { None };
        // GIF is always a re-encode, so there is always something to save.
        let can_save = gif || cropped || trim_changed || !self.keep_metadata;
        let crop_px = if cropped { Some(self.crop.to_pixels(info.width, info.height)) } else { None };
        let (gw, gh) = match crop_px {
            Some((w, h, _, _)) => (w, h),
            None => (info.width, info.height),
        };
        let gif = if gif { Some(self.quality.gif_opts(gw, gh)) } else { None };
        Some(Plan { mode, cropped, trim_changed, can_save, warn, out_ext, eff_start, dur, crop_px, gif })
    }

    fn start_export(&mut self, ctx: &egui::Context) {
        let (Some(plan), Some(info)) = (self.plan(), self.info.clone()) else {
            return;
        };
        let stem = info.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "video".into());
        let suffix = if plan.cropped {
            "_cropped"
        } else if plan.mode == Mode::Gif {
            "_gif"
        } else if plan.trim_changed {
            "_cut"
        } else {
            "_clean"
        };
        let filter = if plan.mode == Mode::Gif { "GIF" } else { "Video" };
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
            crf: self.quality.crf(),
            audio_aac: false,
            gif: plan.gif,
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
        let scale = (avail.width() / fw).min(avail.height() / fh).max(0.01);
        let disp = Rect::from_center_size(avail.center(), vec2(fw * scale, fh * scale));
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
        let busy = self.export.is_some();
        let playing = self.is_playing();
        let resp = ui.interact(disp.expand(12.0), ui.id().with("crop_area"), Sense::click_and_drag());
        // A click on the picture pauses, the way a player behaves.
        if playing && resp.clicked() {
            self.stop_playback();
        }
        if !busy && !playing {
            let sr = to_screen(&self.crop);
            let hover = resp.hover_pos().and_then(|p| crop::hit_test(&sr, p.x, p.y, 10.0));
            if resp.drag_started() {
                let origin = ui.input(|i| i.pointer.press_origin());
                self.drag = origin.and_then(|p| crop::hit_test(&sr, p.x, p.y, 10.0));
            }
            if resp.dragged() {
                if let Some(h) = self.drag {
                    let d = resp.drag_delta() / scale;
                    crop::apply_drag(&mut self.crop, h, d.x, d.y, fw, fh, 16.0, self.aspect.ratio());
                }
            }
            if resp.drag_stopped() {
                self.drag = None;
            }
            if let Some(h) = self.drag.or(hover) {
                ctx.set_cursor_icon(match h {
                    Handle::Move => CursorIcon::Move,
                    Handle::N | Handle::S => CursorIcon::ResizeVertical,
                    Handle::E | Handle::W => CursorIcon::ResizeHorizontal,
                    Handle::NE | Handle::SW => CursorIcon::ResizeNeSw,
                    Handle::NW | Handle::SE => CursorIcon::ResizeNwSe,
                });
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
                    for f in [OutFormat::Same, OutFormat::Mp4, OutFormat::Mkv, OutFormat::Mov, OutFormat::Gif] {
                        ui.selectable_value(&mut self.format, f, f.label());
                    }
                });
                let reencode = plan.as_ref().map(|p| p.mode != Mode::FastCut).unwrap_or(false);
                ui.add_enabled_ui(reencode, |ui| {
                    egui::ComboBox::from_id_salt("quality").selected_text(self.quality.label()).show_ui(ui, |ui| {
                        for q in [Quality::High, Quality::Medium, Quality::Small] {
                            ui.selectable_value(&mut self.quality, q, q.label());
                        }
                    });
                });
                let gif = plan.as_ref().map(|p| p.mode == Mode::Gif).unwrap_or(false);
                ui.add_enabled(
                    !gif,
                    egui::Checkbox::new(&mut self.keep_metadata, "Keep metadata"),
                )
                .on_hover_text(
                    "Off: removes tags such as title, creation date, GPS location and encoder info.\nVideo and audio are not re-encoded just for this.",
                )
                .on_disabled_hover_text("A GIF has no place to keep the tags.");
                let cropped = plan.as_ref().map(|p| p.cropped).unwrap_or(false);
                ui.add_enabled(
                    !cropped && !gif,
                    egui::Checkbox::new(&mut self.precise, "Precise cut (re-encode)"),
                )
                .on_hover_text("Frame-accurate trim, but slower and re-encodes the video.\nCropping always re-encodes.");
                ui.add_enabled(
                    !self.keyframes.is_empty() && !cropped && !self.precise && !gif,
                    egui::Checkbox::new(&mut self.snap_start, "Snap start to keyframe"),
                )
                .on_hover_text("Fast cut can only start cleanly on a keyframe.\nOn: the cut starts at the keyframe at or before your start point.");
            });
        });

        ui.add_space(4.0);
        // Mode summary.
        if let Some(p) = &plan {
            let (text, color) = match p.mode {
                Mode::Gif => {
                    let g = p.gif.unwrap_or_default();
                    (
                        format!(
                            "GIF: {} x {} at {} fps, {} colours, silent, loops forever. Re-encoded in two passes.",
                            g.width, g.height, g.fps, g.colors
                        ),
                        Color32::from_rgb(240, 180, 90),
                    )
                }
                Mode::Crop => ("Crop: the video is re-encoded (audio is copied).".to_string(), Color32::from_rgb(240, 180, 90)),
                Mode::Precise => ("Precise cut: re-encoded, frame-accurate.".to_string(), Color32::from_rgb(240, 180, 90)),
                Mode::FastCut if p.trim_changed => (
                    "Fast cut: no re-encoding, lossless and near-instant.".to_string(),
                    Color32::from_rgb(110, 200, 140),
                ),
                Mode::FastCut => ("No re-encoding: the streams are copied as they are.".to_string(), Color32::from_rgb(110, 200, 140)),
            };
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(color, text);
                if p.trim_changed {
                    ui.label(RichText::new(format!("Output length {}", fmt_time(p.dur))).weak());
                }
            });
            if p.mode == Mode::FastCut && p.trim_changed && self.snap_start && (p.eff_start - self.start).abs() > 0.001 {
                ui.label(
                    RichText::new(format!("Start snaps to the keyframe at {}", fmt_time(p.eff_start))).weak(),
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
                ui.label("Lossless cutting and cropping for video files.");
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
    fn full_flow_drag_crop_export_and_fast_cut() {
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

        // Nothing changed: no re-encode, nothing to save (metadata kept).
        let p = app.plan().unwrap();
        assert_eq!(p.mode, Mode::FastCut);
        assert!(!p.can_save);
        app.keep_metadata = false;
        assert!(app.plan().unwrap().can_save, "stripping metadata alone is a valid (lossless) save");
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
        assert_eq!(p.mode, Mode::Crop);
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

        // Reset crop, trim 2.4 .. 6.0 with fast cut: start snaps back to the 2.0 keyframe.
        app.crop = CropRect::full(640.0, 360.0);
        app.start = 2.4;
        app.end = 6.0;
        let p = app.plan().unwrap();
        assert_eq!(p.mode, Mode::FastCut);
        assert!((p.eff_start - 2.0).abs() < 0.01, "eff_start {}", p.eff_start);
        assert!((p.dur - 4.0).abs() < 0.01);
        let out2 = std::env::temp_dir().join("vc_ui_tests").join("ui_cut.mp4");
        let _ = std::fs::remove_file(&out2);
        app.begin_export(&ctx, out2.clone());
        wait_until(&mut app, &ctx, &mut t, "fast cut export", |a| a.export.is_none());
        let oi = ffmpeg::probe(&b, &out2).unwrap();
        assert_eq!((oi.width, oi.height, oi.v_codec.as_str()), (640, 360, "h264"));
        assert!((oi.duration - 4.0).abs() < 0.3, "dur {}", oi.duration);

        // Precise cut turns into a re-encode; snapping is off.
        app.precise = true;
        let p = app.plan().unwrap();
        assert_eq!(p.mode, Mode::Precise);
        assert!((p.eff_start - 2.4).abs() < 0.001);

        // Opening a second file mid-session resets state.
        app.open(&ctx, src);
        wait_until(&mut app, &ctx, &mut t, "reopen", |a| a.texture.is_some() && a.info.is_some());
        assert!(app.crop.is_full(640.0, 360.0) && app.start == 0.0);
    }
}
