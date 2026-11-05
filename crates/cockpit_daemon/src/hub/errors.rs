//! The error answers the hub gives.

use cockpit_protocol::{AgentId, ErrorBody, ErrorCode};

use crate::process::ControlFailure;

pub(super) fn agent_not_found(agent: AgentId) -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, format!("no agent {}", agent.get()))
}

pub(super) fn agent_exited(agent: AgentId) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::AgentExited,
        format!("agent {} is not running", agent.get()),
    )
}

pub(super) fn agent_running(agent: AgentId) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::AgentRunning,
        format!("agent {} is running: stop it first", agent.get()),
    )
}

pub(super) fn invalid(message: String) -> ErrorBody {
    ErrorBody::new(ErrorCode::InvalidRequest, message)
}

pub(super) fn internal(message: String) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, message)
}

pub(super) fn control_failed(agent: AgentId, failure: ControlFailure) -> ErrorBody {
    match failure {
        ControlFailure::Refused(message) => ErrorBody::new(ErrorCode::ClaudeFailed, message),
        ControlFailure::TimedOut => ErrorBody::new(
            ErrorCode::Timeout,
            format!("agent {}'s claude did not answer in time", agent.get()),
        ),
        ControlFailure::Exited => agent_exited(agent),
    }
}

pub(super) use crate::error_chain::chain;
