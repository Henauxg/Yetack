//! The agents-cockpit daemon: Claude Code processes, the hub, the store and the socket server.
//!
//! [`serve`] runs it behind its socket: the `agents-cockpit` binary calls it for
//! `agents-cockpit serve`. [`InAppDaemon`] runs it inside the desktop app, for clients in the
//! same process.

mod accounts;
mod browse;
mod daemon;
mod daemon_config;
mod error_chain;
mod hub;
mod in_app_daemon;
mod machine_load;
mod mcp_remove;
mod process;
mod read_file;
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

pub use daemon_config::{
    CLAUDE_PROGRAM_ENV, DaemonConfig, HOME_ENV, SERVE_SUBCOMMAND, STATE_DIR_FLAG,
};
pub use in_app_daemon::InAppDaemon;
pub use process::ProcessError;
#[cfg(unix)]
pub use serve::serve;
pub use serve::{START_TASK, ServeError};
#[cfg(unix)]
pub use server::{PeerError, UserId};
pub use store::{StoreError, default_state_dir, socket_path};
