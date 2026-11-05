//! What the user's clicks and keys change, and the requests they send. The connection sends the
//! returned requests, then the ones `needed_requests` adds.

use std::collections::BTreeSet;
use std::path::PathBuf;

use cockpit_protocol::{
    AgentId, AgentParams, AgentStatus, AgentSummary, Answer, AnswerParams, AnswerState,
    ApprovalAnswer, ApprovalDecision, BackgroundTaskId, CancelQueuedParams, ClaudeSessionId,
    Effort, Image, ItemBody, ItemId, McpRemoveParams, McpScope, McpServerParams, McpToggleParams,
    PendingId, PermissionMode, PlanAnswer, PlanDecision, Prompt, PromptParams, Question,
    QuestionAnswer, Request, SessionResumeParams, SetEffortParams, SetModeParams, SetModelParams,
    SlashCommand, StartParams, StopTaskParams,
};
use strum::VariantArray;

use super::app_state::{AgentEntry, AppState, Loadable};
use super::drafts::{Drafts, Palette, QuestionDraft};
use super::requests::{Outgoing, Purpose, history_request};
use super::transcript::HistoryState;
use super::ui_state::{
    Block, Dialog, FileView, NewAgentForm, NewAgentTab, PaletteOrigin, Popover, Switcher,
    ToolFilter, ViewMode,
};
use super::{export, file_edits};
use crate::format::{self, MODE_CHOICES, MODEL_CHOICES};

/// What starts a slash command in the composer.
pub const COMMAND_PREFIX: char = '/';

/// The commands the palette's Clear and Compact entries send to Claude as prompts.
const CLEAR_COMMAND: &str = "clear";
const COMPACT_COMMAND: &str = "compact";

const MEGABYTE: usize = 1024 * 1024;

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
    state.ui.unread.remove(&agent);
    state.ui.popover = None;
    state.ui.badge = None;
    state.ui.mcp.detail = None;
    state.ui.mcp.filter = ToolFilter::All;
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
                highlighted: 0,
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
fn commands_request(state: &AppState, agent: AgentId) -> Option<Outgoing> {
    let details = state.agents.get(&agent)?.details.loaded()?;
    details.commands.is_empty().then_some(Outgoing {
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
            | Self::ExportConversation => MenuSection::Context,
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
            | Self::ExportConversation => Vec::new(),
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
            drafts.draft_written();
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
                "Copied the conversation to the clipboard as Markdown, without the older messages the window has not loaded. Scroll up to load them, then export again."
            };
            state.ui.inform(note.to_owned());
            Picked {
                requests: Vec::new(),
                window: Some(WindowAction::Copy(markdown)),
            }
        }
        MenuAction::Model | MenuAction::Effort | MenuAction::Mode => Picked::default(),
    };
    state.ui.popover = None;
    picked
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
        | MenuAction::ExportConversation => Vec::new(),
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
        | MenuAction::ExportConversation => None,
    };
    requests.unwrap_or_default()
}

/// Left and Right on the highlighted setting: its previous or next choice. Nothing moves past the
/// first or the last.
pub fn palette_step(
    state: &AppState,
    drafts: &Drafts,
    agent: AgentId,
    step: Step,
) -> Vec<Outgoing> {
    let highlighted = open_palette_entries(state, drafts, agent)
        .get(drafts.palette.highlighted)
        .copied();
    let (Some(PaletteEntry::Action(action)), Some(entry)) = (highlighted, state.agents.get(&agent))
    else {
        return Vec::new();
    };
    let choices = setting_choices(&entry.summary, action);
    let direction = match step {
        Step::Previous => Direction::Up,
        Step::Next => Direction::Down,
    };
    let next = match choices.iter().position(|choice| choice.chosen) {
        Some(current) => {
            let next = moved(current, choices.len(), direction);
            if next == current {
                return Vec::new();
            }
            next
        }
        None => 0,
    };
    choose_setting(state, agent, action, next)
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
    drafts.draft_written();
    state.ui.popover = None;
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

/// Opens the file view on `path`, at `line` when a link names one. A file the agent changed opens
/// on its diff, unless a line is asked for. The text is read again, since it may have changed, and
/// the text of the file shown before is dropped: files can be large.
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
    entry.files.clear();
    entry.files.insert(path.clone(), Loadable::NotRequested);
    state
        .ui
        .file_views
        .insert(agent, FileView { path, line, mode });
    state.ui.popover = None;
    state.ui.badge = None;
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
    let running = state
        .agents
        .get(&agent)
        .is_some_and(|entry| entry.summary.status != AgentStatus::Exited);
    if state.ui.popover != Some(Popover::Usage) || !running {
        return Vec::new();
    }
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

pub fn toggle_switcher(state: &mut AppState) {
    let open = matches!(state.ui.dialog, Some(Dialog::Switcher(_)));
    state.ui.popover = None;
    state.ui.dialog = (!open).then(|| Dialog::Switcher(Switcher::default()));
}

/// Agents whose title or folder holds the query, in the sidebar's order.
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

pub fn switcher_query(state: &mut AppState, query: String) {
    if let Some(Dialog::Switcher(switcher)) = &mut state.ui.dialog {
        switcher.query = query;
        switcher.highlighted = 0;
    }
}

pub fn switcher_move(state: &mut AppState, direction: Direction) {
    let Some(Dialog::Switcher(switcher)) = &state.ui.dialog else {
        return;
    };
    let count = filtered_agents(state, &switcher.query).len();
    let highlighted = moved(switcher.highlighted, count, direction);
    if let Some(Dialog::Switcher(switcher)) = &mut state.ui.dialog {
        switcher.highlighted = highlighted;
    }
}

/// Opens the agent at `index` of the filtered list, or the highlighted one.
pub fn switcher_pick(state: &mut AppState, index: Option<usize>) {
    let Some(Dialog::Switcher(switcher)) = &state.ui.dialog else {
        return;
    };
    let index = index.unwrap_or(switcher.highlighted);
    let Some(agent) = filtered_agents(state, &switcher.query)
        .get(index)
        .map(|entry| entry.id())
    else {
        return;
    };
    state.ui.dialog = None;
    select_agent(state, agent);
}

/// Opens the new agent dialog on the last used folder. The past sessions are listed again, since
/// sessions end and start while the app runs. An open dialog stays as it is: its first message
/// field follows the keyboard, not the form, so a fresh form would not empty it.
pub fn open_new_agent(state: &mut AppState) {
    if matches!(state.ui.dialog, Some(Dialog::NewAgent(_))) {
        return;
    }
    let folder = state
        .recent_folders()
        .first()
        .map(|(folder, _)| folder.to_string_lossy().into_owned())
        .unwrap_or_default();
    if state.sessions != Loadable::Loading {
        state.sessions = Loadable::NotRequested;
    }
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::NewAgent(NewAgentForm {
        folder,
        ..NewAgentForm::default()
    }));
}

