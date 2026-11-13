//! The machines as the interface names them, and the "Machines" dialog: a row per machine, and
//! the servers the app reaches beside this computer, which it adds, edits and removes in the
//! config.

use std::time::Instant;

use cockpit_protocol::{AgentId, ReleaseVersion};

use super::app_state::{AppState, ConnectionState, MachineState};
use super::apply::forget_agent;
use super::daemon_update::{self, DaemonUpdate};
use super::drafts::Drafts;
use super::file_tree;
use super::machine_status::{self, MachineStatus, StatusAction, Tone};
use super::ui_state::{Dialog, MachineEdit, MachineMenu, MachinesView};
use crate::config::MachineConfig;
use crate::daemon_label::{DaemonLabel, DaemonPlace};
use crate::format;
use crate::machine::MachineId;

/// What the interface calls this computer, unless its daemon runs over SSH.
pub const THIS_COMPUTER: &str = "Local";

/// A machine as the interface names it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MachineName {
    pub name: String,
    /// This computer, which shows a laptop where the others show a server.
    pub local: bool,
}

impl MachineName {
    pub fn new(machine: MachineId, known: &MachineState) -> Self {
        let local = machine == MachineId::THIS && known.daemon.place != DaemonPlace::Ssh;
        let name = if local {
            THIS_COMPUTER.to_owned()
        } else {
            known.daemon.machine.clone()
        };
        Self { name, local }
    }

    /// The name of `machine`, empty for a machine the app does not know.
    pub fn of(state: &AppState, machine: MachineId) -> Self {
        state
            .machines
            .get(machine)
            .map(|known| Self::new(machine, known))
            .unwrap_or_default()
    }
}

/// A machine's row in the Machines dialog.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineRow {
    pub machine: MachineId,
    pub name: MachineName,
    /// Where its daemon runs: "Background service", "SSH dev@devbox".
    pub place: String,
    pub agents: usize,
    /// A line under the place: when the app tries again, or that the agents stop with the app.
    pub note: Option<RowNote>,
    pub tone: Tone,
    pub chip: &'static str,
    /// The error, as the chip's tooltip.
    pub error: Option<String>,
    /// The release its daemon runs, while it is connected: an earlier connection's may no longer
    /// be the one it runs.
    pub version: Option<ReleaseVersion>,
    /// Its button: the first link of its line in the sidebar.
    pub action: Option<StatusAction>,
    /// A server the config names, which its ⋯ menu edits, copies and removes.
    pub server: Option<ServerRow>,
}

/// What a server's ⋯ menu acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRow {
    /// The SSH host, which the menu copies.
    pub host: String,
    /// Whether its form can open (`can_edit`).
    pub editable: bool,
}

/// The dialog's rows, this computer first.
pub fn rows(state: &AppState) -> Vec<MachineRow> {
    state
        .machines
        .all()
        .map(|(machine, known)| {
            let MachineStatus { tone, chip, line } = machine_status::status(known);
            let agents = state
                .agents
                .keys()
                .filter(|agent| MachineId::of(**agent) == machine)
                .count();
            let (error, action) = match line {
                Some(line) => (line.error, line.actions.first().copied()),
                None => (None, None),
            };
            MachineRow {
                machine,
                name: MachineName::new(machine, known),
                place: place(known),
                agents,
                note: RowNote::of(known),
                tone,
                chip,
                error,
                version: known
                    .daemon_version
                    .clone()
                    .filter(|_| known.is_connected()),
                action,
                server: known.server.as_ref().map(|config| ServerRow {
                    host: config.host.clone(),
                    editable: can_edit(known),
                }),
            }
        })
        .collect()
}

fn place(known: &MachineState) -> String {
    match known.daemon.place {
        DaemonPlace::InApp => "In this app".to_owned(),
        DaemonPlace::Service => "Background service".to_owned(),
        DaemonPlace::Socket => "Local daemon".to_owned(),
        DaemonPlace::Ssh => {
            let host = known
                .server
                .as_ref()
                .map_or(&known.daemon.machine, |config| &config.host);
            format!("SSH {host}")
        }
    }
}

/// The line under a row's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowNote {
    /// The app tries the machine again then.
    RetryAt(Instant),
    /// The app has tried this many times since the machine last answered.
    Attempt(u32),
    /// The daemon runs in the app, and its agents with it.
    StopsWithApp,
}

