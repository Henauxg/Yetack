//! Starting an agent's `claude` off the hub's task. Resolving a folder the app sends, checking an
//! agent's folder and starting `claude` in it all wait on the file system, and a mount that never
//! answers would hold the hub, with every agent and every client. Each runs in a task with a time
//! limit, and the hub carries on when the answer comes back as a [`HubInput`].
//!
//! While `claude` starts, the agent's prompts wait in its queue. A stop or a close gives the start
//! up, and a process that starts after it is killed.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use claude_protocol::ControlRequest;
use cockpit_protocol::{
    AccountId, AgentId, ErrorBody, ErrorCode, Prompt, ResponseBody, StartParams, TranscriptItem,
};
use tokio::sync::mpsc;
use tokio::time;

use crate::accounts::ClaudeEnv;
use crate::blocking::{self, BlockingError, Scope};
use crate::hub::agents::respond;
use crate::hub::input::{ControlPurpose, HubInput, Reply};
use crate::hub::state::{Agent, Hub};
use crate::hub::{errors, state};
use crate::process::{ClaudeCommand, InterruptedTurn, Spawned};
use crate::store::{AgentRecord, RawLog};
use crate::transcript::{ProcessSession, words};
use crate::{machine_load, read_file};

/// How long resolving a folder, or checking it and starting `claude` or a terminal's shell in it,
/// may take.
pub(super) const FOLDER_TIME: Duration = Duration::from_secs(10);
/// The generation of a new agent's first process.
pub(super) const FIRST_GENERATION: u32 = 1;

/// A request that goes on once the folder the app sent is resolved.
pub(crate) enum FolderRequest {
    /// `agent.start`, whose title and model were checked.
    Start(StartParams),
    /// `agent.fork` of the agent.
    Fork(AgentId),
    /// `agent.change_folder` of the agent.
    Move(AgentId),
}

/// A start of an agent's `claude`, from the hub to the task that starts it, and back.
pub(crate) struct Start {
    agent: AgentId,
    /// The process's generation. The agent waits for this start while it is starting with it.
    generation: u32,
    then: AfterStart,
}

/// What the hub does once the start comes back, with the request that waits for it.
pub(crate) enum AfterStart {
    /// A new agent, listed once `claude` runs. Nothing is listed when it cannot start.
    New(Box<NewAgent>),
    /// The daemon resumes an agent that ran when it stopped. A failed start shows in the
    /// transcript.
    Restored,
    /// `agent.resume`, or `sessions.resume` of an agent that has the session: `answer` once
    /// `claude` runs.
    Resume { answer: ResponseBody, reply: Reply },
    /// `agent.change_folder` runs the agent again once the session is copied. `copied` is the
    /// copy's outcome, which the answer gives first.
    Move {
        copied: Result<(), ErrorBody>,
        reply: Reply,
    },
}

/// A new agent whose first `claude` starts. It is listed once `claude` runs.
pub(crate) struct StartingAgent {
    pub(super) record: AgentRecord,
    /// The later `sessions.resume` requests of its session, which get the answer of the request
    /// that started it.
    pub(super) also_waiting: Vec<Reply>,
}

/// What a new agent brings besides its record.
pub(crate) struct NewAgent {
    /// The transcript of the session it takes over.
    pub(super) past: Option<Vec<TranscriptItem>>,
    pub(super) opening: Opening,
    /// Answered with `AgentStarted` once the agent is listed.
    pub(super) reply: Reply,
}

/// What a new agent does once it is listed.
pub(crate) enum Opening {
    /// `agent.start`: the name the client gave goes into the session, then the first prompt.
    Start {
        named: bool,
        prompt: Option<Prompt>,
    },
    /// `sessions.resume`: the choices kept from an agent closed with the session are used up.
    Session,
    Fork,
}

