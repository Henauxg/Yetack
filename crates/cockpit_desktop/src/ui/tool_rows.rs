//! Rows of the main conversation that stand for more than one item: a skill's call with the
//! instructions that follow it, and a run of reads and searches folded into one line.

use std::collections::BTreeSet;
use std::path::PathBuf;

use cockpit_protocol::{AgentId, ItemBody, ItemId, ToolCall, TranscriptItem};
use dioxus::prelude::*;
use serde_json::Value;

use crate::format;
use crate::render::SearchTool;
use crate::state::file_edits;
use crate::ui::find_folds::{FoldChip, FoldKey};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::kept_folds::use_kept_fold;
use crate::ui::markdown::MarkdownView;
use crate::ui::transcript::{self, CallStage, CompactItem, RunningMeta};

/// Claude Code's tool that starts a skill, and its input fields.
const SKILL_TOOL: &str = "Skill";
const SKILL_FIELD: &str = "skill";
const ARGS_FIELD: &str = "args";

/// A run of reads and searches shorter than this shows each call on its own row.
const MIN_EXPLORED: usize = 2;

/// A row of the main conversation, over the items it shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Row<'item> {
    Item(&'item TranscriptItem),
    /// A skill's call, with the notice of its instructions once that notice came.
    Skill {
        call: &'item TranscriptItem,
        instructions: Option<&'item TranscriptItem>,
    },
    /// Reads and searches in a row, under the same parent.
    Explored {
        first: &'item TranscriptItem,
        rest: Vec<&'item TranscriptItem>,
    },
}

impl<'item> Row<'item> {
    /// The item the row starts with, which names it.
    pub fn first(&self) -> &'item TranscriptItem {
        match self {
            Self::Item(item)
            | Self::Skill { call: item, .. }
            | Self::Explored { first: item, .. } => item,
        }
    }

    /// Every item the row shows, in order.
    pub fn items(&self) -> Vec<&'item TranscriptItem> {
        match self {
            Self::Item(item) => vec![item],
            Self::Skill { call, instructions } => {
                let mut items = vec![*call];
                items.extend(instructions);
                items
            }
            Self::Explored { first, rest } => {
                let mut items = vec![*first];
                items.extend(rest);
                items
            }
        }
    }
}

/// The rows of the main conversation's items, in order. A skill's call takes the notice of its
/// instructions right after it. Two reads or searches in a row or more, under the same parent,
/// take one row. A call that waits for approval stays on its own row, beside its card. A run ends
/// before `divider`, the first new item, so that the "New" divider stays above new items only.
pub fn rows<'item>(
    items: &[&'item TranscriptItem],
    awaiting: &BTreeSet<ItemId>,
    divider: Option<ItemId>,
) -> Vec<Row<'item>> {
    let mut rows = Vec::new();
    let mut rest = items;
    while let Some((&item, after)) = rest.split_first() {
        if let Some(call) = skill_call(item) {
            let instructions = after
                .first()
                .copied()
                .filter(|next| is_instructions_of(next, item, call));
            rest = after
                .get(usize::from(instructions.is_some())..)
                .unwrap_or_default();
            rows.push(Row::Skill {
                call: item,
                instructions,
            });
            continue;
        }
        let run: Vec<&TranscriptItem> = rest
            .iter()
            .copied()
            .enumerate()
            .take_while(|&(at, next)| {
                explores(next, awaiting)
                    && next.parent == item.parent
                    && (at == 0 || Some(next.id) != divider)
            })
            .map(|(_, next)| next)
            .collect();
        if run.len() >= MIN_EXPLORED {
            rest = rest.get(run.len()..).unwrap_or_default();
            rows.push(Row::Explored {
                first: item,
                rest: run.into_iter().skip(1).collect(),
            });
            continue;
        }
        rest = after;
        rows.push(Row::Item(item));
    }
    rows
}

fn skill_call(item: &TranscriptItem) -> Option<&ToolCall> {
    match &item.body {
        ItemBody::ToolCall(call) if is_skill(call) => Some(call),
        _ => None,
    }
}

pub fn is_skill(call: &ToolCall) -> bool {
    call.tool == SKILL_TOOL
}

