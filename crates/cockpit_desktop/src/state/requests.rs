//! The requests the app sends, and how each answer updates the state.

use std::path::PathBuf;

use cockpit_client::RequestError;
use cockpit_protocol::{
    AgentDetails, AgentId, AgentParams, ErrorCode, FindFilesParams, FoldersListParams,
    HistoryParams, ItemId, ListDirParams, Method, Position, ReadFileParams, Request, ResponseBody,
};

use super::actions;
use super::app_state::{AgentEntry, AppState, FileRead, Loadable};
use super::drafts::{Draft, Drafts};
use super::transcript::HistoryState;
use super::ui_state::{Dialog, OpenFileView};

/// The most transcript items one history request asks for.
pub const HISTORY_PAGE: u16 = 100;

#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub request: Request,
    pub purpose: Purpose,
}

/// What to do with the answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Purpose {
    /// Answered by `Done`: only a failure shows.
    Action,
    History {
        agent: AgentId,
        older: bool,
    },
    Details {
        agent: AgentId,
    },
    ContextUsage {
        agent: AgentId,
    },
    McpStatus {
        agent: AgentId,
    },
    Commands {
        agent: AgentId,
    },
    ReadFile {
        agent: AgentId,
        path: PathBuf,
    },
    Sessions,
    /// A folder for the "Open file" dialog.
    ListDir {
        agent: AgentId,
        folder: PathBuf,
    },
    /// The project's files for the "Open file" dialog's bare name.
    FindFiles {
        agent: AgentId,
        name: String,
    },
    /// The folders under the new agent dialog's folder field.
    Folders {
        folder: PathBuf,
    },
    /// `agent.start` or `sessions.resume`: the new agent opens.
    Start,
    /// The draft comes back to the composer when the prompt fails.
    Prompt {
        agent: AgentId,
        draft: Draft,
    },
    /// `agent.move`, whose positions the sidebar already shows. They go back when the move
    /// fails, unless an event changed them meanwhile.
    Move {
        shifts: Vec<Shift>,
    },
    /// `agent.close`, whose agent the app already took off. It comes back when the close fails,
    /// unless the daemon has no such agent.
    Close {
        entry: Box<AgentEntry>,
    },
    /// `sessions.resume` from the undo note: the agent opens, then goes back to its place.
    Reopen,
}

/// An agent's position before and after a move the daemon has not answered yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shift {
    pub agent: AgentId,
    pub from: Position,
    pub to: Position,
}

impl Outgoing {
    pub fn action(request: Request) -> Self {
        Self {
            request,
            purpose: Purpose::Action,
        }
    }
}

/// The requests the open views need and that are not on their way yet: the selected agent's
/// history, details and open file, and the past sessions and the folder field's list while the new
/// agent dialog is open.
pub fn needed_requests(state: &mut AppState) -> Vec<Outgoing> {
    let mut requests = Vec::new();
    if !state.is_connected() {
        return requests;
    }
    if let Some(reopening) = &state.ui.reopening {
        let back = state
            .agents
            .values()
            .find(|entry| entry.summary.session.as_ref() == Some(&reopening.session))
            .map(AgentEntry::id);
        if let Some(agent) = back {
            let before = reopening.before;
            state.ui.reopening = None;
            requests.extend(actions::move_agent(state, agent, before));
        }
    }
    if matches!(state.ui.dialog, Some(Dialog::NewAgent(_)))
        && state.sessions == Loadable::NotRequested
    {
        state.sessions = Loadable::Loading;
        requests.push(Outgoing {
            request: Request::SessionsList,
            purpose: Purpose::Sessions,
        });
    }
    if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog
        && let Some(list) = &mut form.folders
        && list.listing == Loadable::NotRequested
    {
        list.listing = Loadable::Loading;
        let folder = list.folder.clone();
        requests.push(Outgoing {
            request: Request::FoldersList(FoldersListParams {
                path: folder.clone(),
            }),
            purpose: Purpose::Folders { folder },
        });
    }
    if let Some(Dialog::OpenFile(view)) = &mut state.ui.dialog {
        open_file_requests(view, &mut requests);
    }
    let Some(agent) = state.ui.selected else {
        return requests;
    };
    let open_file = state.ui.file_view(agent).map(|view| view.path.clone());
    let Some(entry) = state.agents.get_mut(&agent) else {
        return requests;
    };
    if entry.transcript.history() == &HistoryState::NotLoaded {
        entry.transcript.set_history(HistoryState::Loading);
        requests.push(history_request(agent, None));
    }
    if entry.details == Loadable::NotRequested {
        entry.details = Loadable::Loading;
        requests.push(Outgoing {
            request: Request::AgentDetails(AgentParams { agent }),
            purpose: Purpose::Details { agent },
        });
    }
    if let Some(path) = open_file {
        let text = entry.files.entry(path.clone()).or_default();
        if *text == Loadable::NotRequested {
            *text = Loadable::Loading;
            requests.push(Outgoing {
                request: Request::AgentReadFile(ReadFileParams {
                    agent,
                    path: path.clone(),
                }),
                purpose: Purpose::ReadFile { agent, path },
            });
        }
    }
    requests
}

