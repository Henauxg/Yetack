//! The sampling task. It starts with the first `machine.load` and ends 30 seconds after the last
//! one, so the daemon reads the machine only while a client shows the load. Its history then
//! starts afresh with the next question.

use std::time::Duration;

use cockpit_protocol::{ErrorBody, ErrorCode, MachineLoad, ResponseBody, ResponseResult};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::error_chain;
use crate::machine_load::agent_root::AgentRoot;
use crate::machine_load::{LoadError, Sampler};

/// Where the answer to a `machine.load` goes.
pub(crate) type Reply = oneshot::Sender<ResponseResult>;

/// Questions waiting while a reading runs. Clients ask every few seconds, so a full queue means
/// something asks far too often.
const QUERY_QUEUE: usize = 16;

/// When the task samples, and when it stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Timing {
    /// From the first reading to the second, which makes the first sample.
    first_sample: Duration,
    interval: Duration,
    /// How long the task runs after the last question.
    idle_stop: Duration,
}

const TIMING: Timing = Timing {
    first_sample: Duration::from_secs(1),
    interval: Duration::from_secs(3),
    idle_stop: Duration::from_secs(30),
};

struct Query {
    roots: Vec<AgentRoot>,
    reply: Reply,
}

/// The hub's side of the sampling task, which it starts when needed.
#[derive(Debug)]
pub(crate) struct LoadSampler {
    queries: Option<mpsc::Sender<Query>>,
    timing: Timing,
}

impl Default for LoadSampler {
    fn default() -> Self {
        Self {
            queries: None,
            timing: TIMING,
        }
    }
}

impl LoadSampler {
    /// Answers with the latest sample, or with the first one once it is taken. `roots` are the
    /// agents' `claude` processes now, which the next readings walk from.
    pub(crate) fn query(&mut self, roots: Vec<AgentRoot>, reply: Reply) {
        let mut query = Query { roots, reply };
        if let Some(queries) = &self.queries {
            match queries.try_send(query) {
                Ok(()) => return,
                Err(TrySendError::Full(full)) => {
                    let busy = ErrorBody::new(
                        ErrorCode::Internal,
                        "too many machine.load requests wait".to_owned(),
                    );
                    let _ = full.reply.send(ResponseResult::Error(busy));
                    return;
                }
                Err(TrySendError::Closed(closed)) => query = closed,
            }
        }
        let (sender, receiver) = mpsc::channel(QUERY_QUEUE);
        // A new queue has room for one.
        let _ = sender.try_send(query);
        tokio::spawn(run(receiver, self.timing));
        self.queries = Some(sender);
    }
}

async fn run(mut queries: mpsc::Receiver<Query>, timing: Timing) {
    let mut sampler = Some(Sampler::new());
    let mut roots: Vec<AgentRoot> = Vec::new();
    let mut waiting: Vec<Reply> = Vec::new();
    let mut last_asked = Instant::now();
    let mut next = Instant::now();
    let mut first = true;
    loop {
        tokio::select! {
            // The first question gives the roots of the first reading.
            biased;
            query = queries.recv() => {
                let Some(query) = query else {
                    return;
                };
                last_asked = Instant::now();
                roots = query.roots;
                match sampler.as_ref().and_then(Sampler::latest) {
                    Some(load) => answer(query.reply, Ok(load.clone())),
                    None => waiting.push(query.reply),
                }
            }
            () = tokio::time::sleep_until(next) => {
                if last_asked.elapsed() >= timing.idle_stop {
                    let latest = sampler.as_ref().and_then(Sampler::latest).cloned();
                    let last = latest.ok_or_else(|| {
                        ErrorBody::new(ErrorCode::Internal, "the sampling stopped".to_owned())
                    });
                    return finish(queries, waiting, &last);
                }
                let Some(mut owned) = sampler.take() else {
                    return;
                };
                // The roots go to the reading and come back: on Windows, each holds a handle.
                let walked = std::mem::take(&mut roots);
                let stepped = tokio::task::spawn_blocking(move || {
                    let result = owned.step(&walked);
                    (owned, walked, result)
                })
                .await;
                let failure = match stepped {
                    Ok((back, walked, Ok(()))) => {
                        if let Some(load) = back.latest() {
                            for reply in waiting.drain(..) {
                                answer(reply, Ok(load.clone()));
                            }
                        }
                        sampler = Some(back);
                        roots = walked;
                        None
                    }
                    Ok((_, _, Err(error))) => Some(unsupported(&error)),
                    Err(error) => Some(ErrorBody::new(ErrorCode::Internal, error.to_string())),
                };
                if let Some(failure) = failure {
                    tracing::info!(error = %failure.message, "cannot read the machine load");
                    return finish(queries, waiting, &Err(failure));
                }
                next = if first {
                    Instant::now() + timing.first_sample
                } else {
                    next + timing.interval
                };
                first = false;
            }
        }
    }
}

