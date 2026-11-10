//! The Settings dialog, from the gear in the sidebar's foot. Its General tab holds the app's
//! notifications, as the sidebar's bell sets them, and where comments on quotes show. Its Keyboard
//! tab lists every shortcut and records new ones for the commands.

use dioxus::prelude::*;
use strum::VariantArray;

use crate::connection::Cockpit;
use crate::format;
use crate::keymap::{self, Chord, Command, FixedShortcut, Group, Keymap, Refusal};
use crate::prefs::{CommentPlacement, NotificationLevel};
use crate::state::{RefusedChord, SettingsTab, SettingsView, notification_levels, settings};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::notification_levels::choice_label;

const NOTIFICATIONS_LABEL_ID: &str = "setting-notifications";
const COMMENTS_LABEL_ID: &str = "setting-comments";

/// What the key Esc reads as, which cancels a recording.
const ESCAPE_KEY: &str = "Escape";

fn placement_label(placement: CommentPlacement) -> &'static str {
    match placement {
        CommentPlacement::Beside => "Beside the text",
        CommentPlacement::Hidden => "Hidden",
    }
}

#[component]
pub fn SettingsDialog(view: SettingsView) -> Element {
    let cockpit: Cockpit = use_context();
    let close = move |_| cockpit.update(|state| state.ui.close_dialog());
    let tab = |tab: SettingsTab, icon: IconKind, label: &'static str| {
        let selected = view.tab == tab;
        rsx! {
            button {
                class: if selected { "dialog-tab selected" } else { "dialog-tab" },
                role: "tab",
                aria_selected: selected,
                onclick: move |_| cockpit.update(|state| settings::show_tab(state, tab)),
                Icon { kind: icon, size: 14 }
                "{label}"
            }
        }
    };
    rsx! {
        div {
            class: "dialog settings",
            role: "dialog",
            aria_label: "Settings",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "Settings" }
                button { class: "icon-button small", aria_label: "Close", onclick: close,
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            div { class: "dialog-tabs", role: "tablist", aria_label: "Settings",
                {tab(SettingsTab::General, IconKind::Gear, "General")}
                {tab(SettingsTab::Keyboard, IconKind::Keyboard, "Keyboard")}
            }
            match view.tab {
                SettingsTab::General => rsx! {
                    GeneralTab {}
                },
                SettingsTab::Keyboard => rsx! {
                    KeyboardTab { recording: view.recording, refused: view.refused }
                },
            }
        }
    }
}

#[component]
fn GeneralTab() -> Element {
    let cockpit: Cockpit = use_context();
    let (notifications, comments) = {
        let state = cockpit.state.read();
        (
            notification_levels::app_level(&state),
            state.prefs.chat.comments,
        )
    };
    rsx! {
        div { class: "dialog-body",
            h3 { class: "settings-title", "Notifications" }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: NOTIFICATIONS_LABEL_ID, class: "setting-label", "Notifications for the app" }
                    span { class: "setting-help",
                        "The bell in the sidebar sets it too. Each agent keeps its own setting, from its row's menu, which applies again once the app's are on."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: NOTIFICATIONS_LABEL_ID,
                    for &level in NotificationLevel::VARIANTS {
                        button {
                            key: "{choice_label(level)}",
                            class: if level == notifications { "segment chosen" } else { "segment" },
                            aria_pressed: level == notifications,
                            onclick: move |_| cockpit.set_notifications(None, level),
                            "{choice_label(level)}"
                        }
                    }
                }
            }
            h3 { class: "settings-title", "Chat" }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: COMMENTS_LABEL_ID, class: "setting-label", "Comments on quotes" }
                    span { class: "setting-help",
                        "Where a new comment shows while it waits to be sent. Hidden, it shows as its quote's number in the text: hover the number to read it. You can still show or hide each comment."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: COMMENTS_LABEL_ID,
                    for &placement in CommentPlacement::VARIANTS {
                        button {
                            key: "{placement_label(placement)}",
                            class: if placement == comments { "segment chosen" } else { "segment" },
                            aria_pressed: placement == comments,
                            onclick: move |_| cockpit.update(|state| settings::set_comment_placement(state, placement)),
                            "{placement_label(placement)}"
                        }
                    }
                }
            }
        }
    }
}

