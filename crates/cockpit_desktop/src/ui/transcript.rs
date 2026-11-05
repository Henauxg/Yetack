//! The conversation: user bubbles, agent text, tool calls, inline diffs, subagents and the cards
//! that wait for an answer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use cockpit_protocol::{
    AgentId, AgentStatus, AnswerState, ItemBody, ItemId, ToolCall, ToolCallStatus, TranscriptItem,
    TurnEnd, UserMessage,
};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::render::{Diff, Language, diff_lines};
use crate::state::{HistoryState, actions, file_edits};
use crate::ui::cards::{ApprovalCard, PlanCard, QuestionCard};
use crate::ui::code::{LineNumbers, diff_row};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::markdown::MarkdownView;
use crate::ui::sidebar::activity_line;
use crate::{bridge, format};

/// Command output longer than this folds behind "Show more".
const OUTPUT_PREVIEW_LINES: usize = 5;

/// Diff rows an inline edit shows before "open in the file view".
const EDIT_PREVIEW_ROWS: usize = 8;

/// Unchanged lines kept around each change of an inline edit.
const EDIT_CONTEXT_LINES: usize = 2;

/// The prefix and separator of an MCP tool's name, as in `mcp__server__tool`.
const MCP_TOOL_PREFIX: &str = "mcp__";
const MCP_TOOL_SEPARATOR: &str = "__";

#[component]
pub fn TranscriptView() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let Some(entry) = state.selected() else {
        return rsx! {
            div { class: "chat-scroll scroll",
                div { class: "chat-column",
                    p { class: "chat-empty", "Pick an agent on the left, or start one with Ctrl+N." }
                }
            }
        };
    };
    let agent = entry.id();
    let folder = entry.summary.folder.clone();
    let status = entry.summary.status;
    let items: Vec<&TranscriptItem> = entry.transcript.items().collect();
    let children = children_by_parent(&items);
    let awaiting = awaiting_approval(&items);
    let top: Vec<(TranscriptItem, Vec<TranscriptItem>, bool)> = items
        .iter()
        .filter(|item| shows_at_top(item))
        .map(|item| {
            let nested = subtree(&children, item.id);
            ((*item).clone(), nested, awaiting.contains(&item.id))
        })
        .collect();
    let history = entry.transcript.history().clone();
    let activity = activity_line(entry);
    rsx! {
        div { class: "chat-scroll scroll",
            div { class: "chat-column",
                match history {
                    HistoryState::NotLoaded | HistoryState::Loading => rsx! {
                        p { class: "history-note", "Loading the conversation" }
                    },
                    HistoryState::LoadingOlder => rsx! {
                        p { class: "history-note", "Loading older messages" }
                    },
                    HistoryState::Loaded { has_older: true } => rsx! {
                        button {
                            class: "link-button history-note",
                            onclick: move |_| cockpit.change(|state| actions::load_older(state, agent)),
                            "Load older messages"
                        }
                    },
                    HistoryState::Loaded { has_older: false } => rsx! {},
                    HistoryState::Failed(error) => rsx! {
                        p { class: "history-note",
                            "The conversation did not load: {error}. "
                            button {
                                class: "link-button",
                                onclick: move |_| cockpit.update(|state| actions::retry_history(state, agent)),
                                "Retry"
                            }
                        }
                    },
                }
                if status == AgentStatus::Exited {
                    div { class: "exited-banner",
                        span { "The claude process ended. Resume starts it again on the same conversation." }
                        button {
                            class: "button primary",
                            onclick: move |_| cockpit.change(|_| actions::resume_agent(agent)),
                            "Resume"
                        }
                    }
                }
                // Item ids repeat across agents: with the agent in the key, another agent's item
                // never takes over the state of this one's (a fold, a typed answer).
                for (item, nested, awaits_approval) in top {
                    ItemView { key: "{agent.get()}-{item.id.get()}", agent, folder: folder.clone(), item, nested, awaits_approval }
                }
                if status == AgentStatus::Working {
                    div { class: "working-line",
                        span { class: "working-dot", aria_hidden: true }
                        "{activity}"
                    }
                }
            }
        }
    }
}

/// Top-level items, and the cards still waiting for an answer, even a subagent's: a folded
/// subagent must not hide them.
fn shows_at_top(item: &TranscriptItem) -> bool {
    item.parent.is_none() || waits_for_answer(item)
}

