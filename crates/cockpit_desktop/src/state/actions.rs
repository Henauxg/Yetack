//! What the user's clicks and keys change, and the requests they send. The connection sends the
//! returned requests, then the ones `needed_requests` adds.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use cockpit_client::RequestError;
use cockpit_protocol::{
    AccountId, AgentId, AgentParams, AgentStatus, AgentSummary, Answer, AnswerParams, AnswerState,
    ApprovalAnswer, ApprovalDecision, BackgroundTaskId, COMMAND_PREFIX, ClaudeSessionId, Effort,
    EffortSource, Image, ItemBody, ItemId, McpRemoveParams, McpScope, McpToggleAction,
    McpToggleParams, MoveParams, PastSession, PendingId, PermissionMode, Placed, PlanAnswer,
    PlanDecision, Prompt, PromptDelivery, PromptId, PromptParams, Question, QuestionAnswer,
    QueuedPromptParams, RenameParams, Request, Selection, SessionResumeParams, SetEffortParams,
    SetModeParams, SetModelParams, SlashCommand, StartParams, StopTaskParams, Timestamp,
    TranscriptItem,
};
use strum::VariantArray;

use super::app_state::{AgentEntry, AppState, FileFreshness, FileRead, Loadable, MachineState};
use super::apply::forget_agent;
use super::drafts::{Drafts, NewAgentDraft, Palette, QuestionDraft, SlashWord, SwitcherDraft};
use super::projects::ProjectKey;
use super::requests::{Outgoing, Purpose, Shift, history_request};
use super::transcript::HistoryState;
use super::ui_state::{
    Block, Dialog, FileView, NewAgentForm, NewAgentTab, PaletteOrigin, Popover, Reopening,
    TitleEdit, TitlePlace, ViewMode, is_markdown,
};
use super::utf16_offset::Utf16Offset;
use super::{
    accounts, export, file_edits, file_reread, file_tree, folder_field, grouping, open_file,
    projects, quotes, sending, terminals, title_emoji,
};
use crate::format::{self, MODE_CHOICES, MODEL_CHOICES};
use crate::machine::MachineId;
use crate::path_style;
use crate::prefs::{self, EscStop, GroupBy, StatusFilter};

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

/// A rename the window left open, which no field shows once another agent is selected, ends
/// without saving. The map or the stop dialog of another agent closes, as it would cover the one
/// selected.
pub fn select_agent(state: &mut AppState, agent: AgentId) {
    state
        .ui
        .title_edit
        .take_if(|edit| edit.kept_open && edit.agent != agent);
    state.ui.dialog.take_if(|dialog| match dialog {
        Dialog::AgentMap(view) => view.agent != agent,
        // Its keys type into the composer, which shows the selected agent's message.
        Dialog::StopAgent(stopped) => *stopped != agent,
        _ => false,
    });
    let back = state.ui.selected != Some(agent);
    if back && let Some(entry) = state.agents.get(&agent) {
        crate::perf::switched(&entry.summary.title);
    }
    // The agent left keeps no more items than any other agent not looked at.
    if let Some(left) = state.ui.selected.filter(|_| back)
        && let Some(entry) = state.agents.get_mut(&left)
    {
        entry.transcript.trim(state.ui.kept_items_away(left));
    }
    state.ui.selected = Some(agent);
    state.ui.chat_new_from.clear();
    state.ui.unread.remove(&agent);
    state.ui.leave_agent();
    if back {
        file_reread::reread(state, agent);
        if let Some(entry) = state.agents.get_mut(&agent) {
            file_tree::refresh(&mut entry.tree);
        }
    }
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
    let counted = entry
        .transcript
        .items_after(last_seen)
        .filter(|item| is_counted(item));
    let mut news = NewEntries::default();
    for item in counted {
        news.first = news.first.or(Some(item.id));
        news.count += 1;
    }
    news
}

/// Whether a reader counts the item among the new entries: a top-level message, Claude's text, a
/// tool call or a card.
pub fn is_counted(item: &TranscriptItem) -> bool {
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

/// The composer's text changed, with the caret at `caret`. The "/" word the caret is in filters
/// the palette, and opens it, unless Esc closed it on that word; anywhere else, the caret closes
/// a palette opened by typing. A palette opened by the slash button has a filter field of its
/// own, so the composer's text leaves it as it is. Only the drafts change: the caller applies the
/// returned change with `change_palette`, so that a keystroke that leaves the palette as it is
/// leaves the rest of the state alone.
pub fn composer_input(
    state: &AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    text: String,
    caret: Utf16Offset,
) -> Option<PaletteChange> {
    let caret = caret.byte_offset(&text);
    let word = slash_word_at(&text, caret);
    let dismissed = drafts.palette_dismissed.get(&agent).copied();
    if dismissed != word.map(|word| word.start) {
        drafts.palette_dismissed.remove(&agent);
    }
    let word = word.filter(|word| dismissed != Some(word.start));
    let change = match (state.ui.popover, word) {
        (Some(Popover::Palette(PaletteOrigin::Button)), _) => None,
        (popover, Some(word)) => {
            let query = text.get(word.start + 1..caret).unwrap_or_default();
            drafts.palette = Palette {
                query: query.to_owned(),
                word: Some(word),
                ..Palette::default()
            };
            let typed_open = popover == Some(Popover::Palette(PaletteOrigin::Typed));
            (!typed_open).then_some(PaletteChange::Open)
        }
        (Some(Popover::Palette(PaletteOrigin::Typed)), None) => Some(PaletteChange::Close),
        (_, None) => None,
    };
    drafts.draft_mut(agent).text = text;
    drafts.draft_typed(agent);
    change
}

/// The "/" word the caret, a byte offset, is in, after its "/": a "/" at the start of the text or
/// after a space or a line break, then the rest of the word, without a second "/". So
/// "src/lib.rs" and a path such as "/home/dev" are no command.
pub fn slash_word_at(text: &str, caret: usize) -> Option<SlashWord> {
    let before = text.get(..caret)?;
    let start = before
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map_or(0, |(offset, character)| offset + character.len_utf8());
    if caret <= start {
        return None;
    }
    let after = text.get(caret..)?;
    let end = caret + after.find(char::is_whitespace).unwrap_or(after.len());
    let word = text.get(start..end)?.strip_prefix(COMMAND_PREFIX)?;
    (!word.contains(COMMAND_PREFIX)).then_some(SlashWord { start, end })
}

/// `text` with the command in place of `word`, and the caret after it. A space follows the
/// command, the one already there or an added one.
pub(super) fn with_command_at(
    text: &str,
    word: SlashWord,
    name: &str,
) -> Option<(String, Utf16Offset)> {
    let before = text.get(..word.start)?;
    let after = text.get(word.end..)?;
    let spaced = after.starts_with(' ');
    let space = if spaced { "" } else { " " };
    let written = format!("{before}{COMMAND_PREFIX}{name}{space}{after}");
    let caret = before.len() + COMMAND_PREFIX.len_utf8() + name.len() + 1;
    let caret_units = Utf16Offset::end_of(written.get(..caret)?);
    Some((written, caret_units))
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

/// An agent whose commands the app has not seen yet is asked for them.
pub(super) fn commands_request(state: &mut AppState, agent: AgentId) -> Option<Outgoing> {
    let entry = state.agents.get_mut(&agent)?;
    if !entry.commands.is_empty() || entry.commands_requested {
        return None;
    }
    entry.commands_requested = true;
    Some(Outgoing {
        request: Request::AgentCommands(AgentParams { agent }),
        purpose: Purpose::Commands { agent },
        to: MachineId::THIS,
    })
}

/// How the palette's query picks commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandMatch {
    /// The name holds the query, or the description has it from the start of a word.
    Anywhere,
    /// The name starts with the query: for a "/" word inside a message, which may be a path that
    /// Enter would otherwise turn into a command.
    NameStart,
}

/// A "/" word that does not start the message matches command names by their start alone.
pub fn command_match(palette: &Palette) -> CommandMatch {
    match palette.word {
        Some(word) if word.start > 0 => CommandMatch::NameStart,
        Some(_) | None => CommandMatch::Anywhere,
    }
}

/// How well a command matches the query, the best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CommandRank {
    /// `/rev` for `review`.
    NameStart,
    /// `/arch` for `workdocs-archive`.
    NameWordStart,
    /// `/view` for `review`.
    InName,
    /// `/branch` or `the branch` for a command described as "Review the branch".
    DescriptionWordStart,
}

/// Whether `query` is in `text` from the start of a word, as `arch` is in `workdocs-archive` and
/// `review-arch` in `plugin:review-architecture`. The query may run past the word, as
/// `the branch` in "Review the branch".
fn from_a_word_start(text: &str, query: &str) -> bool {
    let mut previous: Option<char> = None;
    for (at, character) in text.char_indices() {
        let word_start = character.is_alphanumeric()
            && previous.is_none_or(|previous| !previous.is_alphanumeric());
        if word_start && text.get(at..).is_some_and(|rest| rest.starts_with(query)) {
            return true;
        }
        previous = Some(character);
    }
    false
}

fn command_rank(command: &SlashCommand, query: &str, how: CommandMatch) -> Option<CommandRank> {
    let name = command.name.to_lowercase();
    if name.starts_with(query) {
        return Some(CommandRank::NameStart);
    }
    if how == CommandMatch::NameStart {
        return None;
    }
    if from_a_word_start(&name, query) {
        return Some(CommandRank::NameWordStart);
    }
    if name.contains(query) {
        return Some(CommandRank::InName);
    }
    from_a_word_start(&command.description.to_lowercase(), query)
        .then_some(CommandRank::DescriptionWordStart)
}

/// The commands that match the query, the best matches first, each rank in the list's order. A
/// query inside a description's word, as "arch" in "search", is no match.
pub fn filtered_commands<'command>(
    commands: &'command [SlashCommand],
    query: &str,
    how: CommandMatch,
) -> Vec<&'command SlashCommand> {
    let query = query
        .trim_start_matches(COMMAND_PREFIX)
        .trim()
        .to_lowercase();
    let mut matching: Vec<(CommandRank, &SlashCommand)> = commands
        .iter()
        .filter_map(|command| Some((command_rank(command, &query, how)?, command)))
        .collect();
    matching.sort_by_key(|(rank, _)| *rank);
    matching.into_iter().map(|(_, command)| command).collect()
}

fn agent_commands(state: &AppState, agent: AgentId) -> &[SlashCommand] {
    state
        .agents
        .get(&agent)
        .map_or(&[], |entry| entry.commands.as_slice())
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
    /// Opens the emoji picker, whose emoji starts the title.
    SetEmoji,
    /// Opens the "Open file" dialog.
    OpenFile,
    CloseAgent,
    /// Shows or hides the box that times each switch of agent.
    PerformanceInfo,
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
            Self::SetEmoji => "Set emoji",
            Self::OpenFile => "Open file",
            Self::CloseAgent => "Close agent",
            Self::PerformanceInfo => "Performance info",
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
            | Self::Rename
            | Self::SetEmoji
            | Self::OpenFile
            | Self::CloseAgent
            | Self::PerformanceInfo => MenuSection::Context,
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
            | Self::SetEmoji
            | Self::OpenFile
            | Self::CloseAgent
            | Self::PerformanceInfo
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
            | Self::Rename
            | Self::SetEmoji
            | Self::OpenFile
            | Self::CloseAgent
            | Self::PerformanceInfo => Vec::new(),
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
    how: CommandMatch,
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
    let commands = filtered_commands(commands, query, how);
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
    let how = command_match(&drafts.palette);
    palette_entries(
        agent_commands(state, agent),
        &drafts.palette.query,
        how,
        origin,
    )
}

/// Whether the open palette lists any entry.
pub fn palette_has_entries(state: &AppState, drafts: &Drafts, agent: AgentId) -> bool {
    !open_palette_entries(state, drafts, agent).is_empty()
}

pub fn palette_move(state: &AppState, drafts: &mut Drafts, agent: AgentId, direction: Direction) {
    let count = open_palette_entries(state, drafts, agent).len();
    drafts.palette.highlighted = moved(drafts.palette.highlighted, count, direction);
    drafts.palette.moved = true;
}

/// The pointer moved onto the entry at `index`: it takes the highlight. Left and Right still
/// change a setting only after Up or Down. An entry of a list the filter has since narrowed
/// changes nothing.
pub fn palette_point(state: &AppState, drafts: &mut Drafts, agent: AgentId, index: usize) {
    if index < open_palette_entries(state, drafts, agent).len() {
        drafts.palette.highlighted = index;
    }
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
            let word = drafts.palette.word;
            let draft = drafts.draft_mut(agent);
            // A command picked while typing replaces its "/" word; one from the slash button's
            // menu goes at the start.
            let typed = word.and_then(|word| with_command_at(&draft.text, word, &name));
            match typed {
                Some((text, caret)) => {
                    draft.text = text;
                    drafts.draft_written_at(agent, caret);
                }
                None => {
                    draft.text = with_command(&draft.text, &name);
                    drafts.draft_written(agent);
                }
            }
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
                "Copied the conversation to the clipboard as Markdown, without the older messages the window has not loaded. Scroll to the top of the conversation to load them, then export again."
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
        MenuAction::SetEmoji => {
            state.ui.popover = Some(Popover::TitleEmoji);
            return Picked::default();
        }
        MenuAction::OpenFile => {
            open_file::open(state, drafts);
            Picked::default()
        }
        MenuAction::CloseAgent => Picked {
            requests: request_close(state, drafts, agent),
            window: None,
        },
        MenuAction::PerformanceInfo => {
            state.ui.perf_info = !state.ui.perf_info;
            let label = MenuAction::PerformanceInfo.label();
            let note = if state.ui.perf_info {
                format!(
                    "{label} is on: the box at the bottom right times each switch of agent. Pick \"{label}\" again to hide it."
                )
            } else {
                format!("{label} is off.")
            };
            state.ui.inform(note);
            Picked::default()
        }
        MenuAction::Model | MenuAction::Effort | MenuAction::Mode => Picked::default(),
    };
    state.ui.popover = None;
    picked
}

/// Starts the agent's title with `emoji`, in place of the emoji it starts with, or removes that
/// emoji for `None`. The rename shows it wherever the title shows, Claude Code's resume list
/// included.
pub fn set_title_emoji(state: &mut AppState, agent: AgentId, emoji: Option<&str>) -> Vec<Outgoing> {
    state.ui.popover = None;
    let Some(entry) = state.agents.get(&agent) else {
        return Vec::new();
    };
    let title = title_emoji::with_emoji(&entry.summary.title, emoji);
    // A title of an emoji alone has nothing left without it, and the daemon refuses it empty.
    if title.is_empty() || title == entry.summary.title {
        return Vec::new();
    }
    vec![Outgoing::action(Request::AgentRename(RenameParams {
        agent,
        title,
    }))]
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
        kept_open: false,
    });
    state.ui.popover = None;
}

/// The window lost the focus while a title field had it, which keeps the edit open, or the field
/// took the focus back.
pub fn set_title_kept_open(state: &mut AppState, kept_open: bool) {
    if let Some(edit) = &mut state.ui.title_edit {
        edit.kept_open = kept_open;
    }
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
    let place = if has_row(state, agent) {
        wanted
    } else {
        TitlePlace::Header
    };
    start_rename_at(state, drafts, agent, place);
}

/// The filter hid the row whose title is being edited, as when the agent's status changed: the
/// edit moves to the header with the text typed so far, as F2 opens it for an agent without a row.
pub fn follow_hidden_title_edit(state: &mut AppState, drafts: &mut Drafts) {
    let in_row = state
        .ui
        .title_edit
        .as_ref()
        .filter(|edit| edit.place == TitlePlace::Sidebar)
        .map(|edit| edit.agent);
    if let Some(agent) = in_row
        && !has_row(state, agent)
    {
        start_rename_at(state, drafts, agent, TitlePlace::Header);
    }
}

