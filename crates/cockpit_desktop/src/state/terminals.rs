//! Each agent's terminals: the docks they sit in and the one each dock shows, whether they show,
//! the shells a new terminal can run, and the requests that list, open and close them. The output
//! does not pass here: it goes from the connection to the page, see `terminal_feed`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cockpit_protocol::{
    AgentId, AgentParams, Request, ShellChoice, TerminalChange, TerminalExit, TerminalId,
    TerminalInfo, TerminalInputParams, TerminalOpenParams, TerminalParams, TerminalRenameParams,
    TerminalResizeParams, TerminalShareParams, TerminalSize, TerminalSizeAt, TerminalUpdate,
};

use super::app_state::AppState;
use strum::VariantArray;

use super::docks::{self, Dock, TabKey};
use super::requests::{Outgoing, Purpose};
use super::ui_state::{MenuSide, Popover};
use super::{blocks_view, terminal_menu};
use crate::machine::MachineId;
use crate::path_style::PathStyle;

/// The size a terminal opens with, before the pane measures itself.
const OPENING_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

/// The shell menu's frame and heading, in pixels, as the style sheet draws them.
const SHELL_MENU_FRAME: i32 = 36;
/// A row of the shell menu, in pixels: the height of `.compact-menu .menu-item`.
const SHELL_MENU_ROW: i32 = 28;
/// The room between the shell menu and its button, in pixels.
const SHELL_MENU_GAP: i32 = 4;
/// The room between the shell menu and the window's edge, in pixels.
const SHELL_MENU_MARGIN: i32 = 8;

/// An agent's terminals, as the pane shows them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentTerminals {
    /// In the order they opened.
    pub tabs: Vec<TerminalTab>,
    /// The terminal the bottom dock shows, when it sits there.
    pub shown: Option<TerminalId>,
    pub listing: Listing,
    /// An `agent.terminal_open` is on its way.
    pub opening: bool,
    /// The dock the terminal on its way opens in.
    pub opening_dock: Dock,
    /// The last opening failed: the pane waits for the "+" rather than trying again.
    pub open_failed: bool,
    /// The shells a terminal can run, the default one among them. Empty from a daemon that
    /// predates them.
    pub shells: Vec<ShellChoice>,
    /// The terminals this window opened whose kept output has not come yet.
    pub fresh: BTreeSet<TerminalId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTab {
    pub terminal: TerminalId,
    /// The shell's name, then the title the shell sets.
    pub title: String,
    /// `Some` once the shell ended.
    pub exited: Option<TerminalExit>,
    /// Whether the agent may read it, as the daemon says.
    pub shared: bool,
    /// The name the user gave the tab, which shows in place of `title`.
    pub name: Option<String>,
}

impl TerminalTab {
    /// What the tab shows: the user's name for it, else the shell's title.
    pub fn shown_title(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.title)
    }
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
    /// The sizes the terminal took over that output, oldest first. Empty from a daemon that
    /// predates them.
    pub sizes: Vec<TerminalSizeAt>,
    /// Whether this is the first screen of a terminal this window opened, whose output is all
    /// recent: a query in it may still wait for an answer, as Windows' pseudo console holds a
    /// starting shell until it learns where the cursor is. [`screen`] sets it.
    pub fresh: bool,
}

impl AgentTerminals {
    /// The shell the "+" opens, when the daemon names one.
    pub fn default_shell(&self) -> Option<&ShellChoice> {
        self.shells.iter().find(|shell| shell.default)
    }

    /// Whether the tab bar offers a choice of shell.
    pub fn offers_shells(&self) -> bool {
        self.shells.len() > 1
    }

    pub fn tab(&self, terminal: TerminalId) -> Option<&TerminalTab> {
        self.tabs.iter().find(|tab| tab.terminal == terminal)
    }

    fn tab_mut(&mut self, terminal: TerminalId) -> Option<&mut TerminalTab> {
        self.tabs.iter_mut().find(|tab| tab.terminal == terminal)
    }

    fn add(&mut self, terminal: TerminalId, title: String) {
        if self.tab_mut(terminal).is_none() {
            self.tabs.push(TerminalTab {
                terminal,
                title,
                exited: None,
                shared: false,
                name: None,
            });
        }
    }

