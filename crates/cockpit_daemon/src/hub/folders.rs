//! An agent's conversation in another working folder: `agent.fork` starts a new agent there on a
//! copy of it, and `agent.change_folder` moves the agent there.
//!
//! Claude Code finds a session only in the project of the folder `claude` starts in, so both copy
//! the session's file to that project first ([`sessions::copy_to_project`]). A session file can
//! be hundreds of megabytes: the copy runs in a blocking task, and the hub carries on when its
//! result comes back as a [`HubInput`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use cockpit_protocol::{
    AccountId, AgentId, ClaudeSessionId, Effort, ErrorBody, ErrorCode, FolderParams,
    MAX_TITLE_CHARS, Mark, PermissionMode, Position, ResponseBody,
};
use tokio::time;

use crate::blocking::{self, Scope};
use crate::hub::agents::respond;
use crate::hub::input::{HubInput, Reply};
use crate::hub::start::{AfterStart, FolderRequest, Opening};
use crate::hub::state::Hub;
use crate::hub::{errors, state};
use crate::process::InterruptedTurn;
use crate::sessions::{self, AccountFolder, FoundSession};
use crate::store::{AgentRecord, ResumeAs};

/// Added to the title of a fork.
const FORK_TITLE_SUFFIX: &str = " (fork)";
/// How long copying a session to another folder's project may take: a session file can reach
/// hundreds of megabytes.
const COPY_TIME: Duration = Duration::from_secs(120);

/// What a fork takes from the agent it forks, read before the copy, so that it still starts if
/// that agent closes meanwhile.
pub(crate) struct ForkOf {
    title: String,
    /// The original's folder, whose project holds the session.
    folder: PathBuf,
    /// The fork's folder.
    target: PathBuf,
    session: ClaudeSessionId,
    model: Option<String>,
    effort: Option<Effort>,
    mode: Option<PermissionMode>,
    account: AccountId,
}

/// A fork whose copy of the session is under way.
pub(crate) struct ForkCopy {
    account: AccountId,
    session: ClaudeSessionId,
    /// The fork's folder.
    folder: PathBuf,
}

impl ForkCopy {
    fn of(fork: &ForkOf) -> Self {
        Self {
            account: fork.account,
            session: fork.session.clone(),
            folder: fork.target.clone(),
        }
    }

    fn is_of(&self, fork: &ForkOf) -> bool {
        self.account == fork.account && self.session == fork.session && self.folder == fork.target
    }
}

/// Another agent that needs a session's file in a Claude Code project, which a copy of the
/// session there would replace.
enum FileUser {
    /// A fork that waits there for its first message, or whose copy goes there.
    WaitingFork,
    HasSession,
    /// An agent of the session that moves there.
    Moving,
}

/// A move under way, from the request until the copy's result: the agent waits for its process
/// to exit, which ends the session file, then for the copy. It does not resume meanwhile.
pub(crate) struct PendingMove {
    folder: PathBuf,
    /// Until the copy starts, which answers it.
    reply: Option<Reply>,
}

impl PendingMove {
    /// Answers the move that waits for the agent's process to exit, as the agent leaves.
    pub(super) fn agent_gone(self, id: AgentId) {
        if let Some(reply) = self.reply {
            respond(reply, Err(errors::agent_not_found(id)));
        }
    }
}

impl Hub {
    /// Resolves the fork's folder off the hub, then forks in [`Hub::fork_to`].
    pub(super) fn fork(&self, params: FolderParams, reply: Reply) {
        let request = FolderRequest::Fork(params.agent);
        self.resolve_folder(params.folder, request, reply);
    }

