//! Updates this computer's daemon when it is the background service: stops the daemon that holds
//! the state folder's lock, then starts the service with this app's own program. The order lives
//! here, on every platform. The system's own calls sit behind `ServiceControl`, which the tests
//! replace.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

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
    /// Stops the daemon that holds `state_dir`'s lock, and returns once it let the lock go. A
    /// lock that no daemon holds counts as stopped. It blocks.
    fn stop(&self, state_dir: &Path) -> Result<(), StopFailure>;

    /// Starts the service for `state_dir`, with its log in `log_folder`, and returns once it
    /// answers. `Err` says why it did not.
    async fn start(&self, state_dir: &Path, log_folder: &Path) -> Result<(), String>;
}

/// Stops the daemon, then starts the service with this app's program, and tells `steps` each step
/// as it starts. Once it returns `Ok`, the app connects to the new daemon.
pub async fn restart(
    control: &impl ServiceControl,
    plan: &ServicePlan,
    mut steps: impl FnMut(UpdateStep),
) -> Result<(), RestartFailure> {
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
        StopLockHolderError::Signal { pid, .. } | StopLockHolderError::StillRunning { pid, .. } => {
            Some(HandStop::Process(*pid))
        }
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
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use cockpit_protocol::ProcessId;

    use super::*;
    use crate::state::daemon_update::HandStop;

    /// A system whose stop and start give the answers the test sets, and that records the calls.
    #[derive(Clone)]
    struct Fake {
        stopped: Result<(), StopFailure>,
        started: Result<(), String>,
        /// What the start writes to the log first.
        logged: &'static str,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                stopped: Ok(()),
                started: Ok(()),
                logged: "",
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

                let mut log = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(log_folder.join(SERVICE_LOG))
                    .unwrap();
                log.write_all(self.logged.as_bytes()).unwrap();
            }
            self.started.clone()
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
        let mut steps = Vec::new();
        let result = restart(fake, plan, |step| steps.push(step)).await;
        (result, steps)
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

    fn log_with(text: &str) -> (tempfile::TempDir, PathBuf) {
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
