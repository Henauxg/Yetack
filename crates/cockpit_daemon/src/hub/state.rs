//! The hub's state, its loop, and how an event goes out.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::SystemTime;

use claude_protocol::InputLine;
use cockpit_protocol::{
    AccountId, AccountPlanUsage, AgentId, Event, EventRecord, Frame, ItemId, PlanUsage, Seq,
    ServerMessage, Snapshot, Timestamp,
};
use tokio::sync::mpsc;

use crate::daemon_config::DaemonConfig;
use crate::hub::input::{ControlPurpose, HubInput, Joined, Registration};
use crate::hub::restore::Restored;
use crate::machine_load::LoadSampler;
use crate::process::ProcessHandle;
use crate::store::{AgentRecord, AgentsFile, DaemonSettings, EventLogs, LogName, StateDir};
use crate::transcript::AgentView;

/// Live events a client's queue holds. A client this far behind is dropped, and catches up when
/// it reconnects.
const CLIENT_QUEUE: usize = 4096;

/// Owns every agent and the daemon's state. Only its task touches them.
pub(crate) struct Hub {
    pub(super) state: StateDir,
    pub(super) config: DaemonConfig,
    pub(super) agents: BTreeMap<AgentId, Agent>,
    pub(super) next_agent: AgentId,
    pub(super) replay_floor: Option<Seq>,
    pub(super) settings: DaemonSettings,
    /// Each account's plan usage, once one of its agents reported it.
    pub(super) plan_usage: BTreeMap<AccountId, PlanUsage>,
    pub(super) logs: EventLogs,
    pub(super) last_seq: Option<Seq>,
    pub(super) clients: Vec<mpsc::Sender<Frame>>,
    /// For the process tasks and the helper tasks, which report to the hub.
    pub(super) inputs: mpsc::Sender<HubInput>,
    /// Samples the machine's load while clients ask for it.
    pub(super) load: LoadSampler,
}

pub(super) struct Agent {
    pub(super) record: AgentRecord,
    pub(super) view: AgentView,
    /// `None` while the agent is exited. A stopped agent keeps it, with its stdin closed and
    /// `record.running` false, until the process ends.
    pub(super) process: Option<ProcessHandle<ControlPurpose>>,
    /// Counts the agent's processes, so that a late event of an old one is ignored.
    pub(super) generation: u32,
    /// Prompts held while the agent works, sent one per turn.
    pub(super) queue: VecDeque<QueuedPrompt>,
}

pub(super) struct QueuedPrompt {
    /// `None` for a prompt the transcript does not show, such as `/effort high`.
    pub(super) item: Option<ItemId>,
    pub(super) line: InputLine,
    pub(super) text: String,
}

impl Hub {
    pub(crate) fn new(
        state: StateDir,
        config: DaemonConfig,
        restored: Restored,
        inputs: mpsc::Sender<HubInput>,
    ) -> Self {
        let logs = EventLogs::new(state.events());
        let agents: BTreeMap<AgentId, Agent> = restored
            .agents
            .into_iter()
            .map(|(record, view)| {
                let agent = Agent {
                    record,
                    view,
                    process: None,
                    generation: 0,
                    queue: VecDeque::new(),
                };
                (agent.record.id, agent)
            })
            .collect();
        Self {
            state,
            config,
            agents,
            next_agent: restored.next_agent,
            replay_floor: restored.replay_floor,
            settings: restored.settings,
            plan_usage: restored.plan_usage,
            logs,
            last_seq: restored.last_seq,
            clients: Vec::new(),
            inputs,
            load: LoadSampler::default(),
        }
    }

    /// Resumes the agents that were running, then serves inputs until the daemon stops.
    pub(crate) async fn run(mut self, mut inputs: mpsc::Receiver<HubInput>) {
        self.start_restored_agents();
        while let Some(input) = inputs.recv().await {
            self.handle(input);
        }
    }

    fn handle(&mut self, input: HubInput) {
        match input {
            HubInput::Connect { last_seq, reply } => {
                let registration = self.register(last_seq);
                // The connection may have closed meanwhile.
                let _ = reply.send(registration);
            }
            HubInput::Request { request, reply } => self.request(request, reply),
            HubInput::Process {
                agent,
                generation,
                event,
            } => self.process_event(agent, generation, event),
            HubInput::SessionFound { found, reply } => self.resume_session(*found, reply),
            HubInput::RestoredCost {
                agent,
                generation,
                cost,
            } => self.restored_cost(agent, generation, cost),
        }
    }

