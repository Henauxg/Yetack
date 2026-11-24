use std::time::{Duration, Instant};

use cockpit_protocol::{AgentId, AgentStatus, ItemId};
use serde::{Deserialize, Serialize};

use super::quotes::Opening;
use super::{AgentEntry, AppState, Dialog, Drafts, NewAgentTab, actions};
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
    /// Why the last download of `model` failed. Its row in Settings says so until the next
    /// download or a change of language.
    pub download_failed: Option<String>,
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
    /// Recording into `to`. A latched recording goes on without the key held.
    Recording {
        to: Destination,
        since: Instant,
        latched: bool,
    },
    /// Stopped, its last words on their way. `covered` when a dialog over the chat stopped it.
    Finishing { to: Destination, covered: bool },
}

/// A recording whose last words came.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finished {
    pub to: Destination,
    /// A dialog over the chat stopped it: the words land as under the dialog, even once it
    /// closed.
    pub covered: bool,
}

/// The field a dictation starts in: the talk key's focus, or the mic the user clicked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    #[default]
    Message,
    /// The comment field of a quote.
    Comment,
    /// The "Other" field of question `asked` on the card of the question `item`.
    Answer { item: ItemId, asked: usize },
    /// The first message of the new agent dialog.
    NewAgent,
}

/// Where a dictation's text goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Message {
        agent: AgentId,
    },
    /// The comment field, while the opening the dictation started in stays open.
    Comment {
        agent: AgentId,
        opening: Opening,
    },
    /// The "Other" field of question `asked` on the card of the question `item`, while it waits.
    Answer {
        agent: AgentId,
        item: ItemId,
        asked: usize,
    },
    /// The first message of the new agent dialog, while the dialog stays open.
    NewAgent,
}

impl Destination {
    pub fn field(self) -> Field {
        match self {
            Self::Message { .. } => Field::Message,
            Self::Comment { .. } => Field::Comment,
            Self::Answer { item, asked, .. } => Field::Answer { item, asked },
            Self::NewAgent => Field::NewAgent,
        }
    }

    /// The agent the text is for. The new agent dialog's agent does not exist yet.
    pub fn agent(self) -> Option<AgentId> {
        match self {
            Self::Message { agent } | Self::Comment { agent, .. } | Self::Answer { agent, .. } => {
                Some(agent)
            }
            Self::NewAgent => None,
        }
    }
}

/// What became of the field a dictation writes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldState {
    Open,
    /// A dialog covers the chat the field is in: the recording stops, and its text still goes in
    /// the field, never sent.
    Covered,
    /// The comment closed or gave way, the question no longer waits, or the new agent dialog shows
    /// its other tab or gave way to "Accounts": the recording stops, and its text goes where
    /// [`landing`] says.
    Closed,
    /// The agent is gone, or the new agent dialog closed: the text has nowhere to go.
    Gone,
}

/// How the final text goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// At the caret of the message, on screen.
    MessageCaret,
    /// At the end of the agent's message, which waits there to be sent.
    MessageEnd(AgentId),
    /// At the end of the agent's message, which then goes.
    MessageEndAndSend(AgentId),
    /// At the caret of the comment field, on screen.
    CommentCaret,
    /// At the end of the comment, whose field is open for an agent not on screen.
    CommentEnd,
    /// At the caret of the "Other" field, on screen.
    AnswerCaret {
        agent: AgentId,
        item: ItemId,
        asked: usize,
    },
    /// At the end of the "Other" text, whose card waits for an agent not on screen.
    AnswerEnd {
        agent: AgentId,
        item: ItemId,
        asked: usize,
    },
    /// At the caret of the new agent dialog's first message.
    NewAgentCaret,
    /// At the end of the first message, which the dialog's other tab hides.
    NewAgentEnd,
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

/// What the row of a language in Settings shows of its model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRowState {
    pub status: ModelStatus,
    /// Why the last download of the model failed.
    pub failed: Option<String>,
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

/// Where a dictation started in `field` writes: the new agent dialog's first message, or else for
/// the agent [`dictation_target`] gives, the comment field open for it, the "Other" field of its
/// waiting question, or its message.
pub fn target(field: Field, state: &AppState, drafts: &Drafts) -> Option<Destination> {
    if field == Field::NewAgent && new_agent_prompt_shown(state) {
        return Some(Destination::NewAgent);
    }
    let agent = dictation_target(state)?;
    let to = match (field, &drafts.quote_editor) {
        (Field::Comment, Some(editor)) if editor.agent == agent => Destination::Comment {
            agent,
            opening: editor.opening,
        },
        (Field::Answer { item, asked }, _) if answer_waits(state, agent, item, asked) => {
            Destination::Answer { agent, item, asked }
        }
        (Field::Message | Field::Comment | Field::Answer { .. } | Field::NewAgent, _) => {
            Destination::Message { agent }
        }
    };
    Some(to)
}

