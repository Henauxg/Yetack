//! The machine's load as the app shows it: the daemon's latest answer to `machine.load`, and what
//! the header's chip and card make of it. The app asks again every few seconds and keeps nothing
//! older: the daemon holds the history.

use std::time::Duration;

use cockpit_client::RequestError;
use cockpit_protocol::{
    AgentId, AgentLoad, Cores, ErrorCode, MachineLoad, ProcessId, Request, ResponseBody,
    ServerLoad, StopProcessParams,
};

use super::app_state::AppState;
use super::requests::Outgoing;
use super::ui_state::Popover;
use super::{actions, title_emoji};
use crate::machine::MachineId;

/// How often the app asks for the load while connected: the daemon's sampling interval.
pub const LOAD_POLL: Duration = Duration::from_secs(3);

/// The server's CPU share, in percent, from which the chip turns amber: most cores are busy, and
/// a build starts to wait for one.
const BUSY_PERCENT: u8 = 75;
/// From here the chip turns red: nearly every core is busy, and everything on the server slows.
const SATURATED_PERCENT: u8 = 95;
/// An agent that uses more than this share of the server's cores alone is heavy.
const HEAVY_AGENT_SHARE: f64 = 0.25;
/// Under this, an agent's CPU reads 0.0 cores, and the card folds it with the other idle agents.
const IDLE_CORES: f64 = 0.05;
/// The idle agents' list orders them by their memory in steps of this many bytes.
const IDLE_ORDER_STEP_BYTES: u64 = 64 * 1024 * 1024;

/// The card folds idle agents into one line from this many.
const MIN_FOLDED: usize = 2;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum LoadView {
    /// Not connected, or the first answer has not come yet.
    #[default]
    Waiting,
    Known(Box<MachineLoad>),
    /// The daemon refused the request with this code, or gave another answer (`None`).
    Unknown(Option<ErrorCode>),
}

/// The latest load the app has, and the machine it measures.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ShownLoad {
    pub machine: MachineId,
    pub view: LoadView,
}

impl ShownLoad {
    /// What the app knows of `machine`'s load: another machine's sample says nothing of it.
    pub fn of(&self, machine: MachineId) -> &LoadView {
        static WAITING: LoadView = LoadView::Waiting;
        if self.machine == machine {
            &self.view
        } else {
            &WAITING
        }
    }
}

impl LoadView {
    pub fn from_result(result: Result<ResponseBody, RequestError>) -> Self {
        match result {
            Ok(ResponseBody::MachineLoad(load)) => Self::Known(Box::new(load)),
            Err(RequestError::NotConnected | RequestError::Disconnected) => Self::Waiting,
            Err(RequestError::Failed(body)) => Self::Unknown(Some(body.code)),
            Ok(_) | Err(_) => Self::Unknown(None),
        }
    }
}

/// Why the load is unknown, as the card says it.
pub fn unknown_note(code: Option<ErrorCode>) -> &'static str {
    match code {
        Some(ErrorCode::Unsupported) => {
            "The daemon cannot measure its machine's load: it needs Linux's /proc."
        }
        // A daemon that does not know `machine.load` cannot parse it.
        Some(ErrorCode::InvalidRequest) => {
            "The daemon is older than this app and does not report the load. Update the daemon to see it."
        }
        Some(_) | None => "The daemon did not report its machine's load.",
    }
}

/// How busy the server is, which colors the chip and the bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadLevel {
    Normal,
    Busy,
    Saturated,
}

impl LoadLevel {
    pub fn from_percent(percent: u8) -> Self {
        if percent >= SATURATED_PERCENT {
            Self::Saturated
        } else if percent >= BUSY_PERCENT {
            Self::Busy
        } else {
            Self::Normal
        }
    }

    /// The class of a value or a bar at this level.
    pub fn class(self) -> &'static str {
        match self {
            Self::Normal => "load-normal",
            Self::Busy => "load-busy",
            Self::Saturated => "load-saturated",
        }
    }
}

/// `part` of `whole` as a whole percentage, from 0 to 100.
pub fn percent_of(part: f64, whole: f64) -> u8 {
    if whole <= 0.0 {
        return 0;
    }
    crate::format::percent(part / whole)
}

pub fn cpu_percent(server: &ServerLoad) -> u8 {
    percent_of(server.cpu.get(), f64::from(server.cores))
}

pub fn memory_percent(server: &ServerLoad) -> u8 {
    let tenths_of_percent = server
        .memory_used_bytes
        .saturating_mul(1000)
        .checked_div(server.memory_total_bytes)
        .unwrap_or(0);
    let percent = tenths_of_percent.saturating_add(5) / 10;
    u8::try_from(percent.min(100)).unwrap_or(100)
}

