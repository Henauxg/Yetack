//! The Claude accounts agents run with: their list in `settings.json`, the environment each
//! one's `claude` gets, and how a user signs in to one. The daemon never reads, stores or passes
//! a credential: it chooses the folder, and which variables `claude` keeps.

mod account_list;
mod claude_env;
mod sign_in;

pub(crate) use account_list::AccountList;
pub use claude_env::API_KEY_ENV;
pub(crate) use claude_env::{ClaudeEnv, environment_api_key};
pub(crate) use sign_in::{SignInShell, account_state, login_written, sign_in_command};
