//! An agent's life: start, prompts and their queue, answers, its process's events, stop, resume
//! and removal.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};

use claude_protocol::{ControlRequest, InputLine, OutputLine};
use cockpit_protocol::{
    AgentId, AgentStatus, AgentSummary, AnswerParams, CancelQueuedParams, ClaudeSessionId, CostUsd,
    ErrorBody, ErrorCode, Event, PastSession, Prompt, ResponseBody, ResponseResult,
    SetEffortParams, StartParams,
};

use crate::hub::input::{ControlPurpose, HubInput, Reply};
use crate::hub::state::{Agent, Hub, QueuedPrompt};
use crate::hub::{errors, state};
use crate::process::{ClaudeCommand, ProcessEvent};
use crate::sessions;
use crate::store::{AgentRecord, LogName, RawLog};
use crate::transcript::{
    AgentView, AnswerError, FollowUp, PromptDelivery, plan_usage, prompts, words,
};

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
            if running && let Err(error) = self.start_process(id) {
                self.exited_at_start(id, &error);
            }
            self.publish(id);
        }
    }

    pub(super) fn start_agent(&mut self, params: StartParams) -> Result<ResponseBody, ErrorBody> {
        if let Some(title) = &params.title {
            check_length("title", title, sessions::TITLE_MAX_CHARS)?;
        }
        if let Some(model) = &params.model {
            check_model(model)?;
        }
        let folder = agent_folder(&params.folder)?;
        let title = params
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| default_title(params.prompt.as_ref(), &folder));
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
        };
        let agent = self.add_agent(record, params.prompt)?;
        Ok(ResponseBody::AgentStarted { agent })
    }

    /// Starts an agent's process, then lists the agent. Nothing is listed when `claude` cannot
    /// start.
    pub(super) fn add_agent(
        &mut self,
        record: AgentRecord,
        prompt: Option<Prompt>,
    ) -> Result<AgentId, ErrorBody> {
        let id = record.id;
        let next = id
            .next()
            .ok_or_else(|| errors::internal("no agent id is left".to_owned()))?;
        let view = AgentView::new(initial_summary(&record), self.settings.prompt_cache_ttl());
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
        if let Err(error) = self.start_process(id) {
            self.agents.remove(&id);
            return Err(ErrorBody::new(ErrorCode::ClaudeFailed, error));
        }
        self.next_agent = next;
        self.save_agents();
        let Some(agent) = self.agents.get_mut(&id) else {
            return Err(errors::agent_not_found(id));
        };
        agent.view.mark_published();
        let summary = agent.view.summary().clone();
        self.emit(LogName::Agent(id), Event::AgentAdded(summary));
        if let Some(prompt) = prompt {
            self.prompt(id, prompt)?;
        }
        Ok(id)
    }

    /// Starts `claude` for an agent and asks for its commands. The error says why it cannot
    /// start.
    pub(super) fn start_process(&mut self, id: AgentId) -> Result<(), String> {
        let raw_path = self.raw_log_path(id);
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
        agent.process = Some(handle);
        agent.view.process_started();
        Ok(())
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
        let follow_ups = agent.view.apply(line, state::now());
        let session = agent.view.summary().session.clone();
        let session_changed = session.is_some() && session != agent.record.session;
        if session_changed {
            agent.record.session = session;
        }
        for follow_up in follow_ups {
            match follow_up {
                FollowUp::TurnEnded => {
                    self.control(
                        id,
                        ControlRequest::GetContextUsage,
                        ControlPurpose::ContextAfterTurn,
                    );
                    self.send_next_queued(id);
                }
                FollowUp::RateLimit(info) => {
                    if plan_usage::apply(&mut self.plan_usage, &info) {
                        let usage = self.plan_usage.clone();
                        self.emit(LogName::Daemon, Event::PlanUsageChanged(usage));
                    }
                }
            }
        }
        if session_changed {
            self.save_agents();
        }
        self.publish(id);
    }

    fn process_exited(&mut self, id: AgentId, notice: String) {
        let Some(agent) = self.agents.get_mut(&id) else {
            return;
        };
        agent.process = None;
        for queued in agent.queue.drain(..) {
            if let Some(item) = queued.item {
                agent.view.cancel_queued(item);
            }
        }
        agent.view.process_exited(notice, state::now());
        self.publish(id);
    }

    /// Sends a prompt now, or queues it while the agent works.
    pub(super) fn prompt(
        &mut self,
        id: AgentId,
        prompt: Prompt,
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
        if agent.view.is_busy() || !agent.queue.is_empty() {
            let item = agent.view.add_prompt(&prompt, PromptDelivery::Queued);
            agent.queue.push_back(QueuedPrompt {
                item: Some(item),
                line,
                text: prompt.text,
            });
        } else {
            agent.view.add_prompt(&prompt, PromptDelivery::Now);
            process.write(line);
        }
        self.publish(id);
        Ok(ResponseBody::Done)
    }

    /// Sends the queued prompts up to the first one the transcript shows, which starts a turn.
    fn send_next_queued(&mut self, id: AgentId) {
        let Some(agent) = self.agents.get_mut(&id) else {
            return;
        };
        let Some(process) = &agent.process else {
            return;
        };
        while let Some(queued) = agent.queue.pop_front() {
            process.write(queued.line);
            match queued.item {
                Some(item) => {
                    agent.view.prompt_sent(item);
                    break;
                }
                None => agent.view.expect_echo(queued.text),
            }
        }
    }

    pub(super) fn cancel_queued(
        &mut self,
        params: CancelQueuedParams,
    ) -> Result<ResponseBody, ErrorBody> {
        let id = params.agent;
        let agent = self
            .agents
            .get_mut(&id)
            .ok_or(errors::agent_not_found(id))?;
        let position = agent
            .queue
            .iter()
            .position(|queued| queued.item == Some(params.item))
            .ok_or_else(|| {
                ErrorBody::new(
                    ErrorCode::NotFound,
                    format!("item {} is not a queued prompt", params.item.get()),
                )
            })?;
        agent.queue.remove(position);
        agent.view.cancel_queued(params.item);
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
        self.start_process(id)
            .map_err(|error| ErrorBody::new(ErrorCode::ClaudeFailed, error))?;
        if let Some(agent) = self.agents.get_mut(&id) {
            agent.record.running = true;
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
        Ok(ResponseBody::Done)
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
        if let Some(process) = &agent.process {
            let line = InputLine::prompt(text.clone());
            if agent.view.is_busy() || !agent.queue.is_empty() {
                agent.queue.push_back(QueuedPrompt {
                    item: None,
                    line,
                    text,
                });
            } else {
                process.write(line);
                agent.view.expect_echo(text);
            }
        }
        agent.record.effort = Some(params.effort);
        agent.view.set_effort(params.effort);
        self.save_agents();
        self.publish(id);
        Ok(ResponseBody::Done)
    }

    /// A past session becomes an agent, or the agent that has it resumes.
    pub(super) fn resume_session(&mut self, session: PastSession, reply: Reply) {
        respond(reply, self.agent_for_session(session));
    }

    fn agent_for_session(&mut self, session: PastSession) -> Result<ResponseBody, ErrorBody> {
        if let Some(agent) = self.agent_with_session(&session.id) {
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
        };
        let agent = self.add_agent(record, None)?;
        Ok(ResponseBody::AgentStarted { agent })
    }

    pub(super) fn agent_with_session(&self, session: &ClaudeSessionId) -> Option<AgentId> {
        self.agents
            .values()
            .find(|agent| agent.record.session.as_ref() == Some(session))
            .map(|agent| agent.record.id)
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
        mode: record.mode.unwrap_or_default(),
        task_progress: None,
        context_percent: None,
        cache_expires_at: None,
        turns: 0,
        cost: CostUsd::new(0.0),
        started_at: record.started_at,
        session: record.session.clone(),
    }
}

