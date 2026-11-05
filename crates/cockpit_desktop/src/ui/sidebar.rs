//! The agent list: a status filter, compact two-line rows in the order the user sets, and the
//! connection's state.

use std::time::Instant;

use cockpit_protocol::{AgentId, AgentStatus, MAX_TITLE_CHARS};
use dioxus::prelude::*;
use strum::VariantArray;

use crate::bridge::{self, ROW_TITLE_FIELD_ID};
use crate::connection::{Clock, Cockpit};
use crate::prefs::StatusFilter;
use crate::state::{AgentEntry, ConnectionState, TitleEdit, TitlePlace, actions};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::reorder::{self, LIST_ID, ROW_CLASS};
use crate::{APP_NAME, format};

#[derive(Debug, Clone, PartialEq)]
struct AgentRow {
    id: AgentId,
    title: String,
    status: AgentStatus,
    activity: String,
    pending: u16,
    unread: bool,
    selected: bool,
    /// "3/5".
    tasks: Option<String>,
    /// The title edit, while the agent is renamed in this row.
    edit: Option<TitleEdit>,
}

impl AgentRow {
    fn new(entry: &AgentEntry, selected: bool, unread: bool, edit: Option<TitleEdit>) -> Self {
        let summary = &entry.summary;
        Self {
            id: summary.id,
            title: summary.title.clone(),
            status: summary.status,
            activity: activity_line(entry),
            pending: summary.pending_count,
            unread,
            selected,
            tasks: summary
                .task_progress
                .map(|progress| format!("{}/{}", progress.done, progress.total)),
            edit,
        }
    }
}

/// What the agent does now, or what its status says when it reports nothing.
pub fn activity_line(entry: &AgentEntry) -> String {
    if let Some(activity) = &entry.summary.activity {
        return activity.clone();
    }
    match entry.summary.status {
        AgentStatus::Exited => "Process ended".to_owned(),
        status => format::status_label(status).to_owned(),
    }
}

/// A chip of the status filter.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FilterChip {
    filter: StatusFilter,
    count: usize,
    chosen: bool,
}

fn filter_label(filter: StatusFilter) -> &'static str {
    filter.status().map_or("All", format::status_label)
}

/// What the list says when the filter leaves no row.
fn empty_list_text(filter: StatusFilter) -> &'static str {
    match filter {
        StatusFilter::All => "No agent yet. Ctrl+N starts one.",
        StatusFilter::NeedsYou => "No agent needs you.",
        StatusFilter::Working => "No agent is working.",
        StatusFilter::Idle => "No agent is idle.",
        StatusFilter::Exited => "No agent has exited.",
    }
}

#[component]
pub fn Sidebar() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let filter = state.prefs.sidebar.filter;
    let chips: Vec<FilterChip> = StatusFilter::VARIANTS
        .iter()
        .map(|&chip| FilterChip {
            filter: chip,
            count: state.filter_count(chip),
            chosen: chip == filter,
        })
        .collect();
    let selected_hidden = state.selected_is_hidden();
    let rows: Vec<AgentRow> = state
        .sidebar_agents()
        .into_iter()
        .map(|entry| {
            let edit = state
                .ui
                .title_edit
                .as_ref()
                .filter(|edit| edit.agent == entry.id() && edit.place == TitlePlace::Sidebar)
                .cloned();
            AgentRow::new(
                entry,
                state.ui.selected == Some(entry.id()),
                state.is_unread(entry.id()),
                edit,
            )
        })
        .collect();
    rsx! {
        nav { class: "sidebar", aria_label: "Agents",
            div { class: "sidebar-head",
                span { class: "app-name", "{APP_NAME}" }
                button {
                    class: "icon-button",
                    title: "Find an agent (Ctrl+K)",
                    aria_label: "Find an agent (Ctrl+K)",
                    onclick: move |_| cockpit.update_with_drafts(actions::toggle_switcher),
                    Icon { kind: IconKind::Search, size: 16 }
                }
                button {
                    class: "icon-button raised",
                    title: "New agent (Ctrl+N)",
                    aria_label: "New agent (Ctrl+N)",
                    onclick: move |_| cockpit.update_with_drafts(actions::open_new_agent),
                    Icon { kind: IconKind::Plus, size: 16 }
                }
            }
            StatusFilters { chips }
            div { id: LIST_ID, class: "agent-list scroll",
                if selected_hidden {
                    p { class: "hidden-note",
                        "The filter hides the selected agent. "
                        button {
                            class: "link-button",
                            onclick: move |_| cockpit.update(|state| actions::set_status_filter(state, StatusFilter::All)),
                            "Show all"
                        }
                    }
                }
                if rows.is_empty() {
                    p { class: "sidebar-empty", "{empty_list_text(filter)}" }
                }
                for row in rows {
                    AgentRowView { key: "{row.id.get()}", row }
                }
            }
            ConnectionFooter {}
        }
    }
}

