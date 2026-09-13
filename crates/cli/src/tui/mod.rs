//! The terminal interface.
//!
//! # Shape
//!
//! ```text
//! keys.rs   KeyEvent -> Action      pure, unit-tested
//! app.rs    state + event loop      no rendering, no network
//! ui/       &App -> Frame           pure, tested against TestBackend
//! fetch.rs  the client calls        shares the CLI's client layer
//! ```
//!
//! The layering is not decoration. AGENTS.md forbids business logic in widgets,
//! rendering, and keyboard handlers, and the practical reason is that a handler
//! which resolved an endpoint, called the agent, and drew a screen could only be
//! tested by driving a terminal. Split this way, the key map and the state
//! transitions are covered by ordinary tests, and only the rendering needs a
//! backend — `ratatui`'s `TestBackend`, which needs no terminal at all.
//!
//! # It is a client, not a controller
//!
//! The TUI reaches the agent the same way the CLI does, over the same
//! [`Client`](crate::client::Client), so a remote agent reached with
//! `--socket https://...` works here without any separate code. It never touches
//! the kernel or the database directly: the process management and versioning live
//! behind the API, and a second path would be one the API's tests do not cover.

pub mod app;
pub mod fetch;
pub mod keys;
pub mod ui;

pub use app::{App, Request, Update};
pub use keys::{Action, Panel};

/// Runs the TUI against an endpoint until the user quits.
///
/// # Errors
///
/// Returns an error when the terminal cannot be set up, restored, or read. A
/// failure to reach the agent is not an error: it is shown in the status bar and
/// retried, because a dashboard that exits when the agent restarts is useless
/// exactly when it is wanted.
pub async fn run(client: crate::client::Client) -> Result<(), String> {
    use crossterm::event::{EventStream, KeyEventKind};
    use futures_util::StreamExt as _;

    // The terminal is restored by `TerminalGuard`'s `Drop`, so an early return or
    // a panic cannot leave the user's shell in raw mode.
    let guard = TerminalGuard::enter()?;
    let terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))
            .map_err(|e| format!("cannot open the terminal: {e}"))?;

    let (updates_tx, updates_rx) = tokio::sync::mpsc::channel(256);
    let (keys_tx, keys_rx) = tokio::sync::mpsc::channel(64);
    // Writes travel separately from refreshes: a refresh wakes every reader,
    // while a write must reach exactly one executor.
    let (actions_tx, actions_rx) = tokio::sync::mpsc::channel(16);
    let (refresh, refresh_rx) = fetch::Refresh::new();
    let (redraw_tx, redraw_rx) = tokio::sync::mpsc::channel(64);

    // Read the keyboard on its own task: `crossterm`'s event stream is the one
    // thing here that cannot be polled from a `select!` without holding a borrow
    // of the terminal.
    let keys = tokio::spawn(async move {
        let mut events = EventStream::new();
        while let Some(Ok(event)) = events.next().await {
            match event {
                crossterm::event::Event::Key(key) => {
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                        && let Some(action) = keys::action_for(key)
                        && keys_tx.send(action).await.is_err()
                    {
                        break;
                    }
                }
                // A resize must produce a redraw; the loop redraws on any wake.
                crossterm::event::Event::Resize(_, _) => {
                    let _ = redraw_tx.send(()).await;
                }
                _ => {}
            }
        }
    });

    // The data sources run as their own tasks and report through one channel, so
    // the main loop does not grow with each source.
    let readers = fetch::spawn_all(client, updates_tx, refresh_rx, actions_rx);

    let result = app::run(
        App::new(),
        updates_rx,
        keys_rx,
        refresh,
        actions_tx,
        redraw_rx,
        terminal,
    )
    .await;

    keys.abort();
    for reader in readers {
        reader.abort();
    }
    guard.restore();

    match result {
        Ok(()) => Ok(()),
        Err(e) => Err(format!("the terminal failed: {e}")),
    }
}

/// Puts the terminal into raw mode and restores it on drop.
///
/// # Why a guard rather than a call at the end
///
/// Raw mode makes the user's shell unusable until it is undone, and every early
/// return and panic is a chance to skip an explicit restore. Tying it to `Drop`
/// means the only way to leave raw mode on is to abort the process, which is
/// exactly the case where nothing can help.
struct TerminalGuard {
    restored: std::cell::Cell<bool>,
}

impl TerminalGuard {
    fn enter() -> Result<Self, String> {
        // The alternate screen is entered as well as raw mode: without it the
        // interface would scroll the user's scrollback away, and leaving the TUI
        // would lose whatever was on the terminal before.
        crossterm::terminal::enable_raw_mode()
            .map_err(|e| format!("cannot enter raw mode: {e}"))?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(
            stdout,
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::EnableMouseCapture,
        )
        .map_err(|e| format!("cannot switch to the alternate screen: {e}"))?;
        Ok(Self {
            restored: std::cell::Cell::new(false),
        })
    }

    fn restore(&self) {
        if !self.restored.replace(true) {
            let mut stdout = std::io::stdout();
            let _ = crossterm::execute!(
                stdout,
                crossterm::event::DisableMouseCapture,
                crossterm::terminal::LeaveAlternateScreen,
            );
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.restore();
    }
}
