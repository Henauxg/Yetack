//! Updates this computer's daemon when it is the background service: stops the daemon that holds
//! the state folder's lock, then starts the service with this app's own program. The order lives
//! here, on every platform. The system's own calls sit behind `ServiceControl`, which the tests
//! replace.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use cockpit_protocol::AgentId;

use crate::state::daemon_update::{RestartFailure, StopFailure};
use crate::state::update_plan::{ServicePlan, UpdateStep};

/// What the service writes to stderr, in the app's state folder.
pub const SERVICE_LOG: &str = "serve.log";

/// Why the background service cannot run on this system.
#[cfg(not(unix))]
pub const UNIX_ONLY: &str = "the background service runs on Unix only";

/// How many of the log's last lines a failure quotes.
const LOG_TAIL_LINES: usize = 20;
/// How many of the log's last bytes it reads for them.
const LOG_TAIL_BYTES: u64 = 4 * 1024;

/// What restarting the service does on this system.
pub trait ServiceControl: Clone + Send + 'static {
    /// Stops the daemon that holds `state_dir`'s lock, and returns once its process ended. A
    /// lock that no daemon holds counts as stopped. Another program may have started a daemon
    /// again by then: the update judges the daemon that answers next. It blocks.
    fn stop(&self, state_dir: &Path) -> Result<(), StopFailure>;

    /// Starts the service for `state_dir`, with its log in `log_folder`, and returns once it
    /// answers. `Err` says why it did not.
    async fn start(&self, state_dir: &Path, log_folder: &Path) -> Result<(), String>;

    /// Those of `agents`, by the app's ids, that may hold a scheduled job in the daemon that
    /// holds `state_dir`'s lock. It reads their raw logs: it blocks.
    fn job_holders(&self, state_dir: &Path, agents: &[AgentId]) -> Vec<AgentId>;
}

/// The agents of the plan's daemon that may hold a scheduled job, among `agents`. A read that
/// ended before it was done counts every one of them.
pub async fn read_job_holders(
    control: &impl ServiceControl,
    plan: &ServicePlan,
    agents: Vec<AgentId>,
) -> Vec<AgentId> {
    let reader = control.clone();
    let state_dir = plan.state_dir.clone();
    let read = tokio::task::spawn_blocking({
        let agents = agents.clone();
        move || reader.job_holders(&state_dir, &agents)
    });
    read.await.unwrap_or(agents)
}

/// What a restart that refuses busy agents checks before the stop (`BusyAgents::Refuse`).
pub struct BusyCheck<'state> {
    /// The resting agents, whose raw logs it reads.
    pub resting: Vec<AgentId>,
    /// Whether an agent runs something now. The read takes a while, and an agent may start a
    /// turn meanwhile, so the restart asks once the read ended.
    pub runs_something: Box<dyn FnOnce() -> bool + 'state>,
}

/// Stops the daemon, then starts the service with this app's program, and tells `steps` each step
/// as it starts. Once it returns `Ok`, the app connects to the new daemon.
///
/// With `check`, it first reads the resting agents' raw logs again, then restarts nothing when an
/// agent runs something or may hold a scheduled job, as the script refuses on a server.
pub async fn restart(
    control: &impl ServiceControl,
    plan: &ServicePlan,
    check: Option<BusyCheck<'_>>,
    mut steps: impl FnMut(UpdateStep),
) -> Result<(), RestartFailure> {
    if let Some(BusyCheck {
        resting,
        runs_something,
    }) = check
    {
        let holders = read_job_holders(control, plan, resting).await;
        if runs_something() {
            return Err(RestartFailure::Busy);
        }
        if !holders.is_empty() {
            return Err(RestartFailure::HoldsJobs(holders));
        }
    }
    steps(UpdateStep::Stop);
    let stopper = control.clone();
    let state_dir = plan.state_dir.clone();
    match tokio::task::spawn_blocking(move || stopper.stop(&state_dir)).await {
        Ok(Ok(())) => {}
        Ok(Err(failure)) => return Err(RestartFailure::NotStopped(failure)),
        Err(ended) => {
            return Err(RestartFailure::NotStopped(StopFailure {
                reason: format!("the stop ended before it was done: {ended}"),
                by_hand: None,
            }));
        }
    }
    steps(UpdateStep::Start);
    let log = plan.log_folder.join(SERVICE_LOG);
    let logged_before = std::fs::metadata(&log).map_or(0, |metadata| metadata.len());
    control
        .start(&plan.state_dir, &plan.log_folder)
        .await
        .map_err(|error| RestartFailure::NotStarted {
            error,
            log: log_tail(&log, logged_before),
        })
}

