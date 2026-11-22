//! The Resume of an exited agent, from the bar above the composer. One request goes at a time,
//! and a refusal stays for the bar to show. When the agent's folder is gone, the bar offers to
//! move the agent to another folder instead.

use std::path::PathBuf;

use cockpit_protocol::{AccountId, AgentId, AgentParams, AgentStatus, ClaudeSessionId, Request};

use super::actions;
use super::app_state::{self, AgentEntry, AppState};
use super::requests::{Outgoing, Purpose};
use crate::machine::MachineId;

/// Where the agent's Resume stands, while the agent has exited.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Resume {
    #[default]
    NotAsked,
    /// On its way, or done and waiting for the update that shows the agent running.
    Sending,
    Refused(ResumeRefusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeRefusal {
    /// The daemon's reason.
    pub reason: String,
    /// The agent's folder when it was refused: a move to another folder makes it stale.
    pub folder: PathBuf,
    pub folder_state: FolderState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderState {
    /// The folder is there, or the refusal does not say.
    Unknown,
    /// The daemon found no folder there, as after a rename.
    Gone,
}

/// Sends `agent.resume`. A click while one is on its way sends nothing.
pub fn send(state: &mut AppState, agent: AgentId) -> Vec<Outgoing> {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return Vec::new();
    };
    if entry.summary.status != AgentStatus::Exited || entry.resume == Resume::Sending {
        return Vec::new();
    }
    entry.resume = Resume::Sending;
    vec![Outgoing {
        request: Request::AgentResume(AgentParams { agent }),
        purpose: Purpose::Resume {
            agent,
            folder: entry.summary.folder.clone(),
        },
        to: MachineId::THIS,
    }]
}

/// The daemon refused the Resume of the agent in `folder`.
pub fn refused(state: &mut AppState, agent: AgentId, folder: PathBuf, refusal: Refusal) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    if entry.summary.status != AgentStatus::Exited {
        return;
    }
    entry.resume = Resume::Refused(ResumeRefusal {
        reason: refusal.reason,
        folder,
        folder_state: refusal.folder_state,
    });
}

/// The Resume did not reach the daemon, or its answer was lost: the agent can resume again.
pub fn not_sent(state: &mut AppState, agent: AgentId) {
    if let Some(entry) = state.agents.get_mut(&agent)
        && entry.resume == Resume::Sending
    {
        entry.resume = Resume::NotAsked;
    }
}

/// What the daemon said when it refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
    pub folder_state: FolderState,
}

/// `sessions.resume` was refused with `NotFound`. When an exited agent of `machine` holds the
/// session, the daemon could not start it in its folder: that agent opens, with the refusal in its
/// bar, and the new agent dialog closes. False when no such agent holds the session.
pub fn session_refused(
    state: &mut AppState,
    machine: MachineId,
    session: &ClaudeSessionId,
    account: AccountId,
    reason: &str,
) -> bool {
    let holder = state.agents.values().find(|entry| {
        MachineId::of(entry.id()) == machine
            && entry.summary.status == AgentStatus::Exited
            && entry.summary.account == account
            && app_state::own_session(&entry.summary) == Some(session)
    });
    let Some((agent, folder)) = holder.map(|entry| (entry.id(), entry.summary.folder.clone()))
    else {
        return false;
    };
    let refusal = Refusal {
        reason: reason.to_owned(),
        folder_state: FolderState::Gone,
    };
    refused(state, agent, folder, refusal);
    actions::select_agent(state, agent);
    state.ui.new_agent_started();
    true
}

/// The agent's summary changed: once the agent runs, its Resume is over.
pub fn summary_changed(entry: &mut AgentEntry) {
    if entry.summary.status != AgentStatus::Exited {
        entry.resume = Resume::NotAsked;
    }
}

/// The refusal the bar shows: none once the agent moved to another folder.
pub fn refusal(entry: &AgentEntry) -> Option<&ResumeRefusal> {
    match &entry.resume {
        Resume::Refused(refusal) if refusal.folder == entry.summary.folder => Some(refusal),
        Resume::NotAsked | Resume::Sending | Resume::Refused(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{ClaudeSessionId, SessionUse};

    use super::*;
    use crate::state::app_state::tests::{state_with, summary};

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    /// An exited agent in `/work/shop`, with a session.
    fn state() -> AppState {
        let mut exited = summary(1, AgentStatus::Exited);
        exited.folder = PathBuf::from("/work/shop");
        exited.session = Some(ClaudeSessionId::new("session-1".to_owned()));
        state_with(vec![exited])
    }

    fn gone() -> Refusal {
        Refusal {
            reason: "the folder /work/shop no longer exists".to_owned(),
            folder_state: FolderState::Gone,
        }
    }

    #[test]
    fn a_resume_goes_once_until_the_daemon_answers() {
        let mut state = state();

        let first = send(&mut state, agent());
        let second = send(&mut state, agent());

        assert!(matches!(
            first.as_slice(),
            [Outgoing {
                request: Request::AgentResume(AgentParams { .. }),
                purpose: Purpose::Resume { .. },
                ..
            }]
        ));
        assert!(second.is_empty(), "a second click sends nothing");
        assert_eq!(state.agents[&agent()].resume, Resume::Sending);
    }

    #[test]
    fn a_running_agent_sends_no_resume() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);

        assert!(send(&mut state, agent()).is_empty());
    }

    #[test]
    fn a_refusal_shows_until_the_agent_moves_or_runs() {
        let mut state = state();
        let _ = send(&mut state, agent());

        refused(&mut state, agent(), PathBuf::from("/work/shop"), gone());
        let shown = refusal(&state.agents[&agent()]).map(|refusal| refusal.folder_state);
        let again = send(&mut state, agent());
        refused(&mut state, agent(), PathBuf::from("/work/shop"), gone());
        let entry = state.agents.get_mut(&agent()).unwrap();
        entry.summary.folder = PathBuf::from("/work/shop-renamed");
        let after_move = refusal(entry).is_some();
        entry.summary.status = AgentStatus::Idle;
        summary_changed(entry);

        assert_eq!(shown, Some(FolderState::Gone));
        assert_eq!(again.len(), 1, "Try again sends the resume again");
        assert!(!after_move, "a move to another folder clears the refusal");
        assert_eq!(entry.resume, Resume::NotAsked);
    }

    #[test]
    fn a_refusal_that_comes_after_the_agent_ran_is_dropped() {
        let mut state = state();
        let _ = send(&mut state, agent());
        let entry = state.agents.get_mut(&agent()).unwrap();
        entry.summary.status = AgentStatus::Idle;
        summary_changed(entry);

        refused(&mut state, agent(), PathBuf::from("/work/shop"), gone());

        assert_eq!(state.agents[&agent()].resume, Resume::NotAsked);
    }

    #[test]
    fn a_refused_session_does_not_open_a_fork_that_copies_it() {
        let mut state = state();
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.summary.session_use = SessionUse::Fork;
        }
        let session = ClaudeSessionId::new("session-1".to_owned());

        let opened = session_refused(
            &mut state,
            MachineId::THIS,
            &session,
            AccountId::DEFAULT,
            "the folder /work/project-1 no longer exists",
        );

        assert!(!opened, "the session is the original's");
        assert_eq!(state.agents[&agent()].resume, Resume::NotAsked);
    }
}
