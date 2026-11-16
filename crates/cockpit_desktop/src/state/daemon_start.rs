//! "Start the daemon": a server answers SSH, but no daemon listens there, which its bridge says.
//! The app starts the daemon over SSH: its systemd unit when it has one, else on its own, apart
//! from the SSH session. This is the last resort: a service or a crontab start keeps the agents
//! working while the app is closed, through a restart of the server too.

use cockpit_daemon::NOT_SERVING_HINT;

use super::app_state::{AppState, ConnectionState, MachineState};
use super::server_setup::{BootStart, InstallReport, Starter};
use crate::config::MachineConfig;
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
    let run = run_of(known)?;
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

/// The daemon on the server as its config names it now.
fn run_of(known: &MachineState) -> Option<StartRun> {
    let server = known.server.as_ref()?;
    Some(StartRun {
        host: server.host.clone(),
        binary: server.remote_command.clone(),
    })
}

/// After a start without systemd, what the server's report said, while it has something to
/// offer: the Machines dialog then shows that server the start at boot, as the setup does. A
/// report on a daemon the server's config no longer names is dropped.
pub fn offer_boot_start(
    state: &mut AppState,
    machine: MachineId,
    run: &StartRun,
    report: InstallReport,
) {
    let needs = report.starter == Starter::Alone
        && (report.boot != Some(BootStart::Yes) || report.cron_stopped());
    if let Some(known) = state.machines.get_mut(machine)
        && run_of(known).as_ref() == Some(run)
    {
        known.boot_offer = needs.then_some(report);
    }
}

/// "Add it to my crontab" on a server's row: only once its report said nothing starts the daemon
/// at boot, one at a time. Returns the daemon to start at boot.
pub fn add_boot_start(state: &mut AppState, machine: MachineId) -> Option<StartRun> {
    let known = state.machines.get_mut(machine)?;
    let run = run_of(known)?;
    let report = known.boot_offer.as_mut()?;
    if report.checking || report.boot != Some(BootStart::No) {
        return None;
    }
    report.checking = true;
    report.failure = None;
    Some(run)
}

/// The add of `run` ended, with the script's lines or why it failed. The row stays, saying where
/// the start at boot now stands.
pub fn boot_start_added(
    state: &mut AppState,
    machine: MachineId,
    run: &StartRun,
    result: Result<Vec<String>, String>,
) {
    let Some(report) = state
        .machines
        .get_mut(machine)
        .filter(|known| run_of(known).as_ref() == Some(run))
        .and_then(|known| known.boot_offer.as_mut())
    else {
        return;
    };
    *report = match result {
        Ok(lines) => InstallReport::read(lines.iter().map(String::as_str)),
        Err(failure) => InstallReport {
            checking: false,
            failure: Some(failure),
            ..report.clone()
        },
    };
}

/// A server's new config: an offer made for another host or daemon goes.
pub fn follow_config(known: &mut MachineState, config: &MachineConfig) {
    let same = known.server.as_ref().is_some_and(|server| {
        server.host == config.host && server.remote_command == config.remote_command
    });
    if !same {
        known.boot_offer = None;
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::machines;

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

    fn report(lines: &[&str]) -> InstallReport {
        InstallReport::read(lines.iter().copied())
    }

    const ALONE: &[&str] = &[
        "install-daemon: service none",
        "install-daemon: boot no",
        "install-daemon: cron @reboot /home/someone/.local/share/yetack/bin/yetack serve",
        "install-daemon: crond yes",
    ];

    fn lab_run() -> StartRun {
        StartRun {
            host: "lab".to_owned(),
            binary: "/home/someone/.local/share/yetack/bin/yetack".to_owned(),
        }
    }

    fn offered(state: &AppState) -> Option<InstallReport> {
        state
            .machines
            .get(lab())
            .and_then(|known| known.boot_offer.clone())
    }

    #[test]
    fn a_start_without_systemd_offers_the_start_at_boot_and_a_service_offers_nothing() {
        let mut alone = server(NOT_SERVING_HINT);
        let mut service = server(NOT_SERVING_HINT);

        offer_boot_start(&mut alone, lab(), &lab_run(), report(ALONE));
        offer_boot_start(
            &mut service,
            lab(),
            &lab_run(),
            report(&["install-daemon: service systemd"]),
        );

        assert_eq!(
            offered(&alone).map(|offer| offer.boot),
            Some(Some(BootStart::No))
        );
        assert_eq!(offered(&service), None);
    }

    #[test]
    fn the_rows_add_runs_once_and_its_answer_stays_on_the_row() {
        let mut state = server(NOT_SERVING_HINT);
        offer_boot_start(&mut state, lab(), &lab_run(), report(ALONE));

        let run = add_boot_start(&mut state, lab());
        let twice = add_boot_start(&mut state, lab());
        let said: Vec<String> = ALONE
            .iter()
            .map(|line| line.replace("boot no", "boot yes"))
            .collect();
        boot_start_added(&mut state, lab(), &lab_run(), Ok(said));

        assert_eq!(run, Some(lab_run()), "the add names the server's daemon");
        assert_eq!(twice, None, "one add at a time");
        assert_eq!(
            offered(&state).map(|offer| (offer.boot, offer.checking)),
            Some((Some(BootStart::Yes), false))
        );
        assert_eq!(
            add_boot_start(&mut state, lab()),
            None,
            "nothing left to add"
        );
    }

    #[test]
    fn an_edit_to_another_host_or_daemon_drops_the_offer_and_a_rename_keeps_it() {
        let mut state = server(NOT_SERVING_HINT);
        offer_boot_start(&mut state, lab(), &lab_run(), report(ALONE));
        let config = |host: &str, remote_command: &str| MachineConfig {
            name: "lab box".to_owned(),
            host: host.to_owned(),
            remote_command: remote_command.to_owned(),
            source_dir: None,
        };

        machines::apply_edit(&mut state, lab(), &config("lab", &lab_run().binary));
        let renamed = offered(&state).is_some();
        machines::apply_edit(&mut state, lab(), &config("lab", "/opt/yetack/bin/yetack"));

        assert!(renamed, "a new name is the same daemon");
        assert_eq!(offered(&state), None);
    }

    #[test]
    fn a_report_on_a_daemon_the_config_no_longer_names_is_dropped() {
        let mut state = server(NOT_SERVING_HINT);
        let before = StartRun {
            binary: "/opt/yetack/bin/yetack".to_owned(),
            ..lab_run()
        };

        offer_boot_start(&mut state, lab(), &before, report(ALONE));

        assert_eq!(offered(&state), None);
    }
}
