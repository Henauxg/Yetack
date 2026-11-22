use std::time::{Duration, Instant};

use cockpit_protocol::{AgentId, AgentStatus};

use super::{AgentEntry, AppState};

/// A press of the talk key shorter than this latches the recording on, until the next press.
const TAP: Duration = Duration::from_millis(300);

/// Dictation as the window shows it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct VoiceState {
    /// This build has dictation, and this computer can run its model.
    pub available: bool,
    pub phase: VoicePhase,
    pub live: LiveWords,
    pub model: ModelStatus,
    /// The card above the mic button.
    pub card: Option<VoiceCard>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoicePhase {
    #[default]
    Idle,
    /// Recording into `agent`'s message. A latched recording goes on without the key held.
    Recording {
        agent: AgentId,
        since: Instant,
        latched: bool,
    },
    /// Stopped, its last words on their way.
    Finishing { agent: AgentId },
}

/// The text of the recording so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveWords {
    /// Before the last pause: it no longer changes.
    pub said: String,
    /// Since the last pause: it may still change.
    pub hearing: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModelStatus {
    #[default]
    Missing,
    Downloading {
        done: u64,
        total: u64,
        from: DownloadFrom,
    },
    Ready,
}

/// Where the user started the model's download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadFrom {
    /// The card above the mic, which then stays open to say that the model is ready.
    Card,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceCard {
    /// The model to download, or its download.
    Model,
    /// Why the last dictation or download failed.
    Failed(String),
}

/// What the voice worker has to do after a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerOrder {
    Start,
    Stop,
    Cancel,
    Nothing,
}

/// The agent a dictation can start for: the selected one, unless it exited, since its message
/// field is then off.
pub fn dictation_target(state: &AppState) -> Option<AgentId> {
    state
        .selected()
        .filter(|entry| entry.summary.status != AgentStatus::Exited)
        .map(AgentEntry::id)
}

impl VoiceState {
    /// The talk key went down. `target` is the agent a recording would start for, from
    /// [`dictation_target`]: without one, the key only stops a recording.
    pub fn press(&mut self, target: Option<AgentId>, now: Instant) -> WorkerOrder {
        match (self.phase, target) {
            (VoicePhase::Idle, Some(agent)) => self.start(agent, now, /* latched */ false),
            (VoicePhase::Recording { latched: true, .. }, _) => self.finish(),
            (VoicePhase::Idle, None)
            | (VoicePhase::Recording { latched: false, .. } | VoicePhase::Finishing { .. }, _) => {
                WorkerOrder::Nothing
            }
        }
    }

    /// The window lost the focus with the talk key down, whose release then never comes: a
    /// recording the key holds stops, however short. A tapped one goes on.
    pub fn blur(&mut self) -> WorkerOrder {
        match self.phase {
            VoicePhase::Recording { latched: false, .. } => self.finish(),
            VoicePhase::Idle
            | VoicePhase::Recording { latched: true, .. }
            | VoicePhase::Finishing { .. } => WorkerOrder::Nothing,
        }
    }

    /// The talk key came up: it stops a recording it held, unless it was only tapped.
    pub fn release(&mut self, now: Instant) -> WorkerOrder {
        match self.phase {
            VoicePhase::Recording {
                agent,
                since,
                latched: false,
            } => {
                if now.saturating_duration_since(since) < TAP {
                    self.phase = VoicePhase::Recording {
                        agent,
                        since,
                        latched: true,
                    };
                    WorkerOrder::Nothing
                } else {
                    self.finish()
                }
            }
            VoicePhase::Idle | VoicePhase::Recording { .. } | VoicePhase::Finishing { .. } => {
                WorkerOrder::Nothing
            }
        }
    }

    /// A click on the mic button: it starts a recording that runs until the next click. `target`
    /// is as for [`Self::press`].
    pub fn click(&mut self, target: Option<AgentId>, now: Instant) -> WorkerOrder {
        match (self.phase, target) {
            (VoicePhase::Idle, Some(agent)) => self.start(agent, now, /* latched */ true),
            (VoicePhase::Recording { .. }, _) => self.finish(),
            (VoicePhase::Idle, None) | (VoicePhase::Finishing { .. }, _) => WorkerOrder::Nothing,
        }
    }

    /// Esc: the recording and its text go.
    pub fn cancel(&mut self) -> WorkerOrder {
        if self.phase == VoicePhase::Idle {
            return WorkerOrder::Nothing;
        }
        self.phase = VoicePhase::Idle;
        self.live = LiveWords::default();
        WorkerOrder::Cancel
    }

