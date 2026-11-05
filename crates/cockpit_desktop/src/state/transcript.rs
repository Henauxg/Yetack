//! An agent's conversation as the app holds it: items merged by id from history pages and live
//! events.

use std::collections::BTreeMap;

use cockpit_protocol::{AnswerState, HistoryPage, ItemBody, ItemId, PendingId, TranscriptItem};

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

    pub fn append(&mut self, item: TranscriptItem) {
        self.items.insert(item.id, item);
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
            has_older: page.has_older,
        };
    }

    /// The newest approval, question or plan that waits for an answer.
    pub fn waiting(&self) -> Option<Waiting> {
        self.items.values().rev().find_map(|item| {
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
            Some(Waiting {
                item: item.id,
                pending: pending.clone(),
                kind,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{Plan, PlanAnswer, PlanDecision};

    use super::*;

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
            has_older: true,
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
}