fn waits_for_answer(item: &TranscriptItem) -> bool {
    match &item.body {
        ItemBody::Approval(approval) => approval.answer == AnswerState::Waiting,
        ItemBody::Question(question) => question.answer == AnswerState::Waiting,
        ItemBody::Plan(plan) => plan.answer == AnswerState::Waiting,
        _ => false,
    }
}

/// Each item's direct children. Built once per render, so that the subtrees cost one pass over
/// the transcript instead of one per item.
type Children<'item> = BTreeMap<ItemId, Vec<&'item TranscriptItem>>;

fn children_by_parent<'item>(items: &[&'item TranscriptItem]) -> Children<'item> {
    let mut children = Children::new();
    for item in items {
        if let Some(parent) = item.parent {
            children.entry(parent).or_default().push(item);
        }
    }
    children
}

/// Every item nested under `root`, at any depth, in transcript order. Each item comes once, even
/// when parents form a cycle.
fn subtree(children: &Children<'_>, root: ItemId) -> Vec<TranscriptItem> {
    let mut found: Vec<TranscriptItem> = Vec::new();
    let mut seen = BTreeSet::from([root]);
    let mut pending = vec![root];
    while let Some(parent) = pending.pop() {
        for child in children.get(&parent).into_iter().flatten() {
            if seen.insert(child.id) {
                found.push((*child).clone());
                pending.push(child.id);
            }
        }
    }
    found.sort_by_key(|item| item.id);
    found
}

/// The running tool calls whose approval card waits for an answer. The protocol does not link an
/// approval to its call, so they match on parent, tool and summary, which the daemon builds the
/// same way for both.
fn awaiting_approval(items: &[&TranscriptItem]) -> BTreeSet<ItemId> {
    let asked: BTreeSet<(Option<ItemId>, &str, &str)> = items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::Approval(approval) if approval.answer == AnswerState::Waiting => Some((
                item.parent,
                approval.tool.as_str(),
                approval.summary.as_str(),
            )),
            _ => None,
        })
        .collect();
    if asked.is_empty() {
        return BTreeSet::new();
    }
    items
        .iter()
        .filter(|item| match &item.body {
            ItemBody::ToolCall(call) if call.status == ToolCallStatus::Running => {
                asked.contains(&(item.parent, call.tool.as_str(), call.summary.as_str()))
            }
            _ => false,
        })
        .map(|item| item.id)
        .collect()
}

#[component]
pub fn ItemView(
    agent: AgentId,
    folder: PathBuf,
    item: TranscriptItem,
    nested: Vec<TranscriptItem>,
    /// Whether the item is a running tool call whose approval card waits for an answer.
    awaits_approval: bool,
) -> Element {
    let id = item.id;
    match item.body {
        ItemBody::UserMessage(message) if message.queued => rsx! {
            QueuedMessage { agent, item: id, message }
        },
        ItemBody::UserMessage(message) => rsx! {
            UserBubble { message }
        },
        ItemBody::AssistantText { markdown } => rsx! {
            MarkdownView { agent, markdown }
        },
        ItemBody::Thinking { text } => rsx! {
            ThinkingView { text }
        },
        ItemBody::ToolCall(call) if call.edit.is_some() => rsx! {
            EditView { agent, folder, stage: CallStage::new(&call, awaits_approval), call }
        },
        ItemBody::ToolCall(call) => rsx! {
            ToolView { stage: CallStage::new(&call, awaits_approval), call }
        },
        ItemBody::Subagent(call) => rsx! {
            SubagentView { agent, folder, id, call, nested }
        },
        ItemBody::Approval(approval) => rsx! {
            ApprovalCard { agent, folder, approval }
        },
        ItemBody::Question(question) => rsx! {
            QuestionCard { agent, item: id, question }
        },
        ItemBody::Plan(plan) => rsx! {
            PlanCard { agent, plan }
        },
        ItemBody::TurnEnd(end) => rsx! {
            TurnEndView { end }
        },
        ItemBody::Notice { text } => rsx! {
            p { class: "notice", "{text}" }
        },
    }
}

