//! Event loop input handling: terminal events become [`AppEvent`]s.
//!
//! M1 is synchronous: [`poll_event`] blocks up to the tick interval using
//! `crossterm::event::poll`, so no blocking I/O ever happens on the render
//! path — rendering and event polling strictly alternate. Later milestones
//! (M4) will move ffmpeg child-process I/O onto tokio tasks that send updates
//! back through `tokio::sync::mpsc`; the render thread stays non-blocking.

use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEvent};

/// Everything the main loop can react to.
#[derive(Debug, Clone, Copy)]
pub enum AppEvent {
    /// A key press (already filtered: press events and, for now, repeats).
    Key(KeyEvent),
    /// Tick timer fired — animations, debounced refreshes, ETA smoothing.
    Tick,
    /// Terminal was resized; the next draw uses the new size.
    Resize,
}

/// Wait up to `timeout` for an event. Returns `Ok(None)` on timeout so the
/// caller can run tick-driven updates even when the user is idle.
pub fn poll_event(timeout: Duration) -> Result<Option<AppEvent>> {
    if !event::poll(timeout).context("polling for terminal events")? {
        return Ok(None);
    }
    match event::read().context("reading terminal event")? {
        Event::Key(key) if key.kind != event::KeyEventKind::Release => Ok(Some(AppEvent::Key(key))),
        Event::Resize(_, _) => Ok(Some(AppEvent::Resize)),
        Event::FocusGained | Event::FocusLost | Event::Key(_) => Ok(None),
        Event::Mouse(_) => Ok(None),
        Event::Paste(_) => Ok(None),
    }
}
