//! The tabs of an agent's two docks: which dock each terminal sits in, the order the user drags
//! the tabs into, and the pinned tabs, which come first. A tab the user never moved keeps its
//! place after the moved ones, in the order it came.
//!
//! The side dock holds the file view's files and the terminals moved there. The bottom dock holds
//! the other terminals.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cockpit_protocol::{AgentId, TerminalId};
use serde::{Deserialize, Serialize};

use super::app_state::AppState;
use crate::machine::MachineId;
pub use crate::prefs::Dock;

/// A tab, as the order and the pins name it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabKey {
    /// A file, as the file view names it: relative to the agent's folder when inside it.
    File(PathBuf),
    Terminal(TerminalId),
}

// The prefixes of the keys the page names tabs by.
const FILE_KEY_PREFIX: &str = "file:";
const TERMINAL_KEY_PREFIX: &str = "terminal:";

impl TabKey {
    /// The key the page's drag script hands back.
    pub fn to_page(&self) -> String {
        match self {
            Self::File(path) => format!("{FILE_KEY_PREFIX}{}", path.display()),
            Self::Terminal(terminal) => format!("{TERMINAL_KEY_PREFIX}{}", terminal.get()),
        }
    }

    pub fn from_page(key: &str) -> Option<Self> {
        if let Some(path) = key.strip_prefix(FILE_KEY_PREFIX) {
            return Some(Self::File(PathBuf::from(path)));
        }
        let terminal = key.strip_prefix(TERMINAL_KEY_PREFIX)?.parse().ok()?;
        Some(Self::Terminal(TerminalId::new(terminal)))
    }
}

/// A dock's order and pins.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DockTabs {
    /// The tabs as the user last arranged them.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<TabKey>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub pinned: BTreeSet<TabKey>,
}

impl DockTabs {
    pub fn is_empty(&self) -> bool {
        self.order.is_empty() && self.pinned.is_empty()
    }

    /// `tabs` in the dock's order: the pinned ones first, then the others, each in the order the
    /// user left them. Tabs the order does not name follow, as `tabs` has them.
    pub fn arrange(&self, mut tabs: Vec<TabKey>) -> Vec<TabKey> {
        let rank = |tab: &TabKey| {
            self.order
                .iter()
                .position(|ordered| ordered == tab)
                .unwrap_or(usize::MAX)
        };
        tabs.sort_by_cached_key(|tab| (!self.pinned.contains(tab), rank(tab)));
        tabs
    }

    fn forget(&mut self, tab: &TabKey) {
        self.order.retain(|ordered| ordered != tab);
        self.pinned.remove(tab);
    }

    fn retain(&mut self, keep: impl Fn(&TabKey) -> bool) {
        self.order.retain(&keep);
        self.pinned.retain(&keep);
    }
}

/// An agent's docks, as the preferences keep them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentLayout {
    #[serde(skip_serializing_if = "DockTabs::is_empty")]
    pub side: DockTabs,
    #[serde(skip_serializing_if = "DockTabs::is_empty")]
    pub bottom: DockTabs,
    /// The dock each terminal sits in. A terminal missing here sits in the agent's default dock.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub terminals: BTreeMap<TerminalId, Dock>,
}

impl AgentLayout {
    pub fn is_empty(&self) -> bool {
        self.side.is_empty() && self.bottom.is_empty() && self.terminals.is_empty()
    }

    fn dock(&self, dock: Dock) -> &DockTabs {
        match dock {
            Dock::Side => &self.side,
            Dock::Bottom => &self.bottom,
        }
    }

    fn dock_mut(&mut self, dock: Dock) -> &mut DockTabs {
        match dock {
            Dock::Side => &mut self.side,
            Dock::Bottom => &mut self.bottom,
        }
    }
}

fn layout(state: &AppState, agent: AgentId) -> Option<&AgentLayout> {
    state.ui.layouts.get(&agent)
}

