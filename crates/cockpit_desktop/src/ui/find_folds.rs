//! The matches of the chat's find in what the chat folds: a call's input and output, an edit's
//! lines, thinking, a subagent's work, the instructions of a skill, a notice's detail. The page
//! holds none of it until the fold opens, so the app counts them from the transcript, and a fold
//! that is closed shows its count on a chip. The find's page script counts each chip as that many
//! matches, and opens the fold when the find reaches it.
//!
//! The counts follow what each fold shows once open, from the item's text: a Markdown text counts
//! its marks too, and an output counts each line of a run the chat shows once. The page counts
//! the matches again once the fold is open.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;

use cockpit_protocol::{
    AgentId, EditTexts, ItemBody, ItemId, ToolCall, ToolCallStatus, TranscriptItem,
};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::render::{self, DiffRow, SearchTool};
use crate::state::find::{FindPane, Needle};
use crate::state::{AgentEntry, Revision, accounts, actions, awaiting_approval};
use crate::ui::code::TOO_LARGE_TO_SHOW;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::tool_rows::{self, Row};
use crate::ui::transcript::{
    CallStage, Children, EDIT_CONTEXT_LINES, EDIT_PREVIEW_ROWS, SUBAGENT_NAME, THINKING_LABEL,
    ToolShape, children_by_parent, compact_line, hides_output, top_items, waits_for_answer,
};
use crate::ui::work_runs::{self, Unit};

/// What closes over hidden matches: an item's own fold, a run of reads and searches, or a run of
/// the agent's work, which its first item names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FoldKey {
    Item(ItemId),
    Explored(ItemId),
    Run(ItemId),
}

/// The matches in each fold that holds some.
pub(crate) type FoldMatches = BTreeMap<FoldKey, u32>;

/// A closed fold's chip. The find's script reads its count from its text.
pub(super) const CHIP_CLASS: &str = "find-chip";

/// The selected agent's fold matches, for the rows of its chat.
#[derive(Clone, Copy)]
struct ChatFolds(Memo<FoldMatches>);

/// What the folds' text depends on: the agent's transcript, and how its rows group.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GatheredFor {
    agent: AgentId,
    revision: Revision,
    /// A run of reads ends at the "New" divider.
    divider: Option<ItemId>,
    /// The sign-in card takes the place of its notice.
    sign_in_notice: Option<ItemId>,
}

/// The folds' text, gathered once per change of the transcript, and the counts of the last text
/// looked for. Typing in the find's field counts again, and gathers nothing.
#[derive(Default)]
struct Kept {
    texts: Option<Gathered>,
    matches: Option<Counted>,
    previews: Previews,
}

#[derive(Debug)]
struct Gathered {
    gathered_for: GatheredFor,
    texts: FoldTexts,
}

#[derive(Debug)]
struct Counted {
    gathered_for: GatheredFor,
    needle: Needle,
    matches: FoldMatches,
}

/// The lines of each edit's preview, from one gathering to the next: the diff behind them is the
/// costly part of a gathering, and an edit's texts seldom change. Each gathering keeps only the
/// edits it met.
#[derive(Debug, Default)]
pub(crate) struct Previews {
    agent: Option<AgentId>,
    by_edit: HashMap<ItemId, Preview>,
}

#[derive(Debug)]
struct Preview {
    /// A hash of the edit's old and new texts, which the lines come from.
    texts_hash: u64,
    lines: Vec<String>,
}