/// The "Open file" dialog's folder and search, when not asked for yet.
fn open_file_requests(view: &mut OpenFileView, requests: &mut Vec<Outgoing>) {
    let agent = view.agent;
    if view.listing == Loadable::NotRequested {
        view.listing = Loadable::Loading;
        let folder = view.folder.clone();
        requests.push(Outgoing {
            request: Request::AgentListDir(ListDirParams {
                agent,
                path: folder.clone(),
            }),
            purpose: Purpose::ListDir { agent, folder },
        });
    }
    if let Some((name, search)) = &mut view.search
        && *search == Loadable::NotRequested
    {
        *search = Loadable::Loading;
        let name = name.clone();
        requests.push(Outgoing {
            request: Request::AgentFindFiles(FindFilesParams {
                agent,
                name: name.clone(),
            }),
            purpose: Purpose::FindFiles { agent, name },
        });
    }
}

/// The page of items before `before`, or the latest page.
pub fn history_request(agent: AgentId, before: Option<ItemId>) -> Outgoing {
    Outgoing {
        request: Request::AgentHistory(HistoryParams {
            agent,
            before,
            limit: HISTORY_PAGE,
        }),
        purpose: Purpose::History {
            agent,
            older: before.is_some(),
        },
    }
}

pub fn apply_response(
    state: &mut AppState,
    drafts: &mut Drafts,
    method: Method,
    purpose: Purpose,
    result: Result<ResponseBody, RequestError>,
) {
    let result = match result {
        Ok(body) => Ok(body),
        Err(error) => Err(Failure::from_request(&error, method)),
    };
    match purpose {
        Purpose::Action => {
            if let Err(failure) = result {
                state
                    .ui
                    .report(format!("{method} failed: {}", failure.message));
            }
        }
        Purpose::History { agent, older } => {
            let Some(entry) = state.agents.get_mut(&agent) else {
                return;
            };
            match result {
                Ok(ResponseBody::History(page)) => entry.transcript.merge_page(page),
                other => {
                    let failure = Failure::from_result(other);
                    let history = match (older, failure.transient) {
                        (true, _) => HistoryState::Loaded { has_older: true },
                        (false, true) => HistoryState::NotLoaded,
                        (false, false) => HistoryState::Failed(failure.message.clone()),
                    };
                    entry.transcript.set_history(history);
                    if older {
                        state
                            .ui
                            .report(format!("Cannot load older messages: {}", failure.message));
                    }
                }
            }
        }
        Purpose::Details { agent } => {
            let Some(entry) = state.agents.get_mut(&agent) else {
                return;
            };
            entry.details = match result {
                Ok(ResponseBody::Details(details)) => Loadable::Loaded(details),
                other => Failure::from_result(other).into_loadable(),
            };
        }
        Purpose::ContextUsage { agent } => {
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.context_loading = false;
            }
            match result {
                Ok(ResponseBody::ContextUsage(context)) => {
                    if let Some(details) = loaded_details(state, agent) {
                        details.context = Some(context);
                    }
                }
                other => report(state, method, other),
            }
        }
        Purpose::McpStatus { agent } => match result {
            Ok(ResponseBody::McpServers { servers }) => {
                if let Some(details) = loaded_details(state, agent) {
                    details.mcp_servers = servers;
                }
            }
            other => report(state, method, other),
        },
        Purpose::Commands { agent } => {
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.commands_requested = false;
            }
            match result {
                Ok(ResponseBody::Commands { commands }) => {
                    if let Some(details) = loaded_details(state, agent) {
                        details.commands = commands;
                    }
                }
                other => report(state, method, other),
            }
        }
        Purpose::ReadFile { agent, path } => {
            let read = match result {
                Ok(ResponseBody::File { path: found, text }) => {
                    let found = found.unwrap_or_else(|| path.clone());
                    actions::show_read_file(state, agent, &path, &found, text);
                    return;
                }
                Ok(ResponseBody::FileMatches { matches }) => {
                    Loadable::Loaded(FileRead::Choices(matches))
                }
                Err(failure) if failure.not_found => {
                    Loadable::Loaded(FileRead::Missing(failure.message))
                }
                other => Failure::from_result(other).into_loadable(),
            };
            // A file the view no longer shows is not kept.
            let held = state
                .agents
                .get_mut(&agent)
                .and_then(|entry| entry.files.get_mut(&path));
            if let Some(held) = held {
                *held = read;
            }
        }
        Purpose::Sessions => {
            state.sessions = match result {
                Ok(ResponseBody::Sessions { sessions }) => Loadable::Loaded(sessions),
                other => Failure::from_result(other).into_loadable(),
            };
        }
        // An answer for a folder or a name the dialog no longer shows is dropped.
        Purpose::ListDir { agent, folder } => {
            if let Some(Dialog::OpenFile(view)) = &mut state.ui.dialog
                && view.agent == agent
                && view.folder == folder
            {
                view.listing = match result {
                    Ok(ResponseBody::Directory(listing)) => Loadable::Loaded(listing),
                    other => Failure::from_result(other).into_loadable(),
                };
            }
        }
        Purpose::FindFiles { agent, name } => {
            if let Some(Dialog::OpenFile(view)) = &mut state.ui.dialog
                && view.agent == agent
                && let Some((searched, search)) = &mut view.search
                && *searched == name
            {
                *search = match result {
                    Ok(ResponseBody::FileMatches { matches }) => Loadable::Loaded(matches),
                    other => Failure::from_result(other).into_loadable(),
                };
            }
        }
        Purpose::Folders { folder } => {
            if let Err(failure) = &result
                && failure.unknown_method
            {
                state.folders_unknown = true;
                if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog {
                    form.folders = None;
                }
                return;
            }
            if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog
                && let Some(list) = &mut form.folders
                && list.folder == folder
            {
                list.listing = match result {
                    Ok(ResponseBody::Directory(listing)) => Loadable::Loaded(listing),
                    other => Failure::from_result(other).into_loadable(),
                };
            }
        }
        Purpose::Start => match result {
            Ok(ResponseBody::AgentStarted { agent }) => {
                actions::select_agent(state, agent);
                if matches!(state.ui.dialog, Some(Dialog::NewAgent(_))) {
                    state.ui.dialog = None;
                }
            }
            other => {
                if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog {
                    form.starting = false;
                }
                report(state, method, other);
            }
        },
        Purpose::Prompt { agent, draft } => {
            if let Err(failure) = result {
                let current = drafts.draft_mut(agent);
                if current.is_empty() {
                    *current = draft;
                    drafts.draft_written(agent);
                }
                state
                    .ui
                    .report(format!("The message was not sent: {}", failure.message));
            }
        }
        Purpose::Move { shifts } => {
            if let Err(failure) = result {
                for shift in shifts {
                    let entry = state
                        .agents
                        .get_mut(&shift.agent)
                        .filter(|entry| entry.summary.position == shift.to);
                    if let Some(entry) = entry {
                        entry.summary.position = shift.from;
                    }
                }
                state
                    .ui
                    .report(format!("The agent was not moved: {}", failure.message));
            }
        }
        // On success the agent stays among the closing ones until its `AgentRemoved`, so that an
        // event sent before the close does not bring it back.
        Purpose::Close { entry } => {
            if let Err(failure) = result {
                let agent = entry.id();
                state.ui.closing.remove(&agent);
                state.ui.closed.take_if(|closed| closed.agent == agent);
                if !failure.not_found {
                    state.agents.entry(agent).or_insert(*entry);
                    state
                        .ui
                        .report(format!("The agent was not closed: {}", failure.message));
                }
            }
        }
        Purpose::Reopen => match result {
            Ok(ResponseBody::AgentStarted { agent }) => actions::select_agent(state, agent),
            other => {
                state.ui.reopening = None;
                let failure = Failure::from_result(other);
                state
                    .ui
                    .report(format!("The agent did not come back: {}", failure.message));
            }
        },
    }
}