/// Whether the new agent dialog shows its first message: open on its "Start in a folder" tab.
pub fn new_agent_prompt_shown(state: &AppState) -> bool {
    matches!(&state.ui.dialog, Some(Dialog::NewAgent(form)) if form.tab == NewAgentTab::Start)
}

/// Whether question `asked` of the agent's question `item` waits for its answer.
fn answer_waits(state: &AppState, agent: AgentId, item: ItemId, asked: usize) -> bool {
    state
        .agents
        .get(&agent)
        .and_then(|entry| actions::waiting_question(entry, item))
        .is_some_and(|question| asked < question.questions.len())
}

/// What became of the field `to` names.
pub fn field_state(to: Destination, state: &AppState, drafts: &Drafts) -> FieldState {
    let open = match to {
        Destination::NewAgent => {
            return if new_agent_prompt_shown(state) {
                FieldState::Open
            } else if state.ui.holds_new_agent_form() {
                FieldState::Closed
            } else {
                FieldState::Gone
            };
        }
        Destination::Message { agent }
        | Destination::Comment { agent, .. }
        | Destination::Answer { agent, .. }
            if !state.agents.contains_key(&agent) =>
        {
            return FieldState::Gone;
        }
        Destination::Message { .. } => true,
        Destination::Comment { agent, opening } => comment_open(drafts, agent, opening),
        Destination::Answer { agent, item, asked } => answer_waits(state, agent, item, asked),
    };
    match (open, state.ui.dialog.is_some()) {
        (false, _) => FieldState::Closed,
        (true, true) => FieldState::Covered,
        (true, false) => FieldState::Open,
    }
}

/// How the final text of `finished` goes in: at the caret of its field when the field shows, else
/// at the end of its text. A comment or an answer takes the text only while its field is open,
/// and never sends anything: once closed, it leaves the text at the end of the message. A message
/// under a dialog, or whose recording a dialog stopped, sends nothing either. The new agent
/// dialog's first message keeps the text while the dialog shows its other tab or "Accounts". A
/// field gone with its agent or its dialog takes nothing: the text can come in before the cancel.
pub fn landing(finished: Finished, state: &AppState, drafts: &Drafts) -> Option<Landing> {
    let to = finished.to;
    let field = field_state(to, state, drafts);
    let open = match field {
        FieldState::Open | FieldState::Covered => true,
        FieldState::Closed => false,
        FieldState::Gone => return None,
    };
    let covered = finished.covered || field == FieldState::Covered;
    let shown = to
        .agent()
        .is_some_and(|agent| state.ui.selected == Some(agent));
    let landing = match to {
        Destination::Message { agent } => match state.prefs.voice.after {
            AfterDictation::InsertAndSend if !covered => Landing::MessageEndAndSend(agent),
            AfterDictation::Insert | AfterDictation::InsertAndSend if shown => {
                Landing::MessageCaret
            }
            AfterDictation::Insert | AfterDictation::InsertAndSend => Landing::MessageEnd(agent),
        },
        Destination::Comment { agent, .. } => match (open, shown) {
            (true, true) => Landing::CommentCaret,
            (true, false) => Landing::CommentEnd,
            (false, _) => Landing::MessageEnd(agent),
        },
        Destination::Answer { agent, item, asked } => match (open, shown) {
            (true, true) => Landing::AnswerCaret { agent, item, asked },
            (true, false) => Landing::AnswerEnd { agent, item, asked },
            (false, _) => Landing::MessageEnd(agent),
        },
        Destination::NewAgent if open => Landing::NewAgentCaret,
        Destination::NewAgent => Landing::NewAgentEnd,
    };
    Some(landing)
}

/// Whether the comment field's `opening` is still open for `agent`.
fn comment_open(drafts: &Drafts, agent: AgentId, opening: Opening) -> bool {
    drafts
        .quote_editor
        .as_ref()
        .is_some_and(|editor| editor.agent == agent && editor.opening == opening)
}

