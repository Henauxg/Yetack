//! The blocks at the top right of the chat: tasks, background work and MCP servers. Each folds to
//! one line. While the file view is open they become badges on the chat's right edge, and a
//! click opens one as a popover.

use std::time::{Duration, SystemTime};

use cockpit_protocol::{
    AgentId, AgentStatus, BackgroundTask, BackgroundTaskKind, BackgroundTaskStatus, TaskListItem,
    TaskStatus,
};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::state::{Block, actions};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::mcp::{McpPanel, mcp_summary};

#[component]
pub fn FloatingBlocks() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let Some(entry) = state.selected() else {
        return rsx! {};
    };
    let agent = entry.id();
    let details = entry.details.loaded().cloned().unwrap_or_default();
    let badge_mode = state.ui.file_view(agent).is_some();
    let badge = state.ui.badge;
    let open_blocks = state.ui.open_blocks.clone();
    let alive = entry.summary.status != AgentStatus::Exited;
    let hidden = state.settings.hidden_mcp_servers.clone();
    let mcp_view = state.ui.mcp.clone();
    let machine = state.daemon.machine.clone();
    let tasks = details.tasks;
    let background = details.background_tasks;
    let servers = details.mcp_servers;
    let done = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .count();
    let tasks_summary = format!("{done} of {}", tasks.len());
    let tasks_badge = format!("{done}/{}", tasks.len());
    let background_head = BackgroundHead::new(&background);
    let (mcp_alert, mcp_text) = mcp_summary(alive, &servers, &hidden);
    let is_open = |block: Block| {
        if badge_mode {
            badge == Some(block)
        } else {
            open_blocks.contains(&block)
        }
    };
    let is_shown =
        |block: Block, has_content: bool| has_content && (!badge_mode || badge == Some(block));
    let mcp_wide = is_open(Block::Mcp) && mcp_view.detail.is_some();
    let show_tasks = is_shown(Block::Tasks, !tasks.is_empty());
    let show_running = is_shown(Block::Running, !background.is_empty());
    let show_mcp = is_shown(Block::Mcp, true);
    let tasks_open = is_open(Block::Tasks);
    let running_open = is_open(Block::Running);
    let mcp_open = is_open(Block::Mcp);
    let has_tasks = !tasks.is_empty();
    let has_background = !background.is_empty();
    rsx! {
        div { class: if badge_mode { "blocks beside-badges" } else { "blocks" },
            if show_tasks {
                BlockCard {
                    agent,
                    block: Block::Tasks,
                    title: "Tasks",
                    icon: IconKind::Tasks,
                    running: false,
                    summary: tasks_summary.clone(),
                    alert: false,
                    open: tasks_open,
                    badge_mode,
                    wide: false,
                    TasksBody { tasks }
                }
            }
            if show_running {
                BlockCard {
                    agent,
                    block: Block::Running,
                    title: background_head.title,
                    icon: background_head.icon(),
                    running: background_head.running,
                    summary: background_head.summary.clone(),
                    alert: false,
                    open: running_open,
                    badge_mode,
                    wide: false,
                    RunningBody { agent, tasks: background }
                }
            }
            if show_mcp {
                BlockCard {
                    agent,
                    block: Block::Mcp,
                    title: "MCP servers",
                    icon: IconKind::Plug,
                    running: false,
                    summary: mcp_text.clone(),
                    alert: mcp_alert,
                    open: mcp_open,
                    badge_mode,
                    wide: mcp_wide,
                    McpPanel { agent, alive, servers, hidden, view: mcp_view, machine }
                }
            }
        }
        if badge_mode {
            div { class: "badges", role: "toolbar", aria_label: "Agent activity", aria_orientation: "vertical",
                if has_tasks {
                    Badge { agent, block: Block::Tasks, pressed: badge == Some(Block::Tasks), icon: IconKind::Tasks, running: false, text: tasks_badge, label: "Tasks, {tasks_summary} done", alert: false }
                }
                if has_background {
                    Badge { agent, block: Block::Running, pressed: badge == Some(Block::Running), icon: background_head.icon(), running: background_head.running, text: background_head.badge, label: "{background_head.label}, {background_head.summary}", alert: false }
                }
                Badge { agent, block: Block::Mcp, pressed: badge == Some(Block::Mcp), icon: IconKind::Plug, running: false, text: "MCP", label: "MCP servers, {mcp_text}", alert: mcp_alert }
            }
        }
    }
}

/// The head of the background block and its badge: the spinner and "Running" only while a task
/// runs.
#[derive(Debug)]
struct BackgroundHead {
    running: bool,
    title: &'static str,
    /// The badge's tooltip, before the summary.
    label: &'static str,
    summary: String,
    badge: String,
}

