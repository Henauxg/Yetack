//! `agent.read_file`: a text file or an image the agent can reach, for the file view. An image is
//! a file whose extension names a type Claude reads, such as `.png`.
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
//! A relative path that names no file in the agent's folder, such as a bare file name in the
//! agent's text, is looked up in this order:
//!
//! 1. the most recent file the agent read or changed whose path ends with it;
//! 2. the files whose path ends with it in the agent's folder and the allowed checkouts, as
//!    `git ls-files` lists them, within [`MAX_MATCHES`] and [`SEARCH_TIME`]. One match is read;
//!    several are listed for the user to pick from.
//!
//! On Windows, names compare whatever their case, and a path on a network share opens only when
//! the agent's folder or one of the allowed checkouts or files is on that share (module
//! `windows_path`).
//!
//! This shows the user nothing they cannot already read. The daemon and the app run as the same
//! user, and the agent's own commands read whatever that user can, so these checkouts are folders
//! the agent already worked in. The limits keep the view to the agent's work: a link in the
//! agent's text cannot open an unrelated file of the user's.

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use cockpit_protocol::{ErrorBody, ErrorCode, Image, ImageMediaType};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinError;
use tokio::time::Instant;

#[cfg(windows)]
use crate::process::program;
#[cfg(windows)]
use crate::{windows_files, windows_path};

/// The largest text file the view shows.
pub(crate) const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// The largest image the view shows. In base64 it stays well under a frame's limit.
pub(crate) const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
/// Links to the process's open files, each naming the file's path.
#[cfg(unix)]
const OPEN_FILES: &str = "/proc/self/fd";
/// The entry that marks a git checkout's top folder: a folder, or a file in a worktree.
const GIT_ENTRY: &str = ".git";

