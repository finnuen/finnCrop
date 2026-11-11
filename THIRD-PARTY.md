# Third-party components

finnCrop itself is Apache License 2.0 (see `LICENSE`). It ships one third-party
program inside the executable.

## FFmpeg (GNU GPL v3)

`ffmpeg.exe` and `ffprobe.exe` are packed into `finnCrop.exe` and are written out
on first run, together with their licence, to
`%LOCALAPPDATA%\finnCrop\ffmpeg\<version>\`.

- Source: https://ffmpeg.org
- Build used: the "essentials" build from https://www.gyan.dev/ffmpeg/builds/,
  version 9.0.2, configured with `--enable-gpl --enable-version3`.
- Licence: GNU General Public License version 3 — see `vendor/FFmpeg-LICENSE.txt`
  in a source checkout, or `FFmpeg-LICENSE.txt` next to the unpacked binaries.

FFmpeg is not linked into finnCrop: it is started as a separate process, which is
why the rest of the program can stay under a permissive licence. The two programs
inside one file are a mere aggregation, in the sense of GPL v3 section 5.

x264 (GPL v2) is included in that FFmpeg build and is used for the re-encoding
modes (Crop and Precise cut).
