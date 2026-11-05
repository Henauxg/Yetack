//! The `/` palette above the composer. Typing `/` opens it on the commands and skills alone,
//! filtered by the composer's text. The slash button opens it as a menu: actions on the
//! conversation, the model settings, then the commands, filtered by a field of its own.

use cockpit_protocol::{AgentId, AgentStatus, CommandKind, SlashCommand};
use dioxus::prelude::*;

use crate::bridge::{self, ATTACH_INPUT_ID, COMPOSER_ID, HIGHLIGHTED_ENTRY_ID, PALETTE_FILTER_ID};
use crate::connection::Cockpit;
use crate::state::actions::{
    self, Direction, MenuAction, MenuSection, PaletteEntry, SettingChoice, Step, WindowAction,
};
use crate::state::{AgentEntry, PaletteOrigin};
use crate::ui::icons::{Icon, IconKind};

/// The key of the placeholder rows, apart from the entries' numbers and the sections' titles.
const LOADING_KEY: &str = "loading";

/// The widths of the placeholder rows' two lines while the commands load, one pair per row.
const LOADING_ROWS: [(&str, &str); 3] = [("34%", "72%"), ("48%", "60%"), ("28%", "66%")];

#[derive(Debug, Clone, PartialEq)]
enum Row {
    Section(MenuSection),
    /// The commands section while the commands are on their way.
    CommandsLoading,
    /// `index` counts the entries, the commands included, for the highlight.
    Action {
        index: usize,
        action: MenuAction,
        disabled: bool,
    },
    Setting {
        index: usize,
        action: MenuAction,
        choices: Vec<SettingChoice>,
    },
    Command {
        index: usize,
        command: SlashCommand,
    },
}

/// The palette's rows. The one the slash button opens puts a title over each section.
fn rows(entry: &AgentEntry, query: &str, origin: PaletteOrigin) -> Vec<Row> {
    let commands = entry
        .details
        .loaded()
        .map_or(&[][..], |details| details.commands.as_slice());
    let exited = entry.summary.status == AgentStatus::Exited;
    let entries = actions::palette_entries(commands, query, origin);
    let mut rows = Vec::with_capacity(entries.len() + 3);
    let mut section = None;
    for (index, palette_entry) in entries.into_iter().enumerate() {
        if origin == PaletteOrigin::Button && section != Some(palette_entry.section()) {
            section = Some(palette_entry.section());
            rows.push(Row::Section(palette_entry.section()));
        }
        let row = match palette_entry {
            PaletteEntry::Command(command) => Row::Command {
                index,
                command: command.clone(),
            },
            PaletteEntry::Action(action) if action.is_setting() => Row::Setting {
                index,
                action,
                choices: actions::setting_choices(&entry.summary, action),
            },
            PaletteEntry::Action(action) => Row::Action {
                index,
                action,
                disabled: exited && action.needs_running_agent(),
            },
        };
        rows.push(row);
    }
    if entry.commands_loading() {
        rows.push(Row::CommandsLoading);
    }
    rows
}

fn section_title(section: MenuSection) -> &'static str {
    match section {
        MenuSection::Context => "Context",
        MenuSection::Model => "Model",
        MenuSection::Commands => "Commands and skills",
    }
}

/// Acts on the entry at `index`, or on the highlighted one, then does what only the window can.
fn act(cockpit: Cockpit, agent: AgentId, index: Option<usize>) {
    let mut window = None;
    cockpit.change_with_drafts(|state, drafts| {
        let picked = actions::palette_pick(state, drafts, agent, index);
        window = picked.window;
        picked.requests
    });
    match window {
        Some(WindowAction::ChooseImages) => bridge::click(ATTACH_INPUT_ID),
        Some(WindowAction::Copy(text)) => bridge::copy_to_clipboard(text),
        Some(WindowAction::FocusComposer) => bridge::focus_field(COMPOSER_ID),
        None => {}
    }
}

/// The highlighted row's id, which keeps it in view.
fn row_id(highlighted: bool) -> Option<&'static str> {
    highlighted.then_some(HIGHLIGHTED_ENTRY_ID)
}

