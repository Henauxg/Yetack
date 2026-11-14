//! The agents-cockpit daemon: Claude Code processes, the hub, the store and the socket server.
//!
//! [`serve`] runs it behind its socket: the `agents-cockpit` binary calls it for
//! `agents-cockpit serve`. [`InAppDaemon`] runs it inside the desktop app, for clients in the
//! same process.

mod accounts;
mod browse;
mod build_commit;
mod daemon;
mod daemon_config;
mod error_chain;
mod hub;
mod in_app_daemon;
mod lock_holder;
mod machine_load;
mod mcp_remove;
mod process;
mod read_file;
mod repository;
mod scheduled_jobs;
mod serve;
mod server;
mod sessions;
mod store;
#[cfg(test)]
#[cfg(unix)]
mod test_script;
mod transcript;
#[cfg(windows)]
mod windows_files;
#[cfg(any(windows, test))]
mod windows_path;

pub use accounts::{API_KEY_ENV, AUTH_TOKEN_ENV, ENVIRONMENT_CREDENTIALS_ENV};
pub use build_commit::{RELEASE_VERSION, build_commit, version_text};
pub use daemon_config::{
    CLAUDE_CONFIG_DIR_ENV, CLAUDE_HOME_FOLDER, CLAUDE_PROGRAM_ENV, DaemonConfig, HOME_ENV,
    SERVE_SUBCOMMAND, STATE_DIR_FLAG,
};
pub use in_app_daemon::InAppDaemon;
#[cfg(unix)]
pub use lock_holder::{
    LockHolder, LockHolderError, StopLockHolderError, lock_held_since, lock_holder,
    stop_lock_holder,
};
pub use process::ProcessError;
pub use scheduled_jobs::may_hold_scheduled_job;
#[cfg(unix)]
pub use serve::serve;
pub use serve::{START_TASK, ServeError};
#[cfg(unix)]
pub use server::{PeerError, UserId};
pub use store::{StoreError, default_state_dir, socket_path};
