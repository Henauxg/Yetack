//! `agent.read_file`: a text file, an image or a PDF, for the file view. An image is a file whose
//! extension names a type Claude reads, such as `.png`, and a PDF one whose extension is `.pdf`.
//!
//! The daemon reads any regular file its user can read, as an IDE opens any file: an absolute
//! path, or one relative to the agent's folder. The daemon and the app run as the same user, and
//! the agent's own commands read whatever that user can, so this shows the user nothing they
//! cannot already read. A path that goes up with `..` is refused.
//!
//! A relative path that names no file in the agent's folder, such as a bare file name in the
//! agent's text, is looked up in this order:
//!
//! 1. the most recent file the agent read or changed whose path ends with it;
//! 2. the files whose path ends with it in the agent's folder and the git checkouts it worked in,
//!    as `git ls-files` lists them, within [`MAX_MATCHES`] and [`SEARCH_TIME`]. One match is
//!    read; several are listed for the user to pick from. A checkout the agent worked in is the
//!    folder with the `.git` entry nearest above a file it read or changed, unless that folder is
//!    `/` or holds the home folder, which would search everything the user owns.
//! 3. the files git ignores in those folders, such as a `.db/` folder of scripts: at the top of
//!    each ignored folder and one folder down, and the ignored files outside those folders. A
//!    match is marked as ignored.
//!
//! When all three find nothing, the user may ask for the search of all the ignored files, within
//! the time [`IgnoredSearch::full_search_time`] gives. The answer then says how many files it went
//! through, and whether it ran out of time.
//!
//! On Windows, names compare whatever their case. A device path or name, such as `\\.\pipe` or
//! `NUL`, is refused. So is a path on a network share that neither the agent's folder nor a
//! checkout or file it worked on is on: opening it would make Windows connect to that host and
//! send it a hash of the user's login (module `windows_path`).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use cockpit_protocol::{
    Base64, ErrorBody, ErrorCode, FileEdit, FoundFile, IgnoredSearch, IgnoredSearched, Image,
    ImageMediaType, Timestamp, is_pdf_name,
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::time::Instant;

use crate::blocking::{self, BlockingError, Scope};
#[cfg(windows)]
use crate::process::program;
#[cfg(windows)]
use crate::{windows_files, windows_path};

/// The largest text file the view shows.
pub(crate) const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// The largest image the view shows. In base64 it stays well under a frame's limit.
pub(crate) const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
/// The largest PDF the app opens. In base64 it stays well under a frame's limit.
pub(crate) const MAX_PDF_BYTES: u64 = 8 * 1024 * 1024;
/// The entry that marks a git checkout's top folder: a folder, or a file in a worktree.
pub(crate) const GIT_ENTRY: &str = ".git";

/// The most files a search lists.
pub(crate) const MAX_MATCHES: usize = 20;
/// How long a search may run, all checkouts together.
pub(crate) const SEARCH_TIME: Duration = Duration::from_secs(3);
/// How long finding and reading a file may take, its search apart.
const READ_TIME: Duration = Duration::from_secs(10);
pub(crate) const GIT_PROGRAM: &str = "git";
/// How long the quick look may spend in the ignored folders, after their listing.
const NEAR_SCAN_TIME: Duration = Duration::from_secs(1);
/// How much longer the wait for that look lasts, so that it returns what it found by then.
const NEAR_SCAN_GRACE: Duration = Duration::from_millis(500);
/// The most entries the quick look reads in one ignored folder.
const MAX_NEAR_ENTRIES: usize = 10_000;
const LS_FILES: &str = "ls-files";
const CACHED_OPTION: &str = "--cached";
const OTHERS_OPTION: &str = "--others";
const IGNORED_OPTION: &str = "--ignored";
const EXCLUDE_STANDARD_OPTION: &str = "--exclude-standard";
const DIRECTORY_OPTION: &str = "--directory";
/// Lists the files of the checkout under the current folder, tracked or not, without the ignored
/// ones, each ended by a NUL byte.
const LIST_FILES: [&str; 5] = [
    LS_FILES,
    NUL_ENDED,
    CACHED_OPTION,
    OTHERS_OPTION,
    EXCLUDE_STANDARD_OPTION,
];
/// Lists the files the checkout ignores.
const LIST_IGNORED: [&str; 5] = [
    LS_FILES,
    NUL_ENDED,
    OTHERS_OPTION,
    IGNORED_OPTION,
    EXCLUDE_STANDARD_OPTION,
];
/// Lists the folders the checkout ignores, each ended by `/`, without what they hold, and the
/// ignored files outside them.
const LIST_IGNORED_FOLDERS: [&str; 6] = [
    LS_FILES,
    NUL_ENDED,
    OTHERS_OPTION,
    IGNORED_OPTION,
    EXCLUDE_STANDARD_OPTION,
    DIRECTORY_OPTION,
];
pub(crate) const GIT_FOLDER_OPTION: &str = "-C";
pub(crate) const CONFIG_OPTION: &str = "-c";
/// Ends each field or record with a NUL byte, and writes paths as they are.
pub(crate) const NUL_ENDED: &str = "-z";
/// Keeps git from running the file system monitor, a command the agent can set in its checkout's
/// git config.
pub(crate) const NO_MONITOR: [&str; 2] = [CONFIG_OPTION, "core.fsmonitor=false"];

/// Where the agent worked, with symlinks resolved: where a search looks for a name, and on
/// Windows, the network shares a path may be on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Access {
    folder: PathBuf,
    /// The git checkouts holding a file the agent read or changed.
    checkouts: Vec<PathBuf>,
    /// The files the agent read or changed that no checkout holds.
    #[cfg(windows)]
    files: Vec<PathBuf>,
}

