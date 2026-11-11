FINNCROP v1.1  -  tiny native video cropper + lossless cutter
=============================================================

NOTHING TO INSTALL
  finnCrop.exe is one file. FFmpeg and ffprobe travel inside it: they are
  written out once, to %LOCALAPPDATA%\finnCrop\ffmpeg\1.1\, the first time the
  program runs, and used from there afterwards. No installer, no admin rights,
  no DLL sitting beside the exe. Deleting that folder is harmless - it is
  written again on the next start.

  That first start takes a few seconds longer than the ones after it, because
  about 205 MB has to be unpacked and virus-checked.

HOW TO USE
  - Drop a video on the window (or click "Open video...").
  - CROP: drag the box or its handles. Pick an aspect (1:1, 16:9, 9:16, 4:3, 3:4).
  - TRIM: drag the two handles on the timeline (or "Set start" / "Set end").
    Click or drag the bar to change the preview frame.
  - PLAY: "Play" (or the Space bar) plays from the playhead to the end of the
    trim. It pauses with "Pause", Space, or a click on the picture; moving the
    playhead stops it. Playback is silent - it is there to judge the crop and
    the cut, not to replace a media player. The crop box and the dimmed edges
    are hidden while it runs.
  - Save...
  - GIF: pick "GIF" as the format and Save. The crop and the trim still apply,
    and the result is an animated GIF that loops forever. There is no sound in a
    GIF, and no metadata.

MODES (shown under the controls, picked automatically)
  - Box covers the whole frame   -> FAST CUT: streams are copied, no re-encoding,
                                   lossless and near-instant.
  - Box smaller than the frame   -> CROP: video is re-encoded with x264
                                   (audio is copied), quality selectable.
  - "Precise cut" checkbox       -> frame-accurate trim by re-encoding (no crop needed).
  - Format "GIF"                 -> GIF: re-encoded to a palette, at the frame rate
                                   and size shown in the summary line.

NOTES
  - Fast cut can only start on a keyframe. "Snap start to keyframe" (on by default)
    starts the cut at the keyframe at or before your start point, so the result is
    clean. Turn it off, or use Precise cut, if you need the exact frame.
  - GIF quality is decided by three things, which the quality setting moves together:
    frame rate (15 / 12 / 10 fps), long side (800 / 640 / 480 px) and palette size
    (256 / 192 / 128 colours). The picture is only ever made smaller, never bigger.
    A GIF is re-encoded in two passes - one to build the palette from the whole
    clip, one to encode - so it takes about twice as long as a crop.
  - If the source codecs can't be copied into the chosen container (for example some
    AVI/WMV audio into MP4) the app tells you; choose MKV or "Same as source".
  - Keep metadata OFF removes tags (title, creation date, GPS, encoder name) and
    chapters. With nothing else changed it just remuxes, so it is lossless.
    Limits: in Fast cut the original video/audio data is copied untouched, so
    anything embedded inside the stream itself (e.g. an encoder string some
    encoders write into the H.264 data) stays. MKV keeps a generic "Lavf" marker.
    Re-encoded output (Crop / Precise) has the x264 settings string removed.
  - Subtitles and extra video streams are not copied (first video + all audio only).
  - The original file is never overwritten.

LICENCE
  Apache License 2.0 - see LICENSE. Copyright 2026 finnuen.
  The licence text is built into the program as well: "About", top right.

  FFmpeg is not covered by that licence. It is packed in under the GNU GPL v3
  and runs as a separate process; its source is at https://ffmpeg.org.

BUILD FROM SOURCE
  Install Rust (https://rustup.rs) with the x86_64-pc-windows-gnu toolchain,
  then run:

      .\build.ps1              release build, version bumped (1.1 -> 1.2)
      .\build.ps1 -Debug       debug build, fast, ffmpeg read from vendor\
      .\build.ps1 -KeepVersion rebuild the same version
      .\build.ps1 -Version 2.0 set the version outright

  Cargo.toml holds the version and nothing else does; the script writes it into
  the build's name, the exe's version block, the title bar and the About window.
  Result: dist\finnCrop_v1.1.exe  (about 205 MB, ffmpeg inside)

  build.ps1 looks for cargo in %USERPROFILE%\.cargo\bin itself, so it works even
  when a rustup install left that folder off PATH.

  The first build downloads FFmpeg once (about 110 MB) into vendor\ and packs it
  into the executable. Tests: cargo test (they use the vendor\ copies).

ICON
  tools\IconDraw.ps1 holds the artwork as drawing code; tools\make-icon.ps1
  renders src\app-icon.png, src\app.ico and the window icon from it. Run
  powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1 after a change.