/// The notice Claude Code adds after a skill's call, which holds the skill's instructions.
fn is_instructions_of(next: &TranscriptItem, item: &TranscriptItem, call: &ToolCall) -> bool {
    let ItemBody::Notice {
        text,
        detail: Some(_),
        ..
    } = &next.body
    else {
        return false;
    };
    next.parent == item.parent
        && *text == cockpit_protocol::skill_instructions_title(&skill_name(call))
}

/// A read or a search that does not wait for approval.
fn explores(item: &TranscriptItem, awaiting: &BTreeSet<ItemId>) -> bool {
    let ItemBody::ToolCall(call) = &item.body else {
        return false;
    };
    only_looks(call) && !awaiting.contains(&item.id)
}

/// A read or a search: a call that only looks at the agent's files.
fn only_looks(call: &ToolCall) -> bool {
    let looks = call.tool == file_edits::READ_TOOL || call.tool.parse::<SearchTool>().is_ok();
    looks && call.edit.is_none()
}

/// Whether a call can show in a row with other items: a skill's call, a read or a search.
pub fn shares_a_row(call: &ToolCall) -> bool {
    is_skill(call) || only_looks(call)
}

/// The skill a call starts, without its slash.
fn skill_name(call: &ToolCall) -> String {
    text_field(&call.input, SKILL_FIELD).unwrap_or_else(|| call.summary.clone())
}

fn text_field(input: &Value, name: &str) -> Option<String> {
    input
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// A skill's call on one line: the skill and its arguments. A click shows its instructions.
#[component]
pub fn SkillView(
    agent: AgentId,
    folder: PathBuf,
    id: ItemId,
    call: ToolCall,
    stage: CallStage,
    instructions: Option<String>,
) -> Element {
    let decided = use_kept_fold(agent, FoldKey::Item(id));
    let open = decided.decided().unwrap_or(false) && instructions.is_some();
    let skill = skill_name(&call);
    let args = text_field(&call.input, ARGS_FIELD);
    let meta = transcript::tool_meta(&call, stage);
    let head = rsx! {
        span { class: "tool-name", "{call.tool}" }
        code { class: "tool-summary", "/{skill}" }
        if let Some(args) = args {
            span { class: "call-description", "{args}" }
        }
        if stage == CallStage::Running {
            RunningMeta { agent, item: id }
        } else if !meta.is_empty() {
            span { class: "tool-meta", "{meta}" }
        }
    };
    rsx! {
        div { class: stage.class(),
            match instructions {
                Some(markdown) => rsx! {
                    button {
                        class: "fold-button tool-head",
                        aria_expanded: open,
                        title: if open { "Hide the skill's instructions" } else { "Show the skill's instructions" },
                        onclick: move |_| decided.set(!open),
                        {head}
                        if !open {
                            FoldChip { fold: FoldKey::Item(id) }
                        }
                        span { class: if open { "chevron open" } else { "chevron" }, Icon { kind: IconKind::ChevronRight, size: 12 } }
                    }
                    if open {
                        div { class: decided.body_class("notice-detail"),
                            MarkdownView { agent, folder, markdown }
                        }
                    }
                },
                None => rsx! {
                    div { class: "tool-head", {head} }
                },
            }
        }
    }
}

/// The words of a skill's line, as `SkillView` shows them, for the find to count.
pub fn skill_line(call: &ToolCall, stage: CallStage) -> Vec<String> {
    let mut words = vec![call.tool.clone(), format!("/{}", skill_name(call))];
    words.extend(text_field(&call.input, ARGS_FIELD));
    // A running call's time changes every second, and the find leaves it out.
    if stage != CallStage::Running {
        words.push(transcript::tool_meta(call, stage));
    }
    words
}

/// The words of a run of reads' line, as `ExploredView` shows them, for the find to count.
pub fn explored_line<'item>(calls: impl IntoIterator<Item = &'item TranscriptItem>) -> Vec<String> {
    let summary = ExploredSummary::of(calls);
    let mut words = vec![summary.title().to_owned(), summary.description()];
    if summary.running.is_none() && summary.failed > 0 {
        words.push(format!("{} failed", summary.failed));
    }
    words
}