/// Whether the agent alone uses more than a quarter of the server.
pub fn is_heavy(cpu: Cores, server: &ServerLoad) -> bool {
    cpu.get() > HEAVY_AGENT_SHARE * f64::from(server.cores)
}

/// The share of the server's cores, as a bar's width: agents, other processes and the server
/// draw on one scale.
pub fn share_of_server(cpu: Cores, server: &ServerLoad) -> u8 {
    percent_of(cpu.get(), f64::from(server.cores))
}

/// A line of the card's agent list.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentLine {
    Agent {
        agent: AgentId,
        title: String,
        cpu: Cores,
        memory_bytes: u64,
        /// Its share of the server's cores.
        bar_percent: u8,
        heavy: bool,
        selected: bool,
    },
    /// The agents that use no CPU, on one line. `agents` lists them, the heaviest in memory first.
    Idle {
        memory_bytes: u64,
        agents: Vec<IdleAgent>,
    },
}

/// An agent of the idle line, which the line lists once unfolded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleAgent {
    pub agent: AgentId,
    pub title: String,
    pub memory_bytes: u64,
}

/// Every agent the answer measured, the heaviest first; the idle ones fold into one line at the
/// end, except the selected one.
pub fn agent_lines(load: &MachineLoad, state: &AppState) -> Vec<AgentLine> {
    let mut agents: Vec<(&AgentLoad, String)> = load
        .agents
        .iter()
        .filter_map(|measured| {
            let entry = state.agents.get(&measured.agent)?;
            Some((measured, entry.summary.title.clone()))
        })
        .collect();
    agents.sort_by(|(left, _), (right, _)| {
        right
            .cpu
            .get()
            .total_cmp(&left.cpu.get())
            .then(right.memory_bytes.cmp(&left.memory_bytes))
            .then(left.agent.cmp(&right.agent))
    });
    let selected = state.ui.selected;
    let is_idle = |cpu: Cores, agent: AgentId| cpu.get() < IDLE_CORES && Some(agent) != selected;
    let idle_count = agents
        .iter()
        .filter(|(measured, _)| is_idle(measured.cpu, measured.agent))
        .count();
    // An open list keeps its line, even for one agent: it would vanish under the pointer.
    let fold = idle_count >= MIN_FOLDED || (state.ui.idle_agents_unfolded && idle_count > 0);
    let mut lines = Vec::with_capacity(agents.len());
    let mut idle_memory: u64 = 0;
    let mut idle_agents = Vec::new();
    for (measured, title) in agents {
        if fold && is_idle(measured.cpu, measured.agent) {
            idle_memory = idle_memory.saturating_add(measured.memory_bytes);
            idle_agents.push(IdleAgent {
                agent: measured.agent,
                title,
                memory_bytes: measured.memory_bytes,
            });
            continue;
        }
        lines.push(AgentLine::Agent {
            agent: measured.agent,
            title,
            cpu: measured.cpu,
            memory_bytes: measured.memory_bytes,
            bar_percent: share_of_server(measured.cpu, &load.server),
            heavy: is_heavy(measured.cpu, &load.server),
            selected: Some(measured.agent) == selected,
        });
    }
    if fold {
        // By steps of memory, so that two agents whose memory moves a little keep their places under
        // the pointer.
        idle_agents.sort_by_cached_key(|idle| {
            (
                std::cmp::Reverse(idle.memory_bytes / IDLE_ORDER_STEP_BYTES),
                title_emoji::split(&idle.title)
                    .map_or(idle.title.as_str(), |(_, rest)| rest.trim_start())
                    .to_lowercase(),
                idle.agent,
            )
        });
        lines.push(AgentLine::Idle {
            memory_bytes: idle_memory,
            agents: idle_agents,
        });
    }
    lines
}

/// A click on an agent of the card opens it, and the card stays open on its commands.
pub fn select_from_card(state: &mut AppState, agent: AgentId) {
    actions::select_agent(state, agent);
    state.ui.popover = Some(Popover::Load);
}

/// SIGTERM to one of the agent's commands. The daemon checks that the process is the agent's; the
/// command leaves the card with the next sample.
pub fn stop_process(agent: AgentId, pid: ProcessId) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentStopProcess(
        StopProcessParams { agent, pid },
    ))]
}

#[cfg(test)]
pub(crate) mod tests {
    use cockpit_protocol::{
        AgentStatus, ErrorBody, ErrorCode, LoadAverage, LoadHistory, ResourceUse, Timestamp,
    };