/// Whether a shortcut shows under the filter: its label or its keys hold the filter's text.
fn matches_filter(filter: &str, label: &str, keys: &str) -> bool {
    let filter = filter.trim().to_lowercase();
    filter.is_empty()
        || label.to_lowercase().contains(&filter)
        || keys.to_lowercase().contains(&filter)
}

fn fixed_keys_text(shortcut: &FixedShortcut) -> String {
    let chords: Vec<String> = shortcut.keys.iter().map(|caps| caps.join("+")).collect();
    chords.join(" ")
}

#[component]
fn KeyboardTab(recording: Option<Command>, refused: Option<RefusedChord>) -> Element {
    let cockpit: Cockpit = use_context();
    let mut filter = use_signal(String::new);
    let keys: Keymap = cockpit.state.read().prefs.keys.clone();
    let changed = keys.changed_count();
    let changed_text = if changed == 0 {
        "Every shortcut has its default".to_owned()
    } else {
        format::count(changed, "shortcut changed", "shortcuts changed")
    };
    let filter_text = filter();
    let groups: Vec<(Group, Vec<Command>, Vec<FixedShortcut>)> = Group::VARIANTS
        .iter()
        .map(|&group| {
            let commands: Vec<Command> = Command::VARIANTS
                .iter()
                .copied()
                .filter(|command| command.group() == group)
                .filter(|&command| {
                    matches_filter(&filter_text, command.label(), &keys.shown(command))
                })
                .collect();
            let fixed: Vec<FixedShortcut> = keymap::FIXED
                .iter()
                .copied()
                .filter(|shortcut| shortcut.group == group)
                .filter(|shortcut| {
                    matches_filter(&filter_text, shortcut.label, &fixed_keys_text(shortcut))
                })
                .collect();
            (group, commands, fixed)
        })
        .filter(|(_, commands, fixed)| !commands.is_empty() || !fixed.is_empty())
        .collect();
    rsx! {
        div { class: "dialog-body keyboard-tab",
            input {
                class: "text-field shortcut-filter",
                r#type: "search",
                placeholder: "Find a shortcut, by name or key",
                aria_label: "Find a shortcut",
                value: "{filter_text}",
                oninput: move |event| filter.set(event.value()),
            }
            if groups.is_empty() {
                p { class: "shortcut-note", "No shortcut matches." }
            }
            for (group, commands, fixed) in groups {
                div { key: "{group.title()}", class: "shortcut-group", role: "group", aria_label: group.title(),
                    h3 { class: "settings-title", "{group.title()}" }
                    for command in commands {
                        CommandRow {
                            key: "{<&str>::from(command)}",
                            command,
                            chord: keys.chord(command),
                            changed: keys.is_changed(command),
                            recording: recording == Some(command),
                            refused: refused.clone().filter(|refused| refused.command == command),
                        }
                    }
                    for shortcut in fixed {
                        FixedRow { key: "{shortcut.label}", label: shortcut.label, context: shortcut.context, keys: shortcut.keys }
                    }
                }
            }
            p { class: "shortcut-note",
                "A shortcut needs Ctrl or Alt, or one of F1 to F12, so that it never takes a key typed in a field. The shortcuts with a lock keep their keys."
            }
        }
        div { class: "settings-foot",
            span { "{changed_text}" }
            button {
                class: "link-button",
                disabled: changed == 0,
                onclick: move |_| cockpit.update(settings::reset_all),
                "Reset all to the defaults"
            }
        }
    }
}

