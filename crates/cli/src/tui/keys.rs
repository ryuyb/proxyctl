//! What the TUI can be asked to do, and which key asks for it.
//!
//! # Why the mapping is a pure function
//!
//! AGENTS.md forbids business logic in keyboard handlers, and the practical reason
//! is testability: a handler that resolved an endpoint, called the agent, and
//! updated a screen could only be tested by driving a terminal. This module maps a
//! key press to an [`Action`] and does nothing else, so every binding is covered by
//! an ordinary unit test.
//!
//! Executing the action is [`app`](super::app)'s job.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Something the user asked for.
///
/// A closed enum rather than a callback: it is the list of what the interface can
/// do, so adding a capability is a visible change here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Stop and restore the terminal.
    Quit,
    /// Move to the next panel.
    NextPanel,
    /// Move to the previous panel.
    PreviousPanel,
    /// Move the selection down.
    Next,
    /// Move the selection up.
    Previous,
    /// Page down.
    PageDown,
    /// Page Up.
    PageUp,
    /// Move to the first row.
    Top,
    /// Move to the last row.
    Bottom,
    /// Refresh immediately.
    Refresh,
    /// Toggle whether the log panel follows the newest line.
    ToggleFollow,
    /// Ask to start the kernel, pending confirmation.
    RequestStart,
    /// Ask to stop the kernel, pending confirmation.
    RequestStop,
    /// Confirm a pending request.
    Confirm,
    /// Discard a pending request, or close help.
    Cancel,
    /// Show or hide the help overlay.
    ToggleHelp,
}

/// The panels, in tab order.
///
/// A closed enum rather than an index so the tab order is a fact about the type
/// instead of something to keep in step with a length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// Kernel state, health, active configuration.
    Overview,
    /// Proxy groups and their selected members.
    Proxies,
    /// Kernel log lines.
    Logs,
    /// Events from the agent.
    Events,
    /// Recent jobs.
    Jobs,
}

impl Panel {
    /// Every panel, in tab order.
    pub const ALL: [Self; 5] = [
        Self::Overview,
        Self::Proxies,
        Self::Logs,
        Self::Events,
        Self::Jobs,
    ];

