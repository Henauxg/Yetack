//! The agent map dialog (Ctrl+M): the outline of the agent and its subagents on the left, the
//! selected row's details on the right.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use cockpit_protocol::{
    AgentId, AgentStatus, ItemBody, ItemId, Subagent, SubagentRun, SubagentStatus, Timestamp,
    ToolCall, TranscriptItem,
};
use dioxus::prelude::*;
use serde_json::Value;

use crate::bridge::{self, HIGHLIGHTED_CLASS};
use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::keymap::Command;
use crate::state::agent_map::{self, MapRow, MapTab, MapView, OutlineRow, RowKind, StopKind};
use crate::state::{self, HistoryState, actions};
use crate::ui::cards::{ApprovalCard, PlanCard, QuestionCard};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::markdown::MarkdownView;
use crate::ui::transcript::{self, CallStage, CompactItem, ItemView};

/// The input field of an `Agent` call that holds its prompt.
const PROMPT_FIELD: &str = "prompt";

/// One level of the outline's indent, in pixels. The stylesheet draws its guide lines at the
/// same step.
const INDENT_PX: u16 = 20;

/// What a row's second line says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SecondLine {
    /// The subagent's latest tool call.
    Tool {
        tool: String,
        summary: String,
    },
    /// The subagent waits for a subagent it started.
    WaitsFor {
        item: ItemId,
        description: String,
    },
    /// Its approval, question or plan waits for you.
    WaitsForYou(String),
    Failed(String),
    Stopped,
    Plain(String),
}

/// A row of the outline, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowView {
    row: MapRow,
    depth: u16,
    folded: Option<bool>,
    /// The status class of the row's dot; `None` for a row without one.
    status: Option<&'static str>,
    title: String,
    agent_type: Option<String>,
    tags: Vec<String>,
    second: Option<SecondLine>,
    calls: String,
    time: String,
    tokens: String,
}

/// What each subagent waits for you about, by the subagent's item.
fn waiting_by_subagent<'item>(
    items: impl Iterator<Item = &'item TranscriptItem>,
) -> BTreeMap<ItemId, String> {
    let mut waiting = BTreeMap::new();
    for item in items {
        let Some(parent) = item.parent.filter(|_| transcript::waits_for_answer(item)) else {
            continue;
        };
        let what = match &item.body {
            ItemBody::Approval(approval) => format!("approve {}", approval.tool),
            ItemBody::Question(_) => "answer a question".to_owned(),
            ItemBody::Plan(_) => "review a plan".to_owned(),
            _ => continue,
        };
        waiting.insert(parent, what);
    }
    waiting
}

/// How long the subagent ran, or runs so far.
fn run_time(started: Timestamp, ended: Option<Timestamp>, now: SystemTime) -> Option<Duration> {
    let end = match ended {
        Some(ended) => ended.to_system_time()?,
        None => now,
    };
    // The daemon's clock may run ahead of the laptop's: a subagent that starts later than now
    // has just started.
    let elapsed = end
        .duration_since(started.to_system_time()?)
        .unwrap_or(Duration::ZERO);
    Some(Duration::from_secs(elapsed.as_secs()))
}

fn subagent_time(subagent: &Subagent, now: SystemTime) -> Option<Duration> {
    if subagent.ended_at.is_none() && !subagent.status.is_running() {
        return None;
    }
    run_time(subagent.started_at, subagent.ended_at, now)
}

fn second_line(
    subagent: &Subagent,
    by_item: &BTreeMap<ItemId, &Subagent>,
    waiting: &BTreeMap<ItemId, String>,
) -> Option<SecondLine> {
    match subagent.status {
        SubagentStatus::NeedsYou => Some(SecondLine::WaitsForYou(
            waiting.get(&subagent.item).map_or_else(
                || "Waits for your answer".to_owned(),
                |what| format!("Waits for you to {what}"),
            ),
        )),
        SubagentStatus::Running => {
            let call = subagent.last_tool.as_ref()?;
            let started = call.item.and_then(|item| by_item.get(&item));
            match started {
                Some(child) if child.status.is_running() => Some(SecondLine::WaitsFor {
                    item: child.item,
                    description: child.description.clone(),
                }),
                Some(_) | None => Some(SecondLine::Tool {
                    tool: call.tool.clone(),
                    summary: call.summary.clone(),
                }),
            }
        }
        SubagentStatus::Failed => Some(SecondLine::Failed(
            subagent
                .error
                .clone()
                .unwrap_or_else(|| "Failed".to_owned()),
        )),
        SubagentStatus::Stopped => Some(SecondLine::Stopped),
        SubagentStatus::Done => None,
    }
}

