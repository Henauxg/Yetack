//! The "Machines" dialog: the servers the app reaches beside this computer, which it adds,
//! edits and removes in the config.

use cockpit_protocol::AgentId;

use super::app_state::AppState;
use super::apply::forget_agent;
use super::drafts::Drafts;
use super::ui_state::{Dialog, MachineEdit, MachinesView};
use super::{daemon_update, file_tree};
use crate::config::MachineConfig;
use crate::daemon_label::DaemonLabel;
use crate::machine::MachineId;

/// A change to the machines, which the connection task carries out and writes to the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineChange {
    Add(MachineConfig),
    /// The server reaches its daemon otherwise, or takes another name: its connection starts
    /// again.
    Edit(MachineId, MachineConfig),
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
    let checked = check(state, &view.name, &view.host, &view.remote_command, None);
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

/// Opens the server's form in its row, filled from its config.
pub fn start_edit(state: &mut AppState, machine: MachineId) {
    let Some(config) = state
        .machines
        .others
        .get(&machine)
        .and_then(|known| known.server.clone())
    else {
        return;
    };
    with_view(state, |view| {
        view.editing = Some(MachineEdit {
            machine,
            name: config.name,
            host: config.host,
            remote_command: config.remote_command,
            source_dir: config.source_dir.unwrap_or_default(),
            error: None,
        });
    });
}

pub fn cancel_edit(state: &mut AppState) {
    with_view(state, |view| view.editing = None);
}

/// The server as its form now names it, checked as a new one is. A form that changes nothing
/// closes without a change, so the server keeps its connection.
pub fn save_edit(state: &mut AppState) -> Option<MachineChange> {
    let Some(Dialog::Machines(view)) = &state.ui.dialog else {
        return None;
    };
    let edit = view.editing.as_ref()?;
    let machine = edit.machine;
    let source_dir = edit.source_dir.trim();
    let checked = if source_dir.is_empty() || source_dir.starts_with('/') {
        check(
            state,
            &edit.name,
            &edit.host,
            &edit.remote_command,
            Some(machine),
        )
        .map(|config| MachineConfig {
            source_dir: (!source_dir.is_empty()).then(|| source_dir.to_owned()),
            ..config
        })
    } else {
        Err("Give the absolute path of the checkout on the server, or leave it empty.")
    };
    let before = state
        .machines
        .others
        .get(&machine)
        .and_then(|known| known.server.as_ref());
    let change = checked
        .map(|config| (before != Some(&config)).then_some(MachineChange::Edit(machine, config)));
    let Some(Dialog::Machines(view)) = &mut state.ui.dialog else {
        return None;
    };
    match change {
        Ok(change) => {
            view.editing = None;
            change
        }
        Err(error) => {
            if let Some(edit) = &mut view.editing {
                edit.error = Some(error.to_owned());
            }
            None
        }
    }
}

/// What a saved edit changes in the window: the server's name and config, the plan of its next
/// update, and the preferences its old name held.
pub fn apply_edit(state: &mut AppState, machine: MachineId, config: &MachineConfig) {
    let Some(known) = state.machines.others.get_mut(&machine) else {
        return;
    };
    let old = known.daemon.machine.clone();
    known.daemon = DaemonLabel::ssh(config.name.clone());
    known.server = Some(config.clone());
    daemon_update::follow_config(known, config);
    state.prefs.rename_machine(&old, &config.name);
}

/// A server's config from a form's fields, or why the app could not reach it. `editing` is the
/// server the form edits, whose own name is free.
fn check(
    state: &AppState,
    name: &str,
    host: &str,
    remote_command: &str,
    editing: Option<MachineId>,
) -> Result<MachineConfig, &'static str> {
    let host = host.trim();
    let remote_command = remote_command.trim();
    let name = match name.trim() {
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
    let taken = std::iter::once((MachineId::THIS, &state.machines.this))
        .chain(state.machines.others.iter().map(|(id, known)| (*id, known)))
        .any(|(machine, known)| Some(machine) != editing && known.daemon.machine == name);
    if taken {
        return Err("Another machine has this name.");
    }
    Ok(MachineConfig {
        name: name.to_owned(),
        host: host.to_owned(),
        remote_command: remote_command.to_owned(),
        source_dir: None,
    })
}

