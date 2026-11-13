//! The hub's state, its loop, and how an event goes out.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use claude_protocol::InputLine;
use cockpit_protocol::{
    AccountId, AccountPlanUsage, AgentDetails, AgentId, Event, EventRecord, Frame, PlanUsage,
    Prompt, PromptId, Seq, ServerMessage, SignIn, Snapshot, Timestamp, WaitingPrompt,
};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::daemon_config::DaemonConfig;
use crate::hub::folders::PendingMove;
use crate::hub::input::{ControlPurpose, HubInput, Joined, Registration};
use crate::hub::restore::Restored;
use crate::machine_load::LoadSampler;
use crate::process::ProcessHandle;
use crate::store::{
    AgentRecord, AgentsFile, DaemonSettings, DetailsFrame, EventLogs, LogName, StateDir,
};
use crate::transcript::AgentView;

/// Live events a client's queue holds. A client this far behind is dropped, and catches up when
/// it reconnects.
const CLIENT_QUEUE: usize = 4096;

/// How often the hub checks whether each account signed in or out: a file's metadata per
/// account.
const SIGN_IN_CHECK: Duration = Duration::from_secs(3);
/// How often the hub looks for working agents that turned quiet.
const QUIET_CHECK: Duration = Duration::from_secs(1);

/// The least time between two details events of one agent. A subagent's burst of tool calls
/// changes the details many times a second, and each event carries them whole.
const DETAILS_SPACING: Duration = Duration::from_millis(200);

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
    /// Whether each account was signed in at the last periodic check.
    pub(super) sign_ins: BTreeMap<AccountId, SignIn>,
    /// A periodic check of the accounts' folders runs.
    pub(super) checking_sign_ins: bool,
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
    /// The folder `agent.change_folder` moves the agent to once its process exits.
    pub(super) moving: Option<PendingMove>,
    pub(super) details_out: DetailsOut,
}

/// How an agent's details go out.
#[derive(Default)]
pub(super) struct DetailsOut {
    /// The last `DetailsChanged` frame, which a replay sends in place of the older ones.
    latest: Option<DetailsFrame>,
    /// When the last details event went out.
    sent_at: Option<Instant>,
    /// The waiting prompts of the last details event.
    sent_waiting: Vec<WaitingPrompt>,
    /// A change that waits for [`DETAILS_SPACING`] to pass since the last one. A later change
    /// replaces it.
    held: Option<AgentDetails>,
}

/// What becomes of a details change.
#[derive(Debug, PartialEq)]
enum Spaced {
    Send(AgentDetails),
    /// It waits. The first change held asks to be woken in `wake_in`; a later one replaces it.
    Held {
        wake_in: Option<Duration>,
    },
}

impl DetailsOut {
    /// Sends a change at once, unless the last one went out less than [`DETAILS_SPACING`] before
    /// `now`. A change of the waiting prompts goes out at once, in its place among the transcript
    /// events: a client matches the two to show a message once.
    fn space(&mut self, details: AgentDetails, now: Instant) -> Spaced {
        let waiting_changed = details.waiting_prompts != self.sent_waiting;
        let wait = self
            .sent_at
            .map(|sent| DETAILS_SPACING.saturating_sub(now.saturating_duration_since(sent)))
            .filter(|wait| !wait.is_zero() && !waiting_changed);
        let Some(wait) = wait else {
            self.sent(&details, now);
            return Spaced::Send(details);
        };
        let first = self.held.replace(details).is_none();
        Spaced::Held {
            wake_in: first.then_some(wait),
        }
    }

    /// The change held, which goes out at `now`.
    fn take_held(&mut self, now: Instant) -> Option<AgentDetails> {
        let details = self.held.take()?;
        self.sent(&details, now);
        Some(details)
    }

    fn sent(&mut self, details: &AgentDetails, now: Instant) {
        self.sent_at = Some(now);
        self.held = None;
        self.sent_waiting.clone_from(&details.waiting_prompts);
    }
}

pub(super) struct QueuedPrompt {
    /// `None` for a prompt the transcript does not show, such as `/effort high`.
    pub(super) id: Option<PromptId>,
    pub(super) line: InputLine,
    pub(super) prompt: Prompt,
}

