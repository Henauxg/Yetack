//! The requests the app sends, and how each answer updates the state.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use cockpit_client::RequestError;
use cockpit_protocol::{
    AgentDetails, AgentId, AgentParams, ErrorCode, FileEdit, FindFilesParams, FoldersListParams,
    HistoryParams, ItemId, ListDirParams, Method, Position, ReadFileParams, Request, ResponseBody,
    Seq, Timestamp,
};

use super::app_state::{AgentEntry, AppState, FileRead, Loadable, MachineState, Reread};
use super::chat_images::ImageKey;
use super::drafts::{Draft, Drafts};
use super::projects::{self, ProjectKey};
use super::sending::{self, SendingId};
use super::transcript::HistoryState;
use super::ui_state::{Dialog, FolderAction, OpenFileView};
use super::{
    accounts, actions, chat_images, details_updates, file_edits, file_tree, mcp_reconnect,
    working_folder,
};
use crate::machine::MachineId;
use crate::path_style;
use crate::prefs::AccountColor;

/// The most transcript items one history request asks for.
pub const HISTORY_PAGE: u16 = 100;

#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub request: Request,
    pub purpose: Purpose,
    /// The machine a request about no agent goes to. One about an agent goes to its agent's.
    pub to: MachineId,
}

/// What to do with the answer.
///
/// A `details_seq` is the agent's when the request left. The answer goes into the details only
/// when it has not moved since (`details_updates::unchanged_since`).
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
        details_seq: Option<Seq>,
    },
    McpStatus {
        agent: AgentId,
        details_seq: Option<Seq>,
    },
    /// `agent.mcp_reconnect` from a server's page, which shows how it goes.
    McpReconnect {
        agent: AgentId,
        server: String,
    },
    /// The servers' status once Claude Code tried to reconnect `server`.
    McpReconnected {
        agent: AgentId,
        server: String,
        details_seq: Option<Seq>,
    },
    Commands {
        agent: AgentId,
    },
    ReadFile {
        agent: AgentId,
        path: PathBuf,
    },
    /// An image a message shows.
    ChatImage {
        agent: AgentId,
        key: ImageKey,
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
    /// A folder of the files pane's tree.
    TreeListing {
        agent: AgentId,
        folder: PathBuf,
    },
    /// The project's files for the files pane's search.
    TreeSearch {
        agent: AgentId,
        name: String,
    },
    /// The folders under the new agent dialog's folder field.
    Folders {
        folder: PathBuf,
    },
    /// `agent.start` or `sessions.resume`: the new agent opens, in the project picked for it.
    Start {
        project: Option<ProjectKey>,
    },
    /// `agent.fork` or `agent.change_folder` from the working folder dialog.
    WorkingFolder {
        action: FolderAction,
    },
    /// The draft comes back to the composer when the prompt fails.
    Prompt {
        agent: AgentId,
        draft: Draft,
        /// The message shown on its way, which a refusal takes away.
        sending: SendingId,
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
    /// `accounts.list`: whether each account is signed in.
    Accounts,
    /// `accounts.add` from the "Add an account" dialog, with the color picked there.
    AccountAdded {
        color: AccountColor,
    },
    /// `accounts.change`: a rename or the environment choice.
    AccountChanged,
    /// The sign-in card's "Send the message again", which it offers again when the prompt fails.
    SendAgain {
        agent: AgentId,
        turn_end: ItemId,
    },
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
            to: MachineId::THIS,
        }
    }

    /// The machine the request goes to: its agent's, else `to`.
    pub fn machine(&mut self) -> MachineId {
        let to = self.to;
        self.request
            .agent_mut()
            .map_or(to, |agent| MachineId::of(*agent))
    }
}

