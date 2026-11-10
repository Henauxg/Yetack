//! Which method does what. The requests that read files or run commands do it in a task of their
//! own, so the hub never waits on them.

use std::path::PathBuf;

use claude_protocol::{ControlRequest, TaskId};
use cockpit_protocol::{
    AgentId, ErrorBody, ErrorCode, Event, FindFilesParams, FoldersListParams, HistoryParams,
    ListDirParams, MAX_LINE_BYTES, McpRemoveParams, McpToggleAction, ReadFileParams, Request,
    ResponseBody, SessionResumeParams, Settings, StopProcessParams,
};

use crate::accounts::ClaudeEnv;
use crate::daemon_config::HOME_ENV;
use crate::hub::input::{ControlPurpose, HubInput, Reply};
use crate::hub::state::{Agent, Hub};
use crate::hub::{agents, errors, history};
use crate::machine_load::AgentRoot;
use crate::read_file::{Content, Found};
use crate::sessions::AccountFolder;
use crate::store::{DaemonSettings, LogName};
use crate::transcript::{finished_edits, touched_files, words};
use crate::{browse, machine_load, mcp_remove, read_file, sessions};

/// The most JSON bytes of edits a file's answer holds.
const MAX_EDITS_BYTES: usize = MAX_LINE_BYTES / 4;
/// What a file's answer adds around its text and edits, its path included, with room to spare.
const FILE_ANSWER_ENVELOPE_BYTES: usize = 64 * 1024;

/// What the requests that read the agent's files need, taken from the hub before they leave it.
struct FileAccess {
    folder: PathBuf,
    touched: Vec<PathBuf>,
    home: Option<PathBuf>,
}

impl Hub {
    pub(super) fn request(&mut self, request: Request, reply: Reply) {
        match request {
            Request::AgentStart(params) => agents::respond(reply, self.start_agent(params)),
            Request::AgentPrompt(params) => agents::respond(
                reply,
                self.prompt(params.agent, params.prompt, params.delivery),
            ),
            Request::AgentCancelQueued(params) => {
                agents::respond(reply, self.cancel_queued(params))
            }
            Request::AgentSendQueued(params) => agents::respond(reply, self.send_queued(params)),
            Request::AgentAnswer(params) => agents::respond(reply, self.answer(params)),
            Request::AgentInterrupt(params) => self.control(
                params.agent,
                ControlRequest::Interrupt,
                ControlPurpose::Done(reply),
            ),
            Request::AgentStop(params) => agents::respond(reply, self.stop(params.agent)),
            Request::AgentResume(params) => agents::respond(reply, self.resume(params.agent)),
            Request::AgentRemove(params) => agents::respond(reply, self.remove(params.agent)),
            Request::AgentClose(params) => agents::respond(reply, self.close(params.agent)),
            Request::AgentHistory(params) => agents::respond(reply, self.history(params)),
            Request::AgentDetails(params) => {
                let details = self
                    .agent(params.agent)
                    .map(|agent| ResponseBody::Details(agent.view.details().clone()));
                agents::respond(reply, details);
            }
            Request::AgentSetModel(params) => self.set_model(params, reply),
            Request::AgentSetEffort(params) => agents::respond(reply, self.set_effort(params)),
            Request::AgentSetMode(params) => self.set_mode(params, reply),
            Request::AgentRename(params) => agents::respond(reply, self.rename(params)),
            Request::AgentMark(params) => agents::respond(reply, self.mark(params)),
            Request::AgentFork(params) => self.fork(params, reply),
            Request::AgentChangeFolder(params) => self.change_folder(params, reply),
            Request::AgentMove(params) => agents::respond(reply, self.move_agent(params)),
            Request::AgentContextUsage(params) => self.control(
                params.agent,
                ControlRequest::GetContextUsage,
                ControlPurpose::ContextUsage(reply),
            ),
            Request::AgentStopTask(params) => self.control(
                params.agent,
                ControlRequest::StopTask {
                    task_id: TaskId::new(params.task.as_str().to_owned()),
                },
                ControlPurpose::Done(reply),
            ),
            Request::AgentStopProcess(params) => self.stop_process(params, reply),
            Request::AgentMcpStatus(params) => self.control(
                params.agent,
                ControlRequest::McpStatus,
                ControlPurpose::McpStatus(Some(reply)),
            ),
            Request::AgentMcpReconnect(params) => self.control(
                params.agent,
                ControlRequest::McpReconnect {
                    server_name: params.server,
                },
                ControlPurpose::McpChanged(reply),
            ),
            Request::AgentMcpToggle(params) => self.control(
                params.agent,
                ControlRequest::McpToggle {
                    server_name: params.server,
                    enabled: params.action == McpToggleAction::Enable,
                },
                ControlPurpose::McpChanged(reply),
            ),
            Request::AgentCommands(params) => {
                let commands = self
                    .agent(params.agent)
                    .map(|agent| ResponseBody::Commands {
                        commands: agent.view.details().commands.clone(),
                    });
                agents::respond(reply, commands);
            }
            Request::AgentReadFile(params) => self.read_file(params, reply),
            Request::AgentListDir(params) => self.list_dir(params, reply),
            Request::AgentFindFiles(params) => self.find_files(params, reply),
            Request::McpRemove(params) => self.mcp_remove(params, reply),
            Request::SettingsSet(settings) => agents::respond(reply, self.set_settings(settings)),
            Request::SessionsList => self.sessions_list(reply),
            Request::SessionsResume(params) => self.sessions_resume(params, reply),
            Request::FoldersList(params) => list_folders(params, reply),
            Request::MachineLoad => {
                let roots = self.agent_roots();
                self.load.query(roots, reply);
            }
            Request::AccountsList => self.accounts_list(reply),
            Request::AccountsAdd(params) => self.accounts_add(params, reply),
            Request::AccountsChange(params) => self.accounts_change(params, reply),
            Request::AccountsRemove(params) => {
                agents::respond(reply, self.accounts_remove(params.account));
            }
        }
    }

