//! The app's state, and the pure functions that change it: daemon events, responses, clicks and
//! keys. None of them needs a window, so they are tested on their own.

pub mod accounts;
pub mod actions;
pub mod agent_map;
mod app_state;
mod apply;
mod drafts;
mod export;
pub mod file_edits;
pub mod folder_field;
pub mod machine_load;
pub mod machines;
pub mod no_daemon;
pub mod open_file;
mod requests;
mod shortcuts;
pub mod title_emoji;
mod transcript;
mod ui_state;
mod utf16_offset;

pub use app_state::{
    AgentEntry, AppState, ConnectionState, FileRead, Loadable, MachineState, TurnStart,
};
pub use apply::{Alert, AlertKind, apply_update, error_chain};
pub use drafts::Drafts;
pub use requests::{Outgoing, Purpose, apply_response, needed_requests};
pub use shortcuts::{ANSWER_KEYS, CLOSE_AGENT_KEY, COMMAND_KEYS, KeyPress, handle_key};
pub use transcript::{HistoryState, WaitingKind, awaiting_approval};
pub use ui_state::{
    Block, Dialog, FileSearch, FolderList, FormMenu, MachinesView, McpView, NewAgentForm,
    NewAgentTab, NoteKind, OpenFileView, PaletteOrigin, Popover, TitleEdit, TitlePlace, ToolFilter,
    ViewMode,
};
pub use utf16_offset::Utf16Offset;

#[cfg(test)]
pub(crate) use app_state::tests::{state_with, summary};
