//! Dictation in the composer: the mic button, the strip that shows the recording and its text,
//! and the card for the model's download, a failure, or a microphone that gave way to the system
//! default. A dictation into another agent shows a strip that leads back to it. A quote's
//! comment, a question's "Other" answer and the new agent's first message have a mic and a row of
//! words of their own.

use std::time::Instant;

use cockpit_protocol::{AgentId, AgentStatus};
use dioxus::prelude::*;

use crate::bridge::{self, NEW_AGENT_PROMPT_ID};
use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::keymap::Command;
use crate::prefs::{AfterDictation, DictationLanguage};
use crate::state::actions;
use crate::state::voice::{
    self, Destination, DownloadFrom, Field, LiveWords, MicMissing, ModelStatus, VoiceCard,
    VoicePhase, VoiceState,
};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::{use_agent_presence, use_presence};
use crate::ui::{COMMENT_FIELD_ID, answer_field_id, dialog_height};
use crate::voice_desk::{VoiceCommand, VoiceDesk};

/// How the level meter's bars stand against each other, so that one level reads as a voice.
const BARS: [f32; 12] = [
    0.45, 0.7, 0.95, 0.75, 1.0, 0.8, 0.55, 0.85, 0.65, 0.9, 0.5, 0.6,
];

/// The strip's clip, whose height follows its text.
const STRIP_CLIP_ID: &str = "voice-strip-clip";

/// The mic, before Send: a click starts a dictation, which runs until the next click. It is off
/// for an exited agent, as the message field is. The card shows above it, except while the new
/// agent dialog shows its first message: the dialog's mic then shows the card.
#[component]
pub fn MicButton(agent: AgentId, exited: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let voice: VoiceDesk = use_context();
    let (enabled, after, keys, card) = {
        let state = cockpit.state.read();
        (
            state.prefs.voice.enabled,
            state.prefs.voice.after,
            state.prefs.keys.shown(Command::Talk),
            !voice::new_agent_prompt_shown(&state),
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
    let here = Destination::Message { agent };
    let recording = matches!(phase, VoicePhase::Recording { to, .. } if to == here);
    let finishing = matches!(phase, VoicePhase::Finishing { to, .. } if to == here);
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
                onclick: move |_| voice.send(VoiceCommand::Click(Field::Message)),
                Icon { kind: IconKind::Mic, size: 16 }
                if let Some(badge) = badge {
                    span {
                        class: if badge.leaving { "mic-badge leaving" } else { "mic-badge" },
                        aria_hidden: true,
                        Icon { kind: IconKind::Download, size: 10 }
                    }
                }
            }
            if card {
                VoiceCardPopover {}
            }
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
    let (keys, language) = {
        let state = cockpit.state.read();
        (
            state.prefs.keys.shown(Command::Talk),
            state.prefs.voice.language,
        )
    };
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
                            "{model_name(language)}, {format::bytes(model_size(language))}, downloaded once. It runs on this computer: your voice never leaves it."
                            if language == DictationLanguage::English {
                                " For other languages, choose {language_label(DictationLanguage::Multilingual)} in Settings."
                            }
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
                VoiceCard::Fallback { chosen, using, missing } => rsx! {
                    h3 { "Recording from the system default" }
                    p { "{fallback_sentence(&chosen, using.as_deref(), missing)}" }
                    div { class: "voice-card-actions",
                        button { class: "button small", onclick: close, "OK" }
                    }
                },
            }
        }
    }
}

