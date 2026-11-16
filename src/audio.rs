//! The sound that plays with the preview.
//!
//! The picture comes from `ffmpeg::Player`, which decodes video and nothing
//! else. This is the other half: a second ffmpeg turns the same trimmed range
//! into 16-bit stereo PCM, and a thread hands that to the sound card.
//!
//! No crates are involved. waveOut is a handful of functions in winmm.dll, and
//! a preview needs nothing richer than "play these bytes, from here, now": no
//! mixer, no filters, no seeking. The card paces the thread, so the sound
//! cannot drift away from the clock the picture is paced by.

use crate::ffmpeg::{self, Bins, Res};
use std::ffi::OsString;
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What the sound is decoded to. Windows resamples to whatever the card runs
/// at, so one rate covers every device; 44.1 kHz is the fallback for a card
/// that will not take it.
const RATE: u32 = 48_000;
const FALLBACK_RATE: u32 = 44_100;
const CHANNELS: u16 = 2;
const BITS: u16 = 16;
/// Bytes in one sample frame: all channels, one sample each.
const BLOCK: usize = CHANNELS as usize * (BITS / 8) as usize;

/// One hand-over to the device: about 43 ms of sound. Big enough that the
/// thread is not woken constantly, small enough that a pause happens now.
const CHUNK: usize = 8192;
/// Buffers kept in flight, so one slow read does not leave a gap in the sound.
const BUFFERS: usize = 4;

/// The preview's loudness, held in thousandths so the window and the sound
/// thread can share it without a lock: 1000 is the source as it is, 2000 is
/// twice as loud.
const GAIN_ONE: u32 = 1000;
/// 200%: the top of the volume box. waveOutSetVolume cannot go past 100%, which is
/// why the scaling happens to the samples before the card sees them.
const GAIN_MAX: u32 = 2000;

/// The sound for one playback: from `from` to `until`, 16-bit stereo.
///
/// Created when playback starts, dropped when it stops. The thread waits for
/// `begin` before it plays anything, because the two decoders seek from
/// scratch and the picture is the slower of them: sound that started at its
/// own first byte would run ahead of the video.
pub struct Sound {
    begun: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<std::process::Child>>>,
    /// Thousandths of the source's loudness, read afresh for every buffer.
    gain: Arc<AtomicU32>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Sound {
    /// Starts decoding and returns at once; nothing is heard until [`begin`].
    ///
    /// [`begin`]: Sound::begin
    pub fn start(bins: &Bins, path: &Path, from: f64, until: f64, gain: f32) -> Res<Self> {
        #[cfg(not(windows))]
        {
            let _ = (bins, path, from, until, gain);
            return Err("Sound is played on Windows only".to_string());
        }

        #[cfg(windows)]
        {
            // The device is opened first: its rate decides what ffmpeg is asked
            // for, and a machine with no working output stops here rather than
            // decoding into nothing.
            let device = Device::open().ok_or_else(no_device)?;
            let mut child = ffmpeg::command(&bins.ffmpeg)
                .args(pcm_args(path, from, until, device.rate))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("Could not start the sound: {e}"))?;
            let pcm = child.stdout.take().ok_or("ffmpeg gave no sound to read")?;

            let begun = Arc::new(AtomicBool::new(false));
            let cancel = Arc::new(AtomicBool::new(false));
            let level = Arc::new(AtomicU32::new(to_milli(gain)));
            let thread = {
                let (b, c, g) = (begun.clone(), cancel.clone(), level.clone());
                std::thread::spawn(move || sink(pcm, device, b, c, g))
            };
            Ok(Self {
                begun,
                cancel,
                child: Arc::new(Mutex::new(Some(child))),
                gain: level,
                thread: Some(thread),
            })
        }
    }

    /// The first picture is on screen: let the sound follow it.
    pub fn begin(&self) {
        self.begun.store(true, Ordering::Relaxed);
    }

    /// Changes the loudness while it plays. The next buffer is filled at the
    /// new level, ramped across its ~43 ms so a drag glides instead of clicking.
    pub fn set_gain(&self, gain: f32) {
        self.gain.store(to_milli(gain), Ordering::Relaxed);
    }

