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
    PermissionMode, Position, Prompt, PromptDelivery, PromptId, QueuedPromptParams, RenameParams,
    Repository, ResponseBody, ResponseResult, SessionUse, SetEffortParams, SignInWait, StartParams,
    Timestamp, TranscriptItem, WaitingState,
};
use tokio::runtime::Handle;
use tokio::time;

use crate::blocking::{self, BlockingError, Scope};
use crate::hub::closed_choices::{Choices, Found};
use crate::hub::input::{ControlPurpose, HubInput, Reply};
use crate::hub::start::{AfterStart, FolderRequest, Opening};
use crate::hub::state::{Agent, DetailsOut, Hub, QueuedPrompt};
use crate::hub::{errors, start, state};
use crate::process::{InterruptedTurn, ProcessEvent, STOP_TIMEOUT, Spawned};
use crate::sessions::{FoundSession, SessionPast};
use crate::store::{AgentRecord, LogName, ResumeAs};
use crate::transcript::prompts::TypedSetting;
use crate::transcript::{AgentView, AnswerError, FollowUp, prompts, words};
use crate::{repository, sessions};

/// The longest model name a client may set: every summary carries it.
const MODEL_MAX_CHARS: usize = 100;
/// How long reading the repository of an agent's folder may take.
const REPOSITORY_TIME: Duration = Duration::from_secs(10);
/// The notice of an agent stopped while its `claude` started.
const STOPPED_WHILE_STARTING: &str = "stopped before claude started";

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
            if running {
                // The first snapshot shows it running, as when `claude` started before the hub
                // served any client.
                if let Some(agent) = self.agents.get_mut(&id) {
                    agent
                        .view
                        .process_started(start::process_session(&agent.record));
                }
                self.start_process(id, InterruptedTurn::Resume, AfterStart::Restored);
                self.publish(id);
            }
        }
    }

    /// Checks what needs no file system, then resolves the folder off the hub. The agent starts
    /// in [`Hub::start_agent_in`].
    pub(super) fn start_agent(&mut self, mut params: StartParams, reply: Reply) {
        let title = params.title.as_deref().map(client_title).transpose();
        let model = params.model.as_deref().map(check_model).transpose();
        match (title, model) {
            (Ok(title), Ok(_)) => params.title = title,
            (Err(error), _) | (_, Err(error)) => return respond(reply, Err(error)),
        }
        let folder = params.folder.clone();
        self.resolve_folder(folder, FolderRequest::Start(params), reply);
    }

    /// Starts a new agent in `folder`, resolved from the request's.
    pub(super) fn start_agent_in(&mut self, params: StartParams, folder: PathBuf, reply: Reply) {
        let account = match self.account(params.account) {
            Ok(account) => account,
            Err(error) => return respond(reply, Err(error)),
        };
        // A name the client gave goes into the Claude session too, as a rename does, so the
        // session keeps it after the agent closes, in the resume list and in `claude --resume`.
        let named = params.title.is_some();
        let title = params.title.unwrap_or_else(|| {
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
        let opening = Opening::Start {
            named,
            prompt: params.prompt,
        };
        self.start_new_agent(record, None, opening, reply);
    }

    /// Lists a new agent on top of the others, with the process that started for it. `past` is
    /// the transcript of the session the agent takes over, which shows above its own items
    /// without any event: clients read it with `agent.history`.
    pub(super) fn add_agent(
        &mut self,
        mut record: AgentRecord,
        past: Option<Vec<TranscriptItem>>,
        spawned: Spawned<ControlPurpose>,
    ) {
        let id = record.id;
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
                generation: start::FIRST_GENERATION,
                starting: true,
                queue: VecDeque::new(),
                moving: None,
                details_out: DetailsOut::default(),
            },
        );
        self.attach(id, spawned);
        // The agents below the new one, renumbered when it found no room above them.
        self.set_places(&places);
        self.save_agents();
        let Some(agent) = self.agents.get_mut(&id) else {
            return;
        };
        agent.view.mark_published();
        let summary = agent.view.summary().clone();
        self.emit(LogName::Agent(id), Event::AgentAdded(summary));
        self.read_repository(id);
        for place in places {
            self.publish(place.agent);
        }
    }

    /// What a new agent does once it is listed, before its request's answer.
    pub(super) fn open_agent(&mut self, id: AgentId, opening: Opening) -> Result<(), ErrorBody> {
        match opening {
            Opening::Start { named, prompt } => {
                // Before the first message: the session has the name even if the agent stops
                // before its first turn ends.
                if named && let Some(started) = self.agents.get_mut(&id) {
                    let text = prompts::rename_command(&started.record.title);
                    send_quiet_prompt(started, text);
                }
                if let Some(prompt) = prompt {
                    self.prompt(id, prompt, PromptDelivery::Steer)?;
                }
            }
            Opening::Session => {
                if let Some(agent) = self.agents.get(&id)
                    && let Some(session) = &agent.record.session
                {
                    self.closed_choices.forget(session, agent.record.account);
                }
            }
            Opening::Fork => {}
        }
        Ok(())
    }

    /// Reads the cost Claude Code restores from the session file, in a task, for the first turn's
    /// cost.
    /// `folder` is the agent's account's, which holds the session file.
    pub(super) fn read_restored_cost(
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

    pub(super) fn exited_at_start(&mut self, id: AgentId, error: &str) {
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
            agent.view.set_session_use(SessionUse::Own);
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

    /// Sends a prompt now, or queues it while the agent works, as `delivery` asks. While `claude`
    /// starts, every prompt waits in the queue.
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
        let process = agent.process.as_ref().filter(|_| agent.record.running);
        if process.is_none() && !agent.starting {
            return Err(errors::agent_exited(id));
        }
        let line = prompts::prompt_line(&prompt)
            .ok_or_else(|| errors::internal("cannot encode the prompt".to_owned()))?;
        let mark = agent.record.mark.after_prompt();
        let mut record_changed = mark != agent.record.mark;
        agent.record.mark = mark;
        agent.view.set_mark(mark);
        let busy = agent.view.is_busy() || !agent.queue.is_empty();
        let steers = delivery == PromptDelivery::Steer && !is_slash_command(&prompt.text);
        match process {
            Some(process) if !busy => {
                agent.view.add_prompt(&prompt, state::now());
                process.write(line);
                record_changed |= record_typed_setting(agent, &prompt.text);
            }
            Some(process) if steers => {
                agent
                    .view
                    .add_waiting_prompt(&prompt, WaitingState::Sent, state::now());
                process.write(line);
            }
            Some(_) | None => {
                let id = agent
                    .view
                    .add_waiting_prompt(&prompt, WaitingState::Queued, state::now());
                agent.queue.push_back(QueuedPrompt {
                    id: Some(id),
                    line,
                    prompt,
                });
            }
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

    /// Closes the process's stdin; the agent shows as exited once the process ends. A stop while
    /// `claude` starts gives the start up.
    pub(super) fn stop(&mut self, id: AgentId) -> Result<ResponseBody, ErrorBody> {
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        if agent.starting {
            // The process of that start, if it comes, finds another generation and is killed.
            agent.generation = agent.generation.wrapping_add(1);
            agent.starting = false;
            agent.queue.clear();
            agent
                .view
                .process_exited(STOPPED_WHILE_STARTING.to_owned(), state::now());
            agent.record.running = false;
            self.save_agents();
            self.publish(id);
            return Ok(ResponseBody::Done);
        }
        let Some(process) = &agent.process else {
            return Err(errors::agent_exited(id));
        };
        process.stop();
        agent.record.running = false;
        self.save_agents();
        Ok(ResponseBody::Done)
    }

    /// Starts the exited agent again. `reply` gets `answer` once `claude` runs.
    pub(super) fn resume(&mut self, id: AgentId, answer: ResponseBody, reply: Reply) {
        let Some(agent) = self.agents.get(&id) else {
            return respond(reply, Err(errors::agent_not_found(id)));
        };
        if agent.process.is_some() || agent.starting {
            return respond(reply, Err(errors::agent_running(id)));
        }
        // A move starts it again in its new folder once the session is there.
        if agent.moving.is_some() {
            return respond(reply, Err(errors::agent_working(id)));
        }
        let then = AfterStart::Resume { answer, reply };
        self.start_process(id, InterruptedTurn::Leave, then);
    }

    /// Answers `sessions.resume` for the agent that has the session, which runs again unless it
    /// runs or starts.
    pub(super) fn resume_for_session(&mut self, id: AgentId, reply: Reply) {
        let answer = ResponseBody::AgentStarted { agent: id };
        let exited = self
            .agents
            .get(&id)
            .is_some_and(|agent| agent.process.is_none() && !agent.starting);
        if exited {
            self.resume(id, answer, reply);
        } else {
            respond(reply, Ok(answer));
        }
    }

    pub(super) fn remove(&mut self, id: AgentId) -> Result<ResponseBody, ErrorBody> {
        let agent = self.agents.get(&id).ok_or(errors::agent_not_found(id))?;
        if agent.process.is_some() || agent.starting {
            return Err(errors::agent_running(id));
        }
        self.discard(id);
        Ok(ResponseBody::Done)
    }

    /// Removes the agent whatever its process does, interrupting a working agent's turn first.
    /// The process handle leaves with the agent, which closes `claude`'s stdin: `claude` ends its
    /// session file and exits, or is killed after a delay. The agent's choices stay, for a resume
    /// of its session.
    pub(super) fn close(&mut self, id: AgentId) -> Result<ResponseBody, ErrorBody> {
        let agent = self.agents.get(&id).ok_or(errors::agent_not_found(id))?;
        self.closed_choices
            .keep(&agent.record, Instant::now(), state::now());
        if agent.process.is_some() && agent.view.is_busy() {
            self.control(id, ControlRequest::Interrupt, ControlPurpose::Closing);
        }
        self.discard(id);
        Ok(ResponseBody::Done)
    }

    /// Takes the agent out of the hub, with its logs, and tells the clients. A move that waits
    /// for its process gets `NotFound`.
    fn discard(&mut self, id: AgentId) {
        let moving = self.agents.remove(&id).and_then(|agent| agent.moving);
        if let Some(moving) = moving {
            moving.agent_gone(id);
        }
        self.close_agent_terminals(id);
        if let Err(error) = self.logs.remove(LogName::Agent(id)) {
            tracing::warn!(error = %errors::chain(&error), "cannot delete an agent's log");
        }
        self.remove_raw_log(id);
        // The agent's events left with its log: a client that saw less cannot replay.
        self.replay_floor = self.last_seq;
        self.save_agents();
        self.emit(LogName::Daemon, Event::AgentRemoved { agent: id });
    }

    pub(super) fn remove_raw_log(&self, id: AgentId) {
        if let Err(error) = std::fs::remove_file(self.raw_log_path(id))
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "cannot delete an agent's raw log");
        }
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

    /// A past session becomes an agent in `folder`, resolved from the session's, with the choices
    /// of an agent closed with it a moment before `asked_at`. The agent that has the session
    /// resumes instead, and the new agent that starts on it answers once it runs.
    pub(super) fn resume_session(
        &mut self,
        found: FoundSession,
        folder: Result<PathBuf, ErrorBody>,
        asked_at: Instant,
        reply: Reply,
    ) {
        let FoundSession { session, past } = found;
        let account = match self.account(session.account) {
            Ok(account) => account,
            Err(error) => return respond(reply, Err(error)),
        };
        if let Some(agent) = self.agent_with_session(&session.id, account.id) {
            return self.resume_for_session(agent, reply);
        }
        let starting = self
            .new_agents
            .values_mut()
            .find(|starting| has_session(&starting.record, &session.id, account.id));
        if let Some(starting) = starting {
            starting.also_waiting.push(reply);
            return;
        }
        let folder = match folder {
            Ok(folder) => folder,
            Err(error) => return respond(reply, Err(error)),
        };
        let kept = self.closed_choices.find(&session.id, account.id, asked_at);
        let choices = resumed_choices(kept, &past);
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
        self.start_new_agent(record, Some(past.items), Opening::Session, reply);
    }

    /// The listed agent that has the session of the account's folder.
    pub(super) fn agent_with_session(
        &self,
        session: &ClaudeSessionId,
        account: AccountId,
    ) -> Option<AgentId> {
        self.agents
            .values()
            .find(|agent| has_session(&agent.record, session, account))
            .map(|agent| agent.record.id)
    }
}

/// The choices a past session goes on with, from the newer of two sources:
/// - the model, effort and mode its last agent had at its close, which the daemon keeps;
/// - the mode of the session file's last prompt, when no agent of the daemon closed it since, as
///   when the session went on in a terminal.
///
/// An undo always gets the kept choices. Later, the file's prompt wins only when it came after the
/// closed `claude` had to exit, [`STOP_TIMEOUT`] after the close: until then, `claude` may still
/// write lines of its own.
///
/// The bypass permissions mode comes back only for an undo, so that a later resume never skips the
/// permission checks unasked. From the file, the plan mode gives the default mode too: the file
/// does not record a plan's approval, after which Claude Code leaves the plan mode. A recorded
/// mode goes as a flag even when it is the default one: without a flag, `claude` would take the
/// settings' default mode, which may let more through.
fn resumed_choices(kept: Option<Found>, past: &SessionPast) -> Choices {
    let stop_millis = u64::try_from(STOP_TIMEOUT.as_millis()).unwrap_or(u64::MAX);
    let prompted_after = |closed_at: Timestamp| {
        let exited = closed_at.unix_millis().saturating_add(stop_millis);
        past.last_prompt_at
            .is_some_and(|at| at.unix_millis() > exited)
    };
    match kept {
        Some(kept) if kept.undo || !prompted_after(kept.closed_at) => {
            let mode = if kept.undo {
                kept.choices.mode
            } else {
                kept.choices.mode.map(|mode| match mode {
                    PermissionMode::BypassPermissions => PermissionMode::Default,
                    other => other,
                })
            };
            Choices {
                mode,
                ..kept.choices
            }
        }
        _ => Choices {
            mode: past.mode.map(|mode| match mode {
                PermissionMode::BypassPermissions | PermissionMode::Plan => PermissionMode::Default,
                other => other,
            }),
            ..Choices::default()
        },
    }
}

/// Whether the agent of `record` has the session of the account's folder. A fork names the
/// session it forks until its `claude` reports its own, and does not have it.
fn has_session(record: &AgentRecord, session: &ClaudeSessionId, account: AccountId) -> bool {
    record.session.as_ref() == Some(session)
        && record.account == account
        && record.resume_as == ResumeAs::Same
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

/// Sends a prompt the transcript does not show, such as `/effort high`, now, after the current
/// turn, or once `claude` runs. An exited agent gets nothing.
fn send_hidden_prompt(agent: &mut Agent, text: String) {
    if agent.process.is_none() && !agent.starting {
        return;
    }
    let line = InputLine::prompt(text.clone());
    match &agent.process {
        Some(process) if !agent.view.is_busy() && agent.queue.is_empty() => {
            process.write(line);
            agent.view.hidden_prompt_sent(text);
        }
        Some(_) | None => agent.queue.push_back(QueuedPrompt {
            id: None,
            line,
            prompt: Prompt {
                text,
                images: Vec::new(),
            },
        }),
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
        session_use: SessionUse::from(record.resume_as),
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
    use cockpit_protocol::{AgentParams, ItemBody, PastSession, Request, Timestamp};
    use tokio::sync::{mpsc, oneshot};

    use crate::sessions::SessionPast;
    use crate::store::{AgentsFile, PastFile, StateDir};
    #[cfg(unix)]
    use crate::test_script;

    use super::*;

    const SESSION: &str = "11111111-2222-3333-4444-555555555555";

    fn start_params(folder: &Path) -> StartParams {
        StartParams {
            folder: folder.to_owned(),
            title: None,
            model: None,
            effort: None,
            mode: None,
            prompt: None,
            account: AccountId::DEFAULT,
        }
    }

    fn prompt(text: &str) -> Prompt {
        Prompt {
            text: text.to_owned(),
            images: Vec::new(),
        }
    }

    /// The notices of the agent's transcript, with their details, in order.
    fn notices(view: &AgentView) -> Vec<(String, Option<String>)> {
        view.items()
            .iter()
            .filter_map(|item| match &item.body {
                ItemBody::Notice { text, detail, .. } => Some((text.clone(), detail.clone())),
                _ => None,
            })
            .collect()
    }

    /// A hub whose `claude` reads its stdin and says nothing, and the inputs its tasks send it.
    #[cfg(unix)]
    fn quiet_hub(temp: &Path) -> (Hub, mpsc::Receiver<HubInput>) {
        let (mut hub, inputs) = Hub::for_tests_with_inputs(&temp.join("state"));
        let claude = temp.join("claude");
        test_script::write_script(&claude, "#!/bin/sh\nexec cat >/dev/null\n");
        hub.config.claude_program = claude;
        (hub, inputs)
    }

    /// Starts an agent in `folder`, handing the hub its inputs until it answers.
    #[cfg(unix)]
    async fn started(
        hub: &mut Hub,
        inputs: &mut mpsc::Receiver<HubInput>,
        folder: &Path,
    ) -> AgentId {
        match hub
            .ask(inputs, Request::AgentStart(start_params(folder)))
            .await
        {
            Ok(ResponseBody::AgentStarted { agent }) => agent,
            other => panic!("the agent did not start: {other:?}"),
        }
    }

    /// A state folder at `state_dir` with one agent, 1, on [`SESSION`] in `folder`.
    fn state_with_agent(state_dir: &Path, folder: &Path, running: bool) -> AgentId {
        let state = StateDir::create(state_dir).unwrap();
        let agent = AgentId::new(1);
        AgentsFile {
            next_agent: AgentId::new(2),
            replay_floor: None,
            agents: vec![AgentRecord {
                id: agent,
                title: "Fix the cart".to_owned(),
                folder: folder.to_owned(),
                session: Some(ClaudeSessionId::new(SESSION.to_owned())),
                model: None,
                effort: None,
                mode: None,
                running,
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
        agent
    }

    /// The past session [`SESSION`] in `folder`, found on disk.
    fn past_session(folder: &Path) -> FoundSession {
        let id = ClaudeSessionId::new(SESSION.to_owned());
        FoundSession {
            session: PastSession {
                id: id.clone(),
                title: "Fix the cart".to_owned(),
                folder: folder.to_owned(),
                last_activity: Timestamp::from_unix_millis(0),
                turns: 1,
                account: AccountId::DEFAULT,
            },
            past: SessionPast {
                items: Vec::new(),
                file: PastFile {
                    session: id,
                    bytes: 0,
                },
                mode: None,
                last_prompt_at: None,
            },
        }
    }

    fn ask_resume(hub: &mut Hub, agent: AgentId) -> oneshot::Receiver<ResponseResult> {
        let (reply, answered) = oneshot::channel();
        hub.request(Request::AgentResume(AgentParams { agent }), reply);
        answered
    }

    async fn next_input(inputs: &mut mpsc::Receiver<HubInput>) -> HubInput {
        tokio::time::timeout(Duration::from_secs(10), inputs.recv())
            .await
            .expect("an input came in time")
            .expect("the hub's inputs are open")
    }

    /// Hands the hub its inputs until a start of `claude` comes back, which it returns unhandled.
    async fn next_start(hub: &mut Hub, inputs: &mut mpsc::Receiver<HubInput>) -> HubInput {
        loop {
            let input = next_input(inputs).await;
            if matches!(input, HubInput::ProcessStarted { .. }) {
                return input;
            }
            hub.handle(input);
        }
    }

    /// Whether `check` holds within 10 seconds.
    #[cfg(unix)]
    async fn eventually(check: impl Fn() -> bool) -> bool {
        for _ in 0..1000 {
            if check() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Titles and model names go into every snapshot, which must stay under the framing maximum.
    #[tokio::test]
    async fn a_title_or_a_model_name_too_long_is_refused() {
        use cockpit_protocol::SetModelParams;

        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&temp.path().join("state"));
        let params = |title: usize, model: usize| {
            Request::AgentStart(StartParams {
                title: Some("t".repeat(title)),
                model: Some("m".repeat(model)),
                ..start_params(temp.path())
            })
        };

        let long_title = hub.ask(&mut inputs, params(MAX_TITLE_CHARS + 1, 1)).await;
        let long_model = hub.ask(&mut inputs, params(1, MODEL_MAX_CHARS + 1)).await;
        // The test's `claude` cannot start: the request got past the checks.
        let longest = hub
            .ask(&mut inputs, params(MAX_TITLE_CHARS, MODEL_MAX_CHARS))
            .await;
        let (reply, mut answer) = oneshot::channel();
        let set_model = SetModelParams {
            agent: AgentId::new(1),
            model: "m".repeat(MODEL_MAX_CHARS + 1),
        };
        hub.set_model(set_model, reply);

        assert_eq!(long_title.unwrap_err().code, ErrorCode::InvalidRequest);
        assert_eq!(long_model.unwrap_err().code, ErrorCode::InvalidRequest);
        assert_eq!(longest.unwrap_err().code, ErrorCode::ClaudeFailed);
        assert!(hub.agents.is_empty() && hub.new_agents.is_empty());
        let ResponseResult::Error(refused) = answer.try_recv().unwrap() else {
            panic!("expected the model name to be refused");
        };
        assert_eq!(refused.code, ErrorCode::InvalidRequest);
    }

    /// The agent's folder was renamed since it ran: the resume names the folder with `NotFound`,
    /// so that the app offers to move the agent, and the agent stays exited.
    #[tokio::test]
    async fn an_agent_whose_folder_is_gone_does_not_resume() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let gone = temp.path().join("renamed-project");
        let agent = state_with_agent(&state_dir, &gone, false);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);

        let answered = ask_resume(&mut hub, agent);
        let refused = hub.await_answer(&mut inputs, answered).await.unwrap_err();

        assert_eq!(refused.code, ErrorCode::NotFound);
        assert_eq!(
            refused.message,
            format!("the folder {} no longer exists", gone.display())
        );
        let exited = &hub.agents[&agent];
        assert!(exited.process.is_none() && !exited.starting);
        assert!(!exited.record.running);
    }

    /// A session file's `cwd` is what `claude` wrote, or anything a process of the user wrote.
    #[tokio::test]
    async fn a_past_session_needs_a_folder_an_agent_can_start_in() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(&temp.path().join("state"));
        let relative = PathBuf::from("work");
        let missing = temp.path().join("missing").join("work");

        let mut codes = Vec::new();
        for folder in [relative, missing] {
            let resolved = start::resolve(folder.clone()).await;
            let (reply, mut answered) = oneshot::channel();
            hub.resume_session(past_session(&folder), resolved, Instant::now(), reply);
            match answered.try_recv() {
                Ok(ResponseResult::Error(error)) => codes.push(error.code),
                other => panic!("expected the folder to be refused, got {other:?}"),
            }
        }

        assert_eq!(codes, [ErrorCode::InvalidRequest, ErrorCode::NotFound]);
        assert!(hub.agents.is_empty() && hub.new_agents.is_empty());
    }

    /// A folder on a mount that never answers holds its reads: earlier ones never ended. The
    /// restored agent, its resume and a new agent there fail in time, and the hub goes on.
    #[tokio::test]
    async fn a_start_in_a_folder_that_does_not_answer_fails() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let releases = blocking::hold_folder(&folder).await;
        let agent = state_with_agent(&state_dir, &folder, true);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);

        hub.start_restored_agents();
        let restored = next_start(&mut hub, &mut inputs).await;
        hub.handle(restored);
        let resumed = ask_resume(&mut hub, agent);
        let resumed = hub.await_answer(&mut inputs, resumed).await;
        let new = hub
            .ask(&mut inputs, Request::AgentStart(start_params(&folder)))
            .await;
        // Lets the held threads end, so that the test's runtime can shut down.
        drop(releases);

        let not_answered = format!("the folder {} did not answer", folder.display());
        let shown = format!("cannot start claude: {not_answered}");
        let restored = &hub.agents[&agent];
        assert!(restored.process.is_none() && !restored.starting);
        assert!(restored.record.running, "the next daemon starts it again");
        assert!(restored.view.items().iter().any(|item| matches!(
            &item.body,
            ItemBody::Notice { text, .. } if *text == shown
        )));
        for refused in [resumed, new] {
            let refused = refused.unwrap_err();
            assert_eq!(refused.code, ErrorCode::Timeout);
            assert_eq!(refused.message, not_answered);
        }
        assert_eq!(hub.agents.len(), 1, "the new agent is not listed");
    }

    /// The folder the app sends resolves in time, but the folder it leads to does not answer: the
    /// start of `claude` there fails in time, and nothing is listed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_agent_whose_resolved_folder_does_not_answer_fails() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&folder, &link).unwrap();
        let resolved = std::fs::canonicalize(&folder).unwrap();
        let releases = blocking::hold_folder(&resolved).await;
        let (mut hub, mut inputs) = quiet_hub(temp.path());

        let refused = hub
            .ask(&mut inputs, Request::AgentStart(start_params(&link)))
            .await;
        drop(releases);

        let refused = refused.unwrap_err();
        assert_eq!(refused.code, ErrorCode::Timeout);
        assert_eq!(
            refused.message,
            format!("the folder {} did not answer", resolved.display())
        );
        assert!(hub.agents.is_empty() && hub.new_agents.is_empty());
    }

    /// The daemon stops while starts run: each request still gets an answer.
    #[tokio::test]
    async fn a_start_answers_when_the_daemon_stops_meanwhile() {
        async fn answer(answered: oneshot::Receiver<ResponseResult>) -> ResponseResult {
            tokio::time::timeout(Duration::from_secs(10), answered)
                .await
                .expect("the request was answered in time")
                .expect("the request kept its reply")
        }

        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let agent = state_with_agent(&state_dir, &folder, false);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);
        let (reply, starting) = oneshot::channel();
        hub.request(Request::AgentStart(start_params(&folder)), reply);
        let resolved = next_input(&mut inputs).await;
        // The test's runtime has one thread: no task runs before the receiver is dropped.
        let (reply, resolving) = oneshot::channel();
        hub.request(Request::AgentStart(start_params(&folder)), reply);
        hub.handle(resolved);
        let resuming = ask_resume(&mut hub, agent);

        drop(inputs);

        let stopping = ResponseResult::Error(errors::stopping());
        assert_eq!(answer(resolving).await, stopping);
        assert_eq!(answer(starting).await, stopping);
        assert_eq!(answer(resuming).await, stopping);
    }

    /// A removal while `claude` starts would leave the start without its agent, and a move would
    /// start `claude` in the folder the agent leaves.
    #[tokio::test]
    async fn a_remove_or_a_move_while_claude_starts_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        let target = temp.path().join("elsewhere");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let agent = state_with_agent(&state_dir, &folder, false);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);

        let mut resumed = ask_resume(&mut hub, agent);
        let removed = hub.remove(agent);
        let (reply, mut moved) = oneshot::channel();
        hub.move_to(agent, target, reply);
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);

        assert_eq!(removed.unwrap_err().code, ErrorCode::AgentRunning);
        let ResponseResult::Error(refused) = moved.try_recv().unwrap() else {
            panic!("expected the move to be refused");
        };
        assert_eq!(refused.code, ErrorCode::AgentRunning);
        assert_eq!(hub.agents[&agent].record.folder, folder);
        assert!(resumed.try_recv().is_ok(), "the resume answered");
    }

    /// An agent the daemon resumes as it starts shows as running from the first snapshot, as
    /// when `claude` started before the hub served any client.
    #[tokio::test]
    async fn a_restored_agent_shows_as_running_while_claude_starts() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let agent = state_with_agent(&state_dir, &folder, true);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);

        hub.start_restored_agents();
        let while_starting: Vec<AgentStatus> = hub
            .snapshot()
            .agents
            .iter()
            .map(|summary| summary.status)
            .collect();
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);

        assert_eq!(while_starting, [AgentStatus::Idle]);
        // The test's `claude` cannot start.
        let failed = hub.agents[&agent].view.summary();
        assert_eq!(failed.status, AgentStatus::Exited);
    }

    /// `agent.start` answers once `claude` runs. Until then, nothing is listed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_start_answers_once_claude_runs() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = quiet_hub(temp.path());
        let (client, mut frames) = mpsc::channel(16);
        hub.clients.push(client);
        let (reply, mut answered) = oneshot::channel();

        hub.request(Request::AgentStart(start_params(temp.path())), reply);
        let at_once = answered.try_recv().is_ok();
        let resolved = next_input(&mut inputs).await;
        let is_resolved = matches!(resolved, HubInput::FolderResolved { .. });
        hub.handle(resolved);
        let waiting = (
            answered.try_recv().is_ok(),
            hub.agents.len(),
            hub.new_agents.len(),
        );
        let started = next_input(&mut inputs).await;
        let is_started = matches!(started, HubInput::ProcessStarted { .. });
        hub.handle(started);

        assert!(!at_once, "the folder is resolved off the hub first");
        assert!(is_resolved && is_started);
        assert_eq!(
            waiting,
            (false, 0, 1),
            "the agent waits for claude, unlisted"
        );
        let Ok(ResponseResult::Ok(ResponseBody::AgentStarted { agent })) = answered.try_recv()
        else {
            panic!("expected the agent to start");
        };
        assert!(hub.agents[&agent].process.is_some());
        assert!(hub.new_agents.is_empty());
        let mut sent = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            sent.push(String::from_utf8_lossy(frame.as_bytes()).into_owned());
        }
        assert!(
            sent.iter()
                .any(|event| event.contains(r#""type":"agent_added""#)),
            "{sent:?}"
        );
    }

    /// `accounts.remove` does not see an agent before it is listed: the new agent's start finds
    /// its account gone, and nothing is listed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_agent_whose_account_goes_while_claude_starts_is_not_listed() {
        use cockpit_protocol::{AccountAddParams, CredentialSource};

        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = quiet_hub(temp.path());
        let (reply, added) = oneshot::channel();
        let work = AccountAddParams {
            name: "Work".to_owned(),
            folder: Some(temp.path().join("work-claude")),
            credentials: CredentialSource::SignIn,
        };
        hub.accounts_add(work, reply);
        let Ok(ResponseResult::Ok(ResponseBody::Account(work))) = added.await else {
            panic!("expected the account to be added");
        };
        let (reply, mut answered) = oneshot::channel();
        let params = StartParams {
            account: work.account.id,
            ..start_params(temp.path())
        };

        hub.request(Request::AgentStart(params), reply);
        let start = next_start(&mut hub, &mut inputs).await;
        let removed = hub.accounts_remove(work.account.id);
        hub.handle(start);

        assert_eq!(removed, Ok(ResponseBody::Done));
        let ResponseResult::Error(refused) = answered.try_recv().unwrap() else {
            panic!("expected the start to fail");
        };
        assert_eq!(refused.code, ErrorCode::ClaudeFailed);
        assert!(
            refused.message.contains("no longer exists"),
            "{}",
            refused.message
        );
        assert!(hub.agents.is_empty() && hub.new_agents.is_empty());
        assert!(!hub.raw_log_path(AgentId::new(1)).exists());
    }

    /// The `claude` that starts has the model the agent had: a model recorded meanwhile would
    /// show on an agent that does not run it.
    #[tokio::test]
    async fn a_model_change_while_claude_starts_is_refused() {
        use cockpit_protocol::SetModelParams;

        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let agent = state_with_agent(&state_dir, &folder, false);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);

        let mut resumed = ask_resume(&mut hub, agent);
        let (reply, mut changed) = oneshot::channel();
        let set_model = SetModelParams {
            agent,
            model: "opus".to_owned(),
        };
        hub.set_model(set_model, reply);
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);

        let ResponseResult::Error(refused) = changed.try_recv().unwrap() else {
            panic!("expected the model change to be refused");
        };
        assert_eq!(refused.code, ErrorCode::AgentExited);
        assert_eq!(hub.agents[&agent].record.model, None);
        assert!(resumed.try_recv().is_ok(), "the resume answered");
    }

    /// A second start would run a second `claude` on the session.
    #[tokio::test]
    async fn a_resume_while_claude_starts_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let agent = state_with_agent(&state_dir, &folder, false);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);

        let mut first = ask_resume(&mut hub, agent);
        let mut second = ask_resume(&mut hub, agent);
        let first_waits = first.try_recv().is_err();
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);

        assert!(first_waits, "the first resume waits for claude");
        let ResponseResult::Error(refused) = second.try_recv().unwrap() else {
            panic!("expected the second resume to be refused");
        };
        assert_eq!(refused.code, ErrorCode::AgentRunning);
        // The test's `claude` cannot start: the first resume says so.
        let ResponseResult::Error(failed) = first.try_recv().unwrap() else {
            panic!("expected the first resume to fail");
        };
        assert_eq!(failed.code, ErrorCode::ClaudeFailed);
    }

    /// The resume answers once its start comes back, and the agent is gone by then.
    #[tokio::test]
    async fn a_close_while_claude_starts_answers_the_resume() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let agent = state_with_agent(&state_dir, &folder, false);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);

        let mut resumed = ask_resume(&mut hub, agent);
        let closed = hub.close(agent);
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);

        assert_eq!(closed, Ok(ResponseBody::Done));
        let ResponseResult::Error(gone) = resumed.try_recv().unwrap() else {
            panic!("expected the resume to find no agent");
        };
        assert_eq!(gone.code, ErrorCode::NotFound);
        assert!(hub.agents.is_empty());
    }

    /// Two resumes of a past session that has no agent, as after a double click: the second waits
    /// for the agent the first starts, so that the session does not run twice.
    #[tokio::test]
    async fn a_second_resume_of_a_session_gets_the_answer_of_the_first() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&temp.path().join("state"));
        let folder = temp.path().to_owned();
        let (first, mut first_answer) = oneshot::channel();
        let (second, mut second_answer) = oneshot::channel();

        hub.resume_session(
            past_session(&folder),
            Ok(folder.clone()),
            Instant::now(),
            first,
        );
        hub.resume_session(
            past_session(&folder),
            Ok(folder.clone()),
            Instant::now(),
            second,
        );
        let second_waits = second_answer.try_recv().is_err();
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);
        let more_starts = inputs.try_recv().is_ok();

        assert!(second_waits, "the second resume waits for the first");
        assert!(!more_starts, "one claude starts");
        // The test's `claude` cannot start: both resumes say so.
        let first = first_answer.try_recv().unwrap();
        let ResponseResult::Error(failed) = &first else {
            panic!("expected the first resume to fail");
        };
        assert_eq!(failed.code, ErrorCode::ClaudeFailed);
        assert_eq!(second_answer.try_recv().unwrap(), first);
        assert!(hub.new_agents.is_empty());
    }

    /// The second resume of a session, asked while the first starts it, gets the agent the first
    /// lists, as when it is asked after.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_second_resume_of_a_session_gets_the_agent_the_first_starts() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = quiet_hub(temp.path());
        let folder = temp.path().to_owned();
        let (first, mut first_answer) = oneshot::channel();
        let (second, mut second_answer) = oneshot::channel();

        hub.resume_session(
            past_session(&folder),
            Ok(folder.clone()),
            Instant::now(),
            first,
        );
        hub.resume_session(
            past_session(&folder),
            Ok(folder.clone()),
            Instant::now(),
            second,
        );
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);

        let started = ResponseResult::Ok(ResponseBody::AgentStarted {
            agent: AgentId::new(1),
        });
        assert_eq!(first_answer.try_recv().unwrap(), started);
        assert_eq!(second_answer.try_recv().unwrap(), started);
        assert_eq!(hub.agents.len(), 1);
    }

    /// A fork names the session it forks until its `claude` reports its own: it takes no session
    /// over, so the session still resumes meanwhile.
    #[tokio::test]
    async fn a_starting_fork_does_not_hold_back_the_session_it_forks() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&temp.path().join("state"));
        let folder = temp.path().to_owned();
        let fork = AgentRecord {
            id: AgentId::new(7),
            title: "Fix the cart (fork)".to_owned(),
            folder: folder.clone(),
            session: Some(ClaudeSessionId::new(SESSION.to_owned())),
            model: None,
            effort: None,
            mode: None,
            running: true,
            started_at: Timestamp::from_unix_millis(0),
            past: None,
            position: Position::default(),
            process: None,
            account: AccountId::DEFAULT,
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Fork,
        };
        let starting = start::StartingAgent {
            record: fork,
            also_waiting: Vec::new(),
        };
        hub.new_agents.insert(AgentId::new(7), starting);
        let (reply, mut answered) = oneshot::channel();

        hub.resume_session(
            past_session(&folder),
            Ok(folder.clone()),
            Instant::now(),
            reply,
        );
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);

        // The test's `claude` cannot start: the resume got past the check.
        let ResponseResult::Error(failed) = answered.try_recv().unwrap() else {
            panic!("expected the resume to fail");
        };
        assert_eq!(failed.code, ErrorCode::ClaudeFailed);
    }

    /// A listed fork names the session it forks until its `claude` reports its own: a resume of
    /// that session starts a new agent on it, and leaves the fork alone.
    #[tokio::test]
    async fn a_session_resume_does_not_open_a_fork_of_the_session() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let fork = state_with_agent(&state_dir, &folder, false);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);
        if let Some(listed) = hub.agents.get_mut(&fork) {
            listed.record.resume_as = ResumeAs::Fork;
        }
        let (reply, mut answered) = oneshot::channel();

        hub.resume_session(
            past_session(&folder),
            Ok(folder.clone()),
            Instant::now(),
            reply,
        );
        let fork_starts = hub.agents[&fork].starting;
        let starting: Vec<(Option<ClaudeSessionId>, ResumeAs)> = hub
            .new_agents
            .values()
            .map(|starting| (starting.record.session.clone(), starting.record.resume_as))
            .collect();
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);

        assert!(!fork_starts, "the fork stays exited");
        let session = ClaudeSessionId::new(SESSION.to_owned());
        assert_eq!(starting, [(Some(session), ResumeAs::Same)]);
        // The test's `claude` cannot start: the resume got past the fork.
        let ResponseResult::Error(failed) = answered.try_recv().unwrap() else {
            panic!("expected the resume to fail");
        };
        assert_eq!(failed.code, ErrorCode::ClaudeFailed);
        assert!(hub.agents[&fork].process.is_none());
    }

    /// The process that a stopped start brings late is killed, and the next start's runs.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_late_start_of_an_old_generation_is_killed() {
        use cockpit_protocol::ProcessId;

        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let agent = state_with_agent(&state_dir, &folder, false);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);
        let claude = temp.path().join("claude");
        // `write_script` runs it once without arguments, in the test's working folder.
        test_script::write_script(
            &claude,
            "#!/bin/sh\n[ $# -gt 0 ] && echo $$ >> claude.pids\nwhile read -r line; do :; done\n",
        );
        hub.config.claude_program = claude;
        let pids_file = folder.join("claude.pids");
        let pids = || -> Vec<u32> {
            let text = std::fs::read_to_string(&pids_file).unwrap_or_default();
            text.lines().filter_map(|line| line.parse().ok()).collect()
        };

        let mut stopped = ask_resume(&mut hub, agent);
        let late = next_start(&mut hub, &mut inputs).await;
        assert!(eventually(|| pids().len() == 1).await);
        hub.stop(agent).unwrap();
        let mut resumed = ask_resume(&mut hub, agent);
        let current = next_start(&mut hub, &mut inputs).await;
        assert!(eventually(|| pids().len() == 2).await);
        hub.handle(late);
        let attached_late = hub.agents[&agent].process.is_some();
        hub.handle(current);

        let [late_pid, current_pid] = pids()[..] else {
            panic!("expected two processes");
        };
        assert!(!attached_late);
        let attached = hub.agents[&agent]
            .process
            .as_ref()
            .and_then(|process| process.pid());
        assert_eq!(attached.map(ProcessId::get), Some(current_pid));
        assert!(
            eventually(|| !test_script::runs(late_pid)).await,
            "the late process was killed"
        );
        assert!(test_script::runs(current_pid));
        // Each resume answers once, with what its start did.
        assert_eq!(
            stopped.try_recv().unwrap(),
            ResponseResult::Ok(ResponseBody::Done)
        );
        assert_eq!(
            resumed.try_recv().unwrap(),
            ResponseResult::Ok(ResponseBody::Done)
        );
    }

    /// A prompt sent while `claude` starts waits in the queue, and goes to `claude` once it runs.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_prompt_sent_while_claude_starts_reaches_it() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let agent = state_with_agent(&state_dir, &folder, false);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);
        let claude = temp.path().join("claude");
        // Its output stays open: the daemon stops writing to a `claude` whose output ended.
        let script =
            "#!/bin/sh\nwhile read -r line; do printf '%s\\n' \"$line\" >> claude.stdin; done\n";
        test_script::write_script(&claude, script);
        hub.config.claude_program = claude;

        let mut resumed = ask_resume(&mut hub, agent);
        let sent = hub.prompt(agent, prompt("Fix the cart total"), PromptDelivery::Steer);
        let waiting: Vec<(String, WaitingState)> = hub.agents[&agent]
            .view
            .details()
            .waiting_prompts
            .iter()
            .map(|waiting| (waiting.text.clone(), waiting.state))
            .collect();
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);

        assert_eq!(sent, Ok(ResponseBody::Done));
        assert_eq!(
            waiting,
            [("Fix the cart total".to_owned(), WaitingState::Queued)]
        );
        assert_eq!(
            resumed.try_recv().unwrap(),
            ResponseResult::Ok(ResponseBody::Done)
        );
        let view = &hub.agents[&agent].view;
        assert!(view.details().waiting_prompts.is_empty());
        assert!(view.items().iter().any(|item| matches!(
            &item.body,
            ItemBody::UserMessage(message) if message.text == "Fix the cart total"
        )));
        let stdin = folder.join("claude.stdin");
        let received = || {
            std::fs::read_to_string(&stdin).is_ok_and(|text| text.contains("Fix the cart total"))
        };
        assert!(eventually(received).await, "claude got the prompt");
    }

    /// A stop while `claude` starts gives the start up: the agent stays stopped, and the prompt
    /// that waited shows as not sent.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stop_while_claude_starts_gives_the_start_up() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let agent = state_with_agent(&state_dir, &folder, false);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);
        let claude = temp.path().join("claude");
        test_script::write_script(&claude, "#!/bin/sh\nexec cat >/dev/null\n");
        hub.config.claude_program = claude;

        let mut resumed = ask_resume(&mut hub, agent);
        hub.prompt(agent, prompt("Then run the tests"), PromptDelivery::Queue)
            .unwrap();
        let stopped = hub.stop(agent);
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);
        let refused = hub
            .prompt(agent, prompt("One more thing"), PromptDelivery::Steer)
            .unwrap_err();

        assert_eq!(stopped, Ok(ResponseBody::Done));
        let stopped = &hub.agents[&agent];
        assert!(stopped.process.is_none() && !stopped.starting);
        assert!(!stopped.record.running);
        assert!(stopped.queue.is_empty());
        assert!(stopped.view.details().waiting_prompts.is_empty());
        assert_eq!(
            notices(&stopped.view),
            [
                (STOPPED_WHILE_STARTING.to_owned(), None),
                (
                    "Message not sent".to_owned(),
                    Some("Then run the tests".to_owned())
                ),
            ]
        );
        assert_eq!(refused.code, ErrorCode::AgentExited);
        assert_eq!(
            resumed.try_recv().unwrap(),
            ResponseResult::Ok(ResponseBody::Done)
        );
        let saved = AgentsFile::load(&hub.state.agents_file()).unwrap();
        assert!(saved.agents.iter().all(|record| !record.running));
    }

    /// A restored agent whose `claude` cannot start says why before the prompt that waited shows
    /// as not sent, as an exit does.
    #[tokio::test]
    async fn a_restored_agent_that_cannot_start_says_why_before_the_prompt_not_sent() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let agent = state_with_agent(&state_dir, &folder, true);
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&state_dir);

        hub.start_restored_agents();
        hub.prompt(agent, prompt("Fix the cart total"), PromptDelivery::Steer)
            .unwrap();
        let start = next_start(&mut hub, &mut inputs).await;
        hub.handle(start);

        let shown = notices(&hub.agents[&agent].view);
        let [(cause, None), not_sent] = &shown[..] else {
            panic!("expected the cause and the prompt not sent: {shown:?}");
        };
        assert!(cause.starts_with("cannot start claude: "), "{cause}");
        assert_eq!(
            *not_sent,
            (
                "Message not sent".to_owned(),
                Some("Fix the cart total".to_owned())
            )
        );
    }

    /// A close takes out a running agent at once, with its log, and tells the clients.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_close_removes_a_running_agent_and_its_log() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = quiet_hub(temp.path());
        let agent = started(&mut hub, &mut inputs, temp.path()).await;
        let log = hub.logs.path(LogName::Agent(agent));
        let (client, mut frames) = mpsc::channel(16);
        hub.clients.push(client);

        let closed = hub.close(agent);
        let again = hub.close(agent).unwrap_err();

        assert_eq!(closed, Ok(ResponseBody::Done));
        assert_eq!(again.code, ErrorCode::NotFound);
        assert!(hub.agents.is_empty());
        assert!(!log.exists());
        let saved = AgentsFile::load(&hub.state.agents_file()).unwrap();
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
        let (mut hub, mut inputs) = quiet_hub(temp.path());
        let agent = started(&mut hub, &mut inputs, temp.path()).await;
        let items_before = hub.agents[&agent].view.items().len();

        hub.stop(agent).unwrap();
        let refused = hub
            .prompt(agent, prompt("one more thing"), PromptDelivery::Steer)
            .unwrap_err();

        assert_eq!(refused.code, ErrorCode::AgentExited);
        assert_eq!(hub.agents[&agent].view.items().len(), items_before);
    }

    /// The mark is saved with the agent, and a prompt clears it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_mark_is_saved_and_a_prompt_clears_it() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = quiet_hub(temp.path());
        let agent = started(&mut hub, &mut inputs, temp.path()).await;
        let saved = |hub: &Hub| {
            let file = AgentsFile::load(&hub.state.agents_file()).unwrap();
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
        hub.prompt(agent, prompt("go on"), PromptDelivery::Steer)
            .unwrap();
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
        let (mut hub, mut inputs) = quiet_hub(temp.path());
        let agent = started(&mut hub, &mut inputs, temp.path()).await;
        hub.mark(MarkParams {
            agent,
            mark: Mark::ToHandleKept,
        })
        .unwrap();

        hub.prompt(agent, prompt("go on"), PromptDelivery::Steer)
            .unwrap();

        assert_eq!(hub.agents[&agent].view.summary().mark, Mark::ToHandleKept);
        assert_eq!(hub.agents[&agent].record.mark, Mark::ToHandleKept);
    }

    /// A queued prompt waits for the sent ones. Once a stop releases them, it starts a turn.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_stop_that_releases_the_sent_prompts_starts_the_queued_one() {
        use serde_json::json;

        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = quiet_hub(temp.path());
        let agent = started(&mut hub, &mut inputs, temp.path()).await;
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
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs) = quiet_hub(temp.path());
        let folder = temp.path().join("shop");
        std::fs::create_dir_all(&folder).unwrap();
        let first = started(&mut hub, &mut inputs, &folder).await;
        // The first agent's own read finds no repository: it comes back before the test gives
        // the agent one.
        while !hub.repository_reads.is_empty() {
            let input = next_input(&mut inputs).await;
            hub.handle(input);
        }
        let shop = Repository::from_remote("github.com/acme/shop");
        if let Some(agent) = hub.agents.get_mut(&first) {
            agent.view.set_repository(Some(shop.clone()));
        }
        let (client, mut frames) = mpsc::channel(16);
        hub.clients.push(client);

        let second = started(&mut hub, &mut inputs, &folder).await;

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
        use cockpit_protocol::{EventRecord, Frame, Seq, ServerMessage};

        use crate::store::EventLogs;

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
