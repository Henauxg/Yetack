//! A handle to another process on Windows. While it is open, the process's id names no other
//! process, so what the daemon checks through it stays true of the process it acts on.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use cockpit_protocol::ProcessId;
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_ACCESS_RIGHTS, TerminateProcess, WaitForSingleObject,
};

/// The exit code of a process the daemon stops.
const STOPPED_EXIT_CODE: u32 = 1;

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
}
