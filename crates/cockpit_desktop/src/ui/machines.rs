//! The "Machines" dialog: a row per machine, Local first, with its status, its version and what to
//! do about it, a ⋯ menu on each server's row that edits, copies or removes it, and the form that
//! adds a server.

use std::rc::Rc;
use std::time::Instant;

use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::machine::MachineId;
use crate::state::machine_status::StatusAction;
use crate::state::machines::{
    self, MachineChange, MachineName, MachineRow, RowNote, RowVersion, ServerRow,
};
use crate::state::{AppState, Dialog, MachineEdit, MachineMenu, MachinesView, daemon_update};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::menu::{FlatMenu, FlatRow};
use crate::ui::presence::use_presence;
use crate::{bridge, format};

/// Between the parts of a machine row's detail line.
const DETAIL_SEPARATOR: &str = " · ";

/// The width of a server's menu in the style sheet (`.server-menu`): the menu opens with its right
/// edge under its button's.
const MENU_WIDTH: f64 = 220.0;
/// The room between the ⋯ button and the menu under it.
const MENU_GAP: f64 = 4.0;

/// A laptop for this computer, a server otherwise.
pub fn machine_icon(local: bool) -> IconKind {
    if local {
        IconKind::Laptop
    } else {
        IconKind::Server
    }
}

/// What a row's button, or a link of a machine's line in the sidebar, does.
pub fn run_action(cockpit: Cockpit, machine: MachineId, action: StatusAction) {
    match action {
        StatusAction::Update(_) => cockpit.update(|state| daemon_update::open(state, machine)),
        StatusAction::Retry => cockpit.retry_machine(machine),
        StatusAction::ChooseDaemon => {
            cockpit.update(|state| state.ui.dialog = Some(Dialog::NoDaemon));
        }
    }
}

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
    state
        .machines
        .all()
        .map(|(machine, known)| {
            let MachineName { name, local } = MachineName::new(machine, known);
            MachineChoice {
                machine,
                name,
                icon: machine_icon(local),
                chosen: machine == chosen,
                answers: known.is_connected(),
            }
        })
        .collect()
}

