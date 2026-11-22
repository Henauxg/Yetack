//! The default microphone through cpal, as mono samples at the device's rate.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};

pub struct Mic {
    _stream: cpal::Stream,
    shared: Arc<Shared>,
    pub rate: u32,
    pub name: String,
}

#[derive(Default)]
struct Shared {
    /// The last block's RMS level, as f32 bits.
    level: AtomicU32,
    recording: AtomicBool,
    buffer: Mutex<Vec<f32>>,
}

impl Mic {
    pub fn open() -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_input_device().context("no microphone")?;
        let name = device.description().map_or_else(|_| "microphone".to_owned(), |d| d.to_string());
        let config = device.default_input_config().context("the microphone's config")?;
        let rate = config.sample_rate();
        let shared = Arc::new(Shared::default());
        let stream = match config.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, &config, &shared)?,
            SampleFormat::I16 => build::<i16>(&device, &config, &shared)?,
            SampleFormat::I32 => build::<i32>(&device, &config, &shared)?,
            SampleFormat::U16 => build::<u16>(&device, &config, &shared)?,
            SampleFormat::U8 => build::<u8>(&device, &config, &shared)?,
            other => bail!("unsupported sample format {other:?}"),
        };
        stream.play()?;
        Ok(Self { _stream: stream, shared, rate, name })
    }

    pub fn start(&self) {
        if let Ok(mut buffer) = self.shared.buffer.lock() {
            buffer.clear();
        }
        self.shared.recording.store(true, Ordering::SeqCst);
    }

    /// Stops recording and returns what is left in the buffer.
    pub fn stop(&self) -> Vec<f32> {
        self.shared.recording.store(false, Ordering::SeqCst);
        self.take()
    }

    /// The audio recorded since the last call.
    pub fn take(&self) -> Vec<f32> {
        self.shared.buffer.lock().map(|mut buffer| std::mem::take(&mut *buffer)).unwrap_or_default()
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.shared.level.load(Ordering::Relaxed))
    }
}

fn build<T>(device: &cpal::Device, config: &cpal::SupportedStreamConfig, shared: &Arc<Shared>) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels()).max(1);
    let shared = Arc::clone(shared);
    let stream = device.build_input_stream(
        (*config).into(),
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|frame| frame.iter().map(|s| f32::from_sample(*s)).sum::<f32>() / frame.len() as f32)
                .collect();
            if !mono.is_empty() {
                let rms = (mono.iter().map(|s| s * s).sum::<f32>() / mono.len() as f32).sqrt();
                shared.level.store(rms.to_bits(), Ordering::Relaxed);
            }
            if shared.recording.load(Ordering::SeqCst)
                && let Ok(mut buffer) = shared.buffer.lock()
            {
                buffer.extend_from_slice(&mono);
            }
        },
        |error| eprintln!("microphone: {error}"),
        None,
    )?;
    Ok(stream)
}

/// A level meter for one terminal line: about -60 dB to 0 dB.
pub fn meter(level: f32) -> String {
    let db = 20.0 * level.max(1e-6).log10();
    let filled = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 30.0).round() as usize;
    format!("{}{}", "#".repeat(filled), ".".repeat(30 - filled))
}

