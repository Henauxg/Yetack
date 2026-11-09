//! The blocks at the top right of the chat: tasks, background work and MCP servers. Each folds to
//! one line. While the file view is open they become badges on the chat's right edge, and a
//! click opens one as a popover.

use std::cmp::Reverse;
use std::time::{Duration, SystemTime};

use cockpit_protocol::{
    AgentId, AgentStatus, BackgroundTask, BackgroundTaskId, BackgroundTaskKind,
    BackgroundTaskStatus, ItemId, TaskListItem, TaskStatus,
};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::machine::MachineId;
use crate::state::{Block, actions, agent_map};
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
    let finished_rows = if state.ui.finished_tasks_unfolded.contains(&agent) {
        FinishedRows::All
    } else {
        FinishedRows::Hidden
    };
    let alive = entry.summary.status != AgentStatus::Exited;
    let known = state
        .machines
        .get(MachineId::of(agent))
        .unwrap_or(&state.machines.this);
    let hidden = known.settings.hidden_mcp_servers.clone();
    let mcp_view = state.ui.mcp.clone();
    let machine = known.daemon.machine.clone();
    let tasks = details.tasks;
    let background = details.background_tasks;
    let subagent_tasks: Vec<SubagentTask> = details
        .subagents
        .iter()
        .filter_map(|subagent| {
            Some(SubagentTask {
                task: subagent.task.clone()?,
                item: subagent.item,
            })
        })
        .collect();
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
                    RunningBody { agent, tasks: background, finished_rows, subagent_tasks }
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

/// Which finished background tasks the block lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinishedRows {
    /// None: the block lists the running tasks alone, until "Show finished".
    Hidden,
    All,
}

/// The background block's rows: every running task in the order they started, then the finished
/// ones, the latest to end first.
#[derive(Debug)]
struct BackgroundRows {
    tasks: Vec<BackgroundTask>,
    /// The finished tasks left out.
    hidden: usize,
    /// Finished tasks exist: the block offers to fold or unfold them.
    foldable: bool,
}

impl BackgroundRows {
    fn new(tasks: Vec<BackgroundTask>, finished_rows: FinishedRows) -> Self {
        let (mut running, mut finished): (Vec<BackgroundTask>, Vec<BackgroundTask>) = tasks
            .into_iter()
            .partition(|task| task.status == BackgroundTaskStatus::Running);
        running.sort_by_key(|task| task.started_at);
        finished.sort_by_key(|task| Reverse(task.ended_at.unwrap_or(task.started_at)));
        let foldable = !finished.is_empty();
        let hidden = match finished_rows {
            FinishedRows::Hidden => finished.len(),
            FinishedRows::All => {
                running.extend(finished);
                0
            }
        };
        Self {
            tasks: running,
            hidden,
            foldable,
        }
    }
}

fn elapsed(task: &BackgroundTask, now: SystemTime) -> Option<Duration> {
    let end = match task.ended_at {
        Some(ended) => ended.to_system_time()?,
        None => now,
    };
    let start = task.started_at.to_system_time()?;
    // The daemon's clock may run ahead of the laptop's: a task that starts later than now has
    // just started.
    Some(end.duration_since(start).unwrap_or(Duration::ZERO))
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

/// A subagent's background task.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SubagentTask {
    task: BackgroundTaskId,
    /// The subagent's item.
    item: ItemId,
}

