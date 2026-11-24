use std::time::{Duration, Instant};

use cockpit_protocol::{AgentId, AgentStatus};
use serde::{Deserialize, Serialize};

use super::quotes::Opening;
use super::{AgentEntry, AppState, Drafts};
use crate::prefs::AfterDictation;

/// A press of the talk key shorter than this latches the recording on, until the next press.
const TAP: Duration = Duration::from_millis(300);

/// Dictation as the window shows it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VoiceState {
    /// This build has dictation, and this computer can run its model.
    pub available: bool,
    pub phase: VoicePhase,
    pub live: LiveWords,
    /// The model of the language chosen in Settings.
    pub model: ModelStatus,
    /// The model of the other language is on this computer too.
    pub other_model: bool,
    /// The card above the mic button.
    pub card: Option<VoiceCard>,
    /// The microphones Settings lists, once listed.
    pub microphones: Option<MicrophoneList>,
}

/// The microphones of this computer, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MicrophoneList {
    /// The system default's name, when there is one.
    pub default: Option<String>,
    pub names: Vec<String>,
}

/// When the microphone chosen in Settings went missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicMissing {
    /// It was not connected when the dictation started.
    AtStart,
    /// It was disconnected during the dictation.
    MidWay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoicePhase {
    #[default]
    Idle,
    /// Recording for `agent`, into `to`. A latched recording goes on without the key held.
    Recording {
        agent: AgentId,
        to: Destination,
        since: Instant,
        latched: bool,
    },
    /// Stopped, its last words on their way.
    Finishing { agent: AgentId, to: Destination },
}

/// The field a dictation starts in: the talk key's focus, or the mic the user clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    #[default]
    Message,
    /// The comment field of a quote.
    Comment,
}

/// Where a dictation's text goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Message,
    /// The comment field, while the opening the dictation started in stays open.
    Comment {
        opening: Opening,
    },
}

impl Destination {
    pub fn field(self) -> Field {
        match self {
            Self::Message => Field::Message,
            Self::Comment { .. } => Field::Comment,
        }
    }
}

/// A dictation: the agent it writes for, and where its text goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dictation {
    pub agent: AgentId,
    pub to: Destination,
}

/// How the final text goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// At the caret of the message, on screen.
    MessageCaret,
    /// At the end of the agent's message, which waits there to be sent.
    MessageEnd,
    /// At the end of the agent's message, which then goes.
    MessageEndAndSend,
    /// At the caret of the comment field, on screen.
    CommentCaret,
    /// At the end of the comment, whose field is open for an agent not on screen.
    CommentEnd,
}

/// The text of the recording so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveWords {
    /// Before the last pause: it no longer changes.
    pub said: String,
    /// Since the last pause: it may still change.
    pub hearing: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelStatus {
    #[default]
    Missing,
    Downloading {
        done: u64,
        total: u64,
        from: DownloadFrom,
    },
    Ready,
}

/// Where the user started the model's download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadFrom {
    /// The card above the mic, which then stays open to say that the model is ready.
    Card,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceCard {
    /// The model to download, or its download.
    Model,
    /// Why the last dictation or download failed.
    Failed(String),
    /// The dictation records from the system default, `using`, since the microphone chosen is
    /// missing.
    Fallback {
        chosen: String,
        using: Option<String>,
        missing: MicMissing,
    },
}

/// What the voice worker has to do after a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerOrder {
    Start,
    Stop,
    Cancel,
    Nothing,
}

/// The agent a dictation can start for: the selected one, unless it exited, since its message
/// field is then off.
pub fn dictation_target(state: &AppState) -> Option<AgentId> {
    state
        .selected()
        .filter(|entry| entry.summary.status != AgentStatus::Exited)
        .map(AgentEntry::id)
}

/// Where a dictation started in `field` for `agent` writes: the comment field open for the
/// agent, with its opening, or else the message.
pub fn destination(field: Field, agent: AgentId, drafts: &Drafts) -> Destination {
    match (field, &drafts.quote_editor) {
        (Field::Comment, Some(editor)) if editor.agent == agent => Destination::Comment {
            opening: editor.opening,
        },
        (Field::Comment | Field::Message, _) => Destination::Message,
    }
}

