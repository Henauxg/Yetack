//! The model, effort and permission mode of the agents closed a moment ago. A client's undo of a
//! close resumes the session as a new agent, which gets them back.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use cockpit_protocol::{AccountId, ClaudeSessionId, Effort, PermissionMode};

use crate::store::{AgentRecord, ResumeAs};

/// How long after its close an agent's choices wait for a resume. The app's undo lasts 8 s. A
/// later resume, from the past sessions, starts with Claude Code's defaults as before.
pub(super) const KEPT_FOR: Duration = Duration::from_secs(30);

/// The most closed agents whose choices the daemon keeps. The oldest goes first.
const MAX_KEPT: usize = 32;

/// What a client chose for an agent. `None` keeps Claude Code's default, as in [`AgentRecord`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Choices {
    pub(super) model: Option<String>,
    pub(super) effort: Option<Effort>,
    pub(super) mode: Option<PermissionMode>,
}

#[derive(Debug)]
struct Kept {
    session: ClaudeSessionId,
    account: AccountId,
    choices: Choices,
    closed_at: Instant,
}

/// The choices of the agents closed last, by their session and account.
#[derive(Debug, Default)]
pub(crate) struct ClosedChoices {
    kept: VecDeque<Kept>,
}

impl ClosedChoices {
    /// Keeps the choices of an agent that closes at `now`. An agent with no session cannot come
    /// back.
    pub(super) fn keep(&mut self, record: &AgentRecord, now: Instant) {
        let Some(session) = &record.session else {
            return;
        };
        // A fork that has not started its own session names its original's, whose choices these
        // are not.
        if record.resume_as == ResumeAs::Fork {
            return;
        }
        self.kept.retain(|kept| {
            (&kept.session, kept.account) != (session, record.account)
                && now.duration_since(kept.closed_at) <= KEPT_FOR
        });
        if self.kept.len() == MAX_KEPT {
            self.kept.pop_front();
        }
        self.kept.push_back(Kept {
            session: session.clone(),
            account: record.account,
            choices: Choices {
                model: record.model.clone(),
                effort: record.effort,
                mode: record.mode,
            },
            closed_at: now,
        });
    }

    /// The choices of the agent closed with the session less than [`KEPT_FOR`] before `now`.
    /// None otherwise.
    pub(super) fn find(
        &self,
        session: &ClaudeSessionId,
        account: AccountId,
        now: Instant,
    ) -> Choices {
        self.kept
            .iter()
            .find(|kept| {
                &kept.session == session
                    && kept.account == account
                    && now.duration_since(kept.closed_at) <= KEPT_FOR
            })
            .map(|kept| kept.choices.clone())
            .unwrap_or_default()
    }

    /// Drops the session's choices, once a new agent took them.
    pub(super) fn forget(&mut self, session: &ClaudeSessionId, account: AccountId) {
        self.kept
            .retain(|kept| (&kept.session, kept.account) != (session, account));
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::{AgentId, Mark, Position, Timestamp};

    use super::*;

    fn record(session: &str, account: AccountId, mode: PermissionMode) -> AgentRecord {
        AgentRecord {
            id: AgentId::new(1),
            title: "Fix the cart".to_owned(),
            folder: PathBuf::from("/srv/projects/shop"),
            session: Some(ClaudeSessionId::new(session.to_owned())),
            model: Some("opus[1m]".to_owned()),
            effort: Some(Effort::High),
            mode: Some(mode),
            running: true,
            started_at: Timestamp::from_unix_millis(0),
            past: None,
            position: Position::default(),
            process: None,
            account,
            mark: Mark::Unmarked,
            resume_as: ResumeAs::Same,
        }
    }

    fn session(name: &str) -> ClaudeSessionId {
        ClaudeSessionId::new(name.to_owned())
    }

    #[test]
    fn a_closed_agents_choices_come_back_until_forgotten() {
        let mut closed = ClosedChoices::default();
        let now = Instant::now();
        closed.keep(
            &record("session-1", AccountId::DEFAULT, PermissionMode::Auto),
            now,
        );

        let found = closed.find(&session("session-1"), AccountId::DEFAULT, now);
        closed.forget(&session("session-1"), AccountId::DEFAULT);
        let again = closed.find(&session("session-1"), AccountId::DEFAULT, now);

        assert_eq!(
            found,
            Choices {
                model: Some("opus[1m]".to_owned()),
                effort: Some(Effort::High),
                mode: Some(PermissionMode::Auto),
            }
        );
        assert_eq!(again, Choices::default());
    }

    #[test]
    fn the_choices_wait_for_a_resume_a_short_time_only() {
        let mut closed = ClosedChoices::default();
        let now = Instant::now();
        closed.keep(
            &record("session-1", AccountId::DEFAULT, PermissionMode::Auto),
            now,
        );
        let soon = now.checked_add(KEPT_FOR).unwrap_or(now);
        let late = soon.checked_add(Duration::from_secs(1)).unwrap_or(soon);

        let in_time = closed.find(&session("session-1"), AccountId::DEFAULT, soon);
        let too_late = closed.find(&session("session-1"), AccountId::DEFAULT, late);

        assert_eq!(in_time.mode, Some(PermissionMode::Auto));
        assert_eq!(too_late, Choices::default());
    }

    #[test]
    fn each_account_and_the_latest_close_of_a_session_count() {
        let mut closed = ClosedChoices::default();
        let now = Instant::now();
        let other = AccountId::new(2);
        closed.keep(
            &record("session-1", AccountId::DEFAULT, PermissionMode::Auto),
            now,
        );
        closed.keep(&record("session-1", other, PermissionMode::Plan), now);
        closed.keep(
            &record("session-1", AccountId::DEFAULT, PermissionMode::AcceptEdits),
            now,
        );

        let default_account = closed.find(&session("session-1"), AccountId::DEFAULT, now);
        let other_account = closed.find(&session("session-1"), other, now);

        assert_eq!(default_account.mode, Some(PermissionMode::AcceptEdits));
        assert_eq!(other_account.mode, Some(PermissionMode::Plan));
    }

    #[test]
    fn a_fork_without_its_own_session_keeps_no_choices() {
        let mut closed = ClosedChoices::default();
        let now = Instant::now();
        let mut fork = record("session-1", AccountId::DEFAULT, PermissionMode::Plan);
        fork.resume_as = ResumeAs::Fork;

        closed.keep(&fork, now);

        assert_eq!(
            closed.find(&session("session-1"), AccountId::DEFAULT, now),
            Choices::default(),
            "the session is its original's"
        );
    }

    #[test]
    fn the_oldest_choices_go_past_the_limit() {
        let mut closed = ClosedChoices::default();
        let now = Instant::now();
        for index in 0..=MAX_KEPT {
            let name = format!("session-{index}");
            closed.keep(
                &record(&name, AccountId::DEFAULT, PermissionMode::Auto),
                now,
            );
        }

        let oldest = closed.find(&session("session-0"), AccountId::DEFAULT, now);
        let newest = closed.find(
            &session(&format!("session-{MAX_KEPT}")),
            AccountId::DEFAULT,
            now,
        );

        assert_eq!(oldest, Choices::default());
        assert_eq!(newest.mode, Some(PermissionMode::Auto));
    }
}
