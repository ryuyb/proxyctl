//! Rendering.
//!
//! Every function here takes `&App` and a `Frame` and draws. None of them decides
//! anything: the state is already resolved by [`app`](super::app), and AGENTS.md
//! forbids business logic in widgets. What that buys is testability — a draw
//! function tested against `TestBackend` renders into a buffer this crate can read,
//! so the panels are covered without a terminal.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, Tabs, Wrap};

use super::app::{App, Permission};
use super::keys::Panel;

/// Draws the whole interface.
pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // A terminal can be resized to almost nothing at any moment — by a tiling
    // window manager mid-drag, most often — and the layout must not index outside
    // the buffer when it is. Two lines are the minimum for a tab bar and a status
    // line; below that only the panel that fits is drawn, because drawing all
    // three would panic.
    if area.height < 2 {
        draw_panel(frame, app, area);
        return;
    }

    // The status bar is always last and always one line, so it cannot be pushed
    // off by a panel's content — it carries the connection state, which is the
    // thing a reader must be able to see at any moment.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    draw_tabs(frame, app, chunks[0]);
    draw_panel(frame, app, chunks[1]);
    draw_status(frame, app, chunks[2]);

    if app.help {
        draw_help(frame, area);
    } else if let Some(pending) = app.pending {
        draw_confirmation(frame, app, area, pending);
    }
}

/// The tab bar.
fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Panel::ALL
        .iter()
        .map(|panel| Line::from(panel.title()))
        .collect();
    let selected = Panel::ALL.iter().position(|p| *p == app.panel).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .select(selected)
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Black)
                .bg(Color::Cyan),
        )
        .divider("|");
    frame.render_widget(tabs, area);
}

/// The body of the current panel.
fn draw_panel(frame: &mut Frame, app: &App, area: Rect) {
    match app.panel {
        Panel::Overview => draw_overview(frame, app, area),
        Panel::Proxies => draw_proxies(frame, app, area),
        Panel::Logs => draw_logs(frame, app, area),
        Panel::Events => draw_events(frame, app, area),
        Panel::Jobs => draw_jobs(frame, app, area),
    }
}