/// What the agent's own row says under its title.
fn agent_line(status: AgentStatus, turn: u32, running: usize) -> String {
    match status {
        AgentStatus::Working | AgentStatus::NeedsYou if running > 0 => {
            format!("Turn {turn}, waiting for its subagents")
        }
        AgentStatus::Working => format!("Turn {turn}, working"),
        AgentStatus::NeedsYou => format!("Turn {turn}, waiting for you"),
        AgentStatus::Idle if turn == 0 => "No turn yet".to_owned(),
        AgentStatus::Idle => format!("Turn {turn} ended"),
        AgentStatus::Exited => "Exited".to_owned(),
    }
}

/// The facts the rows and the details need about the agent.
struct AgentFacts<'facts> {
    title: &'facts str,
    status: AgentStatus,
    turn: u32,
    turn_time: Option<Duration>,
}

fn row_views(
    rows: &[OutlineRow<'_>],
    subagents: &[Subagent],
    agent: &AgentFacts<'_>,
    waiting: &BTreeMap<ItemId, String>,
    now: SystemTime,
) -> Vec<RowView> {
    let by_item: BTreeMap<ItemId, &Subagent> = subagents
        .iter()
        .map(|subagent| (subagent.item, subagent))
        .collect();
    let in_turn = agent_map::turn_subagents(subagents, agent.turn);
    let running = in_turn
        .iter()
        .filter(|subagent| subagent.status.is_running())
        .count();
    let duration = |time: Option<Duration>| time.map(format::duration).unwrap_or_default();
    rows.iter()
        .map(|row| {
            let base = RowView {
                row: row.row,
                depth: row.depth,
                folded: row.folded,
                status: None,
                title: String::new(),
                agent_type: None,
                tags: Vec::new(),
                second: None,
                calls: String::new(),
                time: String::new(),
                tokens: String::new(),
            };
            match row.kind {
                RowKind::Agent => RowView {
                    status: Some(format::status_class(agent.status)),
                    title: agent.title.to_owned(),
                    tags: vec!["this agent".to_owned()],
                    second: Some(SecondLine::Plain(agent_line(
                        agent.status,
                        agent.turn,
                        running,
                    ))),
                    time: duration(agent.turn_time),
                    tokens: agent_map::total_tokens(in_turn.iter().copied())
                        .map(format::tokens)
                        .unwrap_or_default(),
                    ..base
                },
                RowKind::Subagent(subagent) => {
                    let mut tags = Vec::new();
                    if subagent.run == SubagentRun::Background {
                        tags.push("background".to_owned());
                    }
                    if !agent_map::in_current_turn(subagent, agent.turn) {
                        tags.push(format!("turn {}", subagent.turn));
                    }
                    RowView {
                        status: Some(format::subagent_status_class(subagent.status)),
                        title: subagent.description.clone(),
                        agent_type: subagent.agent_type.clone(),
                        tags,
                        second: second_line(subagent, &by_item, waiting),
                        calls: subagent.tool_calls.to_string(),
                        time: duration(subagent_time(subagent, now)),
                        tokens: subagent.tokens.map(format::tokens).unwrap_or_default(),
                        ..base
                    }
                }
                RowKind::Earlier { turns, subagents } => RowView {
                    title: format!(
                        "Earlier turns: {}, {}",
                        format::count(turns, "turn", "turns"),
                        format::count(subagents, "subagent", "subagents")
                    ),
                    ..base
                },
                RowKind::More { count, all_done } => RowView {
                    title: if all_done {
                        format!("{count} more done")
                    } else {
                        format!("{count} more ended")
                    },
                    ..base
                },
            }
        })
        .collect()
}

/// A count the header shows, with the class that colors its dot.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusCount {
    class: &'static str,
    text: String,
}

/// The counts the header shows for the current turn: "2 running", "3 done".
fn status_counts(subagents: &[&Subagent]) -> Vec<StatusCount> {
    let words = [
        (SubagentStatus::NeedsYou, "waiting for you"),
        (SubagentStatus::Running, "running"),
        (SubagentStatus::Done, "done"),
        (SubagentStatus::Failed, "failed"),
        (SubagentStatus::Stopped, "stopped"),
    ];
    let mut counts = Vec::new();
    for (status, word) in words {
        let number = subagents
            .iter()
            .filter(|subagent| subagent.status == status)
            .count();
        if number > 0 {
            counts.push(StatusCount {
                class: format::subagent_status_class(status),
                text: format!("{number} {word}"),
            });
        }
    }
    counts
}

