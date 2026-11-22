//! The loop every engine binary runs: it reads utterances from stdin and reports their text.

use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use anyhow::Result;

use crate::protocol::{self, Event, Frame, Ready, SAMPLE_RATE};
use crate::{ms, usage};

/// The audio of one utterance as it arrives, and where its text goes.
pub trait Utterance {
    /// All the audio that arrived since the last call, waiting for some. `None` once the
    /// utterance ends.
    fn audio(&mut self) -> Option<Vec<f32>>;
    /// The utterance's whole text so far.
    fn text(&mut self, text: &str);
}

pub trait Engine {
    /// Transcribes one utterance and returns its final text.
    fn run(&mut self, utterance: &mut dyn Utterance) -> Result<String>;
}

/// An engine that transcribes a whole clip at once.
pub trait Offline {
    fn transcribe(&mut self, pcm: &[f32]) -> Result<String>;
}

/// Makes live text with an offline engine: it transcribes the whole utterance again once `step`
/// new samples have arrived. Without `live`, it transcribes only at the end.
pub struct Rerun<O> {
    pub inner: O,
    pub step: usize,
    pub live: bool,
}

impl<O: Offline> Engine for Rerun<O> {
    fn run(&mut self, utterance: &mut dyn Utterance) -> Result<String> {
        let mut pcm = Vec::new();
        let mut done = 0;
        let mut last = String::new();
        while let Some(more) = utterance.audio() {
            pcm.extend_from_slice(&more);
            if self.live && pcm.len() - done >= self.step {
                done = pcm.len();
                let text = self.inner.transcribe(&pcm)?;
                if text != last {
                    utterance.text(&text);
                    last = text;
                }
            }
        }
        if pcm.is_empty() {
            return Ok(String::new());
        }
        if done == pcm.len() {
            return Ok(last);
        }
        self.inner.transcribe(&pcm)
    }
}

/// Says whether a frame of audio holds speech.
pub trait Speech {
    fn frame_len(&self) -> usize;
    fn is_speech(&mut self, frame: &[f32]) -> Result<bool>;
    /// Forgets the last utterance.
    fn reset(&mut self) {}
}

/// Speech by level: a 20 ms frame louder than a tenth of the loudest one so far.
#[derive(Default)]
pub struct Level {
    loudest: f32,
}

impl Speech for Level {
    fn frame_len(&self) -> usize {
        SAMPLE_RATE as usize / 50
    }

    fn is_speech(&mut self, frame: &[f32]) -> Result<bool> {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt();
        self.loudest = self.loudest.max(rms);
        Ok(rms >= (self.loudest * 0.1).max(0.003))
    }

    fn reset(&mut self) {
        self.loudest = 0.0;
    }
}

/// Finds the pauses in an utterance, a frame at a time.
pub struct Pauses<'a> {
    speech: &'a mut dyn Speech,
    frame: Vec<f32>,
    /// Samples of quiet since the last frame of speech.
    pub quiet: usize,
    /// The current segment has a frame of speech.
    pub voiced: bool,
}

impl<'a> Pauses<'a> {
    pub fn new(speech: &'a mut dyn Speech) -> Self {
        speech.reset();
        Self { speech, frame: Vec::new(), quiet: 0, voiced: false }
    }

    /// Takes one sample, and says whether it completed a frame.
    pub fn push(&mut self, sample: f32) -> Result<bool> {
        self.frame.push(sample);
        if self.frame.len() < self.speech.frame_len() {
            return Ok(false);
        }
        if self.speech.is_speech(&self.frame)? {
            self.quiet = 0;
            self.voiced = true;
        } else {
            self.quiet += self.frame.len();
        }
        self.frame.clear();
        Ok(true)
    }

    /// Starts a new segment.
    pub fn cut(&mut self) {
        self.voiced = false;
        self.quiet = 0;
    }
}

/// Says whether a whole segment holds speech, checked once when the segment ends.
pub trait Gate {
    fn has_speech(&mut self, segment: &[f32]) -> Result<bool>;
}

/// Cuts the utterance at pauses. Each finished segment is transcribed once and kept, live text
/// comes from transcribing the current segment again every `step` samples, and at the end only
/// the last segment is left to transcribe.
pub struct Segmented<O> {
    pub inner: O,
    pub speech: Box<dyn Speech>,
    /// Skips the finished segments it finds no speech in.
    pub gate: Option<Box<dyn Gate>>,
    pub step: usize,
    /// Samples of quiet that end a segment.
    pub pause: usize,
    /// The shortest segment worth cutting.
    pub min_segment: usize,
}

