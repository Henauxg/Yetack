//! The terminal pane: each agent's terminals, the tab it shows, its tab in the file view, and the
//! requests that list, open and close them. The output does not pass here: it goes from the
//! connection to the page, see `terminal_feed`.

use std::path::Path;

use cockpit_protocol::{
    AgentId, AgentParams, Request, TerminalChange, TerminalExit, TerminalId, TerminalInfo,
    TerminalInputParams, TerminalOpenParams, TerminalParams, TerminalResizeParams, TerminalSize,
    TerminalUpdate,
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
    /// The last opening failed: the pane waits for the "+" rather than trying again.
    pub open_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTab {
    pub terminal: TerminalId,
    /// The shell's name, then the title the shell sets.
    pub title: String,
    /// `Some` once the shell ended.
    pub exited: Option<TerminalExit>,
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

/// Every agent's terminals: the page keeps a screen for these and drops the others.
pub fn live(state: &AppState) -> Vec<TerminalParams> {
    state
        .agents
        .iter()
        .flat_map(|(agent, entry)| {
            entry.terminals.tabs.iter().map(|tab| TerminalParams {
                agent: *agent,
                terminal: tab.terminal,
            })
        })
        .collect()
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

/// Whether the selected agent's pane shows.
pub fn pane_open(state: &AppState) -> bool {
    state
        .ui
        .selected
        .is_some_and(|agent| state.ui.terminal_panes.contains(&agent))
}

/// Whether `agent`'s terminal is a tab of its file view.
pub fn in_file_view(state: &AppState, agent: AgentId) -> bool {
    state.prefs.terminal.place == TerminalPlace::FileView
        && state.ui.terminal_panes.contains(&agent)
}

/// Whether `agent`'s file view shows the terminal: the user picked its tab, or no file is open.
pub fn file_view_shows_terminal(state: &AppState, agent: AgentId) -> bool {
    in_file_view(state, agent)
        && (state.ui.file_view(agent).is_none() || state.ui.terminal_tab_shown.contains(&agent))
}

/// The file `agent`'s file view shows, unless the terminal's tab is over it.
pub fn shown_file(state: &AppState, agent: AgentId) -> Option<&Path> {
    let view = state.ui.file_view(agent)?;
    (!file_view_shows_terminal(state, agent)).then_some(view.path.as_path())
}

/// Whether `agent`'s file view panel shows: with a file open, or with the terminal as its tab.
pub fn file_view_open(state: &AppState, agent: AgentId) -> bool {
    state.ui.file_view(agent).is_some() || in_file_view(state, agent)
}

/// Shows the terminal's tab in `agent`'s file view, when the terminal is a tab there.
pub fn show_tab(state: &mut AppState, agent: AgentId) {
    if state.prefs.terminal.place == TerminalPlace::FileView {
        state.ui.terminal_tab_shown.insert(agent);
    }
}

/// Hides `agent`'s pane. Its shells keep running.
pub fn hide(state: &mut AppState, agent: AgentId) {
    let panel_was_open = file_view_open(state, agent);
    state.ui.terminal_panes.remove(&agent);
    state.ui.terminal_tab_shown.remove(&agent);
    // The cards over the chat leave their badges, as when the file view closes.
    if panel_was_open && !file_view_open(state, agent) {
        state.ui.badge = None;
    }
}

/// The header's button and its shortcut, for the selected agent. In the file view, a file shown
/// over the terminal's tab gives it its place first.
pub fn toggle_pane(state: &mut AppState) -> Vec<Outgoing> {
    let Some(agent) = state.ui.selected else {
        return Vec::new();
    };
    if in_file_view(state, agent) && !file_view_shows_terminal(state, agent) {
        show_tab(state, agent);
    } else if state.ui.terminal_panes.contains(&agent) {
        hide(state, agent);
    } else {
        state.ui.terminal_panes.insert(agent);
        show_tab(state, agent);
    }
    Vec::new()
}

pub fn move_to(state: &mut AppState, place: TerminalPlace) {
    let Some(agent) = state.ui.selected else {
        state.prefs.terminal.place = place;
        return;
    };
    let panel_was_open = file_view_open(state, agent);
    state.prefs.terminal.place = place;
    state.ui.terminal_panes.insert(agent);
    show_tab(state, agent);
    // A terminal that leaves the file view alone in it closes it, and its badges go.
    if panel_was_open && !file_view_open(state, agent) {
        state.ui.badge = None;
    }
}

/// While the pane shows, the selected agent's terminals are listed, and an agent with none gets
/// one.
pub fn needed_requests(state: &mut AppState) -> Vec<Outgoing> {
    if !pane_open(state) {
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
        Listing::Listed
            if terminals.tabs.is_empty() && !terminals.opening && !terminals.open_failed =>
        {
            open(state, agent)
        }
        Listing::Asking | Listing::Listed => Vec::new(),
    }
}

/// The tab bar's "+".
pub fn open(state: &mut AppState, agent: AgentId) -> Vec<Outgoing> {
    let Some(terminals) = terminals_mut(state, agent) else {
        return Vec::new();
    };
    terminals.opening = true;
    terminals.open_failed = false;
    vec![outgoing(
        Request::AgentTerminalOpen(TerminalOpenParams {
            agent,
            size: OPENING_SIZE,
        }),
        Purpose::TerminalOpened { agent },
    )]
}

/// `agent.terminals` answered. A failure leaves the pane empty, with no retry.
pub fn listed(state: &mut AppState, agent: AgentId, listed: Option<Vec<TerminalInfo>>) {
    let Some(terminals) = terminals_mut(state, agent) else {
        return;
    };
    terminals.listing = Listing::Listed;
    let Some(listed) = listed else {
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
    terminals.open_failed = opened.is_none();
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
                tab.exited = Some(TerminalExit { code });
            }
        }
        TerminalChange::Closed => terminals.remove(update.terminal),
        // Output goes to the page, not to the state.
        TerminalChange::Output { .. } => {}
    }
}

/// A connection to `machine`'s daemon came. It may be a new daemon, whose terminals reuse the old
/// ids, and the terminal updates sent while the app was away are lost: the agents' terminals are
/// listed again.
pub fn connected(state: &mut AppState, machine: MachineId) {
    for (agent, entry) in &mut state.agents {
        if MachineId::of(*agent) == machine {
            entry.terminals = AgentTerminals::default();
        }
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
        hide(state, agent);
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
        Purpose::TerminalKeys,
    )
}

pub fn resize(agent: AgentId, terminal: TerminalId, size: TerminalSize) -> Outgoing {
    outgoing(
        Request::TerminalResize(TerminalResizeParams {
            agent,
            terminal,
            size,
        }),
        Purpose::TerminalKeys,
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
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use cockpit_client::RequestError;
    use cockpit_protocol::{AgentStatus, Catchup, Method};

    use super::*;
    use crate::state::find::{self, FindPane};
    use crate::state::tab_menu::{self, TabGroup};
    use crate::state::{
        Block, Drafts, TabMenuOrigin, actions, apply_response,
        apply_update as apply_session_update, connected_update, state_with, summary,
    };

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
        listed(&mut state, agent(), Some(Vec::new()));
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

        listed(&mut state, agent(), Some(vec![tab(2), tab(3), tab(4)]));
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
        listed(&mut state, agent(), Some(vec![tab(2)]));

        close(&mut state, agent(), TerminalId::new(2));

        assert!(!pane_open(&state));
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
        assert_eq!(
            terminals.tabs[0].exited,
            Some(TerminalExit { code: Some(2) })
        );
    }

    #[test]
    fn a_failed_opening_waits_for_the_user_to_ask_again() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        toggle_pane(&mut state);
        needed_requests(&mut state);
        listed(&mut state, agent(), Some(Vec::new()));
        needed_requests(&mut state);

        opened(&mut state, agent(), None);
        let after_failure = needed_requests(&mut state);
        let asked = open(&mut state, agent());

        assert!(after_failure.is_empty(), "{after_failure:?}");
        assert_eq!(asked.len(), 1);
    }

    #[test]
    fn a_new_connection_lists_the_terminals_again() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        toggle_pane(&mut state);
        needed_requests(&mut state);
        listed(&mut state, agent(), Some(vec![tab(2), tab(3)]));

        apply_session_update(
            &mut state,
            &mut Drafts::default(),
            connected_update(Catchup::Replay),
            Instant::now(),
            MachineId::THIS,
        );
        let relist = needed_requests(&mut state);

        assert!(tabs(&state).is_empty());
        assert!(matches!(
            relist.as_slice(),
            [Outgoing {
                request: Request::AgentTerminals(_),
                ..
            }]
        ));
    }

    #[test]
    fn the_page_keeps_the_terminals_of_every_agent_and_no_other() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        listed(&mut state, agent(), Some(vec![tab(2), tab(3)]));
        listed(&mut state, AgentId::new(2), Some(vec![tab(4)]));

        close(&mut state, agent(), TerminalId::new(2));

        assert_eq!(
            live(&state),
            [
                TerminalParams {
                    agent: agent(),
                    terminal: TerminalId::new(3),
                },
                TerminalParams {
                    agent: AgentId::new(2),
                    terminal: TerminalId::new(4),
                },
            ]
        );
    }

    #[test]
    fn a_refused_key_leaves_no_note() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let key = input(agent(), TerminalId::new(2), "l".to_owned());

        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::TerminalInput,
            key.purpose,
            Err(RequestError::NotConnected),
            MachineId::THIS,
        );

        assert!(state.ui.errors.is_empty());
    }

    /// The terminal is a tab of the file view, which shows `a.rs` and `b.rs`.
    fn in_file_view_with_two_files() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        state.prefs.terminal.place = TerminalPlace::FileView;
        actions::open_file(&mut state, agent(), PathBuf::from("a.rs"), None);
        actions::open_file(&mut state, agent(), PathBuf::from("b.rs"), None);
        state
    }

    fn shows_terminal(state: &AppState) -> bool {
        file_view_shows_terminal(state, agent())
    }

    #[test]
    fn in_the_file_view_the_shortcut_brings_the_terminal_over_the_file_before_it_hides_it() {
        let mut state = in_file_view_with_two_files();

        toggle_pane(&mut state);
        let opened = shows_terminal(&state);
        actions::open_file(&mut state, agent(), PathBuf::from("a.rs"), None);
        let file_over_it = (in_file_view(&state, agent()), shows_terminal(&state));
        toggle_pane(&mut state);
        let brought = shows_terminal(&state);
        toggle_pane(&mut state);

        assert!(opened);
        assert_eq!(file_over_it, (true, false));
        assert!(brought);
        assert!(!in_file_view(&state, agent()));
        assert!(state.ui.file_view(agent()).is_some(), "the files stay");
    }

    #[test]
    fn closing_files_behind_the_terminal_leaves_it_shown() {
        let mut state = in_file_view_with_two_files();
        toggle_pane(&mut state);

        actions::close_tab(&mut state, agent(), Path::new("b.rs"));
        let one_file_left = shows_terminal(&state);
        actions::close_tab(&mut state, agent(), Path::new("a.rs"));

        assert!(one_file_left);
        assert!(state.ui.file_view(agent()).is_none());
        assert!(
            shows_terminal(&state),
            "the terminal alone keeps the file view"
        );
    }

    #[test]
    fn closing_the_file_view_hides_its_terminal_and_not_a_terminal_elsewhere() {
        let mut state = in_file_view_with_two_files();
        toggle_pane(&mut state);

        actions::close_file_panel(&mut state, agent());
        let in_view_hidden = !pane_open(&state);
        move_to(&mut state, TerminalPlace::Bottom);
        actions::open_file(&mut state, agent(), PathBuf::from("a.rs"), None);
        actions::close_file_panel(&mut state, agent());

        assert!(in_view_hidden);
        assert!(state.ui.file_view(agent()).is_none());
        assert!(pane_open(&state), "the bottom panel stays");
    }

    #[test]
    fn going_back_to_the_file_under_the_terminal_shows_it_as_it_was() {
        let mut state = in_file_view_with_two_files();
        if let Some(view) = state.ui.file_views.get_mut(&agent()) {
            view.line = Some(4);
        }
        toggle_pane(&mut state);

        actions::show_file(&mut state, agent(), PathBuf::from("b.rs"));

        assert!(!shows_terminal(&state));
        assert_eq!(state.ui.file_view(agent()).unwrap().line, Some(4));
    }

    #[test]
    fn the_tab_menu_closing_files_behind_the_terminal_leaves_it_shown() {
        let mut state = in_file_view_with_two_files();
        toggle_pane(&mut state);

        tab_menu::close(
            &mut state,
            &mut Drafts::default(),
            agent(),
            Path::new("a.rs"),
            TabGroup::Others,
            TabMenuOrigin::Tabs,
        );

        assert_eq!(state.ui.file_view(agent()).unwrap().path, Path::new("a.rs"));
        assert!(shows_terminal(&state));
    }

    #[test]
    fn the_find_over_the_terminal_is_the_chats() {
        let mut state = in_file_view_with_two_files();
        toggle_pane(&mut state);

        find::open(&mut state, &mut Drafts::default(), FindPane::File, "");

        assert!(!state.ui.file_view(agent()).unwrap().find);
        assert!(state.ui.chat_finds.contains(&agent()));
    }

    #[test]
    fn hiding_the_terminal_alone_in_the_file_view_closes_the_panel_and_its_badges() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        state.prefs.terminal.place = TerminalPlace::FileView;
        toggle_pane(&mut state);
        let opened = file_view_open(&state, agent());
        state.ui.badge = Some(Block::Tasks);

        toggle_pane(&mut state);

        assert!(opened);
        assert!(!file_view_open(&state, agent()));
        assert_eq!(state.ui.badge, None);
    }

    #[test]
    fn the_bottom_panel_leaves_no_terminal_tab_for_a_later_move_to_the_file_view() {
        let mut state = in_file_view_with_two_files();
        state.prefs.terminal.place = TerminalPlace::Bottom;
        toggle_pane(&mut state);

        state.prefs.terminal.place = TerminalPlace::FileView;

        assert!(in_file_view(&state, agent()));
        assert!(!shows_terminal(&state), "the file stays over it");
    }

    #[test]
    fn moving_the_terminal_alone_out_of_the_file_view_drops_its_badges() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        state.prefs.terminal.place = TerminalPlace::FileView;
        toggle_pane(&mut state);
        state.ui.badge = Some(Block::Tasks);

        move_to(&mut state, TerminalPlace::Bottom);

        assert!(!file_view_open(&state, agent()));
        assert!(pane_open(&state));
        assert_eq!(state.ui.badge, None);
    }

    #[test]
    fn the_file_under_the_terminal_is_not_the_shown_one_and_its_find_waits() {
        let mut state = in_file_view_with_two_files();
        find::open(&mut state, &mut Drafts::default(), FindPane::File, "");
        toggle_pane(&mut state);

        let shown_under = shown_file(&state, agent()).map(Path::to_path_buf);
        let open_under = find::is_open(&state, FindPane::File, agent());
        let closed_by_escape = find::close_on_escape(&mut state, FindPane::File);
        actions::show_file(&mut state, agent(), PathBuf::from("b.rs"));

        assert_eq!(shown_under, None);
        assert!(!open_under);
        assert!(!closed_by_escape);
        assert_eq!(shown_file(&state, agent()), Some(Path::new("b.rs")));
        assert!(
            find::is_open(&state, FindPane::File, agent()),
            "it finds again"
        );
    }
}