    use super::*;
    use crate::state::{state_with, summary};

    const GIB: u64 = 1 << 30;

    pub(crate) fn server(cores: u16, cpu: f64) -> ServerLoad {
        ServerLoad {
            cores,
            cpu: Cores::new(cpu),
            memory_used_bytes: 312 * GIB,
            memory_total_bytes: 503 * GIB,
            load_average: Some(LoadAverage {
                one_minute: 43.6,
                five_minutes: 38.2,
                fifteen_minutes: 30.1,
            }),
        }
    }

    pub(crate) fn measured(agent: u32, cpu: f64, memory_gib: u64) -> AgentLoad {
        AgentLoad {
            agent: AgentId::new(agent),
            cpu: Cores::new(cpu),
            memory_bytes: memory_gib * GIB,
            processes: 1,
            heaviest: Vec::new(),
        }
    }

    pub(crate) fn load(agents: Vec<AgentLoad>) -> MachineLoad {
        MachineLoad {
            taken_at: Timestamp::from_unix_millis(1_700_000_000_000),
            server: server(64, 46.0),
            agents,
            others: ResourceUse::default(),
            history: LoadHistory::default(),
        }
    }

    fn names(lines: &[AgentLine]) -> Vec<String> {
        lines
            .iter()
            .map(|line| match line {
                AgentLine::Agent { agent, .. } => agent.get().to_string(),
                AgentLine::Idle { agents, .. } => format!("{} idle", agents.len()),
            })
            .collect()
    }

    #[test]
    fn the_chip_turns_amber_from_75_percent_and_red_from_95() {
        let level = |cpu: f64| LoadLevel::from_percent(cpu_percent(&server(64, cpu)));

        assert_eq!(level(0.0), LoadLevel::Normal);
        assert_eq!(level(47.0), LoadLevel::Normal, "73 %");
        assert_eq!(level(47.9), LoadLevel::Busy, "74.8 %, shown as 75 %");
        assert_eq!(level(60.0), LoadLevel::Busy, "94 %");
        assert_eq!(level(60.8), LoadLevel::Saturated, "95 %");
        assert_eq!(level(80.0), LoadLevel::Saturated, "over the core count");
        assert_eq!(percent_of(3.0, 0.0), 0);
        assert_eq!(memory_percent(&server(64, 0.0)), 62);
    }

    #[test]
    fn agents_are_listed_heaviest_first_and_the_idle_ones_fold() {
        let mut state = state_with((1..=6).map(|id| summary(id, AgentStatus::Idle)).collect());
        state.ui.selected = Some(AgentId::new(5));
        let answer = load(vec![
            measured(1, 6.2, 8),
            measured(2, 0.0, 1),
            measured(3, 18.4, 22),
            measured(4, 0.01, 2),
            // Idle, but selected: it keeps its line.
            measured(5, 0.0, 3),
            measured(6, 0.0, 4),
        ]);

        let lines = agent_lines(&answer, &state);

        assert_eq!(names(&lines), ["3", "1", "5", "3 idle"]);
        let Some(AgentLine::Idle {
            memory_bytes,
            agents,
        }) = lines.last()
        else {
            panic!("expected the idle agents last");
        };
        assert_eq!(*memory_bytes, 7 * GIB);
        let heaviest_first: Vec<u32> = agents.iter().map(|idle| idle.agent.get()).collect();
        assert_eq!(heaviest_first, [6, 4, 2], "the heaviest in memory first");
        let Some(AgentLine::Agent {
            bar_percent, heavy, ..
        }) = lines.first()
        else {
            panic!("expected the heaviest agent first");
        };
        assert_eq!((*bar_percent, *heavy), (29, true), "18.4 of 64 cores");
        let Some(AgentLine::Agent {
            bar_percent, heavy, ..
        }) = lines.get(1)
        else {
            panic!("expected the second agent");
        };
        assert_eq!((*bar_percent, *heavy), (10, false));
    }

    #[test]
    fn a_single_idle_agent_keeps_its_line_and_equal_loads_sort_by_memory() {
        let state = state_with((1..=3).map(|id| summary(id, AgentStatus::Idle)).collect());
        let answer = load(vec![
            measured(1, 0.0, 1),
            measured(2, 0.5, 1),
            measured(3, 0.5, 9),
        ]);

        let lines = agent_lines(&answer, &state);

        assert_eq!(names(&lines), ["3", "2", "1"]);
    }