#[component]
fn UserBubble(message: UserMessage) -> Element {
    rsx! {
        div { class: "user-message",
            for (index, image) in message.images.iter().enumerate() {
                img { key: "{index}", class: "message-image", src: bridge::image_url(image), alt: "Attached image" }
            }
            if !message.text.is_empty() {
                div { class: "user-bubble", "{message.text}" }
            }
        }
    }
}

#[component]
fn QueuedMessage(agent: AgentId, item: ItemId, message: UserMessage) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        div { class: "user-message",
            div { class: "user-bubble queued", "{message.text}" }
            span { class: "queued-note",
                "Queued, sent after the current step"
                button {
                    class: "link-button",
                    onclick: move |_| cockpit.change(|_| actions::cancel_queued(agent, item)),
                    "Cancel"
                }
            }
        }
    }
}

#[component]
fn ThinkingView(text: String) -> Element {
    let mut expanded = use_signal(|| false);
    let open = expanded();
    rsx! {
        div { class: "thinking",
            button {
                class: "fold-button",
                aria_expanded: open,
                onclick: move |_| expanded.toggle(),
                span { class: "muted", "Thinking" }
                span { class: if open { "chevron open" } else { "chevron" }, Icon { kind: IconKind::ChevronRight, size: 12 } }
            }
            if open {
                p { class: "thinking-text", "{text}" }
            }
        }
    }
}

/// Where a tool call stands, as its line says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallStage {
    /// Claude Code asked whether the call may run, and nobody answered yet.
    AwaitsApproval,
    Running,
    Done,
    Failed,
}

impl CallStage {
    fn new(call: &ToolCall, awaits_approval: bool) -> Self {
        match call.status {
            ToolCallStatus::Running if awaits_approval => Self::AwaitsApproval,
            ToolCallStatus::Running => Self::Running,
            ToolCallStatus::Done => Self::Done,
            ToolCallStatus::Failed => Self::Failed,
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::AwaitsApproval => "tool waiting",
            Self::Running => "tool running",
            Self::Done => "tool done",
            Self::Failed => "tool failed",
        }
    }
}

/// "mcp__search__find" reads "search/find".
fn tool_label(tool: &str) -> String {
    let Some(rest) = tool.strip_prefix(MCP_TOOL_PREFIX) else {
        return tool.to_owned();
    };
    match rest.split_once(MCP_TOOL_SEPARATOR) {
        Some((server, name)) => format!("{server}/{name}"),
        None => rest.to_owned(),
    }
}

fn tool_meta(call: &ToolCall, stage: CallStage) -> String {
    match (stage, call.exit_code) {
        (CallStage::AwaitsApproval, _) => "waiting for approval".to_owned(),
        (CallStage::Running, _) => "running".to_owned(),
        (_, Some(code)) => format!("exit {code}"),
        (CallStage::Failed, None) => "failed".to_owned(),
        (CallStage::Done, None) => String::new(),
    }
}

/// The end of an edit's line: what the edit did once it ran, where it stands before that.
#[derive(Debug, PartialEq, Eq)]
enum EditMeta {
    Stage(String),
    Created { lines: usize },
    Changed { added: usize, removed: usize },
}

impl EditMeta {
    fn new(call: &ToolCall, stage: CallStage, added: usize, removed: usize) -> Self {
        // A `Write` has no old text until it ran, even over a file: the daemon reads that text
        // from the result.
        let created = call
            .edit
            .as_ref()
            .is_some_and(|edit| edit.old_text.is_empty());
        match stage {
            CallStage::Done if created => Self::Created { lines: added },
            CallStage::Done => Self::Changed { added, removed },
            CallStage::AwaitsApproval | CallStage::Running | CallStage::Failed => {
                Self::Stage(tool_meta(call, stage))
            }
        }
    }
}

