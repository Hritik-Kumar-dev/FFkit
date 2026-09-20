//! Capability detection (spec section 9).
//!
//! Runs once at startup (in a spawned task, never on the render thread) and
//! is cached for the session:
//! `ffmpeg -version`, `-encoders`, `-decoders`, `-filters`, `-hwaccels`.
//!
//! Output formats below were verified against the real FFmpeg 8.0.1
//! binaries, not from memory:
//! - `-encoders`/`-decoders` print an indented `=` legend, a ` ------`
//!   separator, then entries starting at column 0: 6 flag chars, a space,
//!   the codec name. Entries start at column 0; legend lines are indented —
//!   that is the discriminator (no `=` check needed, but both hold).
//! - `-filters` entries are `XYZ name I/O… desc` (3 flag chars); legend
//!   lines contain ` = `.
//! - `-hwaccels` prints `Hardware acceleration methods:` then one name per
//!   line.
//!
//! Detection never fails loudly: a missing binary yields a report with
//! `found == false` (drives the friendly missing-FFmpeg screen), and a
//! failing list command yields an empty set plus a warning rather than
//! aborting the whole report.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::settings::Settings;

/// Per-command timeout so a wedged ffmpeg cannot hang startup forever.
const DETECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Builds older than this get an "ancient build" warning: flag behavior
/// differs meaningfully before 6.0 (2023). Grounded against the reference
/// fork, which is already past 8.0 (libavutil major 61).
const ANCIENT_MAJOR: u32 = 6;

/// Parsed `ffmpeg -version` number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfmpegVersion {
    /// Major release, e.g. `8` in `8.0.1`.
    pub major: u32,
    /// Minor release.
    pub minor: u32,
    /// Patch release.
    pub patch: u32,
}

/// Everything ffkit knows about the user's FFmpeg build for this session.
#[derive(Debug, Clone, Default)]
pub struct CapabilityReport {
    /// False when no `ffmpeg` binary was found — show the install screen.
    pub found: bool,
    /// True when the binary answered `-version` like ffmpeg. A found-but-
    /// unusable binary (stale config, wrong file) also routes to the
    /// missing screen with an explanation instead of failing later.
    pub usable: bool,
    /// Resolved `ffmpeg` binary (configured path or `PATH` discovery).
    pub ffmpeg_path: Option<PathBuf>,
    /// Resolved `ffprobe` binary (configured, sibling of ffmpeg, or `PATH`).
    pub ffprobe_path: Option<PathBuf>,
    /// Raw first line of `ffmpeg -version`, for display and bug reports.
    pub version_raw: String,
    /// Parsed version, if the output matched a release shape. Dev builds
    /// (`N-…`) yield `None` — unknown, not ancient.
    pub version: Option<FfmpegVersion>,
    /// Available encoder names, e.g. `libx264`.
    pub encoders: HashSet<String>,
    /// Available decoder names.
    pub decoders: HashSet<String>,
    /// Available filter names, e.g. `scale`.
    pub filters: HashSet<String>,
    /// Hardware acceleration methods, e.g. `cuda`, `vaapi`, `videotoolbox`.
    pub hwaccels: Vec<String>,
    /// Non-fatal problems: old build, empty list output, missing ffprobe.
    pub warnings: Vec<String>,
}

impl CapabilityReport {
    /// Report used when no binary exists. The UI shows install instructions.
    pub fn missing() -> Self {
        Self::default()
    }

    /// True when `name` can be passed to `-c:v`/`-c:a` with this build.
    pub fn has_encoder(&self, name: &str) -> bool {
        self.encoders.contains(name)
    }

    /// True when `name` can be used in `-vf`/`-af`/graphs with this build.
    pub fn has_filter(&self, name: &str) -> bool {
        self.filters.contains(name)
    }

    /// True when `-hwaccel name` is supported by this build.
    pub fn has_hwaccel(&self, name: &str) -> bool {
        self.hwaccels.iter().any(|h| h == name)
    }
}

/// Parse the first line of `ffmpeg -version`.
/// Release shape: `ffmpeg version 8.0.1-3ubuntu2 Copyright …` → 8.0.1.
/// Dev/git shapes (`ffmpeg version N-…`) and distro oddities yield `None`
/// rather than a guess.
pub fn parse_version_line(line: &str) -> Option<FfmpegVersion> {
    let rest = line.strip_prefix("ffmpeg version ")?;
    let token = rest.split_whitespace().next()?;
    let mut parts = token.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    // Minor/patch may carry suffixes (`1-3ubuntu2`, `0+git`); take leading digits.
    let minor = parts.next().map(leading_digits).unwrap_or(0);
    let patch = parts.next().map(leading_digits).unwrap_or(0);
    Some(FfmpegVersion {
        major,
        minor,
        patch,
    })
}

