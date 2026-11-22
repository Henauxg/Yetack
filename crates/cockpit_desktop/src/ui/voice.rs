//! Dictation in the composer: the mic button, the strip that shows the recording and its text,
//! and the card for the model's download or a failure.

use std::time::Instant;

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::keymap::Command;
use crate::prefs::AfterDictation;
use crate::state::voice::{DownloadFrom, ModelStatus, VoiceCard, VoicePhase};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::{use_agent_presence, use_presence};
use crate::voice_desk::{VoiceCommand, VoiceDesk};

/// How the level meter's bars stand against each other, so that one level reads as a voice.
const BARS: [f32; 12] = [
    0.45, 0.7, 0.95, 0.75, 1.0, 0.8, 0.55, 0.85, 0.65, 0.9, 0.5, 0.6,
];

/// The strip's clip, whose height `STRIP_HEIGHT_SCRIPT` animates.
const STRIP_CLIP_ID: &str = "voice-strip-clip";

/// The strip's text wraps onto more lines as the user talks. CSS cannot animate a height that
/// follows the content, so this script, given the clip's id, watches the clip's content and
/// animates the clip from its last height to the new one, as `dialog_height` does for dialogs. A
/// strip that leaves plays its exit animation alone.
const STRIP_HEIGHT_SCRIPT: &str = r#"
const id = await dioxus.recv();
const clip = document.getElementById(id);
const body = clip?.firstElementChild;
if (body) {
  const token = (name) => getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  const millis = (name) => {
    const value = token(name);
    const number = parseFloat(value);
    if (Number.isNaN(number)) {
      return 0;
    }
    return value.endsWith("ms") ? number : number * 1000;
  };
  let height = body.offsetHeight;
  let running = null;
  const resize = new ResizeObserver(() => {
    if (!clip.isConnected) {
      resize.disconnect();
      return;
    }
    const to = body.offsetHeight;
    if (Math.abs(to - height) < 1) {
      return;
    }
    // A running animation goes on from where it is.
    const from = running === null ? height : clip.offsetHeight;
    running?.cancel();
    running = null;
    height = to;
    const grows = to > from;
    const duration = millis(grows ? "--motion-in" : "--motion-out");
    if (duration <= 0 || clip.closest(".voice-strip.leaving") !== null) {
      return;
    }
    const animation = clip.animate(
      [{ height: `${from}px` }, { height: `${to}px` }],
      { duration, easing: token(grows ? "--ease-out" : "--ease-in") },
    );
    animation.onfinish = () => {
      if (running === animation) {
        running = null;
      }
    };
    running = animation;
  });
  resize.observe(body);
}
"#;

/// The mic, before Send: a click starts a dictation, which runs until the next click. It is off
/// for an exited agent, as the message field is.
#[component]
pub fn MicButton(agent: AgentId, exited: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let voice: VoiceDesk = use_context();
    let (enabled, after, keys) = {
        let state = cockpit.state.read();
        (
            state.prefs.voice.enabled,
            state.prefs.voice.after,
            state.prefs.keys.shown(Command::Talk),
        )
    };
    let (available, phase, missing) = {
        let voice = voice.state.read();
        (
            voice.available,
            voice.phase,
            voice.model != ModelStatus::Ready,
        )
    };
    let badge = use_presence(missing.then_some(()));
    if !available || !enabled {
        return rsx! {};
    }
    let recording = matches!(phase, VoicePhase::Recording { agent: on, .. } if on == agent);
    let finishing = matches!(phase, VoicePhase::Finishing { agent: on } if on == agent);
    // A recording into the agent still stops from here.
    let off = exited && !recording;
    let class = match (recording, finishing) {
        (true, _) => "icon-button small mic-button recording",
        (_, true) => "icon-button small mic-button finishing",
        _ => "icon-button small mic-button",
    };
    let title = match (recording, after) {
        (true, AfterDictation::Insert) => "Stop: the text goes in the message".to_owned(),
        (true, AfterDictation::InsertAndSend) => "Stop and send the message".to_owned(),
        (false, _) if off => "Resume the agent to dictate".to_owned(),
        (false, _) => format!("Dictate: hold {keys}, or click"),
    };
    rsx! {
        span { class: "mic-anchor",
            button {
                class,
                aria_label: if recording { "Stop dictating" } else { "Dictate" },
                aria_pressed: recording,
                disabled: off,
                title,
                // The message keeps the focus and its caret, where the text goes.
                onmousedown: move |event| event.prevent_default(),
                onclick: move |_| voice.send(VoiceCommand::Click),
                Icon { kind: IconKind::Mic, size: 16 }
                if let Some(badge) = badge {
                    span {
                        class: if badge.leaving { "mic-badge leaving" } else { "mic-badge" },
                        aria_hidden: true,
                        Icon { kind: IconKind::Download, size: 10 }
                    }
                }
            }
            VoiceCardPopover {}
        }
    }
}

