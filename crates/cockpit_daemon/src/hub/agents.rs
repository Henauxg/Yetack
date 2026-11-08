//! An agent's life: start, prompts and their queue, answers, its process's events, stop, resume
//! and removal.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};

use claude_protocol::{ControlRequest, InputLine, OutputLine};
use cockpit_protocol::{
    AccountId, AgentId, AgentStatus, AgentSummary, AnswerParams, ClaudeSessionId, CostUsd,
    EffortSource, ErrorBody, ErrorCode, Event, MAX_TITLE_CHARS, Position, Prompt, PromptDelivery,
    PromptId, QueuedPromptParams, RenameParams, ResponseBody, ResponseResult, SetEffortParams,
    SignInWait, StartParams, TranscriptItem, WaitingState,
};

use crate::accounts::ClaudeEnv;
use crate::hub::input::{ControlPurpose, HubInput, Reply};
use crate::hub::state::{Agent, Hub, QueuedPrompt};
use crate::hub::{errors, state};
use crate::machine_load;
use crate::process::{ClaudeCommand, InterruptedTurn, ProcessEvent};
use crate::read_file;
use crate::sessions;
use crate::sessions::FoundSession;
use crate::store::{AgentRecord, LogName, RawLog};
use crate::transcript::prompts::TypedSetting;
use crate::transcript::{AgentView, AnswerError, FollowUp, ProcessSession, prompts, words};

/// A title made from a prompt keeps this many characters.
const TITLE_MAX_CHARS: usize = 60;
/// The longest model name a client may set: every summary carries it.
const MODEL_MAX_CHARS: usize = 100;

impl Hub {
    pub(super) fn start_restored_agents(&mut self) {
        let ids: Vec<AgentId> = self.agents.keys().copied().collect();
        for id in ids {
            self.publish(id);
            let running = self
                .agents
                .get(&id)
                .is_some_and(|agent| agent.record.running);
            if running && let Err(error) = self.start_process(id, InterruptedTurn::Resume) {
                self.exited_at_start(id, &error);
            }
            self.publish(id);
        }
    }

    pub(super) fn start_agent(&mut self, params: StartParams) -> Result<ResponseBody, ErrorBody> {
        let title: Option<String> = params.title.as_deref().map(client_title).transpose()?;
        if let Some(model) = &params.model {
            check_model(model)?;
        }
        let folder = agent_folder(&params.folder)?;
        let account = self.account(params.account)?;
        let title = title.unwrap_or_else(|| default_title(params.prompt.as_ref(), &folder));
        let record = AgentRecord {
            id: self.next_agent,
            title,
            folder,
            session: None,
            model: params.model,
            effort: params.effort,
            mode: params.mode,
            running: true,
            started_at: state::now(),
            past: None,
            position: Position::default(),
            process: None,
            account: account.id,
        };
        let agent = self.add_agent(record, params.prompt, None)?;
        Ok(ResponseBody::AgentStarted { agent })
    }

    /// Starts an agent's process, then lists the agent on top of the others. Nothing is listed
    /// when `claude` cannot start. `past` is the transcript of the session the agent takes over,
    /// which shows above its own items without any event: clients read it with `agent.history`.
    pub(super) fn add_agent(
        &mut self,
        mut record: AgentRecord,
        prompt: Option<Prompt>,
        past: Option<Vec<TranscriptItem>>,
    ) -> Result<AgentId, ErrorBody> {
        let id = record.id;
        let next = id
            .next()
            .ok_or_else(|| errors::internal("no agent id is left".to_owned()))?;
        let mut places = cockpit_protocol::place_on_top(&self.places(), id);
        if let Some(index) = places.iter().position(|place| place.agent == id) {
            record.position = places.swap_remove(index).position;
        }
        let mut view = AgentView::new(initial_summary(&record), self.settings.prompt_cache_ttl());
        if let Some(past) = past {
            view.show_past(past);
        }
        self.agents.insert(
            id,
            Agent {
                record,
                view,
                process: None,
                generation: 0,
                queue: VecDeque::new(),
            },
        );
        if let Err(error) = self.start_process(id, InterruptedTurn::Leave) {
            self.agents.remove(&id);
            return Err(ErrorBody::new(ErrorCode::ClaudeFailed, error));
        }
        self.next_agent = next;
        // The agents below the new one, renumbered when it found no room above them.
        self.set_places(&places);
        self.save_agents();
        let Some(agent) = self.agents.get_mut(&id) else {
            return Err(errors::agent_not_found(id));
        };
        agent.view.mark_published();
        let summary = agent.view.summary().clone();
        self.emit(LogName::Agent(id), Event::AgentAdded(summary));
        for place in places {
            self.publish(place.agent);
        }
        if let Some(prompt) = prompt {
            self.prompt(id, prompt, PromptDelivery::Steer)?;
        }
        Ok(id)
    }

