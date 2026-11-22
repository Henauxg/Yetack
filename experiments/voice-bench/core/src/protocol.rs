//! The driver writes frames to an engine's stdin, and the engine answers with one JSON event per
//! line on stdout. Each engine runs in its own process, so its memory is measured alone and two
//! native runtimes never share a binary.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// Audio frames carry mono f32 samples at this rate.
pub const SAMPLE_RATE: u32 = 16_000;

pub enum Frame {
    Audio(Vec<f32>),
    /// The end of an utterance: the key is released.
    End,
    Quit,
}

const AUDIO: u8 = b'A';
const END: u8 = b'E';
const QUIT: u8 = b'Q';

pub fn write_frame(out: &mut impl Write, frame: &Frame) -> io::Result<()> {
    let mut bytes = Vec::new();
    match frame {
        Frame::Audio(samples) => {
            let len = u32::try_from(samples.len()).map_err(io::Error::other)?;
            bytes.push(AUDIO);
            bytes.extend_from_slice(&len.to_le_bytes());
            for sample in samples {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
        }
        Frame::End => {
            bytes.push(END);
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
        Frame::Quit => {
            bytes.push(QUIT);
            bytes.extend_from_slice(&0u32.to_le_bytes());
        }
    }
    out.write_all(&bytes)?;
    out.flush()
}

/// The next frame, or `None` once the driver closed the pipe.
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 5];
    match input.read_exact(&mut head) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let [kind, a, b, c, d] = head;
    let len = u32::from_le_bytes([a, b, c, d]) as usize;
    match kind {
        AUDIO => {
            let mut raw = vec![0u8; len * 4];
            input.read_exact(&mut raw)?;
            let samples = raw.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
            Ok(Some(Frame::Audio(samples)))
        }
        END => Ok(Some(Frame::End)),
        QUIT => Ok(Some(Frame::Quit)),
        other => Err(io::Error::other(format!("unknown frame kind {other}"))),
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Ready(Ready),
    /// The utterance's whole text so far. `input_ms` is the audio the engine had received when it
    /// started the computation that produced this text.
    Partial {
        text: String,
        input_ms: f64,
        compute_ms: f64,
    },
    /// `compute_ms` runs from the end of the utterance to the final text.
    Final {
        text: String,
        input_ms: f64,
        compute_ms: f64,
        usage: Usage,
    },
    /// The words of the text the engine is about to send as final, with when each ends in the
    /// utterance. Only an engine asked for timestamps sends it.
    Words {
        words: Vec<WordTime>,
    },
    Error {
        message: String,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WordTime {
    pub text: String,
    pub end_ms: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Ready {
    pub engine: String,
    pub model: String,
    pub backend: String,
    pub threads: usize,
    /// How live text is made: `stream` (the model streams), `rerun` (the whole utterance again
    /// at each step) or `offline` (text only at the end).
    pub strategy: String,
    pub settings: String,
    pub features: Vec<String>,
    pub load_ms: f64,
    pub usage: Usage,
}

/// The engine process's memory in bytes and its CPU time since it started.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub struct Usage {
    /// Resident memory, which includes the pages of a memory-mapped model file.
    pub working_set: u64,
    pub peak_working_set: u64,
    /// Memory the process allocated itself, without mapped files.
    pub private: u64,
    pub peak_private: u64,
    pub cpu_ms: f64,
}

pub fn emit(event: &Event) {
    let Ok(line) = serde_json::to_string(event) else {
        return;
    };
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}
