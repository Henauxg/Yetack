//! The matches of the chat's find in what the chat folds: a call's input and output, an edit's
//! lines, thinking, a subagent's work, the instructions of a skill, a notice's detail. The page
//! holds none of it until the fold opens, so the app counts them from the transcript, and a fold
//! that is closed shows its count on a chip. The find's page script counts each chip as that many
//! matches, and opens the fold when the find reaches it.
//!
//! The counts follow what each fold shows once open, from the item's text: a Markdown text counts
//! its marks too, and an output counts each line of a run the chat shows once. The page counts
//! the matches again once the fold is open.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use cockpit_protocol::{AgentId, ItemBody, ItemId, ToolCall, ToolCallStatus, TranscriptItem};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::render::{self, DiffRow, SearchTool};
use crate::state::find::{FindPane, Needle};
use crate::state::{AgentEntry, accounts, actions, awaiting_approval};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::tool_rows::{self, Row};
use crate::ui::transcript::{
    CallStage, Children, EDIT_CONTEXT_LINES, EDIT_PREVIEW_ROWS, ToolShape, children_by_parent,
    hides_output, top_items, waits_for_answer,
};

/// What closes over hidden matches: an item's own fold, or a run of reads and searches, which its
/// first item names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FoldKey {
    Item(ItemId),
    Explored(ItemId),
}

/// The matches in each fold that holds some.
pub(crate) type FoldMatches = BTreeMap<FoldKey, u32>;

/// The selected agent's fold matches, for the rows of its chat.
#[derive(Clone, Copy)]
struct ChatFolds(Memo<FoldMatches>);

/// What the folds' text depends on: the agent's transcript, and how its rows group.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GatheredFor {
    agent: AgentId,
    revision: u64,
    /// A run of reads ends at the "New" divider.
    divider: Option<ItemId>,
    /// The sign-in card takes the place of its notice.
    sign_in_notice: Option<ItemId>,
}

/// The folds' text, gathered once per change of the transcript, and the counts of the last text
/// looked for. Typing in the find's field counts again, and gathers nothing.
#[derive(Default)]
struct Kept {
    texts: Option<(GatheredFor, Rc<FoldTexts>)>,
    matches: Option<(GatheredFor, Needle, FoldMatches)>,
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
        if let Some((counted_for, counted_needle, matches)) = &kept.matches
            && *counted_for == gathered_for
            && *counted_needle == needle
        {
            return matches.clone();
        }
        let texts = match &kept.texts {
            Some((texts_for, texts)) if *texts_for == gathered_for => Rc::clone(texts),
            Some(_) | None => {
                let texts = Rc::new(chat_fold_texts(entry, &gathered_for));
                kept.texts = Some((gathered_for.clone(), Rc::clone(&texts)));
                texts
            }
        };
        let matches = texts.matches(&needle);
        kept.matches = Some((gathered_for, needle, matches.clone()));
        matches
    });
    use_context_provider(|| ChatFolds(folds));
}

fn chat_fold_texts(entry: &AgentEntry, gathered: &GatheredFor) -> FoldTexts {
    let items: Vec<&TranscriptItem> = entry.transcript.items().collect();
    let loaded: BTreeSet<ItemId> = items.iter().map(|item| item.id).collect();
    let top = top_items(&items, &loaded, gathered.sign_in_notice);
    let awaiting = awaiting_approval(&items);
    let rows = tool_rows::rows(&top, &awaiting, gathered.divider);
    let children = children_by_parent(&items);
    FoldTexts::of(&rows, &children, &awaiting)
}

/// A text a fold shows once open, with every fold it sits in: a fold inside a subagent counts for
/// the subagent's chip while the subagent is closed, and for its own once it opens.
#[derive(Debug)]
struct Piece {
    text: String,
    folds: Vec<FoldKey>,
}

/// The text of every fold of the chat's rows, at any depth.
#[derive(Debug, Default)]
pub(crate) struct FoldTexts {
    pieces: Vec<Piece>,
}

impl FoldTexts {
    pub(crate) fn of(
        rows: &[Row<'_>],
        children: &Children<'_>,
        awaiting: &BTreeSet<ItemId>,
    ) -> Self {
        let mut gatherer = Gatherer {
            children,
            awaiting,
            open: Vec::new(),
            pieces: Vec::new(),
            subagents: BTreeSet::new(),
        };
        for row in rows {
            gatherer.row(row);
        }
        Self {
            pieces: gatherer.pieces,
        }
    }

    /// The matches in each fold that holds some.
    pub(crate) fn matches(&self, needle: &Needle) -> FoldMatches {
        let mut found = FoldMatches::new();
        for piece in &self.pieces {
            let count = u32::try_from(needle.count(&piece.text)).unwrap_or(u32::MAX);
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
        let preview = render::diff_lines(
            &edit.old_text,
            &edit.new_text,
            None,
            EDIT_CONTEXT_LINES,
            1,
            EDIT_PREVIEW_ROWS,
        );
        for row in &preview.rows {
            if let DiffRow::Line(line) = row {
                let text: String = line
                    .segments
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .collect();
                self.add(&text);
            }
        }
        if call.status == ToolCallStatus::Failed {
            self.add(call.output.as_deref().unwrap_or_default());
        }
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
            ItemBody::ToolCall(call) | ItemBody::Subagent(call) => {
                self.add(&call.tool);
                self.add(&call.summary);
            }
            _ => {}
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
            class: "find-chip",
            "data-find-folded": "{count}",
            title: "{matches} in what this line folds. The find opens it when it gets there.",
            Icon { kind: IconKind::Search, size: 10 }
            "{count}"
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{FileEdit, NoticeKind, UserMessage};
    use serde_json::json;

    use super::*;

    fn item(id: u32, parent: Option<u32>, body: ItemBody) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: parent.map(ItemId::new),
            body,
        }
    }

    fn call(tool: &str, summary: &str, input: serde_json::Value, output: &str) -> ToolCall {
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
        let refs: Vec<&TranscriptItem> = items.iter().collect();
        let loaded: BTreeSet<ItemId> = refs.iter().map(|item| item.id).collect();
        let top = top_items(&refs, &loaded, None);
        let awaiting = awaiting_approval(&refs);
        let rows = tool_rows::rows(&top, &awaiting, None);
        let children = children_by_parent(&refs);
        FoldTexts::of(&rows, &children, &awaiting).matches(&Needle::new(query, false).unwrap())
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

        assert_eq!(
            found,
            FoldMatches::from([(FoldKey::Item(ItemId::new(1)), 4)])
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
        });

        let found = matches(&[item(1, None, ItemBody::ToolCall(write))], "column");

        assert_eq!(
            found,
            FoldMatches::from([(FoldKey::Item(ItemId::new(1)), EDIT_PREVIEW_ROWS as u32)])
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