/// A run of reads and searches on one line, which says how many files it read and how many
/// searches it ran. A click lists each call.
#[component]
pub fn ExploredView(
    agent: AgentId,
    folder: PathBuf,
    /// The run's first call, which names its fold.
    first: ItemId,
    calls: Vec<TranscriptItem>,
) -> Element {
    let decided = use_kept_fold(agent, FoldKey::Explored(first));
    let open = decided.decided().unwrap_or(false);
    let summary = ExploredSummary::of(&calls);
    let title = summary.title();
    rsx! {
        div { class: summary.stage().class(),
            button {
                class: "fold-button tool-head",
                aria_expanded: open,
                onclick: move |_| decided.set(!open),
                span { class: "tool-name", "{title}" }
                span { class: "call-description", "{summary.description()}" }
                if let Some(item) = summary.running {
                    RunningMeta { agent, item }
                } else if summary.failed > 0 {
                    span { class: "tool-meta", "{summary.failed} failed" }
                }
                if !open {
                    FoldChip { fold: FoldKey::Explored(first) }
                }
                span { class: if open { "chevron open" } else { "chevron" }, Icon { kind: IconKind::ChevronRight, size: 12 } }
            }
            if open {
                div { class: decided.body_class("subagent-body"),
                    for call in calls {
                        CompactItem { key: "{call.id.get()}", agent, folder: folder.clone(), item: call, nested: Vec::new(), awaits_approval: false }
                    }
                }
            }
        }
    }
}

/// What a run of reads and searches did.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExploredSummary {
    /// The files it read, each once: a read that failed read nothing.
    files: usize,
    searches: usize,
    failed: usize,
    /// A call of the run that still runs.
    running: Option<ItemId>,
}

