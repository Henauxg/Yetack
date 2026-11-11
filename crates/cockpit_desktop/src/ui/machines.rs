//! The "Machines" dialog: this computer and the servers the app reaches, with a form that adds
//! a server and a button that removes one.

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::daemon_label::DaemonPlace;
use crate::format;
use crate::machine::MachineId;
use crate::state::daemon_update::{self, UpdateLine};
use crate::state::machines::{self, MachineChange};
use crate::state::{AppState, ConnectionState, MachinesView};
use crate::ui::icons::{Icon, IconKind};
use crate::update_script::APP_VERSION;

/// What the sidebar and the dialogs call this computer, unless its daemon runs over SSH.
pub const THIS_COMPUTER: &str = "This computer";

/// Between the parts of a machine row's detail line.
const DETAIL_SEPARATOR: &str = " · ";

/// A machine a dialog offers to pick.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineChoice {
    pub machine: MachineId,
    pub name: String,
    /// A laptop for this computer, a server otherwise, as in the sidebar's machine headers.
    pub icon: IconKind,
    pub chosen: bool,
    pub answers: bool,
}

/// The machines a dialog offers, this computer first. None while the app reaches this computer
/// alone.
pub fn machine_choices(state: &AppState, chosen: MachineId) -> Vec<MachineChoice> {
    if state.machines.others.is_empty() {
        return Vec::new();
    }
    let this = (MachineId::THIS, &state.machines.this);
    std::iter::once(this)
        .chain(state.machines.others.iter().map(|(id, known)| (*id, known)))
        .map(|(machine, known)| {
            let this_computer =
                machine == MachineId::THIS && known.daemon.place != DaemonPlace::Ssh;
            let (name, icon) = if this_computer {
                (THIS_COMPUTER.to_owned(), IconKind::Laptop)
            } else {
                (known.daemon.machine.clone(), IconKind::Server)
            };
            MachineChoice {
                machine,
                name,
                icon,
                chosen: machine == chosen,
                answers: known.is_connected(),
            }
        })
        .collect()
}

/// A machine's row: what the dialog says of it.
#[derive(Debug, Clone, PartialEq)]
struct MachineRow {
    machine: MachineId,
    name: String,
    detail: String,
    /// Why it does not answer.
    trouble: Option<String>,
    /// What it says of a daemon of another version, which replaces `trouble`.
    update: Option<UpdateLine>,
    agents: usize,
}

fn rows(state: &AppState) -> Vec<MachineRow> {
    let this = (MachineId::THIS, &state.machines.this);
    std::iter::once(this)
        .chain(state.machines.others.iter().map(|(id, known)| (*id, known)))
        .map(|(machine, known)| {
            let this_computer = machine == MachineId::THIS;
            let name = if this_computer && known.daemon.place != DaemonPlace::Ssh {
                THIS_COMPUTER.to_owned()
            } else {
                known.daemon.machine.clone()
            };
            let detail = match known.daemon.place {
                DaemonPlace::InApp => "Agents run in this app. They stop when it closes.",
                DaemonPlace::Service => "Agents run in the background service here.",
                DaemonPlace::Socket => "Agents run in the daemon on this computer.",
                DaemonPlace::Ssh => "Over SSH",
            };
            // A version from an earlier connection may no longer be the one the server runs.
            let detail = match &known.daemon_version {
                Some(version) if known.is_connected() => {
                    format!("{detail}{DETAIL_SEPARATOR}version {version}")
                }
                Some(_) | None => detail.to_owned(),
            };
            let trouble = match &known.connection {
                ConnectionState::Disconnected { error, .. } => Some(error.clone()),
                ConnectionState::Connecting { .. }
                | ConnectionState::Connected
                | ConnectionState::NoDaemon(_) => None,
            };
            let agents = state
                .agents
                .keys()
                .filter(|agent| MachineId::of(**agent) == machine)
                .count();
            MachineRow {
                machine,
                name,
                detail,
                trouble,
                update: daemon_update::update_line(known),
                agents,
            }
        })
        .collect()
}

