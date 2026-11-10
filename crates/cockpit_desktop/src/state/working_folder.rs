//! The working folder dialog, from an agent's row menu: forks the agent's conversation into
//! another folder, or moves the agent there. It acts on the folder `claude` runs in, and on Claude
//! Code's copy of the session there; how the sidebar sorts agents stays as it is.
//!
//! The dialog's field is the new agent dialog's folder field ([`folder_field`]): it lists the
//! folders of the agent's machine as you type.

use std::path::PathBuf;

use cockpit_protocol::{AgentId, AgentStatus, FolderParams, Request, ResponseBody};

use super::app_state::AppState;
use super::drafts::Drafts;
use super::requests::{Outgoing, Purpose};
use super::ui_state::{Dialog, FolderAction, Popover, WorkingFolderForm};
use super::{actions, folder_field};
use crate::machine::MachineId;

/// Why the action cannot run now, for its menu item's tooltip; `None` when it can.
pub fn blocked(state: &AppState, agent: AgentId, action: FolderAction) -> Option<&'static str> {
    let summary = &state.agents.get(&agent)?.summary;
    if summary.session.is_none() {
        return Some("The agent has no Claude session yet");
    }
    let working = summary.status == AgentStatus::Working;
    (action == FolderAction::Move && working)
        .then_some("The agent is working: wait for the turn to end, or stop it")
}

/// Opens the dialog for the agent, its field on the folder that holds the agent's folder, where
/// a sibling such as another worktree is one pick away.
pub fn open(state: &mut AppState, drafts: &mut Drafts, agent: AgentId, action: FolderAction) {
    if matches!(state.ui.popover, Some(Popover::RowMenu { .. })) {
        state.ui.popover = None;
    }
    let Some(entry) = state.agents.get(&agent) else {
        return;
    };
    if blocked(state, agent, action).is_some() {
        return;
    }
    let folder = entry.summary.folder.to_string_lossy().into_owned();
    state.ui.dialog = Some(Dialog::WorkingFolder(WorkingFolderForm {
        agent,
        action,
        machine: MachineId::of(agent),
        folders: None,
        sending: false,
        error: None,
    }));
    let style = folder_field::style(state);
    let above = style.parent(&folder).map_or(folder.clone(), str::to_owned);
    folder_field::set_text(state, drafts, style.folder_text(&above));
}

/// Sends the fork or the move for the folder the field names. A path that is not absolute stays
/// in the field, and the dialog says why: the daemon would refuse it.
pub fn submit(state: &mut AppState, drafts: &Drafts) -> Vec<Outgoing> {
    let style = folder_field::style(state);
    let Some(Dialog::WorkingFolder(form)) = &mut state.ui.dialog else {
        return Vec::new();
    };
    if form.sending {
        return Vec::new();
    }
    let text = drafts.new_agent.folder.trim();
    if !style.is_absolute(text) {
        form.error = Some("Type the folder's whole path, from its root.".to_owned());
        return Vec::new();
    }
    form.error = None;
    let folder = PathBuf::from(style.resolve(text, ""));
    let params = FolderParams {
        agent: form.agent,
        folder,
    };
    let (request, action) = match form.action {
        FolderAction::Fork => (Request::AgentFork(params), FolderAction::Fork),
        FolderAction::Move => (Request::AgentChangeFolder(params), FolderAction::Move),
    };
    form.sending = true;
    vec![Outgoing {
        request,
        purpose: Purpose::WorkingFolder { action },
        to: form.machine,
    }]
}

/// The daemon answered: the dialog closes, and a fork opens. A refusal keeps the dialog open,
/// with the daemon's reason in it, or in a note once the dialog closed.
pub fn answered(state: &mut AppState, action: FolderAction, result: Result<ResponseBody, String>) {
    let failed = match (action, result) {
        (FolderAction::Fork, Ok(ResponseBody::AgentStarted { agent })) => {
            close(state);
            actions::select_agent(state, agent);
            return;
        }
        (FolderAction::Move, Ok(ResponseBody::Done)) => {
            close(state);
            return;
        }
        (_, Ok(other)) => format!("unexpected answer {other:?}"),
        (_, Err(message)) => message,
    };
    let what = match action {
        FolderAction::Fork => "The agent was not forked",
        FolderAction::Move => "The agent was not moved",
    };
    let message = format!("{what}: {failed}");
    match &mut state.ui.dialog {
        Some(Dialog::WorkingFolder(form)) => {
            form.sending = false;
            form.error = Some(message);
        }
        _ => state.ui.report(message),
    }
}

