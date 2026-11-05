//! What the user's clicks and keys change, and the requests they send. The connection sends the
//! returned requests, then the ones `needed_requests` adds.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cockpit_protocol::{
    AgentId, AgentParams, AgentStatus, AgentSummary, Answer, AnswerParams, AnswerState,
    ApprovalAnswer, ApprovalDecision, BackgroundTaskId, CancelQueuedParams, ClaudeSessionId,
    Effort, Image, ItemBody, ItemId, McpRemoveParams, McpScope, McpServerParams, McpToggleParams,
    MoveParams, PastSession, PendingId, PermissionMode, Placed, PlanAnswer, PlanDecision, Prompt,
    PromptParams, Question, QuestionAnswer, RenameParams, Request, SessionResumeParams,
    SetEffortParams, SetModeParams, SetModelParams, SlashCommand, StartParams, StopTaskParams,
};
use strum::VariantArray;

use super::app_state::{AgentEntry, AppState, FileRead, Loadable};
use super::drafts::{Drafts, NewAgentDraft, Palette, QuestionDraft, SwitcherDraft};
use super::requests::{Outgoing, Purpose, Shift, history_request};
use super::transcript::HistoryState;
use super::ui_state::{
    Block, Dialog, FileView, NewAgentForm, NewAgentTab, PaletteOrigin, Popover, TitleEdit,
    TitlePlace, ViewMode,
};
use super::{export, file_edits};
use crate::format::{self, MODE_CHOICES, MODEL_CHOICES};
use crate::prefs::{self, StatusFilter};

/// What starts a slash command in the composer.
pub const COMMAND_PREFIX: char = '/';

/// The commands the palette's Clear and Compact entries send to Claude as prompts.
const CLEAR_COMMAND: &str = "clear";
const COMPACT_COMMAND: &str = "compact";

const MEGABYTE: usize = 1024 * 1024;

/// Ten to the number of decimals a saved width share keeps.
const SHARE_PRECISION: f64 = 10_000.0;

/// The effort Left and Right count from while the agent runs at Claude Code's default effort,
/// which Claude Code does not report: the middle of the levels.
const DEFAULT_EFFORT: Effort = Effort::High;

/// The largest image file the composer takes, in megabytes. Claude counts its own per-image limit
/// on the base64 text, a third larger than the file.
const MAX_IMAGE_MEGABYTES: usize = 5;

/// An image Claude refuses for its size, which the draft does not take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("it is {} MB, and Claude reads images up to {MAX_IMAGE_MEGABYTES} MB", megabytes(*.bytes))]
pub struct ImageTooLarge {
    pub bytes: usize,
}

impl ImageTooLarge {
    /// `bytes` is the image file's size.
    pub fn check(bytes: usize) -> Result<(), Self> {
        if bytes > MAX_IMAGE_MEGABYTES * MEGABYTE {
            return Err(Self { bytes });
        }
        Ok(())
    }

    /// The note the window shows for the image called `name`.
    pub fn note(&self, name: &str) -> String {
        format!("{name} was not attached: {self}.")
    }
}

/// `bytes` in megabytes with one decimal, rounded up so that a size over the limit never reads
/// as the limit.
fn megabytes(bytes: usize) -> String {
    let tenths = bytes.saturating_mul(10).div_ceil(MEGABYTE);
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// Which way the highlight of a list moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// Which way Left and Right move a setting of the palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Previous,
    Next,
}

pub fn select_agent(state: &mut AppState, agent: AgentId) {
    state.ui.selected = Some(agent);
    state.ui.chat_new_from.clear();
    state.ui.unread.remove(&agent);
    state.ui.leave_agent();
}

/// The chat's script says the agent's chat reached its bottom, or left it. Leaving it marks the
/// last entry seen, so that the entries after it count as new. Reaching it puts the "New" divider
/// above the first new entry, if any came.
pub fn chat_at_bottom(state: &mut AppState, agent: AgentId, at_bottom: bool) {
    let Some(entry) = state.agents.get(&agent) else {
        return;
    };
    if !at_bottom {
        let last_seen = entry.transcript.last_id();
        state.ui.chat_scrolled_up.entry(agent).or_insert(last_seen);
        return;
    }
    let Some(last_seen) = state.ui.chat_scrolled_up.remove(&agent) else {
        return;
    };
    if let Some(first) = new_entries(entry, last_seen).first {
        state.ui.chat_new_from.insert(agent, first);
    }
}

/// The entries that arrived while an agent's chat was scrolled up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NewEntries {
    pub count: usize,
    pub first: Option<ItemId>,
}

/// The entries after `last_seen` that a reader counts: messages, Claude's text, tool calls and
/// cards, not notices, thinking or a turn's end.
pub fn new_entries(entry: &AgentEntry, last_seen: Option<ItemId>) -> NewEntries {
    let counted = entry.transcript.items_after(last_seen).filter(|item| {
        item.parent.is_none()
            && matches!(
                item.body,
                ItemBody::UserMessage(_)
                    | ItemBody::AssistantText { .. }
                    | ItemBody::ToolCall(_)
                    | ItemBody::Subagent(_)
                    | ItemBody::Approval(_)
                    | ItemBody::Question(_)
                    | ItemBody::Plan(_)
            )
    });
    let mut news = NewEntries::default();
    for item in counted {
        news.first = news.first.or(Some(item.id));
        news.count += 1;
    }
    news
}

/// Where the chat's "New" divider goes: above the first new entry while scrolled up, else where
/// the chat's last trip down put it.
pub fn new_divider(state: &AppState, agent: AgentId) -> Option<ItemId> {
    let entry = state.agents.get(&agent)?;
    let scrolled_up = state.ui.chat_scrolled_up.get(&agent);
    let waiting = scrolled_up.and_then(|&last_seen| new_entries(entry, last_seen).first);
    waiting.or_else(|| state.ui.chat_new_from.get(&agent).copied())
}

/// Whether the command palette opens or closes as you type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteChange {
    Open,
    Close,
}

/// The composer's text changed. A `/` word alone filters the palette, and opens it; other text
/// closes a palette opened by typing. A palette opened by the slash button has a filter field of
/// its own, so the composer's text leaves it as it is. Only the drafts change: the caller applies
/// the returned change with `change_palette`, so that a keystroke that leaves the palette as it
/// is leaves the rest of the state alone.
pub fn composer_input(
    state: &AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    text: String,
) -> Option<PaletteChange> {
    let command_query = text
        .strip_prefix(COMMAND_PREFIX)
        .filter(|rest| !rest.contains(char::is_whitespace));
    let change = match (state.ui.popover, command_query) {
        (Some(Popover::Palette(PaletteOrigin::Button)), _) => None,
        (popover, Some(query)) => {
            drafts.palette = Palette {
                query: query.to_owned(),
                ..Palette::default()
            };
            let typed_open = popover == Some(Popover::Palette(PaletteOrigin::Typed));
            (!typed_open).then_some(PaletteChange::Open)
        }
        (Some(Popover::Palette(PaletteOrigin::Typed)), None) => Some(PaletteChange::Close),
        (_, None) => None,
    };
    drafts.draft_mut(agent).text = text;
    change
}

/// Opens or closes the palette as the composer's text asks.
pub fn change_palette(
    state: &mut AppState,
    agent: AgentId,
    change: PaletteChange,
) -> Vec<Outgoing> {
    match change {
        PaletteChange::Open => open_palette(state, agent, PaletteOrigin::Typed),
        PaletteChange::Close => {
            if state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed)) {
                state.ui.popover = None;
            }
            Vec::new()
        }
    }
}

/// The slash button: opens the palette, or closes the one it opened.
pub fn toggle_palette(state: &mut AppState, drafts: &mut Drafts, agent: AgentId) -> Vec<Outgoing> {
    if state.ui.popover == Some(Popover::Palette(PaletteOrigin::Button)) {
        state.ui.popover = None;
        return Vec::new();
    }
    drafts.palette = Palette::default();
    open_palette(state, agent, PaletteOrigin::Button)
}

/// Opening the palette asks for the commands when the agent reported none yet.
fn open_palette(state: &mut AppState, agent: AgentId, origin: PaletteOrigin) -> Vec<Outgoing> {
    state.ui.popover = Some(Popover::Palette(origin));
    commands_request(state, agent).into_iter().collect()
}

/// The details carry the commands; an agent that reported none yet is asked directly.
fn commands_request(state: &mut AppState, agent: AgentId) -> Option<Outgoing> {
    let entry = state.agents.get_mut(&agent)?;
    let details = entry.details.loaded()?;
    if !details.commands.is_empty() || entry.commands_requested {
        return None;
    }
    entry.commands_requested = true;
    Some(Outgoing {
        request: Request::AgentCommands(AgentParams { agent }),
        purpose: Purpose::Commands { agent },
    })
}

/// Commands whose name starts with the query come first, then those that contain it in their
/// name or description.
pub fn filtered_commands<'command>(
    commands: &'command [SlashCommand],
    query: &str,
) -> Vec<&'command SlashCommand> {
    let query = query.trim_start_matches(COMMAND_PREFIX).to_lowercase();
    let mut matching: Vec<&SlashCommand> = commands
        .iter()
        .filter(|command| {
            command.name.to_lowercase().contains(&query)
                || command.description.to_lowercase().contains(&query)
        })
        .collect();
    matching.sort_by_key(|command| !command.name.to_lowercase().starts_with(&query));
    matching
}

fn agent_commands(state: &AppState, agent: AgentId) -> &[SlashCommand] {
    state
        .agents
        .get(&agent)
        .and_then(|entry| entry.details.loaded())
        .map_or(&[], |details| details.commands.as_slice())
}

/// An entry of the palette opened by the slash button, other than a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, VariantArray)]
pub enum MenuAction {
    AttachImage,
    ClearConversation,
    CompactConversation,
    ExportConversation,
    /// Turns the header's title into a field.
    Rename,
    /// A setting: its row holds its choices, which Left and Right move through.
    Model,
    Effort,
    Mode,
}

/// The palette's sections, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuSection {
    Context,
    Model,
    Commands,
}

impl MenuAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::AttachImage => "Attach an image",
            Self::ClearConversation => "Clear conversation",
            Self::CompactConversation => "Compact conversation",
            Self::ExportConversation => "Export conversation",
            Self::Rename => "Rename",
            Self::Model => "Model",
            Self::Effort => "Effort",
            Self::Mode => "Permission mode",
        }
    }

    pub fn section(self) -> MenuSection {
        match self {
            Self::AttachImage
            | Self::ClearConversation
            | Self::CompactConversation
            | Self::ExportConversation
            | Self::Rename => MenuSection::Context,
            Self::Model | Self::Effort | Self::Mode => MenuSection::Model,
        }
    }

    pub fn is_setting(self) -> bool {
        self.section() == MenuSection::Model
    }

    /// The prompt Clear and Compact send to Claude, the way `/effort` changes the effort.
    pub fn prompt(self) -> Option<String> {
        let command = match self {
            Self::ClearConversation => CLEAR_COMMAND,
            Self::CompactConversation => COMPACT_COMMAND,
            Self::AttachImage
            | Self::ExportConversation
            | Self::Rename
            | Self::Model
            | Self::Effort
            | Self::Mode => {
                return None;
            }
        };
        Some(format!("{COMMAND_PREFIX}{command}"))
    }

    /// An entry that sends a prompt does nothing for an exited agent, which reads none.
    pub fn needs_running_agent(self) -> bool {
        self.prompt().is_some()
    }

    /// Whether the label, or for a setting the label of a choice, holds the lowercase `query`.
    fn matches(self, query: &str) -> bool {
        let words: Vec<&str> = match self {
            Self::Model => MODEL_CHOICES.iter().map(|choice| choice.label).collect(),
            Self::Effort => Effort::VARIANTS
                .iter()
                .flat_map(|&effort| {
                    [
                        format::effort_label(effort),
                        format::effort_short_label(effort),
                    ]
                })
                .collect(),
            Self::Mode => MODE_CHOICES
                .iter()
                .map(|&mode| format::mode_label(mode))
                .collect(),
            Self::AttachImage
            | Self::ClearConversation
            | Self::CompactConversation
            | Self::ExportConversation
            | Self::Rename => Vec::new(),
        };
        let mut words = words.into_iter().chain([self.label()]);
        words.any(|word| word.to_lowercase().contains(query))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteEntry<'command> {
    Action(MenuAction),
    Command(&'command SlashCommand),
}

impl PaletteEntry<'_> {
    pub fn section(self) -> MenuSection {
        match self {
            Self::Action(action) => action.section(),
            Self::Command(_) => MenuSection::Commands,
        }
    }
}

/// The palette's entries, in order. Opened by typing `/`, it lists the matching commands alone;
/// opened by the slash button, the matching actions first.
pub fn palette_entries<'command>(
    commands: &'command [SlashCommand],
    query: &str,
    origin: PaletteOrigin,
) -> Vec<PaletteEntry<'command>> {
    let mut entries = Vec::new();
    if origin == PaletteOrigin::Button {
        let query = query.trim().to_lowercase();
        let actions = MenuAction::VARIANTS
            .iter()
            .copied()
            .filter(|action| action.matches(&query));
        entries.extend(actions.map(PaletteEntry::Action));
    }
    let commands = filtered_commands(commands, query);
    entries.extend(commands.into_iter().map(PaletteEntry::Command));
    entries
}