impl Access {
    /// `touched` holds the absolute paths of the files the agent read or changed. `home` is the
    /// home folder, which no checkout of the agent's may hold.
    pub(crate) fn new(folder: &Path, touched: &[PathBuf], home: Option<&Path>) -> Self {
        // The agent stays when its worktree is removed. Its views still open other files.
        let folder = canonical(folder).unwrap_or_else(|_| folder.to_owned());
        // Compared with checkouts whose symlinks are resolved.
        let home: Option<PathBuf> =
            home.map(|home| canonical(home).unwrap_or_else(|_| home.to_owned()));
        let mut checkouts: Vec<PathBuf> = Vec::new();
        #[cfg(windows)]
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
                Some(checkout) if !checkouts.contains(&checkout) => checkouts.push(checkout),
                Some(_) => {}
                #[cfg(windows)]
                None => {
                    if let Ok(file) = canonical(file)
                        && !files.contains(&file)
                    {
                        files.push(file);
                    }
                }
                #[cfg(unix)]
                None => {}
            }
        }
        Self {
            folder,
            checkouts,
            #[cfg(windows)]
            files,
        }
    }

    /// Refuses a path the file system must not open even to resolve it: a device path, a name
    /// such as `NUL`, or a network share that neither the agent's folder nor a checkout or file
    /// it worked on is on.
    #[cfg(windows)]
    pub(crate) fn check_before_open(&self, asked: &Path) -> Result<(), ReadFileError> {
        let text = asked.to_str().ok_or(ReadFileError::Share)?;
        let worked_on = std::iter::once(&self.folder)
            .chain(&self.checkouts)
            .chain(&self.files)
            .filter_map(|path| path.to_str());
        if !windows_path::may_open(text, worked_on) {
            return Err(ReadFileError::Share);
        }
        if windows_path::names_a_device(text) {
            return Err(ReadFileError::NotAFile);
        }
        Ok(())
    }

    /// Refuses nothing: resolving a Unix path opens no file and reaches no other host.
    #[cfg(unix)]
    pub(crate) fn check_before_open(&self, _asked: &Path) -> Result<(), ReadFileError> {
        Ok(())
    }

    /// The folders of `earlier`, listed by a search before `root`, that `root` holds, relative to
    /// it. `earlier` starts with the agent's folder, as [`Self::search_roots`] does.
    fn roots_inside(&self, earlier: &[PathBuf], root: &Path) -> Vec<PathBuf> {
        let resolved = earlier.iter().enumerate().map(|(index, folder)| {
            if index == 0 {
                self.folder.as_path()
            } else {
                folder.as_path()
            }
        });
        resolved
            .filter_map(|folder| folder.strip_prefix(root).ok())
            .filter(|inside| !inside.as_os_str().is_empty())
            .map(Path::to_owned)
            .collect()
    }

    /// The folders a search lists: `folder`, the agent's folder as its record names it, then the
    /// checkouts other than that folder, so that no checkout is listed twice.
    fn search_roots(&self, folder: PathBuf) -> Vec<PathBuf> {
        let others = self
            .checkouts
            .iter()
            .filter(|checkout| **checkout != self.folder)
            .cloned();
        std::iter::once(folder).chain(others).collect()
    }
}

/// The git checkout above `folder`, a path without symlinks, unless it is `/` or holds `home`.
pub(crate) fn checkout_holding(folder: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let checkout = folder
        .ancestors()
        .find(|ancestor| ancestor.join(GIT_ENTRY).symlink_metadata().is_ok())?;
    (!too_wide(checkout, home)).then(|| checkout.to_owned())
}

/// Whether the checkout `checkout`, a path without symlinks, is `/` or holds `home`: its files
/// are everything the user owns.
pub(crate) fn too_wide(checkout: &Path, home: Option<&Path>) -> bool {
    checkout.parent().is_none() || home.is_some_and(|home| is_inside(home, checkout))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReadFileError {
    #[error("the path goes up with `..`")]
    GoesUp,
    #[cfg(windows)]
    #[error("the path is a device path, or on a network share the agent does not work on")]
    Share,
    #[error("{} does not exist", .0.display())]
    Missing(PathBuf),
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
    #[error(transparent)]
    Blocking(#[from] BlockingError),
}

impl ReadFileError {
    pub(crate) fn to_error_body(&self) -> ErrorBody {
        let code = match self {
            Self::GoesUp => ErrorCode::InvalidRequest,
            #[cfg(windows)]
            Self::Share => ErrorCode::InvalidRequest,
            Self::Missing(_) | Self::NotFound(_) => ErrorCode::NotFound,
            Self::NotAFile | Self::TooLarge(_) | Self::NotText | Self::Io(_) => {
                ErrorCode::FileUnreadable
            }
            Self::Blocking(error) => return error.to_error_body(),
        };
        ErrorBody::new(code, self.to_string())
    }
}

/// What the file view shows of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Content {
    Text(String),
    Image(Image),
    Pdf(Base64),
}

/// Reads `path`, absolute or relative to the agent's folder.
pub(crate) fn read(access: &Access, path: &Path) -> Result<Content, ReadFileError> {
    use std::io::Read;

    if path
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(ReadFileError::GoesUp);
    }
    let asked = access.folder.join(path);
    access.check_before_open(&asked)?;
    let resolved = canonical(&asked).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => ReadFileError::Missing(path.to_owned()),
        _ => ReadFileError::NotFound(error),
    })?;
    // A link may lead to a file named `AUX`, which the path opens as the device.
    access.check_before_open(&resolved)?;
    // Opening a device can act on it, so the type is checked first. The open file is checked
    // again, in case a link was swapped in since.
    let kind = std::fs::metadata(&resolved).map_err(ReadFileError::NotFound)?;
    if !kind.is_file() {
        return Err(ReadFileError::NotAFile);
    }
    let file = open_to_read(&resolved).map_err(ReadFileError::NotFound)?;
    let metadata = file.metadata().map_err(ReadFileError::Io)?;
    if !metadata.is_file() {
        return Err(ReadFileError::NotAFile);
    }
    let format = FileFormat::of(&resolved.to_string_lossy());
    let limit = format.max_bytes();
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
    match format {
        FileFormat::Image(media_type) => Ok(Content::Image(Image {
            media_type,
            data: Base64::encode(&bytes),
        })),
        FileFormat::Pdf => Ok(Content::Pdf(Base64::encode(&bytes))),
        FileFormat::Text => String::from_utf8(bytes)
            .map(Content::Text)
            .map_err(|_| ReadFileError::NotText),
    }
}

/// How the daemon reads a file, by its name's extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileFormat {
    Text,
    Image(ImageMediaType),
    Pdf,
}

impl FileFormat {
    fn of(name: &str) -> Self {
        if let Some(media_type) = ImageMediaType::from_file_name(name) {
            Self::Image(media_type)
        } else if is_pdf_name(name) {
            Self::Pdf
        } else {
            Self::Text
        }
    }

    fn max_bytes(self) -> u64 {
        match self {
            Self::Text => MAX_FILE_BYTES,
            Self::Image(_) => MAX_IMAGE_BYTES,
            Self::Pdf => MAX_PDF_BYTES,
        }
    }
}

/// What `agent.read_file` found for a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Found {
    /// `path` is where the file was read: the path asked for, joined to the agent's folder when
    /// relative, or what the lookup found for it. `ignored`: the lookup found it among the files
    /// git ignores.
    File {
        path: PathBuf,
        content: Content,
        ignored: bool,
    },
    /// Several files match the path, and none was read.
    Several(Vec<PathBuf>),
    /// Files git ignores match the path, and none was read: several, or one that a search cut
    /// short found. `cut_short`: other files may match too.
    IgnoredMatches {
        matches: Vec<FoundFile>,
        cut_short: bool,
    },
    /// No file matches the relative path, after a search that went as far as it says.
    Nowhere(IgnoredSearched),
}

