//! The terminal pane: each agent's terminals, the tab it shows, and the requests that list, open
//! and close them. The output does not pass here: it goes from the connection to the page, see
//! `terminal_feed`.

use cockpit_protocol::{
    AgentId, AgentParams, Request, TerminalChange, TerminalId, TerminalInfo, TerminalInputParams,
    TerminalOpenParams, TerminalParams, TerminalResizeParams, TerminalSize, TerminalUpdate,
};

use super::app_state::AppState;
use super::requests::{Outgoing, Purpose};
use crate::machine::MachineId;
use crate::prefs::TerminalPlace;

/// The size a terminal opens with, before the pane measures itself.
const OPENING_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

/// An agent's terminals, as the pane shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTerminals {
    /// In the order they opened.
    pub tabs: Vec<TerminalTab>,
    /// The tab the pane shows.
    pub shown: Option<TerminalId>,
    pub listing: Listing,
    /// An `agent.terminal_open` is on its way.
    pub opening: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTab {
    pub terminal: TerminalId,
    /// The shell's name, then the title the shell sets.
    pub title: String,
    /// The exit code once the shell ended, `Some(None)` without one.
    pub exited: Option<Option<i32>>,
}

/// Whether the app knows an agent's terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Listing {
    #[default]
    NotAsked,
    Asking,
    Listed,
}

/// The output a terminal keeps, which the page shows before what comes next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    pub agent: AgentId,
    pub terminal: TerminalId,
    pub offset: u64,
    /// Base64.
    pub data: String,
}

impl AgentTerminals {
    fn tab_mut(&mut self, terminal: TerminalId) -> Option<&mut TerminalTab> {
        self.tabs.iter_mut().find(|tab| tab.terminal == terminal)
    }

    fn add(&mut self, terminal: TerminalId, title: String) {
        if self.tab_mut(terminal).is_none() {
            self.tabs.push(TerminalTab {
                terminal,
                title,
                exited: None,
            });
        }
    }

    fn remove(&mut self, terminal: TerminalId) {
        let index = self.tabs.iter().position(|tab| tab.terminal == terminal);
        let Some(index) = index else {
            return;
        };
        self.tabs.remove(index);
        if self.shown == Some(terminal) {
            // The tab beside it takes its place.
            let next = index.min(self.tabs.len().saturating_sub(1));
            self.shown = self.tabs.get(next).map(|tab| tab.terminal);
        }
    }
}

/// The selected agent's terminals.
pub fn selected(state: &AppState) -> Option<(AgentId, &AgentTerminals)> {
    let agent = state.ui.selected?;
    let entry = state.agents.get(&agent)?;
    Some((agent, &entry.terminals))
}

fn terminals_mut(state: &mut AppState, agent: AgentId) -> Option<&mut AgentTerminals> {
    state
        .agents
        .get_mut(&agent)
        .map(|entry| &mut entry.terminals)
}

fn outgoing(request: Request, purpose: Purpose) -> Outgoing {
    Outgoing {
        request,
        purpose,
        to: MachineId::THIS,
    }
}

/// The header's button and its shortcut.
pub fn toggle_pane(state: &mut AppState) -> Vec<Outgoing> {
    state.ui.terminal_open = !state.ui.terminal_open;
    Vec::new()
}

pub fn move_to(state: &mut AppState, place: TerminalPlace) {
    state.prefs.terminal.place = place;
    state.ui.terminal_open = true;
}

/// While the pane shows, the selected agent's terminals are listed, and an agent with none gets
/// one.
pub fn needed_requests(state: &mut AppState) -> Vec<Outgoing> {
    if !state.ui.terminal_open {
        return Vec::new();
    }
    let Some(agent) = state.ui.selected else {
        return Vec::new();
    };
    let Some(terminals) = terminals_mut(state, agent) else {
        return Vec::new();
    };
    match terminals.listing {
        Listing::NotAsked => {
            terminals.listing = Listing::Asking;
            vec![outgoing(
                Request::AgentTerminals(AgentParams { agent }),
                Purpose::Terminals { agent },
            )]
        }
        Listing::Listed if terminals.tabs.is_empty() && !terminals.opening => open(state, agent),
        Listing::Asking | Listing::Listed => Vec::new(),
    }
}