/// The requests the open views need and that are not on their way yet: the selected agent's
/// history, details and open file, and the past sessions and the folder field's list while the new
/// agent dialog is open.
pub fn needed_requests(state: &mut AppState) -> Vec<Outgoing> {
    let mut requests = Vec::new();
    requests.extend(mcp_reconnect::status_requests(state));
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
    let answering: Vec<MachineId> = std::iter::once(MachineId::THIS)
        .chain(state.machines.others.keys().copied())
        .filter(|machine| {
            state
                .machines
                .get(*machine)
                .is_some_and(MachineState::is_connected)
        })
        .collect();
    for machine in answering {
        requests.extend(accounts::list_request(state, machine));
    }
    let form_machine = match &state.ui.dialog {
        Some(Dialog::NewAgent(form)) => Some(form.machine),
        _ => None,
    };
    if let Some(machine) = form_machine
        && let Some(known) = state.machines.get_mut(machine)
        && known.is_connected()
        && known.sessions == Loadable::NotRequested
    {
        known.sessions = Loadable::Loading;
        requests.push(Outgoing {
            request: Request::SessionsList,
            purpose: Purpose::Sessions,
            to: machine,
        });
    }
    let connected = |state: &AppState, machine| {
        state
            .machines
            .get(machine)
            .is_some_and(MachineState::is_connected)
    };
    let field_machine = state.ui.folder_field().map(|field| field.machine);
    let field_answers = field_machine.is_some_and(|machine| connected(state, machine));
    if field_answers
        && let Some(field) = state.ui.folder_field()
        && let Some(list) = field.folders
        && list.listing == Loadable::NotRequested
    {
        list.listing = Loadable::Loading;
        let folder = list.folder.clone();
        requests.push(Outgoing {
            request: Request::FoldersList(FoldersListParams {
                path: folder.clone(),
            }),
            purpose: Purpose::Folders { folder },
            to: field.machine,
        });
    }
    let open_file_agent = match &state.ui.dialog {
        Some(Dialog::OpenFile(view)) => Some(view.agent),
        _ => None,
    };
    if open_file_agent.is_some_and(|agent| state.reaches_agent(agent))
        && let Some(Dialog::OpenFile(view)) = &mut state.ui.dialog
    {
        open_file_requests(view, &mut requests);
    }
    let Some(agent) = state
        .ui
        .selected
        .filter(|agent| state.reaches_agent(*agent))
    else {
        return requests;
    };
    let files_pane_open = file_tree::pane_open(state, agent);
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
            to: MachineId::THIS,
        });
    }
    if let Some(path) = open_file {
        let text = entry.files.entry(path.clone()).or_default();
        let first = *text == Loadable::NotRequested;
        // A read again keeps the text on screen until the new one arrives.
        let again =
            matches!(text, Loadable::Loaded(_)) && entry.file_freshness.reread == Reread::Wanted;
        if first {
            *text = Loadable::Loading;
        }
        if again {
            entry.file_freshness.reread = Reread::Reading { again: false };
        }
        if first || again {
            requests.push(Outgoing {
                request: Request::AgentReadFile(ReadFileParams {
                    agent,
                    path: path.clone(),
                }),
                purpose: Purpose::ReadFile { agent, path },
                to: MachineId::THIS,
            });
        }
    }
    if files_pane_open {
        tree_requests(entry, agent, &mut requests);
    }
    chat_images::requests(entry, agent, &mut requests);
    requests
}

