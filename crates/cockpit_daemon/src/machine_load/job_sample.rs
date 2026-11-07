//! What two readings of a Windows machine say about the time between them: CPU from the counters'
//! growth, memory from the later reading. Windows has no load average.
//!
//! Windows counts CPU time in units of 100 ns. An agent's job counts the time of every process
//! that was ever in it, those that ended since included, so the agent's CPU is the growth of its
//! job's count. An agent whose `claude` runs without a job counts `claude` alone.
//!
//! The heaviest commands leave out the agent's `claude`: its root process, and for an npm
//! `claude.cmd`, the root's children, which run the real `claude` under `cmd.exe`.

use std::collections::HashMap;
use std::time::Instant;

use cockpit_protocol::{AgentId, Cores, ProcessId, ResourceUse, ServerLoad, Timestamp};

use crate::machine_load::agent_root;
use crate::process::program::Launch;

/// CPU time units in a second.
const UNITS_PER_SECOND: f64 = 10_000_000.0;
/// The heaviest commands each agent's answer lists.
const MAX_HEAVIEST: usize = 3;

/// A process, told apart from a later one that gets its id by its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ProcessKey {
    pub(super) pid: ProcessId,
    /// When it started, in 100 ns units since 1601, as Windows dates it.
    pub(super) started: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProcessFigures {
    pub(super) key: ProcessKey,
    /// The process that started it, when Windows tells.
    pub(super) parent: Option<ProcessId>,
    /// Its user and kernel time.
    pub(super) cpu_time: u64,
    /// Its working set: the memory it has resident.
    pub(super) memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AgentFigures {
    pub(super) agent: AgentId,
    /// The agent's `claude`, or the `cmd.exe` that runs it.
    pub(super) root: ProcessKey,
    pub(super) launch: Launch,
    /// The user and kernel time of every process the job held, or of `claude` without a job.
    pub(super) cpu_time: u64,
    /// The processes running now, `claude` among them.
    pub(super) processes: Vec<ProcessFigures>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Reading {
    pub(super) at: Instant,
    pub(super) taken_at: Timestamp,
    /// Now, in the units and from the start a process's start uses.
    pub(super) clock: u64,
    pub(super) cores: u16,
    /// The time every core spent idle, and in all, since the machine started.
    pub(super) idle_time: u64,
    pub(super) total_time: u64,
    pub(super) memory_total_bytes: u64,
    pub(super) memory_available_bytes: u64,
    pub(super) agents: Vec<AgentFigures>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Sample {
    pub(super) taken_at: Timestamp,
    pub(super) server: ServerLoad,
    pub(super) agents: Vec<AgentSample>,
    pub(super) others: ResourceUse,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct AgentSample {
    pub(super) agent: AgentId,
    pub(super) cpu: Cores,
    pub(super) memory_bytes: u64,
    pub(super) processes: u32,
    /// Heaviest first, the agent's `claude` left out.
    pub(super) heaviest: Vec<HeavyProcess>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct HeavyProcess {
    pub(super) key: ProcessKey,
    pub(super) cpu: Cores,
    pub(super) memory_bytes: u64,
}

pub(super) fn sample(previous: &Reading, current: &Reading) -> Sample {
    let seconds = current
        .at
        .saturating_duration_since(previous.at)
        .as_secs_f64();
    let idle = current.idle_time.saturating_sub(previous.idle_time);
    let total = current.total_time.saturating_sub(previous.total_time);
    let server_cpu = if total == 0 {
        Cores::default()
    } else {
        let busy = total.saturating_sub(idle);
        rounded(to_f64(busy) / to_f64(total) * f64::from(current.cores))
    };
    let server = ServerLoad {
        cores: current.cores,
        cpu: server_cpu,
        memory_used_bytes: current
            .memory_total_bytes
            .saturating_sub(current.memory_available_bytes),
        memory_total_bytes: current.memory_total_bytes,
        load_average: None,
    };
    let before: HashMap<AgentId, &AgentFigures> = previous
        .agents
        .iter()
        .map(|agent| (agent.agent, agent))
        .collect();
    let since = Since {
        clock: previous.clock,
        seconds,
    };
    let agents: Vec<AgentSample> = current
        .agents
        .iter()
        .map(|agent| {
            let earlier = before
                .get(&agent.agent)
                .copied()
                .filter(|earlier| earlier.root == agent.root);
            agent_sample(earlier, agent, since)
        })
        .collect();
    let agents_cpu: f64 = agents.iter().map(|agent| agent.cpu.get()).sum();
    let agents_memory: u64 = agents.iter().map(|agent| agent.memory_bytes).sum();
    let others = ResourceUse {
        cpu: rounded((server.cpu.get() - agents_cpu).max(0.0)),
        memory_bytes: server.memory_used_bytes.saturating_sub(agents_memory),
    };
    Sample {
        taken_at: current.taken_at,
        server,
        agents,
        others,
    }
}

/// The previous reading's clock, and the seconds since.
#[derive(Debug, Clone, Copy)]
struct Since {
    clock: u64,
    seconds: f64,
}

impl Since {
    /// The growth of a count from `before`, the count at the previous reading. A count absent
    /// then grows from 0 when its process started since, and from now otherwise.
    fn growth(self, now: u64, before: Option<u64>, started: u64) -> u64 {
        match before {
            Some(before) => now.saturating_sub(before),
            None if started >= self.clock => now,
            None => 0,
        }
    }

    fn cores(self, time: u64) -> Cores {
        if self.seconds <= 0.0 {
            return Cores::default();
        }
        rounded(to_f64(time) / UNITS_PER_SECOND / self.seconds)
    }
}

/// `earlier` is the agent at the previous reading, `None` for an agent absent then or with
/// another `claude`.
fn agent_sample(earlier: Option<&AgentFigures>, now: &AgentFigures, since: Since) -> AgentSample {
    let cpu_time = since.growth(
        now.cpu_time,
        earlier.map(|agent| agent.cpu_time),
        now.root.started,
    );
    let before: HashMap<ProcessKey, u64> = earlier
        .map(|agent| {
            agent
                .processes
                .iter()
                .map(|process| (process.key, process.cpu_time))
                .collect()
        })
        .unwrap_or_default();
    let mut heavy: Vec<HeavyProcess> = Vec::new();
    let commands = now.processes.iter().filter(|process| {
        !agent_root::is_claude(now.root.pid, now.launch, process.key.pid, process.parent)
    });
    for process in commands {
        let grown = since.growth(
            process.cpu_time,
            before.get(&process.key).copied(),
            process.key.started,
        );
        heavy.push(HeavyProcess {
            key: process.key,
            cpu: since.cores(grown),
            memory_bytes: process.memory_bytes,
        });
    }
    heavy.sort_by(|left, right| {
        right
            .cpu
            .get()
            .total_cmp(&left.cpu.get())
            .then(right.memory_bytes.cmp(&left.memory_bytes))
    });
    heavy.truncate(MAX_HEAVIEST);
    AgentSample {
        agent: now.agent,
        cpu: since.cores(cpu_time),
        memory_bytes: now
            .processes
            .iter()
            .map(|process| process.memory_bytes)
            .sum(),
        processes: u32::try_from(now.processes.len()).unwrap_or(u32::MAX),
        heaviest: heavy,
    }
}

/// To the hundredth of a core: finer is noise, and longer on the wire.
fn rounded(cores: f64) -> Cores {
    Cores::new((cores * 100.0).round() / 100.0)
}

/// Exact below 2^53, which no count of a few seconds reaches.
fn to_f64(count: u64) -> f64 {
    const HIGH_UNIT: f64 = 4_294_967_296.0;

    let high = u32::try_from(count >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(count & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high) * HIGH_UNIT + f64::from(low)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const SECOND: u64 = 10_000_000;
    /// The previous reading's clock.
    const THEN: u64 = 1_000 * SECOND;

    fn key(pid: u32, started: u64) -> ProcessKey {
        ProcessKey {
            pid: ProcessId::new(pid),
            started,
        }
    }

    fn process(key: ProcessKey, cpu_time: u64) -> ProcessFigures {
        ProcessFigures {
            key,
            parent: None,
            cpu_time,
            memory_bytes: 1_000_000,
        }
    }

    fn reading(at: Instant, clock: u64, times: (u64, u64), agents: Vec<AgentFigures>) -> Reading {
        Reading {
            at,
            taken_at: Timestamp::from_unix_millis(clock / 10_000),
            clock,
            cores: 8,
            idle_time: times.0,
            total_time: times.1,
            memory_total_bytes: 16_000_000_000,
            memory_available_bytes: 6_000_000_000,
            agents,
        }
    }

    /// Two readings 2 seconds apart, on 8 cores, 4 of them busy.
    fn pair(before: Vec<AgentFigures>, after: Vec<AgentFigures>) -> Sample {
        let start = Instant::now();
        let first = reading(start, THEN, (100 * SECOND, 200 * SECOND), before);
        let second = reading(
            start + Duration::from_secs(2),
            THEN + 2 * SECOND,
            (108 * SECOND, 216 * SECOND),
            after,
        );
        sample(&first, &second)
    }

    #[test]
    fn the_server_has_its_busy_cores_its_memory_and_no_load_average() {
        let sample = pair(Vec::new(), Vec::new());

        assert_eq!(sample.server.cpu, Cores::new(4.0));
        assert_eq!(sample.server.memory_used_bytes, 10_000_000_000);
        assert_eq!(sample.server.load_average, None);
        assert_eq!(sample.others.cpu, Cores::new(4.0));
    }

    /// The job's count holds the time of a command that ended between the readings: the agent's
    /// CPU counts it, though no process shows it.
    #[test]
    fn an_agent_counts_its_jobs_growth_and_lists_its_heaviest_commands() {
        let claude = key(10, THEN - 50 * SECOND);
        let server = key(11, THEN - 40 * SECOND);
        let build = key(12, THEN + SECOND);
        let agent = |cpu_time: u64, processes: Vec<ProcessFigures>| AgentFigures {
            agent: AgentId::new(1),
            root: claude,
            launch: Launch::Direct,
            cpu_time,
            processes,
        };
        let before = agent(
            30 * SECOND,
            vec![process(claude, 20 * SECOND), process(server, 9 * SECOND)],
        );
        let after = agent(
            34 * SECOND,
            vec![
                process(claude, 20 * SECOND),
                process(server, 10 * SECOND),
                process(build, 2 * SECOND),
            ],
        );

        let sample = pair(vec![before], vec![after]);

        let agent = sample.agents.first().unwrap();
        assert_eq!(agent.cpu, Cores::new(2.0));
        assert_eq!(agent.processes, 3);
        assert_eq!(agent.memory_bytes, 3_000_000);
        let heaviest: Vec<(ProcessKey, Cores)> = agent
            .heaviest
            .iter()
            .map(|heavy| (heavy.key, heavy.cpu))
            .collect();
        assert_eq!(
            heaviest,
            [(build, Cores::new(1.0)), (server, Cores::new(0.5))]
        );
        assert_eq!(sample.others.cpu, Cores::new(2.0));
    }

    /// An npm `claude.cmd` runs `node` under `cmd.exe`: `node` is the agent's `claude`, and the
    /// commands it starts are its own.
    #[test]
    fn the_children_of_an_interpreter_root_are_left_out_of_the_heaviest() {
        let interpreter = key(10, THEN - 50 * SECOND);
        let node = key(11, THEN - 49 * SECOND);
        let build = key(12, THEN - 10 * SECOND);
        let child = |key: ProcessKey, parent: ProcessKey, cpu_time: u64| ProcessFigures {
            parent: Some(parent.pid),
            ..process(key, cpu_time)
        };
        let agent = |launch, cpu_time: u64, node_time: u64, build_time: u64| AgentFigures {
            agent: AgentId::new(1),
            root: interpreter,
            launch,
            cpu_time,
            processes: vec![
                process(interpreter, 0),
                child(node, interpreter, node_time),
                child(build, node, build_time),
            ],
        };
        let heaviest = |launch| {
            let before = agent(launch, 20 * SECOND, 10 * SECOND, 10 * SECOND);
            let after = agent(launch, 26 * SECOND, 14 * SECOND, 12 * SECOND);
            let sample = pair(vec![before], vec![after]);
            let agent = sample.agents.first().unwrap().clone();
            let keys: Vec<ProcessKey> = agent.heaviest.iter().map(|heavy| heavy.key).collect();
            keys
        };

        assert_eq!(heaviest(Launch::Interpreter), [build]);
        assert_eq!(heaviest(Launch::Direct), [node, build]);
    }

    #[test]
    fn a_new_claude_counts_from_its_start_and_one_seen_late_from_now() {
        let old = key(10, THEN - 50 * SECOND);
        let restarted = key(20, THEN + SECOND);
        let agent = |id: u32, root: ProcessKey, cpu_time: u64| AgentFigures {
            agent: AgentId::new(id),
            root,
            launch: Launch::Direct,
            cpu_time,
            processes: vec![process(root, cpu_time)],
        };
        let before = vec![agent(1, old, 90 * SECOND)];
        let after = vec![
            agent(1, restarted, SECOND),
            agent(2, key(30, THEN - SECOND), 500 * SECOND),
        ];

        let sample = pair(before, after);

        let cpu: Vec<Cores> = sample.agents.iter().map(|agent| agent.cpu).collect();
        assert_eq!(cpu, [Cores::new(0.5), Cores::new(0.0)]);
    }

    #[test]
    fn a_count_converts_exactly_past_32_bits() {
        assert_eq!(to_f64(0), 0.0);
        assert_eq!(to_f64(u64::from(u32::MAX) + 1), 4_294_967_296.0);
        assert_eq!(to_f64(5 * SECOND * 1_000), 50_000_000_000.0);
    }
}