#[component]
fn ToolView(call: ToolCall, stage: CallStage) -> Element {
    let mut expanded = use_signal(|| false);
    let output = call.output.clone().unwrap_or_default();
    let lines: Vec<&str> = output.lines().collect();
    let foldable = lines.len() > OUTPUT_PREVIEW_LINES;
    let shown = if expanded() || !foldable {
        output.clone()
    } else {
        lines
            .iter()
            .take(OUTPUT_PREVIEW_LINES)
            .copied()
            .collect::<Vec<&str>>()
            .join("\n")
    };
    let hidden = lines.len().saturating_sub(OUTPUT_PREVIEW_LINES);
    let toggle_text = if expanded() {
        "Show less".to_owned()
    } else {
        format!("Show {} more", format::count(hidden, "line", "lines"))
    };
    let name = tool_label(&call.tool);
    let meta = tool_meta(&call, stage);
    rsx! {
        div { class: stage.class(),
            div { class: "tool-head",
                span { class: "tool-dot", aria_hidden: true }
                span { class: "tool-name", "{name}" }
                code { class: "tool-summary", "{call.summary}" }
                if !meta.is_empty() {
                    span { class: "tool-meta", "{meta}" }
                }
            }
            if !output.is_empty() {
                div { class: "tool-output",
                    pre { "{shown}" }
                    if foldable {
                        button { class: "fold-lines", onclick: move |_| expanded.toggle(), "{toggle_text}" }
                    }
                }
            }
        }
    }
}

#[component]
fn EditView(agent: AgentId, folder: PathBuf, call: ToolCall, stage: CallStage) -> Element {
    let cockpit: Cockpit = use_context();
    let Some(edit) = call.edit.clone() else {
        return rsx! {};
    };
    let language = Language::from_path(&edit.path);
    let Diff {
        rows: preview,
        more,
        added,
        removed,
    } = diff_lines(
        &edit.old_text,
        &edit.new_text,
        language,
        EDIT_CONTEXT_LINES,
        1,
        EDIT_PREVIEW_ROWS,
    );
    let numbers = if edit.old_text.is_empty() {
        LineNumbers::Shown
    } else {
        LineNumbers::Hidden
    };
    let shown_path = file_edits::relative_path(&folder, &edit.path)
        .display()
        .to_string();
    let path = edit.path.clone();
    let open = move |_: MouseEvent| {
        let path = path.clone();
        cockpit.update(|state| actions::open_file(state, agent, path, None));
    };
    let open_more = open.clone();
    let more_text = format!(
        "{} more, open in the file view",
        format::count(more, "line", "lines")
    );
    let meta = EditMeta::new(&call, stage, added, removed);
    let name = tool_label(&call.tool);
    rsx! {
        div { class: stage.class(),
            div { class: "tool-head",
                span { class: "tool-dot", aria_hidden: true }
                span { class: "tool-name", "{name}" }
                button { class: "path-link mono", onclick: open, "{shown_path}" }
                match meta {
                    EditMeta::Stage(words) => rsx! {
                        span { class: "tool-meta", "{words}" }
                    },
                    EditMeta::Created { lines } => {
                        let lines = format::count(lines, "line", "lines");
                        rsx! {
                            span { class: "tool-meta", "Created, {lines}" }
                        }
                    }
                    EditMeta::Changed { added, removed } => rsx! {
                        span { class: "tool-meta mono",
                            span { class: "added", "+{added}" }
                            " "
                            span { class: "removed", "-{removed}" }
                        }
                    },
                }
            }
            if !preview.is_empty() {
                div { class: "diff-box inline-diff scroll-x",
                    for row in preview.iter() {
                        {diff_row(row, numbers, None)}
                    }
                    if more > 0 {
                        button { class: "more-lines", onclick: open_more, "{more_text}" }
                    }
                }
            }
            if call.status == ToolCallStatus::Failed {
                if let Some(output) = &call.output {
                    div { class: "tool-output", pre { "{output}" } }
                }
            }
        }
    }
}