/// Reads `path`, absolute or relative to `folder`, the agent's folder, or looks a relative path
/// up as the module says. `touched` holds the files the agent read or changed, most recent first.
/// `ignored` says how far the lookup goes in the files git ignores:
/// - [`IgnoredSearch::Quick`]: after the files git tracks, near the ignored ones
///   ([`search_ignored_nearby`]).
/// - [`IgnoredSearch::All`] and [`IgnoredSearch::Longer`]: all the ignored files, which the user
///   asked for once the quick lookup found nothing, rather than the files git tracks.
pub(crate) async fn find_and_read(
    folder: PathBuf,
    touched: Vec<PathBuf>,
    home: Option<PathBuf>,
    path: PathBuf,
    ignored: IgnoredSearch,
) -> Result<Found, ReadFileError> {
    let task = {
        let agent_folder = folder.clone();
        let path = path.clone();
        blocking::run(
            Instant::now() + READ_TIME,
            Scope::Folder(&folder),
            move || {
                let access = Access::new(&agent_folder, &touched, home.as_deref());
                let found = read_or_match_touched(&access, &agent_folder, &touched, &path)?;
                Ok::<_, ReadFileError>((access, found))
            },
        )
    };
    let (access, found) = task.await??;
    if let Some(found) = found {
        return Ok(found);
    }
    let tail = name_tail(&path).unwrap_or_else(|| path.clone());
    let ends_with_tail = |relative: &Path| relative.ends_with(&tail);
    let Some(time) = ignored.full_search_time() else {
        let tracked = search(
            &access,
            folder.clone(),
            ends_with_tail,
            Instant::now() + SEARCH_TIME,
        )
        .await;
        if !tracked.is_empty() {
            return pick(&access, &folder, tracked, Listing::Tracked).await;
        }
        let near =
            search_ignored_nearby(&access, folder.clone(), &tail, Instant::now() + SEARCH_TIME)
                .await;
        if near.is_empty() {
            return Ok(Found::Nowhere(IgnoredSearched::Quick));
        }
        return pick(&access, &folder, near, Listing::Ignored).await;
    };
    let listed = list_matching(
        &access,
        folder.clone(),
        &LIST_IGNORED,
        ends_with_tail,
        Instant::now() + time,
    )
    .await;
    if listed.matches.is_empty() {
        return Ok(Found::Nowhere(listed.searched(time)));
    }
    // A file found before the search's end may not be the only one: the user picks it.
    if listed.stopped_early() {
        return ignored_matches(&folder, listed.matches, true).await;
    }
    pick(&access, &folder, listed.matches, Listing::Ignored).await
}

/// The files of a checkout that a search went through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listing {
    Tracked,
    Ignored,
}

/// The one file of `matches`, read, or all of them for the user to pick from. Files git ignores
/// come with when each changed and its size, to tell copies apart.
async fn pick(
    access: &Access,
    folder: &Path,
    mut matches: Vec<PathBuf>,
    listing: Listing,
) -> Result<Found, ReadFileError> {
    let ignored = listing == Listing::Ignored;
    if matches.len() > 1 {
        if !ignored {
            return Ok(Found::Several(matches));
        }
        return ignored_matches(folder, matches, false).await;
    }
    let Some(only) = matches.pop() else {
        return Ok(Found::Nowhere(IgnoredSearched::Quick));
    };
    let content = blocking::run(Instant::now() + READ_TIME, Scope::Folder(folder), {
        let access = access.clone();
        let only = only.clone();
        move || read(&access, &only)
    })
    .await??;
    Ok(Found::File {
        path: only,
        content,
        ignored,
    })
}

/// `matches`, files git ignores, with when each changed and its size, to tell copies apart.
async fn ignored_matches(
    folder: &Path,
    matches: Vec<PathBuf>,
    cut_short: bool,
) -> Result<Found, ReadFileError> {
    let matches = blocking::run(
        Instant::now() + READ_TIME,
        Scope::Folder(folder),
        move || matches.into_iter().map(described).collect(),
    )
    .await?;
    Ok(Found::IgnoredMatches { matches, cut_short })
}