    fn remove(&mut self, terminal: TerminalId) {
        let index = self.tabs.iter().position(|tab| tab.terminal == terminal);
        let Some(index) = index else {
            return;
        };
        self.tabs.remove(index);
        self.fresh.remove(&terminal);
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

/// Whether `terminal` is one the selected agent `agent` shows, in either dock.
pub fn shows(state: &AppState, agent: AgentId, terminal: TerminalId) -> bool {
    state.ui.selected == Some(agent)
        && (bottom_terminal(state, agent) == Some(terminal)
            || side_terminal(state, agent) == Some(terminal))
}

/// Whether the selected agent's terminals show in either dock, which lists them.
pub fn pane_open(state: &AppState) -> bool {
    state.ui.selected.is_some_and(|agent| {
        docks::terminals_shown(state, agent, Dock::Bottom)
            || docks::terminals_shown(state, agent, Dock::Side)
    })
}

/// Whether a terminal of the selected agent is on screen, or a dock that waits for one, which the
/// header's button shows.
pub fn on_screen(state: &AppState) -> bool {
    state
        .ui
        .selected
        .is_some_and(|agent| on_screen_for(state, agent))
}

fn on_screen_for(state: &AppState, agent: AgentId) -> bool {
    bottom_terminal(state, agent).is_some()
        || side_terminal(state, agent).is_some()
        || Dock::VARIANTS
            .iter()
            .any(|&dock| docks::terminals_shown(state, agent, dock) && waits_in(state, agent, dock))
}

/// Whether `dock` waits for a terminal: the agent's are being listed, or one opens there.
fn waits_in(state: &AppState, agent: AgentId, dock: Dock) -> bool {
    let Some(entry) = state.agents.get(&agent) else {
        return false;
    };
    let terminals = &entry.terminals;
    if terminals.opening {
        return terminals.opening_dock == dock;
    }
    terminals.listing != Listing::Listed && dock == docks::default_dock(state, agent)
}

/// Whether the bottom dock shows for `agent`: with files open, or when its terminals show and it
/// has one or waits for one.
pub fn bottom_open(state: &AppState, agent: AgentId) -> bool {
    state.ui.file_view_in(agent, Dock::Bottom).is_some() || bottom_terminals_open(state, agent)
}

fn bottom_terminals_open(state: &AppState, agent: AgentId) -> bool {
    docks::terminals_shown(state, agent, Dock::Bottom)
        && (!docks::terminals_in(state, agent, Dock::Bottom).is_empty()
            || waits_in(state, agent, Dock::Bottom))
}

/// Whether the bottom dock shows its file: the user picked a file tab there last, or no terminal
/// of the dock shows.
pub fn bottom_shows_file(state: &AppState, agent: AgentId) -> bool {
    state.ui.file_view_in(agent, Dock::Bottom).is_some()
        && (state.ui.bottom_picks_file.contains(&agent) || !bottom_terminals_open(state, agent))
}

/// The terminal the bottom dock shows, unless its file shows: the one picked last, else its first
/// terminal.
pub fn bottom_terminal(state: &AppState, agent: AgentId) -> Option<TerminalId> {
    if !docks::terminals_shown(state, agent, Dock::Bottom) || bottom_shows_file(state, agent) {
        return None;
    }
    let picked = state.agents.get(&agent)?.terminals.shown;
    let tabs = docks::tabs(state, agent, Dock::Bottom);
    picked
        .filter(|picked| tabs.contains(&TabKey::Terminal(*picked)))
        .or_else(|| first_terminal(&tabs))
}

fn first_terminal(tabs: &[TabKey]) -> Option<TerminalId> {
    tabs.iter().find_map(|tab| match tab {
        TabKey::Terminal(terminal) => Some(*terminal),
        TabKey::File(_) => None,
    })
}

/// The terminal the side dock shows over its files: the one picked last, or its first terminal
/// when no file is open. `None` while a file shows.
pub fn side_terminal(state: &AppState, agent: AgentId) -> Option<TerminalId> {
    let tabs = docks::tabs(state, agent, Dock::Side);
    let picked = state
        .ui
        .side_terminal
        .get(&agent)
        .copied()
        .filter(|picked| tabs.contains(&TabKey::Terminal(*picked)));
    if state.ui.file_view_in(agent, Dock::Side).is_some() {
        picked
    } else {
        picked.or_else(|| first_terminal(&tabs))
    }
}

/// The file shown in the dock the file actions act on, unless a terminal covers it.
pub fn shown_file(state: &AppState, agent: AgentId) -> Option<&Path> {
    let view = state.ui.file_view(agent)?;
    let covered = match state.ui.active_files_dock(agent) {
        Dock::Side => side_terminal(state, agent).is_some(),
        Dock::Bottom => !bottom_shows_file(state, agent),
    };
    (!covered).then_some(view.path.as_path())
}

/// Whether `agent`'s side dock shows: with a file open, or with a terminal that shows there.
pub fn side_open(state: &AppState, agent: AgentId) -> bool {
    state.ui.file_view_in(agent, Dock::Side).is_some()
        || (docks::terminals_shown(state, agent, Dock::Side)
            && (!docks::terminals_in(state, agent, Dock::Side).is_empty()
                || waits_in(state, agent, Dock::Side)))
}

/// Shows `terminal` in its dock, and that dock's terminals.
pub fn show(state: &mut AppState, agent: AgentId, terminal: TerminalId) {
    let dock = docks::terminal_dock(state, agent, terminal);
    state.ui.terminal_docks.insert((agent, dock));
    match dock {
        Dock::Bottom => {
            state.ui.bottom_picks_file.remove(&agent);
            if let Some(terminals) = terminals_mut(state, agent) {
                terminals.shown = Some(terminal);
            }
        }
        Dock::Side => {
            state.ui.side_terminal.insert(agent, terminal);
        }
    }
}

/// Hides the terminals of `agent`'s `dock`. Their shells keep running. With no terminal left on
/// screen, the other dock's terminals hide too, so that none opens on its own there later.
pub fn hide_dock(state: &mut AppState, agent: AgentId, dock: Dock) {
    following_badges(state, agent, |state| {
        state.ui.terminal_docks.remove(&(agent, dock));
        if dock == Dock::Side {
            state.ui.side_terminal.remove(&agent);
        }
        if !on_screen_for(state, agent)
            && docks::terminals_in(state, agent, dock.other()).is_empty()
        {
            state.ui.terminal_docks.remove(&(agent, dock.other()));
            state.ui.side_terminal_wanted.remove(&agent);
        }
    });
    if !pane_open(state) {
        close_shell_menu(state, |shown| shown == agent);
    }
}

/// Hides `agent`'s terminals in both docks. Their shells keep running.
pub fn hide(state: &mut AppState, agent: AgentId) {
    hide_dock(state, agent, Dock::Bottom);
    hide_dock(state, agent, Dock::Side);
}

/// Runs `change`, which may close the side dock. The chat then takes its width back, and a card
/// opened from a badge closes with the badges.
fn following_badges(state: &mut AppState, agent: AgentId, change: impl FnOnce(&mut AppState)) {
    let badges_before = blocks_view::shows_badges(state, agent);
    change(state);
    if badges_before && !blocks_view::shows_badges(state, agent) {
        state.ui.badge = None;
    }
}

/// The header's button and its shortcut, for the selected agent: hides the terminals on screen,
/// else shows them in both docks. A side dock that holds terminals shows one over its files.
pub fn toggle_pane(state: &mut AppState) -> Vec<Outgoing> {
    let Some(agent) = state.ui.selected else {
        return Vec::new();
    };
    if on_screen(state) {
        hide(state, agent);
        return Vec::new();
    }
    state.ui.terminal_docks.insert((agent, Dock::Bottom));
    state.ui.terminal_docks.insert((agent, Dock::Side));
    let picked = state.ui.side_terminal.get(&agent).copied();
    let side = docks::terminals_in(state, agent, Dock::Side);
    let shown = picked
        .filter(|picked| side.contains(picked))
        .or_else(|| side.first().copied());
    if let Some(terminal) = shown {
        state.ui.side_terminal.insert(agent, terminal);
    }
    // A bottom dock that holds terminals shows one over its file.
    if !docks::terminals_in(state, agent, Dock::Bottom).is_empty() {
        state.ui.bottom_picks_file.remove(&agent);
    }
    let listed = state
        .agents
        .get(&agent)
        .is_some_and(|entry| entry.terminals.listing == Listing::Listed);
    if !listed {
        state.ui.side_terminal_wanted.insert(agent);
    }
    Vec::new()
}

/// Moves `terminal` to `to`, before the tab `before` or at the end, and shows it there. The agent's
/// next terminals open in `to`, and so do the terminals of the agents that start later. The other
/// agents keep theirs where they are.
pub fn move_terminal(
    state: &mut AppState,
    agent: AgentId,
    terminal: TerminalId,
    to: Dock,
    before: Option<&TabKey>,
) {
    let from = docks::terminal_dock(state, agent, terminal);
    following_badges(state, agent, |state| {
        state.ui.terminal_docks.insert((agent, to));
        docks::move_tab(state, agent, &TabKey::Terminal(terminal), to, before);
        if from != to && state.ui.side_terminal.get(&agent) == Some(&terminal) {
            state.ui.side_terminal.remove(&agent);
        }
        show(state, agent, terminal);
    });
    if from == to {
        return;
    }
    let Some(machine) = state.daemon_machine(agent).map(str::to_owned) else {
        return;
    };
    let others: Vec<(String, AgentId)> = state
        .agents
        .keys()
        .filter(|&&other| other != agent)
        .filter_map(|&other| {
            let name = state.daemon_machine(other)?.to_owned();
            Some((name, MachineId::daemon_agent(other)))
        })
        .collect();
    for (name, other) in others {
        state.prefs.terminal.keep_place(&name, other);
    }
    state
        .prefs
        .terminal
        .set_agent(&machine, MachineId::daemon_agent(agent), to);
}

/// While terminals show, the selected agent's are listed, and an agent with none gets one, in the
/// dock it opens terminals in, else in the other dock that shows.
pub fn needed_requests(state: &mut AppState) -> Vec<Outgoing> {
    if !pane_open(state) {
        return Vec::new();
    }
    let Some(agent) = state.ui.selected else {
        return Vec::new();
    };
    let default = docks::default_dock(state, agent);
    let dock = if docks::terminals_shown(state, agent, default) {
        default
    } else {
        default.other()
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
            open(state, agent, None, dock)
        }
        Listing::Asking | Listing::Listed => Vec::new(),
    }
}

/// A dock's "+" with `None`, which opens the default shell, or a row of the shell menu. The new
/// terminal opens in `dock`.
pub fn open(
    state: &mut AppState,
    agent: AgentId,
    shell: Option<PathBuf>,
    dock: Dock,
) -> Vec<Outgoing> {
    state.ui.terminal_docks.insert((agent, dock));
    let Some(terminals) = terminals_mut(state, agent) else {
        return Vec::new();
    };
    terminals.opening = true;
    terminals.opening_dock = dock;
    terminals.open_failed = false;
    vec![outgoing(
        Request::AgentTerminalOpen(TerminalOpenParams {
            agent,
            size: OPENING_SIZE,
            shell,
        }),
        Purpose::TerminalOpened { agent },
    )]
}

/// What the shell menu shows, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellMenu {
    pub agent: AgentId,
    pub dock: Dock,
    pub x: i32,
    pub y: i32,
    pub side: MenuSide,
    pub shells: Vec<ShellChoice>,
}

