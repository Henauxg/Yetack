use std::f64::consts::PI;

/// A streaming windowed-sinc resampler: the microphone's rate to the models' 16 kHz, a block at a
/// time.
pub(crate) struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// The cut-off, as a fraction of the input's Nyquist frequency.
    cutoff: f64,
    /// Input taps on each side of an output sample.
    half: usize,
    buffer: Vec<f32>,
    /// The next output sample's position in `buffer`.
    position: f64,
}

impl Resampler {
    pub(crate) fn new(from: u32, to: u32) -> Self {
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

    pub(crate) fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        self.buffer.extend_from_slice(input);
        loop {
            let center = self.position;
            let base = center.floor() as usize;
            if base + self.half >= self.buffer.len() {
                break;
            }
            let first = (base + 1).saturating_sub(self.half);
            let sum: f64 = self
                .buffer
                .iter()
                .enumerate()
                .skip(first)
                .take(base + self.half + 1 - first)
                .map(|(index, sample)| f64::from(*sample) * self.kernel(center - index as f64))
                .sum();
            out.push(sum as f32);
            self.position += self.step;
        }
        let consumed = (self.position.floor() as usize)
            .saturating_sub(self.half)
            .min(self.buffer.len());
        self.buffer.drain(..consumed);
        self.position -= consumed as f64;
    }

    /// The last samples, once the input has ended.
    pub(crate) fn flush(&mut self, out: &mut Vec<f32>) {
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
        let sinc = if t.abs() < 1e-9 {
            1.0
        } else {
            (PI * t).sin() / (PI * t)
        };
        self.cutoff * sinc * window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resample_all(input: &[f32], from: u32) -> Vec<f32> {
        let mut resampler = Resampler::new(from, 16_000);
        let mut out = Vec::new();
        resampler.process(input, &mut out);
        resampler.flush(&mut out);
        out
    }

    #[test]
    fn a_tone_keeps_its_level_and_its_length() {
        let from = 48_000;
        let tone: Vec<f32> = (0..from)
            .map(|i| (2.0 * PI * 440.0 * f64::from(i) / f64::from(from)).sin() as f32 * 0.5)
            .collect();
        let out = resample_all(&tone, from);
        assert!(out.len().abs_diff(16_000) <= 2, "{}", out.len());
        let middle = &out[4_000..12_000];
        let peak = middle
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!((peak - 0.5).abs() < 0.02, "{peak}");
    }

    #[test]
    fn blocks_give_what_one_block_gives() {
        let from = 44_100;
        let input: Vec<f32> = (0..from)
            .map(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5)
            .collect();
        let whole = resample_all(&input, from);
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
