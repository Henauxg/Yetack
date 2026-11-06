//! What `serve` and the daemon in the app share: the state folder's lock, the state read back,
//! and the hub's task. The socket, the signals and the logging belong to `serve` and its binary.

#[cfg(unix)]
use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::daemon_config::DaemonConfig;
use crate::hub;
use crate::hub::{Hub, HubInput, Restored};
use crate::serve::ServeError;
use crate::store::{ServeLock, StateDir};

/// Inputs waiting for the hub. A full queue makes the process and connection tasks wait.
const HUB_INPUTS: usize = 1024;

/// A daemon that holds its state folder's lock and has read its state back, before its hub
/// starts any agent.
pub(crate) struct PreparedDaemon {
    state: StateDir,
    lock: ServeLock,
    config: DaemonConfig,
    restored: Restored,
}

/// A running daemon. Its agents' processes run on the Tokio runtime it started on, and end with
/// that runtime's tasks.
pub(crate) struct Daemon {
    hub: JoinHandle<()>,
    inputs: mpsc::Sender<HubInput>,
    _lock: ServeLock,
}

impl Daemon {
    /// Takes the lock first, so a second daemon fails with `StoreError::AlreadyRunning` without
    /// touching the first one's files. Reads files: it blocks.
    pub(crate) fn prepare(config: DaemonConfig) -> Result<PreparedDaemon, ServeError> {
        let state = StateDir::create(&config.state_dir)?;
        let lock = state.lock()?;
        let restored = hub::restore(&state, &config.claude_config_dir)?;
        Ok(PreparedDaemon {
            state,
            lock,
            config,
            restored,
        })
    }

    pub(crate) fn inputs(&self) -> &mpsc::Sender<HubInput> {
        &self.inputs
    }

    /// Completes when the hub's task ends, which happens only when it fails.
    #[cfg(unix)]
    pub(crate) async fn hub_ended(&mut self) {
        // Either way the hub is gone.
        let _ = (&mut self.hub).await;
    }

    /// Stops the hub. The agents stay marked as running, so the next daemon resumes them.
    pub(crate) fn stop_hub(&self) {
        self.hub.abort();
    }
}

impl PreparedDaemon {
    #[cfg(unix)]
    pub(crate) fn socket(&self) -> PathBuf {
        self.state.socket()
    }

    /// Starts the hub, which resumes the agents that were running. Spawns on the current Tokio
    /// runtime.
    pub(crate) fn start(self) -> Daemon {
        let Self {
            state,
            lock,
            config,
            restored,
        } = self;
        let (inputs, received) = mpsc::channel(HUB_INPUTS);
        let hub = Hub::new(state, config, restored, inputs.clone());
        Daemon {
            hub: tokio::spawn(hub.run(received)),
            inputs,
            _lock: lock,
        }
    }
}
