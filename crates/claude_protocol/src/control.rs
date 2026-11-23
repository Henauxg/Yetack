use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Map, Value};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::ids::{ControlRequestId, ToolUseId};
use crate::parse_error::ParseError;
use crate::permission::PermissionUpdate;
use crate::wire;

/// A `control_request` line: Claude Code asks something and waits for a `control_response`.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingControlRequest {
    pub request_id: ControlRequestId,
    pub request: IncomingRequest,
}

#[derive(Debug, Clone, PartialEq)]
pub enum IncomingRequest {
    CanUseTool(CanUseTool),
    /// A subtype this crate does not know, as Claude Code wrote it. Claude Code still waits for an
    /// answer, so answer it with an error.
    Other(Value),
}

/// Claude Code asks whether a tool call may run. `AskUserQuestion` and `ExitPlanMode` come
/// this way too.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CanUseTool {
    pub tool_name: String,
    /// A short name for the action, such as "Read file".
    pub display_name: Option<String>,
    #[serde(default)]
    pub input: Map<String, Value>,
    pub tool_use_id: ToolUseId,
    /// The whole prompt sentence, such as "Claude wants to read foo.txt".
    pub title: Option<String>,
    /// A subtitle for the prompt.
    pub description: Option<String>,
    /// Why Claude Code asks, such as the reason a hook gave.
    pub decision_reason: Option<String>,
    /// The path outside the allowed folders that made Claude Code ask.
    pub blocked_path: Option<PathBuf>,
    /// Updates that would let such calls run without asking.
    #[serde(default, deserialize_with = "wire::null_as_default")]
    pub permission_suggestions: Vec<PermissionUpdate>,
}

/// A `control_response` line: Claude Code answers one of our control requests.
///
/// Claude Code also echoes the answers we send to its own requests, with their ids.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlResponse {
    Success(ControlSuccess),
    Error(ControlError),
    /// A subtype this crate does not know: the `response` object as Claude Code wrote it.
    Unknown(Value),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ControlSuccess {
    pub request_id: ControlRequestId,
    /// The body, raw. Read the ones this crate types with `TryFrom`: [`InitializeResponse`],
    /// [`ContextUsage`], [`McpStatus`] and [`McpSignInStart`].
    ///
    /// [`InitializeResponse`]: crate::InitializeResponse
    /// [`ContextUsage`]: crate::ContextUsage
    /// [`McpStatus`]: crate::McpStatus
    /// [`McpSignInStart`]: crate::McpSignInStart
    #[serde(default)]
    pub response: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ControlError {
    pub request_id: ControlRequestId,
    #[serde(default)]
    pub error: String,
}

/// A `control_cancel_request` line: Claude Code no longer needs the answer to one of its
/// requests, such as an approval after an interrupt.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ControlCancelRequest {
    pub request_id: ControlRequestId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
enum IncomingRequestSubtype {
    CanUseTool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
enum ResponseSubtype {
    Success,
    Error,
}

/// The fields around the body of a control line.
#[derive(Deserialize)]
struct RequestEnvelope {
    request_id: ControlRequestId,
    request: Value,
}

#[derive(Deserialize)]
struct ResponseEnvelope {
    response: Value,
}

impl IncomingControlRequest {
    pub(crate) fn from_value(value: &Value, kind: &'static str) -> Result<Self, ParseError> {
        let RequestEnvelope {
            request_id,
            request,
        } = wire::read(value, kind)?;
        let Some(subtype) = wire::word::<IncomingRequestSubtype>(&request, wire::SUBTYPE) else {
            return Ok(Self {
                request_id,
                request: IncomingRequest::Other(request),
            });
        };
        let request = match subtype {
            IncomingRequestSubtype::CanUseTool => {
                CanUseTool::deserialize(&request).map(IncomingRequest::CanUseTool)
            }
        };
        match request {
            Ok(request) => Ok(Self {
                request_id,
                request,
            }),
            Err(error) => Err(ParseError::MalformedLine {
                kind: subtype.into(),
                request_id: Some(request_id),
                error,
            }),
        }
    }
}

impl ControlResponse {
    pub(crate) fn from_value(value: &Value, kind: &'static str) -> Result<Self, ParseError> {
        let ResponseEnvelope { response } = wire::read(value, kind)?;
        let Some(subtype) = wire::word::<ResponseSubtype>(&response, wire::SUBTYPE) else {
            return Ok(Self::Unknown(response));
        };
        match subtype {
            ResponseSubtype::Success => wire::read(&response, kind).map(Self::Success),
            ResponseSubtype::Error => wire::read(&response, kind).map(Self::Error),
        }
    }

    /// The id of the request this answers, when the subtype is known.
    #[must_use]
    pub fn request_id(&self) -> Option<&ControlRequestId> {
        match self {
            Self::Success(success) => Some(&success.request_id),
            Self::Error(error) => Some(&error.request_id),
            Self::Unknown(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtypes_are_pinned() {
        let requests: Vec<&str> = IncomingRequestSubtype::VARIANTS
            .iter()
            .map(|&word| word.into())
            .collect();
        assert_eq!(requests, ["can_use_tool"]);
        let responses: Vec<&str> = ResponseSubtype::VARIANTS
            .iter()
            .map(|&word| word.into())
            .collect();
        assert_eq!(responses, ["success", "error"]);
    }
}
