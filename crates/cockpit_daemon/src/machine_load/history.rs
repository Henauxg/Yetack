//! The last [`HISTORY_SAMPLES`] samples' CPU, for the sparklines. Kept in memory only: the event
//! logs never see them.

use std::collections::{BTreeSet, VecDeque};

use cockpit_protocol::{AgentHistory, AgentId, Cores, LoadHistory, ServerPoint, Timestamp};

use crate::machine_load::sample::Sample;

/// Five minutes of samples taken every 3 seconds.
pub(super) const HISTORY_SAMPLES: usize = 100;

#[derive(Debug, Default)]
pub(super) struct History {
    points: VecDeque<Point>,
}

#[derive(Debug)]
struct Point {
    taken_at: Timestamp,
    server_cpu: Cores,
    agents: Vec<AgentPoint>,
}

#[derive(Debug)]
struct AgentPoint {
    agent: AgentId,
    cpu: Cores,
}

impl History {
    /// Adds the sample, and forgets the oldest past [`HISTORY_SAMPLES`].
    pub(super) fn push(&mut self, sample: &Sample) {
        if self.points.len() >= HISTORY_SAMPLES {
            self.points.pop_front();
        }
        self.points.push_back(Point {
            taken_at: sample.taken_at,
            server_cpu: sample.server.cpu,
            agents: sample
                .agents
                .iter()
                .map(|agent| AgentPoint {
                    agent: agent.agent,
                    cpu: agent.cpu,
                })
                .collect(),
        });
    }

    pub(super) fn to_wire(&self) -> LoadHistory {
        let server = self
            .points
            .iter()
            .map(|point| ServerPoint {
                taken_at: point.taken_at,
                cpu: point.server_cpu,
            })
            .collect();
        let agent_ids: BTreeSet<AgentId> = self
            .points
            .iter()
            .flat_map(|point| point.agents.iter().map(|agent| agent.agent))
            .collect();
        let agents = agent_ids
            .into_iter()
            .map(|agent| AgentHistory {
                agent,
                cpu: self
                    .points
                    .iter()
                    .map(|point| {
                        point
                            .agents
                            .iter()
                            .find(|found| found.agent == agent)
                            .map(|found| found.cpu)
                    })
                    .collect(),
            })
            .collect();
        LoadHistory { server, agents }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{LoadAverage, ResourceUse, ServerLoad};

    use super::*;
    use crate::machine_load::sample::AgentSample;

    /// Sample `index`, with the server at `index` cores and agent 1 at half of it. Agent 2 runs
    /// from sample 150.
    fn sample(index: u32) -> Sample {
        let agent = |id: u32, cpu: f64| AgentSample {
            agent: AgentId::new(id),
            cpu: Cores::new(cpu),
            memory_bytes: 0,
            processes: 1,
            heaviest: Vec::new(),
        };
        let mut agents = vec![agent(1, f64::from(index) / 2.0)];
        if index >= 150 {
            agents.push(agent(2, 1.0));
        }
        Sample {
            taken_at: Timestamp::from_unix_millis(u64::from(index) * 3000),
            server: ServerLoad {
                cores: 64,
                cpu: Cores::new(f64::from(index)),
                memory_used_bytes: 0,
                memory_total_bytes: 0,
                load_average: LoadAverage {
                    one_minute: 0.0,
                    five_minutes: 0.0,
                    fifteen_minutes: 0.0,
                },
            },
            agents,
            others: ResourceUse::default(),
        }
    }

    #[test]
    fn the_history_keeps_the_last_five_minutes_aligned_per_agent() {
        let mut history = History::default();
        for index in 0..200 {
            history.push(&sample(index));
        }

        let wire = history.to_wire();

        assert_eq!(wire.server.len(), HISTORY_SAMPLES);
        let first = wire.server.first().unwrap();
        let last = wire.server.last().unwrap();
        assert_eq!(
            (first.cpu, last.cpu),
            (Cores::new(100.0), Cores::new(199.0))
        );
        assert_eq!(last.taken_at, Timestamp::from_unix_millis(199 * 3000));
        let agents: Vec<u32> = wire.agents.iter().map(|agent| agent.agent.get()).collect();
        assert_eq!(agents, [1, 2]);
        let second = &wire.agents.last().unwrap().cpu;
        assert_eq!(second.len(), HISTORY_SAMPLES);
        assert_eq!(second.iter().filter(|cpu| cpu.is_none()).count(), 50);
        assert_eq!(second.last(), Some(&Some(Cores::new(1.0))));
    }
}
