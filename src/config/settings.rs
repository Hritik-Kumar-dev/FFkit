//! Global settings (spec section 11).
//!
//! M2 implemented the `[general]` binary-path subset so a custom FFmpeg path
//! could persist immediately. M6 grows the file to the full spec shape:
//! output dir, overwrite confirm, theme, job concurrency, encoder defaults,
//! and user `[[presets]]` (see [`presets`](crate::config::presets)).
//! Unknown fields are ignored by serde, so newer configs never break older
//! builds — and a broken config still falls back to defaults with a log
//! line, never a bricked TUI.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Root of `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    /// General behavior.
    #[serde(default)]
    pub general: GeneralSettings,
    /// Encoder defaults applied to matching form fields at open.
    #[serde(default)]
    pub defaults: EncoderDefaults,
    /// User-saved presets ([[presets]]). Built-ins live in code.
    #[serde(default)]
    pub presets: Vec<crate::config::presets::Preset>,
}

/// Settings that apply regardless of operation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeneralSettings {
    /// Custom `ffmpeg` binary path. Empty/`None` means "discover on `PATH`".
    #[serde(default)]
    pub ffmpeg_path: Option<PathBuf>,
    /// Custom `ffprobe` binary path. Empty/`None` means "sibling of the
    /// ffmpeg binary, else discover on `PATH`".
    #[serde(default)]
    pub ffprobe_path: Option<PathBuf>,
    /// Default output directory. Empty means "alongside the input".
    #[serde(default)]
    pub default_output_dir: PathBuf,
    /// Ask before overwriting existing outputs (single runs). Batch runs
    /// confirm once for the whole queue instead of per file.
    #[serde(default = "default_confirm_overwrite")]
    pub confirm_overwrite: bool,
    /// Theme name (`"dark"` today; M7 adds selection/persistence).
    #[serde(default = "default_theme")]
    pub theme: String,
    /// How many ffmpeg processes may run at once. Sequential (1) by default:
    /// parallel encodes contend for CPU and usually finish slower overall.
    #[serde(default = "default_concurrency")]
    pub max_concurrent_jobs: usize,
    /// Batch output naming template. Variables: `{stem}`, `{ext}`,
    /// `{parent}`, `{index}`, `{date}`, `{suffix}`. The default reproduces
    /// each builder's own `{stem}_{suffix}.{ext}` next to the input.
    #[serde(default = "default_batch_template")]
    pub batch_template: String,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            ffmpeg_path: None,
            ffprobe_path: None,
            default_output_dir: PathBuf::new(),
            confirm_overwrite: default_confirm_overwrite(),
            theme: default_theme(),
            max_concurrent_jobs: default_concurrency(),
            batch_template: default_batch_template(),
        }
    }
}

fn default_confirm_overwrite() -> bool {
    true
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_concurrency() -> usize {
    1
}

fn default_batch_template() -> String {
    "{parent}/{stem}_{suffix}.{ext}".to_string()
}

/// Encoder defaults applied to form fields with matching ids at open
/// (`video_codec`, `crf`, `preset`, `audio_codec`, `audio_bitrate`).
/// Fields the operation does not declare are simply unaffected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EncoderDefaults {
    /// e.g. `"libx264"`.
    #[serde(default = "default_video_codec")]
    pub video_codec: String,
    /// e.g. `23`.
    #[serde(default = "default_crf")]
    pub crf: i64,
    /// e.g. `"medium"`.
    #[serde(default = "default_preset")]
    pub preset: String,
    /// e.g. `"aac"`.
    #[serde(default = "default_audio_codec")]
    pub audio_codec: String,
    /// e.g. `"128k"`.
    #[serde(default = "default_audio_bitrate")]
    pub audio_bitrate: String,
}

impl Default for EncoderDefaults {
    fn default() -> Self {
        Self {
            video_codec: default_video_codec(),
            crf: default_crf(),
            preset: default_preset(),
            audio_codec: default_audio_codec(),
            audio_bitrate: default_audio_bitrate(),
        }
    }
}

fn default_video_codec() -> String {
    "libx264".to_string()
}

fn default_crf() -> i64 {
    23
}

