//! Dictation in the window: the talk key and the mic button start and stop recordings, the
//! worker's events update [`VoiceState`], and the final text goes into the message or a comment.
//!
//! The state and the level are signals of their own: the level changes 30 times a second, and
//! only the meter reads it.

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::prefs::DictationLanguage;
use crate::state::voice::{DownloadFrom, Field, VoiceState};

/// What the window asks of dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCommand {
    /// The talk key went down, in the field it names.
    Press(Field),
    /// The talk key came up.
    Release,
    /// The window lost the focus with the talk key down.
    Blur,
    /// The mic button of the message or of the comment field.
    Click(Field),
    /// Esc, or dictation turned off.
    Cancel,
    /// Enter in the field the dictation goes to.
    Stop,
    /// The comment field closed, or another one opened.
    CommentChanged,
    /// The agent dictated to is gone.
    AgentGone,
    Download(DownloadFrom),
    CancelDownload,
    /// Removes the model of the language chosen.
    RemoveModel,
    /// Removes the model of the other language.
    RemoveOtherModel,
    /// The language changed in Settings, and with it the model.
    LanguageChanged,
    CloseCard,
    /// Settings' Voice tab opened: it lists the microphones.
    ListMicrophones,
}

#[derive(Clone, Copy)]
pub struct VoiceDesk {
    pub state: Signal<VoiceState>,
    /// The microphone's level while it records, from 0 to 1.
    pub level: Signal<f32>,
    commands: Coroutine<VoiceCommand>,
}

impl VoiceDesk {
    pub fn send(&self, command: VoiceCommand) {
        self.commands.send(command);
    }
}

/// Starts dictation's task, and provides the [`VoiceDesk`] to every component.
pub fn use_voice(cockpit: Cockpit) -> VoiceDesk {
    let state = use_signal(move || initial_state(cockpit.state.peek().prefs.voice.language));
    let level = use_signal(|| 0.0_f32);
    let commands = use_coroutine(move |commands: UnboundedReceiver<VoiceCommand>| {
        run(cockpit, state, level, commands)
    });
    // A comment that closes, or gives way to another, stops the recording into it.
    let comment = use_memo(move || {
        cockpit
            .drafts
            .read()
            .quote_editor
            .as_ref()
            .map(|editor| editor.opening)
    });
    use_effect(move || {
        comment();
        commands.send(VoiceCommand::CommentChanged);
    });
    // A dictation into an agent that closes, or that its daemon removes, stops and drops its text.
    let gone = use_memo(move || {
        let dictating = state.read().dictating()?;
        (!cockpit.state.read().agents.contains_key(&dictating)).then_some(dictating)
    });
    use_effect(move || {
        if gone().is_some() {
            commands.send(VoiceCommand::AgentGone);
        }
    });
    use_context_provider(|| VoiceDesk {
        state,
        level,
        commands,
    })
}

#[cfg(feature = "voice")]
fn initial_state(language: DictationLanguage) -> VoiceState {
    crate::voice_engine::initial_state(language)
}

/// This build has no dictation: the window shows none.
#[cfg(not(feature = "voice"))]
fn initial_state(_: DictationLanguage) -> VoiceState {
    VoiceState::default()
}

#[cfg(feature = "voice")]
async fn run(
    cockpit: Cockpit,
    state: Signal<VoiceState>,
    level: Signal<f32>,
    commands: UnboundedReceiver<VoiceCommand>,
) {
    crate::voice_engine::run(cockpit, state, level, commands).await;
}

#[cfg(not(feature = "voice"))]
async fn run(
    _: Cockpit,
    _: Signal<VoiceState>,
    _: Signal<f32>,
    mut commands: UnboundedReceiver<VoiceCommand>,
) {
    use futures_util::StreamExt;

    while commands.next().await.is_some() {}
}
