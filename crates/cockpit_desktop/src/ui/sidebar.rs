//! The agent list: compact two-line rows in the order the user sets, and the connection's state.

use std::time::Instant;

use cockpit_protocol::{AgentId, AgentStatus};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::state::{AgentEntry, ConnectionState, actions};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::reorder::{LIST_ID, ROW_CLASS, row_id};
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
}

impl AgentRow {
    fn new(entry: &AgentEntry, selected: bool, unread: bool) -> Self {
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

#[component]
pub fn Sidebar() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let rows: Vec<AgentRow> = state
        .agents_in_order()
        .into_iter()
        .map(|entry| {
            AgentRow::new(
                entry,
                state.ui.selected == Some(entry.id()),
                state.is_unread(entry.id()),
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
                    onclick: move |_| cockpit.update(actions::toggle_switcher),
                    Icon { kind: IconKind::Search, size: 16 }
                }
                button {
                    class: "icon-button raised",
                    title: "New agent (Ctrl+N)",
                    aria_label: "New agent (Ctrl+N)",
                    onclick: move |_| cockpit.update(actions::open_new_agent),
                    Icon { kind: IconKind::Plus, size: 16 }
                }
            }
            div { id: LIST_ID, class: "agent-list scroll",
                if rows.is_empty() {
                    p { class: "sidebar-empty", "No agent yet. Ctrl+N starts one." }
                }
                for row in rows {
                    AgentRowView { key: "{row.id.get()}", row }
                }
            }
            ConnectionFooter {}
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
    let class = classes.join(" ");
    rsx! {
        button {
            id: row_id(agent),
            class,
            aria_current: row.selected,
            onclick: move |_| cockpit.update(|state| actions::select_agent(state, agent)),
            span { class: "row-line",
                span { class: "status-dot", aria_hidden: true }
                span { class: "row-title", "{row.title}" }
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
