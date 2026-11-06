use cockpit_protocol::{AgentId, ProcessId};

#[cfg(windows)]
use crate::process::Job;
use crate::process::ProcessHandle;

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
        Some(Self { agent, pid, job })
    }
}