impl Hub {
    pub(crate) fn new(
        state: StateDir,
        config: DaemonConfig,
        restored: Restored,
        inputs: mpsc::Sender<HubInput>,
    ) -> Self {
        let logs = EventLogs::new(state.events(), state.details());
        let mut details = restored.details;
        let agents: BTreeMap<AgentId, Agent> = restored
            .agents
            .into_iter()
            .map(|(record, view)| {
                let latest = details.remove(&record.id).map(|latest| DetailsFrame {
                    seq: latest.seq,
                    frame: latest.frame,
                });
                let agent = Agent {
                    record,
                    view,
                    process: None,
                    generation: 0,
                    queue: VecDeque::new(),
                    moving: None,
                    details_out: DetailsOut {
                        latest,
                        ..DetailsOut::default()
                    },
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
            sign_ins: BTreeMap::new(),
            checking_sign_ins: false,
        }
    }

    /// Resumes the agents that were running, then serves inputs until the daemon stops. Every
    /// [`SIGN_IN_CHECK`], it checks whether each account signed in or out.
    pub(crate) async fn run(mut self, mut inputs: mpsc::Receiver<HubInput>) {
        self.start_restored_agents();
        let mut checks = tokio::time::interval(SIGN_IN_CHECK);
        checks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut quiet = tokio::time::interval(QUIET_CHECK);
        quiet.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                input = inputs.recv() => match input {
                    Some(input) => self.handle(input),
                    None => return,
                },
                _ = checks.tick() => self.check_sign_ins(),
                _ = quiet.tick() => self.check_quiet(),
            }
        }
    }

    /// Marks the working agents that turned quiet, and publishes each one that did.
    fn check_quiet(&mut self) {
        let now = now();
        let quiet: Vec<AgentId> = self
            .agents
            .iter_mut()
            .filter_map(|(id, agent)| agent.view.check_quiet(now).then_some(*id))
            .collect();
        for agent in quiet {
            self.publish(agent);
        }
    }

