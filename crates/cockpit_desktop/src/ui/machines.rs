//! The "Machines" dialog: this computer and the servers the app reaches, with a form that adds
//! a server and a button that removes one.

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::daemon_label::DaemonPlace;
use crate::format;
use crate::machine::MachineId;
use crate::state::machines::{self, MachineChange};
use crate::state::{AppState, ConnectionState, MachinesView};
use crate::ui::icons::{Icon, IconKind};

/// A machine's row: what the dialog says of it.
#[derive(Debug, Clone, PartialEq)]
struct MachineRow {
    machine: MachineId,
    name: String,
    detail: String,
    /// Why it does not answer.
    trouble: Option<String>,
    agents: usize,
}

fn rows(state: &AppState) -> Vec<MachineRow> {
    let this = (MachineId::THIS, &state.machines.this);
    std::iter::once(this)
        .chain(state.machines.others.iter().map(|(id, known)| (*id, known)))
        .map(|(machine, known)| {
            let this_computer = machine == MachineId::THIS;
            let name = if this_computer && known.daemon.place != DaemonPlace::Ssh {
                "This computer".to_owned()
            } else {
                known.daemon.machine.clone()
            };
            let detail = match known.daemon.place {
                DaemonPlace::InApp => "Agents run in this app. They stop when it closes.",
                DaemonPlace::Service => "Agents run in the background service here.",
                DaemonPlace::Socket => "Agents run in the daemon on this computer.",
                DaemonPlace::Ssh => "Over SSH",
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
                detail: detail.to_owned(),
                trouble,
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
                    "The app reaches each machine at once. Its agents show in a panel of their own in the sidebar."
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
        agents,
    } = row;
    let removable = machine != MachineId::THIS;
    let agents_text = format::count(agents, "agent", "agents");
    rsx! {
        div { class: if trouble.is_some() { "machine-row down" } else { "machine-row" },
            div { class: "machine-row-main",
                span { class: "machine-row-name", "{name}" }
                span { class: "machine-row-detail", "{detail} · {agents_text}" }
                if let Some(trouble) = &trouble {
                    span { class: "machine-row-detail", "Not connected: {trouble}" }
                }
            }
            if trouble.is_some() {
                button {
                    class: "button small",
                    onclick: move |_| cockpit.change_machines(MachineChange::Retry(machine)),
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
