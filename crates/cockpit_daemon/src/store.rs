//! The state folder: the lock, `agents.json`, `settings.json`, and the event and raw logs.

mod agents_file;
mod atomic_file;
mod error;
mod event_log;
mod raw_log;
mod settings_file;
mod state_dir;

pub(crate) use agents_file::{AgentRecord, AgentsFile};
pub use error::StoreError;
pub(crate) use event_log::{EventLogs, LogName, ReplayRead, read_log, read_replay};
pub(crate) use raw_log::{Direction, RawLog};
pub(crate) use settings_file::DaemonSettings;
pub(crate) use state_dir::StateDir;
pub use state_dir::{default_state_dir, socket_path};
