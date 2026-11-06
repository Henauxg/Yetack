//! The load where the daemon has no Linux `/proc` to read it from: it answers `unsupported`.

use cockpit_protocol::{ErrorBody, ErrorCode, ResponseResult};
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
