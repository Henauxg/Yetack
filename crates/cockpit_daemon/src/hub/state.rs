//! The hub's state, its loop, and how an event goes out.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use claude_protocol::InputLine;
use cockpit_protocol::{
    AccountId, AccountPlanUsage, AgentDetails, AgentId, Event, EventRecord, Frame, PlanUsage,
    Prompt, PromptId, Seq, ServerMessage, SignIn, Snapshot, Timestamp,
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
/// changes the details many times a second.
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

/// How an agent's details go out: whole in the agent's first details event, then as updates of
/// the parts that changed.
#[derive(Default)]
pub(super) struct DetailsOut {
    /// The whole details as the last details event left them, as a `DetailsChanged` frame at
    /// that event's seq, which a replay sends in place of the details events before it.
    latest: Option<DetailsFrame>,
    /// What the clients hold since the last details event. `None` before the first one since
    /// the daemon started: a client may hold any details then, so the first one goes out whole.
    sent: Option<SentDetails>,
    /// A change that waits for [`DETAILS_SPACING`] to pass since the last one. A later change
    /// replaces it.
    held: Option<AgentDetails>,
}

/// The details of an agent's last details event.
struct SentDetails {
    details: AgentDetails,
    at: Instant,
}

/// What becomes of a details change.
#[derive(Debug, PartialEq)]
enum Spaced {
    /// It goes out now, as this event.
    Send(Box<Event>),
    /// It waits. The first change held asks to be woken in `wake_in`; a later one replaces it.
    Held { wake_in: Option<Duration> },
    /// The clients already hold these details.
    Unchanged,
}

impl DetailsOut {
    /// Sends a change at once, unless the last one went out less than [`DETAILS_SPACING`] before
    /// `now`. A change of the waiting prompts goes out at once, in its place among the transcript
    /// events: a client matches the two to show a message once.
    fn space(&mut self, agent: AgentId, details: AgentDetails, now: Instant) -> Spaced {
        let wait = self
            .sent
            .as_ref()
            .filter(|sent| sent.details.waiting_prompts == details.waiting_prompts)
            .map(|sent| DETAILS_SPACING.saturating_sub(now.saturating_duration_since(sent.at)))
            .filter(|wait| !wait.is_zero());
        let Some(wait) = wait else {
            return self.send(agent, details, now);
        };
        let first = self.held.replace(details).is_none();
        Spaced::Held {
            wake_in: first.then_some(wait),
        }
    }

    /// The change held, which goes out at `now`.
    fn take_held(&mut self, agent: AgentId, now: Instant) -> Spaced {
        match self.held.take() {
            Some(details) => self.send(agent, details, now),
            None => Spaced::Unchanged,
        }
    }

    /// The event that brings the clients from the details they hold to `details`.
    fn send(&mut self, agent: AgentId, details: AgentDetails, now: Instant) -> Spaced {
        self.held = None;
        let event = match &self.sent {
            None => Event::DetailsChanged {
                agent,
                details: details.clone(),
            },
            Some(sent) => {
                let changes = details.changes_since(&sent.details);
                if changes.is_empty() {
                    return Spaced::Unchanged;
                }
                Event::DetailsUpdated { agent, changes }
            }
        };
        self.sent = Some(SentDetails { details, at: now });
        Spaced::Send(Box::new(event))
    }
}

impl Agent {
    /// The details as the clients hold them from the last details event. Before the first one,
    /// which goes out whole, the view's.
    pub(super) fn sent_details(&self) -> &AgentDetails {
        self.details_out
            .sent
            .as_ref()
            .map_or(self.view.details(), |sent| &sent.details)
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
        let mut quiet: Vec<AgentId> = Vec::new();
        for (id, agent) in &mut self.agents {
            if agent.view.check_quiet(now) {
                quiet.push(*id);
            }
        }
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

    /// Numbers an event, logs it, and sends it to every client.
    pub(super) fn emit(&mut self, log: LogName, event: Event) {
        let Some((seq, frame)) = self.number(event) else {
            return;
        };
        if let Err(error) = self.logs.append(log, &frame) {
            tracing::error!(error = %crate::hub::errors::chain(&error), "cannot log an event");
            self.lost_from_logs(seq);
        }
        self.broadcast(&frame);
    }

    /// Numbers an agent's details event and sends it to every client. Its details file takes the
    /// whole details instead of its log, as a `DetailsChanged` frame at the event's seq.
    fn emit_details(&mut self, agent: AgentId, event: Event) {
        let Some((seq, frame)) = self.number(event) else {
            // The clients did not get the change: the next details go out whole.
            if let Some(found) = self.agents.get_mut(&agent) {
                found.details_out.sent = None;
            }
            return;
        };
        self.save_details(agent, seq);
        self.broadcast(&frame);
    }

    /// Saves an agent's whole details at `seq`, for a replay and for a restart.
    fn save_details(&mut self, agent: AgentId, seq: Seq) {
        let Some(found) = self.agents.get_mut(&agent) else {
            return;
        };
        let whole = ServerMessage::Event(EventRecord {
            seq,
            event: Event::DetailsChanged {
                agent,
                details: found.sent_details().clone(),
            },
        });
        let saved = match Frame::encode(&whole) {
            Ok(frame) => {
                found.details_out.latest = Some(DetailsFrame {
                    seq,
                    frame: frame.as_bytes().to_vec(),
                });
                self.logs
                    .save_details(agent, &frame)
                    .map_err(|error| crate::hub::errors::chain(&error))
            }
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = saved {
            tracing::error!(%error, "cannot save an agent's details");
            self.lost_from_logs(seq);
        }
    }

    /// Gives an event the next sequence number, and encodes it. `None` when it cannot go out.
    fn number(&mut self, event: Event) -> Option<(Seq, Frame)> {
        let seq = match self.last_seq {
            Some(last) => last.next(),
            None => Some(Seq::new(1)),
        };
        let Some(seq) = seq else {
            tracing::error!("no sequence number left: an event is lost");
            return None;
        };
        let frame = match Frame::encode(&ServerMessage::Event(EventRecord { seq, event })) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::error!(%error, "cannot encode an event: it is lost");
                return None;
            }
        };
        self.last_seq = Some(seq);
        Some((seq, frame))
    }

    /// The logs lack the event `seq`: a client that saw less can no longer replay from them.
    fn lost_from_logs(&mut self, seq: Seq) {
        self.replay_floor = Some(seq);
        self.save_agents();
    }

    fn broadcast(&mut self, frame: &Frame) {
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
            match event {
                Event::DetailsChanged { agent, details } => self.space_details(agent, details),
                event => self.emit(LogName::Agent(agent), event),
            }
        }
    }

    /// Sends what changed in the details now, or holds them until [`DETAILS_SPACING`] passed since
    /// the last ones. Details held replace those held before.
    fn space_details(&mut self, agent: AgentId, details: AgentDetails) {
        let Some(found) = self.agents.get_mut(&agent) else {
            return;
        };
        let now = Instant::now();
        // Without a runtime, as in some tests, nothing would send them later.
        let Ok(runtime) = Handle::try_current() else {
            if let Spaced::Send(event) = found.details_out.send(agent, details, now) {
                self.emit_details(agent, *event);
            }
            return;
        };
        match found.details_out.space(agent, details, now) {
            Spaced::Send(event) => self.emit_details(agent, *event),
            Spaced::Held {
                wake_in: Some(wait),
            } => {
                let inputs = self.inputs.clone();
                runtime.spawn(async move {
                    tokio::time::sleep(wait).await;
                    // The hub may have stopped meanwhile.
                    let _ = inputs.send(HubInput::DetailsDue { agent }).await;
                });
            }
            Spaced::Held { wake_in: None } | Spaced::Unchanged => {}
        }
    }

    /// Sends the details change that waited, if any.
    pub(super) fn send_held_details(&mut self, agent: AgentId) {
        let Some(found) = self.agents.get_mut(&agent) else {
            return;
        };
        if let Spaced::Send(event) = found.details_out.take_held(agent, Instant::now()) {
            self.emit_details(agent, *event);
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
        AgentParams, BackgroundTaskId, DetailsChange, ItemId, Mark, Position, PromptId, Request,
        ResponseBody, ResponseResult, Settings, Subagent, SubagentRun, SubagentStatus,
        SubagentToolCall, TaskListItem, TaskStatus, WaitingPrompt, WaitingState,
    };
    use tokio::sync::oneshot;

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

    /// A client of `hub`, which gets every frame it sends from now on.
    fn client(hub: &mut Hub) -> mpsc::Receiver<Frame> {
        let (client, frames) = mpsc::channel(16);
        hub.clients.push(client);
        frames
    }

    /// An event a client got.
    #[derive(Debug)]
    struct Received {
        seq: u64,
        event: Event,
        /// The size of its frame.
        bytes: usize,
    }

    fn received(frames: &mut mpsc::Receiver<Frame>) -> Vec<Received> {
        let mut received = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            let Ok(ServerMessage::Event(record)) = serde_json::from_slice(frame.as_bytes()) else {
                panic!("expected an event");
            };
            received.push(Received {
                seq: record.seq.get(),
                event: record.event,
                bytes: frame.as_bytes().len(),
            });
        }
        received
    }

    /// The events the client got.
    fn received_events(frames: &mut mpsc::Receiver<Frame>) -> Vec<Event> {
        received(frames)
            .into_iter()
            .map(|received| received.event)
            .collect()
    }

    /// The seq and the details of each details frame a replay after `after` sends.
    fn replayed_details(hub: &mut Hub, after: u64) -> Vec<(u64, AgentDetails)> {
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
                (latest.seq.get(), details)
            })
            .collect()
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

    /// The details of an agent that ran many subagents, as many as the daemon keeps.
    fn busy_details() -> AgentDetails {
        let subagents = (1..=200)
            .map(|index: u32| Subagent {
                item: ItemId::new(index * 10),
                parent: index
                    .is_multiple_of(4)
                    .then(|| ItemId::new(index * 10 - 10)),
                description: format!("Review the checkout flow, part {index}"),
                agent_type: Some("general-purpose".to_owned()),
                run: if index.is_multiple_of(3) {
                    SubagentRun::Background
                } else {
                    SubagentRun::Foreground
                },
                status: SubagentStatus::Done,
                started_at: Timestamp::from_unix_millis(1_700_000_000_000 + u64::from(index)),
                ended_at: Some(Timestamp::from_unix_millis(1_700_000_090_000)),
                tokens: Some(24_800),
                tool_calls: 37,
                last_tool: Some(SubagentToolCall {
                    item: Some(ItemId::new(index * 10 + 7)),
                    tool: "Bash".to_owned(),
                    summary: "cargo test -p shop_cart checkout::tests".to_owned(),
                }),
                error: None,
                turn: 4,
                task: Some(BackgroundTaskId::new(format!("task-{index:04}"))),
            })
            .collect();
        AgentDetails {
            subagents,
            ..with_task("Review the checkout")
        }
    }

    #[test]
    fn a_subagents_tool_call_sends_that_subagent_alone() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let agent = state_with_agent(&folder);
        let mut hub = Hub::for_tests(&folder);
        let mut frames = client(&mut hub);
        let before = busy_details();
        let mut after = before.clone();
        let busy = &mut after.subagents[150];
        busy.status = SubagentStatus::Running;
        busy.tool_calls += 1;
        busy.last_tool = Some(SubagentToolCall {
            item: Some(ItemId::new(9_001)),
            tool: "Grep".to_owned(),
            summary: "fn apply_discount".to_owned(),
        });
        let busy = busy.clone();

        hub.space_details(agent, before.clone());
        hub.space_details(agent, after.clone());

        let received = received(&mut frames);
        let [whole, update] = received.as_slice() else {
            panic!("expected two events, got {received:?}");
        };
        assert_eq!((whole.seq, update.seq), (1, 2));
        assert_eq!(
            whole.event,
            Event::DetailsChanged {
                agent,
                details: before,
            }
        );
        assert_eq!(
            update.event,
            Event::DetailsUpdated {
                agent,
                changes: vec![DetailsChange::Subagents {
                    changed: vec![busy],
                    removed: Vec::new(),
                }],
            }
        );
        let (whole, update) = (whole.bytes, update.bytes);
        assert!(whole > 50_000, "the whole details weigh {whole} bytes");
        assert!(update < 1_000, "the update weighs {update} bytes");
        assert_eq!(replayed_details(&mut hub, 1), [(2, after)]);
    }

    #[test]
    fn details_stay_out_of_the_log_and_a_replay_or_a_restart_gets_the_latest_whole() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let agent = state_with_agent(&folder);
        let mut hub = Hub::for_tests(&folder);

        hub.space_details(agent, with_task("one"));
        hub.emit(LogName::Daemon, Event::SettingsChanged(Settings::default()));
        hub.space_details(agent, with_task("two"));
        let live = replayed_details(&mut hub, 0);
        let mut logged = Vec::new();
        for_each_event(&hub.logs.path(LogName::Agent(agent)), |record| {
            logged.push(record.seq.get());
        })
        .unwrap();
        let mut restarted = Hub::for_tests(&folder);

        assert_eq!(live, [(3, with_task("two"))]);
        assert!(logged.is_empty(), "the log holds details: {logged:?}");
        assert_eq!(restarted.last_seq, Some(Seq::new(3)));
        assert_eq!(replayed_details(&mut restarted, 2), [(3, with_task("two"))]);
        assert!(replayed_details(&mut restarted, 3).is_empty());
    }

    /// A client may hold details that the restarted daemon lost, as when the details file could
    /// not be written: the whole details bring it back to the daemon's.
    #[test]
    fn the_first_details_after_a_restart_go_out_whole() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let agent = state_with_agent(&folder);
        let mut hub = Hub::for_tests(&folder);
        hub.space_details(agent, with_task("one"));
        hub.space_details(agent, with_task("two"));
        let mut restarted = Hub::for_tests(&folder);
        let mut frames = client(&mut restarted);

        restarted.space_details(agent, with_task("three"));
        restarted.space_details(agent, with_task("four"));

        let events = received_events(&mut frames);
        assert_eq!(
            events,
            [
                Event::DetailsChanged {
                    agent,
                    details: with_task("three"),
                },
                Event::DetailsUpdated {
                    agent,
                    changes: vec![DetailsChange::Tasks {
                        tasks: with_task("four").tasks,
                    }],
                },
            ]
        );
    }

    /// A change too large for a frame never reaches the clients, so the next one cannot build on
    /// it.
    #[test]
    fn after_a_change_that_cannot_go_out_the_next_details_go_out_whole() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let agent = state_with_agent(&folder);
        let mut hub = Hub::for_tests(&folder);
        let mut frames = client(&mut hub);
        let first = AgentDetails {
            subagents: busy_details().subagents.into_iter().take(2).collect(),
            ..AgentDetails::default()
        };
        let mut lost = first.clone();
        lost.subagents[0].description = "x".repeat(cockpit_protocol::MAX_LINE_BYTES);
        lost.subagents[1].tool_calls += 1;
        let mut last = lost.clone();
        last.subagents[0]
            .description
            .clone_from(&first.subagents[0].description);

        hub.space_details(agent, first.clone());
        hub.space_details(agent, lost);
        hub.space_details(agent, last.clone());

        let events = received_events(&mut frames);
        assert_eq!(
            events,
            [
                Event::DetailsChanged {
                    agent,
                    details: first,
                },
                Event::DetailsChanged {
                    agent,
                    details: last,
                },
            ]
        );
    }

    #[test]
    fn a_quiet_stretch_publishes_one_summary_however_many_checks_see_it() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let agent = state_with_agent(&folder);
        let mut hub = Hub::for_tests(&folder);
        let prompt = Prompt {
            text: "Fix the cart total".to_owned(),
            images: Vec::new(),
        };
        let started = Timestamp::from_unix_millis(1_000);
        hub.agents
            .get_mut(&agent)
            .unwrap()
            .view
            .add_prompt(&prompt, started);
        hub.publish(agent);

        hub.check_quiet();
        hub.check_quiet();

        let mut quiet = Vec::new();
        for_each_event(&hub.logs.path(LogName::Agent(agent)), |record| {
            if let Event::AgentUpdated(summary) = record.event {
                quiet.push(summary.quiet_since);
            }
        })
        .unwrap();
        assert_eq!(quiet.last(), Some(&Some(started)));
        assert_eq!(quiet.iter().filter(|since| since.is_some()).count(), 1);
    }

    #[tokio::test]
    async fn a_held_details_change_goes_out_when_it_is_due() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let agent = state_with_agent(&folder);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&folder);
        let mut frames = client(&mut hub);

        hub.space_details(agent, with_task("one"));
        hub.space_details(agent, with_task("two"));
        hub.space_details(agent, with_task("three"));
        let sent_at_once = received(&mut frames).len();
        let due = inputs.recv().await.unwrap();
        hub.handle(due);

        let events = received_events(&mut frames);
        assert_eq!(sent_at_once, 1, "the second and third changes wait");
        assert_eq!(
            events,
            [Event::DetailsUpdated {
                agent,
                changes: vec![DetailsChange::Tasks {
                    tasks: with_task("three").tasks,
                }],
            }]
        );
        assert_eq!(replayed_details(&mut hub, 0), [(2, with_task("three"))]);
    }

    /// The answer holds the details a client has from the events up to its `last_seq`, so that
    /// each later update applies to them. A change held for the spacing goes out first, so the
    /// answer is current.
    #[tokio::test]
    async fn the_details_answer_holds_what_the_events_sent() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let agent = state_with_agent(&folder);
        let mut hub = Hub::for_tests(&folder);
        let mut frames = client(&mut hub);
        hub.space_details(agent, with_task("one"));
        hub.space_details(agent, with_task("two"));
        let (reply, answer) = oneshot::channel();

        hub.request(Request::AgentDetails(AgentParams { agent }), reply);

        let seqs: Vec<u64> = received(&mut frames)
            .into_iter()
            .map(|received| received.seq)
            .collect();
        assert_eq!(seqs, [1, 2], "the held change goes out before the answer");
        assert_eq!(
            answer.await.unwrap(),
            ResponseResult::Ok(ResponseBody::Details {
                details: with_task("two"),
                last_seq: Some(Seq::new(2)),
            })
        );
    }

    fn waiting(text: &str) -> AgentDetails {
        AgentDetails {
            waiting_prompts: vec![WaitingPrompt {
                id: PromptId::new(1),
                text: text.to_owned(),
                image_count: 0,
                state: WaitingState::Sent,
                sent_at: None,
            }],
            ..AgentDetails::default()
        }
    }

    fn update(changes: Vec<DetailsChange>) -> Spaced {
        Spaced::Send(Box::new(Event::DetailsUpdated {
            agent: AgentId::new(1),
            changes,
        }))
    }

    #[test]
    fn changes_close_together_wait_and_the_last_one_goes_out() {
        let agent = AgentId::new(1);
        let start = Instant::now();
        let mut out = DetailsOut::default();

        let first = out.space(agent, with_task("one"), start);
        let second = out.space(agent, with_task("two"), start + Duration::from_millis(50));
        let third = out.space(agent, with_task("three"), start + Duration::from_millis(80));
        let held = out.take_held(agent, start + DETAILS_SPACING);
        let later = out.space(
            agent,
            with_task("four"),
            start + DETAILS_SPACING + DETAILS_SPACING,
        );

        assert_eq!(
            first,
            Spaced::Send(Box::new(Event::DetailsChanged {
                agent,
                details: with_task("one"),
            }))
        );
        assert_eq!(
            second,
            Spaced::Held {
                wake_in: Some(Duration::from_millis(150))
            }
        );
        assert_eq!(third, Spaced::Held { wake_in: None });
        assert_eq!(
            held,
            update(vec![DetailsChange::Tasks {
                tasks: with_task("three").tasks,
            }])
        );
        assert_eq!(
            later,
            update(vec![DetailsChange::Tasks {
                tasks: with_task("four").tasks,
            }])
        );
    }

    #[test]
    fn details_that_change_back_while_held_send_nothing() {
        let agent = AgentId::new(1);
        let start = Instant::now();
        let mut out = DetailsOut::default();

        out.space(agent, with_task("one"), start);
        out.space(agent, with_task("two"), start + Duration::from_millis(50));
        out.space(agent, with_task("one"), start + Duration::from_millis(80));
        let held = out.take_held(agent, start + DETAILS_SPACING);

        assert_eq!(held, Spaced::Unchanged);
    }

    #[test]
    fn a_change_of_the_waiting_prompts_goes_out_at_once() {
        let agent = AgentId::new(1);
        let start = Instant::now();
        let mut out = DetailsOut::default();

        out.space(agent, AgentDetails::default(), start);
        let changed = out.space(
            agent,
            waiting("Then run the tests"),
            start + Duration::from_millis(10),
        );

        assert_eq!(
            changed,
            update(vec![DetailsChange::WaitingPrompts {
                waiting_prompts: waiting("Then run the tests").waiting_prompts,
            }])
        );
    }

    #[test]
    fn a_change_of_the_waiting_prompts_takes_the_held_change_with_it() {
        let agent = AgentId::new(1);
        let start = Instant::now();
        let mut out = DetailsOut::default();
        let both = AgentDetails {
            tasks: with_task("two").tasks,
            ..waiting("Then run the tests")
        };

        out.space(agent, with_task("one"), start);
        let held = out.space(agent, with_task("two"), start + Duration::from_millis(10));
        let changed = out.space(agent, both.clone(), start + Duration::from_millis(20));
        let due = out.take_held(agent, start + DETAILS_SPACING);

        assert_eq!(
            held,
            Spaced::Held {
                wake_in: Some(Duration::from_millis(190))
            }
        );
        assert_eq!(
            changed,
            update(vec![
                DetailsChange::Tasks { tasks: both.tasks },
                DetailsChange::WaitingPrompts {
                    waiting_prompts: both.waiting_prompts,
                },
            ])
        );
        assert_eq!(due, Spaced::Unchanged);
    }

    /// Before its first details event since the start, the answer holds the details the daemon
    /// read back, and that event goes out whole.
    #[tokio::test]
    async fn the_details_answer_after_a_restart_holds_the_details_read_back() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("state");
        let agent = state_with_agent(&folder);
        let mut hub = Hub::for_tests(&folder);
        hub.space_details(agent, with_task("one"));
        let mut restarted = Hub::for_tests(&folder);
        let (reply, answer) = oneshot::channel();

        restarted.request(Request::AgentDetails(AgentParams { agent }), reply);

        assert_eq!(
            answer.await.unwrap(),
            ResponseResult::Ok(ResponseBody::Details {
                details: with_task("one"),
                last_seq: Some(Seq::new(1)),
            })
        );
    }
}
