//! The "Machines" dialog: the servers the app reaches beside this computer, which it adds and
//! removes in the config.

use super::app_state::AppState;
use super::ui_state::{Dialog, MachinesView};
use crate::config::MachineConfig;
use crate::machine::MachineId;

/// A change to the machines, which the connection task carries out and writes to the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineChange {
    Add(MachineConfig),
    Remove(MachineId),
    /// Tries a machine that does not answer again now.
    Retry(MachineId),
}

pub fn open_machines(state: &mut AppState) {
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::Machines(MachinesView::default()));
}

pub fn with_view(state: &mut AppState, change: impl FnOnce(&mut MachinesView)) {
    if let Some(Dialog::Machines(view)) = &mut state.ui.dialog {
        change(view);
    }
}

/// The server the form names, once it holds what the app needs to reach it. Otherwise the form
/// says why, and nothing is added. An empty name takes the host's.
pub fn add(state: &mut AppState) -> Option<MachineChange> {
    let Some(Dialog::Machines(view)) = &state.ui.dialog else {
        return None;
    };
    let checked = check(state, view);
    let Some(Dialog::Machines(view)) = &mut state.ui.dialog else {
        return None;
    };
    match checked {
        Ok(machine) => {
            *view = MachinesView::default();
            Some(MachineChange::Add(machine))
        }
        Err(error) => {
            view.error = Some(error.to_owned());
            None
        }
    }
}

fn check(state: &AppState, view: &MachinesView) -> Result<MachineConfig, &'static str> {
    let host = view.host.trim();
    let remote_command = view.remote_command.trim();
    let name = match view.name.trim() {
        "" => host,
        name => name,
    };
    // ssh would read such a host as an option.
    if host.is_empty() || host.starts_with('-') {
        return Err("Name the SSH host, such as devbox or dev@devbox.");
    }
    if !remote_command.starts_with('/') {
        return Err("Give the absolute path of agents-cockpit on the server.");
    }
    let taken = std::iter::once(&state.machines.this)
        .chain(state.machines.others.values())
        .any(|known| known.daemon.machine == name);
    if taken {
        return Err("Another machine has this name.");
    }
    Ok(MachineConfig {
        name: name.to_owned(),
        host: host.to_owned(),
        remote_command: remote_command.to_owned(),
    })
}

/// Forgets a machine the app no longer reaches, and its agents.
pub fn forget(state: &mut AppState, machine: MachineId) {
    if machine == MachineId::THIS {
        return;
    }
    state.machines.others.remove(&machine);
    state
        .agents
        .retain(|agent, _| MachineId::of(*agent) != machine);
    state.ui.folded_machines.remove(&machine);
    state.fix_selection();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::{AgentEntry, MachineState};
    use cockpit_protocol::{AgentId, AgentStatus};

    fn typed(state: &mut AppState, name: &str, host: &str, remote_command: &str) {
        open_machines(state);
        with_view(state, |view| {
            view.name = name.to_owned();
            view.host = host.to_owned();
            view.remote_command = remote_command.to_owned();
        });
    }

    fn error(state: &AppState) -> Option<String> {
        match &state.ui.dialog {
            Some(Dialog::Machines(view)) => view.error.clone(),
            _ => None,
        }
    }

    #[test]
    fn a_server_needs_a_host_and_an_absolute_path_and_takes_the_hosts_name() {
        let mut state = state_with(Vec::new());

        typed(&mut state, "", "-oProxyCommand=x", "/opt/agents-cockpit");
        let option = add(&mut state);
        let option_error = error(&state);
        typed(&mut state, "", "devbox", "agents-cockpit");
        let relative = add(&mut state);
        typed(&mut state, " ", " dev@devbox ", " /opt/agents-cockpit ");
        let added = add(&mut state);

        assert_eq!(option, None);
        assert!(option_error.is_some());
        assert_eq!(relative, None);
        assert_eq!(
            added,
            Some(MachineChange::Add(MachineConfig {
                name: "dev@devbox".to_owned(),
                host: "dev@devbox".to_owned(),
                remote_command: "/opt/agents-cockpit".to_owned(),
            }))
        );
        assert_eq!(
            error(&state),
            None,
            "the form empties once the server is added"
        );
    }

    #[test]
    fn a_name_another_machine_has_is_refused() {
        let mut state = state_with(Vec::new());
        state.machines.others.insert(
            MachineId::new(1),
            MachineState::new(DaemonLabel::ssh("devbox".to_owned())),
        );

        typed(&mut state, "devbox", "other-host", "/opt/agents-cockpit");

        assert_eq!(add(&mut state), None);
        assert!(error(&state).is_some());
    }

    #[test]
    fn a_forgotten_machine_takes_its_agents_and_only_them() {
        let devbox = MachineId::new(1);
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.others.insert(
            devbox,
            MachineState::new(DaemonLabel::ssh("devbox".to_owned())),
        );
        let mut remote = summary(1, AgentStatus::Working);
        remote.id = devbox.app_agent(AgentId::new(1));
        state
            .agents
            .insert(remote.id, AgentEntry::new(remote.clone()));
        state.ui.selected = Some(remote.id);

        forget(&mut state, devbox);
        forget(&mut state, MachineId::THIS);

        assert!(state.machines.others.is_empty());
        let agents: Vec<AgentId> = state.agents.keys().copied().collect();
        assert_eq!(agents, [AgentId::new(1)]);
        assert_eq!(state.ui.selected, Some(AgentId::new(1)));
    }
}