impl BackgroundHead {
    fn new(tasks: &[BackgroundTask]) -> Self {
        let count = |status: BackgroundTaskStatus| {
            tasks.iter().filter(|task| task.status == status).count()
        };
        let running = count(BackgroundTaskStatus::Running);
        if running > 0 {
            return Self {
                running: true,
                title: "Running",
                label: "Running in the background",
                summary: format!("{running} running"),
                badge: running.to_string(),
            };
        }
        let ended = [
            (BackgroundTaskStatus::Done, "done"),
            (BackgroundTaskStatus::Failed, "failed"),
            (BackgroundTaskStatus::Stopped, "stopped"),
        ];
        let counts: Vec<String> = ended
            .into_iter()
            .map(|(status, word)| (count(status), word))
            .filter(|(number, _)| *number > 0)
            .map(|(number, word)| format!("{number} {word}"))
            .collect();
        Self {
            running: false,
            title: "Background",
            label: "Background tasks",
            summary: counts.join(", "),
            badge: tasks.len().to_string(),
        }
    }

    fn icon(&self) -> IconKind {
        if self.running {
            IconKind::Running
        } else {
            IconKind::Circle
        }
    }
}

#[component]
fn BlockCard(
    agent: AgentId,
    block: Block,
    title: String,
    icon: IconKind,
    /// Shows the icon in the color of work in progress.
    running: bool,
    summary: String,
    alert: bool,
    open: bool,
    badge_mode: bool,
    wide: bool,
    children: Element,
) -> Element {
    let cockpit: Cockpit = use_context();
    let mut class = vec!["block-card"];
    if wide {
        class.push("wide");
    }
    if running {
        class.push("running-block");
    }
    rsx! {
        section { class: class.join(" "), aria_label: "{title}",
            button {
                class: "block-head",
                aria_expanded: open,
                onclick: move |_| cockpit.change(|state| actions::toggle_block(state, agent, block)),
                span { class: "block-icon", Icon { kind: icon, size: 14 } }
                span { class: "block-title", "{title}" }
                if alert {
                    span { class: "alert-dot", aria_hidden: true }
                }
                span { class: "block-summary", "{summary}" }
                if badge_mode {
                    Icon { kind: IconKind::Close, size: 12 }
                } else {
                    span { class: if open { "chevron open" } else { "chevron" }, Icon { kind: IconKind::ChevronRight, size: 12 } }
                }
            }
            if open {
                div { class: "block-body", {children} }
            }
        }
    }
}

#[component]
fn Badge(
    agent: AgentId,
    block: Block,
    pressed: bool,
    icon: IconKind,
    running: bool,
    text: String,
    label: String,
    alert: bool,
) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        button {
            class: if pressed { "badge pressed" } else { "badge" },
            aria_pressed: pressed,
            aria_label: "{label}",
            title: "{label}",
            onclick: move |_| cockpit.change(|state| actions::toggle_block(state, agent, block)),
            span { class: if running { "badge-icon running" } else { "badge-icon" }, Icon { kind: icon, size: 14 } }
            span { class: "badge-text", "{text}" }
            if alert {
                span { class: "alert-dot badge-alert", aria_hidden: true }
            }
        }
    }
}