impl AfterStart {
    /// Answers the request that waits: the daemon stops before the start reaches the hub.
    fn stopping(self) {
        match self {
            Self::New(new) => respond(new.reply, Err(errors::stopping())),
            Self::Resume { reply, .. } | Self::Move { reply, .. } => {
                respond(reply, Err(errors::stopping()));
            }
            Self::Restored => {}
        }
    }
}

impl Hub {
    /// Resolves `folder` in a task, then goes on with `request` in [`Hub::folder_resolved`].
    pub(super) fn resolve_folder(&self, folder: PathBuf, request: FolderRequest, reply: Reply) {
        let inputs = self.inputs.clone();
        tokio::spawn(async move {
            let folder = resolve(folder).await;
            let input = HubInput::FolderResolved {
                folder,
                request,
                reply,
            };
            if let Err(unsent) = inputs.send(input).await
                && let HubInput::FolderResolved { reply, .. } = unsent.0
            {
                respond(reply, Err(errors::stopping()));
            }
        });
    }

    pub(super) fn folder_resolved(
        &mut self,
        folder: Result<PathBuf, ErrorBody>,
        request: FolderRequest,
        reply: Reply,
    ) {
        let folder = match folder {
            Ok(folder) => folder,
            Err(error) => return respond(reply, Err(error)),
        };
        match request {
            FolderRequest::Start(params) => self.start_agent_in(params, folder, reply),
            FolderRequest::Fork(agent) => self.fork_to(agent, folder, reply),
            FolderRequest::Move(agent) => self.move_to(agent, folder, reply),
        }
    }

    /// Starts `claude` for a new agent. The agent is listed once it runs, in
    /// [`Hub::process_started`], and `reply` gets `AgentStarted` then.
    pub(super) fn start_new_agent(
        &mut self,
        record: AgentRecord,
        past: Option<Vec<TranscriptItem>>,
        opening: Opening,
        reply: Reply,
    ) {
        let id = record.id;
        let Some(next) = id.next() else {
            return respond(
                reply,
                Err(errors::internal("no agent id is left".to_owned())),
            );
        };
        // Taken now, so that another new agent cannot get it meanwhile.
        self.next_agent = next;
        let command = self.claude_command(&record, InterruptedTurn::Leave);
        let starting = StartingAgent {
            record,
            also_waiting: Vec::new(),
        };
        self.new_agents.insert(id, starting);
        let new = NewAgent {
            past,
            opening,
            reply,
        };
        let start = Start {
            agent: id,
            generation: FIRST_GENERATION,
            then: AfterStart::New(Box::new(new)),
        };
        match command {
            Ok(command) => self.spawn_start(start, command),
            Err(error) => self.process_started(start, Err(error)),
        }
    }

    /// Starts `claude` for an agent that has no process. The hub hears back in
    /// [`Hub::process_started`].
    pub(super) fn start_process(
        &mut self,
        id: AgentId,
        interrupted_turn: InterruptedTurn,
        then: AfterStart,
    ) {
        let command = self
            .agent(id)
            .and_then(|agent| self.claude_command(&agent.record, interrupted_turn));
        let Some(agent) = self.agents.get_mut(&id) else {
            let start = Start {
                agent: id,
                generation: 0,
                then,
            };
            return self.process_started(start, Err(errors::agent_not_found(id)));
        };
        agent.generation = agent.generation.wrapping_add(1);
        agent.starting = true;
        let start = Start {
            agent: id,
            generation: agent.generation,
            then,
        };
        match command {
            Ok(command) => self.spawn_start(start, command),
            Err(error) => self.process_started(start, Err(error)),
        }
    }

