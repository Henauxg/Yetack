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
//! - A message steered to a busy agent can wait in the app before it leaves, as long as the
//!   preferences' `SteerHold` says, so that Cancel can still stop it: once sent, Claude Code holds
//!   it, and no request takes it back. A reconnect's snapshot keeps the held messages.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use cockpit_protocol::{
    AgentId, AgentStatus, COMMAND_PREFIX, Image, ItemBody, Prompt, PromptDelivery, PromptId,
    PromptParams, Request, Timestamp, TranscriptItem, WaitingPrompt,
};

use super::accounts;
use super::app_state::{AgentEntry, AppState};
use super::drafts::{Draft, Drafts};
use super::requests::{Outgoing, Purpose};
use crate::machine::MachineId;

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
    /// When the user sent it, which its bubble shows until the echo brings the daemon's time.
    pub sent_at: Option<Timestamp>,
    /// The waiting messages the app knew as it left, which cannot be its echo. `None` while the
    /// agent's details had not loaded: the first ones to load are those.
    known: Option<BTreeSet<PromptId>>,
    /// Set while the app holds the message: it has not left yet.
    held: Option<Held>,
}

/// A message the app holds before it sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Held {
    until: Instant,
    delivery: PromptDelivery,
    /// What Cancel, or a refusal, puts back in the composer.
    draft: Draft,
}

impl SendingPrompt {
    /// When the app sends a held message, `None` once it left.
    pub fn held_until(&self) -> Option<Instant> {
        self.held.as_ref().map(|held| held.until)
    }

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
    /// The images of the waiting messages this window sent, until they leave the list. The
    /// daemon sends a waiting message's image count alone.
    images: BTreeMap<PromptId, Vec<Arc<Image>>>,
}

impl Sending {
    pub fn in_place(&self, place: SendingPlace) -> impl Iterator<Item = &SendingPrompt> {
        self.prompts
            .iter()
            .filter(move |prompt| prompt.place == place)
    }

    /// The images of a waiting message, when this window sent it.
    pub fn waiting_images(&self, id: PromptId) -> Option<&[Arc<Image>]> {
        self.images.get(&id).map(Vec::as_slice)
    }
}

/// Shows `prompt` on its way, where the daemon most likely puts it: the daemon queues a message
/// while the agent's turn runs or while messages wait. A message steered there waits `hold` in
/// the app first: then `None` comes back, and `release_due` sends it.
pub fn start(
    state: &mut AppState,
    agent: AgentId,
    prompt: &Prompt,
    delivery: PromptDelivery,
    draft: &Draft,
    hold: Duration,
    now: Instant,
) -> Option<SendingId> {
    let entry = state.agents.get_mut(&agent)?;
    let id = SendingId(state.ui.next_sending);
    state.ui.next_sending = state.ui.next_sending.wrapping_add(1);
    let busy = matches!(
        accounts::turn_status(&entry.summary),
        AgentStatus::Working | AgentStatus::NeedsYou
    );
    let holding = entry
        .sending
        .prompts
        .iter()
        .any(|prompt| prompt.held.is_some());
    let waiting = entry.waiting_prompts();
    let place = if busy || holding || !waiting.is_empty() {
        SendingPlace::Waiting
    } else {
        SendingPlace::Chat
    };
    let steered = delivery == PromptDelivery::Steer && !prompt.text.starts_with(COMMAND_PREFIX);
    // A message sent while another waits in the app leaves after it, never before.
    let behind = entry
        .sending
        .prompts
        .iter()
        .filter_map(SendingPrompt::held_until)
        .max();
    // Never before an older held message, even after the hold was shortened.
    let until = if place == SendingPlace::Waiting && steered && !hold.is_zero() {
        behind.max(Some(now + hold))
    } else {
        behind
    };
    let held = until.map(|until| Held {
        until,
        delivery,
        draft: draft.clone(),
    });
    let holds = held.is_some();
    entry.sending.prompts.push(SendingPrompt {
        id,
        text: prompt.text.clone(),
        images: prompt.images.clone(),
        place,
        sent_at: Timestamp::from_system_time(SystemTime::now()),
        known: known_waiting(entry),
        held,
    });
    (!holds).then_some(id)
}

