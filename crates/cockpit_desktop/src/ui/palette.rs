//! The `/` palette above the composer. Typing `/` opens it on the commands and skills alone,
//! filtered by the composer's text. The slash button opens it as a menu: actions on the
//! conversation, the model settings, then the commands, filtered by a field of its own.
//!
//! The new agent dialog's first message has one too, on the commands of another agent
//! ([`new_agent_palette`]).

use cockpit_protocol::{AgentId, AgentStatus, CommandKind, SlashCommand};
use dioxus::prelude::*;

use crate::bridge::{
    self, ATTACH_INPUT_ID, COMPOSER_ID, HIGHLIGHTED_CLASS, HIGHLIGHTED_ENTRY_ID,
    NEW_AGENT_PROMPT_ID, PALETTE_FILTER_ID,
};
use crate::connection::Cockpit;
use crate::path_style::{self, PathStyle};
use crate::state::actions::{
    self, CommandMatch, Direction, MenuAction, MenuSection, PaletteEntry, SettingChoice, Step,
    WindowAction,
};
use crate::state::new_agent_palette::{self, SourcePlace, Written};
use crate::state::{AgentEntry, Drafts, PaletteOrigin};
use crate::ui::dialogs::{PointerMoves, use_pointer_moves};
use crate::ui::icons::{Icon, IconKind};

/// The key of the placeholder rows, apart from the entries' numbers and the sections' titles.
const LOADING_KEY: &str = "loading";

/// The widths of the placeholder rows' two lines while the commands load, one pair per row.
const LOADING_ROWS: [(&str, &str); 3] = [("34%", "72%"), ("48%", "60%"), ("28%", "66%")];

/// Where a palette's pick goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteHost {
    /// The agent's composer.
    Agent(AgentId),
    /// The new agent dialog's first message.
    NewAgent,
}

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
fn rows(entry: &AgentEntry, query: &str, how: CommandMatch, origin: PaletteOrigin) -> Vec<Row> {
    let commands = entry.commands.as_slice();
    let exited = entry.summary.status == AgentStatus::Exited;
    let entries = actions::palette_entries(commands, query, how, origin);
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

/// Picks a command into the host's text.
fn pick(cockpit: Cockpit, host: PaletteHost, index: Option<usize>) {
    match host {
        PaletteHost::Agent(agent) => act(cockpit, agent, index),
        PaletteHost::NewAgent => {
            pick_in_new_agent(cockpit, index);
        }
    }
}

/// Puts the command at `index`, or the highlighted one, in the new agent dialog's first message.
/// False when no command is there.
pub fn pick_in_new_agent(cockpit: Cockpit, index: Option<usize>) -> bool {
    let written =
        cockpit.with_drafts(|state, drafts| new_agent_palette::pick(state, drafts, index));
    let Some(Written { text, caret }) = written else {
        return false;
    };
    bridge::set_field_text_at(NEW_AGENT_PROMPT_ID, text, Some(caret));
    bridge::focus_field(NEW_AGENT_PROMPT_ID);
    true
}

/// The highlighted entry of the host's palette.
fn highlighted_entry(drafts: &Drafts, host: PaletteHost) -> Option<usize> {
    match host {
        PaletteHost::Agent(_) => Some(drafts.palette.highlighted),
        PaletteHost::NewAgent => drafts
            .new_agent
            .palette
            .as_ref()
            .map(|open| open.filter.highlighted),
    }
}

/// Left or Right in the menu's filter: on a setting the keys moved to, they change it and the
/// caret stays; elsewhere they move the caret.
fn step_setting(cockpit: Cockpit, agent: AgentId, step: Step, event: &KeyboardEvent) {
    let stepped =
        cockpit.with_drafts(|state, drafts| actions::palette_step(state, drafts, agent, step));
    if let Some(requests) = stepped {
        event.prevent_default();
        cockpit.change(|_| requests);
    }
}

/// The highlighted row's id, which keeps it in view.
fn row_id(highlighted: bool) -> Option<&'static str> {
    highlighted.then_some(HIGHLIGHTED_ENTRY_ID)
}

/// The pointer's part in the highlight, which the rows share.
#[derive(Clone, Copy)]
struct PalettePointer {
    moves: PointerMoves,
    /// The entry the pointer highlighted last, until the scroll effect sees it. It is in view
    /// already: scrolling to it would move the list under the pointer.
    pointed: CopyValue<Option<usize>>,
}