impl VoiceState {
    /// The talk key went down. `target` is where a recording would write, as [`target`] gives:
    /// without one, the key only stops a recording.
    pub fn press(&mut self, target: Option<Destination>, now: Instant) -> WorkerOrder {
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
                to,
                since,
                latched: false,
            } => {
                if now.saturating_duration_since(since) < TAP {
                    self.phase = VoicePhase::Recording {
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
    pub fn click(&mut self, target: Option<Destination>, now: Instant) -> WorkerOrder {
        match (self.phase, target) {
            (VoicePhase::Idle, Some(target)) => self.start(target, now, /* latched */ true),
            (VoicePhase::Recording { .. }, _) => self.finish(),
            (VoicePhase::Idle, None) | (VoicePhase::Finishing { .. }, _) => WorkerOrder::Nothing,
        }
    }

    /// Enter: it stops the recording as the talk key's second tap does, whether tapped or held.
    /// The next Enter sends the message, adds the quote, answers or starts the agent.
    pub fn stop(&mut self) -> WorkerOrder {
        match self.phase {
            VoicePhase::Recording { .. } => self.finish(),
            VoicePhase::Idle | VoicePhase::Finishing { .. } => WorkerOrder::Nothing,
        }
    }

    /// The agents, the drafts or the dialog changed. A recording into a field that closed, gave
    /// way to another, or that a dialog covers, stops as Enter stops it. A dictation into an
    /// agent that is gone, closed or removed by its daemon, or into the new agent dialog once it
    /// closed, stops and drops its text, which has nowhere to go.
    pub fn fields_changed(&mut self, state: &AppState, drafts: &Drafts) -> WorkerOrder {
        let Some(to) = self.writing() else {
            return WorkerOrder::Nothing;
        };
        match field_state(to, state, drafts) {
            FieldState::Open => WorkerOrder::Nothing,
            FieldState::Covered => match self.phase {
                VoicePhase::Recording { to, .. } => {
                    self.phase = VoicePhase::Finishing { to, covered: true };
                    WorkerOrder::Stop
                }
                VoicePhase::Idle | VoicePhase::Finishing { .. } => WorkerOrder::Nothing,
            },
            FieldState::Closed => self.stop(),
            FieldState::Gone => self.cancel(),
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

    /// The worker's final text came: where it goes.
    pub fn done(&mut self) -> Option<Finished> {
        let finished = match self.phase {
            VoicePhase::Recording { to, .. } => Some(Finished { to, covered: false }),
            VoicePhase::Finishing { to, covered } => Some(Finished { to, covered }),
            VoicePhase::Idle => None,
        };
        self.phase = VoicePhase::Idle;
        self.live = LiveWords::default();
        finished
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
        self.download_failed = None;
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
                self.download_failed = Some(why.clone());
                self.card = Some(VoiceCard::Failed(why));
            }
        }
    }

    /// The language changed in Settings: the download that failed was the other model's.
    pub fn language_changed(&mut self) {
        self.download_failed = None;
    }

    /// Only the language chosen downloads its model, so only its row says why a download failed.
    pub fn model_row(&self, chosen: bool) -> ModelRowState {
        if chosen {
            return ModelRowState {
                status: self.model,
                failed: self.download_failed.clone(),
            };
        }
        let status = if self.other_model {
            ModelStatus::Ready
        } else {
            ModelStatus::Missing
        };
        ModelRowState {
            status,
            failed: None,
        }
    }

    /// The agent the text is for, while a recording runs or finishes.
    pub fn dictating(&self) -> Option<AgentId> {
        self.writing().and_then(Destination::agent)
    }

    /// Whether the dictation that runs or finishes writes into the comment field's `opening`.
    pub fn writes_comment(&self, opening: Opening) -> bool {
        matches!(
            self.writing(),
            Some(Destination::Comment { opening: into, .. }) if into == opening
        )
    }

    /// Where the dictation that runs or finishes writes.
    pub fn writing(&self) -> Option<Destination> {
        match self.phase {
            VoicePhase::Recording { to, .. } | VoicePhase::Finishing { to, .. } => Some(to),
            VoicePhase::Idle => None,
        }
    }

    /// Without the model, the card offers its download instead.
    fn start(&mut self, to: Destination, now: Instant, latched: bool) -> WorkerOrder {
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
            to,
            since: now,
            latched,
        };
        WorkerOrder::Start
    }

    fn finish(&mut self) -> WorkerOrder {
        let Some(to) = self.writing() else {
            return WorkerOrder::Nothing;
        };
        self.phase = VoicePhase::Finishing { to, covered: false };
        WorkerOrder::Stop
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{
        AnswerState, AskedQuestion, ItemBody, PendingId, Question, QuestionOption, Selection,
        TranscriptItem,
    };

    use super::*;
    use crate::state::{NewAgentForm, state_with, summary};

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

    fn into_message() -> Option<Destination> {
        Some(Destination::Message { agent: agent() })
    }

    /// A recording into `to` that no dialog stopped.
    fn finished(to: Destination) -> Finished {
        Finished { to, covered: false }
    }

    /// The app with agent 3 on screen, and its question 9 waiting, with two questions in it.
    fn asking() -> (AppState, ItemId) {
        let mut state = state_with(vec![summary(3, AgentStatus::Working)]);
        state.ui.selected = Some(agent());
        let item = ItemId::new(9);
        let asked = |text: &str| AskedQuestion {
            header: String::new(),
            text: text.to_owned(),
            options: vec![QuestionOption {
                label: "Keep".to_owned(),
                description: String::new(),
            }],
            selection: Selection::Single,
        };
        let question = Question {
            pending: PendingId::new("request-9".to_owned()),
            questions: vec![asked("The fixtures?"), asked("The snapshots?")],
            answer: AnswerState::Waiting,
        };
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.transcript.append(TranscriptItem {
                id: item,
                parent: None,
                body: ItemBody::Question(question),
            });
        }
        (state, item)
    }

    /// Answers the agent's question `item`, as the daemon's update does.
    fn answer(state: &mut AppState, item: ItemId) {
        let Some(entry) = state.agents.get_mut(&agent()) else {
            return;
        };
        let Some(mut changed) = entry.transcript.get(item).cloned() else {
            return;
        };
        if let ItemBody::Question(question) = &mut changed.body {
            question.answer = AnswerState::Cancelled;
        }
        entry.transcript.change(changed);
    }

    /// Opens the new agent dialog on its "Start in a folder" tab.
    fn new_agent_dialog(state: &mut AppState) {
        state.ui.dialog = Some(Dialog::NewAgent(NewAgentForm::default()));
    }

    /// Opens the comment field for `agent` on a passage, and returns its opening.
    fn open_comment(drafts: &mut Drafts, agent: AgentId) -> Opening {
        use crate::state::Utf16Offset;
        use crate::state::quotes::{self, Passage, QuoteAnchor};

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

    /// The app with agents 3 and 4, and agent 3 on screen.
    fn two_agents() -> AppState {
        let mut state = state_with(vec![
            summary(3, AgentStatus::Idle),
            summary(4, AgentStatus::Idle),
        ]);
        state.ui.selected = Some(agent());
        state
    }

    #[test]
    fn a_dictation_started_in_the_comment_writes_into_that_opening() {
        let mut state = two_agents();
        let (drafts, opening) = comment_open(agent());
        let comment = Destination::Comment {
            agent: agent(),
            opening,
        };

        assert_eq!(target(Field::Comment, &state, &drafts), Some(comment));
        assert_eq!(target(Field::Message, &state, &drafts), into_message());
        // The comment field of another agent, or none, leaves the message.
        assert_eq!(
            target(Field::Comment, &state, &Drafts::default()),
            into_message()
        );
        state.ui.selected = Some(AgentId::new(4));
        assert_eq!(
            target(Field::Comment, &state, &drafts),
            Some(Destination::Message {
                agent: AgentId::new(4)
            })
        );

        let mut voice = ready();
        voice.click(Some(comment), Instant::now());
        assert_eq!(voice.writing(), Some(comment));
        assert!(voice.writes_comment(opening));
        voice.stop();
        assert_eq!(
            voice.phase,
            VoicePhase::Finishing {
                to: comment,
                covered: false
            }
        );
        assert_eq!(voice.done(), Some(finished(comment)));
    }

    #[test]
    fn a_comment_takes_the_text_only_while_its_opening_stays_open() {
        use crate::state::quotes;

        let mut state = two_agents();
        let (mut drafts, opening) = comment_open(agent());
        let into_comment = Destination::Comment {
            agent: agent(),
            opening,
        };

        assert_eq!(
            landing(finished(into_comment), &state, &drafts),
            Some(Landing::CommentCaret)
        );
        state.ui.selected = Some(AgentId::new(4));
        assert_eq!(
            landing(finished(into_comment), &state, &drafts),
            Some(Landing::CommentEnd)
        );
        state.ui.selected = Some(agent());
        // Another field opened: the text goes at the end of the message.
        let mut reopened = drafts.clone();
        open_comment(&mut reopened, agent());
        assert_eq!(
            landing(finished(into_comment), &state, &reopened),
            Some(Landing::MessageEnd(agent()))
        );
        // The field closed.
        quotes::cancel(&mut drafts, Some(agent()));
        assert_eq!(
            landing(finished(into_comment), &state, &drafts),
            Some(Landing::MessageEnd(agent()))
        );
    }

    #[test]
    fn a_recording_into_a_comment_stops_once_the_comment_closes_or_gives_way() {
        use cockpit_protocol::PromptDelivery;

        use crate::state::quotes::{self, QuoteId};

        let mut state = state_with(vec![summary(3, AgentStatus::Idle)]);
        let recording_into = |opening| {
            let mut voice = ready();
            let to = Destination::Comment {
                agent: agent(),
                opening,
            };
            voice.click(Some(to), Instant::now());
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
        stops.push(voice.fields_changed(&state, &drafts) == WorkerOrder::Nothing);
        // Its ×.
        quotes::cancel(&mut drafts, Some(agent()));
        stops.push(voice.fields_changed(&state, &drafts) == WorkerOrder::Stop);
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
        stops.push(voice.fields_changed(&state, &drafts) == WorkerOrder::Stop);
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
        stops.push(voice.fields_changed(&state, &drafts) == WorkerOrder::Stop);
        // Removing the quote whose comment is open.
        let mut voice = recording_into(drafts.quote_editor.as_ref().unwrap().opening);
        if let Some(quote) = quote {
            quotes::remove(&mut drafts, agent(), quote);
        }
        stops.push(voice.fields_changed(&state, &drafts) == WorkerOrder::Stop);
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
        stops.push(voice.fields_changed(&state, &drafts) == WorkerOrder::Stop);
        // A recording into the message, or one already finishing, goes on as it is.
        let mut voice = ready();
        voice.click(into_message(), Instant::now());
        stops.push(voice.fields_changed(&state, &Drafts::default()) == WorkerOrder::Nothing);
        voice.stop();
        stops.push(voice.fields_changed(&state, &Drafts::default()) == WorkerOrder::Nothing);

        assert_eq!(stops, [true; 9]);
    }

    #[test]
    fn insert_and_send_never_sends_from_a_comment_or_an_answer() {
        let (mut state, item) = asking();
        state.prefs.voice.after = AfterDictation::InsertAndSend;
        let (drafts, opening) = comment_open(agent());
        let into_comment = Destination::Comment {
            agent: agent(),
            opening,
        };
        let into_answer = Destination::Answer {
            agent: agent(),
            item,
            asked: 0,
        };
        let into_message = Destination::Message { agent: agent() };

        assert_eq!(
            landing(finished(into_comment), &state, &drafts),
            Some(Landing::CommentCaret)
        );
        assert_eq!(
            landing(finished(into_comment), &state, &Drafts::default()),
            Some(Landing::MessageEnd(agent()))
        );
        assert_eq!(
            landing(finished(into_answer), &state, &drafts),
            Some(Landing::AnswerCaret {
                agent: agent(),
                item,
                asked: 0
            })
        );
        assert_eq!(
            landing(finished(into_message), &state, &drafts),
            Some(Landing::MessageEndAndSend(agent()))
        );
        state.prefs.voice.after = AfterDictation::Insert;
        assert_eq!(
            landing(finished(into_message), &state, &drafts),
            Some(Landing::MessageCaret)
        );
        state.ui.selected = None;
        assert_eq!(
            landing(finished(into_message), &state, &drafts),
            Some(Landing::MessageEnd(agent()))
        );
    }

    #[test]
    fn a_dictation_started_in_an_answer_writes_into_it_while_the_question_waits() {
        let (mut state, item) = asking();
        let drafts = Drafts::default();
        let second = Destination::Answer {
            agent: agent(),
            item,
            asked: 1,
        };

        assert_eq!(
            target(Field::Answer { item, asked: 1 }, &state, &drafts),
            Some(second)
        );
        // A question the card does not ask, or a card that asks nothing, leaves the message.
        assert_eq!(
            target(Field::Answer { item, asked: 2 }, &state, &drafts),
            into_message()
        );
        assert_eq!(
            target(
                Field::Answer {
                    item: ItemId::new(8),
                    asked: 0
                },
                &state,
                &drafts
            ),
            into_message()
        );
        let mut voice = ready();
        voice.click(Some(second), Instant::now());
        assert_eq!(voice.dictating(), Some(agent()));
        assert_eq!(voice.fields_changed(&state, &drafts), WorkerOrder::Nothing);
        // Out of sight, the text goes at the end of the answer.
        state.ui.selected = None;
        assert_eq!(
            landing(finished(second), &state, &drafts),
            Some(Landing::AnswerEnd {
                agent: agent(),
                item,
                asked: 1
            })
        );
        // Answered or withdrawn: the recording stops, and its text goes at the end of the
        // message.
        answer(&mut state, item);
        assert_eq!(voice.fields_changed(&state, &drafts), WorkerOrder::Stop);
        assert_eq!(
            voice.phase,
            VoicePhase::Finishing {
                to: second,
                covered: false
            }
        );
        assert_eq!(
            landing(finished(second), &state, &drafts),
            Some(Landing::MessageEnd(agent()))
        );
        assert_eq!(
            target(Field::Answer { item, asked: 1 }, &state, &drafts),
            None,
            "no agent on screen"
        );
    }

    #[test]
    fn a_dictation_into_the_new_agent_dialog_needs_no_agent_and_ends_with_the_dialog() {
        let mut state = state_with(Vec::new());
        let drafts = Drafts::default();

        assert_eq!(target(Field::NewAgent, &state, &drafts), None);
        new_agent_dialog(&mut state);
        assert_eq!(
            target(Field::NewAgent, &state, &drafts),
            Some(Destination::NewAgent)
        );
        assert_eq!(
            landing(finished(Destination::NewAgent), &state, &drafts),
            Some(Landing::NewAgentCaret)
        );
        let mut voice = ready();
        voice.click(Some(Destination::NewAgent), Instant::now());
        assert_eq!(voice.dictating(), None);
        assert_eq!(voice.fields_changed(&state, &drafts), WorkerOrder::Nothing);
        // The other tab hides the first message: the recording stops, and the text goes at its
        // end.
        if let Some(form) = state.ui.new_agent_form_mut() {
            form.tab = NewAgentTab::Resume;
        }
        assert_eq!(voice.fields_changed(&state, &drafts), WorkerOrder::Stop);
        assert_eq!(
            landing(finished(Destination::NewAgent), &state, &drafts),
            Some(Landing::NewAgentEnd)
        );
        assert_eq!(target(Field::NewAgent, &state, &drafts), None);
        // The dialog closed: the next opening starts from an empty message, so the text goes.
        state.ui.dialog = None;
        assert_eq!(voice.fields_changed(&state, &drafts), WorkerOrder::Cancel);
        assert_eq!(voice.phase, VoicePhase::Idle);
    }

    #[test]
    fn the_text_of_a_field_gone_goes_nowhere() {
        let (mut state, item) = asking();
        let (drafts, opening) = comment_open(agent());
        state.agents.remove(&agent());
        let gone = [
            Destination::Message { agent: agent() },
            Destination::Comment {
                agent: agent(),
                opening,
            },
            Destination::Answer {
                agent: agent(),
                item,
                asked: 0,
            },
            Destination::NewAgent,
        ];

        let landed = gone.map(|to| landing(finished(to), &state, &drafts));

        assert_eq!(landed, [None; 4]);
    }

    #[test]
    fn a_dialog_over_the_chat_stops_a_dictation_whose_text_stays_in_its_field_unsent() {
        let (mut state, item) = asking();
        state.prefs.voice.after = AfterDictation::InsertAndSend;
        let (drafts, opening) = comment_open(agent());
        let into_message = Destination::Message { agent: agent() };
        let into_comment = Destination::Comment {
            agent: agent(),
            opening,
        };
        let into_answer = Destination::Answer {
            agent: agent(),
            item,
            asked: 0,
        };
        let mut stops = Vec::new();
        for to in [into_message, into_comment, into_answer] {
            state.ui.dialog = None;
            let mut voice = ready();
            voice.click(Some(to), Instant::now());
            stops.push(voice.fields_changed(&state, &drafts) == WorkerOrder::Nothing);
            new_agent_dialog(&mut state);
            stops.push(voice.fields_changed(&state, &drafts) == WorkerOrder::Stop);
        }

        assert_eq!(stops, [true; 6]);
        assert_eq!(
            [into_message, into_comment, into_answer].map(|to| landing(
                finished(to),
                &state,
                &drafts
            )),
            [
                Some(Landing::MessageCaret),
                Some(Landing::CommentCaret),
                Some(Landing::AnswerCaret {
                    agent: agent(),
                    item,
                    asked: 0
                }),
            ],
            "the text stays in its field, and the message is not sent"
        );
    }

    #[test]
    fn a_message_a_dialog_stopped_stays_unsent_when_the_dialog_closes_before_its_words() {
        let mut state = two_agents();
        state.prefs.voice.after = AfterDictation::InsertAndSend;
        let drafts = Drafts::default();
        let mut voice = ready();
        voice.click(into_message(), Instant::now());
        new_agent_dialog(&mut state);
        voice.fields_changed(&state, &drafts);

        state.ui.dialog = None;
        let landed = voice.done().and_then(|done| landing(done, &state, &drafts));

        assert_eq!(landed, Some(Landing::MessageCaret));
    }

    #[test]
    fn accounts_opened_from_the_new_agent_dialog_stop_its_dictation_and_keep_the_text() {
        use crate::state::accounts;

        let mut state = state_with(Vec::new());
        let drafts = Drafts::default();
        new_agent_dialog(&mut state);
        let mut voice = ready();
        voice.click(Some(Destination::NewAgent), Instant::now());

        accounts::open_accounts(&mut state, None);

        assert_eq!(voice.fields_changed(&state, &drafts), WorkerOrder::Stop);
        assert_eq!(
            landing(finished(Destination::NewAgent), &state, &drafts),
            Some(Landing::NewAgentEnd)
        );
    }

    #[test]
    fn leaving_the_window_stops_a_held_recording_however_short() {
        let mut voice = ready();
        let now = Instant::now();
        voice.press(into_message(), now);

        assert_eq!(voice.blur(), WorkerOrder::Stop);
        assert_eq!(
            voice.phase,
            VoicePhase::Finishing {
                to: Destination::Message { agent: agent() },
                covered: false
            }
        );
        assert_eq!(voice.release(now), WorkerOrder::Nothing);
    }

    #[test]
    fn leaving_the_window_keeps_a_tapped_recording() {
        let mut voice = ready();
        let now = Instant::now();
        voice.press(into_message(), now);
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

        voice.press(into_message(), now);
        voice.release(now + Duration::from_millis(120));
        assert_eq!(voice.stop(), WorkerOrder::Stop);
        assert!(matches!(voice.phase, VoicePhase::Finishing { .. }));
        assert_eq!(voice.stop(), WorkerOrder::Nothing);

        voice.done();
        voice.press(into_message(), now);
        assert_eq!(voice.stop(), WorkerOrder::Stop);
        assert_eq!(
            voice.release(now + Duration::from_secs(2)),
            WorkerOrder::Nothing
        );
    }

    #[test]
    fn an_exited_agent_takes_no_dictation() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Exited),
        ]);
        state.ui.selected = Some(AgentId::new(1));
        let running = target(Field::Message, &state, &Drafts::default());
        state.ui.selected = Some(AgentId::new(2));
        let exited = target(Field::Message, &state, &Drafts::default());
        let mut voice = ready();
        let now = Instant::now();

        assert_eq!(running.and_then(Destination::agent), Some(AgentId::new(1)));
        assert_eq!(exited, None);
        assert_eq!(voice.press(exited, now), WorkerOrder::Nothing);
        assert_eq!(voice.click(exited, now), WorkerOrder::Nothing);
        assert_eq!(voice.phase, VoicePhase::Idle);
        // A recording that runs still stops from there.
        voice.click(running, now);
        assert_eq!(voice.press(exited, now), WorkerOrder::Stop);
    }

