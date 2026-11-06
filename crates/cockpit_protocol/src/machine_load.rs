//! How busy the daemon's machine is, and each agent's share of it, as `machine.load` answers.
//!
//! An agent's figures sum its `claude` process and every process under it: the builds, tests and
//! servers it runs.

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, ProcessId};
use crate::time::Timestamp;

/// CPU in use, in cores, averaged over a sample: 2.5 is two cores busy and a third one half the
/// time.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cores(f64);

impl Cores {
    #[must_use]
    pub fn new(cores: f64) -> Self {
        Self(cores)
    }

    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

/// The latest sample, and the samples of the last five minutes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineLoad {
    pub taken_at: Timestamp,
    pub server: ServerLoad,
    /// One per agent whose process runs.
    pub agents: Vec<AgentLoad>,
    /// The server's use minus the agents', never below zero.
    pub others: ResourceUse,
    pub history: LoadHistory,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerLoad {
    /// The cores the kernel has online.
    pub cores: u16,
    pub cpu: Cores,
    /// The total minus what the kernel counts as available.
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    /// `None` on Windows, which has no load average. Missing reads as `None`.
    #[serde(default)]
    pub load_average: Option<LoadAverage>,
}

/// The run queue's length, averaged over each window, as `/proc/loadavg` gives it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LoadAverage {
    pub one_minute: f64,
    pub five_minutes: f64,
    pub fifteen_minutes: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct ResourceUse {
    pub cpu: Cores,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLoad {
    pub agent: AgentId,
    pub cpu: Cores,
    /// The processes' resident memory, summed: a page that several of them share counts once
    /// for each.
    pub memory_bytes: u64,
    /// `claude` and every process under it.
    pub processes: u32,
    /// The processes under `claude` that use the most CPU, then memory, heaviest first.
    pub heaviest: Vec<CommandLoad>,
}

/// One process an agent runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandLoad {
    pub pid: ProcessId,
    /// Its command line, shortened: the program's file name, then its arguments.
    pub command: String,
    pub cpu: Cores,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LoadHistory {
    /// One point per sample, oldest first. The last one is the latest sample.
    pub server: Vec<ServerPoint>,
    /// One per agent that ran in any of the samples.
    pub agents: Vec<AgentHistory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ServerPoint {
    pub taken_at: Timestamp,
    pub cpu: Cores,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentHistory {
    pub agent: AgentId,
    /// One per point of the server's history, at the same moments. `None` where the agent ran no
    /// process.
    pub cpu: Vec<Option<Cores>>,
}
