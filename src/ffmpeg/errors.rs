//! stderr pattern matching → human-readable diagnostics (spec section 10).
//!
//! FFmpeg's stderr is dense and intimidating; the common failures below get
//! plain-language translations. The full raw stderr is always kept and is
//! one keystroke away (`l` toggles the log pane); the "copy error report"
//! action puts command + full stderr on the clipboard.

/// A translated failure: what happened, in plain language, plus what to try.
/// The raw text is never swallowed — [`JobResult`](crate::ffmpeg::runner::JobResult)
/// carries the tail and the log pane keeps the rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    /// Short title, e.g. `"Encoder not available"`.
    pub title: String,
    /// One or two sentences a non-expert can act on.
    pub detail: String,
    /// Concrete next step, when one exists.
    pub suggestion: Option<String>,
}

impl Diagnosis {
    fn new(title: &str, detail: String, suggestion: Option<&str>) -> Self {
        Self {
            title: title.to_string(),
            detail,
            suggestion: suggestion.map(str::to_string),
        }
    }
}

/// Translate ffmpeg stderr into a [`Diagnosis`]. Returns `None` when no
/// known pattern matches — the UI then shows the raw tail with the generic
/// "Conversion failed" framing instead of inventing a cause.
pub fn diagnose(stderr: &str) -> Option<Diagnosis> {
    if let Some(name) = match_pattern(stderr, "Unknown encoder") {
        return Some(Diagnosis::new(
            "Encoder not available",
            format!(
                "Your FFmpeg build doesn't include the encoder {name}. It may need a different build (e.g. with --enable-libx264) or a different codec choice."
            ),
            Some("Pick another video codec in the form — unavailable ones are greyed out with reasons."),
        ));
    }
    if stderr.contains("No such file or directory") {
        let path = last_quoted(stderr).unwrap_or_else(|| "the input".to_string());
        return Some(Diagnosis::new(
            "File not found",
            format!("Input file not found: {path}. It may have been moved, renamed, or deleted after you picked it."),
            Some("Go back and pick the file again."),
        ));
    }
    if stderr.contains("Permission denied") {
        let path = last_quoted(stderr).unwrap_or_else(|| "the output".to_string());
        return Some(Diagnosis::new(
            "Permission denied",
            format!("Can't write to {path} — check folder permissions."),
            Some("Choose an output directory you own, or fix permissions and retry."),
        ));
    }
    if stderr.contains("Invalid data found when processing input") {
        return Some(Diagnosis::new(
            "Unreadable input",
            "This file appears corrupt or isn't a media file FFmpeg recognizes.".to_string(),
            Some("Try playing it elsewhere; if nothing plays it, the file itself is the problem."),
        ));
    }
    if stderr.contains("Output file #0 does not contain any stream") {
        return Some(Diagnosis::new(
            "No streams left to write",
            "The filters removed all streams — check your settings.".to_string(),
            Some(
                "Stream-copy trims and track selections are the usual suspects; re-check the form.",
            ),
        ));
    }
    if stderr.contains("not divisible by 2") {
        return Some(Diagnosis::new(
            "Odd video dimensions",
            "This codec needs even dimensions, but the video (after scaling) has an odd width or height.".to_string(),
            Some("Scale with an even-preserving expression like scale=1280:-2 instead of an exact odd size."),
        ));
    }
    if stderr.contains("Conversion failed!") {
        return Some(Diagnosis::new(
            "Conversion failed",
            "FFmpeg reported a generic failure. The last stderr lines are shown below — the raw log has the full story.".to_string(),
            None,
        ));
    }
    None
}

/// Match `PREFIX 'name'` / `PREFIX "name"` and return `name` with quotes.
/// Used for `Unknown encoder 'libx265'` style lines.
fn match_pattern(stderr: &str, prefix: &str) -> Option<String> {
    stderr.lines().find_map(|line| {
        let rest = line.find(prefix).map(|i| &line[i + prefix.len()..])?;
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote != '\'' && quote != '"' {
            return None;
        }
        rest[1..].split(quote).next().map(str::to_string)
    })
}

/// Last single- or double-quoted span in the text (usually the offending
/// path in `path: Permission denied`-style lines).
fn last_quoted(stderr: &str) -> Option<String> {
    stderr.lines().rev().find_map(|line| {
        let single = line.rfind('\'').and_then(|end| {
            line[..end]
                .rfind('\'')
                .map(|start| line[start + 1..end].to_string())
        });
        let double = line.rfind('"').and_then(|end| {
            line[..end]
                .rfind('"')
                .map(|start| line[start + 1..end].to_string())
        });
        single.or(double)
    })
}

/// Last `n` lines of captured stderr for the failure card.
pub fn tail_lines(lines: &[String], n: usize) -> Vec<String> {
    let skip = lines.len().saturating_sub(n);
    lines[skip..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_encoder_names_the_encoder() {
        let diagnosis = diagnose("  Unknown encoder 'libx265'\n").expect("must match");
        assert_eq!(diagnosis.title, "Encoder not available");
        assert!(diagnosis.detail.contains("libx265"), "{}", diagnosis.detail);
        assert!(diagnosis.suggestion.is_some());
    }

    #[test]
    fn missing_input_names_the_path() {
        let diagnosis = diagnose("/v/clip.mp4: No such file or directory\n").expect("must match");
        assert_eq!(diagnosis.title, "File not found");
    }

    #[test]
    fn permission_denied_points_at_permissions() {
        let diagnosis = diagnose("'/out/x.mp4': Permission denied\n").expect("must match");
        assert_eq!(diagnosis.title, "Permission denied");
        assert!(
            diagnosis.detail.contains("/out/x.mp4"),
            "{}",
            diagnosis.detail
        );
    }

    #[test]
    fn corrupt_input_is_plain_language() {
        let diagnosis = diagnose("Invalid data found when processing input\n").expect("must match");
        assert_eq!(diagnosis.title, "Unreadable input");
    }

    #[test]
    fn empty_output_names_the_filter_cause() {
        let diagnosis =
            diagnose("Output file #0 does not contain any stream\n").expect("must match");
        assert_eq!(diagnosis.title, "No streams left to write");
    }

    #[test]
    fn odd_dimensions_suggest_the_fix() {
        let diagnosis =
            diagnose("[libx264] height not divisible by 2 (1299x719)\n").expect("must match");
        assert_eq!(diagnosis.title, "Odd video dimensions");
        assert!(diagnosis
            .suggestion
            .unwrap_or_default()
            .contains("scale=1280:-2"));
    }

    #[test]
    fn generic_failure_still_matches() {
        let diagnosis = diagnose("Conversion failed!\n").expect("must match");
        assert_eq!(diagnosis.title, "Conversion failed");
    }

    #[test]
    fn unknown_stderr_matches_nothing() {
        assert_eq!(diagnose("some exotic internal error\n"), None);
    }

    #[test]
    fn tail_keeps_the_last_n_lines() {
        let lines: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let tail = tail_lines(&lines, 20);
        assert_eq!(tail.len(), 20);
        assert_eq!(tail[0], "line 10");
    }
}
