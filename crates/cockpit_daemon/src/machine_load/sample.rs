//! What two readings say about the time between them: CPU from the counters' growth, memory and
//! load from the later one.
//!
//! An agent's CPU counts each process's own time and the time of the children it reaped, which
//! the kernel adds to the parent when a child ends. So a short command that starts and ends
//! between two readings still counts, through its parent. A process that ended since the
//! previous reading gives back the time it had then, which its parent now counts again.
//!
//! For example, over 3 seconds at 100 ticks a second, `cargo` grows from 40 to 340 ticks, 250 of
//! them from a `rustc` it reaped, and that `rustc` had 150 ticks at the previous reading: the
//! agent used 340 - 40 - 150 = 150 ticks, 1.5 seconds of CPU, so 0.5 cores.

use std::collections::{HashMap, HashSet};

use cockpit_protocol::{AgentId, Cores, ProcessId, ResourceUse, ServerLoad, Timestamp};

use crate::machine_load::proc_files::{ProcessKey, ProcessStat};
use crate::machine_load::reading::{AgentProcesses, Reading};

/// The heaviest commands each agent's answer lists.
const MAX_HEAVIEST: usize = 3;

/// What turns the kernel's counts into seconds and bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Units {
    pub(super) ticks_per_second: u64,
    pub(super) page_bytes: u64,
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
    /// Heaviest first, the agent's `claude` and ended processes left out.
    pub(super) heaviest: Vec<HeavyProcess>,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct HeavyProcess {
    pub(super) pid: ProcessId,
    pub(super) name: String,
    pub(super) cpu: Cores,
    pub(super) memory_bytes: u64,
}

