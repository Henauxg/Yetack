//! "Start the daemon": a server answers SSH, but no daemon listens there, which its bridge says.
//! The app starts the daemon over SSH: its systemd unit when it has one, else on its own, apart
//! from the SSH session. This is the last resort: a service or a crontab start keeps the agents
//! working while the app is closed, through a restart of the server too.

use cockpit_daemon::NOT_SERVING_HINT;

use super::app_state::{AppState, ConnectionState, MachineState};
use crate::machine::MachineId;

/// Where a start the user asked for stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DaemonStart {
    #[default]
    Idle,
    Starting,
    /// What the start said when it failed.
    Failed(String),
}

/// What the connection task runs: the start of the daemon at `binary` on `host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRun {
    pub host: String,
    pub binary: String,
}

/// Whether the machine is a server that answers SSH while no daemon listens there: its bridge
/// said so as it failed.
pub fn no_daemon_runs(known: &MachineState) -> bool {
    known.server.is_some()
        && matches!(
            &known.connection,
            ConnectionState::Disconnected { error, .. } if error.contains(NOT_SERVING_HINT)
        )
}

/// "Start the daemon": only on a server where no daemon runs, one start at a time.
pub fn start(state: &mut AppState, machine: MachineId) -> Option<StartRun> {
    let known = state.machines.get_mut(machine)?;
    if !no_daemon_runs(known) || known.start == DaemonStart::Starting {
        return None;
    }
    let server = known.server.as_ref()?;
    let run = StartRun {
        host: server.host.clone(),
        binary: server.remote_command.clone(),
    };
    known.start = DaemonStart::Starting;
    Some(run)
}

/// The start ended: the app connects again once it worked, and the line says why it failed
/// otherwise. A connection made meanwhile already ended the start.
pub fn started(state: &mut AppState, machine: MachineId, result: Result<(), String>) {
    if let Some(known) = state.machines.get_mut(machine)
        && known.start == DaemonStart::Starting
    {
        known.start = match result {
            Ok(()) => DaemonStart::Idle,
            Err(message) => DaemonStart::Failed(message),
        };
    }
}

/// A connection forgets the last start: the next "Start the daemon" begins anew.
pub fn connected(known: &mut MachineState) {
    known.start = DaemonStart::Idle;
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::config::MachineConfig;
    use crate::daemon_label::DaemonLabel;

    fn server(error: &str) -> AppState {
        let mut state = crate::state::app_state::tests::state_with(Vec::new());
        let mut known = MachineState::new(DaemonLabel::ssh("lab".to_owned()));
        known.server = Some(MachineConfig {
            name: "lab".to_owned(),
            host: "lab".to_owned(),
            remote_command: "/home/someone/.local/share/yetack/bin/yetack".to_owned(),
            source_dir: None,
        });
        known.connection = ConnectionState::Disconnected {
            error: error.to_owned(),
            retry_at: Instant::now(),
        };
        state.machines.others.insert(MachineId::new(1), known);
        state
    }

    fn lab() -> MachineId {
        MachineId::new(1)
    }

    #[test]
    fn a_start_is_offered_only_when_the_bridge_says_no_daemon_listens() {
        let no_daemon = format!(
            "cannot connect to /home/someone/.local/state/yetack/yetack.sock: {NOT_SERVING_HINT}"
        );
        let mut stopped = server(&no_daemon);
        let mut unreachable = server("ssh: connect to host lab port 22: Connection refused");

        let run = start(&mut stopped, lab());
        let twice = start(&mut stopped, lab());

        assert_eq!(
            run,
            Some(StartRun {
                host: "lab".to_owned(),
                binary: "/home/someone/.local/share/yetack/bin/yetack".to_owned(),
            })
        );
        assert_eq!(twice, None, "one start at a time");
        assert_eq!(start(&mut unreachable, lab()), None);
    }

    #[test]
    fn a_failed_start_keeps_what_it_said_and_a_connection_forgets_it() {
        let mut state = server(NOT_SERVING_HINT);
        start(&mut state, lab());

        started(
            &mut state,
            lab(),
            Err("systemctl could not start yetack".to_owned()),
        );
        let failed = state.machines.get(lab()).map(|known| known.start.clone());
        if let Some(known) = state.machines.get_mut(lab()) {
            connected(known);
        }

        assert_eq!(
            failed,
            Some(DaemonStart::Failed(
                "systemctl could not start yetack".to_owned()
            ))
        );
        assert_eq!(
            state.machines.get(lab()).map(|known| known.start.clone()),
            Some(DaemonStart::Idle)
        );
    }

    #[test]
    fn a_start_that_fails_once_the_app_connected_leaves_no_failure_behind() {
        let mut state = server(NOT_SERVING_HINT);
        start(&mut state, lab());
        if let Some(known) = state.machines.get_mut(lab()) {
            connected(known);
        }

        started(
            &mut state,
            lab(),
            Err("the daemon did not answer".to_owned()),
        );

        assert_eq!(
            state.machines.get(lab()).map(|known| known.start.clone()),
            Some(DaemonStart::Idle)
        );
    }
}
