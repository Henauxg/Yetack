use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use cpal::traits::DeviceTrait;
use cpal::{
    Device, ErrorKind, FromSample, I24, InputCallbackInfo, SampleFormat, SizedSample, Stream,
    SupportedStreamConfig, U24,
};

use crate::voice_error::VoiceError;

/// How often the level goes out.
const LEVEL_EVERY: Duration = Duration::from_millis(33);

/// Audio from the microphone, mono at its own rate.
pub(crate) struct Block {
    /// When the audio thread got it.
    pub(crate) heard: Instant,
    pub(crate) pcm: Vec<f32>,
}

/// The default microphone, recording while this lives. Its audio goes out as [`Block`]s. Its
/// level goes out from the audio thread itself, so it keeps moving while the worker transcribes.
pub(crate) struct Microphone {
    _stream: Stream,
    rate: u32,
    lost: Receiver<cpal::Error>,
}

impl Microphone {
    pub(crate) fn open(
        audio: Sender<Block>,
        level: impl Fn(f32) + Send + 'static,
    ) -> Result<Self, VoiceError> {
        use cpal::traits::{HostTrait, StreamTrait};

        let device = cpal::default_host()
            .default_input_device()
            .ok_or(VoiceError::NoMicrophone)?;
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
        })
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate
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
    level: impl Fn(f32) + Send + 'static,
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
}
