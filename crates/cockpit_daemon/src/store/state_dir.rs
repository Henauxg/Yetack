use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::{env, io};

use cockpit_protocol::AgentId;

use crate::daemon_config::HOME_ENV;
use crate::lock_holder::LockHolder;
use crate::store::store_error::StoreError;

const STATE_FOLDER_NAME: &str = "agents-cockpit";
#[cfg(unix)]
const STATE_HOME_ENV: &str = "XDG_STATE_HOME";
/// Where the state goes under `$HOME` when `XDG_STATE_HOME` is unset.
#[cfg(unix)]
const HOME_STATE: &str = ".local/state";
#[cfg(windows)]
const STATE_HOME_ENV: &str = "LOCALAPPDATA";
/// Where the state goes under `%USERPROFILE%` when `LOCALAPPDATA` is unset.
#[cfg(windows)]
const HOME_STATE: &str = r"AppData\Local";

const SOCKET_FILE: &str = "agents-cockpit.sock";
const LOCK_FILE: &str = "serve.lock";
const AGENTS_FILE: &str = "agents.json";
const SETTINGS_FILE: &str = "settings.json";
const EVENTS_FOLDER: &str = "events";
const RAW_FOLDER: &str = "raw";
const DETAILS_FOLDER: &str = "details";
const RAW_LOG_EXTENSION: &str = "jsonl";

/// Only the owner may enter the folders.
#[cfg(unix)]
const FOLDER_MODE: u32 = 0o700;
/// Only the owner may read or write the files.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// `$XDG_STATE_HOME/agents-cockpit`, or `~/.local/state/agents-cockpit`; on Windows,
/// `%LOCALAPPDATA%\agents-cockpit`. `None` without a home.
#[must_use]
pub fn default_state_dir() -> Option<PathBuf> {
    // The XDG specification ignores a relative path.
    let xdg = env::var_os(STATE_HOME_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let base = match xdg {
        Some(base) => base,
        None => PathBuf::from(env::var_os(HOME_ENV)?).join(HOME_STATE),
    };
    Some(base.join(STATE_FOLDER_NAME))
}

/// The daemon's socket in the state folder `state_dir`.
#[must_use]
pub fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SOCKET_FILE)
}

/// `serve.lock` in the state folder `state_dir`.
pub(crate) fn lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join(LOCK_FILE)
}

/// The state folder, created with mode 700.
#[derive(Debug, Clone)]
pub(crate) struct StateDir {
    root: PathBuf,
}

/// Held while the daemon runs. The operating system releases it when the process ends.
#[derive(Debug)]
pub(crate) struct ServeLock {
    file: File,
}

impl Drop for ServeLock {
    fn drop(&mut self) {
        // A later daemon older than the record writes none: the file must not name this process.
        if let Err(error) = self.file.set_len(0) {
            tracing::warn!(%error, file = LOCK_FILE, "cannot empty the lock's file");
        }
    }
}

impl StateDir {
    /// Creates the folder and its subfolders when missing, and sets their mode to 700.
    pub(crate) fn create(root: &Path) -> Result<Self, StoreError> {
        let state = Self {
            root: root.to_owned(),
        };
        for folder in [
            state.root.clone(),
            state.events(),
            state.raw(),
            state.details(),
        ] {
            create_private_folder(&folder)?;
        }
        Ok(state)
    }

    /// Takes `serve.lock` and writes this process into it, or fails with `AlreadyRunning`.
    pub(crate) fn lock(&self) -> Result<ServeLock, StoreError> {
        let path = lock_path(&self.root);
        let file = owner_only()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|source| StoreError::Lock {
                path: path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => {
                // Only the app reads the record, to stop this daemon: the daemon runs without it.
                if let Err(error) = record_holder(&file) {
                    tracing::warn!(%error, file = LOCK_FILE, "cannot record this daemon");
                }
                Ok(ServeLock { file })
            }
            Err(TryLockError::WouldBlock) => Err(StoreError::AlreadyRunning { path }),
            Err(TryLockError::Error(source)) => Err(StoreError::Lock { path, source }),
        }
    }

    #[cfg(unix)]
    pub(crate) fn socket(&self) -> PathBuf {
        socket_path(&self.root)
    }

    pub(crate) fn agents_file(&self) -> PathBuf {
        self.root.join(AGENTS_FILE)
    }

    pub(crate) fn settings_file(&self) -> PathBuf {
        self.root.join(SETTINGS_FILE)
    }

    pub(crate) fn events(&self) -> PathBuf {
        self.root.join(EVENTS_FOLDER)
    }

    pub(crate) fn raw(&self) -> PathBuf {
        self.root.join(RAW_FOLDER)
    }

    /// `details/`: each agent's latest details, which the event logs leave out.
    pub(crate) fn details(&self) -> PathBuf {
        self.root.join(DETAILS_FOLDER)
    }

    /// `raw/<agent-id>.jsonl`.
    pub(crate) fn raw_log(&self, agent: AgentId) -> PathBuf {
        raw_log_path(&self.root, agent)
    }
}

