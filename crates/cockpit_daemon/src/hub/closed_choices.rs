//! The model, effort and permission mode the agents closed during this run were given. A client's
//! undo of a close resumes the session as a new agent, which gets them back.

use std::collections::VecDeque;

use cockpit_protocol::{AccountId, ClaudeSessionId, Effort, PermissionMode};

use crate::store::AgentRecord;

/// The most closed agents whose choices the daemon keeps. The oldest goes first.
const MAX_KEPT: usize = 32;

/// What a client chose for an agent. `None` keeps Claude Code's default, as in [`AgentRecord`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Choices {
    pub(super) model: Option<String>,
    pub(super) effort: Option<Effort>,
    pub(super) mode: Option<PermissionMode>,
}

/// The choices of the agents closed last, by their session and account.
#[derive(Debug, Default)]
pub(crate) struct ClosedChoices {
    kept: VecDeque<(ClaudeSessionId, AccountId, Choices)>,
}

impl ClosedChoices {
    /// Keeps the choices of an agent that closes. An agent with no session cannot come back.
    pub(super) fn keep(&mut self, record: &AgentRecord) {
        let Some(session) = &record.session else {
            return;
        };
        self.kept
            .retain(|(kept, account, _)| (kept, *account) != (session, record.account));
        if self.kept.len() == MAX_KEPT {
            self.kept.pop_front();
        }
        self.kept.push_back((
            session.clone(),
            record.account,
            Choices {
                model: record.model.clone(),
                effort: record.effort,
                mode: record.mode,
            },
        ));
    }

    /// The choices of the closed agent that had the session, which a new agent takes. None for a
    /// session no agent closed with during this run.
    pub(super) fn take(&mut self, session: &ClaudeSessionId, account: AccountId) -> Choices {
        let found = self
            .kept
            .iter()
            .position(|(kept, kept_account, _)| kept == session && *kept_account == account);
        found
            .and_then(|index| self.kept.remove(index))
            .map(|(_, _, choices)| choices)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::{AgentId, Mark, Position, Timestamp};

    use super::*;
    use crate::store::ResumeAs;

    fn record(session: &str) -> AgentRecord {
        AgentRecord {
            id: AgentId::new(1),
            title: "Fix the cart".to_owned(),
            folder: PathBuf::from("/srv/projects/shop"),
            session: Some(ClaudeSessionId::new(session.to_owned())),
            model: Some("opus[1m]".to_owned()),
            effort: Some(Effort::High),
            mode: Some(PermissionMode::Auto),
            running: true,
            started_at: Timestamp::from_unix_millis(0),
            past: None,
            position: Position::default(),
            process: None,
            account: AccountId::DEFAULT,
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Same,
        }
    }

    #[test]
    fn a_closed_agents_choices_come_back_once_for_its_session() {
        let mut closed = ClosedChoices::default();
        closed.keep(&record("session-1"));
        let session = ClaudeSessionId::new("session-1".to_owned());

        let back = closed.take(&session, AccountId::DEFAULT);
        let again = closed.take(&session, AccountId::DEFAULT);

        assert_eq!(
            back,
            Choices {
                model: Some("opus[1m]".to_owned()),
                effort: Some(Effort::High),
                mode: Some(PermissionMode::Auto),
            }
        );
        assert_eq!(again, Choices::default());
    }

    #[test]
    fn the_oldest_choices_go_past_the_limit() {
        let mut closed = ClosedChoices::default();
        for index in 0..=MAX_KEPT {
            closed.keep(&record(&format!("session-{index}")));
        }

        let oldest = closed.take(
            &ClaudeSessionId::new("session-0".to_owned()),
            AccountId::DEFAULT,
        );
        let newest = closed.take(
            &ClaudeSessionId::new(format!("session-{MAX_KEPT}")),
            AccountId::DEFAULT,
        );

        assert_eq!(oldest, Choices::default());
        assert_eq!(newest.mode, Some(PermissionMode::Auto));
    }
}