/// The side of its button the shell menu of `agent` opens on: under the button when its rows fit
/// between the button's `bottom` and the window's, else above it when they fit between the
/// window's top and the button's `top`, else on the side with more room. A menu over its own
/// button would take the click that closes it as a pick.
pub fn shell_menu_side(
    state: &AppState,
    agent: AgentId,
    top: i32,
    bottom: i32,
    window_height: i32,
) -> MenuSide {
    let rows = state
        .agents
        .get(&agent)
        .map_or(0, |entry| entry.terminals.shells.len());
    let rows = i32::try_from(rows).unwrap_or(i32::MAX);
    let height = SHELL_MENU_FRAME.saturating_add(rows.saturating_mul(SHELL_MENU_ROW));
    let room = |from: i32, to: i32| {
        to.saturating_sub(from)
            .saturating_sub(SHELL_MENU_GAP)
            .saturating_sub(SHELL_MENU_MARGIN)
    };
    let below = room(bottom, window_height);
    let above = room(0, top);
    if height <= below || below >= above {
        MenuSide::Below
    } else {
        MenuSide::Above
    }
}

/// The button beside the "+": opens or closes the agent's shell menu on `side` of the button, at
/// its left edge `x` and its edge `y` on that side.
pub fn toggle_shell_menu(
    state: &mut AppState,
    agent: AgentId,
    dock: Dock,
    x: i32,
    y: i32,
    side: MenuSide,
) {
    let open = matches!(
        state.ui.popover,
        Some(Popover::ShellMenu { agent: shown, dock: from, .. }) if shown == agent && from == dock
    );
    state.ui.popover = (!open).then_some(Popover::ShellMenu {
        agent,
        dock,
        x,
        y,
        side,
    });
}

/// The open shell menu, while its agent offers a choice of shell.
pub fn shell_menu(state: &AppState) -> Option<ShellMenu> {
    let Some(Popover::ShellMenu {
        agent,
        dock,
        x,
        y,
        side,
    }) = state.ui.popover
    else {
        return None;
    };
    let terminals = &state.agents.get(&agent)?.terminals;
    terminals.offers_shells().then(|| ShellMenu {
        agent,
        dock,
        x,
        y,
        side,
        shells: terminals.shells.clone(),
    })
}

/// A row of the shell menu: opens a terminal with that shell.
pub fn pick_shell(
    state: &mut AppState,
    agent: AgentId,
    program: PathBuf,
    dock: Dock,
) -> Vec<Outgoing> {
    close_shell_menu(state, |shown| shown == agent);
    open(state, agent, Some(program), dock)
}

/// Drops a terminal that closed from the layout and from the side dock it showed in.
fn forget_terminal(state: &mut AppState, agent: AgentId, terminal: TerminalId) {
    let gone = TerminalParams { agent, terminal };
    state.ui.terminal_rename.take_if(|renamed| *renamed == gone);
    docks::forget_tab(state, agent, &TabKey::Terminal(terminal));
    if state.ui.side_terminal.get(&agent) == Some(&terminal) {
        state.ui.side_terminal.remove(&agent);
    }
}

/// Closes the shell menu of an agent that `gone` names: the menu would show over nothing, and its
/// backdrop would take the next click.
fn close_shell_menu(state: &mut AppState, gone: impl Fn(AgentId) -> bool) {
    state.ui.popover.take_if(|popover| match popover {
        Popover::ShellMenu { agent, .. } => gone(*agent),
        _ => false,
    });
}

/// What the shell menu shows of a program: its file's name on Windows, whose folders are long, and
/// its path elsewhere, where two shells may share a name.
pub fn program_label(program: &Path) -> String {
    let text = program.to_string_lossy();
    let style = PathStyle::of_text(&text);
    match style {
        PathStyle::Windows => style.file_name(&text).unwrap_or(&text).to_owned(),
        PathStyle::Unix => text.into_owned(),
    }
}

/// `agent.terminals` answered. A failure leaves the pane empty, with no retry.
pub fn listed(
    state: &mut AppState,
    agent: AgentId,
    listed: Option<Vec<TerminalInfo>>,
    shells: Vec<ShellChoice>,
) {
    let Some(terminals) = terminals_mut(state, agent) else {
        return;
    };
    terminals.listing = Listing::Listed;
    terminals.shells = shells;
    let Some(listed) = listed else {
        terminals.opening = true;
        return;
    };
    for info in listed {
        terminals.add(info.terminal, info.title);
        if let Some(tab) = terminals.tab_mut(info.terminal) {
            tab.exited = info.exited;
            tab.shared = info.shared;
            tab.name = info.name;
        }
    }
    if terminals.shown.is_none() {
        terminals.shown = terminals.tabs.last().map(|tab| tab.terminal);
    }
    let ids: BTreeSet<TerminalId> = terminals.tabs.iter().map(|tab| tab.terminal).collect();
    docks::keep_terminals(state, agent, &ids);
    docks::settle_terminals(state, agent);
    // Terminals the shortcut asked for while unlisted show in the side dock too, over its files.
    if state.ui.side_terminal_wanted.remove(&agent)
        && let Some(&last) = docks::terminals_in(state, agent, Dock::Side).last()
    {
        state.ui.side_terminal.insert(agent, last);
    }
}

/// `agent.terminal_open` answered: the pane shows the new terminal.
pub fn opened(state: &mut AppState, agent: AgentId, opened: Option<TerminalId>) {
    let Some(terminals) = terminals_mut(state, agent) else {
        return;
    };
    let dock = terminals.opening_dock;
    terminals.opening = false;
    terminals.open_failed = opened.is_none();
    if let Some(terminal) = opened {
        terminals.add(terminal, String::new());
        terminals.fresh.insert(terminal);
        docks::place_terminal(state, agent, terminal, dock);
        show(state, agent, terminal);
        // The new terminal takes the screen from the one a menu may be open on.
        terminal_menu::close_if(state, |spot| spot.agent == agent);
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
            docks::settle_terminals(state, update.agent);
        }
        TerminalChange::Exited { code } => {
            if let Some(tab) = terminals.tab_mut(update.terminal) {
                tab.exited = Some(TerminalExit { code });
            }
        }
        TerminalChange::Shared { shared } => {
            if let Some(tab) = terminals.tab_mut(update.terminal) {
                tab.shared = shared;
            }
        }
        TerminalChange::Renamed { name } => {
            if let Some(tab) = terminals.tab_mut(update.terminal) {
                tab.name = name;
            }
        }
        TerminalChange::Closed => {
            terminals.remove(update.terminal);
            forget_terminal(state, update.agent, update.terminal);
            terminal_menu::close_if(state, |spot| {
                spot.agent == update.agent && spot.terminal == update.terminal
            });
        }
        // Output goes to the page, not to the state.
        TerminalChange::Output { .. } => {}
    }
}

