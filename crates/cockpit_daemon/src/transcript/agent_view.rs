//! An agent as clients see it, built from the lines its `claude` writes.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Duration;

use claude_protocol::{
    AssistantMessage, CanUseTool, ContentBlock, ContextUsage as ClaudeContextUsage,
    ControlRequestId, ConversationReset, ImageBlock, ImageSource, IncomingControlRequest,
    IncomingRequest, InitializeResponse, InputLine, InterruptNotice, McpStatus, OpenWord,
    OutputLine, RateLimitInfo, ResetTrigger, ResultMessage, SessionState, SystemEvent, SystemInit,
    TaskNotification, TaskStarted, TaskUsage, TerminalReason, ToolInput, ToolName, ToolResultBlock,
    ToolUseBlock, ToolUseId, TypedCommand, Usage, UserMessage as ClaudeUserMessage,
};
use cockpit_protocol::{
    AgentDetails, AgentId, AgentStatus, AgentSummary, Answer, AnswerState, Approval,
    BackgroundTask, BackgroundTaskId, BackgroundTaskStatus, ClaudeSessionId, ContextUsage, CostUsd,
    Effort, Event, Image, ItemBody, ItemId, McpServer, PendingId, PermissionMode, Plan, Position,
    Prompt, Question, Timestamp, TokenUsage, ToolCall, ToolCallStatus, TranscriptItem, TurnEnd,
    UserMessage,
};
use serde_json::{Map, Value};
use strum::{EnumString, IntoStaticStr, VariantArray};

use crate::sessions;
use crate::transcript::past_transcript::LAST_PAST_ID;
use crate::transcript::{answers, details, notices, tools, words};

/// What the hub does after a line, beyond publishing the view's events.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FollowUp {
    /// The turn ended: ask for the context usage, and send the next queued prompt.
    TurnEnded,
    /// The account's plan usage may have changed.
    RateLimit(RateLimitInfo),
}

/// Whether a client's prompt goes to `claude` now or waits for the current turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptDelivery {
    Now,
    Queued,
}

/// Whether a process starts a new Claude session or resumes one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessSession {
    New,
    /// Claude Code restores the session's cost, and counts its total from there.
    Resumed,
}

/// The answer to a pending item, ready to send.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Answered {
    pub(crate) line: InputLine,
    /// The mode to switch to once Claude Code has the answer, after an approved plan.
    pub(crate) then_mode: Option<PermissionMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AnswerError {
    #[error("nothing waits for an answer under `{0}`")]
    NotFound(String),
    #[error("{0}")]
    Mismatch(String),
}

/// `system` subtypes the view reads that `claude_protocol` leaves untyped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
enum UntypedSystemSubtype {
    CompactBoundary,
    /// A local command's output, in a session file. On stdout it comes as a synthetic assistant
    /// message.
    LocalCommand,
    /// Claude Code named the session, after `/rename`.
    SessionTitleChanged,
}

const SUBTYPE_KEY: &str = "subtype";
/// The text of a `local_command` line.
const CONTENT_KEY: &str = "content";
/// The new name in a `session_title_changed` line.
const TITLE_KEY: &str = "title";
/// The model of an assistant message Claude Code writes itself, such as a slash command's output
/// or an API error.
const SYNTHETIC_MODEL: &str = "<synthetic>";
const COMPACTED_NOTICE: &str = "Conversation compacted";
/// Titles a skill's instructions, followed by the skill's command.
const SKILL_INSTRUCTIONS_TITLE: &str = "Skill instructions";
const SUMMARY_TITLE: &str = "Conversation summary";
/// Titles context Claude Code adds that no line before it explains.
const CONTEXT_TITLE: &str = "Context added by Claude Code";
const CLEARED_NOTICE: &str = "Conversation cleared";
/// A new conversation for a reason this daemon does not know.
const RESET_NOTICE: &str = "New conversation";
const QUEUED_CANCELLED_NOTICE: &str = "Queued message cancelled";
/// Where a session file shows that the user stopped a turn: it holds no turn end to say it.
const PAST_STOP_NOTICE: &str = "Stopped";
/// Prompts sent and not yet echoed back. Older ones are forgotten: Claude Code does not echo
/// every prompt, such as some slash commands.
const MAX_AWAITED_ECHOES: usize = 32;

/// What the next synthetic `user` line holds, as the line before it says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpectedContext {
    /// The instructions of the skill a `Skill` call just loaded, such as `review`.
    Skill(String),
    /// The summary that follows a compaction.
    Summary,
}

/// Where a `user` line comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineSource {
    /// `claude`'s stdout, which marks a prompt it echoes with `isReplay`.
    Process,
    /// A session file, which writes a prompt the user typed without it.
    SessionFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemChange {
    Appended(ItemId),
    Changed(ItemId),
}

/// A `can_use_tool` request that waits for a client's answer.
#[derive(Debug, Clone, PartialEq)]
struct Pending {
    item: ItemId,
    /// The request's input, which an answer that allows the call sends back.
    input: Map<String, Value>,
}

/// The transcript, the details and the summary of one agent.
///
/// Each method changes the view and records what changed; [`AgentView::take_events`] turns those
/// changes into the events to publish.
#[derive(Debug)]
pub(crate) struct AgentView {
    summary: AgentSummary,
    /// The summary of the last `AgentAdded` or `AgentUpdated`.
    published: Option<AgentSummary>,
    details: AgentDetails,
    details_changed: bool,
    items: Vec<TranscriptItem>,
    next_item: ItemId,
    changes: Vec<ItemChange>,
    tool_calls: HashMap<ToolUseId, ItemId>,
    pending: HashMap<PendingId, Pending>,
    /// From a prompt, or the start of a turn, until the turn's `result`.
    working: bool,
    exited: bool,
    /// The `total_cost_usd` of the current process so far: Claude Code counts from its start, or
    /// from the cost it restores with a resumed session.
    process_cost: f64,
    /// The part of the agent's cost that the process's `total_cost_usd` does not count: the
    /// earlier processes' cost after a new session, what Claude Code did not restore after a
    /// resumed one, and what came before a `/clear`.
    cost_before_process: f64,
    /// Until the process's first `result`, the cost a resumed session restores can still become
    /// the first turn's base.
    awaits_restored_cost: bool,
    /// Claude Code restates the session's saved name as it starts, before it answers
    /// `initialize`: that name must not replace one the agent got while it was stopped.
    takes_session_titles: bool,
    /// Set by a `Skill` call's result or a compaction, for the synthetic `user` line after it.
    expected_context: Option<ExpectedContext>,
    /// The name Claude Code restated as the process started, until it answers `initialize`.
    restated_title: Option<String>,
    awaited_echoes: VecDeque<String>,
    /// Calls per tool name, for the MCP servers' tool counts.
    tool_counts: HashMap<String, u32>,
    skills: BTreeSet<String>,
    prompt_cache_ttl: Duration,
}

impl AgentView {
    /// A new agent, whose process is starting.
    pub(crate) fn new(summary: AgentSummary, prompt_cache_ttl: Duration) -> Self {
        Self {
            summary,
            published: None,
            details: AgentDetails::default(),
            details_changed: false,
            items: Vec::new(),
            next_item: ItemId::new(1),
            changes: Vec::new(),
            tool_calls: HashMap::new(),
            pending: HashMap::new(),
            working: false,
            exited: false,
            process_cost: 0.0,
            cost_before_process: 0.0,
            awaits_restored_cost: false,
            takes_session_titles: false,
            restated_title: None,
            expected_context: None,
            awaited_echoes: VecDeque::new(),
            tool_counts: HashMap::new(),
            skills: BTreeSet::new(),
            prompt_cache_ttl,
        }
    }

    /// An agent read back from its event log, as the last events left it. Its process ended with
    /// the previous daemon: what waited is cancelled and what ran failed.
    pub(crate) fn restore(
        summary: AgentSummary,
        details: AgentDetails,
        items: Vec<TranscriptItem>,
        prompt_cache_ttl: Duration,
    ) -> Self {
        let next_item = items
            .last()
            .and_then(|item| item.id.next())
            .unwrap_or(ItemId::new(1));
        let mut view = Self::new(summary.clone(), prompt_cache_ttl);
        view.published = Some(summary);
        view.details = details;
        view.items = items;
        view.next_item = next_item;
        view.exited = true;
        view.cost_before_process = view.summary.cost.get();
        let leftovers: Vec<ItemId> = view
            .items
            .iter()
            .filter(|item| is_unfinished(&item.body))
            .map(|item| item.id)
            .collect();
        for id in leftovers {
            if let Some(item) = view.item_mut(id) {
                finish_leftover(&mut item.body);
            }
        }
        view.stop_background_tasks(None);
        view.summary.activity = None;
        // The log does not say when a turn the previous daemon cut off last did something: the
        // later of its start and the last activity logged stands for it.
        let turn_started = view.summary.turn_started_at.take();
        view.summary.last_activity_at = view.summary.last_activity_at.max(turn_started);
        view.refresh_status();
        view
    }

    /// A view that reads a session file's lines, for their items alone. `folder` is the
    /// session's, which tool summaries show paths relative to.
    pub(super) fn new_for_session_file(folder: PathBuf) -> Self {
        let summary = AgentSummary {
            id: AgentId::new(0),
            title: String::new(),
            folder,
            status: AgentStatus::Idle,
            activity: None,
            pending_count: 0,
            model: None,
            effort: None,
            effort_is_default: false,
            mode: PermissionMode::Default,
            task_progress: None,
            context_percent: None,
            cache_expires_at: None,
            turns: 0,
            cost: CostUsd::new(0.0),
            started_at: Timestamp::from_unix_millis(0),
            last_activity_at: None,
            turn_started_at: None,
            session: None,
            position: Position::default(),
        };
        Self::new(summary, Duration::ZERO)
    }

    /// Reads one line of a session file. Its lines read as on stdout, except a prompt the user
    /// typed, which has no `isReplay` there.
    pub(super) fn apply_session_line(&mut self, line: OutputLine) {
        match line {
            OutputLine::User(message) => self.apply_user(message, LineSource::SessionFile),
            OutputLine::System(SystemEvent::Other(value))
                if untyped_subtype(&value) == Some(UntypedSystemSubtype::LocalCommand) =>
            {
                let output = value
                    .get(CONTENT_KEY)
                    .and_then(Value::as_str)
                    .and_then(claude_protocol::local_command_output);
                if let Some(output) = output.filter(|output| !output.is_empty()) {
                    self.push_item(
                        None,
                        ItemBody::Notice {
                            text: output.to_owned(),
                            detail: None,
                        },
                    );
                }
            }
            line => {
                // A session file holds no `result` line, so nothing follows up.
                self.apply(line, Timestamp::from_unix_millis(0));
            }
        }
    }

    /// Forgets all but the newest `keep` items, and the changes no event will publish.
    pub(super) fn keep_newest(&mut self, keep: usize) {
        let older = self.items.len().saturating_sub(keep);
        self.items.drain(..older);
        self.changes.clear();
    }

    /// The items, as an ended process leaves them: what ran failed, what waited is cancelled.
    pub(super) fn into_finished_items(self) -> Vec<TranscriptItem> {
        let mut items = self.items;
        for item in &mut items {
            if is_unfinished(&item.body) {
                finish_leftover(&mut item.body);
            }
        }
        items
    }

    /// Shows a past session's items above the agent's own, which the agent numbers after
    /// [`LAST_PAST_ID`]. A past item whose id an own item already has is left out.
    pub(crate) fn show_past(&mut self, mut past: Vec<TranscriptItem>) {
        let own = std::mem::take(&mut self.items);
        if let Some(first) = own.first() {
            past.retain(|item| item.id < first.id);
        }
        past.extend(own);
        self.items = past;
        let first_own = ItemId::new(LAST_PAST_ID.saturating_add(1));
        self.next_item = self.next_item.max(first_own);
    }

    pub(crate) fn summary(&self) -> &AgentSummary {
        &self.summary
    }

    pub(crate) fn details(&self) -> &AgentDetails {
        &self.details
    }

    pub(crate) fn items(&self) -> &[TranscriptItem] {
        &self.items
    }

