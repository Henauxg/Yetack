//! The agent switcher (Ctrl+K), the new agent dialog (Ctrl+N), the question before closing an
//! agent, and the frame of the "Open file" dialog (Ctrl+O).

use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use cockpit_protocol::{AgentId, AgentStatus, Effort, PastSession, PermissionMode};
use dioxus::prelude::*;

use crate::bridge::{self, FOLDER_FIELD_ID};
use crate::connection::{Clock, Cockpit};
use crate::format::{self, MODE_CHOICES, MODEL_CHOICES};
use crate::state::actions::Direction;
use crate::state::{Dialog, FormMenu, Loadable, NewAgentForm, NewAgentTab, actions};
use crate::ui::agent_map::AgentMapDialog;
use crate::ui::composer::mode_icon;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::open_file::OpenFileDialog;
use crate::ui::presence::{Presence, Shown, use_presence};

/// Recent folders the new agent dialog offers.
const RECENT_FOLDERS: usize = 6;

/// The widths of the rows that stand for folders and sessions on their way.
const PLACEHOLDER_WIDTHS: [&str; RECENT_FOLDERS] = ["72%", "58%", "66%", "50%", "62%", "54%"];

/// What the menu buttons say when the form keeps Claude Code's default.
const DEFAULT_MODEL: &str = "Default model";
const DEFAULT_MODE: &str = "Default mode";