    /// Copies the agent's session to `target`, resolved from the request's folder, in a task.
    pub(super) fn fork_to(&mut self, id: AgentId, target: PathBuf, reply: Reply) {
        let prepared = self.fork_of(id, target).and_then(|fork| {
            let account = self.account(fork.account)?;
            Ok((fork, account.folder))
        });
        let (fork, claude_folder) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return respond(reply, Err(error)),
        };
        self.fork_copies.push(ForkCopy::of(&fork));
        let inputs = self.inputs.clone();
        tokio::spawn(async move {
            let account = fork.account;
            let (session, from, to) = (
                fork.session.clone(),
                fork.folder.clone(),
                fork.target.clone(),
            );
            let deadline = time::Instant::now() + COPY_TIME;
            let copied = blocking::run(deadline, Scope::Sessions, move || {
                sessions::copy_to_project(&claude_folder, &session, &from, &to)?;
                let folder = AccountFolder {
                    account,
                    folder: claude_folder,
                };
                sessions::find(&folder, &session)
                    .map(Box::new)
                    .ok_or_else(|| format!("cannot read the session {}", session.as_str()))
            })
            .await;
            let found = copied.unwrap_or_else(|error| Err(format!("the copy failed: {error}")));
            let input = HubInput::ForkCopied {
                fork: Box::new(fork),
                found,
                reply,
            };
            if let Err(unsent) = inputs.send(input).await
                && let HubInput::ForkCopied { reply, .. } = unsent.0
            {
                respond(reply, Err(errors::stopping()));
            }
        });
    }

    fn fork_of(&self, id: AgentId, target: PathBuf) -> Result<ForkOf, ErrorBody> {
        let record = &self.agent(id)?.record;
        let session = record
            .session
            .clone()
            .ok_or_else(|| errors::no_session(id))?;
        // Its session file is the one it copies: copied into another project, it would replace
        // the session file there, its source's own once the source has left.
        if record.resume_as == ResumeAs::Fork
            && sessions::project_name(&target) != sessions::project_name(&record.folder)
        {
            return Err(errors::invalid(
                "send the fork a message before forking it into another folder".to_owned(),
            ));
        }
        if let Some(user) = self.file_user(&session, record.account, &target, id) {
            let refused = match user {
                FileUser::WaitingFork => {
                    "a fork of this agent already waits for its first message in that folder: send it one, or close it, before forking there again"
                }
                FileUser::HasSession => {
                    "the agent this fork copies is in that folder: fork that agent there instead"
                }
                FileUser::Moving => {
                    "the agent with this session moves to that folder: wait for the move to end"
                }
            };
            return Err(errors::invalid(refused.to_owned()));
        }
        Ok(ForkOf {
            title: fork_title(&record.title),
            folder: record.folder.clone(),
            target,
            session,
            model: record.model.clone(),
            effort: record.effort,
            mode: record.mode,
            account: record.account,
        })
    }

    /// The session is in the fork's folder: the fork starts there, and goes on top of the list
    /// once `claude` runs.
    pub(super) fn fork_copied(
        &mut self,
        fork: ForkOf,
        found: Result<Box<FoundSession>, String>,
        reply: Reply,
    ) {
        if let Some(index) = self.fork_copies.iter().position(|copy| copy.is_of(&fork)) {
            self.fork_copies.swap_remove(index);
        }
        let past = match found {
            Ok(found) => found.past,
            Err(error) => return respond(reply, Err(copy_failed(error))),
        };
        let record = AgentRecord {
            id: self.next_agent,
            title: fork.title,
            folder: fork.target,
            session: Some(fork.session),
            model: fork.model,
            effort: fork.effort,
            mode: fork.mode,
            running: true,
            started_at: state::now(),
            past: Some(past.file),
            position: Position::default(),
            process: None,
            account: fork.account,
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Fork,
        };
        self.start_new_agent(record, Some(past.items), Opening::Fork, reply);
    }

    /// Resolves the new folder off the hub, then moves the agent in [`Hub::move_to`].
    pub(super) fn change_folder(&self, params: FolderParams, reply: Reply) {
        let request = FolderRequest::Move(params.agent);
        self.resolve_folder(params.folder, request, reply);
    }

    /// Moves the agent to `folder`, resolved from the request's, at once when it is exited, or
    /// once its process exits: closing its stdin lets `claude` end the session file first.
    pub(super) fn move_to(&mut self, id: AgentId, folder: PathBuf, reply: Reply) {
        let folder = match self.move_target(id, folder) {
            Ok(Some(folder)) => folder,
            Ok(None) => return respond(reply, Ok(ResponseBody::Done)),
            Err(error) => return respond(reply, Err(error)),
        };
        let Some(agent) = self.agents.get_mut(&id) else {
            return respond(reply, Err(errors::agent_not_found(id)));
        };
        match &agent.process {
            Some(process) => {
                process.stop();
                agent.moving = Some(PendingMove {
                    folder,
                    reply: Some(reply),
                });
            }
            None => {
                agent.moving = Some(PendingMove {
                    folder: folder.clone(),
                    reply: None,
                });
                self.copy_for_move(id, folder, false, reply);
            }
        }
    }

    /// The folder the agent moves to, or `None` when it already runs there. A move copies the
    /// agent's session file into the folder's project, so it is refused while another agent needs
    /// the file there.
    fn move_target(&self, id: AgentId, folder: PathBuf) -> Result<Option<PathBuf>, ErrorBody> {
        let agent = self.agent(id)?;
        let Some(session) = &agent.record.session else {
            return Err(errors::no_session(id));
        };
        // Its session file is the one it forks: in its source's folder, it would replace the
        // source's own.
        if agent.record.resume_as == ResumeAs::Fork {
            return Err(errors::invalid(
                "send the fork a message before moving it".to_owned(),
            ));
        }
        let working = agent.process.is_some() && agent.view.is_busy();
        if agent.moving.is_some() || agent.starting || working {
            return Err(errors::agent_working(id));
        }
        if self
            .file_user(session, agent.record.account, &folder, id)
            .is_some()
        {
            return Err(errors::invalid(
                "a fork of this agent waits for its first message in that folder: send it one, or close it, before moving this agent there"
                    .to_owned(),
            ));
        }
        Ok((folder != agent.record.folder).then_some(folder))
    }

    /// The agent that needs the file of `session` in the Claude Code project of `folder`: an
    /// agent there other than `except`, a fork whose copy goes there, or an agent that moves
    /// there, `except` included.
    fn file_user(
        &self,
        session: &ClaudeSessionId,
        account: AccountId,
        folder: &Path,
        except: AgentId,
    ) -> Option<FileUser> {
        let project = sessions::project_name(folder)?;
        let in_project = |folder: &Path| sessions::project_name(folder).as_ref() == Some(&project);
        let of_session = |record: &AgentRecord| {
            record.account == account && record.session.as_ref() == Some(session)
        };
        let listed = self.agents.iter().map(|(id, agent)| (*id, &agent.record));
        let starting = self
            .new_agents
            .iter()
            .map(|(id, starting)| (*id, &starting.record));
        let there = listed
            .chain(starting)
            .find(|(id, record)| *id != except && of_session(record) && in_project(&record.folder));
        if let Some((_, record)) = there {
            return Some(match record.resume_as {
                ResumeAs::Fork => FileUser::WaitingFork,
                ResumeAs::Same => FileUser::HasSession,
            });
        }
        let copying = self.fork_copies.iter().any(|copy| {
            copy.account == account && copy.session == *session && in_project(&copy.folder)
        });
        if copying {
            return Some(FileUser::WaitingFork);
        }
        let moving = self.agents.values().any(|agent| {
            of_session(&agent.record)
                && agent
                    .moving
                    .as_ref()
                    .is_some_and(|pending| in_project(&pending.folder))
        });
        moving.then_some(FileUser::Moving)
    }

    /// The agent's process exited: a move waiting for it carries on.
    pub(super) fn continue_move(&mut self, id: AgentId) {
        let waiting = self.agents.get_mut(&id).and_then(|agent| {
            let pending = agent.moving.as_mut()?;
            Some((pending.folder.clone(), pending.reply.take()?))
        });
        if let Some((folder, reply)) = waiting {
            self.copy_for_move(id, folder, true, reply);
        }
    }

    fn copy_for_move(&mut self, id: AgentId, folder: PathBuf, restart: bool, reply: Reply) {
        let source = self.agent(id).and_then(|agent| {
            let account = self.account(agent.record.account)?;
            let session = agent
                .record
                .session
                .clone()
                .ok_or_else(|| errors::no_session(id))?;
            Ok((account.folder, session, agent.record.folder.clone()))
        });
        let (claude_folder, session, from) = match source {
            Ok(source) => source,
            Err(error) => return self.move_copied(id, folder, restart, Err(error.message), reply),
        };
        let inputs = self.inputs.clone();
        tokio::spawn(async move {
            let to = folder.clone();
            let deadline = time::Instant::now() + COPY_TIME;
            let copied = blocking::run(deadline, Scope::Sessions, move || {
                sessions::copy_to_project(&claude_folder, &session, &from, &to)
            })
            .await
            .unwrap_or_else(|error| Err(format!("the copy failed: {error}")));
            let input = HubInput::MoveCopied {
                agent: id,
                folder,
                restart,
                copied,
                reply,
            };
            if let Err(unsent) = inputs.send(input).await
                && let HubInput::MoveCopied { reply, .. } = unsent.0
            {
                respond(reply, Err(errors::stopping()));
            }
        });
    }

    /// The session is in the new folder: the agent takes the folder, and runs again if it ran and
    /// no stop came meanwhile. After a failed copy it keeps its folder. The answer waits for
    /// `claude` to run.
    pub(super) fn move_copied(
        &mut self,
        id: AgentId,
        folder: PathBuf,
        restart: bool,
        copied: Result<(), String>,
        reply: Reply,
    ) {
        let Some(agent) = self.agents.get_mut(&id) else {
            return respond(reply, Err(errors::agent_not_found(id)));
        };
        agent.moving = None;
        let moved = copied.is_ok();
        if moved {
            agent.record.folder.clone_from(&folder);
            agent.view.set_folder(folder);
        }
        let restart = restart && agent.process.is_none() && agent.record.running;
        if moved {
            self.read_repository(id);
        }
        self.save_agents();
        self.publish(id);
        let copied = copied.map_err(copy_failed);
        if restart {
            let then = AfterStart::Move { copied, reply };
            self.start_process(id, InterruptedTurn::Leave, then);
        } else {
            respond(reply, copied.map(|()| ResponseBody::Done));
        }
    }
}