#[component]
pub fn AgentMapDialog(view: MapView) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let now = now();
    let mut scrolled_to = use_hook(|| CopyValue::new(None::<MapRow>));
    if scrolled_to() != Some(view.selected) {
        scrolled_to.set(Some(view.selected));
        bridge::scroll_to_highlighted();
    }
    let state = cockpit.state.read();
    let Some(entry) = state.agents.get(&view.agent) else {
        return rsx! {};
    };
    let agent = view.agent;
    let folder = entry.summary.folder.clone();
    let subagents: &[Subagent] = entry
        .details
        .loaded()
        .map_or(&[], |details| details.subagents.as_slice());
    let facts = AgentFacts {
        title: &entry.summary.title,
        status: entry.summary.status,
        turn: agent_map::current_turn(&entry.summary),
        turn_time: entry.turn_start().map(|start| start.elapsed(now)),
    };
    let waiting = waiting_by_subagent(entry.transcript.items());
    let rows = agent_map::outline(subagents, facts.turn, &view);
    let row_views = row_views(&rows, subagents, &facts, &waiting, now);
    let in_turn = agent_map::turn_subagents(subagents, facts.turn);
    let counts = status_counts(&in_turn);
    let total = agent_map::total_tokens(in_turn.iter().copied())
        .map(|tokens| format!("{} tokens", format::tokens(tokens)));
    let only_live = view.only_live;
    let title = facts.title.to_owned();
    let details = match view.selected {
        MapRow::Subagent(item) => {
            subagents
                .iter()
                .find(|subagent| subagent.item == item)
                .map(|subagent| {
                    subagent_details(SubagentDetailsInput {
                        agent,
                        folder: folder.clone(),
                        subagent,
                        subagents,
                        items: entry.transcript.items().collect(),
                        has_older: matches!(
                            entry.transcript.history(),
                            HistoryState::Loaded { has_older: true }
                        ),
                        tab: view.tab,
                    })
                })
        }
        MapRow::Agent | MapRow::Earlier | MapRow::More(_) => None,
    };
    let earlier = subagents.len().saturating_sub(in_turn.len());
    let root = RootSummary {
        title: title.clone(),
        status: facts.status,
        turn: facts.turn,
        counts: counts.iter().map(|count| count.text.clone()).collect(),
        started: in_turn.len(),
        earlier,
    };
    let map_keys = cockpit.state.read().prefs.keys.shown(Command::AgentMap);
    rsx! {
        div {
            class: "dialog agent-map",
            role: "dialog",
            aria_label: "Agent map",
            onclick: move |event| event.stop_propagation(),
            div { class: "map-head",
                span { class: "map-head-icon", Icon { kind: IconKind::Map, size: 18 } }
                h2 { "Agent map" }
                span { class: "map-agent", "{title}" }
                span { class: "spacer" }
                span { class: "map-stats",
                    for StatusCount { class, text } in counts {
                        span { key: "{text}", class: "map-stat {class}",
                            span { class: "status-dot", aria_hidden: true }
                            "{text}"
                        }
                    }
                    if let Some(total) = total {
                        span { class: "map-stat", "{total}" }
                    }
                }
                button {
                    class: if only_live { "chip map-filter open" } else { "chip map-filter" },
                    aria_pressed: only_live,
                    title: "Show only the running and failed subagents",
                    onclick: move |_| cockpit.update(agent_map::toggle_only_live),
                    "Running and failed only"
                }
                button {
                    class: "icon-button small",
                    aria_label: "Close",
                    onclick: move |_| cockpit.update(|state| state.ui.dialog = None),
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            div { class: "map-body",
                div { class: "map-outline",
                    div { class: "map-columns",
                        span { "Agent and subagents" }
                        span { class: "map-col", "Calls" }
                        span { class: "map-col", "Time" }
                        span { class: "map-col", "Tokens" }
                    }
                    div {
                        class: "map-tree scroll",
                        role: "tree",
                        aria_label: "Agent and subagents",
                        tabindex: "0",
                        // The keys go to the map, not to a button behind the dialog that kept the
                        // focus: Enter would press it.
                        onmounted: move |event| async move {
                            // The dialog may have closed already: nothing to focus then.
                            let _ = event.data().set_focus(true).await;
                        },
                        for row in row_views {
                            OutlineRowView { key: "{row.row:?}", selected: row.row == view.selected, row }
                        }
                    }
                }
                div { class: "map-details scroll",
                    match details {
                        Some(details) => details,
                        None => rsx! {
                            RootDetails { summary: root }
                        },
                    }
                }
            }
            div { class: "map-foot",
                span { "Up and Down to move" }
                span { "Left and Right to fold" }
                span { "Esc to close" }
                span { class: "spacer" }
                span { "{map_keys} opens the map" }
            }
        }
    }
}

#[component]
fn OutlineRowView(row: RowView, selected: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let target = row.row;
    let mut class = vec!["map-row"];
    if let Some(status) = row.status {
        class.push(status);
    }
    if row.status.is_none() {
        class.push("group");
    }
    if row.row == MapRow::Agent {
        class.push("root");
    }
    if selected {
        class.push(HIGHLIGHTED_CLASS);
    }
    let indent = row.depth.saturating_mul(INDENT_PX);
    let level = row.depth.saturating_add(1);
    rsx! {
        div {
            class: class.join(" "),
            role: "treeitem",
            aria_selected: selected,
            aria_level: "{level}",
            aria_expanded: row.folded.map(|folded| !folded),
            style: "--indent: {indent}px",
            onclick: move |_| cockpit.update(|state| agent_map::click_row(state, target)),
            div { class: "map-row-main", style: "padding-left: {indent}px",
                match (row.folded, target) {
                    (Some(folded), _) => rsx! {
                        button {
                            class: "map-toggle",
                            aria_label: if folded { "Unfold" } else { "Fold" },
                            onclick: move |event| {
                                event.stop_propagation();
                                cockpit.update(|state| agent_map::toggle_fold(state, target, folded));
                            },
                            span { class: if folded { "chevron" } else { "chevron open" }, Icon { kind: IconKind::ChevronRight, size: 12 } }
                        }
                    },
                    (None, MapRow::Agent) => rsx! {
                        span { class: "map-toggle agent-glyph", Icon { kind: IconKind::Person, size: 13 } }
                    },
                    (None, _) => rsx! {
                        span { class: "map-toggle" }
                    },
                }
                if row.status.is_some() {
                    span { class: "status-dot", aria_hidden: true }
                }
                div { class: "map-row-text",
                    div { class: "map-line",
                        span { class: if row.status.is_some() { "map-title" } else { "map-title group-title" }, "{row.title}" }
                        if let Some(agent_type) = &row.agent_type {
                            span { class: "map-type mono", "{agent_type}" }
                        }
                        for tag in row.tags.iter() {
                            span { key: "{tag}", class: "map-tag", "{tag}" }
                        }
                    }
                    if let Some(second) = row.second.clone() {
                        SecondLineView { second }
                    }
                }
            }
            span { class: "map-col", "{row.calls}" }
            span { class: "map-col time", "{row.time}" }
            span { class: "map-col", "{row.tokens}" }
        }
    }
}

#[component]
fn SecondLineView(second: SecondLine) -> Element {
    let cockpit: Cockpit = use_context();
    match second {
        SecondLine::Tool { tool, summary } => rsx! {
            div { class: "map-line second",
                span { class: "map-tool", "{tool}" }
                span { class: "map-tool-summary mono", "{summary}" }
            }
        },
        SecondLine::WaitsFor { item, description } => rsx! {
            div { class: "map-line second",
                span { "Waits for" }
                button {
                    class: "map-ref",
                    onclick: move |event| {
                        event.stop_propagation();
                        cockpit.update(|state| agent_map::select_subagent(state, item));
                    },
                    "{description}"
                }
            }
        },
        SecondLine::WaitsForYou(text) => rsx! {
            div { class: "map-line second waiting", span { class: "map-line-text", "{text}" } }
        },
        SecondLine::Failed(error) => rsx! {
            div { class: "map-line second failed", span { class: "map-line-text", "{error}" } }
        },
        SecondLine::Stopped => rsx! {
            div { class: "map-line second", span { class: "map-line-text", "Stopped by you" } }
        },
        SecondLine::Plain(text) => rsx! {
            div { class: "map-line second", span { class: "map-line-text", "{text}" } }
        },
    }
}

/// The details of the agent's own row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RootSummary {
    title: String,
    status: AgentStatus,
    turn: u32,
    counts: Vec<String>,
    /// The subagents of the current turn.
    started: usize,
    /// The subagents of earlier turns.
    earlier: usize,
}