/// Why the dictation records from the system default, as the card says it.
fn fallback_sentence(chosen: &str, using: Option<&str>, missing: MicMissing) -> String {
    let using = using.unwrap_or("the system default");
    match missing {
        MicMissing::AtStart => {
            format!("{chosen} is not connected, so this dictation uses {using}.")
        }
        MicMissing::MidWay => {
            format!("{chosen} was disconnected. The dictation goes on with {using}.")
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

/// The language as Settings' choice names it.
pub fn language_label(language: DictationLanguage) -> &'static str {
    match language {
        DictationLanguage::English => "English",
        DictationLanguage::Multilingual => "25 languages",
    }
}

/// The name of the speech model that transcribes `language`.
pub fn model_name(language: DictationLanguage) -> &'static str {
    match language {
        DictationLanguage::English => "Parakeet unified EN",
        DictationLanguage::Multilingual => "Parakeet Ultra",
    }
}

/// The size of the download of the speech model that transcribes `language`.
pub fn model_size(language: DictationLanguage) -> u64 {
    #[cfg(feature = "voice")]
    {
        crate::voice_models::total_size(crate::voice_models::speech_model(language))
    }
    #[cfg(not(feature = "voice"))]
    {
        let _ = language;
        0
    }
}

/// Above the message while a dictation runs into it: the level and time, then the text, the
/// words since the last pause lighter. A dictation into a comment shows in the comment's box.
#[component]
pub fn VoiceStrip(agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    let voice: VoiceDesk = use_context();
    let (phase, live, writing) = {
        let state = voice.state.read();
        (state.phase, state.live.clone(), state.writing())
    };
    let open = writing
        .filter(|&to| to == Destination::Message { agent })
        .and_then(Destination::agent);
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
    rsx! {
        div { class: shown.class("voice-strip"), role: "status", aria_label: "Dictation",
            div {
                id: STRIP_CLIP_ID,
                class: "voice-strip-clip",
                onmounted: move |_| dialog_height::follow_content(STRIP_CLIP_ID),
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
                        LiveText { live, finishing }
                    }
                }
            }
        }
    }
}

/// Above the message of the agent on screen while a dictation runs into another one: which
/// agent, a button back to it, and the keys that stop and cancel. Enter here sends this agent's
/// message, not the dictation.
#[component]
pub fn AwayStrip(agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    let voice: VoiceDesk = use_context();
    let away = {
        let state = voice.state.read();
        state
            .dictating()
            .filter(|&dictating| dictating != agent)
            .map(|dictating| (dictating, state.phase))
    };
    let shown = use_agent_presence(Some(agent), away.map(|_| agent));
    // The agent it leads back to and the phase, kept for the exit: after Esc the phase is idle.
    let kept = use_presence(away);
    let (Some(shown), Some(kept)) = (shown, kept) else {
        return rsx! {};
    };
    let (to, phase) = kept.value;
    let (title, hint) = {
        let state = cockpit.state.read();
        // An agent closed during the dictation has no title, and nothing to go back to.
        let title = state
            .agents
            .get(&to)
            .map(|entry| entry.summary.title.clone());
        let hint = away_hint(
            phase,
            state.prefs.voice.after,
            &state.prefs.keys.shown(Command::Talk),
        );
        (title, hint)
    };
    rsx! {
        div {
            class: shown.class("voice-strip"),
            role: "status",
            aria_label: "Dictation into another agent",
            div { class: "voice-strip-clip",
                div { class: "voice-strip-body",
                    div { class: "voice-strip-head",
                        match phase {
                            VoicePhase::Recording { since, .. } => rsx! {
                                span { class: "voice-dot", aria_hidden: true }
                                LevelMeter {}
                                RecordingTime { since }
                                span { class: "voice-away",
                                    if let Some(title) = &title {
                                        "Dictating into "
                                        b { "{title}" }
                                    } else {
                                        "Dictating into a closed agent"
                                    }
                                }
                            },
                            VoicePhase::Finishing { .. } | VoicePhase::Idle => rsx! {
                                span { class: "voice-finishing",
                                    span { class: "spinner", aria_hidden: true }
                                    span { class: "voice-away",
                                        if let Some(title) = &title {
                                            "Writing the last words for "
                                            b { "{title}" }
                                        } else {
                                            "Writing the last words"
                                        }
                                    }
                                }
                            },
                        }
                        if let Some(title) = &title {
                            button {
                                class: "button small voice-go-back",
                                title: "Show {title}",
                                onclick: move |_| cockpit.update(|state| actions::select_agent(state, to)),
                                Icon { kind: IconKind::ArrowLeft, size: 12 }
                                "Go back"
                            }
                        }
                        span { class: "spacer" }
                        span { class: "voice-hint",
                            "{hint}"
                            kbd { "Esc" }
                            " cancels"
                        }
                    }
                }
            }
        }
    }
}