/// The most files a search lists.
pub(crate) const MAX_MATCHES: usize = 20;
/// How long a search may run, all checkouts together.
pub(crate) const SEARCH_TIME: Duration = Duration::from_secs(3);
const GIT_PROGRAM: &str = "git";
/// Lists the files of the checkout under the current folder, tracked or not, without the ignored
/// ones, each ended by a NUL byte.
const LIST_FILES: [&str; 5] = [
    "ls-files",
    "-z",
    "--cached",
    "--others",
    "--exclude-standard",
];
const GIT_FOLDER_OPTION: &str = "-C";
/// Keeps git from running the file system monitor, a command the agent can set in its checkout's
/// git config.
const NO_MONITOR: [&str; 2] = ["-c", "core.fsmonitor=false"];

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
        let folder = canonical(folder).map_err(ReadFileError::NotFound)?;
        // Compared with checkouts whose symlinks are resolved.
        let home: Option<PathBuf> =
            home.map(|home| canonical(home).unwrap_or_else(|_| home.to_owned()));
        let mut checkouts: Vec<PathBuf> = Vec::new();
        let mut files: Vec<PathBuf> = Vec::new();
        // Files of one folder share their checkout: each folder is looked up once.
        let mut checkout_of: BTreeMap<PathBuf, Option<PathBuf>> = BTreeMap::new();
        for file in touched {
            let Some(parent) = file.parent().and_then(|parent| canonical(parent).ok()) else {
                continue;
            };
            let checkout = checkout_of
                .entry(parent)
                .or_insert_with_key(|parent| checkout_holding(parent, home.as_deref()))
                .clone();
            match checkout {
                Some(checkout) => {
                    if !checkouts.contains(&checkout) {
                        checkouts.push(checkout);
                    }
                }
                None => {
                    if let Ok(file) = canonical(file)
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
        let file = || self.files.iter().find(|file| is_same(resolved, file));
        self.folder_holding(resolved)
            .or_else(|| file().map(PathBuf::as_path))
    }

    /// The agent's folder or the allowed checkout that holds `resolved`, a path without symlinks.
    /// The files allowed alone hold nothing.
    pub(crate) fn folder_holding(&self, resolved: &Path) -> Option<&Path> {
        let mut folders = std::iter::once(&self.folder).chain(&self.checkouts);
        folders
            .find(|folder| is_inside(resolved, folder))
            .map(PathBuf::as_path)
    }

    /// Refuses a path the file system must not open even to resolve it: a device path, a name
    /// such as `NUL`, or a network share that no allowed folder or file is on.
    #[cfg(windows)]
    pub(crate) fn check_before_open(&self, asked: &Path) -> Result<(), ReadFileError> {
        let text = asked.to_str().ok_or(ReadFileError::Outside)?;
        let allowed = std::iter::once(&self.folder)
            .chain(&self.checkouts)
            .chain(&self.files)
            .filter_map(|path| path.to_str());
        if !windows_path::may_open(text, allowed) {
            return Err(ReadFileError::Outside);
        }
        if windows_path::names_a_device(text) {
            return Err(ReadFileError::NotAFile);
        }
        Ok(())
    }

    /// The folders a search lists: `folder`, the agent's folder as its record names it, then the
    /// checkouts other than that folder, so that no checkout is listed twice.
    pub(crate) fn search_roots(&self, folder: PathBuf) -> Vec<PathBuf> {
        let others = self
            .checkouts
            .iter()
            .filter(|checkout| **checkout != self.folder)
            .cloned();
        std::iter::once(folder).chain(others).collect()
    }
}

/// The git checkout above `folder`, a path without symlinks, unless it is `/` or holds `home`.
fn checkout_holding(folder: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let checkout = folder
        .ancestors()
        .find(|ancestor| ancestor.join(GIT_ENTRY).symlink_metadata().is_ok())?;
    let too_wide =
        checkout.parent().is_none() || home.is_some_and(|home| is_inside(home, checkout));
    (!too_wide).then(|| checkout.to_owned())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadFileError {
    #[error("the file is outside the agent's folder and the checkouts it worked in")]
    Outside,
    #[error("{} does not exist", .0.display())]
    Missing(PathBuf),
    /// A relative path that the lookup did not find either.
    #[error("{} is not in the agent's folder or in the files it read or changed", .0.display())]
    Unknown(PathBuf),
    #[error("cannot open the file: {0}")]
    NotFound(#[source] io::Error),
    #[error("not a regular file")]
    NotAFile,
    #[error("the file is larger than {0} bytes")]
    TooLarge(u64),
    #[error("the file is not UTF-8 text")]
    NotText,
    #[error("cannot read the file: {0}")]
    Io(#[source] io::Error),
}

impl ReadFileError {
    pub(crate) fn to_error_body(&self) -> ErrorBody {
        let code = match self {
            Self::Outside => ErrorCode::InvalidRequest,
            Self::Missing(_) | Self::Unknown(_) | Self::NotFound(_) => ErrorCode::NotFound,
            Self::NotAFile | Self::TooLarge(_) | Self::NotText | Self::Io(_) => {
                ErrorCode::FileUnreadable
            }
        };
        ErrorBody::new(code, self.to_string())
    }
}

/// What the file view shows of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Content {
    Text(String),
    Image(Image),
}

/// Reads `path`, absolute or relative to the agent's folder, when `access` allows it once its
/// symlinks are resolved.
pub(crate) fn read(access: &Access, path: &Path) -> Result<Content, ReadFileError> {
    use std::io::Read;

    use base64::Engine;

    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(ReadFileError::Outside);
    }
    let asked = access.folder.join(path);
    #[cfg(windows)]
    access.check_before_open(&asked)?;
    let resolved = canonical(&asked).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => ReadFileError::Missing(path.to_owned()),
        _ => ReadFileError::NotFound(error),
    })?;
    // A link may lead to a file named `AUX`, which the path opens as the device.
    #[cfg(windows)]
    access.check_before_open(&resolved)?;
    let holder = access.holder(&resolved).ok_or(ReadFileError::Outside)?;
    let file = open_to_read(&resolved).map_err(ReadFileError::NotFound)?;
    // The checks use the open file, so a swap after them does not matter.
    ensure_inside(&file, holder)?;
    let metadata = file.metadata().map_err(ReadFileError::Io)?;
    if !metadata.is_file() {
        return Err(ReadFileError::NotAFile);
    }
    let media_type = ImageMediaType::from_file_name(&resolved.to_string_lossy());
    let limit = if media_type.is_some() {
        MAX_IMAGE_BYTES
    } else {
        MAX_FILE_BYTES
    };
    if metadata.len() > limit {
        return Err(ReadFileError::TooLarge(limit));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(ReadFileError::Io)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(ReadFileError::TooLarge(limit));
    }
    match media_type {
        Some(media_type) => Ok(Content::Image(Image {
            media_type,
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        })),
        None => String::from_utf8(bytes)
            .map(Content::Text)
            .map_err(|_| ReadFileError::NotText),
    }
}

