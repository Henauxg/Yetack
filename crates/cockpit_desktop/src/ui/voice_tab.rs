//! Settings' Voice tab: dictation on or off, its key, what happens once it stops, the speech
//! model, and how often the live text comes up to date.

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::format;
use crate::keymap::Command;
use crate::prefs::{AfterDictation, LiveTextRefresh, VoicePrefs};
use crate::state::voice::{DownloadFrom, ModelStatus};
use crate::state::{RefusedChord, settings};
use crate::ui::settings::CommandRow;
use crate::ui::voice::{DownloadBar, model_size};
use crate::voice_desk::{VoiceCommand, VoiceDesk};

const DICTATION_LABEL_ID: &str = "voice-dictation-label";
const AFTER_LABEL_ID: &str = "voice-after-label";
const MODEL_LABEL_ID: &str = "voice-model-label";
const LIVE_LABEL_ID: &str = "voice-live-label";

fn after_label(after: AfterDictation) -> &'static str {
    match after {
        AfterDictation::Insert => "Insert the text",
        AfterDictation::InsertAndSend => "Insert and send",
    }
}

fn live_label(live: LiveTextRefresh) -> &'static str {
    match live {
        LiveTextRefresh::Auto => "Auto",
        LiveTextRefresh::EverySecond => "Every second",
        LiveTextRefresh::Off => "At each pause",
    }
}

#[component]
pub fn VoiceTab(recording: Option<Command>, refused: Option<RefusedChord>) -> Element {
    use strum::VariantArray;

    let cockpit: Cockpit = use_context();
    let voice: VoiceDesk = use_context();
    let (prefs, keys): (VoicePrefs, _) = {
        let state = cockpit.state.read();
        (state.prefs.voice, state.prefs.keys.clone())
    };
    let model = voice.state.read().model;
    rsx! {
        div { class: "dialog-body voice-tab",
            h3 { class: "settings-title", "Dictation" }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: DICTATION_LABEL_ID, class: "setting-label", "Dictation" }
                    span { class: "setting-help",
                        "Talk instead of typing. The text goes in the message at the cursor. Off hides the mic and frees its key."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: DICTATION_LABEL_ID,
                    for (enabled, label) in [(true, "On"), (false, "Off")] {
                        button {
                            key: "{label}",
                            class: if enabled == prefs.enabled { "segment chosen" } else { "segment" },
                            aria_pressed: enabled == prefs.enabled,
                            onclick: move |_| {
                                cockpit.update(|state| settings::set_voice_enabled(state, enabled));
                                // Off hides the mic and frees the key that could stop it.
                                if !enabled {
                                    voice.send(VoiceCommand::Cancel);
                                }
                            },
                            "{label}"
                        }
                    }
                }
            }
            CommandRow {
                command: Command::Talk,
                chord: keys.chord(Command::Talk),
                changed: keys.is_changed(Command::Talk),
                recording: recording == Some(Command::Talk),
                refused: refused.filter(|refused| refused.command == Command::Talk),
            }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: AFTER_LABEL_ID, class: "setting-label", "When you stop" }
                    span { class: "setting-help",
                        "Insert and send puts the text at the end of the message and sends it."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: AFTER_LABEL_ID,
                    for &after in AfterDictation::VARIANTS {
                        button {
                            key: "{after_label(after)}",
                            class: if after == prefs.after { "segment chosen" } else { "segment" },
                            aria_pressed: after == prefs.after,
                            onclick: move |_| cockpit.update(|state| settings::set_voice_after(state, after)),
                            "{after_label(after)}"
                        }
                    }
                }
            }
            h3 { class: "settings-title", "Speech model" }
            div { class: "setting-row voice-model-row",
                div { class: "setting-text",
                    span { id: MODEL_LABEL_ID, class: "setting-label", "Parakeet Ultra" }
                    span { class: "setting-help",
                        "English and 24 other European languages, on this computer: your voice never leaves it. {format::bytes(model_size())}. By Moondream, after NVIDIA's Parakeet. License CC-BY-4.0."
                    }
                }
                div { class: "voice-model-state", aria_labelledby: MODEL_LABEL_ID,
                    match model {
                        ModelStatus::Ready => rsx! {
                            span { class: "voice-model-ready",
                                span { class: "voice-ok", aria_hidden: true }
                                "On this computer"
                            }
                            button {
                                class: "button small",
                                onclick: move |_| voice.send(VoiceCommand::RemoveModel),
                                "Remove"
                            }
                        },
                        ModelStatus::Missing => rsx! {
                            button {
                                class: "button small primary",
                                onclick: move |_| voice.send(VoiceCommand::Download(DownloadFrom::Settings)),
                                "Download"
                            }
                        },
                        ModelStatus::Downloading { done, total, .. } => rsx! {
                            div { class: "voice-model-progress",
                                DownloadBar { done, total }
                                div { class: "voice-card-count",
                                    span { class: "mono", "{format::bytes(done)}" }
                                    " of "
                                    span { class: "mono", "{format::bytes(total)}" }
                                    span { class: "spacer" }
                                    button {
                                        class: "button small",
                                        onclick: move |_| voice.send(VoiceCommand::CancelDownload),
                                        "Cancel"
                                    }
                                }
                            }
                        },
                    }
                }
            }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: LIVE_LABEL_ID, class: "setting-label", "Words while you talk" }
                    span { class: "setting-help",
                        "The strip above the message shows your words as you say them. Auto updates them as often as this computer allows: every half second on a fast one, every second or two on a slower one. At each pause updates them only when you stop for a moment, which takes the least work."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: LIVE_LABEL_ID,
                    for &live in LiveTextRefresh::VARIANTS {
                        button {
                            key: "{live_label(live)}",
                            class: if live == prefs.live { "segment chosen" } else { "segment" },
                            aria_pressed: live == prefs.live,
                            onclick: move |_| cockpit.update(|state| settings::set_voice_live(state, live)),
                            "{live_label(live)}"
                        }
                    }
                }
            }
        }
    }
}
