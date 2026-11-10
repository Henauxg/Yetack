//! The agent switcher (Ctrl+K), the new agent dialog (Ctrl+N), the question before closing an
//! agent, the frame of the "Open file" dialog (Ctrl+O), and the full view of an image.

use std::path::{Path, PathBuf};

use cockpit_protocol::{
    AgentId, AgentStatus, Completeness, Effort, PastSession, PermissionMode, Timestamp,
};
use dioxus::html::geometry::ScreenPoint;
use dioxus::html::input_data::MouseButton;
use dioxus::html::point_interaction::InteractionLocation;
use dioxus::prelude::*;
use rfd::AsyncFileDialog;

use crate::bridge::{self, FOLDER_FIELD_ID, HIGHLIGHTED_CLASS};
use crate::connection::{Clock, Cockpit};
use crate::daemon_label::DaemonPlace;
use crate::format::{self, MODE_CHOICES, MODEL_CHOICES};
use crate::keymap::Command;
use crate::machine::MachineId;
use crate::path_style::{self, PathStyle};
use crate::state::accounts::AccountLabel;
use crate::state::actions::{Direction, StartField};
use crate::state::folder_field::{self, FolderRow};
use crate::state::{
    Dialog, FolderAction, FolderList, FormMenu, ImageView, Loadable, NewAgentForm, NewAgentTab,
    WorkingFolderForm, accounts, actions, working_folder,
};
use crate::ui::accounts::{self as accounts_view, AccountMini, AccountPicker};
use crate::ui::agent_map::AgentMapDialog;
use crate::ui::composer::mode_icon;
use crate::ui::daemon_update::DaemonUpdateScreens;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::machines::{MachinesDialog, machine_choices};
use crate::ui::no_daemon::NoDaemonCard;
use crate::ui::open_file::{self as open_file_view, OpenFileDialog, UP_ROW};
use crate::ui::presence::{Presence, Shown, use_presence};
use crate::ui::settings::SettingsDialog;
use crate::ui::zoom::ZoomImage;

/// Recent folders the new agent dialog offers.
const RECENT_FOLDERS: usize = 6;

/// The widths of the rows that stand for folders and sessions on their way.
const PLACEHOLDER_WIDTHS: [&str; RECENT_FOLDERS] = ["72%", "58%", "66%", "50%", "62%", "54%"];

/// The widths of the rows that stand for the folder field's folders on their way.
const FOLDER_LOADING_ROWS: [&str; 4] = ["34%", "22%", "46%", "28%"];

/// The new agent dialog's first message.
const PROMPT_FIELD_ID: &str = "new-agent-prompt";

/// The "Resume a session" tab's search field.
const SESSION_SEARCH_ID: &str = "new-agent-session-search";

/// What the menu buttons say when the form keeps Claude Code's default.
const DEFAULT_MODEL: &str = "Default model";
const DEFAULT_MODE: &str = "Default mode";

/// Where the pointer last was, for a list whose highlight both the keys and the pointer move.
/// WebKit also sends a mousemove when the list scrolls under a pointer that stays still. Only a
/// pointer that moved takes the highlight, or the keys could not scroll past it.
#[derive(Clone, Copy)]
pub struct PointerMoves(Signal<Option<ScreenPoint>>);

pub fn use_pointer_moves() -> PointerMoves {
    PointerMoves(use_signal(|| None))
}

impl PointerMoves {
    pub fn moved(mut self, event: &MouseEvent) -> bool {
        let at = Some(event.screen_coordinates());
        if *self.0.peek() == at {
            return false;
        }
        self.0.set(at);
        true
    }
}

#[component]
pub fn Dialogs() -> Element {
    let cockpit: Cockpit = use_context();
    // A click beside the dialog closes it only when its press reached the overlay. The full view
    // of an image stops its presses, since a drag of the image may end beside it.
    let mut pressed_beside = use_signal(|| false);
    let dialog = cockpit.state.read().ui.dialog.clone();
    let Some(Shown {
        value: dialog,
        leaving,
        opening,
    }) = use_presence(dialog)
    else {
        return rsx! {};
    };
    let mut overlay_class = vec!["overlay"];
    if matches!(dialog, Dialog::AgentMap(_) | Dialog::ImageView(_)) {
        overlay_class.push("tall-overlay");
    }
    if leaving {
        overlay_class.push("leaving");
    }
    let overlay_class = overlay_class.join(" ");
    rsx! {
        // Keyed by opening: a dialog reopened while the last one leaves mounts afresh, its fields
        // empty.
        for opening in [opening] {
            div {
                key: "{opening}",
                class: "{overlay_class}",
                inert: leaving.then_some(true),
                onmousedown: move |event| {
                    pressed_beside.set(event.trigger_button() == Some(MouseButton::Primary));
                },
                onclick: move |_| {
                    if pressed_beside.take() {
                        cockpit.update(|state| state.ui.close_dialog());
                    }
                },
                match dialog.clone() {
                    Dialog::Switcher => rsx! {
                        SwitcherDialog {}
                    },
                    Dialog::NewAgent(form) => rsx! {
                        NewAgentDialog { form }
                    },
                    Dialog::OpenFile(view) => rsx! {
                        OpenFileDialog { view }
                    },
                    Dialog::CloseAgent(agent) => rsx! {
                        CloseAgentDialog { agent }
                    },
                    Dialog::AgentMap(view) => rsx! {
                        AgentMapDialog { view }
                    },
                    dialog @ (Dialog::Accounts(_) | Dialog::AddAccount(_)) => accounts_view::dialog(dialog),
                    Dialog::Machines(view) => rsx! {
                        MachinesDialog { view }
                    },
                    Dialog::NoDaemon => rsx! {
                        NoDaemonCard {}
                    },
                    Dialog::DaemonUpdate(machine) => rsx! {
                        DaemonUpdateScreens { machine }
                    },
                    Dialog::ImageView(view) => rsx! {
                        ImageViewDialog { view }
                    },
                    Dialog::WorkingFolder(form) => rsx! {
                        WorkingFolderDialog { form }
                    },
                    Dialog::Settings(view) => rsx! {
                        SettingsDialog { view }
                    },
                }
            }
        }
    }
}

