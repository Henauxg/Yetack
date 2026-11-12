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
pub mod grouping;
pub mod machine_load;
pub mod machines;
pub mod mcp_reconnect;
pub mod menu_keys;
pub mod no_daemon;
pub mod notification_levels;
pub mod open_file;
pub mod project_menu;
pub mod projects;
pub mod quotes;
mod requests;
pub mod sending;
pub mod settings;
mod shortcuts;
pub mod sidebar_keys;
pub mod tab_menu;
pub mod title_emoji;
pub mod to_handle;
mod transcript;
mod ui_state;
pub mod unread;
pub mod update_offer;
pub mod update_plan;
mod utf16_offset;
pub mod working_folder;

pub use app_state::{
    AgentEntry, AppState, ConnectionState, FileFreshness, FileRead, FileUpdate, Loadable,
    MachineState, MessageHold, Reread, TurnStart,
};
pub use apply::{Alert, AlertKind, apply_update, error_chain, next_finish_due, release_finishes};
pub use drafts::{Draft, Drafts, QuestionDraft};
pub use requests::{Outgoing, Purpose, apply_response, needed_requests};
pub use shortcuts::{ANSWER_KEYS, KeyPress, PAGE_RELOAD_KEY, handle_key};
pub use transcript::{HistoryState, WaitingKind, awaiting_approval};
pub use ui_state::{
    Block, Dialog, FileSearch, FolderAction, FolderList, FormMenu, ImageView, MachineEdit,
    MachinesView, McpView, NewAgentForm, NewAgentTab, NoteKind, OpenFileView, PaletteOrigin,
    Popover, RefusedChord, RevealPlace, SettingsTab, SettingsView, TabMenuOrigin, TitleEdit,
    TitlePlace, ToolFilter, ViewMode, WorkingFolderForm, is_markdown,
};
pub use utf16_offset::Utf16Offset;

#[cfg(test)]
pub(crate) use app_state::tests::{connected_update, state_with, summary};
