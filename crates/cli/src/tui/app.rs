//! The TUI's state and its event loop.
//!
//! # What this module owns
//!
//! State, the choice of when to refresh, and the execution of an [`Action`]. It
//! does **not** render: [`ui`](super::ui) reads this state and draws it. Keeping
//! the two apart is what lets the rendering be tested against a `TestBackend` and
//! the state transitions be tested without any terminal at all.
//!
//! # Refresh: polling *and* the event stream
//!
//! The two are not alternatives. `EventPublisher`'s contract says events notify
//! and are not a ledger — a subscriber that misses one is expected to re-read
//! state — so the TUI polls as the source of truth and treats an event as a
//! *hint* to re-read sooner. Polling alone would be slow to react; events alone
//! would show stale data after any missed event, and the channel is bounded, so
//! misses are normal rather than exceptional.
//!
//! # Absence is not emptiness
//!
//! Every fetched value is an `Option`. `None` means "not fetched", `Some(vec![])`
//! means "fetched, and there was nothing" — and the two render differently. A
//! failed fetch leaves the previous value in place rather than clearing it, so an
//! agent restarting does not blank the screen an operator is reading.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::sync::mpsc;

use super::keys::{Action, Panel};

/// How many log lines to keep.
///
/// Bounded because a log stream is unbounded and a long session must not grow
/// without limit. Old lines are dropped rather than refusing new ones: the recent
/// end is what a person watching is reading.
pub const LOG_CAPACITY: usize = 2_000;

/// How many events to keep. Smaller than the log buffer: events are occasional.
pub const EVENT_CAPACITY: usize = 500;

/// How often the kernel's status is polled.
pub const STATUS_INTERVAL: Duration = Duration::from_secs(2);

/// How often the proxy list is polled.
///
/// Slower than status: groups change when configuration changes, not continuously,
/// and the response is much larger.
pub const PROXIES_INTERVAL: Duration = Duration::from_secs(5);

/// How often jobs are polled.
pub const JOBS_INTERVAL: Duration = Duration::from_secs(3);

/// A line in the log panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Severity, as the agent rendered it.
    pub level: String,
    /// The text.
    pub message: String,
}

/// A line in the event panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLine {
    /// The event kind, such as `config.activated`.
    pub kind: String,
    /// A one-line summary.
    pub summary: String,
}

/// A kernel status snapshot, as the TUI needs it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusSnapshot {
    /// Lifecycle state.
    pub state: String,
    /// Whether the process exists.
    pub live: bool,
    /// Whether it is serving.
    pub serving: bool,
    /// The active configuration version.
    pub active_config: Option<String>,
    /// The most recent recorded failure.
    pub last_failure: Option<String>,
}

/// A group row, as the TUI needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRow {
    /// Group name.
    pub name: String,
    /// Group type.
    pub kind: String,
    /// The selected member.
    pub now: Option<String>,
    /// How many members it has.
    pub member_count: usize,
}

/// A job row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    /// Identifier.
    pub id: String,
    /// State label.
    pub state: String,
    /// What it operates on.
    pub target: String,
}

/// Data arriving from the background tasks.
///
/// One channel for every source, because the main loop should react to "something
/// arrived" without knowing which reader produced it. Separate channels would make
/// the loop's `select!` grow with each new source.
#[derive(Debug, Clone)]
pub enum Update {
    /// A status snapshot.
    Status(StatusSnapshot),
    /// The proxy groups.
    Proxies(Vec<GroupRow>),
    /// Recent jobs.
    Jobs(Vec<JobRow>),
    /// One log line.
    Log(LogLine),
    /// One event.
    Event(EventLine),
    /// A fetch or stream failed, with a description.
    Failed(String),
    /// A source recovered.
    Recovered,
}

/// A write waiting for confirmation.
///
/// The action is not performed until the user confirms, because both of these
/// interrupt service and a stray keypress should not do that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// Start the kernel.
    Start,
    /// Stop the kernel.
    Stop,
}

/// Whether the connected token may change state.
///
/// Detected rather than assumed: a remote connection with a read-only token must
/// not offer a start button that always fails. The check is one `system` call at
/// startup, and the answer decides whether the write bindings do anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Unknown until the first probe completes.
    Unknown,
    /// Reads and writes.
    Admin,
    /// Reads only.
    ReadOnly,
}