fn loaded_details(state: &mut AppState, agent: AgentId) -> Option<&mut AgentDetails> {
    state.agents.get_mut(&agent)?.details.loaded_mut()
}

fn report(state: &mut AppState, method: Method, result: Result<ResponseBody, Failure>) {
    let failure = Failure::from_result(result);
    state
        .ui
        .report(format!("{method} failed: {}", failure.message));
}

/// Why a request brought nothing usable.
struct Failure {
    message: String,
    /// The connection was missing or dropped: asking again after it comes back may work.
    transient: bool,
    /// The daemon found nothing by that name.
    not_found: bool,
    /// The daemon is older than the method.
    unknown_method: bool,
}

impl Failure {
    fn from_request(error: &RequestError, method: Method) -> Self {
        // A daemon that does not know the method cannot decode the request. The error code alone
        // also answers parameters that do not fit, but only this decode error names the method.
        let unknown = format!("unknown method `{}`", method.as_str());
        Self {
            message: error.to_string(),
            transient: matches!(
                error,
                RequestError::NotConnected | RequestError::Disconnected
            ),
            not_found: matches!(
                error,
                RequestError::Failed(body) if body.code == ErrorCode::NotFound
            ),
            unknown_method: matches!(
                error,
                RequestError::Failed(body)
                    if body.code == ErrorCode::InvalidRequest && body.message.starts_with(&unknown)
            ),
        }
    }

