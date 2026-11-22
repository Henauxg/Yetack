/// The peak level the speech reaches after the control.
const TARGET: f32 = 0.7;
/// The most the control amplifies: 20 dB, so silence stays near silent.
const MAX_GAIN: f32 = 10.0;
/// The lowest peak the control follows, where it reaches [`MAX_GAIN`].
const FLOOR: f32 = TARGET / MAX_GAIN;
/// The followed peak halves in 3 s of quieter audio: 0.5^(1 / 48 000) per sample at 16 kHz.
const DECAY: f32 = 0.999_985_6;

/// Evens out the microphone's level before the speech model, which misses quiet speech. It
/// follows the audio's recent peak, rising at once and falling slowly, and scales the audio so
/// that this peak reaches [`TARGET`].
pub(crate) struct LevelControl {
    peak: f32,
}

impl LevelControl {
    pub(crate) fn new() -> Self {
        Self { peak: FLOOR }
    }

    pub(crate) fn apply(&mut self, pcm: &mut [f32]) {
        for sample in pcm {
            self.peak = (self.peak * DECAY).max(sample.abs()).max(FLOOR);
            *sample = (*sample * TARGET / self.peak).clamp(-1.0, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;

    use super::*;

    fn tone(amplitude: f32, seconds: f32) -> Vec<f32> {
        let count = (16_000.0 * seconds) as usize;
        (0..count)
            .map(|i| amplitude * (2.0 * PI * 220.0 * i as f32 / 16_000.0).sin())
            .collect()
    }

    fn peak(pcm: &[f32]) -> f32 {
        pcm.iter().fold(0.0, |peak, sample| peak.max(sample.abs()))
    }

    #[test]
    fn quiet_speech_comes_up_to_the_target() {
        let mut control = LevelControl::new();
        let mut pcm = tone(0.1, 1.0);
        control.apply(&mut pcm);
        let settled = &pcm[8_000..];
        assert!((peak(settled) - TARGET).abs() < 0.02, "{}", peak(settled));
    }

    #[test]
    fn loud_speech_comes_down_to_the_target_without_clipping() {
        let mut control = LevelControl::new();
        let mut pcm = tone(0.95, 1.0);
        control.apply(&mut pcm);
        assert!(peak(&pcm) <= TARGET + 0.01, "{}", peak(&pcm));
    }

    #[test]
    fn silence_gains_at_most_twenty_decibels() {
        let mut control = LevelControl::new();
        let mut pcm = tone(0.001, 0.5);
        control.apply(&mut pcm);
        assert!(peak(&pcm) <= 0.001 * MAX_GAIN + 1e-4, "{}", peak(&pcm));
    }
}