/// The open palette's entries.
fn open_palette_entries<'state>(
    state: &'state AppState,
    drafts: &Drafts,
    agent: AgentId,
) -> Vec<PaletteEntry<'state>> {
    let Some(Popover::Palette(origin)) = state.ui.popover else {
        return Vec::new();
    };
    palette_entries(agent_commands(state, agent), &drafts.palette.query, origin)
}

pub fn palette_move(state: &AppState, drafts: &mut Drafts, agent: AgentId, direction: Direction) {
    let count = open_palette_entries(state, drafts, agent).len();
    drafts.palette.highlighted = moved(drafts.palette.highlighted, count, direction);
    drafts.palette.moved = true;
}

/// The menu's own filter changed: the highlight goes back to the first entry.
pub fn palette_filter(drafts: &mut Drafts, query: String) {
    drafts.palette = Palette {
        query,
        ..Palette::default()
    };
}

/// What a pick in the palette asks for: requests, and what only the window can do.
#[must_use]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Picked {
    pub requests: Vec<Outgoing>,
    pub window: Option<WindowAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowAction {
    /// Opens the file chooser of the composer's attach button.
    ChooseImages,
    /// Copies the text to the clipboard.
    Copy(String),
    /// Gives the composer the focus, for a command's arguments.
    FocusComposer,
}

/// Acts on the entry at `index`, or on the highlighted one. A command goes at the start of the
/// draft, ready for its arguments, and the rest of the text stays. A setting changes with Left and
/// Right, not here.
pub fn palette_pick(
    state: &mut AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    index: Option<usize>,
) -> Picked {
    let index = index.unwrap_or(drafts.palette.highlighted);
    let action = match open_palette_entries(state, drafts, agent)
        .get(index)
        .copied()
    {
        None => return Picked::default(),
        Some(PaletteEntry::Action(action)) => action,
        Some(PaletteEntry::Command(command)) => {
            let name = command.name.clone();
            let draft = drafts.draft_mut(agent);
            draft.text = with_command(&draft.text, &name);
            drafts.draft_written(agent);
            state.ui.popover = None;
            return Picked {
                requests: Vec::new(),
                window: Some(WindowAction::FocusComposer),
            };
        }
    };
    let Some(entry) = state.agents.get(&agent) else {
        return Picked::default();
    };
    let exited = entry.summary.status == AgentStatus::Exited;
    if action.is_setting() || (exited && action.needs_running_agent()) {
        return Picked::default();
    }
    let picked = match action {
        MenuAction::AttachImage => Picked {
            requests: Vec::new(),
            window: Some(WindowAction::ChooseImages),
        },
        MenuAction::ClearConversation | MenuAction::CompactConversation => {
            let prompts = action.prompt().map(|text| prompt_request(agent, text));
            Picked {
                requests: prompts.into_iter().collect(),
                window: None,
            }
        }
        MenuAction::ExportConversation => {
            let markdown = export::conversation_markdown(entry);
            let note = if export::is_complete(entry) {
                "Copied the conversation to the clipboard as Markdown."
            } else {
                "Copied the conversation to the clipboard as Markdown, without the older messages the window has not loaded. Load them with \"Load older messages\" at the top of the conversation, then export again."
            };
            state.ui.inform(note.to_owned());
            Picked {
                requests: Vec::new(),
                window: Some(WindowAction::Copy(markdown)),
            }
        }
        MenuAction::Rename => {
            start_rename(state, drafts, agent);
            Picked::default()
        }
        MenuAction::Model | MenuAction::Effort | MenuAction::Mode => Picked::default(),
    };
    state.ui.popover = None;
    picked
}

/// Turns the header's title into a field holding it.
pub fn start_rename(state: &mut AppState, drafts: &mut Drafts, agent: AgentId) {
    start_rename_at(state, drafts, agent, TitlePlace::Header);
}

/// Turns the title of the agent's sidebar row into a field holding it.
pub fn start_rename_in_sidebar(state: &mut AppState, drafts: &mut Drafts, agent: AgentId) {
    start_rename_at(state, drafts, agent, TitlePlace::Sidebar);
}

/// An edit of the same agent moves to `place`: the new field starts from the text typed so far,
/// which stays in the drafts.
fn start_rename_at(state: &mut AppState, drafts: &mut Drafts, agent: AgentId, place: TitlePlace) {
    let Some(entry) = state.agents.get(&agent) else {
        return;
    };
    let moving = state
        .ui
        .title_edit
        .as_ref()
        .is_some_and(|edit| edit.agent == agent);
    if !moving {
        drafts.title.clone_from(&entry.summary.title);
    }
    state.ui.title_edit = Some(TitleEdit {
        agent,
        title: drafts.title.clone(),
        place,
        select_all: !moving,
    });
    state.ui.popover = None;
}

/// F2: renames the selected agent in its sidebar row, or moves an open title field to the other
/// place. An agent the filter hides has no row: its title field opens in the header.
pub fn rename_key(state: &mut AppState, drafts: &mut Drafts) {
    let (agent, wanted) = match &state.ui.title_edit {
        Some(edit) => match edit.place {
            TitlePlace::Header => (edit.agent, TitlePlace::Sidebar),
            TitlePlace::Sidebar => (edit.agent, TitlePlace::Header),
        },
        None => match state.ui.selected {
            Some(agent) => (agent, TitlePlace::Sidebar),
            None => return,
        },
    };
    let has_row = state
        .sidebar_agents()
        .iter()
        .any(|entry| entry.id() == agent);
    let place = if has_row { wanted } else { TitlePlace::Header };
    start_rename_at(state, drafts, agent, place);
}

/// Only the drafts change, so that typing a title does not render the transcript again.
pub fn rename_text(state: &AppState, drafts: &mut Drafts, text: String) {
    if state.ui.title_edit.is_some() {
        drafts.title = text;
    }
}

/// A title field left the focus: the edit ends when it is still at that field's place. A field
/// that leaves the page as the edit moves to the other place must not end it.
pub fn finish_rename_at(
    state: &mut AppState,
    drafts: &mut Drafts,
    place: TitlePlace,
) -> Vec<Outgoing> {
    let here = state
        .ui
        .title_edit
        .as_ref()
        .is_some_and(|edit| edit.place == place);
    if !here {
        return Vec::new();
    }
    finish_rename(state, drafts)
}

/// Ends the title edit, and renames the agent when the title changed and is not empty. Esc ends
/// it with the overlays instead, which renames nothing.
pub fn finish_rename(state: &mut AppState, drafts: &mut Drafts) -> Vec<Outgoing> {
    let Some(edit) = state.ui.title_edit.take() else {
        return Vec::new();
    };
    let typed = std::mem::take(&mut drafts.title);
    let title = typed.trim();
    let unchanged = state
        .agents
        .get(&edit.agent)
        .is_none_or(|entry| entry.summary.title == title);
    if title.is_empty() || unchanged {
        return Vec::new();
    }
    vec![Outgoing::action(Request::AgentRename(RenameParams {
        agent: edit.agent,
        title: title.to_owned(),
    }))]
}

/// A prompt the app writes itself: a failure shows as a note, and no draft comes back.
fn prompt_request(agent: AgentId, text: String) -> Outgoing {
    let prompt = Prompt {
        text,
        images: Vec::new(),
    };
    Outgoing::action(Request::AgentPrompt(PromptParams { agent, prompt }))
}

/// A choice of a setting row of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingChoice {
    pub label: &'static str,
    /// The longer label, or what the mode does.
    pub title: &'static str,
    pub chosen: bool,
}

/// The modes the palette offers: the mode menu's, and the agent's own when it is another.
fn mode_choices(current: PermissionMode) -> Vec<PermissionMode> {
    let mut modes = MODE_CHOICES.to_vec();
    if !modes.contains(&current) {
        modes.push(current);
    }
    modes
}

/// The choices of a setting row, for the agent of `summary`. Empty for an action.
pub fn setting_choices(summary: &AgentSummary, action: MenuAction) -> Vec<SettingChoice> {
    match action {
        MenuAction::Model => {
            let current = summary
                .model
                .as_deref()
                .map(format::model_label)
                .unwrap_or_default();
            let choices = MODEL_CHOICES.iter().map(|choice| SettingChoice {
                label: choice.label,
                title: choice.label,
                chosen: current.starts_with(choice.label),
            });
            choices.collect()
        }
        MenuAction::Effort => {
            let choices = Effort::VARIANTS.iter().map(|&effort| SettingChoice {
                label: format::effort_short_label(effort),
                title: format::effort_label(effort),
                chosen: summary.effort == Some(effort),
            });
            choices.collect()
        }
        MenuAction::Mode => {
            let choices = mode_choices(summary.mode)
                .into_iter()
                .map(|mode| SettingChoice {
                    label: format::mode_label(mode),
                    title: format::mode_description(mode),
                    chosen: summary.mode == mode,
                });
            choices.collect()
        }
        MenuAction::AttachImage
        | MenuAction::ClearConversation
        | MenuAction::CompactConversation
        | MenuAction::ExportConversation
        | MenuAction::Rename => Vec::new(),
    }
}

/// Sets the setting of `action` to its choice at `index`, as the composer's menus do.
pub fn choose_setting(
    state: &AppState,
    agent: AgentId,
    action: MenuAction,
    index: usize,
) -> Vec<Outgoing> {
    let Some(entry) = state.agents.get(&agent) else {
        return Vec::new();
    };
    let requests = match action {
        MenuAction::Model => MODEL_CHOICES
            .get(index)
            .map(|choice| set_model(agent, choice.alias.to_owned())),
        MenuAction::Effort => Effort::VARIANTS
            .get(index)
            .map(|&effort| set_effort(agent, effort)),
        MenuAction::Mode => mode_choices(entry.summary.mode)
            .get(index)
            .map(|&mode| set_mode(agent, mode)),
        MenuAction::AttachImage
        | MenuAction::ClearConversation
        | MenuAction::CompactConversation
        | MenuAction::ExportConversation
        | MenuAction::Rename => None,
    };
    requests.unwrap_or_default()
}

/// Left and Right on the highlighted setting, once Up or Down moved the highlight to it: its
/// previous or next choice. An unknown effort counts from the default level. Nothing moves past
/// the first or the last. `None` when the keys are not the setting's: they move the filter's
/// caret then.
pub fn palette_step(
    state: &AppState,
    drafts: &Drafts,
    agent: AgentId,
    step: Step,
) -> Option<Vec<Outgoing>> {
    if !drafts.palette.moved {
        return None;
    }
    let highlighted = open_palette_entries(state, drafts, agent)
        .get(drafts.palette.highlighted)
        .copied();
    let (Some(PaletteEntry::Action(action)), Some(entry)) = (highlighted, state.agents.get(&agent))
    else {
        return None;
    };
    if !action.is_setting() {
        return None;
    }
    let choices = setting_choices(&entry.summary, action);
    let direction = match step {
        Step::Previous => Direction::Up,
        Step::Next => Direction::Down,
    };
    let current = choices
        .iter()
        .position(|choice| choice.chosen)
        .or_else(|| unknown_value_choice(action));
    let next = match current {
        Some(current) => moved(current, choices.len(), direction),
        None => 0,
    };
    if current == Some(next) {
        return Some(Vec::new());
    }
    Some(choose_setting(state, agent, action, next))
}

/// The choice a setting counts from when the agent's value is unknown: the default effort level.
/// A model counts from its first choice, and a mode is always known.
fn unknown_value_choice(action: MenuAction) -> Option<usize> {
    match action {
        MenuAction::Effort => Effort::VARIANTS
            .iter()
            .position(|&effort| effort == DEFAULT_EFFORT),
        MenuAction::Model
        | MenuAction::Mode
        | MenuAction::AttachImage
        | MenuAction::ClearConversation
        | MenuAction::CompactConversation
        | MenuAction::ExportConversation
        | MenuAction::Rename => None,
    }
}

/// `text` starting with the command, in place of the `/word` it starts with, if any.
fn with_command(text: &str, name: &str) -> String {
    let rest = match text.strip_prefix(COMMAND_PREFIX) {
        Some(typed) => typed.trim_start_matches(|character: char| !character.is_whitespace()),
        None => text,
    };
    format!("{COMMAND_PREFIX}{name} {}", rest.trim_start())
}

/// An index moved by one, staying within `count` items.
fn moved(index: usize, count: usize, direction: Direction) -> usize {
    let last = count.saturating_sub(1);
    match direction {
        Direction::Down => index.saturating_add(1).min(last),
        Direction::Up => index.saturating_sub(1).min(last),
    }
}

/// Adds the image to the draft, unless Claude would refuse it for its size.
pub fn add_image(drafts: &mut Drafts, agent: AgentId, image: Image) -> Result<(), ImageTooLarge> {
    ImageTooLarge::check(decoded_len(&image.data))?;
    drafts.draft_mut(agent).images.push(image);
    Ok(())
}

/// The size of the bytes that base64 `data` encodes.
fn decoded_len(data: &str) -> usize {
    let padding = data.bytes().rev().take_while(|&byte| byte == b'=').count();
    (data.len() / 4 * 3).saturating_sub(padding)
}

pub fn remove_image(drafts: &mut Drafts, agent: AgentId, index: usize) {
    let images = &mut drafts.draft_mut(agent).images;
    if index < images.len() {
        images.remove(index);
    }
}