    /// Stops the sound now. Safe to call more than once; also runs on drop.
    ///
    /// The decoder has to be killed before the thread is joined: that thread is
    /// blocked reading the pipe, and only a dead ffmpeg closes it.
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

impl Drop for Sound {
    fn drop(&mut self) {
        self.stop();
    }
}

fn no_device() -> String {
    "No sound device would open".to_string()
}

/// What ffmpeg is asked for: the trimmed range, first audio track, no picture,
/// raw little-endian samples on stdout.
fn pcm_args(path: &Path, from: f64, until: f64, rate: u32) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    for s in ["-hide_banner", "-loglevel", "error", "-ss"] {
        a.push(s.into());
    }
    a.push(ffmpeg::fmt_secs(from).into());
    a.push("-i".into());
    a.push(path.as_os_str().to_os_string());
    a.push("-t".into());
    a.push(ffmpeg::fmt_secs((until - from).max(0.05)).into());
    for s in ["-map", "0:a:0", "-vn", "-sn", "-dn", "-ac"] {
        a.push(s.into());
    }
    a.push(CHANNELS.to_string().into());
    a.push("-ar".into());
    a.push(rate.to_string().into());
    for s in ["-f", "s16le", "-"] {
        a.push(s.into());
    }
    a
}

/// Fills `buf` as far as the decoder has data for. A short read is the end of
/// the range: the last hand-over of a clip is normally shorter than a buffer.
fn read_upto(r: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(_) => break,
        }
    }
    n
}

/// A gain as thousandths, clamped to what the volume box offers.
fn to_milli(gain: f32) -> u32 {
    ((gain as f64 * GAIN_ONE as f64).round() as i64).clamp(0, GAIN_MAX as i64) as u32
}

