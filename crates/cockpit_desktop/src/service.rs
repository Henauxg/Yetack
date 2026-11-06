//! Starts the background service, `agents-cockpit serve`, as a process that outlives the app.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::time::Instant;

/// The program that runs the service, found beside the app's own program or on `PATH`.
const PROGRAM: &str = "agents-cockpit";
const PATH_VAR: &str = "PATH";
/// What the service writes to stderr, in the app's state folder.
const LOG_FILE: &str = "serve.log";
const LOG_MODE: u32 = 0o600;
/// The service reads its state folder back before it listens.
const START_WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(50);
/// How much of the log's end an error may quote.
const LOG_TAIL_BYTES: usize = 4 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("cannot find {PROGRAM} in the app's folder, {}, or on {PATH_VAR}", folder.display())]
    NotFound { folder: PathBuf },
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

/// Starts the service for `state_dir`, unless a daemon already answers on its socket, and waits
/// until the socket answers. The service's stderr goes to a log in `log_folder`.
pub async fn ensure(state_dir: &Path, log_folder: &Path) -> Result<PathBuf, ServiceError> {
    let socket = cockpit_daemon::socket_path(state_dir);
    if answers(&socket).await {
        return Ok(socket);
    }
    let own_program = std::env::current_exe().ok();
    let path = std::env::var_os(PATH_VAR);
    let program = find_program(own_program.as_deref(), path.as_deref())?;
    let log = log_folder.join(LOG_FILE);
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

/// `agents-cockpit` beside `own_program`, else in a folder of `path`. A relative folder of `path`
/// is skipped: it names a folder under the app's working folder, which the service does not run
/// in.
fn find_program(own_program: Option<&Path>, path: Option<&OsStr>) -> Result<PathBuf, ServiceError> {
    let beside = own_program.and_then(Path::parent);
    let folders = beside.map(Path::to_owned).into_iter().chain(
        path.into_iter()
            .flat_map(std::env::split_paths)
            .filter(|folder| folder.is_absolute()),
    );
    for folder in folders {
        let program = folder.join(PROGRAM);
        if is_executable(&program) {
            return Ok(program);
        }
    }
    Err(ServiceError::NotFound {
        folder: beside.map(Path::to_owned).unwrap_or_default(),
    })
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
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
        .arg(format!("--{}", cockpit_daemon::STATE_DIR_FLAG))
        .arg(state_dir)
        .arg(cockpit_daemon::SERVE_SUBCOMMAND)
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

/// The last line the service wrote to its log past `from`, where `agents-cockpit` says why it
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
    use std::ffi::OsString;
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn program_in(folder: &Path, mode: u32) -> PathBuf {
        std::fs::create_dir_all(folder).unwrap();
        let program = folder.join(PROGRAM);
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&program, Permissions::from_mode(mode)).unwrap();
        program
    }

    #[test]
    fn the_program_beside_the_app_comes_before_path() {
        let temp = tempfile::tempdir().unwrap();
        let app_folder = temp.path().join("app");
        let beside = program_in(&app_folder, 0o755);
        let on_path = program_in(&temp.path().join("bin"), 0o755);
        let path = OsString::from(temp.path().join("bin"));

        let own = app_folder.join("agents-cockpit-desktop");
        let other = temp.path().join("elsewhere/agents-cockpit-desktop");

        assert_eq!(find_program(Some(&own), Some(&path)).unwrap(), beside);
        assert_eq!(find_program(Some(&other), Some(&path)).unwrap(), on_path);
    }

    #[test]
    fn a_missing_program_names_where_it_was_looked_for() {
        let temp = tempfile::tempdir().unwrap();
        let app_folder = temp.path().join("app");
        // A file that cannot run does not count.
        program_in(&temp.path().join("bin"), 0o644);
        let path =
            std::env::join_paths([temp.path().join("bin"), temp.path().join("none")]).unwrap();

        let error = find_program(
            Some(&app_folder.join("agents-cockpit-desktop")),
            Some(&path),
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "cannot find agents-cockpit in the app's folder, {}, or on PATH",
                app_folder.display()
            )
        );
    }

    #[test]
    fn a_relative_folder_on_path_does_not_count() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        program_in(&bin, 0o755);
        // The same folder, named from the working folder.
        let working = std::env::current_dir().unwrap();
        let up = "../".repeat(working.components().count().saturating_sub(1));
        let relative = PathBuf::from(up).join(bin.strip_prefix("/").unwrap());
        assert!(relative.join(PROGRAM).exists());
        let path = std::env::join_paths([OsString::new(), relative.into_os_string()]).unwrap();

        let found = find_program(None, Some(&path));

        assert!(
            matches!(found, Err(ServiceError::NotFound { .. })),
            "{found:?}"
        );
    }

    #[tokio::test]
    async fn a_service_that_stops_at_once_says_why() {
        let temp = tempfile::tempdir().unwrap();
        let program = program_in(&temp.path().join("bin"), 0o755);
        std::fs::write(
            &program,
            "#!/bin/sh\necho \"agents-cockpit: another daemon already runs on $2/serve.lock\" >&2\nexit 1\n",
        )
        .unwrap();
        let state = temp.path().join("state");
        let log = temp.path().join("app").join(LOG_FILE);

        let mut child = spawn(&program, &state, &log).unwrap();
        let status = child.wait().await.unwrap();

        assert!(!status.success());
        assert_eq!(
            last_line(&log, 0),
            Some(format!(
                "agents-cockpit: another daemon already runs on {}/serve.lock",
                state.display()
            ))
        );
    }

    #[tokio::test]
    async fn a_silent_service_does_not_quote_an_earlier_run() {
        let temp = tempfile::tempdir().unwrap();
        let program = program_in(&temp.path().join("bin"), 0o755);
        std::fs::write(&program, "#!/bin/sh\nexit 1\n").unwrap();
        let log = temp.path().join("app").join(LOG_FILE);
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, "agents-cockpit: an earlier run stopped\n").unwrap();
        let logged_before = std::fs::metadata(&log).unwrap().len();

        let mut child = spawn(&program, &temp.path().join("state"), &log).unwrap();
        child.wait().await.unwrap();

        assert_eq!(
            last_line(&log, 0).as_deref(),
            Some("agents-cockpit: an earlier run stopped")
        );
        assert_eq!(last_line(&log, logged_before), None);
    }
}
