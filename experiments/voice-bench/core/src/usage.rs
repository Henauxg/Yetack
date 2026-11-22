//! This process's memory and CPU time.

use crate::protocol::Usage;

#[cfg(windows)]
pub fn now() -> Usage {
    use std::mem::{size_of, zeroed};

    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    fn hundred_ns(time: FILETIME) -> u64 {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    }

    // SAFETY: both calls only write into the zeroed structures, whose sizes they are given.
    unsafe {
        let process = GetCurrentProcess();
        let mut counters: PROCESS_MEMORY_COUNTERS_EX = zeroed();
        counters.cb = size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        GetProcessMemoryInfo(
            process,
            (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
            counters.cb,
        );
        let (mut created, mut exited, mut kernel, mut user): (FILETIME, FILETIME, FILETIME, FILETIME) =
            (zeroed(), zeroed(), zeroed(), zeroed());
        GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user);
        Usage {
            working_set: counters.WorkingSetSize as u64,
            peak_working_set: counters.PeakWorkingSetSize as u64,
            private: counters.PrivateUsage as u64,
            peak_private: counters.PeakPagefileUsage as u64,
            cpu_ms: (hundred_ns(kernel) + hundred_ns(user)) as f64 / 10_000.0,
        }
    }
}

#[cfg(target_os = "linux")]
pub fn now() -> Usage {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kilobytes = |key: &str| -> u64 {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    Usage {
        working_set: kilobytes("VmRSS:"),
        peak_working_set: kilobytes("VmHWM:"),
        private: kilobytes("RssAnon:"),
        peak_private: 0,
        cpu_ms: rusage_cpu_ms(),
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
pub fn now() -> Usage {
    Usage {
        cpu_ms: rusage_cpu_ms(),
        ..Usage::default()
    }
}

#[cfg(unix)]
fn rusage_cpu_ms() -> f64 {
    // SAFETY: getrusage only writes into the zeroed structure.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    let seconds = |t: libc::timeval| t.tv_sec as f64 * 1000.0 + t.tv_usec as f64 / 1000.0;
    seconds(usage.ru_utime) + seconds(usage.ru_stime)
}
