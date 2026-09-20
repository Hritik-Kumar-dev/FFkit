//! File browser: navigation, selection, and the lazy probe pane (M2).
//!
//! - Directory navigation with vim keys and arrows (`h`/Backspace goes up)
//! - Media-file filtering by default, `a` toggles showing all files
//! - Multi-select with Space; Enter opens dirs / confirms files
//! - Type-ahead substring filter; `/` jumps to a path; `~` goes home
//! - The highlighted media file is probed lazily (see [`BrowserAction`]);
//!   results render in the side pane via the app's probe cache.
//!
//! Directory reads are synchronous `std::fs` calls made in direct response
//! to a keypress. That is a deliberate scope decision for M2 (see
//! DECISIONS.md): local `readdir` is fast, while the expensive I/O —
//! ffprobe and ffmpeg — is always async. Network mounts may hitch a frame;
//! async directory listing is queued as follow-up work if it bites.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::app::{App, ProbeState};
use crate::ui::theme::Theme;

/// Extensions treated as media (shown when filtering is on). Lowercase,
/// without the dot. Subtitle sidecars are included so the Subtitles operation
/// can see them.
const MEDIA_EXTENSIONS: &[&str] = &[
    // video
    "mp4", "mkv", "mov", "avi", "webm", "m4v", "ts", "m2ts", "mts", "mpg", "mpeg", "wmv", "flv",
    "ogv", "3gp", "3g2", "asf", "rm", "rmvb", "vob", "dv", // audio
    "mp3", "aac", "m4a", "opus", "ogg", "oga", "flac", "wav", "wma", "aiff", "aif", "alac", "ac3",
    "dts", "amr", // image
    "png", "jpg", "jpeg", "webp", "gif", "bmp", "tiff", "tif", "avif", "heic", "heif", "jxl",
    "pnm", "ppm", "pgm", // subtitle sidecars
    "srt", "ass", "ssa", "vtt", "sub",
];

/// True when `name` has a media extension (case-insensitive).
pub fn is_media_file(name: &str) -> bool {
    name.rsplit('.')
        .next()
        .is_some_and(|ext| MEDIA_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// One row in the browser list.
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// Full path.
    pub path: PathBuf,
    /// File name for display.
    pub name: String,
    /// Directories sort first and render with a trailing `/`.
    pub is_dir: bool,
    /// File size; `None` for directories and unreadable metadata.
    pub size: Option<u64>,
    /// Shown when the media filter is on.
    pub is_media: bool,
}

/// Sort: directories first, then case-insensitive name order. Pure — unit-tested.
pub fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

/// Filter to what the list shows. Pure — unit-tested.
pub fn filter_entries<'a>(all: &'a [DirEntry], show_all: bool, filter: &str) -> Vec<&'a DirEntry> {
    let needle = filter.to_lowercase();
    all.iter()
        .filter(|e| show_all || e.is_dir || e.is_media)
        .filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle))
        .collect()
}

/// Stateful browser. Owned by [`App`](crate::app::App).
pub struct BrowserState {
    /// Directory being listed.
    pub cwd: PathBuf,
    /// All entries in `cwd` (sorted, unfiltered).
    all: Vec<DirEntry>,
    /// Visible entries after filter/show_all.
    entries: Vec<DirEntry>,
    /// Cursor into `entries`.
    pub cursor: usize,
    /// Marked files for batch operations.
    pub marked: HashSet<PathBuf>,
    /// Show every file, not just media.
    pub show_all: bool,
    /// Type-ahead substring filter.
    pub filter: String,
    /// `/` path-entry mode.
    pub path_mode: bool,
    /// Path being typed in path-entry mode.
    pub path_input: Input,
    /// Last directory-read error, shown in the footer.
    pub last_error: Option<String>,
}

/// What a browser keypress means to the app.
pub enum BrowserAction {
    /// Nothing further needed (plain navigation; the app re-probes).
    None,
    /// User confirmed file(s) — proceed to the parameter form.
    Confirmed(Vec<PathBuf>),
    /// Show this in the status line (e.g. jump failures).
    Status(String),
    /// Leave the browser (back to the operation picker).
    Back,
}