/// Says what closing the agent would lose, before it closes.
#[component]
fn CloseAgentDialog(agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    let question = {
        let state = cockpit.state.read();
        let drafts = cockpit.drafts.read();
        let Some(entry) = state.agents.get(&agent) else {
            return rsx! {};
        };
        match actions::close_warning(entry, &drafts) {
            Some(warning) => format!("{warning} Close it?"),
            None => format!("Close {}?", entry.summary.title),
        }
    };
    // The shortcut that asked confirms too.
    let close_keys = cockpit
        .state
        .read()
        .prefs
        .keys
        .chord(Command::CloseAgent)
        .caps()
        .join(" ");
    rsx! {
        div {
            class: "dialog close-agent",
            role: "alertdialog",
            aria_label: "Close the agent",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "Close the agent" }
                span { class: "key-hint", "{close_keys}" }
            }
            p { class: "dialog-text",
                "{question} "
                "Its session stays in Resume a session."
            }
            div { class: "dialog-actions",
                button {
                    class: "button",
                    onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
                    "Cancel"
                }
                button {
                    class: "button danger",
                    onmounted: move |event| async move {
                        // The dialog may have closed already: nothing to focus then.
                        let _ = event.data().set_focus(true).await;
                    },
                    onclick: move |_| cockpit.change_with_drafts(|state, drafts| actions::close_agent(state, drafts, agent)),
                    "Close agent"
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct SwitcherRow {
    id: AgentId,
    title: String,
    status: AgentStatus,
    folder: String,
    unread: bool,
}

#[component]
fn SwitcherDialog() -> Element {
    let cockpit: Cockpit = use_context();
    let (query, highlighted) = {
        let drafts = cockpit.drafts.read();
        (drafts.switcher.query.clone(), drafts.switcher.highlighted)
    };
    // Follows the keyboard alone: a render that lags behind typing would put an older query back.
    let query_seed = use_hook(|| query.clone());
    let pointer = use_pointer_moves();
    let rows: Vec<SwitcherRow> = {
        let state = cockpit.state.read();
        actions::filtered_agents(&state, &query)
            .into_iter()
            .map(|entry| SwitcherRow {
                id: entry.id(),
                title: entry.summary.title.clone(),
                status: entry.summary.status,
                folder: path_style::shown(&entry.summary.folder),
                unread: state.is_unread(entry.id()),
            })
            .collect()
    };
    let new_keys = cockpit.state.read().prefs.keys.shown(Command::NewAgent);
    rsx! {
        div {
            class: "dialog switcher",
            role: "dialog",
            aria_label: "Switch to an agent",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-search",
                Icon { kind: IconKind::Search, size: 16 }
                input {
                    class: "search-input",
                    r#type: "text",
                    aria_label: "Find an agent",
                    placeholder: "Type to find an agent by title or folder",
                    initial_value: "{query_seed}",
                    onmounted: move |event| async move {
                        // The dialog may have closed already: nothing to focus then.
                        let _ = event.data().set_focus(true).await;
                    },
                    oninput: move |event: FormEvent| cockpit.with_drafts(|_, drafts| actions::switcher_query(drafts, event.value())),
                    onkeydown: move |event: KeyboardEvent| match event.key() {
                        Key::ArrowDown => {
                            cockpit.with_drafts(|state, drafts| actions::switcher_move(state, drafts, Direction::Down));
                            bridge::scroll_to_highlighted();
                        }
                        Key::ArrowUp => {
                            cockpit.with_drafts(|state, drafts| actions::switcher_move(state, drafts, Direction::Up));
                            bridge::scroll_to_highlighted();
                        }
                        Key::Enter => cockpit.update_with_drafts(|state, drafts| actions::switcher_pick(state, drafts, None)),
                        _ => {}
                    },
                }
                span { class: "key-hint", "Ctrl K" }
                button {
                    class: "icon-button",
                    aria_label: "Close",
                    onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            div { class: "dialog-list scroll",
                if rows.is_empty() {
                    span { class: "dialog-empty", "No agent matches. {new_keys} starts a new one." }
                }
                for (index, row) in rows.into_iter().enumerate() {
                    button {
                        key: "{row.id.get()}",
                        class: if index == highlighted { "switcher-row {HIGHLIGHTED_CLASS} {format::status_class(row.status)}" } else { "switcher-row {format::status_class(row.status)}" },
                        onmousemove: move |event: MouseEvent| {
                            // Writing the drafts renders their readers: only a new row does.
                            if pointer.moved(&event) && cockpit.drafts.peek().switcher.highlighted != index {
                                cockpit.with_drafts(|state, drafts| actions::switcher_point(state, drafts, index));
                            }
                        },
                        onclick: move |_| cockpit.update_with_drafts(|state, drafts| actions::switcher_pick(state, drafts, Some(index))),
                        span { class: "status-dot", aria_hidden: true }
                        span { class: if row.unread { "switcher-title unread" } else { "switcher-title" }, "{row.title}" }
                        span { class: "switcher-status", "{format::status_label(row.status)}" }
                        span { class: "spacer" }
                        span { class: "mono switcher-folder", "{row.folder}" }
                    }
                }
            }
            span { class: "dialog-foot", "Up and Down to move, Enter to open, Esc to close" }
        }
    }
}

#[component]
fn NewAgentDialog(form: NewAgentForm) -> Element {
    let cockpit: Cockpit = use_context();
    let start = form.tab == NewAgentTab::Start;
    let menu_open = form.menu.is_some();
    rsx! {
        div {
            class: "dialog new-agent",
            role: "dialog",
            aria_label: "New agent",
            onclick: move |event| {
                event.stop_propagation();
                if menu_open {
                    change_form(cockpit, |form| form.menu = None);
                }
            },
            div { class: "dialog-head",
                h2 { "New agent" }
                span { class: "key-hint", "Ctrl N" }
                button {
                    class: "icon-button small",
                    aria_label: "Close",
                    onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            NewAgentTarget { form: form.clone() }
            div { class: "dialog-tabs", role: "tablist", aria_label: "How to start",
                button {
                    class: if start { "dialog-tab selected" } else { "dialog-tab" },
                    role: "tab",
                    aria_selected: start,
                    onclick: move |_| change_form(cockpit, |form| form.tab = NewAgentTab::Start),
                    Icon { kind: IconKind::Folder, size: 14 }
                    "Start in a folder"
                }
                button {
                    class: if start { "dialog-tab" } else { "dialog-tab selected" },
                    role: "tab",
                    aria_selected: !start,
                    onclick: move |_| change_form(cockpit, |form| form.tab = NewAgentTab::Resume),
                    Icon { kind: IconKind::Clock, size: 14 }
                    "Resume a session"
                }
            }
            if start {
                StartForm { form }
            } else {
                ResumeList { starting: form.starting }
            }
        }
    }
}

/// The machine and the account, which both tabs use. Each shows once there is a choice.
#[component]
fn NewAgentTarget(form: NewAgentForm) -> Element {
    let cockpit: Cockpit = use_context();
    let (machine_choices, account) = {
        let state = cockpit.state.read();
        let account = accounts::shows_labels(&state, form.machine)
            .then(|| accounts::new_agent_account(&state, &form));
        (machine_choices(&state, form.machine), account)
    };
    if machine_choices.is_empty() && account.is_none() {
        return rsx! {};
    }
    let account_open = form.menu == Some(FormMenu::Account);
    rsx! {
        div { class: "new-agent-target",
            if !machine_choices.is_empty() {
                div { class: "field-group",
                    span { class: "field-label", "Machine" }
                    div { class: "machine-choice", role: "radiogroup", aria_label: "Machine",
                        for choice in machine_choices {
                            {
                                let picked = choice.machine;
                                rsx! {
                                    button {
                                        key: "{choice.name}",
                                        class: if choice.chosen { "machine-option checked" } else { "machine-option" },
                                        role: "radio",
                                        aria_checked: choice.chosen,
                                        disabled: !choice.answers,
                                        title: if choice.answers { "" } else { "This machine does not answer" },
                                        onclick: move |_| pick_machine(cockpit, picked),
                                        Icon { kind: choice.icon, size: 13 }
                                        "{choice.name}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(account) = account {
                div { class: "field-group",
                    span { class: "field-label", "Account" }
                    AccountPicker { account, open: account_open }
                }
            }
        }
    }
}

/// Opens the system's folder picker at `from` when it is a folder, and puts the folder picked in
/// the field.
async fn browse_folder(cockpit: Cockpit, from: String) {
    let mut picker = AsyncFileDialog::new().set_title("Folder for the new agent");
    let from = PathBuf::from(from.trim());
    if from.is_absolute() && from.is_dir() {
        picker = picker.set_directory(&from);
    }
    let Some(folder) = picker.pick_folder().await else {
        return;
    };
    let picked = path_style::shown(folder.path());
    let mut taken = false;
    cockpit.update_with_drafts(|state, drafts| {
        taken = folder_field::pick(state, drafts, picked.clone());
    });
    if taken {
        bridge::set_field_text(FOLDER_FIELD_ID, picked);
    }
}

fn change_form(cockpit: Cockpit, change: impl FnOnce(&mut NewAgentForm)) {
    cockpit.update(|state| actions::with_new_agent_form(state, change));
}

/// A folder's parent, with its separator at the end, and its own name: "/work/" and "parser".
fn folder_parts(folder: &Path) -> (String, String) {
    let style = PathStyle::of(folder);
    let folder = folder.to_string_lossy();
    match (style.parent(&folder), style.file_name(&folder)) {
        (Some(parent), Some(name)) => (style.folder_text(parent), name.to_owned()),
        _ => (String::new(), style.shown(&folder).into_owned()),
    }
}

/// A path on one line, cut at its start since its end tells folders apart: the parent dim, the
/// folder's own name bright.
#[component]
fn FolderPath(folder: PathBuf) -> Element {
    let (parent, name) = folder_parts(&folder);
    rsx! {
        span { class: "mono folder-path", title: "{path_style::shown(&folder)}",
            span { class: "folder-path-text",
                span { class: "folder-parent", "{parent}" }
                span { class: "folder-name", "{name}" }
            }
        }
    }
}

#[component]
fn StartForm(form: NewAgentForm) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let (typed_folder, typed_prompt) = {
        let drafts = cockpit.drafts.read();
        (
            drafts.new_agent.folder.clone(),
            drafts.new_agent.prompt.clone(),
        )
    };
    // The folder and the first message start from the drafts, then follow the keyboard alone: a
    // render that lags behind typing would put an older text back.
    let folder_seed = use_hook(|| typed_folder.clone());
    let prompt_seed = use_hook(|| typed_prompt.clone());
    let focus = use_hook(|| actions::start_focus(&cockpit.drafts.peek().new_agent));
    // The recent folder under the pointer, the one row lit. The field's folder is only marked.
    let mut pointed = use_signal(|| None::<PathBuf>);
    let (folders, machine, local, sessions_loading, folders_unknown) = {
        let state = cockpit.state.read();
        let chosen = state.machines.get(form.machine);
        let mut folders: Vec<(PathBuf, Timestamp, Option<AccountLabel>)> = state
            .recent_folders(form.machine)
            .into_iter()
            .map(|recent| {
                let label = accounts::label(&state, form.machine, recent.account);
                (recent.folder, recent.used, label)
            })
            .collect();
        folders.truncate(RECENT_FOLDERS);
        let loading = chosen.is_none_or(|known| {
            matches!(known.sessions, Loadable::NotRequested | Loadable::Loading)
        });
        // The system's picker shows this computer's folders, which a daemon over SSH cannot use.
        let local = form.machine == MachineId::THIS
            && chosen.is_some_and(|known| known.daemon.place != DaemonPlace::Ssh);
        let machine = chosen.map_or_else(String::new, |known| known.daemon.machine.clone());
        (
            folders,
            machine,
            local,
            loading,
            chosen.is_some_and(|known| known.folders_unknown),
        )
    };
    // The past sessions bring more recent folders: rows stand for them until they arrive, so the
    // list does not grow under the pointer.
    let placeholders = if sessions_loading {
        RECENT_FOLDERS.saturating_sub(folders.len())
    } else {
        0
    };
    let now = now();
    let can_start = !typed_folder.trim().is_empty() && !form.starting;
    let start =
        move || cockpit.change_with_drafts(|state, drafts| actions::start_agent(state, drafts));
    let start_on_command_enter = move |event: KeyboardEvent| {
        let command = event.modifiers().ctrl() || event.modifiers().meta();
        if command && event.key() == Key::Enter {
            start();
        }
    };
    // As in the composer: Enter starts, and Shift+Enter adds a line.
    let start_on_enter = move |event: KeyboardEvent| {
        let modifiers = event.modifiers();
        let plain_enter = event.key() == Key::Enter && !modifiers.shift() && !modifiers.alt();
        if plain_enter && !event.is_composing() {
            event.prevent_default();
            start();
        }
    };
    let toggle_menu = move |menu: FormMenu| {
        move |event: MouseEvent| {
            event.stop_propagation();
            change_form(cockpit, |form| {
                form.menu = (form.menu != Some(menu)).then_some(menu);
            });
        }
    };
    let model_label = form
        .model
        .as_deref()
        .and_then(|alias| MODEL_CHOICES.iter().find(|choice| choice.alias == alias))
        .map_or(DEFAULT_MODEL, |choice| choice.label);
    let effort_label = form.effort.map(format::effort_label);
    let mode_label = form.mode.map_or(DEFAULT_MODE, format::mode_label);
    let mode_icon = mode_icon(form.mode.unwrap_or_default());
    let model_open = form.menu == Some(FormMenu::Model);
    let mode_open = form.menu == Some(FormMenu::Mode);
    rsx! {
        div { class: "dialog-body",
            div { class: "field-group",
                label { class: "field-label", r#for: FOLDER_FIELD_ID, "Folder on {machine}" }
                // Above the recent folders, which arrive after the dialog opens: the field stays
                // where the first click aims.
                div { class: "folder-field",
                    Icon { kind: IconKind::Folder, size: 16 }
                    input {
                        id: FOLDER_FIELD_ID,
                        class: "folder-input mono",
                        r#type: "text",
                        spellcheck: false,
                        placeholder: "Type the folder's path, or pick a recent folder below",
                        initial_value: "{folder_seed}",
                        onmounted: move |_| {
                            if focus == StartField::Folder {
                                bridge::focus_field_at_end(FOLDER_FIELD_ID);
                            }
                        },
                        oninput: move |event: FormEvent| type_folder(cockpit, event.value()),
                        onkeydown: move |event: KeyboardEvent| {
                            if event.modifiers().ctrl() || event.modifiers().meta() {
                                start_on_command_enter(event);
                            } else {
                                folder_key(cockpit, &event);
                            }
                        },
                    }
                    if local {
                        button {
                            class: "folder-browse-button",
                            title: "Pick the folder in the system's folder dialog",
                            onclick: move |_| {
                                let from = cockpit.drafts.peek().new_agent.folder.clone();
                                spawn(browse_folder(cockpit, from));
                            },
                            "Browse…"
                        }
                    }
                }
                if folders_unknown {
                    span { class: "dialog-note",
                        "The daemon is older than this app and does not list folders as you type. Update the daemon to get them."
                    }
                }
            }
            if let Some(list) = form.folders.clone() {
                FolderRows { list, escape: "Esc for the recent folders" }
            } else if !folders.is_empty() || placeholders > 0 {
                div { class: "field-group",
                    span { class: "field-label loading-note",
                        "Recent folders"
                        if sessions_loading {
                            span { class: "spinner", role: "status", aria_label: "Loading the recent folders" }
                        }
                    }
                    div {
                        class: "folder-list",
                        role: "radiogroup",
                        aria_label: "Recent folders",
                        onmouseleave: move |_| pointed.set(None),
                        for (folder, used, label) in folders {
                            {
                                let shown = path_style::shown(&folder);
                                let checked = typed_folder == shown;
                                let lit = pointed.read().as_ref() == Some(&folder);
                                let used = format::ago(used, now);
                                let picked = shown.clone();
                                let hovered = folder.clone();
                                let mut class = vec!["folder-option"];
                                if checked {
                                    class.push("checked");
                                }
                                if lit {
                                    class.push(HIGHLIGHTED_CLASS);
                                }
                                rsx! {
                                    button {
                                        key: "{shown}",
                                        class: class.join(" "),
                                        role: "radio",
                                        aria_checked: checked,
                                        title: "{shown}",
                                        // Writing the signal renders the form: only a new row does.
                                        onmousemove: move |_| {
                                            if pointed.peek().as_ref() != Some(&hovered) {
                                                pointed.set(Some(hovered.clone()));
                                            }
                                        },
                                        onclick: move |_| {
                                            let picked = picked.clone();
                                            bridge::set_field_text(FOLDER_FIELD_ID, picked.clone());
                                            cockpit.update_with_drafts(|_, drafts| {
                                                drafts.new_agent.folder = picked;
                                            });
                                        },
                                        span { class: "folder-icon", Icon { kind: IconKind::Folder, size: 14 } }
                                        FolderPath { folder }
                                        if let Some(label) = label {
                                            AccountMini { label }
                                        }
                                        span { class: "folder-used", "{used}" }
                                    }
                                }
                            }
                        }
                        for (index, width) in PLACEHOLDER_WIDTHS.iter().take(placeholders).enumerate() {
                            div { key: "placeholder-{index}", class: "folder-option placeholder", aria_hidden: true,
                                span { class: "skeleton", style: "width: 14px; height: 12px" }
                                span { class: "skeleton skeleton-line", style: "width: {width}" }
                                span { class: "spacer" }
                                span { class: "skeleton skeleton-line", style: "width: 56px" }
                            }
                        }
                    }
                }
            }
        }
        div { class: "dialog-footer",
            div { class: "prompt-box",
                textarea {
                    id: PROMPT_FIELD_ID,
                    class: "prompt-input",
                    rows: "3",
                    aria_label: "First message",
                    placeholder: "What should the agent do? Optional: it can wait for your first message.",
                    initial_value: "{prompt_seed}",
                    onmounted: move |_| {
                        if focus == StartField::Message {
                            bridge::focus_field_at_end(PROMPT_FIELD_ID);
                        }
                    },
                    oninput: move |event: FormEvent| cockpit.with_drafts(|_, drafts| drafts.new_agent.prompt = event.value()),
                    onkeydown: start_on_enter,
                }
                div { class: "prompt-tools",
                    div { class: "menu-anchor",
                        button {
                            class: if model_open { "menu-button raised open" } else { "menu-button raised" },
                            aria_haspopup: "menu",
                            aria_expanded: model_open,
                            title: "Model and effort",
                            onclick: toggle_menu(FormMenu::Model),
                            "{model_label}"
                            if let Some(effort_label) = effort_label {
                                span { class: "muted", "{effort_label}" }
                            }
                            Icon { kind: IconKind::ChevronDown, size: 12 }
                        }
                        Presence { open: model_open,
                            FormModelMenu { model: form.model.clone(), effort: form.effort }
                        }
                    }
                    span { class: "spacer" }
                    div { class: "menu-anchor",
                        button {
                            class: if mode_open { "menu-button open" } else { "menu-button" },
                            aria_haspopup: "menu",
                            aria_expanded: mode_open,
                            title: "Permission mode",
                            onclick: toggle_menu(FormMenu::Mode),
                            Icon { kind: mode_icon, size: 14 }
                            "{mode_label}"
                            Icon { kind: IconKind::ChevronDown, size: 12 }
                        }
                        Presence { open: mode_open,
                            FormModeMenu { mode: form.mode }
                        }
                    }
                }
            }
            div { class: "dialog-actions",
                button {
                    class: "button",
                    onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
                    "Cancel"
                }
                button {
                    class: if can_start { "button primary" } else { "button disabled" },
                    disabled: !can_start,
                    onclick: move |_| start(),
                    if form.starting {
                        span { class: "spinner", aria_hidden: true }
                        "Starting"
                    } else {
                        "Start the agent"
                    }
                    span { class: "key-hint", "Ctrl Enter" }
                }
            }
        }
    }
}

/// Forks or moves an agent's conversation to another working folder: the folder `claude` runs in,
/// and Claude Code's copy of the session there.
#[component]
fn WorkingFolderDialog(form: WorkingFolderForm) -> Element {
    let cockpit: Cockpit = use_context();
    let (title, folder) = {
        let state = cockpit.state.read();
        let Some(entry) = state.agents.get(&form.agent) else {
            return rsx! {};
        };
        (
            entry.summary.title.clone(),
            path_style::shown(&entry.summary.folder),
        )
    };
    // The field starts from the draft, then follows the keyboard alone.
    let seed = use_hook(|| cockpit.drafts.peek().new_agent.folder.clone());
    let (heading, text, confirm) = match form.action {
        FolderAction::Fork => (
            "Fork into another working folder",
            format!(
                "A new agent carries on this conversation in the folder you pick, under a new Claude session. {title} goes on in {folder}."
            ),
            "Fork",
        ),
        FolderAction::Move => (
            "Move to another working folder",
            format!(
                "{title} stops, then runs again in the folder you pick, on the same conversation. Claude Code's copy of the session in {folder} stays."
            ),
            "Move",
        ),
    };
    let submit = move || {
        cockpit.change_with_drafts(|state, drafts| working_folder::submit(state, drafts));
    };
    rsx! {
        div {
            class: "dialog working-folder",
            role: "dialog",
            aria_label: heading,
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "{heading}" }
            }
            p { class: "dialog-text", "{text}" }
            div { class: "dialog-body",
                div { class: "field-group",
                    label { class: "field-label", r#for: FOLDER_FIELD_ID, "Working folder" }
                    div { class: "folder-field",
                        Icon { kind: IconKind::Folder, size: 16 }
                        input {
                            id: FOLDER_FIELD_ID,
                            class: "folder-input mono",
                            r#type: "text",
                            spellcheck: false,
                            initial_value: "{seed}",
                            onmounted: move |_| bridge::focus_field_at_end(FOLDER_FIELD_ID),
                            oninput: move |event: FormEvent| type_folder(cockpit, event.value()),
                            onkeydown: move |event: KeyboardEvent| folder_key(cockpit, &event),
                        }
                    }
                    if let Some(error) = &form.error {
                        span { class: "form-error", role: "alert", "{error}" }
                    }
                }
                if let Some(list) = form.folders.clone() {
                    FolderRows { list, escape: "Esc to cancel" }
                }
            }
            div { class: "dialog-actions",
                button {
                    class: "button",
                    onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
                    "Cancel"
                }
                button {
                    class: if form.sending { "button primary disabled" } else { "button primary" },
                    disabled: form.sending,
                    onclick: move |_| submit(),
                    if form.sending {
                        span { class: "spinner", aria_hidden: true }
                    }
                    "{confirm}"
                    span { class: "key-hint", "Enter" }
                }
            }
        }
    }
}

/// The folders of the folder the field names, in the recent folders' place. `escape` says what Esc
/// does in the dialog.
#[component]
fn FolderRows(list: FolderList, escape: &'static str) -> Element {
    let cockpit: Cockpit = use_context();
    let pointer = use_pointer_moves();
    let (text, highlighted) = {
        let drafts = cockpit.drafts.read();
        (
            drafts.new_agent.folder.clone(),
            drafts.new_agent.folder_highlighted,
        )
    };
    let style = folder_field::style(&cockpit.state.peek());
    let rows = folder_field::rows(style, &list, &text);
    let (_, segment) = style.split(&text);
    let segment = segment.to_owned();
    let folder_rows = rows
        .iter()
        .filter(|row| matches!(row, FolderRow::Folder(_)))
        .count();
    rsx! {
        div { class: "field-group",
            span { class: "field-label", "Folders" }
            div { class: "dialog-list folder-browse scroll", role: "listbox", aria_label: "Folders",
                for (index, row) in rows.iter().enumerate() {
                    {folder_row(cockpit, pointer, *row, index, highlighted == Some(index))}
                }
                match &list.listing {
                    Loadable::NotRequested | Loadable::Loading => rsx! {
                        for width in FOLDER_LOADING_ROWS {
                            div { class: "open-file-row placeholder", aria_hidden: true,
                                span { class: "skeleton", style: "width: 14px; height: 14px" }
                                span { class: "skeleton skeleton-line", style: "width: {width}" }
                            }
                        }
                    },
                    Loadable::Failed(error) => rsx! {
                        span { class: "dialog-empty", "{open_file_view::sentence(error)}" }
                    },
                    Loadable::Loaded(listing) => rsx! {
                        if folder_rows == 0 {
                            span { class: "dialog-empty",
                                if segment.is_empty() {
                                    "This folder holds no folder."
                                } else {
                                    "No folder here holds \"{segment}\"."
                                }
                            }
                        }
                        if listing.completeness == Completeness::Truncated {
                            span { class: "dialog-empty",
                                "This folder holds too many entries: some folders are left out."
                            }
                        }
                    },
                }
            }
            span { class: "dialog-note",
                "Tab to complete, Backspace to go up, {escape}"
            }
        }
    }
}

fn folder_row(
    cockpit: Cockpit,
    pointer: PointerMoves,
    row: FolderRow<'_>,
    index: usize,
    highlighted: bool,
) -> Element {
    let class = if highlighted {
        format!("switcher-row open-file-row {HIGHLIGHTED_CLASS}")
    } else {
        "switcher-row open-file-row".to_owned()
    };
    let point = move |event: MouseEvent| {
        // Writing the drafts renders their readers: only a new row does.
        if pointer.moved(&event)
            && cockpit.drafts.peek().new_agent.folder_highlighted != Some(index)
        {
            cockpit.with_drafts(|state, drafts| folder_field::point(state, drafts, index));
        }
    };
    let click = move |_| {
        take_folder(cockpit, Some(index));
    };
    match row {
        FolderRow::Up => rsx! {
            button { key: "{UP_ROW}", class, onmousemove: point, onclick: click,
                Icon { kind: IconKind::Folder, size: 14 }
                span { class: "open-file-name mono", "{UP_ROW}" }
            }
        },
        FolderRow::Folder(entry) => rsx! {
            button { key: "{entry.name}", class, onmousemove: point, onclick: click,
                {open_file_view::entry_parts(entry)}
            }
        },
    }
}

/// Keeps the typed folder, and lists the folder it names when that changes.
fn type_folder(cockpit: Cockpit, text: String) {
    let follow =
        cockpit.with_drafts(|state, drafts| folder_field::type_folder(state, drafts, text));
    if follow {
        cockpit.update_with_drafts(|state, drafts| folder_field::follow(state, drafts));
    }
}

/// The folder field's keys, as the "Open file" dialog's path field has them. Enter takes the
/// highlighted folder, or with none highlighted, starts the agent, or forks or moves it in the
/// working folder dialog.
fn folder_key(cockpit: Cockpit, event: &KeyboardEvent) {
    if event.modifiers().alt() || event.is_composing() {
        return;
    }
    let direction = match event.key() {
        Key::ArrowDown => Direction::Down,
        Key::ArrowUp => Direction::Up,
        Key::Tab if !event.modifiers().shift() => {
            // With no row to take, Tab moves the focus as usual.
            if take_folder(cockpit, None) {
                event.prevent_default();
            }
            return;
        }
        Key::Enter => {
            event.prevent_default();
            if !take_folder(cockpit, None) {
                cockpit.change_with_drafts(|state, drafts| match state.ui.dialog {
                    Some(Dialog::WorkingFolder(_)) => working_folder::submit(state, drafts),
                    _ => actions::start_agent(state, drafts),
                });
            }
            return;
        }
        // The page's script passes Backspace on only with the caret at the end and nothing
        // selected.
        Key::Backspace => {
            if let Some(text) =
                cockpit.with_drafts(|state, drafts| folder_field::text_above(state, drafts))
            {
                event.prevent_default();
                cockpit.update_with_drafts(|state, drafts| {
                    folder_field::set_text(state, drafts, text.clone())
                });
                bridge::set_field_text(FOLDER_FIELD_ID, text);
            }
            return;
        }
        _ => return,
    };
    // The keys move the highlight, and the field's caret stays. The first opens the list.
    event.prevent_default();
    if cockpit.with_drafts(|state, _| folder_field::list(state).is_none()) {
        cockpit.update_with_drafts(|state, drafts| folder_field::follow(state, drafts));
    } else {
        cockpit.with_drafts(|state, drafts| folder_field::move_highlight(state, drafts, direction));
        bridge::scroll_to_highlighted();
    }
}

/// Picks the machine, and shows the folder the dialog then holds.
fn pick_machine(cockpit: Cockpit, machine: MachineId) {
    let mut new_text = None;
    cockpit.update_with_drafts(|state, drafts| {
        new_text = actions::pick_new_agent_machine(state, drafts, machine);
    });
    if let Some(text) = new_text {
        bridge::set_field_text(FOLDER_FIELD_ID, text);
    }
}

/// Takes a row, or the highlighted one, and puts the field's new text in it. A click took the
/// focus from the field: it goes back there. Returns whether a row was taken.
fn take_folder(cockpit: Cockpit, index: Option<usize>) -> bool {
    let mut new_text = None;
    cockpit.update_with_drafts(|state, drafts| {
        new_text = folder_field::take(state, drafts, index);
    });
    let Some(text) = new_text else {
        return false;
    };
    bridge::set_field_text(FOLDER_FIELD_ID, text);
    bridge::focus_field_at_end(FOLDER_FIELD_ID);
    true
}

/// The composer's model menu, for the form: with "Default", and without a running agent.
#[component]
fn FormModelMenu(model: Option<String>, effort: Option<Effort>) -> Element {
    use strum::VariantArray;

    let cockpit: Cockpit = use_context();
    let default_model = model.is_none();
    let default_effort = effort.is_none();
    rsx! {
        // A click in the menu stays in it: the dialog closes its menus on the clicks it gets.
        div {
            class: "popover menu model-menu form-menu",
            onclick: move |event| event.stop_propagation(),
            span { class: "menu-title", "Model" }
            button {
                class: if default_model { "menu-item chosen" } else { "menu-item" },
                aria_pressed: default_model,
                onclick: move |_| change_form(cockpit, |form| {
                    form.model = None;
                    form.menu = None;
                }),
                span { "Default" span { class: "muted", " from your Claude Code settings" } }
                if default_model {
                    Icon { kind: IconKind::Check, size: 14 }
                }
            }
            for choice in MODEL_CHOICES {
                {
                    let chosen = model.as_deref() == Some(choice.alias);
                    rsx! {
                        button {
                            key: "{choice.alias}",
                            class: if chosen { "menu-item chosen" } else { "menu-item" },
                            aria_pressed: chosen,
                            onclick: move |_| change_form(cockpit, |form| {
                                form.model = Some(choice.alias.to_owned());
                                form.menu = None;
                            }),
                            "{choice.label}"
                            if chosen {
                                Icon { kind: IconKind::Check, size: 14 }
                            }
                        }
                    }
                }
            }
            div { class: "menu-divider" }
            span { class: "menu-title", "Effort" }
            div { class: "effort-row with-default", role: "group", aria_label: "Effort",
                button {
                    class: if default_effort { "effort chosen" } else { "effort" },
                    aria_pressed: default_effort,
                    title: "The effort your Claude Code settings choose",
                    onclick: move |_| change_form(cockpit, |form| form.effort = None),
                    "Default"
                }
                for level in Effort::VARIANTS.iter().copied() {
                    button {
                        key: "{format::effort_short_label(level)}",
                        class: if effort == Some(level) { "effort chosen" } else { "effort" },
                        aria_pressed: effort == Some(level),
                        title: format::effort_label(level),
                        onclick: move |_| change_form(cockpit, |form| form.effort = Some(level)),
                        "{format::effort_short_label(level)}"
                    }
                }
            }
        }
    }
}

/// The composer's mode menu, for the form, with "Default".
#[component]
fn FormModeMenu(mode: Option<PermissionMode>) -> Element {
    let cockpit: Cockpit = use_context();
    let mut modes: Vec<PermissionMode> = MODE_CHOICES.to_vec();
    if let Some(mode) = mode
        && !modes.contains(&mode)
    {
        modes.push(mode);
    }
    rsx! {
        div {
            class: "popover menu mode-menu form-menu",
            onclick: move |event| event.stop_propagation(),
            button {
                class: if mode.is_none() { "menu-item two-lines chosen" } else { "menu-item two-lines" },
                aria_pressed: mode.is_none(),
                onclick: move |_| change_form(cockpit, |form| {
                    form.mode = None;
                    form.menu = None;
                }),
                span { "Default" }
                span { class: "muted small", "The mode your Claude Code settings choose" }
            }
            for choice in modes {
                button {
                    key: "{format::mode_label(choice)}",
                    class: if mode == Some(choice) { "menu-item two-lines chosen" } else { "menu-item two-lines" },
                    aria_pressed: mode == Some(choice),
                    onclick: move |_| change_form(cockpit, |form| {
                        form.mode = Some(choice);
                        form.menu = None;
                    }),
                    span { "{format::mode_label(choice)}" }
                    span { class: "muted small", "{format::mode_description(choice)}" }
                }
            }
        }
    }
}

#[component]
fn ResumeList(starting: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let (query, highlighted) = {
        let drafts = cockpit.drafts.read();
        (
            drafts.new_agent.session_query.clone(),
            drafts.new_agent.session_highlighted,
        )
    };
    // Follows the keyboard alone, as the other fields.
    let query_seed = use_hook(|| query.clone());
    let pointer = use_pointer_moves();
    let (sessions, machine, of_one_account) = {
        let drafts = cockpit.drafts.read();
        let state = cockpit.state.read();
        let form_machine = match &state.ui.dialog {
            Some(Dialog::NewAgent(form)) => form.machine,
            _ => MachineId::THIS,
        };
        let known = state
            .machines
            .get(form_machine)
            .unwrap_or(&state.machines.this);
        let sessions = match &known.sessions {
            Loadable::Loaded(_) => Loadable::Loaded(
                actions::listed_sessions(&state, &drafts)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<PastSession>>(),
            ),
            Loadable::NotRequested => Loadable::NotRequested,
            Loadable::Loading => Loadable::Loading,
            Loadable::Failed(error) => Loadable::Failed(error.clone()),
        };
        let of_one_account = accounts::resume_account(&state).is_some();
        (sessions, known.daemon.machine.clone(), of_one_account)
    };
    let now = now();
    let searching = !query.trim().is_empty();
    rsx! {
        div { class: "dialog-body",
            div { class: "resume-tools",
                div { class: "folder-field session-search",
                    Icon { kind: IconKind::Search, size: 16 }
                    input {
                        id: SESSION_SEARCH_ID,
                        class: "folder-input",
                        r#type: "text",
                        spellcheck: false,
                        aria_label: "Find a session",
                        placeholder: "Find a session by its title or folder",
                        initial_value: "{query_seed}",
                        onmounted: move |_| bridge::focus_field_at_end(SESSION_SEARCH_ID),
                        oninput: move |event: FormEvent| cockpit.with_drafts(|_, drafts| actions::session_query(drafts, event.value())),
                        onkeydown: move |event: KeyboardEvent| match event.key() {
                            Key::ArrowDown => {
                                cockpit.with_drafts(|state, drafts| actions::session_move(state, drafts, Direction::Down));
                                bridge::scroll_to_highlighted();
                            }
                            Key::ArrowUp => {
                                cockpit.with_drafts(|state, drafts| actions::session_move(state, drafts, Direction::Up));
                                bridge::scroll_to_highlighted();
                            }
                            Key::Enter => cockpit.change_with_drafts(|state, drafts| actions::session_pick(state, drafts, None)),
                            _ => {}
                        },
                    }
                    span { class: "key-hint", "Enter" }
                }
            }
            if of_one_account {
                span { class: "dialog-note", "This account's Claude sessions on {machine} that no agent runs now, newest first." }
            } else {
                span { class: "dialog-note", "Claude sessions on {machine} that no agent runs now, newest first." }
            }
            match sessions {
                Loadable::NotRequested | Loadable::Loading => rsx! {
                    div { class: "session-list", role: "status", aria_label: "Loading the sessions",
                        for (index, width) in PLACEHOLDER_WIDTHS.iter().enumerate() {
                            div { key: "{index}", class: "session-row placeholder", aria_hidden: true,
                                span { class: "session-text",
                                    span { class: "skeleton skeleton-line", style: "width: 38%" }
                                    span { class: "skeleton skeleton-line", style: "width: {width}" }
                                }
                                span { class: "skeleton skeleton-line", style: "width: 64px" }
                            }
                        }
                    }
                },
                Loadable::Failed(error) => rsx! {
                    span { class: "dialog-note", "The sessions did not load: {error}" }
                },
                Loadable::Loaded(sessions) => rsx! {
                    if sessions.is_empty() {
                        span { class: "dialog-note",
                            if searching {
                                "No session matches."
                            } else if of_one_account {
                                "No past session of this account."
                            } else {
                                "No past session."
                            }
                        }
                    }
                    div { class: "session-list", role: "listbox", aria_label: "Sessions",
                        // Keyed by position: the daemon's list does not promise one entry per
                        // session.
                        for (index, session) in sessions.into_iter().enumerate() {
                            {
                                let turns = format::count(usize::try_from(session.turns).unwrap_or(usize::MAX), "turn", "turns");
                                let chosen = index == highlighted;
                                rsx! {
                                    button {
                                        key: "{index}",
                                        class: if chosen { "session-row {HIGHLIGHTED_CLASS}" } else { "session-row" },
                                        role: "option",
                                        aria_selected: chosen,
                                        disabled: starting,
                                        onmousemove: move |event: MouseEvent| {
                                            // Writing the drafts renders their readers: only a new row does.
                                            if pointer.moved(&event) && !starting && cockpit.drafts.peek().new_agent.session_highlighted != index {
                                                cockpit.with_drafts(|state, drafts| actions::session_point(state, drafts, index));
                                            }
                                        },
                                        onclick: move |_| cockpit.change_with_drafts(|state, drafts| actions::session_pick(state, drafts, Some(index))),
                                        span { class: "session-text",
                                            span { class: "session-title-line",
                                                span { class: "session-title", "{session.title}" }
                                            }
                                            FolderPath { folder: session.folder.clone() }
                                        }
                                        span { class: "session-meta",
                                            span { "{format::ago(session.last_activity, now)}" }
                                            span { "{turns}" }
                                        }
                                        span { class: "session-resume", "Resume" }
                                    }
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}

/// The full view's frame, which takes the focus so that Ctrl+0 and Ctrl+= reach it.
const IMAGE_VIEW_FRAME_ID: &str = "image-view-frame";

/// An image over the whole window. Esc, the close button or a click beside it closes it.
#[component]
fn ImageViewDialog(view: ImageView) -> Element {
    let cockpit: Cockpit = use_context();
    let ImageView { agent, path, image } = view;
    let style = PathStyle::of(&path);
    let full = path.to_string_lossy().into_owned();
    let name = style.file_name(&full).unwrap_or(&full).to_owned();
    rsx! {
        div {
            class: "dialog image-view",
            role: "dialog",
            aria_label: "{name}",
            onclick: move |event| event.stop_propagation(),
            onmousedown: move |event| event.stop_propagation(),
            onmounted: move |_| bridge::focus_field(IMAGE_VIEW_FRAME_ID),
            div { class: "dialog-head",
                h2 { class: "image-view-name", title: "{full}", "{name}" }
                span { class: "key-hint", "Ctrl+wheel zooms" }
                button {
                    class: "button",
                    onclick: move |_| {
                        let path = path.clone();
                        cockpit.update(|state| {
                            state.ui.close_dialog();
                            actions::open_file(state, agent, path, None);
                        });
                    },
                    "Open in the file view"
                }
                button {
                    class: "icon-button",
                    aria_label: "Close",
                    title: "Close (Esc)",
                    onclick: move |_| cockpit.update(|state| state.ui.close_dialog()),
                    Icon { kind: IconKind::Close, size: 16 }
                }
            }
            ZoomImage {
                image,
                alt: name.clone(),
                id: IMAGE_VIEW_FRAME_ID.to_owned(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_splits_into_its_parent_and_its_name() {
        assert_eq!(
            folder_parts(Path::new("/work/tools/parser")),
            ("/work/tools/".to_owned(), "parser".to_owned())
        );
        assert_eq!(
            folder_parts(Path::new("/work")),
            ("/".to_owned(), "work".to_owned())
        );
        assert_eq!(
            folder_parts(Path::new("/")),
            (String::new(), "/".to_owned())
        );
        assert_eq!(
            folder_parts(Path::new("C:/work/parser")),
            (r"C:\work\".to_owned(), "parser".to_owned())
        );
        assert_eq!(
            folder_parts(Path::new(r"C:\work")),
            (r"C:\".to_owned(), "work".to_owned())
        );
        assert_eq!(
            folder_parts(Path::new("C:/")),
            (String::new(), r"C:\".to_owned())
        );
    }
}