#[component]
fn RunningBody(
    agent: AgentId,
    tasks: Vec<BackgroundTask>,
    finished_rows: FinishedRows,
    /// The subagents' tasks, which the map opens on.
    subagent_tasks: Vec<SubagentTask>,
) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let now = now();
    let BackgroundRows {
        tasks,
        hidden,
        foldable,
    } = BackgroundRows::new(tasks, finished_rows);
    let fold_text = if hidden > 0 {
        format!("Show {hidden} finished")
    } else {
        "Show less".to_owned()
    };
    rsx! {
        div { class: "running-list scroll",
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
                    let subagent = subagent_tasks
                        .iter()
                        .find(|subagent_task| subagent_task.task == task.id)
                        .map(|subagent_task| subagent_task.item);
                    rsx! {
                        div { key: "{task.id.as_str()}", class: state_class,
                            span { class: "task-icon", Icon { kind: icon, size: 14 } }
                            span { class: "task-text",
                                match subagent {
                                    Some(item) => rsx! {
                                        button {
                                            class: "task-description task-link",
                                            title: "Show it in the agent map",
                                            onclick: move |_| cockpit.update(|state| agent_map::open_on(state, agent, item)),
                                            "{task.description}"
                                        }
                                    },
                                    None => rsx! {
                                        span { class: "task-description", "{task.description}" }
                                    },
                                }
                                span { class: "task-meta",
                                    span { class: "task-state-dot", aria_hidden: true }
                                    "{meta}"
                                }
                            }
                            if running {
                                button {
                                    class: "stop-button",
                                    aria_label: "{stop_label}",
                                    title: "{stop_label}",
                                    onclick: move |_| {
                                        let id = id.clone();
                                        cockpit.change(|_| actions::stop_task(agent, id));
                                    },
                                    span { class: "stop-square", aria_hidden: true }
                                }
                            }
                        }
                    }
                }
            }
        }
        div { class: "running-foot",
            if foldable {
                button {
                    class: "running-fold",
                    aria_expanded: hidden == 0,
                    onclick: move |_| cockpit.update(|state| state.ui.toggle_finished_tasks(agent)),
                    "{fold_text}"
                }
            }
            span { class: "spacer" }
            button {
                class: "running-fold map-link",
                title: "Open the agent map (Ctrl+M)",
                onclick: move |_| cockpit.update(|state| agent_map::open_on_agent(state, agent)),
                Icon { kind: IconKind::Map, size: 13 }
                "Agent map"
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
    fn a_task_started_by_a_clock_ahead_of_the_laptops_reads_zero_seconds() {
        let behind = SystemTime::UNIX_EPOCH + Duration::from_secs(997);
        let command = task(BackgroundTaskStatus::Running, BackgroundTaskKind::Command);

        assert_eq!(task_meta(&command, behind), "running, 0 s");
    }

    /// A task started and ended that many seconds after the epoch; `ended` is `None` while it runs.
    fn timed(
        id: &str,
        status: BackgroundTaskStatus,
        started: u64,
        ended: Option<u64>,
    ) -> BackgroundTask {
        BackgroundTask {
            id: BackgroundTaskId::new(id.to_owned()),
            started_at: Timestamp::from_unix_millis(started * 1_000),
            ended_at: ended.map(|ended| Timestamp::from_unix_millis(ended * 1_000)),
            ..task(status, BackgroundTaskKind::Agent)
        }
    }

    fn ids(rows: &BackgroundRows) -> Vec<&str> {
        rows.tasks.iter().map(|task| task.id.as_str()).collect()
    }

    #[test]
    fn running_tasks_come_first_as_they_started_then_the_latest_finished() {
        use BackgroundTaskStatus::{Done, Running, Stopped};
        // In the daemon's order: the order the tasks started.
        let tasks = vec![
            timed("early-done", Done, 10, Some(90)),
            timed("first-running", Running, 20, None),
            timed("late-done", Done, 30, Some(40)),
            timed("stopped", Stopped, 50, Some(60)),
            timed("second-running", Running, 70, None),
        ];

        let rows = BackgroundRows::new(tasks, FinishedRows::All);

        assert_eq!(
            ids(&rows),
            [
                "first-running",
                "second-running",
                "early-done",
                "stopped",
                "late-done"
            ]
        );
        assert_eq!((rows.hidden, rows.foldable), (0, true));

        let running_only =
            BackgroundRows::new(vec![timed("only", Running, 0, None)], FinishedRows::All);
        assert_eq!((running_only.hidden, running_only.foldable), (0, false));
    }

    #[test]
    fn the_fold_lists_the_running_tasks_alone() {
        use BackgroundTaskStatus::{Done, Failed, Running};
        let tasks = vec![
            timed("running", Running, 0, None),
            timed("failed", Failed, 1, Some(10)),
            timed("done-11", Done, 2, Some(11)),
            timed("done-12", Done, 3, Some(12)),
            timed("done-13", Done, 4, Some(13)),
            timed("done-14", Done, 5, Some(14)),
        ];

        let folded = BackgroundRows::new(tasks.clone(), FinishedRows::Hidden);
        assert_eq!(ids(&folded), ["running"]);
        assert_eq!((folded.hidden, folded.foldable), (5, true));

        let unfolded = BackgroundRows::new(tasks.clone(), FinishedRows::All);
        assert_eq!(unfolded.tasks.len(), 6);
        assert_eq!((unfolded.hidden, unfolded.foldable), (0, true));

        // The head counts every task, the folded ones too.
        let head = BackgroundHead::new(&tasks);
        assert_eq!(
            (head.summary.as_str(), head.badge.as_str()),
            ("1 running", "1")
        );
        let ended: Vec<BackgroundTask> = tasks.into_iter().skip(1).collect();
        let head = BackgroundHead::new(&ended);
        assert_eq!(
            (head.title, head.summary.as_str()),
            ("Background", "4 done, 1 failed")
        );
        assert_eq!(head.badge, "5");
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