    /// Whether a prompt sent now would wait for the current turn.
    pub(crate) fn is_busy(&self) -> bool {
        self.working || !self.pending.is_empty()
    }

    /// Records that the summary as it is went out in an `AgentAdded`.
    pub(crate) fn mark_published(&mut self) {
        self.refresh_status();
        self.published = Some(self.summary.clone());
    }

    /// The events for everything that changed since the last call, in order.
    pub(crate) fn take_events(&mut self) -> Vec<Event> {
        self.refresh_status();
        let agent = self.summary.id;
        let mut order: Vec<ItemChange> = Vec::new();
        for change in self.changes.drain(..) {
            let id = change_item(change);
            if !order.iter().any(|seen| change_item(*seen) == id) {
                order.push(change);
            }
        }
        let mut events = Vec::with_capacity(order.len() + 2);
        for change in order {
            let Some(item) = find_item(&self.items, change_item(change)) else {
                continue;
            };
            let item = item.clone();
            events.push(match change {
                ItemChange::Appended(_) => Event::TranscriptAppended { agent, item },
                ItemChange::Changed(_) => Event::TranscriptChanged { agent, item },
            });
        }
        if self.details_changed {
            self.details_changed = false;
            events.push(Event::DetailsChanged {
                agent,
                details: self.details.clone(),
            });
        }
        if self.published.as_ref() != Some(&self.summary) {
            self.published = Some(self.summary.clone());
            events.push(Event::AgentUpdated(self.summary.clone()));
        }
        events
    }

    /// Reads one line from `claude`.
    pub(crate) fn apply(&mut self, line: OutputLine, now: Timestamp) -> Vec<FollowUp> {
        let mut follow_ups = Vec::new();
        match line {
            OutputLine::System(event) => self.apply_system(event, now),
            OutputLine::Assistant(message) => self.apply_assistant(message, now),
            OutputLine::User(message) => self.apply_user(message, LineSource::Process),
            OutputLine::Result(result) => {
                self.apply_result(&result, now);
                follow_ups.push(FollowUp::TurnEnded);
            }
            OutputLine::RateLimitEvent(event) => {
                follow_ups.push(FollowUp::RateLimit(event.rate_limit_info));
            }
            OutputLine::ControlRequest(request) => self.apply_control_request(request, now),
            OutputLine::ControlCancelRequest(cancel) => {
                self.cancel_pending(&PendingId::new(cancel.request_id.as_str().to_owned()));
            }
            OutputLine::ConversationReset(reset) => self.reset_conversation(&reset),
            OutputLine::StreamEvent(_)
            | OutputLine::ControlResponse(_)
            | OutputLine::Unknown(_) => {}
        }
        follow_ups
    }

    /// A prompt from a client. A queued one waits in the transcript until the hub sends it.
    pub(crate) fn add_prompt(
        &mut self,
        prompt: &Prompt,
        delivery: PromptDelivery,
        now: Timestamp,
    ) -> ItemId {
        let queued = delivery == PromptDelivery::Queued;
        let item = self.push_item(
            None,
            ItemBody::UserMessage(UserMessage {
                text: prompt.text.clone(),
                images: prompt.images.clone(),
                queued,
            }),
        );
        if !queued {
            self.expect_echo(prompt.text.clone());
            self.start_turn(now);
        }
        item
    }

    /// The hub sent a queued prompt.
    pub(crate) fn prompt_sent(&mut self, item: ItemId, now: Timestamp) {
        let mut sent_text = None;
        if let Some(found) = self.item_mut(item)
            && let ItemBody::UserMessage(message) = &mut found.body
        {
            message.queued = false;
            sent_text = Some(message.text.clone());
        }
        if let Some(text) = sent_text {
            self.expect_echo(text);
        }
        self.start_turn(now);
    }

    /// A prompt that shows no item, such as `/effort high`, whose echo must not show either.
    pub(crate) fn expect_echo(&mut self, text: String) {
        if self.awaited_echoes.len() == MAX_AWAITED_ECHOES {
            self.awaited_echoes.pop_front();
        }
        self.awaited_echoes.push_back(text);
    }

    /// A queued prompt that will not be sent. Its item stays, so later ids keep their order.
    pub(crate) fn cancel_queued(&mut self, item: ItemId) {
        if let Some(found) = self.item_mut(item) {
            found.body = ItemBody::Notice {
                text: QUEUED_CANCELLED_NOTICE.to_owned(),
                detail: None,
            };
        }
    }

    /// Answers a pending approval, question or plan, and marks its item answered.
    pub(crate) fn answer(
        &mut self,
        pending: &PendingId,
        answer: Answer,
    ) -> Result<Answered, AnswerError> {
        let not_found = || AnswerError::NotFound(pending.as_str().to_owned());
        let waiting = self.pending.get(pending).ok_or_else(not_found)?;
        let item_id = waiting.item;
        let input = waiting.input.clone();
        let body = find_item(&self.items, item_id)
            .map(|item| item.body.clone())
            .ok_or_else(not_found)?;
        let (decision, then_mode, body) = match (body, answer) {
            (ItemBody::Approval(approval), Answer::Approval(answer)) => {
                let decision = answers::approval(input, &answer);
                let body = ItemBody::Approval(Approval {
                    answer: AnswerState::Answered(answer),
                    ..approval
                });
                (decision, None, body)
            }
            (ItemBody::Question(question), Answer::Question { answers }) => {
                if answers.len() != question.questions.len() {
                    return Err(AnswerError::Mismatch(format!(
                        "{} questions wait for an answer, the request answers {}",
                        question.questions.len(),
                        answers.len()
                    )));
                }
                let decision = answers::questions(input, &question.questions, &answers);
                let body = ItemBody::Question(Question {
                    answer: AnswerState::Answered(answers),
                    ..question
                });
                (decision, None, body)
            }
            (ItemBody::Plan(plan), Answer::Plan(answer)) => {
                let (decision, then_mode) = answers::plan(input, &answer);
                let body = ItemBody::Plan(Plan {
                    answer: AnswerState::Answered(answer),
                    ..plan
                });
                (decision, then_mode, body)
            }
            (_, _) => {
                return Err(AnswerError::Mismatch(
                    "the answer's kind does not match what waits".to_owned(),
                ));
            }
        };
        self.pending.remove(pending);
        if let Some(item) = self.item_mut(item_id) {
            item.body = body;
        }
        if self.pending.is_empty() {
            self.summary.activity = None;
        }
        Ok(Answered {
            line: InputLine::permission(
                ControlRequestId::new(pending.as_str().to_owned()),
                decision,
            ),
            then_mode,
        })
    }

    /// A new `claude` process runs for the agent.
    pub(crate) fn process_started(&mut self, session: ProcessSession) {
        self.exited = false;
        self.end_work();
        self.cost_before_process = match session {
            ProcessSession::New => self.summary.cost.get(),
            ProcessSession::Resumed => 0.0,
        };
        self.process_cost = 0.0;
        self.awaits_restored_cost = session == ProcessSession::Resumed;
        self.takes_session_titles = false;
        self.restated_title = None;
        self.expected_context = None;
        self.pending.clear();
        self.tool_calls.clear();
    }

    /// The process answered `initialize`: from now on, a name Claude Code gives the session, as
    /// after a `/rename`, renames the agent. True when the name Claude Code restated for the
    /// session as it started is not the agent's, as after a rename made while the agent was
    /// stopped: the session needs a `/rename`.
    #[must_use]
    pub(crate) fn initialized(&mut self) -> bool {
        self.takes_session_titles = true;
        self.restated_title
            .take()
            .is_some_and(|restated| restated != self.summary.title)
    }

    /// The cost Claude Code restored with a resumed session, read from the session file. It is
    /// the base of the first turn's cost, unless a `result` came first: the first turn's cost then
    /// counts from 0.
    ///
    /// The agent's cost keeps what the agent counted, which can be more than Claude Code restores:
    /// a process killed with the daemon writes no `cost-state`, and a `/clear` starts a session
    /// whose cost leaves out the one before. The agent's total can then read higher than Claude
    /// Code's own `/cost`.
    pub(crate) fn set_restored_cost(&mut self, cost: CostUsd) {
        if !self.awaits_restored_cost {
            return;
        }
        self.awaits_restored_cost = false;
        self.process_cost = cost.get();
        self.cost_before_process = (self.summary.cost.get() - cost.get()).max(0.0);
        self.summary.cost = CostUsd::new(self.cost_before_process + cost.get());
    }

    /// The `claude` process ended: what waited is cancelled, what ran failed.
    pub(crate) fn process_exited(&mut self, notice: String, now: Timestamp) {
        self.exited = true;
        self.end_work();
        self.summary.last_activity_at = Some(now);
        self.cancel_all_pending();
        self.fail_running_calls();
        self.stop_background_tasks(Some(now));
        self.summary.activity = None;
        self.push_item(
            None,
            ItemBody::Notice {
                text: notice,
                detail: None,
            },
        );
    }

    pub(crate) fn set_commands(&mut self, response: &InitializeResponse) {
        self.details.commands = details::commands(response, &self.skills);
        self.details_changed = true;
    }

    pub(crate) fn set_context(&mut self, usage: &ClaudeContextUsage) -> ContextUsage {
        let usage = details::context_usage(usage);
        self.summary.context_percent = Some(usage.percent);
        self.details.context = Some(usage.clone());
        self.details_changed = true;
        usage
    }

    pub(crate) fn set_mcp_status(&mut self, status: &McpStatus) -> Vec<McpServer> {
        let servers: Vec<McpServer> = status
            .mcp_servers
            .iter()
            .map(|server| details::mcp_server(server, &self.tool_counts))
            .collect();
        self.details.mcp_servers = servers.clone();
        self.details_changed = true;
        servers
    }

    pub(crate) fn set_model(&mut self, model: String) {
        self.summary.model = Some(model);
    }

    pub(crate) fn set_mode(&mut self, mode: PermissionMode) {
        self.summary.mode = mode;
    }

    pub(crate) fn set_effort(&mut self, effort: Effort) {
        self.summary.effort = Some(effort);
        self.summary.effort_is_default = false;
    }

    /// The effort Claude Code applies by itself to an agent that was given none, from its
    /// settings or the model's default; `None` when it applies none, or does not say.
    pub(crate) fn set_default_effort(&mut self, effort: Option<Effort>) {
        self.summary.effort = effort;
        self.summary.effort_is_default = effort.is_some();
    }

    pub(crate) fn set_title(&mut self, title: String) {
        self.summary.title = title;
    }

    pub(crate) fn set_position(&mut self, position: Position) {
        self.summary.position = position;
    }

    fn apply_system(&mut self, event: SystemEvent, now: Timestamp) {
        match event {
            SystemEvent::Init(init) => self.apply_init(init, now),
            SystemEvent::TaskStarted(task) => self.task_started(task, now),
            SystemEvent::TaskProgress(progress) => {
                let id = BackgroundTaskId::new(progress.task_id.as_str().to_owned());
                if let Some(task) = self.background_task_mut(&id) {
                    if !progress.description.is_empty() {
                        task.description = progress.description;
                    }
                    if let Some(usage) = progress.usage {
                        task.tool_calls = usage.tool_uses;
                    }
                    self.details_changed = true;
                }
            }
            SystemEvent::TaskNotification(notification) => self.task_notified(notification, now),
            SystemEvent::TaskUpdated(updated) => {
                if let Some(OpenWord::Known(status)) = updated.patch.status
                    && let Some(status) = words::ended_task_status(status)
                {
                    let id = BackgroundTaskId::new(updated.task_id.as_str().to_owned());
                    self.end_background_task(&id, status, None, now);
                }
            }
            SystemEvent::SessionStateChanged(changed) => match changed.state {
                OpenWord::Known(SessionState::Running) => self.begin_work(now),
                OpenWord::Known(SessionState::Idle) => self.end_work(),
                OpenWord::Known(SessionState::RequiresAction) | OpenWord::Unknown(_) => {}
            },
            SystemEvent::ApiRetry(retry) => {
                let status = retry
                    .error_status
                    .map_or_else(String::new, |status| format!(" {status}"));
                self.summary.activity = Some(format!("Retrying after API error{status}"));
            }
            SystemEvent::Other(value) => match untyped_subtype(&value) {
                Some(UntypedSystemSubtype::CompactBoundary) => {
                    self.push_item(
                        None,
                        ItemBody::Notice {
                            text: COMPACTED_NOTICE.to_owned(),
                            detail: None,
                        },
                    );
                    self.expected_context = Some(ExpectedContext::Summary);
                }
                Some(UntypedSystemSubtype::SessionTitleChanged) => {
                    let title = value.get(TITLE_KEY).and_then(Value::as_str).map(str::trim);
                    if let Some(title) = title.filter(|title| !title.is_empty()) {
                        let title = sessions::shorten(title);
                        if self.takes_session_titles {
                            self.summary.title = title;
                        } else {
                            self.restated_title = Some(title);
                        }
                    }
                }
                Some(UntypedSystemSubtype::LocalCommand) | None => {}
            },
            SystemEvent::ThinkingTokens(_)
            | SystemEvent::HookStarted(_)
            | SystemEvent::HookProgress(_)
            | SystemEvent::HookResponse(_) => {}
        }
    }

