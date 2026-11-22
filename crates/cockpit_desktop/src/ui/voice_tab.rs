//! Settings' Voice tab: dictation on or off, its key, what happens once it stops, the
//! microphone, the language and with it the speech model, and how often the live text comes up
//! to date.

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::format;
use crate::keymap::Command;
use crate::prefs::{AfterDictation, DictationLanguage, LiveTextRefresh, VoicePrefs};
use crate::state::voice::{DownloadFrom, MicrophoneList, ModelStatus};
use crate::state::{RefusedChord, settings};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::Presence;
use crate::ui::settings::CommandRow;
use crate::ui::voice::{DownloadBar, language_label, model_name, model_size};
use crate::voice_desk::{VoiceCommand, VoiceDesk};

const DICTATION_LABEL_ID: &str = "voice-dictation-label";
const AFTER_LABEL_ID: &str = "voice-after-label";
const MICROPHONE_LABEL_ID: &str = "voice-microphone-label";
const LANGUAGE_LABEL_ID: &str = "voice-language-label";
const MODEL_LABEL_ID: &str = "voice-model-label";
const LIVE_LABEL_ID: &str = "voice-live-label";

fn after_label(after: AfterDictation) -> &'static str {
    match after {
        AfterDictation::Insert => "Insert the text",
        AfterDictation::InsertAndSend => "Insert and send",
    }
}

/// What the model row says of the model, after its name.
fn model_help(language: DictationLanguage, size: &str) -> String {
    match language {
        DictationLanguage::English => format!(
            "English only, on this computer: your voice never leaves it. {size}. By NVIDIA. NVIDIA Open Model License."
        ),
        DictationLanguage::Multilingual => format!(
            "English and 24 other European languages, on this computer: your voice never leaves it. {size}. By Moondream, after NVIDIA's Parakeet. License CC-BY-4.0."
        ),
    }
}

/// The language that is not `language`.
fn other_language(language: DictationLanguage) -> DictationLanguage {
    match language {
        DictationLanguage::English => DictationLanguage::Multilingual,
        DictationLanguage::Multilingual => DictationLanguage::English,
    }
}

fn live_label(live: LiveTextRefresh) -> &'static str {
    match live {
        LiveTextRefresh::Auto => "Auto",
        LiveTextRefresh::EverySecond => "Every second",
        LiveTextRefresh::Off => "At each pause",
    }
}