/// Keeps the places of the agents a snapshot of `machine` holds: the others closed while the app
/// was away.
pub fn keep_listed_places(state: &mut AppState, machine: MachineId) {
    let Some(known) = state.machines.get(machine) else {
        return;
    };
    let listed: BTreeSet<AgentId> = state
        .agents
        .keys()
        .filter(|&&agent| MachineId::of(agent) == machine)
        .map(|&agent| MachineId::daemon_agent(agent))
        .collect();
    state
        .prefs
        .terminal
        .keep_agents(&known.daemon.machine, |agent| listed.contains(&agent));
}

/// Drops the places of a machine's agents before the app forgets the machine.
pub fn forget_machine_places(state: &mut AppState, machine: MachineId) {
    if let Some(known) = state.machines.get(machine) {
        state.prefs.terminal.forget_machine(&known.daemon.machine);
    }
}

/// Drops the place of an agent the daemon removed.
pub fn forget_place(state: &mut AppState, agent: AgentId) {
    let Some(machine) = state.daemon_machine(agent).map(str::to_owned) else {
        return;
    };
    state
        .prefs
        .terminal
        .forget_agent(&machine, MachineId::daemon_agent(agent));
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
    terminal_menu::close_if(state, |spot| MachineId::of(spot.agent) == machine);
    close_shell_menu(state, |agent| MachineId::of(agent) == machine);
    state
        .ui
        .terminal_rename
        .take_if(|renamed| MachineId::of(renamed.agent) == machine);
    state.ui.popover.take_if(|popover| {
        matches!(popover, Popover::TerminalTabMenu { agent, .. } if MachineId::of(*agent) == machine)
    });
}

/// A tab's "×": ends its shell.
pub fn close(state: &mut AppState, agent: AgentId, terminal: TerminalId) -> Vec<Outgoing> {
    let Some(terminals) = terminals_mut(state, agent) else {
        return Vec::new();
    };
    terminals.remove(terminal);
    let last = terminals.tabs.is_empty();
    forget_terminal(state, agent, terminal);
    // The last tab closed: the terminals hide with it, rather than opening another.
    if last {
        hide(state, agent);
    }
    vec![outgoing(
        Request::TerminalClose(TerminalParams { agent, terminal }),
        Purpose::Action,
    )]
}

/// Lets the agent read the terminal, or stops it. The tab's marker follows the daemon's answer to
/// every window, this one included.
pub fn share(agent: AgentId, terminal: TerminalId, shared: bool) -> Outgoing {
    outgoing(
        Request::TerminalShare(TerminalShareParams {
            agent,
            terminal,
            shared,
        }),
        Purpose::Action,
    )
}

/// Puts the tab's title in a field, from its menu or a double-click on it.
pub fn start_rename(state: &mut AppState, agent: AgentId, terminal: TerminalId) {
    let exists =
        terminals_mut(state, agent).is_some_and(|terminals| terminals.tab(terminal).is_some());
    if exists {
        state.ui.popover = None;
        state.ui.terminal_rename = Some(TerminalParams { agent, terminal });
    }
}

/// Saves the name typed in the tab's field, when it differs from `opened_with`, the text the field
/// opened with. An empty name gives the tab back the shell's own title.
pub fn finish_rename(
    state: &mut AppState,
    agent: AgentId,
    terminal: TerminalId,
    opened_with: &str,
    typed: &str,
) -> Vec<Outgoing> {
    if state.ui.terminal_rename != Some(TerminalParams { agent, terminal }) {
        return Vec::new();
    }
    state.ui.terminal_rename = None;
    let Some(tab) = terminals_mut(state, agent).and_then(|terminals| terminals.tab(terminal))
    else {
        return Vec::new();
    };
    let typed = typed.trim();
    let name = (!typed.is_empty()).then(|| typed.to_owned());
    if typed == opened_with.trim() || (name.is_none() && tab.name.is_none()) {
        return Vec::new();
    }
    vec![outgoing(
        Request::TerminalRename(TerminalRenameParams {
            agent,
            terminal,
            name,
        }),
        Purpose::Action,
    )]
}

