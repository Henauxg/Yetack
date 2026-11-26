//! A terminal tab's menu, opened by a right-click on the tab, under the terminal's id: let the
//! agent read the terminal or stop it, rename the tab, copy the id, pin or unpin the tab, move the
//! terminal to the other dock, or close it.

use cockpit_protocol::{AgentId, SHARE_LABEL, TERMINAL_WORD, TerminalId};

use super::app_state::AppState;
use super::docks::{self, Dock, TabKey};
use super::requests::Outgoing;
use super::tab_menu::{PIN_LABEL, UNPIN_LABEL};
use super::terminal_menu::{CLOSE_LABEL, move_label};
use super::terminals;
use super::ui_state::Popover;

/// A row of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalTabItem {
    Share,
    StopSharing,
    Rename,
    CopyId,
    Pin,
    Unpin,
    MoveTo(Dock),
    Close,
}

impl TerminalTabItem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Share => SHARE_LABEL,
            Self::StopSharing => STOP_SHARING_LABEL,
            Self::Rename => RENAME_LABEL,
            Self::CopyId => "Copy the id",
            Self::Pin => PIN_LABEL,
            Self::Unpin => UNPIN_LABEL,
            Self::MoveTo(dock) => move_label(dock),
            Self::Close => CLOSE_LABEL,
        }
    }

    /// What the row does, as its tooltip says.
    pub fn title(self) -> &'static str {
        match self {
            Self::Share => "The agent's tools can then read this terminal's screen and history",
            Self::StopSharing => "The agent's tools can no longer read this terminal",
            Self::Rename => "Give this tab a name of your own (double-click its title)",
            Self::CopyId => "Copy \"terminal <id>\", which names this terminal to the agent",
            Self::Pin | Self::Unpin | Self::MoveTo(_) | Self::Close => "",
        }
    }

    /// The letter that picks it while the menu is open.
    pub fn key(self) -> char {
        match self {
            Self::Share | Self::StopSharing => 'r',
            Self::Rename => 'n',
            Self::CopyId => 'i',
            Self::Pin | Self::Unpin => 'p',
            Self::MoveTo(_) => 'm',
            Self::Close => 'x',
        }
    }
}

const STOP_SHARING_LABEL: &str = "Stop letting the agent read it";
const RENAME_LABEL: &str = "Rename";

/// The menu's title line, and the tab's tooltip: "Terminal 3".
pub fn heading(terminal: TerminalId) -> String {
    format!("Terminal {}", terminal.get())
}

/// What "Copy the id" copies, for the user to name the terminal to the agent: "terminal 3".
pub fn id_text(terminal: TerminalId) -> String {
    format!("{TERMINAL_WORD} {}", terminal.get())
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
    let tab = state.agents.get(&agent)?.terminals.tab(terminal)?;
    // A daemon whose agents get no terminal tools has nothing to share with.
    let share = state.agent_reads_terminals(agent).then_some(if tab.shared {
        TerminalTabItem::StopSharing
    } else {
        TerminalTabItem::Share
    });
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
        items: share
            .into_iter()
            .chain([
                TerminalTabItem::Rename,
                TerminalTabItem::CopyId,
                pin,
                TerminalTabItem::MoveTo(dock.other()),
                TerminalTabItem::Close,
            ])
            .collect(),
    })
}

