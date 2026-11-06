//! Where the app's log goes: stderr, or a file in the app's state folder when the app has no
//! stderr. A release build on Windows is a GUI program, and one started from the desktop or the
//! Start menu has none: what it writes there is lost.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;

use crate::config::Environment;
use crate::prefs::PREFS_FOLDER;

/// Log level when `RUST_LOG` is not set.
const DEFAULT_LOG: &str = "warn";

const LOG_FILE: &str = "app.log";

/// The log of the run before, kept for a crash to be read after a restart.
const PREVIOUS_LOG_FILE: &str = "app.previous.log";

/// Starts the log. Without stderr and without a state folder, nothing is logged.
pub fn init(environment: &Environment) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG));
    let logger = tracing_subscriber::fmt().with_env_filter(filter);
    let file = if has_stderr() {
        None
    } else {
        log_folder(environment).and_then(|folder| open_log(&folder).ok())
    };
    match file {
        Some(file) => logger.with_ansi(false).with_writer(Mutex::new(file)).init(),
        None => logger.init(),
    }
}

/// The app's state folder, which holds the preferences too.
fn log_folder(environment: &Environment) -> Option<PathBuf> {
    Some(environment.state_base()?.join(PREFS_FOLDER))
}

/// Whether stderr reaches anything: a console, a file or a pipe. A GUI program gets one only
/// when its starter redirects it.
#[cfg(windows)]
fn has_stderr() -> bool {
    use std::os::windows::io::AsRawHandle;

    let handle = io::stderr().as_raw_handle();
    // Null, or INVALID_HANDLE_VALUE, which is -1.
    !handle.is_null() && handle.addr() != usize::MAX
}

#[cfg(unix)]
fn has_stderr() -> bool {
    true
}

/// Creates the log file in `folder`, after the last run's log becomes the previous one.
fn open_log(folder: &Path) -> io::Result<File> {
    std::fs::create_dir_all(folder)?;
    let file = folder.join(LOG_FILE);
    match std::fs::rename(&file, folder.join(PREVIOUS_LOG_FILE)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    File::create(file)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn each_run_starts_a_log_and_keeps_the_last_one() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join(PREFS_FOLDER);

        writeln!(open_log(&folder).unwrap(), "first run").unwrap();
        writeln!(open_log(&folder).unwrap(), "second run").unwrap();

        let read = |name: &str| std::fs::read_to_string(folder.join(name)).unwrap();
        assert_eq!(read(LOG_FILE), "second run\n");
        assert_eq!(read(PREVIOUS_LOG_FILE), "first run\n");
    }
}