impl ExploredSummary {
    fn of<'item>(calls: impl IntoIterator<Item = &'item TranscriptItem>) -> Self {
        let mut files = BTreeSet::new();
        let mut summary = Self {
            files: 0,
            searches: 0,
            failed: 0,
            running: None,
        };
        for item in calls {
            let ItemBody::ToolCall(call) = &item.body else {
                continue;
            };
            let stage = CallStage::new(call, false);
            if call.tool != file_edits::READ_TOOL {
                summary.searches += 1;
            } else if stage == CallStage::Done {
                files.insert(call.summary.as_str());
            }
            match stage {
                CallStage::Running | CallStage::AwaitsApproval => {
                    summary.running = summary.running.or(Some(item.id));
                }
                CallStage::Failed => summary.failed += 1,
                CallStage::Done => {}
            }
        }
        summary.files = files.len();
        summary
    }

    fn title(&self) -> &'static str {
        if self.running.is_some() {
            "Exploring"
        } else {
            "Explored"
        }
    }

    /// "2 files read, 1 search".
    fn description(&self) -> String {
        let mut parts = Vec::new();
        if self.files > 0 {
            parts.push(format::count(self.files, "file read", "files read"));
        }
        if self.searches > 0 {
            parts.push(format::count(self.searches, "search", "searches"));
        }
        parts.join(", ")
    }

    /// The run's color: running while a call runs, failed when one failed.
    fn stage(&self) -> CallStage {
        if self.running.is_some() {
            CallStage::Running
        } else if self.failed > 0 {
            CallStage::Failed
        } else {
            CallStage::Done
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{NoticeKind, ToolCallStatus};
    use serde_json::json;

    use super::*;

    fn call(id: u32, tool: &str, summary: &str, status: ToolCallStatus) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::ToolCall(ToolCall {
                tool: tool.to_owned(),
                summary: summary.to_owned(),
                input: json!({}),
                status,
                output: None,
                exit_code: None,
                edit: None,
            }),
        }
    }

    fn read(id: u32, path: &str) -> TranscriptItem {
        call(id, "Read", path, ToolCallStatus::Done)
    }

    fn text(id: u32) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::AssistantText {
                markdown: "Now the lexer.".to_owned(),
            },
        }
    }

    fn skill(id: u32, name: &str) -> TranscriptItem {
        let mut item = call(id, "Skill", name, ToolCallStatus::Done);
        if let ItemBody::ToolCall(call) = &mut item.body {
            call.input = json!({ "skill": name, "args": "fix" });
        }
        item
    }

    fn notice(id: u32, text: &str) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::Notice {
                text: text.to_owned(),
                detail: Some("Check that the code works.".to_owned()),
                kind: NoticeKind::Plain,
            },
        }
    }

    fn ids(row: &Row<'_>) -> Vec<u32> {
        row.items().iter().map(|item| item.id.get()).collect()
    }

    #[test]
    fn reads_and_searches_in_a_row_take_one_row() {
        let items = [
            read(1, "src/lexer.rs"),
            call(2, "Grep", "column", ToolCallStatus::Done),
            read(3, "src/position.rs"),
            text(4),
            read(5, "src/error.rs"),
            call(6, "Bash", "cargo test", ToolCallStatus::Done),
        ];
        let refs: Vec<&TranscriptItem> = items.iter().collect();

        let rows = rows(&refs, &BTreeSet::new(), None);

        let shapes: Vec<Vec<u32>> = rows.iter().map(ids).collect();
        assert_eq!(shapes, [vec![1, 2, 3], vec![4], vec![5], vec![6]]);
        assert!(matches!(rows.first(), Some(Row::Explored { .. })));
        assert!(matches!(rows.get(2), Some(Row::Item(_))));
    }

    #[test]
    fn a_run_ends_where_the_new_items_start() {
        let items = [
            read(1, "src/a.rs"),
            read(2, "src/b.rs"),
            read(3, "src/c.rs"),
            read(4, "src/d.rs"),
        ];
        let refs: Vec<&TranscriptItem> = items.iter().collect();

        let shapes: Vec<Vec<u32>> = rows(&refs, &BTreeSet::new(), Some(ItemId::new(3)))
            .iter()
            .map(ids)
            .collect();

        assert_eq!(shapes, [vec![1, 2], vec![3, 4]]);
    }

    #[test]
    fn a_read_that_waits_for_approval_or_has_another_parent_breaks_the_run() {
        let mut nested = read(3, "src/c.rs");
        nested.parent = Some(ItemId::new(9));
        let items = [
            read(1, "src/a.rs"),
            read(2, "src/b.rs"),
            nested,
            read(4, "src/d.rs"),
            read(5, "src/e.rs"),
        ];
        let refs: Vec<&TranscriptItem> = items.iter().collect();
        let awaiting = BTreeSet::from([ItemId::new(4)]);

        let shapes: Vec<Vec<u32>> = rows(&refs, &awaiting, None).iter().map(ids).collect();

        assert_eq!(shapes, [vec![1, 2], vec![3], vec![4], vec![5]]);
    }

    #[test]
    fn a_skill_takes_the_notice_of_its_own_instructions() {
        let items = [
            skill(1, "review"),
            notice(2, "Skill instructions: /review"),
            skill(3, "review"),
            notice(4, "Skill instructions: /simplify"),
            skill(5, "review"),
        ];
        let refs: Vec<&TranscriptItem> = items.iter().collect();

        let rows = rows(&refs, &BTreeSet::new(), None);

        let shapes: Vec<Vec<u32>> = rows.iter().map(ids).collect();
        assert_eq!(shapes, [vec![1, 2], vec![3], vec![4], vec![5]]);
    }

    #[test]
    fn a_run_counts_each_file_once_its_searches_and_its_failures() {
        let calls = [
            read(1, "src/lexer.rs"),
            read(2, "src/lexer.rs"),
            call(3, "Glob", "tests/*.rs", ToolCallStatus::Done),
            call(4, "Read", "src/gone.rs", ToolCallStatus::Failed),
        ];

        let summary = ExploredSummary::of(&calls);
        let running = ExploredSummary::of(&[call(5, "Grep", "x", ToolCallStatus::Running)]);

        assert_eq!(summary.description(), "1 file read, 1 search");
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.stage(), CallStage::Failed);
        assert_eq!(running.running, Some(ItemId::new(5)));
        assert_eq!(running.stage(), CallStage::Running);
    }
}
