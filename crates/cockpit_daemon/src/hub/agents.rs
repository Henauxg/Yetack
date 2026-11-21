//! An agent's life: start, prompts and their queue, answers, its process's events, stop, resume
//! and removal.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use claude_protocol::{ControlRequest, InputLine, OutputLine};
use cockpit_protocol::{
    AccountId, AgentId, AgentStatus, AgentSummary, AnswerParams, COMMAND_PREFIX, ClaudeSessionId,
    CostUsd, EffortSource, ErrorBody, ErrorCode, Event, MAX_TITLE_CHARS, Mark, MarkParams,
    Position, Prompt, PromptDelivery, PromptId, QueuedPromptParams, RenameParams, Repository,
    ResponseBody, ResponseResult, SetEffortParams, SignInWait, StartParams, TranscriptItem,
    WaitingState,
};
use tokio::runtime::Handle;
use tokio::time;

use crate::accounts::ClaudeEnv;
use crate::blocking::{self, BlockingError, Scope};
use crate::hub::input::{ControlPurpose, HubInput, Reply};
use crate::hub::state::{Agent, DetailsOut, Hub, QueuedPrompt};
use crate::hub::{errors, state};
use crate::process::{ClaudeCommand, InterruptedTurn, ProcessEvent};
use crate::sessions::FoundSession;
use crate::store::{AgentRecord, LogName, RawLog, ResumeAs};
use crate::transcript::prompts::TypedSetting;
use crate::transcript::{AgentView, AnswerError, FollowUp, ProcessSession, prompts, words};
use crate::{machine_load, read_file, repository, sessions};

/// The longest model name a client may set: every summary carries it.
const MODEL_MAX_CHARS: usize = 100;
/// How long reading the repository of an agent's folder may take.
const REPOSITORY_TIME: Duration = Duration::from_secs(10);

impl Hub {
    pub(super) fn start_restored_agents(&mut self) {
        let ids: Vec<AgentId> = self.agents.keys().copied().collect();
        for id in ids {
            self.read_repository(id);
            self.publish(id);
            let running = self
                .agents
                .get(&id)
                .is_some_and(|agent| agent.record.running);
            if running && let Err(error) = self.start_process(id, InterruptedTurn::Resume) {
                self.exited_at_start(id, &error.message);
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
        // A name the client gave goes into the Claude session too, as a rename does, so the
        // session keeps it after the agent closes, in the resume list and in `claude --resume`.
        let named = title.is_some();
        let title = title.unwrap_or_else(|| {
            cockpit_protocol::default_title(
                params.prompt.as_ref().map(|prompt| prompt.text.as_str()),
                &folder,
            )
        });
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
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Same,
        };
        let agent = self.add_agent(record, None, None)?;
        // Before the first message: the session has the name even if the agent stops before its
        // first turn ends.
        if named && let Some(started) = self.agents.get_mut(&agent) {
            let text = prompts::rename_command(&started.record.title);
            send_quiet_prompt(started, text);
        }
        if let Some(prompt) = params.prompt {
            self.prompt(agent, prompt, PromptDelivery::Steer)?;
        }
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
        let mut summary = initial_summary(&record);
        // Until its own read comes back, the agent shows the repository of another agent in its
        // folder.
        summary.repository = self
            .agents
            .values()
            .find(|other| other.record.folder == record.folder)
            .and_then(|other| other.view.summary().repository.clone());
        let mut view = AgentView::new(summary, self.settings.prompt_cache_ttl());
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
                moving: None,
                details_out: DetailsOut::default(),
            },
        );
        if let Err(error) = self.start_process(id, InterruptedTurn::Leave) {
            self.agents.remove(&id);
            return Err(error);
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
        self.read_repository(id);
        for place in places {
            self.publish(place.agent);
        }
        if let Some(prompt) = prompt {
            self.prompt(id, prompt, PromptDelivery::Steer)?;
        }
        Ok(id)
    }