/// How `dictation`'s final text goes in, with `shown` telling whether its agent is on screen. A
/// comment takes it only while the opening it started in stays open, and never sends anything.
/// A comment that closed, or gave way to another one, leaves the text at the end of the message.
pub fn landing(
    dictation: Dictation,
    shown: bool,
    after: AfterDictation,
    drafts: &Drafts,
) -> Landing {
    match dictation.to {
        Destination::Comment { opening } => {
            match (field_open(drafts, dictation.agent, opening), shown) {
                (true, true) => Landing::CommentCaret,
                (true, false) => Landing::CommentEnd,
                (false, _) => Landing::MessageEnd,
            }
        }
        Destination::Message => match after {
            AfterDictation::Insert if shown => Landing::MessageCaret,
            AfterDictation::Insert => Landing::MessageEnd,
            AfterDictation::InsertAndSend => Landing::MessageEndAndSend,
        },
    }
}

/// Whether the comment field's `opening` is still open for `agent`.
fn field_open(drafts: &Drafts, agent: AgentId, opening: Opening) -> bool {
    drafts
        .quote_editor
        .as_ref()
        .is_some_and(|editor| editor.agent == agent && editor.opening == opening)
}

impl VoiceState {
    /// The talk key went down. `target` is the dictation a recording would start, for the agent
    /// [`dictation_target`] gives: without one, the key only stops a recording.
    pub fn press(&mut self, target: Option<Dictation>, now: Instant) -> WorkerOrder {
        match (self.phase, target) {
            (VoicePhase::Idle, Some(target)) => self.start(target, now, /* latched */ false),
            (VoicePhase::Recording { latched: true, .. }, _) => self.finish(),
            (VoicePhase::Idle, None)
            | (VoicePhase::Recording { latched: false, .. } | VoicePhase::Finishing { .. }, _) => {
                WorkerOrder::Nothing
            }
        }
    }

    /// The window lost the focus with the talk key down, whose release then never comes: a
    /// recording the key holds stops, however short. A tapped one goes on.
    pub fn blur(&mut self) -> WorkerOrder {
        match self.phase {
            VoicePhase::Recording { latched: false, .. } => self.finish(),
            VoicePhase::Idle
            | VoicePhase::Recording { latched: true, .. }
            | VoicePhase::Finishing { .. } => WorkerOrder::Nothing,
        }
    }

    /// The talk key came up: it stops a recording it held, unless it was only tapped.
    pub fn release(&mut self, now: Instant) -> WorkerOrder {
        match self.phase {
            VoicePhase::Recording {
                agent,
                to,
                since,
                latched: false,
            } => {
                if now.saturating_duration_since(since) < TAP {
                    self.phase = VoicePhase::Recording {
                        agent,
                        to,
                        since,
                        latched: true,
                    };
                    WorkerOrder::Nothing
                } else {
                    self.finish()
                }
            }
            VoicePhase::Idle | VoicePhase::Recording { .. } | VoicePhase::Finishing { .. } => {
                WorkerOrder::Nothing
            }
        }
    }

    /// A click on the mic button: it starts a recording that runs until the next click. `target`
    /// is as for [`Self::press`].
    pub fn click(&mut self, target: Option<Dictation>, now: Instant) -> WorkerOrder {
        match (self.phase, target) {
            (VoicePhase::Idle, Some(target)) => self.start(target, now, /* latched */ true),
            (VoicePhase::Recording { .. }, _) => self.finish(),
            (VoicePhase::Idle, None) | (VoicePhase::Finishing { .. }, _) => WorkerOrder::Nothing,
        }
    }

    /// Enter: it stops the recording as the talk key's second tap does, whether tapped or held.
    /// The next Enter sends the message, or adds the quote.
    pub fn stop(&mut self) -> WorkerOrder {
        match self.phase {
            VoicePhase::Recording { .. } => self.finish(),
            VoicePhase::Idle | VoicePhase::Finishing { .. } => WorkerOrder::Nothing,
        }
    }

    /// The comment field changed: a recording into a comment that closed, or gave way to another,
    /// stops as Enter stops it. Its text then goes at the end of the message.
    pub fn comment_changed(&mut self, drafts: &Drafts) -> WorkerOrder {
        match self.phase {
            VoicePhase::Recording {
                agent,
                to: Destination::Comment { opening },
                ..
            } if !field_open(drafts, agent, opening) => self.finish(),
            VoicePhase::Idle | VoicePhase::Recording { .. } | VoicePhase::Finishing { .. } => {
                WorkerOrder::Nothing
            }
        }
    }

