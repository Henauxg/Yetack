//! Runs of the agent's work between its messages: thinking and tool calls in a row fold into one
//! line that says what they did, such as "Ran 4 commands, read 1 file, thought 2 times". The
//! agent's text, a card, a subagent, a notice and a call that waits for approval end a run, and so
//! does the "New" divider. A run holds the work of one agent: the items of a subagent whose call
//! sits in older messages start a run of their own.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::PathBuf;

use cockpit_protocol::{ItemBody, ItemId, ToolCall, TranscriptItem};

use crate::format;
use crate::render::SearchTool;
use crate::state::{actions, file_edits};
use crate::ui::tool_rows::Row;
use crate::ui::transcript::CallStage;

/// A run shorter than this shows its rows as they are.
const MIN_RUN: usize = 2;

/// A part of the main conversation, as indexes into its rows: one row, or a run that folds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    Row(usize),
    Run(Range<usize>),
}

impl Unit {
    /// The index of its first row.
    pub fn first(&self) -> usize {
        match self {
            Self::Row(index) => *index,
            Self::Run(range) => range.start,
        }
    }
}

/// The rows grouped into runs, in order. A run holds the work of one agent: its rows have the same
/// parent. A run ends before the row that holds `divider`, so that the "New" divider stays above
/// new rows only.
pub fn units(rows: &[Row<'_>], awaiting: &BTreeSet<ItemId>, divider: Option<ItemId>) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut start = 0;
    while let Some(head) = rows.get(start) {
        let parent = head.first().parent;
        let mut end = start;
        while let Some(row) = rows.get(end) {
            let starts_new = end > start
                && (row.first().parent != parent || divider.is_some_and(|id| holds(row, id)));
            if !folds(row, awaiting) || starts_new {
                break;
            }
            end += 1;
        }
        if end - start >= MIN_RUN {
            units.push(Unit::Run(start..end));
            start = end;
        } else {
            units.push(Unit::Row(start));
            start += 1;
        }
    }
    units
}

/// Whether a row is work that folds: thinking, a call, a run of reads, a skill.
fn folds(row: &Row<'_>, awaiting: &BTreeSet<ItemId>) -> bool {
    let first = row.first();
    if awaiting.contains(&first.id) {
        return false;
    }
    match row {
        Row::Explored { .. } | Row::Skill { .. } => true,
        Row::Item(item) => matches!(item.body, ItemBody::Thinking { .. } | ItemBody::ToolCall(_)),
    }
}

fn holds(row: &Row<'_>, item: ItemId) -> bool {
    row.items().iter().any(|held| held.id == item)
}

/// How many units hold new items, from `first_new` on: the count the chat's jump button shows. A
/// run counts once, as it shows.
pub fn new_units(rows: &[Row<'_>], units: &[Unit], first_new: Option<ItemId>) -> usize {
    let Some(first_new) = first_new else {
        return 0;
    };
    units
        .iter()
        .filter(|unit| {
            let held = match unit {
                Unit::Row(index) => rows.get(*index..=*index),
                Unit::Run(range) => rows.get(range.clone()),
            };
            held.unwrap_or_default().iter().any(|row| {
                let first = row.first();
                first.id >= first_new && actions::is_counted(first)
            })
        })
        .count()
}

/// What a run did, for its line.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkSummary {
    commands: usize,
    /// The files read, each once.
    files_read: BTreeSet<String>,
    searches: usize,
    /// The files edited, each once.
    files_edited: BTreeSet<PathBuf>,
    /// Calls of any other tool: a skill, a web fetch, an MCP tool.
    other_calls: usize,
    thoughts: usize,
    pub failed: usize,
    /// A call of the run still runs, or waits for approval.
    pub running: bool,
    /// The kinds of the calls that still run, whose verb the line says as under way.
    running_kinds: BTreeSet<CallKind>,
}

/// A kind of call, as the run's line counts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CallKind {
    Command,
    Read,
    Search,
    Edit,
    Other,
}

impl CallKind {
    fn of(call: &ToolCall) -> Self {
        if call.edit.is_some() {
            Self::Edit
        } else if call.tool == file_edits::BASH_TOOL {
            Self::Command
        } else if call.tool == file_edits::READ_TOOL {
            Self::Read
        } else if call.tool.parse::<SearchTool>().is_ok() {
            Self::Search
        } else {
            Self::Other
        }
    }

    fn verbs(self) -> Verbs {
        match self {
            Self::Command => Verbs {
                ended: "ran",
                under_way: "running",
            },
            Self::Read => Verbs {
                ended: "read",
                under_way: "reading",
            },
            Self::Search => Verbs {
                ended: "searched",
                under_way: "searching",
            },
            Self::Edit => Verbs {
                ended: "edited",
                under_way: "editing",
            },
            Self::Other => Verbs {
                ended: "used",
                under_way: "using",
            },
        }
    }
}

