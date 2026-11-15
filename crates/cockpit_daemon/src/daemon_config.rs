//! Where the daemon keeps its state, and which `claude` it runs.

use std::env;
use std::path::PathBuf;

use crate::store::default_state_dir;

/// The `yetack` subcommand that runs the daemon behind its socket.
pub const SERVE_SUBCOMMAND: &str = "serve";
/// The `yetack` option that names the state folder, without its leading `--`.
pub const STATE_DIR_FLAG: &str = "state-dir";

/// Names the `claude` program to run instead of the one on `PATH`, for tests.
pub const CLAUDE_PROGRAM_ENV: &str = "YETACK_CLAUDE";
pub(crate) const DEFAULT_CLAUDE_PROGRAM: &str = "claude";
/// Claude Code's own setting for its folder, which holds its login, settings and sessions.
pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";
/// The name of Claude Code's own config folder, in the home folder.
pub const CLAUDE_HOME_FOLDER: &str = ".claude";
/// Names the home folder, which the default state folder and Claude Code's folder need.
#[cfg(unix)]
pub const HOME_ENV: &str = "HOME";
/// Names the home folder, which the default state folder and Claude Code's folder need.
#[cfg(windows)]
pub const HOME_ENV: &str = "USERPROFILE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    pub state_dir: PathBuf,
    pub claude_program: PathBuf,
    /// The default account's Claude Code folder, which its `claude` gets from the daemon's own
    /// environment: `CLAUDE_CONFIG_DIR`, or `.claude` in the home folder.
    pub claude_config_dir: PathBuf,
}

impl DaemonConfig {
    /// From the environment, with `state_dir` replacing the default state folder when given.
    /// `None` when `HOME` is unset and a default needs it.
    #[must_use]
    pub fn from_env(state_dir: Option<PathBuf>) -> Option<Self> {
        let state_dir = match state_dir {
            Some(state_dir) => state_dir,
            None => default_state_dir()?,
        };
        let claude_config_dir = match env::var_os(CLAUDE_CONFIG_DIR_ENV) {
            Some(folder) => PathBuf::from(folder),
            None => PathBuf::from(env::var_os(HOME_ENV)?).join(CLAUDE_HOME_FOLDER),
        };
        let claude_program = env::var_os(CLAUDE_PROGRAM_ENV)
            .map_or_else(|| PathBuf::from(DEFAULT_CLAUDE_PROGRAM), PathBuf::from);
        Some(Self {
            state_dir,
            claude_program,
            claude_config_dir,
        })
    }
}