/// The original's title, with the suffix, cut to fit the longest title.
fn fork_title(title: &str) -> String {
    let kept = MAX_TITLE_CHARS.saturating_sub(FORK_TITLE_SUFFIX.chars().count());
    let mut fork: String = title.chars().take(kept).collect();
    fork.push_str(FORK_TITLE_SUFFIX);
    fork
}

fn copy_failed(error: String) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::Internal,
        format!("cannot copy the session: {error}"),
    )
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use claude_protocol::OutputLine;
    use cockpit_protocol::{Event, Repository, Request, ResponseResult, SessionUse, StartParams};
    use tokio::sync::{mpsc, oneshot};

    use super::*;
    use crate::process::ProcessEvent;
    use crate::store::{LogName, for_each_event};
    use crate::test_script::write_script;

    const SESSION: &str = "11111111-2222-3333-4444-555555555555";

    fn session_file(hub: &Hub, folder: &Path) -> PathBuf {
        let project = sessions::project_name(folder).unwrap();
        hub.config
            .claude_config_dir
            .join("projects")
            .join(project)
            .join(format!("{SESSION}.jsonl"))
    }

    /// A hub whose `claude` reads its stdin and exits once it closes, with one running agent in
    /// `<temp>/app` on `SESSION`, whose file is written there.
    async fn hub_with_agent(temp: &Path) -> (Hub, mpsc::Receiver<HubInput>, AgentId) {
        let (mut hub, mut inputs) = Hub::for_tests_with_inputs(&temp.join("state"));
        let claude = temp.join("claude-program");
        // Its output stays open: the daemon takes a closed output as the end of `claude`, and
        // kills it 10 seconds later.
        write_script(&claude, "#!/bin/sh\nwhile read -r line; do :; done\n");
        hub.config.claude_program = claude;
        let folder = temp.join("app");
        std::fs::create_dir_all(&folder).unwrap();
        let start = StartParams {
            folder: folder.clone(),
            title: Some("Fix the cart".to_owned()),
            model: Some("opus".to_owned()),
            effort: None,
            mode: None,
            prompt: None,
            account: AccountId::DEFAULT,
        };
        let Ok(ResponseBody::AgentStarted { agent }) =
            hub.ask(&mut inputs, Request::AgentStart(start)).await
        else {
            panic!("the agent did not start");
        };
        if let Some(started) = hub.agents.get_mut(&agent) {
            started.record.session = Some(ClaudeSessionId::new(SESSION.to_owned()));
        }
        let file = session_file(&hub, &folder);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let line =
            r#"{"type":"user","cwd":"/app","message":{"role":"user","content":"Fix the cart"}}"#;
        std::fs::write(&file, format!("{line}\n")).unwrap();
        (hub, inputs, agent)
    }

    #[tokio::test]
    async fn a_fork_starts_in_its_folder_on_a_copy_of_the_session() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, original) = hub_with_agent(temp.path()).await;
        let target = temp.path().join("app_fix");
        std::fs::create_dir_all(&target).unwrap();
        let (reply, answered) = oneshot::channel();

        hub.fork(
            FolderParams {
                agent: original,
                folder: target.clone(),
            },
            reply,
        );
        let Ok(ResponseBody::AgentStarted { agent }) =
            hub.await_answer(&mut inputs, answered).await
        else {
            panic!("the fork did not start");
        };

        let fork = &hub.agents[&agent].record;
        assert_eq!(fork.title, "Fix the cart (fork)");
        assert_eq!(fork.folder, target);
        assert_eq!(fork.model.as_deref(), Some("opus"));
        assert_eq!(fork.resume_as, ResumeAs::Fork);
        assert_eq!(
            fork.session.as_ref().map(ClaudeSessionId::as_str),
            Some(SESSION)
        );
        assert!(session_file(&hub, &target).is_file());
        assert!(hub.agents[&agent].process.is_some());
        assert_eq!(
            hub.agents[&original].record.folder,
            temp.path().join("app"),
            "the original stays"
        );
        assert_eq!(
            hub.agents[&agent].view.summary().session_use,
            SessionUse::Fork,
            "the summary names the session it copies"
        );
        let init = serde_json::json!({
            "type": "system", "subtype": "init", "session_id": "fork-session",
            "model": "opus", "permissionMode": "default"
        });
        let line: OutputLine = init.to_string().parse().unwrap();
        let generation = hub.agents[&agent].generation;
        hub.handle(HubInput::Process {
            agent,
            generation,
            event: ProcessEvent::Line(Box::new(line)),
        });
        let summary = hub.agents[&agent].view.summary();
        assert_eq!(summary.session_use, SessionUse::Own, "its own session now");
        assert_eq!(
            summary.session.as_ref().map(ClaudeSessionId::as_str),
            Some("fork-session")
        );
        assert_eq!(hub.agents[&agent].record.resume_as, ResumeAs::Same);
        let mut published = Vec::new();
        for_each_event(&hub.logs.path(LogName::Agent(agent)), |record| {
            if let Event::AgentAdded(summary) | Event::AgentUpdated(summary) = record.event {
                published.push(summary.session_use);
            }
        })
        .unwrap();
        assert_eq!(published.first(), Some(&SessionUse::Fork));
        assert_eq!(
            published.last(),
            Some(&SessionUse::Own),
            "the clients get the change"
        );
    }

    /// The original of `hub_with_agent`, forked into `app_fix`: the fork, which waits for its
    /// first message, and its folder.
    async fn forked(
        hub: &mut Hub,
        inputs: &mut mpsc::Receiver<HubInput>,
        original: AgentId,
        temp: &Path,
    ) -> (AgentId, PathBuf) {
        let fork_folder = temp.join("app_fix");
        std::fs::create_dir_all(&fork_folder).unwrap();
        let (reply, answered) = oneshot::channel();
        hub.fork_to(original, fork_folder.clone(), reply);
        let Ok(ResponseBody::AgentStarted { agent }) = hub.await_answer(inputs, answered).await
        else {
            panic!("the fork did not start");
        };
        (agent, fork_folder)
    }

    /// A fork that has not had its first message forks the session file of its folder's project:
    /// moved to its source's folder, it would copy that file over the source's own.
    #[tokio::test]
    async fn a_fork_without_its_own_session_does_not_move() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, original) = hub_with_agent(temp.path()).await;
        let source_folder = hub.agents[&original].record.folder.clone();
        let (fork, fork_folder) = forked(&mut hub, &mut inputs, original, temp.path()).await;
        let source_session = std::fs::read(session_file(&hub, &source_folder)).unwrap();
        let (reply, answered) = oneshot::channel();

        hub.move_to(fork, source_folder.clone(), reply);
        let refused = hub.await_answer(&mut inputs, answered).await.unwrap_err();

        assert_eq!(refused.code, ErrorCode::InvalidRequest);
        assert_eq!(refused.message, "send the fork a message before moving it");
        assert_eq!(hub.agents[&fork].record.folder, fork_folder);
        assert_eq!(
            std::fs::read(session_file(&hub, &source_folder)).unwrap(),
            source_session,
            "the source keeps its session"
        );
    }

    /// Its session copied there would replace the one the fork starts from.
    #[tokio::test]
    async fn an_agent_does_not_move_or_fork_again_where_its_fork_waits_for_its_first_message() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, original) = hub_with_agent(temp.path()).await;
        let source_folder = hub.agents[&original].record.folder.clone();
        let (_fork, fork_folder) = forked(&mut hub, &mut inputs, original, temp.path()).await;
        let fork_copy = std::fs::read(session_file(&hub, &fork_folder)).unwrap();
        let (moving, moved) = oneshot::channel();
        let (forking, forked_again) = oneshot::channel();

        hub.move_to(original, fork_folder.clone(), moving);
        let move_refused = hub.await_answer(&mut inputs, moved).await.unwrap_err();
        hub.fork_to(original, fork_folder.clone(), forking);
        let fork_refused = hub
            .await_answer(&mut inputs, forked_again)
            .await
            .unwrap_err();

        assert_eq!(move_refused.code, ErrorCode::InvalidRequest);
        assert!(
            move_refused
                .message
                .starts_with("a fork of this agent waits"),
            "{move_refused:?}"
        );
        assert_eq!(fork_refused.code, ErrorCode::InvalidRequest);
        assert!(
            fork_refused
                .message
                .starts_with("a fork of this agent already waits"),
            "{fork_refused:?}"
        );
        assert_eq!(hub.agents[&original].record.folder, source_folder);
        assert_eq!(
            std::fs::read(session_file(&hub, &fork_folder)).unwrap(),
            fork_copy,
            "the fork keeps the copy it starts from"
        );
    }

    /// The fork's file is its source's session as it was at the fork: copied to the source's
    /// folder, it would replace the source's own, and lose the turns since.
    #[tokio::test]
    async fn a_fork_of_a_fork_does_not_replace_the_session_of_the_agent_it_copies() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, original) = hub_with_agent(temp.path()).await;
        let source_folder = hub.agents[&original].record.folder.clone();
        let (fork, _) = forked(&mut hub, &mut inputs, original, temp.path()).await;
        let source_file = session_file(&hub, &source_folder);
        let mut source_session = std::fs::read(&source_file).unwrap();
        let later_turn = r#"{"type":"user","cwd":"/app","message":{"role":"user","content":"Then the checkout"}}"#;
        source_session.extend_from_slice(format!("{later_turn}\n").as_bytes());
        std::fs::write(&source_file, &source_session).unwrap();
        let elsewhere = temp.path().join("app_other");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let (to_source, source_answer) = oneshot::channel();
        let (to_elsewhere, elsewhere_answer) = oneshot::channel();

        hub.fork_to(fork, source_folder, to_source);
        let refused = hub
            .await_answer(&mut inputs, source_answer)
            .await
            .unwrap_err();
        hub.fork_to(fork, elsewhere, to_elsewhere);
        let refused_elsewhere = hub
            .await_answer(&mut inputs, elsewhere_answer)
            .await
            .unwrap_err();

        for refused in [refused, refused_elsewhere] {
            assert_eq!(refused.code, ErrorCode::InvalidRequest);
            assert_eq!(
                refused.message,
                "send the fork a message before forking it into another folder"
            );
        }
        assert_eq!(hub.agents.len(), 2, "no other fork");
        assert_eq!(
            std::fs::read(&source_file).unwrap(),
            source_session,
            "the source keeps its session"
        );
    }

    /// A fork is listed once its copy is done and its `claude` runs. A move or another fork
    /// there meanwhile would replace its copy all the same.
    #[tokio::test]
    async fn an_agent_does_not_move_or_fork_where_the_copy_of_its_fork_is_under_way() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, original) = hub_with_agent(temp.path()).await;
        let source_folder = hub.agents[&original].record.folder.clone();
        let fork_folder = temp.path().join("app_fix");
        std::fs::create_dir_all(&fork_folder).unwrap();
        let (forking, forked) = oneshot::channel();
        let (moving, moved) = oneshot::channel();
        let (forking_again, forked_again) = oneshot::channel();

        hub.fork_to(original, fork_folder.clone(), forking);
        hub.move_to(original, fork_folder.clone(), moving);
        hub.fork_to(original, fork_folder, forking_again);
        let move_refused = hub.await_answer(&mut inputs, moved).await.unwrap_err();
        let fork_refused = hub
            .await_answer(&mut inputs, forked_again)
            .await
            .unwrap_err();
        let started = hub.await_answer(&mut inputs, forked).await;

        assert!(
            move_refused
                .message
                .starts_with("a fork of this agent waits"),
            "{move_refused:?}"
        );
        assert!(
            fork_refused
                .message
                .starts_with("a fork of this agent already waits"),
            "{fork_refused:?}"
        );
        assert!(matches!(started, Ok(ResponseBody::AgentStarted { .. })));
        assert_eq!(hub.agents.len(), 2, "one fork");
        let source = &hub.agents[&original];
        assert_eq!(source.record.folder, source_folder);
        assert!(
            source.moving.is_none() && source.process.is_some(),
            "it runs on"
        );
        assert!(hub.fork_copies.is_empty());
    }

    /// The move copies the session once the agent's process exits, which would replace the copy
    /// of a fork made there meanwhile.
    #[tokio::test]
    async fn an_agent_does_not_fork_into_the_folder_it_moves_to() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, original) = hub_with_agent(temp.path()).await;
        let target = temp.path().join("app_moved");
        std::fs::create_dir_all(&target).unwrap();
        let (moving, moved) = oneshot::channel();
        let (forking, forked) = oneshot::channel();

        hub.move_to(original, target.clone(), moving);
        hub.fork_to(original, target.clone(), forking);
        let refused = hub.await_answer(&mut inputs, forked).await.unwrap_err();
        let move_result = hub.await_answer(&mut inputs, moved).await;

        assert_eq!(refused.code, ErrorCode::InvalidRequest);
        assert_eq!(
            refused.message,
            "the agent with this session moves to that folder: wait for the move to end"
        );
        assert_eq!(move_result, Ok(ResponseBody::Done));
        assert_eq!(hub.agents.len(), 1, "no fork");
        assert_eq!(hub.agents[&original].record.folder, target);
    }

    #[tokio::test]
    async fn a_move_stops_the_agent_and_runs_it_again_in_the_new_folder() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, agent) = hub_with_agent(temp.path()).await;
        let target = temp.path().join("app_moved");
        std::fs::create_dir_all(&target).unwrap();
        let (reply, answered) = oneshot::channel();

        hub.change_folder(
            FolderParams {
                agent,
                folder: target.clone(),
            },
            reply,
        );
        let result = hub.await_answer(&mut inputs, answered).await;

        assert_eq!(result, Ok(ResponseBody::Done));
        let moved = &hub.agents[&agent];
        assert_eq!(moved.record.folder, target);
        assert_eq!(moved.view.summary().folder, target);
        assert_eq!(
            moved.record.session.as_ref().map(ClaudeSessionId::as_str),
            Some(SESSION)
        );
        assert_eq!(moved.record.resume_as, ResumeAs::Same);
        assert!(moved.process.is_some(), "it runs again");
        assert!(session_file(&hub, &target).is_file());
    }

    #[tokio::test]
    async fn a_moved_agent_gets_its_new_repository_read_off_the_hub() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, agent) = hub_with_agent(temp.path()).await;
        let target = temp.path().join("shop");
        std::fs::create_dir_all(target.join(".git")).unwrap();
        std::fs::write(
            target.join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:acme/shop.git\n",
        )
        .unwrap();
        let (reply, answered) = oneshot::channel();
        hub.change_folder(
            FolderParams {
                agent,
                folder: target.clone(),
            },
            reply,
        );
        hub.await_answer(&mut inputs, answered).await.unwrap();

        let repository = loop {
            if let Some(read) = &hub.agents[&agent].view.summary().repository {
                break read.clone();
            }
            let input = tokio::time::timeout(Duration::from_secs(10), inputs.recv())
                .await
                .expect("the repository came in time")
                .unwrap();
            hub.handle(input);
        };
        let earlier_folder = temp.path().join("app");
        hub.repository_read(
            &earlier_folder,
            Ok(Some(Repository::from_remote("github.com/acme/app"))),
        );

        assert_eq!(repository.as_str(), "github.com/acme/shop");
        assert_eq!(
            hub.agents[&agent].view.summary().repository,
            Some(repository),
            "a read of the folder the agent left changes nothing"
        );
    }

    #[tokio::test]
    async fn an_agent_does_not_resume_while_it_moves() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, agent) = hub_with_agent(temp.path()).await;
        let target = temp.path().join("app_moved");
        std::fs::create_dir_all(&target).unwrap();
        let (reply, mut answered) = oneshot::channel();
        hub.change_folder(
            FolderParams {
                agent,
                folder: target.clone(),
            },
            reply,
        );

        let mut resumed_meanwhile = None;
        let result = loop {
            if let Ok(result) = answered.try_recv() {
                break result.into_result();
            }
            let input = tokio::time::timeout(Duration::from_secs(10), inputs.recv())
                .await
                .unwrap()
                .unwrap();
            // The process exited, and the copy is done but not handled yet.
            if matches!(input, HubInput::MoveCopied { .. }) {
                let (reply, resumed) = oneshot::channel();
                hub.resume(agent, ResponseBody::Done, reply);
                resumed_meanwhile = Some(resumed);
            }
            hub.handle(input);
        };

        assert_eq!(result, Ok(ResponseBody::Done));
        let mut resumed = resumed_meanwhile.expect("the copy came back");
        let ResponseResult::Error(refused) = resumed.try_recv().unwrap() else {
            panic!("expected the resume to be refused");
        };
        assert_eq!(refused.code, ErrorCode::AgentRunning);
        assert_eq!(hub.agents[&agent].record.folder, target);
        assert!(hub.agents[&agent].moving.is_none());
    }

    /// The move waits for the process to exit, and a stop comes first: the agent moves, and
    /// stays stopped.
    #[tokio::test]
    async fn a_stop_while_a_move_waits_leaves_the_agent_stopped() {
        use crate::store::AgentsFile;

        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, agent) = hub_with_agent(temp.path()).await;
        let target = temp.path().join("app_moved");
        std::fs::create_dir_all(&target).unwrap();
        let (reply, answered) = oneshot::channel();

        hub.move_to(agent, target.clone(), reply);
        let stopped = hub.stop(agent);
        let result = hub.await_answer(&mut inputs, answered).await;

        assert_eq!(stopped, Ok(ResponseBody::Done));
        assert_eq!(result, Ok(ResponseBody::Done));
        let moved = &hub.agents[&agent];
        assert_eq!(moved.record.folder, target);
        assert!(
            moved.process.is_none() && !moved.starting,
            "claude does not run again"
        );
        assert!(!moved.record.running);
        let saved = AgentsFile::load(&hub.state.agents_file()).unwrap();
        assert!(saved.agents.iter().all(|record| !record.running));
    }

    /// The move runs the agent again, and a stop comes while `claude` starts: the start is given
    /// up, and the agent stays stopped in its new folder.
    #[tokio::test]
    async fn a_stop_while_a_moved_agent_starts_gives_the_start_up() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, agent) = hub_with_agent(temp.path()).await;
        let target = temp.path().join("app_moved");
        std::fs::create_dir_all(&target).unwrap();
        let (reply, mut answered) = oneshot::channel();

        hub.move_to(agent, target.clone(), reply);
        let start = loop {
            let input = tokio::time::timeout(Duration::from_secs(10), inputs.recv())
                .await
                .expect("an input came in time")
                .expect("the hub's inputs are open");
            if matches!(input, HubInput::ProcessStarted { .. }) {
                break input;
            }
            hub.handle(input);
        };
        let stopped = hub.stop(agent);
        hub.handle(start);

        assert_eq!(stopped, Ok(ResponseBody::Done));
        assert_eq!(
            answered.try_recv().unwrap(),
            ResponseResult::Ok(ResponseBody::Done)
        );
        let moved = &hub.agents[&agent];
        assert_eq!(moved.record.folder, target);
        assert!(moved.process.is_none() && !moved.starting);
        assert!(!moved.record.running);
    }

    /// A close while the move waits for the process to exit answers the move.
    #[tokio::test]
    async fn a_close_while_a_move_waits_answers_the_move() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, _inputs, agent) = hub_with_agent(temp.path()).await;
        let target = temp.path().join("app_moved");
        std::fs::create_dir_all(&target).unwrap();
        let (reply, mut answered) = oneshot::channel();

        hub.move_to(agent, target, reply);
        let closed = hub.close(agent);

        assert_eq!(closed, Ok(ResponseBody::Done));
        let Ok(ResponseResult::Error(gone)) = answered.try_recv() else {
            panic!("expected the move to find no agent");
        };
        assert_eq!(gone.code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn an_agent_without_a_session_or_a_folder_cannot_move() {
        let temp = tempfile::tempdir().unwrap();
        let (mut hub, mut inputs, agent) = hub_with_agent(temp.path()).await;
        let change = |folder: PathBuf| Request::AgentChangeFolder(FolderParams { agent, folder });

        let missing = hub
            .ask(&mut inputs, change(temp.path().join("nowhere")))
            .await
            .unwrap_err()
            .code;
        if let Some(started) = hub.agents.get_mut(&agent) {
            started.record.session = None;
        }
        let no_session = hub
            .ask(&mut inputs, change(temp.path().to_owned()))
            .await
            .unwrap_err()
            .code;

        assert_eq!(missing, ErrorCode::NotFound);
        assert_eq!(no_session, ErrorCode::InvalidRequest);
    }
}
