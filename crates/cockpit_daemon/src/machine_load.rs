//! The machine's load and each agent's share of it, for `machine.load`, and the stop of one of an
//! agent's processes, for `agent.stop_process`.
//!
//! The sampling reads the server's totals from `/proc/stat`, `/proc/meminfo` and `/proc/loadavg`,
//! and walks each agent's process tree down from its `claude` process. It never reads the other
//! processes of the machine, unless the kernel lacks the children files ([`process_tree`]).

mod history;
mod kept_file;
mod load_sampler;
mod proc_files;
mod process_tree;
mod reading;
mod sample;
mod sampler;
mod stop_process;

pub(crate) use load_sampler::LoadSampler;
pub(crate) use reading::AgentRoot;
pub(crate) use stop_process::stop_descendant;
