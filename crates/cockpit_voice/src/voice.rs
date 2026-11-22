use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::SAMPLE_RATE;
use crate::catalog::SpeechModel;
use crate::level_control::LevelControl;
use crate::live_text::LiveText;
use crate::microphone::{self, Block, Level, Microphone};
use crate::models::Models;
use crate::resampler::Resampler;
use crate::utterance::Utterance;
use crate::voice_error::VoiceError;

/// How often the live text comes up to date.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LiveRefresh {
    /// As often as the computer allows: every half second on a fast one, every second or two on a
    /// slower one.
    #[default]
    Auto,
    EverySecond,
    /// Only at each pause.
    Off,
}

/// What a recording asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecordingOptions {
    pub refresh: LiveRefresh,
    /// The microphone by its name. `None`, or a name not connected, records from the system
    /// default.
    pub microphone: Option<String>,
    pub model: SpeechModel,
}

/// When the microphone chosen for a recording went missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// It was not connected when the recording started.
    AtStart,
    /// It was disconnected during the recording.
    MidWay,
}

/// What a recording reports, in this order: levels and live text while it records, then
/// `Finishing` once stopped, then `Done`. A microphone that stops ends the recording the same way,
/// and `Failed` follows its `Done`, unless another microphone takes over. Otherwise `Cancelled` or
/// `Failed` can come at any point.
#[derive(Debug)]
pub enum Event {
    /// The microphone's level: the RMS of the last few dozen milliseconds, from 0 to 1.
    Level(f32),
    Live(LiveText),
    /// The microphone chosen is missing, so the recording uses the system default, `using`. It
    /// comes once in a recording.
    Fallback {
        chosen: String,
        using: Option<String>,
        missing: Missing,
    },
    /// The recording is stopping, and the last words are on their way.
    Finishing,
    /// The final text, maybe empty.
    Done(String),
    Cancelled,
    Failed(VoiceError),
}

