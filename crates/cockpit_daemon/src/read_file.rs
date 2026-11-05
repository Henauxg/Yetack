//! `agent.read_file`: a text file inside the agent's folder, for the file view.

use std::fs::File;
use std::io;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::{Component, Path};

use cockpit_protocol::{ErrorBody, ErrorCode};
use rustix::fs::{Mode, OFlags};

/// The largest file the view shows.
pub(crate) const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Links to the process's open files, each naming the file's path.
const OPEN_FILES: &str = "/proc/self/fd";

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadFileError {
    #[error("the path leaves the agent's folder")]
    Outside,
    #[error("cannot open the file: {0}")]
    NotFound(#[source] io::Error),
    #[error("not a regular file")]
    NotAFile,
    #[error("the file is larger than {MAX_FILE_BYTES} bytes")]
    TooLarge,
    #[error("the file is not UTF-8 text")]
    NotText,
    #[error("cannot read the file: {0}")]
    Io(#[source] io::Error),
}

impl ReadFileError {
    pub(crate) fn to_error_body(&self) -> ErrorBody {
        let code = match self {
            Self::Outside => ErrorCode::InvalidRequest,
            Self::NotFound(_) => ErrorCode::NotFound,
            Self::NotAFile | Self::TooLarge | Self::NotText | Self::Io(_) => {
                ErrorCode::FileUnreadable
            }
        };
        ErrorBody::new(code, self.to_string())
    }
}

/// Reads `path`, absolute or relative to `folder`, when it stays inside `folder` once its
/// symlinks are resolved.
pub(crate) fn read(folder: &Path, path: &Path) -> Result<String, ReadFileError> {
    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(ReadFileError::Outside);
    }
    let folder = folder.canonicalize().map_err(ReadFileError::NotFound)?;
    let resolved = folder
        .join(path)
        .canonicalize()
        .map_err(ReadFileError::NotFound)?;
    if !resolved.starts_with(&folder) {
        return Err(ReadFileError::Outside);
    }
    let file = open_to_read(&resolved).map_err(ReadFileError::NotFound)?;
    // The checks use the open file, so a swap after them does not matter.
    ensure_inside(&file, &folder)?;
    let metadata = file.metadata().map_err(ReadFileError::Io)?;
    if !metadata.is_file() {
        return Err(ReadFileError::NotAFile);
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(ReadFileError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(ReadFileError::Io)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
        return Err(ReadFileError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| ReadFileError::NotText)
}

/// Opens a file for reading without waiting: opening a FIFO waits for a writer otherwise.
pub(crate) fn open_to_read(path: &Path) -> io::Result<File> {
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    Ok(File::from(rustix::fs::open(path, flags, Mode::empty())?))
}

/// Refuses an open file outside `folder`: a folder of its path may have become a symlink since
/// the path was checked.
fn ensure_inside(file: &File, folder: &Path) -> Result<(), ReadFileError> {
    let link = Path::new(OPEN_FILES).join(file.as_raw_fd().to_string());
    let opened = std::fs::read_link(link).map_err(ReadFileError::Io)?;
    if !opened.starts_with(folder) {
        return Err(ReadFileError::Outside);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn reads_inside_the_folder_by_relative_or_absolute_path() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let relative = read(temp.path(), Path::new("src/main.rs")).unwrap();
        let absolute = read(temp.path(), &temp.path().join("src/main.rs")).unwrap();

        assert_eq!(relative, "fn main() {}\n");
        assert_eq!(absolute, relative);
    }

    #[test]
    fn refuses_to_leave_the_folder() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("agent");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(temp.path().join("secret.txt"), "secret").unwrap();
        symlink(temp.path().join("secret.txt"), folder.join("link.txt")).unwrap();

        let dotdot = read(&folder, Path::new("../secret.txt"));
        let absolute = read(&folder, &temp.path().join("secret.txt"));
        let link = read(&folder, Path::new("link.txt"));

        for result in [dotdot, absolute, link] {
            assert!(matches!(result, Err(ReadFileError::Outside)), "{result:?}");
        }
    }

    /// A folder of the path swapped for a symlink between the path check and the open leads the
    /// open outside: the open file's own path is checked again.
    #[test]
    fn an_open_file_outside_the_folder_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("agent");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("inside.txt"), "inside").unwrap();
        std::fs::write(temp.path().join("secret.txt"), "secret").unwrap();
        let folder = folder.canonicalize().unwrap();
        let inside = open_to_read(&folder.join("inside.txt")).unwrap();
        let outside = open_to_read(&temp.path().join("secret.txt")).unwrap();

        assert!(ensure_inside(&inside, &folder).is_ok());
        let refused = ensure_inside(&outside, &folder);
        assert!(
            matches!(refused, Err(ReadFileError::Outside)),
            "{refused:?}"
        );
    }

    #[test]
    fn a_fifo_is_refused_at_once() {
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("notes.txt");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RUSR).unwrap();
        let folder = temp.path().to_owned();
        let (sender, receiver) = std::sync::mpsc::channel();

        // Opening a FIFO waits for a writer: the read runs in a thread the test can leave behind.
        std::thread::spawn(move || {
            let _ = sender.send(read(&folder, Path::new("notes.txt")));
        });

        let read = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("an answer without a writer on the FIFO");
        assert!(matches!(read, Err(ReadFileError::NotAFile)), "{read:?}");
    }

    #[test]
    fn refuses_what_is_not_small_text() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("image.bin"), [0xff, 0xfe, 0x00]).unwrap();
        let large = File::create(temp.path().join("large.txt")).unwrap();
        large.set_len(MAX_FILE_BYTES + 1).unwrap();

        let binary = read(temp.path(), Path::new("image.bin"));
        let too_large = read(temp.path(), Path::new("large.txt"));
        let folder = read(temp.path(), Path::new("."));
        let missing = read(temp.path(), Path::new("missing.txt"));

        assert!(matches!(binary, Err(ReadFileError::NotText)), "{binary:?}");
        assert!(
            matches!(too_large, Err(ReadFileError::TooLarge)),
            "{too_large:?}"
        );
        assert!(matches!(folder, Err(ReadFileError::NotAFile)), "{folder:?}");
        let error = missing.unwrap_err();
        assert_eq!(error.to_error_body().code, ErrorCode::NotFound);
    }
}
