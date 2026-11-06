//! Takes the readings on Windows, turns each pair into a sample, and keeps the answer to
//! `machine.load`.

use cockpit_protocol::{AgentLoad, CommandLoad, MachineLoad};

use crate::machine_load::agent_root::AgentRoot;
use crate::machine_load::history::History;
use crate::machine_load::job_reading::{self, LoadError};
use crate::machine_load::job_sample::{self, Reading, Sample};

#[derive(Debug, Default)]
pub(super) struct Sampler {
    previous: Option<Reading>,
    history: History,
    latest: Option<MachineLoad>,
}

impl Sampler {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Reads the machine and the jobs of `roots`. From the second reading on, each one makes a
    /// sample.
    pub(super) fn step(&mut self, roots: &[AgentRoot]) -> Result<(), LoadError> {
        let current = job_reading::read(roots)?;
        if let Some(previous) = &self.previous {
            let sample = job_sample::sample(previous, &current);
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
    fn answer(&self, sample: Sample) -> MachineLoad {
        let mut agents = Vec::with_capacity(sample.agents.len());
        for agent in sample.agents {
            let heaviest = agent
                .heaviest
                .into_iter()
                .map(|process| CommandLoad {
                    pid: process.key.pid,
                    command: job_reading::command(process.key),
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
