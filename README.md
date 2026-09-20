# ffkit — a terminal UI for FFmpeg that teaches as it goes

Today, compressing a video means opening a browser, finding a 2014 Stack
Overflow answer, pasting a command you don't understand, and hoping. `ffkit`
replaces that loop:

1. Pick an operation from a menu ("Compress video")
2. Pick input file(s) through a built-in file browser
3. Adjust parameters in a form — dropdowns, sliders, toggles
4. **Watch the real ffmpeg command assemble itself live, on screen, as you type**
5. Press Enter to run it, with a real progress bar
6. Optionally copy the command to the clipboard (`c`)

Point 4 is the differentiating feature: `ffkit` never hides the command. A
month of use should make you measurably better at raw FFmpeg.

## Demo

`demo.tape` is a [vhs](https://github.com/charmbracelet/vhs) script — render
it with `vhs demo.tape` (needs `vhs` + `ttyd` + `ffmpeg` + X) to produce
`demo.gif`. Until then, this is the parameter form, captured deterministically:

```
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│Compress video — tweak, watch the command                                                     │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
┌Parameters─────────────────────────────────────────┐┌Media info───────────────────────────────┐
│▸ Video codec  ◂ H.264 (libx264) ▸                 ││Input                                    │
│  Video  ● Copy stream (faster, no quality loss)  ○││  holiday.mp4                            │
│  Quality (CRF)  0 ────────●────────── 51  [23]    ││    320×240 · 30.00 fps · h264/aac · 0:02│
│  Preset  ◂ medium ▸                               ││                                         │
│  Audio  ◂ AAC ▸                                   ││                                         │
│  Audio bitrate  ◂ 128k ▸                          ││                                         │
│  Resolution  ◂ Keep original ▸                    ││                                         │
│  Output  [holiday_compressed.mp4]                 ││                                         │
└───────────────────────────────────────────────────┘└─────────────────────────────────────────┘
┌Command — [c] copy────────────────────────────────────────────────────────────────────────────┐
│ffmpeg -hide_banner -y -progress pipe:1 -nostats -i holiday.mp4 -c:v copy \                   │
│-c:a aac -b:a 128k holiday_compressed.mp4                                                     │
│                                                                                              │
│Video codec  Encoder for the video stream. libx264 is the safe default; libx265 is ~25%       │
│smaller but slower; NVENC uses your GPU (much faster, larger files).                          │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│↑↓ field · ←→ adjust · Enter edit/run · Tab command · c copy · s preset · ? help · esc back   │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

(The probe saw H.264 already, so the form offered stream copy up front —
`-c:v copy` right in the preview. Flip it to Re-encode, move the CRF slider,
and `-crf 23` becomes `-crf 20` before your finger leaves the key.)

## Install

Requires an `ffmpeg`/`ffprobe` build on `PATH` (`apt install ffmpeg`,
`brew install ffmpeg`, `winget install ffmpeg`). If none is found, ffkit
shows install instructions instead of crashing.

```sh
cargo install --path .     # or: cargo install ffkit  (once published)
ffkit
```

Release binaries for Linux/macOS/Windows attach to every `v*` tag via
`.github/workflows/release.yml`.

> **Use a release build for real workloads** (`cargo build --release` /
> `cargo run --release`). Debug builds don't slow ffmpeg's own encoding (it
> runs as a separate process either way), but they make ffkit's UI loop
> sluggish at high progress-update rates, and the session feels slower.

## Usage

- `Enter` on the picker opens the file browser for that operation.
- Browser: arrows/vim to move, `Space` marks files (batch = one queue job
  each), `Enter` confirms, `a` shows all files, `/` jumps to a path.
- Form: `↑↓` fields, `←→` adjust, `Enter` edits text / runs the job,
  `Tab` focuses the command pane, `c` copies the command, `s` saves a
  preset, `p` on the picker loads one.
- Running: progress bar, ETA, speed; `Ctrl+C` cancels gracefully (SIGTERM,
  second press force-kills); `l` expands the ffmpeg log; `c` copies the
  full error report.
- `Q` opens the job queue from anywhere; `?` shows all bindings.

## Configuration

`~/.config/ffkit/config.toml` (XDG on Linux, native paths elsewhere):

```toml
[general]
ffmpeg_path = ""            # empty = discover on PATH
default_output_dir = ""     # empty = alongside input
confirm_overwrite = true
theme = "dark"              # dark | light
max_concurrent_jobs = 1     # sequential by default
batch_template = "{parent}/{stem}_{suffix}.{ext}"

[defaults]                  # applied to matching form fields
video_codec = "libx264"
crf = 23
preset = "medium"
audio_codec = "aac"
audio_bitrate = "128k"

[[presets]]                 # + 5 built-ins (Web MP4, Archival, …)
name = "Discord upload"
operation = "compress"
[presets.params]
crf = 28
resolution = "1280:-2"
```

## Why standalone, why subprocess

`ffkit` **shells out to the `ffmpeg`/`ffprobe` binaries** — it never links
`libav*`, never uses `ffmpeg-sys`/`ffmpeg-next`, and never copies FFmpeg
source. That keeps the license clean (MIT OR Apache-2.0), works with whatever
FFmpeg build you already have, and needs no C toolchain. The FFmpeg codebase
is used as a read-only reference for flag semantics only. See `DECISIONS.md`.

## Status

M1–M7 complete: skeleton, probe+browse, builder+preview, execution,
all eleven operations, queue+presets, polish (themes, demo tape, release
workflow, crates.io-ready). `DECISIONS.md` logs every deviation from the
build spec with rationale.

## Explicit non-goals (v1)

Terminal video preview/playback, `libav*` linking, GUI/web/Electron, custom
filter-graph editing (use the form values + copied command instead),
streaming/RTMP, screen capture, re-implementing encoding logic.

## License

MIT OR Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