    pub(super) fn agent(&self, id: AgentId) -> Result<&Agent, ErrorBody> {
        self.agents.get(&id).ok_or(errors::agent_not_found(id))
    }

    fn history(&self, params: HistoryParams) -> Result<ResponseBody, ErrorBody> {
        let agent = self.agent(params.agent)?;
        let page = history::page(agent.view.items(), params.before, params.limit);
        Ok(ResponseBody::History(page))
    }

    /// What the requests that read the agent's files need: its folder, the files it read or
    /// changed, and the home folder.
    fn file_access(&self, agent: AgentId) -> Result<FileAccess, ErrorBody> {
        let agent = self.agent(agent)?;
        Ok(FileAccess {
            folder: agent.record.folder.clone(),
            touched: touched_files(agent.view.items(), &agent.view.details().changed_files),
            home: std::env::var_os(HOME_ENV).map(PathBuf::from),
        })
    }

    fn read_file(&self, params: ReadFileParams, reply: Reply) {
        let access = match self.file_access(params.agent) {
            Ok(access) => access,
            Err(error) => return agents::respond(reply, Err(error)),
        };
        // The file found has the name asked for, wherever it is.
        let named = match (self.agent(params.agent), params.path.file_name()) {
            (Ok(agent), Some(name)) => {
                let edits = finished_edits(agent.view.items());
                read_file::edits_named(edits, &access.folder, name, MAX_EDITS_BYTES)
            }
            _ => Vec::new(),
        };
        tokio::spawn(async move {
            let FileAccess {
                folder,
                touched,
                home,
            } = access;
            let found = read_file::find_and_read(folder.clone(), touched, home, params.path).await;
            let result = match found {
                Ok(Found::File {
                    path,
                    content: Content::Text(text),
                }) => {
                    let max_bytes = max_edits_bytes(&text);
                    let edits = read_file::edits_of(named, &folder, &path, max_bytes);
                    Ok(ResponseBody::File {
                        path: Some(path),
                        text,
                        edits: Some(edits),
                    })
                }
                Ok(Found::File {
                    path,
                    content: Content::Image(image),
                }) => Ok(ResponseBody::Image { path, image }),
                Ok(Found::Several(matches)) => Ok(ResponseBody::FileMatches { matches }),
                Err(error) => Err(error.to_error_body()),
            };
            agents::respond(reply, result);
        });
    }

    /// The `claude` process of each agent whose process runs.
    fn agent_roots(&self) -> Vec<AgentRoot> {
        let running = self
            .agents
            .iter()
            .filter_map(|(&agent, found)| AgentRoot::new(agent, found.process.as_ref()?));
        running.collect()
    }