/// Provides the fold matches of the selected agent's chat to the rows under the caller.
pub(super) fn use_chat_folds(cockpit: Cockpit) {
    let mut kept = use_hook(|| CopyValue::new(Kept::default()));
    let folds = use_memo(move || {
        let state = cockpit.state.read();
        let drafts = cockpit.drafts.read();
        let Some(agent) = state.ui.selected else {
            return FoldMatches::new();
        };
        let (Some(entry), Some(needle)) = (
            state.agents.get(&agent),
            Needle::of_open_find(&state, &drafts, FindPane::Chat, agent),
        ) else {
            return FoldMatches::new();
        };
        let gathered_for = GatheredFor {
            agent,
            revision: entry.transcript.revision(),
            divider: actions::new_divider(&state, agent),
            sign_in_notice: accounts::sign_in_turn(entry).and_then(|turn| turn.notice),
        };
        let mut kept = kept.write();
        if let Some(counted) = &kept.matches
            && counted.gathered_for == gathered_for
            && counted.needle == needle
        {
            return counted.matches.clone();
        }
        let texts = match kept.texts.take() {
            Some(gathered) if gathered.gathered_for == gathered_for => gathered.texts,
            Some(_) | None => chat_fold_texts(entry, &gathered_for, &mut kept.previews),
        };
        let matches = texts.matches(&needle);
        kept.texts = Some(Gathered {
            gathered_for: gathered_for.clone(),
            texts,
        });
        kept.matches = Some(Counted {
            gathered_for,
            needle,
            matches: matches.clone(),
        });
        matches
    });
    use_context_provider(|| ChatFolds(folds));
}

fn chat_fold_texts(
    entry: &AgentEntry,
    gathered: &GatheredFor,
    previews: &mut Previews,
) -> FoldTexts {
    if previews.agent != Some(gathered.agent) {
        previews.agent = Some(gathered.agent);
        previews.by_edit.clear();
    }
    let items: Vec<&TranscriptItem> = entry.transcript.items().collect();
    let loaded: BTreeSet<ItemId> = items.iter().map(|item| item.id).collect();
    let top = top_items(&items, &loaded, gathered.sign_in_notice);
    let awaiting = awaiting_approval(&items);
    let rows = tool_rows::rows(&top, &awaiting, gathered.divider);
    let children = children_by_parent(&items);
    FoldTexts::gather(
        &rows,
        &children,
        &awaiting,
        gathered.divider,
        &entry.summary.folder,
        previews,
    )
}

/// A text a fold shows once open, with every fold it sits in: a fold inside a subagent counts for
/// the subagent's chip while the subagent is closed, and for its own once it opens.
#[derive(Debug)]
struct Piece {
    text: String,
    /// `text` in lower case, made once for the texts past ASCII a find looks for.
    lowercase: OnceCell<String>,
    folds: Vec<FoldKey>,
}

/// The text of every fold of the chat's rows, at any depth.
#[derive(Debug, Default)]
pub(crate) struct FoldTexts {
    pieces: Vec<Piece>,
}

impl FoldTexts {
    /// `folder` is the agent's, from which an edit's line shows its path. `previews` holds the
    /// edits' lines of the last gathering, and gets this one's. The rows fold into runs of work as
    /// the chat folds them, which `divider` ends.
    pub(crate) fn gather(
        rows: &[Row<'_>],
        children: &Children<'_>,
        awaiting: &BTreeSet<ItemId>,
        divider: Option<ItemId>,
        folder: &Path,
        previews: &mut Previews,
    ) -> Self {
        let mut gatherer = Gatherer {
            children,
            awaiting,
            folder,
            kept_previews: std::mem::take(&mut previews.by_edit),
            previews: &mut previews.by_edit,
            open: Vec::new(),
            pieces: Vec::new(),
            subagents: BTreeSet::new(),
        };
        for unit in work_runs::units(rows, awaiting, divider) {
            match unit {
                Unit::Row(index) => {
                    if let Some(row) = rows.get(index) {
                        gatherer.row(row);
                    }
                }
                Unit::Run(range) => {
                    let run = rows.get(range).unwrap_or_default();
                    let Some(first) = run.first() else {
                        continue;
                    };
                    gatherer.within(FoldKey::Run(first.first().id), |gatherer| {
                        for row in run {
                            gatherer.run_line(row);
                            gatherer.row(row);
                        }
                    });
                }
            }
        }
        Self {
            pieces: gatherer.pieces,
        }
    }

    /// The matches in each fold that holds some.
    pub(crate) fn matches(&self, needle: &Needle) -> FoldMatches {
        let mut found = FoldMatches::new();
        for piece in &self.pieces {
            let count = if needle.needs_lowercase() {
                needle.count_lowercase(piece.lowercase.get_or_init(|| piece.text.to_lowercase()))
            } else {
                needle.count(&piece.text)
            };
            let count = u32::try_from(count).unwrap_or(u32::MAX);
            if count == 0 {
                continue;
            }
            for fold in &piece.folds {
                let matches = found.entry(*fold).or_default();
                *matches = matches.saturating_add(count);
            }
        }
        found
    }
}

/// Where an item shows: a call in the main conversation shows its input and output when opened,
/// and a call in a subagent or a run of reads shows one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Top,
    Nested,
}