fn default_preset() -> String {
    "medium".to_string()
}

fn default_audio_codec() -> String {
    "aac".to_string()
}

fn default_audio_bitrate() -> String {
    "128k".to_string()
}

impl Settings {
    /// Platform config path, e.g. `~/.config/ffkit/config.toml` on Linux.
    /// `None` when no config dir is known (then settings stay in memory).
    pub fn config_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "ffkit")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }

    /// Load from the platform path. A missing file means defaults; an
    /// unreadable or invalid file also means defaults (logged, never fatal —
    /// a broken config must not brick the TUI; M6 surfaces a warning).
    pub fn load() -> Self {
        match Self::config_path() {
            Some(path) => Self::load_from(&path),
            None => Self::default(),
        }
    }

    /// Load from an explicit path. Same lenient contract as [`load`].
    /// Explicit-path I/O keeps this unit-testable without touching home dirs.
    pub fn load_from(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "ignoring unreadable config");
                return Self::default();
            }
        };
        match toml::from_str(&text) {
            Ok(settings) => settings,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "ignoring invalid config");
                Self::default()
            }
        }
    }

    /// Save to the platform path, creating parent dirs. Errors are returned
    /// (the caller decides how loudly to fail — M2 shows them on the
    /// missing-FFmpeg screen; they never panic).
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path().context("no platform config dir available")?;
        self.save_to(&path)
    }

    /// Save to an explicit path. Same testability rationale as `load_from`.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config dir {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serializing settings")?;
        std::fs::write(path, text).with_context(|| format!("writing config {}", path.display()))?;
        Ok(())
    }

    /// Clamp concurrency into the sane range [1, 8]. More than a handful of
    /// parallel encodes thrashes; the config value is advisory, not a weapon.
    pub fn effective_concurrency(&self) -> usize {
        self.general.max_concurrent_jobs.clamp(1, 8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch dir per test — sharing one dir across tests races
    /// under parallel execution (one test's cleanup deletes another's files).
    fn scratch_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ffkit-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn round_trips_through_toml() {
        let dir = scratch_dir("round-trip");
        let path = dir.join("config.toml");
        let settings = Settings {
            general: GeneralSettings {
                ffmpeg_path: Some(PathBuf::from("/opt/ffmpeg/bin/ffmpeg")),
                ffprobe_path: None,
                ..GeneralSettings::default()
            },
            ..Settings::default()
        };
        settings.save_to(&path).expect("save");
        assert_eq!(Settings::load_from(&path), settings);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_invalid_config_means_defaults() {
        let dir = scratch_dir("missing-or-invalid");
        assert_eq!(
            Settings::load_from(&dir.join("does-not-exist.toml")),
            Settings::default()
        );
        let bad = dir.join("bad.toml");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(&bad, "[[[not toml").expect("write bad config");
        assert_eq!(Settings::load_from(&bad), Settings::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spec_example_parses() {
        let dir = scratch_dir("spec-example");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"
[general]
default_output_dir = "/tmp/out"
confirm_overwrite = false
theme = "dark"
max_concurrent_jobs = 2

[defaults]
video_codec = "libx265"
crf = 28
preset = "slow"
audio_codec = "aac"
audio_bitrate = "96k"

[[presets]]
name = "Discord upload"
operation = "compress"
description = "Under 10 MB, 720p"

[presets.params]
crf = 28
resolution = "1280:-2"
audio_bitrate = "96k"
"#,
        )
        .expect("write spec config");
        let settings = Settings::load_from(&path);
        assert_eq!(settings.general.max_concurrent_jobs, 2);
        assert!(!settings.general.confirm_overwrite);
        assert_eq!(settings.defaults.video_codec, "libx265");
        assert_eq!(settings.presets.len(), 1);
        assert_eq!(settings.presets[0].name, "Discord upload");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrency_clamps_to_sane_range() {
        let mut settings = Settings::default();
        assert_eq!(settings.effective_concurrency(), 1);
        settings.general.max_concurrent_jobs = 0;
        assert_eq!(settings.effective_concurrency(), 1);
        settings.general.max_concurrent_jobs = 64;
        assert_eq!(settings.effective_concurrency(), 8);
    }
}