    /// The agents changed: a dictation into an agent that is gone, closed or removed by its
    /// daemon, stops and drops its text, which has nowhere to go.
    pub fn agents_changed(&mut self, state: &AppState) -> WorkerOrder {
        match self.dictating() {
            Some(agent) if !state.agents.contains_key(&agent) => self.cancel(),
            Some(_) | None => WorkerOrder::Nothing,
        }
    }

    /// Esc: the recording and its text go.
    pub fn cancel(&mut self) -> WorkerOrder {
        if self.phase == VoicePhase::Idle {
            return WorkerOrder::Nothing;
        }
        self.phase = VoicePhase::Idle;
        self.live = LiveWords::default();
        WorkerOrder::Cancel
    }

    /// The worker's final text came: the dictation it belongs to.
    pub fn done(&mut self) -> Option<Dictation> {
        let dictation = self.writing();
        self.phase = VoicePhase::Idle;
        self.live = LiveWords::default();
        dictation
    }

    /// The microphone chosen is missing, and the dictation goes on with the system default: the
    /// card says so, and stays after the dictation until closed.
    pub fn fell_back(&mut self, chosen: String, using: Option<String>, missing: MicMissing) {
        self.card = Some(VoiceCard::Fallback {
            chosen,
            using,
            missing,
        });
    }

    pub fn failed(&mut self, why: String) {
        self.phase = VoicePhase::Idle;
        self.live = LiveWords::default();
        self.card = Some(VoiceCard::Failed(why));
    }

    /// The model's download starts, and the card shows its progress.
    pub fn download_started(&mut self, from: DownloadFrom, total: u64) {
        self.model = ModelStatus::Downloading {
            done: 0,
            total,
            from,
        };
        self.card = Some(VoiceCard::Model);
    }

    pub fn download_progress(&mut self, bytes: u64) {
        if let ModelStatus::Downloading { done, .. } = &mut self.model {
            *done = bytes;
        }
    }

    /// The download ended. Started from the card, the card stays to say that the model is
    /// ready, unless the user hid it. Started from Settings, it closes.
    pub fn download_ended(&mut self, result: Result<(), String>) {
        let from = match self.model {
            ModelStatus::Downloading { from, .. } => Some(from),
            ModelStatus::Missing | ModelStatus::Ready => None,
        };
        match result {
            Ok(()) => {
                self.model = ModelStatus::Ready;
                if from != Some(DownloadFrom::Card) {
                    self.card = None;
                }
            }
            Err(why) => {
                self.model = ModelStatus::Missing;
                self.card = Some(VoiceCard::Failed(why));
            }
        }
    }

    /// The agent the text is for, while a recording runs or finishes.
    pub fn dictating(&self) -> Option<AgentId> {
        self.writing().map(|dictation| dictation.agent)
    }

    /// Whether the dictation that runs or finishes writes into the comment field's `opening`.
    pub fn writes_comment(&self, opening: Opening) -> bool {
        self.writing()
            .is_some_and(|dictation| dictation.to == Destination::Comment { opening })
    }

    /// The dictation that runs or finishes.
    pub fn writing(&self) -> Option<Dictation> {
        match self.phase {
            VoicePhase::Recording { agent, to, .. } | VoicePhase::Finishing { agent, to } => {
                Some(Dictation { agent, to })
            }
            VoicePhase::Idle => None,
        }
    }

    /// Without the model, the card offers its download instead.
    fn start(&mut self, target: Dictation, now: Instant, latched: bool) -> WorkerOrder {
        if !self.available {
            return WorkerOrder::Nothing;
        }
        if self.model != ModelStatus::Ready {
            self.card = Some(VoiceCard::Model);
            return WorkerOrder::Nothing;
        }
        self.card = None;
        self.live = LiveWords::default();
        self.phase = VoicePhase::Recording {
            agent: target.agent,
            to: target.to,
            since: now,
            latched,
        };
        WorkerOrder::Start
    }