pub fn with_new_agent_form(state: &mut AppState, change: impl FnOnce(&mut NewAgentForm)) {
    if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog {
        change(form);
    }
}

pub fn start_agent(state: &mut AppState) -> Vec<Outgoing> {
    let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog else {
        return Vec::new();
    };
    let folder = form.folder.trim();
    if folder.is_empty() || form.starting || form.tab != NewAgentTab::Start {
        return Vec::new();
    }
    form.starting = true;
    let prompt = form.prompt.trim();
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
    use cockpit_protocol::{
        AgentDetails, AskedQuestion, CommandKind, HistoryPage, ImageMediaType, Method,
        QuestionOption, ResponseBody, TranscriptItem,
    };

    use super::*;
    use crate::state::app_state::ConnectionState;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::requests::{apply_response, needed_requests};
    use crate::state::ui_state::NoteKind;

    const AGENT: u32 = 1;

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
        drafts.palette = Palette {
            query: query.to_owned(),
            highlighted: 0,
        };
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
        let on_the_first_command = press(Direction::Down, 7);
        let at_the_bottom = press(Direction::Down, 5);
        let back_on_the_settings = press(Direction::Up, 3);

        assert_eq!(
            (
                up_at_the_top,
                on_the_first_command,
                at_the_bottom,
                back_on_the_settings
            ),
            (0, 7, 8, 5)
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
        assert!(part_note.is_some_and(|(_, text)| text.contains("without the older messages")));
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
            palette_step(&state, &drafts, agent(), step)
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
    fn a_setting_without_a_value_starts_at_its_first_choice() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        open_menu(&mut state, &mut drafts, "effort");

        let requests = palette_step(&state, &drafts, agent(), Step::Previous);
        let summary = &state.agents[&agent()].summary;

        assert_eq!(
            sent(&requests),
            [&Request::AgentSetEffort(SetEffortParams {
                agent: agent(),
                effort: Effort::Low,
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

    #[test]
    fn the_composer_takes_the_draft_only_when_the_app_changes_it() {
        let mut state = state(AgentStatus::Idle);
        let mut drafts = Drafts::default();
        with_commands(&mut state, vec![command("compact", "")]);

        type_text(&mut state, &mut drafts, "/co");
        let after_typing = drafts.composer_writes;
        let _ = palette_pick(&mut state, &mut drafts, agent(), None);
        let after_pick = drafts.composer_writes;
        send_prompt(&mut state, &mut drafts, agent(), None);
        let after_send = drafts.composer_writes;

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
                path: PathBuf::from("src/lib.rs"),
                line: Some(12),
                mode: ViewMode::WholeFile,
            })
        );
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
        toggle_switcher(&mut state);
        switcher_query(&mut state, "project".to_owned());

        switcher_move(&mut state, Direction::Down);
        switcher_move(&mut state, Direction::Down);
        switcher_move(&mut state, Direction::Down);
        switcher_pick(&mut state, None);

        assert_eq!(state.ui.selected, Some(AgentId::new(3)));
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn starting_an_agent_sends_the_form_once() {
        let mut state = state(AgentStatus::Idle);
        open_new_agent(&mut state);
        with_new_agent_form(&mut state, |form| {
            form.prompt = "Fix the flaky test".to_owned();
            form.mode = Some(PermissionMode::Plan);
        });

        let first = start_agent(&mut state);
        let second = start_agent(&mut state);

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
    fn opening_the_open_new_agent_dialog_keeps_its_form() {
        let mut state = state(AgentStatus::Idle);
        open_new_agent(&mut state);
        with_new_agent_form(&mut state, |form| {
            form.prompt = "Fix the flaky test".to_owned();
        });

        open_new_agent(&mut state);

        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::NewAgent(form)) if form.prompt == "Fix the flaky test"
        ));
    }

    #[test]
    fn each_opening_of_the_new_agent_dialog_lists_the_sessions_again() {
        let mut state = state(AgentStatus::Idle);
        open_new_agent(&mut state);
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

        open_new_agent(&mut state);
        let second = needed_requests(&mut state);

        let lists = |requests: &[Outgoing]| {
            requests
                .iter()
                .filter(|outgoing| outgoing.request == Request::SessionsList)
                .count()
        };
        assert_eq!((lists(&first), lists(&second)), (1, 1));
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