/// The tab bar's "+".
pub fn open(state: &mut AppState, agent: AgentId) -> Vec<Outgoing> {
    let Some(terminals) = terminals_mut(state, agent) else {
        return Vec::new();
    };
    terminals.opening = true;
    vec![outgoing(
        Request::AgentTerminalOpen(TerminalOpenParams {
            agent,
            size: OPENING_SIZE,
        }),
        Purpose::TerminalOpened { agent },
    )]
}

/// `agent.terminals` answered. A failure leaves the pane empty, with no retry.
pub fn listed(state: &mut AppState, agent: AgentId, listed: Result<Vec<TerminalInfo>, ()>) {
    let Some(terminals) = terminals_mut(state, agent) else {
        return;
    };
    terminals.listing = Listing::Listed;
    let Ok(listed) = listed else {
        terminals.opening = true;
        return;
    };
    for info in listed {
        terminals.add(info.terminal, info.title);
        if let Some(tab) = terminals.tab_mut(info.terminal) {
            tab.exited = info.exited;
        }
    }
    if terminals.shown.is_none() {
        terminals.shown = terminals.tabs.last().map(|tab| tab.terminal);
    }
}

/// `agent.terminal_open` answered: the pane shows the new terminal.
pub fn opened(state: &mut AppState, agent: AgentId, opened: Option<TerminalId>) {
    let Some(terminals) = terminals_mut(state, agent) else {
        return;
    };
    terminals.opening = false;
    if let Some(terminal) = opened {
        terminals.add(terminal, String::new());
        terminals.shown = Some(terminal);
    }
}

/// An opening, an exit or a closing, from any client.
pub fn apply_update(state: &mut AppState, update: TerminalUpdate) {
    let Some(terminals) = terminals_mut(state, update.agent) else {
        return;
    };
    match update.change {
        TerminalChange::Opened { title } => {
            terminals.add(update.terminal, title.clone());
            if let Some(tab) = terminals.tab_mut(update.terminal)
                && tab.title.is_empty()
            {
                tab.title = title;
            }
        }
        TerminalChange::Exited { code } => {
            if let Some(tab) = terminals.tab_mut(update.terminal) {
                tab.exited = Some(code);
            }
        }
        TerminalChange::Closed => terminals.remove(update.terminal),
        // Output goes to the page, not to the state.
        TerminalChange::Output { .. } => {}
    }
}

pub fn show(state: &mut AppState, agent: AgentId, terminal: TerminalId) {
    if let Some(terminals) = terminals_mut(state, agent) {
        terminals.shown = Some(terminal);
    }
}

/// A tab's "×": ends its shell.
pub fn close(state: &mut AppState, agent: AgentId, terminal: TerminalId) -> Vec<Outgoing> {
    let Some(terminals) = terminals_mut(state, agent) else {
        return Vec::new();
    };
    terminals.remove(terminal);
    // The last tab closed: the pane closes with it, rather than opening another.
    if terminals.tabs.is_empty() {
        state.ui.terminal_open = false;
    }
    vec![outgoing(
        Request::TerminalClose(TerminalParams { agent, terminal }),
        Purpose::Action,
    )]
}

/// The shell's title, as the page reads it from the terminal.
pub fn set_title(state: &mut AppState, agent: AgentId, terminal: TerminalId, title: String) {
    if let Some(tab) = terminals_mut(state, agent).and_then(|terminals| terminals.tab_mut(terminal))
    {
        tab.title = title;
    }
}

pub fn input(agent: AgentId, terminal: TerminalId, data: String) -> Outgoing {
    outgoing(
        Request::TerminalInput(TerminalInputParams {
            agent,
            terminal,
            data,
        }),
        Purpose::Action,
    )
}

pub fn resize(agent: AgentId, terminal: TerminalId, size: TerminalSize) -> Outgoing {
    outgoing(
        Request::TerminalResize(TerminalResizeParams {
            agent,
            terminal,
            size,
        }),
        Purpose::Action,
    )
}