    fn stop_process(&self, params: StopProcessParams, reply: Reply) {
        let root = match self.agent(params.agent) {
            Ok(agent) => agent
                .process
                .as_ref()
                .and_then(|process| AgentRoot::new(params.agent, process)),
            Err(error) => return agents::respond(reply, Err(error)),
        };
        let Some(root) = root else {
            return agents::respond(reply, Err(errors::agent_exited(params.agent)));
        };
        tokio::spawn(async move {
            let stopped = tokio::task::spawn_blocking(move || {
                machine_load::stop_descendant(root, params.pid)
            })
            .await;
            let result = match stopped {
                Ok(Ok(())) => Ok(ResponseBody::Done),
                Ok(Err(error)) => Err(error.to_error_body()),
                Err(error) => Err(errors::internal(error.to_string())),
            };
            agents::respond(reply, result);
        });
    }

    fn list_dir(&self, params: ListDirParams, reply: Reply) {
        let access = match self.file_access(params.agent) {
            Ok(access) => access,
            Err(error) => return agents::respond(reply, Err(error)),
        };
        tokio::spawn(async move {
            let FileAccess {
                folder,
                touched,
                home,
            } = access;
            let listed = browse::list(folder, touched, home, params.path).await;
            let result = match listed {
                Ok(listing) => Ok(ResponseBody::Directory(listing)),
                Err(error) => Err(error.to_error_body()),
            };
            agents::respond(reply, result);
        });
    }

    fn find_files(&self, params: FindFilesParams, reply: Reply) {
        let access = match self.file_access(params.agent) {
            Ok(access) => access,
            Err(error) => return agents::respond(reply, Err(error)),
        };
        tokio::spawn(async move {
            let FileAccess {
                folder,
                touched,
                home,
            } = access;
            let found = browse::find_files(folder, touched, home, params.name).await;
            let result = match found {
                Ok(matches) => Ok(ResponseBody::FileMatches { matches }),
                Err(error) => Err(error.to_error_body()),
            };
            agents::respond(reply, result);
        });
    }

    fn mcp_remove(&self, params: McpRemoveParams, reply: Reply) {
        let found = self.agent(params.agent).and_then(|agent| {
            let account = self.account(agent.record.account)?;
            Ok((agent.record.folder.clone(), ClaudeEnv::new(&account)))
        });
        let (folder, env) = match found {
            Ok(found) => found,
            Err(error) => return agents::respond(reply, Err(error)),
        };
        let Some(scope) = words::removable_scope(params.scope) else {
            let error = errors::invalid(
                "only the user, project and local scopes can be removed".to_owned(),
            );
            return agents::respond(reply, Err(error));
        };
        let program = self.config.claude_program.clone();
        tokio::spawn(async move {
            let result = mcp_remove::remove(&program, &folder, &env, &params.server, scope).await;
            agents::respond(reply, result.map(|()| ResponseBody::Done));
        });
    }

    fn set_settings(&mut self, settings: Settings) -> Result<ResponseBody, ErrorBody> {
        let updated = DaemonSettings {
            shared: settings.clone(),
            ..self.settings.clone()
        };
        updated
            .save(&self.state.settings_file())
            .map_err(|error| errors::internal(errors::chain(&error)))?;
        self.settings = updated;
        self.emit(LogName::Daemon, Event::SettingsChanged(settings));
        Ok(ResponseBody::Done)
    }

    fn sessions_list(&self, reply: Reply) {
        let folders = self.account_folders();
        tokio::spawn(async move {
            let listed = tokio::task::spawn_blocking(move || sessions::list(&folders)).await;
            let result = match listed {
                Ok(sessions) => Ok(ResponseBody::Sessions { sessions }),
                Err(error) => Err(errors::internal(error.to_string())),
            };
            agents::respond(reply, result);
        });
    }

