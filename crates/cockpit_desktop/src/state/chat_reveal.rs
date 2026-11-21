//! Reveals of a chat entry that may be held above the loaded messages, as a subagent call older
//! than them whose work the loaded messages hold. Such an entry shows at the top of the chat, out
//! of its place, so its reveal waits: the older pages load until the entry is among the messages,
//! then the chat scrolls to it there.

use cockpit_protocol::{AgentId, ItemId};

use super::actions;
use super::app_state::AppState;
use super::requests::Outgoing;
use super::transcript::HistoryState;
use super::ui_state::{RevealPlace, WaitingReveal};

/// Scrolls `agent`'s chat to the entry of the top-level `item`, at `place`, now or once the older
/// pages put it in its place.
pub fn reveal(state: &mut AppState, agent: AgentId, item: ItemId, place: RevealPlace) {
    if held_above(state, agent, item) {
        state.ui.waiting_reveal = Some(WaitingReveal { agent, item, place });
    } else {
        state.ui.waiting_reveal = None;
        state.ui.reveal_in_chat(agent, item, place);
    }
}

/// Whether the entry of `item` stands above `agent`'s loaded messages.
fn held_above(state: &AppState, agent: AgentId, item: ItemId) -> bool {
    state
        .agents
        .get(&agent)
        .and_then(|entry| entry.transcript.loaded_from())
        .is_some_and(|from| item < from)
}