    /// Starts `claude` for an agent and asks for its commands. The error says why it cannot
    /// start.
    pub(super) fn start_process(
        &mut self,
        id: AgentId,
        interrupted_turn: InterruptedTurn,
    ) -> Result<(), String> {
        let raw_path = self.raw_log_path(id);
        let account_id = self
            .agents
            .get(&id)
            .map(|agent| agent.record.account)
            .ok_or_else(|| format!("no agent {}", id.get()))?;
        let account = self
            .account(account_id)
            .map_err(|_| format!("the agent's account {} no longer exists", account_id.get()))?;
        let Some(agent) = self.agents.get_mut(&id) else {
            return Err(format!("no agent {}", id.get()));
        };
        let record = &agent.record;
        let command = ClaudeCommand {
            program: self.config.claude_program.clone(),
            folder: record.folder.clone(),
            model: record.model.clone(),
            mode: record.mode.map(words::mode_to_claude),
            effort: record.effort.and_then(words::effort_word),
            resume: record.session.clone(),
            interrupted_turn,
            env: ClaudeEnv::new(&account),
        };
        let raw_log = RawLog::open(&raw_path)
            .inspect_err(|error| {
                tracing::warn!(error = %errors::chain(error), "cannot open the raw log");
            })
            .ok();
        let generation = agent.generation.wrapping_add(1);
        let handle = crate::process::spawn(&command, raw_log, self.inputs.clone(), move |event| {
            HubInput::Process {
                agent: id,
                generation,
                event,
            }
        })
        .map_err(|error| errors::chain(&error))?;
        if let Err(purpose) = handle.control(ControlRequest::Initialize, ControlPurpose::Initialize)
        {
            purpose.fail(errors::agent_exited(id));
        }
        agent.generation = generation;
        agent.record.process = handle.pid().and_then(machine_load::record_process);
        agent.process = Some(handle);
        match command.resume {
            Some(session) => {
                agent.view.process_started(ProcessSession::Resumed);
                self.read_restored_cost(id, generation, account.folder, session);
            }
            None => agent.view.process_started(ProcessSession::New),
        }
        self.save_agents();
        Ok(())
    }

    /// Reads the cost Claude Code restores from the session file, in a task, for the first turn's
    /// cost.
    /// `folder` is the agent's account's, which holds the session file.
    fn read_restored_cost(
        &self,
        id: AgentId,
        generation: u32,
        folder: PathBuf,
        session: ClaudeSessionId,
    ) {
        let inputs = self.inputs.clone();
        tokio::spawn(async move {
            let read =
                tokio::task::spawn_blocking(move || sessions::restored_cost(&folder, &session))
                    .await;
            if let Ok(Some(cost)) = read {
                let input = HubInput::RestoredCost {
                    agent: id,
                    generation,
                    cost,
                };
                // The daemon may be stopping.
                let _ = inputs.send(input).await;
            }
        });
    }

    pub(super) fn restored_cost(&mut self, id: AgentId, generation: u32, cost: CostUsd) {
        let Some(agent) = self
            .agents
            .get_mut(&id)
            .filter(|agent| agent.generation == generation)
        else {
            return;
        };
        agent.view.set_restored_cost(cost);
        self.publish(id);
    }

    fn exited_at_start(&mut self, id: AgentId, error: &str) {
        if let Some(agent) = self.agents.get_mut(&id) {
            tracing::warn!(agent = id.get(), error, "cannot start claude");
            agent
                .view
                .process_exited(format!("cannot start claude: {error}"), state::now());
        }
    }

