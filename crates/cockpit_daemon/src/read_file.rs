//! `agent.read_file`: a text file the agent can reach, for the file view.
//!
//! The daemon reads, once symlinks are resolved:
//!
//! - any file in the agent's folder;
//! - any file in the git checkout that holds a file the agent read or changed, such as a sibling
//!   worktree beside the agent's folder: the folder with the `.git` entry nearest above that
//!   file;
//! - a file the agent read or changed that no such checkout holds, and only that file.
//!
//! A checkout that is `/`, or that holds the home folder (a home kept in git), adds only its
//! files the agent touched: it would open everything the user owns.
//!
//! This shows the user nothing they cannot already read. The daemon and the app run as the same
//! user, and the agent's own commands read whatever that user can, so these checkouts are folders
//! the agent already worked in. The limits keep the view to the agent's work: a link in the
//! agent's text cannot open an unrelated file of the user's.

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};

use cockpit_protocol::{ErrorBody, ErrorCode};
use rustix::fs::{Mode, OFlags};

/// The largest file the view shows.
pub(crate) const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Links to the process's open files, each naming the file's path.
const OPEN_FILES: &str = "/proc/self/fd";
/// The entry that marks a git checkout's top folder: a folder, or a file in a worktree.
const GIT_ENTRY: &str = ".git";

/// The folders and files `read` may open, all with their symlinks resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Access {
    folder: PathBuf,
    /// The git checkouts holding a file the agent read or changed.
    checkouts: Vec<PathBuf>,
    /// The files the agent read or changed that no allowed checkout holds.
    files: Vec<PathBuf>,
}

impl Access {
    /// `touched` holds the absolute paths of the files the agent read or changed. `home` is the
    /// home folder, which no allowed checkout may hold.
    pub(crate) fn new(
        folder: &Path,
        touched: &[PathBuf],
        home: Option<&Path>,
    ) -> Result<Self, ReadFileError> {
        let folder = folder.canonicalize().map_err(ReadFileError::NotFound)?;
        let mut checkouts: Vec<PathBuf> = Vec::new();
        let mut files: Vec<PathBuf> = Vec::new();
        // Files of one folder share their checkout: each folder is looked up once.
        let mut checkout_of: BTreeMap<PathBuf, Option<PathBuf>> = BTreeMap::new();
        for file in touched {
            let Some(parent) = file.parent().and_then(|parent| parent.canonicalize().ok()) else {
                continue;
            };
            let checkout = checkout_of
                .entry(parent)
                .or_insert_with_key(|parent| checkout_holding(parent, home))
                .clone();
            match checkout {
                Some(checkout) => {
                    if !checkouts.contains(&checkout) {
                        checkouts.push(checkout);
                    }
                }
                None => {
                    if let Ok(file) = file.canonicalize()
                        && !files.contains(&file)
                    {
                        files.push(file);
                    }
                }
            }
        }
        Ok(Self {
            folder,
            checkouts,
            files,
        })
    }

    /// The allowed folder or file that holds `resolved`, a path without symlinks.
    fn holder(&self, resolved: &Path) -> Option<&Path> {
        let mut folders = std::iter::once(&self.folder).chain(&self.checkouts);
        let folder = folders.find(|folder| resolved.starts_with(folder));
        let file = || self.files.iter().find(|file| *file == resolved);
        folder.or_else(file).map(PathBuf::as_path)
    }
}