    pub(super) fn handle(&mut self, input: HubInput) {
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
            HubInput::ForkCopied { fork, found, reply } => self.fork_copied(*fork, found, reply),
            HubInput::MoveCopied {
                agent,
                folder,
                restart,
                copied,
                reply,
            } => self.move_copied(agent, folder, restart, copied, reply),
            HubInput::AccountsChecked {
                accounts,
                logins,
                reply,
            } => self.accounts_checked(accounts, &logins, reply),
            HubInput::SignInsChecked(check) => self.sign_ins_checked(check),
            HubInput::RestoredCost {
                agent,
                generation,
                cost,
            } => self.restored_cost(agent, generation, cost),
            HubInput::DetailsDue { agent } => self.send_held_details(agent),
        }
    }

    fn register(&mut self, last_seq: Option<Seq>) -> Registration {
        let (sender, frames) = mpsc::channel(CLIENT_QUEUE);
        let joined = match last_seq {
            Some(after) if self.can_replay(after) => Joined::Replay {
                after,
                upto: self.last_seq,
                events: self.logs.folder().to_owned(),
                details: self
                    .agents
                    .values()
                    .filter_map(|agent| agent.details_out.latest.clone())
                    .filter(|latest| latest.seq > after)
                    .collect(),
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
        let other_plan_usage: Vec<AccountPlanUsage> = accounts
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

    /// Numbers an event, logs it, and sends it to every client. An agent's details go to its
    /// details file instead of its log.
    pub(super) fn emit(&mut self, log: LogName, event: Event) {
        let details_of = match (log, &event) {
            (LogName::Agent(agent), Event::DetailsChanged { .. }) => Some(agent),
            _ => None,
        };
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
        let stored = match details_of {
            Some(agent) => {
                if let Some(found) = self.agents.get_mut(&agent) {
                    found.details_out.latest = Some(DetailsFrame {
                        seq,
                        frame: frame.as_bytes().to_vec(),
                    });
                }
                self.logs.save_details(agent, &frame)
            }
            None => self.logs.append(log, &frame),
        };
        if let Err(error) = stored {
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

    /// Emits what changed in an agent's view. A details change waits when the last one went out
    /// less than [`DETAILS_SPACING`] ago.
    pub(super) fn publish(&mut self, agent: AgentId) {
        let Some(found) = self.agents.get_mut(&agent) else {
            return;
        };
        let events = found.view.take_events();
        for event in events {
            let event = match event {
                Event::DetailsChanged { agent, details } => {
                    match self.space_details(agent, details) {
                        Some(details) => Event::DetailsChanged { agent, details },
                        None => continue,
                    }
                }
                event => event,
            };
            self.emit(LogName::Agent(agent), event);
        }
    }

    /// The details to send now, or `None` when they wait for [`DETAILS_SPACING`] to pass since the
    /// last ones. Waiting details replace those that waited before.
    fn space_details(&mut self, agent: AgentId, details: AgentDetails) -> Option<AgentDetails> {
        let found = self.agents.get_mut(&agent)?;
        // Without a runtime, as in some tests, nothing would send them later.
        let Ok(runtime) = Handle::try_current() else {
            found.details_out.sent(&details, Instant::now());
            return Some(details);
        };
        match found.details_out.space(details, Instant::now()) {
            Spaced::Send(details) => Some(details),
            Spaced::Held { wake_in } => {
                if let Some(wait) = wake_in {
                    let inputs = self.inputs.clone();
                    runtime.spawn(async move {
                        tokio::time::sleep(wait).await;
                        // The hub may have stopped meanwhile.
                        let _ = inputs.send(HubInput::DetailsDue { agent }).await;
                    });
                }
                None
            }
        }
    }

    /// Sends the details change that waited, if any.
    fn send_held_details(&mut self, agent: AgentId) {
        let Some(found) = self.agents.get_mut(&agent) else {
            return;
        };
        let Some(details) = found.details_out.take_held(Instant::now()) else {
            return;
        };
        self.emit(
            LogName::Agent(agent),
            Event::DetailsChanged { agent, details },
        );
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
        Self::for_tests_with_inputs(folder).0
    }

    /// A hub for tests, and the inputs its tasks send it, for the test to hand back.
    #[cfg(test)]
    pub(super) fn for_tests_with_inputs(
        folder: &std::path::Path,
    ) -> (Self, mpsc::Receiver<HubInput>) {
        let state = StateDir::create(folder).unwrap();
        let restored = crate::hub::restore(&state, &folder.join("claude")).unwrap();
        let config = DaemonConfig {
            state_dir: folder.to_owned(),
            claude_program: folder.join("no-claude"),
            claude_config_dir: folder.join("claude"),
        };
        let (inputs, received) = mpsc::channel(16);
        (Self::new(state, config, restored, inputs), received)
    }
}

pub(super) fn now() -> Timestamp {
    Timestamp::from_system_time(SystemTime::now()).unwrap_or(Timestamp::from_unix_millis(0))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use cockpit_protocol::{
        Mark, Position, PromptId, Settings, TaskListItem, TaskStatus, WaitingState,
    };

    use crate::store::{ResumeAs, for_each_event};

    use super::*;

    /// A state folder at `folder` with one stopped agent, 1.
    fn state_with_agent(folder: &Path) -> AgentId {
        let state = StateDir::create(folder).unwrap();
        let agent = AgentId::new(1);
        AgentsFile {
            next_agent: AgentId::new(2),
            replay_floor: None,
            agents: vec![AgentRecord {
                id: agent,
                title: "Fix the cart".to_owned(),
                folder: folder.join("work"),
                session: None,
                model: None,
                effort: None,
                mode: None,
                running: false,
                started_at: Timestamp::from_unix_millis(0),
                past: None,
                position: Position::default(),
                process: None,
                account: AccountId::DEFAULT,
                mark: Mark::Unmarked,
                resume_as: ResumeAs::Same,
            }],
        }
        .save(&state.agents_file())
        .unwrap();
        agent
    }

    /// The seq and first task of each details frame a replay after `after` sends.
    fn replayed_details(hub: &mut Hub, after: u64) -> Vec<(u64, String)> {
        let Joined::Replay { details, .. } = hub.register(Some(Seq::new(after))).joined else {
            panic!("expected a replay");
        };
        details
            .into_iter()
            .map(|latest| {
                let Ok(ServerMessage::Event(EventRecord {
                    event: Event::DetailsChanged { details, .. },
                    ..
                })) = serde_json::from_slice(&latest.frame)
                else {
                    panic!("expected a details frame");
                };
                (latest.seq.get(), details.tasks[0].content.clone())
            })
            .collect()
    }

    #[test]
    fn details_stay_out_of_the_log_and_a_replay_or_a_restart_gets_the_latest() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let agent = state_with_agent(&folder);
        let mut hub = Hub::for_tests(&folder);
        let details = |task: &str| Event::DetailsChanged {
            agent,
            details: with_task(task),
        };

        hub.emit(LogName::Agent(agent), details("one"));
        hub.emit(LogName::Daemon, Event::SettingsChanged(Settings::default()));
        hub.emit(LogName::Agent(agent), details("two"));
        let live = replayed_details(&mut hub, 0);
        let mut logged = Vec::new();
        for_each_event(&hub.logs.path(LogName::Agent(agent)), |record| {
            logged.push(record.seq.get());
        })
        .unwrap();
        let mut restarted = Hub::for_tests(&folder);

        assert_eq!(live, [(3, "two".to_owned())]);
        assert!(logged.is_empty(), "the log holds details: {logged:?}");
        assert_eq!(restarted.last_seq, Some(Seq::new(3)));
        assert_eq!(replayed_details(&mut restarted, 2), [(3, "two".to_owned())]);
        assert!(replayed_details(&mut restarted, 3).is_empty());
    }

    #[tokio::test]
    async fn a_held_details_change_goes_out_when_it_is_due() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let agent = state_with_agent(&folder);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&folder);

        let first = hub.space_details(agent, with_task("one"));
        let second = hub.space_details(agent, with_task("two"));
        let due = inputs.recv().await.unwrap();
        hub.handle(due);

        assert_eq!(first, Some(with_task("one")));
        assert_eq!(second, None);
        assert_eq!(replayed_details(&mut hub, 0), [(1, "two".to_owned())]);
    }

    fn with_task(task: &str) -> AgentDetails {
        AgentDetails {
            tasks: vec![TaskListItem {
                content: task.to_owned(),
                status: TaskStatus::Pending,
                active_form: String::new(),
            }],
            ..AgentDetails::default()
        }
    }

    #[test]
    fn changes_close_together_wait_and_the_last_one_goes_out() {
        let start = Instant::now();
        let mut out = DetailsOut::default();

        let first = out.space(with_task("one"), start);
        let second = out.space(with_task("two"), start + Duration::from_millis(50));
        let third = out.space(with_task("three"), start + Duration::from_millis(80));
        let held = out.take_held(start + DETAILS_SPACING);
        let later = out.space(with_task("four"), start + DETAILS_SPACING + DETAILS_SPACING);

        assert_eq!(first, Spaced::Send(with_task("one")));
        assert_eq!(
            second,
            Spaced::Held {
                wake_in: Some(Duration::from_millis(150))
            }
        );
        assert_eq!(third, Spaced::Held { wake_in: None });
        assert_eq!(held, Some(with_task("three")));
        assert_eq!(later, Spaced::Send(with_task("four")));
    }

    #[test]
    fn a_change_of_the_waiting_prompts_goes_out_at_once() {
        let start = Instant::now();
        let mut out = DetailsOut::default();
        let waiting = AgentDetails {
            waiting_prompts: vec![WaitingPrompt {
                id: PromptId::new(1),
                text: "Then run the tests".to_owned(),
                image_count: 0,
                state: WaitingState::Sent,
                sent_at: None,
            }],
            ..AgentDetails::default()
        };

        out.space(AgentDetails::default(), start);
        let changed = out.space(waiting.clone(), start + Duration::from_millis(10));

        assert_eq!(changed, Spaced::Send(waiting));
    }
}