/// Sends the draft. `typed` is the composer's text as the key event saw it, which may hold a last
/// keystroke the draft has not received yet.
pub fn send_prompt(
    state: &mut AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    typed: Option<String>,
) -> Vec<Outgoing> {
    let exited = state
        .agents
        .get(&agent)
        .is_none_or(|entry| entry.summary.status == AgentStatus::Exited);
    if exited {
        return Vec::new();
    }
    let draft = drafts.draft_mut(agent);
    if let Some(typed) = typed {
        draft.text = typed;
    }
    if draft.is_empty() {
        return Vec::new();
    }
    let draft = std::mem::take(draft);
    drafts.draft_written(agent);
    state.ui.popover = None;
    // The script reports the bottom once its jump got there: what arrived before is not new.
    state.ui.chat_scrolled_up.remove(&agent);
    state.ui.chat_new_from.remove(&agent);
    state.ui.chat_jumps = state.ui.chat_jumps.wrapping_add(1);
    let prompt = Prompt {
        text: draft.text.trim().to_owned(),
        images: draft.images.clone(),
    };
    vec![Outgoing {
        request: Request::AgentPrompt(PromptParams { agent, prompt }),
        purpose: Purpose::Prompt { agent, draft },
    }]
}

/// Stops the current turn. Only a working agent, or one waiting for you, has a turn to stop.
pub fn interrupt(state: &AppState, agent: AgentId) -> Vec<Outgoing> {
    let has_turn = state.agents.get(&agent).is_some_and(|entry| {
        matches!(
            entry.summary.status,
            AgentStatus::Working | AgentStatus::NeedsYou
        )
    });
    if !has_turn {
        return Vec::new();
    }
    vec![Outgoing::action(Request::AgentInterrupt(AgentParams {
        agent,
    }))]
}

fn answer(agent: AgentId, pending: PendingId, answer: Answer) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentAnswer(AnswerParams {
        agent,
        pending,
        answer,
    }))]
}

pub fn answer_approval(
    agent: AgentId,
    pending: PendingId,
    decision: ApprovalDecision,
) -> Vec<Outgoing> {
    let approval = ApprovalAnswer {
        decision,
        message: None,
    };
    answer(agent, pending, Answer::Approval(approval))
}

pub fn answer_plan(agent: AgentId, pending: PendingId, decision: PlanDecision) -> Vec<Outgoing> {
    let plan = PlanAnswer {
        decision,
        message: None,
    };
    answer(agent, pending, Answer::Plan(plan))
}

fn question_draft<'drafts>(
    drafts: &'drafts mut Drafts,
    question: &Question,
) -> &'drafts mut QuestionDraft {
    let count = question.questions.len();
    let draft = drafts
        .questions
        .entry(question.pending.clone())
        .or_default();
    draft.picks.resize_with(count, BTreeSet::new);
    draft.other.resize_with(count, String::new);
    draft
}

/// Picks an option. In a single-choice question it replaces the pick, or clears it when picked
/// again; in a multiple-choice question it toggles.
pub fn pick_option(drafts: &mut Drafts, question: &Question, asked: usize, option: usize) {
    let Some(multi_select) = question
        .questions
        .get(asked)
        .map(|asked| asked.multi_select)
    else {
        return;
    };
    let draft = question_draft(drafts, question);
    let Some(picks) = draft.picks.get_mut(asked) else {
        return;
    };
    if picks.remove(&option) {
        return;
    }
    if !multi_select {
        picks.clear();
    }
    picks.insert(option);
}

pub fn set_other_answer(drafts: &mut Drafts, question: &Question, asked: usize, text: String) {
    if let Some(other) = question_draft(drafts, question).other.get_mut(asked) {
        *other = text;
    }
}

/// One answer per question, or `None` while a question has neither a pick nor an "Other" text.
pub fn question_answers(
    question: &Question,
    draft: Option<&QuestionDraft>,
) -> Option<Vec<QuestionAnswer>> {
    let draft = draft?;
    let mut answers = Vec::with_capacity(question.questions.len());
    for (index, asked) in question.questions.iter().enumerate() {
        let picks = draft.picks.get(index);
        let selected: Vec<String> = asked
            .options
            .iter()
            .enumerate()
            .filter(|(option, _)| picks.is_some_and(|picks| picks.contains(option)))
            .map(|(_, option)| option.label.clone())
            .collect();
        let other = draft
            .other
            .get(index)
            .map(|text| text.trim())
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        if selected.is_empty() && other.is_none() {
            return None;
        }
        answers.push(QuestionAnswer { selected, other });
    }
    Some(answers)
}

fn waiting_question(entry: &AgentEntry, item: ItemId) -> Option<&Question> {
    match &entry.transcript.get(item)?.body {
        ItemBody::Question(question) if question.answer == AnswerState::Waiting => Some(question),
        _ => None,
    }
}

pub fn submit_question(
    state: &AppState,
    drafts: &Drafts,
    agent: AgentId,
    item: ItemId,
) -> Vec<Outgoing> {
    let Some(question) = state
        .agents
        .get(&agent)
        .and_then(|entry| waiting_question(entry, item))
    else {
        return Vec::new();
    };
    let Some(answers) = question_answers(question, drafts.questions.get(&question.pending)) else {
        return Vec::new();
    };
    answer(
        agent,
        question.pending.clone(),
        Answer::Question { answers },
    )
}

/// The 1 to 9 keys: picks an option of the first question still without an answer, or of the
/// last one.
pub fn pick_by_number(
    state: &AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    item: ItemId,
    number: usize,
) {
    let Some(question) = state
        .agents
        .get(&agent)
        .and_then(|entry| waiting_question(entry, item))
        .cloned()
    else {
        return;
    };
    let draft = drafts.questions.get(&question.pending);
    let unanswered = (0..question.questions.len()).find(|&index| {
        let picked = draft
            .and_then(|draft| draft.picks.get(index))
            .is_some_and(|picks| !picks.is_empty());
        !picked
    });
    let asked = unanswered.unwrap_or(question.questions.len().saturating_sub(1));
    if let Some(option) = number.checked_sub(1) {
        let exists = question
            .questions
            .get(asked)
            .is_some_and(|asked| option < asked.options.len());
        if exists {
            pick_option(drafts, &question, asked, option);
        }
    }
}

pub fn cancel_queued(agent: AgentId, item: ItemId) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentCancelQueued(
        CancelQueuedParams { agent, item },
    ))]
}

/// Opens the file view on `path`, at `line` when a link names one, in its tab. A file the agent
/// changed opens on its diff, unless a line is asked for. The text is read again, since it may
/// have changed, and the text of the file shown before is dropped: files can be large.
pub fn open_file(state: &mut AppState, agent: AgentId, path: PathBuf, line: Option<u32>) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let path = file_edits::relative_path(&entry.summary.folder, &path);
    let changed =
        !file_edits::edits_for(&entry.transcript, &entry.summary.folder, &path).is_empty();
    let mode = if line.is_none() && changed {
        ViewMode::Diff
    } else {
        ViewMode::WholeFile
    };
    let changed_paths = entry.changed_paths();
    entry.files.clear();
    entry.files.insert(path.clone(), Loadable::NotRequested);
    let view = state
        .ui
        .file_views
        .entry(agent)
        .or_insert_with(|| FileView::new(path.clone(), line, mode));
    view.closed.remove(&path);
    if !changed_paths.contains(&path) && !view.opened.contains(&path) {
        view.opened.push(path.clone());
    }
    view.path = path;
    view.line = line;
    view.mode = mode;
    state.ui.popover = None;
    state.ui.badge = None;
}

/// Opens `choice`, one of the files that end with `name`, in the tab of `name`.
pub fn pick_file(
    state: &mut AppState,
    agent: AgentId,
    name: &Path,
    choice: PathBuf,
    line: Option<u32>,
) {
    if let Some(view) = state.ui.file_views.get_mut(&agent) {
        view.opened.retain(|opened| opened != name);
    }
    open_file(state, agent, choice, line);
}

/// Closes a tab. When it is the shown one, the tab after it shows, or the one before it for the
/// last tab; closing the only tab closes the file view.
pub fn close_tab(state: &mut AppState, agent: AgentId, path: &Path) {
    let Some(entry) = state.agents.get(&agent) else {
        return;
    };
    let changed = entry.changed_paths();
    let Some(view) = state.ui.file_views.get_mut(&agent) else {
        return;
    };
    let tabs = view.tabs(&changed);
    let Some(index) = tabs.iter().position(|tab| tab == path) else {
        return;
    };
    view.opened.retain(|opened| opened != path);
    if changed.iter().any(|changed| changed == path) {
        view.closed.insert(path.to_owned());
    }
    if view.path != path {
        return;
    }
    let after = tabs.get(index + 1);
    let before = index.checked_sub(1).and_then(|before| tabs.get(before));
    match after.or(before) {
        Some(next) => open_file(state, agent, next.clone(), None),
        None => close_file_view(state, agent),
    }
}

/// Shows the text the daemon read for `requested` at `found`. When the daemon looked a name up and
/// found the file elsewhere, the view moves to it. A file the view no longer shows is not kept.
pub fn show_read_file(
    state: &mut AppState,
    agent: AgentId,
    requested: &Path,
    found: &Path,
    text: String,
) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    if entry.files.remove(requested).is_none() {
        return;
    }
    let folder = &entry.summary.folder;
    let found = file_edits::relative_path(folder, found);
    if let Some(view) = state.ui.file_views.get_mut(&agent)
        && view.path == requested
    {
        let changed = !file_edits::edits_for(&entry.transcript, folder, &found).is_empty();
        if view.line.is_none() && changed {
            view.mode = ViewMode::Diff;
        }
        // The found file takes the tab of the name asked for.
        for opened in &mut view.opened {
            if opened == requested {
                opened.clone_from(&found);
            }
        }
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        view.opened.retain(|opened| seen.insert(opened.clone()));
        view.path = found.clone();
    }
    entry
        .files
        .insert(found, Loadable::Loaded(FileRead::Text(text)));
}

pub fn close_file_view(state: &mut AppState, agent: AgentId) {
    state.ui.file_views.remove(&agent);
    state.ui.badge = None;
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.files.clear();
    }
}

pub fn set_view_mode(state: &mut AppState, agent: AgentId, mode: ViewMode) {
    if let Some(view) = state.ui.file_views.get_mut(&agent) {
        view.mode = mode;
    }
}

/// Wraps the file view's long lines, or lets them scroll sideways again. The choice is saved, and
/// holds for every agent.
pub fn toggle_wrap(state: &mut AppState) {
    state.prefs.file_view.wrap = !state.prefs.file_view.wrap;
}

/// Keeps the file view's width, as a share of the window's, where the splitter left it. The page
/// sends it: a share outside 0 to 1 is ignored.
pub fn set_file_view_share(state: &mut AppState, share: f64) {
    if !prefs::is_share(share) {
        tracing::warn!("ignored a file view width of {share} of the window");
        return;
    }
    // Four decimals place the splitter within a fraction of a pixel and keep the file readable.
    let rounded = (share * SHARE_PRECISION).round() / SHARE_PRECISION;
    state.prefs.file_view.width_share = Some(rounded);
}

pub fn reset_file_view_share(state: &mut AppState) {
    state.prefs.file_view.width_share = None;
}

/// Folds or unfolds a floating block, or opens its badge's popover while the file view is open.
/// Opening the MCP servers asks Claude Code for their status.
pub fn toggle_block(state: &mut AppState, agent: AgentId, block: Block) -> Vec<Outgoing> {
    let opening = if state.ui.file_view(agent).is_some() {
        let opening = state.ui.badge != Some(block);
        state.ui.badge = opening.then_some(block);
        opening
    } else if state.ui.open_blocks.remove(&block) {
        false
    } else {
        state.ui.open_blocks.insert(block);
        true
    };
    let running = state
        .agents
        .get(&agent)
        .is_some_and(|entry| entry.summary.status != AgentStatus::Exited);
    if !(opening && block == Block::Mcp && running) {
        return Vec::new();
    }
    vec![Outgoing {
        request: Request::AgentMcpStatus(AgentParams { agent }),
        purpose: Purpose::McpStatus { agent },
    }]
}

/// Opens or closes the usage card. Opening it asks Claude Code for the context usage now.
pub fn toggle_usage(state: &mut AppState, agent: AgentId) -> Vec<Outgoing> {
    state.ui.toggle_popover(Popover::Usage);
    let opened = state.ui.popover == Some(Popover::Usage);
    let Some(entry) = state
        .agents
        .get_mut(&agent)
        .filter(|entry| opened && entry.summary.status != AgentStatus::Exited)
    else {
        return Vec::new();
    };
    entry.context_loading = true;
    vec![Outgoing {
        request: Request::AgentContextUsage(AgentParams { agent }),
        purpose: Purpose::ContextUsage { agent },
    }]
}

pub fn stop_task(agent: AgentId, task: BackgroundTaskId) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentStopTask(StopTaskParams {
        agent,
        task,
    }))]
}

pub fn mcp_reconnect(agent: AgentId, server: String) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentMcpReconnect(
        McpServerParams { agent, server },
    ))]
}

pub fn mcp_toggle(agent: AgentId, server: String, enabled: bool) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentMcpToggle(McpToggleParams {
        agent,
        server,
        enabled,
    }))]
}

pub fn mcp_remove(agent: AgentId, server: String, scope: McpScope) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::McpRemove(McpRemoveParams {
        agent,
        server,
        scope,
    }))]
}

