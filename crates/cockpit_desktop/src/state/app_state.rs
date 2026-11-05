//! Everything the window shows, in one value that the event and response handlers update.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use cockpit_protocol::{
    AgentDetails, AgentId, AgentStatus, AgentSummary, PastSession, PlanUsage, Settings, Timestamp,
};

use super::transcript::Transcript;
use super::ui_state::UiState;
use crate::config::DaemonLabel;
use crate::prefs::Prefs;

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
    /// The file view's texts, by the path the view uses.
    pub files: BTreeMap<PathBuf, Loadable<String>>,
}

impl AgentEntry {
    pub fn new(summary: AgentSummary) -> Self {
        Self {
            summary,
            transcript: Transcript::default(),
            details: Loadable::NotRequested,
            files: BTreeMap::new(),
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

    /// The sidebar's order: by attention, unread first within a status, then by id.
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

    /// Keeps the selection on an agent that exists, or picks the first by attention.
    pub fn fix_selection(&mut self) {
        let valid = self
            .ui
            .selected
            .is_some_and(|selected| self.agents.contains_key(&selected));
        if !valid {
            self.ui.selected = self.agents_by_attention().first().map(|entry| entry.id());
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
    use cockpit_protocol::{CostUsd, PermissionMode};

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
            mode: PermissionMode::Default,
            task_progress: None,
            context_percent: None,
            cache_expires_at: None,
            turns: 0,
            cost: CostUsd::new(0.0),
            started_at: Timestamp::from_unix_millis(u64::from(id) * 1000),
            session: None,
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

    fn order(state: &AppState) -> Vec<u32> {
        state
            .agents_by_attention()
            .iter()
            .map(|entry| entry.id().get())
            .collect()
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

        assert_eq!(order(&state), [4, 3, 5, 2, 1]);
    }

    #[test]
    fn fix_selection_picks_the_first_by_attention_and_marks_it_read() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::NeedsYou),
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
