//! One `claude` process per running agent: its command line, its pipes, the daemon's control
//! requests and their answers, and its exit.

mod claude_command;
mod line_reader;
mod process_error;
pub(crate) mod program;
mod task;

pub(crate) use claude_command::{ClaudeCommand, InterruptedTurn};
pub use process_error::ProcessError;
pub(crate) use task::{ControlFailure, ProcessEvent, ProcessHandle, spawn};
