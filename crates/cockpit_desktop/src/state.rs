//! The app's state, and the pure functions that change it: daemon events, responses, clicks and
//! keys. None of them needs a window, so they are tested on their own.

pub mod accounts;
pub mod actions;
pub mod agent_map;
mod app_state;
pub mod app_update;
mod apply;
pub mod blocks_view;
pub mod chat_images;
pub mod chat_reveal;
pub mod claude_code_version;
pub mod daemon_probe;
pub mod daemon_start;
pub mod daemon_update;
mod details_updates;
pub mod docks;
mod drafts;
mod export;
pub mod file_edits;
pub mod file_list;
pub mod file_reread;
pub mod file_tree;
pub mod find;
pub mod folder_field;
pub mod git;
pub mod grouping;
pub mod machine_load;
pub mod machine_status;
pub mod machines;
pub mod mcp_reconnect;
pub mod mcp_sign_in;
pub mod menu_keys;
pub mod new_agent_palette;
pub mod no_daemon;
pub mod notification_levels;
pub mod open_file;
pub mod page_copy;
pub mod project_menu;
pub mod projects;
pub mod quotes;
pub mod reload;
mod requests;
pub mod resume;
pub mod saved_files;
pub mod sending;
pub mod server_setup;
pub mod settings;
mod shortcuts;
pub mod sidebar_keys;
pub mod subagent_menu;
pub mod tab_menu;
pub mod terminal_menu;
pub mod terminal_tab_menu;
pub mod terminals;
pub mod title_emoji;
pub mod to_handle;
mod transcript;
mod ui_state;
pub mod unread;
pub mod update_offer;
pub mod update_plan;
mod utf16_offset;
// A build without the `voice` feature never records, and shows no mic.
#[cfg_attr(not(feature = "voice"), allow(dead_code))]
pub mod voice;
pub mod working_folder;

pub use app_state::{
    AgentEntry, AppState, CARD_EXIT, ConnectionState, FileFreshness, FileRead, FileUpdate,
    Loadable, MachineState, MessageHold, Reread, TurnStart,
};
pub use apply::{Alert, AlertKind, apply_update, error_chain, next_finish_due, release_finishes};
pub use drafts::{Draft, Drafts, QuestionDraft};
pub use requests::{Outgoing, Purpose, apply_response, keep_first_answers, needed_requests};
pub use shortcuts::{ANSWER_KEYS, KeyPress, PAGE_RELOAD_KEY, handle_key};
pub use transcript::{HistoryState, Revision, WaitingKind, awaiting_approval};
pub use ui_state::{
    Block, BranchCard, ChatReveal, Dialog, FileSearch, FolderAction, FolderList, FormMenu,
    ImageView, MachineEdit, MachineMenu, MachinesView, McpTab, McpView, MenuSide, NewAgentForm,
    NewAgentTab, NoteKind, OpenFileView, PaletteOrigin, Popover, RefusedChord, RevealPlace,
    SettingsTab, SettingsView, TabMenuOrigin, TitleEdit, TitlePlace, ToolFilter, Travel, ViewMode,
    WorkingFolderForm, is_markdown,
};
pub use utf16_offset::Utf16Offset;

#[cfg(test)]
pub(crate) use app_state::tests::{connected_update, state_with, summary};
