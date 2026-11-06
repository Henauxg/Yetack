//! Takes the readings, turns each pair into a sample, and keeps the answer to `machine.load`.

use cockpit_protocol::{AgentLoad, CommandLoad, MachineLoad};

use crate::machine_load::agent_root::AgentRoot;
use crate::machine_load::history::History;
use crate::machine_load::reading::{LoadError, MachineReader, Reading};
use crate::machine_load::sample::{self, Sample, Units};

#[derive(Debug)]
pub(super) struct Sampler {
    reader: MachineReader,
    units: Units,
    previous: Option<Reading>,
    history: History,
    latest: Option<MachineLoad>,
}

impl Sampler {
    pub(super) fn new() -> Self {
        let units = Units {
            ticks_per_second: rustix::param::clock_ticks_per_second(),
            page_bytes: u64::try_from(rustix::param::page_size()).unwrap_or(u64::MAX),
        };
        Self {
            reader: MachineReader::new(units.ticks_per_second),
            units,
            previous: None,
            history: History::default(),
            latest: None,
        }
    }

    /// Reads the machine and the processes under `roots`. From the second reading on, each one
    /// makes a sample.
    pub(super) fn step(&mut self, roots: &[AgentRoot]) -> Result<(), LoadError> {
        let current = self.reader.read(roots, self.previous.as_ref())?;
        if let Some(previous) = &self.previous {
            let sample = sample::sample(previous, &current, self.units);
            let agents = sample.agents.iter().map(|agent| (agent.agent, agent.cpu));
            self.history
                .push(sample.taken_at, sample.server.cpu, agents);
            self.latest = Some(self.answer(sample));
        }
        self.previous = Some(current);
        Ok(())
    }

    /// `None` until the second reading.
    pub(super) fn latest(&self) -> Option<&MachineLoad> {
        self.latest.as_ref()
    }

