//! The machine's load and each agent's share of it, for `machine.load`, and the stop of one of an
//! agent's processes, for `agent.stop_process`.
//!
//! On Linux, the sampling reads the server's totals from `/proc/stat`, `/proc/meminfo` and
//! `/proc/loadavg`, and walks each agent's process tree down from its `claude` process. It never
//! reads the other processes of the machine, unless the kernel lacks the children files
//! ([`process_tree`]).
//!
//! On Windows, it reads the server's totals from `GetSystemTimes` and `GlobalMemoryStatusEx`, and
//! each agent's figures from its Job Object, which holds every process the agent runs. A stop
//! ends a process of the agent's job.
//!
//! A daemon that starts stops the `claude` a daemon that died left running ([`leftover`]).

mod agent_root;
mod command_text;
mod history;
#[cfg(windows)]
mod job_reading;
#[cfg(any(windows, test))]
mod job_sample;
#[cfg(windows)]
mod job_sampler;
#[cfg(unix)]
mod kept_file;
mod leftover;
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

pub(crate) use agent_root::AgentRoot;
#[cfg(windows)]
use job_reading::LoadError;
#[cfg(windows)]
use job_sampler::Sampler;
pub(crate) use leftover::{record_process, stop_leftovers};
#[cfg(unix)]
pub(crate) use leftover::{running_for, still_runs};
pub(crate) use load_sampler::LoadSampler;
#[cfg(unix)]
pub(crate) use proc_files::{PROC_ROOT, STAT_FILE};
#[cfg(unix)]
use reading::LoadError;
#[cfg(unix)]
use sampler::Sampler;
#[cfg(windows)]
pub(crate) use stop_in_job::stop_descendant;
#[cfg(unix)]
pub(crate) use stop_process::stop_descendant;