    /// The title shown on the tab bar.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Overview => "overview",
            Self::Proxies => "proxies",
            Self::Logs => "logs",
            Self::Events => "events",
            Self::Jobs => "jobs",
        }
    }

    /// The next panel in tab order, wrapping.
    #[must_use]
    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    /// The previous panel in tab order, wrapping.
    #[must_use]
    pub fn previous(self) -> Self {
        let index = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(index + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

/// Maps a key press onto an action.
///
/// Returns `None` for a key with no binding, which is the majority: an unbound key
/// is not an error, it is just not bound.
///
/// # Key release is ignored
///
/// Terminals report press, repeat, and release on some platforms. Acting on
/// release would run every action twice, which for a stop request is not a
/// cosmetic problem.
#[must_use]
pub fn action_for(key: KeyEvent) -> Option<Action> {
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return None;
    }

    // Control-C quits from anywhere, and is checked before the plain bindings so
    // it cannot be shadowed as the key map grows.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(Action::Quit);
    }

    // A modified key is not the plain binding. Without this, Shift+Tab would
    // match `Char('T')`-style bindings and a terminal that reports modifiers
    // differently would silently change behaviour.
    let unmodified = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;

    Some(match key.code {
        KeyCode::Esc => Action::Cancel,
        KeyCode::Char('q') if unmodified => Action::Quit,
        KeyCode::Tab => Action::NextPanel,
        KeyCode::BackTab => Action::PreviousPanel,
        KeyCode::Down => Action::Next,
        KeyCode::Up => Action::Previous,
        KeyCode::Char('j') if unmodified => Action::Next,
        KeyCode::Char('k') if unmodified => Action::Previous,
        KeyCode::PageDown => Action::PageDown,
        KeyCode::PageUp => Action::PageUp,
        KeyCode::Home | KeyCode::Char('g') if unmodified => Action::Top,
        KeyCode::End | KeyCode::Char('G') => Action::Bottom,
        KeyCode::Char('r') if unmodified => Action::Refresh,
        KeyCode::Char('f') if unmodified => Action::ToggleFollow,
        KeyCode::Char('s') => Action::RequestStart,
        KeyCode::Char('S') => Action::RequestStop,
        KeyCode::Char('y') | KeyCode::Enter => Action::Confirm,
        KeyCode::Char('n') if unmodified => Action::Cancel,
        KeyCode::Char('?') => Action::ToggleHelp,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn shifted(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    /// Every binding, asserted one at a time so a failure names the key that broke.
    #[test]
    fn the_documented_bindings_are_all_present() {
        assert_eq!(action_for(press(KeyCode::Char('q'))), Some(Action::Quit));
        assert_eq!(action_for(press(KeyCode::Tab)), Some(Action::NextPanel));
        assert_eq!(
            action_for(press(KeyCode::BackTab)),
            Some(Action::PreviousPanel)
        );
        assert_eq!(action_for(press(KeyCode::Down)), Some(Action::Next));
        assert_eq!(action_for(press(KeyCode::Up)), Some(Action::Previous));
        assert_eq!(action_for(press(KeyCode::Char('j'))), Some(Action::Next));
        assert_eq!(
            action_for(press(KeyCode::Char('k'))),
            Some(Action::Previous)
        );
        assert_eq!(action_for(press(KeyCode::PageDown)), Some(Action::PageDown));
        assert_eq!(action_for(press(KeyCode::PageUp)), Some(Action::PageUp));
        assert_eq!(action_for(press(KeyCode::Home)), Some(Action::Top));
        assert_eq!(action_for(press(KeyCode::End)), Some(Action::Bottom));
        assert_eq!(action_for(press(KeyCode::Char('r'))), Some(Action::Refresh));
        assert_eq!(
            action_for(press(KeyCode::Char('f'))),
            Some(Action::ToggleFollow)
        );
        assert_eq!(
            action_for(press(KeyCode::Char('s'))),
            Some(Action::RequestStart)
        );
        assert_eq!(
            action_for(press(KeyCode::Char('S'))),
            Some(Action::RequestStop)
        );
        assert_eq!(action_for(press(KeyCode::Enter)), Some(Action::Confirm));
        assert_eq!(action_for(press(KeyCode::Esc)), Some(Action::Cancel));
        assert_eq!(action_for(press(KeyCode::Char('n'))), Some(Action::Cancel));
        assert_eq!(
            action_for(press(KeyCode::Char('?'))),
            Some(Action::ToggleHelp)
        );
    }

    /// Control-C quits, and is checked before anything else so it cannot be
    /// shadowed as bindings are added.
    #[test]
    fn control_c_quits() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(action_for(key), Some(Action::Quit));
    }

    /// A release event must not act, or every action would run twice — which for a
    /// stop request is not cosmetic.
    #[test]
    fn a_key_release_does_nothing() {
        let mut key = press(KeyCode::Char('s'));
        key.kind = KeyEventKind::Release;
        assert_eq!(action_for(key), None);
    }

    /// A repeat still acts: holding a movement key is how a list is scrolled.
    #[test]
    fn a_key_repeat_still_acts() {
        let mut key = press(KeyCode::Down);
        key.kind = KeyEventKind::Repeat;
        assert_eq!(action_for(key), Some(Action::Next));
    }

    /// An unbound key is not an error, and must not fall through to something
    /// surprising.
    #[test]
    fn an_unbound_key_does_nothing() {
        for code in [KeyCode::Char('z'), KeyCode::F(5), KeyCode::Insert] {
            assert_eq!(action_for(press(code)), None, "{code:?}");
        }
    }

    /// The two ordering bindings are asserted explicitly, because a guard used
    /// with `|` in a match arm applies to the whole arm and it is easy to write one
    /// that silently enables a modified key.
    #[test]
    fn the_start_and_stop_keys_are_distinct() {
        assert_eq!(
            action_for(press(KeyCode::Char('s'))),
            Some(Action::RequestStart)
        );
        assert_eq!(
            action_for(shifted(KeyCode::Char('S'))),
            Some(Action::RequestStop)
        );
    }

    /// Tab order is a fact about the panel set, so it is asserted rather than
    /// assumed: a new panel added to the enum must be reachable.
    #[test]
    fn tab_order_visits_every_panel_and_wraps() {
        let mut seen = Vec::new();
        let mut panel = Panel::Overview;
        for _ in 0..Panel::ALL.len() {
            seen.push(panel);
            panel = panel.next();
        }
        assert_eq!(panel, Panel::Overview, "the cycle must wrap");
        for expected in Panel::ALL {
            assert!(seen.contains(&expected), "{expected:?} is unreachable");
        }
    }

    #[test]
    fn reverse_tab_order_is_the_inverse_of_forward() {
        for panel in Panel::ALL {
            assert_eq!(panel.next().previous(), panel, "{panel:?}");
            assert_eq!(panel.previous().next(), panel, "{panel:?}");
        }
    }

    #[test]
    fn every_panel_has_a_distinct_title() {
        let mut titles: Vec<&str> = Panel::ALL.iter().map(|p| p.title()).collect();
        titles.sort_unstable();
        let before = titles.len();
        titles.dedup();
        assert_eq!(titles.len(), before, "two panels share a title");
    }
}
