use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::SAMPLE_RATE;
use crate::level_control::LevelControl;
use crate::live_text::LiveText;
use crate::microphone::{Block, Microphone};
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

/// What a recording reports, in this order: levels and live text while it records, then
/// `Finishing` once stopped, then `Done`. A microphone that stops ends the recording the same way,
/// and `Failed` follows its `Done`. Otherwise `Cancelled` or `Failed` can come at any point.
#[derive(Debug)]
pub enum Event {
    /// The microphone's level: the RMS of the last few dozen milliseconds, from 0 to 1.
    Level(f32),
    Live(LiveText),
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
        refresh: LiveRefresh,
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
        refresh: LiveRefresh,
        events: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<(), VoiceError> {
        self.send(Command::Start {
            refresh,
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
        let Command::Start { refresh, events } = command else {
            continue;
        };
        if let Err(error) = record(folder, &mut models, refresh, &events, commands) {
            events(Event::Failed(error));
        }
    }
}

/// One recording, from the microphone's opening to the final text. The models load after the
/// microphone opens, while its audio waits in the channel, so the first word is kept.
fn record(
    folder: &Path,
    models: &mut Option<Models>,
    refresh: LiveRefresh,
    events: &Events,
    commands: &Receiver<Command>,
) -> Result<(), VoiceError> {
    let (audio, blocks) = mpsc::channel();
    let levels = Arc::clone(events);
    let microphone = Microphone::open(audio, move |level| levels(Event::Level(level)))?;
    let models = match models {
        Some(models) => models,
        unloaded @ None => {
            let threads = thread::available_parallelism().map_or(MAX_THREADS, |n| n.get());
            unloaded.insert(Models::load(folder, threads.min(MAX_THREADS))?)
        }
    };
    let mut resampler = Resampler::new(microphone.rate(), SAMPLE_RATE);
    let mut control = LevelControl::new();
    let mut utterance = Utterance::new(&mut models.speech, &mut models.detector, refresh);
    let (end, stopped) = loop {
        match commands.recv_timeout(TICK) {
            Ok(Command::Stop(at)) => break (at + TAIL, microphone.lost()),
            Ok(Command::Cancel) | Err(RecvTimeoutError::Disconnected) => {
                events(Event::Cancelled);
                return Ok(());
            }
            Ok(Command::Start { .. }) | Err(RecvTimeoutError::Timeout) => {}
        }
        if let Some(error) = microphone.lost() {
            break (Instant::now(), Some(error));
        }
        let mut pcm = take(&blocks, None, &mut resampler);
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
    drop(microphone);
    let mut pcm = take(&blocks, Some(end), &mut resampler);
    resampler.flush(&mut pcm);
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
