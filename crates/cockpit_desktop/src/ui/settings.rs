//! The Settings dialog, from the gear in the sidebar's foot. Its General tab holds the app's
//! notifications, as the sidebar's bell sets them, where comments on quotes show, how long a message
//! sent to a working agent can still be cancelled, and whether a new agent's files pane shows. Its
//! last section, About, gives the app's version with a button that copies it. Its Keyboard tab
//! lists every shortcut and records new ones for the commands. Its Projects tab edits the projects
//! the sidebar can group agents by.

use std::sync::LazyLock;

use base64::engine::general_purpose::STANDARD as BASE64;
use dioxus::prelude::*;
use strum::VariantArray;

use crate::connection::Cockpit;
use crate::keymap::{self, Chord, Command, FixedShortcut, Group, Keymap, Refusal};
use crate::prefs::{
    CommentPlacement, EscStop, FirstMark, GroupBy, NotificationLevel, PaneState, ProjectSplit,
    SteerHold, ToolCallDetail,
};
use crate::state::{
    RefusedChord, SettingsTab, SettingsView, grouping, notification_levels, settings,
};
use crate::ui::code::{CopyButton, CopyLook};
use crate::ui::grouping::{group_by_label, project_split_label};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::notification_levels as levels_ui;
use crate::ui::presence::{Presence, Shown, use_presence};
use crate::ui::projects_tab::ProjectsTab;
use crate::update_script::APP_VERSION;
use crate::{APP_NAME, format};

static APP_ICON: LazyLock<String> = LazyLock::new(|| {
    use base64::Engine;

    let svg = include_bytes!("../../assets/agents-cockpit.svg");
    format!("data:image/svg+xml;base64,{}", BASE64.encode(svg))
});

const NOTIFICATIONS_LABEL_ID: &str = "setting-notifications";
const COMMENTS_LABEL_ID: &str = "setting-comments";
const STEER_HOLD_LABEL_ID: &str = "setting-steer-hold";
const ESC_STOP_LABEL_ID: &str = "setting-esc-stop";
const TOOL_CALLS_LABEL_ID: &str = "setting-tool-calls";
const FILES_PANE_LABEL_ID: &str = "setting-files-pane";
const GROUP_BY_LABEL_ID: &str = "setting-group-by";
const PROJECT_SPLIT_LABEL_ID: &str = "setting-project-split";
const FIRST_MARK_LABEL_ID: &str = "setting-first-mark";

fn first_mark_label(first: FirstMark) -> &'static str {
    match first {
        FirstMark::Kept => "Until you clear it",
        FirstMark::UntilMessage => "Until a message",
    }
}

fn pane_label(pane: PaneState) -> &'static str {
    match pane {
        PaneState::Open => "Open",
        PaneState::Closed => "Closed",
    }
}

fn hold_label(hold: SteerHold) -> &'static str {
    match hold {
        SteerHold::Off => "Off",
        SteerHold::ThreeSeconds => "3 s",
        SteerHold::FiveSeconds => "5 s",
        SteerHold::TenSeconds => "10 s",
    }
}

fn tool_calls_label(detail: ToolCallDetail) -> &'static str {
    match detail {
        ToolCallDetail::OneLine => "One line",
        ToolCallDetail::Full => "In full",
    }
}

fn esc_stop_label(esc_stop: EscStop) -> &'static str {
    match esc_stop {
        EscStop::Ask => "Ask",
        EscStop::AtOnce => "Stop at once",
    }
}

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
                {tab(SettingsTab::Projects, IconKind::Folder, "Projects")}
            }
            match view.tab {
                SettingsTab::General => rsx! {
                    GeneralTab {}
                },
                SettingsTab::Keyboard => rsx! {
                    KeyboardTab { recording: view.recording, refused: view.refused }
                },
                SettingsTab::Projects => rsx! {
                    ProjectsTab { view: view.clone() }
                },
            }
        }
    }
}

