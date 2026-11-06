//! The machine's load as the app shows it: the daemon's latest answer to `machine.load`, and what
//! the header's chip and card make of it. The app asks again every few seconds and keeps nothing
//! older: the daemon holds the history.

use std::time::Duration;

use cockpit_client::RequestError;
use cockpit_protocol::{AgentId, Cores, MachineLoad, ResponseBody, ServerLoad};

use super::actions;
use super::app_state::AppState;
use super::ui_state::Popover;

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
/// The card folds idle agents into one line from this many.
const MIN_FOLDED: usize = 2;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum LoadView {
    /// Not connected, or the first answer has not come yet.
    #[default]
    Waiting,
    Known(Box<MachineLoad>),
    /// The daemon cannot measure its machine, or does not know the request.
    Unknown,
}

impl LoadView {
    pub fn from_result(result: Result<ResponseBody, RequestError>) -> Self {
        match result {
            Ok(ResponseBody::MachineLoad(load)) => Self::Known(Box::new(load)),
            Err(RequestError::NotConnected | RequestError::Disconnected) => Self::Waiting,
            Ok(_) | Err(_) => Self::Unknown,
        }
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
    /// The agents that use no CPU, together.
    Idle { count: usize, memory_bytes: u64 },
}

/// Every agent the answer measured, the heaviest first; the idle ones fold into one line at the
/// end, except the selected one.
pub fn agent_lines(load: &MachineLoad, state: &AppState) -> Vec<AgentLine> {
    let mut agents: Vec<_> = load
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
    let fold = idle_count >= MIN_FOLDED;
    let mut lines = Vec::with_capacity(agents.len());
    let mut idle_memory: u64 = 0;
    for (measured, title) in agents {
        if fold && is_idle(measured.cpu, measured.agent) {
            idle_memory = idle_memory.saturating_add(measured.memory_bytes);
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
        lines.push(AgentLine::Idle {
            count: idle_count,
            memory_bytes: idle_memory,
        });
    }
    lines
}

/// A click on an agent of the card opens it, and the card stays open on its commands.
pub fn select_from_card(state: &mut AppState, agent: AgentId) {
    actions::select_agent(state, agent);
    state.ui.popover = Some(Popover::Load);
}

#[cfg(test)]
pub(crate) mod tests {
    use cockpit_protocol::{
        AgentLoad, AgentStatus, ErrorBody, ErrorCode, LoadAverage, LoadHistory, ResourceUse,
        Timestamp,
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
            load_average: LoadAverage {
                one_minute: 43.6,
                five_minutes: 38.2,
                fifteen_minutes: 30.1,
            },
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
                AgentLine::Idle { count, .. } => format!("{count} idle"),
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
        assert_eq!(
            lines.last(),
            Some(&AgentLine::Idle {
                count: 3,
                memory_bytes: 7 * GIB,
            })
        );
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
    fn an_agent_the_app_does_not_know_is_left_out() {
        let state = state_with(vec![summary(1, AgentStatus::Working)]);
        let answer = load(vec![measured(1, 2.0, 1), measured(9, 4.0, 1)]);

        assert_eq!(names(&agent_lines(&answer, &state)), ["1"]);
    }

    #[test]
    fn a_failed_answer_reads_as_unknown_and_a_dropped_connection_as_waiting() {
        let unsupported = RequestError::Failed(ErrorBody::new(
            ErrorCode::Unsupported,
            "no /proc".to_owned(),
        ));

        assert_eq!(LoadView::from_result(Err(unsupported)), LoadView::Unknown);
        assert_eq!(
            LoadView::from_result(Ok(ResponseBody::Done)),
            LoadView::Unknown
        );
        assert_eq!(
            LoadView::from_result(Err(RequestError::Disconnected)),
            LoadView::Waiting
        );
        let known = LoadView::from_result(Ok(ResponseBody::MachineLoad(load(Vec::new()))));
        assert!(matches!(known, LoadView::Known(_)));
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