/// The verb of a kind of call once its calls ended, and while one runs.
struct Verbs {
    ended: &'static str,
    under_way: &'static str,
}

impl WorkSummary {
    pub fn of(rows: &[Row<'_>]) -> Self {
        let mut summary = Self::default();
        for item in rows.iter().flat_map(Row::items) {
            summary.add(item);
        }
        summary
    }

    fn add(&mut self, item: &TranscriptItem) {
        let call = match &item.body {
            ItemBody::Thinking { .. } => {
                self.thoughts += 1;
                return;
            }
            ItemBody::ToolCall(call) => call,
            _ => return,
        };
        let kind = CallKind::of(call);
        match CallStage::new(call, false) {
            CallStage::Failed => self.failed += 1,
            CallStage::Running | CallStage::AwaitsApproval => {
                self.running = true;
                self.running_kinds.insert(kind);
            }
            CallStage::Done => {}
        }
        match kind {
            CallKind::Edit => {
                if let Some(edit) = &call.edit {
                    self.files_edited.insert(edit.path.clone());
                }
            }
            CallKind::Command => self.commands += 1,
            CallKind::Read => {
                self.files_read.insert(call.summary.clone());
            }
            CallKind::Search => self.searches += 1,
            CallKind::Other => self.other_calls += 1,
        }
    }

    /// The verb of `kind`: "ran" once its calls ended, "running" while one runs.
    fn verb(&self, kind: CallKind) -> &'static str {
        let verbs = kind.verbs();
        if self.running_kinds.contains(&kind) {
            verbs.under_way
        } else {
            verbs.ended
        }
    }

    /// "Ran 4 commands, read 1 file, thought 2 times", and "Running 2 commands, read 1 file" while
    /// a command runs.
    pub fn words(&self) -> String {
        let mut parts = Vec::new();
        if self.commands > 0 {
            let verb = self.verb(CallKind::Command);
            parts.push(counted(verb, self.commands, "command", "commands"));
        }
        if !self.files_read.is_empty() {
            let verb = self.verb(CallKind::Read);
            parts.push(counted(verb, self.files_read.len(), "file", "files"));
        }
        if self.searches > 0 {
            parts.push(times(self.verb(CallKind::Search), self.searches));
        }
        if !self.files_edited.is_empty() {
            let verb = self.verb(CallKind::Edit);
            parts.push(counted(verb, self.files_edited.len(), "file", "files"));
        }
        if self.other_calls > 0 {
            let verb = self.verb(CallKind::Other);
            parts.push(counted(verb, self.other_calls, "tool", "tools"));
        }
        if self.thoughts > 0 {
            parts.push(times("thought", self.thoughts));
        }
        capitalized(&parts.join(", "))
    }
}

/// "ran 1 command", "ran 4 commands".
fn counted(verb: &str, number: usize, singular: &str, plural: &str) -> String {
    format!("{verb} {}", format::count(number, singular, plural))
}

