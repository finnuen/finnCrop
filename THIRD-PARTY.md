# Third-party components

finnCrop itself is Apache License 2.0 (see `LICENSE`). Two things made by other
people travel inside `finnCrop.exe`: one program and one set of fonts.

## FFmpeg — GNU GPL v3

`ffmpeg.exe` and `ffprobe.exe` are packed into the executable and written out on
first run, with the licence beside them, to
`%LOCALAPPDATA%\finnCrop\ffmpeg\<version>\`.

- **Version:** 9.0.2 (release branch, built 2026-09-19)
- **Package:** gyan.dev "essentials"
  <https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-9.0.2-essentials_build.zip>
- **SHA256:** `60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba`
  — `build.ps1` refuses to unpack any other file, so the version in this document
  and the version in the release cannot drift apart.
- **Configuration:** `--enable-gpl --enable-version3`, statically linked, so the
  binary as a whole is under the GPL v3.
- **Licence text:** [`third-party/GPL-3.0.txt`](third-party/GPL-3.0.txt), also
  written next to the binaries as `FFmpeg-LICENSE.txt`.
- **Corresponding source:** FFmpeg at commit `946fcce07b`, the commit gyan.dev
  names for this package —
  <https://github.com/FFmpeg/FFmpeg/commit/946fcce07b> — together with gyan.dev's
  build recipe, <https://github.com/GyanD/codexffmpeg>. The package index with
  every version, and its checksums, is <https://www.gyan.dev/ffmpeg/builds/>.

The essentials build also carries these GPL libraries, which is why the binary as
a whole is GPL v3 rather than LGPL: **x264**, **x265**, **xvid** and **vidstab**.
Each is available from its own project under its own terms (x264 from VideoLAN,
x265 from MulticoreWare). The remaining libraries in the build — freetype,
harfbuzz, libvpx, libaom, opus, lame and the rest — are under the LGPL or a
permissive licence.

FFmpeg is not linked into finnCrop. It is started as a separate process, and the
only interface between the two is the command line, which is the arrangement the
GPL calls mere aggregation (section 5): two programs travelling in one file, not
one program built from both. That is why the file is not itself GPL v3, and why
finnCrop's own terms stay Apache 2.0.

## Fonts

`eframe` is built with its default fonts, so four typefaces are embedded in the
executable, by way of the `epaint_default_fonts` crate (0.29.1):

| Font | Used for | Licence | Text |
| --- | --- | --- | --- |
| Ubuntu-Light | the UI's text | Ubuntu Font Licence 1.0 | [`third-party/fonts/UFL-1.0.txt`](third-party/fonts/UFL-1.0.txt) |
| NotoEmoji-Regular | emoji | SIL Open Font License 1.1 | [`third-party/fonts/OFL-1.1.txt`](third-party/fonts/OFL-1.1.txt) |
| Hack-Regular | monospaced text | MIT, with the DejaVu work in the public domain | [`third-party/fonts/Hack-MIT.txt`](third-party/fonts/Hack-MIT.txt) |
| emoji-icon-font | icon glyphs | MIT, Copyright 2014 John Slegers | [`third-party/fonts/emoji-icon-font-MIT.txt`](third-party/fonts/emoji-icon-font-MIT.txt) |

## Rust crates

Everything finnCrop is compiled from is listed in `Cargo.lock` and is under the
MIT or Apache 2.0 licence, or both, with some under Zlib, BSD, ISC, BSL-1.0,
Unicode-3.0, the Unlicense or 0BSD. There is no copyleft among them, so nothing
in the dependency tree affects how finnCrop may be licensed or distributed.