fn has_row(state: &AppState, agent: AgentId) -> bool {
    state
        .sidebar_agents()
        .iter()
        .any(|entry| entry.id() == agent)
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
    Outgoing::action(Request::AgentPrompt(PromptParams {
        agent,
        prompt,
        delivery: PromptDelivery::Steer,
    }))
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
        | MenuAction::Rename
        | MenuAction::SetEmoji
        | MenuAction::OpenFile
        | MenuAction::CloseAgent
        | MenuAction::PerformanceInfo => Vec::new(),
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
        | MenuAction::Rename
        | MenuAction::SetEmoji
        | MenuAction::OpenFile
        | MenuAction::CloseAgent
        | MenuAction::PerformanceInfo => None,
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
        | MenuAction::Rename
        | MenuAction::SetEmoji
        | MenuAction::OpenFile
        | MenuAction::CloseAgent
        | MenuAction::PerformanceInfo => None,
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
pub(super) fn moved(index: usize, count: usize, direction: Direction) -> usize {
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
/// keystroke the draft has not received yet. A working agent reads the prompt at its next step,
/// or after its turn with `PromptDelivery::Queue`.
pub fn send_prompt(
    state: &mut AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    typed: Option<String>,
    delivery: PromptDelivery,
) -> Vec<Outgoing> {
    let exited = state
        .agents
        .get(&agent)
        .is_none_or(|entry| entry.summary.status == AgentStatus::Exited);
    if exited {
        return Vec::new();
    }
    // A comment still being written goes with the message.
    if drafts
        .quote_editor
        .as_ref()
        .is_some_and(|editor| editor.agent == agent)
    {
        quotes::commit(drafts, state.prefs.chat.comments);
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
        text: quotes::message(&draft.quotes, &draft.text),
        images: draft.images.clone(),
    };
    // Shown at once: the daemon's echo takes a round trip, long to a server. A message steered to
    // a busy agent waits in the app first, and leaves later.
    let hold = state.prefs.chat.steer_hold.duration();
    let Some(sent) = sending::start(
        state,
        agent,
        &prompt,
        delivery,
        &draft,
        hold,
        Instant::now(),
    ) else {
        return Vec::new();
    };
    vec![Outgoing {
        request: Request::AgentPrompt(PromptParams {
            agent,
            prompt,
            delivery,
        }),
        purpose: Purpose::Prompt {
            agent,
            draft,
            sending: sent,
        },
        to: MachineId::THIS,
    }]
}

/// The Stop of the dialog that Esc opens: it keeps the dialog's choice of asking again, then
/// stops the agent's turn.
pub fn stop_from_dialog(state: &mut AppState, agent: AgentId, esc_stop: EscStop) -> Vec<Outgoing> {
    state.ui.close_dialog();
    state.prefs.chat.esc_stop = esc_stop;
    interrupt(state, agent)
}

/// Closes the stop dialog once its agent has no turn left to stop.
pub fn follow_stop_dialog(state: &mut AppState) {
    if let Some(Dialog::StopAgent(agent)) = state.ui.dialog
        && !has_turn(state, agent)
    {
        state.ui.close_dialog();
    }
}

/// Only a working agent, or one waiting for you, has a turn to stop.
pub fn has_turn(state: &AppState, agent: AgentId) -> bool {
    state.agents.get(&agent).is_some_and(|entry| {
        matches!(
            accounts::turn_status(&entry.summary),
            AgentStatus::Working | AgentStatus::NeedsYou
        )
    })
}

/// Stops the current turn, when the agent has one.
pub fn interrupt(state: &AppState, agent: AgentId) -> Vec<Outgoing> {
    if !has_turn(state, agent) {
        return Vec::new();
    }
    vec![Outgoing::action(Request::AgentInterrupt(AgentParams {
        agent,
    }))]
}

fn answer(agent: AgentId, pending: PendingId, answer: Answer) -> Vec<Outgoing> {
    let request = Request::AgentAnswer(AnswerParams {
        agent,
        pending: pending.clone(),
        answer,
    });
    vec![Outgoing {
        purpose: Purpose::Answer { agent, pending },
        ..Outgoing::action(request)
    }]
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

/// Picks an option. In a single-choice question, a pick replaces the previous one, and picking the
/// same option again clears it. A new pick shows the next question. In a multiple-choice question,
/// a pick toggles.
pub fn pick_option(drafts: &mut Drafts, question: &Question, asked: usize, option: usize) {
    let Some(selection) = question.questions.get(asked).map(|asked| asked.selection) else {
        return;
    };
    let draft = question_draft(drafts, question);
    let Some(picks) = draft.picks.get_mut(asked) else {
        return;
    };
    if picks.remove(&option) {
        return;
    }
    if selection == Selection::Single {
        picks.clear();
    }
    picks.insert(option);
    if selection == Selection::Single && asked + 1 < question.questions.len() {
        draft.shown = asked + 1;
    }
}

/// Sets an "Other" text. In a single-choice question, a text replaces the pick.
pub fn set_other_answer(drafts: &mut Drafts, question: &Question, asked: usize, text: String) {
    let single = question
        .questions
        .get(asked)
        .is_some_and(|asked| asked.selection == Selection::Single);
    let draft = question_draft(drafts, question);
    if single
        && !text.trim().is_empty()
        && let Some(picks) = draft.picks.get_mut(asked)
    {
        picks.clear();
    }
    if let Some(other) = draft.other.get_mut(asked) {
        *other = text;
    }
}

/// The question on screen: the draft's, within the card's questions.
pub fn shown_question(question: &Question, draft: Option<&QuestionDraft>) -> usize {
    let shown = draft.map_or(0, |draft| draft.shown);
    shown.min(question.questions.len().saturating_sub(1))
}

/// Shows the question at `asked`, as a click on its tab does.
pub fn show_question(drafts: &mut Drafts, question: &Question, asked: usize) {
    if asked < question.questions.len() {
        question_draft(drafts, question).shown = asked;
    }
}

/// The next question without an answer after the one on screen. After the last question, the
/// search goes on from the first.
pub fn next_unanswered(question: &Question, draft: Option<&QuestionDraft>) -> Option<usize> {
    let count = question.questions.len();
    let shown = shown_question(question, draft);
    (1..count)
        .map(|step| (shown + step) % count)
        .find(|&asked| !draft.is_some_and(|draft| draft.has_answer(asked)))
}

/// One answer per question, or `None` while a question has neither a pick nor an "Other" text.
/// A single-choice question with a pick sends the pick alone.
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
        let replaced = asked.selection == Selection::Single && !selected.is_empty();
        let other = draft
            .other
            .get(index)
            .map(|text| text.trim())
            .filter(|text| !text.is_empty() && !replaced)
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

/// Enter on a question card: sends the answers once every question has one, else shows the next
/// question without an answer. `None` when there is neither.
pub fn enter_on_question(
    state: &AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    item: ItemId,
) -> Option<Vec<Outgoing>> {
    let question = state
        .agents
        .get(&agent)
        .and_then(|entry| waiting_question(entry, item))?;
    let draft = drafts.questions.get(&question.pending);
    if question_answers(question, draft).is_some() {
        return Some(submit_question(state, drafts, agent, item));
    }
    let next = next_unanswered(question, draft)?;
    show_question(drafts, question, next);
    Some(Vec::new())
}

/// Left and Right: shows the question before or after the one on screen.
pub fn step_question(
    state: &AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    item: ItemId,
    step: Step,
) {
    let Some(question) = state
        .agents
        .get(&agent)
        .and_then(|entry| waiting_question(entry, item))
    else {
        return;
    };
    let shown = shown_question(question, drafts.questions.get(&question.pending));
    let next = match step {
        Step::Next => shown.checked_add(1),
        Step::Previous => shown.checked_sub(1),
    };
    if let Some(next) = next {
        show_question(drafts, question, next);
    }
}

/// The 1 to 9 keys: picks an option of the question on screen.
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
    else {
        return;
    };
    let asked = shown_question(question, drafts.questions.get(&question.pending));
    if let Some(option) = number.checked_sub(1) {
        let exists = question
            .questions
            .get(asked)
            .is_some_and(|asked| option < asked.options.len());
        if exists {
            pick_option(drafts, question, asked, option);
        }
    }
}

pub fn cancel_queued(agent: AgentId, prompt: PromptId) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentCancelQueued(
        QueuedPromptParams { agent, prompt },
    ))]
}

/// Sends a queued prompt now, for the agent to read at its next step.
pub fn send_queued(agent: AgentId, prompt: PromptId) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentSendQueued(
        QueuedPromptParams { agent, prompt },
    ))]
}

/// Opens the file view on `path`, at `line` when a link names one, in its tab, in the mode
/// `opening_mode` picks. The text is read again, since it may have changed, and the text of the
/// file shown before is dropped: files can be large.
pub fn open_file(state: &mut AppState, agent: AgentId, path: PathBuf, line: Option<u32>) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let path = file_edits::relative_path(&entry.summary.folder, &path);
    let changed_paths = entry.changed_paths();
    // The changed files cover the whole session, and the loaded messages an edit that just
    // finished.
    let changed = entry.has_changed(&path)
        || !file_edits::edits_for(&entry.transcript, &entry.summary.folder, &path).is_empty();
    let shown_mode = state.ui.file_views.get(&agent).map(|view| view.mode);
    let mode = opening_mode(&path, line, changed, shown_mode);
    entry.files.clear();
    entry.files.insert(path.clone(), Loadable::NotRequested);
    entry.file_freshness = FileFreshness::default();
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
    state.ui.terminal_tab_shown.remove(&agent);
    state.ui.popover = None;
    state.ui.badge = None;
    // A link in the agent map opens the file view, which the map would cover.
    state
        .ui
        .dialog
        .take_if(|dialog| matches!(dialog, Dialog::AgentMap(_)));
}

/// A file's tab, or its row in the list of open files. The file under the terminal's tab shows
/// as it was: opening it again would read it again and reset its mode, its line and its scroll.
pub fn show_file(state: &mut AppState, agent: AgentId, path: PathBuf) {
    if state
        .ui
        .file_view(agent)
        .is_some_and(|view| view.path == path)
    {
        state.ui.terminal_tab_shown.remove(&agent);
        state.ui.popover = None;
    } else {
        open_file(state, agent, path, None);
    }
}

/// The mode a file opens in, given the mode of the file shown before it, if any:
/// - with a `line` to mark, the whole file;
/// - a markdown file, rendered, unless the agent changed it: then rendered only when the file
///   shown before was;
/// - a file the agent changed, its diff;
/// - any other file, the whole file.
fn opening_mode(
    path: &Path,
    line: Option<u32>,
    changed: bool,
    shown_mode: Option<ViewMode>,
) -> ViewMode {
    if line.is_some() {
        ViewMode::WholeFile
    } else if is_markdown(path) && (!changed || shown_mode == Some(ViewMode::Preview)) {
        ViewMode::Preview
    } else if changed {
        ViewMode::Diff
    } else {
        ViewMode::WholeFile
    }
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
    let terminal_shown = state.ui.terminal_tab_shown.contains(&agent);
    match after.or(before) {
        Some(next) => open_file(state, agent, next.clone(), None),
        None => close_file_view(state, agent),
    }
    // A file closed behind the terminal's tab leaves the terminal shown.
    if terminal_shown {
        terminals::show_tab(state, agent);
    }
}

/// Shows the text the daemon read for `requested` at `found`. When the daemon looked a name up and
/// found the file elsewhere, the view moves to it. A file the view no longer shows is not kept.
pub fn show_read_file(
    state: &mut AppState,
    agent: AgentId,
    requested: &Path,
    found: &Path,
    read: FileRead,
    now: Option<Timestamp>,
) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let Some(old) = entry.files.remove(requested) else {
        return;
    };
    file_reread::arrived(&mut entry.file_freshness, old.loaded(), &read, now);
    let folder = &entry.summary.folder;
    let found = file_edits::relative_path(folder, found);
    if let Some(view) = state.ui.file_views.get_mut(&agent)
        && view.path == requested
    {
        // A read again keeps the mode the user may have chosen since the first.
        let first = old.loaded().is_none();
        let changed = matches!(&read, FileRead::Text { edits, .. } if !edits.is_empty());
        if first && view.line.is_none() && changed {
            view.mode = opening_mode(&found, None, changed, Some(view.mode));
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
    entry.files.insert(found, Loadable::Loaded(read));
}

pub fn close_file_view(state: &mut AppState, agent: AgentId) {
    state.ui.file_views.remove(&agent);
    state.ui.badge = None;
    state.ui.popover.take_if(|popover| match popover {
        Popover::FileList => true,
        Popover::TabMenu { agent: shown, .. } => *shown == agent,
        _ => false,
    });
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.files.clear();
        entry.file_freshness = FileFreshness::default();
    }
}

/// The file view's "×": closes its files, and hides the terminal when it is one of its tabs.
pub fn close_file_panel(state: &mut AppState, agent: AgentId) {
    if terminals::in_file_view(state, agent) {
        terminals::hide(state, agent);
    }
    close_file_view(state, agent);
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

/// Keeps the sidebar's width, in pixels, where its splitter left it. The page sends it: a width
/// the style sheet's bounds would not give is ignored.
pub fn set_sidebar_width(state: &mut AppState, width: f64) {
    let rounded = width.round();
    let bounds = f64::from(prefs::MIN_SIDEBAR_WIDTH)..=f64::from(prefs::MAX_SIDEBAR_WIDTH);
    if !bounds.contains(&rounded) {
        tracing::warn!("ignored a sidebar width of {width} pixels");
        return;
    }
    // Within the bounds, the cast keeps the value.
    state.prefs.sidebar.width = Some(rounded as u16);
}

pub fn reset_sidebar_width(state: &mut AppState) {
    state.prefs.sidebar.width = None;
}

/// Moves `block` just above `before`, or below the others with `None`. Every agent's blocks and
/// badges follow the order, which the preferences keep.
pub fn drop_block(state: &mut AppState, block: Block, before: Option<Block>) {
    let mut order = state.prefs.blocks.order();
    order.retain(|other| *other != block);
    let at = before
        .and_then(|before| order.iter().position(|other| *other == before))
        .unwrap_or(order.len());
    order.insert(at, block);
    state.prefs.blocks.set_order(&order);
}

/// Moves `block` past the next block up or down among `shown`, the blocks the agent shows, as
/// Alt+Up and Alt+Down on its head do. A block the agent does not show keeps its place.
pub fn step_block(state: &mut AppState, block: Block, direction: Direction, shown: &[Block]) {
    let Some(at) = shown.iter().position(|other| *other == block) else {
        return;
    };
    let neighbour = match direction {
        Direction::Up => at.checked_sub(1).and_then(|to| shown.get(to)),
        Direction::Down => shown.get(at + 1),
    };
    let Some(&neighbour) = neighbour else {
        return;
    };
    let mut order = state.prefs.blocks.order();
    order.retain(|other| *other != block);
    let Some(next_to) = order.iter().position(|other| *other == neighbour) else {
        return;
    };
    let to = match direction {
        Direction::Up => next_to,
        Direction::Down => next_to + 1,
    };
    order.insert(to, block);
    state.prefs.blocks.set_order(&order);
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
    let Some(entry) = state
        .agents
        .get(&agent)
        .filter(|entry| entry.summary.status != AgentStatus::Exited)
    else {
        return Vec::new();
    };
    if !(opening && block == Block::Mcp) {
        return Vec::new();
    }
    vec![Outgoing {
        request: Request::AgentMcpStatus(AgentParams { agent }),
        purpose: Purpose::McpStatus {
            agent,
            details_seq: entry.details_seq,
        },
        to: MachineId::THIS,
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
        purpose: Purpose::ContextUsage {
            agent,
            details_seq: entry.details_seq,
        },
        to: MachineId::THIS,
    }]
}

pub fn stop_task(agent: AgentId, task: BackgroundTaskId) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentStopTask(StopTaskParams {
        agent,
        task,
    }))]
}