#[component]
pub fn MachinesDialog(view: MachinesView) -> Element {
    let cockpit: Cockpit = use_context();
    let rows = machines::rows(&cockpit.state.read());
    let close = move |_| cockpit.update(|state| state.ui.close_dialog());
    let menu_open = view.menu.is_some();
    let menu = view.menu.and_then(|menu| {
        let row = rows.iter().find(|row| row.machine == menu.machine)?;
        Some(OpenMenu {
            menu,
            server: row.server.clone()?,
        })
    });
    rsx! {
        div {
            class: "dialog accounts machines",
            role: "dialog",
            aria_label: "Machines",
            onclick: move |event| {
                event.stop_propagation();
                if menu_open {
                    cockpit.update(machines::close_menu);
                }
            },
            div { class: "dialog-head",
                h2 { "Machines" }
                button { class: "icon-button small", aria_label: "Close", onclick: close,
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            div { class: "dialog-body",
                div { class: "machine-table",
                    div { class: "machine-columns",
                        span {}
                        span { "Machine" }
                        span { "Status" }
                        span { "Version" }
                        span {}
                        span {}
                    }
                    for row in rows {
                        match view.editing.as_ref().filter(|edit| edit.machine == row.machine).cloned() {
                            Some(edit) => rsx! {
                                MachineEditView { key: "{row.machine.get()}", name: row.name.name.clone(), agents: row.agents, edit }
                            },
                            None => rsx! {
                                MachineRowView {
                                    key: "{row.machine.get()}",
                                    menu_open: view.menu.is_some_and(|menu| menu.machine == row.machine),
                                    row,
                                }
                            },
                        }
                    }
                }
                AddServer { view: view.clone() }
            }
            ServerMenu { open: menu }
        }
    }
}

#[component]
fn MachineRowView(row: MachineRow, menu_open: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let mut more = use_signal(|| None::<Rc<MountedData>>);
    let MachineRow {
        machine,
        name,
        place,
        agents,
        note,
        tone,
        chip,
        error,
        version,
        action,
        server,
    } = row;
    let agents_text = format::count(agents, "agent", "agents");
    let toggle_menu = move |event: MouseEvent| {
        // The dialog closes an open menu on the clicks it gets.
        event.stop_propagation();
        let Some(button) = more() else {
            return;
        };
        spawn(async move {
            // The row may have gone meanwhile: no menu then.
            let Ok(rect) = button.get_client_rect().await else {
                return;
            };
            // Whole pixels are enough to place a menu.
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a window's pixels fit in i32"
            )]
            let (x, y) = (
                (rect.max_x() - MENU_WIDTH).round() as i32,
                (rect.max_y() + MENU_GAP).round() as i32,
            );
            cockpit.update(|state| machines::toggle_menu(state, machine, x, y));
        });
    };
    rsx! {
        div { class: "machine-grid-row",
            span { class: "machine-cell-icon", Icon { kind: machine_icon(name.local), size: 18 } }
            div { class: "machine-cell-main",
                span { class: "machine-cell-name", "{name.name}" }
                span { class: "machine-cell-detail", "{place}{DETAIL_SEPARATOR}{agents_text}" }
                if let Some(note) = note {
                    RowNoteView { note }
                }
            }
            span { class: "machine-cell-state",
                span { class: "machine-chip {tone.class()}", title: error.unwrap_or_default(),
                    span { class: "chip-dot", aria_hidden: true }
                    "{chip}"
                }
            }
            span { class: "machine-cell-version mono",
                match version {
                    Some(RowVersion::Runs(release)) => rsx! { "{release}" },
                    Some(RowVersion::Update { from, to }) => rsx! {
                        if let Some(from) = from {
                            span { class: "version-from", "{from}" }
                        }
                        Icon { kind: IconKind::ArrowRight, size: 12 }
                        span { class: "version-to", "{to}" }
                    },
                    None => rsx! {},
                }
            }
            span { class: "machine-cell-action",
                if let Some(action) = action {
                    button {
                        class: if matches!(action, StatusAction::Update(_)) { "button small line-button primary" } else { "button small line-button" },
                        onclick: move |_| run_action(cockpit, machine, action),
                        "{action.button_label()}"
                    }
                }
            }
            if server.is_some() {
                button {
                    class: if menu_open { "icon-button small server-more active" } else { "icon-button small server-more" },
                    title: "More",
                    aria_label: "More for {name.name}",
                    aria_haspopup: "menu",
                    aria_expanded: menu_open,
                    onmounted: move |event| more.set(Some(event.data())),
                    onclick: toggle_menu,
                    Icon { kind: IconKind::Dots, size: 16 }
                }
            } else {
                span {}
            }
        }
    }
}

/// The only part of a row that reads the clock, so its tick renders this text alone.
#[component]
fn RowNoteView(note: RowNote) -> Element {
    let Clock(now) = use_context::<Clock>();
    // Reading the clock counts down to the next try every second.
    let _tick = now();
    rsx! {
        span { class: "machine-cell-note", "{note.text(Instant::now())}" }
    }
}

/// A server's open menu, and what it acts on.
#[derive(Debug, Clone, PartialEq)]
struct OpenMenu {
    menu: MachineMenu,
    server: ServerRow,
}

/// The rows of a server's menu, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerItem {
    Edit,
    CopyHost,
    Remove,
}

const SERVER_ITEMS: [ServerItem; 3] = [ServerItem::Edit, ServerItem::CopyHost, ServerItem::Remove];