    /// How to start `claude` for the agent of `record`. Fails with `ClaudeFailed` when its
    /// account is gone.
    fn claude_command(
        &self,
        record: &AgentRecord,
        interrupted_turn: InterruptedTurn,
    ) -> Result<ClaudeCommand, ErrorBody> {
        let account = self
            .account(record.account)
            .map_err(|_| account_gone(record.account))?;
        Ok(ClaudeCommand {
            program: self.config.claude_program.clone(),
            folder: record.folder.clone(),
            model: record.model.clone(),
            mode: record.mode.map(words::mode_to_claude),
            effort: record.effort.and_then(words::effort_word),
            resume: record.session.clone(),
            resume_as: record.resume_as,
            interrupted_turn,
            env: ClaudeEnv::new(&account),
            agent_tools: self
                .agent_tools
                .as_ref()
                .map(|tools| tools.claude_args(record.id))
                .unwrap_or_default(),
        })
    }

    /// Checks the agent's folder and starts `claude` in it, in a task with [`FOLDER_TIME`], then
    /// hands the process to the hub. No event of the process reaches the hub before it runs the
    /// process's task.
    fn spawn_start(&self, start: Start, command: ClaudeCommand) {
        let raw_log = RawLog::open(&self.raw_log_path(start.agent))
            .inspect_err(|error| {
                tracing::warn!(error = %errors::chain(error), "cannot open the raw log");
            })
            .ok();
        let inputs = self.inputs.clone();
        tokio::spawn(async move {
            let (agent, generation) = (start.agent, start.generation);
            let events = inputs.clone();
            let folder = command.folder.clone();
            let deadline = time::Instant::now() + FOLDER_TIME;
            let spawned = blocking::run(deadline, Scope::Folder(&folder), move || {
                spawn_in_folder(&command, raw_log, events, agent, generation)
            })
            .await;
            let started = spawned.unwrap_or_else(|error| Err(not_answered(&folder, &error)));
            let input = HubInput::ProcessStarted {
                start: Box::new(start),
                started,
            };
            // Dropping the unsent process kills it.
            if let Err(unsent) = inputs.send(input).await
                && let HubInput::ProcessStarted { start, .. } = unsent.0
            {
                start.then.stopping();
            }
        });
    }

    /// A task started an agent's `claude`, or could not.
    pub(super) fn process_started(
        &mut self,
        start: Start,
        started: Result<Spawned<ControlPurpose>, ErrorBody>,
    ) {
        let Start {
            agent: id,
            generation,
            then,
        } = start;
        let current = self
            .agents
            .get(&id)
            .is_some_and(|agent| agent.starting && agent.generation == generation);
        match then {
            AfterStart::New(new) => self.new_agent_started(id, *new, started),
            AfterStart::Restored => {
                let result = self.finish_start(id, current, started);
                if current {
                    // The transcript says why before it shows the waiting prompts as not sent.
                    if let Err(error) = result {
                        self.exited_at_start(id, &error.message);
                    }
                    self.publish(id);
                }
            }
            AfterStart::Resume { answer, reply } => {
                let result = self.finish_start(id, current, started);
                if current && let Some(agent) = self.agents.get_mut(&id) {
                    if result.is_ok() {
                        agent.record.running = true;
                        agent.view.resumed_by_user(state::now());
                        self.save_agents();
                    } else {
                        agent.view.drop_waiting_prompts();
                    }
                    self.publish(id);
                }
                respond(reply, self.unless_gone(id, result.map(|()| answer)));
            }
            AfterStart::Move { copied, reply } => {
                let result = self.finish_start(id, current, started);
                if current && let Some(agent) = self.agents.get_mut(&id) {
                    if result.is_ok() {
                        agent.view.resumed_by_user(state::now());
                    } else {
                        agent.view.drop_waiting_prompts();
                    }
                    self.publish(id);
                }
                let moved = copied.and(result).map(|()| ResponseBody::Done);
                respond(reply, self.unless_gone(id, moved));
            }
        }
    }