    /// Finds the session on disk and reads its transcript in a task, which hands them back to the
    /// hub before the agent exists.
    fn sessions_resume(&mut self, params: SessionResumeParams, reply: Reply) {
        let account = match self.account(params.account) {
            Ok(account) => account,
            Err(error) => return agents::respond(reply, Err(error)),
        };
        if let Some(agent) = self.agent_with_session(&params.session, account.id) {
            if self
                .agents
                .get(&agent)
                .is_some_and(|found| found.process.is_none())
                && let Err(error) = self.resume(agent)
            {
                return agents::respond(reply, Err(error));
            }
            return agents::respond(reply, Ok(ResponseBody::AgentStarted { agent }));
        }
        let folder = AccountFolder {
            account: account.id,
            folder: account.folder,
        };
        let inputs = self.inputs.clone();
        tokio::spawn(async move {
            let id = params.session;
            let found = tokio::task::spawn_blocking(move || sessions::find(&folder, &id)).await;
            match found {
                Ok(Some(found)) => {
                    let input = HubInput::SessionFound {
                        found: Box::new(found),
                        reply,
                    };
                    if let Err(unsent) = inputs.send(input).await
                        && let HubInput::SessionFound { reply, .. } = unsent.0
                    {
                        agents::respond(
                            reply,
                            Err(errors::internal("the daemon is stopping".to_owned())),
                        );
                    }
                }
                Ok(None) => agents::respond(
                    reply,
                    Err(ErrorBody::new(
                        ErrorCode::NotFound,
                        "no such Claude session".to_owned(),
                    )),
                ),
                Err(error) => agents::respond(reply, Err(errors::internal(error.to_string()))),
            }
        });
    }
}

/// The most JSON bytes of edits that fit in a file's answer with `text`. A control character
/// takes six bytes in JSON, so a text can fill most of a frame on its own.
fn max_edits_bytes(text: &str) -> usize {
    let text_bytes = serde_json::to_vec(text).map_or(usize::MAX, |json| json.len());
    let room = MAX_LINE_BYTES
        .saturating_sub(FILE_ANSWER_ENVELOPE_BYTES)
        .saturating_sub(text_bytes);
    MAX_EDITS_BYTES.min(room)
}

fn list_folders(params: FoldersListParams, reply: Reply) {
    tokio::spawn(async move {
        let listed = tokio::task::spawn_blocking(move || browse::list_folders(&params.path)).await;
        let result = match listed {
            Ok(Ok(listing)) => Ok(ResponseBody::Directory(listing)),
            Ok(Err(error)) => Err(error.to_error_body()),
            Err(error) => Err(errors::internal(error.to_string())),
        };
        agents::respond(reply, result);
    });
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn the_edits_leave_room_for_a_text_that_json_makes_larger() {
        // 2.5 MiB of control characters take 15 MiB in JSON.
        let escaped = "\u{1}".repeat(5 * 512 * 1024);

        let room = max_edits_bytes(&escaped);

        assert_eq!(max_edits_bytes("fn main() {}\n"), MAX_EDITS_BYTES);
        assert!(room < 1024 * 1024, "{room}");
        let text_bytes = serde_json::to_vec(&escaped).unwrap().len();
        assert!(text_bytes + room + FILE_ANSWER_ENVELOPE_BYTES <= MAX_LINE_BYTES);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn folders_list_answers_the_folders_and_refuses_a_relative_path() {
        use cockpit_protocol::{EntryKind, ResponseResult};
        use tokio::sync::oneshot;

        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(temp.path());
        let projects = temp.path().canonicalize().unwrap().join("projects");
        std::fs::create_dir_all(projects.join("shop")).unwrap();
        std::fs::write(projects.join("notes.txt"), "notes").unwrap();
        let mut ask = |path: PathBuf| {
            let (reply, answer) = oneshot::channel();
            hub.request(Request::FoldersList(FoldersListParams { path }), reply);
            answer
        };

        let listed = ask(projects.clone()).await.unwrap();
        let refused = ask(PathBuf::from("~/projects")).await.unwrap();

        let ResponseResult::Ok(ResponseBody::Directory(listing)) = listed else {
            panic!("a listing, not {listed:?}");
        };
        let names: Vec<(&str, EntryKind)> = listing
            .entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind))
            .collect();
        assert_eq!(names, [("shop", EntryKind::Folder)]);
        let ResponseResult::Error(error) = refused else {
            panic!("an error, not {refused:?}");
        };
        assert_eq!(error.code, ErrorCode::InvalidRequest);
    }

    #[test]
    fn settings_that_cannot_be_saved_are_not_applied() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(temp.path());
        // A folder in the file's place makes the save fail.
        std::fs::create_dir(hub.state.settings_file()).unwrap();
        let settings = Settings {
            hidden_mcp_servers: BTreeSet::from(["notes".to_owned()]),
        };

        let result = hub.set_settings(settings);

        assert!(result.is_err(), "{result:?}");
        assert_eq!(hub.settings.shared, Settings::default());
        assert_eq!(hub.snapshot().settings, Settings::default());
    }
}
