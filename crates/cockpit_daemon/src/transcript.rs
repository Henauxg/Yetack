//! The conversion between Claude Code's lines and the cockpit's model: the only module that knows
//! both formats.
//!
//! - [`AgentView`] turns the lines `claude` writes into transcript items, details and a summary.
//! - [`answers`] turns a client's answer into the `control_response` Claude Code waits for.
//! - [`plan_usage`] keeps the account's plan usage from `rate_limit_event` lines.

pub(crate) mod answers;
mod details;
pub(crate) mod plan_usage;
mod tools;
mod view;
pub(crate) mod words;

pub(crate) use view::{AgentView, AnswerError, FollowUp};
