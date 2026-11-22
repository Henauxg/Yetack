use std::path::{Path, PathBuf};

use transcribe_cpp::{
    Backend, Model, ModelOptions, RunOptions, Session, SessionOptions, VadOptions, VadSession,
};

use crate::catalog::{SILERO, SpeechModel};
use crate::utterance::{SpeechCheck, Transcribe};
use crate::voice_error::VoiceError;

/// The speech models, loaded. The weights are memory-mapped from their files.
pub(crate) struct Models {
    /// The model `speech` runs.
    pub(crate) model: SpeechModel,
    pub(crate) speech: Speech,
    pub(crate) detector: SpeechDetector,
}

/// The model that writes the text.
pub(crate) struct Speech {
    session: Session,
}

/// Silero, which tells speech from other sounds.
pub(crate) struct SpeechDetector {
    session: VadSession,
}

impl Models {
    pub(crate) fn load(
        folder: &Path,
        model: SpeechModel,
        threads: usize,
    ) -> Result<Self, VoiceError> {
        transcribe_cpp::disable_logging();
        let parakeet = Model::load_with(
            present(folder, model.file().name)?,
            &ModelOptions {
                backend: Backend::Cpu,
                ..Default::default()
            },
        )?;
        let session = parakeet.session_with(&SessionOptions {
            n_threads: i32::try_from(threads).unwrap_or(4),
            ..Default::default()
        })?;
        let silero = Model::load(present(folder, SILERO.name)?)?;
        Ok(Self {
            model,
            speech: Speech { session },
            detector: SpeechDetector {
                session: silero.vad_session()?,
            },
        })
    }
}

fn present(folder: &Path, name: &str) -> Result<PathBuf, VoiceError> {
    let path = folder.join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(VoiceError::MissingModel(path))
    }
}

impl Transcribe for Speech {
    fn transcribe(&mut self, pcm: &[f32]) -> Result<String, VoiceError> {
        // Parakeet Ultra ignores a language hint, so a word can still come out in another one.
        // Parakeet unified EN knows only English.
        Ok(self.session.run(pcm, &RunOptions::default())?.text)
    }
}

impl SpeechCheck for SpeechDetector {
    fn has_speech(&mut self, pcm: &[f32]) -> Result<bool, VoiceError> {
        Ok(!self
            .session
            .run(pcm, &VadOptions::default())?
            .segments
            .is_empty())
    }
}