/// Hides a server from every agent's view, or shows it again. The daemon keeps the setting.
pub fn set_mcp_hidden(state: &mut AppState, server: &str, hidden: bool) -> Vec<Outgoing> {
    let mut settings = state.settings.clone();
    if hidden {
        settings.hidden_mcp_servers.insert(server.to_owned());
    } else {
        settings.hidden_mcp_servers.remove(server);
    }
    state.settings = settings.clone();
    vec![Outgoing::action(Request::SettingsSet(settings))]
}

pub fn set_model(agent: AgentId, model: String) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentSetModel(SetModelParams {
        agent,
        model,
    }))]
}

pub fn set_effort(agent: AgentId, effort: Effort) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentSetEffort(SetEffortParams {
        agent,
        effort,
    }))]
}

pub fn set_mode(agent: AgentId, mode: PermissionMode) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentSetMode(SetModeParams {
        agent,
        mode,
    }))]
}

pub fn resume_agent(agent: AgentId) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentResume(AgentParams {
        agent,
    }))]
}

pub fn load_older(state: &mut AppState, agent: AgentId) -> Vec<Outgoing> {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return Vec::new();
    };
    if entry.transcript.history() != &(HistoryState::Loaded { has_older: true }) {
        return Vec::new();
    }
    entry.transcript.set_history(HistoryState::LoadingOlder);
    vec![history_request(agent, entry.transcript.first_id())]
}

/// After a failed history request: `needed_requests` asks again.
pub fn retry_history(state: &mut AppState, agent: AgentId) {
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.transcript.set_history(HistoryState::NotLoaded);
    }
}

pub fn toggle_switcher(state: &mut AppState, drafts: &mut Drafts) {
    let open = state.ui.dialog == Some(Dialog::Switcher);
    state.ui.popover = None;
    state.ui.dialog = (!open).then_some(Dialog::Switcher);
    drafts.switcher = SwitcherDraft::default();
}

/// Moves `agent` just above `before`, or to the bottom of the list. The sidebar shows the new
/// order at once, with the positions the daemon then sends in its events.
pub fn move_agent(state: &mut AppState, agent: AgentId, before: Option<AgentId>) -> Vec<Outgoing> {
    let list: Vec<Placed> = state
        .agents_in_order()
        .iter()
        .map(|entry| entry.summary.place())
        .collect();
    let moved = cockpit_protocol::move_agent(&list, agent, before);
    let mut shifts = Vec::with_capacity(moved.len());
    for place in moved {
        if let Some(entry) = state.agents.get_mut(&place.agent) {
            shifts.push(Shift {
                agent: place.agent,
                from: entry.summary.position,
                to: place.position,
            });
            entry.summary.position = place.position;
        }
    }
    if shifts.is_empty() {
        return Vec::new();
    }
    vec![Outgoing {
        request: Request::AgentMove(MoveParams { agent, before }),
        purpose: Purpose::Move { shifts },
    }]
}

/// Drops a dragged sidebar row just above the row of `before_row`, or below the last row when
/// it is `None`. Hidden agents keep their places: a drop below the last row lands just under it.
pub fn drop_agent(
    state: &mut AppState,
    agent: AgentId,
    before_row: Option<AgentId>,
) -> Vec<Outgoing> {
    let before = match before_row {
        Some(row) => Some(row),
        None => {
            let shown = state.sidebar_agents();
            let Some(last) = shown
                .iter()
                .rev()
                .map(|entry| entry.id())
                .find(|id| *id != agent)
            else {
                return Vec::new();
            };
            below_in_order(state, last, agent)
        }
    };
    move_agent(state, agent, before)
}

/// Alt+Up and Alt+Down: the selected agent goes one row up or down among the rows the sidebar
/// shows.
pub fn move_selected(state: &mut AppState, direction: Direction) -> Vec<Outgoing> {
    let Some(agent) = state.ui.selected else {
        return Vec::new();
    };
    let shown: Vec<AgentId> = state
        .sidebar_agents()
        .iter()
        .map(|entry| entry.id())
        .collect();
    let Some(index) = shown.iter().position(|id| *id == agent) else {
        return Vec::new();
    };
    let before = match direction {
        Direction::Up => {
            let Some(above) = index.checked_sub(1).and_then(|above| shown.get(above)) else {
                return Vec::new();
            };
            Some(*above)
        }
        Direction::Down => {
            let Some(below) = shown.get(index + 1) else {
                return Vec::new();
            };
            below_in_order(state, *below, agent)
        }
    };
    move_agent(state, agent, before)
}

/// The agent just below `row` in the whole order, leaving `moving` out: the one `moving` goes
/// above to land just below `row`. `None` when `row` is the last.
fn below_in_order(state: &AppState, row: AgentId, moving: AgentId) -> Option<AgentId> {
    let order: Vec<AgentId> = state
        .agents_in_order()
        .iter()
        .map(|entry| entry.id())
        .filter(|id| *id != moving)
        .collect();
    let index = order.iter().position(|id| *id == row)?;
    order.get(index + 1).copied()
}

/// Shows only the agents of one status in the sidebar, or all of them. The selection stays.
pub fn set_status_filter(state: &mut AppState, filter: StatusFilter) {
    state.prefs.sidebar.filter = filter;
}

/// Agents whose title or folder holds the query, the ones that wait for you first: the switcher
/// finds an agent, so it ranks by attention rather than in the sidebar's order.
pub fn filtered_agents<'state>(state: &'state AppState, query: &str) -> Vec<&'state AgentEntry> {
    let query = query.to_lowercase();
    state
        .agents_by_attention()
        .into_iter()
        .filter(|entry| {
            entry.summary.title.to_lowercase().contains(&query)
                || entry
                    .summary
                    .folder
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&query)
        })
        .collect()
}

/// Only the drafts change, so that typing in the switcher does not render the transcript again.
pub fn switcher_query(drafts: &mut Drafts, query: String) {
    drafts.switcher = SwitcherDraft {
        query,
        highlighted: 0,
    };
}

pub fn switcher_move(state: &AppState, drafts: &mut Drafts, direction: Direction) {
    let switcher = &mut drafts.switcher;
    let count = filtered_agents(state, &switcher.query).len();
    switcher.highlighted = moved(switcher.highlighted, count, direction);
}

/// Opens the agent at `index` of the filtered list, or the highlighted one.
pub fn switcher_pick(state: &mut AppState, drafts: &Drafts, index: Option<usize>) {
    if state.ui.dialog != Some(Dialog::Switcher) {
        return;
    }
    let index = index.unwrap_or(drafts.switcher.highlighted);
    let Some(agent) = filtered_agents(state, &drafts.switcher.query)
        .get(index)
        .map(|entry| entry.id())
    else {
        return;
    };
    state.ui.dialog = None;
    select_agent(state, agent);
}

/// Opens the new agent dialog on the last used folder, with the model, effort and permission
/// mode of the last agent started. A mode the mode menu does not offer, or Plan, which is
/// usually for a while, is not carried over. The past sessions are listed again, since sessions
/// end and start while the app runs. An open dialog stays as it is, with what its fields hold.
pub fn open_new_agent(state: &mut AppState, drafts: &mut Drafts) {
    if matches!(state.ui.dialog, Some(Dialog::NewAgent(_))) {
        return;
    }
    let folder = state
        .recent_folders()
        .first()
        .map(|(folder, _)| folder.to_string_lossy().into_owned())
        .unwrap_or_default();
    let last = state
        .agents
        .values()
        .map(|entry| &entry.summary)
        .max_by_key(|summary| summary.started_at);
    let model = last
        .and_then(|summary| summary.model.as_deref())
        .and_then(format::model_alias)
        .map(str::to_owned);
    let effort = last
        .filter(|summary| !summary.effort_is_default)
        .and_then(|summary| summary.effort);
    let mode = last
        .map(|summary| summary.mode)
        .filter(|mode| MODE_CHOICES.contains(mode) && *mode != PermissionMode::Plan);
    if state.sessions != Loadable::Loading {
        state.sessions = Loadable::NotRequested;
    }
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::NewAgent(NewAgentForm {
        model,
        effort,
        mode,
        ..NewAgentForm::default()
    }));
    drafts.new_agent = NewAgentDraft {
        folder,
        ..NewAgentDraft::default()
    };
}

pub fn with_new_agent_form(state: &mut AppState, change: impl FnOnce(&mut NewAgentForm)) {
    if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog {
        change(form);
    }
}

pub fn start_agent(state: &mut AppState, drafts: &Drafts) -> Vec<Outgoing> {
    let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog else {
        return Vec::new();
    };
    let folder = drafts.new_agent.folder.trim();
    if folder.is_empty() || form.starting || form.tab != NewAgentTab::Start {
        return Vec::new();
    }
    form.starting = true;
    let prompt = drafts.new_agent.prompt.trim();
    let params = StartParams {
        folder: PathBuf::from(folder),
        title: None,
        model: form.model.clone(),
        effort: form.effort,
        mode: form.mode,
        prompt: (!prompt.is_empty()).then(|| Prompt {
            text: prompt.to_owned(),
            images: Vec::new(),
        }),
    };
    vec![Outgoing {
        request: Request::AgentStart(params),
        purpose: Purpose::Start,
    }]
}

/// The past sessions the "Resume a session" tab lists: those no agent runs, whose title or
/// folder holds the query, newest first. A session's title is its first prompt unless it was
/// named.
pub fn resumable_sessions<'state>(
    state: &'state AppState,
    query: &str,
) -> Vec<&'state PastSession> {
    let query = query.to_lowercase();
    let running: BTreeSet<&ClaudeSessionId> = state
        .agents
        .values()
        .filter_map(|entry| entry.summary.session.as_ref())
        .collect();
    let mut sessions: Vec<&PastSession> = state
        .sessions
        .loaded()
        .into_iter()
        .flatten()
        .filter(|session| !running.contains(&session.id))
        .filter(|session| {
            session.title.to_lowercase().contains(&query)
                || session
                    .folder
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&query)
        })
        .collect();
    sessions.sort_by_key(|session| Reverse(session.last_activity));
    sessions
}

/// Only the drafts change, so that typing a search does not render the transcript again.
pub fn session_query(drafts: &mut Drafts, query: String) {
    drafts.new_agent.session_query = query;
    drafts.new_agent.session_highlighted = 0;
}

pub fn session_move(state: &AppState, drafts: &mut Drafts, direction: Direction) {
    let new_agent = &mut drafts.new_agent;
    let count = resumable_sessions(state, &new_agent.session_query).len();
    new_agent.session_highlighted = moved(new_agent.session_highlighted, count, direction);
}

/// Resumes the session at `index` of the listed ones, or the highlighted one.
pub fn session_pick(state: &mut AppState, drafts: &Drafts, index: Option<usize>) -> Vec<Outgoing> {
    if !matches!(state.ui.dialog, Some(Dialog::NewAgent(_))) {
        return Vec::new();
    }
    let new_agent = &drafts.new_agent;
    let index = index.unwrap_or(new_agent.session_highlighted);
    let Some(session) = resumable_sessions(state, &new_agent.session_query)
        .get(index)
        .map(|session| session.id.clone())
    else {
        return Vec::new();
    };
    resume_session(state, session)
}

pub fn resume_session(state: &mut AppState, session: ClaudeSessionId) -> Vec<Outgoing> {
    if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog {
        if form.starting {
            return Vec::new();
        }
        form.starting = true;
    }
    vec![Outgoing {
        request: Request::SessionsResume(SessionResumeParams { session }),
        purpose: Purpose::Start,
    }]
}

#[cfg(test)]
mod tests {
    use cockpit_client::RequestError;
    use cockpit_protocol::{
        AgentDetails, AskedQuestion, ChangedFile, CommandKind, ErrorBody, ErrorCode, HistoryPage,
        ImageMediaType, Method, QuestionOption, ResponseBody, Timestamp, TranscriptItem,
    };

    use super::*;
    use crate::state::app_state::ConnectionState;
    use crate::state::app_state::tests::{ids, placed, state_with, summary};
    use crate::state::requests::{apply_response, needed_requests};
    use crate::state::ui_state::NoteKind;

    const AGENT: u32 = 1;

    /// Agents 3, 2 and 1 from the top, 1024 apart, with agent 2 selected.
    fn ordered() -> AppState {
        let mut state = state_with(vec![
            placed(1, AgentStatus::Idle, 1024),
            placed(2, AgentStatus::Working, 2048),
            placed(3, AgentStatus::NeedsYou, 3072),
        ]);
        state.connection = ConnectionState::Connected;
        state.ui.selected = Some(AgentId::new(2));
        state
    }

    fn sidebar(state: &AppState) -> Vec<u32> {
        ids(&state.sidebar_agents())
    }

    fn sent_move(requests: &[Outgoing]) -> Option<MoveParams> {
        match &requests.first()?.request {
            Request::AgentMove(params) => Some(*params),
            _ => None,
        }
    }

    fn agent() -> AgentId {
        AgentId::new(AGENT)
    }

    fn state(status: AgentStatus) -> AppState {
        let mut state = state_with(vec![summary(AGENT, status)]);
        state.connection = ConnectionState::Connected;
        state.ui.selected = Some(agent());
        state
    }

    fn command(name: &str, description: &str) -> SlashCommand {
        SlashCommand {
            name: name.to_owned(),
            description: description.to_owned(),
            kind: CommandKind::Command,
        }
    }

