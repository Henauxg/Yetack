//! Dictation: records the microphone and turns speech into text on this computer.
//!
//! [`Voice`] owns a worker thread, which loads the speech models on the first recording and
//! drops them after a while unused. [`Voice::start`] records until [`Voice::stop`] or
//! [`Voice::cancel`], and reports the level and the text as [`Event`]s.
//!
//! The speech is cut at its pauses. Each finished segment is transcribed once, while the user
//! goes on, so only the last one is left when the recording stops. Silero checks each segment and
//! skips the ones without speech, and the text drops filler words (um, uh). The investigation
//! behind these choices is in docs/voice/investigation.md.

mod catalog;
mod fillers;
mod level_control;
mod live_text;
mod microphone;
mod models;
mod pauses;
mod resampler;
mod utterance;
mod voice;
mod voice_error;

pub use catalog::{MODEL_FILES, ModelFile};
pub use live_text::LiveText;
pub use microphone::{Microphones, microphones};
pub use voice::{Event, LiveRefresh, Missing, Voice, supported};
pub use voice_error::VoiceError;

/// The rate of the audio the models take.
const SAMPLE_RATE: u32 = 16_000;

/// Samples in `ms` milliseconds of audio at [`SAMPLE_RATE`].
const fn samples(ms: u32) -> usize {
    (SAMPLE_RATE / 1000 * ms) as usize
}
