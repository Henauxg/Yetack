//! Dictation's task, in a build with the `voice` feature: it owns the worker and the models'
//! download.

use std::fmt::Display;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cockpit_protocol::{AgentId, ItemId, PromptDelivery};
use cockpit_voice::{
    Event, LiveRefresh, Microphones, Missing, RecordingOptions, SpeechModel, Voice, VoiceError,
};
use dioxus::prelude::*;
use tokio::sync::mpsc::{self, UnboundedReceiver as Receiver};

use crate::bridge::{self, COMPOSER_ID, Focus, NEW_AGENT_PROMPT_ID};
use crate::connection::Cockpit;
use crate::prefs::{DictationLanguage, LiveTextRefresh};
use crate::state::voice::{
    self, Destination, DownloadFrom, Field, Finished, Landing, LiveWords, MicMissing,
    MicrophoneList, ModelStatus, VoiceCard, VoiceState, WorkerOrder,
};
use crate::state::{AppState, Drafts, actions};
use crate::ui::{COMMENT_FIELD_ID, answer_field_id};
use crate::voice_desk::VoiceCommand;
use crate::voice_models::{self, DownloadNews};

/// Tries to delete the models this many times, while the worker lets go of them.
const REMOVE_TRIES: u32 = 20;
const REMOVE_WAIT: Duration = Duration::from_millis(100);

pub fn initial_state(language: DictationLanguage) -> VoiceState {
    let folder = voice_models::folder();
    let (model, other_model) = on_disk(folder.as_deref(), voice_models::speech_model(language));
    VoiceState {
        available: cockpit_voice::supported() && folder.is_some(),
        model,
        other_model,
        ..VoiceState::default()
    }
}

/// Whether `model` is in `folder`, and whether the other model is too.
fn on_disk(folder: Option<&Path>, model: SpeechModel) -> (ModelStatus, bool) {
    let present = |model| folder.is_some_and(|folder| voice_models::present(folder, model));
    let status = if present(model) {
        ModelStatus::Ready
    } else {
        ModelStatus::Missing
    };
    (status, present(voice_models::other(model)))
}

/// The task's own state.
struct Desk {
    cockpit: Cockpit,
    state: Signal<VoiceState>,
    level: Signal<f32>,
    folder: Option<PathBuf>,
    voice: Option<Voice>,
    /// The current recording's number: the events of an earlier one are dropped.
    recording: u32,
    events: mpsc::UnboundedSender<(u32, Event)>,
    /// The running download's news. Dropping it cancels the download.
    news: Option<Receiver<DownloadNews>>,
    /// Where the microphones go once listed.
    listed: mpsc::UnboundedSender<Result<Microphones, VoiceError>>,
}

pub async fn run(
    cockpit: Cockpit,
    state: Signal<VoiceState>,
    level: Signal<f32>,
    mut commands: UnboundedReceiver<VoiceCommand>,
) {
    use futures_util::StreamExt;

    let (events, mut heard) = mpsc::unbounded_channel();
    let (listed, mut lists) = mpsc::unbounded_channel();
    let mut desk = Desk {
        cockpit,
        state,
        level,
        folder: voice_models::folder(),
        voice: None,
        recording: 0,
        events,
        news: None,
        listed,
    };
    loop {
        tokio::select! {
            command = commands.next() => {
                let Some(command) = command else {
                    return;
                };
                desk.command(command).await;
            }
            Some((recording, event)) = heard.recv() => {
                if recording == desk.recording {
                    desk.event(event);
                }
            }
            Some(news) = next_news(&mut desk.news) => desk.download_news(news),
            Some(list) = lists.recv() => desk.listed(list),
        }
    }
}

async fn next_news(news: &mut Option<Receiver<DownloadNews>>) -> Option<DownloadNews> {
    match news {
        Some(news) => news.recv().await,
        None => std::future::pending().await,
    }
}