/// The waiting messages the app knows, which a message leaving now cannot take for its echo.
fn known_waiting(entry: &AgentEntry) -> Option<BTreeSet<PromptId>> {
    entry.details.loaded().map(|details| {
        details
            .waiting_prompts
            .iter()
            .map(|waiting| waiting.id)
            .collect()
    })
}

/// The next moment a held message is due, among the agents whose machine answers: the others'
/// wait for the reconnect.
pub fn next_release(state: &AppState) -> Option<Instant> {
    state
        .agents
        .iter()
        .filter(|(agent, _)| state.reaches_agent(**agent))
        .map(|(_, entry)| entry)
        .flat_map(|entry| entry.sending.prompts.iter())
        .filter_map(SendingPrompt::held_until)
        .min()
}

/// Sends the held messages due at `now`.
pub fn release_due(state: &mut AppState, now: Instant) -> Vec<Outgoing> {
    // A message for a machine that does not answer would be refused: it waits for the reconnect,
    // whose snapshot keeps it.
    let unreached: BTreeSet<AgentId> = state
        .agents
        .keys()
        .copied()
        .filter(|agent| !state.reaches_agent(*agent))
        .collect();
    let mut requests = Vec::new();
    for (agent, entry) in &mut state.agents {
        if unreached.contains(agent) {
            continue;
        }
        let due: Vec<SendingId> = entry
            .sending
            .prompts
            .iter()
            .filter(|prompt| prompt.held_until().is_some_and(|until| until <= now))
            .map(|prompt| prompt.id)
            .collect();
        for id in due {
            requests.extend(release(entry, *agent, id));
        }
    }
    requests
}

/// Sends a held message at once, from its "Send now" button, with the held messages sent before
/// it: they reach the agent in the order the user sent them.
pub fn send_now(state: &mut AppState, agent: AgentId, id: SendingId) -> Vec<Outgoing> {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return Vec::new();
    };
    let Some(at) = entry
        .sending
        .prompts
        .iter()
        .position(|prompt| prompt.id == id)
    else {
        return Vec::new();
    };
    let first: Vec<SendingId> = entry
        .sending
        .prompts
        .iter()
        .take(at + 1)
        .filter(|prompt| prompt.held.is_some())
        .map(|prompt| prompt.id)
        .collect();
    first
        .into_iter()
        .filter_map(|earlier| release(entry, agent, earlier))
        .collect()
}

fn release(entry: &mut AgentEntry, agent: AgentId, id: SendingId) -> Option<Outgoing> {
    let known = known_waiting(entry);
    let prompt = entry
        .sending
        .prompts
        .iter_mut()
        .find(|prompt| prompt.id == id)?;
    let held = prompt.held.take()?;
    // It leaves now: the waiting messages known now cannot be its echo. Ids never come back, so
    // those known at the send still hold when no details are loaded now.
    if known.is_some() {
        prompt.known = known;
    }
    Some(Outgoing {
        request: Request::AgentPrompt(PromptParams {
            agent,
            prompt: Prompt {
                text: prompt.text.clone(),
                images: prompt.images.clone(),
            },
            delivery: held.delivery,
        }),
        purpose: Purpose::Prompt {
            agent,
            draft: held.draft,
            sending: id,
        },
        to: MachineId::THIS,
    })
}

/// Cancel on a held message: it does not leave, and its text comes back to the composer, after
/// the text there, if any.
pub fn cancel(state: &mut AppState, drafts: &mut Drafts, agent: AgentId, id: SendingId) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let Some(index) = entry
        .sending
        .prompts
        .iter()
        .position(|prompt| prompt.id == id && prompt.held.is_some())
    else {
        return;
    };
    let removed = entry.sending.prompts.remove(index);
    let Some(held) = removed.held else {
        return;
    };
    restore_draft(drafts, agent, held.draft);
}

