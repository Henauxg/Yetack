//! The agent switcher (Ctrl+K) and the new agent dialog (Ctrl+N).

use std::cmp::Reverse;
use std::collections::BTreeSet;

use cockpit_protocol::{AgentId, AgentStatus, ClaudeSessionId, Effort, PastSession};
use dioxus::prelude::*;
use strum::VariantArray;

use crate::connection::{Clock, Cockpit};
use crate::format::{self, MODE_CHOICES, MODEL_CHOICES};
use crate::state::{Dialog, Loadable, NewAgentForm, NewAgentTab, actions};
use crate::ui::icons::{Icon, IconKind};

/// Recent folders the new agent dialog offers.
const RECENT_FOLDERS: usize = 6;

/// The value of a select's "keep Claude Code's default" option.
const DEFAULT_CHOICE: &str = "";

#[component]
pub fn Dialogs() -> Element {
    let cockpit = use_context::<Cockpit>();
    let dialog = cockpit.state.read().ui.dialog.clone();
    let Some(dialog) = dialog else {
        return rsx! {};
    };
    rsx! {
        div {
            class: "overlay",
            onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
            match dialog {
                Dialog::Switcher(switcher) => rsx! {
                    SwitcherDialog { query: switcher.query, highlighted: switcher.highlighted }
                },
                Dialog::NewAgent(form) => rsx! {
                    NewAgentDialog { form }
                },
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
fn SwitcherDialog(query: String, highlighted: usize) -> Element {
    let cockpit = use_context::<Cockpit>();
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
            "role": "dialog",
            "aria-label": "Switch to an agent",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-search",
                Icon { kind: IconKind::Search, size: 16 }
                input {
                    class: "search-input",
                    r#type: "text",
                    "aria-label": "Find an agent",
                    placeholder: "Type to find an agent by title or folder",
                    value: "{query}",
                    onmounted: move |event| async move {
                        // The dialog may have closed already: nothing to focus then.
                        let _ = event.data().set_focus(true).await;
                    },
                    oninput: move |event: FormEvent| cockpit.update(|state| actions::switcher_query(state, event.value())),
                    onkeydown: move |event: KeyboardEvent| match event.key() {
                        Key::ArrowDown => cockpit.update(|state| actions::switcher_move(state, true)),
                        Key::ArrowUp => cockpit.update(|state| actions::switcher_move(state, false)),
                        Key::Enter => cockpit.change(|state| {
                            actions::switcher_pick(state, None);
                            Vec::new()
                        }),
                        _ => {}
                    },
                }
                span { class: "key-hint", "Ctrl K" }
                button {
                    class: "icon-button",
                    "aria-label": "Close",
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
                        onclick: move |_| cockpit.update(|state| actions::switcher_pick(state, Some(index))),
                        span { class: "status-dot", "aria-hidden": "true" }
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
    let cockpit = use_context::<Cockpit>();
    let start = form.tab == NewAgentTab::Start;
    rsx! {
        div {
            class: "dialog new-agent",
            "role": "dialog",
            "aria-label": "New agent",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "New agent" }
                span { class: "key-hint", "Ctrl N" }
                button {
                    class: "icon-button",
                    "aria-label": "Close",
                    onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            div { class: "dialog-tabs", "role": "tablist", "aria-label": "How to start",
                button {
                    class: if start { "dialog-tab selected" } else { "dialog-tab" },
                    "role": "tab",
                    "aria-selected": if start { "true" } else { "false" },
                    onclick: move |_| cockpit.update(|state| actions::with_new_agent_form(state, |form| form.tab = NewAgentTab::Start)),
                    "Start in a folder"
                }
                button {
                    class: if start { "dialog-tab" } else { "dialog-tab selected" },
                    "role": "tab",
                    "aria-selected": if start { "false" } else { "true" },
                    onclick: move |_| cockpit.update(|state| actions::with_new_agent_form(state, |form| form.tab = NewAgentTab::Resume)),
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

#[component]
fn StartForm(form: NewAgentForm) -> Element {
    let cockpit = use_context::<Cockpit>();
    let Clock(now) = use_context::<Clock>();
    let (folders, machine) = {
        let state = cockpit.state.read();
        let mut folders = state.recent_folders();
        folders.truncate(RECENT_FOLDERS);
        (folders, state.daemon.machine.clone())
    };
    let now = now();
    let model_value = form.model.clone().unwrap_or_default();
    let effort_value = form
        .effort
        .map(|effort| format::effort_label(effort).to_owned())
        .unwrap_or_default();
    let mode_value = form
        .mode
        .map(|mode| format::mode_label(mode).to_owned())
        .unwrap_or_default();
    let can_start = !form.folder.trim().is_empty() && !form.starting;
    rsx! {
        div { class: "dialog-body",
            div { class: "field-group",
                span { class: "field-label", "Folder on {machine}" }
                div { class: "folder-list", "role": "radiogroup", "aria-label": "Recent folders",
                    for (folder, used) in folders {
                        {
                            let shown = folder.display().to_string();
                            let checked = form.folder == shown;
                            let used = format::ago(used, now);
                            let picked = shown.clone();
                            rsx! {
                                button {
                                    key: "{shown}",
                                    class: if checked { "folder-option checked" } else { "folder-option" },
                                    "role": "radio",
                                    "aria-checked": if checked { "true" } else { "false" },
                                    onclick: move |_| {
                                        let picked = picked.clone();
                                        cockpit.update(|state| actions::with_new_agent_form(state, |form| form.folder = picked));
                                    },
                                    span { class: "radio-mark", "aria-hidden": "true" }
                                    span { class: "mono folder-path", "{shown}" }
                                    span { class: "muted small", "{used}" }
                                }
                            }
                        }
                    }
                }
                input {
                    class: "text-field mono",
                    r#type: "text",
                    "aria-label": "Folder",
                    placeholder: "Or type the folder's path on {machine}",
                    value: "{form.folder}",
                    oninput: move |event: FormEvent| {
                        cockpit.update(|state| actions::with_new_agent_form(state, |form| form.folder = event.value()));
                    },
                }
            }
            div { class: "select-row",
                label { class: "select-label",
                    "Model"
                    select {
                        class: "select",
                        onchange: move |event: FormEvent| {
                            let value = event.value();
                            cockpit.update(|state| actions::with_new_agent_form(state, |form| {
                                form.model = (value != DEFAULT_CHOICE).then_some(value);
                            }));
                        },
                        option { value: DEFAULT_CHOICE, selected: model_value.is_empty(), "Default" }
                        for choice in MODEL_CHOICES {
                            option { value: choice.alias, selected: model_value == choice.alias, "{choice.label}" }
                        }
                    }
                }
                label { class: "select-label",
                    "Effort"
                    select {
                        class: "select",
                        onchange: move |event: FormEvent| {
                            let value = event.value();
                            let effort = Effort::VARIANTS
                                .iter()
                                .copied()
                                .find(|effort| format::effort_label(*effort) == value);
                            cockpit.update(|state| actions::with_new_agent_form(state, |form| form.effort = effort));
                        },
                        option { value: DEFAULT_CHOICE, selected: effort_value.is_empty(), "Default" }
                        for effort in Effort::VARIANTS.iter().copied() {
                            option {
                                value: format::effort_label(effort),
                                selected: effort_value == format::effort_label(effort),
                                "{format::effort_label(effort)}"
                            }
                        }
                    }
                }
                label { class: "select-label",
                    "Permissions"
                    select {
                        class: "select",
                        onchange: move |event: FormEvent| {
                            let value = event.value();
                            let mode = MODE_CHOICES
                                .iter()
                                .copied()
                                .find(|mode| format::mode_label(*mode) == value);
                            cockpit.update(|state| actions::with_new_agent_form(state, |form| form.mode = mode));
                        },
                        option { value: DEFAULT_CHOICE, selected: mode_value.is_empty(), "Default" }
                        for mode in MODE_CHOICES {
                            option {
                                value: format::mode_label(mode),
                                selected: mode_value == format::mode_label(mode),
                                "{format::mode_label(mode)}"
                            }
                        }
                    }
                }
            }
            div { class: "field-group",
                span { class: "field-label", "First message" }
                textarea {
                    class: "text-field prompt-field",
                    rows: "3",
                    "aria-label": "First message",
                    placeholder: "What should the agent do?",
                    value: "{form.prompt}",
                    oninput: move |event: FormEvent| {
                        cockpit.update(|state| actions::with_new_agent_form(state, |form| form.prompt = event.value()));
                    },
                    onkeydown: move |event: KeyboardEvent| {
                        let command = event.modifiers().ctrl() || event.modifiers().meta();
                        if command && event.key() == Key::Enter {
                            cockpit.change(actions::start_agent);
                        }
                    },
                }
            }
            div { class: "dialog-actions",
                span { class: "muted small spacer", "Ctrl+Enter starts the agent" }
                button {
                    class: "button",
                    onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
                    "Cancel"
                }
                button {
                    class: if can_start { "button primary" } else { "button disabled" },
                    disabled: !can_start,
                    onclick: move |_| cockpit.change(actions::start_agent),
                    if form.starting { "Starting" } else { "Start the agent" }
                }
            }
        }
    }
}

#[component]
fn ResumeList(starting: bool) -> Element {
    let cockpit = use_context::<Cockpit>();
    let Clock(now) = use_context::<Clock>();
    let (sessions, machine) = {
        let state = cockpit.state.read();
        let running: BTreeSet<ClaudeSessionId> = state
            .agents
            .values()
            .filter_map(|entry| entry.summary.session.clone())
            .collect();
        let sessions = match &state.sessions {
            Loadable::Loaded(sessions) => {
                let mut free: Vec<PastSession> = sessions
                    .iter()
                    .filter(|session| !running.contains(&session.id))
                    .cloned()
                    .collect();
                free.sort_by_key(|session| Reverse(session.last_activity));
                Loadable::Loaded(free)
            }
            other => other.clone(),
        };
        (sessions, state.daemon.machine.clone())
    };
    let now = now();
    rsx! {
        div { class: "dialog-body",
            span { class: "muted", "Claude sessions on {machine} that no agent runs now, newest first." }
            match sessions {
                Loadable::NotRequested | Loadable::Loading => rsx! {
                    span { class: "muted", "Loading the sessions" }
                },
                Loadable::Failed(error) => rsx! {
                    span { class: "muted", "The sessions did not load: {error}" }
                },
                Loadable::Loaded(sessions) => rsx! {
                    if sessions.is_empty() {
                        span { class: "muted", "No past session." }
                    }
                    for session in sessions {
                        {
                            let meta = format!(
                                "Active {}, {}",
                                format::ago(session.last_activity, now),
                                format::count(usize::try_from(session.turns).unwrap_or(usize::MAX), "turn", "turns")
                            );
                            let id = session.id.clone();
                            rsx! {
                                div { key: "{session.id.as_str()}", class: "session-row",
                                    span { class: "session-text",
                                        span { class: "session-title", "{session.title}" }
                                        span { class: "mono muted small", "{session.folder.display()}" }
                                        span { class: "muted small", "{meta}" }
                                    }
                                    button {
                                        class: "button small",
                                        disabled: starting,
                                        onclick: move |_| {
                                            let id = id.clone();
                                            cockpit.change(|state| actions::resume_session(state, id));
                                        },
                                        "Resume"
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