#[component]
fn RootDetails(summary: RootSummary) -> Element {
    let status_class = format::status_class(summary.status);
    let label = format::status_label(summary.status);
    let started = if summary.started == 0 {
        "This turn started no subagent.".to_owned()
    } else {
        format!(
            "This turn started {}: {}.",
            format::count(summary.started, "subagent", "subagents"),
            summary.counts.join(", ")
        )
    };
    rsx! {
        div { class: "map-detail-head {status_class}",
            span { class: "status-dot", aria_hidden: true }
            h3 { "{summary.title}" }
        }
        div { class: "map-detail-sub",
            span { class: "status-pill {status_class}",
                span { class: "status-dot", aria_hidden: true }
                "{label}"
            }
            if summary.turn > 0 {
                span { "Turn {summary.turn}" }
            }
        }
        div { class: "map-root-text",
            p { "{started}" }
            if summary.earlier > 0 {
                p { "Earlier turns started {summary.earlier} more." }
            }
            p { class: "dim", "Select a subagent to see the prompt it got, its transcript and its tool calls." }
        }
    }
}

struct SubagentDetailsInput<'input> {
    agent: AgentId,
    folder: PathBuf,
    subagent: &'input Subagent,
    subagents: &'input [Subagent],
    items: Vec<&'input TranscriptItem>,
    has_older: bool,
    tab: MapTab,
}

