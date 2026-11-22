use crate::samples;

/// Finds the pauses in speech, a 20 ms frame at a time: a frame is quiet below a tenth of the
/// loudest frame so far.
pub(crate) struct Pauses {
    frame: Vec<f32>,
    loudest: f32,
    /// Samples of quiet since the last loud frame.
    quiet: usize,
    /// The current segment has a loud frame.
    voiced: bool,
}

/// Under this level, a frame is quiet whatever the loudest one was.
const SILENCE: f32 = 0.003;

impl Pauses {
    pub(crate) fn new() -> Self {
        Self {
            frame: Vec::with_capacity(samples(20)),
            loudest: 0.0,
            quiet: 0,
            voiced: false,
        }
    }

    /// Takes one sample, and says whether it completed a frame.
    pub(crate) fn push(&mut self, sample: f32) -> bool {
        self.frame.push(sample);
        if self.frame.len() < samples(20) {
            return false;
        }
        let energy: f32 = self.frame.iter().map(|sample| sample * sample).sum();
        let rms = (energy / self.frame.len() as f32).sqrt();
        self.frame.clear();
        self.loudest = self.loudest.max(rms);
        if rms < (self.loudest * 0.1).max(SILENCE) {
            self.quiet += samples(20);
        } else {
            self.quiet = 0;
            self.voiced = true;
        }
        true
    }

    pub(crate) fn quiet(&self) -> usize {
        self.quiet
    }

    pub(crate) fn voiced(&self) -> bool {
        self.voiced
    }

    /// Starts a new segment.
    pub(crate) fn cut(&mut self) {
        self.voiced = false;
        self.quiet = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_all(pauses: &mut Pauses, value: f32, count: usize) {
        for i in 0..count {
            pauses.push(if i % 2 == 0 { value } else { -value });
        }
    }

    #[test]
    fn quiet_counts_from_the_last_loud_frame() {
        let mut pauses = Pauses::new();
        push_all(&mut pauses, 0.5, samples(200));
        assert!(pauses.voiced());
        assert_eq!(pauses.quiet(), 0);
        push_all(&mut pauses, 0.01, samples(300));
        assert_eq!(pauses.quiet(), samples(300));
        push_all(&mut pauses, 0.5, samples(20));
        assert_eq!(pauses.quiet(), 0);
    }

    #[test]
    fn silence_alone_is_no_speech() {
        let mut pauses = Pauses::new();
        push_all(&mut pauses, 0.001, samples(500));
        assert!(!pauses.voiced());
    }

    #[test]
    fn a_cut_starts_a_segment_without_speech() {
        let mut pauses = Pauses::new();
        push_all(&mut pauses, 0.5, samples(100));
        pauses.cut();
        assert!(!pauses.voiced());
        assert_eq!(pauses.quiet(), 0);
    }
}