/// The next older page the waiting reveal needs, while its agent is reachable. Once its entry is
/// in place, or no page can bring it there, the reveal runs. It is dropped once another agent
/// shows.
pub(super) fn requests(state: &mut AppState) -> Vec<Outgoing> {
    let Some(waiting) = state.ui.waiting_reveal else {
        return Vec::new();
    };
    let history: Option<HistoryState> = state
        .agents
        .get(&waiting.agent)
        .map(|entry| entry.transcript.history().clone());
    let Some(history) = history.filter(|_| state.ui.selected == Some(waiting.agent)) else {
        state.ui.waiting_reveal = None;
        return Vec::new();
    };
    let held_above = held_above(state, waiting.agent, waiting.item);
    match history {
        HistoryState::NotLoaded | HistoryState::Loading | HistoryState::LoadingOlder => Vec::new(),
        HistoryState::Loaded { has_older: true } if held_above => {
            if state.reaches_agent(waiting.agent) {
                actions::load_older(state, waiting.agent)
            } else {
                Vec::new()
            }
        }
        HistoryState::Loaded { .. } | HistoryState::OlderFailed(_) | HistoryState::Failed(_) => {
            state.ui.waiting_reveal = None;
            state
                .ui
                .reveal_in_chat(waiting.agent, waiting.item, waiting.place);
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ops::RangeInclusive;

    use cockpit_protocol::{
        AgentStatus, HistoryPage, ItemBody, NoticeKind, OlderItems, ToolCall, ToolCallStatus,
        TranscriptItem,
    };
    use serde_json::Value;

    use super::*;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::app_state::{AgentEntry, ConnectionState};
    use crate::state::requests::{Purpose, needed_requests};

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn notice(id: u32) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::Notice {
                text: format!("item {id}"),
                detail: None,
                kind: NoticeKind::Plain,
            },
        }
    }

    fn agent_call(id: u32) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::Subagent(ToolCall {
                tool: "Agent".to_owned(),
                summary: "Replace a crate".to_owned(),
                input: Value::Null,
                status: ToolCallStatus::Done,
                output: None,
                exit_code: None,
                edit: None,
            }),
        }
    }

    fn under(parent: u32, id: u32) -> TranscriptItem {
        TranscriptItem {
            parent: Some(ItemId::new(parent)),
            ..notice(id)
        }
    }

    fn page(ids: RangeInclusive<u32>, older: OlderItems) -> HistoryPage {
        HistoryPage {
            items: ids.map(notice).collect(),
            older_items: older,
            ancestors: Vec::new(),
        }
    }

    /// The latest page holds the work of the subagent of item 10, whose call came with it.
    fn opened_chat() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.this.connection = ConnectionState::Connected;
        state.ui.selected = Some(agent());
        needed_requests(&mut state);
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.transcript.merge_page(HistoryPage {
                items: (30..=40).map(|id| under(10, id)).collect(),
                older_items: OlderItems::Exist,
                ancestors: vec![agent_call(10)],
            });
        }
        state
    }

    fn older_pages_asked(requests: &[Outgoing]) -> usize {
        requests
            .iter()
            .filter(|outgoing| matches!(outgoing.purpose, Purpose::History { older: true, .. }))
            .count()
    }

    #[test]
    fn a_reveal_of_a_call_above_the_loaded_messages_loads_older_pages_until_it_is_in_place() {
        let mut state = opened_chat();

        reveal(&mut state, agent(), ItemId::new(10), RevealPlace::Middle);
        let first = needed_requests(&mut state);
        let while_loading = needed_requests(&mut state);
        let before_any_page = state.ui.chat_reveal;
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry
                .transcript
                .merge_page(page(20..=29, OlderItems::Exist));
        }
        let second = needed_requests(&mut state);
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.transcript.merge_page(page(5..=19, OlderItems::Exist));
        }
        let done = needed_requests(&mut state);

        assert_eq!(older_pages_asked(&first), 1);
        assert_eq!(older_pages_asked(&while_loading), 0);
        assert_eq!(before_any_page, None, "the chat waits to scroll");
        assert_eq!(older_pages_asked(&second), 1, "one page did not reach it");
        assert_eq!(older_pages_asked(&done), 0);
        let revealed = state.ui.chat_reveal.map(|reveal| reveal.item);
        assert_eq!(revealed, Some(ItemId::new(10)));
        assert_eq!(state.ui.waiting_reveal, None);
    }

    #[test]
    fn a_reveal_in_place_scrolls_at_once_and_a_failed_page_scrolls_where_the_call_is() {
        let mut state = opened_chat();

        reveal(&mut state, agent(), ItemId::new(35), RevealPlace::Middle);
        let at_once = state.ui.chat_reveal.map(|reveal| reveal.item);
        reveal(&mut state, agent(), ItemId::new(10), RevealPlace::Middle);
        needed_requests(&mut state);
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry
                .transcript
                .set_history(HistoryState::OlderFailed("busy".to_owned()));
        }
        let after_failure = needed_requests(&mut state);

        assert_eq!(at_once, Some(ItemId::new(35)));
        assert_eq!(older_pages_asked(&after_failure), 0, "no retry on its own");
        assert_eq!(
            state.ui.chat_reveal.map(|reveal| reveal.item),
            Some(ItemId::new(10))
        );
    }

    #[test]
    fn another_agent_drops_the_waiting_reveal() {
        let mut state = opened_chat();
        state.agents.insert(
            AgentId::new(2),
            AgentEntry::new(summary(2, AgentStatus::Idle)),
        );

        reveal(&mut state, agent(), ItemId::new(10), RevealPlace::Middle);
        needed_requests(&mut state);
        actions::select_agent(&mut state, AgentId::new(2));
        needed_requests(&mut state);

        assert_eq!(state.ui.waiting_reveal, None);
        assert_eq!(state.ui.chat_reveal, None);
    }

    #[test]
    fn another_agent_drops_the_waiting_reveal_while_disconnected() {
        let mut state = opened_chat();
        state.agents.insert(
            AgentId::new(2),
            AgentEntry::new(summary(2, AgentStatus::Idle)),
        );
        reveal(&mut state, agent(), ItemId::new(10), RevealPlace::Middle);
        needed_requests(&mut state);
        state.machines.this.connection = ConnectionState::Connecting { attempt: 1 };

        actions::select_agent(&mut state, AgentId::new(2));
        needed_requests(&mut state);

        assert_eq!(state.ui.waiting_reveal, None);
    }

    #[test]
    fn a_later_reveal_drops_the_waiting_one() {
        let mut state = opened_chat();
        reveal(&mut state, agent(), ItemId::new(10), RevealPlace::Middle);
        needed_requests(&mut state);

        state
            .ui
            .reveal_in_chat(agent(), ItemId::new(35), RevealPlace::Top);
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.transcript.merge_page(page(5..=29, OlderItems::Exist));
        }
        needed_requests(&mut state);

        let revealed = state.ui.chat_reveal.map(|reveal| reveal.item);
        assert_eq!(revealed, Some(ItemId::new(35)));
    }

    #[test]
    fn a_jump_to_the_bottom_drops_the_waiting_reveal() {
        let mut state = opened_chat();
        reveal(&mut state, agent(), ItemId::new(10), RevealPlace::Middle);
        needed_requests(&mut state);

        state.ui.jump_chat();
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.transcript.merge_page(page(5..=29, OlderItems::Exist));
        }
        needed_requests(&mut state);

        assert_eq!(state.ui.chat_reveal, None);
    }
}
