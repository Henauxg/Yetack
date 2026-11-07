//! Where the app's log goes: stderr, or a file in the app's state folder when the app has no
//! stderr. A release build on Windows is a GUI program, and one started from the desktop or the
//! Start menu has none: what it writes there is lost.
//!
//! The file rotates by size: `app.log` becomes `app.1.log` once it reaches its limit, the older
//! files move up one number, and the oldest goes. A new run appends to `app.log`, so the
//! rotation carries on across runs.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;

use crate::config::Environment;
use crate::prefs::PREFS_FOLDER;

/// Log level when `RUST_LOG` is not set.
const DEFAULT_LOG: &str = "warn";

const LOG_NAME: &str = "app";
const LOG_EXTENSION: &str = "log";
/// The size at which `app.log` becomes `app.1.log`.
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
/// The files kept, `app.log` included: at most 50 MiB in all.
const KEPT_FILES: u32 = 5;

/// The log of the run before, as older versions kept it.
const PREVIOUS_LOG_FILE: &str = "app.previous.log";

/// Starts the log. Without stderr and without a state folder, nothing is logged.
pub fn init(environment: &Environment) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG));
    let logger = tracing_subscriber::fmt().with_env_filter(filter);
    let file = if has_stderr() {
        None
    } else {
        log_folder(environment)
            .and_then(|folder| RotatingLog::open(&folder, MAX_LOG_BYTES, KEPT_FILES).ok())
    };
    match file {
        Some((file, migration)) => {
            logger.with_ansi(false).with_writer(Mutex::new(file)).init();
            if let Err(error) = migration {
                tracing::warn!("cannot move {PREVIOUS_LOG_FILE} to the rotated logs: {error}");
            }
        }
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

/// `app.log`, which becomes `app.1.log` at `max_bytes`, keeping `kept` files in all.
#[derive(Debug)]
struct RotatingLog {
    folder: PathBuf,
    file: File,
    /// The bytes in `file`.
    written: u64,
    max_bytes: u64,
    kept: u32,
}

impl RotatingLog {
    /// Appends to the log in `folder`, after an older version's `app.previous.log` becomes
    /// `app.1.log`, as the run before the current `app.log`. A failed move of that file leaves
    /// the log working, and comes back as the second value.
    fn open(folder: &Path, max_bytes: u64, kept: u32) -> io::Result<(Self, io::Result<()>)> {
        std::fs::create_dir_all(folder)?;
        let migration = migrate_previous_log(folder, kept);
        let file = append(&folder.join(file_name(0)))?;
        let written = file.metadata()?.len();
        let log = Self {
            folder: folder.to_owned(),
            file,
            written,
            max_bytes,
            kept,
        };
        Ok((log, migration))
    }

    /// Moves each file up one number, the oldest out, and starts `app.log` afresh.
    fn rotate(&mut self) -> io::Result<()> {
        debug_assert!(self.kept >= 2, "the rotation keeps the file it moves away");
        let oldest = self.folder.join(file_name(self.kept - 1));
        match std::fs::remove_file(&oldest) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        for number in (0..self.kept - 1).rev() {
            let from = self.folder.join(file_name(number));
            match std::fs::rename(&from, self.folder.join(file_name(number + 1))) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.file = append(&self.folder.join(file_name(0)))?;
        self.written = 0;
        Ok(())
    }
}

impl Write for RotatingLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let size = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if self.written > 0
            && self.written.saturating_add(size) > self.max_bytes
            && self.rotate().is_err()
        {
            // The log goes on in the file it has, and tries again after as many bytes more.
            self.written = 0;
        }
        // A short write would let the rest of the line start the next file.
        self.file.write_all(buf)?;
        self.written = self.written.saturating_add(size);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Moves an older version's `app.previous.log` to `app.1.log`, or deletes it when `app.1.log`
/// exists already.
fn migrate_previous_log(folder: &Path, kept: u32) -> io::Result<()> {
    let previous = folder.join(PREVIOUS_LOG_FILE);
    if !previous.exists() {
        return Ok(());
    }
    let first = folder.join(file_name(1));
    if kept > 1 && !first.exists() {
        std::fs::rename(&previous, first)
    } else {
        std::fs::remove_file(&previous)
    }
}

/// `app.log` for 0, else `app.<number>.log`.
fn file_name(number: u32) -> String {
    if number == 0 {
        format!("{LOG_NAME}.{LOG_EXTENSION}")
    } else {
        format!("{LOG_NAME}.{number}.{LOG_EXTENSION}")
    }
}

fn append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The files in `folder`, by name, with their text.
    fn files(folder: &Path) -> Vec<(String, String)> {
        let mut files: Vec<(String, String)> = std::fs::read_dir(folder)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().to_string_lossy().into_owned();
                (name, std::fs::read_to_string(entry.path()).unwrap())
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn the_log_rotates_by_size_and_keeps_the_last_files() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join(PREFS_FOLDER);
        let (mut log, _) = RotatingLog::open(&folder, 10, 3).unwrap();

        for line in ["line 1\n", "line 2\n", "line 3\n", "line 4\n"] {
            log.write_all(line.as_bytes()).unwrap();
        }

        assert_eq!(
            files(&folder),
            [
                ("app.1.log".to_owned(), "line 3\n".to_owned()),
                ("app.2.log".to_owned(), "line 2\n".to_owned()),
                ("app.log".to_owned(), "line 4\n".to_owned()),
            ]
        );
    }

    #[test]
    fn a_new_run_carries_on_the_rotation() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join(PREFS_FOLDER);

        let (mut first, _) = RotatingLog::open(&folder, 20, 5).unwrap();
        first.write_all(b"first run\n").unwrap();
        drop(first);
        let (mut second, _) = RotatingLog::open(&folder, 20, 5).unwrap();
        second.write_all(b"second\n").unwrap();
        second.write_all(b"second again\n").unwrap();

        assert_eq!(
            files(&folder),
            [
                ("app.1.log".to_owned(), "first run\nsecond\n".to_owned()),
                ("app.log".to_owned(), "second again\n".to_owned()),
            ]
        );
    }

    #[test]
    fn a_line_longer_than_the_limit_is_written_whole() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join(PREFS_FOLDER);
        let (mut log, _) = RotatingLog::open(&folder, 4, 2).unwrap();

        log.write_all(b"a long line\n").unwrap();
        log.write_all(b"next\n").unwrap();

        assert_eq!(
            files(&folder),
            [
                ("app.1.log".to_owned(), "a long line\n".to_owned()),
                ("app.log".to_owned(), "next\n".to_owned()),
            ]
        );
    }

    #[test]
    fn an_older_versions_previous_log_becomes_the_first_rotated_file() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join(PREFS_FOLDER);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(PREVIOUS_LOG_FILE), "two runs ago\n").unwrap();
        std::fs::write(folder.join("app.log"), "last run\n").unwrap();

        let (mut log, migration) = RotatingLog::open(&folder, 1024, 5).unwrap();
        log.write_all(b"this run\n").unwrap();

        assert!(migration.is_ok());
        assert_eq!(
            files(&folder),
            [
                ("app.1.log".to_owned(), "two runs ago\n".to_owned()),
                ("app.log".to_owned(), "last run\nthis run\n".to_owned()),
            ]
        );
    }

    #[test]
    fn a_previous_log_that_does_not_go_still_leaves_a_log() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join(PREFS_FOLDER);
        // A folder in its place cannot be removed as a file.
        std::fs::create_dir_all(folder.join(PREVIOUS_LOG_FILE)).unwrap();
        std::fs::write(folder.join("app.1.log"), "two runs ago\n").unwrap();

        let (mut log, migration) = RotatingLog::open(&folder, 1024, 5).unwrap();
        log.write_all(b"this run\n").unwrap();

        assert!(migration.is_err());
        assert_eq!(
            std::fs::read_to_string(folder.join("app.log")).unwrap(),
            "this run\n"
        );
    }
}
