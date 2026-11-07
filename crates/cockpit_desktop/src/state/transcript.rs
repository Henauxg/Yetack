//! An agent's conversation as the app holds it: items merged by id from history pages and live
//! events.

use std::collections::{BTreeMap, BTreeSet};

use cockpit_protocol::{
    AnswerState, HistoryPage, ItemBody, ItemId, OlderItems, PendingId, ToolCallStatus,
    TranscriptItem,
};

/// The most items the app keeps of an agent you are not looking at.
pub const KEPT_ITEMS_AWAY: usize = 2_000;

/// How much of the history the app holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HistoryState {
    /// Only live items, if any: the app asks for the latest page when the agent opens.
    #[default]
    NotLoaded,
    Loading,
    Loaded {
        has_older: bool,
    },
    LoadingOlder,
    Failed(String),
}

impl HistoryState {
    /// Whether the first page of the history is still to come. A failed one is not.
    pub fn awaits_first_page(&self) -> bool {
        matches!(self, Self::NotLoaded | Self::Loading)
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Transcript {
    items: BTreeMap<ItemId, TranscriptItem>,
    history: HistoryState,
}

/// What a card waiting for an answer asks, which decides what the Y, N and number keys do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaitingKind {
    Approval,
    Question,
    Plan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiting {
    pub item: ItemId,
    pub pending: PendingId,
    pub kind: WaitingKind,
}

impl Waiting {
    /// The item, when it is an approval, a question or a plan that waits for an answer.
    fn new(item: &TranscriptItem) -> Option<Self> {
        let (pending, kind) = match &item.body {
            ItemBody::Approval(approval) if approval.answer == AnswerState::Waiting => {
                (&approval.pending, WaitingKind::Approval)
            }
            ItemBody::Question(question) if question.answer == AnswerState::Waiting => {
                (&question.pending, WaitingKind::Question)
            }
            ItemBody::Plan(plan) if plan.answer == AnswerState::Waiting => {
                (&plan.pending, WaitingKind::Plan)
            }
            _ => return None,
        };
        Some(Self {
            item: item.id,
            pending: pending.clone(),
            kind,
        })
    }
}

impl Transcript {
    /// In transcript order.
    pub fn items(&self) -> impl Iterator<Item = &TranscriptItem> {
        self.items.values()
    }

    pub fn get(&self, id: ItemId) -> Option<&TranscriptItem> {
        self.items.get(&id)
    }

    pub fn history(&self) -> &HistoryState {
        &self.history
    }

    pub fn set_history(&mut self, history: HistoryState) {
        self.history = history;
    }

    pub fn first_id(&self) -> Option<ItemId> {
        self.items.keys().next().copied()
    }

    pub fn last_id(&self) -> Option<ItemId> {
        self.items.keys().next_back().copied()
    }

    /// In transcript order, the items after `after`, or all of them with `None`.
    pub fn items_after(&self, after: Option<ItemId>) -> impl Iterator<Item = &TranscriptItem> {
        use std::ops::Bound;

        let start = after.map_or(Bound::Unbounded, Bound::Excluded);
        self.items
            .range((start, Bound::Unbounded))
            .map(|(_, item)| item)
    }

    pub fn append(&mut self, item: TranscriptItem) {
        self.items.insert(item.id, item);
    }

    /// Drops the oldest items past `kept`, and marks that older items exist, so that scrolling up
    /// loads them again through history. While a history page is on its way, nothing is dropped:
    /// the page would come back with a gap before it.
    pub fn trim(&mut self, kept: usize) {
        if matches!(
            self.history,
            HistoryState::Loading | HistoryState::LoadingOlder
        ) {
            return;
        }
        let mut dropped = false;
        while self.items.len() > kept {
            self.items.pop_first();
            dropped = true;
        }
        if let (true, HistoryState::Loaded { has_older }) = (dropped, &mut self.history) {
            *has_older = true;
        }
    }

    /// Replaces the item with the same id. An item the app does not hold is left out: the next
    /// history page brings it in its latest state.
    pub fn change(&mut self, item: TranscriptItem) {
        if let Some(held) = self.items.get_mut(&item.id) {
            *held = item;
        }
    }

    /// Adds a history page. An item already held came from a live event, which is at least as
    /// recent as the page, so it stays.
    pub fn merge_page(&mut self, page: HistoryPage) {
        for item in page.items {
            self.items.entry(item.id).or_insert(item);
        }
        self.history = HistoryState::Loaded {
            has_older: page.older_items == OlderItems::Exist,
        };
    }

    /// The newest approval, question or plan that waits for an answer.
    pub fn waiting(&self) -> Option<Waiting> {
        self.items.values().rev().find_map(Waiting::new)
    }

    /// The newest approval, question or plan of the subagent of item `parent` that waits for an
    /// answer.
    pub fn waiting_under(&self, parent: ItemId) -> Option<Waiting> {
        self.items
            .values()
            .rev()
            .filter(|item| item.parent == Some(parent))
            .find_map(Waiting::new)
    }
}

/// The running tool calls whose approval card waits for an answer. An approval names its call;
/// one from an older daemon's log does not, and matches a call on parent, tool and summary, which
/// the daemon builds the same way for both.
pub fn awaiting_approval(items: &[&TranscriptItem]) -> BTreeSet<ItemId> {
    let mut linked: BTreeSet<ItemId> = BTreeSet::new();
    let mut asked: BTreeSet<(Option<ItemId>, &str, &str)> = BTreeSet::new();
    for item in items {
        let ItemBody::Approval(approval) = &item.body else {
            continue;
        };
        if approval.answer != AnswerState::Waiting {
            continue;
        }
        match approval.tool_call {
            Some(call) => {
                linked.insert(call);
            }
            None => {
                asked.insert((
                    item.parent,
                    approval.tool.as_str(),
                    approval.summary.as_str(),
                ));
            }
        }
    }
    if linked.is_empty() && asked.is_empty() {
        return BTreeSet::new();
    }
    items
        .iter()
        .filter(|item| match &item.body {
            ItemBody::ToolCall(call) if call.status == ToolCallStatus::Running => {
                linked.contains(&item.id)
                    || asked.contains(&(item.parent, call.tool.as_str(), call.summary.as_str()))
            }
            _ => false,
        })
        .map(|item| item.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::{
        Approval, ApprovalAnswer, ApprovalDecision, FileEdit, Plan, PlanAnswer, PlanDecision,
        ToolCall,
    };
    use serde_json::Value;

    use super::*;

    fn call(tool: &str, summary: &str, status: ToolCallStatus) -> ToolCall {
        ToolCall {
            tool: tool.to_owned(),
            summary: summary.to_owned(),
            input: Value::Null,
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
            input: Value::Null,
            edit: None,
            reason: None,
            answer,
            tool_call: None,
        })
    }

    fn item(id: u32, parent: Option<u32>, body: ItemBody) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: parent.map(ItemId::new),
            body,
        }
    }