    pub(super) fn process_event(
        &mut self,
        id: AgentId,
        generation: u32,
        event: ProcessEvent<ControlPurpose>,
    ) {
        let current = self
            .agents
            .get(&id)
            .is_some_and(|agent| agent.generation == generation && agent.process.is_some());
        if !current {
            // An event of a process the agent no longer runs.
            if let ProcessEvent::ControlAnswer { tag, .. } = event {
                tag.fail(errors::agent_exited(id));
            }
            return;
        }
        match event {
            ProcessEvent::Line(line) => self.claude_line(id, *line),
            ProcessEvent::ControlAnswer { tag, result } => self.control_answered(id, tag, result),
            ProcessEvent::Exited { notice } => self.process_exited(id, notice),
        }
    }

    fn claude_line(&mut self, id: AgentId, line: OutputLine) {
        let Some(agent) = self.agents.get_mut(&id) else {
            return;
        };
        let model_before = agent.view.summary().model.clone();
        let follow_ups = agent.view.apply(line, state::now());
        // A `/model` typed in the composer shows in the next turn's `system/init`: the new model
        // has its own default effort.
        let refresh_effort = model_before.is_some()
            && agent.view.summary().model != model_before
            && agent.record.effort.is_none();
        let account = agent.record.account;
        let session = agent.view.summary().session.clone();
        let session_changed = session.is_some() && session != agent.record.session;
        if session_changed {
            agent.record.session = session;
        }
        // Claude Code names the session itself after a `/rename` typed in the composer.
        let title_changed = agent.view.summary().title != agent.record.title;
        if title_changed {
            agent.record.title = agent.view.summary().title.clone();
        }
        for follow_up in follow_ups {
            match follow_up {
                FollowUp::TurnEnded => {
                    self.control(
                        id,
                        ControlRequest::GetContextUsage,
                        ControlPurpose::ContextRefresh,
                    );
                    self.send_next_queued(id);
                }
                FollowUp::RateLimit(info) => self.plan_usage_reported(account, &info),
                FollowUp::SignInNeeded => {
                    let notice = self.sign_in_notice(account);
                    if let Some(agent) = self.agents.get_mut(&id) {
                        agent.view.add_notice(notice);
                    }
                }
            }
        }
        if session_changed || title_changed {
            self.save_agents();
        }
        if refresh_effort {
            self.control(
                id,
                ControlRequest::GetSettings,
                ControlPurpose::DefaultEffort,
            );
        }
        self.publish(id);
    }

    fn process_exited(&mut self, id: AgentId, notice: String) {
        let Some(agent) = self.agents.get_mut(&id) else {
            return;
        };
        agent.process = None;
        let recorded = agent.record.process.take().is_some();
        agent.queue.clear();
        agent.view.process_exited(notice, state::now());
        self.publish(id);
        if recorded {
            self.save_agents();
        }
    }

    /// Sends a prompt now, or queues it while the agent works, as `delivery` asks.
    pub(super) fn prompt(
        &mut self,
        id: AgentId,
        prompt: Prompt,
        delivery: PromptDelivery,
    ) -> Result<ResponseBody, ErrorBody> {
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        let Some(process) = agent.process.as_ref().filter(|_| agent.record.running) else {
            return Err(errors::agent_exited(id));
        };
        let line = prompts::prompt_line(&prompt)
            .ok_or_else(|| errors::internal("cannot encode the prompt".to_owned()))?;
        let mut record_changed = false;
        let busy = agent.view.is_busy() || !agent.queue.is_empty();
        let steers = delivery == PromptDelivery::Steer && !is_slash_command(&prompt.text);
        if !busy {
            agent.view.add_prompt(&prompt, state::now());
            process.write(line);
            record_changed = record_typed_setting(agent, &prompt.text);
        } else if steers {
            agent.view.add_waiting_prompt(&prompt, WaitingState::Sent);
            process.write(line);
        } else {
            let id = agent.view.add_waiting_prompt(&prompt, WaitingState::Queued);
            agent.queue.push_back(QueuedPrompt {
                id: Some(id),
                line,
                prompt,
            });
        }
        if record_changed {
            self.save_agents();
        }
        self.publish(id);
        Ok(ResponseBody::Done)
    }

