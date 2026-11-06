//! `agent.stop_process`: SIGTERM to one process under an agent's `claude`, and to no other.
//!
//! The daemon opens a pidfd on the process first, then checks its parents in `/proc`, then
//! signals through the pidfd. When the process ends and its pid goes to another process during
//! the check, the check reads the other process, but the signal goes to the ended one, and fails.

use std::io;

use cockpit_protocol::{ErrorBody, ErrorCode, ProcessId};
use rustix::process::{Pid, PidfdFlags, Signal};

use crate::error_chain;
use crate::machine_load::proc_files::{PROC_ROOT, ProcessStat, STAT_FILE};

/// Longer than any real chain: a loop in a corrupt `/proc` cannot run forever.
const MAX_CHAIN: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub(crate) enum StopProcessError {
    #[error("process {} is the agent's claude: stop the agent instead", .pid.get())]
    IsTheAgent { pid: ProcessId },
    #[error("process {} is not one of the agent's", .pid.get())]
    NotTheAgents { pid: ProcessId },
    #[error("process {} has ended", .pid.get())]
    Ended { pid: ProcessId },
    #[error("cannot read the parents of process {}", .pid.get())]
    Proc {
        pid: ProcessId,
        #[source]
        source: io::Error,
    },
    #[error("cannot stop process {}", .pid.get())]
    Signal {
        pid: ProcessId,
        #[source]
        source: io::Error,
    },
}

impl StopProcessError {
    pub(crate) fn to_error_body(&self) -> ErrorBody {
        let code = match self {
            Self::IsTheAgent { .. } => ErrorCode::InvalidRequest,
            Self::NotTheAgents { .. } | Self::Ended { .. } => ErrorCode::NotFound,
            Self::Proc { .. } | Self::Signal { .. } => ErrorCode::Internal,
        };
        ErrorBody::new(code, error_chain::chain(self))
    }
}

/// Sends SIGTERM to `target` when it descends from `root`, the agent's `claude`.
pub(crate) fn stop_descendant(root: ProcessId, target: ProcessId) -> Result<(), StopProcessError> {
    if target == root {
        return Err(StopProcessError::IsTheAgent { pid: target });
    }
    let pid = i32::try_from(target.get())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or(StopProcessError::NotTheAgents { pid: target })?;
    let pidfd = rustix::process::pidfd_open(pid, PidfdFlags::empty()).map_err(|errno| {
        if errno == rustix::io::Errno::SRCH {
            StopProcessError::Ended { pid: target }
        } else {
            StopProcessError::Signal {
                pid: target,
                source: errno.into(),
            }
        }
    })?;
    if !descends_from(target, root, read_parent)? {
        return Err(StopProcessError::NotTheAgents { pid: target });
    }
    rustix::process::pidfd_send_signal(&pidfd, Signal::TERM).map_err(|errno| {
        if errno == rustix::io::Errno::SRCH {
            StopProcessError::Ended { pid: target }
        } else {
            StopProcessError::Signal {
                pid: target,
                source: errno.into(),
            }
        }
    })
}

/// Whether `root` is among the parents of `target`, read by `read_parent`, which gives `None`
/// for a process that ended.
fn descends_from(
    target: ProcessId,
    root: ProcessId,
    read_parent: impl Fn(ProcessId) -> io::Result<Option<ProcessId>>,
) -> Result<bool, StopProcessError> {
    let mut pid = target;
    for _ in 0..MAX_CHAIN {
        let parent = read_parent(pid).map_err(|source| StopProcessError::Proc { pid, source })?;
        let Some(parent) = parent else {
            return Ok(false);
        };
        if parent == root {
            return Ok(true);
        }
        // 1 is init, the parent of every orphan; 0 is the kernel.
        if parent.get() <= 1 {
            return Ok(false);
        }
        pid = parent;
    }
    Ok(false)
}

fn read_parent(pid: ProcessId) -> io::Result<Option<ProcessId>> {
    let path = format!("{PROC_ROOT}/{}/{STAT_FILE}", pid.get());
    match std::fs::read_to_string(path) {
        Ok(line) => ProcessStat::parse(&line)
            .map(|stat| Some(stat.parent))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "a malformed stat line")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// 1 init; 20 an agent's claude, 21 its shell, 22 the build the shell runs; 30 another
    /// agent's claude; 40 the user's editor.
    fn parents() -> impl Fn(ProcessId) -> io::Result<Option<ProcessId>> {
        let table: HashMap<u32, u32> = HashMap::from([
            (1, 0),
            (20, 10),
            (10, 1),
            (21, 20),
            (22, 21),
            (30, 10),
            (40, 1),
        ]);
        move |pid| Ok(table.get(&pid.get()).copied().map(ProcessId::new))
    }

    #[test]
    fn only_a_descendant_of_the_agent_is_its_process() {
        let claude = ProcessId::new(20);
        let check = |pid: u32| descends_from(ProcessId::new(pid), claude, parents()).unwrap();

        assert!(check(21));
        assert!(check(22));
        assert!(!check(30), "another agent's claude");
        assert!(!check(40), "a process of the user");
        assert!(!check(10), "the daemon");
        assert!(!check(99), "an ended process");
    }

    #[test]
    fn a_loop_in_the_parents_ends_the_walk() {
        let looping = |pid: ProcessId| Ok(Some(pid));

        let found = descends_from(ProcessId::new(5), ProcessId::new(20), looping).unwrap();

        assert!(!found);
    }

    #[test]
    fn the_agent_itself_and_a_process_outside_it_are_refused() {
        let claude = ProcessId::new(std::process::id());

        let itself = stop_descendant(claude, claude);
        // The test's parent does not descend from the test.
        let parent = read_parent(claude).unwrap().unwrap();
        let outside = stop_descendant(claude, parent);
        let gone = stop_descendant(claude, ProcessId::new(u32::MAX));

        assert!(
            matches!(itself, Err(StopProcessError::IsTheAgent { .. })),
            "{itself:?}"
        );
        assert!(
            matches!(outside, Err(StopProcessError::NotTheAgents { .. })),
            "{outside:?}"
        );
        assert!(
            matches!(gone, Err(StopProcessError::NotTheAgents { .. })),
            "{gone:?}"
        );
        assert_eq!(
            outside.unwrap_err().to_error_body().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn a_child_of_the_agent_gets_sigterm() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let claude = ProcessId::new(std::process::id());

        let stopped = stop_descendant(claude, ProcessId::new(child.id()));

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut status = child.try_wait().unwrap();
        while status.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
            status = child.try_wait().unwrap();
        }
        if status.is_none() {
            let _ = child.kill();
        }
        assert!(stopped.is_ok(), "{stopped:?}");
        let signal = status.and_then(|status| {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        });
        assert_eq!(signal, Some(Signal::TERM.as_raw()));
    }
}
