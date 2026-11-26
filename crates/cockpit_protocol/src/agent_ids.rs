//! Rewrites the agent ids a message carries. An app that reaches several daemons gives each
//! daemon's agents ids of their own, and translates them as messages come and go.

use crate::event::Event;
use crate::ids::AgentId;
use crate::machine_load::MachineLoad;
use crate::request::Request;
use crate::response::ResponseBody;
use crate::session::Snapshot;
use crate::terminal::TerminalUpdate;

impl Request {
    /// The agent the request is about, if any. `agent.move` names the agent it moves.
    pub fn agent_mut(&mut self) -> Option<&mut AgentId> {
        let agent = match self {
            Self::AgentPrompt(params) => &mut params.agent,
            Self::AgentCancelQueued(params) | Self::AgentSendQueued(params) => &mut params.agent,
            Self::AgentAnswer(params) => &mut params.agent,
            Self::AgentInterrupt(params)
            | Self::AgentStop(params)
            | Self::AgentResume(params)
            | Self::AgentRemove(params)
            | Self::AgentClose(params)
            | Self::AgentDetails(params)
            | Self::AgentContextUsage(params)
            | Self::AgentMcpStatus(params)
            | Self::AgentCommands(params)
            | Self::AgentReloadSkills(params)
            | Self::AgentReloadPlugins(params)
            | Self::AgentGitStatus(params)
            | Self::AgentTerminals(params)
            | Self::TerminalListShared(params) => &mut params.agent,
            Self::AgentTerminalOpen(params) => &mut params.agent,
            Self::TerminalAttach(params) | Self::TerminalClose(params) => &mut params.agent,
            Self::TerminalInput(params) => &mut params.agent,
            Self::TerminalResize(params) => &mut params.agent,
            Self::TerminalShare(params) => &mut params.agent,
            Self::TerminalRename(params) => &mut params.agent,
            Self::TerminalReadText(params) => &mut params.agent,
            Self::AgentHistory(params) => &mut params.agent,
            Self::AgentSetModel(params) => &mut params.agent,
            Self::AgentSetEffort(params) => &mut params.agent,
            Self::AgentSetMode(params) => &mut params.agent,
            Self::AgentRename(params) => &mut params.agent,
            Self::AgentMark(params) => &mut params.agent,
            Self::AgentFork(params) | Self::AgentChangeFolder(params) => &mut params.agent,
            Self::AgentMove(params) => &mut params.agent,
            Self::AgentStopTask(params) => &mut params.agent,
            Self::AgentStopProcess(params) => &mut params.agent,
            Self::AgentMcpReconnect(params) => &mut params.agent,
            Self::AgentMcpToggle(params) => &mut params.agent,
            Self::AgentMcpSignIn(params) => &mut params.agent,
            Self::AgentMcpSignInCallback(params) => &mut params.agent,
            Self::AgentReadFile(params) => &mut params.agent,
            Self::AgentListDir(params) => &mut params.agent,
            Self::AgentFindFiles(params) => &mut params.agent,
            Self::AgentGitFile(params)
            | Self::AgentGitBlame(params)
            | Self::AgentGitLog(params) => &mut params.agent,
            Self::AgentGitShow(params) => &mut params.agent,
            Self::McpRemove(params) => &mut params.agent,
            Self::AgentStart(_)
            | Self::SettingsSet(_)
            | Self::SessionsList
            | Self::SessionsResume(_)
            | Self::FoldersList(_)
            | Self::MachineLoad
            | Self::AccountsList
            | Self::AccountsAdd(_)
            | Self::AccountsChange(_)
            | Self::AccountsRemove(_) => return None,
        };
        Some(agent)
    }

    /// Replaces each agent id the request carries with `map` of it.
    pub fn map_agents(&mut self, map: impl Fn(AgentId) -> AgentId) {
        if let Self::AgentMove(params) = self {
            params.before = params.before.map(&map);
        }
        if let Some(agent) = self.agent_mut() {
            *agent = map(*agent);
        }
    }
}

impl ResponseBody {
    /// Replaces each agent id the answer carries with `map` of it.
    pub fn map_agents(&mut self, map: impl Fn(AgentId) -> AgentId) {
        match self {
            Self::AgentStarted { agent } => *agent = map(*agent),
            Self::MachineLoad(load) => load.map_agents(map),
            Self::Done
            | Self::History(_)
            | Self::Details { .. }
            | Self::ContextUsage(_)
            | Self::McpServers { .. }
            | Self::McpSignIn(_)
            | Self::Commands { .. }
            | Self::SkillsReloaded { .. }
            | Self::PluginsReloaded { .. }
            | Self::TerminalOpened { .. }
            | Self::Terminals { .. }
            | Self::TerminalScreen { .. }
            | Self::SharedTerminals { .. }
            | Self::TerminalText { .. }
            | Self::File { .. }
            | Self::Image { .. }
            | Self::Pdf { .. }
            | Self::FileMatches { .. }
            | Self::IgnoredFileMatches { .. }
            | Self::FileNotFound { .. }
            | Self::Directory(_)
            | Self::GitFile(_)
            | Self::GitBlame(_)
            | Self::GitLog { .. }
            | Self::GitCommitFile(_)
            | Self::GitStatus(_)
            | Self::Sessions { .. }
            | Self::Accounts { .. }
            | Self::Account(_) => {}
        }
    }
}

impl TerminalUpdate {
    /// Replaces the agent id the update carries with `map` of it.
    pub fn map_agents(&mut self, map: impl Fn(AgentId) -> AgentId) {
        self.agent = map(self.agent);
    }
}

impl Event {
    /// Replaces each agent id the event carries with `map` of it.
    pub fn map_agents(&mut self, map: impl Fn(AgentId) -> AgentId) {
        match self {
            Self::AgentAdded(summary) | Self::AgentUpdated(summary) => {
                summary.id = map(summary.id);
            }
            Self::AgentRemoved { agent }
            | Self::TranscriptAppended { agent, .. }
            | Self::TranscriptChanged { agent, .. }
            | Self::DetailsChanged { agent, .. }
            | Self::DetailsUpdated { agent, .. }
            | Self::CommandsChanged { agent, .. } => *agent = map(*agent),
            Self::PlanUsageChanged(_)
            | Self::SettingsChanged(_)
            | Self::AccountPlanUsageChanged(_)
            | Self::AccountsChanged { .. } => {}
        }
    }
}

impl Snapshot {
    /// Replaces each agent id the snapshot carries with `map` of it.
    pub fn map_agents(&mut self, map: impl Fn(AgentId) -> AgentId) {
        for summary in &mut self.agents {
            summary.id = map(summary.id);
        }
    }
}

impl MachineLoad {
    /// Replaces each agent id the load carries with `map` of it.
    pub fn map_agents(&mut self, map: impl Fn(AgentId) -> AgentId) {
        for agent in &mut self.agents {
            agent.agent = map(agent.agent);
        }
        for agent in &mut self.history.agents {
            agent.agent = map(agent.agent);
        }
    }
}
