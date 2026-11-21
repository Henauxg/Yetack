//! The git requests: a file's last committed text (`agent.git_file`), its blame
//! (`agent.git_blame`) and history (`agent.git_log`), one commit's change to it
//! (`agent.git_show`), and the state of the checkouts the agent works in (`agent.git_status`).
//!
//! The agent can write its checkout's git config and `.gitattributes`, and through them git runs
//! programs they name. The daemon runs git without the user's approval and outside Claude Code's
//! sandbox, so it never lets git run one:
//!
//! - every call goes through one command builder, which turns off the file system monitor, the
//!   pager, git's optional index lock, the index refresh of `git diff` (writing the index runs the
//!   `post-index-change` hook), fetches (a partial clone fetches a missing object, and a fetch
//!   runs programs the config names), and pathspec magic;
//! - the requests on one file read commits only: a blob by its hash, the blame of a given commit,
//!   and a log without textconv, external diff or signature check;
//! - `agent.git_status`, the only request that reads the working tree, first lists the filters
//!   the config defines and turns each off, and leaves submodules out. A filter whose name git
//!   cannot be told to turn off refuses the checkout.
//!
//! A path reaches git after `--`, relative to the checkout's top folder, once it passed the
//! checks of `agent.read_file`. A checkout is the folder with the `.git` entry nearest above the
//! file, unless it is `/` or holds the home folder, as for a search.

mod blame;
mod blob;
mod checkout;
mod command;
mod commit;
mod file;
mod git_error;
mod log;
mod numstat;
mod status;
#[cfg(test)]
mod test_repo;

pub(crate) use blame::blame;
pub(crate) use file::{file, show};
pub(crate) use git_error::GitError;
pub(crate) use log::log;
pub(crate) use status::status;

#[cfg(test)]
mod tests;