    /// Claude Code starts a new conversation, as `/clear` does. Its total cost counts from 0
    /// again, so the agent's cost so far becomes the base, even when a resumed session's restored
    /// cost was not read yet.
    fn reset_conversation(&mut self, reset: &ConversationReset) {
        self.cost_before_process = self.summary.cost.get();
        self.process_cost = 0.0;
        self.awaits_restored_cost = false;
        let text = match reset.trigger {
            Some(OpenWord::Known(ResetTrigger::Clear)) => CLEARED_NOTICE,
            Some(OpenWord::Unknown(_)) | None => RESET_NOTICE,
        };
        self.push_item(
            None,
            ItemBody::Notice {
                text: text.to_owned(),
                detail: None,
            },
        );
    }

    /// `system/init` starts each turn and says what the session uses.
    fn apply_init(&mut self, init: SystemInit, now: Timestamp) {
        self.begin_work(now);
        self.summary.session = Some(ClaudeSessionId::new(init.session_id.as_str().to_owned()));
        if let Some(model) = init.model {
            self.summary.model = Some(model);
        }
        if let Some(OpenWord::Known(mode)) = init.permission_mode {
            self.summary.mode = words::mode_to_cockpit(mode);
        }
        let commands_before = self.details.commands.clone();
        let servers_before = self.details.mcp_servers.clone();
        self.skills = init.skills.into_iter().collect();
        if self.details.commands.is_empty() {
            self.details.commands =
                details::commands_from_names(&init.slash_commands, &self.skills);
        } else {
            details::mark_skills(&mut self.details.commands, &self.skills);
        }
        details::merge_init_servers(&mut self.details.mcp_servers, &init.mcp_servers);
        if commands_before != self.details.commands || servers_before != self.details.mcp_servers {
            self.details_changed = true;
        }
    }

    fn apply_assistant(&mut self, message: AssistantMessage, now: Timestamp) {
        self.expected_context = None;
        let parent = self.parent_item(message.parent_tool_use_id.as_ref());
        let synthetic = message.message.model.as_deref() == Some(SYNTHETIC_MODEL);
        if message.parent_tool_use_id.is_none() {
            self.begin_work(now);
            if !synthetic {
                self.summary.cache_expires_at = Some(add(now, self.prompt_cache_ttl));
            }
        }
        for block in message.message.content {
            match block {
                ContentBlock::Text(text) if synthetic && !text.text.trim().is_empty() => {
                    self.push_item(
                        parent,
                        ItemBody::Notice {
                            text: text.text,
                            detail: None,
                        },
                    );
                }
                ContentBlock::Text(text) if !text.text.trim().is_empty() => {
                    self.push_item(
                        parent,
                        ItemBody::AssistantText {
                            markdown: text.text,
                        },
                    );
                }
                ContentBlock::Thinking(thinking) if !thinking.thinking.trim().is_empty() => {
                    self.push_item(
                        parent,
                        ItemBody::Thinking {
                            text: thinking.thinking,
                        },
                    );
                }
                ContentBlock::ToolUse(tool_use) => self.start_tool_call(tool_use, parent),
                ContentBlock::Text(_)
                | ContentBlock::Thinking(_)
                | ContentBlock::ToolResult(_)
                | ContentBlock::Image(_)
                | ContentBlock::Unknown(_) => {}
            }
        }
    }

    fn start_tool_call(&mut self, tool_use: ToolUseBlock, parent: Option<ItemId>) {
        let ToolUseBlock { id, name, input } = tool_use;
        // A malformed input still shows, as an unknown tool's would.
        let parsed = ToolInput::parse(&name, &input).unwrap_or(ToolInput::Unknown);
        let summary = tools::summary(&input, &self.summary.folder);
        let call = ToolCall {
            tool: name.clone(),
            summary: summary.clone(),
            input: Value::Object(input),
            status: ToolCallStatus::Running,
            output: None,
            exit_code: None,
            edit: tools::file_edit(&parsed).map(tools::without_large_texts),
        };
        let body = if tools::is_subagent(&name) {
            ItemBody::Subagent(call)
        } else {
            ItemBody::ToolCall(call)
        };
        let item = self.push_item(parent, body);
        self.tool_calls.insert(id, item);
        let count = self.tool_counts.entry(name.clone()).or_default();
        *count = count.saturating_add(1);
        if details::is_mcp_tool(&name) {
            details::count_mcp_calls(&mut self.details.mcp_servers, &self.tool_counts);
            self.details_changed = true;
        }
        if let ToolInput::TodoWrite(todo_write) = &parsed {
            self.details.tasks = details::task_list(&todo_write.todos);
            self.summary.task_progress = details::task_progress(&self.details.tasks);
            self.summary.activity = details::task_in_progress(&self.details.tasks);
            self.details_changed = true;
        } else if summary.is_empty() {
            self.summary.activity = Some(name);
        } else {
            self.summary.activity = Some(format!("{name}: {summary}"));
        }
    }

    fn apply_user(&mut self, message: ClaudeUserMessage, source: LineSource) {
        let parent = self.parent_item(message.parent_tool_use_id.as_ref());
        let ClaudeUserMessage {
            message: body,
            is_replay,
            origin,
            is_synthetic,
            tool_use_result,
            ..
        } = message;
        let mut texts: Vec<String> = Vec::new();
        let mut images: Vec<Image> = Vec::new();
        for block in body.content {
            match block {
                ContentBlock::ToolResult(result) => {
                    self.finish_tool_call(&result, tool_use_result.as_ref());
                }
                ContentBlock::Text(text) => texts.push(text.text),
                ContentBlock::Image(image) => images.extend(cockpit_image(&image)),
                ContentBlock::Thinking(_) | ContentBlock::ToolUse(_) | ContentBlock::Unknown(_) => {
                }
            }
        }
        if texts.is_empty() && images.is_empty() {
            return;
        }
        let text = texts.join("\n");
        if let Some(notices) = notices::notices(origin.as_ref(), &text) {
            // A message another source wrote whole, such as a subagent's report, unfolds to it.
            let detail = (is_synthetic && notices.len() == 1).then(|| text.clone());
            for notice in notices {
                self.push_item(
                    parent,
                    ItemBody::Notice {
                        text: notice,
                        detail: detail.clone(),
                    },
                );
            }
            return;
        }
        if is_synthetic {
            let expected = self.expected_context.take();
            // A session file marks other context the same way, such as reminders: it stays hidden.
            if expected.is_some() || source == LineSource::Process {
                let body = ItemBody::Notice {
                    text: context_title(expected),
                    detail: Some(text),
                };
                self.push_item(parent, body);
            }
            return;
        }
        if let Some(output) = claude_protocol::local_command_output(&text) {
            // Shown as `/effort`'s output is, which comes as a synthetic assistant message.
            if !output.is_empty() {
                self.push_item(
                    parent,
                    ItemBody::Notice {
                        text: output.to_owned(),
                        detail: None,
                    },
                );
            }
            return;
        }
        // A slash command reads as the user typed it, which is also the prompt its echo matches.
        let text = match TypedCommand::parse(&text) {
            Some(command) => command.to_string(),
            None => text,
        };
        let interrupt = text.trim().parse::<InterruptNotice>().is_ok();
        if source == LineSource::SessionFile {
            let body = if interrupt {
                ItemBody::Notice {
                    text: PAST_STOP_NOTICE.to_owned(),
                    detail: None,
                }
            } else {
                user_message(text, images)
            };
            self.push_item(parent, body);
            return;
        }
        // The turn end says the user stopped the turn.
        if !is_replay && interrupt {
            return;
        }
        if is_replay {
            if self.take_echo(&text) {
                return;
            }
            self.push_item(parent, user_message(text, images));
        } else if parent.is_some() {
            // A subagent's prompt, which the agent wrote.
            self.push_item(parent, user_message(text, images));
        } else {
            // Claude Code's own lines.
            self.push_item(None, ItemBody::Notice { text, detail: None });
        }
    }

    fn finish_tool_call(&mut self, result: &ToolResultBlock, tool_use_result: Option<&Value>) {
        let Some(&item_id) = self.tool_calls.get(&result.tool_use_id) else {
            return;
        };
        let skill = find_item(&self.items, item_id).and_then(|item| match &item.body {
            ItemBody::ToolCall(call) => tools::loaded_skill(&call.tool, &call.input),
            ItemBody::UserMessage(_)
            | ItemBody::AssistantText { .. }
            | ItemBody::Thinking { .. }
            | ItemBody::Subagent(_)
            | ItemBody::Approval(_)
            | ItemBody::Question(_)
            | ItemBody::Plan(_)
            | ItemBody::TurnEnd(_)
            | ItemBody::Notice { .. } => None,
        });
        if let Some(skill) = skill.filter(|_| !result.is_error) {
            self.expected_context = Some(ExpectedContext::Skill(skill));
        }
        let output = tools::result_text(&result.content);
        let Some(item) = self.item_mut(item_id) else {
            return;
        };
        let (ItemBody::ToolCall(call) | ItemBody::Subagent(call)) = &mut item.body else {
            // A question or a plan: its item shows the answer.
            return;
        };
        if result.is_error {
            call.status = ToolCallStatus::Failed;
            if call.tool.parse::<ToolName>() == Ok(ToolName::Bash) {
                call.exit_code = tools::exit_code(&output);
            }
            call.output = Some(output);
            return;
        }
        call.status = ToolCallStatus::Done;
        call.output = Some(output);
        if call.edit.is_none() {
            return;
        }
        // The item's edit may have lost its texts to the size limit: the input still holds them.
        let Some(mut edit) = tools::input_edit(&call.tool, &call.input) else {
            return;
        };
        if edit.old_text.is_empty()
            && let Some(original) = tools::original_file(tool_use_result)
        {
            edit.old_text = original;
        }
        let lines = tools::changed_lines(&edit.old_text, &edit.new_text);
        let path = edit.path.clone();
        call.edit = Some(tools::without_large_texts(edit));
        details::add_changed_file(&mut self.details.changed_files, &path, lines);
        self.details_changed = true;
    }

    fn apply_result(&mut self, result: &ResultMessage, now: Timestamp) {
        let total = result.total_cost_usd.unwrap_or(self.process_cost);
        if self.awaits_restored_cost {
            // The restored cost is unknown: the total keeps what the agent counted.
            self.cost_before_process = (self.summary.cost.get() - total).max(0.0);
        }
        self.awaits_restored_cost = false;
        let turn_cost = (total - self.process_cost).max(0.0);
        self.process_cost = total;
        self.summary.cost = CostUsd::new(self.cost_before_process + total);
        // The result says it rather than the daemon's own `interrupt`, which may arrive after the
        // turn ended.
        let interrupted = matches!(
            result.terminal_reason,
            Some(OpenWord::Known(
                TerminalReason::AbortedStreaming | TerminalReason::AbortedTools
            ))
        );
        if result.is_error || interrupted {
            self.fail_running_calls();
        }
        self.cancel_all_pending();
        self.end_work();
        self.summary.activity = None;
        // A slash command Claude Code runs itself, such as `/effort`, makes no API call: it is not
        // a turn of the conversation.
        if result.num_turns == 0 && !result.is_error && !interrupted {
            return;
        }
        self.summary.last_activity_at = Some(now);
        self.summary.turns = self.summary.turns.saturating_add(1);
        self.push_item(
            None,
            ItemBody::TurnEnd(TurnEnd {
                duration: result.duration.unwrap_or_default(),
                cost: CostUsd::new(turn_cost),
                usage: token_usage(result.usage),
                error: turn_error(result, interrupted),
                stopped: interrupted,
            }),
        );
    }