    /// Starts `claude` for an agent and asks for its commands. The error says why it cannot
    /// start: `NotFound` when the agent's folder is gone, else `ClaudeFailed`.
    pub(super) fn start_process(
        &mut self,
        id: AgentId,
        interrupted_turn: InterruptedTurn,
    ) -> Result<(), ErrorBody> {
        let raw_path = self.raw_log_path(id);
        let account_id = self
            .agents
            .get(&id)
            .map(|agent| agent.record.account)
            .ok_or(errors::agent_not_found(id))?;
        let account = self.account(account_id).map_err(|_| {
            ErrorBody::new(
                ErrorCode::ClaudeFailed,
                format!("the agent's account {} no longer exists", account_id.get()),
            )
        })?;
        let Some(agent) = self.agents.get_mut(&id) else {
            return Err(errors::agent_not_found(id));
        };
        let record = &agent.record;
        check_folder(&record.folder)?;
        let command = ClaudeCommand {
            program: self.config.claude_program.clone(),
            folder: record.folder.clone(),
            model: record.model.clone(),
            mode: record.mode.map(words::mode_to_claude),
            effort: record.effort.and_then(words::effort_word),
            resume: record.session.clone(),
            resume_as: record.resume_as,
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
        .map_err(|error| ErrorBody::new(ErrorCode::ClaudeFailed, errors::chain(&error)))?;
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
            let deadline = time::Instant::now() + sessions::READ_TIME;
            let read = blocking::run(deadline, Scope::Sessions, move || {
                sessions::restored_cost(&folder, &session)
            })
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

    /// Reads the repository of the agent's folder in a task. A `.git` file the agent wrote can
    /// point into a mount that never answers, which would hold the hub.
    pub(super) fn read_repository(&mut self, id: AgentId) {
        let Some(agent) = self.agents.get(&id) else {
            return;
        };
        // Without a runtime, as in some tests, the agent keeps the repository it has.
        let Ok(runtime) = Handle::try_current() else {
            return;
        };
        let folder = agent.record.folder.clone();
        // A task already reads it, for another agent in the folder.
        if !self.repository_reads.insert(folder.clone()) {
            return;
        }
        let inputs = self.inputs.clone();
        runtime.spawn(async move {
            let deadline = time::Instant::now() + REPOSITORY_TIME;
            let read_folder = folder.clone();
            let read = blocking::run(deadline, Scope::Folder(&folder), move || {
                repository::of_folder(&read_folder)
            })
            .await;
            // The daemon may be stopping.
            let _ = inputs.send(HubInput::RepositoryRead { folder, read }).await;
        });
    }

    /// Gives the repository read for `folder` to the agents in it now. After a failed read, they
    /// keep the one they have.
    pub(super) fn repository_read(
        &mut self,
        folder: &Path,
        read: Result<Option<Repository>, BlockingError>,
    ) {
        self.repository_reads.remove(folder);
        let repository = match read {
            Ok(repository) => repository,
            Err(error) => {
                tracing::warn!(%error, "cannot read the repository of an agent's folder");
                return;
            }
        };
        let in_folder: Vec<AgentId> = self
            .agents
            .iter_mut()
            .filter(|(_, agent)| agent.record.folder == folder)
            .map(|(id, agent)| {
                agent.view.set_repository(repository.clone());
                *id
            })
            .collect();
        for agent in in_folder {
            self.publish(agent);
        }
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
            // A fork has its own session from now on.
            agent.record.resume_as = ResumeAs::Same;
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
        self.continue_move(id);
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
        let mark = agent.record.mark.after_prompt();
        let mut record_changed = mark != agent.record.mark;
        agent.record.mark = mark;
        agent.view.set_mark(mark);
        let busy = agent.view.is_busy() || !agent.queue.is_empty();
        let steers = delivery == PromptDelivery::Steer && !is_slash_command(&prompt.text);
        if !busy {
            agent.view.add_prompt(&prompt, state::now());
            process.write(line);
            record_changed |= record_typed_setting(agent, &prompt.text);
        } else if steers {
            agent
                .view
                .add_waiting_prompt(&prompt, WaitingState::Sent, state::now());
            process.write(line);
        } else {
            let id = agent
                .view
                .add_waiting_prompt(&prompt, WaitingState::Queued, state::now());
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
    pub(super) fn send_next_queued(&mut self, id: AgentId) {
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
                None => agent.view.hidden_prompt_sent(queued.prompt.text),
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
        let answered = match agent.view.answer(&params.pending, params.answer) {
            Ok(answered) => answered,
            // Claude Code has it from the first request.
            Err(AnswerError::AlreadyGiven(_)) => return Ok(ResponseBody::Done),
            Err(error @ AnswerError::NotFound(_)) => {
                return Err(ErrorBody::new(ErrorCode::NotFound, error.to_string()));
            }
            Err(error @ (AnswerError::Mismatch(_) | AnswerError::AlreadyAnswered(_))) => {
                return Err(errors::invalid(error.to_string()));
            }
        };
        if let Some(process) = &agent.process {
            process.write(answered.line);
        }
        agent.view.note_answer(state::now());
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
        // A move starts it again in its new folder once the session is there.
        if agent.moving.is_some() {
            return Err(errors::agent_working(id));
        }
        self.start_process(id, InterruptedTurn::Leave)?;
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
    /// session file and exits, or is killed after a delay. The agent's choices stay a moment, for
    /// an undo that resumes its session.
    pub(super) fn close(&mut self, id: AgentId) -> Result<ResponseBody, ErrorBody> {
        let agent = self.agents.get(&id).ok_or(errors::agent_not_found(id))?;
        self.closed_choices.keep(&agent.record, Instant::now());
        if agent.process.is_some() && agent.view.is_busy() {
            self.control(id, ControlRequest::Interrupt, ControlPurpose::Closing);
        }
        self.discard(id);
        Ok(ResponseBody::Done)
    }

    /// Takes the agent out of the hub, with its logs, and tells the clients.
    fn discard(&mut self, id: AgentId) {
        self.agents.remove(&id);
        self.close_agent_terminals(id);
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

    /// Marks the agent as one to handle, or clears the mark.
    pub(super) fn mark(&mut self, params: MarkParams) -> Result<ResponseBody, ErrorBody> {
        let id = params.agent;
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        agent.record.mark = params.mark;
        agent.view.set_mark(params.mark);
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

    /// A past session becomes an agent, with the choices of an agent closed with it a moment
    /// before `asked_at`, or the agent that has it resumes.
    pub(super) fn resume_session(&mut self, found: FoundSession, asked_at: Instant, reply: Reply) {
        respond(reply, self.agent_for_session(found, asked_at));
    }

    fn agent_for_session(
        &mut self,
        found: FoundSession,
        asked_at: Instant,
    ) -> Result<ResponseBody, ErrorBody> {
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
        let folder = agent_folder(&session.folder)?;
        let choices = self.closed_choices.find(&session.id, account.id, asked_at);
        let record = AgentRecord {
            id: self.next_agent,
            title: session.title,
            folder,
            session: Some(session.id.clone()),
            model: choices.model,
            effort: choices.effort,
            mode: choices.mode,
            running: true,
            started_at: state::now(),
            past: Some(past.file),
            position: Position::default(),
            process: None,
            account: account.id,
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Same,
        };
        let agent = self.add_agent(record, None, Some(past.items))?;
        self.closed_choices.forget(&session.id, account.id);
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

fn is_slash_command(text: &str) -> bool {
    text.starts_with(COMMAND_PREFIX)
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
        agent.view.hidden_prompt_sent(text);
    }
}

/// Sends a hidden prompt whose output shows no notice, now if the agent is idle. Queued behind a
/// turn, it goes as any hidden prompt does, and its output shows.
fn send_quiet_prompt(agent: &mut Agent, text: String) {
    match &agent.process {
        Some(process) if !agent.view.is_busy() && agent.queue.is_empty() => {
            process.write(InputLine::prompt(text.clone()));
            agent.view.quiet_prompt_sent(text);
        }
        Some(_) | None => send_hidden_prompt(agent, text),
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
        quiet_since: None,
        turn_started_at: None,
        session: record.session.clone(),
        position: record.position,
        account: record.account,
        sign_in_wait: SignInWait::NotWaiting,
        background_running: 0,
        mark: record.mark,
        // Read off the hub: `read_repository`.
        repository: None,
        claude_code_version: None,
    }
}

/// An existing folder, absolute, with its symlinks resolved. On Windows it keeps the form a user
/// writes, `C:\work` and not `\\?\C:\work`: Claude Code records the session under it, and the
/// recent folders show it.
pub(super) fn agent_folder(folder: &Path) -> Result<PathBuf, ErrorBody> {
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

/// Refuses a folder that is gone, as after a rename, with `NotFound`, so that a client can offer
/// to move the agent. Windows would only say that the directory name is not valid.
fn check_folder(folder: &Path) -> Result<(), ErrorBody> {
    match std::fs::metadata(folder) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(ErrorBody::new(
            ErrorCode::NotFound,
            format!("{} is no longer a folder", folder.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(ErrorBody::new(
            ErrorCode::NotFound,
            format!("the folder {} no longer exists", folder.display()),
        )),
        Err(error) => Err(ErrorBody::new(
            ErrorCode::ClaudeFailed,
            format!("cannot open the folder {}: {error}", folder.display()),
        )),
    }
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

    /// The agent's folder was renamed since it ran: the resume names the folder with `NotFound`,
    /// so that the app offers to move the agent, and the agent stays exited.
    #[test]
    fn an_agent_whose_folder_is_gone_does_not_resume() {
        use cockpit_protocol::Timestamp;

        use crate::store::{AgentsFile, StateDir};

        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let state = StateDir::create(&state_dir).unwrap();
        let agent = AgentId::new(1);
        let gone = temp.path().join("renamed-project");
        AgentsFile {
            next_agent: AgentId::new(2),
            replay_floor: None,
            agents: vec![AgentRecord {
                id: agent,
                title: "Fix the cart".to_owned(),
                folder: gone.clone(),
                session: Some(ClaudeSessionId::new(
                    "11111111-2222-3333-4444-555555555555".to_owned(),
                )),
                model: None,
                effort: None,
                mode: None,
                running: false,
                started_at: Timestamp::from_unix_millis(0),
                past: None,
                position: Position::default(),
                process: None,
                account: AccountId::DEFAULT,
                mark: Mark::Unmarked,
                resume_as: ResumeAs::Same,
            }],
        }
        .save(&state.agents_file())
        .unwrap();
        let mut hub = Hub::for_tests(&state_dir);

        let refused = hub.resume(agent).unwrap_err();

        assert_eq!(refused.code, ErrorCode::NotFound);
        assert_eq!(
            refused.message,
            format!("the folder {} no longer exists", gone.display())
        );
        assert!(hub.agents[&agent].process.is_none());
        assert!(!hub.agents[&agent].record.running);
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

        let relative = hub
            .agent_for_session(session("work"), Instant::now())
            .unwrap_err();
        let missing = hub
            .agent_for_session(session("/nonexistent/yetack/work"), Instant::now())
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

    /// The mark is saved with the agent, and a prompt clears it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_mark_is_saved_and_a_prompt_clears_it() {
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
        let saved = |hub: &Hub| {
            let file = crate::store::AgentsFile::load(&hub.state.agents_file()).unwrap();
            file.agents
                .iter()
                .any(|record| record.mark == Mark::ToHandle)
        };

        hub.mark(MarkParams {
            agent,
            mark: Mark::ToHandle,
        })
        .unwrap();
        let marked = (hub.agents[&agent].view.summary().mark, saved(&hub));
        let prompt = Prompt {
            text: "go on".to_owned(),
            images: Vec::new(),
        };
        hub.prompt(agent, prompt, PromptDelivery::Steer).unwrap();
        let unknown = hub
            .mark(MarkParams {
                agent: AgentId::new(9),
                mark: Mark::ToHandle,
            })
            .unwrap_err();

        assert_eq!(marked, (Mark::ToHandle, true));
        assert_eq!(hub.agents[&agent].view.summary().mark, Mark::Unmarked);
        assert!(!saved(&hub));
        assert_eq!(unknown.code, ErrorCode::NotFound);
    }

    /// A mark kept until you clear it stays through a prompt.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_kept_mark_stays_through_a_prompt() {
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
        hub.mark(MarkParams {
            agent,
            mark: Mark::ToHandleKept,
        })
        .unwrap();
        let prompt = Prompt {
            text: "go on".to_owned(),
            images: Vec::new(),
        };

        hub.prompt(agent, prompt, PromptDelivery::Steer).unwrap();

        assert_eq!(hub.agents[&agent].view.summary().mark, Mark::ToHandleKept);
        assert_eq!(hub.agents[&agent].record.mark, Mark::ToHandleKept);
    }

    /// A queued prompt waits for the sent ones. Once a stop releases them, it starts a turn.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stop_that_releases_the_sent_prompts_starts_the_queued_one() {
        use cockpit_protocol::ItemBody;
        use serde_json::json;
        use tokio::sync::oneshot;

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
        let prompt = |text: &str| Prompt {
            text: text.to_owned(),
            images: Vec::new(),
        };
        hub.prompt(agent, prompt("First"), PromptDelivery::Steer)
            .unwrap();
        hub.prompt(agent, prompt("Lost"), PromptDelivery::Steer)
            .unwrap();
        hub.prompt(agent, prompt("Then this"), PromptDelivery::Queue)
            .unwrap();
        let result = json!({
            "type": "result", "subtype": "success", "is_error": false, "duration_ms": 1500,
            "num_turns": 1, "total_cost_usd": 0.1,
            "usage": {"input_tokens": 10, "output_tokens": 20,
                      "cache_creation_input_tokens": 30, "cache_read_input_tokens": 40}
        });
        hub.claude_line(agent, result.to_string().parse().unwrap());
        let (reply, _answer) = oneshot::channel();

        hub.control_answered(
            agent,
            ControlPurpose::Interrupt(reply),
            Ok(json!({"still_queued": []})),
        );

        let view = &hub.agents[&agent].view;
        assert!(hub.agents[&agent].queue.is_empty());
        assert!(view.details().waiting_prompts.is_empty());
        assert_eq!(view.summary().status, AgentStatus::Working);
        assert!(view.items().iter().any(|item| matches!(
            &item.body,
            ItemBody::UserMessage(message) if message.text == "Then this"
        )));
    }

    /// The new agent's `AgentAdded` carries the repository of another agent in its folder, before
    /// the read of its own comes back.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_agent_takes_the_repository_of_another_agent_in_its_folder() {
        use tokio::sync::mpsc;

        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(&temp.path().join("state"));
        let claude = temp.path().join("claude");
        test_script::write_script(&claude, "#!/bin/sh\nexec cat >/dev/null\n");
        hub.config.claude_program = claude;
        let folder = temp.path().join("shop");
        std::fs::create_dir_all(&folder).unwrap();
        let start = || StartParams {
            folder: folder.clone(),
            title: None,
            model: None,
            effort: None,
            mode: None,
            prompt: None,
            account: AccountId::DEFAULT,
        };
        let Ok(ResponseBody::AgentStarted { agent: first }) = hub.start_agent(start()) else {
            panic!("the first agent did not start");
        };
        let shop = Repository::from_remote("github.com/acme/shop");
        if let Some(agent) = hub.agents.get_mut(&first) {
            agent.view.set_repository(Some(shop.clone()));
        }
        let (client, mut frames) = mpsc::channel(16);
        hub.clients.push(client);

        let Ok(ResponseBody::AgentStarted { agent: second }) = hub.start_agent(start()) else {
            panic!("the second agent did not start");
        };

        assert_eq!(hub.agents[&second].view.summary().repository, Some(shop));
        let mut added = None;
        while let Ok(frame) = frames.try_recv() {
            let sent = String::from_utf8_lossy(frame.as_bytes()).into_owned();
            if sent.contains(r#""type":"agent_added""#) {
                added = Some(sent);
            }
        }
        let added = added.expect("the clients got the new agent");
        assert!(
            added.contains(r#""repository":"github.com/acme/shop""#),
            "{added}"
        );
    }

    /// A restored agent shows the repository of its last summary until the read of its folder
    /// comes back.
    #[tokio::test]
    async fn a_restored_agent_keeps_its_repository_until_its_folder_is_read() {
        use cockpit_protocol::{EventRecord, Frame, Seq, ServerMessage, Timestamp};

        use crate::store::{AgentsFile, EventLogs, StateDir};

        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("shop");
        std::fs::create_dir_all(folder.join(".git")).unwrap();
        std::fs::write(
            folder.join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:acme/shop.git\n",
        )
        .unwrap();
        let state_dir = temp.path().join("state");
        let state = StateDir::create(&state_dir).unwrap();
        let agent = AgentId::new(1);
        let record = AgentRecord {
            id: agent,
            title: "Fix the cart".to_owned(),
            folder,
            session: None,
            model: None,
            effort: None,
            mode: None,
            running: false,
            started_at: Timestamp::from_unix_millis(0),
            past: None,
            position: Position::default(),
            process: None,
            account: AccountId::DEFAULT,
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Same,
        };
        let mut logged = initial_summary(&record);
        logged.repository = Some(Repository::from_remote("github.com/acme/app"));
        let added = Frame::encode(&ServerMessage::Event(EventRecord {
            seq: Seq::new(1),
            event: Event::AgentAdded(logged),
        }))
        .unwrap();
        EventLogs::new(state.events(), state.details())
            .append(LogName::Agent(agent), &added)
            .unwrap();
        AgentsFile {
            next_agent: AgentId::new(2),
            replay_floor: None,
            agents: vec![record],
        }
        .save(&state.agents_file())
        .unwrap();
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);
        let repository = |hub: &Hub| {
            let summary = hub.agents[&agent].view.summary();
            summary
                .repository
                .as_ref()
                .map(|read| read.as_str().to_owned())
        };

        hub.start_restored_agents();
        let before_the_read = repository(&hub);
        while repository(&hub) == before_the_read {
            let input = tokio::time::timeout(Duration::from_secs(10), inputs.recv())
                .await
                .expect("the repository came in time")
                .unwrap();
            hub.handle(input);
        }

        assert_eq!(before_the_read.as_deref(), Some("github.com/acme/app"));
        assert_eq!(repository(&hub).as_deref(), Some("github.com/acme/shop"));
    }

    /// Many agents in a folder on a mount that does not answer would otherwise hold a thread each.
    #[tokio::test]
    async fn the_agents_of_one_folder_share_one_read_of_its_repository() {
        use cockpit_protocol::Timestamp;

        use crate::store::{AgentsFile, StateDir};

        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("shop");
        std::fs::create_dir_all(folder.join(".git")).unwrap();
        std::fs::write(
            folder.join(".git").join("config"),
            "[remote \"origin\"]\n\turl = git@github.com:acme/shop.git\n",
        )
        .unwrap();
        let state_dir = temp.path().join("state");
        let state = StateDir::create(&state_dir).unwrap();
        let record = |id: u32| AgentRecord {
            id: AgentId::new(id),
            title: format!("Agent {id}"),
            folder: folder.clone(),
            session: None,
            model: None,
            effort: None,
            mode: None,
            running: false,
            started_at: Timestamp::from_unix_millis(0),
            past: None,
            position: Position::default(),
            process: None,
            account: AccountId::DEFAULT,
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Same,
        };
        AgentsFile {
            next_agent: AgentId::new(3),
            replay_floor: None,
            agents: vec![record(1), record(2)],
        }
        .save(&state.agents_file())
        .unwrap();
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);

        hub.start_restored_agents();
        let reads = hub.repository_reads.len();
        let input = tokio::time::timeout(Duration::from_secs(10), inputs.recv())
            .await
            .expect("the repository came in time")
            .unwrap();
        assert!(matches!(input, HubInput::RepositoryRead { .. }));
        hub.handle(input);

        assert_eq!(reads, 1, "one read for the folder");
        assert!(hub.repository_reads.is_empty());
        for agent in [AgentId::new(1), AgentId::new(2)] {
            let summary = hub.agents[&agent].view.summary();
            assert_eq!(
                summary.repository.as_ref().map(Repository::as_str),
                Some("github.com/acme/shop")
            );
        }
    }
}