    /// Reads the command line of each agent's heaviest processes, and only theirs.
    fn answer(&mut self, sample: Sample) -> MachineLoad {
        let mut agents = Vec::with_capacity(sample.agents.len());
        for agent in sample.agents {
            let heaviest = agent
                .heaviest
                .into_iter()
                .map(|process| CommandLoad {
                    pid: process.pid,
                    command: self.reader.command(process.pid, &process.name),
                    cpu: process.cpu,
                    memory_bytes: process.memory_bytes,
                })
                .collect();
            agents.push(AgentLoad {
                agent: agent.agent,
                cpu: agent.cpu,
                memory_bytes: agent.memory_bytes,
                processes: agent.processes,
                heaviest,
            });
        }
        MachineLoad {
            taken_at: sample.taken_at,
            server: sample.server,
            agents,
            others: sample.others,
            history: self.history.to_wire(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use cockpit_protocol::{AgentId, ProcessId};

    use super::*;
    use crate::machine_load::proc_files::ProcessStat;
    use crate::machine_load::process_tree::{self, ChildrenFiles, FullScan, ProcessSource};

    #[test]
    fn the_second_reading_gives_an_answer_with_this_process_in_it() {
        let mut sampler = Sampler::new();
        let own = AgentRoot {
            agent: AgentId::new(1),
            pid: ProcessId::new(std::process::id()),
        };

        sampler.step(&[own]).unwrap();
        let first = sampler.latest().cloned();
        std::thread::sleep(Duration::from_millis(50));
        sampler.step(&[own]).unwrap();

        assert_eq!(first, None);
        let load = sampler.latest().unwrap();
        assert!(load.server.cores >= 1);
        assert!(load.server.memory_total_bytes > 0);
        let agent = load.agents.first().unwrap();
        assert_eq!(agent.agent, own.agent);
        assert!(agent.memory_bytes > 0);
        assert_eq!(load.history.server.len(), 1);
    }

    /// The CPU time this thread used, in nanoseconds, from the scheduler's own count.
    fn thread_cpu_nanos() -> u64 {
        let text = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap();
        text.split_whitespace().next().unwrap().parse().unwrap()
    }

    /// Runs `work` `count` times and prints its wall and CPU time per run.
    fn measure(label: &str, count: u32, mut work: impl FnMut()) {
        let cpu = thread_cpu_nanos();
        let wall = Instant::now();
        for _ in 0..count {
            work();
        }
        let wall = wall.elapsed() / count;
        let cpu = Duration::from_nanos((thread_cpu_nanos() - cpu) / u64::from(count));
        eprintln!("{label}: {wall:?} wall, {cpu:?} CPU per sample");
    }

    /// The nearest ancestor named `claude`: the agent that runs this test, if one does.
    fn claude_ancestor() -> Option<ProcessId> {
        let mut files = ChildrenFiles::default();
        let mut pid = ProcessId::new(std::process::id());
        while pid.get() > 1 {
            let stat = files.stat(pid)?;
            if stat.name == "claude" {
                return Some(pid);
            }
            pid = stat.parent;
        }
        None
    }

    fn tree_size(root: ProcessId) -> usize {
        process_tree::walk(
            &mut ChildrenFiles::default(),
            root,
            10_000,
            &mut HashSet::new(),
        )
        .len()
    }

    /// Kills the made-up tree when the measure ends.
    struct Tree(Child);

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = Command::new("pkill")
                .arg("-TERM")
                .arg("-g")
                .arg(self.0.id().to_string())
                .status();
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// 20 shells of 15 `sleep`s each under one shell: 321 processes, in their own group.
    fn made_up_tree() -> Tree {
        use std::os::unix::process::CommandExt;

        let script = "for i in $(seq 20); do sh -c 'for j in $(seq 15); do sleep 600 & done; wait' & done; wait";
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        Tree(child)
    }

    #[test]
    #[ignore = "measures the sampling's cost: cargo test -p cockpit_daemon sampling_cost -- --ignored --nocapture"]
    fn sampling_cost() {
        const SAMPLES: u32 = 100;
        let processes = std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.parse::<u32>().is_ok())
            })
            .count();
        eprintln!(
            "{processes} processes on this machine, children files: {}",
            process_tree::children_files_exist()
        );

        let mut server_only = Sampler::new();
        measure("server totals, no agent", SAMPLES, || {
            server_only.step(&[]).unwrap()
        });

        if let Some(claude) = claude_ancestor() {
            let root = AgentRoot {
                agent: AgentId::new(1),
                pid: claude,
            };
            let size = tree_size(claude);
            let threads: u32 = process_tree::walk(
                &mut ChildrenFiles::default(),
                claude,
                10_000,
                &mut HashSet::new(),
            )
            .iter()
            .map(|stat| stat.threads)
            .sum();
            let mut sampler = Sampler::new();
            measure(
                &format!("an agent's tree of {size} processes, {threads} threads"),
                SAMPLES,
                || sampler.step(&[root]).unwrap(),
            );
            let mut files = ChildrenFiles::default();
            measure("  its walk alone", SAMPLES, || {
                process_tree::walk(&mut files, claude, 10_000, &mut HashSet::new());
                files.sweep();
            });
        }

        // Every `claude` of this user, as if one daemon ran them all. Nix's wrapper runs it as
        // `.claude-wrapped`.
        let own_user = rustix::process::getuid().as_raw();
        let claudes: Vec<AgentRoot> = std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                use std::os::unix::fs::MetadataExt;
                entry
                    .metadata()
                    .is_ok_and(|metadata| metadata.uid() == own_user)
            })
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .map(ProcessId::new)
            .filter(|&pid| {
                std::fs::read_to_string(format!("/proc/{}/comm", pid.get()))
                    .is_ok_and(|name| matches!(name.trim(), "claude" | ".claude-wrapped"))
            })
            .zip(10..)
            .map(|(pid, agent)| AgentRoot {
                agent: AgentId::new(agent),
                pid,
            })
            .collect();
        let mut every = ChildrenFiles::default();
        let mut seen = HashSet::new();
        let (mut processes, mut threads) = (0, 0);
        for root in &claudes {
            for stat in process_tree::walk(&mut every, root.pid, 10_000, &mut seen) {
                processes += 1;
                threads += stat.threads;
            }
        }
        let mut sampler = Sampler::new();
        measure(
            &format!(
                "{} agents: {processes} processes, {threads} threads",
                claudes.len()
            ),
            SAMPLES,
            || sampler.step(&claudes).unwrap(),
        );

        let tree = made_up_tree();
        let root = ProcessId::new(tree.0.id());
        let deadline = Instant::now() + Duration::from_secs(20);
        while tree_size(root) < 321 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let size = tree_size(root);
        let agent = AgentRoot {
            agent: AgentId::new(2),
            pid: root,
        };
        let mut sampler = Sampler::new();
        measure(
            &format!("a made-up tree of {size} processes"),
            SAMPLES,
            || {
                sampler.step(&[agent]).unwrap();
            },
        );
        let mut walked: Vec<ProcessStat> = Vec::new();
        measure("the full scan the fallback makes", 10, || {
            let mut scan = FullScan::read().unwrap();
            walked = process_tree::walk(&mut scan, root, 10_000, &mut HashSet::new());
        });
        assert_eq!(walked.len(), size);
    }
}