/// `raw/<agent-id>.jsonl` in the state folder `state_dir`.
pub(crate) fn raw_log_path(state_dir: &Path, agent: AgentId) -> PathBuf {
    state_dir
        .join(RAW_FOLDER)
        .join(format!("{}.{RAW_LOG_EXTENSION}", agent.get()))
}

/// Writes this process into the lock's file, in place of the record a killed daemon left.
fn record_holder(mut file: &File) -> io::Result<()> {
    use std::io::Write;

    let line = LockHolder::this_process().encode()?;
    file.set_len(0)?;
    file.write_all(&line)
}

/// Options that create a file with mode 600.
///
/// On Windows, a new file or folder takes the permissions of the folder it is in. Under
/// `%LOCALAPPDATA%`, those admit only the user.
pub(crate) fn owner_only() -> OpenOptions {
    #[cfg_attr(windows, expect(unused_mut, reason = "only Unix sets a mode"))]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(FILE_MODE);
    }
    options
}

#[cfg(unix)]
fn create_private_folder(path: &Path) -> Result<(), StoreError> {
    use std::fs::{DirBuilder, Permissions};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let error = |source| StoreError::CreateFolder {
        path: path.to_owned(),
        source,
    };
    DirBuilder::new()
        .recursive(true)
        .mode(FOLDER_MODE)
        .create(path)
        .map_err(error)?;
    // An existing folder keeps its mode otherwise.
    std::fs::set_permissions(path, Permissions::from_mode(FOLDER_MODE)).map_err(error)
}

#[cfg(windows)]
fn create_private_folder(path: &Path) -> Result<(), StoreError> {
    std::fs::create_dir_all(path).map_err(|source| StoreError::CreateFolder {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn creates_private_folders() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");

        let state = StateDir::create(&root).unwrap();

        for folder in [root.clone(), state.events(), state.raw()] {
            let mode = std::fs::metadata(&folder).unwrap().mode() & 0o777;
            assert_eq!(mode, FOLDER_MODE, "{}", folder.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn tightens_an_existing_folder() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, Permissions::from_mode(0o755)).unwrap();

        StateDir::create(&root).unwrap();

        let mode = std::fs::metadata(&root).unwrap().mode() & 0o777;
        assert_eq!(mode, FOLDER_MODE);
    }

    /// On Windows, the lock keeps the test from reading the file.
    #[cfg(unix)]
    #[test]
    fn a_taken_lock_names_this_process() {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();
        let left = "{\"pid\":4242,\"started\":987654,\"boot\":\"the boot of a killed daemon\"}\n";
        std::fs::write(lock_path(temp.path()), left).unwrap();

        let _lock = state.lock().unwrap();

        let record = std::fs::read(lock_path(temp.path())).unwrap();
        assert_eq!(record, LockHolder::this_process().encode().unwrap());
    }

    #[test]
    fn a_released_lock_names_no_process() {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();

        drop(state.lock().unwrap());

        assert_eq!(std::fs::read(lock_path(temp.path())).unwrap(), b"");
    }

    #[test]
    fn a_second_lock_is_refused_until_the_first_is_released() {
        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();

        let first = state.lock().unwrap();
        let second = state.lock();
        assert!(
            matches!(second, Err(StoreError::AlreadyRunning { .. })),
            "{second:?}"
        );
        drop(first);
        // A child process another test forks shares the lock until it starts its program.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut again = state.lock();
        while again.is_err() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
            again = state.lock();
        }
        again.unwrap();
    }

    /// Whatever the umask of the process that runs the test.
    #[cfg(unix)]
    #[test]
    fn every_file_is_for_the_owner_only() {
        use cockpit_protocol::{Event, EventRecord, Frame, Seq, ServerMessage, Settings};

        use crate::store::{AgentsFile, DaemonSettings, EventLogs, LogName, RawLog};

        let temp = tempfile::tempdir().unwrap();
        let state = StateDir::create(temp.path()).unwrap();
        let _lock = state.lock().unwrap();
        AgentsFile::default().save(&state.agents_file()).unwrap();
        DaemonSettings::default()
            .save(&state.settings_file())
            .unwrap();
        let mut logs = EventLogs::new(state.events(), state.details());
        let event = ServerMessage::Event(EventRecord {
            seq: Seq::new(1),
            event: Event::SettingsChanged(Settings::default()),
        });
        logs.append(LogName::Daemon, &Frame::encode(&event).unwrap())
            .unwrap();
        let raw = state.raw_log(AgentId::new(1));
        RawLog::open(&raw).unwrap();

        let files = [
            temp.path().join(LOCK_FILE),
            state.agents_file(),
            state.settings_file(),
            logs.path(LogName::Daemon),
            raw,
        ];
        for file in files {
            let mode = std::fs::metadata(&file).unwrap().mode() & 0o777;
            assert_eq!(mode, FILE_MODE, "{}", file.display());
        }
    }
}