impl Desk {
    async fn command(&mut self, command: VoiceCommand) {
        let now = Instant::now();
        let order = match command {
            VoiceCommand::Press(field) => {
                let target = self.target(field);
                self.state.write().press(target, now)
            }
            VoiceCommand::Click(field) => {
                let target = self.target(field);
                self.state.write().click(target, now)
            }
            VoiceCommand::Release => self.state.write().release(now),
            VoiceCommand::Blur => self.state.write().blur(),
            VoiceCommand::Cancel => self.state.write().cancel(),
            VoiceCommand::Stop => self.state.write().stop(),
            VoiceCommand::FieldChanged => self
                .state
                .write()
                .fields_changed(&self.cockpit.state.peek(), &self.cockpit.drafts.peek()),
            VoiceCommand::Download(from) => {
                self.start_download(from);
                WorkerOrder::Nothing
            }
            // A click on Cancel can come in after the download's end.
            VoiceCommand::CancelDownload => {
                if self.news.take().is_some() {
                    self.state.write().model = ModelStatus::Missing;
                }
                WorkerOrder::Nothing
            }
            VoiceCommand::RemoveModel => {
                self.remove_model(self.chosen()).await;
                WorkerOrder::Nothing
            }
            VoiceCommand::RemoveOtherModel => {
                self.remove_model(voice_models::other(self.chosen())).await;
                WorkerOrder::Nothing
            }
            // A download of the other model stops: its progress would show in the place of
            // this model's.
            VoiceCommand::LanguageChanged => {
                self.news = None;
                self.state.write().language_changed();
                self.look_at_disk();
                WorkerOrder::Nothing
            }
            VoiceCommand::CloseCard => {
                self.state.write().card = None;
                WorkerOrder::Nothing
            }
            VoiceCommand::ListMicrophones => {
                let listed = self.listed.clone();
                tokio::task::spawn_blocking(move || {
                    // The window may have closed meanwhile.
                    let _ = listed.send(cockpit_voice::microphones());
                });
                WorkerOrder::Nothing
            }
        };
        if let Err(error) = self.order(order) {
            // The next recording starts another worker.
            if matches!(error, VoiceError::WorkerGone) {
                self.voice = None;
            }
            self.state.write().failed(sentence(&error));
        }
    }

    /// Where a press or a click in `field` would write.
    fn target(&self, field: Field) -> Option<Destination> {
        voice::target(
            field,
            &self.cockpit.state.peek(),
            &self.cockpit.drafts.peek(),
        )
    }

    fn order(&mut self, order: WorkerOrder) -> Result<(), VoiceError> {
        match order {
            WorkerOrder::Start => {
                self.recording = self.recording.wrapping_add(1);
                self.level.set(0.0);
                let folder = self.folder.clone().ok_or(VoiceError::WorkerGone)?;
                let voice = match self.voice.take() {
                    Some(voice) => voice,
                    None => Voice::spawn(folder)?,
                };
                let events = self.events.clone();
                let recording = self.recording;
                let options = {
                    let state = self.cockpit.state.peek();
                    let refresh = match state.prefs.voice.live {
                        LiveTextRefresh::Auto => LiveRefresh::Auto,
                        LiveTextRefresh::EverySecond => LiveRefresh::EverySecond,
                        LiveTextRefresh::Off => LiveRefresh::Off,
                    };
                    RecordingOptions {
                        refresh,
                        microphone: state.prefs.voice.microphone.clone(),
                        model: voice_models::speech_model(state.prefs.voice.language),
                    }
                };
                let started = voice.start(options, move |event| {
                    // The task ends with the window: the last events have no one to go to.
                    let _ = events.send((recording, event));
                });
                self.voice = Some(voice);
                started
            }
            WorkerOrder::Stop => self.voice.as_ref().map_or(Ok(()), Voice::stop),
            WorkerOrder::Cancel => {
                self.recording = self.recording.wrapping_add(1);
                self.voice.as_ref().map_or(Ok(()), Voice::cancel)
            }
            WorkerOrder::Nothing => Ok(()),
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Level(level) => self.level.set(level),
            Event::Live(text) => {
                self.state.write().live = LiveWords {
                    said: text.said,
                    hearing: text.hearing,
                };
            }
            Event::Fallback {
                chosen,
                using,
                missing,
            } => {
                let missing = match missing {
                    Missing::AtStart => MicMissing::AtStart,
                    Missing::MidWay => MicMissing::MidWay,
                };
                self.state.write().fell_back(chosen, using, missing);
            }
            Event::Finishing | Event::Cancelled => {}
            Event::Done(text) => {
                let finished = self.state.write().done();
                if let Some(finished) = finished {
                    insert(self.cockpit, finished, &text);
                }
            }
            Event::Failed(error) => {
                let mut state = self.state.write();
                if matches!(error, VoiceError::MissingModel(_)) {
                    state.model = ModelStatus::Missing;
                }
                state.failed(sentence(&error));
            }
        }
    }

