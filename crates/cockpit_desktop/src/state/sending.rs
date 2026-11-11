//! The messages the user sent that the daemon has not echoed yet. Each shows at once, dimmed,
//! where the daemon will most likely put it: the chat for an agent between turns, the waiting
//! list above the composer for a busy one. The daemon's echo replaces it: a message added to the
//! chat, or a new waiting message, with the same text and as many images. A refused message
//! leaves, and its draft comes back to the composer.
//!
//! - A waiting message is an echo when its id is one the app did not know as the message left,
//!   and no older message took it. The daemon never gives an id twice.
//! - A message added to the chat while a waiting one holds the same text is that one, read by
//!   Claude, and no echo. The daemon publishes a read message's leaving before any later echo,
//!   so a message the daemon added to the chat always finds the waiting list without its text.
//! - The daemon's answer to `agent.prompt` can come before its echo: only the echo ends a wait.
//!   A reconnect with a snapshot builds the agent again without its messages, and the transcript
//!   it then loads holds the ones the daemon took.

use std::collections::BTreeSet;

use cockpit_protocol::{
    AgentId, AgentStatus, Image, ItemBody, Prompt, PromptId, TranscriptItem, WaitingPrompt,
};

use super::accounts;
use super::app_state::{AgentEntry, AppState};

/// Names one message on its way, among all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SendingId(u64);

impl SendingId {
    pub fn get(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
impl SendingId {
    /// The id the app's first message on its way gets.
    pub(crate) fn first() -> Self {
        Self(0)
    }
}

/// Where a message on its way shows until its echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendingPlace {
    /// At the end of the chat: the message starts a turn.
    Chat,
    /// Above the composer, with the messages Claude has not read.
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendingPrompt {
    pub id: SendingId,
    pub text: String,
    pub images: Vec<Image>,
    pub place: SendingPlace,
    /// The waiting messages the app knew as it left, which cannot be its echo. `None` while the
    /// agent's details had not loaded: the first ones to load are those.
    known: Option<BTreeSet<PromptId>>,
}

impl SendingPrompt {
    fn holds(&self, text: &str, images: usize) -> bool {
        self.text == text && self.images.len() == images
    }

    fn holds_waiting(&self, waiting: &WaitingPrompt) -> bool {
        usize::try_from(waiting.image_count).is_ok_and(|images| self.holds(&waiting.text, images))
    }
}

/// One agent's messages on their way, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sending {
    prompts: Vec<SendingPrompt>,
    /// The waiting messages that ended a wait, until they leave: none ends a second one.
    claimed: BTreeSet<PromptId>,
}

impl Sending {
    pub fn in_place(&self, place: SendingPlace) -> impl Iterator<Item = &SendingPrompt> {
        self.prompts
            .iter()
            .filter(move |prompt| prompt.place == place)
    }
}

/// Shows `prompt` on its way, where the daemon most likely puts it: the daemon queues a message
/// while the agent's turn runs or while messages wait.
pub fn start(state: &mut AppState, agent: AgentId, prompt: &Prompt) -> Option<SendingId> {
    let entry = state.agents.get_mut(&agent)?;
    let id = SendingId(state.ui.next_sending);
    state.ui.next_sending = state.ui.next_sending.wrapping_add(1);
    let busy = matches!(
        accounts::turn_status(&entry.summary),
        AgentStatus::Working | AgentStatus::NeedsYou
    );
    let waiting = entry.waiting_prompts();
    let place = if busy || !waiting.is_empty() {
        SendingPlace::Waiting
    } else {
        SendingPlace::Chat
    };
    let known = entry.details.loaded().map(|details| {
        details
            .waiting_prompts
            .iter()
            .map(|waiting| waiting.id)
            .collect()
    });
    entry.sending.prompts.push(SendingPrompt {
        id,
        text: prompt.text.clone(),
        images: prompt.images.clone(),
        place,
        known,
    });
    Some(id)
}

/// The daemon refused the message.
pub fn failed(entry: &mut AgentEntry, id: SendingId) {
    entry.sending.prompts.retain(|prompt| prompt.id != id);
}

/// `item` joined the agent's chat: a message of the user's ends the oldest wait it matches, unless
/// a waiting message holds its text, which Claude has just read.
pub fn item_appended(entry: &mut AgentEntry, item: &TranscriptItem) {
    let ItemBody::UserMessage(message) = &item.body else {
        return;
    };
    if item.parent.is_some() {
        return;
    }
    let images = message.images.len();
    let read = entry.waiting_prompts().iter().any(|waiting| {
        waiting.text == message.text
            && usize::try_from(waiting.image_count).is_ok_and(|count| count == images)
    });
    if read {
        return;
    }
    let echoed = entry
        .sending
        .prompts
        .iter()
        .position(|prompt| prompt.holds(&message.text, images));
    if let Some(index) = echoed {
        entry.sending.prompts.remove(index);
    }
}

/// The agent's details loaded from a request: the messages sent before any loaded know its
/// waiting messages from then on.
pub fn details_loaded(entry: &mut AgentEntry) {
    let ids: BTreeSet<PromptId> = entry
        .waiting_prompts()
        .iter()
        .map(|waiting| waiting.id)
        .collect();
    for prompt in &mut entry.sending.prompts {
        if prompt.known.is_none() {
            prompt.known = Some(ids.clone());
        }
    }
}

/// The agent's waiting messages changed: a new one ends the oldest wait it matches.
pub fn waiting_changed(entry: &mut AgentEntry) {
    let waiting = entry.waiting_prompts().to_vec();
    let sending = &mut entry.sending;
    sending
        .claimed
        .retain(|id| waiting.iter().any(|prompt| prompt.id == *id));
    let claimed = &mut sending.claimed;
    sending.prompts.retain(|prompt| {
        let echo = waiting.iter().find(|candidate| {
            prompt.holds_waiting(candidate)
                && !claimed.contains(&candidate.id)
                && prompt
                    .known
                    .as_ref()
                    .is_none_or(|known| !known.contains(&candidate.id))
        });
        match echo {
            Some(echo) => {
                claimed.insert(echo.id);
                false
            }
            None => true,
        }
    });
}

#[cfg(test)]
mod tests {
    use cockpit_client::RequestError;
    use cockpit_protocol::{
        AgentDetails, AgentId, ImageMediaType, ItemId, Method, PromptDelivery, PromptId,
        UserMessage, WaitingState,
    };

