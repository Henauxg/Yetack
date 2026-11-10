//! The components, matching the round 5 mockups.

mod accounts;
mod agent_map;
mod app;
mod blocks;
mod cards;
mod changes_list;
mod chat_scroll;
mod code;
mod composer;
mod daemon_update;
mod dialogs;
mod emoji_picker;
mod file_list;
mod file_view;
mod header;
mod icons;
mod last_instruction;
mod load_card;
mod loading;
mod machines;
mod markdown;
mod mcp;
mod no_daemon;
mod open_file;
mod palette;
mod presence;
mod reorder;
mod sidebar;
mod splitter;
mod transcript;
mod waiting_prompts;
mod zoom;

pub use app::App;
pub use sidebar::item_id as sidebar_item_id;