    /// Gives the process to the agent when it still waits for this start, or ends the start that
    /// failed. Returns the start's own outcome. After a failure, the caller shows the prompts
    /// that waited as not sent.
    fn finish_start(
        &mut self,
        id: AgentId,
        current: bool,
        started: Result<Spawned<ControlPurpose>, ErrorBody>,
    ) -> Result<(), ErrorBody> {
        match started {
            Ok(spawned) if current => {
                self.attach(id, spawned);
                Ok(())
            }
            Ok(late) => {
                // A stop, a close or a newer start came meanwhile.
                drop(late);
                Ok(())
            }
            Err(error) => {
                if current && let Some(agent) = self.agents.get_mut(&id) {
                    end_start(agent);
                }
                Err(error)
            }
        }
    }

    /// The agent runs the process that started for it, and gets the prompts that waited.
    pub(super) fn attach(&mut self, id: AgentId, spawned: Spawned<ControlPurpose>) {
        let account_folder = self
            .agent(id)
            .and_then(|agent| self.account(agent.record.account))
            .map(|account| account.folder);
        let Some(agent) = self.agents.get_mut(&id) else {
            return;
        };
        let process = spawned.run();
        if let Err(purpose) =
            process.control(ControlRequest::Initialize, ControlPurpose::Initialize)
        {
            purpose.fail(errors::agent_exited(id));
        }
        agent.starting = false;
        agent.record.process = process.pid().and_then(machine_load::record_process);
        agent.process = Some(process);
        let generation = agent.generation;
        agent.view.process_started(process_session(&agent.record));
        if let (Some(session), Ok(folder)) = (agent.record.session.clone(), account_folder) {
            self.read_restored_cost(id, generation, folder, session);
        }
        self.send_next_queued(id);
        self.save_agents();
    }

    /// A new agent's `claude` started, or could not. The agent is listed only once it runs.
    fn new_agent_started(
        &mut self,
        id: AgentId,
        new: NewAgent,
        started: Result<Spawned<ControlPurpose>, ErrorBody>,
    ) {
        let NewAgent {
            past,
            opening,
            reply,
        } = new;
        let Some(StartingAgent {
            record,
            also_waiting,
        }) = self.new_agents.remove(&id)
        else {
            return respond(reply, Err(errors::agent_not_found(id)));
        };
        // `accounts.remove` does not see an agent before it is listed.
        let started = started.and_then(|spawned| {
            self.account(record.account)
                .map_err(|_| account_gone(record.account))?;
            Ok(spawned)
        });
        let spawned = match started {
            Ok(spawned) => spawned,
            Err(error) => {
                self.remove_raw_log(id);
                return answer_all(reply, also_waiting, Err(error));
            }
        };
        self.add_agent(record, past, spawned);
        let opened = self.open_agent(id, opening);
        let answer = opened.map(|()| ResponseBody::AgentStarted { agent: id });
        answer_all(reply, also_waiting, answer);
    }

    /// `answer`, or `NotFound` when the agent was closed meanwhile.
    fn unless_gone(
        &self,
        id: AgentId,
        answer: Result<ResponseBody, ErrorBody>,
    ) -> Result<ResponseBody, ErrorBody> {
        if self.agents.contains_key(&id) {
            answer
        } else {
            Err(errors::agent_not_found(id))
        }
    }
}

/// Whether `claude` takes up the agent's session or makes a new one.
pub(super) fn process_session(record: &AgentRecord) -> ProcessSession {
    match record.session {
        Some(_) => ProcessSession::Resumed,
        None => ProcessSession::New,
    }
}

/// Gives `answer` to the request that started a new agent, and to those that waited for it.
fn answer_all(reply: Reply, also_waiting: Vec<Reply>, answer: Result<ResponseBody, ErrorBody>) {
    for waiting in also_waiting {
        respond(waiting, answer.clone());
    }
    respond(reply, answer);
}

/// Ends the start that failed: the prompts that waited for it are not sent.
fn end_start(agent: &mut Agent) {
    agent.starting = false;
    agent.queue.clear();
}