#[component]
fn TasksBody(tasks: Vec<TaskListItem>) -> Element {
    let done = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .count();
    let percent = done * 100 / tasks.len().max(1);
    rsx! {
        div { class: "bar small", div { class: "bar-fill done", style: "width: {percent}%" } }
        div { class: "task-list", role: "list",
            for (index, task) in tasks.into_iter().enumerate() {
                {
                    let (class, words) = match task.status {
                        TaskStatus::Completed => ("task done", "done"),
                        TaskStatus::InProgress => ("task doing", "in progress"),
                        TaskStatus::Pending => ("task todo", "not started"),
                    };
                    let text = if task.status == TaskStatus::InProgress && !task.active_form.is_empty() {
                        task.active_form.clone()
                    } else {
                        task.content.clone()
                    };
                    rsx! {
                        div { key: "{index}", class, role: "listitem",
                            if task.status == TaskStatus::Completed {
                                span { class: "task-mark", Icon { kind: IconKind::Check, size: 14 } }
                            } else {
                                span { class: "task-mark", span { class: "task-dot" } }
                            }
                            span {
                                "{text}"
                                span { class: "visually-hidden", ", {words}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn elapsed(task: &BackgroundTask, now: SystemTime) -> Option<Duration> {
    let end = match task.ended_at {
        Some(ended) => ended.to_system_time()?,
        None => now,
    };
    let start = task.started_at.to_system_time()?;
    end.duration_since(start).ok()
}

fn task_meta(task: &BackgroundTask, now: SystemTime) -> String {
    let time = elapsed(task, now)
        .map(|duration| format::duration(Duration::from_secs(duration.as_secs())))
        .unwrap_or_default();
    let calls = format::count(
        usize::try_from(task.tool_calls).unwrap_or(usize::MAX),
        "tool call",
        "tool calls",
    );
    match (task.status, task.kind) {
        (BackgroundTaskStatus::Running, BackgroundTaskKind::Agent) => format!("{calls}, {time}"),
        (BackgroundTaskStatus::Running, BackgroundTaskKind::Command) => format!("running, {time}"),
        (BackgroundTaskStatus::Stopped, _) => "Stopped".to_owned(),
        (BackgroundTaskStatus::Done, _) => format!("Done in {time}"),
        (BackgroundTaskStatus::Failed, _) => format!("Failed after {time}"),
    }
}

#[component]
fn RunningBody(agent: AgentId, tasks: Vec<BackgroundTask>) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let now = now();
    rsx! {
        div { class: "running-list",
            for task in tasks {
                {
                    let meta = task_meta(&task, now);
                    let running = task.status == BackgroundTaskStatus::Running;
                    let state_class = match task.status {
                        BackgroundTaskStatus::Running => "background-task running",
                        BackgroundTaskStatus::Stopped => "background-task stopped",
                        BackgroundTaskStatus::Done => "background-task done",
                        BackgroundTaskStatus::Failed => "background-task failed",
                    };
                    let icon = match task.kind {
                        BackgroundTaskKind::Agent => IconKind::Person,
                        BackgroundTaskKind::Command => IconKind::Terminal,
                    };
                    let stop_label = format!("Stop: {}", task.description);
                    let id = task.id.clone();
                    rsx! {
                        div { key: "{task.id.as_str()}", class: state_class,
                            span { class: "task-icon", Icon { kind: icon, size: 14 } }
                            span { class: "task-text",
                                span { class: "task-description", "{task.description}" }
                                span { class: "task-meta",
                                    span { class: "task-state-dot", aria_hidden: true }
                                    "{meta}"
                                }
                            }
                            if running {
                                button {
                                    class: "round-button",
                                    aria_label: "{stop_label}",
                                    title: "{stop_label}",
                                    onclick: move |_| {
                                        let id = id.clone();
                                        cockpit.change(|_| actions::stop_task(agent, id));
                                    },
                                    Icon { kind: IconKind::StopSmall, size: 10 }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{BackgroundTaskId, Timestamp};

    use super::*;

    fn task(status: BackgroundTaskStatus, kind: BackgroundTaskKind) -> BackgroundTask {
        BackgroundTask {
            id: BackgroundTaskId::new("task-1".to_owned()),
            kind,
            description: "Review the parser".to_owned(),
            status,
            started_at: Timestamp::from_unix_millis(1_000_000),
            ended_at: None,
            tool_calls: 5,
        }
    }

    #[test]
    fn running_tasks_count_their_time_and_ended_ones_stop_counting() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_041);
        let agent = task(BackgroundTaskStatus::Running, BackgroundTaskKind::Agent);
        let command = task(BackgroundTaskStatus::Running, BackgroundTaskKind::Command);
        let mut done = task(BackgroundTaskStatus::Done, BackgroundTaskKind::Command);
        done.ended_at = Some(Timestamp::from_unix_millis(1_012_000));

        assert_eq!(task_meta(&agent, now), "5 tool calls, 41 s");
        assert_eq!(task_meta(&command, now), "running, 41 s");
        assert_eq!(task_meta(&done, now), "Done in 12 s");
    }

    #[test]
    fn the_background_head_says_running_only_while_a_task_runs() {
        let running = task(BackgroundTaskStatus::Running, BackgroundTaskKind::Agent);
        let stopped = task(BackgroundTaskStatus::Stopped, BackgroundTaskKind::Command);
        let done = task(BackgroundTaskStatus::Done, BackgroundTaskKind::Command);

        let head = BackgroundHead::new(&[running, stopped.clone()]);
        assert_eq!((head.title, head.icon()), ("Running", IconKind::Running));
        assert_eq!(
            (head.summary.as_str(), head.badge.as_str()),
            ("1 running", "1")
        );

        let head = BackgroundHead::new(std::slice::from_ref(&stopped));
        assert_eq!((head.title, head.icon()), ("Background", IconKind::Circle));
        assert!(!head.running);
        assert_eq!(
            (head.summary.as_str(), head.badge.as_str()),
            ("1 stopped", "1")
        );

        let head = BackgroundHead::new(&[stopped, done.clone(), done]);
        assert_eq!(head.summary, "2 done, 1 stopped");
        assert_eq!(head.badge, "3");
    }
}