/// The key caps of a chord.
#[component]
fn KeyCaps(caps: Vec<String>) -> Element {
    rsx! {
        for (index, cap) in caps.into_iter().enumerate() {
            kbd { key: "{index}", class: "key-cap", "{cap}" }
        }
    }
}

/// A command's shortcut: a click records a new one, the next key pressed with its modifiers.
#[component]
fn CommandRow(
    command: Command,
    chord: Chord,
    changed: bool,
    recording: bool,
    refused: Option<RefusedChord>,
) -> Element {
    let cockpit: Cockpit = use_context();
    let default = command.default_chord().shown();
    let record = move |event: KeyboardEvent| {
        event.prevent_default();
        let modifiers = event.modifiers();
        let key = event.key().to_string();
        let ctrl = modifiers.ctrl() || modifiers.meta();
        let (alt, shift) = (modifiers.alt(), modifiers.shift());
        if key == ESCAPE_KEY && !ctrl && !alt && !shift {
            cockpit.update(settings::stop_recording);
            return;
        }
        if let Some(chord) = Chord::of(&key, ctrl, alt, shift) {
            cockpit.update(|state| settings::press_chord(state, chord));
        }
    };
    rsx! {
        div { class: if changed { "shortcut-row changed" } else { "shortcut-row" },
            span { class: "shortcut-name",
                "{command.label()}"
                if changed {
                    span { class: "shortcut-context", " · changed" }
                }
            }
            span { class: "shortcut-keys",
                if recording {
                    button {
                        class: "binding recording",
                        "data-records-keys": true,
                        aria_label: "Press the new shortcut for {command.label()}, or Esc to cancel",
                        onmounted: move |event| async move {
                            // The row may have stopped recording already: nothing to focus then.
                            let _ = event.data().set_focus(true).await;
                        },
                        onkeydown: record,
                        onclick: move |_| cockpit.update(settings::stop_recording),
                        "Press the new keys · Esc cancels"
                    }
                } else {
                    button {
                        class: "binding",
                        title: "Change the shortcut",
                        aria_label: "{command.label()}: {chord.shown()}. Change it",
                        onclick: move |_| cockpit.update(|state| settings::toggle_recording(state, command)),
                        KeyCaps { caps: chord.caps() }
                    }
                }
                span { class: "shortcut-end",
                    if changed {
                        button {
                            class: "icon-button tiny binding-reset",
                            title: "Back to {default}",
                            aria_label: "Back to {default}",
                            onclick: move |_| cockpit.update(|state| settings::reset(state, command)),
                            Icon { kind: IconKind::Undo, size: 13 }
                        }
                    }
                }
            }
        }
        if let Some(RefusedChord { chord, refusal, .. }) = refused {
            div { class: "shortcut-refused", role: "alert",
                "{refusal.message(&chord)}"
                if matches!(refusal, Refusal::Taken(_)) {
                    button {
                        class: "link-button",
                        onclick: move |_| cockpit.update(settings::swap_refused),
                        "Swap them"
                    }
                }
            }
        }
    }
}

/// A shortcut whose meaning comes from where it is pressed: listed, never changed.
#[component]
fn FixedRow(
    label: &'static str,
    context: Option<&'static str>,
    keys: &'static [&'static [&'static str]],
) -> Element {
    rsx! {
        div { class: "shortcut-row fixed", title: "This shortcut keeps its keys",
            span { class: "shortcut-name",
                "{label}"
                if let Some(context) = context {
                    span { class: "shortcut-context", " · {context}" }
                }
            }
            span { class: "shortcut-keys",
                for (index, chord) in keys.iter().enumerate() {
                    span { key: "{index}", class: "chord",
                        KeyCaps { caps: chord.iter().map(|&cap| cap.to_owned()).collect::<Vec<String>>() }
                    }
                }
                span { class: "shortcut-end shortcut-lock", aria_label: "Fixed",
                    Icon { kind: IconKind::Lock, size: 12 }
                }
            }
        }
    }
}
