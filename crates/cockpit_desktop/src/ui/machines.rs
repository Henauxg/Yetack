//! The "Machines" dialog: a row per machine, Local first, with its status, its version and what to
//! do about it, a ⋯ menu on each server's row that edits, copies or removes it, and the form that
//! adds a server.

use std::cmp::Ordering;
use std::rc::Rc;
use std::time::Instant;

use cockpit_daemon::PROGRAM;
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::machine::MachineId;
use crate::state::daemon_probe::{APP_BIN_FOLDER, LinuxCpu, UNIT_FILE};
use crate::state::machine_status::StatusAction;
use crate::state::machines::{
    self, MachineChange, MachineName, MachineRow, RowNote, RowVersion, ServerRow, UpdateLine,
    VersionCell,
};
use crate::state::server_setup::{
    self, BootStart, InstallReport, InstallStep, ServerSetup, SetupStage, Starter,
};
use crate::state::update_offer::ProgramBuild;
use crate::state::update_plan::SendProgress;
use crate::state::{AppState, Dialog, MachineEdit, MachineMenu, MachinesView, daemon_update};
use crate::ui::accounts::CommandBox;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::menu::{FlatMenu, FlatRow};
use crate::ui::presence::use_presence;
use crate::{bridge, format};

/// Between the parts of a machine row's detail line.
const DETAIL_SEPARATOR: &str = " · ";

/// The class of the releases in the version cell when the commits show under them.
const RELEASE_LINE: &str = "version-release";

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
        StatusAction::StartDaemon => cockpit.start_daemon(machine),
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
                if let Some(version) = version {
                    VersionCellView { version }
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

/// The releases over the commits, in a grid whose columns line up. A screen reader reads the
/// builds as one text instead.
#[component]
fn VersionCellView(version: RowVersion) -> Element {
    let grid = match version.cell() {
        VersionCell::Runs { release, commit } => rsx! {
            span { class: "version-grid runs", aria_hidden: true,
                span { class: if commit.is_some() { RELEASE_LINE }, "{release}" }
                if let Some(commit) = &commit {
                    span { class: "version-commit", "{commit}" }
                }
            }
        },
        VersionCell::Update { releases, commits } => rsx! {
            span { class: "version-grid", aria_hidden: true,
                UpdateLineView { line: releases, small: commits.is_some() }
                if let Some(commits) = commits {
                    UpdateLineView { line: commits, small: false }
                }
            }
        },
    };
    rsx! {
        span { class: "visually-hidden", "{version}" }
        {grid}
    }
}

/// The three cells of a line of an update: the daemon's side, an arrow, then this app's. `small`
/// is for the releases above the commits.
#[component]
fn UpdateLineView(line: UpdateLine, small: bool) -> Element {
    let UpdateLine { from, to } = line;
    let (line_class, arrow) = if small { (RELEASE_LINE, 10) } else { ("", 12) };
    rsx! {
        span { class: "version-from {line_class}", if let Some(from) = from { "{from}" } }
        Icon { kind: IconKind::ArrowRight, size: arrow }
        span { class: "version-to {line_class}", if let Some(to) = to { "{to}" } }
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
    let setup = cockpit.state.read().machines.setup.clone();
    if let Some(setup) = setup {
        return rsx! {
            div {
                key: "{shown.opening}",
                class: if shown.leaving { "machine-add setup leaving" } else { "machine-add setup" },
                inert: shown.leaving.then_some(true),
                role: "group",
                aria_label: "Set up the server",
                SetupPanel { setup }
            }
        };
    }
    let add = move |_| cockpit.add_server();
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
                label { class: "field-label",
                    "Path of {PROGRAM} on the server"
                    span { class: "optional", "optional" }
                }
                input {
                    class: "text-input mono",
                    r#type: "text",
                    spellcheck: false,
                    placeholder: "Found, or installed by the app",
                    value: "{view.remote_command}",
                    oninput: type_into(|view| &mut view.remote_command),
                }
            }
            if let Some(error) = &view.error {
                span { class: "form-error", role: "alert", "{error}" }
            }
            div { class: "machine-add-actions",
                span { class: "hint",
                    "The app connects with your SSH keys, as ssh does. When the server has no {PROGRAM}, the app offers to install it."
                }
                div { class: "machine-edit-actions",
                    button { class: "button small", onclick: move |_| cockpit.update(machines::close_add_form), "Cancel" }
                    button { class: "button primary small", onclick: add, "Add the server" }
                }
            }
        }
    }
}

