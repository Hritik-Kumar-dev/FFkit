//! Terminal image support for the trim filmstrip (§10).
//!
//! Capability is detected once at startup from the environment (alongside
//! the ffmpeg detection): Kitty graphics when `KITTY_WINDOW_ID` is set,
//! iTerm2 inline images when `TERM_PROGRAM` is `iTerm.app`, and a
//! colored-block ANSI approximation otherwise (decoded with the `image`
//! crate — no graphics protocol needed, works everywhere truecolor does).
//! When no thumbnails are available at all, the timeline degrades to
//! timestamp ticks rather than nothing.
//!
//! Native Sixel encoding is deliberately out of scope (documented in
//! DECISIONS.md): the terminals that speak Sixel are rare, and the block
//! approximation covers the no-graphics case on all of them.
//!
//! Inline images bypass ratatui: escape sequences inside buffered cells
//! would corrupt the diff engine, so [`App`](crate::app::App) collects
//! payloads during render and the main loop prints them after each draw.

use ratatui::style::Color;
use ratatui::text::Span;

/// How the filmstrip renders on this terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBackend {
    /// Kitty graphics protocol (`KITTY_WINDOW_ID` set).
    Kitty,
    /// iTerm2 inline images (`TERM_PROGRAM` is `iTerm.app`).
    ITerm2,
    /// Colored-block approximation rendered as native ratatui spans.
    Blocks,
}

/// Detect once at startup. Pure over the environment — unit-tested with
/// injected vars.
pub fn detect() -> ImageBackend {
    detect_with(&std::env::vars().collect::<Vec<_>>())
}

fn detect_with(vars: &[(String, String)]) -> ImageBackend {
    let get = |name: &str| {
        vars.iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    if !get("KITTY_WINDOW_ID").is_empty() {
        ImageBackend::Kitty
    } else if get("TERM_PROGRAM") == "iTerm.app" {
        ImageBackend::ITerm2
    } else {
        ImageBackend::Blocks
    }
}

/// One filmstrip cell: terminal columns wide, rows tall, per thumbnail.
pub const THUMB_COLS: usize = 12;
/// Rows per thumbnail in the strip.
pub const THUMB_ROWS: usize = 3;

/// Render one thumbnail file as [`THUMB_COLS`]×[`THUMB_ROWS`] colored
/// blocks. Each cell averages its region to a single background color —
/// crude, but scene changes read clearly across the strip. Undecodable
/// files become blank cells (the timeline falls back to ticks per row).
pub fn blocks_for(path: &std::path::Path) -> Vec<Vec<Color>> {
    let empty = vec![vec![Color::Reset; THUMB_COLS]; THUMB_ROWS];
    let Ok(image) = image::open(path) else {
        return empty;
    };
    let small = image.thumbnail(THUMB_COLS as u32 * 8, THUMB_ROWS as u32 * 16);
    let rgb = small.to_rgb8();
    let (width, height) = (rgb.width().max(1), rgb.height().max(1));
    let mut rows = Vec::with_capacity(THUMB_ROWS);
    for row in 0..THUMB_ROWS {
        let mut cells = Vec::with_capacity(THUMB_COLS);
        for col in 0..THUMB_COLS {
            let x0 = col as u32 * width / THUMB_COLS as u32;
            let x1 = (col as u32 + 1) * width / THUMB_COLS as u32;
            let y0 = row as u32 * height / THUMB_ROWS as u32;
            let y1 = (row as u32 + 1) * height / THUMB_ROWS as u32;
            let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
            for y in y0..y1.max(y0 + 1) {
                for x in x0..x1.max(x0 + 1) {
                    let pixel = rgb.get_pixel(x.min(width - 1), y.min(height - 1));
                    r += pixel[0] as u64;
                    g += pixel[1] as u64;
                    b += pixel[2] as u64;
                    n += 1;
                }
            }
            cells.push(Color::Rgb(
                (r / n.max(1)) as u8,
                (g / n.max(1)) as u8,
                (b / n.max(1)) as u8,
            ));
        }
        rows.push(cells);
    }
    rows
}

/// Build a block row line: one background-colored space per cell, with a
/// one-column gap between thumbnails. Pure — unit-tested without a terminal.
pub fn block_line(thumbs: &[Vec<Vec<Color>>], row: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (i, thumb) in thumbs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        let cells = thumb.get(row).map(Vec::as_slice).unwrap_or(&[]);
        for color in cells.iter().take(THUMB_COLS) {
            spans.push(Span::styled(
                " ".to_string(),
                ratatui::style::Style::default().bg(*color),
            ));
        }
    }
    spans
}

