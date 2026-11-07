//! A handle to another process on Windows. While it is open, the process's id names no other
//! process, so what the daemon checks through it stays true of the process it acts on.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr;

use cockpit_protocol::ProcessId;
use windows_sys::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessBasicInformation, ProcessCommandLineInformation,
};
use windows_sys::Win32::Foundation::{FILETIME, UNICODE_STRING, WAIT_OBJECT_0};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_BASIC_INFORMATION,
    PROCESS_NAME_WIN32, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};

/// The exit code of a process the daemon stops.
const STOPPED_EXIT_CODE: u32 = 1;
/// The longest command line Windows takes, in UTF-16 units.
const MAX_COMMAND_UNITS: usize = 32_768;

/// When a process started, and the CPU time it used, both in units of 100 ns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProcessTimes {
    /// Since 1601, as `GetSystemTimeAsFileTime` counts.
    pub(super) started: u64,
    pub(super) cpu_time: u64,
}

#[derive(Debug)]
pub(super) struct OpenedProcess(OwnedHandle);

impl OpenedProcess {
    pub(super) fn open(pid: ProcessId, access: PROCESS_ACCESS_RIGHTS) -> io::Result<Self> {
        // SAFETY: the call takes plain values and returns a new handle or null.
        let handle = unsafe { OpenProcess(access, 0, pid.get()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the handle is open and nothing else owns it.
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle) }))
    }

    pub(super) fn raw(&self) -> RawHandle {
        self.0.as_raw_handle()
    }

    /// Needs `PROCESS_TERMINATE`.
    pub(super) fn terminate(&self) -> io::Result<()> {
        // SAFETY: the handle is open.
        if unsafe { TerminateProcess(self.raw(), STOPPED_EXIT_CODE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Needs `PROCESS_SYNCHRONIZE`.
    pub(super) fn has_ended(&self) -> bool {
        // SAFETY: the handle is open; a wait of 0 ms returns at once.
        unsafe { WaitForSingleObject(self.raw(), 0) == WAIT_OBJECT_0 }
    }

    /// Waits up to `limit` for the process to end. Needs `PROCESS_SYNCHRONIZE`.
    pub(super) fn wait(&self, limit: std::time::Duration) {
        let milliseconds = u32::try_from(limit.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: the handle is open.
        unsafe { WaitForSingleObject(self.raw(), milliseconds) };
    }

    /// Needs `PROCESS_QUERY_LIMITED_INFORMATION`.
    pub(super) fn times(&self) -> io::Result<ProcessTimes> {
        let mut started = FILETIME::default();
        let mut ended = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: the handle is open, and each pointer is to a `FILETIME`.
        let read = unsafe {
            GetProcessTimes(
                self.raw(),
                &raw mut started,
                &raw mut ended,
                &raw mut kernel,
                &raw mut user,
            )
        };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ProcessTimes {
            started: to_u64(started),
            cpu_time: to_u64(kernel).saturating_add(to_u64(user)),
        })
    }

    /// The memory the process has resident, in bytes. Needs `PROCESS_QUERY_LIMITED_INFORMATION`
    /// and `PROCESS_VM_READ`.
    pub(super) fn working_set(&self) -> io::Result<u64> {
        let mut counters = PROCESS_MEMORY_COUNTERS {
            cb: size_of_u32::<PROCESS_MEMORY_COUNTERS>(),
            ..PROCESS_MEMORY_COUNTERS::default()
        };
        // SAFETY: the handle is open, and the pointer and size describe `counters`.
        let read = unsafe { K32GetProcessMemoryInfo(self.raw(), &raw mut counters, counters.cb) };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(u64::try_from(counters.WorkingSetSize).unwrap_or(u64::MAX))
    }

    /// The id of the process that started it, which may have ended since and given its id to
    /// another. Needs `PROCESS_QUERY_LIMITED_INFORMATION`.
    pub(super) fn parent(&self) -> io::Result<ProcessId> {
        let mut info = PROCESS_BASIC_INFORMATION::default();
        // SAFETY: the handle is open, and the pointer and size describe `info`.
        let status = unsafe {
            NtQueryInformationProcess(
                self.raw(),
                ProcessBasicInformation,
                (&raw mut info).cast(),
                size_of_u32::<PROCESS_BASIC_INFORMATION>(),
                ptr::null_mut(),
            )
        };
        if status < 0 {
            return Err(io::Error::other(format!(
                "cannot read the parent: status {status:#x}"
            )));
        }
        u32::try_from(info.InheritedFromUniqueProcessId)
            .map(ProcessId::new)
            .map_err(|_| io::Error::other("a parent id past 32 bits"))
    }

    /// The process's command line. Needs `PROCESS_QUERY_LIMITED_INFORMATION`.
    pub(super) fn command_line(&self) -> io::Result<String> {
        let mut needed: u32 = 0;
        // SAFETY: the handle is open; with no buffer, the call only writes the size it needs.
        unsafe {
            NtQueryInformationProcess(
                self.raw(),
                ProcessCommandLineInformation,
                ptr::null_mut(),
                0,
                &raw mut needed,
            )
        };
        let header = size_of::<UNICODE_STRING>();
        let needed = usize::try_from(needed).unwrap_or(usize::MAX);
        if needed < header || needed > header + MAX_COMMAND_UNITS * 2 {
            return Err(io::Error::other("no command line"));
        }
        // In 8-byte words, aligned for the header and its pointer.
        let mut buffer: Vec<u64> = vec![0; needed.div_ceil(size_of::<u64>())];
        let size = u32::try_from(buffer.len() * size_of::<u64>()).unwrap_or(u32::MAX);
        // SAFETY: the handle is open, and the buffer holds `size` bytes, aligned for the
        // `UNICODE_STRING` the call writes first.
        let status = unsafe {
            NtQueryInformationProcess(
                self.raw(),
                ProcessCommandLineInformation,
                buffer.as_mut_ptr().cast(),
                size,
                ptr::null_mut(),
            )
        };
        if status < 0 {
            return Err(io::Error::other(format!(
                "cannot read the command line: status {status:#x}"
            )));
        }
        // SAFETY: the call wrote the header at the start of the buffer, which is aligned for it.
        let text = unsafe { ptr::read(buffer.as_ptr().cast::<UNICODE_STRING>()) };
        // The text follows the header in the same buffer: its pointer is checked to be there.
        let start = (text.Buffer as usize).wrapping_sub(buffer.as_ptr() as usize);
        let units = usize::from(text.Length) / 2;
        let end = start.saturating_add(units * 2);
        if start < header || end > buffer.len() * size_of::<u64>() || !start.is_multiple_of(2) {
            return Err(io::Error::other("a command line outside its buffer"));
        }
        // SAFETY: `start..end` lies inside the buffer, at an even offset, and holds `units`
        // UTF-16 units the call wrote.
        let line = unsafe {
            std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>().add(start).cast::<u16>(), units)
        };
        Ok(OsString::from_wide(line).to_string_lossy().into_owned())
    }

    /// The path of the process's program. Needs `PROCESS_QUERY_LIMITED_INFORMATION`.
    pub(super) fn image_path(&self) -> io::Result<String> {
        let mut buffer = vec![0_u16; MAX_COMMAND_UNITS];
        let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        // SAFETY: the handle is open, and `length` is the buffer's size in UTF-16 units.
        let read = unsafe {
            QueryFullProcessImageNameW(
                self.raw(),
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &raw mut length,
            )
        };
        if read == 0 {
            return Err(io::Error::last_os_error());
        }
        let path = buffer
            .get(..usize::try_from(length).unwrap_or(0))
            .unwrap_or_default();
        Ok(OsString::from_wide(path).to_string_lossy().into_owned())
    }
}

fn to_u64(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

/// The size of `T`, as the Windows calls take it.
fn size_of_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).unwrap_or(u32::MAX)
}