#[component]
fn GeneralTab() -> Element {
    let cockpit: Cockpit = use_context();
    let (
        group_by,
        project_split,
        notifications,
        comments,
        tool_calls,
        steer_hold,
        esc_stop,
        new_pane,
        pane_keys,
    ) = {
        let state = cockpit.state.read();
        (
            state.prefs.sidebar.group_by,
            state.prefs.sidebar.project_split,
            notification_levels::app_level(&state),
            state.prefs.chat.comments,
            state.prefs.chat.tool_calls,
            state.prefs.chat.steer_hold,
            state.prefs.chat.esc_stop,
            state.prefs.files_pane.new_agents,
            state.prefs.keys.shown(Command::FilesPane),
        )
    };
    let (first_mark, mark_keys) = {
        let state = cockpit.state.read();
        (
            state.prefs.sidebar.first_mark,
            state.prefs.keys.shown(Command::MarkToHandle),
        )
    };
    rsx! {
        div { class: "dialog-body",
            h3 { class: "settings-title", "Sidebar" }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: GROUP_BY_LABEL_ID, class: "setting-label", "Group the agents by" }
                    span { class: "setting-help", "The line above the sidebar's list sets it too." }
                }
                div { class: "segmented", role: "group", aria_labelledby: GROUP_BY_LABEL_ID,
                    for &choice in GroupBy::VARIANTS {
                        button {
                            key: "{group_by_label(choice)}",
                            class: if choice == group_by { "segment chosen" } else { "segment" },
                            aria_pressed: choice == group_by,
                            onclick: move |_| cockpit.update(|state| grouping::set_group_by(state, choice)),
                            "{group_by_label(choice)}"
                        }
                    }
                }
            }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: PROJECT_SPLIT_LABEL_ID, class: "setting-label", "A project's agents on several machines" }
                    span { class: "setting-help",
                        "Grouped by project: one list where each row names its machine, or a line per machine above its agents."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: PROJECT_SPLIT_LABEL_ID,
                    for &choice in ProjectSplit::VARIANTS {
                        button {
                            key: "{project_split_label(choice)}",
                            class: if choice == project_split { "segment chosen" } else { "segment" },
                            aria_pressed: choice == project_split,
                            onclick: move |_| cockpit.update(|state| grouping::set_project_split(state, choice)),
                            "{project_split_label(choice)}"
                        }
                    }
                }
            }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: FIRST_MARK_LABEL_ID, class: "setting-label", "Mark to handle" }
                    span { class: "setting-help",
                        "The kind of mark that {mark_keys} and the row's menu put on an agent first. The second time puts the other kind, and the third clears the mark."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: FIRST_MARK_LABEL_ID,
                    for &choice in FirstMark::VARIANTS {
                        button {
                            key: "{first_mark_label(choice)}",
                            class: if choice == first_mark { "segment chosen" } else { "segment" },
                            aria_pressed: choice == first_mark,
                            onclick: move |_| cockpit.update(|state| state.prefs.sidebar.first_mark = choice),
                            "{first_mark_label(choice)}"
                        }
                    }
                }
            }
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
                            key: "{levels_ui::choice_label(level)}",
                            class: if level == notifications { "segment chosen" } else { "segment" },
                            aria_pressed: level == notifications,
                            onclick: move |_| cockpit.set_notifications(None, level),
                            "{levels_ui::choice_label(level)}"
                        }
                    }
                }
            }
            h3 { class: "settings-title", "Chat" }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: TOOL_CALLS_LABEL_ID, class: "setting-label", "Tool calls" }
                    span { class: "setting-help",
                        "One line per call, which a click unfolds to its input and output, or every call in full."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: TOOL_CALLS_LABEL_ID,
                    for &detail in ToolCallDetail::VARIANTS {
                        button {
                            key: "{tool_calls_label(detail)}",
                            class: if detail == tool_calls { "segment chosen" } else { "segment" },
                            aria_pressed: detail == tool_calls,
                            onclick: move |_| cockpit.update(|state| settings::set_tool_calls(state, detail)),
                            "{tool_calls_label(detail)}"
                        }
                    }
                }
            }
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
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: STEER_HOLD_LABEL_ID, class: "setting-label", "Time to cancel a message sent while the agent works" }
                    span { class: "setting-help",
                        "Claude reads such a message at its next step, and once it has it, nothing takes it back. With a time set, the message waits that long above the composer first, with Send now and Cancel."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: STEER_HOLD_LABEL_ID,
                    for &hold in SteerHold::VARIANTS {
                        button {
                            key: "{hold_label(hold)}",
                            class: if hold == steer_hold { "segment chosen" } else { "segment" },
                            aria_pressed: hold == steer_hold,
                            onclick: move |_| cockpit.update(|state| settings::set_steer_hold(state, hold)),
                            "{hold_label(hold)}"
                        }
                    }
                }
            }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: ESC_STOP_LABEL_ID, class: "setting-label", "Ask before Esc stops an agent" }
                    span { class: "setting-help",
                        "With nothing open to close, Esc stops the turn of the agent that works or waits for you. Ask shows a dialog first."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: ESC_STOP_LABEL_ID,
                    for &choice in EscStop::VARIANTS {
                        button {
                            key: "{esc_stop_label(choice)}",
                            class: if choice == esc_stop { "segment chosen" } else { "segment" },
                            aria_pressed: choice == esc_stop,
                            onclick: move |_| cockpit.update(|state| settings::set_esc_stop(state, choice)),
                            "{esc_stop_label(choice)}"
                        }
                    }
                }
            }
            h3 { class: "settings-title", "Files pane" }
            div { class: "setting-row",
                div { class: "setting-text",
                    span { id: FILES_PANE_LABEL_ID, class: "setting-label", "For a new agent" }
                    span { class: "setting-help",
                        "{pane_keys} opens or closes the selected agent's pane, and each agent keeps its own. An agent whose pane you never opened or closed starts like this."
                    }
                }
                div { class: "segmented", role: "group", aria_labelledby: FILES_PANE_LABEL_ID,
                    for &pane in PaneState::VARIANTS {
                        button {
                            key: "{pane_label(pane)}",
                            class: if pane == new_pane { "segment chosen" } else { "segment" },
                            aria_pressed: pane == new_pane,
                            onclick: move |_| cockpit.update(|state| settings::set_new_agents_pane(state, pane)),
                            "{pane_label(pane)}"
                        }
                    }
                }
            }
            h3 { class: "settings-title", "About" }
            div { class: "setting-row",
                img { class: "about-icon", src: APP_ICON.as_str(), alt: "" }
                div { class: "setting-text",
                    span { class: "setting-label", "{APP_NAME}" }
                    span { class: "setting-help",
                        "Version "
                        span { class: "mono", "{APP_VERSION}" }
                    }
                }
                CopyButton {
                    text: format!("{APP_NAME} {APP_VERSION}"),
                    label: "Copy the app's version",
                    look: CopyLook::Worded,
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
    // The refusal stays on screen while it folds away, so the rows under it move up with it.
    let refused_shown = use_presence(refused);
    let record = move |event: KeyboardEvent| {
        event.prevent_default();
        let modifiers = event.modifiers();
        let key = event.key().to_string();
        let ctrl = modifiers.ctrl() || modifiers.meta();
        let (alt, shift) = (modifiers.alt(), modifiers.shift());
        if key == keymap::ESCAPE && !ctrl && !alt && !shift {
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
                        onblur: move |_| cockpit.update(settings::recording_left),
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
                    Presence { open: changed,
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
        if let Some(Shown { value: RefusedChord { chord, refusal, .. }, leaving, .. }) = refused_shown {
            div {
                class: if leaving { "presence leaving" } else { "presence" },
                inert: leaving.then_some(true),
                div { class: "shortcut-refused", role: "alert",
                    div { class: "refused-clip",
                        div { class: "refused-text",
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