    /// For a result whose expected body was already matched: any other body is unexpected.
    fn from_result(result: Result<ResponseBody, Failure>) -> Self {
        match result {
            Ok(_) => Self {
                message: "the daemon answered with an unexpected response".to_owned(),
                transient: false,
                not_found: false,
                unknown_method: false,
            },
            Err(failure) => failure,
        }
    }

    fn into_loadable<T>(self) -> Loadable<T> {
        if self.transient {
            Loadable::NotRequested
        } else {
            Loadable::Failed(self.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use cockpit_client::SessionUpdate;
    use cockpit_protocol::{
        AgentStatus, Catchup, DirListing, ErrorBody, ErrorCode, HistoryPage, ItemBody,
        TranscriptItem,
    };

    use super::*;
    use crate::state::app_state::ConnectionState;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::folder_field;
    use crate::state::ui_state::{Block, FileView};

    fn connected_with_selection() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.connection = ConnectionState::Connected;
        state.ui.selected = Some(AgentId::new(1));
        state
    }

    fn methods(requests: &[Outgoing]) -> Vec<Method> {
        requests
            .iter()
            .map(|outgoing| outgoing.request.method())
            .collect()
    }

    #[test]
    fn opening_an_agent_asks_for_its_history_and_details_once() {
        let mut state = connected_with_selection();

        let first = needed_requests(&mut state);
        let second = needed_requests(&mut state);

        assert_eq!(
            methods(&first),
            [Method::AgentHistory, Method::AgentDetails]
        );
        assert!(second.is_empty());
    }

    #[test]
    fn nothing_is_asked_while_disconnected() {
        let mut state = connected_with_selection();
        state.connection = ConnectionState::Connecting { attempt: 1 };

        assert!(needed_requests(&mut state).is_empty());
    }

    #[test]
    fn the_open_file_dialog_asks_for_its_folder_and_drops_a_late_answer() {
        let mut state = connected_with_selection();
        let agent = AgentId::new(1);
        needed_requests(&mut state);
        crate::state::open_file::toggle(&mut state, &mut Drafts::default());

        let requests = needed_requests(&mut state);

        assert_eq!(methods(&requests), [Method::AgentListDir]);
        let answer = |state: &mut AppState, folder: &str| {
            let listing = DirListing {
                path: PathBuf::from(folder),
                entries: Vec::new(),
                truncated: false,
            };
            apply_response(
                state,
                &mut Drafts::default(),
                Method::AgentListDir,
                Purpose::ListDir {
                    agent,
                    folder: PathBuf::from(folder),
                },
                Ok(ResponseBody::Directory(listing)),
            );
        };
        answer(&mut state, "/work/project-1/src");
        let Some(Dialog::OpenFile(view)) = &state.ui.dialog else {
            panic!("the dialog stays open");
        };
        assert_eq!(view.listing, Loadable::Loading);

        answer(&mut state, "/work/project-1");
        let Some(Dialog::OpenFile(view)) = &state.ui.dialog else {
            panic!("the dialog stays open");
        };
        assert!(matches!(view.listing, Loadable::Loaded(_)));
    }

    #[test]
    fn the_folder_field_asks_for_its_folder_once_and_drops_a_late_answer() {
        let mut state = connected_with_selection();
        let mut drafts = Drafts::default();
        needed_requests(&mut state);
        actions::open_new_agent(&mut state, &mut drafts);
        assert_eq!(
            methods(&needed_requests(&mut state)),
            [Method::SessionsList]
        );
        drafts.new_agent.folder = "/home/dev/".to_owned();
        crate::state::folder_field::follow(&mut state, &drafts);

        let first = needed_requests(&mut state);
        let second = needed_requests(&mut state);

        assert_eq!(
            first,
            [Outgoing {
                request: Request::FoldersList(FoldersListParams {
                    path: PathBuf::from("/home/dev"),
                }),
                purpose: Purpose::Folders {
                    folder: PathBuf::from("/home/dev"),
                },
            }]
        );
        assert!(second.is_empty());
        let answer = |state: &mut AppState, folder: &str| {
            let listing = DirListing {
                path: PathBuf::from(folder),
                entries: Vec::new(),
                truncated: false,
            };
            apply_response(
                state,
                &mut Drafts::default(),
                Method::FoldersList,
                Purpose::Folders {
                    folder: PathBuf::from(folder),
                },
                Ok(ResponseBody::Directory(listing)),
            );
        };
        let listing = |state: &AppState| match &state.ui.dialog {
            Some(Dialog::NewAgent(form)) => form.folders.as_ref().map(|list| list.listing.clone()),
            _ => None,
        };
        answer(&mut state, "/home");
        assert_eq!(listing(&state), Some(Loadable::Loading));

        answer(&mut state, "/home/dev");
        assert!(matches!(listing(&state), Some(Loadable::Loaded(_))));
    }

    #[test]
    fn a_daemon_without_folders_list_leaves_the_recent_folders_until_it_reconnects() {
        let mut state = connected_with_selection();
        let mut drafts = Drafts::default();
        actions::open_new_agent(&mut state, &mut drafts);
        let mut type_folder = |state: &mut AppState, text: &str| {
            drafts.new_agent.folder = text.to_owned();
            folder_field::follow(state, &drafts);
            needed_requests(state)
        };
        let refuse = |state: &mut AppState, folder: &str, message: &str| {
            let body = ErrorBody::new(ErrorCode::InvalidRequest, message.to_owned());
            apply_response(
                state,
                &mut Drafts::default(),
                Method::FoldersList,
                Purpose::Folders {
                    folder: PathBuf::from(folder),
                },
                Err(RequestError::Failed(body)),
            );
        };
        let list = |state: &AppState| folder_field::list(state).cloned();
        needed_requests(&mut state);

        // A folder the daemon refuses shows why.
        type_folder(&mut state, "/home/dev/notes.txt/");
        refuse(&mut state, "/home/dev/notes.txt", "not a folder");
        let failed = list(&state).map(|list| list.listing);
        assert_eq!(failed, Some(Loadable::Failed("not a folder".to_owned())));

        type_folder(&mut state, "/home/dev/");
        let unknown = "unknown method `folders.list` at line 1 column 61";
        refuse(&mut state, "/home/dev", unknown);
        assert_eq!(list(&state), None);
        assert!(type_folder(&mut state, "/home/").is_empty());
        assert_eq!(list(&state), None);

        crate::state::apply::apply_update(
            &mut state,
            &mut Drafts::default(),
            SessionUpdate::Connected(Catchup::Replay),
            Instant::now(),
        );
        assert_eq!(
            methods(&type_folder(&mut state, "/srv/")),
            [Method::FoldersList]
        );
    }

    #[test]
    fn an_open_file_view_asks_for_the_file() {
        let mut state = connected_with_selection();
        let agent = AgentId::new(1);
        state.ui.file_views.insert(
            agent,
            FileView::new(PathBuf::from("src/main.rs"), Some(3), Default::default()),
        );

        let requests = needed_requests(&mut state);

        assert_eq!(
            requests.last().map(|outgoing| &outgoing.purpose),
            Some(&Purpose::ReadFile {
                agent,
                path: PathBuf::from("src/main.rs")
            })
        );
    }

    #[test]
    fn a_history_page_loads_and_a_dropped_connection_asks_again() {
        let mut state = connected_with_selection();
        let agent = AgentId::new(1);
        needed_requests(&mut state);
        let page = HistoryPage {
            items: vec![TranscriptItem {
                id: ItemId::new(1),
                parent: None,
                body: ItemBody::Notice {
                    text: "compacted".to_owned(),
                    detail: None,
                },
            }],
            has_older: false,
        };

        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentHistory,
            Purpose::History {
                agent,
                older: false,
            },
            Ok(ResponseBody::History(page)),
        );
        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentDetails,
            Purpose::Details { agent },
            Err(RequestError::Disconnected),
        );

        let entry = &state.agents[&agent];
        assert_eq!(
            entry.transcript.history(),
            &HistoryState::Loaded { has_older: false }
        );
        assert_eq!(entry.details, Loadable::NotRequested);
        assert_eq!(
            methods(&needed_requests(&mut state)),
            [Method::AgentDetails]
        );
    }