/// The TUI's state.
#[derive(Debug)]
pub struct App {
    /// Which panel is showing.
    pub panel: Panel,
    /// The selected row in the current panel.
    pub selected: usize,
    /// Whether the log panel follows the newest line.
    pub follow: bool,
    /// Whether the help overlay is showing.
    pub help: bool,
    /// A write waiting for confirmation.
    pub pending: Option<Pending>,
    /// Whether the connection is currently working.
    pub connected: bool,
    /// The last failure, shown until a success clears it.
    pub last_error: Option<String>,
    /// What the token permits.
    pub permission: Permission,
    /// The most recent status, if one was fetched.
    pub status: Option<StatusSnapshot>,
    /// The groups, if they were fetched.
    pub groups: Option<Vec<GroupRow>>,
    /// The jobs, if they were fetched.
    pub jobs: Option<Vec<JobRow>>,
    /// Recent log lines.
    pub logs: VecDeque<LogLine>,
    /// Recent events.
    pub events: VecDeque<EventLine>,
    /// Set when the user asked to quit.
    pub should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// A fresh state, before anything is fetched.
    #[must_use]
    pub fn new() -> Self {
        Self {
            panel: Panel::Overview,
            selected: 0,
            follow: true,
            help: false,
            pending: None,
            connected: false,
            last_error: None,
            permission: Permission::Unknown,
            status: None,
            groups: None,
            jobs: None,
            logs: VecDeque::with_capacity(LOG_CAPACITY),
            events: VecDeque::with_capacity(EVENT_CAPACITY),
            should_quit: false,
        }
    }

    /// How many rows the current panel holds.
    #[must_use]
    pub fn row_count(&self) -> usize {
        match self.panel {
            Panel::Overview => usize::from(self.status.is_some()),
            Panel::Proxies => self.groups.as_ref().map_or(0, Vec::len),
            Panel::Logs => self.logs.len(),
            Panel::Events => self.events.len(),
            Panel::Jobs => self.jobs.as_ref().map_or(0, Vec::len),
        }
    }

    /// Applies an update from a background source.
    pub fn apply(&mut self, update: Update) {
        match update {
            Update::Status(snapshot) => {
                self.status = Some(snapshot);
                self.mark_connected();
            }
            Update::Proxies(groups) => {
                self.groups = Some(groups);
                self.mark_connected();
            }
            Update::Jobs(jobs) => {
                self.jobs = Some(jobs);
                self.mark_connected();
            }
            Update::Log(line) => {
                push_bounded(&mut self.logs, line, LOG_CAPACITY);
                // Following means the view tracks the newest line, so the
                // selection moves with it. Not following leaves the selection
                // where the reader put it.
                if self.follow && self.panel == Panel::Logs {
                    self.selected = self.logs.len().saturating_sub(1);
                }
            }
            Update::Event(line) => {
                push_bounded(&mut self.events, line, EVENT_CAPACITY);
                if self.follow && self.panel == Panel::Events {
                    self.selected = self.events.len().saturating_sub(1);
                }
            }
            Update::Failed(reason) => {
                // The connection is marked down but nothing is cleared: an agent
                // restarting must not blank a screen someone is reading.
                self.connected = false;
                self.last_error = Some(reason);
            }
            Update::Recovered => {
                self.mark_connected();
            }
        }
    }

    /// Marks the connection healthy and clears a stale error.
    fn mark_connected(&mut self) {
        self.connected = true;
        self.last_error = None;
    }

