//! WAV files and resampling to the engines' 16 kHz.

use std::f64::consts::PI;
use std::path::Path;

use anyhow::{Context, Result};

use crate::protocol::SAMPLE_RATE;

/// A WAV file as 16 kHz mono samples.
pub fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("{}", path.display()))?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|value| value as f32 / scale))
                .collect::<Result<_, _>>()?
        }
    };
    let mono = to_mono(&samples, usize::from(spec.channels));
    Ok(resample_all(&mono, spec.sample_rate, SAMPLE_RATE))
}

/// Writes 16 kHz mono samples as 16-bit PCM.
pub fn write_wav(path: &Path, pcm: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for sample in pcm {
        let value = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
        writer.write_sample(value)?;
    }
    writer.finalize()?;
    Ok(())
}

pub fn to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

pub fn resample_all(pcm: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return pcm.to_vec();
    }
    let mut resampler = Resampler::new(from, to);
    let mut out = Vec::with_capacity(pcm.len() * to as usize / from as usize + 1);
    resampler.process(pcm, &mut out);
    resampler.flush(&mut out);
    out
}

/// A streaming windowed-sinc resampler: the microphone's rate to 16 kHz, a block at a time.
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// The cut-off as a fraction of the input's Nyquist frequency.
    cutoff: f64,
    /// Input taps on each side of an output sample.
    half: usize,
    buffer: Vec<f32>,
    /// The next output sample's position in `buffer`.
    position: f64,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        let cutoff = (f64::from(to) / f64::from(from)).min(1.0) * 0.95;
        let half = (16.0 / cutoff).ceil() as usize;
        Self {
            step: f64::from(from) / f64::from(to),
            cutoff,
            half,
            buffer: vec![0.0; half],
            position: half as f64,
        }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buffer.extend_from_slice(input);
        let half = self.half as isize;
        loop {
            let center = self.position;
            let base = center.floor() as isize;
            if base + half >= self.buffer.len() as isize {
                break;
            }
            let mut sum = 0.0;
            for index in (base - half + 1)..=(base + half) {
                let Some(&sample) = usize::try_from(index).ok().and_then(|i| self.buffer.get(i))
                else {
                    continue;
                };
                let distance = center - index as f64;
                sum += f64::from(sample) * self.kernel(distance);
            }
            out.push(sum as f32);
            self.position += self.step;
        }
        let consumed = (self.position.floor() as isize - half).max(0) as usize;
        let consumed = consumed.min(self.buffer.len());
        self.buffer.drain(..consumed);
        self.position -= consumed as f64;
    }

    pub fn flush(&mut self, out: &mut Vec<f32>) {
        let padding = vec![0.0; self.half + 1];
        self.process(&padding, out);
    }

    fn kernel(&self, distance: f64) -> f64 {
        let x = distance / self.half as f64;
        if x.abs() >= 1.0 {
            return 0.0;
        }
        let window = 0.42 + 0.5 * (PI * x).cos() + 0.08 * (2.0 * PI * x).cos();
        let t = self.cutoff * distance;
        let sinc = if t.abs() < 1e-9 { 1.0 } else { (PI * t).sin() / (PI * t) };
        self.cutoff * sinc * window
    }
}

/// Where speech starts and ends in a clip, in ms: the first and the last 20 ms window louder
/// than a tenth of the loudest one.
pub fn speech_span_ms(pcm: &[f32]) -> (f64, f64) {
    let window = SAMPLE_RATE as usize / 50;
    let levels: Vec<f32> = pcm
        .chunks(window)
        .map(|chunk| (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt())
        .collect();
    let loudest = levels.iter().copied().fold(0.0f32, f32::max);
    let loud = |level: &f32| *level > loudest * 0.1;
    let first = levels.iter().position(loud).unwrap_or(0);
    let last = levels.iter().rposition(loud).map_or(levels.len(), |i| i + 1);
    let to_ms = |windows: usize| (windows * window) as f64 * 1000.0 / f64::from(SAMPLE_RATE);
    (to_ms(first), to_ms(last))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_a_tone_and_its_length() {
        let from = 48_000;
        let tone: Vec<f32> = (0..from)
            .map(|i| (2.0 * PI * 440.0 * f64::from(i) / f64::from(from)).sin() as f32 * 0.5)
            .collect();
        let out = resample_all(&tone, from, 16_000);
        assert!((out.len() as i64 - 16_000).abs() <= 2, "{}", out.len());
        let middle = &out[4000..12000];
        let peak = middle.iter().copied().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!((peak - 0.5).abs() < 0.02, "{peak}");
    }

    #[test]
    fn streaming_resampling_matches_one_block() {
        let from = 44_100;
        let input: Vec<f32> = (0..from).map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5).collect();
        let whole = resample_all(&input, from, 16_000);
        let mut resampler = Resampler::new(from, 16_000);
        let mut pieces = Vec::new();
        for block in input.chunks(441) {
            resampler.process(block, &mut pieces);
        }
        resampler.flush(&mut pieces);
        assert_eq!(whole.len(), pieces.len());
        for (a, b) in whole.iter().zip(&pieces) {
            assert!((a - b).abs() < 1e-5);
        }
    }
}
