use cockpit_protocol::{AgentId, ProcessId};

/// An agent and its `claude` process, where the walk starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AgentRoot {
    pub(crate) agent: AgentId,
    pub(crate) pid: ProcessId,
}