    fn listed(&mut self, list: Result<Microphones, VoiceError>) {
        match list {
            Ok(Microphones { default, names }) => {
                self.state.write().microphones = Some(MicrophoneList { default, names });
            }
            Err(error) => tracing::warn!("cannot list the microphones: {error}"),
        }
    }

    /// The model of the language chosen in Settings.
    fn chosen(&self) -> SpeechModel {
        voice_models::speech_model(self.cockpit.state.peek().prefs.voice.language)
    }

    /// Reads again which models are on this computer.
    fn look_at_disk(&mut self) {
        let (model, other_model) = on_disk(self.folder.as_deref(), self.chosen());
        let mut state = self.state.write();
        // A running download says how far the model is.
        if self.news.is_none() {
            state.model = model;
        }
        state.other_model = other_model;
    }

    fn start_download(&mut self, from: DownloadFrom) {
        if self.news.is_some() {
            return;
        }
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let model = self.chosen();
        let (news, heard) = mpsc::unbounded_channel();
        self.news = Some(heard);
        self.state
            .write()
            .download_started(from, voice_models::total_size(model));
        tokio::task::spawn_blocking(move || {
            let result = voice_models::download(&folder, model, &news);
            // A cancelled download has no one to tell.
            let _ = news.send(DownloadNews::Done(result));
        });
    }

    fn download_news(&mut self, news: DownloadNews) {
        match news {
            DownloadNews::Progress(done) => self.state.write().download_progress(done),
            DownloadNews::Done(result) => {
                self.news = None;
                let chosen = self.chosen();
                download_finished(
                    &mut self.state.write(),
                    self.folder.as_deref(),
                    chosen,
                    result,
                );
            }
        }
    }

    /// Ends the worker first: on Windows, a file it maps cannot be deleted.
    async fn remove_model(&mut self, model: SpeechModel) {
        self.state.write().cancel();
        self.voice = None;
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let removed = tokio::task::spawn_blocking(move || remove_when_free(&folder, model)).await;
        self.look_at_disk();
        let mut state = self.state.write();
        match removed {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                state.card = Some(VoiceCard::Failed(format!(
                    "Could not delete the speech model: {error}"
                )));
            }
            Err(error) => state.card = Some(VoiceCard::Failed(error.to_string())),
        }
    }
}

/// The download of `chosen`, the model of the language chosen, ended. The Silero it brought in
/// may complete the other model.
fn download_finished(
    state: &mut VoiceState,
    folder: Option<&Path>,
    chosen: SpeechModel,
    result: Result<(), String>,
) {
    state.download_ended(result);
    let (_, other_model) = on_disk(folder, chosen);
    state.other_model = other_model;
}

fn remove_when_free(folder: &Path, model: SpeechModel) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match voice_models::remove(folder, model) {
            Err(_) if tries < REMOVE_TRIES => {
                tries += 1;
                std::thread::sleep(REMOVE_WAIT);
            }
            result => return result,
        }
    }
}