impl ServerItem {
    fn row(self, server: &ServerRow) -> FlatRow {
        let (label, key) = match self {
            Self::Edit => ("Edit the server", 'e'),
            Self::CopyHost => ("Copy the SSH host", 'c'),
            Self::Remove => ("Remove from the app", 'r'),
        };
        let title = match self {
            Self::Edit if server.editable => "Change how the app reaches the server, or its name",
            Self::Edit => "Its update runs with the server's config: edit it once the update ends",
            Self::CopyHost => server.host.as_str(),
            Self::Remove => "Forget the server. Its agents keep running there.",
        };
        FlatRow {
            label,
            key,
            enabled: self != Self::Edit || server.editable,
            title: title.to_owned(),
            divided: self == Self::Remove,
            danger: self == Self::Remove,
        }
    }
}

#[component]
fn ServerMenu(open: Option<OpenMenu>) -> Element {
    let cockpit: Cockpit = use_context();
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    let OpenMenu { menu, server } = shown.value;
    let machine = menu.machine;
    // A menu opened on another server starts with nothing highlighted.
    let key = format!("{}-{}", shown.opening, machine.get());
    let rows: Vec<FlatRow> = SERVER_ITEMS.iter().map(|item| item.row(&server)).collect();
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            // A click in the menu stays in it: the dialog closes the menu on the clicks it gets.
            onclick: move |event| event.stop_propagation(),
            for key in [key] {
                FlatMenu {
                    key: "{key}",
                    x: menu.x,
                    y: menu.y,
                    rows: rows.clone(),
                    focus_back: None,
                    class: "server-menu",
                    on_pick: {
                        let host = server.host.clone();
                        move |index: usize| match SERVER_ITEMS.get(index) {
                            Some(ServerItem::Edit) => {
                                cockpit.update(|state| machines::start_edit(state, machine));
                            }
                            Some(ServerItem::CopyHost) => {
                                bridge::copy_to_clipboard(host.clone());
                                cockpit.update(machines::close_menu);
                            }
                            Some(ServerItem::Remove) => {
                                cockpit.update(machines::close_menu);
                                cockpit.change_machines(MachineChange::Remove(machine));
                            }
                            None => {}
                        }
                    },
                }
            }
        }
    }
}

/// "Add a server", which opens the form for a new server in its place.
#[component]
fn AddServer(view: MachinesView) -> Element {
    let cockpit: Cockpit = use_context();
    let Some(shown) = use_presence(view.adding.then_some(view)) else {
        return rsx! {
            button { class: "add-machine", onclick: move |_| cockpit.update(machines::open_add_form),
                Icon { kind: IconKind::Plus, size: 14 }
                "Add a server"
            }
        };
    };
    let view = shown.value;
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
            key: "{shown.opening}",
            class: if shown.leaving { "machine-add leaving" } else { "machine-add" },
            inert: shown.leaving.then_some(true),
            role: "group",
            aria_label: "Add a server",
            div { class: "form-row",
                div { class: "field-group",
                    label { class: "field-label", "SSH host" }
                    input {
                        class: "text-input mono",
                        r#type: "text",
                        spellcheck: false,
                        placeholder: "devbox, or dev@devbox",
                        value: "{view.host}",
                        onmounted: move |event| async move {
                            // The form may have closed already: nothing to focus then.
                            let _ = event.data().set_focus(true).await;
                        },
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
                    placeholder: "/home/you/.local/bin/agents-cockpit",
                    value: "{view.remote_command}",
                    oninput: type_into(|view| &mut view.remote_command),
                }
            }
            if let Some(error) = &view.error {
                span { class: "form-error", role: "alert", "{error}" }
            }
            div { class: "machine-add-actions",
                span { class: "hint", "The app connects with your SSH keys, as ssh does, and saves the server in its config." }
                div { class: "machine-edit-actions",
                    button { class: "button small", onclick: move |_| cockpit.update(machines::close_add_form), "Cancel" }
                    button { class: "button primary small", onclick: add, "Add the server" }
                }
            }
        }
    }
}