/// Writes `change` into the agent's layout, and drops a layout left empty.
fn change_layout(state: &mut AppState, agent: AgentId, change: impl FnOnce(&mut AgentLayout)) {
    let layout = state.ui.layouts.entry(agent).or_default();
    change(layout);
    if layout.is_empty() {
        state.ui.layouts.remove(&agent);
    }
}

/// The dock a terminal of `agent` opens in when nothing else says.
pub fn default_dock(state: &AppState, agent: AgentId) -> Dock {
    state
        .daemon_machine(agent)
        .map_or(state.prefs.terminal.place, |machine| {
            state
                .prefs
                .terminal
                .agent(machine, MachineId::daemon_agent(agent))
        })
}

/// The dock `terminal` sits in.
pub fn terminal_dock(state: &AppState, agent: AgentId, terminal: TerminalId) -> Dock {
    layout(state, agent)
        .and_then(|layout| layout.terminals.get(&terminal).copied())
        .unwrap_or_else(|| default_dock(state, agent))
}

/// The dock a tab sits in.
pub fn tab_dock(state: &AppState, agent: AgentId, tab: &TabKey) -> Dock {
    match tab {
        TabKey::File(path) => dock_of_file(state, agent, path),
        TabKey::Terminal(terminal) => terminal_dock(state, agent, *terminal),
    }
}

/// The dock that holds the tab of `path`: the bottom dock when its file view has it, else the
/// side dock.
pub fn dock_of_file(state: &AppState, agent: AgentId, path: &Path) -> Dock {
    let in_bottom = state
        .ui
        .file_view_in(agent, Dock::Bottom)
        .is_some_and(|view| view.tabs(&[]).iter().any(|tab| tab == path));
    if in_bottom { Dock::Bottom } else { Dock::Side }
}

/// The agent's terminals in `dock`, in the order they opened.
pub fn terminals_in(state: &AppState, agent: AgentId, dock: Dock) -> Vec<TerminalId> {
    let Some(entry) = state.agents.get(&agent) else {
        return Vec::new();
    };
    entry
        .terminals
        .tabs
        .iter()
        .map(|tab| tab.terminal)
        .filter(|&terminal| terminal_dock(state, agent, terminal) == dock)
        .collect()
}

/// Whether the terminals of `agent`'s `dock` show.
pub fn terminals_shown(state: &AppState, agent: AgentId, dock: Dock) -> bool {
    state.ui.terminal_docks.contains(&(agent, dock))
}

/// The tabs `dock` shows, in its order: its files, and its terminals when they show. The side
/// dock's file tabs include the changed files. The bottom dock's hold only the files opened there.
pub fn tabs(state: &AppState, agent: AgentId, dock: Dock) -> Vec<TabKey> {
    let mut tabs = Vec::new();
    if let (Some(entry), Some(view)) =
        (state.agents.get(&agent), state.ui.file_view_in(agent, dock))
    {
        // A changed file whose tab is in the bottom dock is not the side dock's.
        let changed = match dock {
            Dock::Side => {
                let bottom = state
                    .ui
                    .file_view_in(agent, Dock::Bottom)
                    .map(|view| view.tabs(&[]))
                    .unwrap_or_default();
                let mut changed = entry.changed_paths();
                changed.retain(|path| !bottom.contains(path));
                changed
            }
            Dock::Bottom => Vec::new(),
        };
        tabs.extend(view.tabs(&changed).into_iter().map(TabKey::File));
    }
    if terminals_shown(state, agent, dock) {
        tabs.extend(
            terminals_in(state, agent, dock)
                .into_iter()
                .map(TabKey::Terminal),
        );
    }
    match layout(state, agent) {
        Some(layout) => layout.dock(dock).arrange(tabs),
        None => tabs,
    }
}

/// The file tabs of the dock the file actions act on, in its order.
pub fn file_tabs(state: &AppState, agent: AgentId) -> Vec<PathBuf> {
    file_tabs_in(state, agent, state.ui.active_files_dock(agent))
}