#[component]
pub fn Palette(agent: AgentId, origin: PaletteOrigin) -> Element {
    let cockpit: Cockpit = use_context();
    let highlighted = use_memo(move || cockpit.drafts.read().palette.highlighted);
    use_effect(move || {
        highlighted();
        bridge::reveal(HIGHLIGHTED_ENTRY_ID);
    });
    let (rows, query) = {
        let state = cockpit.state.read();
        let drafts = cockpit.drafts.read();
        let Some(entry) = state.agents.get(&agent) else {
            return rsx! {};
        };
        let query = drafts.palette.query.clone();
        (rows(entry, &query, origin), query)
    };
    let highlighted = highlighted();
    let menu = origin == PaletteOrigin::Button;
    rsx! {
        div {
            class: if menu { "popover palette palette-menu" } else { "popover palette" },
            role: "dialog",
            aria_label: if menu { "Actions, commands and skills" } else { "Commands and skills" },
            div { class: "palette-head",
                if menu {
                    Icon { kind: IconKind::Search, size: 14 }
                    MenuFilter { agent }
                } else {
                    span { class: "mono muted", "/" }
                    TypedFilter { agent, query }
                }
            }
            div { class: "palette-list scroll",
                if rows.is_empty() {
                    span { class: "palette-empty",
                        if menu { "Nothing matches." } else { "No command or skill matches." }
                    }
                }
                for row in rows {
                    match row {
                        Row::Section(section) => rsx! {
                            span { key: "{section_title(section)}", class: "palette-section", "{section_title(section)}" }
                        },
                        Row::CommandsLoading => rsx! {
                            CommandsLoading { key: "{LOADING_KEY}" }
                        },
                        Row::Action { index, action, disabled } => rsx! {
                            ActionRow { key: "{index}", agent, index, action, disabled, highlighted: index == highlighted }
                        },
                        Row::Setting { index, action, choices } => rsx! {
                            SettingRow { key: "{index}", agent, index, action, choices, highlighted: index == highlighted }
                        },
                        // Keyed by position: Claude Code may list two commands under one name.
                        Row::Command { index, command } => rsx! {
                            CommandRow { key: "{index}", agent, index, command, highlighted: index == highlighted }
                        },
                    }
                }
            }
            span { class: "palette-foot",
                if menu {
                    "Up and Down to move, Left and Right to change a setting, Enter to pick, Esc to close"
                } else {
                    "Up and Down to move, Enter to insert, Esc to close"
                }
            }
        }
    }
}

/// Shows the composer's `/` word. Opened by typing, the palette leaves the focus in the composer,
/// whose text filters it.
#[component]
fn TypedFilter(agent: AgentId, query: String) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        input {
            class: "palette-input",
            r#type: "text",
            aria_label: "Filter commands",
            placeholder: "Type to filter commands and skills",
            value: "{query}",
            oninput: move |event: FormEvent| {
                cockpit.with_drafts(|_, drafts| {
                    drafts.palette.query = event.value();
                    drafts.palette.highlighted = 0;
                });
            },
            onkeydown: move |event: KeyboardEvent| match event.key() {
                Key::ArrowDown => cockpit.with_drafts(|state, drafts| actions::palette_move(state, drafts, agent, Direction::Down)),
                Key::ArrowUp => cockpit.with_drafts(|state, drafts| actions::palette_move(state, drafts, agent, Direction::Up)),
                Key::Enter => act(cockpit, agent, None),
                _ => {}
            },
        }
    }
}