/// One step of a setup: done, under way, failed, waiting for the user, or still to come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Done,
    Now,
    Failed,
    Warn,
    Later,
}

#[component]
fn SetupStepRow(mark: Mark, title: String, detail: Option<String>) -> Element {
    let (class, icon) = match mark {
        Mark::Done => ("done", rsx! { Icon { kind: IconKind::Check, size: 14 } }),
        Mark::Now => (
            "now",
            rsx! { span { class: "spinner", role: "status", aria_label: "In progress" } },
        ),
        Mark::Failed => ("failed", rsx! { Icon { kind: IconKind::Close, size: 14 } }),
        Mark::Warn => ("warn", rsx! { Icon { kind: IconKind::Warning, size: 14 } }),
        Mark::Later => ("", rsx! { Icon { kind: IconKind::Circle, size: 12 } }),
    };
    rsx! {
        li { class,
            span { class: "update-mark", {icon} }
            span { class: "step-text",
                "{title}"
                if let Some(detail) = detail {
                    span { class: "step-detail",
                        span { "{detail}" }
                    }
                }
            }
        }
    }
}

/// How the setup's current step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CurrentStep {
    Runs,
    Failed,
}

/// The mark of `step` while `current` stands as `standing` says.
fn mark_of(step: InstallStep, current: InstallStep, standing: CurrentStep) -> Mark {
    match (step.cmp(&current), standing) {
        (Ordering::Less, _) => Mark::Done,
        (Ordering::Equal, CurrentStep::Runs) => Mark::Now,
        (Ordering::Equal, CurrentStep::Failed) => Mark::Failed,
        (Ordering::Greater, _) => Mark::Later,
    }
}