fn close(state: &mut AppState) {
    if matches!(state.ui.dialog, Some(Dialog::WorkingFolder(_))) {
        state.ui.dialog = None;
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::ClaudeSessionId;

    use super::*;
    use crate::state::ConnectionState;
    use crate::state::app_state::tests::{state_with, summary};

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    /// An idle agent in `/work/shop`, with a session.
    fn state() -> AppState {
        let mut first = summary(1, AgentStatus::Idle);
        first.folder = PathBuf::from("/work/shop");
        first.session = Some(ClaudeSessionId::new("session-1".to_owned()));
        let mut state = state_with(vec![first]);
        state.machines.this.connection = ConnectionState::Connected;
        state
    }

    #[test]
    fn the_dialog_opens_on_the_folder_above_the_agents() {
        let mut state = state();
        let mut drafts = Drafts::default();

        open(&mut state, &mut drafts, agent(), FolderAction::Move);

        assert_eq!(drafts.new_agent.folder, "/work/");
        let Some(Dialog::WorkingFolder(form)) = &state.ui.dialog else {
            panic!("the dialog did not open");
        };
        assert_eq!(form.action, FolderAction::Move);
        assert_eq!(
            form.folders.as_ref().map(|list| list.folder.clone()),
            Some(PathBuf::from("/work/"))
        );
    }

    #[test]
    fn a_working_agent_forks_but_does_not_move_and_no_session_does_neither() {
        let mut state = state();
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.summary.status = AgentStatus::Working;
        }
        assert_eq!(blocked(&state, agent(), FolderAction::Fork), None);
        assert!(blocked(&state, agent(), FolderAction::Move).is_some());

        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.summary.session = None;
        }
        assert!(blocked(&state, agent(), FolderAction::Fork).is_some());
    }

    #[test]
    fn a_submit_sends_the_folder_once_and_a_relative_path_stays() {
        let mut state = state();
        let mut drafts = Drafts::default();
        open(&mut state, &mut drafts, agent(), FolderAction::Fork);

        drafts.new_agent.folder = "shop_fix".to_owned();
        assert!(submit(&mut state, &drafts).is_empty());
        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::WorkingFolder(form)) if form.error.is_some()
        ));

        drafts.new_agent.folder = "/work/shop_fix/".to_owned();
        let sent = submit(&mut state, &drafts);
        let again = submit(&mut state, &drafts);

        assert!(matches!(
            sent.as_slice(),
            [Outgoing {
                request: Request::AgentFork(FolderParams { folder, .. }),
                purpose: Purpose::WorkingFolder {
                    action: FolderAction::Fork
                },
                ..
            }] if folder == &PathBuf::from("/work/shop_fix")
        ));
        assert!(again.is_empty(), "one request at a time");
    }

    #[test]
    fn a_fork_opens_and_a_refusal_keeps_the_dialog() {
        let mut state = state();
        let mut drafts = Drafts::default();
        open(&mut state, &mut drafts, agent(), FolderAction::Move);
        let _ = submit(&mut state, &drafts);

        answered(
            &mut state,
            FolderAction::Move,
            Err("no such folder".to_owned()),
        );
        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::WorkingFolder(form))
                if !form.sending && form.error.as_deref() == Some("The agent was not moved: no such folder")
        ));
        assert!(state.ui.errors.is_empty(), "the dialog says it, not a note");

        answered(
            &mut state,
            FolderAction::Fork,
            Ok(ResponseBody::AgentStarted { agent: agent() }),
        );
        assert_eq!(state.ui.dialog, None);
        assert_eq!(state.ui.selected, Some(agent()));
    }
}
