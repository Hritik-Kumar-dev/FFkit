//! The command builder — the core of ffkit (spec section 5).
//!
//! `build()` is a pure function: parameter state in, [`CommandSpec`] out.
//! No I/O, no side effects — trivially testable via snapshot tests.
//!
//! A spec renders two ways: an argv vector for `tokio::process::Command`
//! and a shell-quoted display string for the preview pane and clipboard.
//! Argv is never built by splitting a string — that is how filenames with
//! spaces break.

use std::path::{Path, PathBuf};

/// A fully-specified ffmpeg invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Binary to run, e.g. `"ffmpeg"` or a user-configured absolute path.
    pub program: String,
    /// Properly separated argv, never pre-joined.
    pub args: Vec<String>,
    /// Commands that must run first (e.g. palettegen pass, concat list file).
    pub pre_commands: Vec<CommandSpec>,
    /// Working directory override.
    pub working_dir: Option<PathBuf>,
    /// Concat demuxer list file: written by the runner before the main
    /// command runs (pure builders cannot do I/O). `None` for all other ops.
    pub concat_list: Option<ConcatListFile>,
    /// One plain-English explanation per flag group for the teaching pane.
    pub explanation: Vec<FlagExplanation>,
}

/// A `concat` demuxer list file the runner materializes next to the output:
/// one `file '…'` line per input, then `-f concat -safe 0 -i <list>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcatListFile {
    /// Where the runner writes the list.
    pub path: PathBuf,
    /// Inputs in join order.
    pub inputs: Vec<PathBuf>,
}

/// Maps one flag to the plain-English text shown under the command preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagExplanation {
    /// The flag as shown, e.g. `"-crf 23"`.
    pub flag: String,
    /// What it means, e.g. `"Quality level — lower means better quality"`.
    pub plain_english: String,
}

impl CommandSpec {
    /// Empty spec for `program`.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            pre_commands: Vec::new(),
            working_dir: None,
            concat_list: None,
            explanation: Vec::new(),
        }
    }

    /// Push one argv element. Chainable.
    pub fn arg(&mut self, arg: impl Into<String>) -> &mut Self {
        self.args.push(arg.into());
        self
    }

    /// Push a flag and record its teaching explanation. Chainable.
    pub fn flag(&mut self, flag: impl Into<String>, plain_english: impl Into<String>) -> &mut Self {
        let flag = flag.into();
        self.args.push(flag.clone());
        self.explanation.push(FlagExplanation {
            flag,
            plain_english: plain_english.into(),
        });
        self
    }

    /// Push a flag with a separate value argv element, e.g. `-crf`, `23`.
    /// The explanation names them together (`-crf 23`).
    pub fn flag_value(
        &mut self,
        flag: &str,
        value: impl Into<String>,
        plain_english: impl Into<String>,
    ) -> &mut Self {
        let value = value.into();
        self.args.push(flag.to_string());
        self.args.push(value.clone());
        self.explanation.push(FlagExplanation {
            flag: format!("{flag} {value}"),
            plain_english: plain_english.into(),
        });
        self
    }

    /// Full argv including the program: `[program, args…]`.
    pub fn to_argv(&self) -> Vec<String> {
        let mut argv = Vec::with_capacity(self.args.len() + 1);
        argv.push(self.program.clone());
        argv.extend(self.args.iter().cloned());
        argv
    }

    /// Shell-quoted display string: pre-commands first (one per line), then
    /// the main command wrapped with `\` continuations. This is what the
    /// preview pane shows and what `c` copies.
    pub fn to_display(&self) -> String {
        let mut lines: Vec<String> = self
            .pre_commands
            .iter()
            .map(|pre| display_command(&pre.program, &pre.args))
            .collect();
        lines.push(display_command(&self.program, &self.args));
        lines.join("\n")
    }

    /// Output path by convention: builders always push the output operand
    /// last. Batch enqueue reads this back for naming templates.
    pub fn output_path(&self) -> Option<PathBuf> {
        self.args.last().map(PathBuf::from)
    }
}

/// Append the non-negotiable globals: `-hide_banner` (less stderr noise),
/// `-y`/`-n` (ffmpeg must never block on an interactive overwrite prompt —
/// the TUI owns the terminal and the prompt would deadlock), and the
/// machine-readable progress channel `-progress pipe:1 -nostats` (spec §7).
///
/// Argument order follows ffmpeg semantics: globals first, then per-input
/// options, `-i`, per-output options, output path. Call this first, then
/// push `-i` + inputs, then output options + output.
pub fn push_globals(spec: &mut CommandSpec, overwrite: bool) {
    spec.arg("-hide_banner");
    spec.arg(if overwrite { "-y" } else { "-n" });
    spec.arg("-progress");
    spec.arg("pipe:1");
    spec.arg("-nostats");
}