/// A new server's setup, in the form's place: what the app found, its offer to install, the
/// install's steps, then what the daemon still needs.
#[component]
fn SetupPanel(setup: ServerSetup) -> Element {
    let cockpit: Cockpit = use_context();
    let name = setup.config.name.clone();
    let host = setup.config.host.clone();
    let release = ProgramBuild::this_program().version.to_string();
    let close = move |_| cockpit.update(server_setup::close);
    let done = move |_| {
        cockpit.update(|state| {
            server_setup::close(state);
            machines::close_add_form(state);
        });
    };
    let stage: &'static str = (&setup.stage).into();
    let content = match setup.stage {
        SetupStage::Looking => rsx! {
            SetupHead { title: format!("Set up {name}"), lead: format!("SSH {host}") }
            ol { class: "update-steps",
                SetupStepRow { mark: Mark::Now, title: format!("Reach {name}"), detail: None }
                SetupStepRow { mark: Mark::Later, title: format!("Look for {PROGRAM}"), detail: None }
            }
            div { class: "machine-add-actions",
                span {}
                div { class: "machine-edit-actions",
                    button { class: "button small", onclick: close, "Cancel" }
                }
            }
        },
        SetupStage::Offer { system } => rsx! {
            SetupHead { title: format!("Set up {name}"), lead: format!("SSH {host}") }
            ol { class: "update-steps",
                SetupStepRow { mark: Mark::Done, title: format!("Reach {name}"), detail: Some(system) }
                SetupStepRow { mark: Mark::Done, title: format!("Look for {PROGRAM}"), detail: Some("None installed".to_owned()) }
            }
            div { class: "setup-offer",
                p {
                    b { "Install {PROGRAM} {release} on {name}?" }
                    " The app writes these files on {name}, and never runs "
                    code { "sudo" }
                    ":"
                }
                ul { class: "setup-files mono",
                    li { "{APP_BIN_FOLDER}/{PROGRAM}" }
                    li { "{UNIT_FILE}" }
                }
            }
            div { class: "machine-add-actions",
                span {}
                div { class: "machine-edit-actions",
                    button { class: "button small", onclick: close, "Cancel" }
                    button {
                        class: "button primary small",
                        onclick: move |_| cockpit.install_server(),
                        "Install on {name}"
                    }
                }
            }
        },
        SetupStage::CannotInstall { system } => {
            let why = if LinuxCpu::of_system(&system).is_some() {
                format!("This app carries no daemon for {system} to install.")
            } else {
                use strum::VariantArray;

                let cpus: Vec<&str> = LinuxCpu::VARIANTS.iter().map(|cpu| cpu.word()).collect();
                format!(
                    "{system}: the release builds the daemon for Linux on {} only.",
                    cpus.join(" and ")
                )
            };
            rsx! {
                SetupHead { title: format!("{name} needs a build from source"), lead: format!("SSH {host}") }
                ol { class: "update-steps",
                    SetupStepRow { mark: Mark::Failed, title: format!("Reach {name}"), detail: Some(why) }
                }
                p { class: "setup-note",
                    "Build {PROGRAM} on {name} as the README says, then give its path in the form."
                }
                div { class: "machine-add-actions",
                    span {}
                    div { class: "machine-edit-actions",
                        button { class: "button primary small", onclick: close, "Back to the form" }
                    }
                }
            }
        }
        SetupStage::Installing { step, sent } => {
            let sent_detail = sent.map(SendProgress::detail);
            rsx! {
                SetupHead {
                    title: format!("Installing on {name}"),
                    lead: "Closing this dialog does not stop the install: the server joins the app once its daemon runs.".to_owned(),
                }
                ol { class: "update-steps",
                    SetupStepRow { mark: Mark::Done, title: format!("Reach {name}"), detail: None }
                    SetupStepRow {
                        mark: mark_of(InstallStep::Send, step, CurrentStep::Runs),
                        title: format!("Send {release} to {name}"),
                        detail: sent_detail,
                    }
                    SetupStepRow { mark: mark_of(InstallStep::Check, step, CurrentStep::Runs), title: format!("Check it on {name}"), detail: None }
                    SetupStepRow { mark: mark_of(InstallStep::Start, step, CurrentStep::Runs), title: "Start the service", detail: None }
                    SetupStepRow { mark: Mark::Later, title: "Keep it running after you log out", detail: None }
                    SetupStepRow { mark: Mark::Later, title: "Find Claude Code", detail: None }
                }
            }
        }
        SetupStage::Installed(report) => rsx! {
            InstalledReport { name, release, report: report.clone() }
            div { class: "machine-add-actions",
                match &report.failure {
                    Some(failure) => rsx! {
                        span { key: "{failure}", class: "form-error setup-failure", role: "alert", "{failure}" }
                    },
                    None => rsx! { span {} },
                }
                div { class: "machine-edit-actions",
                    button {
                        class: "button small",
                        disabled: report.checking,
                        onclick: move |_| cockpit.check_server_again(),
                        if report.checking { "Checking…" } else { "Check again" }
                    }
                    button { class: "button primary small", onclick: done, "Done" }
                }
            }
        },
        SetupStage::Failed { step, message } => rsx! {
            SetupHead { title: format!("The setup of {name} failed"), lead: format!("SSH {host}") }
            ol { class: "update-steps",
                SetupStepRow {
                    mark: mark_of(InstallStep::Reach, step, CurrentStep::Failed),
                    title: format!("Reach {name}"),
                    detail: (step == InstallStep::Reach).then(|| message.clone()),
                }
                if step > InstallStep::Reach {
                    SetupStepRow {
                        mark: mark_of(InstallStep::Send, step, CurrentStep::Failed),
                        title: format!("Send {release} to {name}"),
                        detail: (step == InstallStep::Send).then(|| message.clone()),
                    }
                    SetupStepRow {
                        mark: mark_of(InstallStep::Check, step, CurrentStep::Failed),
                        title: format!("Check it on {name}"),
                        detail: (step == InstallStep::Check).then(|| message.clone()),
                    }
                    SetupStepRow {
                        mark: mark_of(InstallStep::Start, step, CurrentStep::Failed),
                        title: "Start the service",
                        detail: (step == InstallStep::Start).then(|| message.clone()),
                    }
                }
            }
            div { class: "machine-add-actions",
                span {}
                div { class: "machine-edit-actions",
                    button { class: "button small", onclick: close, "Back to the form" }
                    button {
                        class: "button primary small",
                        onclick: move |_| cockpit.retry_server_setup(),
                        "Try again"
                    }
                }
            }
        },
    };
    // A list of one, keyed by the stage: each stage's content comes in new, and fades in.
    let keyed = std::iter::once(rsx! {
        div { key: "{stage}", class: "setup-stage", {content} }
    });
    rsx! {
        {keyed}
    }
}

#[component]
fn SetupHead(title: String, lead: String) -> Element {
    rsx! {
        div { class: "setup-head",
            span { class: "setup-title", "{title}" }
            span { class: "setup-lead", "{lead}" }
        }
    }
}

