//! Everything the window shows, in one value that the event and response handlers update.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use cockpit_protocol::{
    AgentDetails, AgentId, AgentStatus, AgentSummary, PastSession, PlanUsage, Settings, Timestamp,
    list_order,
};
use strum::VariantArray;

use super::file_edits;
use super::transcript::Transcript;
use super::ui_state::UiState;
use crate::config::DaemonLabel;
use crate::prefs::{Prefs, StatusFilter};

/// Data the app asks the daemon for when it needs it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Loadable<T> {
    #[default]
    NotRequested,
    Loading,
    Loaded(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn loaded(&self) -> Option<&T> {
        match self {
            Self::Loaded(value) => Some(value),
            Self::NotRequested | Self::Loading | Self::Failed(_) => None,
        }
    }

    pub fn loaded_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Loaded(value) => Some(value),
            Self::NotRequested | Self::Loading | Self::Failed(_) => None,
        }
    }
}

/// What the daemon answered for the file view's path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRead {
    Text(String),
    /// Several files end with the path, which names none in the agent's folder: the user picks
    /// one. Absolute paths.
    Choices(Vec<PathBuf>),
    /// No file matches: the daemon's sentence that says so.
    Missing(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// `attempt` counts from 1 since the last connection.
    Connecting {
        attempt: u32,
    },
    Connected,
    Disconnected {
        error: String,
        retry_at: Instant,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentEntry {
    pub summary: AgentSummary,
    pub transcript: Transcript,
    pub details: Loadable<AgentDetails>,
    /// An `agent.context_usage` request is on its way: the usage card shows that it loads.
    pub context_loading: bool,
    /// The file view's texts, by the path the view uses.
    pub files: BTreeMap<PathBuf, Loadable<FileRead>>,
    /// An `agent.commands` request is on its way.
    pub commands_requested: bool,
    /// When the app saw the agent's current turn start. `None` between turns, and for a turn
    /// already running when the app connected: the daemon does not say when a turn started.
    pub turn_started: Option<Instant>,
}

impl AgentEntry {
    pub fn new(summary: AgentSummary) -> Self {
        Self {
            summary,
            transcript: Transcript::default(),
            details: Loadable::NotRequested,
            context_loading: false,
            files: BTreeMap::new(),
            commands_requested: false,
            turn_started: None,
        }
    }

    /// The files the agent changed, by the paths the file view uses.
    pub fn changed_paths(&self) -> Vec<PathBuf> {
        let folder = &self.summary.folder;
        self.details
            .loaded()
            .into_iter()
            .flat_map(|details| details.changed_files.iter())
            .map(|file| file_edits::relative_path(folder, &file.path))
            .collect()
    }

    /// Whether the commands and skills are on their way, with the details or on their own.
    pub fn commands_loading(&self) -> bool {
        match &self.details {
            Loadable::NotRequested | Loadable::Loading => true,
            Loadable::Loaded(_) => self.commands_requested,
            Loadable::Failed(_) => false,
        }
    }

    pub fn id(&self) -> AgentId {
        self.summary.id
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppState {
    pub daemon: DaemonLabel,
    pub connection: ConnectionState,
    pub agents: BTreeMap<AgentId, AgentEntry>,
    pub plan_usage: PlanUsage,
    pub settings: Settings,
    /// Past Claude sessions, for the new agent dialog.
    pub sessions: Loadable<Vec<PastSession>>,
    pub ui: UiState,
    /// The choices the app saves on this machine.
    pub prefs: Prefs,
}

/// Agents that wait for you come first, then working, idle and exited ones.
pub fn attention_rank(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::NeedsYou => 0,
        AgentStatus::Working => 1,
        AgentStatus::Idle => 2,
        AgentStatus::Exited => 3,
    }
}

impl AppState {
    pub fn new(daemon: DaemonLabel) -> Self {
        Self {
            daemon,
            connection: ConnectionState::Connecting { attempt: 1 },
            agents: BTreeMap::new(),
            plan_usage: PlanUsage::default(),
            settings: Settings::default(),
            sessions: Loadable::NotRequested,
            ui: UiState::default(),
            prefs: Prefs::default(),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connection == ConnectionState::Connected
    }

    /// The sidebar's order, which the user sets and the daemon keeps.
    pub fn agents_in_order(&self) -> Vec<&AgentEntry> {
        let mut agents: Vec<&AgentEntry> = self.agents.values().collect();
        agents.sort_by(|left, right| list_order(left.summary.place(), right.summary.place()));
        agents
    }

    /// The agents the sidebar shows, in its order: those its status filter keeps.
    pub fn sidebar_agents(&self) -> Vec<&AgentEntry> {
        let filter = self.prefs.sidebar.filter;
        let mut agents = self.agents_in_order();
        agents.retain(|entry| filter.admits(entry.summary.status));
        agents
    }

    /// How many agents `filter` keeps, for its chip.
    pub fn filter_count(&self, filter: StatusFilter) -> usize {
        self.agents
            .values()
            .filter(|entry| filter.admits(entry.summary.status))
            .count()
    }

    /// The status filter's chips, with their counts. A status's chip shows while an agent has
    /// that status or the filter keeps it, and "All" shows beside them. When "All" would show
    /// alone, no chip shows.
    pub fn filter_chips(&self) -> Vec<(StatusFilter, usize)> {
        let chosen = self.prefs.sidebar.filter;
        let chips: Vec<(StatusFilter, usize)> = StatusFilter::VARIANTS
            .iter()
            .map(|&filter| (filter, self.filter_count(filter)))
            .filter(|&(filter, count)| filter == StatusFilter::All || filter == chosen || count > 0)
            .collect();
        if chips.len() > 1 { chips } else { Vec::new() }
    }

    /// Whether the status filter hides the selected agent, which the center still shows.
    pub fn selected_is_hidden(&self) -> bool {
        let filter = self.prefs.sidebar.filter;
        self.selected()
            .is_some_and(|entry| !filter.admits(entry.summary.status))
    }

    /// The agent switcher's order: by attention, unread first within a status, then by id.
    pub fn agents_by_attention(&self) -> Vec<&AgentEntry> {
        let mut agents: Vec<&AgentEntry> = self.agents.values().collect();
        agents.sort_by_key(|entry| {
            (
                attention_rank(entry.summary.status),
                !self.ui.unread.contains(&entry.id()),
                entry.id(),
            )
        });
        agents
    }

    pub fn selected(&self) -> Option<&AgentEntry> {
        self.agents.get(&self.ui.selected?)
    }

    pub fn is_unread(&self, agent: AgentId) -> bool {
        self.ui.unread.contains(&agent)
    }

    /// Keeps the selection on an agent that exists, or picks the sidebar's first row. When the
    /// filter hides every agent, it picks the first of the whole order.
    pub fn fix_selection(&mut self) {
        let valid = self
            .ui
            .selected
            .is_some_and(|selected| self.agents.contains_key(&selected));
        if !valid {
            // What is open belonged to the agent that is gone, such as its typed palette.
            self.ui.leave_agent();
            let first_row = self.sidebar_agents().first().map(|entry| entry.id());
            let first =
                first_row.or_else(|| self.agents_in_order().first().map(|entry| entry.id()));
            self.ui.selected = first;
        }
        if let Some(selected) = self.ui.selected {
            self.ui.unread.remove(&selected);
        }
    }

    /// Folders where agents ran, with their last use, most recent first, for the new agent
    /// dialog.
    pub fn recent_folders(&self) -> Vec<(PathBuf, Timestamp)> {
        let mut uses: BTreeMap<PathBuf, Timestamp> = BTreeMap::new();
        let agent_uses = self
            .agents
            .values()
            .map(|entry| (&entry.summary.folder, entry.summary.started_at));
        let session_uses = self
            .sessions
            .loaded()
            .into_iter()
            .flatten()
            .map(|session: &PastSession| (&session.folder, session.last_activity));
        for (folder, moment) in agent_uses.chain(session_uses) {
            let latest = uses.entry(folder.clone()).or_insert(moment);
            *latest = (*latest).max(moment);
        }
        let mut folders: Vec<(PathBuf, Timestamp)> = uses.into_iter().collect();
        folders.sort_by(|(_, left), (_, right)| right.cmp(left));
        folders
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use cockpit_protocol::{CostUsd, PermissionMode, Position};

    use super::*;

    pub(crate) fn summary(id: u32, status: AgentStatus) -> AgentSummary {
        AgentSummary {
            id: AgentId::new(id),
            title: format!("Agent {id}"),
            folder: PathBuf::from(format!("/work/project-{id}")),
            status,
            activity: None,
            pending_count: 0,
            model: None,
            effort: None,
            effort_is_default: false,
            mode: PermissionMode::Default,
            task_progress: None,
            context_percent: None,
            cache_expires_at: None,
            turns: 0,
            cost: CostUsd::new(0.0),
            started_at: Timestamp::from_unix_millis(u64::from(id) * 1000),
            last_activity_at: None,
            turn_started_at: None,
            session: None,
            position: Position::default(),
        }
    }

    pub(crate) fn state_with(summaries: Vec<AgentSummary>) -> AppState {
        let mut state = AppState::new(DaemonLabel {
            machine: "build-box".to_owned(),
            over_ssh: true,
        });
        for summary in summaries {
            state.agents.insert(summary.id, AgentEntry::new(summary));
        }
        state
    }

    pub(crate) fn ids(entries: &[&AgentEntry]) -> Vec<u32> {
        entries.iter().map(|entry| entry.id().get()).collect()
    }

    /// A summary at `position` in the list.
    pub(crate) fn placed(id: u32, status: AgentStatus, position: u32) -> AgentSummary {
        AgentSummary {
            position: Position::new(position),
            ..summary(id, status)
        }
    }

    #[test]
    fn the_sidebar_follows_the_positions_whatever_the_status() {
        let mut state = state_with(vec![
            placed(1, AgentStatus::NeedsYou, 1024),
            placed(2, AgentStatus::Exited, 3072),
            placed(3, AgentStatus::Idle, 2048),
        ]);
        state.ui.unread.insert(AgentId::new(1));
        let before = ids(&state.agents_in_order());

        if let Some(entry) = state.agents.get_mut(&AgentId::new(2)) {
            entry.summary.status = AgentStatus::Working;
        }

        assert_eq!(before, [2, 3, 1]);
        assert_eq!(ids(&state.agents_in_order()), [2, 3, 1]);
    }

    #[test]
    fn the_filter_hides_rows_without_changing_their_order() {
        let mut state = state_with(vec![
            placed(1, AgentStatus::Idle, 1024),
            placed(2, AgentStatus::Working, 2048),
            placed(3, AgentStatus::Idle, 3072),
            placed(4, AgentStatus::NeedsYou, 4096),
        ]);
        state.ui.selected = Some(AgentId::new(2));

        state.prefs.sidebar.filter = StatusFilter::Idle;

        assert_eq!(ids(&state.sidebar_agents()), [3, 1]);
        assert_eq!(state.filter_count(StatusFilter::Idle), 2);
        assert_eq!(state.filter_count(StatusFilter::All), 4);
        assert_eq!(state.filter_count(StatusFilter::Exited), 0);
        assert!(state.selected_is_hidden());
        assert_eq!(state.selected().map(AgentEntry::id), Some(AgentId::new(2)));
    }

    #[test]
    fn the_filter_shows_only_the_chips_with_agents_and_the_chosen_one() {
        let mut state = state_with(vec![
            placed(1, AgentStatus::Idle, 1024),
            placed(2, AgentStatus::Working, 2048),
            placed(3, AgentStatus::Idle, 3072),
        ]);
        let shown = state.filter_chips();
        state.prefs.sidebar.filter = StatusFilter::Exited;

        assert_eq!(
            shown,
            [
                (StatusFilter::All, 3),
                (StatusFilter::Working, 1),
                (StatusFilter::Idle, 2),
            ]
        );
        assert_eq!(
            state.filter_chips(),
            [
                (StatusFilter::All, 3),
                (StatusFilter::Working, 1),
                (StatusFilter::Idle, 2),
                (StatusFilter::Exited, 0),
            ]
        );
    }

    #[test]
    fn the_filter_hides_when_all_would_be_its_only_chip() {
        let mut state = state_with(Vec::new());
        let without_agents = state.filter_chips();
        state.prefs.sidebar.filter = StatusFilter::NeedsYou;

        assert_eq!(without_agents, []);
        assert_eq!(
            state.filter_chips(),
            [(StatusFilter::All, 0), (StatusFilter::NeedsYou, 0)]
        );
    }

    #[test]
    fn fix_selection_picks_the_first_row_the_filter_shows() {
        let mut state = state_with(vec![
            placed(1, AgentStatus::Idle, 1024),
            placed(2, AgentStatus::Working, 2048),
        ]);
        state.prefs.sidebar.filter = StatusFilter::Idle;

        state.fix_selection();
        let shown = state.ui.selected;
        state.prefs.sidebar.filter = StatusFilter::Exited;
        state.ui.selected = None;
        state.fix_selection();

        assert_eq!(shown, Some(AgentId::new(1)));
        assert_eq!(state.ui.selected, Some(AgentId::new(2)));
    }

    #[test]
    fn agents_sort_by_attention_then_unread_then_id() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Exited),
            summary(2, AgentStatus::Idle),
            summary(3, AgentStatus::Working),
            summary(4, AgentStatus::NeedsYou),
            summary(5, AgentStatus::Idle),
        ]);
        state.ui.unread.insert(AgentId::new(5));

        assert_eq!(ids(&state.agents_by_attention()), [4, 3, 5, 2, 1]);
    }

    #[test]
    fn fix_selection_picks_the_sidebars_first_and_marks_it_read() {
        let mut state = state_with(vec![
            placed(1, AgentStatus::NeedsYou, 1024),
            placed(2, AgentStatus::Idle, 2048),
        ]);
        state.ui.selected = Some(AgentId::new(9));
        state.ui.unread.insert(AgentId::new(2));

        state.fix_selection();

        assert_eq!(state.ui.selected, Some(AgentId::new(2)));
        assert!(!state.is_unread(AgentId::new(2)));
    }

    #[test]
    fn recent_folders_put_the_latest_use_first_without_duplicates() {
        let mut first = summary(1, AgentStatus::Idle);
        first.folder = PathBuf::from("/work/shared");
        let mut second = summary(2, AgentStatus::Idle);
        second.folder = PathBuf::from("/work/other");
        let mut third = summary(3, AgentStatus::Idle);
        third.folder = PathBuf::from("/work/shared");
        let state = state_with(vec![first, second, third]);

        let folders: Vec<PathBuf> = state
            .recent_folders()
            .into_iter()
            .map(|(folder, _)| folder)
            .collect();
        assert_eq!(
            folders,
            [PathBuf::from("/work/shared"), PathBuf::from("/work/other")]
        );
    }
}