/// The text goes in the field it was dictated into, as [`voice::landing`] decides: at the cursor
/// when that field shows, at its end otherwise, or at the message's end and sent. A field gone
/// with its agent or its dialog drops it.
fn insert(cockpit: Cockpit, finished: Finished, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let Some(landing) = voice::landing(finished, &cockpit.state.peek(), &cockpit.drafts.peek())
    else {
        return;
    };
    // A field under a dialog takes the text and leaves the dialog the focus.
    let focus = if cockpit.state.peek().ui.dialog.is_none() {
        Focus::Take
    } else {
        Focus::Leave
    };
    match landing {
        Landing::MessageCaret => bridge::insert_at_caret(COMPOSER_ID, text, focus),
        Landing::CommentCaret => bridge::insert_at_caret(COMMENT_FIELD_ID, text, focus),
        Landing::AnswerCaret { agent, item, asked } => {
            bridge::insert_at_caret(&answer_field_id(agent, item, asked), text, focus);
        }
        Landing::NewAgentCaret => bridge::insert_at_caret(NEW_AGENT_PROMPT_ID, text, Focus::Take),
        Landing::MessageEnd(agent) => cockpit.with_drafts(|_, drafts| append(drafts, agent, text)),
        Landing::CommentEnd => cockpit.with_drafts(|_, drafts| append_to_comment(drafts, text)),
        Landing::AnswerEnd { agent, item, asked } => cockpit.with_drafts(|state, drafts| {
            append_to_answer(state, drafts, agent, item, asked, text);
        }),
        Landing::NewAgentEnd => cockpit.with_drafts(|_, drafts| {
            append_to(&mut drafts.new_agent.prompt, text);
        }),
        Landing::MessageEndAndSend(agent) => cockpit.change_with_drafts(|state, drafts| {
            append(drafts, agent, text);
            actions::send_prompt(state, drafts, agent, None, PromptDelivery::Steer)
        }),
    }
}

/// Adds `text` at the end of `field`, a space apart from a word.
fn append_to(field: &mut String, text: &str) {
    if !field.is_empty() && !field.ends_with(char::is_whitespace) {
        field.push(' ');
    }
    field.push_str(text);
}

fn append(drafts: &mut Drafts, agent: AgentId, text: &str) {
    append_to(&mut drafts.draft_mut(agent).text, text);
    drafts.draft_written(agent);
}

/// Adds `text` at the end of the open comment, whose field shows it when it opens again.
fn append_to_comment(drafts: &mut Drafts, text: &str) {
    if let Some(editor) = &mut drafts.quote_editor {
        append_to(&mut editor.text, text);
    }
}

/// Adds `text` at the end of the "Other" answer to question `asked` of the agent's question
/// `item`, as typing does: on a single-choice question it replaces the pick. The card shows it
/// when the agent comes on screen.
fn append_to_answer(
    state: &AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    item: ItemId,
    asked: usize,
    text: &str,
) {
    let Some(question) = state
        .agents
        .get(&agent)
        .and_then(|entry| actions::waiting_question(entry, item))
    else {
        return;
    };
    let mut other = drafts
        .questions
        .get(&question.pending)
        .and_then(|draft| draft.other.get(asked))
        .cloned()
        .unwrap_or_default();
    append_to(&mut other, text);
    actions::set_other_answer(drafts, question, asked, other);
}