    /// Applies a key action.
    ///
    /// Returns a request for the caller to execute when the action needs I/O. The
    /// loop executes it; this function never touches the network, which is what
    /// makes the transitions testable.
    pub fn dispatch(&mut self, action: Action) -> Option<Request> {
        // A pending confirmation owns the keyboard: `y` and `n` mean confirm and
        // cancel, and nothing else should act until it is resolved. Without this a
        // user could move the selection while a stop is pending, and confirm
        // something they can no longer see.
        if let Some(pending) = self.pending {
            return match action {
                Action::Confirm => {
                    self.pending = None;
                    Some(match pending {
                        Pending::Start => Request::Start,
                        Pending::Stop => Request::Stop,
                    })
                }
                Action::Cancel | Action::Quit => {
                    self.pending = None;
                    None
                }
                // Everything else is ignored while a confirmation is open.
                _ => None,
            };
        }

        if self.help {
            // Help is modal in the same way: any dismissal key closes it.
            if matches!(
                action,
                Action::ToggleHelp | Action::Cancel | Action::Confirm | Action::Quit
            ) {
                self.help = false;
            }
            return None;
        }

        match action {
            Action::Quit => self.should_quit = true,
            Action::NextPanel => self.switch_panel(self.panel.next()),
            Action::PreviousPanel => self.switch_panel(self.panel.previous()),
            Action::Next => self.move_selection(1),
            Action::Previous => self.move_selection(-1),
            Action::PageDown => self.move_selection(10),
            Action::PageUp => self.move_selection(-10),
            Action::Top => self.selected = 0,
            Action::Bottom => self.selected = self.row_count().saturating_sub(1),
            Action::Refresh => return Some(Request::Refresh),
            Action::ToggleFollow => self.follow = !self.follow,
            Action::ToggleHelp => self.help = true,
            Action::RequestStart => {
                if self.permission == Permission::ReadOnly {
                    // Reported rather than sent: the request would be refused, and
                    // saying so here points at the token rather than at the agent.
                    self.last_error = Some(
                        "the token is read-only, so it cannot start or stop the kernel".to_owned(),
                    );
                    return None;
                }
                self.pending = Some(Pending::Start);
            }
            Action::RequestStop => {
                if self.permission == Permission::ReadOnly {
                    self.last_error = Some(
                        "the token is read-only, so it cannot start or stop the kernel".to_owned(),
                    );
                    return None;
                }
                self.pending = Some(Pending::Stop);
            }
            // Handled above, because they only mean something while something is
            // pending or the help overlay is open.
            Action::Confirm | Action::Cancel => {}
        }
        None
    }

    /// Switches panels, resetting the selection.
    ///
    /// Reset because a row index means something different per panel: keeping it
    /// would land on an arbitrary row of the new one.
    fn switch_panel(&mut self, panel: Panel) {
        self.panel = panel;
        self.selected = 0;
    }

    /// Moves the selection, clamped to the panel's rows.
    ///
    /// Clamped rather than wrapping: a list that jumps from the last row to the
    /// first makes "hold down to skim" overshoot.
    fn move_selection(&mut self, delta: i64) {
        let count = self.row_count();
        if count == 0 {
            self.selected = 0;
            return;
        }
        let current = self.selected as i64;
        self.selected = (current + delta).clamp(0, count as i64 - 1) as usize;
    }
}

/// Pushes onto a bounded queue, dropping the oldest.
fn push_bounded<T>(queue: &mut VecDeque<T>, item: T, capacity: usize) {
    if queue.len() >= capacity {
        queue.pop_front();
    }
    queue.push_back(item);
}

/// Something the loop must do, because it needs I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Re-fetch everything now.
    Refresh,
    /// Start the kernel.
    Start,
    /// Stop the kernel.
    Stop,
}