    /// Sends the queued prompts up to the first one the transcript shows, which starts a turn.
    /// While `claude` holds a prompt Claude has not read, that prompt starts the next turn, and
    /// the queue waits for its end.
    fn send_next_queued(&mut self, id: AgentId) {
        let Some(agent) = self.agents.get_mut(&id) else {
            return;
        };
        let Some(process) = &agent.process else {
            return;
        };
        if agent.view.has_unread_prompts() {
            return;
        }
        let mut shown_text = None;
        while let Some(queued) = agent.queue.pop_front() {
            process.write(queued.line);
            match queued.id {
                Some(prompt) => {
                    agent
                        .view
                        .queued_prompt_started(prompt, &queued.prompt, state::now());
                    shown_text = Some(queued.prompt.text);
                    break;
                }
                None => agent.view.expect_echo(queued.prompt.text),
            }
        }
        if shown_text.is_some_and(|text| record_typed_setting(agent, &text)) {
            self.save_agents();
        }
    }

    /// Sends a queued prompt during the turn, for Claude to read at its next step.
    pub(super) fn send_queued(
        &mut self,
        params: QueuedPromptParams,
    ) -> Result<ResponseBody, ErrorBody> {
        let id = params.agent;
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        if agent.process.is_none() || !agent.record.running {
            return Err(errors::agent_exited(id));
        }
        let waiting = agent
            .queue
            .iter()
            .find(|queued| queued.id == Some(params.prompt));
        if waiting.is_some_and(|queued| is_slash_command(&queued.prompt.text)) {
            return Err(errors::invalid(
                "a slash command waits for the turn's end: Claude Code does not echo every one"
                    .to_owned(),
            ));
        }
        let queued = take_queued(agent, params.prompt)?;
        if let Some(process) = &agent.process {
            process.write(queued.line);
        }
        agent.view.queued_prompt_steered(params.prompt);
        self.publish(id);
        Ok(ResponseBody::Done)
    }

    pub(super) fn cancel_queued(
        &mut self,
        params: QueuedPromptParams,
    ) -> Result<ResponseBody, ErrorBody> {
        let id = params.agent;
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        take_queued(agent, params.prompt)?;
        agent.view.remove_waiting_prompt(params.prompt);
        self.publish(id);
        Ok(ResponseBody::Done)
    }

    pub(super) fn answer(&mut self, params: AnswerParams) -> Result<ResponseBody, ErrorBody> {
        let id = params.agent;
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        if agent.process.is_none() || !agent.record.running {
            return Err(errors::agent_exited(id));
        }
        let answered = agent
            .view
            .answer(&params.pending, params.answer)
            .map_err(|error| match error {
                AnswerError::NotFound(_) => ErrorBody::new(ErrorCode::NotFound, error.to_string()),
                AnswerError::Mismatch(_) => errors::invalid(error.to_string()),
            })?;
        if let Some(process) = &agent.process {
            process.write(answered.line);
        }
        if let Some(mode) = answered.then_mode {
            let request = ControlRequest::SetPermissionMode {
                mode: words::mode_to_claude(mode),
            };
            self.control(id, request, ControlPurpose::SetMode { mode, reply: None });
        }
        self.publish(id);
        Ok(ResponseBody::Done)
    }

    /// Closes the process's stdin; the agent shows as exited once the process ends.
    pub(super) fn stop(&mut self, id: AgentId) -> Result<ResponseBody, ErrorBody> {
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        let Some(process) = &agent.process else {
            return Err(errors::agent_exited(id));
        };
        process.stop();
        agent.record.running = false;
        self.save_agents();
        Ok(ResponseBody::Done)
    }

    pub(super) fn resume(&mut self, id: AgentId) -> Result<ResponseBody, ErrorBody> {
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        if agent.process.is_some() {
            return Err(errors::agent_running(id));
        }
        self.start_process(id, InterruptedTurn::Leave)
            .map_err(|error| ErrorBody::new(ErrorCode::ClaudeFailed, error))?;
        if let Some(agent) = self.agents.get_mut(&id) {
            agent.record.running = true;
            agent.view.resumed_by_user(state::now());
        }
        self.save_agents();
        self.publish(id);
        Ok(ResponseBody::Done)
    }

