//! The app's state, and the pure functions that change it: daemon events, responses, clicks and
//! keys. None of them needs a window, so they are tested on their own.

pub mod actions;
mod app_state;
mod apply;
mod drafts;
mod export;
pub mod file_edits;
mod requests;
mod shortcuts;
mod transcript;
mod ui_state;

pub use app_state::{AgentEntry, AppState, ConnectionState, Loadable};
pub use apply::{Alert, AlertKind, apply_update};
pub use drafts::Drafts;
pub use requests::{Outgoing, apply_response, needed_requests};
pub use shortcuts::{KeyPress, handle_key};
pub use transcript::{HistoryState, WaitingKind};
pub use ui_state::{
    Block, Dialog, McpView, NewAgentForm, NewAgentTab, NoteKind, PaletteOrigin, Popover,
    ToolFilter, ViewMode,
};

#[cfg(test)]
pub(crate) use app_state::tests::{state_with, summary};