/// The last lines of `log` past `from`, where the service said why it stopped. The lines before
/// are other runs'.
fn log_tail(log: &Path, from: u64) -> Option<String> {
    let mut file = File::open(log).ok()?;
    let length = file.metadata().ok()?.len();
    let start = from.max(length.saturating_sub(LOG_TAIL_BYTES));
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    // A start inside this run's lines cuts the first one.
    let whole = if start > from {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    let lines: Vec<&str> = whole
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    let last = lines.get(lines.len().saturating_sub(LOG_TAIL_LINES)..)?;
    (!last.is_empty()).then(|| last.join("\n"))
}

/// This system's background service.
#[derive(Debug, Clone, Copy)]
pub struct SystemService;

impl ServiceControl for SystemService {
    fn stop(&self, state_dir: &Path) -> Result<(), StopFailure> {
        stop_lock_holder(state_dir)
    }

    async fn start(&self, state_dir: &Path, log_folder: &Path) -> Result<(), String> {
        start_service(state_dir, log_folder).await
    }

    fn job_holders(&self, state_dir: &Path, agents: &[AgentId]) -> Vec<AgentId> {
        job_holders(state_dir, agents)
    }
}

/// The rule of `contrib/update-daemon.sh`: Claude called `CronCreate` more often than
/// `CronDelete` in the agent's raw log since the daemon started. A lock the app cannot read
/// counts every call, and a log it cannot read counts as a job. No daemon holds no job.
#[cfg(unix)]
fn job_holders(state_dir: &Path, agents: &[AgentId]) -> Vec<AgentId> {
    use std::time::SystemTime;

    use crate::machine::MachineId;

    let since = match cockpit_daemon::lock_held_since(state_dir) {
        Ok(Some(since)) => since,
        Ok(None) => return Vec::new(),
        Err(_) => SystemTime::UNIX_EPOCH,
    };
    agents
        .iter()
        .copied()
        .filter(|agent| {
            let daemon_agent = MachineId::daemon_agent(*agent);
            cockpit_daemon::may_hold_scheduled_job(state_dir, daemon_agent, since).unwrap_or(true)
        })
        .collect()
}

#[cfg(not(unix))]
fn job_holders(_state_dir: &Path, _agents: &[AgentId]) -> Vec<AgentId> {
    Vec::new()
}

/// Stops the daemon as systemd stops the service, after checking that the lock names it.
#[cfg(unix)]
fn stop_lock_holder(state_dir: &Path) -> Result<(), StopFailure> {
    use cockpit_daemon::{LockHolderError, StopLockHolderError};

    use crate::state::daemon_update::HandStop;

    let error = match cockpit_daemon::stop_lock_holder(state_dir) {
        Ok(()) | Err(StopLockHolderError::NoDaemon) => return Ok(()),
        Err(error) => error,
    };
    let by_hand = match &error {
        StopLockHolderError::Signal { pid, .. }
        | StopLockHolderError::Wait { pid, .. }
        | StopLockHolderError::StillRunning { pid, .. } => Some(HandStop::Process(*pid)),
        StopLockHolderError::Holder(LockHolderError::Unnamed { .. }) => Some(HandStop::Unrecorded),
        StopLockHolderError::Unverified { .. } => Some(HandStop::Unverified),
        StopLockHolderError::NoDaemon
        | StopLockHolderError::ThisProcess
        | StopLockHolderError::Holder(LockHolderError::Read { .. }) => None,
    };
    Err(StopFailure {
        reason: crate::state::error_chain(&error),
        by_hand,
    })
}

#[cfg(not(unix))]
fn stop_lock_holder(_state_dir: &Path) -> Result<(), StopFailure> {
    Err(StopFailure {
        reason: UNIX_ONLY.to_owned(),
        by_hand: None,
    })
}

#[cfg(unix)]
async fn start_service(state_dir: &Path, log_folder: &Path) -> Result<(), String> {
    crate::service::ensure(state_dir, log_folder)
        .await
        .map(drop)
        .map_err(|error| crate::state::error_chain(&error))
}

#[cfg(not(unix))]
async fn start_service(_state_dir: &Path, _log_folder: &Path) -> Result<(), String> {
    Err(UNIX_ONLY.to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use cockpit_protocol::ProcessId;
    use tempfile::TempDir;

    use super::*;
    use crate::state::daemon_update::HandStop;

    /// A system whose stop and start give the answers the test sets, and that records the calls.
    #[derive(Clone)]
    struct Fake {
        stopped: Result<(), StopFailure>,
        started: Result<(), String>,
        /// What the start writes to the log first.
        logged: &'static str,
        /// The agents that may hold a scheduled job.
        holders: Vec<AgentId>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                stopped: Ok(()),
                started: Ok(()),
                logged: "",
                holders: Vec::new(),
                calls: Arc::default(),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ServiceControl for Fake {
        fn stop(&self, state_dir: &Path) -> Result<(), StopFailure> {
            let call = format!("stop {}", state_dir.display());
            self.calls.lock().unwrap().push(call);
            self.stopped.clone()
        }

        async fn start(&self, state_dir: &Path, log_folder: &Path) -> Result<(), String> {
            let call = format!("start {} {}", state_dir.display(), log_folder.display());
            self.calls.lock().unwrap().push(call);
            if !self.logged.is_empty() {
                use std::io::Write;

                let mut log = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_folder.join(SERVICE_LOG))
                    .unwrap();
                log.write_all(self.logged.as_bytes()).unwrap();
            }
            self.started.clone()
        }

        fn job_holders(&self, state_dir: &Path, agents: &[AgentId]) -> Vec<AgentId> {
            let call = format!("read the jobs of {agents:?} in {}", state_dir.display());
            self.calls.lock().unwrap().push(call);
            agents
                .iter()
                .copied()
                .filter(|agent| self.holders.contains(agent))
                .collect()
        }
    }

    fn plan(root: &Path) -> ServicePlan {
        ServicePlan {
            state_dir: root.join("state"),
            log_folder: root.join("app"),
        }
    }

    async fn restarted(
        fake: &Fake,
        plan: &ServicePlan,
    ) -> (Result<(), RestartFailure>, Vec<UpdateStep>) {
        restarted_keeping(fake, plan, None).await
    }

    /// A restart with `BusyAgents::Refuse` when `resting` is given, while no agent runs anything.
    async fn restarted_keeping(
        fake: &Fake,
        plan: &ServicePlan,
        resting: Option<Vec<AgentId>>,
    ) -> (Result<(), RestartFailure>, Vec<UpdateStep>) {
        let check = resting.map(|resting| BusyCheck {
            resting,
            runs_something: Box::new(|| false),
        });
        restarted_checking(fake, plan, check).await
    }

    async fn restarted_checking(
        fake: &Fake,
        plan: &ServicePlan,
        check: Option<BusyCheck<'_>>,
    ) -> (Result<(), RestartFailure>, Vec<UpdateStep>) {
        let mut steps = Vec::new();
        let result = restart(fake, plan, check, |step| steps.push(step)).await;
        (result, steps)
    }

    /// The user sends a prompt while the restart reads the logs.
    #[tokio::test]
    async fn an_agent_that_runs_something_once_the_read_ends_stops_the_restart_before_the_stop() {
        let fake = Fake::new();
        let read_first = fake.clone();
        let check = BusyCheck {
            resting: vec![AgentId::new(1)],
            runs_something: Box::new(move || {
                assert_eq!(read_first.calls().len(), 1, "asked once the read ended");
                true
            }),
        };

        let (result, steps) =
            restarted_checking(&fake, &plan(Path::new("/made-up")), check.into()).await;

        assert_eq!(result, Err(RestartFailure::Busy));
        assert_eq!(steps, [], "nothing stopped");
        assert_eq!(fake.calls().len(), 1, "the read alone: {:?}", fake.calls());
    }

    #[tokio::test]
    async fn an_agent_that_may_hold_a_scheduled_job_stops_the_restart_before_the_stop() {
        let fake = Fake {
            holders: vec![AgentId::new(2)],
            ..Fake::new()
        };
        let plan = plan(Path::new("/made-up"));
        let resting = vec![AgentId::new(1), AgentId::new(2)];

        let (result, steps) = restarted_keeping(&fake, &plan, Some(resting)).await;

        assert_eq!(
            result,
            Err(RestartFailure::HoldsJobs(vec![AgentId::new(2)]))
        );
        assert_eq!(steps, [], "nothing stopped");
        assert_eq!(fake.calls().len(), 1, "the read alone: {:?}", fake.calls());
    }

    #[tokio::test]
    async fn resting_agents_without_a_job_let_the_restart_go_on() {
        let fake = Fake::new();
        let plan = plan(Path::new("/made-up"));

        let (result, steps) = restarted_keeping(&fake, &plan, Some(vec![AgentId::new(1)])).await;

        assert_eq!(result, Ok(()));
        assert_eq!(steps, [UpdateStep::Stop, UpdateStep::Start]);
        assert_eq!(
            fake.calls().first().map(String::as_str),
            Some("read the jobs of [AgentId(1)] in /made-up/state")
        );
    }

    #[tokio::test]
    async fn update_now_reads_no_job() {
        let fake = Fake {
            holders: vec![AgentId::new(1)],
            ..Fake::new()
        };

        let (result, _) = restarted(&fake, &plan(Path::new("/made-up"))).await;

        assert_eq!(result, Ok(()));
        assert_eq!(fake.calls().len(), 2, "the stop and the start");
    }

    /// The state folder of a daemon this test plays: it holds the lock, and Claude called
    /// `tools` for agent 3, at `at` milliseconds after the epoch.
    #[cfg(unix)]
    fn daemon_with_calls(tools: &[&str], at: u128) -> (TempDir, File) {
        let temp = folder_with_calls(tools, at);
        let lock = File::open(temp.path().join("serve.lock")).unwrap();
        lock.try_lock().unwrap();
        (temp, lock)
    }

    /// The state folder of `daemon_with_calls`, whose lock no daemon holds. A lock the test took
    /// and let go may stay held a moment: a process another test forks shares it until it runs
    /// its program.
    #[cfg(unix)]
    fn folder_with_calls(tools: &[&str], at: u128) -> TempDir {
        use std::io::Write;

        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("raw")).unwrap();
        File::create(temp.path().join("serve.lock")).unwrap();
        let mut log = File::create(temp.path().join("raw/3.jsonl")).unwrap();
        for tool in tools {
            let claude_line = format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","id":"toolu_made_up","name":"{tool}","input":{{}}}}]}}}}"#
            );
            let line = format!(
                "{{\"at\":{at},\"direction\":\"from_claude\",\"line\":{}}}\n",
                serde_json::to_string(&claude_line).unwrap()
            );
            log.write_all(line.as_bytes()).unwrap();
        }
        temp
    }

    #[cfg(unix)]
    fn now_millis() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    }

    #[cfg(unix)]
    #[test]
    fn an_agent_holds_the_jobs_it_created_and_did_not_delete_since_the_daemon_started() {
        let agents = [AgentId::new(3)];
        let (created, _lock) = daemon_with_calls(&["CronCreate"], now_millis() + 1000);
        let (deleted, _other_lock) =
            daemon_with_calls(&["CronCreate", "CronDelete"], now_millis() + 1000);
        let (before_the_start, _old_lock) = daemon_with_calls(&["CronCreate"], 1000);

        assert_eq!(job_holders(created.path(), &agents), agents);
        assert_eq!(job_holders(deleted.path(), &agents), []);
        assert_eq!(job_holders(before_the_start.path(), &agents), []);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_no_daemon_holds_has_no_job() {
        let temp = folder_with_calls(&["CronCreate"], now_millis() + 1000);

        assert_eq!(job_holders(temp.path(), &[AgentId::new(3)]), []);
    }

    #[tokio::test]
    async fn the_daemon_stops_before_the_service_starts() {
        let fake = Fake::new();
        let plan = plan(Path::new("/made-up"));

        let (result, steps) = restarted(&fake, &plan).await;

        assert_eq!(result, Ok(()));
        assert_eq!(steps, [UpdateStep::Stop, UpdateStep::Start]);
        assert_eq!(
            fake.calls(),
            [
                format!("stop {}", plan.state_dir.display()),
                format!(
                    "start {} {}",
                    plan.state_dir.display(),
                    plan.log_folder.display()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn a_daemon_that_did_not_stop_starts_nothing() {
        let failure = StopFailure {
            reason: "the daemon, process 4242, did not stop within 10 seconds".to_owned(),
            by_hand: Some(HandStop::Process(ProcessId::new(4242))),
        };
        let fake = Fake {
            stopped: Err(failure.clone()),
            ..Fake::new()
        };

        let (result, steps) = restarted(&fake, &plan(Path::new("/made-up"))).await;

        assert_eq!(result, Err(RestartFailure::NotStopped(failure)));
        assert_eq!(steps, [UpdateStep::Stop]);
        assert_eq!(fake.calls().len(), 1, "no start");
    }

    #[tokio::test]
    async fn a_service_that_did_not_start_quotes_its_log_from_this_start() {
        let temp = tempfile::tempdir().unwrap();
        let plan = plan(temp.path());
        std::fs::create_dir_all(&plan.log_folder).unwrap();
        std::fs::write(
            plan.log_folder.join(SERVICE_LOG),
            "agents-cockpit: an earlier run stopped\n",
        )
        .unwrap();
        let fake = Fake {
            started: Err("the service stopped as it started (exit status: 1)".to_owned()),
            logged: "INFO starting\nagents-cockpit: another daemon already runs\n",
            ..Fake::new()
        };

        let (result, _) = restarted(&fake, &plan).await;

        assert_eq!(
            result,
            Err(RestartFailure::NotStarted {
                error: "the service stopped as it started (exit status: 1)".to_owned(),
                log: Some("INFO starting\nagents-cockpit: another daemon already runs".to_owned()),
            })
        );
    }

    #[tokio::test]
    async fn a_service_with_no_log_says_its_error_alone() {
        let temp = tempfile::tempdir().unwrap();
        let fake = Fake {
            started: Err("cannot start /made-up/agents-cockpit".to_owned()),
            ..Fake::new()
        };

        let (result, _) = restarted(&fake, &plan(temp.path())).await;

        assert_eq!(
            result,
            Err(RestartFailure::NotStarted {
                error: "cannot start /made-up/agents-cockpit".to_owned(),
                log: None,
            })
        );
    }

    fn log_with(text: &str) -> (TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join(SERVICE_LOG);
        std::fs::write(&log, text).unwrap();
        (temp, log)
    }

    #[test]
    fn the_tail_keeps_the_last_lines() {
        let text: String = (1..=30).map(|number| format!("line {number}\n")).collect();
        let (_temp, log) = log_with(&text);

        let tail = log_tail(&log, 0).unwrap();

        assert_eq!(tail.lines().count(), LOG_TAIL_LINES);
        assert_eq!(tail.lines().next(), Some("line 11"));
        assert_eq!(tail.lines().last(), Some("line 30"));
    }

    #[test]
    fn a_tail_cut_by_its_size_starts_on_a_whole_line() {
        let long_line = "x".repeat(5000);
        let (_temp, log) = log_with(&format!("{long_line}\nthe last line\n"));

        assert_eq!(log_tail(&log, 0).as_deref(), Some("the last line"));
    }

    #[test]
    fn a_log_with_nothing_new_has_no_tail() {
        let (_temp, log) = log_with("an earlier run\n\n");

        assert_eq!(log_tail(&log, 0).as_deref(), Some("an earlier run"));
        assert_eq!(log_tail(&log, 15), None);
        assert_eq!(log_tail(&log.with_file_name("missing.log"), 0), None);
    }
}
