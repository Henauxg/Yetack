//! The conversion between Claude Code's lines and the cockpit's model: the only module that knows
//! both formats.
//!
//! - [`AgentView`] turns the lines `claude` writes into transcript items, details and a summary.
//! - [`PastTranscript`] turns the lines of a past session's file into transcript items, the same
//!   way.
//! - [`answers`] turns a client's answer into the `control_response` Claude Code waits for, and
//!   [`prompts`] a client's prompt into a `user` line.
//! - [`plan_usage`] keeps an account's plan usage from its agents' `rate_limit_event` lines.

mod agent_view;
mod answers;
mod details;
mod notices;
mod past_transcript;
pub(crate) mod plan_usage;
pub(crate) mod prompts;
mod subagents;
mod tools;
pub(crate) mod words;

pub(crate) use agent_view::{AgentView, AnswerError, FollowUp, ProcessSession};
pub(crate) use past_transcript::PastTranscript;
pub(crate) use tools::{finished_edits, touched_files};
