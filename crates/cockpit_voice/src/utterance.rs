use std::time::{Duration, Instant};

use crate::live_text::LiveText;
use crate::pauses::Pauses;
use crate::voice::LiveRefresh;
use crate::voice_error::VoiceError;
use crate::{SAMPLE_RATE, fillers, samples};

/// Turns a segment of speech into text.
pub(crate) trait Transcribe {
    fn transcribe(&mut self, pcm: &[f32]) -> Result<String, VoiceError>;
}

/// Says whether a segment holds speech at all.
pub(crate) trait SpeechCheck {
    fn has_speech(&mut self, pcm: &[f32]) -> Result<bool, VoiceError>;
}

/// The quiet that ends a segment.
const PAUSE: usize = samples(400);
/// The shortest segment worth cutting.
const MIN_SEGMENT: usize = samples(1000);
/// The quiet a segment keeps before its first loud frame, where speech can start softly. More
/// would only make each transcription of the segment slower.
const QUIET_BEFORE: usize = samples(400);
/// A segment this long without a pause, as in steady noise, is cut anyway: each live text
/// transcribes the whole segment again.
const LONGEST: usize = samples(20_000);
/// The frame the pauses measure.
const FRAME: usize = samples(20);
/// The live text's quickest refresh.
const FASTEST: usize = samples(500);
/// The live text's slowest refresh.
const SLOWEST: usize = samples(2000);
/// A refresh waits this many times as long as the last one took, so the model computes at most
/// 40% of the time.
const PACE: f64 = 2.5;

/// One recording's speech, cut at its pauses. Each finished segment is transcribed once and its
/// text kept. The live text comes from transcribing the current segment again, as often as the
/// computer allows. When the recording ends, only the last segment is left.
pub(crate) struct Utterance<'models, M, C> {
    model: &'models mut M,
    check: &'models mut C,
    refresh: LiveRefresh,
    said: String,
    segment: Vec<f32>,
    pauses: Pauses,
    /// The segment's length at its last refresh.
    refreshed_at: usize,
    step: usize,
}

impl<'models, M: Transcribe, C: SpeechCheck> Utterance<'models, M, C> {
    pub(crate) fn new(model: &'models mut M, check: &'models mut C, refresh: LiveRefresh) -> Self {
        let step = match refresh {
            LiveRefresh::EverySecond => samples(1000),
            LiveRefresh::Auto | LiveRefresh::Off => FASTEST,
        };
        Self {
            model,
            check,
            refresh,
            said: String::new(),
            segment: Vec::new(),
            pauses: Pauses::new(),
            refreshed_at: 0,
            step,
        }
    }

    /// Takes the next audio, and returns the text when it changed.
    pub(crate) fn feed(&mut self, pcm: &[f32]) -> Result<Option<LiveText>, VoiceError> {
        if self.push(pcm)? {
            return Ok(Some(LiveText {
                said: self.said.clone(),
                hearing: String::new(),
            }));
        }
        let due = self.segment.len().saturating_sub(self.refreshed_at) >= self.step;
        if self.refresh == LiveRefresh::Off || !self.pauses.voiced() || !due {
            return Ok(None);
        }
        self.refreshed_at = self.segment.len();
        let started = Instant::now();
        let hearing = fillers::without_fillers(&text_of(&mut *self.model, &self.segment)?);
        self.adapt(started.elapsed());
        Ok(Some(LiveText {
            said: self.said.clone(),
            hearing,
        }))
    }

    /// The final text, with the audio left once the recording has ended.
    pub(crate) fn finish(mut self, rest: &[f32]) -> Result<String, VoiceError> {
        self.push(rest)?;
        if self.pauses.voiced() {
            self.commit()?;
        }
        Ok(self.said)
    }

