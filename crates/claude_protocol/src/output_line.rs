use std::str::FromStr;

use serde_json::Value;
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::control::{ControlCancelRequest, ControlResponse, IncomingControlRequest};
use crate::conversation_reset::ConversationReset;
use crate::message::{AssistantMessage, StreamEvent, UserMessage};
use crate::parse_error::ParseError;
use crate::rate_limit::RateLimitEvent;
use crate::result_message::ResultMessage;
use crate::system_event::SystemEvent;
use crate::wire;

/// A line `claude` writes on stdout, by its `type`. Read one with [`str::parse`].
#[derive(Debug, Clone, PartialEq)]
pub enum OutputLine {
    System(SystemEvent),
    Assistant(AssistantMessage),
    User(UserMessage),
    Result(ResultMessage),
    StreamEvent(StreamEvent),
    RateLimitEvent(RateLimitEvent),
    ControlRequest(IncomingControlRequest),
    ControlResponse(ControlResponse),
    ControlCancelRequest(ControlCancelRequest),
    ConversationReset(ConversationReset),
    /// JSON of a type this crate does not know, or without a type, as Claude Code wrote it.
    Unknown(Value),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
enum LineType {
    System,
    Assistant,
    User,
    Result,
    StreamEvent,
    RateLimitEvent,
    ControlRequest,
    ControlResponse,
    ControlCancelRequest,
    ConversationReset,
}

impl FromStr for OutputLine {
    type Err = ParseError;

    fn from_str(line: &str) -> Result<Self, ParseError> {
        let value: Value = serde_json::from_str(line).map_err(ParseError::NotJson)?;
        Self::try_from(value)
    }
}

/// Reads a line already parsed as JSON, such as a line of a session file whose other keys the
/// caller reads too.
impl TryFrom<Value> for OutputLine {
    type Error = ParseError;

    fn try_from(value: Value) -> Result<Self, ParseError> {
        let Some(line_type) = wire::word::<LineType>(&value, wire::TYPE) else {
            return Ok(Self::Unknown(value));
        };
        let kind: &'static str = line_type.into();
        match line_type {
            LineType::System => SystemEvent::from_value(value).map(Self::System),
            LineType::Assistant => wire::read(&value, kind).map(Self::Assistant),
            LineType::User => wire::read(&value, kind).map(Self::User),
            LineType::Result => wire::read(&value, kind).map(Self::Result),
            LineType::StreamEvent => wire::read(&value, kind).map(Self::StreamEvent),
            LineType::RateLimitEvent => wire::read(&value, kind).map(Self::RateLimitEvent),
            LineType::ControlRequest => {
                IncomingControlRequest::from_value(&value, kind).map(Self::ControlRequest)
            }
            LineType::ControlResponse => {
                ControlResponse::from_value(&value, kind).map(Self::ControlResponse)
            }
            LineType::ControlCancelRequest => {
                wire::read(&value, kind).map(Self::ControlCancelRequest)
            }
            LineType::ConversationReset => wire::read(&value, kind).map(Self::ConversationReset),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_types_are_pinned() {
        let words: Vec<&str> = LineType::VARIANTS.iter().map(|&word| word.into()).collect();
        assert_eq!(
            words,
            [
                "system",
                "assistant",
                "user",
                "result",
                "stream_event",
                "rate_limit_event",
                "control_request",
                "control_response",
                "control_cancel_request",
                "conversation_reset",
            ]
        );
    }
}