    pub(super) fn remove(&mut self, id: AgentId) -> Result<ResponseBody, ErrorBody> {
        let agent = self.agents.get(&id).ok_or(errors::agent_not_found(id))?;
        if agent.process.is_some() {
            return Err(errors::agent_running(id));
        }
        self.discard(id);
        Ok(ResponseBody::Done)
    }

    /// Removes the agent whatever its process does, interrupting a working agent's turn first.
    /// The process handle leaves with the agent, which closes `claude`'s stdin: `claude` ends its
    /// session file and exits, or is killed after a delay.
    pub(super) fn close(&mut self, id: AgentId) -> Result<ResponseBody, ErrorBody> {
        let agent = self.agents.get(&id).ok_or(errors::agent_not_found(id))?;
        if agent.process.is_some() && agent.view.is_busy() {
            self.control(id, ControlRequest::Interrupt, ControlPurpose::Closing);
        }
        self.discard(id);
        Ok(ResponseBody::Done)
    }

    /// Takes the agent out of the hub, with its logs, and tells the clients.
    fn discard(&mut self, id: AgentId) {
        self.agents.remove(&id);
        if let Err(error) = self.logs.remove(LogName::Agent(id)) {
            tracing::warn!(error = %errors::chain(&error), "cannot delete an agent's log");
        }
        if let Err(error) = std::fs::remove_file(self.raw_log_path(id))
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "cannot delete an agent's raw log");
        }
        // The agent's events left with its log: a client that saw less cannot replay.
        self.replay_floor = self.last_seq;
        self.save_agents();
        self.emit(LogName::Daemon, Event::AgentRemoved { agent: id });
    }

    /// Effort has no control request: `/effort <level>` goes as a prompt the transcript does not
    /// show. An exited agent takes it at its next start.
    pub(super) fn set_effort(
        &mut self,
        params: SetEffortParams,
    ) -> Result<ResponseBody, ErrorBody> {
        let id = params.agent;
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        let text = prompts::effort_command(params.effort)
            .ok_or_else(|| errors::internal("the effort has no word".to_owned()))?;
        send_hidden_prompt(agent, text);
        agent.record.effort = Some(params.effort);
        agent.view.set_effort(params.effort);
        self.save_agents();
        self.publish(id);
        Ok(ResponseBody::Done)
    }

    /// Renames the agent. While its process runs, `/rename` names its Claude session too, so
    /// Claude Code's resume list and `sessions.list` show the name.
    pub(super) fn rename(&mut self, params: RenameParams) -> Result<ResponseBody, ErrorBody> {
        let title = client_title(&params.title)?;
        let id = params.agent;
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        send_hidden_prompt(agent, prompts::rename_command(&title));
        agent.record.title = title.clone();
        agent.view.set_title(title);
        self.save_agents();
        self.publish(id);
        Ok(ResponseBody::Done)
    }

    /// Gives the agent's name to its Claude session, whose saved name is another.
    pub(super) fn rename_session(&mut self, id: AgentId) {
        if let Some(agent) = self.agents.get_mut(&id) {
            let text = prompts::rename_command(&agent.record.title);
            send_hidden_prompt(agent, text);
        }
    }

    /// A past session becomes an agent, or the agent that has it resumes.
    pub(super) fn resume_session(&mut self, found: FoundSession, reply: Reply) {
        respond(reply, self.agent_for_session(found));
    }

    fn agent_for_session(&mut self, found: FoundSession) -> Result<ResponseBody, ErrorBody> {
        let FoundSession { session, past } = found;
        let account = self.account(session.account)?;
        if let Some(agent) = self.agent_with_session(&session.id, account.id) {
            if self
                .agents
                .get(&agent)
                .is_some_and(|found| found.process.is_none())
            {
                self.resume(agent)?;
            }
            return Ok(ResponseBody::AgentStarted { agent });
        }
        let record = AgentRecord {
            id: self.next_agent,
            title: session.title,
            folder: agent_folder(&session.folder)?,
            session: Some(session.id),
            model: None,
            effort: None,
            mode: None,
            running: true,
            started_at: state::now(),
            past: Some(past.file),
            position: Position::default(),
            process: None,
            account: account.id,
        };
        let agent = self.add_agent(record, None, Some(past.items))?;
        Ok(ResponseBody::AgentStarted { agent })
    }

    /// The agent that has the session of the account's folder.
    pub(super) fn agent_with_session(
        &self,
        session: &ClaudeSessionId,
        account: AccountId,
    ) -> Option<AgentId> {
        self.agents
            .values()
            .find(|agent| {
                agent.record.session.as_ref() == Some(session) && agent.record.account == account
            })
            .map(|agent| agent.record.id)
    }
}