/// Puts a message that did not leave back in the composer, after the text typed there meanwhile.
pub fn restore_draft(drafts: &mut Drafts, agent: AgentId, draft: Draft) {
    let current = drafts.draft_mut(agent);
    if current.is_empty() {
        *current = draft;
    } else {
        if !current.text.trim().is_empty() && !draft.text.is_empty() {
            current.text.push_str("\n\n");
        }
        current.text.push_str(&draft.text);
        current.images.extend(draft.images);
        current.quotes.extend(draft.quotes);
    }
    drafts.draft_written(agent);
}

/// The held messages of an agent a snapshot builds again: they have not left, so no transcript
/// holds them.
pub fn keep_held(before: &Sending) -> Sending {
    Sending {
        prompts: before
            .prompts
            .iter()
            .filter(|prompt| prompt.held.is_some())
            .cloned()
            .collect(),
        claimed: BTreeSet::new(),
        images: BTreeMap::new(),
    }
}

/// The daemon refused the message.
pub fn failed(entry: &mut AgentEntry, id: SendingId) {
    entry.sending.prompts.retain(|prompt| prompt.id != id);
}

/// `item` joined the agent's chat: a message of the user's ends the oldest wait it matches among
/// the messages that left, unless a waiting message holds its text, which Claude has just read.
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
        .position(|prompt| prompt.held.is_none() && prompt.holds(&message.text, images));
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
    let still_waits = |id: &PromptId| waiting.iter().any(|prompt| prompt.id == *id);
    sending.claimed.retain(still_waits);
    sending.images.retain(|id, _| still_waits(id));
    let claimed = &mut sending.claimed;
    let images = &mut sending.images;
    // An echo names its message by text and image count alone: two messages that hold the same
    // ones could swap images, so neither keeps them.
    let alike = |prompt: &SendingPrompt, prompts: &[SendingPrompt]| {
        prompts
            .iter()
            .filter(|other| other.holds(&prompt.text, prompt.images.len()))
            .count()
            > 1
    };
    let shared: Vec<bool> = sending
        .prompts
        .iter()
        .map(|prompt| alike(prompt, &sending.prompts))
        .collect();
    let mut shared = shared.into_iter();
    sending.prompts.retain(|prompt| {
        let shared = shared.next().unwrap_or(true);
        // A held message has not left: nothing is its echo yet.
        if prompt.held.is_some() {
            return true;
        }
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
                if !prompt.images.is_empty() && !shared {
                    let kept: Vec<Arc<Image>> =
                        prompt.images.iter().cloned().map(Arc::new).collect();
                    images.insert(echo.id, kept);
                }
                false
            }
            None => true,
        }
    });
}

#[cfg(test)]
mod tests {
    use cockpit_client::{RequestError, SessionUpdate};
    use cockpit_protocol::{
        AgentDetails, AgentId, DetailsChange, Event, EventRecord, ImageMediaType, ItemId, Method,
        PromptDelivery, PromptId, ResponseBody, Seq, UserMessage, WaitingState,
    };

    use super::*;
    use crate::machine::MachineId;

    /// The hold of the tests that hold a message.
    const HOLD: Duration = Duration::from_secs(3);
    use crate::state::app_state::{AppState, Loadable};
    use crate::state::drafts::Drafts;
    use crate::state::{actions, apply_response, apply_update, state_with, summary};

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

