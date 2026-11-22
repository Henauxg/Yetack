//! Runs a GGUF speech model with transcribe.cpp (ggml), behind the bench's pipe protocol.
//!
//! `--model <file.gguf> [--threads 4] [--backend cpu|vulkan|cuda|auto]
//! [--strategy auto|stream|rerun|segmented|offline] [--step-ms 500] [--terms <file>]
//! [--att-right <frames>] [--buffer <left,chunk,right ms>] [--final-pass]
//! [--final-model <file.gguf>] [--pause-ms 400] [--min-segment-ms 1000] [--vad <silero.gguf>]
//! [--vad-mode cut|check]`
//!
//! With `--vad`, Silero either finds the pauses (`cut`), or only checks each finished segment
//! for speech while the audio's level finds the pauses (`check`).
//!
//! With `--final-model`, the streaming model makes the live text and the final model the final
//! text, one segment between pauses at a time.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use bench_core::args::Args;
use bench_core::engine::{
    self, Engine, Gate, Level, Offline, Pauses, Rerun, Segmented, Speech, Utterance, checked, join, passes,
};
use bench_core::protocol::{self, Event, Ready, SAMPLE_RATE, WordTime};
use transcribe_cpp::{
    Backend, CommitPolicy, Feature, TimestampKind, Model, ModelOptions, ParakeetBufferedStreamOptions,
    ParakeetStreamOptions, RunExtension, RunOptions, Session, SessionOptions, StreamExtension,
    StreamOptions, WhisperRunOptions,
};

const FEATURES: &[(Feature, &str)] = &[
    (Feature::Vocabulary, "vocabulary"),
    (Feature::InitialPrompt, "initial prompt"),
    (Feature::ContextPrompt, "context prompt"),
    (Feature::Pnc, "punctuation"),
    (Feature::Itn, "itn"),
    (Feature::LongForm, "long form"),
];