#[component]
fn SubagentView(
    agent: AgentId,
    folder: PathBuf,
    id: ItemId,
    call: ToolCall,
    nested: Vec<TranscriptItem>,
) -> Element {
    let mut expanded = use_signal(|| false);
    let open = expanded();
    let calls = nested
        .iter()
        .filter(|item| matches!(item.body, ItemBody::ToolCall(_) | ItemBody::Subagent(_)))
        .count();
    let calls_text = format::count(calls, "tool call", "tool calls");
    let meta = match call.status {
        ToolCallStatus::Running => format!("running, {calls_text}"),
        ToolCallStatus::Done => calls_text,
        ToolCallStatus::Failed => format!("failed, {calls_text}"),
    };
    let nested_refs: Vec<&TranscriptItem> = nested.iter().collect();
    let by_parent = children_by_parent(&nested_refs);
    let awaiting = awaiting_approval(&nested_refs);
    let children: Vec<(TranscriptItem, Vec<TranscriptItem>, bool)> = nested
        .iter()
        .filter(|item| item.parent == Some(id) && !waits_for_answer(item))
        .map(|item| {
            let grandchildren = subtree(&by_parent, item.id);
            (item.clone(), grandchildren, awaiting.contains(&item.id))
        })
        .collect();
    rsx! {
        div { class: CallStage::new(&call, false).class(),
            button {
                class: "fold-button tool-head",
                aria_expanded: open,
                onclick: move |_| expanded.toggle(),
                span { class: "tool-dot", aria_hidden: true }
                span { class: "tool-name", "Agent" }
                span { class: "subagent-title", "{call.summary}" }
                span { class: "tool-meta", "{meta}" }
                span { class: if open { "chevron open" } else { "chevron" }, Icon { kind: IconKind::ChevronRight, size: 12 } }
            }
            if open {
                div { class: "subagent-body",
                    for (child, grandchildren, awaits_approval) in children {
                        CompactItem { key: "{child.id.get()}", agent, folder: folder.clone(), item: child, nested: grandchildren, awaits_approval }
                    }
                    if let Some(result) = &call.output {
                        p { class: "subagent-result", "{result}" }
                    }
                }
            }
        }
    }
}

/// A subagent's item: tool calls on one line, the rest as in the main conversation.
#[component]
fn CompactItem(
    agent: AgentId,
    folder: PathBuf,
    item: TranscriptItem,
    nested: Vec<TranscriptItem>,
    awaits_approval: bool,
) -> Element {
    match &item.body {
        ItemBody::ToolCall(call) => {
            let stage = CallStage::new(call, awaits_approval);
            let name = tool_label(&call.tool);
            let meta = tool_meta(call, stage);
            let summary = match &call.edit {
                Some(edit) => file_edits::relative_path(&folder, &edit.path)
                    .display()
                    .to_string(),
                None => call.summary.clone(),
            };
            rsx! {
                div { class: "compact-tool {stage.class()}",
                    span { class: "tool-dot", aria_hidden: true }
                    span { class: "tool-name", "{name}" }
                    code { class: "tool-summary", "{summary}" }
                    if !meta.is_empty() {
                        span { class: "tool-meta", "{meta}" }
                    }
                }
            }
        }
        _ => rsx! {
            ItemView { agent, folder, item: item.clone(), nested, awaits_approval }
        },
    }
}