/// Quote one argv element for display. POSIX single-quote rules on Unix
/// (embedded `'` becomes `'\''`); double-quote rules on Windows.
/// Safe characters pass through unquoted for readability.
pub fn shell_quote(arg: &str) -> String {
    #[cfg(windows)]
    {
        return windows_quote(arg);
    }
    #[cfg(not(windows))]
    {
        if arg.is_empty() {
            return "''".to_string();
        }
        let safe = arg.bytes().all(|b| {
            matches!(
                b,
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'@' | b'%' | b'+' | b'=' | b':' | b',' | b'.' | b'/' | b'-'
            )
        });
        if safe {
            return arg.to_string();
        }
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// Windows quoting: wrap in double quotes when the arg contains whitespace
/// or special characters; embedded `"` becomes `\"`.
#[cfg(windows)]
fn windows_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    let needs_quotes = arg.bytes().any(|b| {
        matches!(
            b,
            b' ' | b'\t' | b'"' | b'&' | b'|' | b'<' | b'>' | b'^' | b'%'
        )
    });
    if !needs_quotes {
        return arg.to_string();
    }
    format!("\"{}\"", arg.replace('"', "\\\""))
}

/// Render one command as a shell string, wrapping at `WRAP_WIDTH` columns on
/// arg boundaries with `\` continuations. Deterministic — snapshot tests
/// assert on it.
pub fn display_command(program: &str, args: &[String]) -> String {
    const WRAP_WIDTH: usize = 76;
    const CONTINUATION: &str = " \\\n  ";
    let mut out = shell_quote(program);
    let mut line_len = out.len();
    for arg in args {
        let quoted = shell_quote(arg);
        // +1 for the separating space.
        if line_len + 1 + quoted.len() > WRAP_WIDTH && line_len > 0 {
            out.push_str(CONTINUATION);
            line_len = 2;
        } else {
            out.push(' ');
            line_len += 1;
        }
        out.push_str(&quoted);
        line_len += quoted.len();
    }
    out
}

/// Make a path safe as an ffmpeg operand: a file named `-rf.mp4` must not
/// become a flag. Absolute paths and `sub/dir` paths are already safe; a
/// bare leading-dash name gets a `./` prefix (spec §14).
pub fn safe_path_arg(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.starts_with('-') {
        format!("./{text}")
    } else {
        text.into_owned()
    }
}

/// Default output name for batch/single runs: `{stem}_{suffix}.{ext}` next
/// to the input (spec §12 naming template). `ext` replaces the input
/// extension; pass the input's own extension to keep the container.
pub fn default_output_name(input: &Path, suffix: &str, ext: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    let file_name = format!("{stem}_{suffix}.{ext}");
    match input.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(file_name),
        _ => PathBuf::from(file_name),
    }
}

/// Container extension of `path`, lowercased, without the dot.
/// Empty when the input has no extension.
pub fn input_extension(input: &Path) -> String {
    input
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_handles_hostile_filenames() {
        assert_eq!(shell_quote("simple.mp4"), "simple.mp4");
        assert_eq!(
            shell_quote("my holiday video.mp4"),
            "'my holiday video.mp4'"
        );
        assert_eq!(shell_quote("it's.mp4"), "'it'\\''s.mp4'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("café_日本語.mp4"), "'café_日本語.mp4'");
        // A leading dash is shell-safe but NOT flag-safe — see safe_path_arg.
        assert_eq!(shell_quote("-rf.mp4"), "-rf.mp4");
    }

    #[test]
    fn leading_dash_filenames_get_dot_slash() {
        assert_eq!(safe_path_arg(Path::new("-rf.mp4")), "./-rf.mp4");
        assert_eq!(safe_path_arg(Path::new("clip.mp4")), "clip.mp4");
        assert_eq!(safe_path_arg(Path::new("sub/-rf.mp4")), "sub/-rf.mp4");
        assert_eq!(safe_path_arg(Path::new("/abs/-rf.mp4")), "/abs/-rf.mp4");
    }

    #[test]
    fn output_naming_uses_stem_suffix_ext_template() {
        assert_eq!(
            default_output_name(Path::new("/v/holiday.mp4"), "compressed", "mp4"),
            PathBuf::from("/v/holiday_compressed.mp4")
        );
        assert_eq!(
            default_output_name(Path::new("clip.mkv"), "trimmed", "mp4"),
            PathBuf::from("clip_trimmed.mp4")
        );
    }

    #[test]
    fn display_wraps_with_continuations() {
        let display = display_command(
            "ffmpeg",
            &[
                "-hide_banner".into(),
                "-y".into(),
                "-i".into(),
                "my video.mp4".into(),
            ],
        );
        assert!(display.starts_with("ffmpeg -hide_banner -y -i 'my video.mp4'"));
        let long: Vec<String> = (0..30).map(|i| format!("-opt{i}")).collect();
        let wrapped = display_command("ffmpeg", &long);
        assert!(wrapped.contains(" \\\n  "), "long commands must wrap");
        // Every continuation line is indented.
        for line in wrapped.lines().skip(1) {
            assert!(line.starts_with("  "), "line not indented: {line}");
        }
    }

    #[test]
    fn argv_never_splits_strings() {
        let mut spec = CommandSpec::new("ffmpeg");
        push_globals(&mut spec, true);
        spec.arg("-i");
        spec.arg("my video.mp4");
        let argv = spec.to_argv();
        assert_eq!(
            argv,
            vec![
                "ffmpeg",
                "-hide_banner",
                "-y",
                "-progress",
                "pipe:1",
                "-nostats",
                "-i",
                "my video.mp4"
            ]
        );
    }
}
