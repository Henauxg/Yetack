use crate::ids::ControlRequestId;
use crate::tool_input::ToolName;

/// A line or a body that cannot be read.
///
/// The [crate documentation](crate) says which lines give one. The message carries serde's
/// detail, so logging it is enough.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("the line is not JSON: {0}")]
    NotJson(serde_json::Error),
    /// `kind` is the line's `type`, or its `subtype` when it has one.
    #[error("malformed `{kind}` line: {error}")]
    MalformedLine {
        kind: &'static str,
        /// Set for a `control_request` whose request does not read. Claude Code still waits for
        /// an answer, so answer it with [`InputLine::control_error`](crate::InputLine::control_error).
        request_id: Option<ControlRequestId>,
        error: serde_json::Error,
    },
    #[error("malformed {body} response: {error}")]
    MalformedResponse {
        body: &'static str,
        error: serde_json::Error,
    },
    #[error("malformed `{tool}` input: {error}")]
    MalformedToolInput {
        tool: ToolName,
        error: serde_json::Error,
    },
}