impl BrowserState {
    /// Open a browser on `dir`, falling back to the home dir and then `/`
    /// when the start dir cannot be listed.
    pub fn open(dir: PathBuf) -> Self {
        let mut state = Self {
            cwd: dir,
            all: Vec::new(),
            entries: Vec::new(),
            cursor: 0,
            marked: HashSet::new(),
            show_all: false,
            filter: String::new(),
            path_mode: false,
            path_input: Input::default(),
            last_error: None,
        };
        state.refresh();
        if state.all.is_empty() && state.last_error.is_some() {
            for dir in [home_dir(), Some(PathBuf::from("/"))].into_iter().flatten() {
                state.cwd = dir;
                state.refresh();
                if state.last_error.is_none() {
                    break;
                }
            }
        }
        state
    }

    /// Re-read `cwd` and re-apply the filter. Marks survive refreshes when
    /// the paths still exist; the cursor is clamped.
    pub fn refresh(&mut self) {
        match read_entries(&self.cwd) {
            Ok(mut all) => {
                sort_entries(&mut all);
                self.all = all;
                self.last_error = None;
            }
            Err(e) => {
                self.all = Vec::new();
                self.last_error = Some(e);
            }
        }
        self.apply_filter();
    }

    /// Recompute the visible list from `all` + filter flags.
    fn apply_filter(&mut self) {
        self.entries = filter_entries(&self.all, self.show_all, &self.filter)
            .into_iter()
            .cloned()
            .collect();
        self.cursor = self.cursor.min(self.entries.len().saturating_sub(1));
    }

    /// Currently highlighted entry, if the list is non-empty.
    pub fn highlighted(&self) -> Option<&DirEntry> {
        self.entries.get(self.cursor)
    }

    /// Move the cursor, wrapping around.
    fn move_cursor(&mut self, delta: i32) {
        if self.entries.is_empty() {
            return;
        }
        let n = self.entries.len() as i32;
        self.cursor = (self.cursor as i32 + delta).rem_euclid(n) as usize;
    }

    /// Descend into a directory (or the parent with `..` handling at the fs
    /// level). Marks are cleared — they belong to the previous directory.
    fn descend(&mut self, dir: PathBuf) {
        self.cwd = dir;
        self.cursor = 0;
        self.marked.clear();
        self.filter.clear();
        self.refresh();
    }

    /// Handle one keypress. Never blocks on subprocesses; probing is the
    /// app's job after each key (it knows the binary paths and the cache).
    pub fn on_key(&mut self, key: KeyEvent) -> BrowserAction {
        if self.path_mode {
            return self.on_key_path_mode(key);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
            KeyCode::Home | KeyCode::Char('g') => self.cursor = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.cursor = self.entries.len().saturating_sub(1);
            }
            KeyCode::Backspace if !self.filter.is_empty() => {
                self.filter.pop();
                self.apply_filter();
            }
            KeyCode::Backspace | KeyCode::Char('h') => {
                if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
                    self.descend(parent);
                }
            }
            KeyCode::Char('~') => {
                if let Some(home) = home_dir() {
                    self.descend(home);
                }
            }
            KeyCode::Char('/') => {
                self.path_mode = true;
                self.path_input = Input::new(self.cwd.to_string_lossy().into_owned());
            }
            KeyCode::Char('a') => {
                self.show_all = !self.show_all;
                self.apply_filter();
            }
            KeyCode::Char(' ') => {
                if let Some(entry) = self.highlighted() {
                    if !entry.is_dir {
                        let path = entry.path.clone();
                        if !self.marked.insert(path.clone()) {
                            self.marked.remove(&path);
                        }
                    }
                }
            }
            KeyCode::Enter => {
                let Some(entry) = self.highlighted().cloned() else {
                    return BrowserAction::None;
                };
                if entry.is_dir {
                    self.descend(entry.path);
                    return BrowserAction::None;
                }
                let mut inputs: Vec<PathBuf> = self.marked.iter().cloned().collect();
                if !inputs.contains(&entry.path) {
                    inputs.push(entry.path);
                }
                inputs.sort();
                return BrowserAction::Confirmed(inputs);
            }
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.apply_filter();
                } else {
                    return BrowserAction::Back;
                }
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() && !c.is_control() && self.is_type_ahead(c) =>
            {
                self.filter.push(c);
                self.cursor = 0;
                self.apply_filter();
            }
            _ => {}
        }
        BrowserAction::None
    }

    /// Whether `c` should extend the type-ahead filter. `?` is reserved for
    /// help, `q` for quit — both handled globally before reaching the browser.
    fn is_type_ahead(&self, c: char) -> bool {
        !matches!(c, '?' | 'q')
    }

    /// Keys while `/` path entry is active.
    fn on_key_path_mode(&mut self, key: KeyEvent) -> BrowserAction {
        match key.code {
            KeyCode::Esc => {
                self.path_mode = false;
            }
            KeyCode::Enter => {
                self.path_mode = false;
                let raw = self.path_input.value().to_string();
                let target = expand_tilde(&raw);
                let target = PathBuf::from(&target);
                if target.is_dir() {
                    self.descend(target);
                } else if target.is_file() {
                    if let Some(parent) = target.parent().map(Path::to_path_buf) {
                        self.descend(parent);
                    }
                    return BrowserAction::Confirmed(vec![target]);
                } else {
                    return BrowserAction::Status(format!("No such file or directory: {raw}"));
                }
            }
            _ => {
                self.path_input
                    .handle_event(&crossterm::event::Event::Key(key));
            }
        }
        BrowserAction::None
    }
}

