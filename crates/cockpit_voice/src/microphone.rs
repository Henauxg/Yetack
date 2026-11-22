use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{
    Device, ErrorKind, FromSample, I24, InputCallbackInfo, SampleFormat, SizedSample, Stream,
    SupportedStreamConfig, U24,
};

use crate::voice_error::VoiceError;

/// How often the level goes out.
const LEVEL_EVERY: Duration = Duration::from_millis(33);
/// A recording opens its microphone again at most this many times, so that a stream that fails
/// as soon as it opens cannot loop.
const MAX_REOPENS: u32 = 8;

/// Where the microphone's level goes, from its audio thread.
pub(crate) type Level = Arc<dyn Fn(f32) + Send + Sync>;

/// The microphones of this computer by name, as a choice in Settings lists them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Microphones {
    /// The system default's name, when there is one.
    pub default: Option<String>,
    /// Each microphone's name once, in the system's order.
    pub names: Vec<String>,
}

/// Lists the microphones. It records nothing, but it can take a moment on Linux.
pub fn microphones() -> Result<Microphones, VoiceError> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|device| name(&device));
    let mut names: Vec<String> = Vec::new();
    for device in host.input_devices()? {
        if let Some(name) = name(&device)
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    Ok(Microphones { default, names })
}

fn name(device: &Device) -> Option<String> {
    device
        .description()
        .ok()
        .map(|description| description.name().to_owned())
}

/// Audio from the microphone, mono at its own rate.
pub(crate) struct Block {
    /// When the audio thread got it.
    pub(crate) heard: Instant,
    pub(crate) pcm: Vec<f32>,
}

/// A microphone, recording while this lives. Its audio goes out as [`Block`]s. Its level goes
/// out from the audio thread itself, so it keeps moving while the worker transcribes.
pub(crate) struct Microphone {
    _stream: Stream,
    rate: u32,
    lost: Receiver<cpal::Error>,
    /// The name of the device it records, when it has one.
    name: Option<String>,
    /// The microphone chosen was missing, and this is the system default instead.
    instead_of_chosen: bool,
}

impl Microphone {
    /// Opens the microphone named `chosen` when it is connected, the system default otherwise.
    pub(crate) fn open(
        chosen: Option<&str>,
        audio: Sender<Block>,
        level: Level,
    ) -> Result<Self, VoiceError> {
        use cpal::traits::StreamTrait;

        let host = cpal::default_host();
        let found = match chosen {
            Some(chosen) => host
                .input_devices()?
                .find(|device| name(device).as_deref() == Some(chosen)),
            None => None,
        };
        let instead_of_chosen = chosen.is_some() && found.is_none();
        let device = match found {
            Some(device) => device,
            None => host
                .default_input_device()
                .ok_or(VoiceError::NoMicrophone)?,
        };
        let config = device.default_input_config()?;
        let (lose, lost) = mpsc::channel();
        let stream = match config.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, audio, level, lose)?,
            SampleFormat::F64 => build::<f64>(&device, &config, audio, level, lose)?,
            SampleFormat::I8 => build::<i8>(&device, &config, audio, level, lose)?,
            SampleFormat::I16 => build::<i16>(&device, &config, audio, level, lose)?,
            SampleFormat::I24 => build::<I24>(&device, &config, audio, level, lose)?,
            SampleFormat::I32 => build::<i32>(&device, &config, audio, level, lose)?,
            SampleFormat::I64 => build::<i64>(&device, &config, audio, level, lose)?,
            SampleFormat::U8 => build::<u8>(&device, &config, audio, level, lose)?,
            SampleFormat::U16 => build::<u16>(&device, &config, audio, level, lose)?,
            SampleFormat::U24 => build::<U24>(&device, &config, audio, level, lose)?,
            SampleFormat::U32 => build::<u32>(&device, &config, audio, level, lose)?,
            SampleFormat::U64 => build::<u64>(&device, &config, audio, level, lose)?,
            other @ (SampleFormat::DsdU8 | SampleFormat::DsdU16 | SampleFormat::DsdU32 | _) => {
                return Err(VoiceError::SampleFormat(other));
            }
        };
        stream.play()?;
        Ok(Self {
            _stream: stream,
            rate: config.sample_rate(),
            lost,
            name: name(&device),
            instead_of_chosen,
        })
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate
    }

    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Whether this is the system default, opened because the microphone chosen was missing.
    pub(crate) fn instead_of_chosen(&self) -> bool {
        self.instead_of_chosen
    }

    /// Why the microphone stopped sending audio, once it has.
    pub(crate) fn lost(&self) -> Option<cpal::Error> {
        self.lost.try_recv().ok()
    }
}