    fn apply_control_request(&mut self, request: IncomingControlRequest, now: Timestamp) {
        // The process task answers the other subtypes itself.
        if let IncomingRequest::CanUseTool(can_use_tool) = request.request {
            self.ask(&request.request_id, can_use_tool, now);
        }
    }

    /// Claude Code asks whether a tool call may run.
    fn ask(&mut self, request_id: &ControlRequestId, request: CanUseTool, now: Timestamp) {
        let pending = PendingId::new(request_id.as_str().to_owned());
        let tool_item = self.tool_calls.get(&request.tool_use_id).copied();
        let parent = tool_item
            .and_then(|id| find_item(&self.items, id))
            .and_then(|item| item.parent);
        let parsed =
            ToolInput::parse(&request.tool_name, &request.input).unwrap_or(ToolInput::Unknown);
        let body = match &parsed {
            ToolInput::AskUserQuestion(_) => ItemBody::Question(Question {
                pending: pending.clone(),
                questions: answers::asked_questions(&parsed).unwrap_or_default(),
                answer: AnswerState::Waiting,
            }),
            ToolInput::ExitPlanMode(plan) => ItemBody::Plan(Plan {
                pending: pending.clone(),
                markdown: plan.plan.clone().unwrap_or_default(),
                answer: AnswerState::Waiting,
            }),
            ToolInput::TodoWrite(_)
            | ToolInput::Edit(_)
            | ToolInput::Write(_)
            | ToolInput::Bash(_)
            | ToolInput::Read(_)
            | ToolInput::Skill(_)
            | ToolInput::Unknown => ItemBody::Approval(Approval {
                pending: pending.clone(),
                tool: request.tool_name.clone(),
                summary: tools::summary(&request.input, &self.summary.folder),
                input: Value::Object(request.input.clone()),
                edit: tools::file_edit(&parsed).map(tools::without_large_texts),
                reason: request.decision_reason.clone().or_else(|| {
                    request
                        .blocked_path
                        .as_ref()
                        .map(|path| format!("{} is outside the allowed folders", path.display()))
                }),
                answer: AnswerState::Waiting,
                tool_call: tool_item,
            }),
        };
        // A question or a plan takes the place of its tool call's item, which says nothing more.
        let replaces_call = matches!(body, ItemBody::Question(_) | ItemBody::Plan(_));
        let item = match tool_item {
            Some(id) if replaces_call => {
                if let Some(item) = self.item_mut(id) {
                    item.body = body;
                }
                id
            }
            Some(_) | None => self.push_item(parent, body),
        };
        self.summary.activity = Some(format!("Waiting for you: {}", request.tool_name));
        if self.pending.is_empty() {
            self.summary.last_activity_at = Some(now);
        }
        self.pending.insert(
            pending,
            Pending {
                item,
                input: request.input,
            },
        );
    }

    fn cancel_pending(&mut self, pending: &PendingId) {
        let Some(waiting) = self.pending.remove(pending) else {
            return;
        };
        if let Some(item) = self.item_mut(waiting.item) {
            cancel_answer(&mut item.body);
        }
    }

    fn cancel_all_pending(&mut self) {
        let pending: Vec<PendingId> = self.pending.keys().cloned().collect();
        for id in pending {
            self.cancel_pending(&id);
        }
    }

    fn fail_running_calls(&mut self) {
        let running: Vec<ItemId> = self
            .items
            .iter()
            .filter(|item| is_running_call(&item.body))
            .map(|item| item.id)
            .collect();
        for id in running {
            if let Some(item) = self.item_mut(id)
                && let ItemBody::ToolCall(call) | ItemBody::Subagent(call) = &mut item.body
            {
                call.status = ToolCallStatus::Failed;
            }
        }
    }

    fn task_started(&mut self, task: TaskStarted, now: Timestamp) {
        let id = BackgroundTaskId::new(task.task_id.as_str().to_owned());
        self.details
            .background_tasks
            .retain(|existing| existing.id != id);
        self.details.background_tasks.push(BackgroundTask {
            id,
            kind: details::background_kind(task.task_type.as_deref()),
            description: task.description,
            status: BackgroundTaskStatus::Running,
            started_at: now,
            ended_at: None,
            tool_calls: 0,
        });
        details::prune_background_tasks(&mut self.details.background_tasks);
        self.details_changed = true;
    }

    fn task_notified(&mut self, notification: TaskNotification, now: Timestamp) {
        // A notification reports the end of a task: completed when it does not say otherwise.
        let status = match notification.status {
            Some(OpenWord::Known(status)) => {
                words::ended_task_status(status).unwrap_or(BackgroundTaskStatus::Done)
            }
            Some(OpenWord::Unknown(_)) | None => BackgroundTaskStatus::Done,
        };
        let id = BackgroundTaskId::new(notification.task_id.as_str().to_owned());
        self.end_background_task(&id, status, notification.usage, now);
    }

    fn end_background_task(
        &mut self,
        id: &BackgroundTaskId,
        status: BackgroundTaskStatus,
        usage: Option<TaskUsage>,
        now: Timestamp,
    ) {
        let Some(task) = self.background_task_mut(id) else {
            return;
        };
        task.status = status;
        task.ended_at = Some(now);
        if let Some(usage) = usage {
            task.tool_calls = usage.tool_uses;
        }
        self.details_changed = true;
    }

    /// Marks the running background tasks stopped, at `now` when known.
    fn stop_background_tasks(&mut self, now: Option<Timestamp>) {
        for task in &mut self.details.background_tasks {
            if task.status == BackgroundTaskStatus::Running {
                task.status = BackgroundTaskStatus::Stopped;
                task.ended_at = now.or(task.ended_at);
                self.details_changed = true;
            }
        }
    }

    fn background_task_mut(&mut self, id: &BackgroundTaskId) -> Option<&mut BackgroundTask> {
        self.details
            .background_tasks
            .iter_mut()
            .find(|task| &task.id == id)
    }

    fn take_echo(&mut self, text: &str) -> bool {
        match self
            .awaited_echoes
            .iter()
            .position(|awaited| awaited == text)
        {
            Some(index) => {
                self.awaited_echoes.remove(index);
                true
            }
            None => false,
        }
    }

    /// The item of the subagent call a line runs under.
    fn parent_item(&self, parent_tool_use: Option<&ToolUseId>) -> Option<ItemId> {
        self.tool_calls.get(parent_tool_use?).copied()
    }

    fn push_item(&mut self, parent: Option<ItemId>, body: ItemBody) -> ItemId {
        let id = self.next_item;
        // Past `u32::MAX` items the last id repeats; an agent never gets near it.
        self.next_item = id.next().unwrap_or(id);
        self.items.push(TranscriptItem { id, parent, body });
        self.changes.push(ItemChange::Appended(id));
        id
    }

    /// The item, recorded as changed.
    fn item_mut(&mut self, id: ItemId) -> Option<&mut TranscriptItem> {
        let index = self.items.binary_search_by_key(&id, |item| item.id).ok()?;
        self.changes.push(ItemChange::Changed(id));
        self.items.get_mut(index)
    }

    /// A prompt went to `claude`: a new turn starts now, even when the last one had not ended.
    fn start_turn(&mut self, now: Timestamp) {
        self.working = true;
        self.summary.turn_started_at = Some(now);
        self.summary.last_activity_at = Some(now);
    }

    /// A line shows that a turn runs, which may be one Claude Code started without a prompt.
    fn begin_work(&mut self, now: Timestamp) {
        self.working = true;
        if self.summary.turn_started_at.is_none() {
            self.summary.turn_started_at = Some(now);
        }
    }

    fn end_work(&mut self) {
        self.working = false;
        self.summary.turn_started_at = None;
    }

    fn refresh_status(&mut self) {
        let pending = self.pending.len();
        self.summary.pending_count = u16::try_from(pending).unwrap_or(u16::MAX);
        self.summary.status = if self.exited {
            AgentStatus::Exited
        } else if pending > 0 {
            AgentStatus::NeedsYou
        } else if self.working {
            AgentStatus::Working
        } else {
            AgentStatus::Idle
        };
    }
}

/// The title of context Claude Code adds, such as `Skill instructions: /review`.
fn context_title(expected: Option<ExpectedContext>) -> String {
    match expected {
        Some(ExpectedContext::Skill(skill)) => format!("{SKILL_INSTRUCTIONS_TITLE}: /{skill}"),
        Some(ExpectedContext::Summary) => SUMMARY_TITLE.to_owned(),
        None => CONTEXT_TITLE.to_owned(),
    }
}

fn untyped_subtype(value: &Value) -> Option<UntypedSystemSubtype> {
    value.get(SUBTYPE_KEY)?.as_str()?.parse().ok()
}

fn change_item(change: ItemChange) -> ItemId {
    match change {
        ItemChange::Appended(id) | ItemChange::Changed(id) => id,
    }
}

fn find_item(items: &[TranscriptItem], id: ItemId) -> Option<&TranscriptItem> {
    let index = items.binary_search_by_key(&id, |item| item.id).ok()?;
    items.get(index)
}

fn user_message(text: String, images: Vec<Image>) -> ItemBody {
    ItemBody::UserMessage(UserMessage {
        text,
        images,
        queued: false,
    })
}

fn cockpit_image(image: &ImageBlock) -> Option<Image> {
    let ImageSource::Base64(base64) = &image.source else {
        return None;
    };
    Some(Image {
        media_type: words::media_type_from_word(&base64.media_type)?,
        data: base64.data.clone(),
    })
}

fn is_running_call(body: &ItemBody) -> bool {
    matches!(
        body,
        ItemBody::ToolCall(ToolCall {
            status: ToolCallStatus::Running,
            ..
        }) | ItemBody::Subagent(ToolCall {
            status: ToolCallStatus::Running,
            ..
        })
    )
}

fn is_unfinished(body: &ItemBody) -> bool {
    is_running_call(body)
        || matches!(
            body,
            ItemBody::Approval(Approval {
                answer: AnswerState::Waiting,
                ..
            }) | ItemBody::Question(Question {
                answer: AnswerState::Waiting,
                ..
            }) | ItemBody::Plan(Plan {
                answer: AnswerState::Waiting,
                ..
            }) | ItemBody::UserMessage(UserMessage { queued: true, .. })
        )
}

/// What a previous process left unfinished: a waiting answer is cancelled, a running call failed
/// and a queued prompt was never sent.
fn finish_leftover(body: &mut ItemBody) {
    match body {
        ItemBody::ToolCall(call) | ItemBody::Subagent(call) => {
            call.status = ToolCallStatus::Failed;
        }
        ItemBody::UserMessage(_) => {
            *body = ItemBody::Notice {
                text: QUEUED_CANCELLED_NOTICE.to_owned(),
                detail: None,
            };
        }
        ItemBody::Approval(_) | ItemBody::Question(_) | ItemBody::Plan(_) => cancel_answer(body),
        ItemBody::AssistantText { .. }
        | ItemBody::Thinking { .. }
        | ItemBody::TurnEnd(_)
        | ItemBody::Notice { .. } => {}
    }
}

fn cancel_answer(body: &mut ItemBody) {
    match body {
        ItemBody::Approval(approval) => approval.answer = AnswerState::Cancelled,
        ItemBody::Question(question) => question.answer = AnswerState::Cancelled,
        ItemBody::Plan(plan) => plan.answer = AnswerState::Cancelled,
        ItemBody::UserMessage(_)
        | ItemBody::AssistantText { .. }
        | ItemBody::Thinking { .. }
        | ItemBody::ToolCall(_)
        | ItemBody::Subagent(_)
        | ItemBody::TurnEnd(_)
        | ItemBody::Notice { .. } => {}
    }
}

