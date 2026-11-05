use std::env;
use std::fs::{DirBuilder, File, OpenOptions, Permissions, TryLockError};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use cockpit_protocol::AgentId;

use crate::store::error::StoreError;

const STATE_FOLDER_NAME: &str = "agents-cockpit";
const XDG_STATE_HOME: &str = "XDG_STATE_HOME";
const HOME: &str = "HOME";
/// Where the state goes under `$HOME` when `XDG_STATE_HOME` is unset.
const HOME_STATE: &str = ".local/state";

const SOCKET_FILE: &str = "agents-cockpit.sock";
const LOCK_FILE: &str = "serve.lock";
const AGENTS_FILE: &str = "agents.json";
const SETTINGS_FILE: &str = "settings.json";
const EVENTS_FOLDER: &str = "events";
const RAW_FOLDER: &str = "raw";
const RAW_LOG_EXTENSION: &str = "jsonl";

/// Only the owner may enter the folders.
const FOLDER_MODE: u32 = 0o700;
/// Only the owner may read or write the files.
pub(crate) const FILE_MODE: u32 = 0o600;

/// `$XDG_STATE_HOME/agents-cockpit`, or `~/.local/state/agents-cockpit`. `None` without a home.
pub fn default_state_dir() -> Option<PathBuf> {
    // The XDG specification ignores a relative path.
    let xdg = env::var_os(XDG_STATE_HOME)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute());
    let base = match xdg {
        Some(base) => base,
        None => PathBuf::from(env::var_os(HOME)?).join(HOME_STATE),
    };
    Some(base.join(STATE_FOLDER_NAME))
}

/// The daemon's socket in the state folder `state_dir`.
pub fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SOCKET_FILE)
}

/// The state folder, created with mode 700.
#[derive(Debug, Clone)]
pub(crate) struct StateDir {
    root: PathBuf,
}

/// Held while the daemon runs. The operating system releases it when the process ends.
#[derive(Debug)]
pub(crate) struct ServeLock {
    _file: File,
}

impl StateDir {
    /// Creates the folder and its subfolders when missing, and sets their mode to 700.
    pub(crate) fn create(root: &Path) -> Result<Self, StoreError> {
        let state = Self {
            root: root.to_owned(),
        };
        for folder in [state.root.clone(), state.events(), state.raw()] {
            create_private_folder(&folder)?;
        }
        Ok(state)
    }

    /// Takes `serve.lock`, or fails with `AlreadyRunning`.
    pub(crate) fn lock(&self) -> Result<ServeLock, StoreError> {
        let path = self.root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(FILE_MODE)
            .open(&path)
            .map_err(|source| StoreError::Lock {
                path: path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => Ok(ServeLock { _file: file }),
            Err(TryLockError::WouldBlock) => Err(StoreError::AlreadyRunning { path }),
            Err(TryLockError::Error(source)) => Err(StoreError::Lock { path, source }),
        }
    }

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

    /// `raw/<agent-id>.jsonl`.
    pub(crate) fn raw_log(&self, agent: AgentId) -> PathBuf {
        self.raw()
            .join(format!("{}.{RAW_LOG_EXTENSION}", agent.get()))
    }
}

fn create_private_folder(path: &Path) -> Result<(), StoreError> {
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

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

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

    #[test]
    fn tightens_an_existing_folder() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, Permissions::from_mode(0o755)).unwrap();

        StateDir::create(&root).unwrap();

        let mode = std::fs::metadata(&root).unwrap().mode() & 0o777;
        assert_eq!(mode, FOLDER_MODE);
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
        state.lock().unwrap();
    }

    /// Whatever the umask of the process that runs the test.
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
        let mut logs = EventLogs::new(state.events());
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
