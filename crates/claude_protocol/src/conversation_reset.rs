use serde::Deserialize;
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use crate::ids::SessionId;
use crate::open_word::OpenWord;

/// A `conversation_reset` line: Claude Code starts a new conversation in the same process, as
/// `/clear` does. A `system/init` with the new session id follows, and the process's
/// `total_cost_usd` counts from 0 again.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ConversationReset {
    pub trigger: Option<OpenWord<ResetTrigger>>,
    /// The session that ended.
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Display, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum ResetTrigger {
    Clear,
}