/// Records the setting a prompt typed in the composer changes, as the agent's menus do, once the
/// prompt goes to `claude`. True when the agent's record changed.
fn record_typed_setting(agent: &mut Agent, text: &str) -> bool {
    match TypedSetting::parse(text) {
        Some(TypedSetting::Effort(effort)) => {
            agent.record.effort = Some(effort);
            agent.view.set_effort(effort);
            true
        }
        // The summary shows the model once a turn's `system/init` reports it, and the daemon then
        // asks for the effort it applies.
        Some(TypedSetting::Model(model)) if check_model(&model).is_ok() => {
            agent.record.model = Some(model);
            true
        }
        Some(TypedSetting::Model(_)) | None => false,
    }
}

/// A slash command always waits for the turn's end: Claude Code does not echo every one, so the
/// daemon could not tell when it was read.
fn is_slash_command(text: &str) -> bool {
    text.starts_with('/')
}

/// Takes a prompt out of the agent's queue, before `claude` gets it.
fn take_queued(agent: &mut Agent, prompt: PromptId) -> Result<QueuedPrompt, ErrorBody> {
    agent
        .queue
        .iter()
        .position(|queued| queued.id == Some(prompt))
        .and_then(|position| agent.queue.remove(position))
        .ok_or_else(|| {
            ErrorBody::new(
                ErrorCode::NotFound,
                format!("prompt {} is not queued", prompt.get()),
            )
        })
}

/// Sends a prompt the transcript does not show, such as `/effort high`, now or after the current
/// turn. An agent without a process gets nothing.
fn send_hidden_prompt(agent: &mut Agent, text: String) {
    let Some(process) = &agent.process else {
        return;
    };
    let line = InputLine::prompt(text.clone());
    if agent.view.is_busy() || !agent.queue.is_empty() {
        agent.queue.push_back(QueuedPrompt {
            id: None,
            line,
            prompt: Prompt {
                text,
                images: Vec::new(),
            },
        });
    } else {
        process.write(line);
        agent.view.expect_echo(text);
    }
}

pub(super) fn respond(reply: Reply, result: Result<ResponseBody, ErrorBody>) {
    // The client may have left.
    let _ = reply.send(ResponseResult::from(result));
}

/// The summary of an agent before `claude` says anything.
pub(super) fn initial_summary(record: &AgentRecord) -> AgentSummary {
    AgentSummary {
        id: record.id,
        title: record.title.clone(),
        folder: record.folder.clone(),
        status: AgentStatus::Idle,
        activity: None,
        pending_count: 0,
        model: record.model.clone(),
        effort: record.effort,
        effort_source: EffortSource::Chosen,
        mode: record.mode.unwrap_or_default(),
        task_progress: None,
        context_percent: None,
        cache_expires_at: None,
        turns: 0,
        cost: CostUsd::new(0.0),
        started_at: record.started_at,
        last_activity_at: Some(record.started_at),
        turn_started_at: None,
        session: record.session.clone(),
        position: record.position,
        account: record.account,
        sign_in_wait: SignInWait::NotWaiting,
    }
}

/// An existing folder, absolute, with its symlinks resolved. On Windows it keeps the form a user
/// writes, `C:\work` and not `\\?\C:\work`: Claude Code records the session under it, and the
/// recent folders show it.
fn agent_folder(folder: &Path) -> Result<PathBuf, ErrorBody> {
    if !folder.is_absolute() {
        return Err(errors::invalid(format!(
            "the folder {} is not absolute",
            folder.display()
        )));
    }
    let resolved = read_file::canonical(folder).map_err(|error| {
        ErrorBody::new(
            ErrorCode::NotFound,
            format!("cannot open the folder {}: {error}", folder.display()),
        )
    })?;
    if !resolved.is_dir() {
        return Err(errors::invalid(format!(
            "{} is not a folder",
            folder.display()
        )));
    }
    Ok(resolved)
}