    /// Adds audio to the segment, and says whether it was cut.
    fn push(&mut self, pcm: &[f32]) -> Result<bool, VoiceError> {
        let mut cut = false;
        for &sample in pcm {
            self.segment.push(sample);
            if !self.pauses.push(sample) {
                continue;
            }
            if !self.pauses.voiced() {
                let older = self.segment.len().saturating_sub(QUIET_BEFORE);
                self.segment.drain(..older);
            } else if self.pauses.quiet() >= PAUSE && self.segment.len() >= MIN_SEGMENT {
                self.commit()?;
                cut = true;
            } else if self.segment.len() >= LONGEST {
                self.settle(quietest_end(&self.segment))?;
                cut = true;
            }
        }
        Ok(cut)
    }

    /// Settles the whole segment, at a pause or at the end.
    fn commit(&mut self) -> Result<(), VoiceError> {
        self.settle(self.segment.len())?;
        self.pauses.cut();
        Ok(())
    }

    /// Transcribes the segment's first `len` samples once, and keeps the rest as the next segment.
    fn settle(&mut self, len: usize) -> Result<(), VoiceError> {
        let len = len.min(self.segment.len());
        let settled = self.segment.get(..len).unwrap_or_default();
        if !settled.is_empty() && self.check.has_speech(settled)? {
            let text = text_of(&mut *self.model, settled)?;
            fillers::push_without_fillers(&mut self.said, &text);
        }
        self.segment.drain(..len);
        self.refreshed_at = 0;
        Ok(())
    }

    fn adapt(&mut self, took: Duration) {
        if self.refresh == LiveRefresh::Auto {
            let wanted = (took.as_secs_f64() * PACE * f64::from(SAMPLE_RATE)) as usize;
            self.step = wanted.clamp(FASTEST, SLOWEST);
        }
    }
}

/// The text of `pcm`, or nothing when the text has more words than the audio can hold: a model
/// can write words on audio without speech, as Qwen3 did with hints.
fn text_of(model: &mut impl Transcribe, pcm: &[f32]) -> Result<String, VoiceError> {
    let text = model.transcribe(pcm)?;
    let most = 3 + pcm.len() * 6 / SAMPLE_RATE as usize;
    Ok(if text.split_whitespace().count() > most {
        String::new()
    } else {
        text
    })
}