fn token_usage(usage: Option<Usage>) -> TokenUsage {
    let usage = usage.unwrap_or_default();
    TokenUsage {
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_creation: usage.cache_creation_input_tokens,
        cache_read: usage.cache_read_input_tokens,
    }
}

/// Why the turn failed; `None` when it succeeded or the user stopped it.
fn turn_error(result: &ResultMessage, interrupted: bool) -> Option<String> {
    if interrupted || !result.is_error {
        return None;
    }
    if !result.errors.is_empty() {
        return Some(result.errors.join("; "));
    }
    if let Some(text) = result.result.as_ref().filter(|text| !text.is_empty()) {
        return Some(text.clone());
    }
    Some(result.subtype.as_str().to_owned())
}

fn add(time: Timestamp, duration: Duration) -> Timestamp {
    let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    Timestamp::from_unix_millis(time.unix_millis().saturating_add(millis))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use claude_protocol::PermissionDecision;
    use cockpit_protocol::{
        AgentId, ApprovalAnswer, ApprovalDecision, BackgroundTaskKind, CommandKind, FileEdit,
        McpServerStatus, PlanAnswer, PlanDecision, QuestionAnswer, TaskProgress, TaskStatus,
    };
    use serde_json::json;

    use super::*;

    const FOLDER: &str = "/srv/work/shop";

    fn view() -> AgentView {
        let summary = AgentSummary {
            id: AgentId::new(4),
            title: "Shop".to_owned(),
            folder: PathBuf::from(FOLDER),
            status: AgentStatus::Idle,
            activity: None,
            pending_count: 0,
            model: None,
            effort: None,
            effort_is_default: false,
            mode: PermissionMode::Default,
            task_progress: None,
            context_percent: None,
            cache_expires_at: None,
            turns: 0,
            cost: CostUsd::new(0.0),
            started_at: Timestamp::from_unix_millis(0),
            last_activity_at: None,
            turn_started_at: None,
            session: None,
            position: Position::default(),
        };
        let mut view = AgentView::new(summary, Duration::from_secs(3600));
        view.mark_published();
        view
    }

    fn now() -> Timestamp {
        Timestamp::from_unix_millis(10_000)
    }

    fn apply(view: &mut AgentView, line: Value) -> Vec<FollowUp> {
        let parsed: OutputLine = line.to_string().parse().unwrap();
        view.apply(parsed, now())
    }

    fn prompt(text: &str) -> Prompt {
        Prompt {
            text: text.to_owned(),
            images: Vec::new(),
        }
    }

    fn bodies(view: &AgentView) -> Vec<&ItemBody> {
        view.items().iter().map(|item| &item.body).collect()
    }

    fn init() -> Value {
        json!({
            "type": "system", "subtype": "init", "session_id": "session-1",
            "model": "claude-test-model", "permissionMode": "default",
            "slash_commands": ["review", "tidy"], "skills": ["tidy"],
            "mcp_servers": [{"name": "notes", "status": "connected"}]
        })
    }

    fn echo(text: &str) -> Value {
        json!({"type": "user", "isReplay": true, "message": {"role": "user", "content": text}})
    }

    fn assistant(content: Value) -> Value {
        json!({"type": "assistant", "message": {"content": content}})
    }

    fn tool_use(id: &str, name: &str, input: Value) -> Value {
        assistant(json!([{"type": "tool_use", "id": id, "name": name, "input": input}]))
    }

    fn tool_result(id: &str, text: &str, is_error: bool) -> Value {
        json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": text, "is_error": is_error}
        ]}})
    }

    fn can_use_tool(request_id: &str, tool_use_id: &str, name: &str, input: Value) -> Value {
        json!({"type": "control_request", "request_id": request_id, "request": {
            "subtype": "can_use_tool", "tool_name": name, "tool_use_id": tool_use_id, "input": input
        }})
    }

    fn result(total_cost: f64) -> Value {
        json!({
            "type": "result", "subtype": "success", "is_error": false, "duration_ms": 1500,
            "num_turns": 1, "total_cost_usd": total_cost,
            "usage": {"input_tokens": 10, "output_tokens": 20,
                      "cache_creation_input_tokens": 30, "cache_read_input_tokens": 40}
        })
    }

    fn interrupted_result() -> Value {
        json!({
            "type": "result", "subtype": "error_during_execution", "is_error": true,
            "terminal_reason": "aborted_streaming", "errors": ["internal detail"]
        })
    }

    #[test]
    fn a_plain_turn_shows_the_prompt_the_answer_and_its_end() {
        let mut view = view();

        view.add_prompt(&prompt("Fix the cart total"), PromptDelivery::Now, now());
        assert_eq!(
            view.summary().status,
            AgentStatus::Idle,
            "refreshed on publish"
        );
        view.take_events();
        assert_eq!(view.summary().status, AgentStatus::Working);
        apply(&mut view, init());
        apply(&mut view, echo("Fix the cart total"));
        apply(
            &mut view,
            assistant(json!([{"type": "thinking", "thinking": ""}])),
        );
        apply(
            &mut view,
            assistant(json!([{"type": "text", "text": "The total is fixed."}])),
        );
        let follow_ups = apply(&mut view, result(0.25));
        let events = view.take_events();

        assert_eq!(follow_ups, [FollowUp::TurnEnded]);
        assert_eq!(
            bodies(&view),
            [
                &user_message("Fix the cart total".to_owned(), Vec::new()),
                &ItemBody::AssistantText {
                    markdown: "The total is fixed.".to_owned()
                },
                &ItemBody::TurnEnd(TurnEnd {
                    duration: Duration::from_millis(1500),
                    cost: CostUsd::new(0.25),
                    usage: TokenUsage {
                        input: 10,
                        output: 20,
                        cache_creation: 30,
                        cache_read: 40,
                    },
                    error: None,
                    stopped: false,
                }),
            ]
        );
        let summary = view.summary();
        assert_eq!(summary.status, AgentStatus::Idle);
        assert_eq!(summary.turns, 1);
        assert_eq!(summary.cost, CostUsd::new(0.25));
        assert_eq!(
            summary.session.as_ref().map(ClaudeSessionId::as_str),
            Some("session-1")
        );
        assert_eq!(summary.model.as_deref(), Some("claude-test-model"));
        assert_eq!(
            summary.cache_expires_at,
            Some(Timestamp::from_unix_millis(10_000 + 3_600_000))
        );
        let kinds: Vec<&str> = events
            .iter()
            .map(|event| match event {
                Event::TranscriptAppended { .. } => "appended",
                Event::TranscriptChanged { .. } => "changed",
                Event::DetailsChanged { .. } => "details",
                Event::AgentUpdated(_) => "updated",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["appended", "appended", "details", "updated"]);
    }

    #[test]
    fn init_lists_commands_skills_and_mcp_servers() {
        let mut view = view();

        apply(&mut view, init());

        let details = view.details();
        let commands: Vec<(&str, CommandKind)> = details
            .commands
            .iter()
            .map(|command| (command.name.as_str(), command.kind))
            .collect();
        assert_eq!(
            commands,
            [
                ("review", CommandKind::Command),
                ("tidy", CommandKind::Skill)
            ]
        );
        assert_eq!(details.mcp_servers.len(), 1);
        assert_eq!(details.mcp_servers[0].status, McpServerStatus::Connected);
    }

    #[test]
    fn a_failed_command_shows_its_exit_code() {
        let mut view = view();

        apply(
            &mut view,
            tool_use("t1", "Bash", json!({"command": "cargo test"})),
        );
        assert_eq!(view.summary().activity.as_deref(), Some("Bash: cargo test"));
        apply(
            &mut view,
            tool_result("t1", "Exit code 101\ntest failed", true),
        );

        let ItemBody::ToolCall(call) = &view.items()[0].body else {
            panic!("expected a tool call");
        };
        assert_eq!(call.status, ToolCallStatus::Failed);
        assert_eq!(call.exit_code, Some(101));
        assert_eq!(call.summary, "cargo test");
    }

    #[test]
    fn a_write_records_the_changed_file() {
        let mut view = view();
        let path = format!("{FOLDER}/notes.txt");

        apply(
            &mut view,
            tool_use(
                "t1",
                "Write",
                json!({"file_path": path, "content": "one\ntwo\n"}),
            ),
        );
        let mut done = tool_result("t1", "File written", false);
        done["tool_use_result"] = json!({"type": "update", "originalFile": "one\n"});
        apply(&mut view, done);

        let ItemBody::ToolCall(call) = &view.items()[0].body else {
            panic!("expected a tool call");
        };
        assert_eq!(call.summary, "notes.txt");
        assert_eq!(call.edit.as_ref().unwrap().old_text, "one\n");
        let file = &view.details().changed_files[0];
        assert_eq!((file.lines_added, file.lines_removed), (1, 0));
    }

    #[test]
    fn a_write_over_a_large_file_keeps_its_path_and_line_counts_but_not_its_texts() {
        let mut view = view();
        let path = format!("{FOLDER}/prices.csv");
        apply(
            &mut view,
            tool_use(
                "t1",
                "Write",
                json!({"file_path": path, "content": "sku,price\n"}),
            ),
        );
        let mut done = tool_result("t1", "File written", false);
        // 1.2 MB, over the limit of an edit's text.
        let original = "A-1,10\n".repeat(170_000);
        done["tool_use_result"] = json!({"type": "update", "originalFile": original});

        apply(&mut view, done);

        let ItemBody::ToolCall(call) = &view.items()[0].body else {
            panic!("expected a tool call");
        };
        let edit = call.edit.as_ref().unwrap();
        assert_eq!(edit.path, PathBuf::from(&path));
        assert_eq!((edit.old_text.as_str(), edit.new_text.as_str()), ("", ""));
        let file = &view.details().changed_files[0];
        assert_eq!((file.lines_added, file.lines_removed), (1, 170_000));
    }

    #[test]
    fn a_large_new_text_is_left_out_of_the_call_and_its_approval() {
        let mut view = view();
        let path = format!("{FOLDER}/prices.csv");
        // 1.4 MB, over the limit of an edit's text.
        let input = json!({"file_path": path, "content": "A-1,10\n".repeat(200_000)});
        apply(&mut view, tool_use("t1", "Write", input.clone()));
        apply(&mut view, can_use_tool("req-1", "t1", "Write", input));

        apply(&mut view, tool_result("t1", "File created", false));

        let edits: Vec<&FileEdit> = view
            .items()
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::ToolCall(call) => call.edit.as_ref(),
                ItemBody::Approval(approval) => approval.edit.as_ref(),
                _ => None,
            })
            .collect();
        let empty = FileEdit {
            path: PathBuf::from(&path),
            old_text: String::new(),
            new_text: String::new(),
        };
        assert_eq!(edits, [&empty, &empty]);
        let file = &view.details().changed_files[0];
        assert_eq!((file.lines_added, file.lines_removed), (200_000, 0));
    }

    #[test]
    fn each_approval_names_its_own_call_among_identical_parallel_calls() {
        let mut view = view();
        let input = json!({"command": "touch same.txt"});
        apply(&mut view, tool_use("t1", "Bash", input.clone()));
        apply(&mut view, tool_use("t2", "Bash", input.clone()));

        apply(
            &mut view,
            can_use_tool("req-2", "t2", "Bash", input.clone()),
        );
        apply(&mut view, can_use_tool("req-1", "t1", "Bash", input));

        let calls: Vec<ItemId> = view.items()[..2].iter().map(|item| item.id).collect();
        let links: Vec<Option<ItemId>> = view.items()[2..]
            .iter()
            .map(|item| match &item.body {
                ItemBody::Approval(approval) => approval.tool_call,
                other => panic!("expected an approval, got {other:?}"),
            })
            .collect();
        assert_eq!(links, [Some(calls[1]), Some(calls[0])]);
    }

    #[test]
    fn an_approval_without_a_known_call_names_none() {
        let mut view = view();

        apply(
            &mut view,
            can_use_tool("req-1", "t9", "Bash", json!({"command": "ls"})),
        );

        let [item] = view.items() else {
            panic!("expected one item, got {:?}", view.items());
        };
        assert!(matches!(
            &item.body,
            ItemBody::Approval(Approval {
                tool_call: None,
                ..
            })
        ));
    }

    #[test]
    fn an_approval_waits_then_allows_with_the_input() {
        let mut view = view();
        view.add_prompt(&prompt("Clean up"), PromptDelivery::Now, now());
        apply(
            &mut view,
            tool_use("t1", "Bash", json!({"command": "rm -r build"})),
        );

        apply(
            &mut view,
            can_use_tool("req-1", "t1", "Bash", json!({"command": "rm -r build"})),
        );
        view.take_events();
        assert_eq!(view.summary().status, AgentStatus::NeedsYou);
        assert_eq!(view.summary().pending_count, 1);
        let ItemBody::Approval(approval) = &view.items()[2].body else {
            panic!("expected an approval, got {:?}", view.items()[2].body);
        };
        assert_eq!(approval.summary, "rm -r build");
        assert_eq!(approval.answer, AnswerState::Waiting);

        let answer = Answer::Approval(ApprovalAnswer {
            decision: ApprovalDecision::Allow,
            message: None,
        });
        let answered = view
            .answer(&PendingId::new("req-1".to_owned()), answer)
            .unwrap();
        view.take_events();

        assert_eq!(
            answered.line,
            InputLine::permission(
                ControlRequestId::new("req-1".to_owned()),
                PermissionDecision::allow(object(json!({"command": "rm -r build"})))
            )
        );
        assert_eq!(view.summary().status, AgentStatus::Working);
        let ItemBody::Approval(approval) = &view.items()[2].body else {
            panic!("expected an approval");
        };
        assert!(matches!(approval.answer, AnswerState::Answered(_)));
        let again = view.answer(
            &PendingId::new("req-1".to_owned()),
            Answer::Approval(ApprovalAnswer {
                decision: ApprovalDecision::Deny,
                message: None,
            }),
        );
        assert!(matches!(again, Err(AnswerError::NotFound(_))), "{again:?}");
    }

    #[test]
    fn a_question_replaces_its_tool_call_and_takes_one_answer_per_question() {
        let mut view = view();
        let input = json!({"questions": [{
            "question": "Which store?", "header": "Store",
            "options": [{"label": "Paris"}, {"label": "Lyon"}]
        }]});
        apply(&mut view, tool_use("t1", "AskUserQuestion", input.clone()));

        apply(
            &mut view,
            can_use_tool("req-2", "t1", "AskUserQuestion", input),
        );

        assert_eq!(view.items().len(), 1);
        let ItemBody::Question(question) = &view.items()[0].body else {
            panic!("expected a question");
        };
        assert_eq!(question.questions[0].text, "Which store?");
        assert_eq!(question.questions[0].options.len(), 2);
        let pending = PendingId::new("req-2".to_owned());
        let wrong = view.answer(
            &pending,
            Answer::Question {
                answers: Vec::new(),
            },
        );
        assert!(matches!(wrong, Err(AnswerError::Mismatch(_))), "{wrong:?}");
        let mismatched_kind = view.answer(
            &pending,
            Answer::Approval(ApprovalAnswer {
                decision: ApprovalDecision::Allow,
                message: None,
            }),
        );
        assert!(matches!(mismatched_kind, Err(AnswerError::Mismatch(_))));

        let answers = vec![QuestionAnswer {
            selected: vec!["Lyon".to_owned()],
            other: None,
        }];
        let answered = view.answer(&pending, Answer::Question { answers }).unwrap();

        let InputLine::ControlResponse(response) = answered.line else {
            panic!("expected a control response");
        };
        let text = serde_json::to_value(&response).unwrap();
        assert_eq!(
            text["response"]["response"]["updatedInput"]["answers"],
            json!({"Which store?": "Lyon"})
        );
        // The tool's own result is not a tool call's: the question already shows the answer.
        apply(&mut view, tool_result("t1", "User answered", false));
        assert!(matches!(view.items()[0].body, ItemBody::Question(_)));
    }

    #[test]
    fn an_accepted_plan_asks_for_the_mode_change() {
        let mut view = view();
        let input = json!({"plan": "1. Read\n2. Fix"});

        apply(
            &mut view,
            can_use_tool("req-3", "t9", "ExitPlanMode", input),
        );

        let ItemBody::Plan(plan) = &view.items()[0].body else {
            panic!("expected a plan");
        };
        assert_eq!(plan.markdown, "1. Read\n2. Fix");
        let answered = view
            .answer(
                &PendingId::new("req-3".to_owned()),
                Answer::Plan(PlanAnswer {
                    decision: PlanDecision::AcceptEdits,
                    message: None,
                }),
            )
            .unwrap();
        assert_eq!(answered.then_mode, Some(PermissionMode::AcceptEdits));
    }

    #[test]
    fn an_interrupt_cancels_what_waits_and_fails_what_runs() {
        let mut view = view();
        view.add_prompt(&prompt("Deploy"), PromptDelivery::Now, now());
        apply(
            &mut view,
            tool_use("t1", "Bash", json!({"command": "make deploy"})),
        );
        apply(
            &mut view,
            tool_use("t2", "Bash", json!({"command": "make check"})),
        );
        apply(
            &mut view,
            can_use_tool("req-1", "t1", "Bash", json!({"command": "make deploy"})),
        );
        apply(
            &mut view,
            can_use_tool("req-2", "t2", "Bash", json!({"command": "make check"})),
        );

        apply(
            &mut view,
            json!({"type": "control_cancel_request", "request_id": "req-1"}),
        );
        for notice in [
            "[Request interrupted by user for tool use]",
            "[Request interrupted by user]",
        ] {
            apply(
                &mut view,
                json!({"type": "user", "message": {"role": "user", "content": [
                    {"type": "text", "text": notice}
                ]}}),
            );
        }
        apply(&mut view, interrupted_result());
        view.take_events();

        let answers: Vec<&AnswerState<ApprovalAnswer>> = view
            .items()
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::Approval(approval) => Some(&approval.answer),
                _ => None,
            })
            .collect();
        assert_eq!(answers, [&AnswerState::Cancelled, &AnswerState::Cancelled]);
        assert!(matches!(
            &view.items()[1].body,
            ItemBody::ToolCall(ToolCall {
                status: ToolCallStatus::Failed,
                ..
            })
        ));
        assert!(
            !view
                .items()
                .iter()
                .any(|item| matches!(item.body, ItemBody::Notice { .. })),
            "the turn end says the user stopped the turn"
        );
        let ends: Vec<&TurnEnd> = view
            .items()
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::TurnEnd(end) => Some(end),
                _ => None,
            })
            .collect();
        let [end] = ends.as_slice() else {
            panic!("expected one turn end, got {ends:?}");
        };
        assert!(end.stopped);
        assert_eq!(end.error, None);
        assert_eq!(view.summary().status, AgentStatus::Idle);
        assert_eq!(view.summary().pending_count, 0);
    }

    #[test]
    fn a_failed_turn_carries_its_error() {
        let mut view = view();
        view.add_prompt(&prompt("Refactor everything"), PromptDelivery::Now, now());

        apply(
            &mut view,
            json!({
                "type": "result", "subtype": "error_max_turns", "is_error": true,
                "terminal_reason": "max_turns", "num_turns": 30,
                "errors": ["Reached the maximum number of turns"]
            }),
        );

        let Some(ItemBody::TurnEnd(end)) = view.items().last().map(|item| &item.body) else {
            panic!("expected a turn end");
        };
        assert!(!end.stopped);
        assert_eq!(
            end.error.as_deref(),
            Some("Reached the maximum number of turns")
        );
    }

    #[test]
    fn subagent_lines_nest_under_their_call() {
        let mut view = view();

        apply(
            &mut view,
            tool_use(
                "t1",
                "Task",
                json!({"description": "Survey the tests", "prompt": "Look"}),
            ),
        );
        let mut nested = assistant(json!([{"type": "text", "text": "Found 12 tests."}]));
        nested["parent_tool_use_id"] = json!("t1");
        apply(&mut view, nested);
        let mut nested_prompt =
            json!({"type": "user", "message": {"role": "user", "content": "Look"}});
        nested_prompt["parent_tool_use_id"] = json!("t1");
        apply(&mut view, nested_prompt);

        let subagent = view.items()[0].id;
        assert!(matches!(view.items()[0].body, ItemBody::Subagent(_)));
        assert_eq!(view.items()[1].parent, Some(subagent));
        assert_eq!(view.items()[2].parent, Some(subagent));
        assert!(matches!(view.items()[2].body, ItemBody::UserMessage(_)));
    }

    #[test]
    fn todo_write_fills_the_task_list_and_the_progress() {
        let mut view = view();

        apply(
            &mut view,
            tool_use(
                "t1",
                "TodoWrite",
                json!({"todos": [
                    {"content": "Read", "status": "completed", "activeForm": "Reading"},
                    {"content": "Fix", "status": "in_progress", "activeForm": "Fixing the total"},
                    {"content": "Test", "status": "pending"}
                ]}),
            ),
        );

        assert_eq!(
            view.summary().task_progress,
            Some(TaskProgress { done: 1, total: 3 })
        );
        assert_eq!(view.summary().activity.as_deref(), Some("Fixing the total"));
        let statuses: Vec<TaskStatus> = view
            .details()
            .tasks
            .iter()
            .map(|task| task.status)
            .collect();
        assert_eq!(
            statuses,
            [
                TaskStatus::Completed,
                TaskStatus::InProgress,
                TaskStatus::Pending
            ]
        );
        assert_eq!(view.details().tasks[2].active_form, "Test");
    }

    #[test]
    fn background_tasks_follow_their_events() {
        let mut view = view();

        apply(
            &mut view,
            json!({"type": "system", "subtype": "task_started", "task_id": "bg-1",
                   "description": "Watch the build", "task_type": "local_bash"}),
        );
        apply(
            &mut view,
            json!({"type": "system", "subtype": "task_progress", "task_id": "bg-1",
                   "description": "", "usage": {"total_tokens": 5, "tool_uses": 3}}),
        );
        let running = view.details().background_tasks[0].clone();
        apply(
            &mut view,
            json!({"type": "system", "subtype": "task_notification", "task_id": "bg-1",
                   "status": "failed", "summary": "The build broke"}),
        );

        assert_eq!(running.status, BackgroundTaskStatus::Running);
        assert_eq!(running.kind, BackgroundTaskKind::Command);
        assert_eq!(running.tool_calls, 3);
        let ended = &view.details().background_tasks[0];
        assert_eq!(ended.status, BackgroundTaskStatus::Failed);
        assert_eq!(ended.ended_at, Some(now()));
    }

    #[test]
    fn a_queued_prompt_waits_then_its_echo_adds_nothing() {
        let mut view = view();
        view.add_prompt(&prompt("First"), PromptDelivery::Now, now());
        assert!(view.is_busy());

        let queued = view.add_prompt(&prompt("Second"), PromptDelivery::Queued, now());
        let ItemBody::UserMessage(message) = &view.items()[1].body else {
            panic!("expected a user message");
        };
        assert!(message.queued);
        apply(&mut view, result(0.1));
        view.prompt_sent(queued, now());
        apply(&mut view, echo("Second"));
        apply(&mut view, echo("Typed elsewhere"));

        let ItemBody::UserMessage(message) = &view.items()[1].body else {
            panic!("expected a user message");
        };
        assert!(!message.queued);
        let users = view
            .items()
            .iter()
            .filter(|item| matches!(item.body, ItemBody::UserMessage(_)))
            .count();
        assert_eq!(users, 3);
    }

    #[test]
    fn a_cancelled_queued_prompt_becomes_a_notice() {
        let mut view = view();
        let queued = view.add_prompt(&prompt("Later"), PromptDelivery::Queued, now());

        view.cancel_queued(queued);

        assert_eq!(
            view.items()[0].body,
            ItemBody::Notice {
                text: QUEUED_CANCELLED_NOTICE.to_owned(),
                detail: None
            }
        );
    }

    #[test]
    fn the_cost_adds_up_across_processes() {
        let mut view = view();
        apply(&mut view, result(0.5));
        apply(&mut view, result(0.75));

        view.process_exited("claude exited".to_owned(), now());
        view.process_started(ProcessSession::New);
        apply(&mut view, result(0.25));

        assert_eq!(view.summary().cost, CostUsd::new(1.0));
        assert_eq!(view.summary().turns, 3);
        let costs: Vec<CostUsd> = view
            .items()
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::TurnEnd(end) => Some(end.cost),
                _ => None,
            })
            .collect();
        assert_eq!(
            costs,
            [CostUsd::new(0.5), CostUsd::new(0.25), CostUsd::new(0.25)]
        );
    }

    #[test]
    fn a_resumed_session_counts_from_the_cost_claude_code_restores() {
        let mut view = view();
        apply(&mut view, result(0.5));
        view.process_exited("claude exited".to_owned(), now());

        view.process_started(ProcessSession::Resumed);
        view.set_restored_cost(CostUsd::new(86.0));
        let restored = view.summary().cost;
        apply(&mut view, result(88.5));
        apply(&mut view, result(89.0));

        assert_eq!(restored, CostUsd::new(86.0));
        assert_eq!(view.summary().cost, CostUsd::new(89.0));
        assert_eq!(
            turn_costs(&view),
            [CostUsd::new(0.5), CostUsd::new(2.5), CostUsd::new(0.5)]
        );
    }

    #[test]
    fn a_restored_cost_after_the_first_result_changes_nothing() {
        let mut view = view();
        view.process_started(ProcessSession::Resumed);

        apply(&mut view, result(88.5));
        view.set_restored_cost(CostUsd::new(86.0));
        apply(&mut view, result(89.0));

        assert_eq!(view.summary().cost, CostUsd::new(89.0));
        assert_eq!(turn_costs(&view), [CostUsd::new(88.5), CostUsd::new(0.5)]);
    }

    #[test]
    fn a_clear_before_the_restored_cost_keeps_the_cost_spent() {
        let mut view = view();
        apply(&mut view, result(2.0));
        view.process_exited("claude exited".to_owned(), now());
        view.process_started(ProcessSession::Resumed);

        apply(
            &mut view,
            json!({"type": "conversation_reset", "trigger": "clear", "session_id": "session-1"}),
        );
        view.set_restored_cost(CostUsd::new(2.0));
        apply(&mut view, result(0.25));

        assert_eq!(view.summary().cost, CostUsd::new(2.25));
        assert_eq!(turn_costs(&view), [CostUsd::new(2.0), CostUsd::new(0.25)]);
    }

    /// A process killed with the daemon writes no `cost-state`, so Claude Code restores less than
    /// the agent spent, here nothing.
    #[test]
    fn a_resume_after_a_kill_keeps_the_cost_the_agent_counted() {
        let mut view = view();
        apply(&mut view, result(2.0));
        view.process_exited("claude was killed by signal 9".to_owned(), now());

        view.process_started(ProcessSession::Resumed);
        view.set_restored_cost(CostUsd::new(0.0));
        let restored = view.summary().cost;
        apply(&mut view, result(0.25));

        assert_eq!(restored, CostUsd::new(2.0));
        assert_eq!(view.summary().cost, CostUsd::new(2.25));
        assert_eq!(turn_costs(&view), [CostUsd::new(2.0), CostUsd::new(0.25)]);
    }

    /// The daemon reads the restored cost from the session file, which may take longer than the
    /// first turn, or not be found.
    #[test]
    fn a_first_result_before_the_restored_cost_keeps_the_cost_the_agent_counted() {
        let mut view = view();
        apply(&mut view, result(2.0));
        view.process_exited("claude was killed by signal 9".to_owned(), now());

        view.process_started(ProcessSession::Resumed);
        apply(&mut view, result(0.25));
        let first = view.summary().cost;
        apply(&mut view, result(0.75));

        assert_eq!(first, CostUsd::new(2.0));
        assert_eq!(view.summary().cost, CostUsd::new(2.5));
    }

    /// Claude Code restores the cost of the conversation a `/clear` started, not of the one
    /// before it.
    #[test]
    fn a_resume_after_a_clear_keeps_the_cost_spent_before_it() {
        let mut view = view();
        apply(&mut view, result(1.5));
        apply(
            &mut view,
            json!({"type": "conversation_reset", "trigger": "clear", "session_id": "session-1"}),
        );
        apply(&mut view, result(0.25));
        view.process_exited("claude exited".to_owned(), now());

        view.process_started(ProcessSession::Resumed);
        view.set_restored_cost(CostUsd::new(0.25));
        let restored = view.summary().cost;
        apply(&mut view, result(0.75));

        assert_eq!(restored, CostUsd::new(1.75));
        assert_eq!(view.summary().cost, CostUsd::new(2.25));
        assert_eq!(
            turn_costs(&view),
            [CostUsd::new(1.5), CostUsd::new(0.25), CostUsd::new(0.5)]
        );
    }

    fn turn_costs(view: &AgentView) -> Vec<CostUsd> {
        view.items()
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::TurnEnd(end) => Some(end.cost),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_exit_cancels_what_waits_and_shows_a_notice() {
        let mut view = view();
        apply(
            &mut view,
            can_use_tool("req-1", "t1", "Bash", json!({"command": "ls"})),
        );

        view.process_exited("claude exited with status 1".to_owned(), now());
        view.take_events();

        assert_eq!(view.summary().status, AgentStatus::Exited);
        assert!(matches!(
            &view.items()[0].body,
            ItemBody::Approval(Approval {
                answer: AnswerState::Cancelled,
                ..
            })
        ));
        assert_eq!(
            view.items()[1].body,
            ItemBody::Notice {
                text: "claude exited with status 1".to_owned(),
                detail: None
            }
        );
    }

    #[test]
    fn a_restored_agent_continues_its_ids_and_closes_what_was_open() {
        let mut live = view();
        live.add_prompt(&prompt("Go"), PromptDelivery::Now, now());
        apply(
            &mut live,
            tool_use("t1", "Bash", json!({"command": "sleep 100"})),
        );
        apply(
            &mut live,
            can_use_tool("req-1", "t1", "Bash", json!({"command": "sleep 100"})),
        );
        live.add_prompt(&prompt("Then this"), PromptDelivery::Queued, now());
        live.take_events();

        let mut restored = AgentView::restore(
            live.summary().clone(),
            live.details().clone(),
            live.items().to_vec(),
            Duration::from_secs(60),
        );
        let events = restored.take_events();

        assert_eq!(restored.summary().status, AgentStatus::Exited);
        assert!(matches!(
            &restored.items()[1].body,
            ItemBody::ToolCall(ToolCall {
                status: ToolCallStatus::Failed,
                ..
            })
        ));
        assert!(matches!(
            &restored.items()[2].body,
            ItemBody::Approval(Approval {
                answer: AnswerState::Cancelled,
                ..
            })
        ));
        assert_eq!(
            restored.items()[3].body,
            ItemBody::Notice {
                text: QUEUED_CANCELLED_NOTICE.to_owned(),
                detail: None
            }
        );
        let changed = events
            .iter()
            .filter(|event| matches!(event, Event::TranscriptChanged { .. }))
            .count();
        assert_eq!(changed, 3);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::AgentUpdated(_)))
        );
        restored.process_started(ProcessSession::Resumed);
        let next = restored.add_prompt(&prompt("Again"), PromptDelivery::Now, now());
        assert_eq!(next, ItemId::new(5));
    }

    #[test]
    fn a_typed_command_reads_as_typed_and_its_echo_adds_nothing() {
        let mut view = view();
        view.add_prompt(&prompt("/greet world"), PromptDelivery::Now, now());

        apply(
            &mut view,
            echo(
                "<command-message>greet</command-message>\n\
                 <command-name>/greet</command-name>\n<command-args>world</command-args>",
            ),
        );
        apply(
            &mut view,
            echo("<command-message>review</command-message>\n<command-name>/review</command-name>"),
        );

        assert_eq!(
            bodies(&view),
            [
                &user_message("/greet world".to_owned(), Vec::new()),
                &user_message("/review".to_owned(), Vec::new()),
            ]
        );
    }

    #[test]
    fn a_local_commands_output_shows_as_a_notice() {
        let mut view = view();

        apply(
            &mut view,
            echo("<local-command-stdout>Compacted </local-command-stdout>"),
        );
        apply(
            &mut view,
            echo("<local-command-stdout></local-command-stdout>"),
        );

        assert_eq!(
            bodies(&view),
            [&ItemBody::Notice {
                text: "Compacted".to_owned(),
                detail: None
            }]
        );
    }

    #[test]
    fn untyped_system_subtypes_are_pinned() {
        let words: Vec<&str> = UntypedSystemSubtype::VARIANTS
            .iter()
            .map(|&subtype| subtype.into())
            .collect();

        assert_eq!(
            words,
            ["compact_boundary", "local_command", "session_title_changed"]
        );
    }

    #[test]
    fn a_cleared_conversation_shows_a_notice_and_keeps_the_cost_spent() {
        let mut view = view();
        apply(&mut view, init());
        apply(&mut view, result(0.5));
        view.add_prompt(&prompt("/clear"), PromptDelivery::Now, now());

        apply(
            &mut view,
            json!({"type": "conversation_reset", "trigger": "clear", "session_id": "session-1",
                   "new_conversation_id": "session-2"}),
        );
        let mut new_session = init();
        new_session["session_id"] = json!("session-2");
        apply(&mut view, new_session);
        let mut empty = result(0.0);
        empty["num_turns"] = json!(0);
        empty["usage"] = json!({});
        apply(&mut view, empty);
        let cleared = view.summary().cost;
        apply(&mut view, result(0.25));

        let kinds: Vec<String> = bodies(&view)
            .into_iter()
            .map(|body| match body {
                ItemBody::TurnEnd(end) => format!("end {}", end.cost.get()),
                ItemBody::UserMessage(message) => format!("user {}", message.text),
                ItemBody::Notice { text, .. } => format!("notice {text}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "end 0.5",
                "user /clear",
                "notice Conversation cleared",
                "end 0.25"
            ]
        );
        assert_eq!(cleared, CostUsd::new(0.5));
        assert_eq!(view.summary().cost, CostUsd::new(0.75));
        assert_eq!(view.summary().turns, 2);
        assert_eq!(
            view.summary().session.as_ref().map(ClaudeSessionId::as_str),
            Some("session-2")
        );
    }

    #[test]
    fn claude_code_naming_the_session_renames_the_agent() {
        let mut view = view();
        let renames_session = view.initialized();

        apply(
            &mut view,
            json!({"type": "system", "subtype": "session_title_changed", "title": " Cart fix ",
                   "session_id": "session-1"}),
        );
        let named = view.summary().title.clone();
        apply(
            &mut view,
            json!({"type": "system", "subtype": "session_title_changed", "title": "",
                   "session_id": "session-1"}),
        );

        assert!(!renames_session, "Claude Code restated no name");
        assert_eq!(named, "Cart fix");
        assert_eq!(view.summary().title, "Cart fix");
    }

    fn named(title: &str) -> Value {
        json!({"type": "system", "subtype": "session_title_changed", "title": title,
               "session_id": "session-1"})
    }

    #[test]
    fn the_name_claude_code_restates_as_it_starts_keeps_the_agents_own() {
        let mut view = view();
        view.process_exited("claude exited".to_owned(), now());
        view.set_title("Checkout".to_owned());

        view.process_started(ProcessSession::Resumed);
        apply(&mut view, named("Cart total"));
        let at_start = view.summary().title.clone();
        let renames_session = view.initialized();
        apply(&mut view, named("Cart fix"));

        assert_eq!(at_start, "Checkout");
        assert!(renames_session, "the session still has its old name");
        assert_eq!(view.summary().title, "Cart fix");
    }

    #[test]
    fn a_restated_name_that_is_the_agents_needs_no_rename() {
        let mut view = view();
        view.set_title("Checkout".to_owned());

        view.process_started(ProcessSession::Resumed);
        apply(&mut view, named("  Checkout "));
        let same = view.initialized();
        view.process_started(ProcessSession::Resumed);
        let none = view.initialized();

        assert!(!same, "the session has the agent's name");
        assert!(!none, "Claude Code restated no name");
        assert_eq!(view.summary().title, "Checkout");
    }

    #[test]
    fn a_given_effort_replaces_the_default_one_and_is_no_default() {
        let mut view = view();

        view.set_default_effort(Some(Effort::Medium));
        let default = (view.summary().effort, view.summary().effort_is_default);
        view.set_effort(Effort::High);
        let given = (view.summary().effort, view.summary().effort_is_default);
        view.set_default_effort(None);
        let unknown = (view.summary().effort, view.summary().effort_is_default);

        assert_eq!(default, (Some(Effort::Medium), true));
        assert_eq!(given, (Some(Effort::High), false));
        assert_eq!(unknown, (None, false));
    }

    fn synthetic(text: &str) -> Value {
        json!({"type": "user", "isSynthetic": true, "message": {
            "role": "user", "content": [{"type": "text", "text": text}]
        }})
    }

    fn notice(text: &str, detail: Option<&str>) -> ItemBody {
        ItemBody::Notice {
            text: text.to_owned(),
            detail: detail.map(str::to_owned),
        }
    }

    const GREET: &str = "# /greet — greet someone by name\n\nAnswer `Hello, <name>!`.";

    #[test]
    fn a_skills_instructions_become_a_titled_notice_that_unfolds_to_them() {
        let mut view = view();

        apply(
            &mut view,
            tool_use("t1", "Skill", json!({"skill": "greet", "args": "world"})),
        );
        apply(
            &mut view,
            tool_result("t1", "Launching skill: greet", false),
        );
        apply(&mut view, synthetic(GREET));
        apply(&mut view, echo("Hello there"));

        let bodies = bodies(&view);
        assert!(matches!(bodies[0], ItemBody::ToolCall(_)), "{bodies:?}");
        assert_eq!(
            bodies[1..],
            [
                &notice("Skill instructions: /greet", Some(GREET)),
                &user_message("Hello there".to_owned(), Vec::new()),
            ]
        );
    }

    #[test]
    fn the_summary_after_a_compaction_becomes_a_titled_notice() {
        let mut view = view();
        let summary = "This session is being continued.\n\n1. The user asked for a parser.";

        apply(
            &mut view,
            json!({"type": "system", "subtype": "compact_boundary", "compact_metadata": {}}),
        );
        apply(&mut view, synthetic(summary));

        assert_eq!(
            bodies(&view),
            [
                &notice(COMPACTED_NOTICE, None),
                &notice(SUMMARY_TITLE, Some(summary)),
            ]
        );
    }

    #[test]
    fn context_nothing_explains_gets_a_plain_title() {
        let mut view = view();

        apply(
            &mut view,
            tool_use("t1", "Skill", json!({"skill": "missing"})),
        );
        apply(&mut view, tool_result("t1", "Unknown skill: missing", true));
        apply(&mut view, synthetic("Some context"));
        apply(
            &mut view,
            assistant(json!([{"type": "text", "text": "Done."}])),
        );
        apply(&mut view, synthetic("More context"));

        let notices: Vec<&ItemBody> = bodies(&view)
            .into_iter()
            .filter(|body| matches!(body, ItemBody::Notice { .. }))
            .collect();
        assert_eq!(
            notices,
            [
                &notice(CONTEXT_TITLE, Some("Some context")),
                &notice(CONTEXT_TITLE, Some("More context")),
            ]
        );
    }

    #[test]
    fn a_report_another_source_wrote_unfolds_to_it() {
        let mut view = view();
        let mut report = synthetic("[Subagent hand-back] The report follows:\n  All tests pass.");
        report["origin"] = json!({"kind": "peer"});
        report["isReplay"] = json!(true);

        apply(&mut view, report);

        assert_eq!(
            bodies(&view),
            [&notice(
                "peer: [Subagent hand-back] The report follows:",
                Some("[Subagent hand-back] The report follows:\n  All tests pass.")
            )]
        );
    }

    #[test]
    fn a_compaction_shows_a_notice() {
        let mut view = view();

        apply(
            &mut view,
            json!({"type": "system", "subtype": "compact_boundary", "compact_metadata": {}}),
        );

        assert_eq!(
            view.items()[0].body,
            ItemBody::Notice {
                text: COMPACTED_NOTICE.to_owned(),
                detail: None
            }
        );
    }

    #[test]
    fn a_task_notification_shows_its_summary_as_a_notice() {
        let mut view = view();
        let summary = "Task \"Wait 60 seconds, then print finished\" was stopped by the user or by the app connected to this session";

        apply(
            &mut view,
            json!({
                "type": "user", "isReplay": true, "parent_tool_use_id": null,
                "message": {"role": "user", "content": format!(
                    "<task-notification>\n<task-id>b7k2q</task-id>\n\
                     <tool-use-id>toolu_01Example</tool-use-id>\n\
                     <output-file>/tmp/example/tasks/b7k2q.output</output-file>\n\
                     <status>killed</status>\n<summary>{summary}</summary>\n</task-notification>"
                )},
                "origin": {"kind": "task-notification", "runId": "example-run"}
            }),
        );
        apply(&mut view, echo("Typed in another app"));

        assert_eq!(
            bodies(&view),
            [
                &ItemBody::Notice {
                    text: summary.to_owned(),
                    detail: None
                },
                &user_message("Typed in another app".to_owned(), Vec::new()),
            ]
        );
    }

    #[test]
    fn a_local_command_shows_a_notice_and_no_turn() {
        let mut view = view();
        view.expect_echo("/effort low".to_owned());

        apply(&mut view, init());
        let mut answer = assistant(json!([{"type": "text", "text": "Set effort level to low"}]));
        answer["message"]["model"] = json!("<synthetic>");
        apply(&mut view, answer);
        let mut done = result(0.0);
        done["num_turns"] = json!(0);
        let follow_ups = apply(&mut view, done);
        view.take_events();

        assert_eq!(follow_ups, [FollowUp::TurnEnded]);
        assert_eq!(
            bodies(&view),
            [&ItemBody::Notice {
                text: "Set effort level to low".to_owned(),
                detail: None
            }]
        );
        assert_eq!(view.summary().turns, 0);
        assert_eq!(view.summary().status, AgentStatus::Idle);
        assert_eq!(view.summary().cache_expires_at, None);
    }

    #[test]
    fn an_answer_clears_the_waiting_activity() {
        let mut view = view();
        apply(
            &mut view,
            can_use_tool("req-1", "t1", "Bash", json!({"command": "ls"})),
        );
        assert_eq!(
            view.summary().activity.as_deref(),
            Some("Waiting for you: Bash")
        );

        let answer = Answer::Approval(ApprovalAnswer {
            decision: ApprovalDecision::Allow,
            message: None,
        });
        view.answer(&PendingId::new("req-1".to_owned()), answer)
            .unwrap();

        assert_eq!(view.summary().activity, None);
    }

    #[test]
    fn a_rate_limit_event_goes_to_the_hub() {
        let mut view = view();

        let follow_ups = apply(
            &mut view,
            json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}}),
        );

        assert!(matches!(follow_ups.as_slice(), [FollowUp::RateLimit(_)]));
    }

    fn at(seconds: u64) -> Timestamp {
        Timestamp::from_unix_millis(seconds * 1000)
    }

    fn apply_at(view: &mut AgentView, line: Value, seconds: u64) -> Vec<FollowUp> {
        let parsed: OutputLine = line.to_string().parse().unwrap();
        view.apply(parsed, at(seconds))
    }

    /// The summary's last activity and turn start.
    fn times(view: &AgentView) -> (Option<Timestamp>, Option<Timestamp>) {
        let summary = view.summary();
        (summary.last_activity_at, summary.turn_started_at)
    }

    #[test]
    fn a_turn_is_activity_from_its_prompt_to_its_result() {
        let mut view = view();

        view.add_prompt(&prompt("Fix the cart total"), PromptDelivery::Now, at(100));
        assert_eq!(times(&view), (Some(at(100)), Some(at(100))));
        apply_at(&mut view, init(), 101);
        apply_at(
            &mut view,
            assistant(json!([{"type": "text", "text": "Done."}])),
            140,
        );
        assert_eq!(times(&view), (Some(at(100)), Some(at(100))));
        apply_at(&mut view, result(0.5), 142);

        assert_eq!(times(&view), (Some(at(142)), None));
    }

    #[test]
    fn a_turn_claude_code_starts_itself_starts_at_its_first_line() {
        let mut view = view();

        apply_at(&mut view, init(), 300);
        apply_at(
            &mut view,
            assistant(json!([{"type": "text", "text": "Again."}])),
            330,
        );

        assert_eq!(times(&view), (None, Some(at(300))));
        view.take_events();
        assert_eq!(view.summary().status, AgentStatus::Working);
    }

    #[test]
    fn a_queued_prompt_sent_at_a_turns_end_starts_a_new_turn() {
        let mut view = view();
        view.add_prompt(&prompt("First"), PromptDelivery::Now, at(100));
        let queued = view.add_prompt(&prompt("Second"), PromptDelivery::Queued, at(120));
        assert_eq!(times(&view), (Some(at(100)), Some(at(100))));

        apply_at(&mut view, result(0.5), 200);
        view.prompt_sent(queued, at(200));

        assert_eq!(times(&view), (Some(at(200)), Some(at(200))));
    }

    #[test]
    fn waiting_for_you_counts_from_the_first_request() {
        let mut view = view();
        view.add_prompt(&prompt("Deploy"), PromptDelivery::Now, at(100));
        apply_at(
            &mut view,
            tool_use("t1", "Bash", json!({"command": "ls"})),
            110,
        );
        apply_at(
            &mut view,
            tool_use("t2", "Bash", json!({"command": "pwd"})),
            110,
        );

        apply_at(
            &mut view,
            can_use_tool("req-1", "t1", "Bash", json!({"command": "ls"})),
            120,
        );
        apply_at(
            &mut view,
            can_use_tool("req-2", "t2", "Bash", json!({"command": "pwd"})),
            125,
        );

        assert_eq!(times(&view), (Some(at(120)), Some(at(100))));
    }

    #[test]
    fn a_command_that_runs_no_turn_moves_no_activity() {
        let mut view = view();
        view.add_prompt(&prompt("Fix it"), PromptDelivery::Now, at(100));
        apply_at(&mut view, result(0.5), 150);

        apply_at(&mut view, init(), 900);
        apply_at(
            &mut view,
            json!({"type": "result", "subtype": "success", "is_error": false, "num_turns": 0}),
            901,
        );

        assert_eq!(times(&view), (Some(at(150)), None));
    }

    #[test]
    fn an_exit_is_activity_and_ends_the_turn() {
        let mut view = view();
        view.add_prompt(&prompt("Go"), PromptDelivery::Now, at(100));

        view.process_exited("claude exited".to_owned(), at(160));

        assert_eq!(times(&view), (Some(at(160)), None));
    }

    #[test]
    fn a_turn_the_daemon_cut_off_was_last_active_at_its_start_or_later() {
        let logged = |last: u64, turn: u64| AgentSummary {
            status: AgentStatus::Working,
            last_activity_at: Some(at(last)),
            turn_started_at: Some(at(turn)),
            ..view().summary().clone()
        };
        let restore = |summary: AgentSummary| {
            AgentView::restore(summary, AgentDetails::default(), Vec::new(), Duration::ZERO)
        };

        // A turn that started after the last activity, or one that waited for you since.
        assert_eq!(times(&restore(logged(50, 100))), (Some(at(100)), None));
        assert_eq!(times(&restore(logged(130, 100))), (Some(at(130)), None));
    }

    fn object(value: Value) -> Map<String, Value> {
        let Value::Object(map) = value else {
            panic!("not an object");
        };
        map
    }
}