/// The card above the mic: the model's download, or why dictation stopped.
#[component]
fn VoiceCardPopover() -> Element {
    let cockpit: Cockpit = use_context();
    let voice: VoiceDesk = use_context();
    let (card, model) = {
        let state = voice.state.read();
        (state.card.clone(), state.model)
    };
    let Some(shown) = use_presence(card) else {
        return rsx! {};
    };
    let keys = cockpit.state.read().prefs.keys.shown(Command::Talk);
    let class = if shown.leaving {
        "popover voice-card leaving"
    } else {
        "popover voice-card"
    };
    let close = move |_| voice.send(VoiceCommand::CloseCard);
    rsx! {
        div { class, role: "dialog", aria_label: "Dictation",
            match shown.value {
                VoiceCard::Model => match model {
                    ModelStatus::Missing => rsx! {
                        h3 { "Dictation needs a speech model" }
                        p {
                            "Parakeet Ultra, {format::bytes(model_size())}, downloaded once. It runs on this computer: your voice never leaves it."
                        }
                        div { class: "voice-card-actions",
                            button { class: "button small", onclick: close, "Not now" }
                            button {
                                class: "button small primary",
                                onclick: move |_| voice.send(VoiceCommand::Download(DownloadFrom::Card)),
                                "Download"
                            }
                        }
                    },
                    ModelStatus::Downloading { done, total, .. } => rsx! {
                        h3 { "Downloading the speech model" }
                        DownloadBar { done, total }
                        div { class: "voice-card-count",
                            span { class: "mono", "{format::bytes(done)}" }
                            " of "
                            span { class: "mono", "{format::bytes(total)}" }
                            span { class: "spacer" }
                            button {
                                class: "button small",
                                title: "The download goes on. The mic and Settings show it.",
                                onclick: close,
                                "Hide"
                            }
                            button {
                                class: "button small",
                                onclick: move |_| voice.send(VoiceCommand::CancelDownload),
                                "Cancel"
                            }
                        }
                    },
                    ModelStatus::Ready => rsx! {
                        h3 { "The speech model is ready" }
                        p { "Hold {keys} and talk, or click the mic." }
                        div { class: "voice-card-actions",
                            button { class: "button small primary", onclick: close, "OK" }
                        }
                    },
                },
                VoiceCard::Failed(why) => rsx! {
                    h3 { "Dictation stopped" }
                    p { "{why}" }
                    div { class: "voice-card-actions",
                        button { class: "button small", onclick: close, "OK" }
                    }
                },
            }
        }
    }
}

/// The model's download, as the app's update bar.
#[component]
pub fn DownloadBar(done: u64, total: u64) -> Element {
    let fraction = if total == 0 {
        0.0
    } else {
        (done as f64 / total as f64).min(1.0)
    };
    let percent = (fraction * 100.0).round();
    rsx! {
        span {
            class: "update-bar",
            role: "progressbar",
            aria_label: "Download of the speech model",
            aria_valuenow: "{percent}",
            span { style: "transform: scaleX({fraction})" }
        }
    }
}

/// The size of the speech model's download, as the card says it.
pub fn model_size() -> u64 {
    #[cfg(feature = "voice")]
    {
        crate::voice_models::total_size()
    }
    #[cfg(not(feature = "voice"))]
    {
        0
    }
}