/// `path`, with when it last changed and its size.
fn described(path: PathBuf) -> FoundFile {
    let metadata = std::fs::metadata(&path).ok();
    FoundFile {
        modified: metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(Timestamp::from_system_time),
        bytes: metadata.map(|metadata| metadata.len()),
        path,
    }
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
            return Ok(Some(Found::File {
                path,
                content,
                ignored: false,
            }));
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
            return Ok(Some(Found::File {
                path,
                content,
                ignored: false,
            }));
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

/// The files whose path passes `matches_path`, as `git ls-files` lists them in `folder` and in the
/// checkouts of `access`, each once, until `deadline` or [`MAX_MATCHES`]. `folder` is the agent's
/// folder as its record names it. `matches_path` gets a path relative to the folder listed. A
/// folder that is not in a git checkout adds nothing, and neither does a symlink to nothing.
pub(crate) async fn search(
    access: &Access,
    folder: PathBuf,
    matches_path: impl Fn(&Path) -> bool,
    deadline: Instant,
) -> Vec<PathBuf> {
    list_matching(access, folder, &LIST_FILES, matches_path, deadline)
        .await
        .matches
}

/// What a listing found: its matches, the files it listed, and whether it ran out of time.
#[derive(Debug, Default)]
struct Listed {
    matches: Vec<PathBuf>,
    files: u32,
    timed_out: bool,
}

impl Listed {
    /// Whether the listing stopped before git listed every file: at its time limit, or at
    /// [`MAX_MATCHES`].
    fn stopped_early(&self) -> bool {
        self.timed_out || self.matches.len() >= MAX_MATCHES
    }

    /// How far a search of all the ignored files that matched nothing went, within `time`.
    fn searched(&self, time: Duration) -> IgnoredSearched {
        if self.timed_out {
            IgnoredSearched::CutShort {
                files: self.files,
                seconds: u32::try_from(time.as_secs()).unwrap_or(u32::MAX),
            }
        } else {
            IgnoredSearched::All { files: self.files }
        }
    }
}

/// As [`search`], with the files `list`, `git`'s arguments, lists.
async fn list_matching(
    access: &Access,
    folder: PathBuf,
    list: &[&str],
    matches_path: impl Fn(&Path) -> bool,
    deadline: Instant,
) -> Listed {
    let mut listed = Listed::default();
    // The agent's folder may sit in one of the checkouts: a file found twice is listed once.
    let mut seen: Vec<PathBuf> = Vec::new();
    let roots = access.search_roots(folder);
    for (index, root) in roots.iter().enumerate() {
        let Some((_child, mut lines)) = listing(root, list) else {
            continue;
        };
        let inner = access.roots_inside(roots.get(..index).unwrap_or_default(), root);
        let mut entry: Vec<u8> = Vec::new();
        loop {
            entry.clear();
            match tokio::time::timeout_at(deadline, lines.read_until(0, &mut entry)).await {
                Ok(Ok(0) | Err(_)) => break,
                Err(_) => {
                    listed.timed_out = true;
                    return listed;
                }
                Ok(Ok(_)) => {}
            }
            let name = entry.strip_suffix(&[0]).unwrap_or(&entry);
            let Some(relative) = git_path(name) else {
                continue;
            };
            // The listing of that root counted it already.
            if inner.iter().any(|inside| relative.starts_with(inside)) {
                continue;
            }
            listed.files = listed.files.saturating_add(1);
            if !matches_path(&relative) {
                continue;
            }
            let found = root.join(relative);
            // An answer carries paths as text.
            if found.to_str().is_none() || access.check_before_open(&found).is_err() {
                continue;
            }
            let resolving = found.clone();
            let resolving =
                blocking::run(deadline, Scope::Folder(root), move || canonical(&resolving));
            let resolved = match resolving.await {
                Ok(Ok(resolved)) => resolved,
                Ok(Err(_)) => continue,
                Err(_) => {
                    listed.timed_out = true;
                    return listed;
                }
            };
            if seen.contains(&resolved) {
                continue;
            }
            seen.push(resolved);
            listed.matches.push(found);
            if listed.matches.len() >= MAX_MATCHES {
                return listed;
            }
        }
    }
    listed
}

/// `git` listing files in `root` with the arguments `list`, NUL-ended, and its output. The
/// process ends when the first is dropped. `None` when it does not start.
fn listing(root: &Path, list: &[&str]) -> Option<(Child, BufReader<ChildStdout>)> {
    let mut git = Command::new(GIT_PROGRAM);
    git.args(NO_MONITOR)
        .arg(GIT_FOLDER_OPTION)
        .arg(root)
        .args(list)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    program::hide_console(&mut git);
    let mut child = git.spawn().ok()?;
    let stdout = child.stdout.take()?;
    Some((child, BufReader::new(stdout)))
}

/// The files that end with `tail` near the files git ignores in `folder` and the checkouts of
/// `access`: in each ignored folder, directly or one folder down, and among the ignored files
/// outside those folders. Each once, until [`MAX_MATCHES`]. The listing stops at `deadline`, and
/// the look in the folders it named [`NEAR_SCAN_TIME`] later.
///
/// `git ls-files --directory` names an ignored folder without listing what it holds, so this
/// stays quick in a checkout whose ignored folders hold many files.
async fn search_ignored_nearby(
    access: &Access,
    folder: PathBuf,
    tail: &Path,
    deadline: Instant,
) -> Vec<PathBuf> {
    let scan_deadline = deadline + NEAR_SCAN_TIME;
    let mut matches: Vec<PathBuf> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for root in access.search_roots(folder) {
        let Some((_child, mut lines)) = listing(&root, &LIST_IGNORED_FOLDERS) else {
            continue;
        };
        let mut entries: Vec<IgnoredEntry> = Vec::new();
        let mut entry: Vec<u8> = Vec::new();
        let mut timed_out = false;
        loop {
            entry.clear();
            match tokio::time::timeout_at(deadline, lines.read_until(0, &mut entry)).await {
                Ok(Ok(0) | Err(_)) => break,
                Err(_) => {
                    timed_out = true;
                    break;
                }
                Ok(Ok(_)) => entries.extend(IgnoredEntry::of(&entry, tail)),
            }
        }
        let scan = {
            let access = access.clone();
            let root = root.clone();
            let tail = tail.to_owned();
            move || files_near(&access, &root, &entries, &tail, scan_deadline)
        };
        let wait = scan_deadline + NEAR_SCAN_GRACE;
        let Ok(found) = blocking::run(wait, Scope::Folder(&root), scan).await else {
            return matches;
        };
        for NearFile { path, resolved } in found {
            if seen.contains(&resolved) {
                continue;
            }
            seen.push(resolved);
            matches.push(path);
            if matches.len() >= MAX_MATCHES {
                return matches;
            }
        }
        if timed_out {
            break;
        }
    }
    matches
}

/// An entry of `git ls-files --directory` that the quick look keeps, relative to its checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
enum IgnoredEntry {
    Folder(PathBuf),
    /// An ignored file outside the ignored folders, whose path ends with the tail.
    File(PathBuf),
}

impl IgnoredEntry {
    /// The entry `bytes`, NUL-ended, if the look for `tail` needs it. A folder ends with `/`.
    fn of(bytes: &[u8], tail: &Path) -> Option<Self> {
        let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
        match bytes.strip_suffix(b"/") {
            Some(folder) => git_path(folder).map(Self::Folder),
            None => git_path(bytes)
                .filter(|file| file.ends_with(tail))
                .map(Self::File),
        }
    }
}

/// A file the quick look found, with its path without symlinks.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NearFile {
    path: PathBuf,
    resolved: PathBuf,
}

/// The files that end with `tail` near the ignored `entries` of `root`, until `deadline` or
/// [`MAX_MATCHES`].
fn files_near(
    access: &Access,
    root: &Path,
    entries: &[IgnoredEntry],
    tail: &Path,
    deadline: Instant,
) -> Vec<NearFile> {
    let mut found: Vec<NearFile> = Vec::new();
    for entry in entries {
        let candidates = match entry {
            IgnoredEntry::File(relative) => vec![root.join(relative)],
            IgnoredEntry::Folder(relative) => {
                let folder = root.join(relative);
                let mut candidates = paths_in(&folder, tail);
                for below in folders_in(&folder, deadline) {
                    candidates.extend(paths_in(&below, tail));
                }
                candidates
            }
        };
        for path in candidates {
            if Instant::now() >= deadline || found.len() >= MAX_MATCHES {
                return found;
            }
            // An answer carries paths as text.
            if path.to_str().is_none()
                || access.check_before_open(&path).is_err()
                || !path.is_file()
            {
                continue;
            }
            if let Ok(resolved) = canonical(&path) {
                found.push(NearFile { path, resolved });
            }
        }
    }
    found
}

/// Where a file whose path ends with `tail` is when `folder` holds it: `folder/tail`, and when
/// `tail` starts with the last names of `folder`, `folder` joined to the rest of `tail`. For
/// example, `.db/fix.sql` in `api/.db` is `api/.db/fix.sql`.
fn paths_in(folder: &Path, tail: &Path) -> Vec<PathBuf> {
    let names: Vec<Component> = tail.components().collect();
    let mut paths = vec![folder.join(tail)];
    // The file's own name stays in the rest.
    for shared in 1..names.len() {
        let Some((first, rest)) = names.split_at_checked(shared) else {
            break;
        };
        if folder.ends_with(first.iter().collect::<PathBuf>()) {
            paths.push(folder.join(rest.iter().collect::<PathBuf>()));
        }
    }
    paths
}

