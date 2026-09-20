//! ffkit — a terminal UI that teaches FFmpeg by showing its work.
//!
//! Library root. The binary in `main.rs` is a thin wrapper around this
//! library so that `tests/` (integration snapshot tests for the command
//! builder, see spec section 5) can link against the same code.
//! See DECISIONS.md for why `lib.rs` exists alongside `main.rs`.

pub mod app;
pub mod background;
pub mod config;
pub mod event;
pub mod ffmpeg;
pub mod ops;
pub mod queue;
pub mod ui;
