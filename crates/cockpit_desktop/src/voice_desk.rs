//! Dictation in the window: the talk key and the mic button start and stop recordings, the
//! worker's events update [`VoiceState`], and the final text goes into the message.
//!
//! The state and the level are signals of their own: the level changes 30 times a second, and
//! only the meter reads it.

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::state::voice::{DownloadFrom, VoiceState};

/// What the window asks of dictation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCommand {
    /// The talk key went down.
    Press,
    /// The talk key came up.
    Release,
    /// The window lost the focus with the talk key down.
    Blur,
    /// The mic button.
    Click,
    /// Esc, or dictation turned off.
    Cancel,
    Download(DownloadFrom),
    CancelDownload,
    RemoveModel,
    CloseCard,
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
    let state = use_signal(initial_state);
    let level = use_signal(|| 0.0_f32);
    let commands = use_coroutine(move |commands: UnboundedReceiver<VoiceCommand>| {
        run(cockpit, state, level, commands)
    });
    use_context_provider(|| VoiceDesk {
        state,
        level,
        commands,
    })
}

#[cfg(feature = "voice")]
fn initial_state() -> VoiceState {
    crate::voice_engine::initial_state()
}

/// This build has no dictation: the window shows none.
#[cfg(not(feature = "voice"))]
fn initial_state() -> VoiceState {
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