    use super::*;
    use crate::machine::MachineId;
    use crate::state::app_state::{AppState, Loadable};
    use crate::state::drafts::Drafts;
    use crate::state::{actions, apply_response, state_with, summary};

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn idle_agent() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        state
    }

    fn entry(state: &mut AppState) -> &mut AgentEntry {
        state.agents.get_mut(&agent()).unwrap()
    }

    fn prompt(text: &str) -> Prompt {
        Prompt {
            text: text.to_owned(),
            images: Vec::new(),
        }
    }

    fn texts(state: &mut AppState, place: SendingPlace) -> Vec<String> {
        entry(state)
            .sending
            .in_place(place)
            .map(|prompt| prompt.text.clone())
            .collect()
    }

    fn user_message(id: u32, text: &str, images: Vec<Image>) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::UserMessage(UserMessage {
                text: text.to_owned(),
                images,
                sent_at: None,
            }),
        }
    }

    /// Details whose waiting messages are `(id, text)`.
    fn waiting(prompts: &[(u32, &str)]) -> AgentDetails {
        AgentDetails {
            waiting_prompts: prompts
                .iter()
                .map(|(id, text)| WaitingPrompt {
                    id: PromptId::new(*id),
                    text: (*text).to_owned(),
                    image_count: 0,
                    state: WaitingState::Sent,
                    sent_at: None,
                })
                .collect(),
            ..AgentDetails::default()
        }
    }

    #[test]
    fn an_idle_agents_message_shows_in_the_chat_until_its_echo() {
        let mut state = idle_agent();

        start(&mut state, agent(), &prompt("Run the tests")).unwrap();
        assert_eq!(texts(&mut state, SendingPlace::Chat), ["Run the tests"]);

        item_appended(
            entry(&mut state),
            &user_message(1, "Run the tests", Vec::new()),
        );
        assert!(texts(&mut state, SendingPlace::Chat).is_empty());
    }

    #[test]
    fn a_busy_agents_message_waits_until_one_more_waiting_message_holds_its_text() {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        entry(&mut state).details = Loadable::Loaded(waiting(&[(1, "yes")]));

        start(&mut state, agent(), &prompt("yes")).unwrap();
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["yes"]);

        // The older "yes" alone is no echo.
        waiting_changed(entry(&mut state));
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["yes"]);

        entry(&mut state).details = Loadable::Loaded(waiting(&[(1, "yes"), (2, "yes")]));
        waiting_changed(entry(&mut state));
        assert!(texts(&mut state, SendingPlace::Waiting).is_empty());
    }

    #[test]
    fn one_echo_ends_the_oldest_of_two_same_messages() {
        let mut state = idle_agent();
        let first = start(&mut state, agent(), &prompt("again")).unwrap();
        let second = start(&mut state, agent(), &prompt("again")).unwrap();

        item_appended(entry(&mut state), &user_message(1, "again", Vec::new()));

        let left: Vec<SendingId> = entry(&mut state)
            .sending
            .in_place(SendingPlace::Chat)
            .map(|prompt| prompt.id)
            .collect();
        assert_ne!(first, second);
        assert_eq!(left, [second]);
    }

    #[test]
    fn a_message_with_other_images_is_no_echo() {
        let mut state = idle_agent();
        start(&mut state, agent(), &prompt("Look at this")).unwrap();
        let image = Image {
            media_type: ImageMediaType::Png,
            data: "iVBORw0KGgo=".to_owned(),
        };

        item_appended(
            entry(&mut state),
            &user_message(1, "Look at this", vec![image]),
        );

        assert_eq!(texts(&mut state, SendingPlace::Chat), ["Look at this"]);
    }

    #[test]
    fn a_refused_message_leaves_and_its_draft_comes_back() {
        let mut state = idle_agent();
        let mut drafts = Drafts::default();
        drafts.draft_mut(agent()).text = "Deploy".to_owned();

        let sent = actions::send_prompt(
            &mut state,
            &mut drafts,
            agent(),
            None,
            PromptDelivery::Steer,
        );
        assert_eq!(texts(&mut state, SendingPlace::Chat), ["Deploy"]);
        let purpose = sent
            .into_iter()
            .next()
            .map(|outgoing| outgoing.purpose)
            .unwrap();
        apply_response(
            &mut state,
            &mut drafts,
            Method::AgentPrompt,
            purpose,
            Err(RequestError::NotConnected),
            MachineId::THIS,
        );

        assert!(texts(&mut state, SendingPlace::Chat).is_empty());
        assert_eq!(
            drafts.draft(agent()).map(|draft| draft.text.as_str()),
            Some("Deploy")
        );
    }

    fn working_agent(details: Loadable<AgentDetails>) -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        entry(&mut state).details = details;
        state
    }

    fn set_waiting(state: &mut AppState, prompts: &[(u32, &str)]) {
        entry(state).details = Loadable::Loaded(waiting(prompts));
        waiting_changed(entry(state));
    }

    #[test]
    fn a_message_sent_again_after_a_cancel_ends_on_its_own_echo() {
        let mut state = working_agent(Loadable::Loaded(waiting(&[(1, "yes")])));
        start(&mut state, agent(), &prompt("yes")).unwrap();

        // The daemon cancels the older "yes" first, then queues the new one.
        set_waiting(&mut state, &[]);
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["yes"]);
        set_waiting(&mut state, &[(2, "yes")]);
        assert!(texts(&mut state, SendingPlace::Waiting).is_empty());
    }

    #[test]
    fn an_older_same_message_read_by_claude_is_no_echo() {
        let mut state = working_agent(Loadable::Loaded(waiting(&[(1, "yes")])));
        start(&mut state, agent(), &prompt("yes")).unwrap();

        // Claude reads the older "yes": it joins the chat while it still waits in the list.
        item_appended(entry(&mut state), &user_message(7, "yes", Vec::new()));
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["yes"]);

        set_waiting(&mut state, &[]);
        set_waiting(&mut state, &[(2, "yes")]);
        assert!(texts(&mut state, SendingPlace::Waiting).is_empty());
    }

    #[test]
    fn details_still_loading_at_the_send_count_as_known_once_loaded() {
        let mut state = working_agent(Loadable::Loading);
        start(&mut state, agent(), &prompt("yes")).unwrap();

        // The details asked for before the send hold an older "yes".
        entry(&mut state).details = Loadable::Loaded(waiting(&[(1, "yes")]));
        details_loaded(entry(&mut state));
        waiting_changed(entry(&mut state));
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["yes"]);

        set_waiting(&mut state, &[(1, "yes"), (2, "yes")]);
        assert!(texts(&mut state, SendingPlace::Waiting).is_empty());
    }

    #[test]
    fn one_waiting_echo_ends_one_wait_even_as_the_list_changes_again() {
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        start(&mut state, agent(), &prompt("again")).unwrap();
        start(&mut state, agent(), &prompt("again")).unwrap();

        set_waiting(&mut state, &[(5, "again")]);
        set_waiting(&mut state, &[(5, "again")]);
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["again"]);

        set_waiting(&mut state, &[(5, "again"), (6, "again")]);
        assert!(texts(&mut state, SendingPlace::Waiting).is_empty());
    }

    #[test]
    fn a_subagents_prompt_is_no_echo() {
        let mut state = idle_agent();
        start(&mut state, agent(), &prompt("Review the parser")).unwrap();
        let mut nested = user_message(3, "Review the parser", Vec::new());
        nested.parent = Some(ItemId::new(2));

        item_appended(entry(&mut state), &nested);

        assert_eq!(texts(&mut state, SendingPlace::Chat), ["Review the parser"]);
    }

    #[test]
    fn a_details_answer_older_than_an_event_leaves_the_events_details() {
        let mut state = working_agent(Loadable::Loading);
        // Claude read the steered "yes" meanwhile: the event says the list is empty.
        entry(&mut state).details = Loadable::Loaded(waiting(&[]));
        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentDetails,
            crate::state::Purpose::Details { agent: agent() },
            Ok(cockpit_protocol::ResponseBody::Details(waiting(&[(
                1, "yes",
            )]))),
            MachineId::THIS,
        );
        assert!(entry(&mut state).waiting_prompts().is_empty());

        // The agent between turns again: the user's "yes" joins the chat.
        entry(&mut state).summary.status = AgentStatus::Idle;
        start(&mut state, agent(), &prompt("yes")).unwrap();
        item_appended(entry(&mut state), &user_message(9, "yes", Vec::new()));

        assert!(texts(&mut state, SendingPlace::Chat).is_empty());
    }
}