/// The error as the card shows it: a sentence.
fn sentence(error: &impl Display) -> String {
    let text = error.to_string();
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return text;
    };
    let rest = chars.as_str();
    let stop = if rest.ends_with(['.', '!', '?']) {
        ""
    } else {
        "."
    };
    format!("{}{rest}{stop}", first.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_goes_at_the_end_of_the_draft_a_space_apart() {
        let agent = AgentId::new(2);
        let mut drafts = Drafts::default();
        drafts.draft_mut(agent).text = "Run the tests".to_owned();

        append(&mut drafts, agent, "and commit");
        let once = drafts.composer_writes(agent);
        drafts.draft_mut(agent).text.push('\n');
        append(&mut drafts, agent, "Then push.");
        append(&mut drafts, AgentId::new(3), "Hello");

        assert_eq!(
            drafts.draft(agent).map(|draft| draft.text.as_str()),
            Some("Run the tests and commit\nThen push.")
        );
        assert_eq!(drafts.composer_writes(agent), once + 1);
        assert_eq!(
            drafts
                .draft(AgentId::new(3))
                .map(|draft| draft.text.as_str()),
            Some("Hello")
        );
    }

    #[test]
    fn a_comment_out_of_sight_takes_the_text_at_its_end() {
        use crate::state::Utf16Offset;
        use crate::state::quotes::{self, Passage, QuoteAnchor};
        use cockpit_protocol::ItemId;

        let agent = AgentId::new(2);
        let mut drafts = Drafts::default();
        append_to_comment(&mut drafts, "Nothing is open");
        let passage = Passage {
            anchor: QuoteAnchor {
                item: ItemId::new(1),
                start: Utf16Offset::new(0),
                end: Utf16Offset::new(5),
            },
            raw: "lexer".to_owned(),
            text: "lexer".to_owned(),
            code: false,
        };
        quotes::start(&mut drafts, agent, passage);
        quotes::type_comment(&mut drafts, "Count from 1".to_owned());

        append_to_comment(&mut drafts, "in the lexer.");

        assert_eq!(
            drafts
                .quote_editor
                .as_ref()
                .map(|editor| editor.text.as_str()),
            Some("Count from 1 in the lexer.")
        );
        assert_eq!(drafts.draft(agent), None);
    }

    #[test]
    fn an_answer_out_of_sight_takes_the_text_at_its_end_in_place_of_the_pick() {
        use cockpit_protocol::{
            AgentStatus, AnswerState, AskedQuestion, ItemBody, PendingId, Question, QuestionOption,
            Selection, TranscriptItem,
        };

        use crate::state::{state_with, summary};

        let agent = AgentId::new(2);
        let item = ItemId::new(9);
        let mut state = state_with(vec![summary(2, AgentStatus::NeedsYou)]);
        let question = Question {
            pending: PendingId::new("request-9".to_owned()),
            questions: vec![AskedQuestion {
                header: String::new(),
                text: "The fixtures?".to_owned(),
                options: vec![QuestionOption {
                    label: "Keep".to_owned(),
                    description: String::new(),
                }],
                selection: Selection::Single,
            }],
            answer: AnswerState::Waiting,
        };
        if let Some(entry) = state.agents.get_mut(&agent) {
            entry.transcript.append(TranscriptItem {
                id: item,
                parent: None,
                body: ItemBody::Question(question.clone()),
            });
        }
        let mut drafts = Drafts::default();
        actions::pick_option(&mut drafts, &question, 0, 0);

        append_to_answer(&state, &mut drafts, agent, item, 0, "Move them");
        append_to_answer(&state, &mut drafts, agent, item, 0, "to tests/data.");
        // A question that does not wait takes nothing.
        append_to_answer(&state, &mut drafts, agent, ItemId::new(8), 0, "Lost");

        let draft = drafts.questions.get(&question.pending);
        assert_eq!(
            draft
                .and_then(|draft| draft.other.first())
                .map(String::as_str),
            Some("Move them to tests/data.")
        );
        assert_eq!(
            draft
                .and_then(|draft| draft.picks.first())
                .map(|picks| picks.len()),
            Some(0),
            "the text replaces the pick of a single-choice question"
        );
    }

    #[test]
    fn a_download_that_brings_silero_in_shows_the_other_model_whole() {
        use crate::state::voice::DownloadFrom;

        let folder = tempfile::tempdir().unwrap();
        let mut state = VoiceState::default();
        state.download_started(DownloadFrom::Settings, 10);
        // Parakeet Ultra lacks only Silero, which the English download brings in.
        voice_models::tests::whole_files(folder.path(), SpeechModel::Ultra);
        voice_models::tests::whole_files(folder.path(), SpeechModel::UnifiedEn);

        download_finished(
            &mut state,
            Some(folder.path()),
            SpeechModel::UnifiedEn,
            Ok(()),
        );

        assert_eq!((state.model, state.other_model), (ModelStatus::Ready, true));
    }

    #[test]
    fn an_error_reads_as_a_sentence() {
        assert_eq!(
            sentence(&VoiceError::NoMicrophone),
            "This computer has no microphone."
        );
        assert_eq!(
            sentence(&"the microphone stopped: the device is gone."),
            "The microphone stopped: the device is gone."
        );
    }
}