/// An existing folder, absolute, with its symlinks resolved.
fn agent_folder(folder: &Path) -> Result<PathBuf, ErrorBody> {
    if !folder.is_absolute() {
        return Err(errors::invalid(format!(
            "the folder {} is not absolute",
            folder.display()
        )));
    }
    let resolved = folder.canonicalize().map_err(|error| {
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
    use cockpit_protocol::Timestamp;

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
            };
            hub.start_agent(params).unwrap_err().code
        };

        let long_title = start(sessions::TITLE_MAX_CHARS + 1, 1);
        let long_model = start(1, MODEL_MAX_CHARS + 1);
        // The test's `claude` cannot start: the request got past the checks.
        let longest = start(sessions::TITLE_MAX_CHARS, MODEL_MAX_CHARS);
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
    #[test]
    fn a_past_session_needs_a_folder_an_agent_can_start_in() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(temp.path());
        let session = |folder: &str| PastSession {
            id: ClaudeSessionId::new("11111111-2222-3333-4444-555555555555".to_owned()),
            title: "Fix the cart".to_owned(),
            folder: PathBuf::from(folder),
            last_activity: Timestamp::from_unix_millis(0),
            turns: 1,
        };

        let relative = hub.agent_for_session(session("work")).unwrap_err();
        let missing = hub
            .agent_for_session(session("/nonexistent/agents-cockpit/work"))
            .unwrap_err();

        assert_eq!(relative.code, ErrorCode::InvalidRequest);
        assert_eq!(missing.code, ErrorCode::NotFound);
        assert!(hub.agents.is_empty());
    }

    /// Once stopped, the process's stdin is closed: a prompt would be shown as sent and never
    /// reach `claude`.
    #[tokio::test]
    async fn a_stopping_agent_takes_no_prompt() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

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
        let refused = hub.prompt(agent, prompt).unwrap_err();

        assert_eq!(refused.code, ErrorCode::AgentExited);
        assert_eq!(hub.agents[&agent].view.items().len(), items_before);
    }
}