/// The folders in `folder`, among its first [`MAX_NEAR_ENTRIES`] entries, until `deadline`. An
/// entry that does not read is left out, as is a symlink.
fn folders_in(folder: &Path, deadline: Instant) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return folders;
    };
    for entry in entries.take(MAX_NEAR_ENTRIES) {
        if Instant::now() >= deadline {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            folders.push(entry.path());
        }
    }
    folders
}

/// A path as `git ls-files -z` writes it, relative to the checkout. `None` for an absolute path or
/// one with `..`, which the checkout's index file can hold when the agent wrote it: on Windows,
/// `//host/share/file` would make the search connect to that host.
pub(crate) fn git_path(bytes: &[u8]) -> Option<PathBuf> {
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

/// Whether two file names are the same name, by the rule of [`is_same`].
#[cfg(unix)]
fn same_name(left: &OsStr, right: &OsStr) -> bool {
    left == right
}

/// Whether two file names are the same name, by the rule of [`is_same`].
#[cfg(windows)]
fn same_name(left: &OsStr, right: &OsStr) -> bool {
    windows_path::same_name(&left.to_string_lossy(), &right.to_string_lossy())
}

/// Copies of `edits` (oldest first) that change a file named `name`, oldest first. Each file
/// keeps only its newest edits, up to the first one whose texts pass `max_bytes`: an answer
/// holds no more, and the copies are made on the hub. A relative edit path is in `folder`.
pub(crate) fn edits_named<'edit>(
    edits: impl DoubleEndedIterator<Item = &'edit FileEdit>,
    folder: &Path,
    name: &OsStr,
    max_bytes: usize,
) -> Vec<FileEdit> {
    // Each file named `name`, with the bytes of the texts of its edits kept so far.
    let mut files: Vec<(PathBuf, usize)> = Vec::new();
    let mut kept = Vec::new();
    for edit in edits.rev() {
        let named = edit
            .path
            .file_name()
            .is_some_and(|edited| same_name(edited, name));
        if !named {
            continue;
        }
        let path = folder.join(&edit.path);
        let index = match files.iter().position(|(file, _)| is_same(file, &path)) {
            Some(index) => index,
            None => {
                files.push((path, 0));
                files.len() - 1
            }
        };
        let Some((_, bytes)) = files.get_mut(index) else {
            continue;
        };
        if *bytes > max_bytes {
            continue;
        }
        *bytes = bytes.saturating_add(edit.old_text.len().saturating_add(edit.new_text.len()));
        kept.push(edit.clone());
    }
    kept.reverse();
    kept
}

/// The edits of `file`, out of `edits` oldest first: the newest whose JSON fits in `max_bytes`.
/// A relative edit path is in `folder`.
pub(crate) fn edits_of(
    edits: Vec<FileEdit>,
    folder: &Path,
    file: &Path,
    max_bytes: usize,
) -> Vec<FileEdit> {
    let mut kept = Vec::new();
    let mut bytes: usize = 0;
    for edit in edits.into_iter().rev() {
        if !is_same(&folder.join(&edit.path), file) {
            continue;
        }
        let size = serde_json::to_vec(&edit).map_or(usize::MAX, |encoded| encoded.len());
        bytes = bytes.saturating_add(size);
        if bytes > max_bytes {
            break;
        }
        kept.push(edit);
    }
    kept.reverse();
    kept
}

/// Opens a file for reading without waiting: opening a FIFO waits for a writer otherwise. A
/// terminal it opens does not become the daemon's controlling terminal, whose hangup would end
/// the daemon.
#[cfg(unix)]
pub(crate) fn open_to_read(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    Ok(File::from(rustix::fs::open(path, flags, Mode::empty())?))
}

