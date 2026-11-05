//! Claude Code's JSON mode: the lines `claude` reads and writes, and its control protocol.
//!
//! Types and parsing only: the caller owns the process and its pipes.
//!
//! - [`OutputLine`] is a line `claude` writes on stdout. Read it with [`str::parse`].
//! - [`InputLine`] is a line for its stdin. [`InputLine::to_line`] gives the text to write.
//!
//! ```
//! use claude_protocol::{IncomingRequest, InputLine, OutputLine, PermissionDecision};
//!
//! let line = r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"t1","input":{"command":"ls"}}}"#;
//! let OutputLine::ControlRequest(request) = line.parse()? else { return Ok(()) };
//! if let IncomingRequest::CanUseTool(can_use_tool) = request.request {
//!     let decision = PermissionDecision::allow(can_use_tool.input);
//!     let answer = InputLine::permission(request.request_id, decision);
//!     let text = answer.to_line()?;
//!     assert_eq!(
//!         text,
//!         format!("{}\n", r#"{"type":"control_response","response":{"subtype":"success","request_id":"r1","response":{"behavior":"allow","updatedInput":{"command":"ls"}}}}"#),
//!     );
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Claude Code changes its output often, so reading is lenient:
//!
//! - unknown fields are ignored;
//! - an unknown `type` or `subtype` gives a variant holding the raw JSON;
//! - a word from a set Claude Code may extend, such as a status, gives an [`OpenWord`].
//!
//! Only a line that is not JSON, or a known kind whose required field is missing or has the
//! wrong type, gives a [`ParseError`]. A field is required only when the line is useless
//! without it, so most fields are optional.

mod content_block;
mod context_usage;
mod control;
mod ids;
mod initialize_response;
mod input_line;
mod mcp_status;
mod message;
mod open_word;
mod output_line;
mod parse_error;
mod permission;
mod rate_limit;
mod result_message;
mod system_event;
mod tool_input;
mod wire;

pub use content_block::{
    Base64Image, ContentBlock, ImageBlock, ImageSource, TextBlock, ThinkingBlock, ToolResultBlock,
    ToolUseBlock, UrlImage,
};
pub use context_usage::{ContextCategory, ContextUsage};
pub use control::{
    CanUseTool, ControlCancelRequest, ControlError, ControlResponse, ControlSuccess,
    IncomingControlRequest, IncomingRequest,
};
pub use ids::{ControlRequestId, HookId, MessageUuid, SessionId, TaskId, ToolUseId};
pub use initialize_response::{InitializeResponse, SlashCommand};
pub use input_line::{
    ControlAnswer, ControlRequest, InputLine, OutgoingControlRequest, OutgoingControlResponse,
    UserInput,
};
pub use mcp_status::{
    McpScope, McpServerConfig, McpServerInfo, McpServerState, McpServerStatus, McpStatus, McpTool,
    McpToolAnnotations, McpTransport,
};
pub use message::{AssistantBody, AssistantMessage, StreamEvent, Usage, UserBody, UserMessage};
pub use open_word::OpenWord;
pub use output_line::OutputLine;
pub use parse_error::ParseError;
pub use permission::{
    DirectoriesUpdate, ModeUpdate, PermissionBehavior, PermissionDecision, PermissionDestination,
    PermissionMode, PermissionRule, PermissionUpdate, RulesUpdate,
};
pub use rate_limit::{
    RateLimitEvent, RateLimitInfo, RateLimitStatus, RateLimitType, RateLimitWindow,
};
pub use result_message::{
    ModelUsage, PermissionDenial, ResultMessage, ResultSubtype, TerminalReason,
};
pub use system_event::{
    ApiRetry, HookOutcome, HookRun, InitMcpServer, SessionState, SessionStateChanged, SystemEvent,
    SystemInit, TaskNotification, TaskPatch, TaskProgress, TaskStarted, TaskStatus, TaskUpdated,
    TaskUsage, ThinkingTokens,
};
pub use tool_input::{
    AskUserQuestionInput, BashInput, EditInput, ExitPlanModeInput, Question, QuestionAnswer,
    QuestionOption, ReadInput, Todo, TodoStatus, TodoWriteInput, ToolInput, ToolName, WriteInput,
};