#[component]
pub fn MachinesDialog(view: MachinesView) -> Element {
    let cockpit: Cockpit = use_context();
    let rows = rows(&cockpit.state.read());
    let close = move |_| cockpit.update(|state| state.ui.close_dialog());
    let add = move |_| {
        let mut change = None;
        cockpit.update(|state| change = machines::add(state));
        if let Some(change) = change {
            cockpit.change_machines(change);
        }
    };
    let type_into = move |field: fn(&mut MachinesView) -> &mut String| {
        move |event: FormEvent| {
            let typed = event.value();
            cockpit.update(|state| machines::with_view(state, |view| *field(view) = typed));
        }
    };
    rsx! {
        div {
            class: "dialog accounts machines",
            role: "dialog",
            aria_label: "Machines",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "Machines" }
                button { class: "icon-button small", aria_label: "Close", onclick: close,
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            div { class: "dialog-body",
                span { class: "dialog-note",
                    "The app reaches each machine at once. Its agents show in a panel of their own in the sidebar. This app is version {APP_VERSION}."
                }
                div { class: "machine-rows",
                    for row in rows {
                        MachineRowView { key: "{row.name}", row }
                    }
                }
                div { class: "machine-add",
                    span { class: "field-label", "Add a server" }
                    div { class: "form-row",
                        div { class: "field-group",
                            label { class: "field-label", "SSH host" }
                            input {
                                class: "text-input mono",
                                r#type: "text",
                                spellcheck: false,
                                placeholder: "devbox, or dev@devbox",
                                value: "{view.host}",
                                oninput: type_into(|view| &mut view.host),
                            }
                        }
                        div { class: "field-group",
                            label { class: "field-label", "Name in the app" }
                            input {
                                class: "text-input",
                                r#type: "text",
                                spellcheck: false,
                                placeholder: "The host",
                                value: "{view.name}",
                                oninput: type_into(|view| &mut view.name),
                            }
                        }
                    }
                    div { class: "field-group",
                        label { class: "field-label", "Path of agents-cockpit on the server" }
                        input {
                            class: "text-input mono",
                            r#type: "text",
                            spellcheck: false,
                            placeholder: "/home/you/.cargo/bin/agents-cockpit",
                            value: "{view.remote_command}",
                            oninput: type_into(|view| &mut view.remote_command),
                        }
                    }
                    if let Some(error) = &view.error {
                        span { class: "form-error", role: "alert", "{error}" }
                    }
                    div { class: "machine-add-actions",
                        span { class: "hint", "The app connects with your SSH keys, as ssh does, and saves the server in its config." }
                        button { class: "button primary", onclick: add, "Add the server" }
                    }
                }
            }
        }
    }
}

#[component]
fn MachineRowView(row: MachineRow) -> Element {
    let cockpit: Cockpit = use_context();
    let MachineRow {
        machine,
        name,
        detail,
        trouble,
        update,
        agents,
    } = row;
    let removable = machine != MachineId::THIS;
    let agents_text = format::count(agents, "agent", "agents");
    rsx! {
        div { class: if trouble.is_some() || update.is_some() { "machine-row down" } else { "machine-row" },
            div { class: "machine-row-main",
                span { class: "machine-row-name", "{name}" }
                span { class: "machine-row-detail", "{detail}{DETAIL_SEPARATOR}{agents_text}" }
                if let Some(update) = update {
                    span { class: "machine-row-detail", "{update.text}" }
                } else if let Some(trouble) = &trouble {
                    span { class: "machine-row-detail", "Not connected: {trouble}" }
                }
            }
            if let Some(update) = update {
                button {
                    class: "button small",
                    onclick: move |_| cockpit.update(|state| daemon_update::open(state, machine)),
                    "{update.link}"
                }
            }
            if update.is_some_and(|update| update.down) || (update.is_none() && trouble.is_some()) {
                button {
                    class: "button small",
                    onclick: move |_| cockpit.retry_machine(machine),
                    "Retry now"
                }
            }
            if removable {
                button {
                    class: "button small",
                    title: "Forget the server. Its agents keep running there.",
                    onclick: move |_| cockpit.change_machines(MachineChange::Remove(machine)),
                    "Remove"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use cockpit_protocol::ReleaseVersion;

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::MachineState;

    #[test]
    fn a_machine_button_shows_a_laptop_for_this_computer_and_a_server_otherwise() {
        let choices = |this: DaemonLabel| -> Vec<(String, IconKind)> {
            let mut state = AppState::new(this);
            state.machines.others.insert(
                MachineId::new(1),
                MachineState::new(DaemonLabel::ssh("devbox".to_owned())),
            );
            machine_choices(&state, MachineId::THIS)
                .into_iter()
                .map(|choice| (choice.name, choice.icon))
                .collect()
        };

        assert_eq!(
            choices(DaemonLabel::in_app()),
            [
                (THIS_COMPUTER.to_owned(), IconKind::Laptop),
                ("devbox".to_owned(), IconKind::Server),
            ]
        );
        assert_eq!(
            choices(DaemonLabel::ssh("build-box".to_owned())),
            [
                ("build-box".to_owned(), IconKind::Server),
                ("devbox".to_owned(), IconKind::Server),
            ]
        );
    }

    #[test]
    fn a_row_shows_the_version_of_a_daemon_it_is_connected_to() {
        let mut state = AppState::new(DaemonLabel::in_app());
        let machine = |host: &str, connection: ConnectionState| {
            let mut known = MachineState::new(DaemonLabel::ssh(host.to_owned()));
            known.daemon_version = Some(ReleaseVersion::new("0.4.2".to_owned()));
            known.connection = connection;
            known
        };
        let gone = ConnectionState::Disconnected {
            error: "ssh: connection refused".to_owned(),
            retry_at: Instant::now(),
        };
        let machines = [
            machine("devbox", ConnectionState::Connected),
            machine("old-box", gone),
        ];
        for (id, known) in (1..).zip(machines) {
            state.machines.others.insert(MachineId::new(id), known);
        }

        let details: Vec<String> = rows(&state).into_iter().map(|row| row.detail).collect();

        assert_eq!(
            details,
            [
                "Agents run in this app. They stop when it closes.",
                "Over SSH · version 0.4.2",
                "Over SSH",
            ]
        );
    }
}