/// The file tabs of `dock`, in its order.
pub fn file_tabs_in(state: &AppState, agent: AgentId, dock: Dock) -> Vec<PathBuf> {
    tabs(state, agent, dock)
        .into_iter()
        .filter_map(|tab| match tab {
            TabKey::File(path) => Some(path),
            TabKey::Terminal(_) => None,
        })
        .collect()
}

pub fn is_pinned(state: &AppState, agent: AgentId, tab: &TabKey) -> bool {
    let dock = tab_dock(state, agent, tab);
    layout(state, agent).is_some_and(|layout| layout.dock(dock).pinned.contains(tab))
}

/// Puts `tab` in `to`, just before `before`, or at the end when `before` is `None`. A pinned tab
/// stays among the pinned ones, and another tab after them. A file's tab moves to another dock
/// with `actions::move_file`, which calls this.
pub fn move_tab(
    state: &mut AppState,
    agent: AgentId,
    tab: &TabKey,
    to: Dock,
    before: Option<&TabKey>,
) {
    let from = tab_dock(state, agent, tab);
    let pinned = is_pinned(state, agent, tab);
    let left: Vec<TabKey> = tabs(state, agent, from)
        .into_iter()
        .filter(|shown| shown != tab)
        .collect();
    let mut arrived: Vec<TabKey> = if from == to {
        left.clone()
    } else {
        tabs(state, agent, to)
    };
    let at = before
        .and_then(|before| arrived.iter().position(|shown| shown == before))
        .unwrap_or(arrived.len());
    arrived.insert(at, tab.clone());
    change_layout(state, agent, |layout| {
        if from != to {
            let source = layout.dock_mut(from);
            let old: Vec<TabKey> = source
                .order
                .iter()
                .filter(|old| *old != tab)
                .cloned()
                .collect();
            source.order = keep_hidden(&old, left);
            source.pinned.remove(tab);
        }
        let target = layout.dock_mut(to);
        target.order = keep_hidden(&target.order, arrived);
        if pinned {
            target.pinned.insert(tab.clone());
        }
        if let TabKey::Terminal(terminal) = tab {
            layout.terminals.insert(*terminal, to);
        }
    });
}

/// Pins `tab`, at the end of the pinned tabs, or unpins it, at the start of the others.
pub fn toggle_pin(state: &mut AppState, agent: AgentId, tab: &TabKey) {
    let dock = tab_dock(state, agent, tab);
    let mut tabs = tabs(state, agent, dock);
    let Some(from) = tabs.iter().position(|shown| shown == tab) else {
        return;
    };
    let moved = tabs.remove(from);
    change_layout(state, agent, |layout| {
        let dock = layout.dock_mut(dock);
        if !dock.pinned.remove(tab) {
            dock.pinned.insert(tab.clone());
        }
        let at = tabs
            .iter()
            .filter(|shown| dock.pinned.contains(shown))
            .count();
        tabs.insert(at, moved);
        dock.order = keep_hidden(&dock.order, tabs);
    });
}

/// Gives each of the agent's terminals that has no dock yet the agent's default dock, so that a
/// later change of the default leaves it where it is.
pub fn settle_terminals(state: &mut AppState, agent: AgentId) {
    let default = default_dock(state, agent);
    let Some(entry) = state.agents.get(&agent) else {
        return;
    };
    let terminals: Vec<TerminalId> = entry
        .terminals
        .tabs
        .iter()
        .map(|tab| tab.terminal)
        .collect();
    change_layout(state, agent, |layout| {
        for terminal in terminals {
            layout.terminals.entry(terminal).or_insert(default);
        }
    });
}

/// `shown`, the tabs a dock shows in a new order, with the tabs `old` holds that it does not show,
/// such as hidden terminals, each before the shown tab that followed it in `old`.
fn keep_hidden(old: &[TabKey], mut shown: Vec<TabKey>) -> Vec<TabKey> {
    let visible: Vec<TabKey> = shown.clone();
    for (index, tab) in old.iter().enumerate() {
        if visible.contains(tab) || shown.contains(tab) {
            continue;
        }
        let next = old
            .iter()
            .skip(index + 1)
            .find(|later| visible.contains(later));
        let at = next
            .and_then(|next| shown.iter().position(|placed| placed == next))
            .unwrap_or(shown.len());
        shown.insert(at, tab.clone());
    }
    shown
}

