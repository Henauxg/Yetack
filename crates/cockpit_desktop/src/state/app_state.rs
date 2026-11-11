//! Everything the window shows, in one value that the event and response handlers update.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use cockpit_protocol::{
    Account, AccountId, AgentDetails, AgentId, AgentStatus, AgentSummary, AnswerState, FileEdit,
    Image, ItemBody, ItemId, PastSession, PlanUsage, ReleaseVersion, Settings, Timestamp,
    ToolCallStatus, TranscriptItem, WaitingPrompt, list_order,
};
use strum::VariantArray;

use super::accounts::SignIns;
use super::chat_images::KeptImage;
use super::daemon_update::{DaemonUpdate, VersionGap};
use super::file_edits;
use super::file_tree::FileTree;
use super::no_daemon::NoDaemon;
use super::sending::Sending;
use super::transcript::Transcript;
use super::ui_state::UiState;
use crate::daemon_label::DaemonLabel;
use crate::format;
use crate::machine::MachineId;
use crate::path_style::PathStyle;
use crate::prefs::{NotificationLevel, Prefs, StatusFilter};

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
    /// `edits` are the agent's edits of the file, oldest first, read with the text.
    Text {
        text: String,
        edits: Vec<FileEdit>,
    },
    Image(Image),
    /// Several files end with the path, which names none in the agent's folder: the user picks
    /// one. Absolute paths.
    Choices(Vec<PathBuf>),
    /// No file matches: the daemon's sentence that says so.
    Missing(String),
}

#[cfg(test)]
impl FileRead {
    /// A text the agent did not edit.
    pub fn unedited(text: &str) -> Self {
        Self::Text {
            text: text.to_owned(),
            edits: Vec::new(),
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
    /// The first connection failed: the "No daemon answers" screen asks what to run.
    NoDaemon(NoDaemon),
}

/// A folder where an agent ran, for the new agent dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentFolder {
    pub folder: PathBuf,
    pub used: Timestamp,
    /// The account of the last agent or past session there.
    pub account: AccountId,
}

/// When the file view read its shown file, and what reading it again changed. Reading it again
/// keeps the old text on screen until the new one arrives.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileFreshness {
    /// When the shown text arrived.
    pub read_at: Option<Timestamp>,
    pub reread: Reread,
    /// The last read again that changed the text.
    pub update: Option<FileUpdate>,
}

/// Whether the shown file is to be read again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reread {
    #[default]
    Idle,
    Wanted,
    /// A read is on its way. With `again`, the file changed since it left: another read follows.
    Reading {
        again: bool,
    },
}

/// A read again that changed the shown text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUpdate {
    pub at: Timestamp,
    /// Counts the updates, so that the view plays the highlight again for a new one.
    pub serial: u64,
    /// The new text's lines, from 1, that the read changed or added.
    pub lines: BTreeSet<u32>,
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
    /// When the file view read its shown file, and what reading it again changed.
    pub file_freshness: FileFreshness,
    /// When the app saw each running tool call start, for the timer on its card. A call already
    /// running when the app connected has none.
    pub calls_running_since: BTreeMap<ItemId, Instant>,
    /// An `agent.commands` request is on its way.
    pub commands_requested: bool,
    /// When the app saw the agent's current turn start, for a daemon that does not send
    /// `turn_started_at`. `None` between turns, and for a turn already running when the app
    /// connected.
    pub turn_started: Option<Instant>,
    /// The end of the turn whose message the sign-in card sent again: the card sends it once.
    pub sent_again: Option<ItemId>,
    /// When the app announces the agent's finish, unless a turn starts before. Set when the last
    /// background task of an idle agent ends.
    pub finish_due: Option<Instant>,
    /// The images its messages show, the least recently shown first.
    pub images: VecDeque<KeptImage>,
    /// The files pane's tree of its folder.
    pub tree: FileTree,
    /// The messages sent that the daemon has not echoed yet.
    pub sending: Sending,
}

/// When the agent's running turn started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStart {
    /// The daemon's record, which a reconnect keeps.
    Daemon(Timestamp),
    /// When the app saw the turn start, from a daemon that does not send its own.
    Seen(Instant),
}