/// The details of a subagent: what its component needs, taken from the agent's transcript.
fn subagent_details(input: SubagentDetailsInput<'_>) -> Element {
    let SubagentDetailsInput {
        agent,
        folder,
        subagent,
        subagents,
        items,
        has_older,
        tab,
    } = input;
    let by_parent = transcript::children_by_parent(&items);
    let call = items
        .iter()
        .find(|item| item.id == subagent.item)
        .and_then(|item| match &item.body {
            ItemBody::Subagent(call) => Some(call.clone()),
            _ => None,
        });
    let nested = transcript::subtree(&by_parent, subagent.item);
    let started_by = subagent.parent.and_then(|parent| {
        subagents
            .iter()
            .find(|other| other.item == parent)
            .map(|other| StartedBy {
                item: other.item,
                description: other.description.clone(),
            })
    });
    let started_subagents: Vec<Subagent> = subagents
        .iter()
        .filter(|other| other.parent == Some(subagent.item))
        .cloned()
        .collect();
    rsx! {
        SubagentDetails {
            agent,
            folder,
            subagent: subagent.clone(),
            started_by,
            started_subagents,
            call,
            nested,
            has_older,
            tab,
        }
    }
}

/// The subagent that started a subagent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StartedBy {
    item: ItemId,
    description: String,
}

/// The class that colors a tool call's dot in the list of calls.
fn call_stage(body: &ItemBody, awaits_approval: bool) -> &'static str {
    let stage = match body {
        ItemBody::ToolCall(call) => CallStage::new(call, awaits_approval),
        _ => CallStage::Done,
    };
    stage.name()
}

/// The class of a tool line that stands for a subagent: its dot's color.
fn subagent_line_class(status: SubagentStatus) -> String {
    match status {
        SubagentStatus::Running => CallStage::Running.class(),
        SubagentStatus::NeedsYou => CallStage::AwaitsApproval.class(),
        SubagentStatus::Done => CallStage::Done.class(),
        SubagentStatus::Failed => CallStage::Failed.class(),
        SubagentStatus::Stopped => "tool stopped".to_owned(),
    }
}