struct Gatherer<'run, 'item> {
    children: &'run Children<'item>,
    awaiting: &'run BTreeSet<ItemId>,
    folder: &'run Path,
    /// The previews of the last gathering, which this one takes from.
    kept_previews: HashMap<ItemId, Preview>,
    previews: &'run mut HashMap<ItemId, Preview>,
    /// The folds the texts added now sit in, the outermost first.
    open: Vec<FoldKey>,
    pieces: Vec<Piece>,
    /// The subagents gathered: parents that form a cycle count once.
    subagents: BTreeSet<ItemId>,
}

impl Gatherer<'_, '_> {
    fn add(&mut self, text: &str) {
        if !self.open.is_empty() && !text.is_empty() {
            self.pieces.push(Piece {
                text: text.to_owned(),
                lowercase: OnceCell::new(),
                folds: self.open.clone(),
            });
        }
    }

    fn within(&mut self, fold: FoldKey, gather: impl FnOnce(&mut Self)) {
        self.open.push(fold);
        gather(self);
        self.open.pop();
    }

    fn row(&mut self, row: &Row<'_>) {
        match row {
            Row::Item(item) => self.item(item, Place::Top),
            Row::Skill { call, instructions } => {
                let detail = instructions.and_then(|notice| match &notice.body {
                    ItemBody::Notice { detail, .. } => detail.as_deref(),
                    _ => None,
                });
                self.within(FoldKey::Item(call.id), |gatherer| {
                    gatherer.add(detail.unwrap_or_default());
                });
            }
            Row::Explored { first, rest } => {
                self.within(FoldKey::Explored(first.id), |gatherer| {
                    for call in std::iter::once(first).chain(rest) {
                        gatherer.shown(call);
                        gatherer.item(call, Place::Nested);
                    }
                });
            }
        }
    }

    /// What `item`'s own fold shows once open, and the folds inside it.
    fn item(&mut self, item: &TranscriptItem, place: Place) {
        let fold = FoldKey::Item(item.id);
        match &item.body {
            ItemBody::Thinking { text } => self.within(fold, |gatherer| gatherer.add(text)),
            ItemBody::Notice {
                detail: Some(detail),
                ..
            } => self.within(fold, |gatherer| gatherer.add(detail)),
            ItemBody::ToolCall(call) => match place {
                Place::Top => self.within(fold, |gatherer| gatherer.call(item.id, call)),
                Place::Nested => self.within(fold, |gatherer| gatherer.found_output(call)),
            },
            ItemBody::Subagent(call) if self.subagents.insert(item.id) => {
                self.within(fold, |gatherer| gatherer.subagent(item.id, call));
            }
            _ => {}
        }
    }

    /// A call of the main conversation: an edit's lines, or the input and output.
    fn call(&mut self, id: ItemId, call: &ToolCall) {
        let Some(edit) = &call.edit else {
            let stage = CallStage::new(call, self.awaiting.contains(&id));
            self.add(ToolShape::new(call).input().unwrap_or_default());
            if !hides_output(call, stage) {
                self.add(call.output.as_deref().unwrap_or_default());
            }
            return;
        };
        match edit.texts {
            EditTexts::Kept => self.preview(id, &edit.old_text, &edit.new_text),
            EditTexts::TooLarge => self.add(TOO_LARGE_TO_SHOW),
        }
        if call.status == ToolCallStatus::Failed {
            self.add(call.output.as_deref().unwrap_or_default());
        }
    }

