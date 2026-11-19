use serde::Serialize;

use crate::content_block::ContentBlock;
use crate::ids::{ControlRequestId, TaskId};
use crate::permission::{PermissionDecision, PermissionMode};

/// A line to write on `claude`'s stdin.
#[must_use]
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputLine {
    User(UserInput),
    ControlRequest(OutgoingControlRequest),
    ControlResponse(OutgoingControlResponse),
}

/// A prompt. Build one with [`InputLine::prompt`] or [`InputLine::prompt_blocks`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UserInput {
    message: UserInputMessage,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct UserInputMessage {
    role: Role,
    content: UserContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Role {
    User,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
enum UserContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// One of our control requests. Claude Code answers it with a
/// [`ControlResponse`](crate::ControlResponse) carrying the same id.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutgoingControlRequest {
    pub request_id: ControlRequestId,
    pub request: ControlRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlRequest {
    /// The first request: the answer lists the slash commands and skills.
    Initialize,
    /// Stops the turn, as Esc does.
    Interrupt,
    SetPermissionMode {
        mode: PermissionMode,
    },
    /// `None` goes back to the default model.
    SetModel {
        model: Option<String>,
    },
    GetContextUsage,
    /// The answer says what Claude Code applies, such as the effort level.
    GetSettings,
    McpStatus,
    #[serde(rename_all = "camelCase")]
    McpReconnect {
        server_name: String,
    },
    #[serde(rename_all = "camelCase")]
    McpToggle {
        server_name: String,
        enabled: bool,
    },
    StopTask {
        task_id: TaskId,
    },
    /// Reads the skill folders again. The answer lists the skills.
    ReloadSkills,
    /// Reads the plugins again. The answer lists the commands, the plugins and the errors.
    ReloadPlugins,
}

/// Our answer to one of Claude Code's control requests.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutgoingControlResponse {
    pub response: ControlAnswer,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlAnswer {
    Success {
        request_id: ControlRequestId,
        response: PermissionDecision,
    },
    Error {
        request_id: ControlRequestId,
        error: String,
    },
}

impl InputLine {
    pub fn prompt(text: String) -> Self {
        Self::User(UserInput {
            message: UserInputMessage {
                role: Role::User,
                content: UserContent::Text(text),
            },
        })
    }

    /// A prompt made of blocks, such as images followed by text.
    pub fn prompt_blocks(blocks: Vec<ContentBlock>) -> Self {
        Self::User(UserInput {
            message: UserInputMessage {
                role: Role::User,
                content: UserContent::Blocks(blocks),
            },
        })
    }

    pub fn control_request(request_id: ControlRequestId, request: ControlRequest) -> Self {
        Self::ControlRequest(OutgoingControlRequest {
            request_id,
            request,
        })
    }

    /// Answers a `can_use_tool` request.
    pub fn permission(request_id: ControlRequestId, decision: PermissionDecision) -> Self {
        Self::ControlResponse(OutgoingControlResponse {
            response: ControlAnswer::Success {
                request_id,
                response: decision,
            },
        })
    }

    /// Answers a control request with an error, such as a request this crate does not know.
    pub fn control_error(request_id: ControlRequestId, error: String) -> Self {
        Self::ControlResponse(OutgoingControlResponse {
            response: ControlAnswer::Error { request_id, error },
        })
    }

    /// The JSON text of the line, `\n` included.
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        let mut line = serde_json::to_string(self)?;
        line.push('\n');
        Ok(line)
    }
}