/// The overview panel.
fn draw_overview(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("kernel");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // "not fetched" and "fetched, empty" are different states, and the panel says
    // which rather than drawing an empty box the reader has to interpret.
    let Some(status) = &app.status else {
        frame.render_widget(
            Paragraph::new("waiting for the first report…")
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    };

    let mut lines = vec![
        Line::from(vec![
            Span::raw("state      "),
            Span::styled(&status.state, state_style(&status.state)),
        ]),
        Line::from(format!("live       {}", status.live)),
        Line::from(format!("serving    {}", status.serving)),
        Line::from(format!(
            "config     {}",
            status.active_config.as_deref().unwrap_or("-")
        )),
    ];
    if let Some(failure) = &status.last_failure {
        lines.push(Line::from(Span::styled(
            format!("failure    {failure}"),
            Style::default().fg(Color::Red),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "s/S start or stop the kernel, ? for help",
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// The proxy groups panel.
fn draw_proxies(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("proxy groups");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(groups) = &app.groups else {
        frame.render_widget(
            Paragraph::new("waiting for the first report…")
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    };
    if groups.is_empty() {
        // The kernel is most likely not running. Saying so beats an empty table.
        frame.render_widget(
            Paragraph::new("no groups (is the kernel running?)")
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let rows: Vec<Row> = groups
        .iter()
        .map(|group| {
            Row::new(vec![
                group.name.clone(),
                group.kind.clone(),
                group.now.clone().unwrap_or_else(|| "-".to_owned()),
                group.member_count.to_string(),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(40),
            Constraint::Percentage(15),
            Constraint::Percentage(35),
            Constraint::Percentage(10),
        ],
    )
    .header(
        Row::new(vec!["group", "type", "selected", "members"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol("> ");
    frame.render_stateful_widget(table, inner, &mut table_state(app));
}

/// The log panel.
fn draw_logs(frame: &mut Frame, app: &App, area: Rect) {
    let title = if app.follow {
        "logs (following)"
    } else {
        "logs (paused)"
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.logs.is_empty() {
        frame.render_widget(
            Paragraph::new("no log lines yet").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }
    frame.render_widget(Paragraph::new(log_lines(app)), inner);
}

/// The lines visible in the log panel, scrolled to the selection.
fn log_lines(app: &App) -> Vec<Line<'static>> {
    let lines: Vec<Line> = app
        .logs
        .iter()
        .map(|entry| {
            Line::from(vec![
                Span::styled(format!("{:<7} ", entry.level), level_style(&entry.level)),
                Span::raw(entry.message.clone()),
            ])
        })
        .collect();
    scroll_to_selection(lines, app.selected, app.follow)
}

/// The events panel.
fn draw_events(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("events");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.events.is_empty() {
        frame.render_widget(
            Paragraph::new("no events yet").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }
    let lines: Vec<Line> = app
        .events
        .iter()
        .map(|event| {
            Line::from(vec![
                Span::styled(
                    format!("{:<20} ", event.kind),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(event.summary.clone()),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(scroll_to_selection(lines, app.selected, app.follow)),
        inner,
    );
}

/// The jobs panel.
fn draw_jobs(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title("jobs");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(jobs) = &app.jobs else {
        frame.render_widget(
            Paragraph::new("waiting for the first report…")
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    };
    if jobs.is_empty() {
        frame.render_widget(
            Paragraph::new("no jobs").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }
    let rows: Vec<Row> = jobs
        .iter()
        .map(|job| Row::new(vec![job.id.clone(), job.state.clone(), job.target.clone()]))
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(35),
            Constraint::Percentage(20),
            Constraint::Percentage(45),
        ],
    )
    .header(
        Row::new(vec!["id", "state", "target"])
            .style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol("> ");
    frame.render_stateful_widget(table, inner, &mut table_state(app));
}

/// Builds the table state from the app's selection.
///
/// `ratatui` keeps selection in its own type, so it is derived here rather than
/// stored twice — two copies of "which row" would be one too many.
fn table_state(app: &App) -> ratatui::widgets::TableState {
    let mut state = ratatui::widgets::TableState::default();
    let count = app.row_count();
    if count > 0 {
        state.select(Some(app.selected.min(count - 1)));
    }
    state
}

/// Scrolls a list so the selection is visible.
///
/// A bounded window rather than the whole list: `Paragraph` with a large vector
/// renders every line and lets the terminal clip, which is wasteful and makes the
/// visible region depend on terminal size in a way this cannot test.
fn scroll_to_selection(
    lines: Vec<Line<'static>>,
    selected: usize,
    follow: bool,
) -> Vec<Line<'static>> {
    const WINDOW: usize = 200;
    if lines.len() <= WINDOW {
        return lines;
    }
    // When following, the newest end is what matters; otherwise keep the selection
    // in view.
    let start = if follow {
        lines.len().saturating_sub(WINDOW)
    } else {
        selected
            .saturating_sub(WINDOW / 2)
            .min(lines.len() - WINDOW)
    };
    lines[start..start + WINDOW].to_vec()
}

/// The status bar.
fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let (label, style) = if app.connected {
        ("connected", Style::default().fg(Color::Green))
    } else {
        ("disconnected", Style::default().fg(Color::Red))
    };

    let mut spans = vec![Span::styled(format!(" {label} "), style)];
    if app.permission == Permission::ReadOnly {
        spans.push(Span::styled(
            " read-only ",
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(error) = &app.last_error {
        spans.push(Span::styled(
            format!(" {error}"),
            Style::default().fg(Color::Red),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The confirmation overlay.
fn draw_confirmation(frame: &mut Frame, _app: &App, area: Rect, pending: super::app::Pending) {
    let action = match pending {
        super::app::Pending::Start => "start",
        super::app::Pending::Stop => "stop",
    };
    let text = format!(" {action} the kernel?  y / enter to confirm, n / esc to cancel ");
    draw_overlay(frame, area, &text, 3);
}

/// The help overlay.
fn draw_help(frame: &mut Frame, area: Rect) {
    let text = concat!(
        " q quit          tab / shift-tab  switch panel\n",
        " j k / arrows    move            pgup/pgdn  page\n",
        " g G             first / last    r          refresh\n",
        " f               follow logs     s / S      start / stop\n",
        " ? or esc        close this help ",
    );
    draw_overlay(frame, area, text, 7);
}

/// Draws a centred box with `text`.
fn draw_overlay(frame: &mut Frame, area: Rect, text: &str, height: u16) {
    // Clamped to the area: a box taller or wider than the terminal would render
    // outside the buffer and panic, and an overlay is exactly what someone might
    // open on a resized window.
    let width = area.width.min(64);
    let height = height.min(area.height);
    if width == 0 || height == 0 {
        return;
    }
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    // Cleared first, or the panel underneath shows through the box.
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
}

/// The style for a lifecycle state.
fn state_style(state: &str) -> Style {
    match state.to_ascii_lowercase().as_str() {
        "running" => Style::default().fg(Color::Green),
        "stopped" | "failed" => Style::default().fg(Color::Red),
        "starting" | "stopping" => Style::default().fg(Color::Yellow),
        _ => Style::default(),
    }
}

/// The style for a log level.
fn level_style(level: &str) -> Style {
    match level.to_ascii_lowercase().as_str() {
        "error" => Style::default().fg(Color::Red),
        "warning" | "warn" => Style::default().fg(Color::Yellow),
        "debug" => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::Cyan),
    }
}

#[cfg(test)]
mod tests;