#[component]
fn SubagentDetails(
    agent: AgentId,
    folder: PathBuf,
    subagent: Subagent,
    /// `None` when the agent started it.
    started_by: Option<StartedBy>,
    started_subagents: Vec<Subagent>,
    /// Its call's item, when the app holds it.
    call: Option<ToolCall>,
    /// Every item under its call, at any depth.
    nested: Vec<TranscriptItem>,
    has_older: bool,
    tab: MapTab,
) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let now = now();
    let item = subagent.item;
    let status_class = format::subagent_status_class(subagent.status);
    let running = subagent.status.is_running();
    let time = subagent_time(&subagent, now)
        .map(format::duration)
        .unwrap_or_default();
    let tokens = subagent.tokens.map(format::tokens);
    let started = format::time_of_day(subagent.started_at).unwrap_or_default();
    let run = match subagent.run {
        SubagentRun::Foreground => "Foreground",
        SubagentRun::Background => "Background",
    };
    let nested_refs: Vec<&TranscriptItem> = nested.iter().collect();
    let by_parent = transcript::children_by_parent(&nested_refs);
    let awaiting = state::awaiting_approval(&nested_refs);
    let direct: Vec<&TranscriptItem> = nested
        .iter()
        .filter(|nested_item| nested_item.parent == Some(item))
        .collect();
    let waiting: Vec<TranscriptItem> = direct
        .iter()
        .filter(|nested_item| transcript::waits_for_answer(nested_item))
        .map(|nested_item| (*nested_item).clone())
        .collect();
    let calls: Vec<&TranscriptItem> = direct
        .iter()
        .copied()
        .filter(|nested_item| {
            matches!(
                nested_item.body,
                ItemBody::ToolCall(_) | ItemBody::Subagent(_)
            )
        })
        .collect();
    let call_count = calls.len();
    let by_child: BTreeMap<ItemId, &Subagent> = started_subagents
        .iter()
        .map(|child| (child.item, child))
        .collect();
    let prompt = call
        .as_ref()
        .and_then(|call| call.input.get(PROMPT_FIELD))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let stop = agent_map::stop_kind(&subagent);
    let loaded = call.is_some();
    let waits_for_child = started_subagents
        .iter()
        .any(|child| child.status.is_running());
    let body = match tab {
        MapTab::Transcript => {
            let entries: Vec<(TranscriptItem, Vec<TranscriptItem>, bool)> = direct
                .iter()
                .filter(|nested_item| !transcript::waits_for_answer(nested_item))
                .map(|nested_item| {
                    (
                        (*nested_item).clone(),
                        transcript::subtree(&by_parent, nested_item.id),
                        awaiting.contains(&nested_item.id),
                    )
                })
                .collect();
            let quiet = entries.is_empty();
            rsx! {
                div { class: "map-transcript",
                    if quiet {
                        p { class: "map-note", "No messages from it yet." }
                    }
                    for (entry, grandchildren, awaits_approval) in entries {
                        match by_child.get(&entry.id).copied() {
                            Some(child) => rsx! {
                                SubagentRef { key: "{entry.id.get()}", child: child.clone() }
                            },
                            None => rsx! {
                                CompactItem { key: "{entry.id.get()}", agent, folder: folder.clone(), item: entry, nested: grandchildren, awaits_approval }
                            },
                        }
                    }
                    if running {
                        div { class: "map-working",
                            span { class: "spinner", role: "status", aria_label: "Running" }
                            if waits_for_child {
                                "Waiting for its subagents, {time}"
                            } else {
                                "Working, {time}"
                            }
                        }
                    }
                }
                if subagent.status == SubagentStatus::Done {
                    if let Some(result) = call.as_ref().and_then(|call| call.output.clone()) {
                        span { class: "map-section", "Result" }
                        p { class: "subagent-result", "{result}" }
                    }
                }
                if let Some(error) = subagent.error.clone().filter(|_| subagent.status == SubagentStatus::Failed) {
                    div { class: "map-error", "{error}" }
                }
                if subagent.status == SubagentStatus::Stopped {
                    p { class: "map-note", "You stopped it." }
                }
            }
        }
        MapTab::Prompt => {
            let sender = started_by.as_ref().map_or_else(
                || "this agent".to_owned(),
                |started_by| started_by.description.clone(),
            );
            rsx! {
                match prompt {
                    Some(prompt) => rsx! {
                        div { class: "map-prompt",
                            MarkdownView { agent, folder: folder.clone(), markdown: prompt }
                        }
                        p { class: "map-note", "Sent by {sender} at {started}, through its Agent call." }
                    },
                    None => rsx! {
                        p { class: "map-note", "The prompt is not in the part of the chat the app holds." }
                    },
                }
            }
        }
        MapTab::ToolCalls => {
            let cards: Vec<(TranscriptItem, bool, &str)> = calls
                .iter()
                .map(|call_item| {
                    let awaits_approval = awaiting.contains(&call_item.id);
                    let stage = call_stage(&call_item.body, awaits_approval);
                    ((*call_item).clone(), awaits_approval, stage)
                })
                .collect();
            rsx! {
                if cards.is_empty() {
                    p { class: "map-note", "No tool calls yet." }
                } else {
                    div { class: "map-calls",
                        for (card, awaits_approval, stage) in cards {
                            match by_child.get(&card.id).copied() {
                                Some(child) => rsx! {
                                    div { key: "{card.id.get()}", class: "map-call",
                                        SubagentRef { child: child.clone() }
                                    }
                                },
                                None => rsx! {
                                    div { key: "{card.id.get()}", class: "map-call {stage}",
                                        span { class: "tool-dot", aria_hidden: true }
                                        div { class: "map-call-body",
                                            ItemView { agent, folder: folder.clone(), item: card, nested: Vec::new(), awaits_approval }
                                        }
                                    }
                                },
                            }
                        }
                    }
                }
            }
        }
    };
    let tab_class = |shown: MapTab| {
        if tab == shown {
            "dialog-tab selected"
        } else {
            "dialog-tab"
        }
    };
    rsx! {
        div { class: "map-detail-head {status_class}",
            span { class: "status-dot", aria_hidden: true }
            h3 { "{subagent.description}" }
            match stop {
                Some(StopKind::Task) => rsx! {
                    button {
                        class: "button small danger",
                        title: "Stop this subagent and the subagents it started",
                        onclick: move |_| cockpit.change(|state| agent_map::stop(state, agent, item)),
                        span { class: "stop-square", aria_hidden: true }
                        "Stop"
                    }
                },
                Some(StopKind::Turn) => rsx! {
                    button {
                        class: "button small danger",
                        title: "Claude Code stops a foreground subagent only with the agent's turn",
                        onclick: move |_| cockpit.change(|state| agent_map::stop(state, agent, item)),
                        span { class: "stop-square", aria_hidden: true }
                        "Stop the turn"
                    }
                },
                None => rsx! {},
            }
        }
        div { class: "map-detail-sub",
            span { class: "status-pill {status_class}",
                span { class: "status-dot", aria_hidden: true }
                "{format::subagent_status_label(subagent.status)}"
            }
            if let Some(agent_type) = &subagent.agent_type {
                span { class: "map-type mono", "{agent_type}" }
            }
            span { "{run}" }
            match started_by.clone() {
                Some(StartedBy { item: parent, description }) => rsx! {
                    span {
                        "Started by "
                        button {
                            class: "link-button",
                            onclick: move |_| cockpit.update(|state| agent_map::select_subagent(state, parent)),
                            "{description}"
                        }
                    }
                },
                None => rsx! {
                    span { "Started by this agent" }
                },
            }
        }
        div { class: "map-facts",
            div { class: "map-fact",
                span { class: "map-fact-label", "Time" }
                span { class: "map-fact-value", "{time}" }
            }
            div { class: "map-fact",
                span { class: "map-fact-label", "Tokens" }
                match tokens {
                    Some(tokens) => rsx! {
                        span { class: "map-fact-value", "{tokens}" }
                    },
                    None => rsx! {
                        span { class: "map-fact-value unknown", title: "Claude Code reported no usage for this subagent", "Not reported" }
                    },
                }
            }
            div { class: "map-fact",
                span { class: "map-fact-label", "Tool calls" }
                span { class: "map-fact-value", "{subagent.tool_calls}" }
            }
            div { class: "map-fact",
                span { class: "map-fact-label", "Started" }
                span { class: "map-fact-value", "{started}" }
            }
        }
        if !waiting.is_empty() {
            div { class: "map-waiting",
                for waiting_item in waiting {
                    match waiting_item.body.clone() {
                        ItemBody::Approval(approval) => rsx! {
                            ApprovalCard { key: "{waiting_item.id.get()}", agent, folder: folder.clone(), approval }
                        },
                        ItemBody::Question(question) => rsx! {
                            QuestionCard { key: "{waiting_item.id.get()}", agent, item: waiting_item.id, question }
                        },
                        ItemBody::Plan(plan) => rsx! {
                            PlanCard { key: "{waiting_item.id.get()}", agent, folder: folder.clone(), plan }
                        },
                        _ => rsx! {},
                    }
                }
            }
        }
        div { class: "dialog-tabs map-tabs", role: "tablist",
            button {
                class: tab_class(MapTab::Transcript),
                role: "tab",
                aria_selected: tab == MapTab::Transcript,
                onclick: move |_| cockpit.update(|state| agent_map::set_tab(state, MapTab::Transcript)),
                "Transcript"
            }
            button {
                class: tab_class(MapTab::Prompt),
                role: "tab",
                aria_selected: tab == MapTab::Prompt,
                onclick: move |_| cockpit.update(|state| agent_map::set_tab(state, MapTab::Prompt)),
                "Prompt"
            }
            button {
                class: tab_class(MapTab::ToolCalls),
                role: "tab",
                aria_selected: tab == MapTab::ToolCalls,
                onclick: move |_| cockpit.update(|state| agent_map::set_tab(state, MapTab::ToolCalls)),
                "Tool calls"
                span { class: "map-tab-count", "{call_count}" }
            }
        }
        div { class: "map-tab-body",
            if loaded {
                {body}
            } else {
                p { class: "map-note", "Its messages are older than the part of the chat the app holds." }
                if has_older {
                    button {
                        class: "button small",
                        onclick: move |_| cockpit.change(|state| actions::load_older(state, agent)),
                        "Load older messages"
                    }
                }
            }
        }
        // Only a call the app holds has an entry in the chat to scroll to.
        if loaded {
            button {
                class: "link-button map-show-call",
                onclick: move |_| cockpit.update(|state| agent_map::show_call(state, item)),
                "Show its call in the chat"
            }
        }
    }
}