#[component]
fn TurnEndView(end: TurnEnd) -> Element {
    let duration = format::duration(end.duration);
    let cost = format::cost(end.cost);
    rsx! {
        match end.error {
            Some(error) => rsx! {
                p { class: "turn-end failed", "The turn stopped after {duration}: {error}" }
            },
            None => rsx! {
                p { class: "turn-end", "Turn done in {duration}, {cost} at API prices" }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{Approval, ApprovalAnswer, ApprovalDecision, FileEdit, PendingId};

    use super::*;

    fn notice(id: u32, parent: Option<u32>) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: parent.map(ItemId::new),
            body: ItemBody::Notice {
                text: String::new(),
            },
        }
    }

    #[test]
    fn subtree_collects_every_depth_in_order() {
        let items = [
            notice(1, None),
            notice(2, Some(1)),
            notice(3, Some(2)),
            notice(4, None),
            notice(5, Some(1)),
        ];
        let refs: Vec<&TranscriptItem> = items.iter().collect();

        let ids: Vec<u32> = subtree(&children_by_parent(&refs), ItemId::new(1))
            .iter()
            .map(|item| item.id.get())
            .collect();

        assert_eq!(ids, [2, 3, 5]);
    }

    #[test]
    fn a_cycle_of_parents_ends() {
        let items = [notice(1, Some(2)), notice(2, Some(1)), notice(3, Some(3))];
        let refs: Vec<&TranscriptItem> = items.iter().collect();
        let children = children_by_parent(&refs);

        let ids = |root: u32| -> Vec<u32> {
            subtree(&children, ItemId::new(root))
                .iter()
                .map(|item| item.id.get())
                .collect()
        };

        assert_eq!(ids(1), [2]);
        assert!(ids(3).is_empty());
    }

    #[test]
    fn mcp_tool_names_read_as_server_and_tool() {
        assert_eq!(tool_label("mcp__search__find_pages"), "search/find_pages");
        assert_eq!(tool_label("Bash"), "Bash");
    }

    fn call(tool: &str, summary: &str, status: ToolCallStatus) -> ToolCall {
        ToolCall {
            tool: tool.to_owned(),
            summary: summary.to_owned(),
            input: serde_json::Value::Null,
            status,
            output: None,
            exit_code: None,
            edit: None,
        }
    }

    fn write_call(status: ToolCallStatus) -> ToolCall {
        ToolCall {
            edit: Some(FileEdit {
                path: PathBuf::from("/work/notes.txt"),
                old_text: String::new(),
                new_text: "one line\n".to_owned(),
            }),
            ..call("Write", "notes.txt", status)
        }
    }

    fn approval(tool: &str, summary: &str, answer: AnswerState<ApprovalAnswer>) -> ItemBody {
        ItemBody::Approval(Approval {
            pending: PendingId::new("request-1".to_owned()),
            tool: tool.to_owned(),
            summary: summary.to_owned(),
            input: serde_json::Value::Null,
            edit: None,
            reason: None,
            answer,
        })
    }

    fn item(id: u32, parent: Option<u32>, body: ItemBody) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: parent.map(ItemId::new),
            body,
        }
    }

    #[test]
    fn tool_meta_shows_the_stage_and_exit_code() {
        let failed = ToolCall {
            exit_code: Some(101),
            ..call("Bash", "cargo test", ToolCallStatus::Failed)
        };
        let meta = |status, awaits_approval| {
            let call = call("Bash", "cargo test", status);
            tool_meta(&call, CallStage::new(&call, awaits_approval))
        };

        assert_eq!(meta(ToolCallStatus::Running, true), "waiting for approval");
        assert_eq!(meta(ToolCallStatus::Running, false), "running");
        assert_eq!(tool_meta(&failed, CallStage::Failed), "exit 101");
        assert_eq!(meta(ToolCallStatus::Failed, false), "failed");
        assert_eq!(meta(ToolCallStatus::Done, false), "");
    }

    #[test]
    fn an_edit_shows_its_result_only_once_it_ran() {
        let meta = |status, awaits_approval| {
            let call = write_call(status);
            EditMeta::new(&call, CallStage::new(&call, awaits_approval), 1, 0)
        };

        assert_eq!(
            meta(ToolCallStatus::Running, true),
            EditMeta::Stage("waiting for approval".to_owned())
        );
        assert_eq!(
            meta(ToolCallStatus::Running, false),
            EditMeta::Stage("running".to_owned())
        );
        assert_eq!(
            meta(ToolCallStatus::Failed, false),
            EditMeta::Stage("failed".to_owned())
        );
        assert_eq!(
            meta(ToolCallStatus::Done, false),
            EditMeta::Created { lines: 1 }
        );

        let mut edit = write_call(ToolCallStatus::Done);
        if let Some(file) = &mut edit.edit {
            file.old_text = "old line\n".to_owned();
        }
        assert_eq!(
            EditMeta::new(&edit, CallStage::Done, 1, 1),
            EditMeta::Changed {
                added: 1,
                removed: 1
            }
        );
    }

    #[test]
    fn a_running_call_waits_while_its_approval_waits() {
        let allowed = AnswerState::Answered(ApprovalAnswer {
            decision: ApprovalDecision::Allow,
            message: None,
        });
        let items = [
            item(
                1,
                None,
                ItemBody::ToolCall(write_call(ToolCallStatus::Running)),
            ),
            item(
                2,
                None,
                approval("Write", "notes.txt", AnswerState::Waiting),
            ),
            item(
                3,
                None,
                ItemBody::ToolCall(call("Bash", "ls", ToolCallStatus::Running)),
            ),
            item(4, None, approval("Bash", "ls", allowed)),
            item(
                5,
                Some(9),
                ItemBody::ToolCall(call("Bash", "rm -r build", ToolCallStatus::Running)),
            ),
            item(
                6,
                None,
                approval("Bash", "rm -r build", AnswerState::Waiting),
            ),
            item(
                7,
                None,
                ItemBody::ToolCall(call("Bash", "rm -r build", ToolCallStatus::Done)),
            ),
        ];
        let refs: Vec<&TranscriptItem> = items.iter().collect();

        let ids: Vec<u32> = awaiting_approval(&refs).iter().map(|id| id.get()).collect();

        assert_eq!(ids, [1]);
    }
}