/// The daemon runs: what else it needs, with the command that fixes each need on the server.
#[component]
fn InstalledReport(name: String, release: String, report: InstallReport) -> Element {
    let left = report.left_to_do();
    let title = match left {
        0 => format!("{name} is set up"),
        1 => format!("{name} is set up, with one thing left to do"),
        _ => format!("{name} is set up, with two things left to do"),
    };
    let lead = match (left, report.left_on_server()) {
        (0, _) => format!("{name}'s agents work now."),
        (_, 0) => format!(
            "{name}'s agents work now. Add the start at boot, so that they work again after {name} restarts."
        ),
        _ => format!("{name}'s agents work now. Fix these on {name}, then check again."),
    };
    rsx! {
        SetupHead { title, lead }
        ol { class: "update-steps",
            SetupStepRow { mark: Mark::Done, title: format!("Send and check {release}"), detail: None }
            match report.starter {
                Starter::Systemd => rsx! {
                    SetupStepRow { mark: Mark::Done, title: "Start the service", detail: Some("Running".to_owned()) }
                    if report.linger == Some(true) {
                        SetupStepRow { mark: Mark::Done, title: "Keep it running after you log out", detail: Some("Linger is on".to_owned()) }
                    } else {
                        SetupStepRow {
                            mark: Mark::Warn,
                            title: "Keep it running after you log out",
                            detail: Some(format!("Linger is off: systemd stops {name}'s daemon, and its agents, when you log out of {name}. Run this on {name} (some servers need sudo before it):")),
                        }
                    }
                },
                Starter::Alone => rsx! {
                    SetupStepRow {
                        mark: Mark::Done,
                        title: "Start the daemon",
                        detail: Some(format!("{name} runs no systemd user session, so the daemon runs on its own, apart from your SSH session: it keeps running after you log out.")),
                    }
                    BootStartRow { name: name.clone(), report: report.clone() }
                },
            }
        }
        if report.starter == Starter::Systemd && report.linger != Some(true) {
            div { class: "step-command",
                CommandBox { command: "loginctl enable-linger".to_owned() }
            }
        }
        if report.starter == Starter::Alone {
            BootStartAction { report: report.clone() }
        }
        ol { class: "update-steps",
            match &report.claude {
                Some(path) => rsx! {
                    SetupStepRow { mark: Mark::Done, title: "Find Claude Code", detail: Some(path.clone()) }
                },
                None => rsx! {
                    SetupStepRow {
                        mark: Mark::Warn,
                        title: "Find Claude Code",
                        detail: Some(format!("The daemon finds no claude on its PATH. Install Claude Code on {name} and sign in with claude.")),
                    }
                },
            }
        }
    }
}

/// Without systemd: whether something starts the daemon again when the server restarts.
#[component]
fn BootStartRow(name: String, report: InstallReport) -> Element {
    let title = format!("Start it again when {name} restarts");
    let (mark, detail) = match report.boot {
        Some(BootStart::Yes) => (Mark::Done, "Your crontab starts it".to_owned()),
        Some(BootStart::Unavailable) => (
            Mark::Warn,
            format!(
                "{name} has no crontab, so nothing starts the daemon after a restart. Run this command when {name} starts, from its init system:"
            ),
        ),
        Some(BootStart::No) | None => (
            Mark::Warn,
            format!(
                "Nothing starts the daemon after {name} restarts. The app can add this line to your crontab on {name}, which cron runs at each start:"
            ),
        ),
    };
    rsx! {
        SetupStepRow { mark, title, detail: Some(detail) }
    }
}

/// The command `BootStartRow` names, and the button that adds the crontab line. The app adds it
/// only on that click.
#[component]
fn BootStartAction(report: InstallReport) -> Element {
    let cockpit: Cockpit = use_context();
    match report.boot {
        Some(BootStart::Yes) => rsx! {},
        Some(BootStart::Unavailable) => rsx! {
            if let Some(command) = report.start_command.clone() {
                div { class: "step-command",
                    CommandBox { command }
                }
            }
        },
        Some(BootStart::No) | None => rsx! {
            if let Some(line) = report.cron_line.clone() {
                div { class: "step-command",
                    CommandBox { command: line }
                    button {
                        class: "button small",
                        disabled: report.checking,
                        onclick: move |_| cockpit.add_boot_start_on_server(),
                        if report.checking { "Adding…" } else { "Add it to my crontab" }
                    }
                }
            }
        },
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
                label { class: "field-label", "Path of {PROGRAM} on the server" }
                input {
                    class: "text-input mono",
                    r#type: "text",
                    spellcheck: false,
                    placeholder: "/home/you/.local/bin/{PROGRAM}",
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