pub fn cancel_rename(state: &mut AppState) {
    state.ui.terminal_rename = None;
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

/// `terminal.attach` answered: the page writes it before the output that follows. Only the first
/// screen of a terminal this window opened is fresh.
pub fn screen(state: &mut AppState, mut screen: Screen) {
    screen.fresh = terminals_mut(state, screen.agent)
        .is_some_and(|terminals| terminals.fresh.remove(&screen.terminal));
    state.ui.terminal_screens.push(screen);
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use cockpit_client::RequestError;
    use cockpit_protocol::{AgentStatus, Catchup, CommitHash, Method, ResponseBody};

    use super::*;
    use crate::prefs::BlocksForm;
    use crate::state::find::{self, FindPane};
    use crate::state::tab_menu::{self, TabGroup};
    use crate::state::{
        Block, Drafts, TabMenuOrigin, Travel, actions, apply_response,
        apply_update as apply_session_update, connected_update, git, state_with, summary,
        terminal_tab_menu,
    };

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn tab(terminal: u32) -> TerminalInfo {
        TerminalInfo {
            terminal: TerminalId::new(terminal),
            title: "zsh".to_owned(),
            exited: None,
            shared: false,
            name: None,
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
        listed(&mut state, agent(), Some(Vec::new()), Vec::new());
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

        listed(
            &mut state,
            agent(),
            Some(vec![tab(2), tab(3), tab(4)]),
            Vec::new(),
        );
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
        listed(&mut state, agent(), Some(vec![tab(2)]), Vec::new());

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
        listed(&mut state, agent(), Some(Vec::new()), Vec::new());
        needed_requests(&mut state);

        opened(&mut state, agent(), None);
        let after_failure = needed_requests(&mut state);
        let asked = open(&mut state, agent(), None, Dock::Bottom);

        assert!(after_failure.is_empty(), "{after_failure:?}");
        assert_eq!(asked.len(), 1);
    }

    #[test]
    fn a_new_connection_lists_the_terminals_again() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        toggle_pane(&mut state);
        needed_requests(&mut state);
        listed(&mut state, agent(), Some(vec![tab(2), tab(3)]), Vec::new());

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

    fn shell(program: &str, default: bool) -> ShellChoice {
        ShellChoice {
            program: PathBuf::from(program),
            name: program.rsplit('/').next().unwrap().to_owned(),
            default,
        }
    }

    /// The selected agent's pane is open, and its daemon offers `shells`.
    fn pane_with_shells(shells: Vec<ShellChoice>) -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        toggle_pane(&mut state);
        needed_requests(&mut state);
        listed(&mut state, agent(), Some(vec![tab(2)]), shells);
        state
    }

    fn opened_shell(outgoing: &[Outgoing]) -> Option<Option<PathBuf>> {
        match outgoing {
            [
                Outgoing {
                    request: Request::AgentTerminalOpen(params),
                    ..
                },
            ] => Some(params.shell.clone()),
            _ => None,
        }
    }

    #[test]
    fn a_pick_in_the_shell_menu_opens_that_shell_and_the_plus_the_default() {
        let mut state = pane_with_shells(vec![
            shell("/usr/bin/bash", false),
            shell("/usr/bin/zsh", true),
        ]);
        let terminals = &state.agents[&agent()].terminals;
        let default = terminals.default_shell().map(|shell| shell.name.clone());

        toggle_shell_menu(&mut state, agent(), Dock::Bottom, 40, 60, MenuSide::Below);
        let menu = shell_menu(&state);
        let picked = pick_shell(
            &mut state,
            agent(),
            PathBuf::from("/usr/bin/bash"),
            Dock::Bottom,
        );
        let plus = open(&mut state, agent(), None, Dock::Bottom);

        assert_eq!(default.as_deref(), Some("zsh"));
        let menu = menu.expect("the menu shows");
        assert_eq!((menu.x, menu.y, menu.shells.len()), (40, 60, 2));
        assert_eq!(
            opened_shell(&picked),
            Some(Some(PathBuf::from("/usr/bin/bash")))
        );
        assert_eq!(state.ui.popover, None, "the pick closes the menu");
        assert_eq!(opened_shell(&plus), Some(None));
    }

    #[test]
    fn the_answer_to_the_listing_gives_the_pane_its_shells() {
        let shells = vec![shell("/usr/bin/bash", true), shell("/usr/bin/zsh", false)];
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        toggle_pane(&mut state);
        let list = needed_requests(&mut state);
        let [Outgoing { purpose, .. }] = list.as_slice() else {
            panic!("expected the listing, got {list:?}");
        };

        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentTerminals,
            purpose.clone(),
            Ok(ResponseBody::Terminals {
                terminals: vec![tab(2)],
                shells: shells.clone(),
            }),
            MachineId::THIS,
        );

        assert_eq!(state.agents[&agent()].terminals.shells, shells);
    }

    #[test]
    fn a_daemon_with_one_shell_or_none_offers_no_menu() {
        let mut one = pane_with_shells(vec![shell("/usr/bin/bash", true)]);
        let mut older = pane_with_shells(Vec::new());

        toggle_shell_menu(&mut one, agent(), Dock::Bottom, 40, 60, MenuSide::Below);
        toggle_shell_menu(&mut older, agent(), Dock::Bottom, 40, 60, MenuSide::Below);

        assert!(!one.agents[&agent()].terminals.offers_shells());
        assert!(!older.agents[&agent()].terminals.offers_shells());
        assert_eq!(shell_menu(&one), None);
        assert_eq!(shell_menu(&older), None);
    }

    /// A menu left open would show over nothing, and its backdrop would take the next click.
    #[test]
    fn hiding_the_pane_or_a_new_connection_closes_the_shell_menu() {
        let shells = vec![shell("/usr/bin/bash", true), shell("/usr/bin/zsh", false)];
        let mut hidden = pane_with_shells(shells.clone());
        let mut reconnected = pane_with_shells(shells);
        toggle_shell_menu(&mut hidden, agent(), Dock::Bottom, 40, 60, MenuSide::Below);
        toggle_shell_menu(
            &mut reconnected,
            agent(),
            Dock::Bottom,
            40,
            60,
            MenuSide::Below,
        );

        toggle_pane(&mut hidden);
        connected(&mut reconnected, MachineId::THIS);

        assert_eq!(hidden.ui.popover, None);
        assert_eq!(reconnected.ui.popover, None);
        assert!(reconnected.agents[&agent()].terminals.shells.is_empty());
    }

    /// A menu pushed over its own button would take the click that closes it as a pick.
    #[test]
    fn the_shell_menu_opens_above_its_button_when_its_rows_do_not_fit_under_it() {
        let three = pane_with_shells(vec![
            shell("/usr/bin/bash", true),
            shell("/usr/bin/zsh", false),
            shell("/usr/bin/fish", false),
        ]);
        // 36 px of frame and heading, and 28 px a row: 120 px.
        let side = |top, bottom, window| shell_menu_side(&three, agent(), top, bottom, window);

        let room_below = side(640, 666, 900);
        let just_enough = side(752, 768, 900);
        let short_pane = side(800, 820, 900);
        let no_room_anywhere = side(100, 116, 220);

        assert_eq!(room_below, MenuSide::Below);
        assert_eq!(just_enough, MenuSide::Below);
        assert_eq!(short_pane, MenuSide::Above);
        assert_eq!(no_room_anywhere, MenuSide::Below, "below has more room");
    }

    #[test]
    fn the_menu_shows_a_windows_programs_file_and_a_unix_programs_path() {
        assert_eq!(
            program_label(Path::new(r"C:\Program Files\PowerShell\7\pwsh.exe")),
            "pwsh.exe"
        );
        assert_eq!(
            program_label(Path::new("/opt/homebrew/bin/bash")),
            "/opt/homebrew/bin/bash"
        );
    }

    #[test]
    fn the_page_keeps_the_terminals_of_every_agent_and_no_other() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        listed(&mut state, agent(), Some(vec![tab(2), tab(3)]), Vec::new());
        listed(&mut state, AgentId::new(2), Some(vec![tab(4)]), Vec::new());

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

    /// The page writes each part of the kept output at the size the shell wrote it for.
    #[test]
    fn the_kept_output_goes_to_the_page_with_its_sizes() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let asked = attach(agent(), TerminalId::new(2));
        let sizes = vec![
            TerminalSizeAt {
                offset: 0,
                size: TerminalSize { cols: 80, rows: 24 },
            },
            TerminalSizeAt {
                offset: 6,
                size: TerminalSize {
                    cols: 160,
                    rows: 40,
                },
            },
        ];

        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::TerminalAttach,
            asked.purpose,
            Ok(ResponseBody::TerminalScreen {
                offset: 4,
                data: "bHMK".to_owned(),
                sizes: sizes.clone(),
            }),
            MachineId::THIS,
        );

        assert_eq!(
            state.ui.terminal_screens,
            [Screen {
                agent: agent(),
                terminal: TerminalId::new(2),
                offset: 4,
                data: "bHMK".to_owned(),
                sizes,
                fresh: false,
            }]
        );
    }

    fn screen_is_fresh(state: &mut AppState, terminal: u32) -> Option<bool> {
        apply_response(
            state,
            &mut Drafts::default(),
            Method::TerminalAttach,
            attach(agent(), TerminalId::new(terminal)).purpose,
            Ok(ResponseBody::TerminalScreen {
                offset: 0,
                data: "bHMK".to_owned(),
                sizes: Vec::new(),
            }),
            MachineId::THIS,
        );
        state.ui.terminal_screens.pop().map(|screen| screen.fresh)
    }

    /// The output of a terminal this window just opened is all recent: a query in it may wait for
    /// the page's answer. Any later screen of it, and another client's terminal, hold history.
    #[test]
    fn only_the_first_screen_of_a_terminal_this_window_opened_is_fresh() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        opened(&mut state, agent(), Some(TerminalId::new(2)));
        apply_update(
            &mut state,
            TerminalUpdate {
                agent: agent(),
                terminal: TerminalId::new(3),
                change: TerminalChange::Opened {
                    title: "fish".to_owned(),
                },
            },
        );

        assert_eq!(screen_is_fresh(&mut state, 2), Some(true));
        assert_eq!(screen_is_fresh(&mut state, 2), Some(false));
        assert_eq!(screen_is_fresh(&mut state, 3), Some(false));
    }

    /// The daemon may tell every client of the new terminal before it answers the window that
    /// opened it.
    #[test]
    fn a_terminal_this_window_opened_is_fresh_when_its_opening_event_comes_first() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        apply_update(
            &mut state,
            TerminalUpdate {
                agent: agent(),
                terminal: TerminalId::new(2),
                change: TerminalChange::Opened {
                    title: "pwsh".to_owned(),
                },
            },
        );
        opened(&mut state, agent(), Some(TerminalId::new(2)));

        assert_eq!(screen_is_fresh(&mut state, 2), Some(true));
    }

    /// A tab closed before its screen keeps no mark, and after a reconnect the output may be old.
    #[test]
    fn a_terminal_closed_or_reconnected_before_its_screen_is_not_fresh() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        opened(&mut state, agent(), Some(TerminalId::new(2)));
        opened(&mut state, agent(), Some(TerminalId::new(3)));

        close(&mut state, agent(), TerminalId::new(2));
        let fresh_after_close = state.agents[&agent()].terminals.fresh.clone();
        connected(&mut state, MachineId::THIS);
        listed(&mut state, agent(), Some(vec![tab(3)]), Vec::new());

        assert_eq!(fresh_after_close, BTreeSet::from([TerminalId::new(3)]));
        assert_eq!(screen_is_fresh(&mut state, 3), Some(false));
    }

    /// The file view shows `a.rs` and `b.rs`, and the agent's terminal 1 sits in the side dock.
    fn in_file_view_with_two_files() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        state.prefs.terminal.place = Dock::Side;
        actions::open_file(&mut state, agent(), PathBuf::from("a.rs"), None);
        actions::open_file(&mut state, agent(), PathBuf::from("b.rs"), None);
        listed(&mut state, agent(), Some(vec![tab(1)]), Vec::new());
        state
    }

    /// The agent shows no file, and its terminal 1 sits in the side dock.
    fn alone_in_the_side_dock() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        state.prefs.terminal.place = Dock::Side;
        listed(&mut state, agent(), Some(vec![tab(1)]), Vec::new());
        state
    }

    fn shows_terminal(state: &AppState) -> bool {
        side_terminal(state, agent()).is_some()
    }

    fn shown_path(state: &AppState) -> Option<PathBuf> {
        shown_file(state, agent()).map(Path::to_path_buf)
    }

    #[test]
    fn back_over_the_terminal_shows_the_file_under_it_first() {
        let mut state = in_file_view_with_two_files();
        toggle_pane(&mut state);

        actions::travel_files(&mut state, agent(), Travel::Back);
        let first = shown_path(&state);
        actions::travel_files(&mut state, agent(), Travel::Back);
        let second = shown_path(&state);

        assert_eq!(first, Some(PathBuf::from("b.rs")));
        assert_eq!(second, Some(PathBuf::from("a.rs")));
    }

    #[test]
    fn back_after_the_terminal_left_the_side_dock_goes_to_the_file_before() {
        let mut state = in_file_view_with_two_files();
        toggle_pane(&mut state);
        move_terminal(&mut state, agent(), TerminalId::new(1), Dock::Bottom, None);

        actions::travel_files(&mut state, agent(), Travel::Back);

        assert_eq!(shown_path(&state), Some(PathBuf::from("a.rs")));
    }

    /// Forward to a commit of the file under the terminal shows that file, at the commit.
    #[test]
    fn forward_within_the_file_under_the_terminal_shows_the_file() {
        let mut state = in_file_view_with_two_files();
        let hash = CommitHash::new(&"a1".repeat(20)).unwrap();
        git::show_commit(&mut state, agent(), Some(hash));
        actions::travel_files(&mut state, agent(), Travel::Back);
        toggle_pane(&mut state);

        actions::travel_files(&mut state, agent(), Travel::Forward);

        assert_eq!(shown_path(&state), Some(PathBuf::from("b.rs")));
    }

    #[test]
    fn in_the_side_dock_the_shortcut_brings_the_terminal_over_the_file_before_it_hides_it() {
        let mut state = in_file_view_with_two_files();

        toggle_pane(&mut state);
        let opened = shows_terminal(&state);
        actions::open_file(&mut state, agent(), PathBuf::from("a.rs"), None);
        let file_over_it = (
            docks::terminals_shown(&state, agent(), Dock::Side),
            shows_terminal(&state),
        );
        toggle_pane(&mut state);
        let brought = shows_terminal(&state);
        toggle_pane(&mut state);

        assert!(opened);
        assert_eq!(file_over_it, (true, false));
        assert!(brought);
        assert!(!docks::terminals_shown(&state, agent(), Dock::Side));
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
            "the terminal alone keeps the side dock"
        );
    }

    #[test]
    fn closing_the_side_dock_hides_its_terminal_and_not_the_bottom_docks() {
        let mut state = in_file_view_with_two_files();
        toggle_pane(&mut state);

        actions::close_file_panel(&mut state, agent());
        let side_hidden = !docks::terminals_shown(&state, agent(), Dock::Side);
        move_terminal(&mut state, agent(), TerminalId::new(1), Dock::Bottom, None);
        actions::open_file(&mut state, agent(), PathBuf::from("a.rs"), None);
        actions::close_file_panel(&mut state, agent());

        assert!(side_hidden);
        assert!(state.ui.file_view(agent()).is_none());
        assert!(bottom_open(&state, agent()), "the bottom dock stays");
    }

    #[test]
    fn going_back_to_the_file_under_the_terminal_shows_it_as_it_was() {
        let mut state = in_file_view_with_two_files();
        if let Some(view) = state.ui.file_view_mut(agent()) {
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
    fn hiding_the_terminal_alone_in_the_side_dock_closes_it_and_its_badges() {
        let mut state = alone_in_the_side_dock();
        toggle_pane(&mut state);
        let opened = side_open(&state, agent());
        state.ui.badge = Some(Block::Tasks);

        toggle_pane(&mut state);

        assert!(opened);
        assert!(!side_open(&state, agent()));
        assert_eq!(state.ui.badge, None);
    }

    #[test]
    fn hiding_the_terminal_keeps_the_badges_card_while_the_user_keeps_the_blocks_folded() {
        let mut state = alone_in_the_side_dock();
        state.prefs.blocks.form = BlocksForm::Badges;
        toggle_pane(&mut state);
        state.ui.badge = Some(Block::Tasks);

        toggle_pane(&mut state);

        assert!(!side_open(&state, agent()));
        assert_eq!(state.ui.badge, Some(Block::Tasks));
    }

    #[test]
    fn a_terminal_moved_to_the_side_dock_sits_among_the_files_and_shows() {
        let mut state = in_file_view_with_two_files();
        state.prefs.terminal.place = Dock::Bottom;
        state.ui.layouts.clear();
        toggle_pane(&mut state);
        let bottom_before = bottom_open(&state, agent());

        move_terminal(
            &mut state,
            agent(),
            TerminalId::new(1),
            Dock::Side,
            Some(&TabKey::File(PathBuf::from("b.rs"))),
        );

        assert!(bottom_before);
        assert_eq!(
            docks::tabs(&state, agent(), Dock::Side),
            [
                TabKey::File(PathBuf::from("a.rs")),
                TabKey::Terminal(TerminalId::new(1)),
                TabKey::File(PathBuf::from("b.rs")),
            ]
        );
        assert!(shows_terminal(&state));
        assert!(
            !bottom_open(&state, agent()),
            "the bottom dock has no terminal left"
        );
    }

    #[test]
    fn each_agent_keeps_its_terminals_dock_and_a_later_agent_takes_the_last_one() {
        let other = AgentId::new(2);
        let later = AgentId::new(3);
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        state.ui.selected = Some(agent());
        listed(&mut state, agent(), Some(vec![tab(1)]), Vec::new());
        listed(&mut state, other, Some(vec![tab(2)]), Vec::new());

        move_terminal(&mut state, agent(), TerminalId::new(1), Dock::Side, None);

        assert_eq!(docks::default_dock(&state, agent()), Dock::Side);
        assert_eq!(
            docks::default_dock(&state, other),
            Dock::Bottom,
            "the other agent's stays"
        );
        assert_eq!(docks::default_dock(&state, later), Dock::Side);
        assert_eq!(
            docks::terminal_dock(&state, other, TerminalId::new(2)),
            Dock::Bottom
        );
    }

    #[test]
    fn a_removed_agent_leaves_no_terminal_dock_behind() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        listed(&mut state, agent(), Some(vec![tab(1)]), Vec::new());
        move_terminal(&mut state, agent(), TerminalId::new(1), Dock::Side, None);

        forget_place(&mut state, agent());

        assert!(state.prefs.terminal.agents.is_empty());
    }

    #[test]
    fn moving_the_terminal_alone_out_of_the_side_dock_drops_its_badges() {
        let mut state = alone_in_the_side_dock();
        toggle_pane(&mut state);
        state.ui.badge = Some(Block::Tasks);

        move_terminal(&mut state, agent(), TerminalId::new(1), Dock::Bottom, None);

        assert!(!side_open(&state, agent()));
        assert!(bottom_open(&state, agent()));
        assert_eq!(state.ui.badge, None);
    }

    #[test]
    fn a_closed_terminal_leaves_its_dock_and_its_pin() {
        let mut state = alone_in_the_side_dock();
        toggle_pane(&mut state);
        docks::toggle_pin(&mut state, agent(), &TabKey::Terminal(TerminalId::new(1)));

        close(&mut state, agent(), TerminalId::new(1));

        assert!(state.ui.layouts.is_empty());
        assert_eq!(state.ui.side_terminal.get(&agent()), None);
    }

    #[test]
    fn moving_one_terminal_leaves_the_others_where_they_were() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        toggle_pane(&mut state);
        needed_requests(&mut state);
        listed(&mut state, agent(), Some(vec![tab(1), tab(2)]), Vec::new());

        move_terminal(&mut state, agent(), TerminalId::new(1), Dock::Side, None);

        assert_eq!(
            docks::terminal_dock(&state, agent(), TerminalId::new(2)),
            Dock::Bottom
        );
        assert!(bottom_open(&state, agent()));
    }

    #[test]
    fn the_shortcut_hides_a_dock_that_waits_for_its_terminals() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        toggle_pane(&mut state);
        needed_requests(&mut state);
        let waiting = bottom_open(&state, agent());

        toggle_pane(&mut state);

        assert!(waiting);
        assert!(!pane_open(&state));
    }

    #[test]
    fn no_terminal_opens_once_the_user_hid_the_one_dock_on_screen() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        toggle_pane(&mut state);
        needed_requests(&mut state);
        listed(&mut state, agent(), Some(vec![tab(1)]), Vec::new());

        hide_dock(&mut state, agent(), Dock::Bottom);
        connected(&mut state, MachineId::THIS);
        let asked = needed_requests(&mut state);

        assert!(!pane_open(&state));
        assert!(asked.is_empty(), "{asked:?}");
    }

    #[test]
    fn a_new_listing_leaves_the_file_the_user_picked_over_the_side_terminal() {
        let mut state = in_file_view_with_two_files();
        toggle_pane(&mut state);
        actions::show_file(&mut state, agent(), PathBuf::from("b.rs"));

        connected(&mut state, MachineId::THIS);
        needed_requests(&mut state);
        listed(&mut state, agent(), Some(vec![tab(1)]), Vec::new());

        assert!(!shows_terminal(&state));
        assert_eq!(shown_path(&state), Some(PathBuf::from("b.rs")));
    }

    #[test]
    fn a_file_moved_while_the_side_terminals_hide_leaves_their_place() {
        let mut state = in_file_view_with_two_files();
        actions::open_file(&mut state, agent(), PathBuf::from("c.rs"), None);
        toggle_pane(&mut state);
        let a = TabKey::File(PathBuf::from("a.rs"));
        move_terminal(
            &mut state,
            agent(),
            TerminalId::new(1),
            Dock::Side,
            Some(&a),
        );
        hide(&mut state, agent());

        docks::move_tab(
            &mut state,
            agent(),
            &TabKey::File(PathBuf::from("c.rs")),
            Dock::Side,
            Some(&TabKey::File(PathBuf::from("b.rs"))),
        );
        toggle_pane(&mut state);

        assert_eq!(
            docks::tabs(&state, agent(), Dock::Side),
            [
                TabKey::Terminal(TerminalId::new(1)),
                a,
                TabKey::File(PathBuf::from("c.rs")),
                TabKey::File(PathBuf::from("b.rs")),
            ]
        );
    }

    /// Both files of `in_file_view_with_two_files` in the bottom dock, the terminal over the side.
    fn files_at_the_bottom_terminal_at_the_side() -> AppState {
        let mut state = in_file_view_with_two_files();
        toggle_pane(&mut state);
        actions::move_file(&mut state, agent(), Path::new("b.rs"), Dock::Bottom, None);
        actions::move_file(&mut state, agent(), Path::new("a.rs"), Dock::Bottom, None);
        state
    }

    #[test]
    fn the_side_terminal_shows_once_its_files_moved_to_the_bottom() {
        let mut state = files_at_the_bottom_terminal_at_the_side();
        state.ui.side_terminal.clear();

        assert_eq!(side_terminal(&state, agent()), Some(TerminalId::new(1)));
    }

    #[test]
    fn back_in_the_bottom_dock_goes_back_there_while_the_side_shows_a_terminal() {
        let mut state = files_at_the_bottom_terminal_at_the_side();
        state.ui.pressed_dock.insert(agent(), Dock::Bottom);

        actions::travel_files(&mut state, agent(), Travel::Back);

        let bottom = state.ui.file_view_in(agent(), Dock::Bottom).unwrap();
        assert_eq!(bottom.path, PathBuf::from("b.rs"));
        assert_eq!(side_terminal(&state, agent()), Some(TerminalId::new(1)));
    }

    #[test]
    fn ctrl_f_in_the_bottom_file_finds_there_while_the_side_shows_a_terminal() {
        let mut state = files_at_the_bottom_terminal_at_the_side();
        state.ui.pressed_dock.insert(agent(), Dock::Bottom);

        find::open(&mut state, &mut Drafts::default(), FindPane::File, "");

        assert!(state.ui.file_view_in(agent(), Dock::Bottom).unwrap().find);
        assert!(find::is_open(&state, FindPane::File, agent()));
        assert!(!state.ui.chat_finds.contains(&agent()));
    }

    #[test]
    fn the_shortcut_brings_the_bottom_terminal_over_its_file() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        listed(&mut state, agent(), Some(vec![tab(1)]), Vec::new());
        toggle_pane(&mut state);
        actions::open_file(&mut state, agent(), PathBuf::from("a.rs"), None);
        actions::move_file(&mut state, agent(), Path::new("a.rs"), Dock::Bottom, None);
        let covered = bottom_terminal(&state, agent());

        hide_dock(&mut state, agent(), Dock::Side);
        let kept = docks::terminals_shown(&state, agent(), Dock::Bottom);
        toggle_pane(&mut state);

        assert_eq!(covered, None);
        assert!(kept, "hiding the side leaves the bottom's terminals");
        assert_eq!(bottom_terminal(&state, agent()), Some(TerminalId::new(1)));
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

    /// Agent 1 with terminal 5, "zsh", listed shared and named "tests" by the daemon, and
    /// terminal 6, plain.
    fn named_and_shared() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let named = TerminalInfo {
            shared: true,
            name: Some("tests".to_owned()),
            ..tab(5)
        };
        listed(&mut state, agent(), Some(vec![named, tab(6)]), Vec::new());
        state
    }

    fn terminal_tab(state: &AppState, terminal: u32) -> TerminalTab {
        state.agents[&agent()]
            .terminals
            .tab(TerminalId::new(terminal))
            .cloned()
            .unwrap()
    }

    fn requests(sent: Vec<Outgoing>) -> Vec<Request> {
        sent.into_iter().map(|outgoing| outgoing.request).collect()
    }

    fn renamed(terminal: u32, name: Option<&str>) -> Request {
        Request::TerminalRename(TerminalRenameParams {
            agent: agent(),
            terminal: TerminalId::new(terminal),
            name: name.map(str::to_owned),
        })
    }

    #[test]
    fn the_listing_and_the_updates_carry_the_share_and_the_name() {
        let mut state = named_and_shared();
        let listed_named = terminal_tab(&state, 5);
        let update = |change| TerminalUpdate {
            agent: agent(),
            terminal: TerminalId::new(6),
            change,
        };

        apply_update(&mut state, update(TerminalChange::Shared { shared: true }));
        apply_update(
            &mut state,
            update(TerminalChange::Renamed {
                name: Some("build".to_owned()),
            }),
        );
        let changed = terminal_tab(&state, 6);
        apply_update(&mut state, update(TerminalChange::Shared { shared: false }));
        apply_update(&mut state, update(TerminalChange::Renamed { name: None }));
        let back = terminal_tab(&state, 6);

        assert!(listed_named.shared);
        assert_eq!(listed_named.shown_title(), "tests");
        assert!(changed.shared);
        assert_eq!(changed.shown_title(), "build");
        assert!(!back.shared);
        assert_eq!(back.shown_title(), "zsh");
    }

    /// Enter and a blur both finish the edit; the second finish of one edit sends nothing.
    #[test]
    fn a_rename_sends_the_new_name_once_and_ends_the_edit() {
        let mut state = named_and_shared();

        start_rename(&mut state, agent(), TerminalId::new(6));
        let editing = state.ui.terminal_rename;
        let sent = finish_rename(&mut state, agent(), TerminalId::new(6), "zsh", "  build ");
        let blurred_after = finish_rename(&mut state, agent(), TerminalId::new(6), "zsh", "other");

        assert_eq!(
            editing,
            Some(TerminalParams {
                agent: agent(),
                terminal: TerminalId::new(6),
            })
        );
        assert_eq!(requests(sent), [renamed(6, Some("build"))]);
        assert_eq!(state.ui.terminal_rename, None);
        assert!(blurred_after.is_empty());
    }

    #[test]
    fn an_empty_name_gives_back_the_shells_title_and_an_unchanged_one_sends_nothing() {
        let mut state = named_and_shared();

        start_rename(&mut state, agent(), TerminalId::new(5));
        let emptied = finish_rename(&mut state, agent(), TerminalId::new(5), "tests", "   ");
        start_rename(&mut state, agent(), TerminalId::new(5));
        let same = finish_rename(&mut state, agent(), TerminalId::new(5), "tests", "tests");
        start_rename(&mut state, agent(), TerminalId::new(6));
        let unnamed_and_empty = finish_rename(&mut state, agent(), TerminalId::new(6), "zsh", "");
        start_rename(&mut state, agent(), TerminalId::new(6));
        let shell_title = finish_rename(&mut state, agent(), TerminalId::new(6), "zsh", "zsh");

        assert_eq!(requests(emptied), [renamed(5, None)]);
        assert!(same.is_empty());
        assert!(unnamed_and_empty.is_empty());
        assert!(shell_title.is_empty());
    }

    /// The field keeps the text it opened with while the shell sets another title, and shows
    /// "shell" for a shell without one: an untouched field names the tab with neither.
    #[test]
    fn an_untouched_field_sends_nothing_once_the_shells_title_changed() {
        let mut state = named_and_shared();
        set_title(&mut state, agent(), TerminalId::new(5), String::new());

        start_rename(&mut state, agent(), TerminalId::new(6));
        set_title(&mut state, agent(), TerminalId::new(6), "vim".to_owned());
        let retitled = finish_rename(&mut state, agent(), TerminalId::new(6), "zsh", "zsh");
        start_rename(&mut state, agent(), TerminalId::new(5));
        cancel_rename(&mut state);
        apply_update(
            &mut state,
            TerminalUpdate {
                agent: agent(),
                terminal: TerminalId::new(5),
                change: TerminalChange::Renamed { name: None },
            },
        );
        start_rename(&mut state, agent(), TerminalId::new(5));
        let untitled = finish_rename(&mut state, agent(), TerminalId::new(5), "shell", "shell");

        assert!(retitled.is_empty());
        assert!(untitled.is_empty());
    }

    /// Esc cancels: the blur that follows as the field leaves saves nothing. Esc reaches the
    /// overlays too, which also end the edit.
    #[test]
    fn escape_cancels_the_rename() {
        let mut state = named_and_shared();

        start_rename(&mut state, agent(), TerminalId::new(6));
        cancel_rename(&mut state);
        let blurred = finish_rename(&mut state, agent(), TerminalId::new(6), "zsh", "build");
        start_rename(&mut state, agent(), TerminalId::new(6));
        let closed = state.ui.close_overlays();

        assert!(blurred.is_empty());
        assert!(closed);
        assert_eq!(state.ui.terminal_rename, None);
    }

    /// After a reconnection the tabs are listed again, and a new daemon reuses the ids: a field
    /// or a tab's menu left open would come back on another terminal's tab, whose menu would then
    /// share a terminal the user did not pick.
    #[test]
    fn a_reconnection_ends_the_rename_and_closes_the_tabs_menu() {
        let mut state = named_and_shared();

        start_rename(&mut state, agent(), TerminalId::new(6));
        connected(&mut state, MachineId::THIS);
        let renamed = state.ui.terminal_rename;
        terminal_tab_menu::open(&mut state, agent(), TerminalId::new(5), 10, 20);
        connected(&mut state, MachineId::THIS);

        assert_eq!(renamed, None);
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn a_closed_terminal_ends_its_rename_and_a_gone_one_starts_none() {
        let mut state = named_and_shared();

        start_rename(&mut state, agent(), TerminalId::new(9));
        let gone = state.ui.terminal_rename;
        start_rename(&mut state, agent(), TerminalId::new(6));
        apply_update(
            &mut state,
            TerminalUpdate {
                agent: agent(),
                terminal: TerminalId::new(6),
                change: TerminalChange::Closed,
            },
        );

        assert_eq!(gone, None);
        assert_eq!(state.ui.terminal_rename, None);
    }
}