impl RowNote {
    fn of(known: &MachineState) -> Option<Self> {
        match &known.connection {
            ConnectionState::Disconnected { retry_at, .. } => Some(Self::RetryAt(*retry_at)),
            ConnectionState::Connecting { attempt } if *attempt > 1 => {
                Some(Self::Attempt(*attempt))
            }
            ConnectionState::Connected => {
                (known.daemon.place == DaemonPlace::InApp).then_some(Self::StopsWithApp)
            }
            ConnectionState::Connecting { .. } | ConnectionState::NoDaemon(_) => None,
        }
    }

    /// Its text at `now`, which a countdown needs.
    pub fn text(self, now: Instant) -> String {
        match self {
            Self::RetryAt(retry_at) => format!(
                "Retrying in {}.",
                format::duration(retry_at.saturating_duration_since(now))
            ),
            Self::Attempt(attempt) => format!("Attempt {attempt}."),
            Self::StopsWithApp => "Its agents stop when the app closes.".to_owned(),
        }
    }
}

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

/// Shows the form for a new server, from "Add a server".
pub fn open_add_form(state: &mut AppState) {
    with_view(state, |view| {
        view.adding = true;
        view.menu = None;
    });
}

/// Closes the form for a new server, and empties it.
pub fn close_add_form(state: &mut AppState) {
    with_view(state, |view| {
        *view = MachinesView {
            editing: view.editing.take(),
            ..MachinesView::default()
        };
    });
}

/// Opens a server's ⋯ menu at `x`, `y`, or closes it when it is open already.
pub fn toggle_menu(state: &mut AppState, machine: MachineId, x: i32, y: i32) {
    with_view(state, |view| {
        view.menu = match view.menu {
            Some(open) if open.machine == machine => None,
            Some(_) | None => Some(MachineMenu { machine, x, y }),
        };
    });
}

