use std::path::PathBuf;

use cpal::SampleFormat;

/// Why dictation could not go on.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("this computer has no microphone")]
    NoMicrophone,
    #[error("the microphone: {0}")]
    Microphone(#[from] cpal::Error),
    #[error("the microphone stopped: {0}")]
    MicrophoneStopped(cpal::Error),
    #[error("the microphone's sample format, {0:?}, is not supported")]
    SampleFormat(SampleFormat),
    #[error("the speech model {} is missing", .0.display())]
    MissingModel(PathBuf),
    #[error("the speech model: {0}")]
    Model(#[from] transcribe_cpp::Error),
    #[error("dictation stopped working")]
    WorkerGone,
}