#[component]
pub fn Dialogs() -> Element {
    let cockpit: Cockpit = use_context();
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
    if matches!(dialog, Dialog::AgentMap(_)) {
        overlay_class.push("map-overlay");
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
                onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
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
    rsx! {
        div {
            class: "dialog close-agent",
            role: "alertdialog",
            aria_label: "Close the agent",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "Close the agent" }
                span { class: "key-hint", "Ctrl Shift W" }
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
    let rows: Vec<SwitcherRow> = {
        let state = cockpit.state.read();
        actions::filtered_agents(&state, &query)
            .into_iter()
            .map(|entry| SwitcherRow {
                id: entry.id(),
                title: entry.summary.title.clone(),
                status: entry.summary.status,
                folder: entry.summary.folder.display().to_string(),
                unread: state.is_unread(entry.id()),
            })
            .collect()
    };
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
                        Key::ArrowDown => cockpit.with_drafts(|state, drafts| actions::switcher_move(state, drafts, Direction::Down)),
                        Key::ArrowUp => cockpit.with_drafts(|state, drafts| actions::switcher_move(state, drafts, Direction::Up)),
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
                    span { class: "dialog-empty", "No agent matches. Ctrl+N starts a new one." }
                }
                for (index, row) in rows.into_iter().enumerate() {
                    button {
                        key: "{row.id.get()}",
                        class: if index == highlighted { "switcher-row highlighted {format::status_class(row.status)}" } else { "switcher-row {format::status_class(row.status)}" },
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

fn change_form(cockpit: Cockpit, change: impl FnOnce(&mut NewAgentForm)) {
    cockpit.update(|state| actions::with_new_agent_form(state, change));
}

/// A folder's parent, with its separator at the end, and its own name: "/work/" and "parser".
fn folder_parts(folder: &Path) -> (String, String) {
    match (folder.parent(), folder.file_name()) {
        (Some(parent), Some(name)) => {
            let mut parent = parent.display().to_string();
            if !parent.ends_with(MAIN_SEPARATOR) {
                parent.push(MAIN_SEPARATOR);
            }
            (parent, name.to_string_lossy().into_owned())
        }
        _ => (String::new(), folder.display().to_string()),
    }
}

/// A path on one line, cut at its start since its end tells folders apart: the parent dim, the
/// folder's own name bright.
#[component]
fn FolderPath(folder: PathBuf) -> Element {
    let (parent, name) = folder_parts(&folder);
    rsx! {
        span { class: "mono folder-path", title: "{folder.display()}",
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
    let (folders, machine, sessions_loading) = {
        let state = cockpit.state.read();
        let mut folders = state.recent_folders();
        folders.truncate(RECENT_FOLDERS);
        let loading = matches!(state.sessions, Loadable::NotRequested | Loadable::Loading);
        (folders, state.daemon.machine.clone(), loading)
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
                        oninput: move |event: FormEvent| cockpit.with_drafts(|_, drafts| drafts.new_agent.folder = event.value()),
                        onkeydown: start_on_command_enter,
                    }
                }
            }
            if !folders.is_empty() || placeholders > 0 {
                div { class: "field-group",
                    span { class: "field-label loading-note",
                        "Recent folders"
                        if sessions_loading {
                            span { class: "spinner", role: "status", aria_label: "Loading the recent folders" }
                        }
                    }
                    div { class: "folder-list", role: "radiogroup", aria_label: "Recent folders",
                        for (folder, used) in folders {
                            {
                                let shown = folder.display().to_string();
                                let checked = typed_folder == shown;
                                let used = format::ago(used, now);
                                let picked = shown.clone();
                                rsx! {
                                    button {
                                        key: "{shown}",
                                        class: if checked { "folder-option checked" } else { "folder-option" },
                                        role: "radio",
                                        aria_checked: checked,
                                        title: "{shown}",
                                        onclick: move |_| {
                                            let picked = picked.clone();
                                            bridge::set_field_text(FOLDER_FIELD_ID, picked.clone());
                                            cockpit.with_drafts(|_, drafts| drafts.new_agent.folder = picked);
                                        },
                                        span { class: "folder-icon", Icon { kind: IconKind::Folder, size: 14 } }
                                        FolderPath { folder }
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
                    class: "prompt-input",
                    rows: "3",
                    aria_label: "First message",
                    placeholder: "What should the agent do? Optional: it can wait for your first message.",
                    initial_value: "{prompt_seed}",
                    oninput: move |event: FormEvent| cockpit.with_drafts(|_, drafts| drafts.new_agent.prompt = event.value()),
                    onkeydown: start_on_command_enter,
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
        let new_agent = &drafts.new_agent;
        (
            new_agent.session_query.clone(),
            new_agent.session_highlighted,
        )
    };
    // Follows the keyboard alone, as the other fields.
    let query_seed = use_hook(|| query.clone());
    let (sessions, machine) = {
        let state = cockpit.state.read();
        let sessions = match &state.sessions {
            Loadable::Loaded(_) => Loadable::Loaded(
                actions::resumable_sessions(&state, &query)
                    .into_iter()
                    .cloned()
                    .collect::<Vec<PastSession>>(),
            ),
            Loadable::NotRequested => Loadable::NotRequested,
            Loadable::Loading => Loadable::Loading,
            Loadable::Failed(error) => Loadable::Failed(error.clone()),
        };
        (sessions, state.daemon.machine.clone())
    };
    let now = now();
    let searching = !query.trim().is_empty();
    rsx! {
        div { class: "dialog-body",
            div { class: "folder-field session-search",
                Icon { kind: IconKind::Search, size: 16 }
                input {
                    class: "folder-input",
                    r#type: "text",
                    spellcheck: false,
                    aria_label: "Find a session",
                    placeholder: "Find a session by its title or folder",
                    initial_value: "{query_seed}",
                    onmounted: move |event| async move {
                        // The tab may have changed already: nothing to focus then.
                        let _ = event.data().set_focus(true).await;
                    },
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
            span { class: "dialog-note", "Claude sessions on {machine} that no agent runs now, newest first." }
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
                            if searching { "No session matches." } else { "No past session." }
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
                                        class: if chosen { "session-row highlighted" } else { "session-row" },
                                        role: "option",
                                        aria_selected: chosen,
                                        disabled: starting,
                                        onclick: move |_| cockpit.change_with_drafts(|state, drafts| actions::session_pick(state, drafts, Some(index))),
                                        span { class: "session-text",
                                            span { class: "session-title", "{session.title}" }
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
    }
}