/// The menu's own filter, which takes the focus when the menu opens.
#[component]
fn MenuFilter(agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        input {
            id: PALETTE_FILTER_ID,
            class: "palette-input",
            r#type: "text",
            aria_label: "Filter actions, commands and skills",
            placeholder: "Filter actions…",
            // Follows the keyboard alone: a render that lags behind typing would put an older
            // filter back.
            initial_value: "",
            onmounted: move |event| async move {
                // The menu may have closed already: nothing to focus then.
                let _ = event.data().set_focus(true).await;
            },
            oninput: move |event: FormEvent| {
                cockpit.with_drafts(|_, drafts| {
                    drafts.palette.query = event.value();
                    drafts.palette.highlighted = 0;
                });
            },
            onkeydown: move |event: KeyboardEvent| match event.key() {
                Key::ArrowDown => cockpit.with_drafts(|state, drafts| actions::palette_move(state, drafts, agent, Direction::Down)),
                Key::ArrowUp => cockpit.with_drafts(|state, drafts| actions::palette_move(state, drafts, agent, Direction::Up)),
                Key::ArrowLeft => cockpit.change_with_drafts(|state, drafts| actions::palette_step(state, drafts, agent, Step::Previous)),
                Key::ArrowRight => cockpit.change_with_drafts(|state, drafts| actions::palette_step(state, drafts, agent, Step::Next)),
                Key::Enter => act(cockpit, agent, None),
                _ => {}
            },
        }
    }
}

#[component]
fn ActionRow(
    agent: AgentId,
    index: usize,
    action: MenuAction,
    disabled: bool,
    highlighted: bool,
) -> Element {
    let cockpit: Cockpit = use_context();
    let hint = action.prompt();
    rsx! {
        button {
            id: row_id(highlighted),
            class: if highlighted { "palette-action highlighted" } else { "palette-action" },
            disabled,
            title: if disabled { "The agent exited: resume it first" },
            onclick: move |_| act(cockpit, agent, Some(index)),
            span { "{action.label()}" }
            if let Some(hint) = hint {
                span { class: "mono palette-hint", "{hint}" }
            }
        }
    }
}

#[component]
fn SettingRow(
    agent: AgentId,
    index: usize,
    action: MenuAction,
    choices: Vec<SettingChoice>,
    highlighted: bool,
) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        div {
            id: row_id(highlighted),
            class: if highlighted { "palette-setting highlighted" } else { "palette-setting" },
            span { "{action.label()}" }
            div { class: "segmented", role: "group", aria_label: action.label(),
                for (choice_index, choice) in choices.into_iter().enumerate() {
                    button {
                        key: "{choice_index}",
                        class: if choice.chosen { "segment chosen" } else { "segment" },
                        aria_pressed: choice.chosen,
                        title: choice.title,
                        onclick: move |_| {
                            cockpit.change_with_drafts(|state, drafts| {
                                drafts.palette.highlighted = index;
                                actions::choose_setting(state, agent, action, choice_index)
                            });
                            // The button took the focus: the keys go back to the filter.
                            bridge::focus_field(PALETTE_FILTER_ID);
                        },
                        "{choice.label}"
                    }
                }
            }
        }
    }
}

#[component]
fn CommandsLoading() -> Element {
    rsx! {
        div {
            span { class: "palette-section",
                "{section_title(MenuSection::Commands)}"
                span { class: "spinner", role: "status", aria_label: "Loading the commands and skills" }
            }
            for (row, (name_width, description_width)) in LOADING_ROWS.into_iter().enumerate() {
                div { key: "{row}", class: "palette-skeleton", aria_hidden: true,
                    span { class: "skeleton skeleton-line", style: "width: {name_width}" }
                    span { class: "skeleton skeleton-line", style: "width: {description_width}" }
                }
            }
        }
    }
}

#[component]
fn CommandRow(agent: AgentId, index: usize, command: SlashCommand, highlighted: bool) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        button {
            id: row_id(highlighted),
            class: if highlighted { "palette-item highlighted" } else { "palette-item" },
            onclick: move |_| act(cockpit, agent, Some(index)),
            span { class: "palette-item-head",
                span { class: "mono palette-name", "/{command.name}" }
                span { class: "palette-kind",
                    match command.kind {
                        CommandKind::Skill => "skill",
                        CommandKind::Command => "command",
                    }
                }
            }
            if !command.description.is_empty() {
                span { class: "palette-description", "{command.description}" }
            }
        }
    }
}