/// Stops taking questions, and answers the ones waiting with `last`.
fn finish(
    mut queries: mpsc::Receiver<Query>,
    waiting: Vec<Reply>,
    last: &Result<MachineLoad, ErrorBody>,
) {
    queries.close();
    let mut replies = waiting;
    while let Ok(query) = queries.try_recv() {
        replies.push(query.reply);
    }
    for reply in replies {
        answer(reply, last.clone());
    }
}

fn answer(reply: Reply, load: Result<MachineLoad, ErrorBody>) {
    let result = match load {
        Ok(load) => ResponseResult::Ok(ResponseBody::MachineLoad(load)),
        Err(error) => ResponseResult::Error(error),
    };
    // The client may have left.
    let _ = reply.send(result);
}

fn unsupported(error: &LoadError) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::Unsupported,
        format!("cannot measure the load: {}", error_chain::chain(error)),
    )
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentId, ProcessId};

    use super::*;

    const QUICK: Timing = Timing {
        first_sample: Duration::from_millis(20),
        interval: Duration::from_millis(20),
        idle_stop: Duration::from_millis(200),
    };

    async fn ask(sampler: &mut LoadSampler) -> MachineLoad {
        // On Windows, an agent without a job counts its `claude` alone: here, the test.
        let own = AgentRoot {
            agent: AgentId::new(1),
            pid: ProcessId::new(std::process::id()),
            #[cfg(windows)]
            job: None,
            #[cfg(windows)]
            launch: crate::process::program::Launch::Direct,
        };
        let (reply, answer) = oneshot::channel();
        sampler.query(vec![own], reply);
        let result = tokio::time::timeout(Duration::from_secs(5), answer)
            .await
            .unwrap()
            .unwrap();
        match result {
            ResponseResult::Ok(ResponseBody::MachineLoad(load)) => load,
            other => panic!("expected the load, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_sampling_runs_while_asked_and_starts_again_after_it_stopped() {
        let mut sampler = LoadSampler {
            queries: None,
            timing: QUICK,
        };

        let first = ask(&mut sampler).await;
        tokio::time::sleep(QUICK.interval * 3).await;
        let later = ask(&mut sampler).await;
        let running = sampler
            .queries
            .as_ref()
            .is_some_and(|queries| !queries.is_closed());
        tokio::time::sleep(QUICK.idle_stop + QUICK.interval * 3).await;
        let stopped = sampler
            .queries
            .as_ref()
            .is_some_and(mpsc::Sender::is_closed);
        let restarted = ask(&mut sampler).await;

        assert!(
            first
                .agents
                .iter()
                .any(|agent| agent.agent == AgentId::new(1))
        );
        assert!(later.history.server.len() > first.history.server.len());
        assert!(running, "the task runs while asked");
        assert!(stopped, "the task ends once nobody asks");
        assert_eq!(restarted.history.server.len(), 1, "a new history");
    }
}
