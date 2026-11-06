//! The load and the stop of a process where the daemon has no Linux `/proc` to read them from: both
//! answer `unsupported`.

use cockpit_protocol::{ErrorBody, ErrorCode, ProcessId, ResponseResult};
use tokio::sync::oneshot;

use crate::machine_load::agent_root::AgentRoot;

/// The hub's side of the load. It answers each question at once.
#[derive(Debug, Default)]
pub(crate) struct LoadSampler {}

impl LoadSampler {
    pub(crate) fn query(&mut self, _roots: Vec<AgentRoot>, reply: oneshot::Sender<ResponseResult>) {
        let unsupported = ErrorBody::new(
            ErrorCode::Unsupported,
            "cannot measure the load: the daemon reads it from Linux's /proc".to_owned(),
        );
        // The client may be gone.
        let _ = reply.send(ResponseResult::Error(unsupported));
    }
}

#[derive(Debug, thiserror::Error)]
#[error("cannot stop one of the agent's processes: the daemon finds them in Linux's /proc")]
pub(crate) struct StopProcessError;

impl StopProcessError {
    pub(crate) fn to_error_body(&self) -> ErrorBody {
        ErrorBody::new(ErrorCode::Unsupported, self.to_string())
    }
}

pub(crate) fn stop_descendant(
    _root: ProcessId,
    _target: ProcessId,
) -> Result<(), StopProcessError> {
    Err(StopProcessError)
}