fn main() -> Result<()> {
    let args = Args::from_env();
    let started = Instant::now();
    let path = args.required("model")?;
    let threads: usize = args.or("threads", 4)?;
    let backend = match args.get("backend").unwrap_or("cpu") {
        "cpu" => Backend::Cpu,
        "vulkan" => Backend::Vulkan,
        "cuda" => Backend::Cuda,
        "auto" => Backend::Auto,
        other => bail!("unknown backend {other}"),
    };
    transcribe_cpp::disable_logging();
    let model = Model::load_with(path, &ModelOptions { backend, ..Default::default() })
        .with_context(|| format!("loading {path}"))?;
    let capabilities = model.capabilities()?;
    let session = model.session_with(&SessionOptions {
        n_threads: i32::try_from(threads)?,
        ..Default::default()
    })?;

    let mut features: Vec<String> = FEATURES
        .iter()
        .filter(|(feature, _)| model.supports(*feature))
        .map(|(_, name)| (*name).to_owned())
        .collect();
    if capabilities.supports_streaming {
        features.push("streaming".to_owned());
    }

    let mut run = RunOptions::default();
    let timestamps = args.has("timestamps");
    if timestamps {
        run.timestamps = TimestampKind::Word;
    }
    if capabilities.languages.len() > 1 && capabilities.languages.iter().any(|l| l == "en") {
        run.language = Some("en".to_owned());
    }
    let terms = match args.get("terms") {
        Some(file) => read_terms(Path::new(file))?,
        None => Vec::new(),
    };
    let mut hints = "none";
    if !terms.is_empty() {
        if model.supports(Feature::Vocabulary) {
            run.vocabulary = terms.clone();
            hints = "vocabulary";
        } else if model.supports(Feature::InitialPrompt) {
            run.family = Some(RunExtension::Whisper(WhisperRunOptions {
                initial_prompt: Some(terms.join(", ")),
                ..Default::default()
            }));
            hints = "initial prompt";
        } else if model.supports(Feature::ContextPrompt) {
            run.prompt = Some(terms.join(", "));
            hints = "context prompt";
        }
    }

    let strategy = match args.get("strategy").unwrap_or("auto") {
        "auto" if capabilities.supports_streaming => "stream",
        "auto" => "rerun",
        other => other,
    }
    .to_owned();
    let mut settings = vec![format!("hints: {hints}")];
    let mut stream_options = StreamOptions {
        commit_policy: CommitPolicy::Auto,
        ..Default::default()
    };
    if let Some(right) = args.optional::<i32>("att-right")? {
        stream_options.family = Some(StreamExtension::ParakeetStream(ParakeetStreamOptions {
            att_context_right: Some(right),
        }));
        settings.push(format!("att right {right}"));
    }
    if let Some(window) = args.get("buffer") {
        let parts: Vec<i32> = window
            .split(',')
            .map(str::parse)
            .collect::<Result<_, _>>()
            .context("--buffer left,chunk,right")?;
        let [left, chunk, right] = parts[..] else {
            bail!("--buffer takes three numbers: left,chunk,right in ms");
        };
        stream_options.family = Some(StreamExtension::ParakeetBuffered(
            ParakeetBufferedStreamOptions {
                left_ms: Some(left),
                chunk_ms: Some(chunk),
                right_ms: Some(right),
            },
        ));
        settings.push(format!("buffer {left}/{chunk}/{right} ms"));
    }
    let final_pass = args.has("final-pass");
    let step_ms: usize = args.or("step-ms", 500)?;
    let samples = |ms: usize| ms * SAMPLE_RATE as usize / 1000;
    let pause = samples(args.or("pause-ms", 400)?);
    let min_segment = samples(args.or("min-segment-ms", 1000)?);
    let mut speech: Box<dyn Speech> = Box::new(Level::default());
    let mut gate: Option<Box<dyn Gate>> = None;
    if let Some(path) = args.get("vad") {
        let model = Model::load(path).with_context(|| format!("loading {path}"))?;
        let session = model.vad_session()?;
        match args.get("vad-mode").unwrap_or("cut") {
            "cut" => {
                let frame = usize::try_from(model.vad_info()?.frame_samples)?;
                settings.push("pauses by Silero VAD".to_owned());
                speech = Box::new(Silero { session, frame, speaking: false });
            }
            "check" => {
                settings.push("segments checked by Silero VAD".to_owned());
                gate = Some(Box::new(SileroGate { session }));
            }
            other => bail!("unknown --vad-mode {other}"),
        }
    }
    let final_model = match args.get("final-model") {
        Some(path) => {
            let model = Model::load_with(path, &ModelOptions { backend, ..Default::default() })
                .with_context(|| format!("loading {path}"))?;
            let session = model.session_with(&SessionOptions {
                n_threads: i32::try_from(threads)?,
                ..Default::default()
            })?;
            let mut final_run = RunOptions::default();
            let capabilities = model.capabilities()?;
            if capabilities.languages.len() > 1 && capabilities.languages.iter().any(|l| l == "en") {
                final_run.language = Some("en".to_owned());
            }
            settings.push(format!("final text by {} between pauses", file_stem(path)));
            Some(Gguf { session, run: final_run, timestamps: false })
        }
        None => None,
    };

    let mut ready = Ready {
        engine: "transcribe.cpp".to_owned(),
        model: file_stem(path),
        backend: model.backend(),
        threads,
        strategy: strategy.clone(),
        features,
        ..Default::default()
    };
    let gguf = Gguf { session, run, timestamps };
    match strategy.as_str() {
        "stream" if final_model.is_some() => {
            ready.settings = settings.join(", ");
            let Some(final_model) = final_model else { bail!("no final model") };
            let mut hybrid =
                Hybrid { live: gguf, options: stream_options, final_model, speech, gate, pause, min_segment };
            engine::serve(ready, started, &mut hybrid)
        }
        "segmented" => {
            settings.push(format!("step {step_ms} ms, cut at pauses"));
            ready.settings = settings.join(", ");
            let step = samples(step_ms);
            engine::serve(ready, started, &mut Segmented { inner: gguf, speech, gate, step, pause, min_segment })
        }
        "stream" => {
            if final_pass {
                settings.push("offline final pass".to_owned());
            }
            ready.settings = settings.join(", ");
            let mut streaming = Streaming { gguf, options: stream_options, final_pass };
            engine::serve(ready, started, &mut streaming)
        }
        "rerun" => {
            settings.push(format!("step {step_ms} ms"));
            ready.settings = settings.join(", ");
            let step = step_ms * SAMPLE_RATE as usize / 1000;
            engine::serve(ready, started, &mut Rerun { inner: gguf, step, live: true })
        }
        "offline" => {
            ready.settings = settings.join(", ");
            engine::serve(ready, started, &mut Rerun { inner: gguf, step: usize::MAX, live: false })
        }
        other => bail!("unknown strategy {other}"),
    }
}