    #[test]
    fn an_open_list_keeps_its_line_for_one_idle_agent() {
        let mut state = state_with((1..=2).map(|id| summary(id, AgentStatus::Idle)).collect());
        let answer = load(vec![measured(1, 0.0, 1), measured(2, 0.5, 1)]);
        let folded = names(&agent_lines(&answer, &state));

        state.ui.idle_agents_unfolded = true;

        assert_eq!(folded, ["2", "1"]);
        assert_eq!(names(&agent_lines(&answer, &state)), ["2", "1 idle"]);
    }

    #[test]
    fn idle_agents_whose_memory_moves_a_little_keep_their_order() {
        let state = state_with((1..=3).map(|id| summary(id, AgentStatus::Idle)).collect());
        let order = |memory: [u64; 3]| {
            let measured = (1..=3).zip(memory).map(|(id, bytes)| AgentLoad {
                memory_bytes: bytes,
                ..measured(id, 0.0, 0)
            });
            let lines = agent_lines(&load(measured.collect()), &state);
            let Some(AgentLine::Idle { agents, .. }) = lines.last() else {
                panic!("expected the idle agents");
            };
            agents
                .iter()
                .map(|idle| idle.agent.get())
                .collect::<Vec<u32>>()
        };
        const MIB: u64 = 1024 * 1024;

        let before = order([412 * MIB, 413 * MIB, 900 * MIB]);
        let after = order([414 * MIB, 412 * MIB, 900 * MIB]);

        assert_eq!(before, [3, 1, 2], "the heaviest first, then by title");
        assert_eq!(after, before);
    }

    #[test]
    fn an_agent_the_app_does_not_know_is_left_out() {
        let state = state_with(vec![summary(1, AgentStatus::Working)]);
        let answer = load(vec![measured(1, 2.0, 1), measured(9, 4.0, 1)]);

        assert_eq!(names(&agent_lines(&answer, &state)), ["1"]);
    }

    #[test]
    fn a_daemon_that_does_not_know_the_request_reads_apart_from_one_that_cannot_measure() {
        let failed = |code| {
            LoadView::from_result(Err(RequestError::Failed(ErrorBody::new(
                code,
                "the daemon refused".to_owned(),
            ))))
        };

        assert_ne!(
            failed(ErrorCode::InvalidRequest),
            failed(ErrorCode::Unsupported)
        );
        assert!(unknown_note(Some(ErrorCode::InvalidRequest)).contains("Update the daemon"));
        assert!(unknown_note(Some(ErrorCode::Unsupported)).contains("/proc"));
    }

    #[test]
    fn a_sample_of_one_machine_says_nothing_of_another() {
        let devbox = MachineId::new(1);
        let known = LoadView::Known(Box::new(load(Vec::new())));
        let shown = ShownLoad {
            machine: MachineId::THIS,
            view: known.clone(),
        };

        assert_eq!(shown.of(MachineId::THIS), &known);
        assert_eq!(shown.of(devbox), &LoadView::Waiting);
    }

    #[test]
    fn a_failed_answer_reads_as_unknown_and_a_dropped_connection_as_waiting() {
        let unsupported = RequestError::Failed(ErrorBody::new(
            ErrorCode::Unsupported,
            "no /proc".to_owned(),
        ));

        assert_eq!(
            LoadView::from_result(Err(unsupported)),
            LoadView::Unknown(Some(ErrorCode::Unsupported))
        );
        assert_eq!(
            LoadView::from_result(Ok(ResponseBody::Done)),
            LoadView::Unknown(None)
        );
        assert_eq!(
            LoadView::from_result(Err(RequestError::Disconnected)),
            LoadView::Waiting
        );
        let known = LoadView::from_result(Ok(ResponseBody::MachineLoad(load(Vec::new()))));
        assert!(matches!(known, LoadView::Known(_)));
    }

    #[test]
    fn a_stop_names_the_agent_and_the_process() {
        let requests = stop_process(AgentId::new(3), ProcessId::new(41_872));

        let sent: Vec<Request> = requests
            .into_iter()
            .map(|outgoing| outgoing.request)
            .collect();
        assert_eq!(
            sent,
            [Request::AgentStopProcess(StopProcessParams {
                agent: AgentId::new(3),
                pid: ProcessId::new(41_872),
            })]
        );
    }

    #[test]
    fn selecting_from_the_card_keeps_it_open() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Working),
        ]);
        state.ui.selected = Some(AgentId::new(1));
        state.ui.popover = Some(Popover::Load);

        select_from_card(&mut state, AgentId::new(2));

        assert_eq!(state.ui.selected, Some(AgentId::new(2)));
        assert_eq!(state.ui.popover, Some(Popover::Load));
    }
}