    /// The worker's final text came: the agent whose message takes it.
    pub fn done(&mut self) -> Option<AgentId> {
        let agent = self.dictating();
        self.phase = VoicePhase::Idle;
        self.live = LiveWords::default();
        agent
    }

    pub fn failed(&mut self, why: String) {
        self.phase = VoicePhase::Idle;
        self.live = LiveWords::default();
        self.card = Some(VoiceCard::Failed(why));
    }

    /// The model's download starts, and the card shows its progress.
    pub fn download_started(&mut self, from: DownloadFrom, total: u64) {
        self.model = ModelStatus::Downloading {
            done: 0,
            total,
            from,
        };
        self.card = Some(VoiceCard::Model);
    }

    pub fn download_progress(&mut self, bytes: u64) {
        if let ModelStatus::Downloading { done, .. } = &mut self.model {
            *done = bytes;
        }
    }

    /// The download ended. Started from the card, the card stays to say that the model is
    /// ready, unless the user hid it. Started from Settings, it closes.
    pub fn download_ended(&mut self, result: Result<(), String>) {
        let from = match self.model {
            ModelStatus::Downloading { from, .. } => Some(from),
            ModelStatus::Missing | ModelStatus::Ready => None,
        };
        match result {
            Ok(()) => {
                self.model = ModelStatus::Ready;
                if from != Some(DownloadFrom::Card) {
                    self.card = None;
                }
            }
            Err(why) => {
                self.model = ModelStatus::Missing;
                self.card = Some(VoiceCard::Failed(why));
            }
        }
    }

    /// The agent whose message takes the text, while a recording runs or finishes.
    pub fn dictating(&self) -> Option<AgentId> {
        match self.phase {
            VoicePhase::Recording { agent, .. } | VoicePhase::Finishing { agent } => Some(agent),
            VoicePhase::Idle => None,
        }
    }

    /// Without the model, the card offers its download instead.
    fn start(&mut self, agent: AgentId, now: Instant, latched: bool) -> WorkerOrder {
        if !self.available {
            return WorkerOrder::Nothing;
        }
        if self.model != ModelStatus::Ready {
            self.card = Some(VoiceCard::Model);
            return WorkerOrder::Nothing;
        }
        self.card = None;
        self.live = LiveWords::default();
        self.phase = VoicePhase::Recording {
            agent,
            since: now,
            latched,
        };
        WorkerOrder::Start
    }