pub(super) fn check_model(model: &str) -> Result<(), ErrorBody> {
    check_length("model name", model, MODEL_MAX_CHARS)
}

/// A title from a client, its whitespace collapsed to single spaces and trimmed. It is refused
/// empty or over [`MAX_TITLE_CHARS`] characters.
fn client_title(text: &str) -> Result<String, ErrorBody> {
    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return Err(errors::invalid("the title is empty".to_owned()));
    }
    check_length("title", &title, MAX_TITLE_CHARS)?;
    Ok(title)
}

/// Refuses a client's text longer than `max_chars`, so that the snapshots stay small.
fn check_length(what: &str, text: &str, max_chars: usize) -> Result<(), ErrorBody> {
    let chars = text.chars().count();
    if chars > max_chars {
        return Err(errors::invalid(format!(
            "the {what} has {chars} characters, more than {max_chars}"
        )));
    }
    Ok(())
}

/// The prompt's first line, or the folder's name.
fn default_title(prompt: Option<&Prompt>, folder: &Path) -> String {
    let first_line = prompt.and_then(|prompt| {
        prompt
            .text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
    });
    if let Some(line) = first_line {
        if line.chars().count() <= TITLE_MAX_CHARS {
            return line.to_owned();
        }
        let mut title: String = line.chars().take(TITLE_MAX_CHARS - 1).collect();
        title.push('…');
        return title;
    }
    folder.file_name().map_or_else(
        || folder.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use cockpit_protocol::{PastSession, Timestamp};

    #[cfg(unix)]
    use crate::sessions::SessionPast;
    #[cfg(unix)]
    use crate::store::PastFile;
    #[cfg(unix)]
    use crate::test_script;

    use super::*;

    #[test]
    fn a_title_comes_from_the_prompt_or_the_folder() {
        let folder = Path::new("/srv/work/shop");
        let prompt = |text: &str| Prompt {
            text: text.to_owned(),
            images: Vec::new(),
        };

        assert_eq!(default_title(None, folder), "shop");
        assert_eq!(
            default_title(Some(&prompt("\n  Fix the cart\nthen test")), folder),
            "Fix the cart"
        );
        let long = default_title(Some(&prompt(&"word ".repeat(40))), folder);
        assert_eq!(long.chars().count(), TITLE_MAX_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn a_folder_must_exist_and_be_absolute() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();

        assert!(agent_folder(temp.path()).is_ok());
        let relative = agent_folder(Path::new("work")).unwrap_err();
        let missing = agent_folder(&temp.path().join("missing")).unwrap_err();
        let not_a_folder = agent_folder(&file).unwrap_err();

        assert_eq!(relative.code, ErrorCode::InvalidRequest);
        assert_eq!(missing.code, ErrorCode::NotFound);
        assert_eq!(not_a_folder.code, ErrorCode::InvalidRequest);
    }

    /// Titles and model names go into every snapshot, which must stay under the framing maximum.
    #[test]
    fn a_title_or_a_model_name_too_long_is_refused() {
        use cockpit_protocol::SetModelParams;
        use tokio::sync::oneshot;

        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(&temp.path().join("state"));
        let mut start = |title: usize, model: usize| {
            let params = StartParams {
                folder: temp.path().to_owned(),
                title: Some("t".repeat(title)),
                model: Some("m".repeat(model)),
                effort: None,
                mode: None,
                prompt: None,
                account: AccountId::DEFAULT,
            };
            hub.start_agent(params).unwrap_err().code
        };

        let long_title = start(MAX_TITLE_CHARS + 1, 1);
        let long_model = start(1, MODEL_MAX_CHARS + 1);
        // The test's `claude` cannot start: the request got past the checks.
        let longest = start(MAX_TITLE_CHARS, MODEL_MAX_CHARS);
        let (reply, mut answer) = oneshot::channel();
        let set_model = SetModelParams {
            agent: AgentId::new(1),
            model: "m".repeat(MODEL_MAX_CHARS + 1),
        };
        hub.set_model(set_model, reply);

        assert_eq!(long_title, ErrorCode::InvalidRequest);
        assert_eq!(long_model, ErrorCode::InvalidRequest);
        assert_eq!(longest, ErrorCode::ClaudeFailed);
        let ResponseResult::Error(refused) = answer.try_recv().unwrap() else {
            panic!("expected the model name to be refused");
        };
        assert_eq!(refused.code, ErrorCode::InvalidRequest);
    }

    /// A session file's `cwd` is what `claude` wrote, or anything a process of the user wrote.
    #[cfg(unix)]
    #[test]
    fn a_past_session_needs_a_folder_an_agent_can_start_in() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(temp.path());
        let id = ClaudeSessionId::new("11111111-2222-3333-4444-555555555555".to_owned());
        let session = |folder: &str| FoundSession {
            session: PastSession {
                id: id.clone(),
                title: "Fix the cart".to_owned(),
                folder: PathBuf::from(folder),
                last_activity: Timestamp::from_unix_millis(0),
                turns: 1,
                account: AccountId::DEFAULT,
            },
            past: SessionPast {
                items: Vec::new(),
                file: PastFile {
                    session: id.clone(),
                    bytes: 0,
                },
            },
        };

        let relative = hub.agent_for_session(session("work")).unwrap_err();
        let missing = hub
            .agent_for_session(session("/nonexistent/agents-cockpit/work"))
            .unwrap_err();

        assert_eq!(relative.code, ErrorCode::InvalidRequest);
        assert_eq!(missing.code, ErrorCode::NotFound);
        assert!(hub.agents.is_empty());
    }

    /// A close takes out a running agent at once, with its log, and tells the clients.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_close_removes_a_running_agent_and_its_log() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        use tokio::sync::mpsc;

        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(&temp.path().join("state"));
        let claude = temp.path().join("claude");
        std::fs::write(&claude, "#!/bin/sh\nexec cat >/dev/null\n").unwrap();
        std::fs::set_permissions(&claude, Permissions::from_mode(0o755)).unwrap();
        hub.config.claude_program = claude;
        let start = StartParams {
            folder: temp.path().to_owned(),
            title: None,
            model: None,
            effort: None,
            mode: None,
            prompt: None,
            account: AccountId::DEFAULT,
        };
        let Ok(ResponseBody::AgentStarted { agent }) = hub.start_agent(start) else {
            panic!("the agent did not start");
        };
        let log = hub.logs.path(LogName::Agent(agent));
        let (client, mut frames) = mpsc::channel(16);
        hub.clients.push(client);

        let closed = hub.close(agent);
        let again = hub.close(agent).unwrap_err();

        assert_eq!(closed, Ok(ResponseBody::Done));
        assert_eq!(again.code, ErrorCode::NotFound);
        assert!(hub.agents.is_empty());
        assert!(!log.exists());
        let saved = crate::store::AgentsFile::load(&hub.state.agents_file()).unwrap();
        assert!(saved.agents.is_empty());
        let frame = frames.try_recv().unwrap();
        let sent = String::from_utf8_lossy(frame.as_bytes()).into_owned();
        assert!(sent.contains(r#""type":"agent_removed""#), "{sent}");
    }

    /// Once stopped, the process's stdin is closed: a prompt would be shown as sent and never
    /// reach `claude`.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stopping_agent_takes_no_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(&temp.path().join("state"));
        let claude = temp.path().join("claude");
        test_script::write_script(&claude, "#!/bin/sh\nexec cat >/dev/null\n");
        hub.config.claude_program = claude;
        let start = StartParams {
            folder: temp.path().to_owned(),
            title: None,
            model: None,
            effort: None,
            mode: None,
            prompt: None,
            account: AccountId::DEFAULT,
        };
        let Ok(ResponseBody::AgentStarted { agent }) = hub.start_agent(start) else {
            panic!("the agent did not start");
        };
        let items_before = hub.agents[&agent].view.items().len();

        hub.stop(agent).unwrap();
        let prompt = Prompt {
            text: "one more thing".to_owned(),
            images: Vec::new(),
        };
        let refused = hub
            .prompt(agent, prompt, PromptDelivery::Steer)
            .unwrap_err();

        assert_eq!(refused.code, ErrorCode::AgentExited);
        assert_eq!(hub.agents[&agent].view.items().len(), items_before);
    }
}