/// The microphone's menu button, and its menu: the system default, then each microphone. A
/// chosen one that is not connected stays in the list, marked.
#[component]
fn MicrophoneChoice(
    chosen: Option<String>,
    microphones: Option<MicrophoneList>,
    mut menu: Signal<bool>,
) -> Element {
    let cockpit: Cockpit = use_context();
    let listed = microphones.is_some();
    let MicrophoneList { default, mut names } = microphones.unwrap_or_default();
    // Before the list arrives, the chosen one counts as connected.
    let missing = chosen
        .as_ref()
        .filter(|chosen| listed && !names.contains(chosen))
        .cloned();
    if let Some(missing) = &missing {
        names.push(missing.clone());
    }
    let open = menu();
    let mut pick = move |microphone: Option<String>| {
        menu.set(false);
        cockpit.update(|state| settings::set_microphone(state, microphone));
    };
    rsx! {
        span { class: "mic-choice-anchor",
            button {
                class: if open { "menu-button mic-choice raised" } else { "menu-button mic-choice" },
                aria_labelledby: MICROPHONE_LABEL_ID,
                aria_expanded: open,
                onclick: move |event| {
                    event.stop_propagation();
                    menu.set(!open);
                },
                span { class: "menu-button-label",
                    match (&chosen, &default) {
                        (Some(chosen), _) => rsx! {
                            "{chosen}"
                            if missing.is_some() {
                                span { class: "muted", " · not connected" }
                            }
                        },
                        (None, Some(default)) => rsx! {
                            "System default"
                            span { class: "muted", " · {default}" }
                        },
                        (None, None) => rsx! { "System default" },
                    }
                }
                Icon { kind: IconKind::ChevronDown, size: 12 }
            }
            Presence { open,
                div {
                    class: "popover menu compact-menu mic-menu",
                    onclick: move |event| event.stop_propagation(),
                    button {
                        class: if chosen.is_none() { "menu-item two-lines chosen" } else { "menu-item two-lines" },
                        aria_pressed: chosen.is_none(),
                        onclick: move |_| pick(None),
                        span { class: "mic-menu-line",
                            span { class: "menu-label", "System default" }
                            if chosen.is_none() {
                                Icon { kind: IconKind::Check, size: 14 }
                            }
                        }
                        if let Some(default) = &default {
                            span { class: "menu-sub", "{default}" }
                        }
                    }
                    if !names.is_empty() {
                        div { class: "menu-divider" }
                    }
                    for name in names {
                        {
                            let picked = chosen.as_ref() == Some(&name);
                            let absent = missing.as_ref() == Some(&name);
                            let value = name.clone();
                            rsx! {
                                button {
                                    key: "{name}",
                                    class: if picked { "menu-item chosen" } else { "menu-item" },
                                    aria_pressed: picked,
                                    onclick: move |_| pick(Some(value.clone())),
                                    span { class: "menu-label", "{name}" }
                                    if absent {
                                        span { class: "menu-tag", "not connected" }
                                    }
                                    if picked {
                                        Icon { kind: IconKind::Check, size: 14 }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
pub fn VoiceTab(recording: Option<Command>, refused: Option<RefusedChord>) -> Element {
    use strum::VariantArray;

    let cockpit: Cockpit = use_context();
    let voice: VoiceDesk = use_context();
    let (prefs, keys): (VoicePrefs, _) = {
        let state = cockpit.state.read();
        (state.prefs.voice.clone(), state.prefs.keys.clone())
    };
    let (model, other_model, microphones) = {
        let state = voice.state.read();
        (state.model, state.other_model, state.microphones.clone())
    };
    let language = prefs.language;
    let other = other_language(language);
    // Listed at each opening of the tab, so a microphone plugged in since then shows.
    use_hook(move || voice.send(VoiceCommand::ListMicrophones));
    let mut menu = use_signal(|| false);
    rsx! {
        div {
            class: "dialog-body voice-tab",
            onclick: move |_| {
                if menu() {
                    menu.set(false);
                }
            },
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
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: MICROPHONE_LABEL_ID, class: "setting-label", "Microphone" }
                    span { class: "setting-help",
                        "System default follows the microphone this computer uses, and a dictation goes on when it changes, as when you plug in a headset. A chosen microphone that is not connected gives way to the system default."
                    }
                }
                MicrophoneChoice { chosen: prefs.microphone, microphones, menu }
            }
            h3 { class: "settings-title", "Speech model" }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: LANGUAGE_LABEL_ID, class: "setting-label", "Language" }
                    span { class: "setting-help",
                        "English uses a model trained on English alone, which never switches to another language. 25 languages lets the model pick the language of each sentence: English, French, German, Spanish and 21 others."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: LANGUAGE_LABEL_ID,
                    for &choice in DictationLanguage::VARIANTS {
                        button {
                            key: "{language_label(choice)}",
                            class: if choice == language { "segment chosen" } else { "segment" },
                            aria_pressed: choice == language,
                            onclick: move |_| {
                                if choice != language {
                                    cockpit.update(|state| settings::set_voice_language(state, choice));
                                    voice.send(VoiceCommand::LanguageChanged);
                                }
                            },
                            "{language_label(choice)}"
                        }
                    }
                }
            }
            div { class: "setting-row voice-model-row",
                div { class: "setting-text",
                    span { id: MODEL_LABEL_ID, class: "setting-label", "{model_name(language)}" }
                    span { class: "setting-help",
                        "{model_help(language, &format::bytes(model_size(language)))}"
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
            Presence { open: other_model,
                div { class: "setting-row voice-other-model",
                    div { class: "setting-text",
                        span { class: "setting-help",
                            "{model_name(other)}, for the other choice of language, is also on this computer: {format::bytes(model_size(other))}."
                        }
                    }
                    div { class: "voice-model-state",
                        button {
                            class: "button small",
                            onclick: move |_| voice.send(VoiceCommand::RemoveOtherModel),
                            "Remove"
                        }
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
