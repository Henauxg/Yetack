//! A Windows Job Object for each agent's `claude`, which holds every process `claude` starts.
//!
//! Ending the job ends the whole tree at once, and Windows ends it when the job's last handle
//! closes, as when the daemon's process dies. No process can leave the job.
//!
//! The process starts suspended and joins the job before it runs any code, so a process it
//! starts is always in the job too.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::{io, mem, ptr};

use cockpit_protocol::ProcessId;
use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

/// The exit code of the processes a stop ends.
const KILLED_EXIT_CODE: u32 = 1;
/// What `ResumeThread` returns when it fails.
const RESUME_FAILED: u32 = u32::MAX;

/// A handle to a job. Its processes die when the last handle to it closes.
#[derive(Debug)]
pub(crate) struct Job(OwnedHandle);

impl Job {
    /// An empty job whose processes die when its last handle closes.
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: null attributes and a null name ask for an unnamed job with the default
        // security; the call reads neither.
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the handle is open and nothing else owns it.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the job handle is open, and the pointer and size describe `limits`, the
        // structure this class takes.
        let set = unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of_u32::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(),
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Puts the process `process`, an open handle, in the job.
    pub(crate) fn assign(&self, process: RawHandle) -> io::Result<()> {
        // SAFETY: both handles are open for the length of the call.
        let assigned = unsafe { AssignProcessToJobObject(self.raw(), process) };
        if assigned == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Ends every process in the job.
    pub(crate) fn terminate(&self) -> io::Result<()> {
        // SAFETY: the job handle is open.
        let terminated = unsafe { TerminateJobObject(self.raw(), KILLED_EXIT_CODE) };
        if terminated == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Whether `process`, an open handle, is in the job. An open handle keeps its process's id
    /// from going to another process, so the answer stays true for that process.
    pub(crate) fn contains(&self, process: RawHandle) -> io::Result<bool> {
        let mut result = 0;
        // SAFETY: both handles are open for the length of the call, and `result` is a `BOOL`.
        let checked = unsafe { IsProcessInJob(process, self.raw(), &raw mut result) };
        if checked == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(result != 0)
    }

    /// The user and kernel time of every process the job ever held, in units of 100 ns.
    pub(crate) fn cpu_time(&self) -> io::Result<u64> {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: the job handle is open, and the pointer and size describe `accounting`, the
        // structure this class fills.
        let queried = unsafe {
            QueryInformationJobObject(
                self.raw(),
                JobObjectBasicAccountingInformation,
                (&raw mut accounting).cast(),
                size_of_u32::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>(),
                ptr::null_mut(),
            )
        };
        if queried == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = u64::try_from(accounting.TotalUserTime).unwrap_or(0);
        let kernel = u64::try_from(accounting.TotalKernelTime).unwrap_or(0);
        Ok(user.saturating_add(kernel))
    }

    /// The ids of the processes running in the job, at most `limit` of them.
    pub(crate) fn process_ids(&self, limit: usize) -> io::Result<Vec<ProcessId>> {
        let header_words =
            mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList) / size_of::<usize>();
        let mut buffer: Vec<usize> = vec![0; header_words.saturating_add(limit)];
        let size = u32::try_from(buffer.len() * size_of::<usize>()).unwrap_or(u32::MAX);
        // SAFETY: the job handle is open, and the buffer holds `size` bytes, aligned for the
        // list's header and its ids.
        let queried = unsafe {
            QueryInformationJobObject(
                self.raw(),
                JobObjectBasicProcessIdList,
                buffer.as_mut_ptr().cast(),
                size,
                ptr::null_mut(),
            )
        };
        if queried == 0 {
            let error = io::Error::last_os_error();
            // A full buffer holds the first ids.
            if error
                .raw_os_error()
                .and_then(|code| u32::try_from(code).ok())
                != Some(ERROR_MORE_DATA)
            {
                return Err(error);
            }
        }
        // SAFETY: the buffer holds the header, aligned for it, and the call wrote it.
        let listed = unsafe {
            (*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()).NumberOfProcessIdsInList
        };
        let listed = usize::try_from(listed).unwrap_or(usize::MAX).min(limit);
        let ids = buffer
            .get(header_words..header_words.saturating_add(listed))
            .unwrap_or_default();
        Ok(ids
            .iter()
            .filter_map(|&id| u32::try_from(id).ok())
            .map(ProcessId::new)
            .collect())
    }

    /// Another handle to the same job, which keeps it open as this one does.
    pub(crate) fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle()
    }
}

/// Puts `process`, started suspended, in a new job, then resumes its threads. The process runs
/// without a job when none can be made, and is then stopped alone. The error is a failure to
/// resume it.
pub(crate) fn start_in_job(process: RawHandle, pid: u32) -> io::Result<Option<Job>> {
    let job = Job::new().and_then(|job| job.assign(process).map(|()| job));
    resume_threads(pid)?;
    match job {
        Ok(job) => Ok(Some(job)),
        Err(error) => {
            tracing::warn!(%error, pid, "cannot put claude in a job: a stop ends claude alone");
            Ok(None)
        }
    }
}

/// Resumes each thread of the process `pid`. A process started suspended has one.
fn resume_threads(pid: u32) -> io::Result<()> {
    // SAFETY: the flag asks for a snapshot of the threads; the process id is ignored for them.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the handle is open and nothing else owns it.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of_u32::<THREADENTRY32>(),
        ..THREADENTRY32::default()
    };
    let mut resumed: usize = 0;
    // SAFETY: the snapshot is open, and `entry` is a `THREADENTRY32` whose size field is set.
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            resume_thread(entry.th32ThreadID)?;
            resumed += 1;
        }
        // SAFETY: as for `Thread32First`.
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &raw mut entry) } != 0;
    }
    if resumed == 0 {
        return Err(io::Error::other(format!(
            "process {pid} has no thread to resume"
        )));
    }
    Ok(())
}

fn resume_thread(thread_id: u32) -> io::Result<()> {
    // SAFETY: the call takes plain values and returns a new handle or null.
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if thread.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the handle is open and nothing else owns it.
    let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
    // SAFETY: the handle is open with the right to resume the thread.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == RESUME_FAILED {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The size of `T`, as the Windows calls take it.
fn size_of_u32<T>() -> u32 {
    u32::try_from(mem::size_of::<T>()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

    use super::*;

    /// A shell that says it started, then waits about 30 seconds, started suspended.
    fn suspended_shell() -> Child {
        Command::new("cmd.exe")
            .raw_arg("/d /c \"echo started& ping -n 30 127.0.0.1 >nul\"")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED)
            .spawn()
            .unwrap()
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
    fn a_process_started_in_a_job_runs_and_ends_with_the_job() {
        let mut child = suspended_shell();

        let job = start_in_job(child.as_raw_handle(), child.id())
            .unwrap()
            .expect("a job");

        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line.trim(), "started");
        let ids = job.process_ids(16).unwrap();
        job.terminate().unwrap();
        assert!(ended(&mut child), "the shell ends with its job");
        assert!(ids.contains(&ProcessId::new(child.id())), "{ids:?}");
        assert!(job.process_ids(16).unwrap().is_empty());
        job.cpu_time().unwrap();
    }

    #[test]
    fn closing_the_last_handle_ends_the_jobs_processes() {
        let mut child = suspended_shell();
        let job = start_in_job(child.as_raw_handle(), child.id())
            .unwrap()
            .expect("a job");

        drop(job);

        assert!(ended(&mut child), "the shell ends when the job closes");
    }
}