    fn register(&mut self, last_seq: Option<Seq>) -> Registration {
        let (sender, frames) = mpsc::channel(CLIENT_QUEUE);
        let joined = match last_seq {
            Some(after) if self.can_replay(after) => Joined::Replay {
                after,
                upto: self.last_seq,
                events: self.logs.folder().to_owned(),
            },
            Some(_) | None => Joined::Snapshot(Box::new(self.snapshot())),
        };
        self.clients.push(sender);
        Registration { joined, frames }
    }

    /// Whether the logs hold every event after `after`.
    fn can_replay(&self, after: Seq) -> bool {
        // A client ahead of the daemon saw another state folder.
        let known = self.last_seq.is_some_and(|last| after <= last);
        let kept = self.replay_floor.is_none_or(|floor| after >= floor);
        known && kept
    }

    pub(super) fn snapshot(&self) -> Snapshot {
        let accounts = self.accounts();
        let other_plan_usage = accounts
            .iter()
            .filter(|account| !account.id.is_default())
            .filter_map(|account| {
                let usage = self.plan_usage.get(&account.id)?.clone();
                Some(AccountPlanUsage {
                    account: account.id,
                    usage,
                })
            })
            .collect();
        Snapshot {
            agents: self
                .agents
                .values()
                .map(|agent| agent.view.summary().clone())
                .collect(),
            plan_usage: self
                .plan_usage
                .get(&AccountId::DEFAULT)
                .cloned()
                .unwrap_or_default(),
            settings: self.settings.shared.clone(),
            last_seq: self.last_seq,
            accounts,
            other_plan_usage,
        }
    }

    /// Numbers an event, logs it, and sends it to every client.
    pub(super) fn emit(&mut self, log: LogName, event: Event) {
        let seq = match self.last_seq {
            Some(last) => last.next(),
            None => Some(Seq::new(1)),
        };
        let Some(seq) = seq else {
            tracing::error!("no sequence number left: an event is lost");
            return;
        };
        let frame = match Frame::encode(&ServerMessage::Event(EventRecord { seq, event })) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::error!(%error, "cannot encode an event: it is lost");
                return;
            }
        };
        self.last_seq = Some(seq);
        if let Err(error) = self.logs.append(log, &frame) {
            tracing::error!(error = %crate::hub::errors::chain(&error), "cannot log an event");
            // A client that saw less can no longer replay from the logs.
            self.replay_floor = Some(seq);
            self.save_agents();
        }
        self.clients.retain(|client| {
            let sent = client.try_send(frame.clone());
            if sent.is_err() {
                tracing::info!("dropped a client that reads too slowly or left");
            }
            sent.is_ok()
        });
    }

    /// Emits what changed in an agent's view.
    pub(super) fn publish(&mut self, agent: AgentId) {
        let Some(found) = self.agents.get_mut(&agent) else {
            return;
        };
        let events = found.view.take_events();
        for event in events {
            self.emit(LogName::Agent(agent), event);
        }
    }

    pub(super) fn save_agents(&self) {
        let file = AgentsFile {
            next_agent: self.next_agent,
            replay_floor: self.replay_floor,
            agents: self
                .agents
                .values()
                .map(|agent| agent.record.clone())
                .collect(),
        };
        if let Err(error) = file.save(&self.state.agents_file()) {
            tracing::error!(error = %crate::hub::errors::chain(&error), "cannot save the agents");
        }
    }

    pub(super) fn raw_log_path(&self, agent: AgentId) -> PathBuf {
        self.state.raw_log(agent)
    }

    /// A hub on the state folder `folder`, whose `claude` cannot start, and with no task.
    #[cfg(test)]
    pub(super) fn for_tests(folder: &std::path::Path) -> Self {
        let state = StateDir::create(folder).unwrap();
        let restored = crate::hub::restore(&state, &folder.join("claude")).unwrap();
        let config = DaemonConfig {
            state_dir: folder.to_owned(),
            claude_program: folder.join("no-claude"),
            claude_config_dir: folder.join("claude"),
        };
        let (inputs, _) = mpsc::channel(1);
        Self::new(state, config, restored, inputs)
    }
}

pub(super) fn now() -> Timestamp {
    Timestamp::from_system_time(SystemTime::now()).unwrap_or(Timestamp::from_unix_millis(0))
}