/// Opens a file or a folder for reading. [`Access::check_before_open`] refuses a device first.
#[cfg(windows)]
pub(crate) fn open_to_read(path: &Path) -> io::Result<File> {
    windows_files::open(path)
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(unix)]
    use std::time::Duration;

    #[cfg(unix)]
    use rustix::fs::Mode;

    use cockpit_protocol::EditTexts;

    use super::*;

    fn only_folder(folder: &Path) -> Access {
        Access::new(folder, &[], None)
    }

    fn text(text: &str) -> Content {
        Content::Text(text.to_owned())
    }

    fn assert_goes_up<T: Debug>(result: Result<T, ReadFileError>) {
        assert!(matches!(result, Err(ReadFileError::GoesUp)), "{result:?}");
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
            Access::new(&self.agent, touched, Some(&home))
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
    fn reads_outside_the_folder_but_not_up_from_it() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("agent");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(temp.path().join("notes.txt"), "notes").unwrap();
        symlink(temp.path().join("notes.txt"), folder.join("link.txt")).unwrap();

        let access = only_folder(&folder);

        let absolute = read(&access, &temp.path().join("notes.txt"));
        let link = read(&access, Path::new("link.txt"));

        assert_eq!(absolute.unwrap(), text("notes"));
        assert_eq!(link.unwrap(), text("notes"));
        assert_goes_up(read(&access, Path::new("../notes.txt")));
    }

    #[test]
    fn reads_files_the_agent_never_touched() {
        let layout = Layout::new();
        let access = layout.access(&[layout.loose.join("todo.txt")]);

        let beside = read(&access, &layout.feature.join("src/app/beside.ts"));
        let diary = read(&access, &layout.loose.join("diary.txt"));
        let untouched = read(&access, &layout.untouched.join("src/secret.ts"));

        assert_eq!(beside.unwrap(), text("beside\n"));
        assert_eq!(diary.unwrap(), text("diary\n"));
        assert_eq!(untouched.unwrap(), text("secret\n"));
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
        find_ignored(layout, touched, path, IgnoredSearch::Quick).await
    }

    async fn find_ignored(
        layout: &Layout,
        touched: &[PathBuf],
        path: &str,
        ignored: IgnoredSearch,
    ) -> Result<Found, ReadFileError> {
        let home = canonical(layout.temp.path()).unwrap();
        find_and_read(
            layout.agent.clone(),
            touched.to_vec(),
            Some(home),
            PathBuf::from(path),
            ignored,
        )
        .await
    }

    fn file(path: PathBuf, text: &str) -> Found {
        Found::File {
            path,
            content: Content::Text(text.to_owned()),
            ignored: false,
        }
    }

    fn ignored_file(path: PathBuf, text: &str) -> Found {
        Found::File {
            path,
            content: Content::Text(text.to_owned()),
            ignored: true,
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
        let ignored = find(&layout, &touched, "report.ts").await.unwrap();
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
        assert_eq!(
            ignored,
            ignored_file(layout.agent.join("build/report.ts"), "built\n")
        );
        assert_eq!(none.unwrap(), Found::Nowhere(IgnoredSearched::Quick));
    }

    /// Git ignores `.db/`, `cache/` and `*.log`. The quick look finds a file at the top of an
    /// ignored folder or one folder down, and an ignored file outside those folders. A file deeper
    /// down takes the search of all the ignored files.
    #[tokio::test]
    async fn the_quick_look_checks_each_ignored_folder_and_one_folder_down() {
        let layout = Layout::new();
        git_init(&layout.agent);
        write(&layout.agent.join(".gitignore"), "/.db\ncache/\n*.log\n");
        write(&layout.agent.join(".db/refund_fix.sql"), "UPDATE\n");
        write(&layout.agent.join("cache/2026/export.csv"), "a,b\n");
        write(&layout.agent.join("cache/2026/11/deep.csv"), "deep\n");
        write(&layout.agent.join("src/run.log"), "log\n");

        let top = find(&layout, &[], "refund_fix.sql").await.unwrap();
        let below = find(&layout, &[], "export.csv").await.unwrap();
        let loose = find(&layout, &[], "run.log").await.unwrap();
        let deep = find(&layout, &[], "deep.csv").await.unwrap();
        let searched = find_ignored(&layout, &[], "deep.csv", IgnoredSearch::All)
            .await
            .unwrap();

        assert_eq!(
            top,
            ignored_file(layout.agent.join(".db/refund_fix.sql"), "UPDATE\n")
        );
        assert_eq!(
            below,
            ignored_file(layout.agent.join("cache/2026/export.csv"), "a,b\n")
        );
        assert_eq!(
            loose,
            ignored_file(layout.agent.join("src/run.log"), "log\n")
        );
        assert_eq!(deep, Found::Nowhere(IgnoredSearched::Quick));
        assert_eq!(
            searched,
            ignored_file(layout.agent.join("cache/2026/11/deep.csv"), "deep\n")
        );
    }

    /// Copies that git ignores come with their size, to tell them apart. A name no ignored file
    /// has says how many ignored files the search went through.
    #[tokio::test]
    async fn the_search_of_all_ignored_files_lists_copies_or_counts_the_files() {
        let layout = Layout::new();
        git_init(&layout.agent);
        write(&layout.agent.join(".gitignore"), "out/\n");
        write(&layout.agent.join("out/a/b/report.csv"), "1\n");
        write(&layout.agent.join("out/c/d/report.csv"), "1,2\n");
        write(&layout.agent.join("src/report.csv.md"), "tracked\n");

        let copies = find_ignored(&layout, &[], "report.csv", IgnoredSearch::All)
            .await
            .unwrap();
        let none = find_ignored(&layout, &[], "missing.csv", IgnoredSearch::Longer)
            .await
            .unwrap();

        let Found::IgnoredMatches {
            matches: mut copies,
            cut_short: false,
        } = copies
        else {
            panic!("{copies:?}");
        };
        copies.sort_by(|one, other| one.path.cmp(&other.path));
        let sizes: Vec<(PathBuf, Option<u64>)> = copies
            .iter()
            .map(|copy| (copy.path.clone(), copy.bytes))
            .collect();
        assert_eq!(
            sizes,
            [
                (layout.agent.join("out/a/b/report.csv"), Some(2)),
                (layout.agent.join("out/c/d/report.csv"), Some(4)),
            ]
        );
        assert!(copies.iter().all(|copy| copy.modified.is_some()));
        assert_eq!(none, Found::Nowhere(IgnoredSearched::All { files: 2 }));
    }

    fn git_add(folder: &Path, path: &str) {
        let status = std::process::Command::new(GIT_PROGRAM)
            .args(["add", path])
            .current_dir(folder)
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// The agent names the file with its ignored folder, which sits in a tracked folder. Git then
    /// lists `api/.db/`, not `api/`.
    #[tokio::test]
    async fn the_quick_look_finds_a_name_that_starts_with_its_ignored_folder() {
        let layout = Layout::new();
        git_init(&layout.agent);
        write(&layout.agent.join(".gitignore"), ".db/\n");
        write(&layout.agent.join("api/main.rs"), "fn main() {}\n");
        write(&layout.agent.join("api/.db/refund_fix.sql"), "UPDATE\n");
        write(&layout.agent.join("web/.db/old/seed.sql"), "INSERT\n");
        git_add(&layout.agent, "api/main.rs");

        let top = find(&layout, &[], ".db/refund_fix.sql").await.unwrap();
        let below = find(&layout, &[], ".db/old/seed.sql").await.unwrap();

        assert_eq!(
            top,
            ignored_file(layout.agent.join("api/.db/refund_fix.sql"), "UPDATE\n")
        );
        assert_eq!(
            below,
            ignored_file(layout.agent.join("web/.db/old/seed.sql"), "INSERT\n")
        );
    }

    /// The agent works in `repo/sub` and touched a file of `repo`: both are listed, and the files
    /// of `sub` count once.
    #[tokio::test]
    async fn a_checkout_that_holds_the_agents_folder_counts_its_files_once() {
        let layout = Layout::new();
        let repo = layout.agent.parent().unwrap().join("repo");
        git_init(&repo);
        write(&repo.join(".gitignore"), "out/\n");
        write(&repo.join("top.rs"), "top\n");
        write(&repo.join("sub/out/a.csv"), "a\n");
        write(&repo.join("sub/out/b.csv"), "b\n");
        let home = canonical(layout.temp.path()).unwrap();

        let found = find_and_read(
            repo.join("sub"),
            vec![repo.join("top.rs")],
            Some(home),
            PathBuf::from("missing.csv"),
            IgnoredSearch::All,
        )
        .await
        .unwrap();

        assert_eq!(found, Found::Nowhere(IgnoredSearched::All { files: 2 }));
    }

    /// More copies than a search lists: the user sees that others may exist.
    #[tokio::test]
    async fn a_search_that_stops_at_its_most_matches_says_so() {
        let layout = Layout::new();
        git_init(&layout.agent);
        write(&layout.agent.join(".gitignore"), "out/\n");
        for copy in 0..=MAX_MATCHES {
            write(&layout.agent.join(format!("out/{copy}/report.csv")), "1\n");
        }

        let found = find_ignored(&layout, &[], "report.csv", IgnoredSearch::All)
            .await
            .unwrap();

        let Found::IgnoredMatches { matches, cut_short } = found else {
            panic!("{found:?}");
        };
        assert_eq!(matches.len(), MAX_MATCHES);
        assert!(cut_short);
    }

    #[test]
    fn a_listing_out_of_time_or_matches_stopped_early() {
        let listed = |timed_out: bool, matches: usize| Listed {
            matches: vec![PathBuf::from("out/report.csv"); matches],
            files: 10,
            timed_out,
        };

        assert!(listed(true, 1).stopped_early());
        assert!(listed(false, MAX_MATCHES).stopped_early());
        assert!(!listed(false, 1).stopped_early());
    }

    #[test]
    fn a_search_out_of_time_says_how_far_it_went() {
        let listed = |timed_out: bool| Listed {
            matches: Vec::new(),
            files: 2_100_000,
            timed_out,
        };
        let time = Duration::from_secs(15);

        assert_eq!(
            listed(true).searched(time),
            IgnoredSearched::CutShort {
                files: 2_100_000,
                seconds: 15
            }
        );
        assert_eq!(
            listed(false).searched(time),
            IgnoredSearched::All { files: 2_100_000 }
        );
    }

    #[test]
    fn a_tail_that_starts_with_the_folders_names_joins_it_once() {
        let folder = Path::new("/work/api/.db");

        assert_eq!(
            paths_in(folder, Path::new("fix.sql")),
            [folder.join("fix.sql")]
        );
        assert_eq!(
            paths_in(folder, Path::new(".db/fix.sql")),
            [folder.join(".db/fix.sql"), folder.join("fix.sql")]
        );
        assert_eq!(
            paths_in(folder, Path::new("api/.db/fix.sql")),
            [folder.join("api/.db/fix.sql"), folder.join("fix.sql")]
        );
    }

    /// An answer carries paths as text.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_search_leaves_out_a_name_that_is_not_utf8() {
        use std::os::unix::ffi::OsStrExt;

        let layout = Layout::new();
        git_init(&layout.agent);
        write(&layout.agent.join("src/a/widget.ts"), "widget\n");
        let folder = layout.agent.join("src").join(OsStr::from_bytes(b"\xff"));
        write(&folder.join("widget.ts"), "unnamed\n");

        let found = find(&layout, &[], "widget.ts").await.unwrap();

        assert_eq!(
            found,
            file(layout.agent.join("src/a/widget.ts"), "widget\n")
        );
    }

    /// A symlink to nothing is no file the user can open. One that leads out of the checkout is.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_search_leaves_out_a_symlink_to_nothing() {
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
            Found::Several(vec![
                layout.agent.join("src/a/widget.ts"),
                layout.agent.join("src/b/widget.ts"),
            ])
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
        assert_goes_up(dots);
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
    fn a_checkout_holding_the_home_folder_is_not_searched() {
        let layout = Layout::new();
        let home = canonical(layout.temp.path()).unwrap();
        std::fs::create_dir(home.join(GIT_ENTRY)).unwrap();
        write(&home.join("dotfile.txt"), "dotfile\n");
        let access = Access::new(&layout.agent, &[home.join("dotfile.txt")], Some(&home));

        let roots = access.search_roots(layout.agent.clone());

        assert_eq!(roots, std::slice::from_ref(&layout.agent));
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
        );

        let roots = access.search_roots(layout.agent.clone());

        assert_eq!(roots, std::slice::from_ref(&layout.agent));
    }

    #[cfg(unix)]
    #[test]
    fn a_checkout_reached_through_a_symlink_is_searched_once() {
        let layout = Layout::new();
        let root = layout.agent.parent().unwrap();
        symlink(&layout.feature, root.join("feature_link")).unwrap();
        // The agent read one file through a symlinked folder, the other one directly.
        let access = layout.access(&[
            root.join("feature_link/src/app/touched.ts"),
            layout.feature.join("src/app/beside.ts"),
        ]);

        assert_eq!(
            access.search_roots(layout.agent.clone()),
            [layout.agent.clone(), layout.feature.clone()]
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

    /// Opening a device can act on it: a serial port resets the board it drives.
    #[cfg(target_os = "linux")]
    #[test]
    fn what_is_not_a_regular_file_is_refused_before_it_opens() {
        use std::ffi::{CStr, CString};
        use std::mem::MaybeUninit;

        use rustix::fs::inotify::{self, CreateFlags, WatchFlags};
        use rustix::io::Errno;

        let temp = tempfile::tempdir().unwrap();
        rustix::fs::mkfifoat(rustix::fs::CWD, temp.path().join("notes.txt"), Mode::RUSR).unwrap();
        let access = only_folder(temp.path());
        let watch = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC).unwrap();
        inotify::add_watch(&watch, temp.path(), WatchFlags::OPEN).unwrap();

        let read = read(&access, Path::new("notes.txt"));

        assert!(matches!(read, Err(ReadFileError::NotAFile)), "{read:?}");
        let mut buffer: [MaybeUninit<u8>; 1024] = [MaybeUninit::uninit(); 1024];
        let mut events = inotify::Reader::new(&watch, &mut buffer);
        let opened: Result<Option<CString>, Errno> = events
            .next()
            .map(|event| event.file_name().map(CStr::to_owned));
        assert_eq!(opened, Err(Errno::AGAIN));
    }

    /// An agent stays in the list when its worktree is removed.
    #[tokio::test]
    async fn a_file_elsewhere_reads_when_the_agents_folder_is_gone() {
        let layout = Layout::new();
        std::fs::remove_dir_all(&layout.agent).unwrap();
        let todo = layout.loose.join("todo.txt");

        let found = find(&layout, &[], todo.to_str().unwrap()).await;

        assert_eq!(found.unwrap(), file(todo, "todo\n"));
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
                data: Base64::new("iVBORw==".to_owned()),
            })
        );
        assert!(
            matches!(too_large, Err(ReadFileError::TooLarge(MAX_IMAGE_BYTES))),
            "{too_large:?}"
        );
    }

    /// A PDF holds bytes that are not UTF-8, which the daemon refuses in a text file.
    #[test]
    fn a_pdf_reads_as_base64_up_to_its_own_limit() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("report.PDF"), b"%PDF-\xe2\xe3").unwrap();
        let large = File::create(temp.path().join("large.pdf")).unwrap();
        large.set_len(MAX_PDF_BYTES + 1).unwrap();
        let access = only_folder(temp.path());

        let report = read(&access, Path::new("report.PDF"));
        let too_large = read(&access, Path::new("large.pdf"));

        assert_eq!(
            report.unwrap(),
            Content::Pdf(Base64::new("JVBERi3i4w==".to_owned()))
        );
        assert!(
            matches!(too_large, Err(ReadFileError::TooLarge(MAX_PDF_BYTES))),
            "{too_large:?}"
        );
    }

    /// The name a link leads to decides how the file reads, not the link's own name.
    #[cfg(unix)]
    #[test]
    fn a_link_reads_as_the_file_it_leads_to() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("report.pdf"), b"%PDF-\xe2\xe3").unwrap();
        std::fs::write(temp.path().join("notes.txt"), "notes").unwrap();
        symlink(temp.path().join("report.pdf"), temp.path().join("latest")).unwrap();
        symlink(temp.path().join("notes.txt"), temp.path().join("notes.pdf")).unwrap();
        let access = only_folder(temp.path());

        let latest = read(&access, Path::new("latest"));
        let notes = read(&access, Path::new("notes.pdf"));

        assert_eq!(
            latest.unwrap(),
            Content::Pdf(Base64::new("JVBERi3i4w==".to_owned()))
        );
        assert_eq!(notes.unwrap(), text("notes"));
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
    fn a_device_or_a_share_the_agent_does_not_work_on_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let access = only_folder(temp.path());

        for path in [
            r"\\attacker.invalid\share\notes.txt",
            "//attacker.invalid/share/notes.txt",
            r"\\?\UNC\attacker.invalid\share\notes.txt",
            r"\\.\pipe\yetack",
            r"\\.\C:\notes.txt",
            r"\\?\GLOBALROOT\Device\Null",
        ] {
            let refused = read(&access, Path::new(path));
            assert!(
                matches!(refused, Err(ReadFileError::Share)),
                "{path}: {refused:?}"
            );
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
    fn a_file_outside_the_folder_reads_whatever_the_case() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("agent");
        write(&folder.join("inside.txt"), "inside");
        write(&temp.path().join("agent_other").join("notes.txt"), "notes");
        let access = only_folder(&folder);

        let sibling = read(&access, &temp.path().join(r"AGENT_OTHER\notes.txt"));

        assert_eq!(sibling.unwrap(), text("notes"));
        assert_eq!(
            read(&access, Path::new("INSIDE.TXT")).unwrap(),
            text("inside")
        );
    }

    fn edit(path: &str, old_text: &str, new_text: &str) -> FileEdit {
        FileEdit {
            path: PathBuf::from(path),
            old_text: old_text.to_owned(),
            new_text: new_text.to_owned(),
            texts: EditTexts::Kept,
        }
    }

    fn new_texts(edits: &[FileEdit]) -> Vec<&str> {
        edits.iter().map(|edit| edit.new_text.as_str()).collect()
    }

    #[cfg(unix)]
    #[test]
    fn edits_named_copies_the_edits_of_each_file_of_that_name() {
        let edits = [
            edit("/work/app/notes.txt", "a", "1"),
            edit("/work/app/notes.md", "a", "2"),
            edit("/work/app/NOTES.txt", "a", "3"),
            edit("old/notes.txt", "a", "4"),
            edit("/work/app/old/notes.txt", "a", "5"),
        ];
        let name = OsStr::new("notes.txt");

        let named = edits_named(edits.iter(), Path::new("/work/app"), name, usize::MAX);

        assert_eq!(new_texts(&named), ["1", "4", "5"]);
    }

    #[cfg(unix)]
    #[test]
    fn edits_named_stops_copying_a_file_past_the_limit_but_not_the_others() {
        let large = "x".repeat(100);
        let edits = [
            edit("/work/app/old/notes.txt", "a", "1"),
            edit("/work/app/notes.txt", &large, "2"),
            edit("/work/app/notes.txt", &large, "3"),
            edit("/work/app/notes.txt", &large, "4"),
            edit("/work/app/old/notes.txt", "a", "5"),
        ];
        let name = OsStr::new("notes.txt");

        // The two newest edits of notes.txt pass 150 bytes, so no older one could fit an answer.
        let named = edits_named(edits.iter(), Path::new("/work/app"), name, 150);

        assert_eq!(new_texts(&named), ["1", "3", "4", "5"]);
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_file_name_matches_whatever_its_case() {
        let edits = [
            edit(r"C:\work\app\NOTES.TXT", "a", "1"),
            edit(r"C:\work\app\notes.md", "a", "2"),
        ];
        let name = OsStr::new("notes.txt");

        let named = edits_named(edits.iter(), Path::new(r"C:\work\app"), name, usize::MAX);

        assert_eq!(new_texts(&named), ["1"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_keeps_its_own_edits_and_a_relative_one_in_the_folder() {
        let folder = Path::new("/work/app");
        let edits = vec![
            edit("/work/app/notes.txt", "a", "b"),
            edit("/work/app/old/notes.txt", "c", "d"),
            edit("notes.txt", "b", "e"),
        ];

        let kept = edits_of(edits, folder, Path::new("/work/app/notes.txt"), usize::MAX);

        let new_texts: Vec<&str> = kept.iter().map(|edit| edit.new_text.as_str()).collect();
        assert_eq!(new_texts, ["b", "e"]);
    }

    #[cfg(unix)]
    #[test]
    fn edits_over_the_limit_lose_the_oldest() {
        let file = "/work/app/notes.txt";
        let edits = vec![
            edit(file, "a", "b"),
            edit(file, "b", "c"),
            edit(file, "c", "d"),
        ];
        let one = serde_json::to_vec(&edits[0]).unwrap().len();

        let kept = edits_of(edits, Path::new("/work/app"), Path::new(file), 2 * one);

        let new_texts: Vec<&str> = kept.iter().map(|edit| edit.new_text.as_str()).collect();
        assert_eq!(new_texts, ["c", "d"]);
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_file_keeps_its_edits_whatever_their_case_and_separators() {
        let folder = Path::new(r"C:\work\app");
        let edits = vec![
            edit("c:/work/APP/src/Notes.txt", "a", "b"),
            edit(r"C:\work\app\old\notes.txt", "c", "d"),
            edit(r"src\notes.txt", "b", "e"),
        ];

        let kept = edits_of(
            edits,
            folder,
            Path::new(r"C:\work\app\src\notes.txt"),
            usize::MAX,
        );

        let new_texts: Vec<&str> = kept.iter().map(|edit| edit.new_text.as_str()).collect();
        assert_eq!(new_texts, ["b", "e"]);
    }

    #[test]
    fn an_edit_too_large_to_keep_reaches_the_answer_marked() {
        let folder = Path::new(if cfg!(windows) {
            r"C:\work\app"
        } else {
            "/work/app"
        });
        let file = folder.join("prices.csv");
        let path = file.to_str().unwrap();
        let too_large = FileEdit {
            texts: EditTexts::TooLarge,
            ..edit(path, "", "")
        };
        let edits = [edit(path, "a", "b"), too_large, edit(path, "b", "c")];

        let named = edits_named(edits.iter(), folder, OsStr::new("prices.csv"), usize::MAX);
        let kept = edits_of(named, folder, &file, usize::MAX);

        assert_eq!(kept, edits);
    }
}
