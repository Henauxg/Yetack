//! A terminal tab's menu, opened by a right-click on the tab: pin or unpin it, move the terminal
//! to the other dock, or close it.

use cockpit_protocol::{AgentId, TerminalId};

use super::app_state::AppState;
use super::docks::{self, Dock, TabKey};
use super::tab_menu::{PIN_LABEL, UNPIN_LABEL};
use super::terminal_menu::{CLOSE_LABEL, move_label};
use super::ui_state::Popover;

/// A row of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalTabItem {
    Pin,
    Unpin,
    MoveTo(Dock),
    Close,
}

impl TerminalTabItem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pin => PIN_LABEL,
            Self::Unpin => UNPIN_LABEL,
            Self::MoveTo(dock) => move_label(dock),
            Self::Close => CLOSE_LABEL,
        }
    }

    /// The letter that picks it while the menu is open.
    pub fn key(self) -> char {
        match self {
            Self::Pin | Self::Unpin => 'p',
            Self::MoveTo(_) => 'm',
            Self::Close => 'x',
        }
    }
}

/// The open menu, and its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTabTarget {
    pub agent: AgentId,
    pub terminal: TerminalId,
    pub x: i32,
    pub y: i32,
    pub items: Vec<TerminalTabItem>,
}

/// Opens the menu of `terminal`'s tab at `x`, `y` in the window's pixels.
pub fn open(state: &mut AppState, agent: AgentId, terminal: TerminalId, x: i32, y: i32) {
    state.ui.popover = Some(Popover::TerminalTabMenu {
        agent,
        terminal,
        x,
        y,
    });
}

/// The open menu, while its terminal is still there.
pub fn target(state: &AppState) -> Option<TerminalTabTarget> {
    let Some(Popover::TerminalTabMenu {
        agent,
        terminal,
        x,
        y,
    }) = state.ui.popover
    else {
        return None;
    };
    state.agents.get(&agent)?.terminals.tab(terminal)?;
    let pin = if docks::is_pinned(state, agent, &TabKey::Terminal(terminal)) {
        TerminalTabItem::Unpin
    } else {
        TerminalTabItem::Pin
    };
    let dock = docks::terminal_dock(state, agent, terminal);
    Some(TerminalTabTarget {
        agent,
        terminal,
        x,
        y,
        items: vec![
            pin,
            TerminalTabItem::MoveTo(dock.other()),
            TerminalTabItem::Close,
        ],
    })
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentStatus, TerminalInfo};

    use super::*;
    use crate::state::{state_with, summary, terminals};

    #[test]
    fn the_menu_pins_the_terminal_and_moves_it_to_the_other_dock() {
        let agent = AgentId::new(1);
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent);
        state.prefs.terminal.place = Dock::Side;
        let info = TerminalInfo {
            terminal: TerminalId::new(1),
            title: "zsh".to_owned(),
            exited: None,
        };
        terminals::listed(&mut state, agent, Some(vec![info]), Vec::new());

        open(&mut state, agent, TerminalId::new(1), 10, 20);
        let items = target(&state).map(|menu| menu.items);
        open(&mut state, agent, TerminalId::new(9), 10, 20);
        let gone = target(&state);

        assert_eq!(
            items,
            Some(vec![
                TerminalTabItem::Pin,
                TerminalTabItem::MoveTo(Dock::Bottom),
                TerminalTabItem::Close,
            ])
        );
        assert_eq!(gone, None, "no menu for a terminal that is gone");
    }
}