    /// Sends `text` for after the turn, which the app never holds.
    fn queued(state: &mut AppState, text: &str) -> SendingId {
        start(
            state,
            agent(),
            &prompt(text),
            PromptDelivery::Queue,
            &Draft::default(),
            HOLD,
            Instant::now(),
        )
        .unwrap()
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
    fn a_message_on_its_way_keeps_the_time_it_was_sent() {
        let mut state = idle_agent();
        let before = Timestamp::from_system_time(SystemTime::now());

        queued(&mut state, "Run the tests");
        let after = Timestamp::from_system_time(SystemTime::now());

        let sent_at = entry(&mut state)
            .sending
            .in_place(SendingPlace::Chat)
            .map(|prompt| prompt.sent_at)
            .next()
            .flatten();
        assert!(before <= sent_at && sent_at <= after, "{sent_at:?}");
    }

    #[test]
    fn an_idle_agents_message_shows_in_the_chat_until_its_echo() {
        let mut state = idle_agent();

        queued(&mut state, "Run the tests");
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

        queued(&mut state, "yes");
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
        let first = queued(&mut state, "again");
        let second = queued(&mut state, "again");

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
        queued(&mut state, "Look at this");
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

    /// A message with one image, sent to the busy agent.
    fn sent_with_image(state: &mut AppState, data: &str) {
        start(
            state,
            agent(),
            &Prompt {
                text: "Look at this".to_owned(),
                images: vec![Image {
                    media_type: ImageMediaType::Png,
                    data: data.to_owned(),
                }],
            },
            PromptDelivery::Queue,
            &Draft::default(),
            HOLD,
            Instant::now(),
        );
    }

    /// The echo of `sent_with_image`.
    fn echo_with_image(id: u32) -> AgentDetails {
        let mut echo = waiting(&[(id, "Look at this")]);
        if let Some(prompt) = echo.waiting_prompts.first_mut() {
            prompt.image_count = 1;
        }
        echo
    }

    #[test]
    fn two_alike_messages_keep_no_images_they_could_swap() {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        entry(&mut state).details = Loadable::Loaded(waiting(&[]));
        sent_with_image(&mut state, "first");
        sent_with_image(&mut state, "second");

        entry(&mut state).details = Loadable::Loaded(echo_with_image(1));
        waiting_changed(entry(&mut state));

        assert!(
            entry(&mut state)
                .sending
                .waiting_images(PromptId::new(1))
                .is_none()
        );
    }

    #[test]
    fn a_waiting_message_keeps_the_images_it_left_with_until_it_leaves() {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        entry(&mut state).details = Loadable::Loaded(waiting(&[]));
        let image = Image {
            media_type: ImageMediaType::Png,
            data: "iVBORw0KGgo=".to_owned(),
        };
        start(
            &mut state,
            agent(),
            &Prompt {
                text: "Look at this".to_owned(),
                images: vec![image.clone()],
            },
            PromptDelivery::Queue,
            &Draft::default(),
            HOLD,
            Instant::now(),
        );
        let mut echo = waiting(&[(1, "Look at this")]);
        if let Some(prompt) = echo.waiting_prompts.first_mut() {
            prompt.image_count = 1;
        }
        let kept = |state: &mut AppState| {
            entry(state)
                .sending
                .waiting_images(PromptId::new(1))
                .map(<[Arc<Image>]>::to_vec)
        };

        entry(&mut state).details = Loadable::Loaded(echo);
        waiting_changed(entry(&mut state));
        assert_eq!(kept(&mut state), Some(vec![Arc::new(image)]));

        entry(&mut state).details = Loadable::Loaded(waiting(&[]));
        waiting_changed(entry(&mut state));
        assert_eq!(kept(&mut state), None);
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
        state.machines.this.connection = crate::state::ConnectionState::Connected;
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
        queued(&mut state, "yes");

        // The daemon cancels the older "yes" first, then queues the new one.
        set_waiting(&mut state, &[]);
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["yes"]);
        set_waiting(&mut state, &[(2, "yes")]);
        assert!(texts(&mut state, SendingPlace::Waiting).is_empty());
    }

    #[test]
    fn an_older_same_message_read_by_claude_is_no_echo() {
        let mut state = working_agent(Loadable::Loaded(waiting(&[(1, "yes")])));
        queued(&mut state, "yes");

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
        queued(&mut state, "yes");

        // The details asked for before the send hold an older "yes".
        entry(&mut state).details = Loadable::Loaded(waiting(&[(1, "yes")]));
        details_loaded(entry(&mut state));
        waiting_changed(entry(&mut state));
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["yes"]);

        set_waiting(&mut state, &[(1, "yes"), (2, "yes")]);
        assert!(texts(&mut state, SendingPlace::Waiting).is_empty());
    }