/// Read one directory level. Errors name the directory (the
/// "Input file not found / Permission denied" family from spec section 10,
/// surfaced in-browser instead of as a crash).
fn read_entries(dir: &Path) -> Result<Vec<DirEntry>, String> {
    let read_dir =
        std::fs::read_dir(dir).map_err(|e| format!("Cannot list {}: {e}", dir.display()))?;
    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().is_some_and(std::fs::Metadata::is_dir);
        entries.push(DirEntry {
            path,
            name: name.clone(),
            is_dir,
            size: meta.filter(|m| m.is_file()).map(|m| m.len()),
            is_media: !is_dir && is_media_file(&name),
        });
    }
    Ok(entries)
}

/// Home directory for `~`. `None` on exotic setups — callers skip.
fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

/// Expand a leading `~` to the home dir for `/`-entered paths.
fn expand_tilde(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix('~') {
        if let Some(home) = home_dir() {
            return format!("{}{rest}", home.display());
        }
    }
    raw.to_string()
}

/// Render the browser: header, file list + info pane, footer.
pub fn render(frame: &mut Frame, app: &App, theme: &Theme) {
    let browser = &app.browser;
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(area);

    let header = Paragraph::new(vec![
        Line::from(Span::styled(
            format!("Pick input — {}", app.selected_operation_meta().name),
            theme.title(),
        )),
        Line::from(Span::styled(
            browser.cwd.display().to_string(),
            theme.muted_style(),
        )),
    ])
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, chunks[0]);

    let columns = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[1]);
    render_list(frame, columns[0], app, theme);
    render_info(frame, columns[1], app, theme);

    if browser.path_mode {
        let input_line = format!("/ {}", browser.path_input.value());
        let footer = Paragraph::new(input_line).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Jump to path (Enter jumps, Esc cancels)"),
        );
        frame.render_widget(footer, chunks[2]);
        let cursor_x = chunks[2].x + 3 + browser.path_input.visual_cursor() as u16;
        frame.set_cursor_position((cursor_x, chunks[2].y + 1));
    } else {
        let mut hints = String::from("↑↓ move · Enter open/choose · Space mark");
        if !browser.marked.is_empty() {
            hints.push_str(&format!(" · {} marked", browser.marked.len()));
        }
        if !browser.filter.is_empty() {
            hints.push_str(&format!(" · filter: {}", browser.filter));
        }
        if let Some(err) = browser.last_error.as_ref() {
            hints = err.clone();
        } else if let Some(status) = app.status_message.as_ref() {
            hints = status.clone();
        }
        let footer = Paragraph::new(hints).block(Block::default().borders(Borders::ALL));
        frame.render_widget(footer, chunks[2]);
    }
}

/// File list with scroll windowing so long directories stay navigable.
fn render_list(frame: &mut Frame, area: ratatui::layout::Rect, app: &App, theme: &Theme) {
    let browser = &app.browser;
    let visible_rows = (area.height.saturating_sub(2)) as usize;
    let start = if visible_rows == 0 {
        0
    } else {
        browser
            .cursor
            .saturating_sub(visible_rows.saturating_sub(1))
            .min(browser.entries.len().saturating_sub(visible_rows))
    };
    let items: Vec<ListItem> = browser
        .entries
        .iter()
        .skip(start)
        .take(visible_rows.max(1))
        .map(|e| {
            let mark = if app.browser.marked.contains(&e.path) {
                "● "
            } else {
                "  "
            };
            let name = if e.is_dir {
                format!("{mark}{}/", e.name)
            } else {
                format!("{mark}{}", e.name)
            };
            let mut item = ListItem::new(name);
            if !e.is_dir && !e.is_media && !browser.show_all {
                item = item.style(theme.muted_style());
            }
            item
        })
        .collect();
    let count = if browser.entries.len() == browser.all.len() {
        format!("{} entries", browser.entries.len())
    } else {
        format!("{} of {} entries", browser.entries.len(), browser.all.len())
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Files — {count}")),
        )
        .highlight_style(theme.selected())
        .highlight_symbol("▸ ");
    let mut state = ListState::default();
    if !browser.entries.is_empty() {
        state.select(Some(browser.cursor.saturating_sub(start)));
    }
    frame.render_stateful_widget(list, area, &mut state);
}

