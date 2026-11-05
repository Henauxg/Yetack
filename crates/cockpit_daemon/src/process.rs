//! One `claude` process per running agent: its command line, its pipes, the daemon's control
//! requests and their answers, and its exit.

mod command;
mod error;
mod line_reader;
mod task;

pub(crate) use command::ClaudeCommand;
pub use error::ProcessError;
pub(crate) use task::{ControlFailure, ProcessEvent, ProcessHandle, spawn};