/// One chip per status, with its count: a chip hides the other agents' rows, and never changes
/// their order.
#[component]
fn StatusFilters(chips: Vec<FilterChip>) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        div { class: "status-filters", role: "group", aria_label: "Show agents by status",
            for chip in chips {
                button {
                    key: "{filter_label(chip.filter)}",
                    class: format!(
                        "filter-chip {} {}",
                        if chip.chosen { "chosen" } else { "" },
                        chip.filter.status().map_or("", format::status_class),
                    ),
                    aria_pressed: chip.chosen,
                    title: "{filter_label(chip.filter)}: {chip.count}",
                    aria_label: "{filter_label(chip.filter)}: {chip.count}",
                    onclick: move |_| cockpit.update(|state| actions::set_status_filter(state, chip.filter)),
                    if chip.filter.status().is_some() {
                        span { class: "status-dot", aria_hidden: true }
                    } else {
                        span { class: "filter-word", "{filter_label(chip.filter)}" }
                    }
                    span { class: "filter-count", "{chip.count}" }
                }
            }
        }
    }
}

#[component]
fn AgentRowView(row: AgentRow) -> Element {
    let cockpit: Cockpit = use_context();
    let agent = row.id;
    let mut classes = vec![ROW_CLASS, format::status_class(row.status)];
    if row.selected {
        classes.push("selected");
    }
    if row.unread {
        classes.push("unread");
    }
    if let Some(TitleEdit {
        title, select_all, ..
    }) = row.edit
    {
        classes.push("editing");
        let class = classes.join(" ");
        // A div, not a button: a button would take the field's Space and Enter as clicks.
        return rsx! {
            div { id: reorder::row_id(agent), class,
                span { class: "row-line",
                    span { class: "status-dot", aria_hidden: true }
                    input {
                        id: ROW_TITLE_FIELD_ID,
                        class: "row-title-field",
                        r#type: "text",
                        aria_label: "Agent title",
                        // Follows the keyboard alone: a render that lags behind typing would put
                        // an older title back.
                        initial_value: "{title}",
                        maxlength: "{MAX_TITLE_CHARS}",
                        onmounted: move |_| bridge::focus_title_field(ROW_TITLE_FIELD_ID, select_all),
                        oninput: move |event: FormEvent| cockpit.type_title(event.value()),
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Enter {
                                cockpit.change_with_drafts(actions::finish_rename);
                            }
                        },
                        onblur: move |_| bridge::save_on_blur(move || cockpit.change_with_drafts(|state, drafts| actions::finish_rename_at(state, drafts, TitlePlace::Sidebar))),
                    }
                }
                span { class: "row-line row-second",
                    span { class: "row-activity", "{row.activity}" }
                }
            }
        };
    }
    let class = classes.join(" ");
    // The close button sits beside the row, over its corner: a button cannot hold another.
    rsx! {
        div { class: if row.selected { "row-wrap selected" } else { "row-wrap" },
            AgentRowButton { row: row.clone(), class }
            button {
                class: "row-close",
                aria_label: "Close {row.title}",
                title: "Close the agent (Ctrl+Shift+W)",
                onclick: move |_| cockpit.change_with_drafts(|state, drafts| actions::request_close(state, drafts, agent)),
                Icon { kind: IconKind::Close, size: 12 }
            }
        }
    }
}

#[component]
fn AgentRowButton(row: AgentRow, class: String) -> Element {
    let cockpit: Cockpit = use_context();
    let agent = row.id;
    rsx! {
        button {
            id: reorder::row_id(agent),
            class,
            aria_current: row.selected,
            onclick: move |_| cockpit.update(|state| actions::select_agent(state, agent)),
            span { class: "row-line",
                span { class: "status-dot", aria_hidden: true }
                span {
                    class: "row-title",
                    ondoubleclick: move |_| cockpit.change_with_drafts(|state, drafts| {
                        actions::start_rename_in_sidebar(state, drafts, agent);
                        Vec::new()
                    }),
                    "{row.title}"
                }
                if row.pending > 0 {
                    span { class: "pending-badge", "{row.pending}" }
                }
                if row.unread {
                    span { class: "unread-dot", aria_hidden: true }
                    span { class: "visually-hidden", "unread" }
                }
            }
            span { class: "row-line row-second",
                span { class: "row-activity", "{row.activity}" }
                if let Some(tasks) = &row.tasks {
                    span { class: "row-tasks", "{tasks}" }
                }
            }
        }
    }
}

#[component]
fn ConnectionFooter() -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let state = cockpit.state.read();
    let machine = &state.daemon.machine;
    let (class, line, detail) = match &state.connection {
        ConnectionState::Connected => {
            let line = if state.daemon.over_ssh {
                format!("{machine}, over SSH")
            } else {
                "The daemon on this machine".to_owned()
            };
            ("connection connected", line, None)
        }
        ConnectionState::Connecting { attempt } => {
            let mut line = format!("Connecting to {machine}");
            if *attempt > 1 {
                line.push_str(&format!(", attempt {attempt}"));
            }
            ("connection connecting", line, None)
        }
        ConnectionState::Disconnected { error, retry_at } => {
            // Reading the clock refreshes the countdown every second.
            let _tick = now();
            let wait = retry_at.saturating_duration_since(Instant::now());
            let detail = format!("{error}. Retrying in {}.", format::duration(wait));
            (
                "connection disconnected",
                format!("Not connected to {machine}"),
                Some(detail),
            )
        }
    };
    rsx! {
        div { class: "sidebar-foot",
            span { class,
                span { class: "connection-dot", aria_hidden: true }
                "{line}"
            }
            if let Some(detail) = detail {
                span { class: "connection-error", "{detail}" }
            }
        }
    }
}
