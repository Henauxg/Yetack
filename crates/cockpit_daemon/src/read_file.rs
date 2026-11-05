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
//! A relative path that names no file in the agent's folder, such as a bare file name in the
//! agent's text, is looked up in this order:
//!
//! 1. the most recent file the agent read or changed whose path ends with it;
//! 2. the files whose path ends with it in the agent's folder and the allowed checkouts, as
//!    `git ls-files` lists them, within [`MAX_MATCHES`] and [`SEARCH_TIME`]. One match is read;
//!    several are listed for the user to pick from.
//!
//! This shows the user nothing they cannot already read. The daemon and the app run as the same
//! user, and the agent's own commands read whatever that user can, so these checkouts are folders
//! the agent already worked in. The limits keep the view to the agent's work: a link in the
//! agent's text cannot open an unrelated file of the user's.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use cockpit_protocol::{ErrorBody, ErrorCode};
use rustix::fs::{Mode, OFlags};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::time::Instant;

/// The largest file the view shows.
pub(crate) const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Links to the process's open files, each naming the file's path.
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
    #[error("{} does not exist", .0.display())]
    Missing(PathBuf),
    /// A relative path that the lookup did not find either.
    #[error("{} is not in the agent's folder or in the files it read or changed", .0.display())]
    Unknown(PathBuf),
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
            Self::Missing(_) | Self::Unknown(_) | Self::NotFound(_) => ErrorCode::NotFound,
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
        .map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => ReadFileError::Missing(path.to_owned()),
            _ => ReadFileError::NotFound(error),
        })?;
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

/// What `agent.read_file` found for a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Found {
    /// `path` is where the file was read: the path asked for, joined to the agent's folder when
    /// relative, or what the lookup found for it.
    File { path: PathBuf, text: String },
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
    let roots = std::iter::once(folder).chain(access.checkouts.iter().cloned());
    let tail = name_tail(&path).unwrap_or_else(|| path.clone());
    let matches = search(roots.collect(), &tail, Instant::now() + SEARCH_TIME).await;
    match matches.as_slice() {
        [] => Err(ReadFileError::Unknown(path)),
        [only] => {
            let only = only.clone();
            let text = tokio::task::spawn_blocking({
                let only = only.clone();
                move || read(&access, &only)
            })
            .await
            .map_err(join_error)??;
            Ok(Found::File { path: only, text })
        }
        _ => Ok(Found::Several(matches)),
    }
}

fn join_error(error: tokio::task::JoinError) -> ReadFileError {
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
        Ok(text) => {
            let path = folder.join(path);
            return Ok(Some(Found::File { path, text }));
        }
        Err(ReadFileError::Missing(missing)) => match name_tail(path) {
            Some(tail) => tail,
            None => return Err(ReadFileError::Missing(missing)),
        },
        Err(error) => return Err(error),
    };
    for candidate in touched.iter().filter(|file| file.ends_with(&tail)) {
        // A file the agent touched may be gone since.
        if let Ok(text) = read(access, candidate) {
            let path = candidate.clone();
            return Ok(Some(Found::File { path, text }));
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

/// The files under `roots` whose path ends with `path`, as `git ls-files` lists them, each once,
/// until `deadline` or [`MAX_MATCHES`]. A root that is not in a git checkout adds nothing.
async fn search(roots: Vec<PathBuf>, path: &Path, deadline: Instant) -> Vec<PathBuf> {
    let mut matches: Vec<PathBuf> = Vec::new();
    // The agent's folder may sit in one of the checkouts: a file found twice is listed once.
    let mut seen: Vec<PathBuf> = Vec::new();
    for root in roots {
        let Ok(mut child) = Command::new(GIT_PROGRAM)
            .arg(GIT_FOLDER_OPTION)
            .arg(&root)
            .args(LIST_FILES)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        else {
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
            let relative = Path::new(OsStr::from_bytes(name));
            if !relative.ends_with(path) {
                continue;
            }
            let found = root.join(relative);
            let canonical = found.canonicalize().unwrap_or_else(|_| found.clone());
            if seen.contains(&canonical) {
                continue;
            }
            seen.push(canonical);
            matches.push(found);
            if matches.len() >= MAX_MATCHES {
                return matches;
            }
        }
    }
    matches
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
        let home = layout.temp.path().canonicalize().unwrap();
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
            text: text.to_owned(),
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
        assert_outside(dots.map(|_| String::new()));
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
