# DECISIONS.md — deviations from the build spec, with rationale

Log every deviation here. Format: what the spec says, what we did, why.

## M1

1. **Spec §3 layout shows only `src/main.rs`; we added `src/lib.rs`.**
   Integration tests in `tests/` can only link against a library target, and
   spec §5/§14 require builder snapshot tests under `tests/`. So the crate is
   a lib (`src/lib.rs`, module tree) plus a thin binary (`src/main.rs`: CLI,
   logging, terminal setup, main loop). No behavior change; the layout is
   otherwise exactly §3.

2. **`rust-version = "1.88"` rather than "two behind current stable".**
   Current stable at build time is 1.97, which would suggest 1.95 — but
   `ratatui 0.30` itself declares `rust-version = 1.88` (verified via the
   crates.io versions API), and the MSRV cannot exceed the toolchain floor
   set by dependencies. 1.88 satisfies "at least two releases behind" while
   staying truthful about what actually builds.

3. **Crate versions pinned 2026-09-17 from crates.io max-stable** (spec §2:
   do not trust memory): ratatui 0.30.2, crossterm 0.29.0, tokio 1.53.1,
   serde 1.0.229, serde_json 1.0.151, toml 1.1.6, clap 4.6.7, anyhow 1.0.104,
   thiserror 2.0.20, which 8.0.6, directories 6.0.0, arboard 3.6.1,
   tracing 0.1.44, tracing-subscriber 0.3.23, tui-input 0.15.4. `Cargo.toml`
   uses caret reqs (`"0.30"`, `"1"`, …) — standard Cargo pinning that still
   resolves to these versions and is locked in `Cargo.lock`.

4. **New repo lives at `../ffkit`, next to the FFmpeg fork — not inside it.**
   Per spec §0, building inside the fork would drag LGPL/GPL obligations and
   a permanent rebase burden onto the tool. `ffkit/` is its own crate and
   (forthcoming) git repo; the fork is read-only reference only.

5. **ratatui 0.30 `crossterm` backend needs no explicit feature.**
   Verified via the crates.io versions API: `crossterm` is in ratatui's
   default features, so `ratatui = "0.30"` is sufficient. `ratatui::init()` /
   `restore()` helpers were confirmed to exist in 0.30.2 docs but M1 uses
   explicit `CrosstermBackend` setup anyway — fewer moving parts, same result.

6. **Direct-launch flags (`--operation`, `--input`) are parsed in M1 but
   warn-and-continue to the picker.** `clap` is wired from day one so the CLI
   surface is stable; honoring the flags needs the M2/M3 screens, so the
   current behavior is logged, never silent.

## M2

7. **New `src/background.rs` module (not in the §3 layout).** The spec puts
   async ffmpeg/ffprobe calls behind `tokio::sync::mpsc` (§14) but names no
   module for the spawn helpers and message enum. `background.rs` owns the
   `BackgroundMsg` enum, the channel pair, and the three spawn functions
   (capability detection, custom-path check, lazy probe). Runners/progress
   (M4) will extend the same enum.

8. **Sync main loop on a multi-thread tokio runtime.** The render thread
   alternates `draw → drain channel → poll events (250ms cap)`; subprocess
   tasks run on runtime workers. Blocking `poll` on the main thread is safe
   here because workers are never starved by it. No subprocess I/O touches
   the render path.

9. **Synchronous `readdir` on browser keypresses.** Local directory reads are
   fast; the expensive I/O (ffprobe/ffmpeg) is async. Network mounts may
   hitch a frame — async listing is queued follow-up work if it bites.

10. **Parser shapes verified against ground truth, not memory.** `-encoders`
    entries are `␣<6 flags>␣<name>` (`print_codecs`, fftools/opt_common.c);
    `-filters` entries are `␣<2 flags>␣<name>` (`show_filters`) — the spec's
    "3 flag chars" guess in §7-style prose was wrong for filters and the
    code follows the C source. The legend line ` V..... = Video` initially
    parsed `=` as a codec; the `=` guard fixed it (caught by unit test).

11. **"Ancient build" threshold is major < 6.** Grounded against the
    reference fork (libavutil major 61, i.e. post-8.0): 6.0 (2023) is where
    flag behavior meaningfully stabilizes for this tool's purposes. Dev
    builds (`N-…`) report unknown, never ancient.