/// What `agent.read_file` found for a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Found {
    /// `path` is where the file was read: the path asked for, joined to the agent's folder when
    /// relative, or what the lookup found for it.
    File { path: PathBuf, content: Content },
    /// Several files match the path, and none was read.
    Several(Vec<PathBuf>),
}

/// Reads `path`, absolute or relative to `folder`, the agent's folder, or looks a relative path
/// up as the module says. `touched` holds the files the agent read or changed, most recent first.
pub(crate) async fn find_and_read(
    folder: PathBuf,
    touched: Vec<PathBuf>,
    home: Option<PathBuf>,
    path: PathBuf,
) -> Result<Found, ReadFileError> {
    let task = {
        let folder = folder.clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            let access = Access::new(&folder, &touched, home.as_deref())?;
            let found = read_or_match_touched(&access, &folder, &touched, &path)?;
            Ok::<_, ReadFileError>((access, found))
        })
    };
    let (access, found) = task.await.map_err(join_error)??;
    if let Some(found) = found {
        return Ok(found);
    }
    let roots = access.search_roots(folder);
    let tail = name_tail(&path).unwrap_or_else(|| path.clone());
    let ends_with_tail = |relative: &Path| relative.ends_with(&tail);
    let matches = search(&access, roots, ends_with_tail, Instant::now() + SEARCH_TIME).await;
    match matches.as_slice() {
        [] => Err(ReadFileError::Unknown(path)),
        [only] => {
            let only = only.clone();
            let content = tokio::task::spawn_blocking({
                let only = only.clone();
                move || read(&access, &only)
            })
            .await
            .map_err(join_error)??;
            Ok(Found::File {
                path: only,
                content,
            })
        }
        _ => Ok(Found::Several(matches)),
    }
}

pub(crate) fn join_error(error: JoinError) -> ReadFileError {
    ReadFileError::Io(io::Error::other(error))
}

/// The first two steps: `path` itself, then for a name that is not in `folder`, the most recent
/// file the agent touched whose path ends with it. `None` when a search is needed.
fn read_or_match_touched(
    access: &Access,
    folder: &Path,
    touched: &[PathBuf],
    path: &Path,
) -> Result<Option<Found>, ReadFileError> {
    let tail = match read(access, path) {
        Ok(content) => {
            let path = folder.join(path);
            return Ok(Some(Found::File { path, content }));
        }
        Err(ReadFileError::Missing(missing)) => match name_tail(path) {
            Some(tail) => tail,
            None => return Err(ReadFileError::Missing(missing)),
        },
        Err(error) => return Err(error),
    };
    for candidate in touched.iter().filter(|file| file.ends_with(&tail)) {
        // A file the agent touched may be gone since.
        if let Ok(content) = read(access, candidate) {
            let path = candidate.clone();
            return Ok(Some(Found::File { path, content }));
        }
    }
    Ok(None)
}

/// The names of a relative path, without its `.`, which the lookup matches against the end of a
/// path. `None` for an absolute path, or one with `..`.
fn name_tail(path: &Path) -> Option<PathBuf> {
    let mut tail = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => tail.push(name),
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => return None,
        }
    }
    (!tail.as_os_str().is_empty()).then_some(tail)
}

