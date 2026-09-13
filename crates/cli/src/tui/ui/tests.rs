//! Rendering tests.
//!
//! Every assertion here goes through `TestBackend`, which renders into an
//! in-memory buffer instead of a terminal. That is the reason the drawing
//! functions are pure: the alternative is asserting on ANSI bytes, which encodes
//! the layout as escape sequences and breaks whenever the styling changes.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;
use crate::tui::app::{
    App, EventLine, GroupRow, JobRow, LogLine, Pending, Permission, StatusSnapshot, Update,
};

/// Renders `app` and returns the buffer as text.
fn render(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| draw(frame, app))
        .expect("draw must not fail");
    let buffer = terminal.backend().buffer().clone();
    // Row by row, because ratatui's buffer is a flat cell list.
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| {
                    buffer
                        .cell((x, y))
                        .map(|cell| cell.symbol().to_owned())
                        .unwrap_or_else(|| " ".to_owned())
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn running() -> StatusSnapshot {
    StatusSnapshot {
        state: "Running".to_owned(),
        live: true,
        serving: true,
        active_config: Some("v002".to_owned()),
        last_failure: None,
    }
}

/// The tab bar names every panel, so nothing is reachable only by memory.
#[test]
fn the_tab_bar_lists_every_panel() {
    let text = render(&App::new(), 100, 24);
    for panel in crate::tui::keys::Panel::ALL {
        assert!(
            text.contains(panel.title()),
            "{} is missing from the tab bar:\n{text}",
            panel.title()
        );
    }
}

/// The distinction the state model exists for: not-fetched must not render as an
/// empty result.
#[test]
fn an_unfetched_panel_says_it_is_waiting() {
    let app = App::new();
    let text = render(&app, 100, 24);
    assert!(text.contains("waiting for the first report"), "{text}");
}

/// A fetched-but-empty panel says something different, so a reader can tell a
/// stopped kernel from a slow fetch.
#[test]
fn an_empty_panel_says_so_distinctly() {
    let mut app = App::new();
    app.panel = crate::tui::keys::Panel::Proxies;
    app.apply(Update::Proxies(Vec::new()));
    let text = render(&app, 100, 24);
    assert!(text.contains("no groups"), "{text}");
    assert!(
        !text.contains("waiting for the first report"),
        "an empty result must not look like an unfetched one:\n{text}"
    );
}

#[test]
fn the_overview_shows_the_state_and_active_config() {
    let mut app = App::new();
    app.apply(Update::Status(running()));
    let text = render(&app, 100, 24);
    assert!(text.contains("Running"), "{text}");
    assert!(text.contains("v002"), "{text}");
    assert!(
        text.contains("connected"),
        "the status bar must say so:\n{text}"
    );
}

/// A recorded failure is the reason someone opens this; it must be visible.
#[test]
fn the_overview_shows_a_recorded_failure() {
    let mut app = App::new();
    app.apply(Update::Status(StatusSnapshot {
        last_failure: Some("readiness timed out".to_owned()),
        ..running()
    }));
    let text = render(&app, 100, 24);
    assert!(text.contains("readiness timed out"), "{text}");
}

#[test]
fn the_proxy_panel_shows_groups_and_their_selection() {
    let mut app = App::new();
    app.panel = crate::tui::keys::Panel::Proxies;
    app.apply(Update::Proxies(vec![GroupRow {
        name: "Proxy".to_owned(),
        kind: "select".to_owned(),
        now: Some("Node A".to_owned()),
        member_count: 3,
    }]));
    let text = render(&app, 100, 24);
    assert!(text.contains("Proxy"), "{text}");
    assert!(text.contains("Node A"), "{text}");
    assert!(text.contains('3'), "the member count must show:\n{text}");
}

#[test]
fn the_log_panel_shows_levels_and_messages() {
    let mut app = App::new();
    app.panel = crate::tui::keys::Panel::Logs;
    app.apply(Update::Log(LogLine {
        level: "error".to_owned(),
        message: "bind failed".to_owned(),
    }));
    let text = render(&app, 100, 24);
    assert!(text.contains("error"), "{text}");
    assert!(text.contains("bind failed"), "{text}");
}

/// Following is a state the reader must be able to see, because it decides whether
/// the view moves under them.
#[test]
fn the_log_panel_says_whether_it_follows() {
    let mut app = App::new();
    app.panel = crate::tui::keys::Panel::Logs;
    app.apply(Update::Log(LogLine {
        level: "info".to_owned(),
        message: "line".to_owned(),
    }));

    app.follow = true;
    assert!(render(&app, 100, 24).contains("following"));
    app.follow = false;
    assert!(render(&app, 100, 24).contains("paused"));
}

#[test]
fn the_event_panel_shows_kind_and_summary() {
    let mut app = App::new();
    app.panel = crate::tui::keys::Panel::Events;
    app.apply(Update::Event(EventLine {
        kind: "config.activated".to_owned(),
        summary: "version=v002".to_owned(),
    }));
    let text = render(&app, 100, 24);
    assert!(text.contains("config.activated"), "{text}");
    assert!(text.contains("version=v002"), "{text}");
}

#[test]
fn the_jobs_panel_shows_rows() {
    let mut app = App::new();
    app.panel = crate::tui::keys::Panel::Jobs;
    app.apply(Update::Jobs(vec![JobRow {
        id: "job_1".to_owned(),
        state: "running".to_owned(),
        target: "default".to_owned(),
    }]));
    let text = render(&app, 100, 24);
    assert!(text.contains("job_1"), "{text}");
    assert!(text.contains("running"), "{text}");
}

/// A disconnected agent must be visible without the reader having to find the
/// error text, and the previous data must remain.
#[test]
fn a_disconnection_is_visible_and_keeps_the_last_data() {
    let mut app = App::new();
    app.apply(Update::Status(running()));
    app.apply(Update::Failed("connection refused".to_owned()));
    let text = render(&app, 100, 24);
    assert!(text.contains("disconnected"), "{text}");
    assert!(text.contains("connection refused"), "{text}");
    assert!(
        text.contains("Running"),
        "the last known state must stay on screen:\n{text}"
    );
}

/// A read-only session must say so, or a missing start button looks like a bug.
#[test]
fn a_read_only_session_is_labelled() {
    let mut app = App::new();
    app.permission = Permission::ReadOnly;
    app.apply(Update::Status(running()));
    assert!(render(&app, 100, 24).contains("read-only"));
}

/// The confirmation must name what it will do, not just ask.
#[test]
fn the_confirmation_names_the_action() {
    let mut app = App::new();
    app.pending = Some(Pending::Stop);
    let text = render(&app, 100, 24);
    assert!(text.contains("stop the kernel"), "{text}");
    assert!(text.contains('y'), "it must say how to confirm:\n{text}");
    assert!(text.contains('n'), "and how to cancel:\n{text}");
}

#[test]
fn the_help_overlay_lists_the_bindings() {
    let mut app = App::new();
    app.help = true;
    let text = render(&app, 100, 30);
    for expected in ["quit", "switch panel", "refresh", "start"] {
        assert!(text.contains(expected), "{expected} missing:\n{text}");
    }
}

/// A very small terminal must not panic: a resize can arrive at any time, and the
/// layout arithmetic must tolerate a window too small for the overlay.
#[test]
fn a_tiny_terminal_does_not_panic() {
    for (width, height) in [(1, 1), (4, 3), (10, 2), (20, 5)] {
        let mut app = App::new();
        app.apply(Update::Status(running()));
        app.help = true;
        let _ = render(&app, width, height);
        app.help = false;
        app.pending = Some(Pending::Stop);
        let _ = render(&app, width, height);
    }
}

/// The scrolling window must keep the selected line visible, or moving the
/// selection would appear to do nothing past the window size.
#[test]
fn the_log_window_follows_the_selection() {
    let mut app = App::new();
    app.panel = crate::tui::keys::Panel::Logs;
    app.follow = false;
    for i in 0..500 {
        app.apply(Update::Log(LogLine {
            level: "info".to_owned(),
            message: format!("line {i}"),
        }));
    }
    app.follow = false;
    app.selected = 490;

    let lines = log_lines(&app);
    let rendered: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        rendered.contains("line 490"),
        "the selection must be in view"
    );
}
