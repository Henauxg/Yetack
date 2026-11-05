use crate::tool_input::ToolName;

/// A line or a body that cannot be read.
///
/// Only a line that is not JSON, or a known kind whose required field is missing or has the
/// wrong type, gives an error. The message carries serde's detail, so logging it is enough.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("the line is not JSON: {0}")]
    NotJson(serde_json::Error),
    /// `kind` is the line's `type`, or its `subtype` when it has one.
    #[error("malformed `{kind}` line: {error}")]
    MalformedLine {
        kind: &'static str,
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