/// Forgets a machine the app no longer reaches, and its agents with what the window holds of
/// them: drafts, file views, the undo of a close.
pub fn forget(state: &mut AppState, drafts: &mut Drafts, machine: MachineId) {
    if machine == MachineId::THIS {
        return;
    }
    file_tree::forget_machine_panes(state, machine);
    state.machines.others.remove(&machine);
    let on_machine = |agent: &AgentId| MachineId::of(*agent) == machine;
    let agents: Vec<AgentId> = state.agents.keys().copied().filter(on_machine).collect();
    for agent in agents {
        forget_agent(state, drafts, agent);
    }
    state.ui.closing.retain(|agent| !on_machine(agent));
    state.ui.closed.take_if(|closed| on_machine(&closed.agent));
    state.ui.folded_machines.remove(&machine);
    state
        .ui
        .dialog
        .take_if(|dialog| matches!(dialog, Dialog::DaemonUpdate(shown) if *shown == machine));
    state.fix_selection();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::daemon_update::{ProtocolGap, UpdatePlan, VersionGap};
    use crate::state::{AgentEntry, MachineState};
    use cockpit_protocol::{AccountId, AgentStatus};

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
                source_dir: None,
            }))
        );
        assert_eq!(
            error(&state),
            None,
            "the form empties once the server is added"
        );
    }

    /// A server known as `name`, with its config.
    fn server(state: &mut AppState, machine: u16, name: &str) {
        let mut known = MachineState::new(DaemonLabel::ssh(name.to_owned()));
        known.server = Some(MachineConfig {
            name: name.to_owned(),
            host: format!("dev@{name}"),
            remote_command: "/opt/agents-cockpit".to_owned(),
            source_dir: None,
        });
        state.machines.others.insert(MachineId::new(machine), known);
    }

    fn edit(state: &mut AppState, change: impl FnOnce(&mut MachineEdit)) {
        with_view(state, |view| {
            if let Some(edit) = &mut view.editing {
                change(edit);
            }
        });
    }

    fn edit_error(state: &AppState) -> Option<String> {
        match &state.ui.dialog {
            Some(Dialog::Machines(view)) => {
                view.editing.as_ref().and_then(|edit| edit.error.clone())
            }
            _ => None,
        }
    }

    #[test]
    fn an_edit_starts_from_the_config_and_saves_only_a_change() {
        let mut state = state_with(Vec::new());
        server(&mut state, 1, "devbox");
        open_machines(&mut state);

        start_edit(&mut state, MachineId::new(1));
        assert_eq!(save_edit(&mut state), None, "nothing changed");

        start_edit(&mut state, MachineId::new(1));
        edit(&mut state, |edit| {
            edit.name = "lab".to_owned();
            edit.source_dir = " /src/agents-cockpit ".to_owned();
        });
        assert_eq!(
            save_edit(&mut state),
            Some(MachineChange::Edit(
                MachineId::new(1),
                MachineConfig {
                    name: "lab".to_owned(),
                    host: "dev@devbox".to_owned(),
                    remote_command: "/opt/agents-cockpit".to_owned(),
                    source_dir: Some("/src/agents-cockpit".to_owned()),
                }
            ))
        );
        assert!(matches!(&state.ui.dialog, Some(Dialog::Machines(view)) if view.editing.is_none()));
    }

    #[test]
    fn an_edit_keeps_its_own_name_but_not_another_machines() {
        let mut state = state_with(Vec::new());
        server(&mut state, 1, "devbox");
        server(&mut state, 2, "lab");
        open_machines(&mut state);

        start_edit(&mut state, MachineId::new(1));
        edit(&mut state, |edit| edit.host = "dev@devbox2".to_owned());
        assert!(save_edit(&mut state).is_some(), "its own name is free");

        start_edit(&mut state, MachineId::new(1));
        edit(&mut state, |edit| edit.name = "lab".to_owned());
        assert_eq!(save_edit(&mut state), None);
        assert!(edit_error(&state).is_some());

        edit(&mut state, |edit| {
            edit.name = "devbox".to_owned();
            edit.source_dir = "src/agents-cockpit".to_owned();
        });
        assert_eq!(save_edit(&mut state), None, "a relative checkout");
        assert!(edit_error(&state).is_some());
    }

    #[test]
    fn a_saved_edit_renames_the_server_and_moves_its_preferences() {
        let mut state = state_with(Vec::new());
        server(&mut state, 1, "devbox");
        state
            .prefs
            .notifications
            .agents
            .insert("devbox".to_owned(), Default::default());
        let config = MachineConfig {
            name: "lab".to_owned(),
            host: "dev@lab".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
            source_dir: None,
        };

        apply_edit(&mut state, MachineId::new(1), &config);

        let known = &state.machines.others[&MachineId::new(1)];
        assert_eq!(known.daemon.machine, "lab");
        assert_eq!(known.server.as_ref(), Some(&config));
        assert!(state.prefs.notifications.agents.contains_key("lab"));
        assert!(!state.prefs.notifications.agents.contains_key("devbox"));
    }

    #[test]
    fn update_again_after_an_edit_runs_with_the_new_checkout() {
        let mut state = state_with(Vec::new());
        server(&mut state, 1, "devbox");
        let devbox = MachineId::new(1);
        let old_plan = UpdatePlan {
            host: "dev@devbox".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
            source_dir: None,
            commit: "3f9c2e1a7b4d5c6e".to_owned(),
        };
        if let Some(known) = state.machines.others.get_mut(&devbox) {
            known.version_gap = Some(VersionGap {
                kind: ProtocolGap::Older,
                details: "protocol 3, the app speaks 4".to_owned(),
                plan: Some(old_plan.clone()),
            });
        }
        assert_eq!(daemon_update::start(&mut state, devbox), Some(old_plan));
        daemon_update::finished(&mut state, devbox, Some(1));
        let config = MachineConfig {
            source_dir: Some("/src/agents-cockpit".to_owned()),
            ..state.machines.others[&devbox].server.clone().unwrap()
        };

        apply_edit(&mut state, devbox, &config);

        let again = daemon_update::start(&mut state, devbox);
        assert_eq!(
            again.and_then(|plan| plan.source_dir),
            Some("/src/agents-cockpit".to_owned())
        );
        let gap_plan = state.machines.others[&devbox]
            .version_gap
            .as_ref()
            .and_then(|gap| gap.plan.clone());
        assert_eq!(
            gap_plan.and_then(|plan| plan.source_dir),
            Some("/src/agents-cockpit".to_owned())
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
    fn a_forgotten_machine_takes_its_agents_panes_and_only_them() {
        let devbox = MachineId::new(1);
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.others.insert(
            devbox,
            MachineState::new(DaemonLabel::ssh("devbox".to_owned())),
        );
        let mut remote = summary(1, AgentStatus::Idle);
        remote.id = devbox.app_agent(AgentId::new(1));
        state
            .agents
            .insert(remote.id, AgentEntry::new(remote.clone()));
        for agent in [AgentId::new(1), remote.id] {
            state.ui.selected = Some(agent);
            crate::state::file_tree::toggle_pane(&mut state);
        }
        let local_machine = state.daemon_machine(AgentId::new(1)).map(str::to_owned);

        forget(&mut state, &mut Drafts::default(), devbox);

        let machines: Vec<Option<String>> = state
            .prefs
            .files_pane
            .agents
            .keys()
            .cloned()
            .map(Some)
            .collect();
        assert_eq!(machines, [local_machine]);
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
        state.ui.unread.insert(remote.id);
        state.ui.note_closed(
            devbox.app_agent(AgentId::new(2)),
            "Closed on devbox".to_owned(),
            None,
            AccountId::DEFAULT,
            None,
        );
        let mut drafts = Drafts::default();
        drafts.draft_mut(remote.id).text = "Half a message".to_owned();

        forget(&mut state, &mut drafts, devbox);
        forget(&mut state, &mut drafts, MachineId::THIS);

        assert!(state.machines.others.is_empty());
        let agents: Vec<AgentId> = state.agents.keys().copied().collect();
        assert_eq!(agents, [AgentId::new(1)]);
        assert_eq!(state.ui.selected, Some(AgentId::new(1)));
        assert!(state.ui.unread.is_empty());
        assert_eq!(
            state.ui.closed, None,
            "no undo brings back an agent of a removed server"
        );
        assert_eq!(drafts.draft(remote.id), None);
    }
}