/// The files under `roots` whose path relative to their root passes `matches_path`, as `git
/// ls-files` lists them, each once, until `deadline` or [`MAX_MATCHES`]. A root that is not in a
/// git checkout adds nothing, and neither does a file `access` does not allow once its symlinks
/// are resolved.
pub(crate) async fn search(
    access: &Access,
    roots: Vec<PathBuf>,
    matches_path: impl Fn(&Path) -> bool,
    deadline: Instant,
) -> Vec<PathBuf> {
    let mut matches: Vec<PathBuf> = Vec::new();
    // The agent's folder may sit in one of the checkouts: a file found twice is listed once.
    let mut seen: Vec<PathBuf> = Vec::new();
    for root in roots {
        let mut git = Command::new(GIT_PROGRAM);
        git.args(NO_MONITOR)
            .arg(GIT_FOLDER_OPTION)
            .arg(&root)
            .args(LIST_FILES)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        program::hide_console(&mut git);
        let Ok(mut child) = git.spawn() else {
            continue;
        };
        let Some(stdout) = child.stdout.take() else {
            continue;
        };
        let mut lines = BufReader::new(stdout);
        let mut entry: Vec<u8> = Vec::new();
        loop {
            entry.clear();
            match tokio::time::timeout_at(deadline, lines.read_until(0, &mut entry)).await {
                Ok(Ok(0) | Err(_)) => break,
                Err(_) => return matches,
                Ok(Ok(_)) => {}
            }
            let name = entry.strip_suffix(&[0]).unwrap_or(&entry);
            let Some(relative) = git_path(name) else {
                continue;
            };
            if !matches_path(&relative) {
                continue;
            }
            let found = root.join(relative);
            #[cfg(windows)]
            if access.check_before_open(&found).is_err() {
                continue;
            }
            let Ok(resolved) = canonical(&found) else {
                continue;
            };
            if access.holder(&resolved).is_none() || seen.contains(&resolved) {
                continue;
            }
            seen.push(resolved);
            matches.push(found);
            if matches.len() >= MAX_MATCHES {
                return matches;
            }
        }
    }
    matches
}

/// A path as `git ls-files -z` writes it, relative to the checkout. `None` for an absolute path or
/// one with `..`, which the checkout's index file can hold when the agent wrote it: on Windows,
/// `//host/share/file` would make the search connect to that host.
fn git_path(bytes: &[u8]) -> Option<PathBuf> {
    let path = written_path(bytes)?;
    path.components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
        .then_some(path)
}

#[cfg(unix)]
fn written_path(bytes: &[u8]) -> Option<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    Some(PathBuf::from(OsStr::from_bytes(bytes)))
}

/// Git for Windows writes UTF-8, with `/` between names, which the path gets as `\`.
#[cfg(windows)]
fn written_path(bytes: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(bytes).ok()?;
    Some(Path::new(text).components().collect())
}

/// `path` with its symlinks resolved.
#[cfg(unix)]
pub(crate) fn canonical(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize()
}

/// `path` with its links resolved, written as a user writes it: `C:\work` rather than the
/// verbatim `\\?\C:\work`, so that it shows as such and a relative path joins it.
#[cfg(windows)]
pub(crate) fn canonical(path: &Path) -> io::Result<PathBuf> {
    Ok(windows_path::shown_path(&path.canonicalize()?))
}

/// Whether `path` is `folder` or inside it.
#[cfg(unix)]
fn is_inside(path: &Path, folder: &Path) -> bool {
    path.starts_with(folder)
}

/// Whether `path` is `folder` or inside it, whatever the case of their names.
#[cfg(windows)]
fn is_inside(path: &Path, folder: &Path) -> bool {
    match (path.to_str(), folder.to_str()) {
        (Some(path), Some(folder)) => windows_path::is_inside(path, folder),
        _ => false,
    }
}

fn is_same(left: &Path, right: &Path) -> bool {
    is_inside(left, right) && is_inside(right, left)
}

/// Opens a file for reading without waiting: opening a FIFO waits for a writer otherwise.
#[cfg(unix)]
pub(crate) fn open_to_read(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    Ok(File::from(rustix::fs::open(path, flags, Mode::empty())?))
}

/// Opens a file or a folder for reading. [`Access::check_before_open`] refuses a device first.
#[cfg(windows)]
pub(crate) fn open_to_read(path: &Path) -> io::Result<File> {
    windows_files::open(path)
}