12. **`[general]`-only settings in M2, same file shape as M6.** The custom
    ffmpeg path persists to `config.toml` immediately (spec M2 requires
    persistence), and M6 extends the struct without migration. A broken
    config falls back to defaults with a log line — never a bricked TUI.

13. **Browser extras beyond the spec table: `a` (all/media toggle), `g`/`G`
    (first/last), type-ahead extends on any printable except `?`/`q`** (both
    reserved globally). `Backspace` pops the filter when one is active,
    otherwise goes up a directory. Documented in the `?` overlay.

14. **tui-input 0.15 API differs from memory:** no `to_input_request`
    method — input flows through `EventHandler::handle_event(&Event)`.
    Verified against the vendored crate source.

## M3

15. **M3 ships compress/convert/trim; the other eight ops fail loudly with
    "lands in M5".** The trait, field framework, form, and preview are fully
    generic (proven by three ops exercising all four field kinds), and M5
    fills in the remaining builders + snapshot matrix.

16. **Stream copy is an explicit toggle, never a silent decision.** When the
    probe codec matches and no scaling applies, compress defaults the
    "Video" toggle to copy — visible in the form, and the preview shows
    `-c:v copy`. The principle "never surprise the user" wins over saving a
    click.

17. **No type-to-edit on the form.** A first draft started text editing on
    any printable key, but that collides with the non-negotiable `c` = copy
    binding (`c` would copy on some rows and type on others). Text editing
    always starts with Enter; the footer says so.

18. **NVENC reuses the CRF slider as `-cq`.** Same quality intuition, honest
    flag — the explanation names the mapping. Software `-preset` is skipped
    for NVENC (its presets are a different namespace) and noted as ignored.

19. **`--`/`./` rule for leading-dash names (spec §14):** `safe_path_arg`
    prefixes `./` only when the whole operand starts with `-`
    (`-rf.mp4` → `./-rf.mp4`); `sub/-rf.mp4` and absolute paths are already
    safe. Quoting is display-only — argv keeps hostile names whole.

## M4

20. **Progress keys verified in `print_report` (fftools/ffmpeg.c:586-740),
    not just §7 prose.** Both `out_time_us` and `out_time_ms` print `pts`
    (microseconds) — the code prefers `_us` and falls back to parsing
    `out_time`. Audio-only encodes emit no `frame`/`fps`; everything stays
    `Option`. `q` values of `-1` (no encoder yet) parse to `None`.

21. **No `nix`/`libc` dependency for SIGTERM.** The runner sends SIGTERM via
    a `kill -TERM <pid>` subprocess, waits 3s, then `Child::kill()`
    (SIGKILL). If `kill` itself is missing it escalates immediately.
    Second Ctrl+C calls `kill -KILL` fire-and-forget. Windows uses
    terminate/taskkill throughout.

22. **Runner streams every stderr line over the channel.** Volume is small
    with `-nostats`; the UI ring-buffers 300 lines for the pane while the
    runner keeps 2000 for the error report. Cancel-before-spawn and
    spawn-failure both become `JobFinished` — the runner never panics.

23. **Overwrite and partial-delete are screen phases, not modals.**
    `ConfirmOverwrite` resolves before any spawn (so `-y` is never a
    surprise); `ConfirmDelete` appears only when a partial file actually
    exists. `q` is refused with a hint while a job is active.

## M5

24. **Concat compares probes, so `BuildContext` grew `input_probes`.**
    Ready probes for all inputs in order (short = unknown → safe
    re-encode). Three literal construction sites updated; single-probe
    `probe` kept for the common case.

25. **Pure builders cannot write the concat list file.** `CommandSpec`
    carries a `ConcatListFile` descriptor; the runner writes it (absolute
    paths, quote escaping) before the main command and removes it
    best-effort after. Same pattern already covered pre-commands.

26. **Concat method is a 3-way Select, not a Toggle.** Toggles are exactly
    two options by design; the third choice (force demuxer/filter) needed a
    dropdown. Mixed stream types (video + audio-only) fail loudly with a
    normalize-first message instead of emitting a broken filter.