/// One sample at one gain. Rounded to the nearest step and clipped rather than
/// wrapped: a boosted sample that runs out of range becomes the loudest the
/// format can hold, which is what a volume control should do.
#[cfg_attr(not(windows), allow(dead_code))]
fn scaled(v: i16, milli: u32) -> i16 {
    if milli == GAIN_ONE {
        return v;
    }
    let x = v as i32 * milli as i32;
    let half = GAIN_ONE as i32 / 2;
    let r = if x >= 0 { (x + half) / GAIN_ONE as i32 } else { (x - half) / GAIN_ONE as i32 };
    r.clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// Scales a whole buffer, ramping from the level it last had to the one asked
/// for, so a moving box glides instead of stepping at buffer boundaries.
/// Both channels of a frame always get the same gain. Returns the level the
/// next buffer starts from.
#[cfg_attr(not(windows), allow(dead_code))]
fn apply_gain(buf: &mut [u8], from: u32, to: u32) -> u32 {
    let frames = buf.len() / BLOCK;
    if frames == 0 {
        return to;
    }
    if from == to && from == GAIN_ONE {
        return to;
    }
    let span = to as i64 - from as i64;
    for f in 0..frames {
        let milli = if frames == 1 {
            to
        } else {
            // The last frame lands exactly on the new level.
            (from as i64 + span * f as i64 / (frames as i64 - 1)) as u32
        };
        let o = f * BLOCK;
        for ch in 0..CHANNELS as usize {
            let i = o + ch * 2;
            let v = i16::from_le_bytes([buf[i], buf[i + 1]]);
            buf[i..i + 2].copy_from_slice(&scaled(v, milli).to_le_bytes());
        }
    }
    to
}

#[cfg(windows)]
mod sys {
    //! The calls in winmm this needs, and the two structures they take.

    #[repr(C)]
    pub struct WaveFormatEx {
        pub format_tag: u16,
        pub channels: u16,
        pub samples_per_sec: u32,
        pub avg_bytes_per_sec: u32,
        pub block_align: u16,
        pub bits: u16,
        pub extra: u16,
    }

    /// Field offsets are what matter to winmm; the trailing padding of the
    /// Rust struct is never read.
    #[repr(C)]
    pub struct WaveHdr {
        pub data: *mut u8,
        pub length: u32,
        pub recorded: u32,
        pub user: usize,
        pub flags: u32,
        pub loops: u32,
        pub next: *mut WaveHdr,
        pub reserved: usize,
    }

    /// Where the device has got to: the kind of count, then the count. The C
    /// structure is 12 bytes, because its union is as wide as the MIDI member;
    /// a short size is refused outright.
    #[cfg(test)]
    #[repr(C)]
    pub struct MmTime {
        pub kind: u32,
        pub value: u32,
        pub extra: u32,
    }

    pub const WAVE_MAPPER: usize = usize::MAX;
    pub const CALLBACK_NULL: u32 = 0;
    pub const WHDR_DONE: u32 = 0x0000_0001;
    #[cfg(test)]
    pub const TIME_SAMPLES: u32 = 0x0002;
    #[cfg(test)]
    pub const TIME_BYTES: u32 = 0x0004;

    #[link(name = "winmm")]
    extern "system" {
        pub fn waveOutOpen(
            handle: *mut isize,
            device: usize,
            format: *const WaveFormatEx,
            callback: usize,
            instance: usize,
            flags: u32,
        ) -> u32;
        pub fn waveOutPrepareHeader(handle: isize, hdr: *mut WaveHdr, size: u32) -> u32;
        pub fn waveOutWrite(handle: isize, hdr: *mut WaveHdr, size: u32) -> u32;
        pub fn waveOutReset(handle: isize) -> u32;
        #[cfg(test)]
        pub fn waveOutGetPosition(handle: isize, time: *mut MmTime, size: u32) -> u32;
        pub fn waveOutUnprepareHeader(handle: isize, hdr: *mut WaveHdr, size: u32) -> u32;
        pub fn waveOutClose(handle: isize) -> u32;
    }
}

#[cfg(windows)]
struct Slot {
    hdr: Box<sys::WaveHdr>,
    data: Vec<u8>,
    /// Written to the device and not yet finished with.
    busy: bool,
}

#[cfg(windows)]
struct Device {
    handle: isize,
    /// The rate the device accepted, in Hz.
    rate: u32,
    slots: Vec<Slot>,
}

// The device handle and its buffers travel together and are only ever touched
// by the thread that owns them; the raw pointers inside WAVEHDR are what makes
// the compiler unwilling.
#[cfg(windows)]
unsafe impl Send for Slot {}
#[cfg(windows)]
unsafe impl Send for Device {}

#[cfg(windows)]
impl Device {
    /// Opens the default output at 48 kHz, or 44.1 kHz if it insists.
    fn open() -> Option<Device> {
        for rate in [RATE, FALLBACK_RATE] {
            let format = sys::WaveFormatEx {
                format_tag: 1, // WAVE_FORMAT_PCM
                channels: CHANNELS,
                samples_per_sec: rate,
                avg_bytes_per_sec: rate * BLOCK as u32,
                block_align: BLOCK as u16,
                bits: BITS,
                extra: 0,
            };
            let mut handle: isize = 0;
            let opened =
                unsafe { sys::waveOutOpen(&mut handle, sys::WAVE_MAPPER, &format, 0, 0, sys::CALLBACK_NULL) };
            if opened != 0 {
                continue;
            }
            let mut device = Device { handle, rate, slots: Vec::new() };
            let size = std::mem::size_of::<sys::WaveHdr>() as u32;
            let mut ok = true;
            for _ in 0..BUFFERS {
                let mut data = vec![0u8; CHUNK];
                let mut hdr = Box::new(sys::WaveHdr {
                    data: data.as_mut_ptr(),
                    length: CHUNK as u32,
                    recorded: 0,
                    user: 0,
                    flags: 0,
                    loops: 0,
                    next: std::ptr::null_mut(),
                    reserved: 0,
                });
                if unsafe { sys::waveOutPrepareHeader(handle, hdr.as_mut(), size) } != 0 {
                    ok = false;
                    break;
                }
                device.slots.push(Slot { hdr, data, busy: false });
            }
            if ok {
                return Some(device);
            }
            // Dropping closes the device and puts back whatever was prepared.
        }
        None
    }

    fn done(slot: &Slot) -> bool {
        let flags = unsafe { std::ptr::read_volatile::<u32>(&slot.hdr.flags) };
        flags & sys::WHDR_DONE != 0
    }

    /// A buffer the device has finished with, waiting for one if every buffer
    /// is still ahead of the playhead.
    fn free(&self, cancel: &AtomicBool) -> Option<usize> {
        loop {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(i) = self.slots.iter().position(|s| !s.busy || Device::done(s)) {
                return Some(i);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn play(&mut self, bytes: &[u8], cancel: &AtomicBool) -> bool {
        let Some(i) = self.free(cancel) else { return false };
        let slot = &mut self.slots[i];
        let n = bytes.len().min(slot.data.len());
        slot.data[..n].copy_from_slice(&bytes[..n]);
        slot.hdr.length = n as u32;
        slot.hdr.flags &= !sys::WHDR_DONE;
        slot.busy = true;
        let size = std::mem::size_of::<sys::WaveHdr>() as u32;
        unsafe { sys::waveOutWrite(self.handle, slot.hdr.as_mut(), size) == 0 }
    }

    fn idle(&self) -> bool {
        self.slots.iter().all(|s| !s.busy || Device::done(s))
    }

    /// Sample frames the device has played since it was opened. Drivers may
    /// answer in bytes instead, which is the same count in another unit.
    #[cfg(test)]
    fn played_samples(&self) -> u32 {
        let mut t = sys::MmTime { kind: sys::TIME_SAMPLES, value: 0, extra: 0 };
        let size = std::mem::size_of::<sys::MmTime>() as u32;
        if unsafe { sys::waveOutGetPosition(self.handle, &mut t, size) } != 0 {
            return 0;
        }
        match t.kind {
            sys::TIME_SAMPLES => t.value,
            sys::TIME_BYTES => t.value / BLOCK as u32,
            _ => 0,
        }
    }
}

#[cfg(windows)]
impl Drop for Device {
    fn drop(&mut self) {
        let handle = self.handle;
        let size = std::mem::size_of::<sys::WaveHdr>() as u32;
        unsafe {
            sys::waveOutReset(handle);
            for slot in &mut self.slots {
                sys::waveOutUnprepareHeader(handle, slot.hdr.as_mut(), size);
            }
            sys::waveOutClose(handle);
        }
    }
}

/// The thread: waits for the picture, then feeds the card from the pipe.
#[cfg(windows)]
fn sink(
    mut pcm: impl Read,
    mut device: Device,
    begun: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    gain: Arc<AtomicU32>,
) {
    while !begun.load(Ordering::Relaxed) {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    let mut milli = gain.load(Ordering::Relaxed);
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let n = read_upto(&mut pcm, &mut buf);
        if n == 0 {
            break; // end of the trimmed range
        }
        // Read the volume once per buffer: the ramp then covers the whole
        // hand-over, and a fast drag makes one smooth step per 43 ms.
        let now = gain.load(Ordering::Relaxed);
        milli = apply_gain(&mut buf[..n], milli, now);
        if !device.play(&buf[..n], &cancel) {
            return;
        }
    }

    // Whatever is queued still has to play, or the last word of the clip is cut.
    while !device.idle() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::Instant;

    fn bins() -> Bins {
        ffmpeg::find_bins().expect("ffmpeg/ffprobe needed for tests")
    }

    /// 3 s of a 440 Hz tone over a test pattern, built once and shared.
    fn make_source() -> std::path::PathBuf {
        static SRC: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        SRC.get_or_init(|| {
            let dir = std::env::temp_dir().join("vc_audio_tests");
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join("tone.mp4");
            let b = bins();
            let st = Command::new(&b.ffmpeg)
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30"])
                .args(["-f", "lavfi", "-i", "sine=frequency=440"])
                .args(["-t", "3", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
                .arg(&p)
                .status()
                .unwrap();
            assert!(st.success());
            p
        })
        .clone()
    }

    /// The sound half of playback: ffmpeg must produce 16-bit stereo at the
    /// rate asked for, for exactly the trimmed range, with the tone in it.
    #[test]
    fn the_sound_is_pcm_for_the_trimmed_range_only() {
        let b = bins();
        let src = make_source();
        let mut child = ffmpeg::command(&b.ffmpeg)
            .args(pcm_args(&src, 1.0, 2.0, RATE))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut out = Vec::new();
        child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
        assert!(child.wait().unwrap().success());

        let want = RATE as f64 * BLOCK as f64; // one second of sound
        let got = out.len() as f64;
        assert!((got - want).abs() < (CHUNK * 2) as f64, "got {got} bytes, wanted about {want}");
        assert!(out.iter().any(|b| *b != 0), "the sound came out as silence");
    }

    /// A silent video must ask for nothing rather than fail: the caller only
    /// starts the sound when a track is there.
    #[test]
    fn pcm_args_take_the_first_audio_track_and_no_picture() {
        let args: Vec<String> = pcm_args(Path::new("in.mp4"), 1.5, 4.25, RATE)
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        let joined = args.join(" ");
        assert!(joined.contains("-map 0:a:0"), "{joined}");
        assert!(joined.contains("-vn"), "{joined}");
        assert!(joined.contains("-ss 1.500"), "{joined}");
        assert!(joined.contains("-t 2.750"), "{joined}");
        assert!(joined.contains("-ac 2"), "{joined}");
        assert!(joined.contains("-ar 48000"), "{joined}");
        assert!(joined.ends_with("-f s16le -"), "{joined}");
    }

    /// The preview's volume is applied to the samples by hand: half is half,
    /// double is double, and a boosted sample that runs out of range clips at
    /// the top of the scale instead of wrapping round to noise. 100% must be
    /// bit-exact, or a video with the volume left alone would be altered.
    #[test]
    fn the_gain_scales_samples_and_clips() {
        assert_eq!(scaled(1000, 500), 500);
        assert_eq!(scaled(1000, 2000), 2000);
        assert_eq!(scaled(-1000, 500), -500);
        assert_eq!(scaled(20000, 2000), i16::MAX);
        assert_eq!(scaled(-20000, 2000), i16::MIN);
        assert_eq!(scaled(1234, GAIN_ONE), 1234);
        assert_eq!(scaled(-1234, GAIN_ONE), -1234);
        assert_eq!(scaled(1234, 0), 0);
        // Out-of-range requests are clamped to the box's range, not obeyed.
        assert_eq!(to_milli(2.5), GAIN_MAX);
        assert_eq!(to_milli(-1.0), 0);
        assert_eq!(to_milli(1.0), GAIN_ONE);
    }

    /// A moved volume box ramps across the buffer rather than stepping: the first
    /// frame at the level it had, the last at the level asked for, and both
    /// channels of a frame together so the picture does not wander between
    /// the speakers.
    #[test]
    fn the_gain_moves_along_the_buffer_without_a_step() {
        let frames = 4;
        let mut buf = vec![0u8; frames * BLOCK];
        for f in 0..frames {
            for ch in 0..CHANNELS as usize {
                let i = f * BLOCK + ch * 2;
                buf[i..i + 2].copy_from_slice(&1000i16.to_le_bytes());
            }
        }

        let next = apply_gain(&mut buf, 1000, 2000);
        assert_eq!(next, 2000, "the next buffer must start where this one ended");

        let at = |f: usize, ch: usize| {
            let i = f * BLOCK + ch * 2;
            i16::from_le_bytes([buf[i], buf[i + 1]])
        };
        let heard: Vec<i16> = (0..frames).map(|f| at(f, 0)).collect();
        assert_eq!(heard, vec![1000, 1333, 1666, 2000]);
        for f in 0..frames {
            assert_eq!(at(f, 0), at(f, 1), "frame {f} has a different gain on each channel");
        }

        // 100% is a copy: the samples come back untouched.
        let before = buf.clone();
        apply_gain(&mut buf, GAIN_ONE, GAIN_ONE);
        assert_eq!(buf, before);
    }

    /// The winmm half, as far as a machine without ears can check it: a short
    /// tone is handed to the device, and the device's own clock has to move by
    /// the length of it. A machine with no output device at all skips this;
    /// Windows mixes in software, so a box with no speakers still plays.
    #[test]
    #[cfg(windows)]
    fn the_device_plays_what_it_is_given() {
        let Some(mut device) = Device::open() else { return };
        let rate = device.rate as usize;
        let frames = rate / 5; // 0.2 s
        let mut pcm = Vec::with_capacity(frames * BLOCK);
        for i in 0..frames {
            let phase = i as f64 / rate as f64 * 440.0 * std::f64::consts::TAU;
            let v = (phase.sin() * 3000.0) as i16;
            for _ in 0..CHANNELS {
                pcm.extend_from_slice(&v.to_le_bytes());
            }
        }

        let cancel = AtomicBool::new(false);
        let began = Instant::now();
        for chunk in pcm.chunks(CHUNK) {
            assert!(device.play(chunk, &cancel), "the device refused a buffer");
        }
        // play() waits for a free buffer, so handing it over already took the
        // length of the tone; the clock is then checked until it catches up.
        let mut played = 0;
        while began.elapsed() < Duration::from_secs(5) {
            played = device.played_samples();
            if played as usize >= frames - rate / 50 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(played as usize >= frames / 2, "the device played {played} of {frames} sample frames");
    }
}
