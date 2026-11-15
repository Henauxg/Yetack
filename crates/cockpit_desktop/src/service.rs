//! The background service: this program, run again as a daemon that outlives the app.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus, Stdio};
use std::time::Duration;

use anyhow::Context;
use cockpit_daemon::{DaemonConfig, SERVE_SUBCOMMAND, STATE_DIR_FLAG};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::runtime::Runtime;
use tokio::time::Instant;
use tracing_subscriber::EnvFilter;

use crate::service_update::SERVICE_LOG;

/// The service's log level, unless `RUST_LOG` says otherwise.
const LOG_LEVEL: &str = "info";
const LOG_MODE: u32 = 0o600;
/// The service reads its state folder back before it listens.
const START_WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(50);
/// How much of the log's end an error may quote.
const LOG_TAIL_BYTES: usize = 4 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("cannot tell where this program is")]
    NoProgram(#[source] io::Error),
    #[error("cannot open the service's log {}", path.display())]
    Log {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot start {}", program.display())]
    Spawn {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the service stopped as it started ({status}): {said}")]
    Exited { status: ExitStatus, said: String },
    #[error("the service did not open {} within {} seconds; its log is {}", socket.display(), START_WAIT.as_secs(), log.display())]
    NoSocket { socket: PathBuf, log: PathBuf },
}

/// The state folder when this run of the program is to be the service, from the arguments `spawn`
/// gives it.
pub fn requested(arguments: &[OsString]) -> Option<PathBuf> {
    let [flag, state_dir, subcommand] = arguments else {
        return None;
    };
    let is_service = flag.to_str() == Some(format!("--{STATE_DIR_FLAG}").as_str())
        && subcommand.to_str() == Some(SERVE_SUBCOMMAND);
    is_service.then(|| PathBuf::from(state_dir))
}

/// Runs the service until SIGTERM or Ctrl+C. Its last line says why it stopped.
pub fn run(state_dir: PathBuf) -> ExitCode {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(LOG_LEVEL));
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(filter)
        .init();
    match serve(state_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("yetack: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn serve(state_dir: PathBuf) -> anyhow::Result<()> {
    let config = DaemonConfig::from_env(Some(state_dir)).context("HOME is not set")?;
    let runtime = Runtime::new().context("cannot start the runtime")?;
    runtime.block_on(async {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        let shutdown = async move {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = interrupt.recv() => {}
            }
        };
        cockpit_daemon::serve(config, shutdown).await?;
        Ok(())
    })
}

/// Starts the service for `state_dir`, unless a daemon already answers on its socket, and waits
/// until the socket answers. The service's stderr goes to a log in `log_folder`.
pub async fn ensure(state_dir: &Path, log_folder: &Path) -> Result<PathBuf, ServiceError> {
    let socket = cockpit_daemon::socket_path(state_dir);
    if answers(&socket).await {
        return Ok(socket);
    }
    let program = std::env::current_exe().map_err(ServiceError::NoProgram)?;
    let log = log_folder.join(SERVICE_LOG);
    let logged_before = std::fs::metadata(&log).map_or(0, |metadata| metadata.len());
    let mut child = spawn(&program, state_dir, &log)?;
    tracing::info!(program = %program.display(), "started the background service");
    let deadline = Instant::now() + START_WAIT;
    loop {
        if answers(&socket).await {
            // Reaps the service if it ends while the app runs.
            tokio::spawn(async move { child.wait().await });
            return Ok(socket);
        }
        if let Ok(Some(status)) = child.try_wait() {
            let said =
                last_line(&log, logged_before).unwrap_or_else(|| format!("see {}", log.display()));
            return Err(ServiceError::Exited { status, said });
        }
        if Instant::now() >= deadline {
            return Err(ServiceError::NoSocket { socket, log });
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The service leads a process group of its own, so the terminal's Ctrl+C that stops the app
/// does not reach it.
fn spawn(program: &Path, state_dir: &Path, log: &Path) -> Result<Child, ServiceError> {
    use std::os::unix::fs::OpenOptionsExt;

    let log_error = |source| ServiceError::Log {
        path: log.to_owned(),
        source,
    };
    if let Some(folder) = log.parent() {
        std::fs::create_dir_all(folder).map_err(log_error)?;
    }
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(LOG_MODE)
        .open(log)
        .map_err(log_error)?;
    Command::new(program)
        .arg(format!("--{STATE_DIR_FLAG}"))
        .arg(state_dir)
        .arg(SERVE_SUBCOMMAND)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .process_group(0)
        .spawn()
        .map_err(|source| ServiceError::Spawn {
            program: program.to_owned(),
            source,
        })
}

pub async fn answers(socket: &Path) -> bool {
    UnixStream::connect(socket).await.is_ok()
}

/// The last line the service wrote to its log past `from`, where `yetack` says why it
/// stopped. The earlier lines are other runs'.
fn last_line(log: &Path, from: u64) -> Option<String> {
    let bytes = std::fs::read(log).ok()?;
    let from = usize::try_from(from).unwrap_or(usize::MAX);
    let start = bytes.len().saturating_sub(LOG_TAIL_BYTES).max(from);
    let tail = bytes.get(start..)?;
    let text = String::from_utf8_lossy(tail);
    let line = text.lines().rev().find(|line| !line.trim().is_empty())?;
    Some(line.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Writes `text` as an executable script in `folder`, and returns once it runs.
    ///
    /// A process another test forks while the script is written keeps it open for writing until
    /// it runs its own program, and running the script fails with "Text file busy" until then.
    /// Once the script ran, no process can hold it open for writing again.
    fn script_in(folder: &Path, text: &str) -> PathBuf {
        std::fs::create_dir_all(folder).unwrap();
        let program = folder.join("yetack");
        std::fs::write(&program, text).unwrap();
        std::fs::set_permissions(&program, Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            let run = std::process::Command::new(&program)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            match run {
                Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(_) | Err(_) => break,
            }
        }
        program
    }

    #[test]
    fn only_the_arguments_the_app_gives_make_this_run_the_service() {
        let arguments = |list: &[&str]| list.iter().map(OsString::from).collect::<Vec<_>>();

        assert_eq!(
            requested(&arguments(&["--state-dir", "/state", "serve"])),
            Some(PathBuf::from("/state"))
        );
        assert_eq!(requested(&arguments(&[])), None);
        assert_eq!(requested(&arguments(&["--demo"])), None);
        assert_eq!(
            requested(&arguments(&["--state-dir", "/state", "list"])),
            None
        );
        assert_eq!(requested(&arguments(&["--other", "/state", "serve"])), None);
    }

    #[tokio::test]
    async fn a_service_that_stops_at_once_says_why() {
        let temp = tempfile::tempdir().unwrap();
        let program = script_in(
            &temp.path().join("bin"),
            "#!/bin/sh\necho \"yetack: another daemon already runs on $2/serve.lock\" >&2\nexit 1\n",
        );
        let state = temp.path().join("state");
        let log = temp.path().join("app").join(SERVICE_LOG);

        let mut child = spawn(&program, &state, &log).unwrap();
        let status = child.wait().await.unwrap();

        assert!(!status.success());
        assert_eq!(
            last_line(&log, 0),
            Some(format!(
                "yetack: another daemon already runs on {}/serve.lock",
                state.display()
            ))
        );
    }

    #[tokio::test]
    async fn a_silent_service_does_not_quote_an_earlier_run() {
        let temp = tempfile::tempdir().unwrap();
        let program = script_in(&temp.path().join("bin"), "#!/bin/sh\nexit 1\n");
        let log = temp.path().join("app").join(SERVICE_LOG);
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, "yetack: an earlier run stopped\n").unwrap();
        let logged_before = std::fs::metadata(&log).unwrap().len();

        let mut child = spawn(&program, &temp.path().join("state"), &log).unwrap();
        child.wait().await.unwrap();

        assert_eq!(
            last_line(&log, 0).as_deref(),
            Some("yetack: an earlier run stopped")
        );
        assert_eq!(last_line(&log, logged_before), None);
    }
}