    fn finish(&mut self) -> WorkerOrder {
        let Some(Dictation { agent, to }) = self.writing() else {
            return WorkerOrder::Nothing;
        };
        self.phase = VoicePhase::Finishing { agent, to };
        WorkerOrder::Stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> VoiceState {
        VoiceState {
            available: true,
            model: ModelStatus::Ready,
            ..VoiceState::default()
        }
    }

    fn agent() -> AgentId {
        AgentId::new(3)
    }

    fn target() -> Option<Dictation> {
        Some(Dictation {
            agent: agent(),
            to: Destination::Message,
        })
    }

    fn message_of(agent: Option<AgentId>) -> Option<Dictation> {
        agent.map(|agent| Dictation {
            agent,
            to: Destination::Message,
        })
    }

    /// Opens the comment field for `agent` on a passage, and returns its opening.
    fn open_comment(drafts: &mut Drafts, agent: AgentId) -> Opening {
        use crate::state::Utf16Offset;
        use crate::state::quotes::{self, Passage, QuoteAnchor};
        use cockpit_protocol::ItemId;

        let passage = Passage {
            anchor: QuoteAnchor {
                item: ItemId::new(7),
                start: Utf16Offset::new(0),
                end: Utf16Offset::new(4),
            },
            raw: "lexer".to_owned(),
            text: "lexer".to_owned(),
            code: false,
        };
        quotes::start(drafts, agent, passage);
        drafts.quote_editor.as_ref().unwrap().opening
    }

    /// Drafts with a comment field open for `agent`, at `opening`.
    fn comment_open(agent: AgentId) -> (Drafts, Opening) {
        let mut drafts = Drafts::default();
        let opening = open_comment(&mut drafts, agent);
        (drafts, opening)
    }

    #[test]
    fn a_dictation_started_in_the_comment_writes_into_that_opening() {
        let (drafts, opening) = comment_open(agent());
        let other = AgentId::new(4);

        assert_eq!(
            destination(Field::Comment, agent(), &drafts),
            Destination::Comment { opening }
        );
        assert_eq!(
            destination(Field::Message, agent(), &drafts),
            Destination::Message
        );
        // The comment field of another agent, or none, leaves the message.
        assert_eq!(
            destination(Field::Comment, other, &drafts),
            Destination::Message
        );
        assert_eq!(
            destination(Field::Comment, agent(), &Drafts::default()),
            Destination::Message
        );

        let mut voice = ready();
        let to = Destination::Comment { opening };
        voice.click(Some(Dictation { agent: agent(), to }), Instant::now());
        assert_eq!(voice.writing(), Some(Dictation { agent: agent(), to }));
        voice.stop();
        assert_eq!(voice.phase, VoicePhase::Finishing { agent: agent(), to });
        assert_eq!(voice.done(), Some(Dictation { agent: agent(), to }));
    }

    #[test]
    fn a_comment_takes_the_text_only_while_its_opening_stays_open() {
        use crate::state::quotes;

        let (mut drafts, opening) = comment_open(agent());
        let into_comment = Dictation {
            agent: agent(),
            to: Destination::Comment { opening },
        };
        let insert = AfterDictation::Insert;
        let (shown, hidden) = (true, false);

        assert_eq!(
            landing(into_comment, shown, insert, &drafts),
            Landing::CommentCaret
        );
        assert_eq!(
            landing(into_comment, hidden, insert, &drafts),
            Landing::CommentEnd
        );
        // Another field opened: the text goes at the end of the message.
        let mut reopened = drafts.clone();
        open_comment(&mut reopened, agent());
        assert_eq!(
            landing(into_comment, shown, insert, &reopened),
            Landing::MessageEnd
        );
        // The field closed.
        quotes::cancel(&mut drafts, Some(agent()));
        assert_eq!(
            landing(into_comment, shown, insert, &drafts),
            Landing::MessageEnd
        );
    }

    #[test]
    fn a_recording_into_a_comment_stops_once_the_comment_closes_or_gives_way() {
        use cockpit_protocol::PromptDelivery;

        use crate::state::quotes::{self, QuoteId};
        use crate::state::{actions, state_with, summary};

        let mut state = state_with(vec![summary(3, AgentStatus::Idle)]);
        let recording_into = |opening| {
            let mut voice = ready();
            let to = Destination::Comment { opening };
            voice.click(Some(Dictation { agent: agent(), to }), Instant::now());
            voice
        };
        let first_quote = |drafts: &Drafts| -> Option<QuoteId> {
            drafts
                .draft(agent())
                .and_then(|draft| draft.quotes.first())
                .map(|quote| quote.id)
        };
        let mut stops = Vec::new();

        // While the comment stays open, the recording goes on.
        let (mut drafts, opening) = comment_open(agent());
        let mut voice = recording_into(opening);
        stops.push(voice.comment_changed(&drafts) == WorkerOrder::Nothing);
        // Its ×.
        quotes::cancel(&mut drafts, Some(agent()));
        stops.push(voice.comment_changed(&drafts) == WorkerOrder::Stop);
        stops.push(matches!(
            voice.phase,
            VoicePhase::Finishing {
                to: Destination::Comment { .. },
                ..
            }
        ));
        // Reply on another passage.
        let opening = open_comment(&mut drafts, agent());
        let mut voice = recording_into(opening);
        open_comment(&mut drafts, agent());
        stops.push(voice.comment_changed(&drafts) == WorkerOrder::Stop);
        // Editing a quote.
        quotes::commit(&mut drafts, state.prefs.chat.comments);
        let quote = first_quote(&drafts);
        if let Some(quote) = quote {
            quotes::edit(&mut drafts, agent(), quote);
        }
        let mut voice = recording_into(drafts.quote_editor.as_ref().unwrap().opening);
        if let Some(quote) = quote {
            quotes::edit(&mut drafts, agent(), quote);
        }
        stops.push(voice.comment_changed(&drafts) == WorkerOrder::Stop);
        // Removing the quote whose comment is open.
        let mut voice = recording_into(drafts.quote_editor.as_ref().unwrap().opening);
        if let Some(quote) = quote {
            quotes::remove(&mut drafts, agent(), quote);
        }
        stops.push(voice.comment_changed(&drafts) == WorkerOrder::Stop);
        // Sending the message, which takes the open comment with it.
        let opening = open_comment(&mut drafts, agent());
        let mut voice = recording_into(opening);
        actions::send_prompt(
            &mut state,
            &mut drafts,
            agent(),
            None,
            PromptDelivery::Steer,
        );
        stops.push(voice.comment_changed(&drafts) == WorkerOrder::Stop);
        // A recording into the message, or one already finishing, goes on as it is.
        let mut voice = ready();
        voice.click(target(), Instant::now());
        stops.push(voice.comment_changed(&Drafts::default()) == WorkerOrder::Nothing);
        voice.stop();
        stops.push(voice.comment_changed(&Drafts::default()) == WorkerOrder::Nothing);

        assert_eq!(stops, [true; 9]);
    }

    #[test]
    fn insert_and_send_never_sends_from_a_comment() {
        let (drafts, opening) = comment_open(agent());
        let into_comment = Dictation {
            agent: agent(),
            to: Destination::Comment { opening },
        };
        let into_message = Dictation {
            agent: agent(),
            to: Destination::Message,
        };
        let send = AfterDictation::InsertAndSend;
        let (shown, hidden) = (true, false);

        assert_eq!(
            landing(into_comment, shown, send, &drafts),
            Landing::CommentCaret
        );
        assert_eq!(
            landing(into_comment, shown, send, &Drafts::default()),
            Landing::MessageEnd
        );
        assert_eq!(
            landing(into_message, shown, send, &drafts),
            Landing::MessageEndAndSend
        );
        assert_eq!(
            landing(into_message, shown, AfterDictation::Insert, &drafts),
            Landing::MessageCaret
        );
        assert_eq!(
            landing(into_message, hidden, AfterDictation::Insert, &drafts),
            Landing::MessageEnd
        );
    }

    #[test]
    fn leaving_the_window_stops_a_held_recording_however_short() {
        let mut voice = ready();
        let now = Instant::now();
        voice.press(target(), now);

        assert_eq!(voice.blur(), WorkerOrder::Stop);
        assert_eq!(
            voice.phase,
            VoicePhase::Finishing {
                agent: agent(),
                to: Destination::Message
            }
        );
        assert_eq!(voice.release(now), WorkerOrder::Nothing);
    }

    #[test]
    fn leaving_the_window_keeps_a_tapped_recording() {
        let mut voice = ready();
        let now = Instant::now();
        voice.press(target(), now);
        voice.release(now + Duration::from_millis(120));

        assert_eq!(voice.blur(), WorkerOrder::Nothing);
        assert!(matches!(
            voice.phase,
            VoicePhase::Recording { latched: true, .. }
        ));
    }

    #[test]
    fn enter_stops_a_tapped_or_held_recording_and_never_starts_one() {
        let mut voice = ready();
        let now = Instant::now();
        assert_eq!(voice.stop(), WorkerOrder::Nothing);
        assert_eq!(voice.phase, VoicePhase::Idle);

        voice.press(target(), now);
        voice.release(now + Duration::from_millis(120));
        assert_eq!(voice.stop(), WorkerOrder::Stop);
        assert!(matches!(voice.phase, VoicePhase::Finishing { .. }));
        assert_eq!(voice.stop(), WorkerOrder::Nothing);

        voice.done();
        voice.press(target(), now);
        assert_eq!(voice.stop(), WorkerOrder::Stop);
        assert_eq!(
            voice.release(now + Duration::from_secs(2)),
            WorkerOrder::Nothing
        );
    }

    #[test]
    fn an_exited_agent_takes_no_dictation() {
        use crate::state::{state_with, summary};

        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Exited),
        ]);
        state.ui.selected = Some(AgentId::new(1));
        let running = message_of(dictation_target(&state));
        state.ui.selected = Some(AgentId::new(2));
        let exited = message_of(dictation_target(&state));
        let mut voice = ready();
        let now = Instant::now();

        assert_eq!(
            running.map(|dictation| dictation.agent),
            Some(AgentId::new(1))
        );
        assert_eq!(exited, None);
        assert_eq!(voice.press(exited, now), WorkerOrder::Nothing);
        assert_eq!(voice.click(exited, now), WorkerOrder::Nothing);
        assert_eq!(voice.phase, VoicePhase::Idle);
        // A recording that runs still stops from there.
        voice.click(running, now);
        assert_eq!(voice.press(exited, now), WorkerOrder::Stop);
    }

    #[test]
    fn a_download_from_the_card_leaves_it_open_to_say_the_model_is_ready() {
        let mut from_card = VoiceState::default();
        from_card.download_started(DownloadFrom::Card, 10);
        from_card.download_progress(4);
        let progress = from_card.model;
        from_card.download_ended(Ok(()));
        let mut hidden = VoiceState::default();
        hidden.download_started(DownloadFrom::Card, 10);
        hidden.card = None;
        hidden.download_ended(Ok(()));
        let mut from_settings = VoiceState::default();
        from_settings.download_started(DownloadFrom::Settings, 10);
        from_settings.download_ended(Ok(()));
        let mut failed = VoiceState::default();
        failed.download_started(DownloadFrom::Card, 10);
        failed.card = None;
        failed.download_ended(Err("no network".to_owned()));

        assert_eq!(
            progress,
            ModelStatus::Downloading {
                done: 4,
                total: 10,
                from: DownloadFrom::Card
            }
        );
        assert_eq!(
            (from_card.model, from_card.card),
            (ModelStatus::Ready, Some(VoiceCard::Model))
        );
        assert_eq!((hidden.model, hidden.card), (ModelStatus::Ready, None));
        assert_eq!(
            (from_settings.model, from_settings.card),
            (ModelStatus::Ready, None)
        );
        assert_eq!(
            (failed.model, failed.card),
            (
                ModelStatus::Missing,
                Some(VoiceCard::Failed("no network".to_owned()))
            )
        );
    }

    #[test]
    fn a_held_key_records_until_it_comes_up() {
        let mut voice = ready();
        let now = Instant::now();
        assert_eq!(voice.press(target(), now), WorkerOrder::Start);
        assert_eq!(
            voice.release(now + Duration::from_secs(2)),
            WorkerOrder::Stop
        );
        assert_eq!(
            voice.phase,
            VoicePhase::Finishing {
                agent: agent(),
                to: Destination::Message
            }
        );
        assert_eq!(voice.done(), target());
        assert_eq!(voice.phase, VoicePhase::Idle);
    }

    #[test]
    fn a_tapped_key_records_until_the_next_press() {
        let mut voice = ready();
        let now = Instant::now();
        assert_eq!(voice.press(target(), now), WorkerOrder::Start);
        assert_eq!(
            voice.release(now + Duration::from_millis(120)),
            WorkerOrder::Nothing
        );
        assert!(matches!(
            voice.phase,
            VoicePhase::Recording { latched: true, .. }
        ));
        assert_eq!(
            voice.press(target(), now + Duration::from_secs(5)),
            WorkerOrder::Stop
        );
        // The release of the press that stopped it changes nothing.
        assert_eq!(
            voice.release(now + Duration::from_secs(5)),
            WorkerOrder::Nothing
        );
    }

    #[test]
    fn the_mic_button_starts_and_stops() {
        let mut voice = ready();
        let now = Instant::now();
        assert_eq!(voice.click(target(), now), WorkerOrder::Start);
        assert_eq!(voice.click(target(), now), WorkerOrder::Stop);
        assert_eq!(voice.click(target(), now), WorkerOrder::Nothing);
    }

    #[test]
    fn without_the_model_the_card_offers_it() {
        let mut voice = VoiceState {
            available: true,
            ..VoiceState::default()
        };
        assert_eq!(voice.press(target(), Instant::now()), WorkerOrder::Nothing);
        assert_eq!(voice.card, Some(VoiceCard::Model));
        assert_eq!(voice.phase, VoicePhase::Idle);
    }

    #[test]
    fn without_dictation_nothing_starts() {
        let mut voice = VoiceState {
            model: ModelStatus::Ready,
            ..VoiceState::default()
        };
        assert_eq!(voice.click(target(), Instant::now()), WorkerOrder::Nothing);
        assert_eq!(voice.card, None);
    }

    #[test]
    fn esc_drops_the_recording_and_its_text() {
        let mut voice = ready();
        assert_eq!(voice.cancel(), WorkerOrder::Nothing);
        voice.press(target(), Instant::now());
        voice.live.said = "Run just check".to_owned();
        assert_eq!(voice.cancel(), WorkerOrder::Cancel);
        assert_eq!(voice.phase, VoicePhase::Idle);
        assert_eq!(voice.live, LiveWords::default());
    }

    #[test]
    fn a_failure_shows_on_the_card() {
        let mut voice = ready();
        voice.press(target(), Instant::now());
        voice.failed("this computer has no microphone".to_owned());
        assert_eq!(voice.phase, VoicePhase::Idle);
        assert_eq!(
            voice.card,
            Some(VoiceCard::Failed(
                "this computer has no microphone".to_owned()
            ))
        );
    }

    #[test]
    fn closing_the_agent_dictated_to_cancels_the_dictation() {
        use crate::state::{actions, state_with, summary};

        let mut state = state_with(vec![
            summary(3, AgentStatus::Idle),
            summary(4, AgentStatus::Idle),
        ]);
        let mut drafts = Drafts::default();
        let mut voice = ready();
        voice.press(target(), Instant::now());
        actions::close_agent(&mut state, &mut drafts, AgentId::new(4));
        let other_closed = voice.agents_changed(&state);
        actions::close_agent(&mut state, &mut drafts, agent());
        let closed = voice.agents_changed(&state);

        assert_eq!(other_closed, WorkerOrder::Nothing);
        assert_eq!(closed, WorkerOrder::Cancel);
        assert_eq!(voice.phase, VoicePhase::Idle);
        assert_eq!(voice.agents_changed(&state), WorkerOrder::Nothing);
    }

    #[test]
    fn the_daemon_removing_the_agent_cancels_a_dictation_still_finishing() {
        use cockpit_client::SessionUpdate;
        use cockpit_protocol::{Event, EventRecord, Seq};

        use crate::machine::MachineId;
        use crate::state::{apply_update, state_with, summary};

        let mut state = state_with(vec![summary(3, AgentStatus::Idle)]);
        let (mut drafts, opening) = comment_open(agent());
        let mut voice = ready();
        let to = Destination::Comment { opening };
        voice.click(Some(Dictation { agent: agent(), to }), Instant::now());
        voice.stop();
        let removed = EventRecord {
            seq: Seq::new(1),
            event: Event::AgentRemoved { agent: agent() },
        };
        apply_update(
            &mut state,
            &mut drafts,
            SessionUpdate::Event(removed),
            Instant::now(),
            MachineId::THIS,
        );

        assert_eq!(voice.agents_changed(&state), WorkerOrder::Cancel);
        assert_eq!(voice.phase, VoicePhase::Idle);
        assert_eq!(voice.done(), None);
    }

    #[test]
    fn a_fallback_shows_on_the_card_while_the_dictation_goes_on_and_after() {
        let mut voice = ready();
        voice.press(target(), Instant::now());
        voice.fell_back(
            "Headset".to_owned(),
            Some("Built-in microphone".to_owned()),
            MicMissing::MidWay,
        );
        let recording = voice.dictating();
        voice.release(Instant::now() + TAP);
        voice.done();

        let card = Some(VoiceCard::Fallback {
            chosen: "Headset".to_owned(),
            using: Some("Built-in microphone".to_owned()),
            missing: MicMissing::MidWay,
        });
        assert_eq!(recording, Some(agent()));
        assert_eq!(voice.card, card);
    }
}