/// Above the message while a dictation runs into it: the level and time, then the text, the
/// words since the last pause lighter.
#[component]
pub fn VoiceStrip(agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    let voice: VoiceDesk = use_context();
    let (phase, live, dictating) = {
        let state = voice.state.read();
        (state.phase, state.live.clone(), state.dictating())
    };
    let open = dictating.filter(|&on| on == agent);
    let Some(shown) = use_agent_presence(Some(agent), open) else {
        return rsx! {};
    };
    let hint = {
        let state = cockpit.state.read();
        stop_hint(
            phase,
            state.prefs.voice.after,
            &state.prefs.keys.shown(Command::Talk),
        )
    };
    let finishing = matches!(phase, VoicePhase::Finishing { .. });
    let listening = live.said.is_empty() && live.hearing.is_empty();
    rsx! {
        div { class: shown.class("voice-strip"), role: "status", aria_label: "Dictation",
            div {
                id: STRIP_CLIP_ID,
                class: "voice-strip-clip",
                onmounted: move |_| {
                    let script = document::eval(STRIP_HEIGHT_SCRIPT);
                    if let Err(error) = script.send(STRIP_CLIP_ID) {
                        tracing::warn!("cannot animate the dictation strip's height: {error}");
                    }
                },
                div { class: "voice-strip-body",
                    div { class: "voice-strip-head",
                        if finishing {
                            span { class: "voice-finishing",
                                span { class: "spinner", aria_hidden: true }
                                "Writing the last words"
                            }
                        } else {
                            span { class: "voice-dot", aria_hidden: true }
                            LevelMeter {}
                            if let VoicePhase::Recording { since, .. } = phase {
                                RecordingTime { since }
                            }
                        }
                        span { class: "spacer" }
                        span { class: "voice-hint",
                            "{hint}"
                            kbd { "Esc" }
                            " cancels"
                        }
                    }
                    div { class: "voice-live",
                        if listening && !finishing {
                            span { class: "voice-hearing", "Listening…" }
                        }
                        span { class: "voice-said", "{live.said}" }
                        if !live.said.is_empty() && !live.hearing.is_empty() {
                            " "
                        }
                        span { class: if finishing { "voice-said" } else { "voice-hearing" }, "{live.hearing}" }
                    }
                }
            }
        }
    }
}

/// How the recording stops, and what its text then does, as the strip says it.
fn stop_hint(phase: VoicePhase, after: AfterDictation, keys: &str) -> String {
    let (verb, verbs) = match after {
        AfterDictation::Insert => ("insert", "inserts"),
        AfterDictation::InsertAndSend => ("send", "sends"),
    };
    match phase {
        VoicePhase::Recording { latched: false, .. } => format!("Release {keys} to {verb} · "),
        VoicePhase::Recording { latched: true, .. } => format!("{keys} again {verbs} · "),
        VoicePhase::Finishing { .. } | VoicePhase::Idle => String::new(),
    }
}

/// The microphone's level, as bars. It reads the level signal alone, so that only the meter
/// draws again 30 times a second.
#[component]
fn LevelMeter() -> Element {
    let voice: VoiceDesk = use_context();
    let level = (voice.level)();
    // From -60 dB, the quiet of a room, to 0 dB.
    let loudness = ((20.0 * level.max(1e-6).log10() + 60.0) / 60.0).clamp(0.0, 1.0);
    rsx! {
        span { class: "voice-meter", aria_hidden: true,
            for (index, weight) in BARS.iter().enumerate() {
                i { key: "{index}", style: "transform: scaleY({(loudness * weight).max(0.15)})" }
            }
        }
    }
}

/// The recording's time, which the app's clock updates each second.
#[component]
fn RecordingTime(since: Instant) -> Element {
    let clock: Clock = use_context();
    // Read for the tick alone.
    let _ = (clock.0)();
    let seconds = Instant::now().saturating_duration_since(since).as_secs();
    rsx! {
        span { class: "voice-time mono", "{seconds / 60}:{seconds % 60:02}" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strip_says_whether_the_stop_sends() {
        let held = VoicePhase::Recording {
            agent: AgentId::new(1),
            since: Instant::now(),
            latched: false,
        };
        let tapped = VoicePhase::Recording {
            agent: AgentId::new(1),
            since: Instant::now(),
            latched: true,
        };

        assert_eq!(
            stop_hint(held, AfterDictation::Insert, "Ctrl+Space"),
            "Release Ctrl+Space to insert · "
        );
        assert_eq!(
            stop_hint(held, AfterDictation::InsertAndSend, "Ctrl+Space"),
            "Release Ctrl+Space to send · "
        );
        assert_eq!(
            stop_hint(tapped, AfterDictation::InsertAndSend, "Ctrl+Space"),
            "Ctrl+Space again sends · "
        );
        assert_eq!(
            stop_hint(
                VoicePhase::Finishing {
                    agent: AgentId::new(1)
                },
                AfterDictation::Insert,
                "Ctrl+Space"
            ),
            ""
        );
    }
}