impl<O: Offline> Engine for Segmented<O> {
    fn run(&mut self, utterance: &mut dyn Utterance) -> Result<String> {
        let mut committed = String::new();
        let mut segment = Vec::new();
        let mut pauses = Pauses::new(self.speech.as_mut());
        let mut live_at = 0;
        while let Some(pcm) = utterance.audio() {
            let mut cut = false;
            for &sample in &pcm {
                segment.push(sample);
                if pauses.push(sample)?
                    && pauses.voiced
                    && pauses.quiet >= self.pause
                    && segment.len() >= self.min_segment
                {
                    if passes(&mut self.gate, &segment)? {
                        join(&mut committed, &checked(&mut self.inner, &segment)?);
                    }
                    segment.clear();
                    pauses.cut();
                    live_at = 0;
                    cut = true;
                }
            }
            if cut {
                utterance.text(&committed);
            } else if pauses.voiced && segment.len() - live_at >= self.step {
                live_at = segment.len();
                let mut shown = committed.clone();
                join(&mut shown, &checked(&mut self.inner, &segment)?);
                utterance.text(&shown);
            }
        }
        if pauses.voiced && passes(&mut self.gate, &segment)? {
            join(&mut committed, &checked(&mut self.inner, &segment)?);
        }
        Ok(committed)
    }
}

/// Whether a finished segment goes to the model: always without a gate.
pub fn passes(gate: &mut Option<Box<dyn Gate>>, segment: &[f32]) -> Result<bool> {
    match gate {
        Some(gate) => gate.has_speech(segment),
        None => Ok(true),
    }
}

/// The segment's text, or nothing when it has more words than the audio can hold: a model
/// with hints writes them out when it hears no speech.
pub fn checked(model: &mut impl Offline, segment: &[f32]) -> Result<String> {
    let text = model.transcribe(segment)?;
    let most = 3 + segment.len() * 6 / SAMPLE_RATE as usize;
    Ok(if text.split_whitespace().count() > most { String::new() } else { text })
}

/// Appends `text` after a space.
pub fn join(to: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !to.is_empty() {
        to.push(' ');
    }
    to.push_str(text);
}

/// Announces the loaded engine, then transcribes utterances until the driver quits.
pub fn serve(mut ready: Ready, load_started: Instant, engine: &mut dyn Engine) -> Result<()> {
    ready.load_ms = ms(load_started.elapsed());
    ready.usage = usage::now();
    protocol::emit(&Event::Ready(ready));

    // A thread drains stdin, so the driver's writes never block while the engine computes.
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        loop {
            match protocol::read_frame(&mut stdin) {
                Ok(Some(frame)) => {
                    if sender.send(frame).is_err() {
                        break;
                    }
                }
                Ok(None) | Err(_) => {
                    let _ = sender.send(Frame::Quit);
                    break;
                }
            }
        }
    });

    let mut pipe = Pipe {
        receiver,
        held: None,
        input: 0,
        quit: false,
        mark: Instant::now(),
    };
    loop {
        match pipe.next_frame() {
            Frame::Quit => return Ok(()),
            frame => pipe.held = Some(frame),
        }
        pipe.input = 0;
        pipe.mark = Instant::now();
        let result = engine.run(&mut pipe);
        let compute_ms = ms(pipe.mark.elapsed());
        match result {
            Ok(text) => protocol::emit(&Event::Final {
                text: text.trim().to_owned(),
                input_ms: pipe.input_ms(),
                compute_ms,
                usage: usage::now(),
            }),
            Err(error) => protocol::emit(&Event::Error {
                message: format!("{error:#}"),
            }),
        }
        if pipe.quit {
            return Ok(());
        }
    }
}

struct Pipe {
    receiver: Receiver<Frame>,
    held: Option<Frame>,
    input: usize,
    quit: bool,
    /// When the last audio was handed out, or when the utterance ended.
    mark: Instant,
}

impl Pipe {
    fn next_frame(&mut self) -> Frame {
        self.held
            .take()
            .unwrap_or_else(|| self.receiver.recv().unwrap_or(Frame::Quit))
    }

    fn input_ms(&self) -> f64 {
        self.input as f64 * 1000.0 / f64::from(protocol::SAMPLE_RATE)
    }
}

impl Utterance for Pipe {
    fn audio(&mut self) -> Option<Vec<f32>> {
        let mut pcm = match self.next_frame() {
            Frame::Audio(pcm) => pcm,
            Frame::End => {
                self.mark = Instant::now();
                return None;
            }
            Frame::Quit => {
                self.quit = true;
                self.mark = Instant::now();
                return None;
            }
        };
        // An engine that fell behind gets everything that waits, in one piece.
        while let Ok(frame) = self.receiver.try_recv() {
            match frame {
                Frame::Audio(more) => pcm.extend_from_slice(&more),
                other => {
                    self.held = Some(other);
                    break;
                }
            }
        }
        self.input += pcm.len();
        self.mark = Instant::now();
        Some(pcm)
    }

    fn text(&mut self, text: &str) {
        protocol::emit(&Event::Partial {
            text: text.trim().to_owned(),
            input_ms: self.input_ms(),
            compute_ms: ms(self.mark.elapsed()),
        });
    }
}
