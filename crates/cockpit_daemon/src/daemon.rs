//! What `serve` and the daemon in the app share: the state folder's lock, the state read back,
//! and the hub's task. The socket, the signals and the logging belong to `serve` and its binary.

#[cfg(unix)]
use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::daemon_config::DaemonConfig;
use crate::hub;
use crate::hub::{Hub, HubInput, Restored};
use crate::machine_load;
use crate::serve::ServeError;
use crate::store::{RecordedProcess, ServeLock, StateDir};

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
        let mut restored = hub::restore(&state, &config.claude_config_dir)?;
        stop_leftovers(&mut restored);
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

/// Stops the `claude` processes a daemon that died left running, before any agent resumes its
/// session, and forgets them.
fn stop_leftovers(restored: &mut Restored) {
    let recorded: Vec<RecordedProcess> = restored
        .agents
        .iter_mut()
        .filter_map(|(record, _)| record.process.take())
        .collect();
    machine_load::stop_leftovers(&recorded);
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

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Command, Stdio};

    use cockpit_protocol::{AccountId, AgentId, Position, ProcessId, Timestamp};
    use rustix::process::Signal;

    use super::*;
    use crate::store::{AgentRecord, AgentsFile};

    /// The daemon died while an agent's `claude` ran: the next daemon stops that `claude` before
    /// it resumes the agent's session.
    #[test]
    fn a_restarted_daemon_stops_the_claude_its_predecessor_left() {
        let temp = tempfile::tempdir().unwrap();
        let config = DaemonConfig {
            state_dir: temp.path().join("state"),
            claude_program: temp.path().join("no-claude"),
            claude_config_dir: temp.path().join("claude"),
        };
        let mut leftover = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let process = machine_load::record_process(ProcessId::new(leftover.id()));
        assert!(process.is_some());
        let state = StateDir::create(&config.state_dir).unwrap();
        AgentsFile {
            next_agent: AgentId::new(2),
            replay_floor: None,
            agents: vec![AgentRecord {
                id: AgentId::new(1),
                title: "Build".to_owned(),
                folder: temp.path().to_owned(),
                session: None,
                model: None,
                effort: None,
                mode: None,
                running: true,
                started_at: Timestamp::from_unix_millis(0),
                past: None,
                position: Position::default(),
                process,
                account: AccountId::DEFAULT,
            }],
        }
        .save(&state.agents_file())
        .unwrap();

        let prepared = Daemon::prepare(config).unwrap();

        let status = leftover.wait().unwrap();
        assert_eq!(status.signal(), Some(Signal::TERM.as_raw()));
        let (record, _) = prepared.restored.agents.first().unwrap();
        assert_eq!(record.process, None);
        assert!(record.running, "the agent still resumes");
    }
}
