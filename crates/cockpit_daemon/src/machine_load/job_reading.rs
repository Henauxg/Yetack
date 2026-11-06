//! One look at a Windows machine: the server's CPU and memory counters, and each agent's job.

use std::io;
use std::time::{Instant, SystemTime};

use cockpit_protocol::{ProcessId, Timestamp};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::SystemInformation::{
    GetSystemTimeAsFileTime, GlobalMemoryStatusEx, MEMORYSTATUSEX,
};
use windows_sys::Win32::System::Threading::{
    ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetSystemTimes, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

use crate::machine_load::agent_root::AgentRoot;
use crate::machine_load::command_text;
use crate::machine_load::job_sample::{AgentFigures, ProcessFigures, ProcessKey, Reading};
use crate::machine_load::opened_process::OpenedProcess;
use crate::process::Job;

/// The most processes one reading opens, all agents together: a fork bomb under an agent costs
/// the daemon a bounded amount of work.
const MAX_PROCESSES: usize = 4096;
/// What the reading needs of each process: its times, and its working set.
const READ_ACCESS: u32 = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ;
/// `IsProcessInJob` needs the limited query right on Windows, and the full one on Wine.
const ROOT_ACCESS: u32 = PROCESS_QUERY_INFORMATION | PROCESS_VM_READ;

#[derive(Debug, thiserror::Error)]
pub(crate) enum LoadError {
    #[error("cannot read the {what}")]
    Read {
        what: &'static str,
        #[source]
        source: io::Error,
    },
}

/// Reads the server's counters and the processes of each root's job.
pub(super) fn read(roots: &[AgentRoot]) -> Result<Reading, LoadError> {
    let at = Instant::now();
    let taken_at =
        Timestamp::from_system_time(SystemTime::now()).unwrap_or(Timestamp::from_unix_millis(0));
    let (idle_time, total_time) = system_times().map_err(|source| LoadError::Read {
        what: "processors' times",
        source,
    })?;
    let (memory_total_bytes, memory_available_bytes) =
        memory().map_err(|source| LoadError::Read {
            what: "memory's use",
            source,
        })?;
    // SAFETY: the call takes a plain value.
    let cores = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
    let mut budget = MAX_PROCESSES;
    let mut agents = Vec::with_capacity(roots.len());
    for root in roots {
        if let Some(agent) = agent_figures(root, &mut budget) {
            agents.push(agent);
        }
    }
    Ok(Reading {
        at,
        taken_at,
        clock: clock(),
        cores: u16::try_from(cores).unwrap_or(u16::MAX),
        idle_time,
        total_time,
        memory_total_bytes,
        memory_available_bytes,
        agents,
    })
}

/// The command line of the process `key` names, shortened. Its image's file name stands in when
/// the line cannot be read, and `[ended]` when the process has.
pub(super) fn command(key: ProcessKey) -> String {
    const ENDED: &str = "[ended]";

    let Ok(process) = OpenedProcess::open(key.pid, PROCESS_QUERY_LIMITED_INFORMATION) else {
        return ENDED.to_owned();
    };
    if process.times().map(|times| times.started).ok() != Some(key.started) {
        return ENDED.to_owned();
    }
    let line = process
        .command_line()
        .ok()
        .and_then(|line| command_text::windows_command(&line));
    line.or_else(|| {
        let image = process.image_path().ok()?;
        command_text::windows_command(&format!("\"{image}\""))
    })
    .unwrap_or_else(|| ENDED.to_owned())
}

/// The agent's processes, within `budget`. `None` when its `claude` has ended, or is no longer
/// the job's.
fn agent_figures(root: &AgentRoot, budget: &mut usize) -> Option<AgentFigures> {
    let claude = OpenedProcess::open(root.pid, ROOT_ACCESS).ok()?;
    let claude_times = claude.times().ok()?;
    let root_key = ProcessKey {
        pid: root.pid,
        started: claude_times.started,
    };
    let Some(job) = &root.job else {
        *budget = budget.saturating_sub(1);
        let figures = ProcessFigures {
            key: root_key,
            cpu_time: claude_times.cpu_time,
            memory_bytes: claude.working_set().unwrap_or(0),
        };
        return Some(AgentFigures {
            agent: root.agent,
            root: root_key,
            cpu_time: claude_times.cpu_time,
            processes: vec![figures],
        });
    };
    if !job.contains(claude.raw()).unwrap_or(false) {
        return None;
    }
    let cpu_time = job.cpu_time().ok()?;
    let processes = job_processes(job, budget);
    Some(AgentFigures {
        agent: root.agent,
        root: root_key,
        cpu_time,
        processes,
    })
}

/// The processes running in `job`, within `budget`. A process that ends meanwhile is left out.
fn job_processes(job: &Job, budget: &mut usize) -> Vec<ProcessFigures> {
    let ids: Vec<ProcessId> = job.process_ids(*budget).unwrap_or_default();
    *budget = budget.saturating_sub(ids.len());
    let mut processes = Vec::with_capacity(ids.len());
    for pid in ids {
        let Ok(process) = OpenedProcess::open(pid, READ_ACCESS) else {
            continue;
        };
        let Ok(times) = process.times() else {
            continue;
        };
        processes.push(ProcessFigures {
            key: ProcessKey {
                pid,
                started: times.started,
            },
            cpu_time: times.cpu_time,
            memory_bytes: process.working_set().unwrap_or(0),
        });
    }
    processes
}

/// The time all the processors spent idle, and in all, in units of 100 ns.
fn system_times() -> io::Result<(u64, u64)> {
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: each pointer is to a `FILETIME`.
    let read = unsafe { GetSystemTimes(&raw mut idle, &raw mut kernel, &raw mut user) };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    // The kernel's time counts the idle time.
    let total = to_u64(kernel).saturating_add(to_u64(user));
    Ok((to_u64(idle), total))
}

/// The physical memory, in all and available, in bytes.
fn memory() -> io::Result<(u64, u64)> {
    let mut status = MEMORYSTATUSEX {
        dwLength: u32::try_from(size_of::<MEMORYSTATUSEX>()).unwrap_or(u32::MAX),
        ..MEMORYSTATUSEX::default()
    };
    // SAFETY: the pointer is to a `MEMORYSTATUSEX` whose length field is set.
    if unsafe { GlobalMemoryStatusEx(&raw mut status) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((status.ullTotalPhys, status.ullAvailPhys))
}

/// Now, in units of 100 ns since 1601, as a process's start is dated.
fn clock() -> u64 {
    let mut now = FILETIME::default();
    // SAFETY: the pointer is to a `FILETIME`.
    unsafe { GetSystemTimeAsFileTime(&raw mut now) };
    to_u64(now)
}

fn to_u64(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use cockpit_protocol::AgentId;
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

    use super::*;
    use crate::process::job;

    #[test]
    fn a_reading_has_the_server_and_each_agents_job() {
        let mut claude = Command::new("cmd.exe")
            .raw_arg("/d /c \"ping -n 30 127.0.0.1 >nul\"")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
            .spawn()
            .unwrap();
        let job = job::start_in_job(claude.as_raw_handle(), claude.id())
            .unwrap()
            .expect("a job");
        // The shell starts `ping` meanwhile.
        std::thread::sleep(Duration::from_millis(500));
        let roots = [
            AgentRoot {
                agent: AgentId::new(1),
                pid: ProcessId::new(claude.id()),
                job: Some(job),
            },
            AgentRoot {
                agent: AgentId::new(2),
                pid: ProcessId::new(std::process::id()),
                job: None,
            },
        ];

        let reading = read(&roots).unwrap();

        let _ = claude.kill();
        let _ = claude.wait();
        assert!(reading.cores >= 1);
        assert!(reading.total_time >= reading.idle_time);
        assert!(reading.memory_total_bytes >= reading.memory_available_bytes);
        assert!(reading.clock > 0);
        let agents: Vec<(u32, usize)> = reading
            .agents
            .iter()
            .map(|agent| (agent.agent.get(), agent.processes.len()))
            .collect();
        let in_job = agents.first().map_or(0, |(_, processes)| *processes);
        assert!(in_job >= 1, "{agents:?}");
        assert_eq!(agents.get(1), Some(&(2, 1)), "{agents:?}");
        let own = reading.agents.get(1).unwrap();
        assert!(own.processes.first().unwrap().memory_bytes > 0);
        let own_command = command(own.root);
        assert!(own_command.contains("cockpit_daemon"), "{own_command}");
    }
}
