//! The app's state, and the pure functions that change it: daemon events, responses, clicks and
//! keys. None of them needs a window, so they are tested on their own.

pub mod actions;
mod app_state;
mod apply;
pub mod file_edits;
mod requests;
mod shortcuts;
mod transcript;
mod ui_state;

pub use app_state::{AgentEntry, AppState, ConnectionState, Loadable};
pub use apply::{Alert, AlertKind, apply, apply_catchup, apply_update, error_chain};
pub use requests::{Outgoing, Purpose, apply_response, needed_requests};
pub use shortcuts::{KeyPress, KeyTarget, handle_key};
pub use transcript::{HistoryState, Transcript, Waiting, WaitingKind};
pub use ui_state::{
    Block, Dialog, Draft, FileView, NewAgentForm, NewAgentTab, Popover, QuestionDraft, Switcher,
    ToolFilter, UiState, ViewMode,
};
