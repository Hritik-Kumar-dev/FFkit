//! ffkit entry point: CLI parsing, logging, terminal setup/teardown, main loop.

use std::io::Stdout;
use std::panic;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event as ct_event;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use directories::ProjectDirs;
use ffkit::app::App;
use ffkit::background::{spawn_capability_detection, BackgroundChannel};
use ffkit::config::settings::Settings;
use ffkit::event::{poll_event, AppEvent};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tracing_subscriber::EnvFilter;

const TICK_RATE: Duration = Duration::from_millis(250);

/// ffkit — a terminal UI for FFmpeg that always shows the command it builds.
#[derive(Parser, Debug)]
#[command(name = "ffkit", version, about)]
struct Cli {
    /// Jump straight to this operation (id, e.g. `compress`). M3+.
    #[arg(long, value_name = "OPERATION")]
    operation: Option<String>,

    /// Input file to open with the operation. M3+.
    #[arg(long, value_name = "PATH")]
    input: Option<std::path::PathBuf>,
}

/// Terminal type used throughout the app.
type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Multi-thread runtime: the main thread alternates blocking event polls
/// with rendering, while ffmpeg/ffprobe subprocess tasks run on worker
/// threads and report back over the background channel. No subprocess I/O
/// ever happens on the render path.
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging().context("initializing file logging")?;
    install_panic_hook();

    // M2: direct-launch flags are parsed but not yet honored; record the
    // intent in the log so the behavior is observable, not silent.
    if cli.operation.is_some() || cli.input.is_some() {
        tracing::warn!(
            operation = ?cli.operation,
            input = ?cli.input,
            "direct-launch flags are parsed but not yet implemented (M3); starting at the operation picker"
        );
    }

    let settings = Settings::load();
    let channel = BackgroundChannel::new();
    spawn_capability_detection(channel.tx.clone(), settings.clone());

    let mut terminal = init_terminal().context("initializing terminal")?;
    let mut app = App::new(channel.tx, channel.rx, settings);
    // Terminal image support is pure environment sniffing (instant) and
    // belongs with the ffmpeg capability check at startup.
    app.image_backend = ffkit::ui::images::detect();
    let result = run_app(&mut terminal, app);
    restore_terminal()?;
    result
}

/// The main loop. Everything else is a shell around this.
fn run_app(terminal: &mut Tui, mut app: App) -> Result<()> {
    use std::io::Write as _;
    loop {
        terminal
            .draw(|frame| app.render(frame))
            .context("rendering TUI frame")?;
        // Inline-image payloads staged during render print after the draw
        // (Kitty/iTerm2 filmstrip overlay); nothing is staged on other
        // backends, so this is a no-op almost everywhere.
        for payload in app.take_image_payloads() {
            write!(std::io::stdout(), "{payload}").context("printing image payload")?;
        }
        let _ = std::io::stdout().flush();

        // Drain background work (capability reports, probe results) before
        // handling input so the UI reflects finished tasks immediately.
        app.poll_background();

        match poll_event(TICK_RATE).context("polling terminal events")? {
            Some(AppEvent::Tick) => app.on_tick(),
            Some(AppEvent::Key(key)) => app.on_key(key),
            Some(AppEvent::Resize) => app.on_resize(),
            None => {}
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// Enter the alternate screen with raw mode. Must be paired with
/// [`restore_terminal`], including on panic (see [`install_panic_hook`]).
fn init_terminal() -> Result<Tui> {
    enable_raw_mode().context("enabling raw mode")?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;
    Terminal::new(CrosstermBackend::new(stdout)).context("creating terminal")
}

/// Leave the alternate screen and disable raw mode. Best-effort ordering:
/// report the first error but always attempt both steps so a failing step
/// cannot leave the user's terminal in a broken state.
fn restore_terminal() -> Result<()> {
    let mut stdout = std::io::stdout();
    let alt_result = execute!(stdout, LeaveAlternateScreen);
    let raw_result = disable_raw_mode();
    alt_result.context("leaving alternate screen")?;
    raw_result.context("disabling raw mode")?;
    // Drain any queued crossterm input events (e.g. pending resize notifications).
    let _ = ct_event::poll(Duration::from_millis(0));
    Ok(())
}

/// A panicking TUI that leaves the terminal in raw mode / alternate screen
/// is unforgivable, so restore the terminal before delegating to the
/// default panic hook.
fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        // Best effort: the process is panicking, there is nothing useful to
        // do with an error here.
        let _ = restore_terminal();
        original(info);
    }));
}

/// Log to a file, never to stdout — stdout is the TUI.
fn init_logging() -> Result<()> {
    let log_path = ProjectDirs::from("", "", "ffkit")
        .map(|dirs| dirs.data_local_dir().join("ffkit.log"))
        .context("locating local data dir for log file")?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating log dir {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .try_init()
        .map_err(|e| anyhow::anyhow!("installing tracing subscriber: {e}"))?;
    tracing::info!(log_file = %log_path.display(), "ffkit starting");
    Ok(())
}