/// The files pane's listings and search, when not asked for yet or when a listing is stale.
fn tree_requests(entry: &mut AgentEntry, agent: AgentId, requests: &mut Vec<Outgoing>) {
    let footprint = file_tree::footprint(entry);
    let folders = file_tree::shown_folders(&entry.tree, &entry.summary.folder, &footprint);
    for folder in folders {
        let listing = entry.tree.listings.entry(folder.clone()).or_default();
        let first = listing.entries == Loadable::NotRequested;
        if listing.reading || !(first || listing.stale) {
            continue;
        }
        listing.stale = false;
        listing.reading = true;
        if first {
            listing.entries = Loadable::Loading;
        }
        requests.push(Outgoing {
            request: Request::AgentListDir(ListDirParams {
                agent,
                path: folder.clone(),
            }),
            purpose: Purpose::TreeListing { agent, folder },
            to: MachineId::THIS,
        });
    }
    if let Some(search) = &mut entry.tree.search
        && search.found == Loadable::NotRequested
    {
        search.found = Loadable::Loading;
        let name = search.name.clone();
        requests.push(Outgoing {
            request: Request::AgentFindFiles(FindFilesParams {
                agent,
                name: name.clone(),
            }),
            purpose: Purpose::TreeSearch { agent, name },
            to: MachineId::THIS,
        });
    }
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
            to: MachineId::THIS,
        });
    }
    if let Some(search) = &mut view.search
        && search.found == Loadable::NotRequested
    {
        search.found = Loadable::Loading;
        let name = search.name.clone();
        requests.push(Outgoing {
            request: Request::AgentFindFiles(FindFilesParams {
                agent,
                name: name.clone(),
            }),
            purpose: Purpose::FindFiles { agent, name },
            to: MachineId::THIS,
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
        to: MachineId::THIS,
    }
}

pub fn apply_response(
    state: &mut AppState,
    drafts: &mut Drafts,
    method: Method,
    purpose: Purpose,
    result: Result<ResponseBody, RequestError>,
    machine: MachineId,
) {
    // The machine was removed while the request was on its way.
    if state.machines.get(machine).is_none() {
        return;
    }
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
            match result {
                Ok(ResponseBody::Details { details, last_seq }) => {
                    details_updates::answered(entry, details, last_seq);
                }
                other => {
                    details_updates::failed(entry, Failure::from_result(other).into_loadable());
                    sending::details_loaded(entry);
                }
            }
            sending::waiting_changed(entry);
        }
        Purpose::ContextUsage { agent, details_seq } => {
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.context_loading = false;
            }
            match result {
                Ok(ResponseBody::ContextUsage(context)) => {
                    if let Some(details) = details_unchanged_since(state, agent, details_seq) {
                        details.context = Some(context);
                    }
                }
                other => report(state, method, other),
            }
        }
        Purpose::McpStatus { agent, details_seq } => match result {
            Ok(ResponseBody::McpServers { servers }) => {
                if let Some(details) = details_unchanged_since(state, agent, details_seq) {
                    details.mcp_servers = servers;
                }
            }
            other => report(state, method, other),
        },
        Purpose::McpReconnect { agent, server } => {
            let refusal = result.err().map(|failure| failure.message);
            mcp_reconnect::answered(state, agent, &server, refusal);
        }
        Purpose::McpReconnected {
            agent,
            server,
            details_seq,
        } => match result {
            Ok(ResponseBody::McpServers { servers }) => {
                mcp_reconnect::checked(state, agent, &server, Ok(&servers), Instant::now());
                if let Some(details) = details_unchanged_since(state, agent, details_seq) {
                    details.mcp_servers = servers;
                }
            }
            Ok(_) => {
                let unexpected = format!("{method} gave an unexpected answer.");
                mcp_reconnect::checked(state, agent, &server, Err(unexpected), Instant::now());
            }
            Err(failure) => {
                mcp_reconnect::checked(state, agent, &server, Err(failure.message), Instant::now());
            }
        },
        Purpose::Commands { agent } => {
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.commands_requested = false;
            }
            match result {
                Ok(ResponseBody::Commands { commands }) => {
                    if let Some(entry) = state.agents.get_mut(&agent) {
                        entry.commands = commands;
                    }
                }
                other => report(state, method, other),
            }
        }
        Purpose::ChatImage { agent, key } => {
            let answer = match result {
                Ok(ResponseBody::Image { image, .. }) => Ok(image),
                Ok(ResponseBody::File { .. }) => Err("This file is not an image.".to_owned()),
                Ok(ResponseBody::FileMatches { .. }) => {
                    Err("Several files have this name: give its full path.".to_owned())
                }
                Ok(_) => Err(format!("{method} gave an unexpected answer.")),
                Err(failure) => Err(failure.message),
            };
            chat_images::answered(state, agent, &key, answer);
        }
        Purpose::ReadFile { agent, path } => {
            let now = Timestamp::from_system_time(SystemTime::now());
            let read = match result {
                Ok(ResponseBody::File {
                    path: found,
                    text,
                    edits,
                }) => {
                    let found = found.unwrap_or_else(|| path.clone());
                    // A daemon that predates the edits leaves the ones of the loaded messages.
                    let edits = edits.unwrap_or_else(|| loaded_edits(state, agent, &found));
                    let read = FileRead::Text { text, edits };
                    actions::show_read_file(state, agent, &path, &found, read, now);
                    return;
                }
                Ok(ResponseBody::Image { path: found, image }) => {
                    actions::show_read_file(
                        state,
                        agent,
                        &path,
                        &found,
                        FileRead::Image(image),
                        now,
                    );
                    return;
                }
                Ok(ResponseBody::FileMatches { matches }) => {
                    Loadable::Loaded(FileRead::Choices(matches))
                }
                Err(failure) if failure.not_found => {
                    Loadable::Loaded(FileRead::Missing(failure.message))
                }
                other => {
                    let failure = Failure::from_result(other);
                    if reread_failed(state, agent, &path, &failure) {
                        return;
                    }
                    failure.into_loadable()
                }
            };
            // A file the view no longer shows is not kept.
            let Some(entry) = state.agents.get_mut(&agent) else {
                return;
            };
            if let Some(held) = entry.files.get_mut(&path) {
                *held = read;
                entry.file_freshness.reread = Reread::Idle;
            }
        }
        Purpose::Accounts => match result {
            Err(failure) if failure.transient => {
                if let Some(known) = state.machines.get_mut(machine) {
                    known.sign_ins.checking = false;
                }
            }
            other => accounts::listed(state, machine, other.map_err(|failure| failure.message)),
        },
        Purpose::AccountAdded { color } => {
            accounts::added(
                state,
                machine,
                color,
                result.map_err(|failure| failure.message),
            );
        }
        Purpose::AccountChanged => {
            accounts::changed(state, machine, result.map_err(|failure| failure.message));
        }
        Purpose::SendAgain { agent, turn_end } => {
            if let Err(failure) = result {
                accounts::send_again_failed(state, agent, turn_end, &failure.message);
            }
        }
        Purpose::Sessions => {
            if let Some(known) = state.machines.get_mut(machine) {
                known.sessions = match result {
                    Ok(ResponseBody::Sessions { sessions }) => Loadable::Loaded(sessions),
                    other => Failure::from_result(other).into_loadable(),
                };
            }
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
        // A listing replaces the old one, which showed meanwhile. A failure keeps the old one.
        Purpose::TreeListing { agent, folder } => {
            if let Some(entry) = state.agents.get_mut(&agent)
                && let Some(listing) = entry.tree.listings.get_mut(&folder)
            {
                listing.reading = false;
                match result {
                    Ok(ResponseBody::Directory(found)) => listing.entries = Loadable::Loaded(found),
                    other if !matches!(listing.entries, Loadable::Loaded(_)) => {
                        listing.entries = Failure::from_result(other).into_loadable();
                    }
                    _ => {}
                }
            }
        }
        Purpose::TreeSearch { agent, name } => {
            if let Some(entry) = state.agents.get_mut(&agent)
                && let Some(search) = &mut entry.tree.search
                && search.name == name
            {
                search.found = match result {
                    Ok(ResponseBody::FileMatches { matches }) => Loadable::Loaded(matches),
                    other => Failure::from_result(other).into_loadable(),
                };
            }
        }
        Purpose::FindFiles { agent, name } => {
            if let Some(Dialog::OpenFile(view)) = &mut state.ui.dialog
                && view.agent == agent
                && let Some(search) = &mut view.search
                && search.name == name
            {
                search.found = match result {
                    Ok(ResponseBody::FileMatches { matches }) => Loadable::Loaded(matches),
                    other => Failure::from_result(other).into_loadable(),
                };
            }
        }
        Purpose::Folders { folder } => {
            if let Err(failure) = &result
                && failure.unknown_method
            {
                if let Some(known) = state.machines.get_mut(machine) {
                    known.folders_unknown = true;
                }
                if let Some(field) = state.ui.folder_field()
                    && field.machine == machine
                {
                    *field.folders = None;
                }
                return;
            }
            // The new agent dialog's list may wait behind the "Accounts" dialog.
            let folders = if state.ui.folder_field().is_some() {
                state
                    .ui
                    .folder_field()
                    .filter(|field| field.machine == machine)
                    .map(|field| field.folders)
            } else {
                state
                    .ui
                    .new_agent_form_mut()
                    .filter(|form| form.machine == machine)
                    .map(|form| &mut form.folders)
            };
            if let Some(Some(list)) = folders
                && list.folder == folder
            {
                list.listing = match result {
                    Ok(ResponseBody::Directory(listing)) => Loadable::Loaded(listing),
                    other => Failure::from_result(other).into_loadable(),
                };
            }
        }
        Purpose::WorkingFolder { action } => {
            working_folder::answered(state, action, result.map_err(|failure| failure.message));
        }
        Purpose::Start { project } => match result {
            Ok(ResponseBody::AgentStarted { agent }) => {
                if let Some(project) = project {
                    projects::pick_for_new_agent(state, agent, project);
                }
                actions::select_agent(state, agent);
                state.ui.new_agent_started();
            }
            other => {
                if let Some(form) = state.ui.new_agent_form_mut() {
                    form.starting = false;
                }
                report(state, method, other);
            }
        },
        Purpose::Prompt {
            agent,
            draft,
            sending: sent,
        } => {
            if let Err(failure) = result {
                if let Some(entry) = state.agents.get_mut(&agent) {
                    sending::failed(entry, sent);
                }
                // A held message took seconds to leave: the composer may hold new text by now.
                sending::restore_draft(drafts, agent, draft);
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
                    let mut entry = *entry;
                    // The app dropped the agent's details events while it was closing.
                    details_updates::forget(&mut entry);
                    state.agents.entry(agent).or_insert(entry);
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

fn details_unchanged_since(
    state: &mut AppState,
    agent: AgentId,
    asked: Option<Seq>,
) -> Option<&mut AgentDetails> {
    details_updates::unchanged_since(state.agents.get_mut(&agent)?, asked)
}

/// The edits of the file at `path` in `agent`'s loaded messages.
fn loaded_edits(state: &AppState, agent: AgentId, path: &Path) -> Vec<FileEdit> {
    let Some(entry) = state.agents.get(&agent) else {
        return Vec::new();
    };
    let folder = &entry.summary.folder;
    let path = file_edits::relative_path(folder, path);
    file_edits::edits_for(&entry.transcript, folder, &path)
        .into_iter()
        .cloned()
        .collect()
}

/// A read again of the shown file failed, but for a missing file: the text read before stays, and
/// a note says why it did not update.
fn reread_failed(state: &mut AppState, agent: AgentId, path: &Path, failure: &Failure) -> bool {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return false;
    };
    let shown = matches!(entry.files.get(path), Some(Loadable::Loaded(_)));
    if !shown || !matches!(entry.file_freshness.reread, Reread::Reading { .. }) {
        return false;
    }
    entry.file_freshness.reread = Reread::Idle;
    state.ui.report(format!(
        "Cannot read {} again: {}",
        path_style::shown(path),
        failure.message
    ));
    true
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
        let unknown = Method::unknown_message(method.as_str());
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

    use cockpit_protocol::{
        AgentStatus, Catchup, Completeness, DirListing, ErrorBody, ErrorCode, HistoryPage,
        ItemBody, NoticeKind, OlderItems, TranscriptItem,
    };

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::machine::MachineId;
    use crate::prefs::CommentPlacement;
    use crate::state::app_state::ConnectionState;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::folder_field;
    use crate::state::quotes::{self, Passage, QuoteAnchor};
    use crate::state::ui_state::{Block, FileView};
    use crate::state::utf16_offset::Utf16Offset;

    fn connected_with_selection() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.this.connection = ConnectionState::Connected;
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
        state.machines.this.connection = ConnectionState::Connecting { attempt: 1 };

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
                completeness: Completeness::Complete,
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
                MachineId::THIS,
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
                to: MachineId::THIS,
            }]
        );
        assert!(second.is_empty());
        let answer = |state: &mut AppState, folder: &str| {
            let listing = DirListing {
                path: PathBuf::from(folder),
                entries: Vec::new(),
                completeness: Completeness::Complete,
            };
            apply_response(
                state,
                &mut Drafts::default(),
                Method::FoldersList,
                Purpose::Folders {
                    folder: PathBuf::from(folder),
                },
                Ok(ResponseBody::Directory(listing)),
                MachineId::THIS,
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
    fn an_answer_goes_to_the_machine_it_was_asked_for() {
        let devbox = MachineId::new(1);
        let gone = MachineId::new(2);
        let mut state = connected_with_selection();
        let mut drafts = Drafts::default();
        let mut known = MachineState::new(DaemonLabel::ssh("devbox".to_owned()));
        known.connection = ConnectionState::Connected;
        state.machines.others.insert(devbox, known);
        actions::open_new_agent(&mut state, &mut drafts);
        drafts.new_agent.folder = "/srv/".to_owned();
        folder_field::follow(&mut state, &drafts);
        needed_requests(&mut state);
        // The machine changed while this computer listed the same folder.
        actions::pick_new_agent_machine(&mut state, &mut drafts, devbox);
        folder_field::follow(&mut state, &drafts);
        let asked = needed_requests(&mut state);
        let listing = DirListing {
            path: PathBuf::from("/srv"),
            entries: Vec::new(),
            completeness: Completeness::Complete,
        };
        let answer = |state: &mut AppState, machine: MachineId, purpose: Purpose, body| {
            let method = Method::FoldersList;
            apply_response(
                state,
                &mut Drafts::default(),
                method,
                purpose,
                Ok(body),
                machine,
            );
        };
        let folders = Purpose::Folders {
            folder: PathBuf::from("/srv"),
        };

        answer(
            &mut state,
            MachineId::THIS,
            folders.clone(),
            ResponseBody::Directory(listing),
        );
        let after_this_computer = folder_field::list(&state).map(|list| list.listing.clone());
        let started = devbox.app_agent(AgentId::new(5));
        answer(
            &mut state,
            gone,
            Purpose::Start { project: None },
            ResponseBody::AgentStarted {
                agent: gone.app_agent(AgentId::new(5)),
            },
        );
        let after_a_removed_machine = state.ui.selected;
        answer(
            &mut state,
            devbox,
            Purpose::Start { project: None },
            ResponseBody::AgentStarted { agent: started },
        );

        assert!(asked.iter().any(|outgoing| outgoing.to == devbox));
        assert_eq!(after_this_computer, Some(Loadable::Loading));
        assert_eq!(after_a_removed_machine, Some(AgentId::new(1)));
        assert_eq!(state.ui.selected, Some(started));
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
                MachineId::THIS,
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
            crate::state::connected_update(Catchup::Replay),
            Instant::now(),
            MachineId::THIS,
        );
        assert_eq!(
            methods(&type_folder(&mut state, "/srv/")),
            [Method::FoldersList]
        );
    }

    #[test]
    fn a_read_again_that_finds_no_file_shows_it_missing_and_ends() {
        let mut state = connected_with_selection();
        let agent = AgentId::new(1);
        let path = PathBuf::from("notes.md");
        actions::open_file(&mut state, agent, path.clone(), None);
        let text = FileRead::unedited("one\n");
        actions::show_read_file(&mut state, agent, &path, &path, text, None);
        crate::state::file_reread::reread(&mut state, agent);
        needed_requests(&mut state);
        let gone = ErrorBody::new(ErrorCode::NotFound, "notes.md does not exist".to_owned());

        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentReadFile,
            Purpose::ReadFile {
                agent,
                path: path.clone(),
            },
            Err(RequestError::Failed(gone)),
            MachineId::THIS,
        );

        let entry = &state.agents[&agent];
        assert_eq!(
            entry.files[&path],
            Loadable::Loaded(FileRead::Missing("notes.md does not exist".to_owned()))
        );
        assert_eq!(entry.file_freshness.reread, Reread::Idle);
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
                    kind: NoticeKind::Plain,
                },
            }],
            older_items: OlderItems::None,
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
            MachineId::THIS,
        );
        apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentDetails,
            Purpose::Details { agent },
            Err(RequestError::Disconnected),
            MachineId::THIS,
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
            MachineId::THIS,
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
        let mut quoted = Drafts::default();
        quotes::start(
            &mut quoted,
            agent,
            Passage {
                anchor: QuoteAnchor {
                    item: ItemId::new(4),
                    start: Utf16Offset::new(0),
                    end: Utf16Offset::new(5),
                },
                raw: "cache".to_owned(),
                text: "cache".to_owned(),
                code: false,
            },
        );
        quotes::commit(&mut quoted, CommentPlacement::Beside);
        let draft = Draft {
            text: "Run the tests".to_owned(),
            images: Vec::new(),
            quotes: quoted
                .draft(agent)
                .map(|draft| draft.quotes.clone())
                .unwrap_or_default(),
        };
        assert_eq!(draft.quotes.len(), 1);
        let mut drafts = Drafts::default();

        apply_response(
            &mut state,
            &mut drafts,
            Method::AgentPrompt,
            Purpose::Prompt {
                agent,
                draft: draft.clone(),
                sending: SendingId::first(),
            },
            Err(RequestError::NotConnected),
            MachineId::THIS,
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
                    quotes: Vec::new(),
                },
                sending: SendingId::first(),
            },
            Err(RequestError::NotConnected),
            MachineId::THIS,
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
            Purpose::Start { project: None },
            Ok(ResponseBody::AgentStarted {
                agent: AgentId::new(9),
            }),
            MachineId::THIS,
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
            MachineId::THIS,
        );

        assert_eq!(
            state.ui.errors.first().map(|note| note.text.as_str()),
            Some("agent.commands failed: the daemon answered with an unexpected response")
        );
    }
}
