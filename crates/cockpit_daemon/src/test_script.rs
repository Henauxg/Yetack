//! Shell scripts that stand in for `claude` in unit tests.

use std::fs::Permissions;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Writes `text` at `path` as an executable script, and returns once it runs.
///
/// A process another test forks while the script is written keeps it open for writing until it
/// runs its own program, and running the script fails with "Text file busy" until then. Once the
/// script ran, no process can hold it open for writing again.
pub(crate) fn write_script(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
    for _ in 0..200 {
        let run = Command::new(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match run {
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(_) | Err(_) => return,
        }
    }
}

/// Whether the process runs: it has not ended, and is no zombie left to reap.
#[cfg(target_os = "linux")]
pub(crate) fn runs(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next());
    state.is_some_and(|state| state != 'Z')
}