    /// The lines an edit's preview shows, from the last gathering while its texts are the same.
    fn preview(&mut self, id: ItemId, old: &str, new: &str) {
        let mut hasher = DefaultHasher::new();
        (old, new).hash(&mut hasher);
        let texts_hash = hasher.finish();
        let lines: Vec<String> = match self.kept_previews.remove(&id) {
            Some(kept) if kept.texts_hash == texts_hash => kept.lines,
            Some(_) | None => {
                let diff =
                    render::diff_lines(old, new, None, EDIT_CONTEXT_LINES, 1, EDIT_PREVIEW_ROWS);
                diff.rows
                    .iter()
                    .filter_map(|row| match row {
                        DiffRow::Line(line) => Some(
                            line.segments
                                .iter()
                                .map(|segment| segment.text.as_str())
                                .collect(),
                        ),
                        DiffRow::Folded { .. } => None,
                    })
                    .collect()
            }
        };
        for line in &lines {
            self.add(line);
        }
        self.previews.insert(id, Preview { texts_hash, lines });
    }

    /// A search's line in a subagent or a run of reads opens on what it found.
    fn found_output(&mut self, call: &ToolCall) {
        if call.tool.parse::<SearchTool>().is_ok() {
            self.add(call.output.as_deref().unwrap_or_default());
        }
    }

    /// Opened, a subagent shows its own items on their lines, and its result. A card that waits
    /// for an answer shows in the main conversation instead.
    fn subagent(&mut self, id: ItemId, call: &ToolCall) {
        self.add(call.output.as_deref().unwrap_or_default());
        let children = self.children;
        let shown = children
            .get(&id)
            .into_iter()
            .flatten()
            .filter(|child| !waits_for_answer(child));
        for child in shown {
            self.shown(child);
            self.item(child, Place::Nested);
        }
    }

    /// What an item inside a subagent or a run of reads shows without opening.
    fn shown(&mut self, item: &TranscriptItem) {
        match &item.body {
            ItemBody::UserMessage(message) => self.add(&message.text),
            ItemBody::AssistantText { markdown } => self.add(markdown),
            ItemBody::Notice { text, .. } => self.add(text),
            ItemBody::ToolCall(call) => {
                let stage = CallStage::new(call, self.awaiting.contains(&item.id));
                for words in compact_line(self.folder, call, stage) {
                    self.add(&words);
                }
            }
            ItemBody::Subagent(call) => {
                self.add(SUBAGENT_NAME);
                self.add(&call.summary);
            }
            _ => {}
        }
    }

    /// The line a row of a run of work shows once the run opens.
    fn run_line(&mut self, row: &Row<'_>) {
        let words = match row {
            Row::Explored { .. } => tool_rows::explored_line(row.items()),
            Row::Skill { call, .. } => match &call.body {
                ItemBody::ToolCall(skill) => {
                    let stage = CallStage::new(skill, self.awaiting.contains(&call.id));
                    tool_rows::skill_line(skill, stage)
                }
                _ => Vec::new(),
            },
            Row::Item(item) => match &item.body {
                ItemBody::Thinking { .. } => vec![THINKING_LABEL.to_owned()],
                ItemBody::ToolCall(_) => {
                    self.shown(item);
                    Vec::new()
                }
                _ => Vec::new(),
            },
        };
        for word in &words {
            self.add(word);
        }
    }
}

