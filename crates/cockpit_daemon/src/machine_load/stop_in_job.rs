//! `agent.stop_process` on Windows: ends one process of an agent's job, and no other.
//!
//! The daemon opens the process first, then checks that it is in the agent's job, then ends it
//! through the same handle. The open handle keeps the process's id from going to another process
//! in between.

use std::io;

use cockpit_protocol::{ErrorBody, ErrorCode, ProcessId};
use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER};
use windows_sys::Win32::System::Threading::{
    PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

use crate::error_chain;
use crate::machine_load::agent_root::AgentRoot;
use crate::machine_load::opened_process::OpenedProcess;

#[derive(Debug, thiserror::Error)]
pub(crate) enum StopProcessError {
    #[error("process {} is the agent's claude: stop the agent instead", .pid.get())]
    IsTheAgent { pid: ProcessId },
    #[error("process {} is not one of the agent's", .pid.get())]
    NotTheAgents { pid: ProcessId },
    #[error("process {} has ended", .pid.get())]
    Ended { pid: ProcessId },
    #[error("cannot stop process {}", .pid.get())]
    Stop {
        pid: ProcessId,
        #[source]
        source: io::Error,
    },
}

impl StopProcessError {
    pub(crate) fn to_error_body(&self) -> ErrorBody {
        let code = match self {
            Self::IsTheAgent { .. } => ErrorCode::InvalidRequest,
            Self::NotTheAgents { .. } | Self::Ended { .. } => ErrorCode::NotFound,
            Self::Stop { .. } => ErrorCode::Internal,
        };
        ErrorBody::new(code, error_chain::chain(self))
    }
}

/// Ends `target` when it is in the job of `root`, the agent's `claude`.
pub(crate) fn stop_descendant(root: AgentRoot, target: ProcessId) -> Result<(), StopProcessError> {
    if target == root.pid {
        return Err(StopProcessError::IsTheAgent { pid: target });
    }
    let Some(job) = &root.job else {
        return Err(StopProcessError::NotTheAgents { pid: target });
    };
    // `IsProcessInJob` needs the limited query right on Windows, and the full one on Wine.
    let access = PROCESS_TERMINATE | PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE;
    let process = OpenedProcess::open(target, access).map_err(|source| {
        match source
            .raw_os_error()
            .and_then(|code| u32::try_from(code).ok())
        {
            // No process has this id.
            Some(ERROR_INVALID_PARAMETER) => StopProcessError::Ended { pid: target },
            // Another user's process, or a protected one: the agent's run as the daemon's user.
            Some(ERROR_ACCESS_DENIED) => StopProcessError::NotTheAgents { pid: target },
            _ => StopProcessError::Stop {
                pid: target,
                source,
            },
        }
    })?;
    let in_job = job
        .contains(process.raw())
        .map_err(|source| StopProcessError::Stop {
            pid: target,
            source,
        })?;
    if !in_job {
        return Err(StopProcessError::NotTheAgents { pid: target });
    }
    process.terminate().map_err(|source| {
        if process.has_ended() {
            StopProcessError::Ended { pid: target }
        } else {
            StopProcessError::Stop {
                pid: target,
                source,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use cockpit_protocol::AgentId;
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

    use super::*;
    use crate::process::Job;

    /// A shell that waits about 30 seconds.
    fn waiting_shell(flags: u32) -> Child {
        Command::new("cmd.exe")
            .raw_arg("/d /c \"ping -n 30 127.0.0.1 >nul\"")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW | flags)
            .spawn()
            .unwrap()
    }

    /// An agent whose `claude` is a waiting shell in a job, with the job.
    fn agent() -> (Child, AgentRoot) {
        let child = waiting_shell(CREATE_SUSPENDED);
        let job = crate::process::job::start_in_job(child.as_raw_handle(), child.id())
            .unwrap()
            .expect("a job");
        let root = AgentRoot {
            agent: AgentId::new(1),
            pid: ProcessId::new(child.id()),
            job: Some(job),
        };
        (child, root)
    }

    fn again(root: &AgentRoot) -> AgentRoot {
        AgentRoot {
            agent: root.agent,
            pid: root.pid,
            job: root.job.as_ref().map(|job| job.try_clone().unwrap()),
        }
    }

    /// Waits up to 10 seconds for `child` to end.
    fn ended(child: &mut Child) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        false
    }

    #[test]
    fn a_process_in_the_agents_job_is_stopped() {
        let (mut claude, root) = agent();
        let job: &Job = root.job.as_ref().unwrap();
        // A suspended command starts no child of its own, which would outlive the test.
        let mut command = waiting_shell(CREATE_SUSPENDED);
        job.assign(command.as_raw_handle()).unwrap();
        let pid = ProcessId::new(command.id());

        let stopped = stop_descendant(again(&root), pid);

        assert!(stopped.is_ok(), "{stopped:?}");
        assert!(ended(&mut command), "the command ends");
        let running = claude.try_wait().unwrap().is_none();
        let _ = claude.kill();
        let _ = claude.wait();
        assert!(running, "claude runs on");
    }

    #[test]
    fn the_agent_itself_a_process_outside_it_and_an_ended_one_are_refused() {
        let (claude, root) = agent();
        let outside = waiting_shell(0);
        let mut gone = waiting_shell(0);
        let gone_pid = ProcessId::new(gone.id());
        gone.kill().unwrap();
        gone.wait().unwrap();

        let itself = stop_descendant(again(&root), root.pid);
        let other = stop_descendant(again(&root), ProcessId::new(outside.id()));
        let ended = stop_descendant(again(&root), gone_pid);
        let without_job = stop_descendant(
            AgentRoot {
                job: None,
                ..again(&root)
            },
            ProcessId::new(outside.id()),
        );

        for mut child in [outside, claude] {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(
            matches!(itself, Err(StopProcessError::IsTheAgent { .. })),
            "{itself:?}"
        );
        assert!(
            matches!(other, Err(StopProcessError::NotTheAgents { .. })),
            "{other:?}"
        );
        assert!(
            matches!(
                ended,
                Err(StopProcessError::Ended { .. } | StopProcessError::NotTheAgents { .. })
            ),
            "{ended:?}"
        );
        assert!(
            matches!(without_job, Err(StopProcessError::NotTheAgents { .. })),
            "{without_job:?}"
        );
        assert_eq!(other.unwrap_err().to_error_body().code, ErrorCode::NotFound);
    }
}