    fn with_commands(state: &mut AppState, commands: Vec<SlashCommand>) {
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.details = Loadable::Loaded(AgentDetails {
                commands,
                ..AgentDetails::default()
            });
        }
    }

    fn question(multi_select: bool) -> Question {
        let options = ["Keep", "Rewrite", "Drop"]
            .into_iter()
            .map(|label| QuestionOption {
                label: label.to_owned(),
                description: String::new(),
            })
            .collect();
        Question {
            pending: PendingId::new("request-7".to_owned()),
            questions: vec![AskedQuestion {
                header: "Fixtures".to_owned(),
                text: "What should happen to the fixtures?".to_owned(),
                options,
                multi_select,
            }],
            answer: AnswerState::Waiting,
        }
    }

    fn with_question(state: &mut AppState, question: Question) -> ItemId {
        let item = ItemId::new(4);
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.transcript.append(TranscriptItem {
                id: item,
                parent: None,
                body: ItemBody::Question(question),
            });
        }
        item
    }

    /// Types in the composer as the window does: the drafts, then the palette when it changes.
    fn type_text(state: &mut AppState, drafts: &mut Drafts, text: &str) -> Vec<Outgoing> {
        match composer_input(state, drafts, agent(), text.to_owned()) {
            Some(change) => change_palette(state, agent(), change),
            None => Vec::new(),
        }
    }

    #[test]
    fn a_slash_word_opens_the_palette_and_asks_for_missing_commands() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(&mut state, Vec::new());

        let requests = type_text(&mut state, &mut drafts, "/rev");

        assert_eq!(
            state.ui.popover,
            Some(Popover::Palette(PaletteOrigin::Typed))
        );
        assert_eq!(drafts.palette.query, "rev");
        assert_eq!(
            requests.first().map(|outgoing| outgoing.request.method()),
            Some(Method::AgentCommands)
        );

        type_text(&mut state, &mut drafts, "/review the branch");
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn the_commands_load_until_the_details_or_their_own_request_answer() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        let loading = |state: &AppState| state.agents[&agent()].commands_loading();
        let before_the_details = loading(&state);
        with_commands(&mut state, Vec::new());
        let with_no_commands = loading(&state);

        let first = toggle_palette(&mut state, &mut drafts, agent());
        let while_asked = loading(&state);
        toggle_palette(&mut state, &mut drafts, agent());
        let second = toggle_palette(&mut state, &mut drafts, agent());
        apply_response(
            &mut state,
            &mut drafts,
            Method::AgentCommands,
            Purpose::Commands { agent: agent() },
            Ok(ResponseBody::Commands {
                commands: vec![command("review", "")],
            }),
        );

        assert_eq!(
            (
                before_the_details,
                with_no_commands,
                while_asked,
                loading(&state)
            ),
            (true, false, true, false)
        );
        // The palette opened again while the request was on its way asks nothing more.
        assert_eq!((first.len(), second.len()), (1, 0));
        assert_eq!(agent_commands(&state, agent()).len(), 1);
    }

    /// Appends a notice with the id `id` to the agent's transcript.
    fn append_notice(state: &mut AppState, id: u32) {
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.transcript.append(TranscriptItem {
                id: ItemId::new(id),
                parent: None,
                body: ItemBody::Notice {
                    text: format!("notice {id}"),
                },
            });
        }
    }

    /// Appends an item with the id `id` and the body `body` to the agent's transcript.
    fn append(state: &mut AppState, id: u32, body: ItemBody) {
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.transcript.append(TranscriptItem {
                id: ItemId::new(id),
                parent: None,
                body,
            });
        }
    }

    fn text(markdown: &str) -> ItemBody {
        ItemBody::AssistantText {
            markdown: markdown.to_owned(),
        }
    }

    #[test]
    fn new_entries_count_messages_text_tool_calls_and_cards_only() {
        let mut state = state(AgentStatus::Working);
        append_notice(&mut state, 1);
        chat_at_bottom(&mut state, agent(), false);
        append_notice(&mut state, 2);
        append(&mut state, 3, text("Reading the lexer."));
        append(
            &mut state,
            4,
            ItemBody::Thinking {
                text: "Hmm.".to_owned(),
            },
        );
        append(
            &mut state,
            5,
            ItemBody::UserMessage(cockpit_protocol::UserMessage {
                text: "Also the docs".to_owned(),
                images: Vec::new(),
                queued: true,
            }),
        );
        append(&mut state, 6, ItemBody::Question(question(false)));

        let last_seen = state.ui.chat_scrolled_up[&agent()];
        let news = new_entries(&state.agents[&agent()], last_seen);

        assert_eq!(
            news,
            NewEntries {
                count: 3,
                first: Some(ItemId::new(3)),
            }
        );
    }

    #[test]
    fn the_new_divider_stays_above_the_first_new_entry_until_a_send_or_a_switch() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        append_notice(&mut state, 1);
        let divider = |state: &AppState| new_divider(state, agent());
        chat_at_bottom(&mut state, agent(), false);
        let nothing_new = divider(&state);
        append_notice(&mut state, 2);
        append(&mut state, 3, text("Done."));
        let scrolled_up = divider(&state);
        chat_at_bottom(&mut state, agent(), true);
        let back_down = divider(&state);

        type_text(&mut state, &mut drafts, "Thanks");
        send_prompt(&mut state, &mut drafts, agent(), None);
        let after_send = divider(&state);
        chat_at_bottom(&mut state, agent(), false);
        append(&mut state, 4, text("More."));
        chat_at_bottom(&mut state, agent(), true);
        let again = divider(&state);
        select_agent(&mut state, agent());

        assert_eq!(nothing_new, None);
        assert_eq!(scrolled_up, Some(ItemId::new(3)));
        assert_eq!(back_down, Some(ItemId::new(3)));
        assert_eq!(after_send, None);
        assert_eq!(again, Some(ItemId::new(4)));
        assert_eq!(divider(&state), None);
    }

    /// The script reports the bottom only once its jump got there, after the send.
    #[test]
    fn a_send_while_scrolled_up_leaves_no_divider_above_what_arrived_meanwhile() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        append_notice(&mut state, 1);
        chat_at_bottom(&mut state, agent(), false);
        append(&mut state, 2, text("Done."));

        type_text(&mut state, &mut drafts, "Thanks");
        send_prompt(&mut state, &mut drafts, agent(), None);
        let jump_shown = state.ui.chat_scrolled_up.contains_key(&agent());
        chat_at_bottom(&mut state, agent(), true);

        assert!(!jump_shown);
        assert_eq!(new_divider(&state, agent()), None);
    }

    #[test]
    fn a_send_takes_the_chat_to_its_bottom_and_an_empty_one_does_not() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        chat_at_bottom(&mut state, agent(), false);

        send_prompt(&mut state, &mut drafts, agent(), None);
        let after_empty = state.ui.chat_jumps;
        type_text(&mut state, &mut drafts, "Thanks");
        send_prompt(&mut state, &mut drafts, agent(), None);

        assert_eq!(after_empty, 0);
        assert_eq!(state.ui.chat_jumps, 1);
    }

    #[test]
    fn leaving_the_bottom_of_the_chat_marks_the_last_entry_seen() {
        let mut state = state(AgentStatus::Working);
        let marked = |state: &AppState| state.ui.chat_scrolled_up.get(&agent()).copied();
        chat_at_bottom(&mut state, agent(), false);
        let empty = marked(&state);
        chat_at_bottom(&mut state, agent(), true);
        let back = marked(&state);

        append_notice(&mut state, 5);
        chat_at_bottom(&mut state, agent(), false);
        append_notice(&mut state, 6);
        chat_at_bottom(&mut state, agent(), false);

        assert_eq!(empty, Some(None));
        assert_eq!(back, None);
        // A second report while still scrolled up keeps the first mark.
        assert_eq!(marked(&state), Some(Some(ItemId::new(5))));
    }

    #[test]
    fn typing_changes_the_rest_of_the_state_only_to_open_or_close_the_palette() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(&mut state, vec![command("review", "")]);

        let plain = composer_input(&state, &mut drafts, agent(), "Run the te".to_owned());
        let opening = composer_input(&state, &mut drafts, agent(), "/re".to_owned());
        change_palette(&mut state, agent(), PaletteChange::Open);
        let filtering = composer_input(&state, &mut drafts, agent(), "/rev".to_owned());
        let closing = composer_input(&state, &mut drafts, agent(), "/rev the".to_owned());

        assert_eq!(
            (plain, opening, filtering, closing),
            (
                None,
                Some(PaletteChange::Open),
                None,
                Some(PaletteChange::Close)
            )
        );
        assert_eq!(
            drafts.draft(agent()).map(|draft| draft.text.as_str()),
            Some("/rev the")
        );
    }

    #[test]
    fn erasing_the_slash_closes_a_palette_opened_by_typing() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(&mut state, vec![command("review", "")]);

        type_text(&mut state, &mut drafts, "/");
        let opened = state.ui.popover;
        type_text(&mut state, &mut drafts, "");
        let erased = state.ui.popover;
        type_text(&mut state, &mut drafts, "/re");
        type_text(&mut state, &mut drafts, "re");
        let slash_erased = state.ui.popover;

        assert_eq!(opened, Some(Popover::Palette(PaletteOrigin::Typed)));
        assert_eq!((erased, slash_erased), (None, None));
    }

    #[test]
    fn a_palette_opened_by_the_button_stays_open_while_typing_until_a_pick() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(&mut state, vec![command("review", "")]);
        let button = Some(Popover::Palette(PaletteOrigin::Button));

        toggle_palette(&mut state, &mut drafts, agent());
        for text in ["", "Run the", "/re", ""] {
            type_text(&mut state, &mut drafts, text);
            assert_eq!(state.ui.popover, button, "after typing {text:?}");
        }
        // Its own filter field filters it, not the composer's text.
        assert_eq!(drafts.palette.query, "");

        let picked = palette_pick(&mut state, &mut drafts, agent(), None);
        assert_eq!(picked.window, Some(WindowAction::ChooseImages));
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn the_slash_button_takes_over_a_palette_opened_by_typing() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(&mut state, vec![command("review", "")]);
        type_text(&mut state, &mut drafts, "/re");

        toggle_palette(&mut state, &mut drafts, agent());
        let opened = state.ui.popover;
        toggle_palette(&mut state, &mut drafts, agent());

        assert_eq!(opened, Some(Popover::Palette(PaletteOrigin::Button)));
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn commands_filter_with_prefix_matches_first() {
        let commands = vec![
            command("compact", "Summarize to free context"),
            command("context", "Show what fills the context"),
            command("review", "Review the branch"),
        ];

        let names: Vec<&str> = filtered_commands(&commands, "/con")
            .iter()
            .map(|command| command.name.as_str())
            .collect();

        assert_eq!(names, ["context", "compact"]);
    }

    #[test]
    fn picking_from_the_palette_fills_the_composer() {
        let mut state = state(AgentStatus::Idle);
        with_commands(
            &mut state,
            vec![command("compact", ""), command("context", "")],
        );
        let mut drafts = Drafts::default();
        type_text(&mut state, &mut drafts, "/c");

        palette_move(&state, &mut drafts, agent(), Direction::Down);
        palette_move(&state, &mut drafts, agent(), Direction::Down);
        let picked = palette_pick(&mut state, &mut drafts, agent(), None);

        assert_eq!(
            drafts.draft(agent()).map(|draft| draft.text.as_str()),
            Some("/context ")
        );
        assert_eq!(picked.window, Some(WindowAction::FocusComposer));
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn a_command_picked_from_the_slash_button_keeps_the_draft() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(&mut state, vec![command("review", "")]);
        let mut pick_on = |text: &str| {
            type_text(&mut state, &mut drafts, text);
            toggle_palette(&mut state, &mut drafts, agent());
            drafts.palette.query = "review".to_owned();
            let _ = palette_pick(&mut state, &mut drafts, agent(), None);
            drafts.draft(agent()).map(|draft| draft.text.clone())
        };

        assert_eq!(
            pick_on("check the parser\nthen the lexer").as_deref(),
            Some("/review check the parser\nthen the lexer")
        );
        assert_eq!(
            pick_on("/rev the branch").as_deref(),
            Some("/review the branch")
        );
        assert_eq!(pick_on("").as_deref(), Some("/review "));
    }

    /// Opens the palette with the slash button, filtered by `query` as if typed in its field.
    fn open_menu(state: &mut AppState, drafts: &mut Drafts, query: &str) {
        if state.ui.popover != Some(Popover::Palette(PaletteOrigin::Button)) {
            toggle_palette(state, drafts, agent());
        }
        palette_filter(drafts, query.to_owned());
    }

    /// The entries as the palette names them.
    fn entry_names(commands: &[SlashCommand], query: &str, origin: PaletteOrigin) -> Vec<String> {
        let entries = palette_entries(commands, query, origin);
        let names = entries.into_iter().map(|entry| match entry {
            PaletteEntry::Action(action) => action.label().to_owned(),
            PaletteEntry::Command(command) => format!("/{}", command.name),
        });
        names.collect()
    }

    fn sent(requests: &[Outgoing]) -> Vec<&Request> {
        requests.iter().map(|outgoing| &outgoing.request).collect()
    }

    #[test]
    fn the_menu_filter_matches_actions_settings_and_commands() {
        let commands = vec![
            command("compact", "Summarize to free context"),
            command("context", "Show what fills the context"),
            command("review", "Review the branch"),
        ];
        let menu = |query| entry_names(&commands, query, PaletteOrigin::Button);

        assert_eq!(
            menu(""),
            [
                "Attach an image",
                "Clear conversation",
                "Compact conversation",
                "Export conversation",
                "Rename",
                "Model",
                "Effort",
                "Permission mode",
                "/compact",
                "/context",
                "/review",
            ]
        );
        assert_eq!(
            menu("con"),
            [
                "Clear conversation",
                "Compact conversation",
                "Export conversation",
                "/context",
                "/compact",
            ]
        );
        // A setting matches by the label of one of its choices too.
        assert_eq!(menu("sonnet"), ["Model"]);
        assert_eq!(menu("x-high"), ["Effort"]);
        assert_eq!(menu("plan"), ["Permission mode"]);
        assert_eq!(menu("REVIEW"), ["/review"]);
        assert_eq!(
            entry_names(&commands, "", PaletteOrigin::Typed),
            ["/compact", "/context", "/review"]
        );
    }

    #[test]
    fn up_and_down_move_through_actions_then_commands_and_stop_at_the_ends() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(
            &mut state,
            vec![command("review", ""), command("compact", "")],
        );
        open_menu(&mut state, &mut drafts, "");
        let mut press = |direction, times| {
            for _ in 0..times {
                palette_move(&state, &mut drafts, agent(), direction);
            }
            drafts.palette.highlighted
        };

        let up_at_the_top = press(Direction::Up, 1);
        let on_the_first_command = press(Direction::Down, 8);
        let at_the_bottom = press(Direction::Down, 5);
        let back_on_the_settings = press(Direction::Up, 3);

        assert_eq!(
            (
                up_at_the_top,
                on_the_first_command,
                at_the_bottom,
                back_on_the_settings
            ),
            (0, 8, 9, 6)
        );
    }

    #[test]
    fn clear_and_compact_send_their_command_as_a_prompt_and_keep_the_draft() {
        let mut state = state(AgentStatus::Working);
        let mut drafts = Drafts::default();
        type_text(&mut state, &mut drafts, "half-written message");
        let mut pick = |query: &str| {
            open_menu(&mut state, &mut drafts, query);
            let picked = palette_pick(&mut state, &mut drafts, agent(), None);
            assert_eq!(picked.window, None);
            assert_eq!(state.ui.popover, None);
            picked.requests
        };

        let clear = pick("clear");
        let compact = pick("compact");

        let prompt = |text: &str| {
            Request::AgentPrompt(PromptParams {
                agent: agent(),
                prompt: Prompt {
                    text: text.to_owned(),
                    images: Vec::new(),
                },
            })
        };
        assert_eq!(sent(&clear), [&prompt("/clear")]);
        assert_eq!(sent(&compact), [&prompt("/compact")]);
        assert_eq!(
            drafts.draft(agent()).map(|draft| draft.text.as_str()),
            Some("half-written message")
        );
    }

    #[test]
    fn an_exited_agent_gets_no_clear_and_the_menu_stays_open() {
        let mut state = state(AgentStatus::Exited);
        let mut drafts = Drafts::default();
        open_menu(&mut state, &mut drafts, "clear");

        let picked = palette_pick(&mut state, &mut drafts, agent(), None);

        assert_eq!(picked, Picked::default());
        assert_eq!(
            state.ui.popover,
            Some(Popover::Palette(PaletteOrigin::Button))
        );
    }

    #[test]
    fn export_copies_the_conversation_and_says_what_it_left_out() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        let mut export = |has_older| {
            if let Some(entry) = state.agents.get_mut(&agent()) {
                entry.transcript.merge_page(HistoryPage {
                    items: vec![TranscriptItem {
                        id: ItemId::new(1),
                        parent: None,
                        body: ItemBody::AssistantText {
                            markdown: "Done.".to_owned(),
                        },
                    }],
                    has_older,
                });
            }
            open_menu(&mut state, &mut drafts, "export");
            let picked = palette_pick(&mut state, &mut drafts, agent(), None);
            let note = state
                .ui
                .errors
                .last()
                .map(|note| (note.kind, note.text.clone()));
            (picked, note)
        };

        let (whole, whole_note) = export(false);
        let (part, part_note) = export(true);

        let copied = "# Agent 1\n\n## Claude\n\nDone.\n".to_owned();
        assert_eq!(whole.window, Some(WindowAction::Copy(copied.clone())));
        assert!(whole.requests.is_empty());
        assert_eq!(
            whole_note,
            Some((
                NoteKind::Info,
                "Copied the conversation to the clipboard as Markdown.".to_owned()
            ))
        );
        assert_eq!(part.window, Some(WindowAction::Copy(copied)));
        // Only the button loads older messages: the note names it.
        assert!(part_note.is_some_and(|(_, text)| {
            text.contains("without the older messages") && text.contains("\"Load older messages\"")
        }));
    }

    #[test]
    fn rename_from_the_menu_opens_the_title_field_and_saves_a_changed_title() {
        let mut state = state(AgentStatus::Exited);
        let mut drafts = Drafts::default();
        open_menu(&mut state, &mut drafts, "rename");

        let picked = palette_pick(&mut state, &mut drafts, agent(), None);
        let opened = state.ui.title_edit.clone();
        let typed_from = drafts.title.clone();
        rename_text(&state, &mut drafts, "  Cart total ".to_owned());
        let saved = finish_rename(&mut state, &mut drafts);
        let again = finish_rename(&mut state, &mut drafts);

        assert_eq!(picked, Picked::default());
        assert_eq!(state.ui.popover, None);
        assert_eq!(
            opened,
            Some(TitleEdit {
                agent: agent(),
                title: "Agent 1".to_owned(),
                place: TitlePlace::Header,
                select_all: true,
            })
        );
        assert_eq!(typed_from, "Agent 1");
        assert_eq!(
            sent(&saved),
            [&Request::AgentRename(RenameParams {
                agent: agent(),
                title: "Cart total".to_owned()
            })]
        );
        assert!(again.is_empty(), "the field closed with the first save");
        assert_eq!(state.ui.title_edit, None);
    }

    #[test]
    fn f2_renames_in_the_sidebar_and_moves_the_field_with_its_text() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        let place = |state: &AppState| state.ui.title_edit.as_ref().map(|edit| edit.place);

        rename_key(&mut state, &mut drafts);
        let opened = state.ui.title_edit.clone();
        rename_text(&state, &mut drafts, "Cart total".to_owned());
        rename_key(&mut state, &mut drafts);
        let in_header = place(&state);
        // The row's field leaves the page as the edit moves: its blur does not end the edit.
        let stale_blur = finish_rename_at(&mut state, &mut drafts, TitlePlace::Sidebar);
        rename_key(&mut state, &mut drafts);
        let back = state.ui.title_edit.clone();
        let typed = drafts.title.clone();
        let saved = finish_rename_at(&mut state, &mut drafts, TitlePlace::Sidebar);

        assert_eq!(
            opened,
            Some(TitleEdit {
                agent: agent(),
                title: "Agent 1".to_owned(),
                place: TitlePlace::Sidebar,
                select_all: true,
            })
        );
        assert_eq!(in_header, Some(TitlePlace::Header));
        assert!(stale_blur.is_empty());
        assert_eq!(
            back.map(|edit| (edit.title, edit.place, edit.select_all)),
            Some(("Cart total".to_owned(), TitlePlace::Sidebar, false)),
            "the field that opens starts from the typed text, the caret at its end"
        );
        assert_eq!(typed, "Cart total");
        assert_eq!(
            sent(&saved),
            [&Request::AgentRename(RenameParams {
                agent: agent(),
                title: "Cart total".to_owned()
            })]
        );
        assert_eq!(state.ui.title_edit, None);
    }

    #[test]
    fn f2_on_an_agent_the_filter_hides_opens_the_headers_field() {
        let mut state = state(AgentStatus::Idle);
        set_status_filter(&mut state, StatusFilter::Working);

        rename_key(&mut state, &mut Drafts::default());

        assert_eq!(
            state.ui.title_edit.map(|edit| edit.place),
            Some(TitlePlace::Header)
        );
    }

    #[test]
    fn an_empty_or_unchanged_title_or_escape_renames_nothing() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        let mut saved = |state: &mut AppState, text: &str| {
            start_rename(state, &mut drafts, agent());
            rename_text(state, &mut drafts, text.to_owned());
            finish_rename(state, &mut drafts)
        };

        let empty = saved(&mut state, "   ");
        let unchanged = saved(&mut state, "Agent 1");
        let mut drafts = Drafts::default();
        start_rename(&mut state, &mut drafts, agent());
        rename_text(&state, &mut drafts, "Typed, then Esc".to_owned());
        let closed = state.ui.close_overlays();
        let after_escape = finish_rename(&mut state, &mut drafts);

        assert!(empty.is_empty());
        assert!(unchanged.is_empty());
        assert!(closed);
        assert!(after_escape.is_empty());
    }

    #[test]
    fn left_and_right_change_the_highlighted_setting() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.summary.model = Some("claude-opus-5-5".to_owned());
            entry.summary.effort = Some(Effort::High);
        }
        let mut step = |query: &str, step| {
            open_menu(&mut state, &mut drafts, query);
            // Up on the first entry keeps it highlighted, as moved to.
            palette_move(&state, &mut drafts, agent(), Direction::Up);
            palette_step(&state, &drafts, agent(), step).unwrap_or_default()
        };

        let next_effort = step("effort", Step::Next);
        let before_opus = step("model", Step::Previous);
        let after_opus = step("model", Step::Next);
        let next_mode = step("permission", Step::Next);
        // Left and Right on an action change nothing.
        let on_an_action = step("export", Step::Next);

        assert_eq!(
            sent(&next_effort),
            [&Request::AgentSetEffort(SetEffortParams {
                agent: agent(),
                effort: Effort::Xhigh,
            })]
        );
        assert!(before_opus.is_empty());
        assert_eq!(
            sent(&after_opus),
            [&Request::AgentSetModel(SetModelParams {
                agent: agent(),
                model: "sonnet".to_owned(),
            })]
        );
        assert_eq!(
            sent(&next_mode),
            [&Request::AgentSetMode(SetModeParams {
                agent: agent(),
                mode: PermissionMode::AcceptEdits,
            })]
        );
        assert!(on_an_action.is_empty());
    }

    #[test]
    fn an_unknown_effort_counts_from_the_default_level() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        open_menu(&mut state, &mut drafts, "effort");
        palette_move(&state, &mut drafts, agent(), Direction::Up);

        let lower = palette_step(&state, &drafts, agent(), Step::Previous).unwrap_or_default();
        let higher = palette_step(&state, &drafts, agent(), Step::Next).unwrap_or_default();
        let summary = &state.agents[&agent()].summary;

        assert_eq!(
            sent(&lower),
            [&Request::AgentSetEffort(SetEffortParams {
                agent: agent(),
                effort: Effort::Medium,
            })]
        );
        assert_eq!(
            sent(&higher),
            [&Request::AgentSetEffort(SetEffortParams {
                agent: agent(),
                effort: Effort::Xhigh,
            })]
        );
        assert!(
            setting_choices(summary, MenuAction::Effort)
                .iter()
                .all(|choice| !choice.chosen)
        );
        assert_eq!(
            state.ui.popover,
            Some(Popover::Palette(PaletteOrigin::Button))
        );
    }

    /// Typing "eff" highlights Effort: Left to fix a typo must move the caret, not send
    /// `/effort` to Claude.
    #[test]
    fn left_and_right_change_a_setting_only_once_up_or_down_moved_to_it() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.summary.effort = Some(Effort::High);
        }
        open_menu(&mut state, &mut drafts, "eff");

        let typed = palette_step(&state, &drafts, agent(), Step::Previous);
        palette_move(&state, &mut drafts, agent(), Direction::Up);
        let moved_to = palette_step(&state, &drafts, agent(), Step::Previous);
        palette_filter(&mut drafts, "effo".to_owned());
        let typed_again = palette_step(&state, &drafts, agent(), Step::Previous);
        palette_move(&state, &mut drafts, agent(), Direction::Up);
        open_menu(&mut state, &mut drafts, "export");
        palette_move(&state, &mut drafts, agent(), Direction::Up);
        let on_an_action = palette_step(&state, &drafts, agent(), Step::Next);

        assert_eq!(typed, None);
        assert_eq!(
            moved_to.as_deref().map(sent),
            Some(vec![&Request::AgentSetEffort(SetEffortParams {
                agent: agent(),
                effort: Effort::Medium,
            })])
        );
        assert_eq!(typed_again, None);
        assert_eq!(on_an_action, None);
    }

    #[test]
    fn the_composer_takes_the_draft_only_when_the_app_changes_it() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(&mut state, vec![command("compact", "")]);

        type_text(&mut state, &mut drafts, "/co");
        let after_typing = drafts.composer_writes(agent());
        let _ = palette_pick(&mut state, &mut drafts, agent(), None);
        let after_pick = drafts.composer_writes(agent());
        send_prompt(&mut state, &mut drafts, agent(), None);
        let after_send = drafts.composer_writes(agent());

        assert_eq!((after_typing, after_pick, after_send), (0, 1, 2));
    }

    #[test]
    fn sending_takes_the_typed_text_and_clears_the_draft() {
        let mut state = state(AgentStatus::Working);
        let mut drafts = Drafts::default();
        type_text(&mut state, &mut drafts, "Also cover the edge ca");

        let requests = send_prompt(
            &mut state,
            &mut drafts,
            agent(),
            Some("Also cover the edge case ".to_owned()),
        );

        let Some(Outgoing {
            request: Request::AgentPrompt(params),
            ..
        }) = requests.first()
        else {
            panic!("expected a prompt, got {requests:?}");
        };
        assert_eq!(params.prompt.text, "Also cover the edge case");
        assert!(drafts.draft(agent()).is_some_and(|draft| draft.is_empty()));
    }

    #[test]
    fn an_empty_draft_or_an_exited_agent_sends_nothing() {
        let mut drafts = Drafts::default();
        let mut idle = state(AgentStatus::Idle);
        let typed = Some("  ".to_owned());
        assert!(send_prompt(&mut idle, &mut drafts, agent(), typed).is_empty());

        let mut exited = state(AgentStatus::Exited);
        let typed = Some("hello".to_owned());
        assert!(send_prompt(&mut exited, &mut drafts, agent(), typed).is_empty());
    }

    #[test]
    fn an_image_over_five_megabytes_is_refused_with_a_note() {
        use base64::Engine;

        let png = |bytes: usize| Image {
            media_type: ImageMediaType::Png,
            data: base64::engine::general_purpose::STANDARD.encode(vec![0; bytes]),
        };
        let limit = 5 * 1024 * 1024;
        let mut drafts = Drafts::default();

        let at_limit = add_image(&mut drafts, agent(), png(limit));
        let over = add_image(&mut drafts, agent(), png(limit + 1));

        assert_eq!(at_limit, Ok(()));
        assert_eq!(over, Err(ImageTooLarge { bytes: limit + 1 }));
        assert_eq!(
            ImageTooLarge::check(7_654_321).map_err(|too_large| too_large.note("shot.png")),
            Err(
                "shot.png was not attached: it is 7.3 MB, and Claude reads images up to 5 MB."
                    .to_owned()
            )
        );
        let images = drafts.draft(agent()).map(|draft| draft.images.len());
        assert_eq!(images, Some(1));
    }

    #[test]
    fn the_stop_button_stops_a_working_or_waiting_agent() {
        assert_eq!(interrupt(&state(AgentStatus::Working), agent()).len(), 1);
        assert_eq!(interrupt(&state(AgentStatus::NeedsYou), agent()).len(), 1);
        assert!(interrupt(&state(AgentStatus::Idle), agent()).is_empty());
    }

    #[test]
    fn a_single_choice_question_keeps_one_pick() {
        let mut drafts = Drafts::default();
        let question = question(false);

        pick_option(&mut drafts, &question, 0, 0);
        pick_option(&mut drafts, &question, 0, 2);

        let answers = question_answers(&question, drafts.questions.get(&question.pending));
        assert_eq!(
            answers,
            Some(vec![QuestionAnswer {
                selected: vec!["Drop".to_owned()],
                other: None,
            }])
        );
    }

    #[test]
    fn a_multiple_choice_question_toggles_and_takes_other_text() {
        let mut drafts = Drafts::default();
        let question = question(true);

        pick_option(&mut drafts, &question, 0, 0);
        pick_option(&mut drafts, &question, 0, 1);
        pick_option(&mut drafts, &question, 0, 0);
        set_other_answer(&mut drafts, &question, 0, " Ask the owner ".to_owned());

        let answers = question_answers(&question, drafts.questions.get(&question.pending));
        assert_eq!(
            answers,
            Some(vec![QuestionAnswer {
                selected: vec!["Rewrite".to_owned()],
                other: Some("Ask the owner".to_owned()),
            }])
        );
    }

    #[test]
    fn a_question_is_sent_only_once_answered() {
        let mut state = state(AgentStatus::NeedsYou);
        let mut drafts = Drafts::default();
        let item = with_question(&mut state, question(false));

        assert!(submit_question(&state, &drafts, agent(), item).is_empty());

        pick_by_number(&state, &mut drafts, agent(), item, 2);
        let requests = submit_question(&state, &drafts, agent(), item);

        let Some(Outgoing {
            request: Request::AgentAnswer(params),
            ..
        }) = requests.first()
        else {
            panic!("expected an answer, got {requests:?}");
        };
        assert_eq!(
            params.answer,
            Answer::Question {
                answers: vec![QuestionAnswer {
                    selected: vec!["Rewrite".to_owned()],
                    other: None,
                }]
            }
        );
    }

    #[test]
    fn a_number_past_the_options_picks_nothing() {
        let mut state = state(AgentStatus::NeedsYou);
        let mut drafts = Drafts::default();
        let item = with_question(&mut state, question(false));

        pick_by_number(&state, &mut drafts, agent(), item, 9);

        assert!(submit_question(&state, &drafts, agent(), item).is_empty());
    }

    #[test]
    fn opening_a_link_shows_the_whole_file_at_its_line() {
        let mut state = state(AgentStatus::Idle);

        open_file(
            &mut state,
            agent(),
            PathBuf::from("/work/project-1/src/lib.rs"),
            Some(12),
        );

        assert_eq!(
            state.ui.file_view(agent()),
            Some(&FileView {
                opened: vec![PathBuf::from("src/lib.rs")],
                ..FileView::new(PathBuf::from("src/lib.rs"), Some(12), ViewMode::WholeFile)
            })
        );
    }

    #[test]
    fn tabs_close_one_by_one_and_the_last_closes_the_view() {
        let mut state = state(AgentStatus::Idle);
        let changed = |path: &str| ChangedFile {
            path: PathBuf::from("/work/project-1").join(path),
            lines_added: 1,
            lines_removed: 0,
        };
        state.agents.get_mut(&agent()).unwrap().details = Loadable::Loaded(AgentDetails {
            changed_files: vec![changed("a.rs"), changed("b.rs")],
            ..AgentDetails::default()
        });
        let tabs = |state: &AppState| -> (Vec<String>, String) {
            let entry = &state.agents[&agent()];
            let view = state.ui.file_view(agent()).unwrap();
            let names = view.tabs(&entry.changed_paths());
            let names = names.iter().map(|tab| tab.display().to_string()).collect();
            (names, view.path.display().to_string())
        };
        let path = |name: &str| PathBuf::from(name);

        open_file(&mut state, agent(), path("c.rs"), None);
        assert_eq!(
            tabs(&state),
            (
                vec!["a.rs".into(), "b.rs".into(), "c.rs".into()],
                "c.rs".into()
            )
        );

        // The shown last tab gives way to the one before it.
        close_tab(&mut state, agent(), Path::new("c.rs"));
        assert_eq!(
            tabs(&state),
            (vec!["a.rs".into(), "b.rs".into()], "b.rs".into())
        );

        // A changed file's tab stays closed until the file opens again.
        close_tab(&mut state, agent(), Path::new("a.rs"));
        assert_eq!(tabs(&state), (vec!["b.rs".into()], "b.rs".into()));
        open_file(&mut state, agent(), path("a.rs"), None);
        assert_eq!(
            tabs(&state),
            (vec!["a.rs".into(), "b.rs".into()], "a.rs".into())
        );

        // The shown first tab gives way to the one after it.
        close_tab(&mut state, agent(), Path::new("a.rs"));
        assert_eq!(tabs(&state), (vec!["b.rs".into()], "b.rs".into()));

        close_tab(&mut state, agent(), Path::new("b.rs"));
        assert_eq!(state.ui.file_view(agent()), None);
        assert!(state.agents[&agent()].files.is_empty());
    }

    #[test]
    fn a_picked_file_takes_the_tab_of_the_name() {
        let mut state = state(AgentStatus::Idle);
        let picked = PathBuf::from("/work/project-1_review/src/b/widget.ts");

        open_file(&mut state, agent(), PathBuf::from("widget.ts"), Some(3));
        pick_file(
            &mut state,
            agent(),
            Path::new("widget.ts"),
            picked.clone(),
            Some(3),
        );

        let view = state.ui.file_view(agent()).unwrap();
        assert_eq!(view.tabs(&[]), [picked]);
        assert_eq!(view.line, Some(3));
    }

    #[test]
    fn only_the_shown_file_keeps_its_text() {
        let mut state = state(AgentStatus::Idle);
        let read = |state: &mut AppState, path: &str| {
            apply_response(
                state,
                &mut Drafts::default(),
                Method::AgentReadFile,
                Purpose::ReadFile {
                    agent: agent(),
                    path: PathBuf::from(path),
                },
                Ok(ResponseBody::File {
                    path: PathBuf::from("/work/project-1").join(path),
                    text: format!("text of {path}"),
                }),
            );
        };
        open_file(&mut state, agent(), PathBuf::from("a.rs"), None);
        read(&mut state, "a.rs");

        open_file(&mut state, agent(), PathBuf::from("b.rs"), None);
        read(&mut state, "c.rs");
        let held: Vec<PathBuf> = state.agents[&agent()].files.keys().cloned().collect();
        assert_eq!(held, [PathBuf::from("b.rs")]);

        close_file_view(&mut state, agent());
        assert!(state.agents[&agent()].files.is_empty());
    }

    fn read_answer(state: &mut AppState, path: &str, result: Result<ResponseBody, RequestError>) {
        apply_response(
            state,
            &mut Drafts::default(),
            Method::AgentReadFile,
            Purpose::ReadFile {
                agent: agent(),
                path: PathBuf::from(path),
            },
            result,
        );
    }

    #[test]
    fn a_name_the_daemon_found_elsewhere_moves_the_view_to_the_file() {
        let mut state = state(AgentStatus::Idle);
        let name = "register-parcel.component.ts";
        let found = "/work/project-1_parcel/front/src/app/register-parcel.component.ts";
        open_file(&mut state, agent(), PathBuf::from(name), Some(249));

        read_answer(
            &mut state,
            name,
            Ok(ResponseBody::File {
                path: PathBuf::from(found),
                text: "export class RegisterParcel {}\n".to_owned(),
            }),
        );

        let view = state.ui.file_view(agent()).unwrap();
        assert_eq!(
            (view.path.as_path(), view.line),
            (Path::new(found), Some(249))
        );
        let held: Vec<&PathBuf> = state.agents[&agent()].files.keys().collect();
        assert_eq!(held, [&PathBuf::from(found)]);
    }

    #[test]
    fn several_matches_wait_for_a_pick_and_a_miss_says_why() {
        let mut state = state(AgentStatus::Idle);
        let matches = vec![
            PathBuf::from("/work/project-1/src/a/widget.ts"),
            PathBuf::from("/work/project-1_review/src/b/widget.ts"),
        ];
        let missing = "cache.rs is not in the agent's folder or in the files it read or changed";
        let shown = |state: &AppState, path: &str| {
            state.agents[&agent()].files.get(Path::new(path)).cloned()
        };

        open_file(&mut state, agent(), PathBuf::from("widget.ts"), None);
        read_answer(
            &mut state,
            "widget.ts",
            Ok(ResponseBody::FileMatches {
                matches: matches.clone(),
            }),
        );
        assert_eq!(
            shown(&state, "widget.ts"),
            Some(Loadable::Loaded(FileRead::Choices(matches)))
        );

        open_file(&mut state, agent(), PathBuf::from("cache.rs"), None);
        let error = ErrorBody::new(ErrorCode::NotFound, missing.to_owned());
        read_answer(&mut state, "cache.rs", Err(RequestError::Failed(error)));
        assert_eq!(
            shown(&state, "cache.rs"),
            Some(Loadable::Loaded(FileRead::Missing(missing.to_owned())))
        );

        open_file(&mut state, agent(), PathBuf::from("image.png"), None);
        let error = ErrorBody::new(ErrorCode::FileUnreadable, "not text".to_owned());
        read_answer(&mut state, "image.png", Err(RequestError::Failed(error)));
        assert_eq!(
            shown(&state, "image.png"),
            Some(Loadable::Failed("not text".to_owned()))
        );
    }

    #[test]
    fn a_move_shows_at_once_and_sends_the_request() {
        let mut state = ordered();

        let requests = move_agent(&mut state, AgentId::new(1), Some(AgentId::new(3)));

        assert_eq!(sidebar(&state), [1, 3, 2]);
        assert_eq!(
            sent_move(&requests),
            Some(MoveParams {
                agent: AgentId::new(1),
                before: Some(AgentId::new(3)),
            })
        );
        assert!(move_agent(&mut state, AgentId::new(1), Some(AgentId::new(3))).is_empty());
    }

    #[test]
    fn a_refused_move_puts_the_agents_back_unless_an_event_moved_them() {
        let mut state = ordered();
        let mut drafts = Drafts::default();
        let refused = || {
            Err(cockpit_client::RequestError::Failed(
                cockpit_protocol::ErrorBody::new(
                    cockpit_protocol::ErrorCode::NotFound,
                    "no agent 3".to_owned(),
                ),
            ))
        };

        let first = move_agent(&mut state, AgentId::new(1), Some(AgentId::new(3)));
        for outgoing in first {
            apply_response(
                &mut state,
                &mut drafts,
                Method::AgentMove,
                outgoing.purpose,
                refused(),
            );
        }
        let after_refusal = sidebar(&state);
        let second = move_agent(&mut state, AgentId::new(1), Some(AgentId::new(2)));
        if let Some(entry) = state.agents.get_mut(&AgentId::new(1)) {
            entry.summary.position = cockpit_protocol::Position::new(9000);
        }
        for outgoing in second {
            apply_response(
                &mut state,
                &mut drafts,
                Method::AgentMove,
                outgoing.purpose,
                refused(),
            );
        }

        assert_eq!(after_refusal, [3, 2, 1]);
        assert_eq!(sidebar(&state), [1, 3, 2], "the event's position stays");
        assert_eq!(state.ui.errors.len(), 2);
    }

    #[test]
    fn a_row_dropped_below_the_last_one_goes_to_the_bottom() {
        let mut state = ordered();

        let to_bottom = drop_agent(&mut state, AgentId::new(3), None);
        let to_top = drop_agent(&mut state, AgentId::new(1), Some(AgentId::new(2)));

        assert_eq!(sent_move(&to_bottom).and_then(|params| params.before), None);
        assert_eq!(
            sent_move(&to_top).and_then(|params| params.before),
            Some(AgentId::new(2))
        );
        assert_eq!(sidebar(&state), [1, 2, 3]);
    }

    #[test]
    fn alt_arrows_move_the_selected_agent_one_row() {
        let mut state = ordered();

        let up = move_selected(&mut state, Direction::Up);
        let after_up = sidebar(&state);
        let at_top = move_selected(&mut state, Direction::Up);
        move_selected(&mut state, Direction::Down);
        let down = move_selected(&mut state, Direction::Down);

        assert_eq!(
            sent_move(&up).and_then(|params| params.before),
            Some(AgentId::new(3))
        );
        assert_eq!(after_up, [2, 3, 1]);
        assert!(at_top.is_empty());
        assert_eq!(sent_move(&down).and_then(|params| params.before), None);
        assert_eq!(sidebar(&state), [3, 1, 2]);
    }

    /// Agents 3, 2 and 1 from the top, with agent 2 working and the sidebar showing idle agents.
    #[test]
    fn moves_go_past_the_rows_the_filter_hides_and_leave_them_in_place() {
        let mut state = ordered();
        if let Some(entry) = state.agents.get_mut(&AgentId::new(3)) {
            entry.summary.status = AgentStatus::Idle;
        }
        set_status_filter(&mut state, StatusFilter::Idle);
        state.ui.selected = Some(AgentId::new(3));

        let down = move_selected(&mut state, Direction::Down);
        let after_down = ids(&state.agents_in_order());
        let dropped = drop_agent(&mut state, AgentId::new(1), None);
        state.ui.selected = Some(AgentId::new(2));
        let hidden = move_selected(&mut state, Direction::Up);

        assert_eq!(sent_move(&down).and_then(|params| params.before), None);
        assert_eq!(after_down, [2, 1, 3]);
        assert_eq!(
            sent_move(&dropped).and_then(|params| params.before),
            None,
            "below the last row it shows"
        );
        assert_eq!(ids(&state.agents_in_order()), [2, 3, 1]);
        assert!(hidden.is_empty(), "the hidden selected agent stays");
    }

    #[test]
    fn the_splitter_keeps_a_share_of_the_window_and_double_click_resets_it() {
        let mut state = state(AgentStatus::Idle);
        let default = state.prefs.file_view.share();

        set_file_view_share(&mut state, 0.437_512_3);
        assert_eq!(state.prefs.file_view.width_share, Some(0.4375));

        for refused in [0.0, 1.0, -0.2, f64::NAN, f64::INFINITY] {
            set_file_view_share(&mut state, refused);
            assert_eq!(state.prefs.file_view.share(), 0.4375, "{refused}");
        }

        reset_file_view_share(&mut state);
        assert_eq!(state.prefs.file_view.width_share, None);
        assert_eq!(state.prefs.file_view.share(), default);
    }

    #[test]
    fn blocks_fold_when_floating_and_open_as_badges_beside_the_file_view() {
        let mut state = state(AgentStatus::Idle);

        let requests = toggle_block(&mut state, agent(), Block::Mcp);
        assert!(state.ui.open_blocks.contains(&Block::Mcp));
        assert_eq!(
            requests.first().map(|outgoing| outgoing.request.method()),
            Some(Method::AgentMcpStatus)
        );

        toggle_block(&mut state, agent(), Block::Tasks);
        assert!(!state.ui.open_blocks.contains(&Block::Tasks));

        open_file(&mut state, agent(), PathBuf::from("a.rs"), None);
        toggle_block(&mut state, agent(), Block::Running);
        assert_eq!(state.ui.badge, Some(Block::Running));
        toggle_block(&mut state, agent(), Block::Running);
        assert_eq!(state.ui.badge, None);
    }

    #[test]
    fn hiding_a_server_sends_the_new_settings() {
        let mut state = state(AgentStatus::Idle);

        let requests = set_mcp_hidden(&mut state, "search", true);

        assert!(state.settings.hidden_mcp_servers.contains("search"));
        assert_eq!(
            requests.first().map(|outgoing| &outgoing.request),
            Some(&Request::SettingsSet(state.settings.clone()))
        );
    }

    #[test]
    fn the_switcher_filters_and_opens_the_highlighted_agent() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Working),
            summary(3, AgentStatus::Idle),
        ]);
        let mut drafts = Drafts::default();
        toggle_switcher(&mut state, &mut drafts);
        switcher_query(&mut drafts, "project".to_owned());

        switcher_move(&state, &mut drafts, Direction::Down);
        switcher_move(&state, &mut drafts, Direction::Down);
        switcher_move(&state, &mut drafts, Direction::Down);
        switcher_pick(&mut state, &drafts, None);

        assert_eq!(state.ui.selected, Some(AgentId::new(3)));
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn starting_an_agent_sends_the_form_once() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        drafts.new_agent.prompt = "Fix the flaky test".to_owned();
        with_new_agent_form(&mut state, |form| form.mode = Some(PermissionMode::Plan));

        let first = start_agent(&mut state, &drafts);
        let second = start_agent(&mut state, &drafts);

        let Some(Outgoing {
            request: Request::AgentStart(params),
            ..
        }) = first.first()
        else {
            panic!("expected a start, got {first:?}");
        };
        assert_eq!(params.folder, PathBuf::from("/work/project-1"));
        assert_eq!(params.mode, Some(PermissionMode::Plan));
        assert_eq!(
            params.prompt.as_ref().map(|prompt| prompt.text.as_str()),
            Some("Fix the flaky test")
        );
        assert!(second.is_empty());
    }

    #[test]
    fn the_new_agent_form_starts_from_the_last_agent_started() {
        // The last agent started has the lower id, so that the order of the ids cannot pass for
        // the order of the starts.
        let mut older = summary(2, AgentStatus::Idle);
        older.model = Some("claude-haiku-4-5".to_owned());
        older.effort = Some(Effort::Low);
        older.mode = PermissionMode::Plan;
        let mut last = summary(1, AgentStatus::Exited);
        last.model = Some("claude-opus-5-5[1m]".to_owned());
        last.effort = Some(Effort::Xhigh);
        last.mode = PermissionMode::AcceptEdits;
        last.started_at = Timestamp::from_unix_millis(90_000);
        let mut state = state_with(vec![last, older]);

        open_new_agent(&mut state, &mut Drafts::default());

        let Some(Dialog::NewAgent(form)) = &state.ui.dialog else {
            panic!("expected the new agent dialog, got {:?}", state.ui.dialog);
        };
        assert_eq!(
            (form.model.as_deref(), form.effort, form.mode),
            (
                Some("opus"),
                Some(Effort::Xhigh),
                Some(PermissionMode::AcceptEdits)
            )
        );
    }

    #[test]
    fn the_new_agent_form_keeps_the_defaults_without_an_agent_or_a_known_model() {
        let mut state = state_with(Vec::new());
        open_new_agent(&mut state, &mut Drafts::default());
        let Some(Dialog::NewAgent(form)) = &state.ui.dialog else {
            panic!("expected the new agent dialog, got {:?}", state.ui.dialog);
        };
        assert_eq!(
            (form.model.as_deref(), form.effort, form.mode),
            (None, None, None)
        );

        let mut unknown = summary(1, AgentStatus::Idle);
        unknown.model = Some("claude-mythical-1".to_owned());
        let mut state = state_with(vec![unknown]);
        open_new_agent(&mut state, &mut Drafts::default());
        let Some(Dialog::NewAgent(form)) = &state.ui.dialog else {
            panic!("expected the new agent dialog, got {:?}", state.ui.dialog);
        };
        assert_eq!(form.model, None);
        assert_eq!(form.mode, Some(PermissionMode::Default));
    }

    /// Ctrl+N then Ctrl+Enter would otherwise start an agent that runs everything without
    /// asking, in a mode the dialog never offers.
    #[test]
    fn the_new_agent_form_carries_over_only_a_mode_it_offers_other_than_plan() {
        let carried = |mode| {
            let mut last = summary(1, AgentStatus::Idle);
            last.mode = mode;
            let mut state = state_with(vec![last]);
            open_new_agent(&mut state, &mut Drafts::default());
            match &state.ui.dialog {
                Some(Dialog::NewAgent(form)) => form.mode,
                other => panic!("expected the new agent dialog, got {other:?}"),
            }
        };

        assert_eq!(carried(PermissionMode::BypassPermissions), None);
        assert_eq!(carried(PermissionMode::DontAsk), None);
        assert_eq!(carried(PermissionMode::Plan), None);
        assert_eq!(carried(PermissionMode::Auto), Some(PermissionMode::Auto));
    }

    #[test]
    fn opening_the_open_new_agent_dialog_keeps_its_form() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        drafts.new_agent.prompt = "Fix the flaky test".to_owned();
        with_new_agent_form(&mut state, |form| form.effort = Some(Effort::Max));

        open_new_agent(&mut state, &mut drafts);

        assert_eq!(drafts.new_agent.prompt, "Fix the flaky test");
        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::NewAgent(form)) if form.effort == Some(Effort::Max)
        ));
    }

    #[test]
    fn each_opening_of_the_new_agent_dialog_lists_the_sessions_again() {
        let mut state = state(AgentStatus::Idle);
        open_new_agent(&mut state, &mut Drafts::default());
        let first = needed_requests(&mut state);
        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::SessionsList,
            Purpose::Sessions,
            Ok(ResponseBody::Sessions {
                sessions: Vec::new(),
            }),
        );
        state.ui.dialog = None;

        open_new_agent(&mut state, &mut Drafts::default());
        let second = needed_requests(&mut state);

        let lists = |requests: &[Outgoing]| {
            requests
                .iter()
                .filter(|outgoing| outgoing.request == Request::SessionsList)
                .count()
        };
        assert_eq!((lists(&first), lists(&second)), (1, 1));
    }

    fn past_session(id: &str, title: &str, folder: &str, minutes_ago: u64) -> PastSession {
        PastSession {
            id: ClaudeSessionId::new(id.to_owned()),
            title: title.to_owned(),
            folder: PathBuf::from(folder),
            last_activity: Timestamp::from_unix_millis(10_000_000 - minutes_ago * 60_000),
            turns: 4,
        }
    }

    #[test]
    fn the_session_search_filters_by_title_and_folder_and_resumes_the_highlighted_one() {
        let mut running = summary(1, AgentStatus::Idle);
        running.session = Some(ClaudeSessionId::new("running".to_owned()));
        let mut state = state_with(vec![running]);
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        state.sessions = Loadable::Loaded(vec![
            past_session("lexer", "Speed up the lexer", "/work/parser", 30),
            past_session("running", "Fix the parser", "/work/parser", 5),
            past_session("docs", "Rewrite the error page", "/work/docs-site", 60),
            past_session("columns", "Parser columns", "/work/tools", 10),
        ]);
        let ids = |state: &AppState, query: &str| -> Vec<String> {
            resumable_sessions(state, query)
                .iter()
                .map(|session| session.id.as_str().to_owned())
                .collect()
        };

        assert_eq!(ids(&state, ""), ["columns", "lexer", "docs"]);
        assert_eq!(ids(&state, "PARSER"), ["columns", "lexer"]);
        assert_eq!(ids(&state, "docs-site"), ["docs"]);
        assert!(ids(&state, "nothing like it").is_empty());

        session_query(&mut drafts, "parser".to_owned());
        session_move(&state, &mut drafts, Direction::Down);
        session_move(&state, &mut drafts, Direction::Down);
        let requests = session_pick(&mut state, &drafts, None);

        assert_eq!(
            requests.first().map(|outgoing| &outgoing.request),
            Some(&Request::SessionsResume(SessionResumeParams {
                session: ClaudeSessionId::new("lexer".to_owned())
            }))
        );
        session_query(&mut drafts, "nothing like it".to_owned());
        assert_eq!(drafts.new_agent.session_highlighted, 0);
    }

    #[test]
    fn the_context_loads_from_the_opening_of_the_usage_card_to_its_answer() {
        let mut state = state(AgentStatus::Idle);
        let loading = |state: &AppState| {
            state
                .agents
                .get(&agent())
                .is_some_and(|entry| entry.context_loading)
        };

        let requests = toggle_usage(&mut state, agent());
        assert!(loading(&state));

        let Some(Outgoing { purpose, .. }) = requests.into_iter().next() else {
            panic!("expected a context usage request");
        };
        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentContextUsage,
            purpose,
            Err(cockpit_client::RequestError::Disconnected),
        );
        assert!(!loading(&state));
    }

    #[test]
    fn loading_older_items_asks_before_the_first_one() {
        let mut state = state(AgentStatus::Idle);
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.transcript.merge_page(HistoryPage {
                items: vec![TranscriptItem {
                    id: ItemId::new(50),
                    parent: None,
                    body: ItemBody::Notice {
                        text: "compacted".to_owned(),
                    },
                }],
                has_older: true,
            });
        }

        let requests = load_older(&mut state, agent());

        let Some(Outgoing {
            request: Request::AgentHistory(params),
            ..
        }) = requests.first()
        else {
            panic!("expected a history request, got {requests:?}");
        };
        assert_eq!(params.before, Some(ItemId::new(50)));
        assert!(load_older(&mut state, agent()).is_empty());
    }
}