/// Does what the picked row says, and closes the menu.
pub fn run(
    state: &mut AppState,
    agent: AgentId,
    terminal: TerminalId,
    item: TerminalTabItem,
) -> Vec<Outgoing> {
    state.ui.popover = None;
    match item {
        TerminalTabItem::Share => vec![terminals::share(agent, terminal, true)],
        TerminalTabItem::StopSharing => vec![terminals::share(agent, terminal, false)],
        TerminalTabItem::Rename => {
            terminals::start_rename(state, agent, terminal);
            Vec::new()
        }
        // The window copies the text: the state has no clipboard.
        TerminalTabItem::CopyId => Vec::new(),
        TerminalTabItem::Pin | TerminalTabItem::Unpin => {
            docks::toggle_pin(state, agent, &TabKey::Terminal(terminal));
            Vec::new()
        }
        TerminalTabItem::MoveTo(dock) => {
            terminals::move_terminal(state, agent, terminal, dock, None);
            Vec::new()
        }
        TerminalTabItem::Close => terminals::close(state, agent, terminal),
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{
        AgentStatus, Request, TerminalChange, TerminalInfo, TerminalParams, TerminalShareParams,
        TerminalUpdate,
    };

    use super::*;
    use crate::state::{AppState, state_with, summary};

    #[test]
    fn the_menu_pins_the_terminal_and_moves_it_to_the_other_dock() {
        let agent = AgentId::new(1);
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.this.agent_tools = true;
        state.ui.selected = Some(agent);
        state.prefs.terminal.place = Dock::Side;
        let info = TerminalInfo {
            terminal: TerminalId::new(1),
            title: "zsh".to_owned(),
            exited: None,
            shared: false,
            name: None,
        };
        terminals::listed(&mut state, agent, Some(vec![info]), Vec::new());

        open(&mut state, agent, TerminalId::new(1), 10, 20);
        let items = target(&state).map(|menu| menu.items);
        open(&mut state, agent, TerminalId::new(9), 10, 20);
        let gone = target(&state);

        assert_eq!(
            items,
            Some(vec![
                TerminalTabItem::Share,
                TerminalTabItem::Rename,
                TerminalTabItem::CopyId,
                TerminalTabItem::Pin,
                TerminalTabItem::MoveTo(Dock::Bottom),
                TerminalTabItem::Close,
            ])
        );
        assert_eq!(gone, None, "no menu for a terminal that is gone");
    }

    /// Agent 1, selected, with terminal 1, unshared, on a daemon whose agents read terminals.
    fn one_terminal() -> (AppState, AgentId, TerminalId) {
        let agent = AgentId::new(1);
        let terminal = TerminalId::new(1);
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.this.agent_tools = true;
        state.ui.selected = Some(agent);
        let info = TerminalInfo {
            terminal,
            title: "zsh".to_owned(),
            exited: None,
            shared: false,
            name: None,
        };
        terminals::listed(&mut state, agent, Some(vec![info]), Vec::new());
        (state, agent, terminal)
    }

    fn first_item(state: &AppState) -> Option<TerminalTabItem> {
        target(state).and_then(|menu| menu.items.first().copied())
    }

    /// The daemon in the app gives its agents no terminal tools: a share would do nothing.
    #[test]
    fn a_daemon_without_terminal_tools_offers_no_share_row() {
        let (mut state, agent, terminal) = one_terminal();
        state.machines.this.agent_tools = false;

        open(&mut state, agent, terminal, 10, 20);
        let items = target(&state).map(|menu| menu.items).unwrap_or_default();

        assert!(
            !items
                .iter()
                .any(|item| matches!(item, TerminalTabItem::Share | TerminalTabItem::StopSharing)),
            "{items:?}"
        );
        assert_eq!(items.first(), Some(&TerminalTabItem::Rename));
    }

    /// The menu asks the daemon, and the row follows the daemon's update, which every window gets.
    #[test]
    fn the_share_row_asks_the_daemon_and_turns_into_its_opposite_once_shared() {
        let (mut state, agent, terminal) = one_terminal();
        open(&mut state, agent, terminal, 10, 20);
        let before = first_item(&state);

        let sent = run(&mut state, agent, terminal, TerminalTabItem::Share);
        let closed = state.ui.popover.is_none();
        terminals::apply_update(
            &mut state,
            TerminalUpdate {
                agent,
                terminal,
                change: TerminalChange::Shared { shared: true },
            },
        );
        open(&mut state, agent, terminal, 10, 20);
        let after = first_item(&state);
        let stop = run(&mut state, agent, terminal, TerminalTabItem::StopSharing);

        assert_eq!(before, Some(TerminalTabItem::Share));
        assert_eq!(after, Some(TerminalTabItem::StopSharing));
        assert!(closed, "the pick closes the menu");
        let shared = |shared| {
            Request::TerminalShare(TerminalShareParams {
                agent,
                terminal,
                shared,
            })
        };
        let requests = |sent: Vec<Outgoing>| -> Vec<Request> {
            sent.into_iter().map(|outgoing| outgoing.request).collect()
        };
        assert_eq!(requests(sent), [shared(true)]);
        assert_eq!(requests(stop), [shared(false)]);
        assert_eq!(TerminalTabItem::Share.key(), 'r');
        assert_eq!(TerminalTabItem::Share.label(), "Let the agent read it");
        assert_eq!(
            TerminalTabItem::StopSharing.label(),
            "Stop letting the agent read it"
        );
    }

    /// The id, which the tab no longer shows, heads the menu, and its row copies the words that
    /// name the terminal to the agent.
    #[test]
    fn the_menu_is_headed_by_the_id_that_its_copy_row_copies() {
        let (mut state, agent, terminal) = one_terminal();
        open(&mut state, agent, terminal, 10, 20);

        let sent = run(&mut state, agent, terminal, TerminalTabItem::CopyId);

        assert!(sent.is_empty());
        assert_eq!(state.ui.popover, None, "the pick closes the menu");
        assert_eq!(heading(TerminalId::new(7)), "Terminal 7");
        assert_eq!(id_text(TerminalId::new(7)), "terminal 7");
        assert_eq!(TerminalTabItem::CopyId.label(), "Copy the id");
        assert_eq!(TerminalTabItem::CopyId.key(), 'i');
    }

    #[test]
    fn the_rename_row_puts_the_title_in_a_field() {
        let (mut state, agent, terminal) = one_terminal();
        open(&mut state, agent, terminal, 10, 20);

        let sent = run(&mut state, agent, terminal, TerminalTabItem::Rename);

        assert!(sent.is_empty());
        assert_eq!(state.ui.popover, None);
        assert_eq!(
            state.ui.terminal_rename,
            Some(TerminalParams { agent, terminal })
        );
        assert_eq!(TerminalTabItem::Rename.key(), 'n');
    }
}