    fn finish(&mut self) -> WorkerOrder {
        let Some(agent) = self.dictating() else {
            return WorkerOrder::Nothing;
        };
        self.phase = VoicePhase::Finishing { agent };
        WorkerOrder::Stop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> VoiceState {
        VoiceState {
            available: true,
            model: ModelStatus::Ready,
            ..VoiceState::default()
        }
    }

    fn agent() -> AgentId {
        AgentId::new(3)
    }

    fn target() -> Option<AgentId> {
        Some(agent())
    }

    #[test]
    fn leaving_the_window_stops_a_held_recording_however_short() {
        let mut voice = ready();
        let now = Instant::now();
        voice.press(target(), now);

        assert_eq!(voice.blur(), WorkerOrder::Stop);
        assert_eq!(voice.phase, VoicePhase::Finishing { agent: agent() });
        assert_eq!(voice.release(now), WorkerOrder::Nothing);
    }

    #[test]
    fn leaving_the_window_keeps_a_tapped_recording() {
        let mut voice = ready();
        let now = Instant::now();
        voice.press(target(), now);
        voice.release(now + Duration::from_millis(120));

        assert_eq!(voice.blur(), WorkerOrder::Nothing);
        assert!(matches!(
            voice.phase,
            VoicePhase::Recording { latched: true, .. }
        ));
    }

    #[test]
    fn an_exited_agent_takes_no_dictation() {
        use crate::state::{state_with, summary};

        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Exited),
        ]);
        state.ui.selected = Some(AgentId::new(1));
        let running = dictation_target(&state);
        state.ui.selected = Some(AgentId::new(2));
        let exited = dictation_target(&state);
        let mut voice = ready();
        let now = Instant::now();

        assert_eq!(running, Some(AgentId::new(1)));
        assert_eq!(exited, None);
        assert_eq!(voice.press(exited, now), WorkerOrder::Nothing);
        assert_eq!(voice.click(exited, now), WorkerOrder::Nothing);
        assert_eq!(voice.phase, VoicePhase::Idle);
        // A recording that runs still stops from there.
        voice.click(running, now);
        assert_eq!(voice.press(exited, now), WorkerOrder::Stop);
    }

    #[test]
    fn a_download_from_the_card_leaves_it_open_to_say_the_model_is_ready() {
        let mut from_card = VoiceState::default();
        from_card.download_started(DownloadFrom::Card, 10);
        from_card.download_progress(4);
        let progress = from_card.model;
        from_card.download_ended(Ok(()));
        let mut hidden = VoiceState::default();
        hidden.download_started(DownloadFrom::Card, 10);
        hidden.card = None;
        hidden.download_ended(Ok(()));
        let mut from_settings = VoiceState::default();
        from_settings.download_started(DownloadFrom::Settings, 10);
        from_settings.download_ended(Ok(()));
        let mut failed = VoiceState::default();
        failed.download_started(DownloadFrom::Card, 10);
        failed.card = None;
        failed.download_ended(Err("no network".to_owned()));

        assert_eq!(
            progress,
            ModelStatus::Downloading {
                done: 4,
                total: 10,
                from: DownloadFrom::Card
            }
        );
        assert_eq!(
            (from_card.model, from_card.card),
            (ModelStatus::Ready, Some(VoiceCard::Model))
        );
        assert_eq!((hidden.model, hidden.card), (ModelStatus::Ready, None));
        assert_eq!(
            (from_settings.model, from_settings.card),
            (ModelStatus::Ready, None)
        );
        assert_eq!(
            (failed.model, failed.card),
            (
                ModelStatus::Missing,
                Some(VoiceCard::Failed("no network".to_owned()))
            )
        );
    }

    #[test]
    fn a_held_key_records_until_it_comes_up() {
        let mut voice = ready();
        let now = Instant::now();
        assert_eq!(voice.press(target(), now), WorkerOrder::Start);
        assert_eq!(
            voice.release(now + Duration::from_secs(2)),
            WorkerOrder::Stop
        );
        assert_eq!(voice.phase, VoicePhase::Finishing { agent: agent() });
        assert_eq!(voice.done(), Some(agent()));
        assert_eq!(voice.phase, VoicePhase::Idle);
    }

    #[test]
    fn a_tapped_key_records_until_the_next_press() {
        let mut voice = ready();
        let now = Instant::now();
        assert_eq!(voice.press(target(), now), WorkerOrder::Start);
        assert_eq!(
            voice.release(now + Duration::from_millis(120)),
            WorkerOrder::Nothing
        );
        assert!(matches!(
            voice.phase,
            VoicePhase::Recording { latched: true, .. }
        ));
        assert_eq!(
            voice.press(target(), now + Duration::from_secs(5)),
            WorkerOrder::Stop
        );
        // The release of the press that stopped it changes nothing.
        assert_eq!(
            voice.release(now + Duration::from_secs(5)),
            WorkerOrder::Nothing
        );
    }

    #[test]
    fn the_mic_button_starts_and_stops() {
        let mut voice = ready();
        let now = Instant::now();
        assert_eq!(voice.click(target(), now), WorkerOrder::Start);
        assert_eq!(voice.click(target(), now), WorkerOrder::Stop);
        assert_eq!(voice.click(target(), now), WorkerOrder::Nothing);
    }

    #[test]
    fn without_the_model_the_card_offers_it() {
        let mut voice = VoiceState {
            available: true,
            ..VoiceState::default()
        };
        assert_eq!(voice.press(target(), Instant::now()), WorkerOrder::Nothing);
        assert_eq!(voice.card, Some(VoiceCard::Model));
        assert_eq!(voice.phase, VoicePhase::Idle);
    }

    #[test]
    fn without_dictation_nothing_starts() {
        let mut voice = VoiceState {
            model: ModelStatus::Ready,
            ..VoiceState::default()
        };
        assert_eq!(voice.click(target(), Instant::now()), WorkerOrder::Nothing);
        assert_eq!(voice.card, None);
    }

    #[test]
    fn esc_drops_the_recording_and_its_text() {
        let mut voice = ready();
        assert_eq!(voice.cancel(), WorkerOrder::Nothing);
        voice.press(target(), Instant::now());
        voice.live.said = "Run just check".to_owned();
        assert_eq!(voice.cancel(), WorkerOrder::Cancel);
        assert_eq!(voice.phase, VoicePhase::Idle);
        assert_eq!(voice.live, LiveWords::default());
    }

    #[test]
    fn a_failure_shows_on_the_card() {
        let mut voice = ready();
        voice.press(target(), Instant::now());
        voice.failed("this computer has no microphone".to_owned());
        assert_eq!(voice.phase, VoicePhase::Idle);
        assert_eq!(
            voice.card,
            Some(VoiceCard::Failed(
                "this computer has no microphone".to_owned()
            ))
        );
    }
}