    /// An update can reach the app before the details answer it follows. The message sent while
    /// the details loaded knows the answer's waiting messages, so the update holds its echo.
    #[test]
    fn an_echo_that_overtakes_the_details_answer_ends_the_wait() {
        let mut state = working_agent(Loadable::Loading);
        queued(&mut state, "yes");
        let changes = vec![DetailsChange::WaitingPrompts {
            waiting_prompts: waiting(&[(1, "yes")]).waiting_prompts,
        }];
        let echo = EventRecord {
            seq: Seq::new(101),
            event: Event::DetailsUpdated {
                agent: agent(),
                changes,
            },
        };

        apply_update(
            &mut state,
            &mut Drafts::default(),
            SessionUpdate::Event(echo),
            Instant::now(),
            MachineId::THIS,
        );
        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentDetails,
            crate::state::Purpose::Details { agent: agent() },
            Ok(ResponseBody::Details {
                details: waiting(&[]),
                last_seq: Some(Seq::new(100)),
            }),
            MachineId::THIS,
        );

        assert!(texts(&mut state, SendingPlace::Waiting).is_empty());
    }

    #[test]
    fn one_waiting_echo_ends_one_wait_even_as_the_list_changes_again() {
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        queued(&mut state, "again");
        queued(&mut state, "again");

        set_waiting(&mut state, &[(5, "again")]);
        set_waiting(&mut state, &[(5, "again")]);
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["again"]);

        set_waiting(&mut state, &[(5, "again"), (6, "again")]);
        assert!(texts(&mut state, SendingPlace::Waiting).is_empty());
    }

    #[test]
    fn a_subagents_prompt_is_no_echo() {
        let mut state = idle_agent();
        queued(&mut state, "Review the parser");
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
            Ok(cockpit_protocol::ResponseBody::Details {
                details: waiting(&[(1, "yes")]),
                last_seq: None,
            }),
            MachineId::THIS,
        );
        assert!(entry(&mut state).waiting_prompts().is_empty());

        // The agent between turns again: the user's "yes" joins the chat.
        entry(&mut state).summary.status = AgentStatus::Idle;
        queued(&mut state, "yes");
        item_appended(entry(&mut state), &user_message(9, "yes", Vec::new()));

        assert!(texts(&mut state, SendingPlace::Chat).is_empty());
    }

    fn steer(state: &mut AppState, text: &str, now: Instant) -> Option<SendingId> {
        let draft = Draft {
            text: text.to_owned(),
            ..Draft::default()
        };
        start(
            state,
            agent(),
            &prompt(text),
            PromptDelivery::Steer,
            &draft,
            HOLD,
            now,
        )
    }

    fn held(state: &mut AppState) -> Vec<String> {
        entry(state)
            .sending
            .in_place(SendingPlace::Waiting)
            .filter(|prompt| prompt.held_until().is_some())
            .map(|prompt| prompt.text.clone())
            .collect()
    }

    #[test]
    fn only_a_message_steered_to_a_busy_agent_waits_in_the_app() {
        let now = Instant::now();
        let mut idle = idle_agent();
        assert!(steer(&mut idle, "Start", now).is_some());

        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        assert!(steer(&mut state, "/compact", now).is_some());
        assert_eq!(steer(&mut state, "Also the docs", now), None);
        assert_eq!(held(&mut state), ["Also the docs"]);
    }

    #[test]
    fn a_held_message_leaves_once_its_hold_ends() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        steer(&mut state, "Also the docs", now);

        assert!(release_due(&mut state, now + HOLD / 2).is_empty());
        let sent = release_due(&mut state, now + HOLD);

        let [
            Outgoing {
                request: Request::AgentPrompt(params),
                purpose: Purpose::Prompt { draft, .. },
                ..
            },
        ] = sent.as_slice()
        else {
            panic!("one prompt leaves: {sent:?}");
        };
        assert_eq!(params.delivery, PromptDelivery::Steer);
        assert_eq!(params.prompt.text, "Also the docs");
        assert_eq!(draft.text, "Also the docs");
        assert!(held(&mut state).is_empty());
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["Also the docs"]);
        assert!(release_due(&mut state, now + HOLD * 2).is_empty());
    }

    #[test]
    fn send_now_sends_a_held_message_at_once() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        steer(&mut state, "Also the docs", now);
        let id = entry(&mut state).sending.prompts[0].id;

        assert_eq!(send_now(&mut state, agent(), id).len(), 1);
        assert!(send_now(&mut state, agent(), id).is_empty());
    }

    #[test]
    fn cancel_puts_the_text_back_after_the_composers() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        let mut drafts = Drafts::default();
        steer(&mut state, "First", now);
        steer(&mut state, "Second", now);
        let ids: Vec<SendingId> = entry(&mut state)
            .sending
            .in_place(SendingPlace::Waiting)
            .map(|prompt| prompt.id)
            .collect();

        cancel(&mut state, &mut drafts, agent(), ids[0]);
        assert_eq!(
            drafts.draft(agent()).map(|draft| draft.text.as_str()),
            Some("First")
        );
        cancel(&mut state, &mut drafts, agent(), ids[1]);
        assert_eq!(
            drafts.draft(agent()).map(|draft| draft.text.as_str()),
            Some("First\n\nSecond")
        );
        assert!(held(&mut state).is_empty());
        assert!(release_due(&mut state, now + HOLD).is_empty());
    }

    #[test]
    fn a_message_that_left_cannot_be_cancelled() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        let mut drafts = Drafts::default();
        steer(&mut state, "Also the docs", now);
        let id = entry(&mut state).sending.prompts[0].id;
        release_due(&mut state, now + HOLD);

        cancel(&mut state, &mut drafts, agent(), id);

        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["Also the docs"]);
        assert_eq!(drafts.draft(agent()), None);
    }

    #[test]
    fn a_same_waiting_message_is_no_echo_of_a_held_one() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        steer(&mut state, "yes", now);

        // Another app sent "yes" meanwhile.
        set_waiting(&mut state, &[(4, "yes")]);
        assert_eq!(held(&mut state), ["yes"]);

        // Once it leaves, the "yes" already there is no echo either.
        release_due(&mut state, now + HOLD);
        set_waiting(&mut state, &[(4, "yes")]);
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["yes"]);
        set_waiting(&mut state, &[(4, "yes"), (5, "yes")]);
        assert!(texts(&mut state, SendingPlace::Waiting).is_empty());
    }

    #[test]
    fn a_reconnect_keeps_the_held_messages_only() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        queued(&mut state, "Left already");
        steer(&mut state, "Still held", now);

        let kept = keep_held(&entry(&mut state).sending);

        let texts: Vec<&str> = kept
            .prompts
            .iter()
            .map(|prompt| prompt.text.as_str())
            .collect();
        assert_eq!(texts, ["Still held"]);
    }

    fn left_texts(sent: &[Outgoing]) -> Vec<&str> {
        sent.iter()
            .filter_map(|outgoing| match &outgoing.request {
                Request::AgentPrompt(params) => Some(params.prompt.text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_message_sent_during_a_hold_leaves_after_the_held_one() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        assert_eq!(steer(&mut state, "A first", now), None);
        // The turn ends during the hold: "B" alone would start a turn before "A".
        entry(&mut state).summary.status = AgentStatus::Idle;
        assert_eq!(steer(&mut state, "B second", now + HOLD / 2), None);
        assert_eq!(steer(&mut state, "/compact", now + HOLD / 2), None);

        let sent = release_due(&mut state, now + HOLD * 2);

        assert_eq!(left_texts(&sent), ["A first", "B second", "/compact"]);
    }

    #[test]
    fn send_now_sends_the_held_messages_before_it_first() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        steer(&mut state, "A first", now);
        steer(&mut state, "B second", now);
        let second = entry(&mut state).sending.prompts[1].id;

        let sent = send_now(&mut state, agent(), second);

        assert_eq!(left_texts(&sent), ["A first", "B second"]);
    }

    #[test]
    fn a_held_message_waits_while_its_machine_does_not_answer() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        steer(&mut state, "Also the docs", now);
        state.machines.this.connection = crate::state::ConnectionState::Connecting { attempt: 1 };

        assert!(release_due(&mut state, now + HOLD).is_empty());
        assert_eq!(held(&mut state), ["Also the docs"]);
        // Nothing comes due meanwhile, so the window does not look again and again.
        assert_eq!(next_release(&state), None);
    }

    #[test]
    fn a_refused_held_message_comes_back_after_the_text_typed_meanwhile() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        let mut drafts = Drafts::default();
        steer(&mut state, "Also the docs", now);
        drafts.draft_mut(agent()).text = "Next thought".to_owned();
        let purpose = release_due(&mut state, now + HOLD)
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

        assert_eq!(
            drafts.draft(agent()).map(|draft| draft.text.as_str()),
            Some("Next thought\n\nAlso the docs")
        );
    }

    #[test]
    fn a_draft_comes_back_without_a_blank_line_beside_images_alone() {
        let mut drafts = Drafts::default();
        drafts.draft_mut(agent()).images.push(Image {
            media_type: ImageMediaType::Png,
            data: "iVBORw0KGgo=".to_owned(),
        });

        restore_draft(
            &mut drafts,
            agent(),
            Draft {
                text: "Look at this".to_owned(),
                ..Draft::default()
            },
        );

        assert_eq!(
            drafts.draft(agent()).map(|draft| draft.text.as_str()),
            Some("Look at this")
        );
    }

    #[test]
    fn without_a_hold_a_steered_message_leaves_at_once() {
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        let draft = Draft {
            text: "Also the docs".to_owned(),
            ..Draft::default()
        };

        let sent = start(
            &mut state,
            agent(),
            &prompt("Also the docs"),
            PromptDelivery::Steer,
            &draft,
            Duration::ZERO,
            Instant::now(),
        );

        assert!(sent.is_some());
        assert!(held(&mut state).is_empty());
        assert_eq!(texts(&mut state, SendingPlace::Waiting), ["Also the docs"]);
        assert_eq!(state.prefs.chat.steer_hold.duration(), Duration::ZERO);
    }

    #[test]
    fn a_shorter_hold_set_meanwhile_keeps_the_order_sent() {
        let now = Instant::now();
        let mut state = working_agent(Loadable::Loaded(waiting(&[])));
        let steer_for = |state: &mut AppState, text: &str, hold: Duration, at: Instant| {
            let draft = Draft {
                text: text.to_owned(),
                ..Draft::default()
            };
            start(
                state,
                agent(),
                &prompt(text),
                PromptDelivery::Steer,
                &draft,
                hold,
                at,
            )
        };
        steer_for(&mut state, "A first", Duration::from_secs(10), now);
        steer_for(&mut state, "B second", HOLD, now + Duration::from_secs(1));

        assert!(release_due(&mut state, now + Duration::from_secs(5)).is_empty());
        let sent = release_due(&mut state, now + Duration::from_secs(10));
        assert_eq!(left_texts(&sent), ["A first", "B second"]);
    }
}
