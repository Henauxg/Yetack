//! The machine's load and each agent's share of it, for `machine.load`, and the stop of one of an
//! agent's processes, for `agent.stop_process`.
//!
//! The sampling reads the server's totals from `/proc/stat`, `/proc/meminfo` and `/proc/loadavg`,
//! and walks each agent's process tree down from its `claude` process. It never reads the other
//! processes of the machine, unless the kernel lacks the children files ([`process_tree`]).
//!
//! On Windows, `machine.load` answers `unsupported`, and `agent.stop_process` ends a process of
//! the agent's Job Object (module `stop_in_job`).

mod agent_root;
#[cfg(unix)]
mod history;
#[cfg(unix)]
mod kept_file;
#[cfg(unix)]
mod load_sampler;
#[cfg(windows)]
mod opened_process;
#[cfg(unix)]
mod proc_files;
#[cfg(unix)]
mod process_tree;
#[cfg(unix)]
mod reading;
#[cfg(unix)]
mod sample;
#[cfg(unix)]
mod sampler;
#[cfg(windows)]
mod stop_in_job;
#[cfg(unix)]
mod stop_process;
#[cfg(windows)]
mod unsupported;

pub(crate) use agent_root::AgentRoot;
#[cfg(unix)]
pub(crate) use load_sampler::LoadSampler;
#[cfg(unix)]
pub(crate) use proc_files::{PROC_ROOT, STAT_FILE};
#[cfg(windows)]
pub(crate) use stop_in_job::stop_descendant;
#[cfg(unix)]
pub(crate) use stop_process::stop_descendant;
#[cfg(windows)]
pub(crate) use unsupported::LoadSampler;