/// "thought once", "thought 2 times".
fn times(verb: &str, number: usize) -> String {
    if number == 1 {
        format!("{verb} once")
    } else {
        format!("{verb} {number} times")
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{EditTexts, FileEdit, ToolCallStatus};
    use serde_json::json;

    use crate::ui::tool_rows;

    use super::*;

    fn item(id: u32, body: ItemBody) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body,
        }
    }

    fn call(id: u32, tool: &str, summary: &str, status: ToolCallStatus) -> TranscriptItem {
        item(
            id,
            ItemBody::ToolCall(ToolCall {
                tool: tool.to_owned(),
                summary: summary.to_owned(),
                input: json!({}),
                status,
                output: None,
                exit_code: None,
                edit: None,
            }),
        )
    }

    fn bash(id: u32) -> TranscriptItem {
        call(id, "Bash", "cargo test", ToolCallStatus::Done)
    }

    fn thinking(id: u32) -> TranscriptItem {
        item(
            id,
            ItemBody::Thinking {
                text: "The lexer counts from 0.".to_owned(),
            },
        )
    }

    fn text(id: u32) -> TranscriptItem {
        item(
            id,
            ItemBody::AssistantText {
                markdown: "Fixed.".to_owned(),
            },
        )
    }

    #[test]
    fn work_between_messages_folds_and_a_lone_call_stays() {
        let items = [
            thinking(1),
            bash(2),
            bash(3),
            text(4),
            bash(5),
            text(6),
            thinking(7),
            bash(8),
        ];
        let rows: Vec<Row<'_>> = items.iter().map(Row::Item).collect();

        assert_eq!(
            units(&rows, &BTreeSet::new(), None),
            [
                Unit::Run(0..3),
                Unit::Row(3),
                Unit::Row(4),
                Unit::Row(5),
                Unit::Run(6..8),
            ]
        );
    }

    #[test]
    fn a_call_waiting_for_approval_and_the_new_divider_end_a_run() {
        let items = [bash(1), bash(2), bash(3), bash(4), bash(5)];
        let rows: Vec<Row<'_>> = items.iter().map(Row::Item).collect();
        let awaiting = BTreeSet::from([ItemId::new(3)]);

        assert_eq!(
            units(&rows, &awaiting, Some(ItemId::new(5))),
            [Unit::Run(0..2), Unit::Row(2), Unit::Row(3), Unit::Row(4),]
        );
    }

    #[test]
    fn work_under_another_parent_starts_another_run() {
        let mut orphans = [bash(3), bash(4)];
        for orphan in &mut orphans {
            orphan.parent = Some(ItemId::new(99));
        }
        let [orphan_3, orphan_4] = orphans;
        let items = [bash(1), bash(2), orphan_3, orphan_4];
        let rows: Vec<Row<'_>> = items.iter().map(Row::Item).collect();

        assert_eq!(
            units(&rows, &BTreeSet::new(), None),
            [Unit::Run(0..2), Unit::Run(2..4)]
        );
    }

    #[test]
    fn a_new_run_counts_once_for_the_jump_to_the_latest() {
        let items = [text(1), bash(2), thinking(3), bash(4), text(5)];
        let rows: Vec<Row<'_>> = items.iter().map(Row::Item).collect();
        let first_new = Some(ItemId::new(2));
        let units = units(&rows, &BTreeSet::new(), first_new);

        assert_eq!(new_units(&rows, &units, first_new), 2);
        assert_eq!(new_units(&rows, &units, None), 0);
    }

    #[test]
    fn a_new_run_of_reads_counts_once_for_the_jump_to_the_latest() {
        let read = |id: u32| call(id, "Read", "src/lexer.rs", ToolCallStatus::Done);
        let items = [read(1), text(2), read(3), read(4), read(5)];
        let refs: Vec<&TranscriptItem> = items.iter().collect();
        let first_new = Some(ItemId::new(2));
        let rows = tool_rows::rows(&refs, &BTreeSet::new(), first_new);
        let units = units(&rows, &BTreeSet::new(), first_new);

        assert_eq!(new_units(&rows, &units, first_new), 2);
    }

    #[test]
    fn a_run_says_what_it_did() {
        let mut edit = call(5, "Edit", "src/lexer.rs", ToolCallStatus::Done);
        if let ItemBody::ToolCall(call) = &mut edit.body {
            call.edit = Some(FileEdit {
                path: PathBuf::from("/home/dev/calc/src/lexer.rs"),
                old_text: "0".to_owned(),
                new_text: "1".to_owned(),
                texts: EditTexts::Kept,
            });
        }
        let items = [
            thinking(1),
            bash(2),
            call(3, "Read", "src/lexer.rs", ToolCallStatus::Done),
            call(4, "Read", "src/lexer.rs", ToolCallStatus::Done),
            edit,
            call(6, "Grep", "column", ToolCallStatus::Failed),
            thinking(7),
            bash(8),
        ];
        let rows: Vec<Row<'_>> = items.iter().map(Row::Item).collect();

        let summary = WorkSummary::of(&rows);

        assert_eq!(
            summary.words(),
            "Ran 2 commands, read 1 file, searched once, edited 1 file, thought 2 times"
        );
        assert_eq!(summary.failed, 1);
        assert!(!summary.running);
    }

    #[test]
    fn a_run_says_the_kind_of_its_running_call_as_under_way() {
        let items = [
            call(1, "Read", "CHANGELOG.md", ToolCallStatus::Done),
            bash(2),
            call(3, "Bash", "cargo test parser", ToolCallStatus::Running),
            thinking(4),
        ];
        let rows: Vec<Row<'_>> = items.iter().map(Row::Item).collect();

        let summary = WorkSummary::of(&rows);

        assert_eq!(
            summary.words(),
            "Running 2 commands, read 1 file, thought once"
        );
        assert!(summary.running);
    }

    #[test]
    fn a_run_says_every_kind_of_running_call_as_under_way() {
        let mut edit = call(4, "Edit", "src/lexer.rs", ToolCallStatus::Running);
        if let ItemBody::ToolCall(call) = &mut edit.body {
            call.edit = Some(FileEdit {
                path: PathBuf::from("/home/dev/calc/src/lexer.rs"),
                old_text: "0".to_owned(),
                new_text: "1".to_owned(),
                texts: EditTexts::Kept,
            });
        }
        let items = [
            call(1, "Bash", "cargo test", ToolCallStatus::Running),
            call(2, "Read", "src/lexer.rs", ToolCallStatus::Running),
            call(3, "Grep", "column", ToolCallStatus::Running),
            edit,
            call(5, "WebFetch", "docs.rs", ToolCallStatus::Running),
            thinking(6),
        ];
        let rows: Vec<Row<'_>> = items.iter().map(Row::Item).collect();

        assert_eq!(
            WorkSummary::of(&rows).words(),
            "Running 1 command, reading 1 file, searching once, editing 1 file, using 1 tool, thought once"
        );
    }
}