27. **Frames pattern requires `%04d`.** Without a number every frame would
    overwrite one file — a loud builder error instead of silent loss.

28. **GIF palette is kept next to the output** (`{stem}_palette.png`) rather
    than hidden in tmp: the preview names a real file and the user sees
    what the second pass used. Verified live: demuxer join (2.0s output
    from 1+1s inputs) and palettegen→paletteuse both ran green.

## M6

29. **Every spawn takes a shared job id (single runs included).** Queue and
    single messages share one channel, so `JobStarted/Progress/StderrLine/
    Finished` all carry `job_id`; `RunState` filters to its own. Late
    messages can never paint the wrong job.

30. **Batch outputs rebuild; they are never argv-edited.** Per input: build
    with `output: None` (builder default), split `{suffix}`/`{ext}` off the
    default name, expand the template, rebuild with the final path. Pure
    functions, tested twice.

31. **One overwrite confirm per batch, not per file.** Existing outputs arm
    the queue screen with a conflicts banner (`y` runs, `Esc` backs out);
    `confirm_overwrite = false` skips it everywhere.

32. **Single and batch execution are exclusive.** Starting work while a job
    or the queue runs is refused with a hint — no interleaved runners, no
    channel crosstalk.

33. **Queue `Q` is Shift+q.** Lowercase `q` quits (spec table); `Q` opens the
    queue from picker/browser/form. Both are in the `?` overlay.

34. **`[defaults]` ride the preset machinery.** A synthetic preset applies
    config defaults through the same coercion as user presets — unknown ids
    and disabled options are skipped, sliders clamp.

35. **Resize grew a center-square crop** for the "Square Social Clip"
    built-in (`crop=min(iw\\,ih):min(iw\\,ih)` before scale). The backslash-
    comma is a literal comma inside the filter expression.

36. **Verified live:** 3-job batch (good, doomed, good) ran sequentially —
    middle job failed, queue continued, summary read "2 ok, 1 failed".

## M7

38. **Theme selection is `dark`/`light` with dark fallback.** Unknown names
    log nothing and fall back — a typo must not brick the UI. All screens
    take the theme as a parameter; no `Color` literals outside `theme.rs`.

39. **No rendered GIF in this environment.** vhs 0.12 renders via headless
    Chromium under X (rod + xvfb); this sandbox has Chromium but no Xvfb
    and no root to install it, so `vhs demo.tape` parses (`vhs validate`
    passes) but produces no output. The committed `demo.tape` renders on
    any machine with `vhs`/`ttyd`/`ffmpeg`/X; the README shows a
    deterministically captured ASCII frame until then.

40. **crates.io excludes large media.** `cargo publish --dry-run` first
    tried to ship 642MB (user videos sitting in `tests/`). `exclude` now
    keeps video extensions and `demo.gif` local; the package is 495KB.
    Actual publishing needs `CARGO_REGISTRY_TOKEN` — the release workflow
    runs `cargo publish` on `v*` tags after building all three platforms.

## Perf diagnostic ("slower than ffmpeg directly")

37. **No overhead found in the encode path; fixed the one real hotspot.**
    Step-by-step: (1) command parity holds — compress defaults
    (`-preset medium`, `-crf 23`) equal x264/manual defaults, divergences
    are user-visible choices; (2) spawn is direct
    (`Command::new(path).args(argv).spawn()`, no shell), stdin null,
    stdout/stderr piped and both drained every loop iteration;
    (3) profile ruled out empirically — identical encodes take ~1s via
    direct spawn and via the runner in both release and debug
    (`tests/runner_parity.rs` keeps this assertion); (4) probing is lazy
    (highlight/confirm only, per-path cache, nothing on directory open);
    (5) Enter→spawn holds only `exists()`/`metadata`/pure build (sub-ms),
    no clipboard/config on the hot path; (6) environment couldn't be checked
    for the reporter — same-path `time` comparison rules out WSL mounts.
    The one genuine ffkit-side cost was `Vec::remove(0)` per stderr line
    past the 2000-line cap (quadratic on verbose encodes) — now a
    `VecDeque` with a cap test. If a report persists, compare the preview
    (`c` copies it) token-by-token against the hand command; that is where
    any remaining gap lives.