    #[test]
    fn a_failed_download_says_why_in_settings_until_the_next_one() {
        let mut voice = VoiceState::default();
        voice.download_started(DownloadFrom::Settings, 10);
        voice.download_ended(Err("no network".to_owned()));
        let failed = (voice.model, voice.download_failed.clone());
        voice.download_started(DownloadFrom::Settings, 10);
        let again = voice.download_failed.clone();
        voice.download_ended(Ok(()));

        assert_eq!(
            failed,
            (ModelStatus::Missing, Some("no network".to_owned()))
        );
        assert_eq!(again, None);
        assert_eq!(
            (voice.model, voice.download_failed),
            (ModelStatus::Ready, None)
        );
    }

    #[test]
    fn only_the_chosen_languages_row_says_why_its_download_failed() {
        let mut voice = VoiceState::default();
        voice.download_started(DownloadFrom::Settings, 10);
        voice.download_ended(Err("no network".to_owned()));
        let (chosen, other) = (true, false);

        assert_eq!(
            voice.model_row(chosen),
            ModelRowState {
                status: ModelStatus::Missing,
                failed: Some("no network".to_owned()),
            }
        );
        assert_eq!(
            voice.model_row(other),
            ModelRowState {
                status: ModelStatus::Missing,
                failed: None,
            }
        );
    }