/// The words so far, the words since the last pause lighter, or "Listening…" before the first.
/// Once the recording stops, every word is final.
#[component]
fn LiveText(live: LiveWords, finishing: bool) -> Element {
    let listening = live.said.is_empty() && live.hearing.is_empty();
    rsx! {
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

/// The mic of a field besides the message: a quote's comment, a question's "Other" answer, or the
/// new agent's first message. A click starts a dictation into the field, which runs until the
/// next click, and gives the field the focus, so that Enter and Esc reach it. It is off for an
/// exited agent, as the message's mic is. The new agent dialog's mic has the card above it, since
/// the dialog covers the message's.
#[component]
pub fn FieldMic(to: Destination) -> Element {
    let cockpit: Cockpit = use_context();
    let voice: VoiceDesk = use_context();
    let (enabled, exited, keys) = {
        let state = cockpit.state.read();
        let exited = to.agent().is_some_and(|agent| {
            state
                .agents
                .get(&agent)
                .is_some_and(|entry| entry.summary.status == AgentStatus::Exited)
        });
        (
            state.prefs.voice.enabled,
            exited,
            state.prefs.keys.shown(Command::Talk),
        )
    };
    let (available, phase) = {
        let voice = voice.state.read();
        (voice.available, voice.phase)
    };
    if !available || !enabled {
        return rsx! {};
    }
    let recording = matches!(phase, VoicePhase::Recording { to: into, .. } if into == to);
    let finishing = matches!(phase, VoicePhase::Finishing { to: into, .. } if into == to);
    let off = exited && !recording;
    let (size, icon) = match to {
        Destination::NewAgent => ("small", 16),
        Destination::Message { .. } | Destination::Comment { .. } | Destination::Answer { .. } => {
            ("tiny", 13)
        }
    };
    let class = match (recording, finishing) {
        (true, _) => format!("icon-button {size} mic-button recording"),
        (_, true) => format!("icon-button {size} mic-button finishing"),
        _ => format!("icon-button {size} mic-button"),
    };
    let what = field_name(to);
    let title = if recording {
        format!("Stop: the text goes in {what}")
    } else if off {
        "Resume the agent to dictate".to_owned()
    } else {
        format!("Dictate {what}: hold {keys}, or click")
    };
    let field = to.field();
    let field_id = field_id(to);
    let button = rsx! {
        button {
            class,
            aria_label: if recording { "Stop dictating".to_owned() } else { format!("Dictate {what}") },
            aria_pressed: recording,
            disabled: off,
            title,
            // The field keeps its caret, where the text goes.
            onmousedown: move |event| event.prevent_default(),
            onclick: move |_| {
                bridge::focus_keeping_caret(&field_id);
                voice.send(VoiceCommand::Click(field));
            },
            Icon { kind: IconKind::Mic, size: icon }
        }
    };
    match to {
        Destination::NewAgent => rsx! {
            span { class: "mic-anchor",
                {button}
                VoiceCardPopover {}
            }
        },
        Destination::Message { .. } | Destination::Comment { .. } | Destination::Answer { .. } => {
            button
        }
    }
}

/// Returns true when a dictation runs or finishes into a field that `writes_into` accepts, and
/// stops it as Enter does. The button that would send the field's text then sends nothing: its
/// next press sends the text with the words in.
pub fn stop_dictation_first(voice: VoiceDesk, writes_into: impl Fn(Destination) -> bool) -> bool {
    let writing = voice.state.peek().writing();
    if !writing.is_some_and(writes_into) {
        return false;
    }
    voice.send(VoiceCommand::Stop);
    true
}

/// The field `to` names, as the mic's title says it.
fn field_name(to: Destination) -> &'static str {
    match to {
        Destination::Message { .. } => "the message",
        Destination::Comment { .. } => "the comment",
        Destination::Answer { .. } => "the answer",
        Destination::NewAgent => "the first message",
    }
}

/// The id of the field `to` names.
fn field_id(to: Destination) -> String {
    match to {
        Destination::Message { .. } => bridge::COMPOSER_ID.to_owned(),
        Destination::Comment { .. } => COMMENT_FIELD_ID.to_owned(),
        Destination::Answer { agent, item, asked } => answer_field_id(agent, item, asked),
        Destination::NewAgent => NEW_AGENT_PROMPT_ID.to_owned(),
    }
}

/// The words of the dictation that runs or finishes into `to`, and whether it finishes.
fn words_into(state: &VoiceState, to: Destination) -> Option<(LiveWords, bool)> {
    match state.phase {
        VoicePhase::Recording { to: into, .. } if into == to => Some((state.live.clone(), false)),
        VoicePhase::Finishing { to: into, .. } if into == to => Some((state.live.clone(), true)),
        VoicePhase::Idle | VoicePhase::Recording { .. } | VoicePhase::Finishing { .. } => None,
    }
}

/// Under a field besides the message while a dictation runs into it: the level, the words, and
/// the keys that stop and cancel. It plays its exit with the last words.
#[component]
pub fn FieldWords(to: Destination) -> Element {
    let voice: VoiceDesk = use_context();
    let shown = words_into(&voice.state.read(), to);
    let Some(shown) = use_presence(shown) else {
        return rsx! {};
    };
    let (live, finishing) = shown.value;
    rsx! {
        div {
            class: if shown.leaving { "field-voice leaving" } else { "field-voice" },
            role: "status",
            aria_label: "Dictation into {field_name(to)}",
            if finishing {
                span { class: "spinner", aria_hidden: true }
            } else {
                LevelMeter {}
            }
            span { class: "field-voice-words",
                span { LiveText { live, finishing } }
            }
            span { class: "field-voice-keys",
                if !finishing {
                    kbd { "Enter" }
                    " stops · "
                }
                kbd { "Esc" }
                " cancels"
            }
        }
    }
}

/// [`FieldWords`] in a row of their own, under a field with no key hints to give way to: the row
/// opens and closes with the dictation, and its height follows the words. A row open as it
/// mounts, as when its agent comes on screen, draws at once.
#[component]
pub fn FieldWordsRow(to: Destination, clip_id: String) -> Element {
    let voice: VoiceDesk = use_context();
    let open = words_into(&voice.state.read(), to).is_some();
    let open_at_mount = use_hook(|| open);
    let Some(shown) = use_presence(open.then_some(())) else {
        return rsx! {};
    };
    let class = match (shown.leaving, open_at_mount && shown.opening == 1) {
        (true, _) => "field-voice-row leaving",
        (false, true) => "field-voice-row instant",
        (false, false) => "field-voice-row",
    };
    let follow = clip_id.clone();
    rsx! {
        div { class,
            div {
                id: "{clip_id}",
                class: "field-voice-clip",
                onmounted: move |_| dialog_height::follow_content(&follow),
                div { class: "field-voice-body",
                    FieldWords { to }
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
        VoicePhase::Recording { latched: true, .. } => format!("{keys} again or Enter {verbs} · "),
        VoicePhase::Finishing { .. } | VoicePhase::Idle => String::new(),
    }
}

/// How a dictation into another agent stops, as its strip says it: Enter here would send this
/// agent's message, so only the talk key stops it.
fn away_hint(phase: VoicePhase, after: AfterDictation, keys: &str) -> String {
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
        let to = Destination::Message {
            agent: AgentId::new(1),
        };
        let held = VoicePhase::Recording {
            to,
            since: Instant::now(),
            latched: false,
        };
        let tapped = VoicePhase::Recording {
            to,
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
            "Ctrl+Space again or Enter sends · "
        );
        assert_eq!(
            stop_hint(
                VoicePhase::Finishing { to, covered: false },
                AfterDictation::Insert,
                "Ctrl+Space"
            ),
            ""
        );
    }

    #[test]
    fn the_strip_of_another_agent_stops_with_the_talk_key_alone() {
        let to = Destination::Message {
            agent: AgentId::new(1),
        };
        let tapped = VoicePhase::Recording {
            to,
            since: Instant::now(),
            latched: true,
        };
        let held = VoicePhase::Recording {
            to,
            since: Instant::now(),
            latched: false,
        };

        assert_eq!(
            away_hint(tapped, AfterDictation::Insert, "Ctrl+Space"),
            "Ctrl+Space again inserts · "
        );
        assert_eq!(
            away_hint(held, AfterDictation::InsertAndSend, "Ctrl+Space"),
            "Release Ctrl+Space to send · "
        );
    }

    #[test]
    fn the_card_says_which_microphone_the_dictation_uses_and_why() {
        assert_eq!(
            fallback_sentence("Headset", Some("Built-in microphone"), MicMissing::AtStart),
            "Headset is not connected, so this dictation uses Built-in microphone."
        );
        assert_eq!(
            fallback_sentence("Headset", None, MicMissing::MidWay),
            "Headset was disconnected. The dictation goes on with the system default."
        );
    }
}