struct Gguf {
    session: Session,
    run: RunOptions,
    /// Sends the words' end times before each text.
    timestamps: bool,
}

impl Offline for Gguf {
    fn transcribe(&mut self, pcm: &[f32]) -> Result<String> {
        let transcript = self.session.run(pcm, &self.run)?;
        if self.timestamps {
            let words = transcript
                .words
                .iter()
                .map(|word| WordTime { text: word.text.clone(), end_ms: word.t1_ms as f64 })
                .collect();
            protocol::emit(&Event::Words { words });
        }
        Ok(transcript.text)
    }
}

struct Streaming {
    gguf: Gguf,
    options: StreamOptions,
    /// Makes the final text with one offline pass over the whole utterance.
    final_pass: bool,
}

impl Engine for Streaming {
    fn run(&mut self, utterance: &mut dyn Utterance) -> Result<String> {
        let mut all = Vec::new();
        let streamed = {
            let mut stream = self.gguf.session.stream(&self.gguf.run, &self.options)?;
            let mut last = String::new();
            while let Some(pcm) = utterance.audio() {
                if self.final_pass {
                    all.extend_from_slice(&pcm);
                }
                let update = stream.feed(&pcm)?;
                if update.committed_changed || update.tentative_changed {
                    let text = stream.text().display();
                    if text != last {
                        utterance.text(&text);
                        last = text;
                    }
                }
            }
            stream.finalize()?;
            stream.text().full
        };
        if self.final_pass && !all.is_empty() {
            return self.gguf.transcribe(&all);
        }
        Ok(streamed)
    }
}

/// Live text from a streaming model, and the final text from an offline model, transcribed one
/// segment between pauses at a time while the user speaks.
struct Hybrid {
    live: Gguf,
    options: StreamOptions,
    final_model: Gguf,
    speech: Box<dyn Speech>,
    gate: Option<Box<dyn Gate>>,
    pause: usize,
    min_segment: usize,
}

impl Engine for Hybrid {
    fn run(&mut self, utterance: &mut dyn Utterance) -> Result<String> {
        let mut committed = String::new();
        let mut segment = Vec::new();
        let mut pauses = Pauses::new(self.speech.as_mut());
        // How much of the stream's text the committed segments cover.
        let mut covered = 0;
        let mut shown = String::new();
        let mut stream = self.live.session.stream(&self.live.run, &self.options)?;
        while let Some(pcm) = utterance.audio() {
            stream.feed(&pcm)?;
            let streamed = stream.text().display();
            for &sample in &pcm {
                segment.push(sample);
                if pauses.push(sample)?
                    && pauses.voiced
                    && pauses.quiet >= self.pause
                    && segment.len() >= self.min_segment
                {
                    if passes(&mut self.gate, &segment)? {
                        join(&mut committed, &checked(&mut self.final_model, &segment)?);
                    }
                    segment.clear();
                    pauses.cut();
                    covered = streamed.len();
                }
            }
            let mut text = committed.clone();
            join(&mut text, streamed.get(covered..).unwrap_or_default());
            if text != shown {
                utterance.text(&text);
                shown = text;
            }
        }
        drop(stream);
        if pauses.voiced && passes(&mut self.gate, &segment)? {
            join(&mut committed, &checked(&mut self.final_model, &segment)?);
        }
        Ok(committed)
    }
}

/// Speech by Silero VAD, with hysteresis: a frame starts speech at 0.5 and ends it below 0.35.
struct Silero {
    session: transcribe_cpp::VadSession,
    frame: usize,
    speaking: bool,
}

impl Speech for Silero {
    fn frame_len(&self) -> usize {
        self.frame
    }

    fn is_speech(&mut self, frame: &[f32]) -> Result<bool> {
        for p in self.session.stream_feed(frame)?.probs {
            self.speaking = if self.speaking { p >= 0.35 } else { p >= 0.5 };
        }
        Ok(self.speaking)
    }

    fn reset(&mut self) {
        self.session.stream_reset();
        self.speaking = false;
    }
}

/// A finished segment holds speech when Silero finds a speech segment in it.
struct SileroGate {
    session: transcribe_cpp::VadSession,
}

impl Gate for SileroGate {
    fn has_speech(&mut self, segment: &[f32]) -> Result<bool> {
        Ok(!self.session.run(segment, &transcribe_cpp::VadOptions::default())?.segments.is_empty())
    }
}

fn read_terms(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .map_or_else(|| path.to_owned(), |stem| stem.to_string_lossy().into_owned())
}