pub fn mcp_toggle(agent: AgentId, server: String, action: McpToggleAction) -> Vec<Outgoing> {
    vec![Outgoing::action(Request::AgentMcpToggle(McpToggleParams {
        agent,
        server,
        action,
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
/// The setting is the selected agent's machine's.
pub fn set_mcp_hidden(state: &mut AppState, server: &str, hidden: bool) -> Vec<Outgoing> {
    let machine = state.selected_machine();
    let Some(known) = state.machines.get_mut(machine) else {
        return Vec::new();
    };
    let settings = &mut known.settings;
    if hidden {
        settings.hidden_mcp_servers.insert(server.to_owned());
    } else {
        settings.hidden_mcp_servers.remove(server);
    }
    vec![Outgoing {
        request: Request::SettingsSet(settings.clone()),
        purpose: Purpose::Action,
        to: machine,
    }]
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

pub fn load_older(state: &mut AppState, agent: AgentId) -> Vec<Outgoing> {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return Vec::new();
    };
    if !entry.transcript.history().can_load_older() {
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

/// Opens or closes the agent switcher. The new agent dialog stays open, with what its fields
/// hold, and so does an "Accounts" dialog that goes back to it.
pub fn toggle_switcher(state: &mut AppState, drafts: &mut Drafts) {
    if state.ui.holds_new_agent_form() {
        return;
    }
    let open = state.ui.dialog == Some(Dialog::Switcher);
    state.ui.popover = None;
    state.ui.dialog = (!open).then_some(Dialog::Switcher);
    drafts.switcher = SwitcherDraft::default();
}

/// Moves `agent` just above `before`, or to the bottom of the list. Grouped by machine, the
/// sidebar shows the new order at once, with the positions the daemon then sends in its events;
/// without a connection nothing moves, and a refused move puts the agents back only where no
/// later move took them. Otherwise the app keeps the order itself.
pub fn move_agent(state: &mut AppState, agent: AgentId, before: Option<AgentId>) -> Vec<Outgoing> {
    if state.prefs.sidebar.group_by != GroupBy::Machine {
        grouping::move_in_app_order(state, agent, before);
        return Vec::new();
    }
    if !state.reaches_agent(agent) {
        let error = RequestError::NotConnected;
        state.ui.report(format!("The agent was not moved: {error}"));
        return Vec::new();
    }
    // Each machine keeps its own order: the list holds the agent's machine alone, and a row of
    // another machine stands for the top or the bottom of it.
    let machine = MachineId::of(agent);
    let list: Vec<Placed> = state
        .agents_in_order()
        .iter()
        .filter(|entry| MachineId::of(entry.id()) == machine)
        .map(|entry| entry.summary.place())
        .collect();
    let before = match before {
        Some(row) if MachineId::of(row) < machine => list.first().map(|first| first.agent),
        Some(row) if MachineId::of(row) > machine => None,
        before => before,
    };
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
        to: MachineId::THIS,
    }]
}

/// Drops a dragged sidebar row just above the row of `before_row`, or at the end of the project
/// group `end`, or below the last row when both are `None`. Hidden agents keep their places: a
/// drop below a last row lands just under it. Grouped by project, a drop in another group moves
/// the agent to that project.
///
/// Split by machine, the row stays among its machine's rows:
/// - a drop on the rows of a machine listed above lands at the top of them;
/// - a drop on the rows of a machine listed below, or at the group's end, lands at the bottom.
pub fn drop_agent(
    state: &mut AppState,
    agent: AgentId,
    before_row: Option<AgentId>,
    end: Option<&ProjectKey>,
) -> Vec<Outgoing> {
    let by_project = state.prefs.sidebar.group_by == GroupBy::Project;
    let target = match (end, before_row) {
        _ if !by_project => None,
        (Some(end), _) => Some(end.clone()),
        (None, Some(row)) => state
            .agents
            .get(&row)
            .map(|entry| projects::project_of(state, entry)),
        (None, None) => None,
    };
    let split = grouping::splits_by_machine(state);
    let machine = MachineId::of(agent);
    // The rows the agent can land among, from top to bottom, the agent left out.
    let among: Vec<AgentId> = state
        .sidebar_agents()
        .iter()
        .filter(|entry| {
            target
                .as_ref()
                .is_none_or(|target| projects::project_of(state, entry) == *target)
        })
        .map(|entry| entry.id())
        .filter(|id| *id != agent && (!split || MachineId::of(*id) == machine))
        .collect();
    let before_row = match (before_row, among.first()) {
        (Some(row), Some(&first)) if split && MachineId::of(row) != machine => {
            (MachineId::of(row) < machine).then_some(first)
        }
        (before_row, _) => before_row,
    };
    let before = match before_row {
        Some(row) => Some(row),
        None => {
            let last = among.last().copied();
            match last {
                Some(last) => below_in_order(state, last, agent),
                None if target.is_some() => None,
                None => return Vec::new(),
            }
        }
    };
    let outgoing = move_agent(state, agent, before);
    if let Some(target) = target {
        let current = state
            .agents
            .get(&agent)
            .map(|entry| projects::project_of(state, entry));
        if current.as_ref() != Some(&target) {
            projects::move_agent(state, agent, &target);
        }
    }
    outgoing
}

/// Folds a machine's rows under its header in the sidebar, or unfolds them.
pub fn toggle_machine(state: &mut AppState, machine: MachineId) {
    let folded = &mut state.ui.folded_machines;
    if !folded.remove(&machine) {
        folded.insert(machine);
    }
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
        .sidebar_order()
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

/// What closing the agent would lose, as the confirmation says it, or `None` when nothing would
/// be lost and the agent closes at once.
pub fn close_warning(entry: &AgentEntry, drafts: &Drafts) -> Option<String> {
    let mut losses = Vec::new();
    match accounts::turn_status(&entry.summary) {
        AgentStatus::Working => losses.push("is working"),
        AgentStatus::NeedsYou => losses.push("waits for your answer"),
        AgentStatus::Idle | AgentStatus::Exited => {}
    }
    if drafts
        .draft(entry.id())
        .is_some_and(|draft| !draft.is_empty())
    {
        losses.push("holds a message you have not sent");
    }
    if losses.is_empty() {
        return None;
    }
    Some(format!("{} {}.", entry.summary.title, losses.join(" and ")))
}

/// Closes the agent, after asking when it works, waits for you, or holds a draft.
pub fn request_close(state: &mut AppState, drafts: &mut Drafts, agent: AgentId) -> Vec<Outgoing> {
    let Some(entry) = state.agents.get(&agent) else {
        return Vec::new();
    };
    if close_warning(entry, drafts).is_some() {
        state.ui.popover = None;
        state.ui.dialog = Some(Dialog::CloseAgent(agent));
        return Vec::new();
    }
    close_agent(state, drafts, agent)
}

/// Closes the agent: it leaves the sidebar at once, with its drafts and file view, and the undo
/// note offers it back. The selection goes to the next row, or to the one above for the last.
pub fn close_agent(state: &mut AppState, drafts: &mut Drafts, agent: AgentId) -> Vec<Outgoing> {
    let ids = |entries: Vec<&AgentEntry>| -> Vec<AgentId> {
        entries.iter().map(|entry| entry.id()).collect()
    };
    let order = ids(state.sidebar_order());
    let before = order.iter().skip_while(|id| **id != agent).nth(1).copied();
    let rows = ids(state.sidebar_agents());
    let neighbour = rows.iter().position(|id| *id == agent).and_then(|index| {
        let above = index.checked_sub(1).and_then(|above| rows.get(above));
        rows.get(index + 1).or(above).copied()
    });
    let project = projects::moved_to(state, agent);
    let Some(entry) = forget_agent(state, drafts, agent) else {
        return Vec::new();
    };
    state.ui.closing.insert(agent);
    state.ui.note_closed(&entry.summary, before, project);
    if state.ui.selected == Some(agent) {
        match neighbour {
            Some(next) => select_agent(state, next),
            None => {
                state.ui.leave_agent();
                state.ui.selected = None;
                state.fix_selection();
            }
        }
    }
    vec![Outgoing {
        request: Request::AgentClose(AgentParams { agent }),
        purpose: Purpose::Close {
            entry: Box::new(entry),
        },
        to: MachineId::THIS,
    }]
}

/// Brings the agent of the undo note back from its session, to the place and the project it left.
pub fn undo_close(state: &mut AppState) -> Vec<Outgoing> {
    let Some(closed) = state.ui.closed.take() else {
        return Vec::new();
    };
    let Some(session) = closed.session else {
        return Vec::new();
    };
    let machine = MachineId::of(closed.agent);
    // A fork keeps its source's session until its first turn: the daemon answers with the agent
    // that has the session, which keeps its own place and project.
    let shared = state.agents.values().any(|entry| {
        MachineId::of(entry.id()) == machine
            && entry.summary.session.as_ref() == Some(&session)
            && entry.summary.account == closed.account
    });
    if !shared {
        state.ui.reopening = Some(Reopening {
            session: session.clone(),
            machine,
            account: closed.account,
            before: closed.before,
            project: closed.project,
        });
    }
    vec![Outgoing {
        request: Request::SessionsResume(SessionResumeParams {
            session,
            account: closed.account,
        }),
        purpose: Purpose::Reopen,
        to: machine,
    }]
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

/// The pointer moved onto the agent at `index` of the filtered list: it takes the highlight, as
/// the keys do. A row of a list the query has since narrowed changes nothing.
pub fn switcher_point(state: &AppState, drafts: &mut Drafts, index: usize) {
    if index < filtered_agents(state, &drafts.switcher.query).len() {
        drafts.switcher.highlighted = index;
    }
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

/// The model a new agent starts with when there is no agent to copy or its model is not one the
/// menu offers.
const NEW_AGENT_MODEL: &str = format::OPUS.alias;

/// The permission mode a new agent starts with when there is no agent to copy or its mode is not
/// one the dialog carries over.
const NEW_AGENT_MODE: PermissionMode = PermissionMode::Auto;

/// Opens the new agent dialog as a copy of the selected agent, else of the last agent started
/// (`AppState::copied_agent`): on its machine, its project when it was picked by hand, its
/// model, effort and permission mode. The selected agent's folder and account follow too
/// (`opening_folder`, `accounts::new_agent_account`). A machine that does not answer gives way
/// to this computer and its last used folder. Without an agent, the model and mode are Opus and
/// Auto. A mode the mode menu does not offer, or Plan, which is usually for a while, is not
/// carried over. The past sessions are listed again, since sessions end and start while the app
/// runs. An open dialog stays as it is, with what its fields hold, and an "Accounts" dialog that
/// goes back to it does so.
pub fn open_new_agent(state: &mut AppState, drafts: &mut Drafts) {
    if matches!(state.ui.dialog, Some(Dialog::NewAgent(_))) {
        return;
    }
    if state.ui.holds_new_agent_form() {
        state.ui.close_dialog();
        return;
    }
    let selected = state.selected().map(AgentEntry::id);
    let copied = state.copied_agent();
    let machine = copied
        .map(|summary| MachineId::of(summary.id))
        .filter(|machine| {
            state
                .machines
                .get(*machine)
                .is_some_and(MachineState::is_connected)
        })
        .unwrap_or(MachineId::THIS);
    let folder = opening_folder(state, selected, machine);
    // A project picked by hand, which the folder alone would not give again.
    let project = copied
        .filter(|summary| MachineId::of(summary.id) == machine)
        .and_then(|summary| projects::moved_to(state, summary.id));
    let model = copied
        .and_then(|summary| summary.model.as_deref())
        .and_then(format::model_alias)
        .unwrap_or(NEW_AGENT_MODEL)
        .to_owned();
    let effort = copied
        .filter(|summary| summary.effort_source == EffortSource::Chosen)
        .and_then(|summary| summary.effort);
    let mode = copied
        .map(|summary| summary.mode)
        .filter(|mode| MODE_CHOICES.contains(mode) && *mode != PermissionMode::Plan)
        .unwrap_or(NEW_AGENT_MODE);
    if let Some(known) = state.machines.get_mut(machine) {
        if known.sessions != Loadable::Loading {
            known.sessions = Loadable::NotRequested;
        }
        known.sign_ins.refresh();
    }
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::NewAgent(NewAgentForm {
        machine,
        model: Some(model),
        effort,
        mode: Some(mode),
        selected,
        project,
        ..NewAgentForm::default()
    }));
    drafts.new_agent = NewAgentDraft {
        folder,
        ..NewAgentDraft::default()
    };
}

/// A field of the new agent dialog's "Start in a folder" tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartField {
    Folder,
    Message,
}

/// The field that takes the focus when the "Start in a folder" tab shows: the folder field while
/// it is empty, else the first message.
pub fn start_focus(draft: &NewAgentDraft) -> StartField {
    if draft.folder.trim().is_empty() {
        StartField::Folder
    } else {
        StartField::Message
    }
}

pub fn with_new_agent_form(state: &mut AppState, change: impl FnOnce(&mut NewAgentForm)) {
    if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog {
        change(form);
    }
}

pub fn start_agent(state: &mut AppState, drafts: &Drafts) -> Vec<Outgoing> {
    let Some(Dialog::NewAgent(form)) = &state.ui.dialog else {
        return Vec::new();
    };
    let folder = drafts.new_agent.folder.trim();
    if folder.is_empty() || form.starting || form.tab != NewAgentTab::Start {
        return Vec::new();
    }
    let account = accounts::new_agent_account(state, form);
    let machine = form.machine;
    let project = form.project.clone();
    let prompt = drafts.new_agent.prompt.trim();
    let name = drafts.new_agent.name.trim();
    let params = StartParams {
        folder: PathBuf::from(folder),
        title: (!name.is_empty()).then(|| name.to_owned()),
        model: form.model.clone(),
        effort: form.effort,
        mode: form.mode,
        prompt: (!prompt.is_empty()).then(|| Prompt {
            text: prompt.to_owned(),
            images: Vec::new(),
        }),
        account,
    };
    with_new_agent_form(state, |form| form.starting = true);
    vec![Outgoing {
        request: Request::AgentStart(params),
        purpose: Purpose::Start { project },
        to: machine,
    }]
}

/// The folder `machine` last used, as the folder field shows it, else nothing.
fn last_folder(state: &AppState, machine: MachineId) -> String {
    state
        .recent_folders(machine)
        .first()
        .map(|recent| path_style::shown(&recent.folder))
        .unwrap_or_default()
}

/// The folder the new agent dialog shows on `machine` until you type: the folder of the agent
/// `selected` when the dialog opened, when it runs there, else the machine's last used folder.
fn opening_folder(state: &AppState, selected: Option<AgentId>, machine: MachineId) -> String {
    selected
        .filter(|agent| MachineId::of(*agent) == machine)
        .and_then(|agent| state.agents.get(&agent))
        .map_or_else(
            || last_folder(state, machine),
            |entry| path_style::shown(&entry.summary.folder),
        )
}

/// Starts the new agent on `machine`, whose folders and sessions the dialog then lists. A
/// machine that does not answer cannot be picked.
///
/// The folder field takes `machine`'s opening folder while it holds the previous machine's, or
/// nothing: a folder you typed stays. Returns the field's new text.
pub fn pick_new_agent_machine(
    state: &mut AppState,
    drafts: &mut Drafts,
    machine: MachineId,
) -> Option<String> {
    if !state
        .machines
        .get(machine)
        .is_some_and(MachineState::is_connected)
    {
        return None;
    }
    let Some(Dialog::NewAgent(form)) = &state.ui.dialog else {
        return None;
    };
    let previous = form.machine;
    if previous == machine {
        return None;
    }
    let selected = form.selected;
    // A picked project puts its latest folder on each machine, as its pick did.
    let project = form.project.clone();
    let project_folder = |state: &AppState, machine: MachineId| {
        let project = project.as_ref()?;
        let folder = projects::folder_for_pick(state, machine, "", project)?;
        Some(path_style::shown(&folder))
    };
    let folder = &drafts.new_agent.folder;
    let follows = folder.trim().is_empty()
        || *folder == opening_folder(state, selected, previous)
        || project_folder(state, previous).is_some_and(|picked| picked == *folder);
    let new_folder = follows.then(|| {
        project_folder(state, machine).unwrap_or_else(|| opening_folder(state, selected, machine))
    });
    if let Some(text) = &new_folder {
        drafts.new_agent.folder.clone_from(text);
        drafts.new_agent.folder_highlighted = None;
    }
    with_new_agent_form(state, |form| {
        form.machine = machine;
        form.folders = None;
        form.account = None;
    });
    // Its sessions and sign-ins are asked again, as when the dialog opens.
    if let Some(known) = state.machines.get_mut(machine) {
        if known.sessions != Loadable::Loading {
            known.sessions = Loadable::NotRequested;
        }
        known.sign_ins.refresh();
    }
    new_folder
}

/// Picks the new agent's project. The folder field takes the project's latest recent folder,
/// unless it already names one of its folders, and the list under the field then closes, as on
/// a click on a recent folder. Returns the field's new text.
pub fn pick_new_agent_project(
    state: &mut AppState,
    drafts: &mut Drafts,
    project: ProjectKey,
) -> Option<String> {
    let Some(Dialog::NewAgent(form)) = &state.ui.dialog else {
        return None;
    };
    let new_folder =
        projects::folder_for_pick(state, form.machine, &drafts.new_agent.folder, &project)
            .map(|folder| path_style::shown(&folder));
    if let Some(text) = &new_folder {
        folder_field::pick(state, drafts, text.clone());
    }
    with_new_agent_form(state, |form| {
        form.project = Some(project);
        form.menu = None;
    });
    new_folder
}

/// The past sessions the "Resume a session" tab lists: those no agent runs, of `account` when
/// it is one the daemon has, whose title or folder holds the query, newest first. A session's
/// title is its first prompt unless it was named.
pub fn resumable_sessions<'state>(
    state: &'state AppState,
    query: &str,
    account: Option<AccountId>,
) -> Vec<&'state PastSession> {
    let machine = match &state.ui.dialog {
        Some(Dialog::NewAgent(form)) => form.machine,
        _ => MachineId::THIS,
    };
    let account = account.filter(|&id| accounts::account(state, machine, id).is_some());
    let query = query.to_lowercase();
    let running: BTreeSet<&ClaudeSessionId> = state
        .agents
        .values()
        .filter_map(|entry| entry.summary.session.as_ref())
        .collect();
    let mut sessions: Vec<&PastSession> = state
        .machines
        .get(machine)
        .into_iter()
        .flat_map(|known| known.sessions.loaded())
        .flatten()
        .filter(|session| !running.contains(&session.id))
        .filter(|session| account.is_none_or(|id| session.account == id))
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

/// The sessions the "Resume a session" tab lists: those of the dialog's account that its search
/// finds.
pub fn listed_sessions<'state>(
    state: &'state AppState,
    drafts: &Drafts,
) -> Vec<&'state PastSession> {
    let account = accounts::resume_account(state);
    resumable_sessions(state, &drafts.new_agent.session_query, account)
}

pub fn session_move(state: &AppState, drafts: &mut Drafts, direction: Direction) {
    let count = listed_sessions(state, drafts).len();
    let new_agent = &mut drafts.new_agent;
    new_agent.session_highlighted = moved(new_agent.session_highlighted, count, direction);
}

/// The pointer moved onto the session at `index` of the listed ones: it takes the highlight, so
/// that one row only shows its Resume button. A row of a list the query has since narrowed
/// changes nothing.
pub fn session_point(state: &AppState, drafts: &mut Drafts, index: usize) {
    if index < listed_sessions(state, drafts).len() {
        drafts.new_agent.session_highlighted = index;
    }
}

/// Resumes the session at `index` of the listed ones, or the highlighted one.
pub fn session_pick(state: &mut AppState, drafts: &Drafts, index: Option<usize>) -> Vec<Outgoing> {
    if !matches!(state.ui.dialog, Some(Dialog::NewAgent(_))) {
        return Vec::new();
    }
    let index = index.unwrap_or(drafts.new_agent.session_highlighted);
    let Some((session, account)) = listed_sessions(state, drafts)
        .get(index)
        .map(|session| (session.id.clone(), session.account))
    else {
        return Vec::new();
    };
    resume_session(state, session, account)
}

/// Resumes a session of the account's folder.
pub fn resume_session(
    state: &mut AppState,
    session: ClaudeSessionId,
    account: AccountId,
) -> Vec<Outgoing> {
    let mut machine = MachineId::THIS;
    let mut project = None;
    if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog {
        if form.starting {
            return Vec::new();
        }
        form.starting = true;
        machine = form.machine;
        project = form.project.clone();
    }
    vec![Outgoing {
        request: Request::SessionsResume(SessionResumeParams {
            session: session.clone(),
            account,
        }),
        purpose: Purpose::ResumeSession {
            project,
            session,
            account,
        },
        to: machine,
    }]
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use cockpit_client::RequestError;
    use cockpit_protocol::{
        Account, AgentDetails, AskedQuestion, ChangedFile, CommandKind, CredentialSource,
        EditTexts, ErrorBody, ErrorCode, FileEdit, HistoryPage, ImageMediaType, Method, NoticeKind,
        OlderItems, Position, QuestionOption, Repository, ResponseBody, Timestamp, ToolCall,
        ToolCallStatus, TranscriptItem, UserMessage,
    };

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::prefs::{CommentPlacement, ProjectSplit};
    use crate::state::app_state::ConnectionState;
    use crate::state::app_state::tests::{ids, placed, state_with, summary};
    use crate::state::grouping::GroupKey;
    use crate::state::requests::{apply_response, needed_requests};
    use crate::state::transcript::KEPT_ITEMS_AWAY;
    use crate::state::ui_state::{ClosedAgent, NoteKind};

    const AGENT: u32 = 1;

    #[test]
    fn the_blocks_keep_the_messages_last_until_one_moves() {
        use Block::{Mcp, Messages, Running, Tasks};
        let mut state = state_with(Vec::new());
        let order = |state: &AppState| state.prefs.blocks.order();
        assert_eq!(order(&state), [Tasks, Running, Mcp, Messages]);

        drop_block(&mut state, Messages, Some(Tasks));
        assert_eq!(order(&state), [Messages, Tasks, Running, Mcp]);
        drop_block(&mut state, Tasks, None);
        assert_eq!(order(&state), [Messages, Running, Mcp, Tasks]);

        let all = [Messages, Running, Mcp, Tasks];
        step_block(&mut state, Tasks, Direction::Down, &all);
        step_block(&mut state, Messages, Direction::Up, &all);
        assert_eq!(order(&state), [Messages, Running, Mcp, Tasks]);
        step_block(&mut state, Mcp, Direction::Up, &all);
        assert_eq!(order(&state), [Messages, Mcp, Running, Tasks]);
    }

    #[test]
    fn a_step_passes_the_next_block_shown() {
        use Block::{Mcp, Messages, Running, Tasks};
        let mut state = state_with(Vec::new());
        // Nothing runs in the background: Running does not show.
        let shown = |state: &AppState| -> Vec<Block> {
            let mut order = state.prefs.blocks.order();
            order.retain(|block| *block != Running);
            order
        };
        let step = |state: &mut AppState, block: Block, direction: Direction| {
            let blocks = shown(state);
            step_block(state, block, direction, &blocks);
        };

        step(&mut state, Tasks, Direction::Down);
        assert_eq!(state.prefs.blocks.order(), [Running, Mcp, Tasks, Messages]);
        step(&mut state, Tasks, Direction::Up);
        assert_eq!(state.prefs.blocks.order(), [Running, Tasks, Mcp, Messages]);
        step(&mut state, Running, Direction::Up);
        assert_eq!(state.prefs.blocks.order(), [Running, Tasks, Mcp, Messages]);
    }

    /// Agents 3, 2 and 1 from the top, 1024 apart, with agent 2 selected.
    fn ordered() -> AppState {
        let mut state = state_with(vec![
            placed(1, AgentStatus::Idle, 1024),
            placed(2, AgentStatus::Working, 2048),
            placed(3, AgentStatus::NeedsYou, 3072),
        ]);
        state.machines.this.connection = ConnectionState::Connected;
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
        state.machines.this.connection = ConnectionState::Connected;
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
            entry.details = Loadable::Loaded(AgentDetails::default());
            entry.commands = commands;
        }
    }

    fn question(selection: Selection) -> Question {
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
                selection,
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
        let caret = Utf16Offset::end_of(text);
        match composer_input(state, drafts, agent(), text.to_owned(), caret) {
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
    fn the_commands_load_while_their_request_is_on_its_way() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        let loading = |state: &AppState| state.agents[&agent()].commands_loading();
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
            MachineId::THIS,
        );

        assert_eq!(
            (with_no_commands, while_asked, loading(&state)),
            (false, true, false)
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
                    detail: None,
                    kind: NoticeKind::Plain,
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
            ItemBody::UserMessage(UserMessage {
                text: "Also the docs".to_owned(),
                images: Vec::new(),
                sent_at: None,
            }),
        );
        append(
            &mut state,
            6,
            ItemBody::Question(question(Selection::Single)),
        );

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
        send_prompt(
            &mut state,
            &mut drafts,
            agent(),
            None,
            PromptDelivery::Steer,
        );
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
        send_prompt(
            &mut state,
            &mut drafts,
            agent(),
            None,
            PromptDelivery::Steer,
        );
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

        send_prompt(
            &mut state,
            &mut drafts,
            agent(),
            None,
            PromptDelivery::Steer,
        );
        let after_empty = state.ui.chat_jumps;
        type_text(&mut state, &mut drafts, "Thanks");
        send_prompt(
            &mut state,
            &mut drafts,
            agent(),
            None,
            PromptDelivery::Steer,
        );

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

        let plain = composer_input(
            &state,
            &mut drafts,
            agent(),
            "Run the te".to_owned(),
            Utf16Offset::new(10),
        );
        let opening = composer_input(
            &state,
            &mut drafts,
            agent(),
            "/re".to_owned(),
            Utf16Offset::new(3),
        );
        change_palette(&mut state, agent(), PaletteChange::Open);
        let filtering = composer_input(
            &state,
            &mut drafts,
            agent(),
            "/rev".to_owned(),
            Utf16Offset::new(4),
        );
        let closing = composer_input(
            &state,
            &mut drafts,
            agent(),
            "/rev the".to_owned(),
            Utf16Offset::new(8),
        );

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

        let names = |query: &str, how: CommandMatch| -> Vec<&str> {
            filtered_commands(&commands, query, how)
                .iter()
                .map(|command| command.name.as_str())
                .collect()
        };

        assert_eq!(
            names("/con", CommandMatch::Anywhere),
            ["context", "compact"]
        );
        assert_eq!(names("/con", CommandMatch::NameStart), ["context"]);
        assert_eq!(names("/branch", CommandMatch::Anywhere), ["review"]);
        assert!(names("/branch", CommandMatch::NameStart).is_empty());
    }

    #[test]
    fn a_word_of_the_name_matches_before_the_description() {
        let commands = vec![
            command("deep-research", "Research a topic"),
            command("find-notes", "Search the notes"),
            command("plugin:review-architecture", "Evaluate a crate"),
            command("workdocs-archive", "Copy the docs into the store"),
            command("archive", "Archive the session"),
            command("plugin:search", "Find a plugin"),
        ];
        let names = |query: &str, how: CommandMatch| -> Vec<&str> {
            filtered_commands(&commands, query, how)
                .iter()
                .map(|command| command.name.as_str())
                .collect()
        };

        assert_eq!(
            names("/arch", CommandMatch::Anywhere),
            [
                "archive",
                "plugin:review-architecture",
                "workdocs-archive",
                "deep-research",
                "plugin:search"
            ]
        );
        assert_eq!(names("/arch", CommandMatch::NameStart), ["archive"]);
        assert_eq!(names("/ARCH", CommandMatch::Anywhere).len(), 5);
        assert_eq!(
            names("/docs-arch", CommandMatch::Anywhere),
            ["workdocs-archive"]
        );
        assert_eq!(
            names(" the docs ", CommandMatch::Anywhere),
            ["workdocs-archive"]
        );
        assert_eq!(names("/", CommandMatch::Anywhere).len(), commands.len());
        assert_eq!(
            names("/review-arch", CommandMatch::Anywhere),
            ["plugin:review-architecture"]
        );
        assert_eq!(
            names("/sea", CommandMatch::Anywhere),
            ["plugin:search", "deep-research", "find-notes"]
        );
    }

    #[test]
    fn a_slash_word_starts_the_text_or_follows_a_space_or_a_line_break() {
        let word = |text: &'static str, caret: usize| {
            slash_word_at(text, caret).and_then(|word| text.get(word.start..word.end))
        };

        assert_eq!(word("/rev", 4), Some("/rev"));
        assert_eq!(word("please run /rev", 15), Some("/rev"));
        assert_eq!(word("first line\n/rev", 15), Some("/rev"));
        // A "/" inside a word, and a path, are no command.
        assert_eq!(word("open src/lib.rs", 15), None);
        assert_eq!(word("look in /home", 13), Some("/home"));
        assert_eq!(word("look in /home/dev", 17), None);
        // The word the caret is in counts, up to the next space.
        assert_eq!(word("/rev the branch", 2), Some("/rev"));
        assert_eq!(word("/rev the branch", 8), None);
        assert_eq!(word("/rev ", 5), None);
        // A caret just before the "/" is not in the word.
        assert_eq!(word("fix /mcp", 4), None);
        assert_eq!(word("/rev", 0), None);
    }

    #[test]
    fn a_slash_word_mid_message_opens_the_palette_filtered_by_it() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(
            &mut state,
            vec![command("review", ""), command("compact", "")],
        );
        let palette = Some(Popover::Palette(PaletteOrigin::Typed));

        type_text(&mut state, &mut drafts, "please run /rev");
        let opened = (state.ui.popover, drafts.palette.query.clone());
        type_text(&mut state, &mut drafts, "please run /rev ");
        let after_space = state.ui.popover;
        type_text(
            &mut state,
            &mut drafts,
            "please run /rev now and look in src/",
        );
        let inside_a_word = state.ui.popover;
        type_text(
            &mut state,
            &mut drafts,
            "please run /rev now and look in /home/",
        );
        let a_path = state.ui.popover;

        assert_eq!(opened, (palette, "rev".to_owned()));
        assert_eq!((after_space, inside_a_word, a_path), (None, None, None));
    }

    #[test]
    fn a_pick_replaces_only_the_slash_word_and_puts_the_caret_after_the_command() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(&mut state, vec![command("review", "")]);
        let mut pick = |text: &str, caret: usize| {
            let caret = Utf16Offset::new(caret);
            let change = composer_input(&state, &mut drafts, agent(), text.to_owned(), caret);
            if let Some(change) = change {
                change_palette(&mut state, agent(), change);
            }
            let _ = palette_pick(&mut state, &mut drafts, agent(), None);
            let text = drafts.draft(agent()).map(|draft| draft.text.clone());
            (text.unwrap_or_default(), drafts.written_caret(agent()))
        };

        // In the middle of the message, before a space: the space stays, the caret goes after it.
        let middle = pick("please run /rev now", 15);
        // At the end: a space follows the command.
        let end = pick("please run /rev", 15);
        // Before a line break: a space goes in before it.
        let line_break = pick("/re\nthen the docs", 3);

        let at = |units| Some(Utf16Offset::new(units));
        assert_eq!(middle, ("please run /review now".to_owned(), at(19)));
        assert_eq!(end, ("please run /review ".to_owned(), at(19)));
        assert_eq!(line_break, ("/review \nthen the docs".to_owned(), at(8)));
    }

    #[test]
    fn the_caret_counts_utf16_units_as_the_page_does() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(&mut state, vec![command("review", "")]);
        // "é" is one UTF-16 unit and two bytes; "🙂" is two units and four bytes.
        let text = "café 🙂 /re";
        let caret = Utf16Offset::end_of(text);

        let change = composer_input(&state, &mut drafts, agent(), text.to_owned(), caret);
        if let Some(change) = change {
            change_palette(&mut state, agent(), change);
        }
        let query = drafts.palette.query.clone();
        let _ = palette_pick(&mut state, &mut drafts, agent(), None);

        let written = drafts.draft(agent()).map(|draft| draft.text.clone());
        assert_eq!(query, "re");
        assert_eq!(written.as_deref(), Some("café 🙂 /review "));
        assert_eq!(
            drafts.written_caret(agent()),
            Some(Utf16Offset::end_of("café 🙂 /review "))
        );
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
    fn the_pointer_moves_the_palette_highlight_within_the_listed_entries() {
        let mut state = state(AgentStatus::Idle);
        with_commands(
            &mut state,
            vec![
                command("compact", ""),
                command("context", ""),
                command("review", ""),
            ],
        );
        let mut drafts = Drafts::default();
        type_text(&mut state, &mut drafts, "/");

        palette_point(&state, &mut drafts, agent(), 2);
        assert_eq!(drafts.palette.highlighted, 2);
        palette_move(&state, &mut drafts, agent(), Direction::Up);
        assert_eq!(drafts.palette.highlighted, 1);

        // An entry the narrowed list no longer has: the pointer was on the old list.
        type_text(&mut state, &mut drafts, "/c");
        palette_point(&state, &mut drafts, agent(), 2);
        assert_eq!(drafts.palette.highlighted, 0);
        palette_point(&state, &mut drafts, agent(), 1);
        let picked = palette_pick(&mut state, &mut drafts, agent(), None);

        assert_eq!(
            drafts.draft(agent()).map(|draft| draft.text.as_str()),
            Some("/context ")
        );
        assert_eq!(picked.window, Some(WindowAction::FocusComposer));
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
        let entries = palette_entries(commands, query, CommandMatch::Anywhere, origin);
        let names = entries.into_iter().map(|entry| match entry {
            PaletteEntry::Action(action) => action.label().to_owned(),
            PaletteEntry::Command(command) => format!("/{}", command.name),
        });
        names.collect()
    }

    fn sent(requests: &[Outgoing]) -> Vec<&Request> {
        requests.iter().map(|outgoing| &outgoing.request).collect()
    }

    fn event(event: cockpit_protocol::Event) -> cockpit_protocol::EventRecord {
        cockpit_protocol::EventRecord {
            seq: cockpit_protocol::Seq::new(1),
            event,
        }
    }

    /// `ordered()` with every agent idle and its own session.
    fn idle_ordered() -> AppState {
        let mut state = ordered();
        for entry in state.agents.values_mut() {
            entry.summary.status = AgentStatus::Idle;
            entry.summary.session = Some(ClaudeSessionId::new(format!(
                "session-{}",
                entry.id().get()
            )));
        }
        state
    }

    fn sent_close(requests: &[Outgoing]) -> Option<AgentId> {
        match &requests.first()?.request {
            Request::AgentClose(params) => Some(params.agent),
            _ => None,
        }
    }

    #[test]
    fn an_idle_agent_closes_at_once_and_the_next_row_is_selected() {
        let mut state = idle_ordered();
        let mut drafts = Drafts::default();
        open_file(
            &mut state,
            AgentId::new(2),
            PathBuf::from("src/lib.rs"),
            None,
        );

        let requests = request_close(&mut state, &mut drafts, AgentId::new(2));

        assert_eq!(sent_close(&requests), Some(AgentId::new(2)));
        assert_eq!(sidebar(&state), [3, 1]);
        assert_eq!(state.ui.selected, Some(AgentId::new(1)));
        assert_eq!(state.ui.file_view(AgentId::new(2)), None);
        assert_eq!(state.ui.dialog, None);
        let closed = state.ui.closed.clone().unwrap();
        assert_eq!(closed.title, "Agent 2");
        assert_eq!(closed.before, Some(AgentId::new(1)));
    }

    #[test]
    fn closing_the_last_row_selects_the_one_above() {
        let mut state = idle_ordered();
        state.ui.selected = Some(AgentId::new(1));

        request_close(&mut state, &mut Drafts::default(), AgentId::new(1));

        assert_eq!(state.ui.selected, Some(AgentId::new(2)));
    }

    #[test]
    fn a_working_waiting_or_drafted_agent_asks_first() {
        let mut state = ordered();
        let mut drafts = Drafts::default();
        drafts.draft_mut(AgentId::new(3)).text = "One more thing".to_owned();
        let warning = |state: &AppState, drafts: &Drafts, agent: u32| {
            close_warning(&state.agents[&AgentId::new(agent)], drafts)
        };

        let working = warning(&state, &drafts, 2);
        let waiting_with_draft = warning(&state, &drafts, 3);
        let idle = warning(&state, &drafts, 1);
        let asked = request_close(&mut state, &mut drafts, AgentId::new(2));
        let shown = sidebar(&state);
        let confirmed = close_agent(&mut state, &mut drafts, AgentId::new(2));

        assert_eq!(working.as_deref(), Some("Agent 2 is working."));
        assert_eq!(
            waiting_with_draft.as_deref(),
            Some("Agent 3 waits for your answer and holds a message you have not sent.")
        );
        assert_eq!(idle, None);
        assert!(asked.is_empty());
        assert_eq!(shown, [3, 2, 1], "nothing closed before the answer");
        assert_eq!(sent_close(&confirmed), Some(AgentId::new(2)));
        assert_eq!(state.ui.dialog, None);
        assert_eq!(sidebar(&state), [3, 1]);
    }

    #[test]
    fn a_closing_agent_stays_off_until_the_daemon_removes_it() {
        let mut state = idle_ordered();
        let mut drafts = Drafts::default();
        let late = state.agents[&AgentId::new(2)].summary.clone();
        request_close(&mut state, &mut drafts, AgentId::new(2));

        let alert = crate::state::apply::apply(
            &mut state,
            &mut drafts,
            event(cockpit_protocol::Event::AgentUpdated(late)),
            MachineId::THIS,
        );
        let before_removal = sidebar(&state);
        crate::state::apply::apply(
            &mut state,
            &mut drafts,
            event(cockpit_protocol::Event::AgentRemoved {
                agent: AgentId::new(2),
            }),
            MachineId::THIS,
        );

        assert_eq!(alert, None);
        assert_eq!(before_removal, [3, 1]);
        assert!(state.ui.closing.is_empty());
    }

    #[test]
    fn a_refused_close_brings_the_agent_back_unless_the_daemon_has_none() {
        let mut drafts = Drafts::default();
        let refused = |code| {
            Err(RequestError::Failed(ErrorBody::new(
                code,
                "refused".to_owned(),
            )))
        };
        let mut failed = idle_ordered();
        let mut gone = idle_ordered();

        for (state, code) in [
            (&mut failed, ErrorCode::Internal),
            (&mut gone, ErrorCode::NotFound),
        ] {
            let requests = request_close(state, &mut drafts, AgentId::new(2));
            for outgoing in requests {
                let method = outgoing.request.method();
                apply_response(
                    state,
                    &mut drafts,
                    method,
                    outgoing.purpose,
                    refused(code),
                    MachineId::THIS,
                );
            }
        }

        assert_eq!(sidebar(&failed), [3, 2, 1]);
        assert_eq!(failed.ui.closed, None);
        assert_eq!(failed.ui.errors.len(), 1);
        assert_eq!(sidebar(&gone), [3, 1]);
        assert!(gone.ui.errors.is_empty());
        assert!(failed.ui.closing.is_empty() && gone.ui.closing.is_empty());
    }

    #[test]
    fn undo_brings_the_session_back_to_its_place() {
        let mut state = idle_ordered();
        let mut drafts = Drafts::default();
        let mut back = state.agents[&AgentId::new(2)].summary.clone();
        request_close(&mut state, &mut drafts, AgentId::new(2));
        crate::state::apply::apply(
            &mut state,
            &mut drafts,
            event(cockpit_protocol::Event::AgentRemoved {
                agent: AgentId::new(2),
            }),
            MachineId::THIS,
        );

        let undone = undo_close(&mut state);
        // The daemon starts a new agent from the session, on top of the list.
        back.id = AgentId::new(4);
        back.position = cockpit_protocol::Position::new(4096);
        crate::state::apply::apply(
            &mut state,
            &mut drafts,
            event(cockpit_protocol::Event::AgentAdded(back)),
            MachineId::THIS,
        );
        apply_response(
            &mut state,
            &mut drafts,
            Method::SessionsResume,
            Purpose::Reopen,
            Ok(ResponseBody::AgentStarted {
                agent: AgentId::new(4),
            }),
            MachineId::THIS,
        );
        let moved = needed_requests(&mut state);

        assert!(matches!(
            undone.first().map(|outgoing| &outgoing.request),
            Some(Request::SessionsResume(params)) if params.session.as_str() == "session-2"
        ));
        assert_eq!(
            sent_move(&moved),
            Some(MoveParams {
                agent: AgentId::new(4),
                before: Some(AgentId::new(1)),
            })
        );
        assert_eq!(sidebar(&state), [3, 4, 1]);
        assert_eq!(state.ui.selected, Some(AgentId::new(4)));
        assert_eq!(state.ui.reopening, None);
        assert!(
            undo_close(&mut state).is_empty(),
            "the note went with the undo"
        );
    }

    #[test]
    fn undo_brings_the_session_back_to_its_row_in_the_apps_order() {
        let undone = |group_by| {
            let mut state = idle_ordered();
            let mut drafts = Drafts::default();
            state.prefs.sidebar.group_by = group_by;
            move_agent(&mut state, AgentId::new(2), None);
            let before_close = sidebar(&state);
            let mut back = state.agents[&AgentId::new(1)].summary.clone();
            request_close(&mut state, &mut drafts, AgentId::new(1));
            crate::state::apply::apply(
                &mut state,
                &mut drafts,
                event(cockpit_protocol::Event::AgentRemoved {
                    agent: AgentId::new(1),
                }),
                MachineId::THIS,
            );

            undo_close(&mut state);
            back.id = AgentId::new(4);
            crate::state::apply::apply(
                &mut state,
                &mut drafts,
                event(cockpit_protocol::Event::AgentAdded(back)),
                MachineId::THIS,
            );
            apply_response(
                &mut state,
                &mut drafts,
                Method::SessionsResume,
                Purpose::Reopen,
                Ok(ResponseBody::AgentStarted {
                    agent: AgentId::new(4),
                }),
                MachineId::THIS,
            );
            needed_requests(&mut state);
            (before_close, sidebar(&state))
        };

        for group_by in [GroupBy::None, GroupBy::Project] {
            let (before_close, after_undo) = undone(group_by);

            assert_eq!(before_close, [3, 1, 2], "{group_by:?}");
            assert_eq!(after_undo, [3, 4, 2], "{group_by:?}");
        }
    }

    #[test]
    fn undo_leaves_another_accounts_agent_with_the_session_alone() {
        let mut state = idle_ordered();
        let mut drafts = Drafts::default();
        let closed = AgentId::new(3);
        let work_account = AccountId::new(2);
        if let Some(entry) = state.agents.get_mut(&closed) {
            entry.summary.session = Some(ClaudeSessionId::new("session-1".to_owned()));
            entry.summary.account = work_account;
        }
        let work = ProjectKey::Kept(projects::create(&mut state, "Work"));
        projects::move_agent(&mut state, closed, &work);
        let mut back = state.agents[&closed].summary.clone();
        let project = |state: &AppState, agent| projects::project_of(state, &state.agents[&agent]);
        let other_project = project(&state, AgentId::new(1));
        request_close(&mut state, &mut drafts, closed);
        crate::state::apply::apply(
            &mut state,
            &mut drafts,
            event(cockpit_protocol::Event::AgentRemoved { agent: closed }),
            MachineId::THIS,
        );

        undo_close(&mut state);
        let before_answer = needed_requests(&mut state);
        back.id = AgentId::new(4);
        crate::state::apply::apply(
            &mut state,
            &mut drafts,
            event(cockpit_protocol::Event::AgentAdded(back)),
            MachineId::THIS,
        );
        needed_requests(&mut state);

        assert_eq!(sent_move(&before_answer), None);
        assert_eq!(project(&state, AgentId::new(1)), other_project);
        assert_eq!(project(&state, AgentId::new(4)), work);
    }

    #[test]
    fn undo_of_a_fork_without_a_turn_leaves_its_source_in_place() {
        let mut state = idle_ordered();
        let mut drafts = Drafts::default();
        let fork = AgentId::new(3);
        if let Some(entry) = state.agents.get_mut(&fork) {
            entry.summary.session = Some(ClaudeSessionId::new("session-1".to_owned()));
        }
        let work = ProjectKey::Kept(projects::create(&mut state, "Work"));
        projects::move_agent(&mut state, fork, &work);
        let source =
            |state: &AppState| projects::project_of(state, &state.agents[&AgentId::new(1)]);
        let source_project = source(&state);
        request_close(&mut state, &mut drafts, fork);
        crate::state::apply::apply(
            &mut state,
            &mut drafts,
            event(cockpit_protocol::Event::AgentRemoved { agent: fork }),
            MachineId::THIS,
        );

        undo_close(&mut state);
        // The daemon answers with the agent that has the session.
        apply_response(
            &mut state,
            &mut drafts,
            Method::SessionsResume,
            Purpose::Reopen,
            Ok(ResponseBody::AgentStarted {
                agent: AgentId::new(1),
            }),
            MachineId::THIS,
        );
        let moves = needed_requests(&mut state);

        assert_eq!(source(&state), source_project);
        assert_eq!(sent_move(&moves), None);
    }

    #[test]
    fn undo_brings_the_session_back_to_the_project_it_was_moved_to() {
        let mut state = idle_ordered();
        let mut drafts = Drafts::default();
        let work = ProjectKey::Kept(projects::create(&mut state, "Work"));
        projects::move_agent(&mut state, AgentId::new(2), &work);
        let mut back = state.agents[&AgentId::new(2)].summary.clone();
        request_close(&mut state, &mut drafts, AgentId::new(2));
        crate::state::apply::apply(
            &mut state,
            &mut drafts,
            event(cockpit_protocol::Event::AgentRemoved {
                agent: AgentId::new(2),
            }),
            MachineId::THIS,
        );

        undo_close(&mut state);
        back.id = AgentId::new(4);
        crate::state::apply::apply(
            &mut state,
            &mut drafts,
            event(cockpit_protocol::Event::AgentAdded(back)),
            MachineId::THIS,
        );
        apply_response(
            &mut state,
            &mut drafts,
            Method::SessionsResume,
            Purpose::Reopen,
            Ok(ResponseBody::AgentStarted {
                agent: AgentId::new(4),
            }),
            MachineId::THIS,
        );
        needed_requests(&mut state);

        let entry = &state.agents[&AgentId::new(4)];
        assert_eq!(projects::project_of(&state, entry), work);
    }

    #[test]
    fn an_agent_without_a_session_closes_without_undo() {
        let mut state = idle_ordered();
        if let Some(entry) = state.agents.get_mut(&AgentId::new(2)) {
            entry.summary.session = None;
        }

        request_close(&mut state, &mut Drafts::default(), AgentId::new(2));

        assert!(state.ui.closed.is_some());
        assert!(undo_close(&mut state).is_empty());
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
                "Set emoji",
                "Open file",
                "Close agent",
                "Performance info",
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
    fn set_emoji_in_the_menu_opens_the_picker_whose_pick_renames_the_agent() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        open_menu(&mut state, &mut drafts, "emoji");

        let picked = palette_pick(&mut state, &mut drafts, agent(), None);
        let opened = state.ui.popover;
        let renamed = set_title_emoji(&mut state, agent(), Some("🐛"));

        assert_eq!(picked, Picked::default());
        assert_eq!(opened, Some(Popover::TitleEmoji));
        assert_eq!(state.ui.popover, None, "a pick closes the picker");
        assert_eq!(
            sent(&renamed),
            [&Request::AgentRename(RenameParams {
                agent: agent(),
                title: "🐛 Agent 1".to_owned()
            })]
        );
    }

    #[test]
    fn an_emoji_pick_that_changes_nothing_or_empties_the_title_renames_nothing() {
        let mut state = state(AgentStatus::Idle);
        let titled = |state: &mut AppState, title: &str| {
            if let Some(entry) = state.agents.get_mut(&agent()) {
                entry.summary.title = title.to_owned();
            }
        };

        titled(&mut state, "🐛 Agent 1");
        let same = set_title_emoji(&mut state, agent(), Some("🐛"));
        titled(&mut state, "🐛");
        let emptied = set_title_emoji(&mut state, agent(), None);

        assert!(same.is_empty());
        assert!(emptied.is_empty());
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
        let on_the_first_command = press(Direction::Down, 12);
        let at_the_bottom = press(Direction::Down, 5);
        let back_on_the_settings = press(Direction::Up, 3);

        assert_eq!(
            (
                up_at_the_top,
                on_the_first_command,
                at_the_bottom,
                back_on_the_settings
            ),
            (0, 12, 13, 10)
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
                delivery: PromptDelivery::Steer,
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
        let mut export = |older_items| {
            if let Some(entry) = state.agents.get_mut(&agent()) {
                entry.transcript.merge_page(HistoryPage {
                    items: vec![TranscriptItem {
                        id: ItemId::new(1),
                        parent: None,
                        body: ItemBody::AssistantText {
                            markdown: "Done.".to_owned(),
                        },
                    }],
                    older_items,
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

        let (whole, whole_note) = export(OlderItems::None);
        let (part, part_note) = export(OlderItems::Exist);

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
        // The note says how to load the older messages.
        assert!(part_note.is_some_and(|(_, text)| {
            text.contains("without the older messages") && text.contains("Scroll to the top")
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
                kept_open: false,
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
                kept_open: false,
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

    /// Gives the selected agent `AGENT` five more items than an agent away keeps.
    fn agent_holding_more_than_kept_away() -> AppState {
        let mut state = state_with(vec![
            summary(AGENT, AgentStatus::Working),
            summary(2, AgentStatus::Idle),
        ]);
        let held = u32::try_from(KEPT_ITEMS_AWAY).unwrap() + 5;
        let page = HistoryPage {
            items: (1..=held)
                .map(|id| TranscriptItem {
                    id: ItemId::new(id),
                    parent: None,
                    body: ItemBody::Notice {
                        text: "compacted".to_owned(),
                        detail: None,
                        kind: NoticeKind::Plain,
                    },
                })
                .collect(),
            older_items: OlderItems::None,
        };
        state
            .agents
            .get_mut(&agent())
            .unwrap()
            .transcript
            .merge_page(page);
        state.ui.selected = Some(agent());
        state
    }

    #[test]
    fn the_agent_left_keeps_as_many_items_as_any_agent_away() {
        let mut state = agent_holding_more_than_kept_away();

        select_agent(&mut state, agent());
        let while_looked_at = state.agents[&agent()].transcript.items().count();
        select_agent(&mut state, AgentId::new(2));

        let left = &state.agents[&agent()].transcript;
        assert_eq!(while_looked_at, KEPT_ITEMS_AWAY + 5);
        assert_eq!(left.items().count(), KEPT_ITEMS_AWAY);
        assert_eq!(left.first_id(), Some(ItemId::new(6)));
        assert_eq!(left.history(), &HistoryState::Loaded { has_older: true });
    }

    /// The chat goes back to the place of an agent left scrolled up, which can be among its
    /// oldest items.
    #[test]
    fn the_agent_left_scrolled_up_keeps_the_items_the_reader_was_at() {
        let mut state = agent_holding_more_than_kept_away();
        state
            .ui
            .chat_scrolled_up
            .insert(agent(), Some(ItemId::new(300)));

        select_agent(&mut state, AgentId::new(2));

        let left = &state.agents[&agent()].transcript;
        assert_eq!(left.items().count(), KEPT_ITEMS_AWAY + 5);
        assert_eq!(left.first_id(), Some(ItemId::new(1)));
    }

    /// A notification selects another agent without a click in the window, which would have
    /// saved the edit: a hidden edit would then take the first Esc meant to stop that agent.
    #[test]
    fn selecting_another_agent_cancels_a_rename_the_window_left_open() {
        let mut state = state_with(vec![
            summary(AGENT, AgentStatus::Idle),
            summary(2, AgentStatus::Working),
        ]);
        state.ui.selected = Some(agent());
        let mut drafts = Drafts::default();
        let renamed = |state: &mut AppState, drafts: &mut Drafts, kept_open: bool| {
            select_agent(state, agent());
            start_rename(state, drafts, agent());
            rename_text(state, drafts, "Half typ".to_owned());
            set_title_kept_open(state, kept_open);
            select_agent(state, AgentId::new(2));
            state.ui.title_edit.as_ref().map(|edit| edit.agent)
        };

        let left_open = renamed(&mut state, &mut drafts, true);
        let after_left_open = finish_rename(&mut state, &mut drafts);
        let focused = renamed(&mut state, &mut drafts, false);
        select_agent(&mut state, agent());
        start_rename(&mut state, &mut drafts, agent());
        set_title_kept_open(&mut state, true);
        select_agent(&mut state, agent());

        assert_eq!(left_open, None);
        assert!(after_left_open.is_empty(), "nothing is renamed");
        assert_eq!(
            focused,
            Some(agent()),
            "the blur of a field that had the focus saves it"
        );
        assert_eq!(
            state.ui.title_edit.map(|edit| edit.agent),
            Some(agent()),
            "the same agent keeps its edit"
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
        send_prompt(
            &mut state,
            &mut drafts,
            agent(),
            None,
            PromptDelivery::Steer,
        );
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
            PromptDelivery::Queue,
        );

        let Some(Outgoing {
            request: Request::AgentPrompt(params),
            ..
        }) = requests.first()
        else {
            panic!("expected a prompt, got {requests:?}");
        };
        assert_eq!(params.prompt.text, "Also cover the edge case");
        assert_eq!(params.delivery, PromptDelivery::Queue);
        assert!(drafts.draft(agent()).is_some_and(|draft| draft.is_empty()));
    }

    #[test]
    fn sending_quotes_the_passages_and_takes_the_comment_still_being_written() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        let passage = |item: u32, text: &str| quotes::Passage {
            anchor: quotes::QuoteAnchor {
                item: ItemId::new(item),
                start: Utf16Offset::new(0),
                end: Utf16Offset::end_of(text),
            },
            raw: text.to_owned(),
            text: text.to_owned(),
            code: false,
        };
        quotes::start(&mut drafts, agent(), passage(3, "keep one cache per host"));
        quotes::type_comment(&mut drafts, "Per host limits too?".to_owned());
        quotes::commit(&mut drafts, CommentPlacement::Beside);
        quotes::start(&mut drafts, agent(), passage(5, "drop it on read"));
        quotes::type_comment(&mut drafts, "And on a timer?".to_owned());

        let requests = send_prompt(
            &mut state,
            &mut drafts,
            agent(),
            Some("Then go ahead.".to_owned()),
            PromptDelivery::Steer,
        );

        let Some(Outgoing {
            request: Request::AgentPrompt(params),
            ..
        }) = requests.first()
        else {
            panic!("expected a prompt, got {requests:?}");
        };
        assert_eq!(
            params.prompt.text,
            "> keep one cache per host\n\nPer host limits too?\n\n> drop it on read\n\nAnd on a timer?\n\nThen go ahead."
        );
        assert_eq!(drafts.quote_editor, None);
        assert!(drafts.draft(agent()).is_some_and(|draft| draft.is_empty()));
    }

    #[test]
    fn an_empty_draft_or_an_exited_agent_sends_nothing() {
        let mut drafts = Drafts::default();
        let mut idle = state(AgentStatus::Idle);
        let typed = Some("  ".to_owned());
        assert!(
            send_prompt(
                &mut idle,
                &mut drafts,
                agent(),
                typed,
                PromptDelivery::Steer
            )
            .is_empty()
        );

        let mut exited = state(AgentStatus::Exited);
        let typed = Some("hello".to_owned());
        assert!(
            send_prompt(
                &mut exited,
                &mut drafts,
                agent(),
                typed,
                PromptDelivery::Steer
            )
            .is_empty()
        );
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
        let question = question(Selection::Single);

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
        let question = question(Selection::Multiple);

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
        let item = with_question(&mut state, question(Selection::Single));

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
        let item = with_question(&mut state, question(Selection::Single));

        pick_by_number(&state, &mut drafts, agent(), item, 9);

        assert!(submit_question(&state, &drafts, agent(), item).is_empty());
    }

    /// Three questions: "Fixtures" and "Tests", one choice each, then "Notes", any choice.
    fn three_questions() -> Question {
        let mut question = question(Selection::Single);
        let mut tests = question.questions[0].clone();
        tests.header = "Tests".to_owned();
        let mut notes = question.questions[0].clone();
        notes.header = "Notes".to_owned();
        notes.selection = Selection::Multiple;
        question.questions.extend([tests, notes]);
        question
    }

    fn shown(drafts: &Drafts, question: &Question) -> usize {
        shown_question(question, drafts.questions.get(&question.pending))
    }

    #[test]
    fn a_single_choice_pick_shows_the_next_question_and_the_numbers_follow_it() {
        let mut state = state(AgentStatus::NeedsYou);
        let mut drafts = Drafts::default();
        let question = three_questions();
        let item = with_question(&mut state, question.clone());

        pick_by_number(&state, &mut drafts, agent(), item, 1);
        assert_eq!(shown(&drafts, &question), 1);
        pick_by_number(&state, &mut drafts, agent(), item, 2);
        assert_eq!(shown(&drafts, &question), 2);
        // A multiple-choice pick stays on its question.
        pick_by_number(&state, &mut drafts, agent(), item, 3);
        pick_by_number(&state, &mut drafts, agent(), item, 1);
        assert_eq!(shown(&drafts, &question), 2);

        let answers = question_answers(&question, drafts.questions.get(&question.pending));
        let picked: Vec<Vec<String>> = answers
            .unwrap()
            .into_iter()
            .map(|answer| answer.selected)
            .collect();
        assert_eq!(
            picked,
            [
                vec!["Keep".to_owned()],
                vec!["Rewrite".to_owned()],
                vec!["Keep".to_owned(), "Drop".to_owned()],
            ]
        );
    }

    #[test]
    fn enter_shows_the_next_question_without_an_answer_then_sends() {
        let mut state = state(AgentStatus::NeedsYou);
        let mut drafts = Drafts::default();
        let question = three_questions();
        let item = with_question(&mut state, question.clone());
        show_question(&mut drafts, &question, 1);
        pick_option(&mut drafts, &question, 1, 0);

        // From "Notes", the next question without an answer is "Fixtures", round the end.
        assert_eq!(
            enter_on_question(&state, &mut drafts, agent(), item),
            Some(Vec::new())
        );
        assert_eq!(shown(&drafts, &question), 0);

        pick_option(&mut drafts, &question, 0, 0);
        pick_option(&mut drafts, &question, 2, 0);
        let sent = enter_on_question(&state, &mut drafts, agent(), item);

        assert!(matches!(
            sent.as_deref(),
            Some([Outgoing {
                request: Request::AgentAnswer(_),
                ..
            }])
        ));
    }

    #[test]
    fn left_and_right_move_between_questions_and_stop_at_the_ends() {
        let mut state = state(AgentStatus::NeedsYou);
        let mut drafts = Drafts::default();
        let question = three_questions();
        let item = with_question(&mut state, question.clone());

        step_question(&state, &mut drafts, agent(), item, Step::Previous);
        assert_eq!(shown(&drafts, &question), 0);
        for _ in 0..3 {
            step_question(&state, &mut drafts, agent(), item, Step::Next);
        }
        assert_eq!(shown(&drafts, &question), 2);
        step_question(&state, &mut drafts, agent(), item, Step::Previous);
        assert_eq!(shown(&drafts, &question), 1);
    }

    #[test]
    fn a_single_choice_question_sends_either_its_pick_or_its_other_text() {
        let mut drafts = Drafts::default();
        let question = question(Selection::Single);
        let sent = |drafts: &Drafts| {
            question_answers(&question, drafts.questions.get(&question.pending))
                .and_then(|answers| answers.into_iter().next())
        };

        pick_option(&mut drafts, &question, 0, 1);
        set_other_answer(&mut drafts, &question, 0, "Ask the owner".to_owned());
        assert_eq!(
            sent(&drafts),
            Some(QuestionAnswer {
                selected: Vec::new(),
                other: Some("Ask the owner".to_owned()),
            })
        );

        pick_option(&mut drafts, &question, 0, 2);
        assert_eq!(
            sent(&drafts),
            Some(QuestionAnswer {
                selected: vec!["Drop".to_owned()],
                other: None,
            })
        );
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
    fn a_markdown_file_opens_rendered() {
        let mut state = state(AgentStatus::Idle);

        open_file(&mut state, agent(), PathBuf::from("docs/notes.md"), None);

        let view = state.ui.file_view(agent()).unwrap();
        assert_eq!(view.mode, ViewMode::Preview);
    }

    #[test]
    fn the_opening_mode_follows_the_line_the_edits_and_the_shown_mode() {
        let readme = Path::new("README.md");
        let lib = Path::new("src/lib.rs");
        let preview = Some(ViewMode::Preview);
        let diff = Some(ViewMode::Diff);
        let (changed, unchanged) = (true, false);

        assert_eq!(
            opening_mode(readme, Some(4), unchanged, preview),
            ViewMode::WholeFile
        );
        assert_eq!(
            opening_mode(readme, None, unchanged, diff),
            ViewMode::Preview
        );
        assert_eq!(opening_mode(readme, None, changed, diff), ViewMode::Diff);
        assert_eq!(opening_mode(readme, None, changed, None), ViewMode::Diff);
        assert_eq!(
            opening_mode(readme, None, changed, preview),
            ViewMode::Preview
        );
        assert_eq!(opening_mode(lib, None, changed, preview), ViewMode::Diff);
        assert_eq!(
            opening_mode(lib, None, unchanged, preview),
            ViewMode::WholeFile
        );
    }

    #[test]
    fn tabs_close_one_by_one_and_the_last_closes_the_view() {
        let mut state = state(AgentStatus::Idle);
        let changed = |path: &str| ChangedFile {
            path: PathBuf::from(format!("/work/project-1/{path}")),
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

    /// The loaded messages may not reach back to the edits: the daemon's answer brings them.
    #[test]
    fn a_file_changed_before_the_loaded_messages_shows_its_diff() {
        let mut state = state(AgentStatus::Idle);
        let full = PathBuf::from("/work/project-1/src/cart.rs");
        state.agents.get_mut(&agent()).unwrap().details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: full.clone(),
                lines_added: 1,
                lines_removed: 1,
            }],
            ..AgentDetails::default()
        });
        let edit = FileEdit {
            path: full.clone(),
            old_text: "let total = 1;".to_owned(),
            new_text: "let total = 2;".to_owned(),
            texts: EditTexts::Kept,
        };

        open_file(&mut state, agent(), full.clone(), None);
        read_answer(
            &mut state,
            "src/cart.rs",
            Ok(ResponseBody::File {
                path: Some(full),
                text: "let total = 2;\n".to_owned(),
                edits: Some(vec![edit.clone()]),
            }),
        );

        assert_eq!(state.ui.file_view(agent()).unwrap().mode, ViewMode::Diff);
        let shown = state.agents[&agent()].files.get(Path::new("src/cart.rs"));
        let read = FileRead::Text {
            text: "let total = 2;\n".to_owned(),
            edits: vec![edit],
        };
        assert_eq!(shown, Some(&Loadable::Loaded(read)));
    }

    #[test]
    fn edits_in_the_first_answer_turn_the_view_to_the_diff() {
        let mut state = state(AgentStatus::Idle);
        let full = PathBuf::from("/work/project-1/src/cart.rs");

        open_file(&mut state, agent(), full.clone(), None);
        assert_eq!(
            state.ui.file_view(agent()).unwrap().mode,
            ViewMode::WholeFile
        );
        read_answer(
            &mut state,
            "src/cart.rs",
            Ok(ResponseBody::File {
                path: Some(full.clone()),
                text: "let total = 2;\n".to_owned(),
                edits: Some(vec![FileEdit {
                    path: full,
                    old_text: "let total = 1;".to_owned(),
                    new_text: "let total = 2;".to_owned(),
                    texts: EditTexts::Kept,
                }]),
            }),
        );

        assert_eq!(state.ui.file_view(agent()).unwrap().mode, ViewMode::Diff);
    }

    #[test]
    fn on_windows_a_file_is_changed_whatever_the_case_of_its_path() {
        let mut state = state(AgentStatus::Idle);
        let entry = state.agents.get_mut(&agent()).unwrap();
        entry.summary.folder = PathBuf::from(r"C:\work\project-1");
        entry.details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: PathBuf::from(r"C:\work\project-1\src\cart.rs"),
                lines_added: 1,
                lines_removed: 0,
            }],
            ..AgentDetails::default()
        });

        let entry = &state.agents[&agent()];
        assert!(entry.has_changed(Path::new(r"src\Cart.rs")));
        assert!(!entry.has_changed(Path::new(r"src\carts.rs")));
    }

    #[test]
    fn a_daemon_that_predates_the_edits_leaves_those_of_the_loaded_messages() {
        let mut state = state(AgentStatus::Idle);
        let full = PathBuf::from("/work/project-1/src/cart.rs");
        let edit = FileEdit {
            path: full.clone(),
            old_text: "let total = 1;".to_owned(),
            new_text: "let total = 2;".to_owned(),
            texts: EditTexts::Kept,
        };
        let entry = state.agents.get_mut(&agent()).unwrap();
        entry.transcript.append(TranscriptItem {
            id: ItemId::new(1),
            parent: None,
            body: ItemBody::ToolCall(ToolCall {
                tool: "Edit".to_owned(),
                summary: "src/cart.rs".to_owned(),
                input: serde_json::Value::Null,
                status: ToolCallStatus::Done,
                output: None,
                exit_code: None,
                edit: Some(edit.clone()),
            }),
        });

        open_file(&mut state, agent(), full.clone(), None);
        read_answer(
            &mut state,
            "src/cart.rs",
            Ok(ResponseBody::File {
                path: Some(full),
                text: "let total = 2;\n".to_owned(),
                edits: None,
            }),
        );

        let shown = state.agents[&agent()].files.get(Path::new("src/cart.rs"));
        let read = FileRead::Text {
            text: "let total = 2;\n".to_owned(),
            edits: vec![edit],
        };
        assert_eq!(shown, Some(&Loadable::Loaded(read)));
    }

    #[test]
    fn on_windows_a_link_with_slashes_opens_the_changed_files_tab() {
        let mut state = state(AgentStatus::Idle);
        let entry = state.agents.get_mut(&agent()).unwrap();
        entry.summary.folder = PathBuf::from(r"C:\work\project-1");
        entry.details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: PathBuf::from(r"C:\work\project-1\src\a.rs"),
                lines_added: 1,
                lines_removed: 0,
            }],
            ..AgentDetails::default()
        });

        open_file(&mut state, agent(), PathBuf::from("src/a.rs"), None);

        let changed = state.agents[&agent()].changed_paths();
        let view = state.ui.file_view(agent()).unwrap();
        assert_eq!(view.tabs(&changed), [PathBuf::from(r"src\a.rs")]);
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
                    path: Some(PathBuf::from(format!("/work/project-1/{path}"))),
                    text: format!("text of {path}"),
                    edits: Some(Vec::new()),
                }),
                MachineId::THIS,
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
            MachineId::THIS,
        );
    }

    #[test]
    fn a_file_opened_from_the_agent_map_closes_the_map() {
        let mut state = state(AgentStatus::Working);
        crate::state::agent_map::open_on_agent(&mut state, agent());

        open_file(&mut state, agent(), PathBuf::from("src/lexer.rs"), Some(12));

        assert_eq!(state.ui.dialog, None);
        assert!(state.ui.file_view(agent()).is_some());
    }

    #[test]
    fn an_image_the_daemon_read_shows_in_the_view() {
        let mut state = state(AgentStatus::Idle);
        let image = Image {
            media_type: ImageMediaType::Jpeg,
            data: "aGVsbG8=".to_owned(),
        };
        open_file(
            &mut state,
            agent(),
            PathBuf::from("shots/sheet05.jpg"),
            None,
        );

        read_answer(
            &mut state,
            "shots/sheet05.jpg",
            Ok(ResponseBody::Image {
                path: PathBuf::from("/work/project-1/shots/sheet05.jpg"),
                image: image.clone(),
            }),
        );

        assert_eq!(
            state.agents[&agent()].files[Path::new("shots/sheet05.jpg")],
            Loadable::Loaded(FileRead::Image(image))
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
                path: Some(PathBuf::from(found)),
                text: "export class RegisterParcel {}\n".to_owned(),
                edits: Some(Vec::new()),
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

    /// A daemon that predates the path in its answer reads only the path asked for.
    #[test]
    fn an_answer_without_its_path_shows_the_path_asked_for() {
        let mut state = state(AgentStatus::Idle);
        open_file(&mut state, agent(), PathBuf::from("src/main.rs"), None);

        read_answer(
            &mut state,
            "src/main.rs",
            Ok(ResponseBody::File {
                path: None,
                text: "fn main() {}\n".to_owned(),
                edits: None,
            }),
        );

        let view = state.ui.file_view(agent()).unwrap();
        assert_eq!(view.path, Path::new("src/main.rs"));
        let shown = state.agents[&agent()].files.get(Path::new("src/main.rs"));
        let text = "fn main() {}\n";
        assert_eq!(shown, Some(&Loadable::Loaded(FileRead::unedited(text))));
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

    /// Two refused moves of one agent would leave it where the first one put it: the second
    /// puts back the position the first gave.
    #[test]
    fn a_move_while_disconnected_leaves_the_order_and_says_why() {
        let mut state = ordered();
        state.machines.this.connection = ConnectionState::Connecting { attempt: 2 };

        let dropped = drop_agent(&mut state, AgentId::new(1), Some(AgentId::new(3)), None);
        let pressed = move_selected(&mut state, Direction::Up);

        assert!(dropped.is_empty());
        assert!(pressed.is_empty());
        assert_eq!(sidebar(&state), [3, 2, 1]);
        let notes: Vec<&str> = state
            .ui
            .errors
            .iter()
            .map(|note| note.text.as_str())
            .collect();
        assert_eq!(
            notes,
            [
                "The agent was not moved: not connected to the daemon",
                "The agent was not moved: not connected to the daemon"
            ]
        );
    }

    #[test]
    fn a_refused_move_puts_the_agents_back_unless_an_event_moved_them() {
        let mut state = ordered();
        let mut drafts = Drafts::default();
        let refused = || {
            Err(RequestError::Failed(ErrorBody::new(
                ErrorCode::NotFound,
                "no agent 3".to_owned(),
            )))
        };

        let first = move_agent(&mut state, AgentId::new(1), Some(AgentId::new(3)));
        for outgoing in first {
            apply_response(
                &mut state,
                &mut drafts,
                Method::AgentMove,
                outgoing.purpose,
                refused(),
                MachineId::THIS,
            );
        }
        let after_refusal = sidebar(&state);
        let second = move_agent(&mut state, AgentId::new(1), Some(AgentId::new(2)));
        if let Some(entry) = state.agents.get_mut(&AgentId::new(1)) {
            entry.summary.position = Position::new(9000);
        }
        for outgoing in second {
            apply_response(
                &mut state,
                &mut drafts,
                Method::AgentMove,
                outgoing.purpose,
                refused(),
                MachineId::THIS,
            );
        }

        assert_eq!(after_refusal, [3, 2, 1]);
        assert_eq!(sidebar(&state), [1, 3, 2], "the event's position stays");
        assert_eq!(state.ui.errors.len(), 2);
    }

    #[test]
    fn a_row_dropped_below_the_last_one_goes_to_the_bottom() {
        let mut state = ordered();

        let to_bottom = drop_agent(&mut state, AgentId::new(3), None, None);
        let to_top = drop_agent(&mut state, AgentId::new(1), Some(AgentId::new(2)), None);

        assert_eq!(sent_move(&to_bottom).and_then(|params| params.before), None);
        assert_eq!(
            sent_move(&to_top).and_then(|params| params.before),
            Some(AgentId::new(2))
        );
        assert_eq!(sidebar(&state), [1, 2, 3]);
    }

    /// Agents 1 to 3 on this computer, and 1 and 2 of devbox's daemon, all connected.
    fn two_machines() -> (AppState, MachineId) {
        let devbox = MachineId::new(1);
        let mut state = ordered();
        let mut known = MachineState::new(DaemonLabel::ssh("devbox".to_owned()));
        known.connection = ConnectionState::Connected;
        state.machines.others.insert(devbox, known);
        for (id, position) in [(1, 1024), (2, 2048)] {
            let mut remote = placed(id, AgentStatus::Idle, position);
            remote.id = devbox.app_agent(AgentId::new(id));
            state.agents.insert(remote.id, AgentEntry::new(remote));
        }
        (state, devbox)
    }

    #[test]
    fn the_sidebar_heads_each_machine_with_its_own_rows_in_order() {
        let (state, devbox) = two_machines();
        let ids = |rows: &[&AgentEntry]| -> Vec<AgentId> {
            rows.iter().map(|entry| entry.id()).collect()
        };

        let groups = state.sidebar_groups();

        let keys: Vec<GroupKey> = groups.iter().map(|group| group.key.clone()).collect();
        assert_eq!(
            keys,
            [
                GroupKey::Machine(MachineId::THIS),
                GroupKey::Machine(devbox)
            ]
        );
        assert_eq!(
            ids(&groups[0].rows),
            [AgentId::new(3), AgentId::new(2), AgentId::new(1)]
        );
        assert_eq!(
            ids(&groups[1].rows),
            [
                devbox.app_agent(AgentId::new(2)),
                devbox.app_agent(AgentId::new(1))
            ]
        );
        let alone: Vec<GroupKey> = ordered()
            .sidebar_groups()
            .into_iter()
            .map(|group| group.key)
            .collect();
        assert_eq!(
            alone,
            [GroupKey::Machine(MachineId::THIS)],
            "this computer alone has its panel too"
        );
    }

    #[test]
    fn an_agent_dropped_by_another_machines_row_goes_to_its_own_machines_end() {
        let (mut state, devbox) = two_machines();
        let first_on_devbox = devbox.app_agent(AgentId::new(2));
        let last_on_devbox = devbox.app_agent(AgentId::new(1));

        let below = drop_agent(&mut state, AgentId::new(3), Some(first_on_devbox), None);
        let above = drop_agent(&mut state, last_on_devbox, Some(AgentId::new(1)), None);

        let moved = |outgoing: &[Outgoing]| match outgoing.first().map(|outgoing| &outgoing.request)
        {
            Some(Request::AgentMove(params)) => Some(params.before),
            _ => None,
        };
        assert_eq!(moved(&below), Some(None), "below this computer's last row");
        assert_eq!(
            moved(&above),
            Some(Some(first_on_devbox)),
            "above devbox's first row"
        );
    }

    /// Split by machine, in the app's order this computer's agents 1, 2 and 3 alternate with
    /// devbox's 1 and 2. The sidebar lists this computer's 1, 2 and 3, then devbox's 1 and 2.
    fn split_by_machine() -> (AppState, [AgentId; 5]) {
        let (mut state, devbox) = two_machines();
        state.prefs.sidebar.group_by = GroupBy::Project;
        state.prefs.sidebar.project_split = ProjectSplit::ByMachine;
        let there = |id| devbox.app_agent(AgentId::new(id));
        let here = AgentId::new;
        for agent in [here(1), there(1), here(2), there(2), here(3)] {
            grouping::move_in_app_order(&mut state, agent, None);
        }
        let rows = [here(1), here(2), here(3), there(1), there(2)];
        assert_eq!(shown_ids(&state), rows);
        (state, rows)
    }

    fn shown_ids(state: &AppState) -> Vec<AgentId> {
        state
            .sidebar_agents()
            .iter()
            .map(|entry| entry.id())
            .collect()
    }

    #[test]
    fn split_by_machine_a_drop_on_a_machine_below_puts_the_row_at_its_machines_bottom() {
        let (mut state, [a1, a2, a3, d1, d2]) = split_by_machine();

        drop_agent(&mut state, a1, Some(d2), None);

        assert_eq!(shown_ids(&state), [a2, a3, a1, d1, d2]);
    }

    #[test]
    fn split_by_machine_a_drop_on_a_machine_above_puts_the_row_at_its_machines_top() {
        let (mut state, [a1, a2, a3, d1, d2]) = split_by_machine();

        drop_agent(&mut state, d2, Some(a3), None);

        assert_eq!(shown_ids(&state), [a1, a2, a3, d2, d1]);
    }

    #[test]
    fn split_by_machine_a_drop_at_the_groups_end_puts_the_row_at_its_machines_bottom() {
        let (mut state, [a1, a2, a3, d1, d2]) = split_by_machine();

        drop_agent(&mut state, a1, None, Some(&ProjectKey::None));

        assert_eq!(shown_ids(&state), [a2, a3, a1, d1, d2]);
    }

    /// Selects devbox's agent 2, in `/srv/shop`.
    fn devbox_selected(state: &mut AppState, devbox: MachineId) {
        let shop = devbox.app_agent(AgentId::new(2));
        if let Some(entry) = state.agents.get_mut(&shop) {
            entry.summary.folder = PathBuf::from("/srv/shop");
        }
        state.ui.selected = Some(shop);
    }

    /// This computer's agent 3 started last.
    #[test]
    fn the_new_agent_dialog_opens_where_the_selected_agent_runs() {
        let (mut state, devbox) = two_machines();
        devbox_selected(&mut state, devbox);
        if let Some(entry) = state.agents.get_mut(&AgentId::new(3)) {
            entry.summary.started_at = Timestamp::from_unix_millis(90_000);
        }
        let mut drafts = Drafts::default();

        open_new_agent(&mut state, &mut drafts);
        let asked = crate::state::needed_requests(&mut state);

        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::NewAgent(form)) if form.machine == devbox
        ));
        assert_eq!(drafts.new_agent.folder, "/srv/shop");
        assert!(
            asked
                .iter()
                .any(|outgoing| outgoing.request == Request::SessionsList && outgoing.to == devbox),
            "the dialog lists devbox's past sessions: {asked:?}"
        );
    }

    #[test]
    fn without_a_selection_the_new_agent_dialog_opens_where_the_last_agent_started() {
        let (mut state, devbox) = two_machines();
        if let Some(entry) = state.agents.get_mut(&devbox.app_agent(AgentId::new(2))) {
            entry.summary.started_at = Timestamp::from_unix_millis(90_000);
            entry.summary.folder = PathBuf::from("/srv/shop");
        }
        state.ui.selected = None;
        let mut drafts = Drafts::default();

        open_new_agent(&mut state, &mut drafts);

        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::NewAgent(form)) if form.machine == devbox
        ));
        assert_eq!(drafts.new_agent.folder, "/srv/shop");
    }

    /// Devbox's agent 1 started after the selected agent 2, so devbox last used its folder.
    #[test]
    fn the_folder_follows_the_machine_picked_unless_it_was_typed() {
        let (mut state, devbox) = two_machines();
        devbox_selected(&mut state, devbox);
        if let Some(entry) = state.agents.get_mut(&devbox.app_agent(AgentId::new(1))) {
            entry.summary.started_at = Timestamp::from_unix_millis(90_000);
        }
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);

        let to_this = pick_new_agent_machine(&mut state, &mut drafts, MachineId::THIS);
        let this_folder = drafts.new_agent.folder.clone();
        let back = pick_new_agent_machine(&mut state, &mut drafts, devbox);
        drafts.new_agent.folder = "/srv/typed".to_owned();
        let typed = pick_new_agent_machine(&mut state, &mut drafts, MachineId::THIS);

        assert!(this_folder.starts_with("/work/project-"), "{this_folder}");
        assert_eq!(to_this, Some(this_folder));
        assert_eq!(back.as_deref(), Some("/srv/shop"));
        assert_eq!(typed, None);
        assert_eq!(drafts.new_agent.folder, "/srv/typed");
    }

    /// A click on a notification selects another agent and leaves the dialog open. Devbox's
    /// agent 1 started after the agent selected at opening, so devbox last used its folder.
    #[test]
    fn the_folder_still_follows_the_machine_after_another_agent_is_selected() {
        let (mut state, devbox) = two_machines();
        devbox_selected(&mut state, devbox);
        if let Some(entry) = state.agents.get_mut(&devbox.app_agent(AgentId::new(1))) {
            entry.summary.started_at = Timestamp::from_unix_millis(90_000);
        }
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        select_agent(&mut state, AgentId::new(1));

        let to_this = pick_new_agent_machine(&mut state, &mut drafts, MachineId::THIS);
        let back = pick_new_agent_machine(&mut state, &mut drafts, devbox);

        assert!(to_this.is_some_and(|folder| folder.starts_with("/work/project-")));
        assert_eq!(back.as_deref(), Some("/srv/shop"));
    }

    /// This computer's agent 1 runs in billing's `/work/project-1`, devbox's in `/srv/billing`.
    #[test]
    fn a_machine_switch_puts_the_picked_project_s_folder_on_the_new_machine() {
        let (mut state, devbox) = two_machines();
        devbox_selected(&mut state, devbox);
        let billing = Repository::from_remote("github.com/acme/billing");
        for agent in [AgentId::new(1), devbox.app_agent(AgentId::new(1))] {
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.summary.repository = Some(billing.clone());
            }
        }
        if let Some(entry) = state.agents.get_mut(&devbox.app_agent(AgentId::new(1))) {
            entry.summary.folder = PathBuf::from("/srv/billing");
        }
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);

        let picked =
            pick_new_agent_project(&mut state, &mut drafts, ProjectKey::Repository(billing));
        let switched = pick_new_agent_machine(&mut state, &mut drafts, MachineId::THIS);

        assert_eq!(picked.as_deref(), Some("/srv/billing"));
        assert_eq!(switched.as_deref(), Some("/work/project-1"));
    }

    /// "No project" starts the agent where you say: the field stays, and every folder lists.
    /// Devbox's `/srv/shop` is in billing, so another pick would leave it.
    #[test]
    fn picking_no_project_leaves_the_folder_and_the_list() {
        let (mut state, devbox) = two_machines();
        devbox_selected(&mut state, devbox);
        if let Some(entry) = state.agents.get_mut(&devbox.app_agent(AgentId::new(2))) {
            entry.summary.repository = Some(Repository::from_remote("github.com/acme/billing"));
        }
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        let every = projects::recent_folders(&state, devbox, None).len();

        let picked = pick_new_agent_project(&mut state, &mut drafts, ProjectKey::None);

        assert_eq!(picked, None);
        assert_eq!(drafts.new_agent.folder, "/srv/shop");
        assert_eq!(
            projects::recent_folders(&state, devbox, Some(&ProjectKey::None)).len(),
            every
        );
    }

    #[test]
    fn the_new_agent_dialog_opens_on_this_computer_when_the_agent_s_machine_does_not_answer() {
        let (mut state, devbox) = two_machines();
        devbox_selected(&mut state, devbox);
        if let Some(known) = state.machines.others.get_mut(&devbox) {
            known.connection = ConnectionState::Connecting { attempt: 2 };
        }
        let mut drafts = Drafts::default();

        open_new_agent(&mut state, &mut drafts);

        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::NewAgent(form)) if form.machine == MachineId::THIS
        ));
        assert!(drafts.new_agent.folder.starts_with("/work/project-"));
    }

    #[test]
    fn a_new_agent_starts_on_the_machine_picked_with_its_default_account() {
        let (mut state, devbox) = two_machines();
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        drafts.new_agent.folder = "/srv/shop".to_owned();

        pick_new_agent_machine(&mut state, &mut drafts, devbox);
        let asked = crate::state::needed_requests(&mut state);
        let started = start_agent(&mut state, &drafts);

        assert!(
            asked
                .iter()
                .any(|outgoing| outgoing.request == Request::SessionsList && outgoing.to == devbox),
            "the dialog lists devbox's past sessions"
        );
        let [
            Outgoing {
                request: Request::AgentStart(params),
                to,
                ..
            },
        ] = started.as_slice()
        else {
            panic!("expected a start, got {started:?}");
        };
        assert_eq!(*to, devbox);
        assert_eq!(params.account, AccountId::DEFAULT);
    }

    #[test]
    fn a_machine_picked_for_a_new_agent_is_asked_for_its_sign_ins() {
        let (mut state, devbox) = two_machines();
        let mut drafts = Drafts::default();
        if let Some(known) = state.machines.others.get_mut(&devbox) {
            known.accounts = vec![Account {
                id: AccountId::DEFAULT,
                name: "Default".to_owned(),
                folder: PathBuf::from("/home/dev/.claude"),
                credentials: CredentialSource::SignIn,
            }];
            known.sign_ins.checked_at = Some(SystemTime::UNIX_EPOCH);
        }
        open_new_agent(&mut state, &mut drafts);

        pick_new_agent_machine(&mut state, &mut drafts, devbox);
        let asked = crate::state::needed_requests(&mut state);

        assert!(
            asked
                .iter()
                .any(|outgoing| outgoing.request == Request::AccountsList && outgoing.to == devbox),
            "{asked:?}"
        );
    }

    #[test]
    fn a_machine_that_does_not_answer_cannot_be_picked() {
        let (mut state, devbox) = two_machines();
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        if let Some(known) = state.machines.others.get_mut(&devbox) {
            known.connection = ConnectionState::Connecting { attempt: 2 };
        }

        pick_new_agent_machine(&mut state, &mut drafts, devbox);

        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::NewAgent(form)) if form.machine == MachineId::THIS
        ));
    }

    #[test]
    fn a_servers_agent_is_followed_while_this_computer_does_not_answer() {
        let (mut state, devbox) = two_machines();
        let remote = devbox.app_agent(AgentId::new(2));
        state.machines.this.connection = ConnectionState::Connecting { attempt: 3 };
        state.ui.selected = Some(remote);

        let asked = crate::state::needed_requests(&mut state);
        let hidden = set_mcp_hidden(&mut state, "notes", true);

        assert!(asked.iter().any(|outgoing| matches!(
            &outgoing.request,
            Request::AgentHistory(params) if params.agent == remote
        )));
        assert_eq!(hidden.first().map(|outgoing| outgoing.to), Some(devbox));
        assert!(
            state.machines.others[&devbox]
                .settings
                .hidden_mcp_servers
                .contains("notes")
        );
        assert!(state.machines.this.settings.hidden_mcp_servers.is_empty());
    }

    #[test]
    fn a_closed_servers_agent_comes_back_on_its_machine() {
        let (mut state, devbox) = two_machines();
        let remote = devbox.app_agent(AgentId::new(2));
        state.ui.closed = Some(ClosedAgent {
            serial: 1,
            agent: remote,
            title: "Release notes".to_owned(),
            session: Some(ClaudeSessionId::new("session-9".to_owned())),
            account: AccountId::DEFAULT,
            before: None,
            project: None,
        });

        let reopened = undo_close(&mut state);

        assert_eq!(reopened.first().map(|outgoing| outgoing.to), Some(devbox));
    }

    #[test]
    fn a_machine_folds_and_unfolds() {
        let (mut state, devbox) = two_machines();

        toggle_machine(&mut state, devbox);
        let folded = state.ui.folded_machines.contains(&devbox);
        toggle_machine(&mut state, devbox);

        assert!(folded);
        assert!(state.ui.folded_machines.is_empty());
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
        let dropped = drop_agent(&mut state, AgentId::new(1), None, None);
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
    fn the_sidebar_splitter_keeps_a_width_and_double_click_resets_it() {
        let mut state = state(AgentStatus::Idle);

        set_sidebar_width(&mut state, 311.6);
        assert_eq!(state.prefs.sidebar.width, Some(312));

        for refused in [199.4, 480.6, -5.0, 1e9, f64::NAN, f64::INFINITY] {
            set_sidebar_width(&mut state, refused);
            assert_eq!(state.prefs.sidebar.width, Some(312), "{refused}");
        }

        reset_sidebar_width(&mut state);
        assert_eq!(state.prefs.sidebar.width, None);
        assert_eq!(state.prefs.sidebar.width(), prefs::DEFAULT_SIDEBAR_WIDTH);
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

        assert!(
            state
                .machines
                .this
                .settings
                .hidden_mcp_servers
                .contains("search")
        );
        assert_eq!(
            requests.first().map(|outgoing| &outgoing.request),
            Some(&Request::SettingsSet(state.machines.this.settings.clone()))
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
    fn the_pointer_moves_the_switcher_highlight_within_the_listed_agents() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Working),
            summary(3, AgentStatus::Idle),
        ]);
        let mut drafts = Drafts::default();
        toggle_switcher(&mut state, &mut drafts);

        switcher_point(&state, &mut drafts, 2);
        assert_eq!(drafts.switcher.highlighted, 2);
        switcher_move(&state, &mut drafts, Direction::Up);
        assert_eq!(drafts.switcher.highlighted, 1);

        // A row the narrowed list no longer has: the pointer was on the old list.
        switcher_query(&mut drafts, "project-3".to_owned());
        switcher_point(&state, &mut drafts, 2);
        assert_eq!(drafts.switcher.highlighted, 0);

        switcher_pick(&mut state, &drafts, None);
        assert_eq!(state.ui.selected, Some(AgentId::new(3)));
    }

    #[test]
    fn a_new_agent_takes_the_name_typed_and_a_blank_one_lets_the_daemon_name_it() {
        let title_with = |name: &str| {
            let mut state = state(AgentStatus::Idle);
            let mut drafts = Drafts::default();
            open_new_agent(&mut state, &mut drafts);
            drafts.new_agent.name = name.to_owned();
            match start_agent(&mut state, &drafts).first() {
                Some(Outgoing {
                    request: Request::AgentStart(params),
                    ..
                }) => params.title.clone(),
                other => panic!("expected a start, got {other:?}"),
            }
        };

        assert_eq!(
            title_with("  Lexer columns "),
            Some("Lexer columns".to_owned())
        );
        assert_eq!(title_with("   "), None);
        assert_eq!(title_with(""), None);
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
    fn the_new_agent_form_copies_the_selected_agent_else_the_last_one_started() {
        // The last agent started has the lower id, so that the order of the ids cannot pass for
        // the order of the starts.
        let mut older = summary(2, AgentStatus::Idle);
        older.model = Some("claude-haiku-4-5".to_owned());
        older.effort = Some(Effort::Low);
        older.mode = PermissionMode::Default;
        let mut last = summary(1, AgentStatus::Exited);
        last.model = Some("claude-opus-5-5[1m]".to_owned());
        last.effort = Some(Effort::Xhigh);
        last.mode = PermissionMode::AcceptEdits;
        last.started_at = Timestamp::from_unix_millis(90_000);
        let mut state = state_with(vec![last, older]);
        let opened = |state: &mut AppState| {
            open_new_agent(state, &mut Drafts::default());
            let opened = match &state.ui.dialog {
                Some(Dialog::NewAgent(form)) => (form.model.clone(), form.effort, form.mode),
                other => panic!("expected the new agent dialog, got {other:?}"),
            };
            state.ui.dialog = None;
            opened
        };

        let unselected = opened(&mut state);
        state.ui.selected = Some(AgentId::new(2));
        let selected = opened(&mut state);

        assert_eq!(
            unselected,
            (
                Some("opus".to_owned()),
                Some(Effort::Xhigh),
                Some(PermissionMode::AcceptEdits)
            )
        );
        assert_eq!(
            selected,
            (
                Some("haiku".to_owned()),
                Some(Effort::Low),
                Some(PermissionMode::Default)
            )
        );
    }

    #[test]
    fn the_new_agent_form_starts_on_opus_and_auto_without_an_agent_or_a_known_model() {
        let mut state = state_with(Vec::new());
        open_new_agent(&mut state, &mut Drafts::default());
        let Some(Dialog::NewAgent(form)) = &state.ui.dialog else {
            panic!("expected the new agent dialog, got {:?}", state.ui.dialog);
        };
        assert_eq!(
            (form.model.as_deref(), form.effort, form.mode),
            (Some("opus"), None, Some(PermissionMode::Auto))
        );

        let mut unknown = summary(1, AgentStatus::Idle);
        unknown.model = Some("claude-mythical-1".to_owned());
        let mut state = state_with(vec![unknown]);
        open_new_agent(&mut state, &mut Drafts::default());
        let Some(Dialog::NewAgent(form)) = &state.ui.dialog else {
            panic!("expected the new agent dialog, got {:?}", state.ui.dialog);
        };
        assert_eq!(form.model.as_deref(), Some("opus"));
        assert_eq!(form.mode, Some(PermissionMode::Default));
    }

    /// Ctrl+N then Enter would otherwise start an agent that runs everything without
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

        assert_eq!(
            carried(PermissionMode::BypassPermissions),
            Some(PermissionMode::Auto)
        );
        assert_eq!(carried(PermissionMode::DontAsk), Some(PermissionMode::Auto));
        assert_eq!(carried(PermissionMode::Plan), Some(PermissionMode::Auto));
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
    fn the_start_tab_focuses_the_folder_field_until_it_holds_a_folder() {
        let mut empty = state_with(Vec::new());
        let mut empty_drafts = Drafts::default();
        open_new_agent(&mut empty, &mut empty_drafts);
        let mut recent = state(AgentStatus::Idle);
        let mut recent_drafts = Drafts::default();
        open_new_agent(&mut recent, &mut recent_drafts);
        let mut spaces = Drafts::default();
        spaces.new_agent.folder = "  ".to_owned();

        assert_eq!(start_focus(&empty_drafts.new_agent), StartField::Folder);
        assert_eq!(recent_drafts.new_agent.folder, "/work/project-1");
        assert_eq!(start_focus(&recent_drafts.new_agent), StartField::Message);
        assert_eq!(start_focus(&spaces.new_agent), StartField::Folder);
    }

    #[test]
    fn the_start_tab_back_from_accounts_focuses_the_field_its_folder_calls_for() {
        let mut state = state_with(Vec::new());
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        let before = start_focus(&drafts.new_agent);
        drafts.new_agent.folder = "/work/calc".to_owned();

        accounts::open_accounts(&mut state, None);
        state.ui.close_dialog();

        assert_eq!(before, StartField::Folder);
        assert!(matches!(state.ui.dialog, Some(Dialog::NewAgent(_))));
        assert_eq!(start_focus(&drafts.new_agent), StartField::Message);
    }

    /// Ctrl+K would replace the dialog, and Ctrl+N then open it empty.
    #[test]
    fn ctrl_k_leaves_the_new_agent_dialog_open_with_what_it_holds() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        drafts.new_agent.folder = "/work/calc".to_owned();
        drafts.new_agent.prompt = "Fix the flaky test".to_owned();

        toggle_switcher(&mut state, &mut drafts);
        open_new_agent(&mut state, &mut drafts);

        assert!(matches!(state.ui.dialog, Some(Dialog::NewAgent(_))));
        assert_eq!(
            (
                drafts.new_agent.folder.as_str(),
                drafts.new_agent.prompt.as_str()
            ),
            ("/work/calc", "Fix the flaky test")
        );
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
            MachineId::THIS,
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
            account: AccountId::DEFAULT,
        }
    }

    #[test]
    fn the_session_search_filters_by_title_and_folder_and_resumes_the_highlighted_one() {
        let mut running = summary(1, AgentStatus::Idle);
        running.session = Some(ClaudeSessionId::new("running".to_owned()));
        let mut state = state_with(vec![running]);
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        state.machines.this.sessions = Loadable::Loaded(vec![
            past_session("lexer", "Speed up the lexer", "/work/parser", 30),
            past_session("running", "Fix the parser", "/work/parser", 5),
            past_session("docs", "Rewrite the error page", "/work/docs-site", 60),
            past_session("columns", "Parser columns", "/work/tools", 10),
        ]);
        let ids = |state: &AppState, query: &str| -> Vec<String> {
            resumable_sessions(state, query, None)
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
                session: ClaudeSessionId::new("lexer".to_owned()),
                account: AccountId::DEFAULT,
            }))
        );
        session_query(&mut drafts, "nothing like it".to_owned());
        assert_eq!(drafts.new_agent.session_highlighted, 0);
    }

    #[test]
    fn the_pointer_moves_the_session_highlight_within_the_listed_sessions() {
        let mut state = state_with(Vec::new());
        let mut drafts = Drafts::default();
        open_new_agent(&mut state, &mut drafts);
        state.machines.this.sessions = Loadable::Loaded(vec![
            past_session("lexer", "Speed up the lexer", "/work/parser", 30),
            past_session("docs", "Rewrite the error page", "/work/docs-site", 60),
            past_session("columns", "Parser columns", "/work/tools", 10),
        ]);

        session_point(&state, &mut drafts, 2);
        assert_eq!(drafts.new_agent.session_highlighted, 2);
        session_move(&state, &mut drafts, Direction::Up);
        assert_eq!(drafts.new_agent.session_highlighted, 1);

        // A row the narrowed list no longer has: the pointer was on the old list.
        session_query(&mut drafts, "parser".to_owned());
        session_point(&state, &mut drafts, 1);
        session_point(&state, &mut drafts, 2);
        assert_eq!(drafts.new_agent.session_highlighted, 1);

        let requests = session_pick(&mut state, &drafts, None);
        assert_eq!(
            requests.first().map(|outgoing| &outgoing.request),
            Some(&Request::SessionsResume(SessionResumeParams {
                session: ClaudeSessionId::new("lexer".to_owned()),
                account: AccountId::DEFAULT,
            }))
        );
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
            MachineId::THIS,
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
                        detail: None,
                        kind: NoticeKind::Plain,
                    },
                }],
                older_items: OlderItems::Exist,
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

    #[test]
    fn selecting_another_agent_closes_the_stop_dialog_of_the_first() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Working),
            summary(2, AgentStatus::Working),
        ]);
        state.ui.dialog = Some(Dialog::StopAgent(agent()));

        select_agent(&mut state, agent());
        let same_agent = state.ui.dialog.clone();
        select_agent(&mut state, AgentId::new(2));

        assert_eq!(same_agent, Some(Dialog::StopAgent(agent())));
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn the_stop_dialog_closes_once_its_agent_has_no_turn_left() {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        state.ui.dialog = Some(Dialog::StopAgent(agent()));

        follow_stop_dialog(&mut state);
        let while_working = state.ui.dialog.clone();
        state.agents.get_mut(&agent()).unwrap().summary.status = AgentStatus::Idle;
        follow_stop_dialog(&mut state);

        assert_eq!(while_working, Some(Dialog::StopAgent(agent())));
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn the_stop_dialog_stops_the_agent_and_keeps_the_choice_of_asking_again() {
        let mut state = state_with(vec![summary(1, AgentStatus::Working)]);
        state.ui.dialog = Some(Dialog::StopAgent(agent()));

        let sent = stop_from_dialog(&mut state, agent(), EscStop::AtOnce);

        assert!(matches!(
            sent.as_slice(),
            [Outgoing {
                request: Request::AgentInterrupt(AgentParams { agent: stopped }),
                ..
            }] if *stopped == agent()
        ));
        assert_eq!(state.ui.dialog, None);
        assert_eq!(state.prefs.chat.esc_stop, EscStop::AtOnce);
    }
}
