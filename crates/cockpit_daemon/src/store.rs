//! The state folder: the lock, `agents.json`, `settings.json`, and the event and raw logs.

mod agents_file;
mod atomic_file;
mod daemon_settings;
mod event_log;
mod raw_log;
mod state_dir;
mod store_error;

pub(crate) use agents_file::{AgentRecord, AgentsFile, PastFile};
pub(crate) use daemon_settings::DaemonSettings;
pub(crate) use event_log::{EventLogs, LogName, ReplayRead, for_each_event, read_replay};
pub(crate) use raw_log::{Direction, RawLog};
pub(crate) use state_dir::StateDir;
pub use state_dir::{default_state_dir, socket_path};
pub use store_error::StoreError;