/// Resolves a folder the app sent, on a blocking thread, with [`FOLDER_TIME`].
pub(super) async fn resolve(folder: PathBuf) -> Result<PathBuf, ErrorBody> {
    let deadline = time::Instant::now() + FOLDER_TIME;
    let asked = folder.clone();
    let resolved = blocking::run(deadline, Scope::Folder(&folder), move || {
        agent_folder(&asked)
    })
    .await;
    resolved.unwrap_or_else(|error| Err(not_answered(&folder, &error)))
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

/// Checks the agent's folder, then starts `claude` in it. Its events reach the hub as those of
/// the agent's process `generation`.
fn spawn_in_folder(
    command: &ClaudeCommand,
    raw_log: Option<RawLog>,
    inputs: mpsc::Sender<HubInput>,
    agent: AgentId,
    generation: u32,
) -> Result<Spawned<ControlPurpose>, ErrorBody> {
    check_folder(&command.folder)?;
    let wrap = move |event| HubInput::Process {
        agent,
        generation,
        event,
    };
    crate::process::spawn(command, raw_log, inputs, wrap)
        .map_err(|error| ErrorBody::new(ErrorCode::ClaudeFailed, errors::chain(&error)))
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

/// Why the file system work on `folder` gave no answer.
pub(super) fn not_answered(folder: &Path, error: &BlockingError) -> ErrorBody {
    match error {
        BlockingError::TimedOut | BlockingError::Held => ErrorBody::new(
            ErrorCode::Timeout,
            format!("the folder {} did not answer", folder.display()),
        ),
        BlockingError::Task(_) => errors::internal(error.to_string()),
    }
}

fn account_gone(account: AccountId) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::ClaudeFailed,
        format!("the agent's account {} no longer exists", account.get()),
    )
}

#[cfg(test)]
mod tests {
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

    /// A `claude` that starts once its start stopped waiting, as in a folder that answers late,
    /// is killed: nothing owns it.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_claude_that_starts_past_the_limit_is_killed() {
        use crate::store::ResumeAs;
        use crate::test_script;

        fn written_pid(file: &Path) -> Option<u32> {
            std::fs::read_to_string(file).ok()?.trim().parse().ok()
        }

        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let claude = temp.path().join("claude");
        // `write_script` runs it once without arguments. It ignores its stdin, so only a kill
        // ends it soon.
        test_script::write_script(
            &claude,
            "#!/bin/sh\n[ $# -gt 0 ] || exit 0\necho $$ > claude.pid\nexec sleep 600\n",
        );
        let command = ClaudeCommand {
            program: claude,
            folder: folder.clone(),
            model: None,
            mode: None,
            effort: None,
            resume: None,
            resume_as: ResumeAs::Same,
            interrupted_turn: InterruptedTurn::Leave,
            env: ClaudeEnv::for_tests(),
            agent_tools: Vec::new(),
        };
        let (inputs, _received) = mpsc::channel(16);
        let pid_file = folder.join("claude.pid");
        let (answer, answered) = std::sync::mpsc::channel::<()>();
        let deadline = time::Instant::now() + Duration::from_millis(50);

        let read_pid = pid_file.clone();
        let started = blocking::run(deadline, Scope::Folder(&folder), move || {
            let _ = answered.recv();
            let spawned =
                spawn_in_folder(&command, None, inputs, AgentId::new(1), FIRST_GENERATION);
            // The work returns once `claude` runs, so that the test sees its pid before the kill.
            for _ in 0..1000 {
                if written_pid(&read_pid).is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            spawned
        })
        .await;
        answer.send(()).unwrap();
        let mut pid = None;
        for _ in 0..500 {
            pid = written_pid(&pid_file);
            if pid.is_some() {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
        let pid = pid.expect("claude started");
        let mut ended = false;
        // Well under the 10 seconds a closed stdin gives `claude` before the daemon kills it.
        for _ in 0..300 {
            ended = !test_script::runs(pid);
            if ended {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }

        assert!(matches!(started, Err(BlockingError::TimedOut)));
        assert!(ended, "the late claude was killed");
    }
}
