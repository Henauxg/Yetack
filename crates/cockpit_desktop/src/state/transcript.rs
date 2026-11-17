//! An agent's conversation as the app holds it: items merged by id from history pages and live
//! events.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

use cockpit_protocol::{
    AnswerState, HistoryPage, ItemBody, ItemId, OlderItems, PendingId, ToolCall, ToolCallStatus,
    TranscriptItem, UserMessage,
};

/// The most items the app keeps of an agent you are not looking at: three history pages. Scrolling
/// up loads the older ones again. Each item the app holds costs the chat a little at each render,
/// and a morning of work gives an agent thousands.
pub const KEPT_ITEMS_AWAY: usize = 300;
/// The most items the app keeps of an agent left with its chat scrolled up: the chat goes back to
/// the reader's place, which can be far above the latest items.
pub const KEPT_ITEMS_SCROLLED_UP: usize = 2_000;

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
    /// The page before the first item held did not load, for this reason. Older items exist.
    OlderFailed(String),
    Failed(String),
}

impl HistoryState {
    /// Whether the first page of the history is still to come. A failed one is not.
    pub fn awaits_first_page(&self) -> bool {
        matches!(self, Self::NotLoaded | Self::Loading)
    }

    /// Whether older items exist and no page of them is on its way.
    pub fn can_load_older(&self) -> bool {
        matches!(
            self,
            Self::Loaded { has_older: true } | Self::OlderFailed(_)
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    items: BTreeMap<ItemId, TranscriptItem>,
    history: HistoryState,
    revision: Revision,
}

/// Names the state of a transcript's items, so that what the app works out from them is kept
/// until they change. The numbers come from one count for every transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Revision(u64);

/// The last revision any transcript took.
static REVISIONS: AtomicU64 = AtomicU64::new(0);

/// Two transcripts with the same items and history are equal, whatever their revisions.
impl PartialEq for Transcript {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items && self.history == other.history
    }
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

    pub fn revision(&self) -> Revision {
        self.revision
    }

    fn changed(&mut self) {
        self.revision = Revision(REVISIONS.fetch_add(1, Ordering::Relaxed).wrapping_add(1));
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
        self.changed();
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
        if dropped {
            self.changed();
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
            self.changed();
        }
    }

    /// Adds a history page. An item already held came from a live event, which is at least as
    /// recent as the page, so it stays.
    pub fn merge_page(&mut self, page: HistoryPage) {
        for item in page.items {
            self.items.entry(item.id).or_insert(item);
        }
        self.changed();
        self.history = HistoryState::Loaded {
            has_older: page.older_items == OlderItems::Exist,
        };
    }

    /// The newest approval, question or plan that waits for an answer.
    pub fn waiting(&self) -> Option<Waiting> {
        self.items.values().rev().find_map(Waiting::new)
    }

    /// The newest approval, question or plan that waits for an answer and holds the agent's next
    /// step: the agent's own, or that of a subagent whose call is one of `running`.
    pub fn waiting_in_step(&self, running: &[ItemId]) -> Option<Waiting> {
        self.items
            .values()
            .rev()
            .filter(|item| item.parent.is_none_or(|parent| running.contains(&parent)))
            .find_map(Waiting::new)
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

    /// The user message that the items from `top` onward answer: the latest top-level user
    /// message before `top`. `None` when `top` is a user message itself or is not held.
    pub fn message_before(&self, top: ItemId) -> Option<(ItemId, &UserMessage)> {
        let top_message = user_message(self.items.get(&top)?);
        if top_message.is_some() {
            return None;
        }
        self.items
            .range(..top)
            .rev()
            .find_map(|(_, item)| user_message(item).map(|message| (item.id, message)))
    }

    /// The agent's own calls that run in its current step, the latest first. Claude reads a
    /// message sent meanwhile once they end. A subagent's calls run under its own call.
    pub fn running_calls(&self) -> impl Iterator<Item = (ItemId, &ToolCall)> {
        self.items
            .values()
            .rev()
            .take_while(|item| {
                !matches!(item.body, ItemBody::TurnEnd(_)) && user_message(item).is_none()
            })
            .filter(|item| item.parent.is_none())
            .filter_map(|item| match &item.body {
                ItemBody::ToolCall(call) | ItemBody::Subagent(call)
                    if call.status == ToolCallStatus::Running =>
                {
                    Some((item.id, call))
                }
                _ => None,
            })
    }

    /// The messages the user sent to the agent, oldest first.
    pub fn sent_messages(&self) -> impl Iterator<Item = (ItemId, &UserMessage)> {
        self.items
            .values()
            .filter_map(|item| user_message(item).map(|message| (item.id, message)))
    }

    /// The user message that the items from `top` onward answer: `top` itself when it is one,
    /// else the latest one before it.
    pub fn message_at(&self, top: ItemId) -> Option<ItemId> {
        self.items
            .range(..=top)
            .rev()
            .find_map(|(id, item)| user_message(item).map(|_| *id))
    }

    /// The message the reader is on, from the first and the last entry in view. Scrolled up, it is
    /// the one whose answer shows at the top, where a click in the Messages block brings a
    /// message. At the bottom, it is the latest message on screen.
    pub fn message_read(&self, top: ItemId, bottom: ItemId, scrolled_up: bool) -> Option<ItemId> {
        self.message_at(if scrolled_up { top } else { bottom })
    }
}

/// A message the user sent to the agent, not one of a subagent's.
fn user_message(item: &TranscriptItem) -> Option<&UserMessage> {
    match &item.body {
        ItemBody::UserMessage(message) if item.parent.is_none() => Some(message),
        _ => None,
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
        Approval, ApprovalAnswer, ApprovalDecision, EditTexts, FileEdit, Plan, PlanAnswer,
        PlanDecision, ToolCall,
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
                texts: EditTexts::Kept,
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
            items: (1..=305).map(|id| text(id, "item")).collect(),
            older_items: OlderItems::None,
        });

        transcript.trim(KEPT_ITEMS_AWAY);

        assert_eq!(transcript.items().count(), 300);
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
    fn the_revision_moves_with_each_change_of_the_items_and_only_then() {
        let mut transcript = Transcript::default();
        let mut revisions = vec![transcript.revision()];
        let mut moved = |transcript: &Transcript| {
            let moved = revisions.last() != Some(&transcript.revision());
            revisions.push(transcript.revision());
            moved
        };

        transcript.append(text(1, "first"));
        assert!(moved(&transcript), "append");
        transcript.change(text(1, "changed"));
        assert!(moved(&transcript), "a change of a held item");
        transcript.change(text(9, "never seen"));
        assert!(!moved(&transcript), "a change of an unknown item");
        transcript.merge_page(HistoryPage {
            items: vec![text(0, "older")],
            older_items: OlderItems::None,
        });
        assert!(moved(&transcript), "a page");
        transcript.trim(5);
        assert!(!moved(&transcript), "a trim that drops nothing");
        transcript.trim(1);
        assert!(moved(&transcript), "a trim that drops items");
    }

    /// A reconnect builds the agent's transcript again: its revisions never meet the ones of the
    /// transcript it replaces, which the app may have kept work for.
    #[test]
    fn a_new_transcript_takes_none_of_an_older_ones_revisions() {
        let mut older = Transcript::default();
        older.append(text(1, "item"));
        let mut newer = Transcript::default();
        newer.append(text(1, "item"));

        assert_ne!(newer.revision(), older.revision());
        assert_eq!(newer, older, "the revision is no part of the content");
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
    fn the_message_before_an_item_is_the_latest_one_the_user_sent() {
        let sent = |id: u32, parent: Option<u32>, text: &str| {
            item(
                id,
                parent,
                ItemBody::UserMessage(UserMessage {
                    text: text.to_owned(),
                    images: Vec::new(),
                    sent_at: None,
                }),
            )
        };
        let mut transcript = Transcript::default();
        transcript.append(text(1, "an answer to an older message"));
        transcript.append(sent(2, None, "Run the tests"));
        transcript.append(text(3, "Running them"));
        transcript.append(sent(4, Some(3), "A subagent's prompt"));
        transcript.append(text(5, "All pass"));
        transcript.append(sent(6, None, "Now the lints"));
        let before = |top: u32| {
            transcript
                .message_before(ItemId::new(top))
                .map(|(id, message)| (id.get(), message.text.clone()))
        };

        assert_eq!(before(5), Some((2, "Run the tests".to_owned())));
        assert_eq!(before(3), Some((2, "Run the tests".to_owned())));
        assert_eq!(before(1), None);
        assert_eq!(before(6), None);
        assert_eq!(before(9), None);

        let sent_ids: Vec<u32> = transcript.sent_messages().map(|(id, _)| id.get()).collect();
        let at = |top: u32| transcript.message_at(ItemId::new(top)).map(ItemId::get);
        assert_eq!(sent_ids, [2, 6]);
        assert_eq!(at(5), Some(2));
        assert_eq!(at(2), Some(2));
        assert_eq!(at(6), Some(6));
        assert_eq!(at(1), None);
        assert_eq!(at(9), Some(6));
    }

    /// Message 2 at the top of the view, its answer and message 6 below it.
    #[test]
    fn scrolled_up_the_message_read_is_the_one_at_the_top() {
        let sent = |id: u32, text: &str| {
            item(
                id,
                None,
                ItemBody::UserMessage(UserMessage {
                    text: text.to_owned(),
                    images: Vec::new(),
                    sent_at: None,
                }),
            )
        };
        let mut transcript = Transcript::default();
        transcript.append(sent(2, "Run the tests"));
        transcript.append(text(3, "All pass"));
        transcript.append(sent(6, "Now the lints"));
        transcript.append(text(7, "Clean"));
        let read = |scrolled_up| {
            transcript
                .message_read(ItemId::new(2), ItemId::new(7), scrolled_up)
                .map(ItemId::get)
        };

        assert_eq!(read(true), Some(2));
        assert_eq!(read(false), Some(6));
    }

    #[test]
    fn the_running_calls_are_the_agents_own_since_the_last_message() {
        let running = |tool: &str| ItemBody::ToolCall(call(tool, "", ToolCallStatus::Running));
        let mut transcript = Transcript::default();
        transcript.append(item(1, None, running("Bash")));
        transcript.append(item(
            2,
            None,
            ItemBody::UserMessage(UserMessage {
                text: "Go on".to_owned(),
                images: Vec::new(),
                sent_at: None,
            }),
        ));
        transcript.append(item(3, None, running("Bash")));
        transcript.append(item(
            4,
            None,
            ItemBody::ToolCall(call("Read", "a.rs", ToolCallStatus::Done)),
        ));
        transcript.append(item(
            5,
            None,
            ItemBody::Subagent(call("Agent", "Survey", ToolCallStatus::Running)),
        ));
        transcript.append(item(6, Some(5), running("Grep")));
        transcript.append(text(7, "Waiting for the build"));

        let ids: Vec<u32> = transcript.running_calls().map(|(id, _)| id.get()).collect();

        assert_eq!(ids, [5, 3]);
    }

    #[test]
    fn only_a_card_of_the_step_holds_it() {
        let subagent = |status| ItemBody::Subagent(call("Agent", "Survey", status));
        let waiting = || approval("Bash", "rm -r build", AnswerState::Waiting);
        let mut transcript = Transcript::default();
        transcript.append(item(1, None, subagent(ToolCallStatus::Done)));
        transcript.append(item(2, Some(1), waiting()));
        transcript.append(item(3, None, subagent(ToolCallStatus::Running)));
        let held = |transcript: &Transcript, running: &[u32]| {
            let running: Vec<ItemId> = running.iter().copied().map(ItemId::new).collect();
            transcript
                .waiting_in_step(&running)
                .map(|waiting| waiting.item.get())
        };

        assert_eq!(held(&transcript, &[3]), None);

        transcript.append(item(4, Some(3), waiting()));
        assert_eq!(held(&transcript, &[3]), Some(4));
        assert_eq!(held(&transcript, &[]), None);

        transcript.append(item(5, None, waiting()));
        assert_eq!(held(&transcript, &[]), Some(5));
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