fn build<T>(
    device: &Device,
    config: &SupportedStreamConfig,
    audio: Sender<Block>,
    level: Level,
    lose: Sender<cpal::Error>,
) -> Result<Stream, VoiceError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    use cpal::Sample;

    let channels = usize::from(config.channels()).max(1);
    let mut energy = 0.0f32;
    let mut count = 0usize;
    let mut last = Instant::now();
    let stream = device.build_input_stream(
        (*config).into(),
        move |data: &[T], _: &InputCallbackInfo| {
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|frame| {
                    frame
                        .iter()
                        .map(|sample| f32::from_sample(*sample))
                        .sum::<f32>()
                        / frame.len() as f32
                })
                .collect();
            energy += mono.iter().map(|sample| sample * sample).sum::<f32>();
            count += mono.len();
            if count > 0 && last.elapsed() >= LEVEL_EVERY {
                level((energy / count as f32).sqrt());
                (energy, count, last) = (0.0, 0, Instant::now());
            }
            // The worker drops its receiver when the recording ends: the last blocks have no
            // one to go to.
            let _ = audio.send(Block {
                heard: Instant::now(),
                pcm: mono,
            });
        },
        move |error| {
            tracing::warn!("microphone: {error}");
            if stops_the_stream(&error) {
                // A recording that has ended has no one to tell.
                let _ = lose.send(error);
            }
        },
        None,
    )?;
    Ok(stream)
}

/// Whether the stream sends no more audio after `error`. cpal keeps it going after a glitch, a
/// move to the new default device, or a refused real-time priority.
fn stops_the_stream(error: &cpal::Error) -> bool {
    match error.kind() {
        ErrorKind::Xrun | ErrorKind::DeviceChanged | ErrorKind::RealtimeDenied => false,
        ErrorKind::DeviceBusy
        | ErrorKind::DeviceNotAvailable
        | ErrorKind::HostUnavailable
        | ErrorKind::InvalidInput
        | ErrorKind::PermissionDenied
        | ErrorKind::ResourceExhausted
        | ErrorKind::StreamInvalidated
        | ErrorKind::UnsupportedConfig
        | ErrorKind::UnsupportedOperation
        | ErrorKind::BackendError
        | ErrorKind::Other
        | _ => true,
    }
}

/// Whether a recording whose microphone stopped with `error`, after `reopens` reopenings, opens
/// a microphone again rather than end.
pub(crate) fn reopens_after(error: &cpal::Error, reopens: u32) -> bool {
    if reopens >= MAX_REOPENS {
        return false;
    }
    match error.kind() {
        // On Windows a new default microphone invalidates the stream. A device that went away
        // may leave another one as the default.
        ErrorKind::StreamInvalidated | ErrorKind::DeviceNotAvailable => true,
        ErrorKind::DeviceBusy
        | ErrorKind::DeviceChanged
        | ErrorKind::HostUnavailable
        | ErrorKind::InvalidInput
        | ErrorKind::PermissionDenied
        | ErrorKind::RealtimeDenied
        | ErrorKind::ResourceExhausted
        | ErrorKind::UnsupportedConfig
        | ErrorKind::UnsupportedOperation
        | ErrorKind::Xrun
        | ErrorKind::BackendError
        | ErrorKind::Other
        | _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_glitch_goes_on_and_a_lost_device_stops() {
        assert!(!stops_the_stream(&ErrorKind::Xrun.into()));
        assert!(!stops_the_stream(&ErrorKind::DeviceChanged.into()));
        assert!(stops_the_stream(&ErrorKind::DeviceNotAvailable.into()));
        assert!(stops_the_stream(&ErrorKind::BackendError.into()));
    }

    #[test]
    fn a_new_or_lost_device_opens_a_microphone_again_and_a_refusal_ends_the_recording() {
        let reopens = |kind: ErrorKind| reopens_after(&kind.into(), 0);

        assert!(reopens(ErrorKind::StreamInvalidated));
        assert!(reopens(ErrorKind::DeviceNotAvailable));
        assert!(!reopens(ErrorKind::PermissionDenied));
        assert!(!reopens(ErrorKind::DeviceBusy));
        assert!(!reopens(ErrorKind::HostUnavailable));
        assert!(!reopens(ErrorKind::BackendError));
    }

    #[test]
    fn a_microphone_that_keeps_failing_ends_the_recording() {
        let invalidated: cpal::Error = ErrorKind::StreamInvalidated.into();

        assert!(reopens_after(&invalidated, MAX_REOPENS - 1));
        assert!(!reopens_after(&invalidated, MAX_REOPENS));
    }
}