/// Where a segment without pauses is cut: after the quietest frame of its second half, so each
/// part is at least half of it.
fn quietest_end(segment: &[f32]) -> usize {
    let start = segment.len() / 2 / FRAME * FRAME;
    let second_half = segment.get(start..).unwrap_or_default();
    let (frames, _) = second_half.as_chunks::<FRAME>();
    let mut quietest = (f32::INFINITY, segment.len());
    for (index, frame) in frames.iter().enumerate() {
        let energy: f32 = frame.iter().map(|sample| sample * sample).sum();
        if energy < quietest.0 {
            quietest = (energy, start + (index + 1) * FRAME);
        }
    }
    quietest.1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes one word per 300 ms of audio, and keeps the length of each audio it transcribes.
    /// `answers` come first, one per transcription, then `words` for each.
    #[derive(Default)]
    struct FakeModel {
        heard: Vec<usize>,
        answers: Vec<&'static str>,
        words: Option<&'static str>,
    }

    impl Transcribe for FakeModel {
        fn transcribe(&mut self, pcm: &[f32]) -> Result<String, VoiceError> {
            let answer = self.answers.get(self.heard.len());
            self.heard.push(pcm.len());
            if let Some(answer) = answer {
                return Ok((*answer).to_owned());
            }
            if let Some(words) = self.words {
                return Ok(words.to_owned());
            }
            let count = (pcm.len() / samples(300)).max(1);
            Ok(vec!["word"; count].join(" "))
        }
    }

    /// Hears speech in a segment louder than 0.1 somewhere. Like Silero, it fails on empty audio.
    struct FakeCheck;

    impl SpeechCheck for FakeCheck {
        fn has_speech(&mut self, pcm: &[f32]) -> Result<bool, VoiceError> {
            if pcm.is_empty() {
                return Err(transcribe_cpp::Error::InvalidArgument("no audio".to_owned()).into());
            }
            Ok(pcm.iter().any(|sample| sample.abs() > 0.1))
        }
    }

    fn speech(ms: u32) -> Vec<f32> {
        (0..samples(ms))
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect()
    }

    fn quiet(ms: u32) -> Vec<f32> {
        vec![0.0; samples(ms)]
    }

    #[test]
    fn each_pause_settles_a_segment_and_the_end_the_last_one() {
        let (mut model, mut check) = (FakeModel::default(), FakeCheck);
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::Off);
        assert_eq!(utterance.feed(&speech(1200)).ok(), Some(None));
        let settled = utterance.feed(&quiet(500)).ok().flatten();
        assert_eq!(
            settled.map(|text| text.said),
            Some("word word word word word".to_owned())
        );
        assert_eq!(utterance.feed(&speech(600)).ok(), Some(None));
        assert_eq!(
            utterance.finish(&[]).ok().as_deref(),
            Some("word word word word word word word")
        );
    }

    #[test]
    fn a_short_pause_does_not_cut() {
        let (mut model, mut check) = (FakeModel::default(), FakeCheck);
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::Off);
        let _ = utterance.feed(&speech(1200));
        assert_eq!(utterance.feed(&quiet(300)).ok(), Some(None));
        let _ = utterance.feed(&speech(600));
        let _ = utterance.finish(&[]);
        assert_eq!(model.heard.len(), 1);
    }

    #[test]
    fn the_live_text_follows_the_current_segment() {
        let (mut model, mut check) = (FakeModel::default(), FakeCheck);
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::EverySecond);
        assert_eq!(utterance.feed(&speech(900)).ok(), Some(None));
        let live = utterance.feed(&speech(300)).ok().flatten();
        assert_eq!(
            live,
            Some(LiveText {
                said: String::new(),
                hearing: "word word word word".to_owned()
            })
        );
    }

    #[test]
    fn the_last_audio_goes_straight_to_the_final_text() {
        let (mut model, mut check) = (FakeModel::default(), FakeCheck);
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::EverySecond);
        assert_eq!(utterance.feed(&speech(900)).ok(), Some(None));
        let text = utterance.finish(&speech(300));
        assert_eq!(text.ok().as_deref(), Some("word word word word"));
        assert_eq!(model.heard.len(), 1);
    }

    #[test]
    fn a_long_quiet_between_segments_is_not_kept() {
        let (mut model, mut check) = (FakeModel::default(), FakeCheck);
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::Off);
        let _ = utterance.feed(&speech(1200));
        let _ = utterance.feed(&quiet(60_000));
        assert_eq!(utterance.segment.len(), samples(400));
    }

    #[test]
    fn a_segment_without_pauses_is_cut_at_its_quietest_frame() {
        let (mut model, mut check) = (FakeModel::default(), FakeCheck);
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::Off);
        let softer: Vec<f32> = (0..samples(20))
            .map(|i| if i % 2 == 0 { 0.2 } else { -0.2 })
            .collect();
        let _ = utterance.feed(&speech(15_000));
        let _ = utterance.feed(&softer);
        let _ = utterance.feed(&speech(10_000));
        let _ = utterance.finish(&[]);
        assert_eq!(model.heard, [samples(15_020), samples(10_000)]);
    }

    #[test]
    fn a_cut_at_the_last_frame_leaves_nothing_to_settle() {
        let (mut model, mut check) = (FakeModel::default(), FakeCheck);
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::Off);
        let softer: Vec<f32> = (0..samples(20))
            .map(|i| if i % 2 == 0 { 0.2 } else { -0.2 })
            .collect();
        let _ = utterance.feed(&speech(19_980));
        let _ = utterance.feed(&softer);
        assert!(utterance.segment.is_empty());
        assert!(utterance.finish(&[]).is_ok());
        assert_eq!(model.heard, [samples(20_000)]);
    }

    #[test]
    fn a_segment_without_speech_gives_no_text() {
        let (mut model, mut check) = (FakeModel::default(), FakeCheck);
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::Off);
        // Loud enough for the pauses, too quiet for the check: a hum, a breath.
        let hum: Vec<f32> = (0..samples(1500))
            .map(|i| if i % 2 == 0 { 0.05 } else { -0.05 })
            .collect();
        let _ = utterance.feed(&hum);
        assert_eq!(utterance.finish(&[]).ok().as_deref(), Some(""));
        assert_eq!(model.heard.len(), 0);
    }

    #[test]
    fn text_too_long_for_its_audio_is_dropped() {
        let mut model = FakeModel {
            words: Some("tokio Dioxus anyhow musl Gentoo Arch serde rustls JSON clippy cargo"),
            ..FakeModel::default()
        };
        let mut check = FakeCheck;
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::Off);
        let _ = utterance.feed(&speech(1000));
        assert_eq!(utterance.finish(&[]).ok().as_deref(), Some(""));
    }

    #[test]
    fn a_filler_after_a_pause_leaves_its_stop_to_the_words_before_the_pause() {
        let mut model = FakeModel {
            answers: vec!["Let me check the parser", "Um."],
            ..FakeModel::default()
        };
        let mut check = FakeCheck;
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::Off);
        let _ = utterance.feed(&speech(1200));
        let _ = utterance.feed(&quiet(500));
        let _ = utterance.feed(&speech(1200));
        assert_eq!(
            utterance.finish(&[]).ok().as_deref(),
            Some("Let me check the parser.")
        );
    }

    /// The real models on a recording: `VOICE_TEST_MODELS` names the models folder, and
    /// `VOICE_TEST_WAV` a 16 kHz mono 16-bit WAV file. The recording goes in as a microphone
    /// would give it, 30 ms at a time, and the final text is printed.
    #[test]
    #[ignore = "needs the models and a recording"]
    fn transcribes_a_recording() {
        use std::path::Path;

        use crate::level_control::LevelControl;
        use crate::models::Models;

        let (Ok(folder), Ok(wav)) = (
            std::env::var("VOICE_TEST_MODELS"),
            std::env::var("VOICE_TEST_WAV"),
        ) else {
            return;
        };
        let bytes = std::fs::read(wav).unwrap_or_default();
        // A plain WAV file's samples start after its 44-byte header.
        let mut pcm: Vec<f32> = bytes
            .get(44..)
            .unwrap_or_default()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0)
            .collect();
        LevelControl::new().apply(&mut pcm);
        let Ok(mut models) = Models::load(Path::new(&folder), 4) else {
            panic!("the models did not load from {folder}");
        };
        let mut utterance =
            Utterance::new(&mut models.speech, &mut models.detector, LiveRefresh::Auto);
        for block in pcm.chunks(samples(30)) {
            if let Ok(Some(text)) = utterance.feed(block) {
                println!("live: {} | {}", text.said, text.hearing);
            }
        }
        let text = utterance.finish(&[]);
        println!("final: {text:?}");
        assert!(text.is_ok_and(|text| !text.is_empty()));
    }

    #[test]
    fn the_refresh_slows_down_on_a_slow_computer() {
        let (mut model, mut check) = (FakeModel::default(), FakeCheck);
        let mut utterance = Utterance::new(&mut model, &mut check, LiveRefresh::Auto);
        utterance.adapt(Duration::from_millis(100));
        assert_eq!(utterance.step, FASTEST);
        utterance.adapt(Duration::from_millis(400));
        assert_eq!(utterance.step, samples(1000));
        utterance.adapt(Duration::from_secs(3));
        assert_eq!(utterance.step, SLOWEST);
    }
}