/// Refuses an open file or folder outside `holder`, a folder or the file itself: a folder of its
/// path may have become a symlink since the path was checked.
#[cfg(unix)]
pub(crate) fn ensure_inside(file: &File, holder: &Path) -> Result<(), ReadFileError> {
    use std::os::fd::AsRawFd;

    let link = Path::new(OPEN_FILES).join(file.as_raw_fd().to_string());
    let opened = std::fs::read_link(link).map_err(ReadFileError::Io)?;
    if !opened.starts_with(holder) {
        return Err(ReadFileError::Outside);
    }
    Ok(())
}

/// Refuses an open file or folder outside `holder`, a folder or the file itself: a folder of its
/// path may have become a link since the path was checked.
#[cfg(windows)]
pub(crate) fn ensure_inside(file: &File, holder: &Path) -> Result<(), ReadFileError> {
    let opened = windows_files::final_path(file).map_err(ReadFileError::Io)?;
    if !is_inside(&opened, holder) {
        return Err(ReadFileError::Outside);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(unix)]
    use std::time::Duration;

    #[cfg(unix)]
    use rustix::fs::Mode;

    use super::*;

    fn only_folder(folder: &Path) -> Access {
        Access::new(folder, &[], None).unwrap()
    }

    fn text(text: &str) -> Content {
        Content::Text(text.to_owned())
    }

    fn assert_outside(result: Result<Content, ReadFileError>) {
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
            let root = canonical(temp.path()).unwrap();
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
            let home = canonical(self.temp.path()).unwrap();
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

        assert_eq!(relative, text("fn main() {}\n"));
        assert_eq!(absolute, relative);
    }

    #[cfg(unix)]
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

        assert_eq!(beside.unwrap(), text("beside\n"));
        assert_eq!(todo.unwrap(), text("todo\n"));
        // No checkout holds the loose file: its neighbours stay out.
        assert_outside(read(&access, &layout.loose.join("diary.txt")));
        assert_outside(read(&access, &layout.untouched.join("src/secret.ts")));
        assert_outside(read(&access, Path::new("../app_feature/src/app/beside.ts")));
    }

    fn git_init(folder: &Path) {
        std::fs::create_dir_all(folder).unwrap();
        let status = std::process::Command::new(GIT_PROGRAM)
            .args(["init", "-q"])
            .current_dir(folder)
            .status()
            .expect("the search tests need git");
        assert!(status.success());
    }

    async fn find(
        layout: &Layout,
        touched: &[PathBuf],
        path: &str,
    ) -> Result<Found, ReadFileError> {
        let home = canonical(layout.temp.path()).unwrap();
        find_and_read(
            layout.agent.clone(),
            touched.to_vec(),
            Some(home),
            PathBuf::from(path),
        )
        .await
    }

    fn file(path: PathBuf, text: &str) -> Found {
        Found::File {
            path,
            content: Content::Text(text.to_owned()),
        }
    }

    #[tokio::test]
    async fn a_name_opens_the_most_recent_touched_file_that_ends_with_it() {
        let layout = Layout::new();
        let beside = layout.feature.join("src/app/beside.ts");
        let touched = layout.feature.join("src/app/touched.ts");
        write(&layout.agent.join("old/beside.ts"), "old\n");
        let files = [
            beside.clone(),
            layout.agent.join("old/beside.ts"),
            touched.clone(),
        ];

        let in_the_folder = find(&layout, &files, "src/main.rs").await.unwrap();
        let by_name = find(&layout, &files, "beside.ts").await.unwrap();
        let by_tail = find(&layout, &files, "./app/touched.ts").await.unwrap();

        assert_eq!(
            in_the_folder,
            file(layout.agent.join("src/main.rs"), "fn main() {}\n")
        );
        assert_eq!(by_name, file(beside, "beside\n"));
        assert_eq!(by_tail, file(touched, "touched\n"));
    }

    #[tokio::test]
    async fn a_name_nobody_touched_is_searched_for_in_the_checkouts() {
        let layout = Layout::new();
        let sibling = layout.agent.parent().unwrap().join("app_review");
        git_init(&layout.agent);
        git_init(&sibling);
        write(&layout.agent.join(".gitignore"), "build/\n");
        write(&layout.agent.join("src/deep/parcel.ts"), "parcel\n");
        write(&layout.agent.join("build/report.ts"), "built\n");
        write(&layout.agent.join("src/a/widget.ts"), "first\n");
        write(&sibling.join("src/b/widget.ts"), "second\n");
        write(&sibling.join("README.md"), "readme\n");
        let touched = [sibling.join("README.md")];

        let one = find(&layout, &touched, "parcel.ts").await.unwrap();
        let several = find(&layout, &touched, "widget.ts").await.unwrap();
        let ignored = find(&layout, &touched, "report.ts").await;
        let none = find(&layout, &touched, "register-parcel.component.ts").await;

        assert_eq!(
            one,
            file(layout.agent.join("src/deep/parcel.ts"), "parcel\n")
        );
        assert_eq!(
            several,
            Found::Several(vec![
                layout.agent.join("src/a/widget.ts"),
                sibling.join("src/b/widget.ts"),
            ])
        );
        assert!(
            matches!(ignored, Err(ReadFileError::Unknown(_))),
            "{ignored:?}"
        );
        let message = none.unwrap_err().to_error_body();
        assert_eq!(message.code, ErrorCode::NotFound);
        assert_eq!(
            message.message,
            "register-parcel.component.ts is not in the agent's folder or in the files it read or changed"
        );
    }

    /// A symlink out of the allowed folders, or to nothing, is no file the user can open.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_search_lists_only_files_the_daemon_reads() {
        let layout = Layout::new();
        git_init(&layout.agent);
        write(&layout.agent.join("src/a/widget.ts"), "widget\n");
        std::fs::create_dir_all(layout.agent.join("src/b")).unwrap();
        std::fs::create_dir_all(layout.agent.join("src/c")).unwrap();
        symlink(
            layout.untouched.join("src/secret.ts"),
            layout.agent.join("src/b/widget.ts"),
        )
        .unwrap();
        symlink(
            layout.agent.join("src/gone.ts"),
            layout.agent.join("src/c/widget.ts"),
        )
        .unwrap();

        let found = find(&layout, &[], "widget.ts").await.unwrap();

        assert_eq!(
            found,
            file(layout.agent.join("src/a/widget.ts"), "widget\n")
        );
    }

    /// The agent can write its checkout's git config: listing the files must not run a command
    /// it names.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_search_runs_no_command_from_the_checkouts_git_config() {
        let layout = Layout::new();
        let marker = layout.loose.join("monitor-ran");
        git_init(&layout.agent);
        write(&layout.agent.join("src/deep/parcel.ts"), "parcel\n");
        let monitor = format!("touch '{}'; false", marker.display());
        let git = |args: &[&str]| {
            let status = std::process::Command::new(GIT_PROGRAM)
                .arg(GIT_FOLDER_OPTION)
                .arg(&layout.agent)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["add", "."]);
        git(&["config", "core.fsmonitor", &monitor]);

        let found = find(&layout, &[], "parcel.ts").await.unwrap();

        assert_eq!(
            found,
            file(layout.agent.join("src/deep/parcel.ts"), "parcel\n")
        );
        assert!(!marker.exists(), "the search ran core.fsmonitor");
    }

    #[tokio::test]
    async fn a_missing_absolute_path_or_a_path_with_dots_is_not_looked_up() {
        let layout = Layout::new();
        let missing = layout.feature.join("src/gone.ts");
        let touched = [layout.feature.join("src/app/touched.ts")];

        let absolute = find(&layout, &touched, missing.to_str().unwrap()).await;
        let dots = find(&layout, &touched, "../app_feature/src/app/touched.ts").await;

        let message = absolute.unwrap_err().to_error_body();
        assert_eq!(message.code, ErrorCode::NotFound);
        assert_eq!(
            message.message,
            format!("{} does not exist", missing.display())
        );
        assert_outside(dots.map(|_| Content::Text(String::new())));
    }

    #[test]
    fn a_git_path_outside_its_checkout_is_left_out() {
        assert_eq!(
            git_path(b"src/main.rs"),
            Some(Path::new("src").join("main.rs"))
        );
        for outside in [
            "/etc/passwd",
            "../app_other/src/secret.ts",
            "src/../../secret.ts",
        ] {
            assert_eq!(git_path(outside.as_bytes()), None, "{outside}");
        }
        #[cfg(windows)]
        for outside in [
            "//attacker.invalid/share/notes.txt",
            r"\\attacker.invalid\share\notes.txt",
            "C:/Windows/win.ini",
            "C:notes.txt",
            r"\Windows\win.ini",
        ] {
            assert_eq!(git_path(outside.as_bytes()), None, "{outside}");
        }
    }

    /// Listing a large checkout takes much of the search's time.
    #[test]
    fn a_search_lists_the_agents_own_checkout_once() {
        let layout = Layout::new();
        std::fs::create_dir(layout.agent.join(GIT_ENTRY)).unwrap();
        let access = layout.access(&[
            layout.agent.join("src/main.rs"),
            layout.feature.join("src/app/touched.ts"),
        ]);

        let roots = access.search_roots(layout.agent.clone());

        assert_eq!(roots, [layout.agent.clone(), layout.feature.clone()]);
    }

    #[test]
    fn a_checkout_holding_the_home_folder_adds_only_the_touched_files() {
        let layout = Layout::new();
        let home = canonical(layout.temp.path()).unwrap();
        std::fs::create_dir(home.join(GIT_ENTRY)).unwrap();
        write(&home.join("dotfile.txt"), "dotfile\n");
        let access = Access::new(&layout.agent, &[home.join("dotfile.txt")], Some(&home)).unwrap();

        assert_eq!(
            read(&access, &home.join("dotfile.txt")).unwrap(),
            text("dotfile\n")
        );
        assert_outside(read(&access, &layout.loose.join("todo.txt")));
        assert_outside(read(&access, &layout.feature.join("src/app/beside.ts")));
    }

    /// Some systems keep the home folders behind a symlink, such as `/home` to `/var/home`.
    #[cfg(unix)]
    #[test]
    fn a_home_folder_reached_through_a_symlink_is_still_the_home_folder() {
        let layout = Layout::new();
        let home = canonical(layout.temp.path()).unwrap();
        std::fs::create_dir(home.join(GIT_ENTRY)).unwrap();
        write(&home.join("dotfile.txt"), "dotfile\n");
        let links = tempfile::tempdir().unwrap();
        let linked_home = links.path().join("home");
        symlink(&home, &linked_home).unwrap();

        let access = Access::new(
            &layout.agent,
            &[home.join("dotfile.txt")],
            Some(&linked_home),
        )
        .unwrap();

        assert_eq!(
            read(&access, &home.join("dotfile.txt")).unwrap(),
            text("dotfile\n")
        );
        assert_outside(read(&access, &layout.loose.join("todo.txt")));
    }

    #[cfg(unix)]
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

        assert_eq!(through_link.unwrap(), text("beside\n"));
        assert_eq!(from_the_folder.unwrap(), text("beside\n"));
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
        let folder = canonical(&folder).unwrap();
        let inside = open_to_read(&folder.join("inside.txt")).unwrap();
        let outside = open_to_read(&temp.path().join("secret.txt")).unwrap();

        assert!(ensure_inside(&inside, &folder).is_ok());
        let refused = ensure_inside(&outside, &folder);
        assert!(
            matches!(refused, Err(ReadFileError::Outside)),
            "{refused:?}"
        );
    }

    #[cfg(unix)]
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
    fn an_image_reads_as_base64_with_its_type_up_to_its_own_limit() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("shot.PNG"), [0x89, b'P', b'N', b'G']).unwrap();
        let large = File::create(temp.path().join("large.jpg")).unwrap();
        large.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        let access = only_folder(temp.path());

        let shot = read(&access, Path::new("shot.PNG"));
        let too_large = read(&access, Path::new("large.jpg"));

        assert_eq!(
            shot.unwrap(),
            Content::Image(Image {
                media_type: ImageMediaType::Png,
                data: "iVBORw==".to_owned(),
            })
        );
        assert!(
            matches!(too_large, Err(ReadFileError::TooLarge(MAX_IMAGE_BYTES))),
            "{too_large:?}"
        );
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
            matches!(too_large, Err(ReadFileError::TooLarge(MAX_FILE_BYTES))),
            "{too_large:?}"
        );
        assert!(matches!(folder, Err(ReadFileError::NotAFile)), "{folder:?}");
        let error = missing.unwrap_err();
        assert_eq!(error.to_error_body().code, ErrorCode::NotFound);
    }

    #[cfg(windows)]
    #[test]
    fn names_compare_whatever_their_case_and_slashes_join_names() {
        let temp = tempfile::tempdir().unwrap();
        write(&temp.path().join("Src/Main.rs"), "fn main() {}\n");
        let folder = canonical(temp.path()).unwrap();
        let upper = PathBuf::from(folder.to_str().unwrap().to_uppercase());
        let access = only_folder(&folder);

        let slashes = read(&access, Path::new("src/main.rs"));
        let shouting = read(&access, &upper.join(r"SRC\MAIN.RS"));
        let from_upper = read(&only_folder(&upper), Path::new(r"src\main.rs"));

        assert_eq!(slashes.unwrap(), text("fn main() {}\n"));
        assert_eq!(shouting.unwrap(), text("fn main() {}\n"));
        assert_eq!(from_upper.unwrap(), text("fn main() {}\n"));
    }

    /// Such a path must not reach the file system: a share would make Windows connect to its
    /// host, and a device may wait for input.
    #[cfg(windows)]
    #[test]
    fn a_device_or_a_share_no_allowed_folder_is_on_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let access = only_folder(temp.path());

        for path in [
            r"\\attacker.invalid\share\notes.txt",
            "//attacker.invalid/share/notes.txt",
            r"\\?\UNC\attacker.invalid\share\notes.txt",
            r"\\.\pipe\agents-cockpit",
            r"\\.\C:\notes.txt",
            r"\\?\GLOBALROOT\Device\Null",
        ] {
            assert_outside(read(&access, Path::new(path)));
        }
        for device in ["NUL", "con.txt", r"sub\COM1", "conin$"] {
            let refused = read(&access, Path::new(device));
            assert!(
                matches!(refused, Err(ReadFileError::NotAFile)),
                "{device}: {refused:?}"
            );
        }
    }

    /// Only a verbatim path makes a file named `AUX`. Its plain path opens the device.
    #[cfg(windows)]
    #[test]
    fn a_link_to_a_file_with_a_device_name_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let folder = canonical(temp.path()).unwrap();
        let device = PathBuf::from(format!(r"\\?\{}\aux", folder.display()));
        std::fs::write(&device, "a file").unwrap();
        std::os::windows::fs::symlink_file(&device, folder.join("notes.txt")).unwrap();
        if std::fs::symlink_metadata(folder.join("notes.txt")).is_err() {
            // Wine makes the link but cannot read it.
            eprintln!("skipped: the symbolic link cannot be read back");
            return;
        }
        let access = only_folder(&folder);

        let read = read(&access, Path::new("notes.txt"));

        assert!(matches!(read, Err(ReadFileError::NotAFile)), "{read:?}");
    }

    #[cfg(windows)]
    #[test]
    fn a_file_outside_the_folder_is_refused_whatever_the_case() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("agent");
        write(&folder.join("inside.txt"), "inside");
        write(
            &temp.path().join("agent_other").join("secret.txt"),
            "secret",
        );
        let access = only_folder(&folder);

        let sibling = read(&access, &temp.path().join(r"AGENT_OTHER\secret.txt"));
        let drive_root = read(&access, Path::new(r"C:\windows\win.ini"));

        assert_outside(sibling);
        assert_outside(drive_root);
        assert_eq!(
            read(&access, Path::new("INSIDE.TXT")).unwrap(),
            text("inside")
        );
    }
}