/// Side pane: highlighted file facts + lazy probe summary.
fn render_info(frame: &mut Frame, area: ratatui::layout::Rect, app: &App, theme: &Theme) {
    let mut lines = Vec::new();
    match app.browser.highlighted() {
        None => lines.push(Line::from(Span::styled(
            "Empty directory",
            theme.muted_style(),
        ))),
        Some(entry) => {
            lines.push(Line::from(Span::styled(entry.name.clone(), theme.title())));
            lines.push(Line::from(Span::styled(
                if entry.is_dir { "directory" } else { "file" },
                theme.muted_style(),
            )));
            if let Some(size) = entry.size {
                lines.push(Line::from(Span::styled(
                    crate::ffmpeg::probe::format_size(size),
                    theme.muted_style(),
                )));
            }
            lines.push(Line::from(""));
            if entry.is_dir {
                lines.push(Line::from(Span::styled(
                    "Enter opens this directory",
                    theme.footer(),
                )));
            } else if !entry.is_media {
                lines.push(Line::from(Span::styled(
                    "Not a media file; showing all files is on with `a`",
                    theme.footer(),
                )));
            } else {
                match app.probes.get(&entry.path) {
                    None => lines.push(Line::from(Span::styled("…", theme.footer()))),
                    Some(ProbeState::Pending) => {
                        lines.push(Line::from(Span::styled("Probing…", theme.footer())));
                    }
                    Some(ProbeState::Ready(result)) => {
                        lines.push(Line::from(Span::styled(
                            result.summary_line(),
                            theme.command_style(),
                        )));
                        if let Some(rot) = result.video_stream().and_then(|s| s.rotation) {
                            if rot != 0.0 {
                                lines.push(Line::from(Span::styled(
                                    format!("Rotated {rot}° — affects output orientation"),
                                    theme.warning_style(),
                                )));
                            }
                        }
                    }
                    Some(ProbeState::Failed(message)) => {
                        lines.push(Line::from(Span::styled(
                            format!("Unreadable: {message}"),
                            theme.warning_style(),
                        )));
                    }
                }
            }
        }
    }
    let info =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Media info"));
    frame.render_widget(info, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> DirEntry {
        DirEntry {
            path: PathBuf::from(name),
            name: name.to_string(),
            is_dir,
            size: None,
            is_media: !is_dir && is_media_file(name),
        }
    }

    #[test]
    fn media_detection_covers_common_containers() {
        assert!(is_media_file("clip.MP4"));
        assert!(is_media_file("song.flac"));
        assert!(is_media_file("frame.webp"));
        assert!(is_media_file("subs.srt"));
        assert!(!is_media_file("notes.txt"));
        assert!(!is_media_file("Makefile"));
        assert!(!is_media_file("archive.tar.gz"));
    }

    #[test]
    fn sorting_puts_dirs_first_case_insensitively() {
        let mut entries = vec![
            entry("zebra.mp4", false),
            entry("Docs", true),
            entry("apple.mp4", false),
            entry("bin", true),
        ];
        sort_entries(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["bin", "Docs", "apple.mp4", "zebra.mp4"]);
    }

    #[test]
    fn media_filter_hides_non_media_but_keeps_dirs() {
        let all = vec![
            entry("video.mp4", false),
            entry("notes.txt", false),
            entry("subdir", true),
        ];
        let visible = filter_entries(&all, false, "");
        assert_eq!(visible.len(), 2);
        let visible = filter_entries(&all, true, "");
        assert_eq!(visible.len(), 3);
        let visible = filter_entries(&all, true, "VID");
        assert_eq!(visible.len(), 1);
    }

    #[test]
    fn tilde_expands_only_as_prefix() {
        assert!(expand_tilde("~/x").ends_with("/x") || expand_tilde("~/x").ends_with("\\x"));
        assert_eq!(expand_tilde("/abs/path"), "/abs/path");
    }
}