/// Puts a new terminal in `dock`, after its tabs.
pub fn place_terminal(state: &mut AppState, agent: AgentId, terminal: TerminalId, dock: Dock) {
    change_layout(state, agent, |layout| {
        layout.terminals.insert(terminal, dock);
    });
}

/// Drops a closed tab from its dock's order and pins.
pub fn forget_tab(state: &mut AppState, agent: AgentId, tab: &TabKey) {
    change_layout(state, agent, |layout| {
        layout.side.forget(tab);
        layout.bottom.forget(tab);
        if let TabKey::Terminal(terminal) = tab {
            layout.terminals.remove(terminal);
        }
    });
}

/// Drops the file tabs of `dock`'s file view, which closed.
pub fn forget_files(state: &mut AppState, agent: AgentId, dock: Dock) {
    change_layout(state, agent, |layout| {
        layout
            .dock_mut(dock)
            .retain(|tab| matches!(tab, TabKey::Terminal(_)));
    });
}

/// Keeps the terminals the daemon lists: a terminal that ended while the app was away goes from
/// the layout.
pub fn keep_terminals(state: &mut AppState, agent: AgentId, listed: &BTreeSet<TerminalId>) {
    let kept = |tab: &TabKey| match tab {
        TabKey::File(_) => true,
        TabKey::Terminal(terminal) => listed.contains(terminal),
    };
    change_layout(state, agent, |layout| {
        layout.side.retain(kept);
        layout.bottom.retain(kept);
        layout
            .terminals
            .retain(|terminal, _| listed.contains(terminal));
    });
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use cockpit_protocol::AgentStatus;

    use super::*;
    use crate::state::{ViewMode, actions, state_with, summary};

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    /// The agent with the files a.rs to d.rs open, in that order.
    fn with_tabs() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
            actions::open_file(&mut state, agent(), PathBuf::from(name), None);
        }
        state
    }

    fn names(state: &AppState) -> Vec<String> {
        file_tabs(state, agent())
            .iter()
            .map(|tab| tab.display().to_string())
            .collect()
    }

    #[test]
    fn a_dragged_tab_lands_before_the_tab_it_was_dropped_on_or_at_the_end() {
        let mut state = with_tabs();

        move_tab(
            &mut state,
            agent(),
            &file("d.rs"),
            Dock::Side,
            Some(&file("b.rs")),
        );
        let before_b = names(&state);
        move_tab(&mut state, agent(), &file("a.rs"), Dock::Side, None);
        actions::open_file(&mut state, agent(), PathBuf::from("e.rs"), None);

        assert_eq!(before_b, ["a.rs", "d.rs", "b.rs", "c.rs"]);
        assert_eq!(names(&state), ["d.rs", "b.rs", "c.rs", "a.rs", "e.rs"]);
    }

    #[test]
    fn a_pinned_tab_goes_after_the_pinned_ones_and_back_after_them_when_unpinned() {
        let mut state = with_tabs();

        toggle_pin(&mut state, agent(), &file("c.rs"));
        toggle_pin(&mut state, agent(), &file("d.rs"));
        let pinned = names(&state);
        toggle_pin(&mut state, agent(), &file("c.rs"));

        assert_eq!(pinned, ["c.rs", "d.rs", "a.rs", "b.rs"]);
        assert_eq!(names(&state), ["d.rs", "c.rs", "a.rs", "b.rs"]);
        assert!(is_pinned(&state, agent(), &file("d.rs")));
        assert!(!is_pinned(&state, agent(), &file("c.rs")));
    }

    #[test]
    fn a_closed_tab_leaves_the_order_and_the_pins() {
        let mut state = with_tabs();
        toggle_pin(&mut state, agent(), &file("c.rs"));

        actions::close_tab(&mut state, agent(), Path::new("c.rs"));
        actions::open_file(&mut state, agent(), PathBuf::from("c.rs"), None);

        assert_eq!(names(&state), ["a.rs", "b.rs", "d.rs", "c.rs"]);
        assert!(!is_pinned(&state, agent(), &file("c.rs")));
    }

    fn shown(state: &AppState, dock: Dock) -> Option<String> {
        let view = state.ui.file_view_in(agent(), dock)?;
        Some(view.path.display().to_string())
    }

    fn files_in(state: &AppState, dock: Dock) -> Vec<String> {
        file_tabs_in(state, agent(), dock)
            .iter()
            .map(|tab| tab.display().to_string())
            .collect()
    }

    #[test]
    fn the_side_dock_closes_when_its_only_file_moves_to_the_bottom() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        actions::open_file(&mut state, agent(), PathBuf::from("a.rs"), None);

        actions::move_file(&mut state, agent(), Path::new("a.rs"), Dock::Bottom, None);

        assert!(!crate::state::terminals::side_open(&state, agent()));
        assert_eq!(shown(&state, Dock::Bottom).as_deref(), Some("a.rs"));
    }

    #[test]
    fn a_file_changed_after_its_move_to_the_bottom_keeps_its_one_tab() {
        let mut state = with_tabs();
        actions::move_file(&mut state, agent(), Path::new("b.rs"), Dock::Bottom, None);

        let entry = state.agents.get_mut(&agent()).unwrap();
        entry.details = crate::state::Loadable::Loaded(cockpit_protocol::AgentDetails {
            changed_files: vec![cockpit_protocol::ChangedFile {
                path: PathBuf::from("/work/project-1/b.rs"),
                lines_added: 1,
                lines_removed: 0,
            }],
            ..cockpit_protocol::AgentDetails::default()
        });

        assert_eq!(files_in(&state, Dock::Side), ["a.rs", "c.rs", "d.rs"]);
        assert_eq!(files_in(&state, Dock::Bottom), ["b.rs"]);
    }

    #[test]
    fn back_in_the_side_skips_a_file_moved_to_the_bottom() {
        let mut state = with_tabs();
        actions::move_file(&mut state, agent(), Path::new("c.rs"), Dock::Bottom, None);
        state.ui.pressed_dock.insert(agent(), Dock::Side);

        actions::travel_files(&mut state, agent(), crate::state::Travel::Back);

        assert_eq!(shown(&state, Dock::Side).as_deref(), Some("b.rs"));
        assert_eq!(files_in(&state, Dock::Side), ["a.rs", "b.rs", "d.rs"]);
    }

    #[test]
    fn each_docks_file_keeps_its_git() {
        let mut state = with_tabs();
        actions::move_file(&mut state, agent(), Path::new("b.rs"), Dock::Bottom, None);
        let text = || crate::state::FileRead::Text {
            text: "fn main() {}\n".to_owned(),
            edits: Vec::new(),
        };
        let entry = state.agents.get_mut(&agent()).unwrap();
        for name in ["b.rs", "d.rs"] {
            entry
                .files
                .insert(PathBuf::from(name), crate::state::Loadable::Loaded(text()));
        }

        crate::state::git::requests(&mut state, agent());

        assert_eq!(state.agents[&agent()].git.files.len(), 2);
    }

    #[test]
    fn a_moved_file_keeps_its_mode() {
        let mut state = with_tabs();
        actions::set_view_mode(&mut state, agent(), ViewMode::Diff);

        actions::move_file(&mut state, agent(), Path::new("d.rs"), Dock::Bottom, None);

        let bottom = state.ui.file_view_in(agent(), Dock::Bottom).unwrap();
        assert_eq!((bottom.mode, bottom.mode_settled), (ViewMode::Diff, true));
    }

    #[test]
    fn a_file_moved_to_the_bottom_dock_shows_there_and_leaves_the_side() {
        let mut state = with_tabs();

        actions::move_file(&mut state, agent(), Path::new("d.rs"), Dock::Bottom, None);
        actions::move_file(&mut state, agent(), Path::new("b.rs"), Dock::Bottom, None);

        assert_eq!(files_in(&state, Dock::Side), ["a.rs", "c.rs"]);
        assert_eq!(files_in(&state, Dock::Bottom), ["d.rs", "b.rs"]);
        assert_eq!(shown(&state, Dock::Side).as_deref(), Some("c.rs"));
        assert_eq!(shown(&state, Dock::Bottom).as_deref(), Some("b.rs"));
        let read: Vec<&PathBuf> = state.agents[&agent()].files.keys().collect();
        assert_eq!(read, [Path::new("b.rs"), Path::new("c.rs")]);
    }

    #[test]
    fn a_file_whose_tab_is_in_the_bottom_dock_opens_there_and_its_actions_follow() {
        let mut state = with_tabs();
        actions::move_file(&mut state, agent(), Path::new("b.rs"), Dock::Bottom, None);
        actions::open_file(&mut state, agent(), PathBuf::from("a.rs"), None);
        let after_side = state.ui.active_files_dock(agent());

        actions::open_file(&mut state, agent(), PathBuf::from("b.rs"), Some(3));
        actions::set_view_mode(&mut state, agent(), ViewMode::Diff);

        assert_eq!(after_side, Dock::Side);
        assert_eq!(state.ui.active_files_dock(agent()), Dock::Bottom);
        let bottom = state.ui.file_view_in(agent(), Dock::Bottom).unwrap();
        assert_eq!((bottom.line, bottom.mode), (Some(3), ViewMode::Diff));
        assert_eq!(shown(&state, Dock::Side).as_deref(), Some("a.rs"));
    }

    #[test]
    fn a_file_moved_back_to_the_side_leaves_the_bottom_dock_closed() {
        let mut state = with_tabs();
        actions::move_file(&mut state, agent(), Path::new("b.rs"), Dock::Bottom, None);

        actions::move_file(
            &mut state,
            agent(),
            Path::new("b.rs"),
            Dock::Side,
            Some(&file("a.rs")),
        );

        assert!(state.ui.file_view_in(agent(), Dock::Bottom).is_none());
        assert_eq!(
            files_in(&state, Dock::Side),
            ["b.rs", "a.rs", "c.rs", "d.rs"]
        );
        assert!(!state.ui.bottom_picks_file.contains(&agent()));
    }

    #[test]
    fn closing_the_bottom_dock_closes_its_files_and_leaves_the_sides() {
        let mut state = with_tabs();
        actions::move_file(&mut state, agent(), Path::new("b.rs"), Dock::Bottom, None);

        actions::close_bottom_dock(&mut state, agent());

        assert!(state.ui.file_view_in(agent(), Dock::Bottom).is_none());
        assert_eq!(files_in(&state, Dock::Side), ["a.rs", "c.rs", "d.rs"]);
        assert_eq!(state.ui.active_files_dock(agent()), Dock::Side);
    }

    fn file(name: &str) -> TabKey {
        TabKey::File(PathBuf::from(name))
    }

    fn files(names: &[&str]) -> Vec<TabKey> {
        names.iter().map(|name| file(name)).collect()
    }

    #[test]
    fn the_pinned_tabs_come_first_then_the_moved_ones_then_the_new_ones() {
        let dock = DockTabs {
            order: files(&["c", "a", "d"]),
            pinned: BTreeSet::from([file("d")]),
        };

        let arranged = dock.arrange(files(&["a", "b", "c", "d", "e"]));

        assert_eq!(arranged, files(&["d", "c", "a", "b", "e"]));
    }

    #[test]
    fn a_tab_key_goes_to_the_page_and_back() {
        let key = file("src/a b:c.rs");

        assert_eq!(TabKey::from_page(&key.to_page()), Some(key));
        assert_eq!(
            TabKey::from_page("terminal:3"),
            Some(TabKey::Terminal(TerminalId::new(3)))
        );
        assert_eq!(TabKey::from_page("folder:src"), None);
    }
}
