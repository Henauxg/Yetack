use cockpit_protocol::{AgentId, ProcessId};

#[cfg(windows)]
use crate::process::Job;
use crate::process::ProcessHandle;
#[cfg(any(windows, test))]
use crate::process::program::Launch;

/// An agent and its `claude` process, where the walk starts.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgentRoot {
    pub(crate) agent: AgentId,
    pub(crate) pid: ProcessId,
}

/// An agent, its `claude` process, and the job that holds every process the agent runs.
#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct AgentRoot {
    pub(crate) agent: AgentId,
    pub(crate) pid: ProcessId,
    /// `None` when `claude` runs without a job, or the job cannot be opened again.
    pub(crate) job: Option<Job>,
    pub(crate) launch: Launch,
}

impl AgentRoot {
    /// `None` when the process has ended.
    #[cfg(unix)]
    pub(crate) fn new<T>(agent: AgentId, process: &ProcessHandle<T>) -> Option<Self> {
        let pid = process.pid()?;
        Some(Self { agent, pid })
    }

    /// `None` when the process has ended.
    #[cfg(windows)]
    pub(crate) fn new<T>(agent: AgentId, process: &ProcessHandle<T>) -> Option<Self> {
        let pid = process.pid()?;
        let job = process.job().and_then(|job| {
            job.try_clone()
                .inspect_err(|error| tracing::warn!(%error, "cannot open an agent's job again"))
                .ok()
        });
        Some(Self {
            agent,
            pid,
            job,
            launch: process.launch(),
        })
    }
}

/// Whether the process `pid`, started by `parent`, counts as the agent's `claude`: the agent's
/// root process, or a child of the root when the root is the interpreter of an npm `claude.cmd`.
#[cfg(any(windows, test))]
pub(crate) fn is_claude(
    root: ProcessId,
    launch: Launch,
    pid: ProcessId,
    parent: Option<ProcessId>,
) -> bool {
    pid == root || (launch == Launch::Interpreter && parent == Some(root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interpreters_children_count_as_claude() {
        let root = ProcessId::new(10);
        let claude = |launch, pid: u32, parent: u32| {
            is_claude(
                root,
                launch,
                ProcessId::new(pid),
                Some(ProcessId::new(parent)),
            )
        };

        assert!(claude(Launch::Direct, 10, 1));
        assert!(!claude(Launch::Direct, 11, 10), "a command claude runs");
        assert!(claude(Launch::Interpreter, 10, 1));
        assert!(claude(Launch::Interpreter, 11, 10), "node under cmd.exe");
        assert!(!claude(Launch::Interpreter, 12, 11), "a command node runs");
        assert!(!is_claude(
            root,
            Launch::Interpreter,
            ProcessId::new(11),
            None
        ));
    }
}