    #[test]
    fn a_refused_request_shows_an_error_note() {
        let mut state = connected_with_selection();

        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentInterrupt,
            Purpose::Action,
            Err(RequestError::Failed(ErrorBody::new(
                ErrorCode::AgentExited,
                "the agent exited".to_owned(),
            ))),
        );

        let texts: Vec<&str> = state
            .ui
            .errors
            .iter()
            .map(|note| note.text.as_str())
            .collect();
        assert_eq!(texts, ["agent.interrupt failed: the agent exited"]);
    }

    #[test]
    fn a_failed_prompt_gives_the_draft_back() {
        let mut state = connected_with_selection();
        let agent = AgentId::new(1);
        let draft = Draft {
            text: "Run the tests".to_owned(),
            images: Vec::new(),
        };
        let mut drafts = Drafts::default();

        apply_response(
            &mut state,
            &mut drafts,
            Method::AgentPrompt,
            Purpose::Prompt {
                agent,
                draft: draft.clone(),
            },
            Err(RequestError::NotConnected),
        );

        assert_eq!(drafts.draft(agent), Some(&draft));
        assert_eq!(drafts.composer_writes(agent), 1);
        assert_eq!(state.ui.errors.len(), 1);
    }

    /// The composer shows another agent: a restored draft must not rewrite its text while its
    /// keystrokes are on their way.
    #[test]
    fn a_failed_prompt_for_another_agent_leaves_the_composer_alone() {
        let mut state = connected_with_selection();
        let mut drafts = Drafts::default();
        let shown = state.ui.selected.expect("the helper selects an agent");
        let other = AgentId::new(shown.get() + 1);

        apply_response(
            &mut state,
            &mut drafts,
            Method::AgentPrompt,
            Purpose::Prompt {
                agent: other,
                draft: Draft {
                    text: "Run the tests".to_owned(),
                    images: Vec::new(),
                },
            },
            Err(RequestError::NotConnected),
        );

        assert_eq!(drafts.composer_writes(shown), 0);
        assert_eq!(drafts.composer_writes(other), 1);
    }

    #[test]
    fn a_started_agent_opens_and_closes_the_dialog() {
        let mut state = connected_with_selection();
        state.ui.dialog = Some(Dialog::NewAgent(Default::default()));
        // What the agent shown before had open.
        state.ui.badge = Some(Block::Mcp);
        state.ui.mcp.detail = Some("search".to_owned());

        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentStart,
            Purpose::Start,
            Ok(ResponseBody::AgentStarted {
                agent: AgentId::new(9),
            }),
        );

        assert_eq!(state.ui.selected, Some(AgentId::new(9)));
        assert_eq!(state.ui.dialog, None);
        assert_eq!(state.ui.badge, None);
        assert_eq!(state.ui.mcp.detail, None);
    }

    #[test]
    fn mcp_status_updates_loaded_details() {
        let mut state = connected_with_selection();
        let agent = AgentId::new(1);
        if let Some(entry) = state.agents.get_mut(&agent) {
            entry.details = Loadable::Loaded(AgentDetails::default());
        }

        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentCommands,
            Purpose::Commands { agent },
            Ok(ResponseBody::Done),
        );

        assert_eq!(
            state.ui.errors.first().map(|note| note.text.as_str()),
            Some("agent.commands failed: the daemon answered with an unexpected response")
        );
    }
}