impl TurnStart {
    /// How long the turn has run at `now`, the daemon's start read on the daemon's clock.
    pub fn elapsed(self, now: SystemTime) -> Duration {
        match self {
            Self::Daemon(moment) => format::since(moment, now),
            Self::Seen(started) => started.elapsed(),
        }
    }
}

impl AgentEntry {
    /// Keeps when a tool call started running, as the app saw it at `now`, and forgets it once the
    /// call ends. A call that waits for approval has not started: it starts at the answer.
    pub fn note_call(&mut self, item: &TranscriptItem, now: Instant) {
        match &item.body {
            ItemBody::ToolCall(call) if call.status == ToolCallStatus::Running => {
                self.calls_running_since.entry(item.id).or_insert(now);
            }
            ItemBody::ToolCall(_) => {
                self.calls_running_since.remove(&item.id);
            }
            ItemBody::Approval(approval) => {
                let Some(call) = approval.tool_call else {
                    return;
                };
                let running = self.transcript.get(call).is_some_and(|item| {
                    matches!(&item.body, ItemBody::ToolCall(call) if call.status == ToolCallStatus::Running)
                });
                if approval.answer == AnswerState::Waiting || !running {
                    self.calls_running_since.remove(&call);
                } else {
                    self.calls_running_since.insert(call, now);
                }
            }
            _ => {}
        }
    }

    pub fn new(summary: AgentSummary) -> Self {
        Self {
            summary,
            transcript: Transcript::default(),
            details: Loadable::NotRequested,
            context_loading: false,
            files: BTreeMap::new(),
            file_freshness: FileFreshness::default(),
            calls_running_since: BTreeMap::new(),
            commands_requested: false,
            turn_started: None,
            sent_again: None,
            finish_due: None,
            images: VecDeque::new(),
            tree: FileTree::default(),
            sending: Sending::default(),
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

    /// Whether the agent changed the file at `path`, a path the file view uses.
    pub fn has_changed(&self, path: &Path) -> bool {
        let style = PathStyle::of(&self.summary.folder);
        let path = path.to_string_lossy();
        self.changed_paths()
            .iter()
            .any(|changed| style.same_path(&changed.to_string_lossy(), &path))
    }

    /// The prompts the agent has not read yet, oldest first.
    pub fn waiting_prompts(&self) -> &[WaitingPrompt] {
        self.details
            .loaded()
            .map_or(&[], |details| details.waiting_prompts.as_slice())
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

    /// The daemon's start of the running turn, else the app's own record of it.
    pub fn turn_start(&self) -> Option<TurnStart> {
        self.summary
            .turn_started_at
            .map(TurnStart::Daemon)
            .or(self.turn_started.map(TurnStart::Seen))
    }
}

/// What the app knows of one machine's daemon.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineState {
    pub daemon: DaemonLabel,
    pub connection: ConnectionState,
    /// The release the daemon said it runs, at its last connection.
    pub daemon_version: Option<ReleaseVersion>,
    /// The default account's plan usage.
    pub plan_usage: PlanUsage,
    pub settings: Settings,
    /// The daemon's Claude accounts, the default one first. Empty from a daemon that predates
    /// accounts.
    pub accounts: Vec<Account>,
    /// The plan usage of each account other than the default one.
    pub other_plan_usage: BTreeMap<AccountId, PlanUsage>,
    /// What `accounts.list` last answered: whether each account is signed in, and how.
    pub sign_ins: SignIns,
    /// Past Claude sessions, for the new agent dialog.
    pub sessions: Loadable<Vec<PastSession>>,
    /// The daemon is older than `folders.list`: the folder field lists nothing until the app
    /// connects again.
    pub folders_unknown: bool,
    /// The daemon refused the last attempt to connect for its protocol.
    pub version_gap: Option<VersionGap>,
    /// The update of this machine's daemon, under way or failed.
    pub update: Option<DaemonUpdate>,
}

impl MachineState {
    pub fn new(daemon: DaemonLabel) -> Self {
        Self {
            daemon,
            connection: ConnectionState::Connecting { attempt: 1 },
            daemon_version: None,
            plan_usage: PlanUsage::default(),
            settings: Settings::default(),
            accounts: Vec::new(),
            other_plan_usage: BTreeMap::new(),
            sign_ins: SignIns::default(),
            sessions: Loadable::NotRequested,
            folders_unknown: false,
            version_gap: None,
            update: None,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connection == ConnectionState::Connected
    }
}

/// The machines the app reaches: this computer, always, and the others it names.
#[derive(Debug, Clone, PartialEq)]
pub struct Machines {
    pub this: MachineState,
    pub others: BTreeMap<MachineId, MachineState>,
}

impl Machines {
    pub fn get(&self, machine: MachineId) -> Option<&MachineState> {
        if machine == MachineId::THIS {
            Some(&self.this)
        } else {
            self.others.get(&machine)
        }
    }

    pub fn get_mut(&mut self, machine: MachineId) -> Option<&mut MachineState> {
        if machine == MachineId::THIS {
            Some(&mut self.this)
        } else {
            self.others.get_mut(&machine)
        }
    }
}

/// A machine's header in the sidebar, and the rows under it.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarGroup<'state> {
    pub machine: MachineId,
    pub rows: Vec<&'state AgentEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppState {
    pub machines: Machines,
    pub agents: BTreeMap<AgentId, AgentEntry>,
    pub ui: UiState,
    /// The choices the app saves on this machine.
    pub prefs: Prefs,
    /// The app's notification level from the config file, until the window saves one. Never
    /// saved.
    pub config_notifications: NotificationLevel,
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
            machines: Machines {
                this: MachineState::new(daemon),
                others: BTreeMap::new(),
            },
            agents: BTreeMap::new(),
            ui: UiState::default(),
            prefs: Prefs::default(),
            config_notifications: NotificationLevel::All,
        }
    }