/// A call that started a subagent, as a line that selects it.
#[component]
fn SubagentRef(child: Subagent) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let item = child.item;
    let calls = format::count(
        usize::try_from(child.tool_calls).unwrap_or(usize::MAX),
        "tool call",
        "tool calls",
    );
    let time = subagent_time(&child, now()).map(format::duration);
    let meta = match (child.status, time) {
        (SubagentStatus::Running | SubagentStatus::NeedsYou, _) => format!("running, {calls}"),
        (SubagentStatus::Failed, _) => format!("failed, {calls}"),
        (SubagentStatus::Stopped, _) => format!("stopped, {calls}"),
        (SubagentStatus::Done, Some(time)) => format!("{calls}, {time}"),
        (SubagentStatus::Done, None) => calls,
    };
    rsx! {
        div { class: "compact-tool {subagent_line_class(child.status)}",
            span { class: "tool-dot", aria_hidden: true }
            span { class: "tool-name", "Agent" }
            button {
                class: "map-ref",
                onclick: move |_| cockpit.update(|state| agent_map::select_subagent(state, item)),
                "{child.description}"
            }
            span { class: "tool-meta", "{meta}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::SubagentToolCall;

    use super::*;

    fn subagent(item: u32, status: SubagentStatus) -> Subagent {
        Subagent {
            item: ItemId::new(item),
            parent: None,
            description: format!("Subagent {item}"),
            agent_type: None,
            run: SubagentRun::Foreground,
            status,
            started_at: Timestamp::from_unix_millis(1_000_000),
            ended_at: None,
            tokens: None,
            tool_calls: 0,
            last_tool: None,
            error: None,
            turn: 1,
            task: None,
        }
    }

    #[test]
    fn a_running_subagent_shows_its_call_or_the_subagent_it_waits_for() {
        let child = subagent(5, SubagentStatus::Running);
        let mut parent = subagent(4, SubagentStatus::Running);
        parent.last_tool = Some(SubagentToolCall {
            item: Some(ItemId::new(5)),
            tool: "Agent".to_owned(),
            summary: "Subagent 5".to_owned(),
        });
        let by_item = BTreeMap::from([(child.item, &child)]);
        let none = BTreeMap::new();

        assert_eq!(
            second_line(&parent, &by_item, &none),
            Some(SecondLine::WaitsFor {
                item: ItemId::new(5),
                description: "Subagent 5".to_owned()
            })
        );
        assert_eq!(
            second_line(&parent, &BTreeMap::new(), &none),
            Some(SecondLine::Tool {
                tool: "Agent".to_owned(),
                summary: "Subagent 5".to_owned()
            })
        );
    }

    #[test]
    fn an_ended_subagent_says_how_it_ended() {
        let mut failed = subagent(1, SubagentStatus::Failed);
        failed.error = Some("API error 529: overloaded".to_owned());
        let waiting = BTreeMap::from([(ItemId::new(2), "approve Bash".to_owned())]);
        let none: BTreeMap<ItemId, &Subagent> = BTreeMap::new();
        let nothing: BTreeMap<ItemId, String> = BTreeMap::new();

        assert_eq!(
            second_line(&failed, &none, &nothing),
            Some(SecondLine::Failed("API error 529: overloaded".to_owned()))
        );
        assert_eq!(
            second_line(&subagent(2, SubagentStatus::NeedsYou), &none, &waiting),
            Some(SecondLine::WaitsForYou(
                "Waits for you to approve Bash".to_owned()
            ))
        );
        assert_eq!(
            second_line(&subagent(3, SubagentStatus::Stopped), &none, &nothing),
            Some(SecondLine::Stopped)
        );
        assert_eq!(
            second_line(&subagent(4, SubagentStatus::Done), &none, &nothing),
            None
        );
    }

    #[test]
    fn a_stopped_subagent_without_an_end_shows_no_time() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_042);
        let running = subagent(1, SubagentStatus::Running);
        let mut done = subagent(2, SubagentStatus::Done);
        done.ended_at = Some(Timestamp::from_unix_millis(1_012_500));

        assert_eq!(subagent_time(&running, now), Some(Duration::from_secs(42)));
        assert_eq!(subagent_time(&done, now), Some(Duration::from_secs(12)));
        assert_eq!(
            subagent_time(&subagent(3, SubagentStatus::Stopped), now),
            None
        );
    }

    #[test]
    fn the_header_counts_each_status_of_the_turn() {
        let list = [
            subagent(1, SubagentStatus::Running),
            subagent(2, SubagentStatus::Running),
            subagent(3, SubagentStatus::Failed),
        ];
        let refs: Vec<&Subagent> = list.iter().collect();

        let texts: Vec<String> = status_counts(&refs)
            .into_iter()
            .map(|count| count.text)
            .collect();

        assert_eq!(texts, ["2 running", "1 failed"]);
    }
}