pub(super) fn sample(previous: &Reading, current: &Reading, units: Units) -> Sample {
    let seconds = current
        .at
        .saturating_duration_since(previous.at)
        .as_secs_f64();
    let rate = Rate { units, seconds };
    let busy = current
        .cpu
        .times
        .busy
        .saturating_sub(previous.cpu.times.busy);
    let total = current
        .cpu
        .times
        .total
        .saturating_sub(previous.cpu.times.total);
    let server_cpu = if total == 0 {
        Cores::default()
    } else {
        rounded(to_f64(busy) / to_f64(total) * f64::from(current.cpu.cores))
    };
    let memory = current.memory;
    let server = ServerLoad {
        cores: current.cpu.cores,
        cpu: server_cpu,
        memory_used_bytes: memory.total_bytes.saturating_sub(memory.available_bytes),
        memory_total_bytes: memory.total_bytes,
        load_average: Some(current.load_average),
    };
    let before: HashMap<AgentId, &AgentProcesses> = previous
        .agents
        .iter()
        .map(|agent| (agent.agent, agent))
        .collect();
    let agents: Vec<AgentSample> = current
        .agents
        .iter()
        .map(|agent| {
            let earlier = before
                .get(&agent.agent)
                .copied()
                .filter(|earlier| same_root(earlier, agent));
            agent_sample(earlier, agent, previous.uptime_ticks, rate)
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

/// A new `claude` for the agent starts its counts afresh: the old one's time went to the daemon.
fn same_root(earlier: &AgentProcesses, now: &AgentProcesses) -> bool {
    let key = |agent: &AgentProcesses| agent.root().map(|stat| stat.key);
    key(earlier) == key(now)
}

#[derive(Debug, Clone, Copy)]
struct Rate {
    units: Units,
    seconds: f64,
}

impl Rate {
    fn cores(self, ticks: u64) -> Cores {
        if self.seconds <= 0.0 || self.units.ticks_per_second == 0 {
            return Cores::default();
        }
        rounded(to_f64(ticks) / to_f64(self.units.ticks_per_second) / self.seconds)
    }
}

/// `earlier` is the agent's processes at the previous reading, `None` for an agent that had none
/// or another `claude`. A process absent then counts in full when it started since
/// (`previous_uptime`), and from the next reading otherwise.
fn agent_sample(
    earlier: Option<&AgentProcesses>,
    now: &AgentProcesses,
    previous_uptime: u64,
    rate: Rate,
) -> AgentSample {
    let before: HashMap<ProcessKey, &ProcessStat> = earlier
        .map(|agent| {
            agent
                .processes
                .iter()
                .map(|stat| (stat.key, stat))
                .collect()
        })
        .unwrap_or_default();
    let grown = |stat: &ProcessStat, ticks: fn(&ProcessStat) -> u64| match before.get(&stat.key) {
        Some(earlier) => ticks(stat).saturating_sub(ticks(earlier)),
        None if stat.key.started >= previous_uptime => ticks(stat),
        None => 0,
    };
    let mut gained: u64 = 0;
    let mut pages: u64 = 0;
    let mut heavy = Vec::new();
    for (index, stat) in now.processes.iter().enumerate() {
        gained = gained.saturating_add(grown(stat, |stat| stat.total_ticks));
        pages = pages.saturating_add(stat.resident_pages);
        if index > 0 && !stat.zombie {
            heavy.push(HeavyProcess {
                pid: stat.key.pid,
                name: stat.name.clone(),
                cpu: rate.cores(grown(stat, |stat| stat.own_ticks)),
                memory_bytes: stat.resident_pages.saturating_mul(rate.units.page_bytes),
            });
        }
    }
    let current: HashSet<ProcessKey> = now.processes.iter().map(|stat| stat.key).collect();
    let ended: u64 = earlier
        .map(|agent| {
            agent
                .processes
                .iter()
                .filter(|stat| {
                    !current.contains(&stat.key) && !now.left_running.contains(&stat.key)
                })
                .map(|stat| stat.total_ticks)
                .sum()
        })
        .unwrap_or(0);
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
        cpu: rate.cores(gained.saturating_sub(ended)),
        memory_bytes: pages.saturating_mul(rate.units.page_bytes),
        processes: u32::try_from(now.processes.len()).unwrap_or(u32::MAX),
        heaviest: heavy,
    }
}

/// To the hundredth of a core: finer is noise, and longer on the wire.
fn rounded(cores: f64) -> Cores {
    Cores::new((cores * 100.0).round() / 100.0)
}

/// Counts between two readings, a few seconds apart, stay far below `u32::MAX`.
fn to_f64(count: u64) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use cockpit_protocol::LoadAverage;

    use super::*;
    use crate::machine_load::proc_files::{CpuTimes, MemoryInfo, ServerCpu};

    const UNITS: Units = Units {
        ticks_per_second: 100,
        page_bytes: 4096,
    };

    /// `pid (name)` started at `started`, child of `parent`, with its own and total ticks.
    fn stat(pid: u32, name: &str, parent: u32, started: u64, ticks: (u64, u64)) -> ProcessStat {
        ProcessStat {
            key: ProcessKey {
                pid: ProcessId::new(pid),
                started,
            },
            name: name.to_owned(),
            parent: ProcessId::new(parent),
            own_ticks: ticks.0,
            total_ticks: ticks.1,
            threads: 1,
            resident_pages: 1000,
            zombie: false,
        }
    }

    fn agent(id: u32, processes: Vec<ProcessStat>, left_running: &[ProcessKey]) -> AgentProcesses {
        AgentProcesses {
            agent: AgentId::new(id),
            processes,
            left_running: left_running.iter().copied().collect(),
        }
    }

    /// A 4-core server, `seconds` after `start`, with its counters and the agents.
    fn reading(
        start: Instant,
        seconds: u64,
        cpu: CpuTimes,
        agents: Vec<AgentProcesses>,
    ) -> Reading {
        Reading {
            at: start + Duration::from_secs(seconds),
            taken_at: Timestamp::from_unix_millis(1_700_000_000_000 + seconds * 1000),
            uptime_ticks: 100_000 + seconds * 100,
            cpu: ServerCpu {
                times: cpu,
                cores: 4,
            },
            memory: MemoryInfo {
                total_bytes: 16_000_000_000,
                available_bytes: 6_000_000_000,
            },
            load_average: LoadAverage {
                one_minute: 2.5,
                five_minutes: 2.0,
                fifteen_minutes: 1.5,
            },
            agents,
        }
    }

    #[test]
    fn the_server_cpu_is_the_busy_share_of_its_cores() {
        let start = Instant::now();
        let before = reading(
            start,
            0,
            CpuTimes {
                busy: 1000,
                total: 4000,
            },
            Vec::new(),
        );
        // 3 seconds on 4 cores is 1200 ticks, 900 of them busy: 3 cores.
        let after = reading(
            start,
            3,
            CpuTimes {
                busy: 1900,
                total: 5200,
            },
            Vec::new(),
        );

        let sample = sample(&before, &after, UNITS);

        assert_eq!(sample.server.cpu, Cores::new(3.0));
        assert_eq!(sample.server.memory_used_bytes, 10_000_000_000);
        assert_eq!(sample.others.cpu, Cores::new(3.0));
    }

    /// The worked example of the module's documentation.
    #[test]
    fn a_reaped_command_counts_once_through_its_parent() {
        let start = Instant::now();
        let busy = |busy| CpuTimes {
            busy,
            total: busy * 2,
        };
        let claude = stat(20, "claude", 10, 50, (500, 500));
        let before = reading(
            start,
            0,
            busy(1000),
            vec![agent(
                1,
                vec![
                    claude.clone(),
                    stat(22, "cargo", 20, 90_000, (40, 40)),
                    stat(23, "rustc", 22, 99_000, (150, 150)),
                ],
                &[],
            )],
        );
        let after = reading(
            start,
            3,
            busy(1600),
            vec![agent(
                1,
                vec![claude, stat(22, "cargo", 20, 90_000, (90, 340))],
                &[],
            )],
        );

        let sample = sample(&before, &after, UNITS);

        let agent = sample.agents.first().unwrap();
        assert_eq!(agent.cpu, Cores::new(0.5));
        assert_eq!(agent.processes, 2);
        assert_eq!(agent.memory_bytes, 2 * 1000 * 4096);
        let heaviest: Vec<(&str, Cores)> = agent
            .heaviest
            .iter()
            .map(|process| (process.name.as_str(), process.cpu))
            .collect();
        assert_eq!(
            heaviest,
            [("cargo", Cores::new(0.17))],
            "claude is left out"
        );
    }

    #[test]
    fn a_new_process_counts_in_full_and_an_unknown_old_one_from_the_next_reading() {
        let start = Instant::now();
        let cpu = CpuTimes { busy: 0, total: 0 };
        let claude = stat(20, "claude", 10, 50, (500, 500));
        let before = reading(start, 0, cpu, vec![agent(1, vec![claude.clone()], &[])]);
        let after = reading(
            start,
            3,
            cpu,
            vec![agent(
                1,
                vec![
                    claude,
                    // Started after the previous reading, at uptime 100 000 ticks.
                    stat(24, "cargo test", 20, 100_100, (150, 150)),
                    // Missed by the previous walk: its 9000 ticks are older.
                    stat(25, "postgres", 20, 2_000, (9_000, 9_000)),
                    // Ended, not reaped yet.
                    ProcessStat {
                        zombie: true,
                        ..stat(26, "sleep", 20, 100_050, (1, 1))
                    },
                ],
                &[],
            )],
        );

        let sample = sample(&before, &after, UNITS);

        let agent = sample.agents.first().unwrap();
        assert_eq!(agent.cpu, Cores::new(0.5));
        assert_eq!(agent.processes, 4);
        let heaviest: Vec<(u32, Cores)> = agent
            .heaviest
            .iter()
            .map(|process| (process.pid.get(), process.cpu))
            .collect();
        assert_eq!(
            heaviest,
            [(24, Cores::new(0.5)), (25, Cores::new(0.0))],
            "the heaviest first"
        );
        assert_eq!(sample.others.cpu, Cores::default(), "never below zero");
    }

    #[test]
    fn a_process_that_left_the_tree_running_gives_nothing_back() {
        let start = Instant::now();
        let cpu = CpuTimes { busy: 0, total: 0 };
        let claude = stat(20, "claude", 10, 50, (500, 500));
        let server = stat(22, "npm run dev", 21, 90_000, (6_000, 6_000));
        let before = reading(
            start,
            0,
            cpu,
            vec![agent(
                1,
                vec![
                    claude.clone(),
                    stat(21, "bash", 20, 89_000, (10, 10)),
                    server.clone(),
                ],
                &[],
            )],
        );
        // The shell ended and claude reaped it; the server went to init.
        let claude_after = stat(20, "claude", 10, 50, (560, 572));
        let after = reading(
            start,
            3,
            cpu,
            vec![agent(1, vec![claude_after], &[server.key])],
        );

        let sample = sample(&before, &after, UNITS);

        // 60 ticks of claude, and the shell's 2 last ones: 62 ticks in 3 seconds.
        assert_eq!(sample.agents.first().unwrap().cpu, Cores::new(0.21));
    }

    #[test]
    fn a_new_claude_starts_its_counts_afresh() {
        let start = Instant::now();
        let cpu = CpuTimes { busy: 0, total: 0 };
        let before = reading(
            start,
            0,
            cpu,
            vec![agent(
                1,
                vec![stat(20, "claude", 10, 50, (9_000, 9_000))],
                &[],
            )],
        );
        // Resumed: the old claude's time is not taken from the new one's.
        let after = reading(
            start,
            3,
            cpu,
            vec![agent(
                1,
                vec![stat(30, "claude", 10, 99_000, (300, 300))],
                &[],
            )],
        );

        let sample = sample(&before, &after, UNITS);

        assert_eq!(sample.agents.first().unwrap().cpu, Cores::new(0.0));
    }

    #[test]
    fn readings_at_the_same_moment_give_no_cpu() {
        let start = Instant::now();
        let cpu = CpuTimes {
            busy: 10,
            total: 10,
        };
        let processes = vec![stat(20, "claude", 10, 50, (500, 900))];
        let before = reading(start, 0, cpu, vec![agent(1, processes.clone(), &[])]);
        let after = reading(start, 0, cpu, vec![agent(1, processes, &[])]);

        let sample = sample(&before, &after, UNITS);

        assert_eq!(sample.server.cpu, Cores::default());
        assert_eq!(sample.agents.first().unwrap().cpu, Cores::default());
    }
}