/// A server's row as its form, filled from its config. Save checks it as Add does and reconnects
/// to the server, Cancel leaves it as it was.
#[component]
fn MachineEditView(name: String, agents: usize, edit: MachineEdit) -> Element {
    let cockpit: Cockpit = use_context();
    let type_into = move |field: fn(&mut MachineEdit) -> &mut String| {
        move |event: FormEvent| {
            let typed = event.value();
            cockpit.update(|state| {
                machines::with_view(state, |view| {
                    if let Some(edit) = &mut view.editing {
                        *field(edit) = typed;
                    }
                });
            });
        }
    };
    let save = move |_| {
        let mut change = None;
        cockpit.update(|state| change = machines::save_edit(state));
        if let Some(change) = change {
            cockpit.change_machines(change);
        }
    };
    let agents_text = format::count(agents, "agent", "agents");
    rsx! {
        div { class: "machine-edit", role: "group", aria_label: "Edit {name}",
            div { class: "machine-edit-head",
                span { class: "machine-cell-name", "{name}" }
                span { class: "machine-cell-detail", "Over SSH{DETAIL_SEPARATOR}{agents_text}" }
            }
            div { class: "form-row",
                div { class: "field-group",
                    label { class: "field-label", "SSH host" }
                    input {
                        class: "text-input mono",
                        r#type: "text",
                        spellcheck: false,
                        placeholder: "devbox, or dev@devbox",
                        initial_value: "{edit.host}",
                        onmounted: move |event| async move {
                            // The form may have closed already: nothing to focus then.
                            let _ = event.data().set_focus(true).await;
                        },
                        oninput: type_into(|edit| &mut edit.host),
                    }
                }
                div { class: "field-group",
                    label { class: "field-label", "Name in the app" }
                    input {
                        class: "text-input",
                        r#type: "text",
                        spellcheck: false,
                        placeholder: "The host",
                        initial_value: "{edit.name}",
                        oninput: type_into(|edit| &mut edit.name),
                    }
                }
            }
            div { class: "field-group",
                label { class: "field-label", "Path of agents-cockpit on the server" }
                input {
                    class: "text-input mono",
                    r#type: "text",
                    spellcheck: false,
                    placeholder: "/home/you/.local/bin/agents-cockpit",
                    initial_value: "{edit.remote_command}",
                    oninput: type_into(|edit| &mut edit.remote_command),
                }
            }
            div { class: "field-group",
                label { class: "field-label",
                    "Source checkout for updates "
                    span { class: "field-optional", "(optional)" }
                }
                input {
                    class: "text-input mono",
                    r#type: "text",
                    spellcheck: false,
                    placeholder: "The update script's default",
                    initial_value: "{edit.source_dir}",
                    oninput: type_into(|edit| &mut edit.source_dir),
                }
            }
            if let Some(error) = &edit.error {
                span { class: "form-error", role: "alert", "{error}" }
            }
            div { class: "machine-add-actions",
                span { class: "hint", "Saving reconnects to the server. Its agents keep running there, and keep their colors and notifications under a new name." }
                div { class: "machine-edit-actions",
                    button { class: "button small", onclick: move |_| cockpit.update(machines::cancel_edit), "Cancel" }
                    button { class: "button primary small", onclick: save, "Save" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::MachineState;
    use crate::state::machines::THIS_COMPUTER;

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
    fn only_the_menu_of_a_server_whose_update_runs_cannot_edit_it() {
        let server = |editable| ServerRow {
            host: "dev@devbox".to_owned(),
            editable,
        };
        let enabled = |server: &ServerRow| -> Vec<bool> {
            SERVER_ITEMS
                .iter()
                .map(|item| item.row(server).enabled)
                .collect()
        };

        assert_eq!(enabled(&server(true)), [true, true, true]);
        assert_eq!(enabled(&server(false)), [false, true, true]);
    }
}