/// Leading ASCII digits of a version component (`"1-3ubuntu2"` → `1`).
fn leading_digits(s: &str) -> u32 {
    s.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

/// Parse `-encoders` / `-decoders` output into codec names.
/// Entry shape from `print_codecs` (fftools/opt_common.c):
/// ` %c%c%c%c%c%c <name> …` — leading space, 6 flag chars, space, name.
/// Indented legend lines and the ` ------` separator are skipped.
pub fn parse_codec_list(output: &str) -> HashSet<String> {
    output
        .lines()
        .filter_map(|line| {
            if line.contains('=') {
                return None;
            }
            if line.as_bytes().first() != Some(&b' ') {
                return None;
            }
            let flags = line.get(1..7)?;
            if !flags
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.')
            {
                return None;
            }
            if line.as_bytes().get(7) != Some(&b' ') {
                return None;
            }
            line[8..].split_whitespace().next().map(str::to_string)
        })
        .collect()
}

/// Parse `-filters` output into filter names.
/// Entry shape from `show_filters` (fftools/opt_common.c):
/// ` %c%c <name> …` — leading space, 2 flag chars (T/S), space, name.
/// Legend lines contain ` = ` and the separator is ` ------`.
pub fn parse_filter_list(output: &str) -> HashSet<String> {
    output
        .lines()
        .filter_map(|line| {
            if line.contains('=') {
                return None;
            }
            if line.as_bytes().first() != Some(&b' ') {
                return None;
            }
            let flags = line.get(1..3)?;
            if !flags.bytes().all(|b| matches!(b, b'T' | b'S' | b'.')) {
                return None;
            }
            if line.as_bytes().get(3) != Some(&b' ') {
                return None;
            }
            line[4..].split_whitespace().next().map(str::to_string)
        })
        .collect()
}

/// Parse `-hwaccels` output: lines after `Hardware acceleration methods:`.
/// Unknown shapes yield an empty vec, never a panic.
pub fn parse_hwaccel_list(output: &str) -> Vec<String> {
    let mut lines = output.lines();
    for line in &mut lines {
        if line.trim() == "Hardware acceleration methods:" {
            return lines
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
    }
    Vec::new()
}

/// `ffprobe` next to `ffmpeg` (`/usr/bin/ffmpeg` → `/usr/bin/ffprobe`,
/// handling Windows `.exe`). Pure path shape — callers check existence.
pub fn sibling_ffprobe(ffmpeg: &Path) -> Option<PathBuf> {
    let dir = ffmpeg.parent()?;
    let stem = if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };
    Some(dir.join(stem))
}

/// Resolve the `ffmpeg` binary: a configured path wins when it exists,
/// otherwise `PATH` discovery. A configured-but-missing path falls back to
/// `PATH` (callers warn about the stale config via [`stale_config_warning`]).
pub fn resolve_ffmpeg(settings: &Settings) -> Option<PathBuf> {
    if let Some(configured) = settings.general.ffmpeg_path.as_ref() {
        if !configured.as_os_str().is_empty() && configured.exists() {
            return Some(configured.clone());
        }
    }
    which::which("ffmpeg").ok()
}

/// Warn when the user configured a path but detection resolved something
/// else (stale config) — surfaces in the report instead of failing silently.
fn stale_config_warning(settings: &Settings, resolved: &Path) -> Option<String> {
    match settings.general.ffmpeg_path.as_ref() {
        Some(configured) if !configured.as_os_str().is_empty() && configured != resolved => {
            Some(format!(
                "Configured ffmpeg path {} does not exist; using {} instead.",
                configured.display(),
                resolved.display()
            ))
        }
        _ => None,
    }
}

/// Resolve `ffprobe`: configured path, then the ffmpeg sibling, then `PATH`.
pub fn resolve_ffprobe(settings: &Settings, ffmpeg: Option<&Path>) -> Option<PathBuf> {
    if let Some(configured) = settings.general.ffprobe_path.as_ref() {
        if configured.exists() {
            return Some(configured.clone());
        }
    }
    if let Some(ffmpeg) = ffmpeg {
        if let Some(sibling) = sibling_ffprobe(ffmpeg) {
            if sibling.exists() {
                return Some(sibling);
            }
        }
    }
    which::which("ffprobe").ok()
}

/// Run one `ffmpeg -hide_banner <arg>` list command with a timeout.
/// Failure yields `None` — the caller degrades to an empty set + warning.
async fn run_list_command(ffmpeg: &Path, arg: &str) -> Option<String> {
    let output = tokio::time::timeout(
        DETECT_TIMEOUT,
        tokio::process::Command::new(ffmpeg)
            .args(["-hide_banner", arg])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Detect capabilities once at startup. Never returns `Err`: every failure
/// mode (missing binary, hung process, unparsable output) is represented in
/// the report so the UI can explain it instead of crashing.
pub async fn detect(settings: &Settings) -> CapabilityReport {
    let ffmpeg_path = match resolve_ffmpeg(settings) {
        Some(path) => path,
        None => return CapabilityReport::missing(),
    };
    let ffprobe_path = resolve_ffprobe(settings, Some(&ffmpeg_path));

    let (version_out, encoders_out, decoders_out, filters_out, hwaccels_out) = tokio::join!(
        run_list_command(&ffmpeg_path, "-version"),
        run_list_command(&ffmpeg_path, "-encoders"),
        run_list_command(&ffmpeg_path, "-decoders"),
        run_list_command(&ffmpeg_path, "-filters"),
        run_list_command(&ffmpeg_path, "-hwaccels"),
    );

    let mut warnings = Vec::new();
    if let Some(warning) = stale_config_warning(settings, &ffmpeg_path) {
        warnings.push(warning);
    }
    let version_raw = version_out
        .as_deref()
        .and_then(|s| s.lines().next())
        .unwrap_or("")
        .to_string();
    let version = parse_version_line(&version_raw);
    match version {
        Some(v) if v.major < ANCIENT_MAJOR => warnings.push(format!(
            "FFmpeg {}.{}.{} looks ancient (older than {ANCIENT_MAJOR}.x); flag behavior may differ from what ffkit expects.",
            v.major, v.minor, v.patch
        )),
        None if !version_raw.is_empty() => warnings.push(
            "Could not parse the FFmpeg version; it may be a dev build. Capability checks still apply.".to_string(),
        ),
        _ => {}
    }

    let mut report = CapabilityReport {
        found: true,
        usable: version_out.is_some(),
        ffmpeg_path: Some(ffmpeg_path),
        ffprobe_path: ffprobe_path.clone(),
        version_raw,
        version,
        warnings,
        ..CapabilityReport::default()
    };
    if ffprobe_path.is_none() {
        report.warnings.push(
            "ffprobe was not found; media info and progress percentages will be unavailable."
                .to_string(),
        );
    }
    match encoders_out {
        Some(text) => report.encoders = parse_codec_list(&text),
        None => report
            .warnings
            .push("Could not list encoders; assuming none.".to_string()),
    }
    match decoders_out {
        Some(text) => report.decoders = parse_codec_list(&text),
        None => report
            .warnings
            .push("Could not list decoders; assuming none.".to_string()),
    }
    match filters_out {
        Some(text) => report.filters = parse_filter_list(&text),
        None => report
            .warnings
            .push("Could not list filters; assuming none.".to_string()),
    }
    match hwaccels_out {
        Some(text) => report.hwaccels = parse_hwaccel_list(&text),
        None => report.warnings.push(
            "Could not list hardware accelerators; hardware options will be hidden.".to_string(),
        ),
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_and_distro_version_lines() {
        assert_eq!(
            parse_version_line("ffmpeg version 8.0.1-3ubuntu2 Copyright (c) 2000-2025"),
            Some(FfmpegVersion {
                major: 8,
                minor: 0,
                patch: 1
            })
        );
        assert_eq!(
            parse_version_line("ffmpeg version 6.1.2 Copyright (c) 2000-2024"),
            Some(FfmpegVersion {
                major: 6,
                minor: 1,
                patch: 2
            })
        );
    }

    #[test]
    fn dev_builds_yield_none_not_garbage() {
        assert_eq!(
            parse_version_line("ffmpeg version N-118345-gabcdef Copyright"),
            None
        );
        assert_eq!(parse_version_line("not ffmpeg output"), None);
        assert_eq!(parse_version_line(""), None);
    }

    #[test]
    fn codec_list_skips_legend_and_separator() {
        let output = "Encoders:\n V..... = Video\n ------\n V....D libx264            libx264 H.264 / AVC\n A....D aac                 AAC (Advanced Audio Coding)\n";
        let got = parse_codec_list(output);
        assert!(got.contains("libx264") && got.contains("aac"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn filter_list_skips_legend() {
        let output = "Filters:\n  T.. = Timeline support\n  ------\n .. acompressor        A->A       Audio compressor.\n T. scale              V->V       Scale the input video size.\n";
        let got = parse_filter_list(output);
        assert!(got.contains("acompressor") && got.contains("scale"));
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn hwaccel_list_parses_and_tolerates_unknown_shapes() {
        let output = "Hardware acceleration methods:\nvdpau\ncuda\nvaapi\n";
        assert_eq!(parse_hwaccel_list(output), vec!["vdpau", "cuda", "vaapi"]);
        assert!(parse_hwaccel_list("garbage\n").is_empty());
    }

    #[test]
    fn ancient_threshold_flags_ffmpeg_5() {
        let v = FfmpegVersion {
            major: 5,
            minor: 1,
            patch: 0,
        };
        assert!(v.major < ANCIENT_MAJOR);
        let v = FfmpegVersion {
            major: 8,
            minor: 0,
            patch: 1,
        };
        assert!(v.major >= ANCIENT_MAJOR);
    }
}
