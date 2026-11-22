//! Dictation's task, in a build with the `voice` feature: it owns the worker and the models'
//! download.

use std::fmt::Display;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cockpit_protocol::{AgentId, PromptDelivery};
use cockpit_voice::{
    Event, LiveRefresh, Microphones, Missing, RecordingOptions, SpeechModel, Voice, VoiceError,
};
use dioxus::prelude::*;
use tokio::sync::mpsc::{self, UnboundedReceiver as Receiver};

use crate::bridge::{self, COMPOSER_ID};
use crate::connection::Cockpit;
use crate::prefs::{DictationLanguage, LiveTextRefresh};
use crate::state::voice::{
    self, Dictation, DownloadFrom, Field, Landing, LiveWords, MicMissing, MicrophoneList,
    ModelStatus, VoiceCard, VoiceState, WorkerOrder, dictation_target,
};
use crate::state::{Drafts, actions};
use crate::ui::COMMENT_FIELD_ID;
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
            VoiceCommand::AgentGone => self
                .state
                .write()
                .agents_changed(&self.cockpit.state.peek()),
            VoiceCommand::CommentChanged => self
                .state
                .write()
                .comment_changed(&self.cockpit.drafts.peek()),
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

    /// The dictation a press or a click in `field` would start.
    fn target(&self, field: Field) -> Option<Dictation> {
        let agent = dictation_target(&self.cockpit.state.peek())?;
        let to = voice::destination(field, agent, &self.cockpit.drafts.peek());
        Some(Dictation { agent, to })
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
                let dictation = self.state.write().done();
                if let Some(dictation) = dictation {
                    insert(self.cockpit, dictation, &text);
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
                self.state.write().download_ended(result);
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

/// The text goes in the message or the comment of the agent it was dictated to, as
/// [`voice::landing`] decides: at the cursor when that field shows, at its end otherwise, or at
/// the message's end and sent.
fn insert(cockpit: Cockpit, dictation: Dictation, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let agent = dictation.agent;
    let landing = {
        let state = cockpit.state.peek();
        let shown = state.ui.selected == Some(agent);
        voice::landing(
            dictation,
            shown,
            state.prefs.voice.after,
            &cockpit.drafts.peek(),
        )
    };
    match landing {
        Landing::MessageCaret => bridge::insert_at_caret(COMPOSER_ID, text),
        Landing::CommentCaret => bridge::insert_at_caret(COMMENT_FIELD_ID, text),
        Landing::MessageEnd => cockpit.with_drafts(|_, drafts| append(drafts, agent, text)),
        Landing::CommentEnd => cockpit.with_drafts(|_, drafts| append_to_comment(drafts, text)),
        Landing::MessageEndAndSend => cockpit.change_with_drafts(|state, drafts| {
            append(drafts, agent, text);
            actions::send_prompt(state, drafts, agent, None, PromptDelivery::Steer)
        }),
    }
}

fn append(drafts: &mut Drafts, agent: AgentId, text: &str) {
    let draft = drafts.draft_mut(agent);
    if !draft.text.is_empty() && !draft.text.ends_with(char::is_whitespace) {
        draft.text.push(' ');
    }
    draft.text.push_str(text);
    drafts.draft_written(agent);
}

/// Adds `text` at the end of the open comment, whose field shows it when it opens again.
fn append_to_comment(drafts: &mut Drafts, text: &str) {
    if let Some(editor) = &mut drafts.quote_editor {
        if !editor.text.is_empty() && !editor.text.ends_with(char::is_whitespace) {
            editor.text.push(' ');
        }
        editor.text.push_str(text);
    }
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