    #[test]
    fn a_change_of_language_takes_the_failed_download_out_of_settings_but_not_the_card() {
        let mut voice = VoiceState::default();
        voice.download_started(DownloadFrom::Settings, 10);
        voice.download_ended(Err("no network".to_owned()));
        voice.language_changed();

        assert_eq!(
            voice.model_row(/* chosen */ true),
            ModelRowState {
                status: ModelStatus::Missing,
                failed: None,
            }
        );
        assert_eq!(voice.card, Some(VoiceCard::Failed("no network".to_owned())));
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
        assert_eq!(voice.press(into_message(), now), WorkerOrder::Start);
        assert_eq!(
            voice.release(now + Duration::from_secs(2)),
            WorkerOrder::Stop
        );
        assert_eq!(
            voice.phase,
            VoicePhase::Finishing {
                to: Destination::Message { agent: agent() },
                covered: false
            }
        );
        assert_eq!(voice.done(), into_message().map(finished));
        assert_eq!(voice.phase, VoicePhase::Idle);
    }

    #[test]
    fn a_tapped_key_records_until_the_next_press() {
        let mut voice = ready();
        let now = Instant::now();
        assert_eq!(voice.press(into_message(), now), WorkerOrder::Start);
        assert_eq!(
            voice.release(now + Duration::from_millis(120)),
            WorkerOrder::Nothing
        );
        assert!(matches!(
            voice.phase,
            VoicePhase::Recording { latched: true, .. }
        ));
        assert_eq!(
            voice.press(into_message(), now + Duration::from_secs(5)),
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
        assert_eq!(voice.click(into_message(), now), WorkerOrder::Start);
        assert_eq!(voice.click(into_message(), now), WorkerOrder::Stop);
        assert_eq!(voice.click(into_message(), now), WorkerOrder::Nothing);
    }

    #[test]
    fn without_the_model_the_card_offers_it() {
        let mut voice = VoiceState {
            available: true,
            ..VoiceState::default()
        };
        assert_eq!(
            voice.press(into_message(), Instant::now()),
            WorkerOrder::Nothing
        );
        assert_eq!(voice.card, Some(VoiceCard::Model));
        assert_eq!(voice.phase, VoicePhase::Idle);
    }

    #[test]
    fn without_dictation_nothing_starts() {
        let mut voice = VoiceState {
            model: ModelStatus::Ready,
            ..VoiceState::default()
        };
        assert_eq!(
            voice.click(into_message(), Instant::now()),
            WorkerOrder::Nothing
        );
        assert_eq!(voice.card, None);
    }

    #[test]
    fn esc_drops_the_recording_and_its_text() {
        let mut voice = ready();
        assert_eq!(voice.cancel(), WorkerOrder::Nothing);
        voice.press(into_message(), Instant::now());
        voice.live.said = "Run just check".to_owned();
        assert_eq!(voice.cancel(), WorkerOrder::Cancel);
        assert_eq!(voice.phase, VoicePhase::Idle);
        assert_eq!(voice.live, LiveWords::default());
    }

    #[test]
    fn a_failure_shows_on_the_card() {
        let mut voice = ready();
        voice.press(into_message(), Instant::now());
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
        let mut state = two_agents();
        let mut drafts = Drafts::default();
        let mut voice = ready();
        voice.press(into_message(), Instant::now());
        actions::close_agent(&mut state, &mut drafts, AgentId::new(4));
        let other_closed = voice.fields_changed(&state, &drafts);
        actions::close_agent(&mut state, &mut drafts, agent());
        let closed = voice.fields_changed(&state, &drafts);

        assert_eq!(other_closed, WorkerOrder::Nothing);
        assert_eq!(closed, WorkerOrder::Cancel);
        assert_eq!(voice.phase, VoicePhase::Idle);
        assert_eq!(voice.fields_changed(&state, &drafts), WorkerOrder::Nothing);
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
        let to = Destination::Comment {
            agent: agent(),
            opening,
        };
        voice.click(Some(to), Instant::now());
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

        assert_eq!(voice.fields_changed(&state, &drafts), WorkerOrder::Cancel);
        assert_eq!(voice.phase, VoicePhase::Idle);
        assert_eq!(voice.done(), None);
    }

    #[test]
    fn a_fallback_shows_on_the_card_while_the_dictation_goes_on_and_after() {
        let mut voice = ready();
        voice.press(into_message(), Instant::now());
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
