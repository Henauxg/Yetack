//! Dictation's task, in a build with the `voice` feature: it owns the worker and the models'
//! download.

use std::fmt::Display;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cockpit_protocol::{AgentId, PromptDelivery};
use cockpit_voice::{Event, LiveRefresh, Voice, VoiceError};
use dioxus::prelude::*;
use tokio::sync::mpsc::{self, UnboundedReceiver as Receiver};

use crate::bridge::{self, COMPOSER_ID};
use crate::connection::Cockpit;
use crate::prefs::{AfterDictation, LiveTextRefresh};
use crate::state::voice::{
    DownloadFrom, LiveWords, ModelStatus, VoiceCard, VoiceState, WorkerOrder, dictation_target,
};
use crate::state::{Drafts, actions};
use crate::voice_desk::VoiceCommand;
use crate::voice_models::{self, DownloadNews};

/// Tries to delete the models this many times, while the worker lets go of them.
const REMOVE_TRIES: u32 = 20;
const REMOVE_WAIT: Duration = Duration::from_millis(100);

pub fn initial_state() -> VoiceState {
    let folder = voice_models::folder();
    let model = if folder.as_deref().is_some_and(voice_models::present) {
        ModelStatus::Ready
    } else {
        ModelStatus::Missing
    };
    VoiceState {
        available: cockpit_voice::supported() && folder.is_some(),
        model,
        ..VoiceState::default()
    }
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
}

pub async fn run(
    cockpit: Cockpit,
    state: Signal<VoiceState>,
    level: Signal<f32>,
    mut commands: UnboundedReceiver<VoiceCommand>,
) {
    use futures_util::StreamExt;

    let (events, mut heard) = mpsc::unbounded_channel();
    let mut desk = Desk {
        cockpit,
        state,
        level,
        folder: voice_models::folder(),
        voice: None,
        recording: 0,
        events,
        news: None,
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
        let target = dictation_target(&self.cockpit.state.peek());
        let now = Instant::now();
        let order = match command {
            VoiceCommand::Press => self.state.write().press(target, now),
            VoiceCommand::Click => self.state.write().click(target, now),
            VoiceCommand::Release => self.state.write().release(now),
            VoiceCommand::Blur => self.state.write().blur(),
            VoiceCommand::Cancel => self.state.write().cancel(),
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
                self.remove_model().await;
                WorkerOrder::Nothing
            }
            VoiceCommand::CloseCard => {
                self.state.write().card = None;
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
                let refresh = match self.cockpit.state.peek().prefs.voice.live {
                    LiveTextRefresh::Auto => LiveRefresh::Auto,
                    LiveTextRefresh::EverySecond => LiveRefresh::EverySecond,
                    LiveTextRefresh::Off => LiveRefresh::Off,
                };
                let started = voice.start(refresh, move |event| {
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
            Event::Finishing | Event::Cancelled => {}
            Event::Done(text) => {
                let agent = self.state.write().done();
                if let Some(agent) = agent {
                    insert(self.cockpit, agent, &text);
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

    fn start_download(&mut self, from: DownloadFrom) {
        if self.news.is_some() {
            return;
        }
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let (news, heard) = mpsc::unbounded_channel();
        self.news = Some(heard);
        self.state
            .write()
            .download_started(from, voice_models::total_size());
        tokio::task::spawn_blocking(move || {
            let result = voice_models::download(&folder, &news);
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
    async fn remove_model(&mut self) {
        self.state.write().cancel();
        self.voice = None;
        let Some(folder) = self.folder.clone() else {
            return;
        };
        let removed = tokio::task::spawn_blocking(move || remove_when_free(&folder)).await;
        let mut state = self.state.write();
        match removed {
            Ok(Ok(())) => state.model = ModelStatus::Missing,
            Ok(Err(error)) => {
                state.card = Some(VoiceCard::Failed(format!(
                    "Could not delete the speech model: {error}"
                )));
            }
            Err(error) => state.card = Some(VoiceCard::Failed(error.to_string())),
        }
    }
}

fn remove_when_free(folder: &Path) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match voice_models::remove(folder) {
            Err(_) if tries < REMOVE_TRIES => {
                tries += 1;
                std::thread::sleep(REMOVE_WAIT);
            }
            result => return result,
        }
    }
}

/// The text goes in the message of the agent it was dictated to: at the cursor when that
/// message shows, at its end otherwise, or at its end and sent.
fn insert(cockpit: Cockpit, agent: AgentId, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let (after, shown) = {
        let state = cockpit.state.peek();
        (state.prefs.voice.after, state.ui.selected == Some(agent))
    };
    match after {
        AfterDictation::Insert if shown => bridge::insert_at_caret(COMPOSER_ID, text),
        AfterDictation::Insert => {
            cockpit.with_drafts(|_, drafts| append(drafts, agent, text));
        }
        AfterDictation::InsertAndSend => cockpit.change_with_drafts(|state, drafts| {
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
