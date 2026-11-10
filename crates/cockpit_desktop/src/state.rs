//! The app's state, and the pure functions that change it: daemon events, responses, clicks and
//! keys. None of them needs a window, so they are tested on their own.

pub mod accounts;
pub mod actions;
pub mod agent_map;
mod app_state;
mod apply;
pub mod chat_images;
pub mod daemon_update;
mod drafts;
mod export;
pub mod file_edits;
pub mod file_list;
pub mod file_reread;
pub mod file_tree;
pub mod folder_field;
pub mod machine_load;
pub mod machines;
pub mod no_daemon;
pub mod notification_levels;
pub mod open_file;
mod requests;
mod shortcuts;
pub mod sidebar_keys;
pub mod title_emoji;
pub mod to_handle;
mod transcript;
mod ui_state;
mod utf16_offset;
pub mod working_folder;

pub use app_state::{
    AgentEntry, AppState, ConnectionState, FileFreshness, FileRead, FileUpdate, Loadable,
    MachineState, Reread, TurnStart,
};
pub use apply::{Alert, AlertKind, apply_update, error_chain, next_finish_due, release_finishes};
pub use drafts::Drafts;
pub use requests::{Outgoing, Purpose, apply_response, needed_requests};
pub use shortcuts::{
    ANSWER_KEYS, CLOSE_AGENT_KEY, COMMAND_KEYS, KeyPress, MARK_KEY, RELOAD_KEY, handle_key,
};
pub use transcript::{HistoryState, WaitingKind, awaiting_approval};
pub use ui_state::{
    Block, Dialog, FileSearch, FolderAction, FolderList, FormMenu, ImageView, MachinesView,
    McpView, NewAgentForm, NewAgentTab, NoteKind, OpenFileView, PaletteOrigin, Popover,
    RevealPlace, TitleEdit, TitlePlace, ToolFilter, ViewMode, WorkingFolderForm, is_markdown,
};
pub use utf16_offset::Utf16Offset;

#[cfg(test)]
pub(crate) use app_state::tests::{state_with, summary};