    fn text(id: u32, markdown: &str) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::AssistantText {
                markdown: markdown.to_owned(),
            },
        }
    }

    fn plan(id: u32, answer: AnswerState<PlanAnswer>) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::Plan(Plan {
                pending: PendingId::new(format!("request-{id}")),
                markdown: "1. Rename the module".to_owned(),
                answer,
            }),
        }
    }

    fn texts(transcript: &Transcript) -> Vec<String> {
        transcript
            .items()
            .map(|item| match &item.body {
                ItemBody::AssistantText { markdown } => markdown.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_history_page_merges_with_live_items_by_id() {
        let mut transcript = Transcript::default();
        transcript.append(text(5, "live five"));
        transcript.append(text(6, "live six"));

        transcript.merge_page(HistoryPage {
            items: vec![text(3, "three"), text(4, "four"), text(5, "stale five")],
            older_items: OlderItems::Exist,
        });

        assert_eq!(
            texts(&transcript),
            ["three", "four", "live five", "live six"]
        );
        assert_eq!(
            transcript.history(),
            &HistoryState::Loaded { has_older: true }
        );
        assert_eq!(transcript.first_id(), Some(ItemId::new(3)));
    }

    #[test]
    fn trimming_keeps_the_newest_items_and_marks_that_older_ones_exist() {
        let mut transcript = Transcript::default();
        transcript.merge_page(HistoryPage {
            items: (1..=2_005).map(|id| text(id, "item")).collect(),
            older_items: OlderItems::None,
        });

        transcript.trim(KEPT_ITEMS_AWAY);

        assert_eq!(transcript.items().count(), 2_000);
        assert_eq!(transcript.first_id(), Some(ItemId::new(6)));
        assert_eq!(
            transcript.history(),
            &HistoryState::Loaded { has_older: true }
        );
    }

    #[test]
    fn trimming_waits_for_a_page_on_its_way() {
        let mut transcript = Transcript::default();
        for id in 1..=3 {
            transcript.append(text(id, "item"));
        }
        transcript.set_history(HistoryState::LoadingOlder);

        transcript.trim(2);
        assert_eq!(transcript.items().count(), 3);

        transcript.set_history(HistoryState::Loaded { has_older: false });
        transcript.trim(3);
        assert_eq!(
            transcript.history(),
            &HistoryState::Loaded { has_older: false }
        );
    }

    #[test]
    fn a_change_replaces_a_held_item_and_skips_an_unknown_one() {
        let mut transcript = Transcript::default();
        transcript.append(text(1, "before"));

        transcript.change(text(1, "after"));
        transcript.change(text(2, "never seen"));

        assert_eq!(texts(&transcript), ["after"]);
    }

    #[test]
    fn waiting_finds_the_newest_unanswered_card() {
        let mut transcript = Transcript::default();
        transcript.append(plan(1, AnswerState::Waiting));
        transcript.append(plan(
            2,
            AnswerState::Answered(PlanAnswer {
                decision: PlanDecision::KeepPlanning,
                message: None,
            }),
        ));
        transcript.append(text(3, "between"));

        let waiting = transcript.waiting().unwrap();

        assert_eq!(waiting.item, ItemId::new(1));
        assert_eq!(waiting.kind, WaitingKind::Plan);

        transcript.change(plan(1, AnswerState::Cancelled));
        assert_eq!(transcript.waiting(), None);
    }

    #[test]
    fn a_linked_approval_marks_only_its_own_call() {
        let running =
            || ItemBody::ToolCall(call("Bash", "touch same.txt", ToolCallStatus::Running));
        let linked = |call: u32, answer| {
            let ItemBody::Approval(approval) = approval("Bash", "touch same.txt", answer) else {
                panic!("the helper builds an approval");
            };
            ItemBody::Approval(Approval {
                tool_call: Some(ItemId::new(call)),
                ..approval
            })
        };
        let allowed = AnswerState::Answered(ApprovalAnswer {
            decision: ApprovalDecision::Allow,
            message: None,
        });
        let items = [
            item(1, None, running()),
            item(2, None, running()),
            item(3, None, linked(1, allowed)),
            item(4, None, linked(2, AnswerState::Waiting)),
        ];
        let refs: Vec<&TranscriptItem> = items.iter().collect();

        assert_eq!(awaiting_approval(&refs), BTreeSet::from([ItemId::new(2)]));
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

    #[test]
    fn the_history_awaits_its_first_page_until_it_comes_or_fails() {
        let awaits = |history: HistoryState| history.awaits_first_page();

        assert!(awaits(HistoryState::NotLoaded));
        assert!(awaits(HistoryState::Loading));
        assert!(!awaits(HistoryState::Failed("refused".to_owned())));
        assert!(!awaits(HistoryState::Loaded { has_older: true }));
        assert!(!awaits(HistoryState::LoadingOlder));
    }
}