    /// The sidebar's order, which the user sets and the daemon keeps.
    pub fn agents_in_order(&self) -> Vec<&AgentEntry> {
        let mut agents: Vec<&AgentEntry> = self.agents.values().collect();
        agents.sort_by(|left, right| {
            MachineId::of(left.id())
                .cmp(&MachineId::of(right.id()))
                .then_with(|| list_order(left.summary.place(), right.summary.place()))
        });
        agents
    }

    /// The machines the sidebar heads, with the rows each shows: this computer first, then the
    /// others in the config's order. Empty while the app reaches this computer alone, which
    /// needs no header.
    pub fn sidebar_groups(&self) -> Vec<SidebarGroup<'_>> {
        if self.machines.others.is_empty() {
            return Vec::new();
        }
        let shown = self.sidebar_agents();
        let machines = std::iter::once(MachineId::THIS).chain(self.machines.others.keys().copied());
        machines
            .map(|machine| {
                let rows = shown
                    .iter()
                    .copied()
                    .filter(|entry| MachineId::of(entry.id()) == machine)
                    .collect();
                SidebarGroup { machine, rows }
            })
            .collect()
    }

    /// The machine of the selected agent, else this computer.
    pub fn selected_machine(&self) -> MachineId {
        self.ui.selected.map_or(MachineId::THIS, MachineId::of)
    }

    /// What the app knows of the selected agent's machine.
    pub fn selected_machine_state(&self) -> &MachineState {
        self.machines
            .get(self.selected_machine())
            .unwrap_or(&self.machines.this)
    }

    /// The name of the daemon machine `agent` is on, which keys the agent's preferences. `None`
    /// for a machine the app no longer knows.
    pub fn daemon_machine(&self, agent: AgentId) -> Option<&str> {
        let machine = self.machines.get(MachineId::of(agent))?;
        Some(&machine.daemon.machine)
    }

    /// Whether the app is connected to the daemon of the machine `agent` is on.
    pub fn reaches_agent(&self, agent: AgentId) -> bool {
        self.machines
            .get(MachineId::of(agent))
            .is_some_and(MachineState::is_connected)
    }

    /// The agents the sidebar shows, in its order: those its status filter keeps.
    pub fn sidebar_agents(&self) -> Vec<&AgentEntry> {
        let filter = self.prefs.sidebar.filter;
        let mut agents = self.agents_in_order();
        agents.retain(|entry| filter.admits(&entry.summary));
        agents
    }

    /// How many agents `filter` keeps, for its chip.
    pub fn filter_count(&self, filter: StatusFilter) -> usize {
        self.agents
            .values()
            .filter(|entry| filter.admits(&entry.summary))
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
            .is_some_and(|entry| !filter.admits(&entry.summary))
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

    /// Keeps the selection on an agent that exists, or picks the sidebar's first row, which it
    /// marks read. When the filter hides every agent, it picks the first of the whole order.
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
            // An agent you marked unread while it was selected stays unread: only a new selection
            // reads it.
            if let Some(selected) = first {
                self.ui.unread.remove(&selected);
            }
        }
    }

    /// Folders where agents ran on `machine`, with their last use and the account of that use,
    /// most recent first, for the new agent dialog. One folder written two ways, as `C:\Work` and
    /// `c:/work`, shows once.
    pub fn recent_folders(&self, machine: MachineId) -> Vec<RecentFolder> {
        let mut uses: Vec<RecentFolder> = Vec::new();
        let agent_uses = self
            .agents
            .values()
            .filter(|entry| MachineId::of(entry.id()) == machine)
            .map(|entry| {
                let summary = &entry.summary;
                (&summary.folder, summary.started_at, summary.account)
            });
        let session_uses = self
            .machines
            .get(machine)
            .into_iter()
            .flat_map(|known| known.sessions.loaded())
            .flatten()
            .map(|session: &PastSession| (&session.folder, session.last_activity, session.account));
        for (folder, moment, account) in agent_uses.chain(session_uses) {
            let raw = folder.to_string_lossy();
            let style = PathStyle::of_text(&raw);
            let text = style.shown(&raw);
            let seen = uses
                .iter_mut()
                .find(|seen| style.same_path(&seen.folder.to_string_lossy(), &text));
            match seen {
                Some(seen) if moment > seen.used => {
                    seen.used = moment;
                    seen.account = account;
                }
                Some(_) => {}
                None => uses.push(RecentFolder {
                    folder: PathBuf::from(text.as_ref()),
                    used: moment,
                    account,
                }),
            }
        }
        uses.sort_by(|left, right| {
            right
                .used
                .cmp(&left.used)
                .then_with(|| left.folder.cmp(&right.folder))
        });
        uses
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use cockpit_client::{SessionUpdate, Welcome};
    use cockpit_protocol::{
        AnswerState, Approval, ApprovalAnswer, ApprovalDecision, Catchup, CostUsd, EffortSource,
        Mark, PendingId, PermissionMode, Position, SignInWait,
    };

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
            effort_source: EffortSource::Chosen,
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
            account: AccountId::DEFAULT,
            sign_in_wait: SignInWait::NotWaiting,
            background_running: 0,
            mark: Mark::Unmarked,
        }
    }

    pub(crate) fn state_with(summaries: Vec<AgentSummary>) -> AppState {
        let mut state = AppState::new(DaemonLabel::ssh("build-box".to_owned()));
        for summary in summaries {
            state.agents.insert(summary.id, AgentEntry::new(summary));
        }
        state
    }

    /// A daemon of release 0.4.2 accepted the connection with `catchup`.
    pub(crate) fn connected(catchup: Catchup) -> SessionUpdate {
        SessionUpdate::Connected(Welcome {
            daemon_version: ReleaseVersion::new("0.4.2".to_owned()),
            catchup,
        })
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

    const CALL: u32 = 4;

    fn bash_call(status: ToolCallStatus) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(CALL),
            parent: None,
            body: ItemBody::ToolCall(cockpit_protocol::ToolCall {
                tool: "Bash".to_owned(),
                summary: "cargo build".to_owned(),
                input: serde_json::Value::Null,
                status,
                output: None,
                exit_code: None,
                edit: None,
            }),
        }
    }

    fn approval_of_the_call(answer: AnswerState<ApprovalAnswer>) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(CALL + 1),
            parent: None,
            body: ItemBody::Approval(Approval {
                pending: PendingId::new("approval-1".to_owned()),
                tool: "Bash".to_owned(),
                summary: "cargo build".to_owned(),
                input: serde_json::Value::Null,
                edit: None,
                reason: None,
                answer,
                tool_call: Some(ItemId::new(CALL)),
            }),
        }
    }

    #[test]
    fn a_running_call_keeps_when_it_started_until_it_ends() {
        let mut entry = AgentEntry::new(summary(1, AgentStatus::Working));
        let start = Instant::now();
        let later = start + Duration::from_secs(5);

        entry.note_call(&bash_call(ToolCallStatus::Running), start);
        entry.note_call(&bash_call(ToolCallStatus::Running), later);
        assert_eq!(
            entry.calls_running_since.get(&ItemId::new(CALL)),
            Some(&start)
        );

        entry.note_call(&bash_call(ToolCallStatus::Done), later);
        assert!(entry.calls_running_since.is_empty());
    }

    #[test]
    fn a_call_that_waits_for_approval_counts_from_the_answer() {
        let mut entry = AgentEntry::new(summary(1, AgentStatus::Working));
        let start = Instant::now();
        let answered = start + Duration::from_secs(90);
        let running = bash_call(ToolCallStatus::Running);
        entry.note_call(&running, start);
        entry.transcript.append(running);

        entry.note_call(&approval_of_the_call(AnswerState::Waiting), start);
        assert!(entry.calls_running_since.is_empty());

        let allow = ApprovalAnswer {
            decision: ApprovalDecision::Allow,
            message: None,
        };
        let answer = approval_of_the_call(AnswerState::Answered(allow));
        entry.note_call(&answer, answered);
        assert_eq!(
            entry.calls_running_since.get(&ItemId::new(CALL)),
            Some(&answered)
        );
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
            .recent_folders(MachineId::THIS)
            .into_iter()
            .map(|recent| recent.folder)
            .collect();
        assert_eq!(
            folders,
            [PathBuf::from("/work/shared"), PathBuf::from("/work/other")]
        );
    }

    #[test]
    fn a_windows_folder_and_its_verbatim_form_are_one_recent_folder() {
        let mut plain = summary(1, AgentStatus::Idle);
        plain.folder = PathBuf::from(r"D:\Images\Screenshots");
        let mut verbatim = summary(2, AgentStatus::Idle);
        verbatim.folder = PathBuf::from(r"\\?\D:\Images\Screenshots");
        let state = state_with(vec![plain, verbatim]);

        let folders: Vec<PathBuf> = state
            .recent_folders(MachineId::THIS)
            .into_iter()
            .map(|recent| recent.folder)
            .collect();

        assert_eq!(folders, [PathBuf::from(r"D:\Images\Screenshots")]);
    }

    #[test]
    fn the_turn_counts_from_the_daemon_start_when_it_sends_one() {
        let daemon_start = Timestamp::from_unix_millis(1_000_000);
        let seen = Instant::now();
        let mut entry = AgentEntry::new(summary(1, AgentStatus::Working));
        assert_eq!(entry.turn_start(), None);

        entry.turn_started = Some(seen);
        assert_eq!(entry.turn_start(), Some(TurnStart::Seen(seen)));

        entry.summary.turn_started_at = Some(daemon_start);
        assert_eq!(entry.turn_start(), Some(TurnStart::Daemon(daemon_start)));

        // A turn already running when the app connected.
        entry.turn_started = None;
        assert_eq!(entry.turn_start(), Some(TurnStart::Daemon(daemon_start)));
    }

    #[test]
    fn the_daemon_turn_start_reads_on_the_daemon_clock() {
        let start = TurnStart::Daemon(Timestamp::from_unix_millis(1_000_000));
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_252);

        assert_eq!(start.elapsed(now), Duration::from_secs(252));
    }
}