/// The git checkout above `folder`, a path without symlinks, unless it is `/` or holds `home`.
fn checkout_holding(folder: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let checkout = folder
        .ancestors()
        .find(|ancestor| ancestor.join(GIT_ENTRY).symlink_metadata().is_ok())?;
    let too_wide =
        checkout.parent().is_none() || home.is_some_and(|home| home.starts_with(checkout));
    (!too_wide).then(|| checkout.to_owned())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadFileError {
    #[error("the file is outside the agent's folder and the checkouts it worked in")]
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

/// Reads `path`, absolute or relative to the agent's folder, when `access` allows it once its
/// symlinks are resolved.
pub(crate) fn read(access: &Access, path: &Path) -> Result<String, ReadFileError> {
    use std::io::Read;

    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(ReadFileError::Outside);
    }
    let resolved = access
        .folder
        .join(path)
        .canonicalize()
        .map_err(ReadFileError::NotFound)?;
    let holder = access.holder(&resolved).ok_or(ReadFileError::Outside)?;
    let file = open_to_read(&resolved).map_err(ReadFileError::NotFound)?;
    // The checks use the open file, so a swap after them does not matter.
    ensure_inside(&file, holder)?;
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

/// Refuses an open file outside `holder`, a folder or the file itself: a folder of its path may
/// have become a symlink since the path was checked.
fn ensure_inside(file: &File, holder: &Path) -> Result<(), ReadFileError> {
    use std::os::fd::AsRawFd;

    let link = Path::new(OPEN_FILES).join(file.as_raw_fd().to_string());
    let opened = std::fs::read_link(link).map_err(ReadFileError::Io)?;
    if !opened.starts_with(holder) {
        return Err(ReadFileError::Outside);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::time::Duration;

    use super::*;

    fn only_folder(folder: &Path) -> Access {
        Access::new(folder, &[], None).unwrap()
    }

    fn assert_outside(result: Result<String, ReadFileError>) {
        assert!(matches!(result, Err(ReadFileError::Outside)), "{result:?}");
    }

    /// Writes `text` at `path`, creating its folders.
    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Beside the agent's folder: a worktree it worked in, one it did not, and a loose file.
    struct Layout {
        temp: tempfile::TempDir,
        agent: PathBuf,
        feature: PathBuf,
        untouched: PathBuf,
        loose: PathBuf,
    }

    impl Layout {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let agent = root.join("app");
            let feature = root.join("app_feature");
            let untouched = root.join("app_other");
            let loose = root.join("notes");
            write(&agent.join("src/main.rs"), "fn main() {}\n");
            write(
                &feature.join(GIT_ENTRY),
                "gitdir: /elsewhere/worktrees/app_feature\n",
            );
            write(&feature.join("src/app/touched.ts"), "touched\n");
            write(&feature.join("src/app/beside.ts"), "beside\n");
            std::fs::create_dir_all(untouched.join(GIT_ENTRY)).unwrap();
            write(&untouched.join("src/secret.ts"), "secret\n");
            write(&loose.join("todo.txt"), "todo\n");
            write(&loose.join("diary.txt"), "diary\n");
            Self {
                temp,
                agent,
                feature,
                untouched,
                loose,
            }
        }

        /// The temporary folder stands for the home folder, so that a checkout above it, as
        /// when the system's temporary folder sits in one, never counts.
        fn access(&self, touched: &[PathBuf]) -> Access {
            let home = self.temp.path().canonicalize().unwrap();
            Access::new(&self.agent, touched, Some(&home)).unwrap()
        }
    }

    #[test]
    fn reads_inside_the_folder_by_relative_or_absolute_path() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let access = only_folder(temp.path());

        let relative = read(&access, Path::new("src/main.rs")).unwrap();
        let absolute = read(&access, &temp.path().join("src/main.rs")).unwrap();

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

        let access = only_folder(&folder);

        let dotdot = read(&access, Path::new("../secret.txt"));
        let absolute = read(&access, &temp.path().join("secret.txt"));
        let link = read(&access, Path::new("link.txt"));

        for result in [dotdot, absolute, link] {
            assert_outside(result);
        }
    }

    #[test]
    fn reads_in_the_checkouts_of_the_files_the_agent_touched() {
        let layout = Layout::new();
        let access = layout.access(&[
            layout.feature.join("src/app/touched.ts"),
            layout.loose.join("todo.txt"),
        ]);

        let beside = read(&access, &layout.feature.join("src/app/beside.ts"));
        let todo = read(&access, &layout.loose.join("todo.txt"));

        assert_eq!(beside.unwrap(), "beside\n");
        assert_eq!(todo.unwrap(), "todo\n");
        // No checkout holds the loose file: its neighbours stay out.
        assert_outside(read(&access, &layout.loose.join("diary.txt")));
        assert_outside(read(&access, &layout.untouched.join("src/secret.ts")));
        assert_outside(read(&access, Path::new("../app_feature/src/app/beside.ts")));
    }

    #[test]
    fn a_checkout_holding_the_home_folder_adds_only_the_touched_files() {
        let layout = Layout::new();
        let home = layout.temp.path().canonicalize().unwrap();
        std::fs::create_dir(home.join(GIT_ENTRY)).unwrap();
        write(&home.join("dotfile.txt"), "dotfile\n");
        let access = Access::new(&layout.agent, &[home.join("dotfile.txt")], Some(&home)).unwrap();

        assert_eq!(
            read(&access, &home.join("dotfile.txt")).unwrap(),
            "dotfile\n"
        );
        assert_outside(read(&access, &layout.loose.join("todo.txt")));
        assert_outside(read(&access, &layout.feature.join("src/app/beside.ts")));
    }

    #[test]
    fn symlinks_are_resolved_before_the_roots_are_checked() {
        let layout = Layout::new();
        let root = layout.agent.parent().unwrap();
        symlink(&layout.feature, root.join("feature_link")).unwrap();
        symlink(
            layout.untouched.join("src/secret.ts"),
            layout.feature.join("src/escape.ts"),
        )
        .unwrap();
        symlink(
            layout.feature.join("src/app/beside.ts"),
            layout.agent.join("beside_link.ts"),
        )
        .unwrap();
        // The agent read the file through a symlinked folder.
        let access = layout.access(&[root.join("feature_link/src/app/touched.ts")]);

        let through_link = read(&access, &root.join("feature_link/src/app/beside.ts"));
        let from_the_folder = read(&access, Path::new("beside_link.ts"));

        assert_eq!(through_link.unwrap(), "beside\n");
        assert_eq!(from_the_folder.unwrap(), "beside\n");
        assert_outside(read(&access, &layout.feature.join("src/escape.ts")));
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
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, Mode::RUSR).unwrap();
        let access = only_folder(temp.path());
        let (sender, receiver) = std::sync::mpsc::channel();

        // Opening a FIFO waits for a writer: the read runs in a thread the test can leave behind.
        std::thread::spawn(move || {
            let _ = sender.send(read(&access, Path::new("notes.txt")));
        });

        let read = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("an answer without a writer on the FIFO");
        assert!(matches!(read, Err(ReadFileError::NotAFile)), "{read:?}");
    }

    #[test]
    fn refuses_what_is_not_small_text() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("image.bin"), [0xff, 0xfe, 0x00]).unwrap();
        let large = File::create(temp.path().join("large.txt")).unwrap();
        large.set_len(MAX_FILE_BYTES + 1).unwrap();

        let access = only_folder(temp.path());

        let binary = read(&access, Path::new("image.bin"));
        let too_large = read(&access, Path::new("large.txt"));
        let folder = read(&access, Path::new("."));
        let missing = read(&access, Path::new("missing.txt"));

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