/// Whether this computer can run the speech model. On x86_64 the build takes Haswell's
/// instructions: AVX2, FMA, F16C and BMI2 (.cargo/config.toml), plus BMI1, LZCNT, MOVBE and
/// POPCNT, which clang's `/arch:AVX2` adds. Intel's CPUs have them since 2013 and AMD's since 2015.
pub fn supported() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("fma")
            && std::arch::is_x86_feature_detected!("f16c")
            && std::arch::is_x86_feature_detected!("bmi1")
            && std::arch::is_x86_feature_detected!("bmi2")
            && std::arch::is_x86_feature_detected!("lzcnt")
            && std::arch::is_x86_feature_detected!("movbe")
            && std::arch::is_x86_feature_detected!("popcnt")
    }
    #[cfg(target_arch = "aarch64")]
    {
        true
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// The models stay loaded this long after a recording, so the next one starts at once.
const KEEP_LOADED: Duration = Duration::from_secs(300);
/// The recording goes on this long after the stop, so its last syllable is not cut.
const TAIL: Duration = Duration::from_millis(200);
/// How often the worker takes the microphone's audio.
const TICK: Duration = Duration::from_millis(30);
/// The worker's threads for the speech model. More adds little and takes cores from the agents'
/// builds.
const MAX_THREADS: usize = 4;

/// Both the worker and the microphone's audio thread report events.
type Events = Arc<dyn Fn(Event) + Send + Sync>;

enum Command {
    Start {
        options: RecordingOptions,
        events: Events,
    },
    /// The user stopped at this instant: the worker may hear of it later.
    Stop(Instant),
    Cancel,
}

/// Dictation's worker thread. One recording runs at a time. Dropping this ends the worker once
/// its recording is over.
pub struct Voice {
    commands: Sender<Command>,
}

impl Voice {
    /// Starts the worker, which reads the models from `models` at the first recording.
    pub fn spawn(models: PathBuf) -> Result<Self, VoiceError> {
        let (commands, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("voice".to_owned())
            .spawn(move || work(&models, &receiver))
            .map_err(|_| VoiceError::WorkerGone)?;
        Ok(Self { commands })
    }

    /// Starts recording, unless a recording runs. Its events go to `events`, from the worker and
    /// from the audio thread.
    pub fn start(
        &self,
        options: RecordingOptions,
        events: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<(), VoiceError> {
        self.send(Command::Start {
            options,
            events: Arc::new(events),
        })
    }

    /// Stops recording: the last words, then the final text.
    pub fn stop(&self) -> Result<(), VoiceError> {
        self.send(Command::Stop(Instant::now()))
    }

    /// Stops recording and drops its text.
    pub fn cancel(&self) -> Result<(), VoiceError> {
        self.send(Command::Cancel)
    }

    fn send(&self, command: Command) -> Result<(), VoiceError> {
        self.commands
            .send(command)
            .map_err(|_| VoiceError::WorkerGone)
    }
}

fn work(folder: &Path, commands: &Receiver<Command>) {
    let mut models: Option<Models> = None;
    loop {
        let command = match commands.recv_timeout(KEEP_LOADED) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => {
                models = None;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let Command::Start { options, events } = command else {
            continue;
        };
        let recording = Recording {
            refresh: options.refresh,
            microphone: options.microphone.as_deref(),
            model: options.model,
            events: &events,
        };
        if let Err(error) = record(folder, &mut models, &recording, commands) {
            events(Event::Failed(error));
        }
    }
}

/// What a recording asked for.
struct Recording<'a> {
    refresh: LiveRefresh,
    /// The microphone chosen, by name.
    microphone: Option<&'a str>,
    model: SpeechModel,
    events: &'a Events,
}

/// A microphone and the resampling of its audio. Each microphone has its own, since its rate may
/// differ from the last one's.
struct Input {
    microphone: Microphone,
    blocks: Receiver<Block>,
    resampler: Resampler,
}

impl Input {
    fn open(chosen: Option<&str>, level: &Level) -> Result<Self, VoiceError> {
        let (audio, blocks) = mpsc::channel();
        let microphone = Microphone::open(chosen, audio, Arc::clone(level))?;
        let resampler = Resampler::new(microphone.rate(), SAMPLE_RATE);
        Ok(Self {
            microphone,
            blocks,
            resampler,
        })
    }

    /// The audio heard so far, at the models' rate.
    fn take(&mut self) -> Vec<f32> {
        take(&self.blocks, None, &mut self.resampler)
    }

    /// Stops the microphone, and gives its last audio, without what it heard after `end`.
    fn close(self, end: Option<Instant>) -> Vec<f32> {
        let Self {
            microphone,
            blocks,
            mut resampler,
        } = self;
        drop(microphone);
        let mut pcm = take(&blocks, end, &mut resampler);
        resampler.flush(&mut pcm);
        pcm
    }

    /// Tells that the recording uses the system default, when the microphone chosen is missing.
    /// Returns whether it told.
    fn tell_fallback(&self, recording: &Recording<'_>, missing: Missing) -> bool {
        let Some(chosen) = recording
            .microphone
            .filter(|_| self.microphone.instead_of_chosen())
        else {
            return false;
        };
        (recording.events)(Event::Fallback {
            chosen: chosen.to_owned(),
            using: self.microphone.name().map(str::to_owned),
            missing,
        });
        true
    }
}

/// One recording, from the microphone's opening to the final text. The models load after the
/// microphone opens, while its audio waits in the channel, so the first word is kept. A
/// microphone that stops gives way to the one [`Microphone::open`] finds then, as when the user
/// plugs in a headset, and the recording goes on with the same text and level control.
fn record(
    folder: &Path,
    models: &mut Option<Models>,
    recording: &Recording<'_>,
    commands: &Receiver<Command>,
) -> Result<(), VoiceError> {
    let events = recording.events;
    let level: Level = {
        let events = Arc::clone(events);
        Arc::new(move |level| events(Event::Level(level)))
    };
    let mut input = Input::open(recording.microphone, &level)?;
    let mut told = input.tell_fallback(recording, Missing::AtStart);
    // Another model than the one loaded takes its place.
    if models
        .as_ref()
        .is_some_and(|loaded| loaded.model != recording.model)
    {
        *models = None;
    }
    let models = match models {
        Some(models) => models,
        unloaded @ None => {
            let threads = thread::available_parallelism().map_or(MAX_THREADS, |n| n.get());
            unloaded.insert(Models::load(
                folder,
                recording.model,
                threads.min(MAX_THREADS),
            )?)
        }
    };
    let mut control = LevelControl::new();
    let mut utterance = Utterance::new(&mut models.speech, &mut models.detector, recording.refresh);
    let mut reopens = 0;
    let (end, stopped) = loop {
        match commands.recv_timeout(TICK) {
            Ok(Command::Stop(at)) => break (at + TAIL, input.microphone.lost()),
            Ok(Command::Cancel) | Err(RecvTimeoutError::Disconnected) => {
                events(Event::Cancelled);
                return Ok(());
            }
            Ok(Command::Start { .. }) | Err(RecvTimeoutError::Timeout) => {}
        }
        let mut pcm = match input.microphone.lost() {
            None => input.take(),
            Some(error) if !microphone::reopens_after(&error, reopens) => {
                break (Instant::now(), Some(error));
            }
            Some(error) => match Input::open(recording.microphone, &level) {
                Ok(next) => {
                    tracing::info!(
                        "microphone: {error}, recording from {} now",
                        next.microphone.name().unwrap_or("the system default")
                    );
                    reopens += 1;
                    let last = std::mem::replace(&mut input, next).close(None);
                    told = told || input.tell_fallback(recording, Missing::MidWay);
                    last
                }
                Err(reopening) => {
                    tracing::warn!("microphone: {error}, and no other one opens: {reopening}");
                    break (Instant::now(), Some(error));
                }
            },
        };
        control.apply(&mut pcm);
        if let Some(text) = utterance.feed(&pcm)? {
            events(Event::Live(text));
        }
    };
    events(Event::Finishing);
    // The tail's audio waits in the channel: a live text now would only delay the final one.
    loop {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match commands.recv_timeout(left) {
            Ok(Command::Cancel) | Err(RecvTimeoutError::Disconnected) => {
                events(Event::Cancelled);
                return Ok(());
            }
            Ok(Command::Start { .. } | Command::Stop(_)) | Err(RecvTimeoutError::Timeout) => {}
        }
    }
    let mut pcm = input.close(Some(end));
    control.apply(&mut pcm);
    events(Event::Done(utterance.finish(&pcm)?));
    if let Some(error) = stopped {
        events(Event::Failed(VoiceError::MicrophoneStopped(error)));
    }
    Ok(())
}

/// Resamples the microphone's blocks so far, except those heard after `end`.
fn take(blocks: &Receiver<Block>, end: Option<Instant>, resampler: &mut Resampler) -> Vec<f32> {
    let mut pcm = Vec::new();
    for block in blocks.try_iter() {
        if end.is_none_or(|end| block.heard <= end) {
            resampler.process(&block.pcm, &mut pcm);
        }
    }
    pcm
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples;

    #[test]
    fn the_audio_heard_after_the_tail_is_dropped() {
        let (audio, blocks) = mpsc::channel();
        let now = Instant::now();
        let end = now + Duration::from_millis(10);
        for heard in [now, end, end + Duration::from_millis(10)] {
            let _ = audio.send(Block {
                heard,
                pcm: vec![0.5; samples(10)],
            });
        }
        let mut resampler = Resampler::new(SAMPLE_RATE, SAMPLE_RATE);
        let mut pcm = take(&blocks, Some(end), &mut resampler);
        resampler.flush(&mut pcm);
        let mut alone = Resampler::new(SAMPLE_RATE, SAMPLE_RATE);
        let mut kept = Vec::new();
        alone.process(&vec![0.5; samples(20)], &mut kept);
        alone.flush(&mut kept);
        assert_eq!(pcm, kept);
    }
}
