//! The agents-cockpit daemon: Claude Code processes, the hub, the store and the socket server.
//!
//! [`serve`] runs it behind its socket: the `agents-cockpit` binary calls it for
//! `agents-cockpit serve`. [`InAppDaemon`] runs it inside the desktop app, for clients in the
//! same process.

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
mod test_script;
mod transcript;

pub use daemon_config::{
    CLAUDE_PROGRAM_ENV, DaemonConfig, HOME_ENV, SERVE_SUBCOMMAND, STATE_DIR_FLAG,
};
pub use in_app_daemon::InAppDaemon;
pub use process::ProcessError;
pub use serve::{ServeError, serve};
pub use server::{PeerError, UserId};
pub use store::{StoreError, default_state_dir, socket_path};