impl PalettePointer {
    /// The pointer moved over the entry at `index`: it takes the highlight.
    fn point(mut self, cockpit: Cockpit, host: PaletteHost, index: usize, event: &MouseEvent) {
        // Writing the drafts renders their readers: only a new entry does.
        let highlighted = highlighted_entry(&cockpit.drafts.peek(), host);
        if self.moves.moved(event) && highlighted != Some(index) {
            let took = cockpit.with_drafts(|state, drafts| {
                match host {
                    PaletteHost::Agent(agent) => {
                        actions::palette_point(state, drafts, agent, index);
                    }
                    PaletteHost::NewAgent => new_agent_palette::point(state, drafts, index),
                }
                highlighted_entry(drafts, host) == Some(index)
            });
            if took {
                self.pointed.set(Some(index));
            }
        }
    }
}

#[component]
pub fn Palette(agent: AgentId, origin: PaletteOrigin) -> Element {
    let cockpit: Cockpit = use_context();
    let highlighted = use_memo(move || cockpit.drafts.read().palette.highlighted);
    let moves = use_pointer_moves();
    let mut pointed = use_hook(|| CopyValue::new(None));
    use_context_provider(|| PalettePointer { moves, pointed });
    use_effect(move || {
        let index = highlighted();
        if pointed.replace(None) != Some(index) {
            bridge::reveal(HIGHLIGHTED_ENTRY_ID);
        }
    });
    let (rows, query) = {
        let state = cockpit.state.read();
        let drafts = cockpit.drafts.read();
        let Some(entry) = state.agents.get(&agent) else {
            return rsx! {};
        };
        let query = drafts.palette.query.clone();
        let how = actions::command_match(&drafts.palette);
        (rows(entry, &query, how, origin), query)
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
                    TypedFilter { query, field: COMPOSER_ID }
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
                            CommandRow { key: "{index}", host: PaletteHost::Agent(agent), index, command, highlighted: index == highlighted }
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

/// Shows the `/` word of the text `field` holds. Opened by typing, the palette leaves the focus in
/// that field, whose text filters it: this field only mirrors it, and gives the focus back, since a
/// field the app writes as you type would lose keystrokes.
#[component]
fn TypedFilter(query: String, field: &'static str) -> Element {
    rsx! {
        input {
            class: "palette-input",
            r#type: "text",
            aria_label: "Filter commands",
            placeholder: "Type to filter commands and skills",
            readonly: true,
            tabindex: "-1",
            value: "{query}",
            onfocus: move |_| bridge::focus_field(field),
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
                cockpit.with_drafts(|_, drafts| actions::palette_filter(drafts, event.value()));
            },
            onkeydown: move |event: KeyboardEvent| match event.key() {
                // The keys move the highlight, and the filter's caret stays: a text field's Up
                // and Down would take it to the start or the end.
                Key::ArrowDown => {
                    event.prevent_default();
                    cockpit.with_drafts(|state, drafts| actions::palette_move(state, drafts, agent, Direction::Down));
                }
                Key::ArrowUp => {
                    event.prevent_default();
                    cockpit.with_drafts(|state, drafts| actions::palette_move(state, drafts, agent, Direction::Up));
                }
                Key::ArrowLeft => step_setting(cockpit, agent, Step::Previous, &event),
                Key::ArrowRight => step_setting(cockpit, agent, Step::Next, &event),
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
    let pointer: PalettePointer = use_context();
    let hint = action.prompt();
    rsx! {
        button {
            id: row_id(highlighted),
            class: if highlighted { "palette-action {HIGHLIGHTED_CLASS}" } else { "palette-action" },
            disabled,
            onmousemove: move |event: MouseEvent| pointer.point(cockpit, PaletteHost::Agent(agent), index, &event),
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
    let pointer: PalettePointer = use_context();
    rsx! {
        div {
            id: row_id(highlighted),
            class: if highlighted { "palette-setting {HIGHLIGHTED_CLASS}" } else { "palette-setting" },
            onmousemove: move |event: MouseEvent| pointer.point(cockpit, PaletteHost::Agent(agent), index, &event),
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
fn CommandRow(
    host: PaletteHost,
    index: usize,
    command: SlashCommand,
    highlighted: bool,
) -> Element {
    let cockpit: Cockpit = use_context();
    let pointer: PalettePointer = use_context();
    rsx! {
        button {
            id: row_id(highlighted),
            class: if highlighted { "palette-item {HIGHLIGHTED_CLASS}" } else { "palette-item" },
            onmousemove: move |event: MouseEvent| pointer.point(cockpit, host, index, &event),
            onclick: move |_| pick(cockpit, host, Some(index)),
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

/// The "/" palette over the new agent dialog's first message. Its commands are another agent's:
/// when that agent runs in another folder, the foot says whose, since the new agent's own list
/// may differ.
#[component]
pub fn NewAgentPalette() -> Element {
    let cockpit: Cockpit = use_context();
    let highlighted = use_memo(move || {
        highlighted_entry(&cockpit.drafts.read(), PaletteHost::NewAgent).unwrap_or_default()
    });
    let moves = use_pointer_moves();
    let mut pointed = use_hook(|| CopyValue::new(None));
    use_context_provider(|| PalettePointer { moves, pointed });
    use_effect(move || {
        let index = highlighted();
        if pointed.replace(None) != Some(index) {
            bridge::reveal(HIGHLIGHTED_ENTRY_ID);
        }
    });
    let state = cockpit.state.read();
    let drafts = cockpit.drafts.read();
    let Some(open) = &drafts.new_agent.palette else {
        return rsx! {};
    };
    let query = open.filter.query.clone();
    let source = open.source;
    let commands: Vec<SlashCommand> = new_agent_palette::entries(&state, &drafts)
        .into_iter()
        .cloned()
        .collect();
    let loading = new_agent_palette::loading(&state, &drafts);
    let elsewhere = source
        .filter(|source| source.place == SourcePlace::OtherFolder)
        .and_then(|source| state.agents.get(&source.agent))
        .map(BorrowedFrom::new);
    let highlighted = highlighted();
    rsx! {
        div {
            class: "popover palette in-dialog",
            role: "dialog",
            aria_label: "Commands and skills",
            // Esc closes the palette alone, and the dialog stays.
            "data-takes-escape": "true",
            // The focus stays in the first message, whose blur closes the palette.
            onmousedown: move |event: MouseEvent| event.prevent_default(),
            div { class: "palette-head",
                span { class: "mono muted", "/" }
                TypedFilter { query, field: NEW_AGENT_PROMPT_ID }
            }
            div { class: "palette-list scroll",
                if source.is_none() {
                    span { class: "palette-empty",
                        "No agent of this account has run on this machine yet, so there is no list of commands. A command you type still works once the agent starts."
                    }
                } else if commands.is_empty() && !loading {
                    span { class: "palette-empty", "No command or skill matches." }
                }
                for (index, command) in commands.into_iter().enumerate() {
                    CommandRow { key: "{index}", host: PaletteHost::NewAgent, index, command, highlighted: index == highlighted }
                }
                if loading {
                    CommandsLoading { key: "{LOADING_KEY}" }
                }
            }
            span { class: "palette-foot",
                if let Some(from) = elsewhere {
                    span { class: "palette-source",
                        span { class: "palette-source-mark", aria_hidden: true }
                        "The agent has not started yet, so these are the commands of "
                        b { "{from.title}" }
                        ", in "
                        span { class: "mono", title: "{from.folder}", "{from.folder_name}" }
                        ". Its own may differ once it starts."
                    }
                }
                "Up and Down to move, Enter to insert, Esc to close"
            }
        }
    }
}

/// The agent in another folder whose commands the new agent dialog's palette lists.
struct BorrowedFrom {
    title: String,
    folder: String,
    /// The folder's last name, which tells it apart in a line.
    folder_name: String,
}

impl BorrowedFrom {
    /// The folder follows the path rules of the daemon's machine, which may not be the app's.
    fn new(entry: &AgentEntry) -> Self {
        let folder = path_style::shown(&entry.summary.folder);
        let folder_name = PathStyle::of(&entry.summary.folder)
            .file_name(&folder)
            .unwrap_or(&folder)
            .to_owned();
        Self {
            title: entry.summary.title.clone(),
            folder,
            folder_name,
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::AgentDetails;

    use super::*;
    use crate::state::{Loadable, summary};

    #[test]
    fn only_the_menu_has_sections_and_loading_rows_come_last() {
        let mut entry = AgentEntry::new(summary(1, AgentStatus::Idle));
        entry.details = Loadable::Loaded(AgentDetails::default());
        entry.commands = vec![SlashCommand {
            name: "review".to_owned(),
            description: "Review the changes on the branch".to_owned(),
            kind: CommandKind::Skill,
        }];
        let sections = |rows: &[Row]| -> Vec<MenuSection> {
            rows.iter()
                .filter_map(|row| match row {
                    Row::Section(section) => Some(*section),
                    _ => None,
                })
                .collect()
        };

        let typed = rows(&entry, "", CommandMatch::Anywhere, PaletteOrigin::Typed);
        let menu = rows(&entry, "", CommandMatch::Anywhere, PaletteOrigin::Button);
        entry.commands_requested = true;
        let reloading = rows(&entry, "", CommandMatch::Anywhere, PaletteOrigin::Button);

        assert!(matches!(typed.as_slice(), [Row::Command { index: 0, .. }]));
        assert_eq!(
            sections(&menu),
            [
                MenuSection::Context,
                MenuSection::Model,
                MenuSection::Commands
            ]
        );
        assert!(matches!(menu.last(), Some(Row::Command { .. })));
        assert!(matches!(reloading.last(), Some(Row::CommandsLoading)));
    }
}