/// The matches of the chat's find in what a fold hides, on the fold's line while it is closed.
/// Nothing shows outside the chat, such as in the agent map.
#[component]
pub(crate) fn FoldChip(fold: FoldKey) -> Element {
    let Some(ChatFolds(folds)) = try_use_context::<ChatFolds>() else {
        return rsx! {};
    };
    let Some(count) = folds.read().get(&fold).copied() else {
        return rsx! {};
    };
    let matches = if count == 1 {
        "1 match".to_owned()
    } else {
        format!("{count} matches")
    };
    rsx! {
        span {
            class: CHIP_CLASS,
            title: "{matches} in what this line folds. The find opens it when it gets there.",
            Icon { kind: IconKind::Search, size: 10 }
            "{count}"
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{EditTexts, FileEdit, NoticeKind, UserMessage};
    use serde_json::{Value, json};

    use super::*;

    fn item(id: u32, parent: Option<u32>, body: ItemBody) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: parent.map(ItemId::new),
            body,
        }
    }

    fn call(tool: &str, summary: &str, input: Value, output: &str) -> ToolCall {
        ToolCall {
            tool: tool.to_owned(),
            summary: summary.to_owned(),
            input,
            status: ToolCallStatus::Done,
            output: Some(output.to_owned()),
            exit_code: None,
            edit: None,
        }
    }

    fn matches(items: &[TranscriptItem], query: &str) -> FoldMatches {
        matches_from(items, query, None)
    }

    /// With the "New" divider above `divider`.
    fn matches_from(items: &[TranscriptItem], query: &str, divider: Option<u32>) -> FoldMatches {
        let refs: Vec<&TranscriptItem> = items.iter().collect();
        let loaded: BTreeSet<ItemId> = refs.iter().map(|item| item.id).collect();
        let top = top_items(&refs, &loaded, None);
        let awaiting = awaiting_approval(&refs);
        let rows = tool_rows::rows(&top, &awaiting, divider.map(ItemId::new));
        let children = children_by_parent(&refs);
        FoldTexts::gather(
            &rows,
            &children,
            &awaiting,
            divider.map(ItemId::new),
            Path::new("/work/app"),
            &mut Previews::default(),
        )
        .matches(&Needle::new(query, false).unwrap())
    }

    #[test]
    fn a_call_counts_its_input_and_output_and_a_read_only_its_line() {
        let items = [
            item(
                1,
                None,
                ItemBody::ToolCall(call(
                    "Bash",
                    "cargo test",
                    json!({"command": "cargo test column", "description": "Run the tests"}),
                    "test columns_count ... FAILED\ncolumn 4, expected column 5",
                )),
            ),
            item(
                2,
                None,
                ItemBody::ToolCall(call(
                    "Read",
                    "src/column.rs",
                    json!({"file_path": "src/column.rs"}),
                    "1\tpub struct Column;",
                )),
            ),
        ];

        let found = matches(&items, "COLUMN");

        // The two calls fold into a run, which also hides the read's line.
        assert_eq!(
            found,
            FoldMatches::from([
                (FoldKey::Item(ItemId::new(1)), 4),
                (FoldKey::Run(ItemId::new(1)), 5),
            ])
        );
    }

    #[test]
    fn an_edit_counts_the_lines_its_preview_shows() {
        let mut write = call("Write", "src/lexer.rs", json!({}), "");
        write.output = None;
        let new_text: String = (1..=20).map(|line| format!("column {line}\n")).collect();
        write.edit = Some(FileEdit {
            path: "src/lexer.rs".into(),
            old_text: String::new(),
            new_text,
            texts: EditTexts::Kept,
        });

        let found = matches(&[item(1, None, ItemBody::ToolCall(write))], "column");

        assert_eq!(
            found,
            FoldMatches::from([(FoldKey::Item(ItemId::new(1)), EDIT_PREVIEW_ROWS as u32)])
        );
    }

    #[test]
    fn an_edit_too_large_to_keep_counts_the_note_in_place_of_its_lines() {
        let mut write = call("Write", "data/prices.csv", json!({}), "");
        write.output = None;
        write.edit = Some(FileEdit {
            path: "data/prices.csv".into(),
            old_text: String::new(),
            new_text: String::new(),
            texts: EditTexts::TooLarge,
        });

        let found = matches(&[item(1, None, ItemBody::ToolCall(write))], "too large");

        assert_eq!(
            found,
            FoldMatches::from([(FoldKey::Item(ItemId::new(1)), 1)])
        );
    }

    #[test]
    fn a_subagent_counts_what_it_shows_and_what_folds_inside_it() {
        let agent = call(
            "Agent",
            "Find the column",
            json!({}),
            "The column is in lexer.rs",
        );
        let items = [
            item(1, None, ItemBody::Subagent(agent)),
            item(
                2,
                Some(1),
                ItemBody::UserMessage(UserMessage {
                    text: "Look for the column".to_owned(),
                    images: Vec::new(),
                    sent_at: None,
                }),
            ),
            item(
                3,
                Some(1),
                ItemBody::Thinking {
                    text: "column, column".to_owned(),
                },
            ),
            item(
                4,
                Some(1),
                ItemBody::ToolCall(call("Grep", "column", json!({}), "src/lexer.rs:12: column")),
            ),
            item(
                5,
                Some(1),
                ItemBody::ToolCall(call("Bash", "ls", json!({"command": "ls"}), "column.rs")),
            ),
            item(
                6,
                None,
                ItemBody::Notice {
                    text: "Skill instructions".to_owned(),
                    detail: Some("No match here.".to_owned()),
                    kind: NoticeKind::Plain,
                },
            ),
        ];

        let found = matches(&items, "column");

        // Result 1, prompt 1, thinking 2, the search's line 1 and its output 1. The command shows
        // its line only, which holds none.
        assert_eq!(
            found,
            FoldMatches::from([
                (FoldKey::Item(ItemId::new(1)), 6),
                (FoldKey::Item(ItemId::new(3)), 2),
                (FoldKey::Item(ItemId::new(4)), 1),
            ])
        );
    }

    #[test]
    fn the_folds_of_the_main_conversation_count_what_they_open_on() {
        let skill = call(
            "Skill",
            "review",
            json!({"skill": "review"}),
            "Launching skill",
        );
        let mut failed = call(
            "Edit",
            "src/grid.rs",
            json!({}),
            "<tool_use_error>column not found</tool_use_error>",
        );
        failed.status = ToolCallStatus::Failed;
        failed.edit = Some(FileEdit {
            path: "src/grid.rs".into(),
            old_text: "column".to_owned(),
            new_text: "row".to_owned(),
            texts: EditTexts::Kept,
        });
        let items = [
            item(
                1,
                None,
                ItemBody::Thinking {
                    text: "Which column?".to_owned(),
                },
            ),
            item(2, None, ItemBody::ToolCall(skill)),
            item(
                3,
                None,
                ItemBody::Notice {
                    text: cockpit_protocol::skill_instructions_title("review"),
                    detail: Some("Check each column, then each row.".to_owned()),
                    kind: NoticeKind::Plain,
                },
            ),
            item(4, None, ItemBody::ToolCall(failed)),
            item(
                5,
                None,
                ItemBody::Notice {
                    text: "A message".to_owned(),
                    detail: Some("The column is fixed.".to_owned()),
                    kind: NoticeKind::Message,
                },
            ),
        ];

        // The edit's removed line and its error each hold one. The thinking, the skill and the
        // edit fold into a run, and the message stays out of it.
        assert_eq!(
            matches(&items, "column"),
            FoldMatches::from([
                (FoldKey::Item(ItemId::new(1)), 1),
                (FoldKey::Item(ItemId::new(2)), 1),
                (FoldKey::Item(ItemId::new(4)), 2),
                (FoldKey::Item(ItemId::new(5)), 1),
                (FoldKey::Run(ItemId::new(1)), 4),
            ])
        );
    }

    #[test]
    fn a_run_of_work_counts_the_lines_of_its_reads_and_its_skill_as_they_show() {
        let read = |id: u32| {
            item(
                id,
                None,
                ItemBody::ToolCall(call("Read", "src/column.rs", json!({}), "1\tfn f() {}")),
            )
        };
        let skill = call(
            "Skill",
            "column-fix",
            json!({"skill": "column-fix", "args": "the column"}),
            "Launching skill",
        );
        let items = [read(1), read(2), item(3, None, ItemBody::ToolCall(skill))];

        // Once open, the run shows the reads' line, "Explored 1 file read", whose fold holds the
        // two reads, and the skill's line, "/column-fix the column".
        assert_eq!(
            matches(&items, "column"),
            FoldMatches::from([
                (FoldKey::Explored(ItemId::new(1)), 2),
                (FoldKey::Run(ItemId::new(1)), 4),
            ])
        );
    }

    #[test]
    fn an_edit_whose_texts_changed_counts_its_new_lines() {
        let edit = |new_text: &str| {
            let mut write = call("Write", "src/grid.rs", json!({}), "");
            write.edit = Some(FileEdit {
                path: "src/grid.rs".into(),
                old_text: String::new(),
                new_text: new_text.to_owned(),
                texts: EditTexts::Kept,
            });
            [item(1, None, ItemBody::ToolCall(write))]
        };
        let mut previews = Previews::default();
        let mut gather = |items: &[TranscriptItem]| {
            let refs: Vec<&TranscriptItem> = items.iter().collect();
            let awaiting = BTreeSet::new();
            let rows = tool_rows::rows(&refs, &awaiting, None);
            FoldTexts::gather(
                &rows,
                &children_by_parent(&refs),
                &awaiting,
                None,
                Path::new("/work/app"),
                &mut previews,
            )
            .matches(&Needle::new("column", false).unwrap())
        };

        let first = gather(&edit("column"));
        let again = gather(&edit("column"));
        let changed = gather(&edit("row"));

        let found = FoldMatches::from([(FoldKey::Item(ItemId::new(1)), 1)]);
        assert_eq!((first, again), (found.clone(), found));
        assert_eq!(changed, FoldMatches::new());
        assert_eq!(previews.by_edit.len(), 1);
    }

    #[test]
    fn the_new_divider_splits_a_run_of_reads_and_its_count() {
        let read = |id: u32| {
            item(
                id,
                None,
                ItemBody::ToolCall(call("Read", "src/column.rs", json!({}), "1	column")),
            )
        };
        let items = [read(1), read(2), read(3), read(4)];

        assert_eq!(
            matches_from(&items, "column", Some(3)),
            FoldMatches::from([
                (FoldKey::Explored(ItemId::new(1)), 2),
                (FoldKey::Explored(ItemId::new(3)), 2),
            ])
        );
    }

    #[test]
    fn a_subagent_counts_its_lines_as_they_show() {
        let bash = call(
            "Bash",
            "rg -c column",
            json!({"command": "rg -c column", "description": "Count the places"}),
            "3",
        );
        let mcp = call(
            "mcp__tickets__list_tickets",
            "open tickets",
            json!({}),
            "[]",
        );
        let mut edit = call("Edit", "/work/app/src/lexer.rs", json!({}), "");
        edit.edit = Some(FileEdit {
            path: "/work/app/src/lexer.rs".into(),
            old_text: "a".to_owned(),
            new_text: "b".to_owned(),
            texts: EditTexts::Kept,
        });
        let items = [
            item(
                1,
                None,
                ItemBody::Subagent(call("Agent", "Look", json!({}), "Done.")),
            ),
            item(2, Some(1), ItemBody::ToolCall(bash)),
            item(3, Some(1), ItemBody::ToolCall(mcp)),
            item(4, Some(1), ItemBody::ToolCall(edit)),
        ];
        let subagent = |query: &str| {
            matches(&items, query)
                .get(&FoldKey::Item(ItemId::new(1)))
                .copied()
        };

        // A command shows its description, an MCP call its server and tool, an edit its path
        // from the agent's folder.
        assert_eq!(subagent("rg -c"), None);
        assert_eq!(subagent("count the places"), Some(1));
        assert_eq!(subagent("mcp__"), None);
        assert_eq!(subagent("[list_tickets]"), Some(1));
        assert_eq!(subagent("/work/app"), None);
        assert_eq!(subagent("src/lexer.rs"), Some(1));
    }

    #[test]
    fn a_run_of_reads_counts_its_lines_and_what_its_searches_found() {
        let items = [
            item(
                1,
                None,
                ItemBody::ToolCall(call(
                    "Grep",
                    "column",
                    json!({}),
                    "Found 2 files\nsrc/column.rs",
                )),
            ),
            item(
                2,
                None,
                ItemBody::ToolCall(call(
                    "Read",
                    "src/lexer.rs",
                    json!({}),
                    "1\tlet column = 1;",
                )),
            ),
        ];

        let found = matches(&items, "column");

        assert_eq!(
            found,
            FoldMatches::from([
                (FoldKey::Explored(ItemId::new(1)), 2),
                (FoldKey::Item(ItemId::new(1)), 1),
            ])
        );
    }
}