/// Runs the main loop until the user quits.
///
/// # The three sources
///
/// `select!` waits on the keyboard, the update channel, and the refresh timer.
/// Each background reader has its own task and reports through the one channel, so
/// adding a source does not lengthen this loop.
///
/// # Errors
///
/// Returns an error when the terminal cannot be read or written. A failure to
/// fetch data is not an error: it is reported in the status bar and retried on the
/// next tick, because a dashboard that exits when the agent restarts would be
/// useless exactly when it is needed.
pub async fn run(
    mut app: App,
    mut updates: mpsc::Receiver<Update>,
    mut keys: mpsc::Receiver<Action>,
    refresh: super::fetch::Refresh,
    actions: mpsc::Sender<Request>,
    mut terminal_events: mpsc::Receiver<()>,
    mut terminal: ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
) -> Result<(), std::io::Error> {
    let mut ticker = tokio::time::interval(STATUS_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Drawn before the first wait, so the interface appears immediately rather
    // than after the first poll returns. Without this the user sees an empty
    // terminal for up to a poll interval and reasonably concludes it is broken.
    draw(&mut terminal, &app)?;

    loop {
        tokio::select! {
            Some(update) = updates.recv() => {
                app.apply(update);
            }
            Some(action) = keys.recv() => {
                if let Some(request) = app.dispatch(action) {
                    match request {
                        // A refresh wakes every reader; a write reaches exactly one
                        // executor. Different jobs, different channels.
                        Request::Refresh => refresh.request(),
                        other => {
                            // A failed send means the executor is gone, which only
                            // happens during shutdown.
                            let _ = actions.send(other).await;
                        }
                    }
                }
            }
            _ = ticker.tick() => {
                refresh.request();
            }
            Some(()) = terminal_events.recv() => {
                // A resize. Nothing to do beyond falling through to the redraw
                // below, which is what makes the new size take effect.
            }
            else => break,
        }

        // Redrawn after every wake, which is what the earlier comment in this
        // function claimed while no such call existed: the interface rendered
        // nothing at all and only a pty run revealed it.
        draw(&mut terminal, &app)?;

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// Draws one frame.
fn draw(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &App,
) -> Result<(), std::io::Error> {
    terminal
        .draw(|frame| super::ui::draw(frame, app))
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(message: &str) -> Update {
        Update::Log(LogLine {
            level: "info".to_owned(),
            message: message.to_owned(),
        })
    }

    #[test]
    fn a_fresh_app_has_fetched_nothing() {
        let app = App::new();
        assert!(app.status.is_none());
        assert!(app.groups.is_none());
        assert!(app.jobs.is_none());
        assert!(!app.connected);
        assert_eq!(app.panel, Panel::Overview);
    }

    /// The distinction the whole state model exists for: not-fetched and
    /// fetched-but-empty render differently.
    #[test]
    fn not_fetched_and_empty_are_different() {
        let mut app = App::new();
        assert!(app.groups.is_none(), "nothing fetched yet");

        app.apply(Update::Proxies(Vec::new()));
        assert_eq!(app.groups, Some(Vec::new()), "fetched, and empty");
    }

    /// A failure must not clear what is already displayed.
    #[test]
    fn a_failure_keeps_the_last_good_data() {
        let mut app = App::new();
        app.apply(Update::Status(StatusSnapshot {
            state: "Running".to_owned(),
            live: true,
            serving: true,
            active_config: Some("v002".to_owned()),
            last_failure: None,
        }));
        assert!(app.connected);

        app.apply(Update::Failed("connection refused".to_owned()));
        assert!(!app.connected, "the connection is reported down");
        assert_eq!(
            app.status.as_ref().map(|s| s.state.as_str()),
            Some("Running"),
            "the last known state must survive, or a restarting agent blanks the screen"
        );
        assert!(app.last_error.is_some());
    }

    /// A successful fetch clears a previous error, or the status bar would lie
    /// forever after one blip.
    #[test]
    fn a_success_clears_the_error() {
        let mut app = App::new();
        app.apply(Update::Failed("gone".to_owned()));
        assert!(app.last_error.is_some());

        app.apply(Update::Proxies(Vec::new()));
        assert!(app.last_error.is_none());
        assert!(app.connected);
    }

    /// The log buffer is bounded, and drops the oldest rather than refusing new
    /// lines.
    #[test]
    fn the_log_buffer_is_bounded_and_keeps_the_newest() {
        let mut app = App::new();
        for i in 0..(LOG_CAPACITY + 50) {
            app.apply(log(&format!("line {i}")));
        }
        assert_eq!(app.logs.len(), LOG_CAPACITY, "the buffer must not grow");
        let newest = app.logs.back().expect("a line").message.clone();
        assert_eq!(newest, format!("line {}", LOG_CAPACITY + 49));
        let oldest = app.logs.front().expect("a line").message.clone();
        assert_eq!(
            oldest,
            format!("line {}", 50),
            "the oldest must be the one dropped"
        );
    }

    /// Following moves the selection to the newest line; not following leaves it
    /// where the reader put it.
    #[test]
    fn follow_tracks_the_newest_line_and_stops_when_disabled() {
        let mut app = App::new();
        app.panel = Panel::Logs;
        app.follow = true;
        app.apply(log("first"));
        app.apply(log("second"));
        assert_eq!(app.selected, 1);

        app.follow = false;
        app.selected = 0;
        app.apply(log("third"));
        assert_eq!(app.selected, 0, "a reader who scrolled up must stay there");
    }

    /// A row index means something different per panel, so switching resets it.
    #[test]
    fn switching_panels_resets_the_selection() {
        let mut app = App::new();
        app.panel = Panel::Proxies;
        app.selected = 3;
        app.dispatch(Action::NextPanel);
        assert_eq!(app.selected, 0);
    }

    /// Movement is clamped: wrapping would make "hold down to skim" overshoot.
    #[test]
    fn movement_is_clamped_to_the_rows() {
        let mut app = App::new();
        app.apply(Update::Proxies(vec![
            GroupRow {
                name: "g1".to_owned(),
                kind: "select".to_owned(),
                now: None,
                member_count: 1,
            },
            GroupRow {
                name: "g2".to_owned(),
                kind: "select".to_owned(),
                now: None,
                member_count: 1,
            },
        ]));
        app.panel = Panel::Proxies;

        app.dispatch(Action::Previous);
        assert_eq!(app.selected, 0, "cannot go above the first row");
        app.dispatch(Action::Next);
        app.dispatch(Action::Next);
        app.dispatch(Action::Next);
        assert_eq!(app.selected, 1, "cannot go past the last row");
    }

    /// Moving in an empty panel must not panic, which is the state most panels
    /// start in.
    #[test]
    fn movement_in_an_empty_panel_does_nothing() {
        let mut app = App::new();
        assert_eq!(app.row_count(), 0);
        app.dispatch(Action::Next);
        app.dispatch(Action::Bottom);
        app.dispatch(Action::PageDown);
        assert_eq!(app.selected, 0);
    }

    /// A write needs confirmation, and only confirmation performs it.
    #[test]
    fn a_stop_request_needs_confirmation() {
        let mut app = App::new();
        app.permission = Permission::Admin;

        assert_eq!(app.dispatch(Action::RequestStop), None);
        assert_eq!(app.pending, Some(Pending::Stop), "the request is held");

        assert_eq!(app.dispatch(Action::Confirm), Some(Request::Stop));
        assert_eq!(app.pending, None, "the confirmation is consumed");
    }

    /// Cancelling discards the request rather than performing it.
    #[test]
    fn cancelling_discards_the_request() {
        let mut app = App::new();
        app.permission = Permission::Admin;
        app.dispatch(Action::RequestStart);
        assert_eq!(app.dispatch(Action::Cancel), None);
        assert_eq!(app.pending, None);
        assert!(!app.should_quit);
    }

    /// While something is pending, the keyboard belongs to the confirmation: a
    /// user must not be able to move the selection and then confirm something they
    /// can no longer see.
    #[test]
    fn a_pending_confirmation_ignores_other_keys() {
        let mut app = App::new();
        app.permission = Permission::Admin;
        app.dispatch(Action::RequestStop);

        assert_eq!(app.dispatch(Action::NextPanel), None);
        assert_eq!(app.panel, Panel::Overview, "the panel must not change");
        assert_eq!(app.pending, Some(Pending::Stop), "it must still be held");
    }

    /// A read-only token must not offer a write that would be refused: saying so
    /// points at the token instead of at the agent.
    #[test]
    fn a_read_only_token_refuses_writes_locally() {
        let mut app = App::new();
        app.permission = Permission::ReadOnly;

        assert_eq!(app.dispatch(Action::RequestStart), None);
        assert_eq!(app.pending, None, "nothing may be sent");
        let error = app.last_error.clone().unwrap_or_default();
        assert!(error.contains("read-only"), "{error}");
    }

    /// The help overlay is modal and any dismissal key closes it.
    #[test]
    fn help_is_modal() {
        let mut app = App::new();
        app.dispatch(Action::ToggleHelp);
        assert!(app.help);

        // A movement key must not act while help is open.
        app.dispatch(Action::NextPanel);
        assert_eq!(app.panel, Panel::Overview);

        app.dispatch(Action::Cancel);
        assert!(!app.help);
        app.dispatch(Action::NextPanel);
        assert_eq!(app.panel, Panel::Proxies, "normal bindings resume");
    }

    #[test]
    fn quit_sets_the_flag() {
        let mut app = App::new();
        app.dispatch(Action::Quit);
        assert!(app.should_quit);
    }

    /// A refresh asks the loop for I/O; it does not perform it here.
    #[test]
    fn refresh_is_a_request_not_an_action() {
        let mut app = App::new();
        assert_eq!(app.dispatch(Action::Refresh), Some(Request::Refresh));
    }

    #[test]
    fn following_can_be_toggled() {
        let mut app = App::new();
        assert!(app.follow, "following is the useful default");
        app.dispatch(Action::ToggleFollow);
        assert!(!app.follow);
        app.dispatch(Action::ToggleFollow);
        assert!(app.follow);
    }

    /// Jumping to the bottom must land on the last row, not one past it.
    #[test]
    fn bottom_lands_on_the_last_row() {
        let mut app = App::new();
        for i in 0..5 {
            app.apply(log(&format!("line {i}")));
        }
        app.panel = Panel::Logs;
        app.dispatch(Action::Bottom);
        assert_eq!(app.selected, 4);
    }
}