/// Encode bytes as standard base64 (no external dependency — the payloads
/// are small thumbnails, and this stays dependency-lean).
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for chunk in bytes.chunks(3) {
        let bits = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | (*chunk.get(2).unwrap_or(&0) as u32);
        out.push(ALPHABET[(bits >> 18) as usize & 63] as char);
        out.push(ALPHABET[(bits >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(bits >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[bits as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Kitty graphics payload for one JPEG: `c`/`r` cell sizing lets the
/// terminal scale, so fixed small thumbs stay crisp. Chunked at 4096
/// payload bytes per escape as the protocol requires.
pub fn kitty_payload(jpeg: &[u8], cols: u32, rows: u32) -> String {
    let encoded = base64_encode(jpeg);
    let mut out = String::new();
    let mut first = true;
    let mut chunks = encoded.as_bytes().chunks(4096).peekable();
    while let Some(chunk) = chunks.next() {
        let last = chunks.peek().is_none();
        let mode = if last { 0 } else { 1 };
        let text = String::from_utf8_lossy(chunk);
        if first {
            out.push_str(&format!(
                "\x1b_Ga=T,f=100,c={cols},r={rows},m={mode};{text}\x1b\\"
            ));
            first = false;
        } else {
            out.push_str(&format!("\x1b_Gm={mode};{text}\x1b\\"));
        }
    }
    out
}

/// iTerm2 inline-image payload for one JPEG at `cols` terminal cells wide.
pub fn iterm2_payload(jpeg: &[u8], cols: u32) -> String {
    format!(
        "\x1b]1337;File=inline=1,width={cols}:{}\x07",
        base64_encode(jpeg)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn detection_prefers_kitty_then_iterm2() {
        assert_eq!(
            detect_with(&vars(&[("KITTY_WINDOW_ID", "1")])),
            ImageBackend::Kitty
        );
        assert_eq!(
            detect_with(&vars(&[("TERM_PROGRAM", "iTerm.app")])),
            ImageBackend::ITerm2
        );
        assert_eq!(detect_with(&vars(&[])), ImageBackend::Blocks);
        // Kitty wins when both are set.
        assert_eq!(
            detect_with(&vars(&[
                ("KITTY_WINDOW_ID", "1"),
                ("TERM_PROGRAM", "iTerm.app")
            ])),
            ImageBackend::Kitty
        );
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn kitty_payload_chunks_and_terminates() {
        let big = vec![0xFFu8; 9000];
        let payload = kitty_payload(&big, 12, 3);
        // 9000 bytes -> 12000 b64 chars -> 3 chunks (4096+4096+3808).
        assert_eq!(payload.matches("\x1b_G").count(), 3);
        assert!(payload.contains("m=0;"), "last chunk closes the stream");
        assert!(payload.contains("c=12,r=3"), "first chunk sizes in cells");
    }

    #[test]
    fn iterm2_payload_is_single_shot() {
        let payload = iterm2_payload(b"abc", 12);
        assert!(payload.starts_with("\x1b]1337;File="));
        assert!(payload.ends_with("\x07"));
    }

    #[test]
    fn undecodable_thumbnails_degrade_to_blank() {
        let rows = blocks_for(std::path::Path::new("/nonexistent/x.jpg"));
        assert_eq!(rows.len(), THUMB_ROWS);
        assert!(rows.iter().all(|row| row.len() == THUMB_COLS));
    }
}
