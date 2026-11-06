//! One look at the machine: the server's counters, and each agent's processes.

use std::collections::{HashMap, HashSet};
use std::io;
use std::time::{Instant, SystemTime};

use cockpit_protocol::{AgentId, LoadAverage, ProcessId, Timestamp};

use crate::machine_load::agent_root::AgentRoot;
use crate::machine_load::kept_file::KeptFile;
use crate::machine_load::proc_files::{
    self, CHILDREN_FILE, CMDLINE_FILE, LOADAVG_PATH, MEMINFO_PATH, MemoryInfo, PROC_ROOT,
    ProcessKey, ProcessStat, SERVER_STAT_PATH, ServerCpu, TASK_FOLDER, UPTIME_PATH,
};
use crate::machine_load::process_tree::{self, ChildrenFiles, FullScan, ProcessSource};

/// The most processes one reading walks, all agents together: a fork bomb under an agent costs
/// the daemon a bounded amount of work.
const MAX_PROCESSES: usize = 4096;

/// Why the machine's load cannot be read: this machine has no `/proc`, or one of another shape.
#[derive(Debug, thiserror::Error)]
pub(crate) enum LoadError {
    #[error("cannot read {path}")]
    Read {
        path: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{path} does not have the expected format")]
    Malformed { path: &'static str },
}

#[derive(Debug, Clone)]
pub(super) struct Reading {
    pub(super) at: Instant,
    pub(super) taken_at: Timestamp,
    pub(super) uptime_ticks: u64,
    pub(super) cpu: ServerCpu,
    pub(super) memory: MemoryInfo,
    pub(super) load_average: LoadAverage,
    pub(super) agents: Vec<AgentProcesses>,
}

#[derive(Debug, Clone)]
pub(super) struct AgentProcesses {
    pub(super) agent: AgentId,
    /// The agent's `claude` process first.
    pub(super) processes: Vec<ProcessStat>,
    /// Processes of the previous reading's tree that left it and still run, as a server started
    /// in the background does when its shell exits. The CPU they used went to no one in the tree.
    pub(super) left_running: HashSet<ProcessKey>,
}

impl AgentProcesses {
    pub(super) fn root(&self) -> Option<&ProcessStat> {
        self.processes.first()
    }
}

/// Where the walk reads processes on this kernel.
#[derive(Debug)]
enum Tree {
    ChildrenFiles(ChildrenFiles),
    /// A new scan for each reading.
    FullScan,
}

#[derive(Debug)]
pub(super) struct MachineReader {
    tree: Tree,
    ticks_per_second: u64,
    uptime: KeptFile,
    server_stat: KeptFile,
    meminfo: KeptFile,
    loadavg: KeptFile,
    buffer: Vec<u8>,
}

impl MachineReader {
    pub(super) fn new(ticks_per_second: u64) -> Self {
        let tree = if process_tree::children_files_exist() {
            Tree::ChildrenFiles(ChildrenFiles::default())
        } else {
            tracing::warn!(
                "this kernel has no {PROC_ROOT}/<pid>/{TASK_FOLDER}/<tid>/{CHILDREN_FILE}: the machine load reads every process"
            );
            Tree::FullScan
        };
        Self {
            tree,
            ticks_per_second,
            uptime: KeptFile::new(UPTIME_PATH),
            server_stat: KeptFile::new(SERVER_STAT_PATH),
            meminfo: KeptFile::new(MEMINFO_PATH),
            loadavg: KeptFile::new(LOADAVG_PATH),
            buffer: Vec::new(),
        }
    }

    /// Reads the server and the processes under each root. `previous` tells which of its
    /// processes left a tree while still running.
    pub(super) fn read(
        &mut self,
        roots: &[AgentRoot],
        previous: Option<&Reading>,
    ) -> Result<Reading, LoadError> {
        let at = Instant::now();
        let taken_at = Timestamp::from_system_time(SystemTime::now())
            .unwrap_or(Timestamp::from_unix_millis(0));
        let ticks_per_second = self.ticks_per_second;
        let uptime_ticks = read_parsed(&mut self.uptime, &mut self.buffer, |text| {
            proc_files::parse_uptime_ticks(text, ticks_per_second)
        })?;
        let cpu = read_parsed(
            &mut self.server_stat,
            &mut self.buffer,
            proc_files::parse_server_cpu,
        )?;
        let memory = read_parsed(
            &mut self.meminfo,
            &mut self.buffer,
            proc_files::parse_meminfo,
        )?;
        let load_average = read_parsed(
            &mut self.loadavg,
            &mut self.buffer,
            proc_files::parse_loadavg,
        )?;
        let agents = match &mut self.tree {
            Tree::ChildrenFiles(files) => {
                let agents = read_agents(files, roots, previous);
                files.sweep();
                agents
            }
            Tree::FullScan => match FullScan::read() {
                Ok(mut scan) => read_agents(&mut scan, roots, previous),
                Err(error) => {
                    tracing::warn!(%error, "cannot list the processes");
                    Vec::new()
                }
            },
        };
        Ok(Reading {
            at,
            taken_at,
            uptime_ticks,
            cpu,
            memory,
            load_average,
            agents,
        })
    }

    /// The process's command line, shortened. `name` stands in when it cannot be read.
    pub(super) fn command(&mut self, pid: ProcessId, name: &str) -> String {
        let path = format!("{PROC_ROOT}/{}/{CMDLINE_FILE}", pid.get());
        let cmdline = std::fs::read(path).unwrap_or_default();
        proc_files::short_command(&cmdline, name)
    }
}

fn read_parsed<T>(
    file: &mut KeptFile,
    buffer: &mut Vec<u8>,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Result<T, LoadError> {
    let path = file.path();
    file.read(buffer)
        .map_err(|source| LoadError::Read { path, source })?;
    std::str::from_utf8(buffer)
        .ok()
        .and_then(parse)
        .ok_or(LoadError::Malformed { path })
}

fn read_agents(
    source: &mut impl ProcessSource,
    roots: &[AgentRoot],
    previous: Option<&Reading>,
) -> Vec<AgentProcesses> {
    let before: HashMap<AgentId, &AgentProcesses> = previous
        .map(|reading| {
            reading
                .agents
                .iter()
                .map(|agent| (agent.agent, agent))
                .collect()
        })
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut agents = Vec::with_capacity(roots.len());
    for root in roots {
        let limit = MAX_PROCESSES.saturating_sub(seen.len());
        let processes = process_tree::walk(source, root.pid, limit, &mut seen);
        if processes.is_empty() {
            continue;
        }
        let left_running = before
            .get(&root.agent)
            .map(|earlier| left_running(source, earlier, &processes))
            .unwrap_or_default();
        agents.push(AgentProcesses {
            agent: root.agent,
            processes,
            left_running,
        });
    }
    agents
}

/// The processes of `earlier` that are not in `now` and still run: their `stat` reads, with the
/// same start time.
fn left_running(
    source: &mut impl ProcessSource,
    earlier: &AgentProcesses,
    now: &[ProcessStat],
) -> HashSet<ProcessKey> {
    let current: HashSet<ProcessKey> = now.iter().map(|stat| stat.key).collect();
    let mut running = HashSet::new();
    for stat in &earlier.processes {
        if current.contains(&stat.key) {
            continue;
        }
        let still = source
            .stat(stat.key.pid)
            .is_some_and(|found| found.key == stat.key);
        if still {
            running.insert(stat.key);
        }
    }
    running
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_load::proc_files::CpuTimes;
    use crate::machine_load::process_tree::tests::FakeProc;

    fn root(agent: u32, pid: u32) -> AgentRoot {
        AgentRoot {
            agent: AgentId::new(agent),
            pid: ProcessId::new(pid),
        }
    }

    #[test]
    fn each_agent_gets_its_own_tree_and_a_server_left_behind_is_kept_apart() {
        let mut proc = FakeProc::default();
        proc.add(20, "claude", 10, (300, 300), 50_000);
        proc.add(21, "bash", 20, (2, 2), 900);
        proc.add(22, "npm run dev", 21, (40, 40), 9_000);
        proc.add(30, "claude", 10, (200, 200), 40_000);
        let roots = [root(1, 20), root(2, 30), root(3, 99)];
        let earlier = read_agents(&mut proc, &roots, None);
        // The shell exits: the dev server now belongs to init, and the shell is gone.
        proc.stats.remove(&ProcessId::new(21));
        proc.children.remove(&ProcessId::new(20));
        proc.add(22, "npm run dev", 1, (40, 40), 9_000);
        let previous = Reading {
            at: Instant::now(),
            taken_at: Timestamp::from_unix_millis(0),
            uptime_ticks: 0,
            cpu: ServerCpu {
                times: CpuTimes { busy: 0, total: 0 },
                cores: 1,
            },
            memory: MemoryInfo {
                total_bytes: 0,
                available_bytes: 0,
            },
            load_average: LoadAverage {
                one_minute: 0.0,
                five_minutes: 0.0,
                fifteen_minutes: 0.0,
            },
            agents: earlier.clone(),
        };

        let now = read_agents(&mut proc, &roots, Some(&previous));

        let sizes: Vec<(u32, usize)> = earlier
            .iter()
            .map(|agent| (agent.agent.get(), agent.processes.len()))
            .collect();
        assert_eq!(sizes, [(1, 3), (2, 1)], "agent 3 has no process");
        let first = now.first().unwrap();
        assert_eq!(first.processes.len(), 1);
        let left: Vec<u32> = first.left_running.iter().map(|key| key.pid.get()).collect();
        assert_eq!(left, [22], "the shell ended, the server still runs");
    }

    #[test]
    fn the_real_proc_reads() {
        let mut reader = MachineReader::new(100);
        let own = root(1, std::process::id());

        let reading = reader.read(&[own], None).unwrap();

        assert!(reading.cpu.cores >= 1);
        assert!(reading.memory.total_bytes >= reading.memory.available_bytes);
        let agent = reading.agents.first().unwrap();
        assert_eq!(agent.root().map(|stat| stat.key.pid), Some(own.pid));
        let command = reader.command(own.pid, "test");
        assert!(!command.is_empty());
    }
}