pub fn close_menu(state: &mut AppState) {
    with_view(state, |view| view.menu = None);
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

/// Whether the server's Edit button shows: a server the config names, whose update is not under
/// way. An update runs with the config it started with, and its cancel must reach that server.
pub fn can_edit(known: &MachineState) -> bool {
    let updating = matches!(
        known.update,
        Some(DaemonUpdate::Running(_) | DaemonUpdate::Reconnecting(_))
    );
    known.server.is_some() && !updating
}

/// Opens the server's form in its row, filled from its config.
pub fn start_edit(state: &mut AppState, machine: MachineId) {
    let Some(config) = state
        .machines
        .others
        .get(&machine)
        .filter(|known| can_edit(known))
        .and_then(|known| known.server.clone())
    else {
        return;
    };
    with_view(state, |view| {
        view.menu = None;
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
    // A name keys the preferences, and the interface shows another one for this computer.
    let taken = state.machines.all().any(|(machine, known)| {
        Some(machine) != editing
            && (known.daemon.machine == name || MachineName::new(machine, known).name == name)
    });
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
    state.ui.dialog.take_if(|dialog| {
        matches!(dialog, Dialog::DaemonUpdate(shown) | Dialog::ConfirmUpdate(shown) if *shown == machine)
    });
    with_view(state, |view| {
        view.menu.take_if(|menu| menu.machine == machine);
        view.editing.take_if(|edit| edit.machine == machine);
    });
    state.fix_selection();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::update_offer::{GapKind, VersionGap};
    use crate::state::update_plan::{ServerPlan, UpdatePlan};
    use crate::state::{AgentEntry, MachineState};
    use cockpit_protocol::{AccountId, AgentStatus, BuildCommit, PROTOCOL_VERSION};

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

    fn devbox_plan() -> ServerPlan {
        ServerPlan {
            host: "dev@devbox".to_owned(),
            remote_command: "/opt/agents-cockpit".to_owned(),
            source_dir: None,
            commit: BuildCommit::new("3f9c2e1a7b4d5c6e".to_owned()),
        }
    }

    /// Makes the server at `devbox` refuse the app for an older protocol.
    fn refuses_as_older(state: &mut AppState, devbox: MachineId) {
        if let Some(known) = state.machines.others.get_mut(&devbox) {
            known.version_gap = Some(VersionGap {
                kind: GapKind::OlderProtocol,
                details: "protocol 3, the app speaks 4".to_owned(),
                daemon: None,
                protocol: PROTOCOL_VERSION - 1,
            });
            known.update_plan = Some(UpdatePlan::Server(devbox_plan()));
        }
    }

    #[test]
    fn a_server_whose_update_runs_cannot_be_edited() {
        let mut state = state_with(Vec::new());
        server(&mut state, 1, "devbox");
        let devbox = MachineId::new(1);
        refuses_as_older(&mut state, devbox);
        assert!(can_edit(&state.machines.others[&devbox]));

        daemon_update::start(&mut state, devbox);
        open_machines(&mut state);
        start_edit(&mut state, devbox);

        assert!(!can_edit(&state.machines.others[&devbox]));
        assert!(matches!(&state.ui.dialog, Some(Dialog::Machines(view)) if view.editing.is_none()));
        daemon_update::finished(&mut state, devbox, Some(1));
        assert!(
            can_edit(&state.machines.others[&devbox]),
            "a failed update allows an edit"
        );

        daemon_update::start(&mut state, devbox);
        daemon_update::finished(&mut state, devbox, Some(0));
        assert!(
            !can_edit(&state.machines.others[&devbox]),
            "an update that waits for the new daemon still holds its config"
        );
    }

    #[test]
    fn update_again_after_an_edit_runs_with_the_new_checkout() {
        let mut state = state_with(Vec::new());
        server(&mut state, 1, "devbox");
        let devbox = MachineId::new(1);
        refuses_as_older(&mut state, devbox);
        let source_dir = |plan: Option<UpdatePlan>| match plan {
            Some(UpdatePlan::Server(plan)) => plan.source_dir,
            other => panic!("expected a server's plan, got {other:?}"),
        };
        assert_eq!(
            daemon_update::start(&mut state, devbox).map(|run| run.plan),
            Some(UpdatePlan::Server(devbox_plan()))
        );
        daemon_update::finished(&mut state, devbox, Some(1));
        let config = MachineConfig {
            source_dir: Some("/src/agents-cockpit".to_owned()),
            ..state.machines.others[&devbox].server.clone().unwrap()
        };

        apply_edit(&mut state, devbox, &config);

        let again = daemon_update::start(&mut state, devbox);
        assert_eq!(
            source_dir(again.map(|run| run.plan)),
            Some("/src/agents-cockpit".to_owned())
        );
        assert_eq!(
            source_dir(state.machines.others[&devbox].update_plan.clone()),
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

    /// Local in the app with agent 1, devbox connected with agent 1 of its own, and gpu-box not
    /// connected, trying again in 8 seconds.
    fn dialog_state(now: Instant) -> AppState {
        let mut state = AppState::new(DaemonLabel::in_app());
        state.machines.this.connection = ConnectionState::Connected;
        state.machines.this.daemon_version = Some(ReleaseVersion::new("0.2.0".to_owned()));
        state.agents.insert(
            AgentId::new(1),
            AgentEntry::new(summary(1, AgentStatus::Idle)),
        );
        server(&mut state, 1, "devbox");
        server(&mut state, 2, "gpu-box");
        let devbox = MachineId::new(1);
        if let Some(known) = state.machines.others.get_mut(&devbox) {
            known.connection = ConnectionState::Connected;
            known.daemon_version = Some(ReleaseVersion::new("0.2.0".to_owned()));
        }
        if let Some(known) = state.machines.others.get_mut(&MachineId::new(2)) {
            known.connection = ConnectionState::Disconnected {
                error: "ssh: connection refused".to_owned(),
                retry_at: now + std::time::Duration::from_secs(8),
            };
            known.daemon_version = Some(ReleaseVersion::new("0.1.0".to_owned()));
        }
        let mut remote = summary(1, AgentStatus::Working);
        remote.id = devbox.app_agent(AgentId::new(1));
        state.agents.insert(remote.id, AgentEntry::new(remote));
        state
    }

    #[test]
    fn a_row_says_where_the_daemon_runs_and_how_many_agents_it_has() {
        let now = Instant::now();
        let state = dialog_state(now);

        let shown: Vec<(String, bool, String, usize)> = rows(&state)
            .into_iter()
            .map(|row| (row.name.name, row.name.local, row.place, row.agents))
            .collect();

        assert_eq!(
            shown,
            [
                ("Local".to_owned(), true, "In this app".to_owned(), 1),
                ("devbox".to_owned(), false, "SSH dev@devbox".to_owned(), 1),
                ("gpu-box".to_owned(), false, "SSH dev@gpu-box".to_owned(), 0),
            ]
        );
    }

    #[test]
    fn a_row_shows_its_status_its_version_while_connected_and_the_action_its_line_offers() {
        let now = Instant::now();
        let state = dialog_state(now);

        let rows = rows(&state);

        let chips: Vec<&str> = rows.iter().map(|row| row.chip).collect();
        let versions: Vec<Option<String>> = rows
            .iter()
            .map(|row| row.version.as_ref().map(ToString::to_string))
            .collect();
        let actions: Vec<Option<StatusAction>> = rows.iter().map(|row| row.action).collect();
        let notes: Vec<Option<String>> = rows
            .iter()
            .map(|row| row.note.map(|note| note.text(now)))
            .collect();
        assert_eq!(chips, ["Connected", "Connected", "Not connected"]);
        assert_eq!(
            versions,
            [Some("0.2.0".to_owned()), Some("0.2.0".to_owned()), None]
        );
        assert_eq!(actions, [None, None, Some(StatusAction::Retry)]);
        assert_eq!(
            notes,
            [
                Some("Its agents stop when the app closes.".to_owned()),
                None,
                Some("Retrying in 8 s.".to_owned()),
            ]
        );
    }

    #[test]
    fn only_a_server_has_a_menu_and_it_copies_its_ssh_host() {
        let now = Instant::now();
        let mut state = dialog_state(now);
        state.machines.this = MachineState::new(DaemonLabel::ssh("build-box".to_owned()));

        let menus: Vec<Option<String>> = rows(&state)
            .into_iter()
            .map(|row| row.server.map(|server| server.host))
            .collect();

        assert_eq!(
            menus,
            [
                None,
                Some("dev@devbox".to_owned()),
                Some("dev@gpu-box".to_owned())
            ]
        );
    }

    #[test]
    fn a_menu_opens_on_one_server_at_a_time_and_its_edit_closes_it() {
        let mut state = dialog_state(Instant::now());
        open_machines(&mut state);
        let menu = |state: &AppState| match &state.ui.dialog {
            Some(Dialog::Machines(view)) => view.menu.map(|menu| menu.machine),
            _ => None,
        };

        toggle_menu(&mut state, MachineId::new(1), 600, 300);
        let first = menu(&state);
        toggle_menu(&mut state, MachineId::new(2), 600, 360);
        let second = menu(&state);
        start_edit(&mut state, MachineId::new(2));

        assert_eq!(first, Some(MachineId::new(1)));
        assert_eq!(second, Some(MachineId::new(2)));
        assert_eq!(menu(&state), None);
        assert!(state.ui.close_overlays(), "Esc closes the form");
        assert!(matches!(&state.ui.dialog, Some(Dialog::Machines(_))));
    }

    #[test]
    fn the_add_form_opens_from_its_button_and_esc_closes_it_before_the_dialog() {
        let mut state = state_with(Vec::new());
        typed(&mut state, "lab", "dev@lab", "");
        open_add_form(&mut state);
        let adding = matches!(&state.ui.dialog, Some(Dialog::Machines(view)) if view.adding);

        let first_escape = state.ui.close_overlays();

        assert!(adding);
        assert!(first_escape);
        assert!(matches!(&state.ui.dialog, Some(Dialog::Machines(view)) if !view.adding));
        state.ui.close_overlays();
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn a_server_cannot_take_the_name_the_interface_gives_this_computer() {
        let mut state = AppState::new(DaemonLabel::in_app());

        typed(&mut state, THIS_COMPUTER, "dev@lab", "/opt/agents-cockpit");

        assert_eq!(add(&mut state), None);
        assert!(error(&state).is_some());
    }

    #[test]
    fn a_removed_server_takes_its_menu_and_its_form_with_it() {
        let mut state = dialog_state(Instant::now());
        open_machines(&mut state);
        start_edit(&mut state, MachineId::new(2));
        toggle_menu(&mut state, MachineId::new(2), 600, 360);

        forget(&mut state, &mut Drafts::default(), MachineId::new(2));

        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::Machines(view)) if view.menu.is_none() && view.editing.is_none()
        ));
    }
}