/// The page shows a terminal it has no output of yet.
pub fn attach(agent: AgentId, terminal: TerminalId) -> Outgoing {
    outgoing(
        Request::TerminalAttach(TerminalParams { agent, terminal }),
        Purpose::TerminalScreen { agent, terminal },
    )
}

/// `terminal.attach` answered: the page writes it before the output that follows.
pub fn screen(state: &mut AppState, screen: Screen) {
    state.ui.terminal_screens.push(screen);
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::AgentStatus;

    use super::*;
    use crate::state::{state_with, summary};

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn tab(terminal: u32) -> TerminalInfo {
        TerminalInfo {
            terminal: TerminalId::new(terminal),
            title: "zsh".to_owned(),
            exited: None,
        }
    }

    fn tabs(state: &AppState) -> Vec<u32> {
        state.agents[&agent()]
            .terminals
            .tabs
            .iter()
            .map(|tab| tab.terminal.get())
            .collect()
    }

    #[test]
    fn opening_the_pane_lists_the_terminals_then_opens_one_for_an_agent_with_none() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        assert!(needed_requests(&mut state).is_empty(), "the pane is closed");

        toggle_pane(&mut state);
        let list = needed_requests(&mut state);
        assert!(needed_requests(&mut state).is_empty(), "asked once");
        listed(&mut state, agent(), Ok(Vec::new()));
        let open = needed_requests(&mut state);
        assert!(needed_requests(&mut state).is_empty(), "opened once");
        opened(&mut state, agent(), Some(TerminalId::new(7)));

        assert!(matches!(
            list.as_slice(),
            [Outgoing {
                request: Request::AgentTerminals(_),
                ..
            }]
        ));
        assert!(matches!(
            open.as_slice(),
            [Outgoing {
                request: Request::AgentTerminalOpen(_),
                ..
            }]
        ));
        assert_eq!(tabs(&state), [7]);
        assert_eq!(
            state.agents[&agent()].terminals.shown,
            Some(TerminalId::new(7))
        );
    }

    #[test]
    fn a_listed_agent_shows_its_last_terminal_and_closing_one_shows_its_neighbour() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        toggle_pane(&mut state);
        needed_requests(&mut state);

        listed(&mut state, agent(), Ok(vec![tab(2), tab(3), tab(4)]));
        show(&mut state, agent(), TerminalId::new(3));
        close(&mut state, agent(), TerminalId::new(3));

        assert_eq!(tabs(&state), [2, 4]);
        assert_eq!(
            state.agents[&agent()].terminals.shown,
            Some(TerminalId::new(4))
        );
        assert!(needed_requests(&mut state).is_empty());
    }

    #[test]
    fn closing_the_last_tab_closes_the_pane() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        toggle_pane(&mut state);
        needed_requests(&mut state);
        listed(&mut state, agent(), Ok(vec![tab(2)]));

        close(&mut state, agent(), TerminalId::new(2));

        assert!(!state.ui.terminal_open);
        assert!(needed_requests(&mut state).is_empty());
    }

    #[test]
    fn updates_from_other_clients_add_mark_and_remove_tabs() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let update = |terminal, change| TerminalUpdate {
            agent: agent(),
            terminal: TerminalId::new(terminal),
            change,
        };

        apply_update(
            &mut state,
            update(
                5,
                TerminalChange::Opened {
                    title: "bash".to_owned(),
                },
            ),
        );
        apply_update(
            &mut state,
            update(5, TerminalChange::Exited { code: Some(2) }),
        );
        apply_update(
            &mut state,
            update(
                6,
                TerminalChange::Opened {
                    title: "bash".to_owned(),
                },
            ),
        );
        apply_update(&mut state, update(6, TerminalChange::Closed));

        let terminals = &state.agents[&agent()].terminals;
        assert_eq!(tabs(&state), [5]);
        assert_eq!(terminals.tabs[0].title, "bash");
        assert_eq!(terminals.tabs[0].exited, Some(Some(2)));
    }
}
