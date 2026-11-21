//! `agent.git_status`: the branch and the changed files of each checkout the agent works in.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use cockpit_protocol::{
    BranchState, CheckoutKind, CheckoutState, Commit, CommitHash, Completeness, FileChange,
    FileStatus, GitStatus, ImageMediaType, LineCounts, MAX_STATUS_FILES, Remote, Repository,
    Upstream,
};
use strum::{EnumString, IntoStaticStr, VariantArray};
use tokio::time::Instant;

use crate::blocking::{self, Scope};
use crate::git::command::{
    self, END_OF_OPTIONS, FIELD_END, Git, HEAD, NO_COLOR, NO_EXT_DIFF, NO_SUBMODULES, NO_TEXTCONV,
    NUMSTAT,
};
use crate::git::commit::Unpushed;
use crate::git::{GitError, checkout, commit, numstat};
use crate::read_file::{self, Access, Content, MAX_FILE_BYTES, NUL_ENDED};
use crate::repository::{self, ConfigRemote};

/// How long the status of the checkouts may take. git reads each file whose state its index does
/// not record, which takes a large checkout seconds.
const STATUS_TIME: Duration = Duration::from_secs(20);
/// The branch, then each changed file, ended by NUL bytes. Untracked files are listed one by one
/// rather than by folder.
const STATUS_OPTIONS: [&str; 6] = [
    "status",
    "--porcelain=v2",
    NUL_ENDED,
    "--branch",
    "--untracked-files=all",
    NO_SUBMODULES,
];
/// The counts of each tracked file against the last commit, staged or not.
const DIFF_OPTIONS: [&str; 7] = [
    "diff",
    NUMSTAT,
    NUL_ENDED,
    NO_COLOR,
    NO_TEXTCONV,
    NO_EXT_DIFF,
    NO_SUBMODULES,
];
/// Lists the config's filter keys, each followed by a newline and its value when it has one, and
/// ended by a NUL byte.
const LIST_FILTERS: [&str; 4] = ["config", NUL_ENDED, "--get-regexp", r"^filter\."];
/// `git config --get-regexp`'s exit code when no key matches.
const NO_SUCH_KEY: i32 = 1;
const FILTER_SECTION: &str = "filter";
const KEY_SEPARATOR: u8 = b'.';
const VALUE_START: u8 = b'\n';
/// git takes a setting's name up to its first `=`.
const SETTING_SEPARATOR: char = '=';
/// The settings that make a filter run a program, set empty, and the one that lets git go on
/// without them.
const FILTER_OFF: [(&str, &str); 4] = [
    ("clean", ""),
    ("smudge", ""),
    ("process", ""),
    ("required", "false"),
];
/// What `branch.head` says of a detached HEAD.
const DETACHED: &str = "(detached)";
const AHEAD_SIGN: char = '+';
const BEHIND_SIGN: char = '-';
const WORD_END: u8 = b' ';
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
/// Past this, the files listed are cut, and some counts missing.
const MAX_STATUS_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;
/// The most bytes of untracked files read to count their lines, in each checkout.
const MAX_COUNTED_BYTES: u64 = 32 * 1024 * 1024;

/// What a `git status --porcelain=v2` entry starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
enum EntryKind {
    #[strum(serialize = "#")]
    Header,
    #[strum(serialize = "1")]
    Ordinary,
    #[strum(serialize = "2")]
    RenamedOrCopied,
    #[strum(serialize = "u")]
    Unmerged,
    #[strum(serialize = "?")]
    Untracked,
    #[strum(serialize = "!")]
    Ignored,
}

impl EntryKind {
    /// How many fields follow the kind, the path last. A renamed or copied file's original path
    /// comes after, as a field of its own.
    fn fields(self) -> usize {
        match self {
            Self::Ordinary => 8,
            Self::RenamedOrCopied => 9,
            Self::Unmerged => 10,
            Self::Header | Self::Untracked | Self::Ignored => 1,
        }
    }
}

/// The headers `--branch` adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
enum BranchHeader {
    #[strum(serialize = "branch.oid")]
    Commit,
    #[strum(serialize = "branch.head")]
    Name,
    #[strum(serialize = "branch.upstream")]
    Upstream,
    #[strum(serialize = "branch.ab")]
    AheadBehind,
}

/// One side of an entry's `XY`: the index against HEAD, or the working tree against the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
enum Code {
    #[strum(serialize = ".")]
    Unmodified,
    #[strum(serialize = "M")]
    Modified,
    #[strum(serialize = "T")]
    TypeChanged,
    #[strum(serialize = "A")]
    Added,
    #[strum(serialize = "D")]
    Deleted,
    #[strum(serialize = "R")]
    Renamed,
    #[strum(serialize = "C")]
    Copied,
    #[strum(serialize = "U")]
    Unmerged,
}

/// What `git status --porcelain=v2 --branch` says of a checkout.
#[derive(Debug, Default, PartialEq, Eq)]
struct Porcelain {
    /// `None` in a checkout without a commit.
    head: Option<CommitHash>,
    /// `None` on a detached HEAD.
    branch: Option<String>,
    upstream: Option<String>,
    /// Missing when the upstream is gone.
    ahead_behind: Option<AheadBehind>,
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AheadBehind {
    ahead: u32,
    behind: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// From the checkout's top folder, as git writes it.
    path: Vec<u8>,
    change: FileChange,
}

/// The state of each checkout of the agent's: the one that holds `folder`, its folder, then those
/// that hold a file of `touched`. A checkout whose status fails is left out, unless all fail.
/// `touched` and `home` are as for `agent.read_file`.
pub(crate) async fn status(
    folder: PathBuf,
    touched: Vec<PathBuf>,
    home: Option<PathBuf>,
) -> Result<GitStatus, GitError> {
    let deadline = Instant::now() + STATUS_TIME;
    let agent_folder = folder.clone();
    let (access, roots) = blocking::run(deadline, Scope::Folder(&agent_folder), move || {
        let access = Access::new(&folder, &touched, home.as_deref());
        let roots = checkout::status_roots(&folder, &touched, home.as_deref());
        (access, roots)
    })
    .await?;
    let tasks: Vec<_> = roots
        .into_iter()
        .map(|root| tokio::spawn(checkout_state(access.clone(), root, deadline)))
        .collect();
    let mut checkouts: Vec<CheckoutState> = Vec::new();
    let mut first_error: Option<GitError> = None;
    for task in tasks {
        match task.await.map_err(GitError::from).and_then(|state| state) {
            Ok(state) => checkouts.push(state),
            Err(error) => {
                tracing::warn!(%error, "cannot read a checkout's git status");
                first_error.get_or_insert(error);
            }
        }
    }
    match first_error {
        Some(error) if checkouts.is_empty() => Err(error),
        Some(_) | None => Ok(GitStatus { checkouts }),
    }
}

/// The state of the checkout whose top folder is `root`.
async fn checkout_state(
    access: Access,
    root: PathBuf,
    deadline: Instant,
) -> Result<CheckoutState, GitError> {
    let filters_off = filters_off(&root, deadline).await?;
    let status = Git::with_config(&root, &filters_off)
        .args(STATUS_OPTIONS)
        .run(deadline, MAX_STATUS_BYTES);
    let diff = Git::with_config(&root, &filters_off)
        .args(DIFF_OPTIONS)
        .arg(HEAD)
        .arg(END_OF_OPTIONS)
        .run(deadline, MAX_DIFF_BYTES);
    let (status, diff) = tokio::join!(status, diff);
    let status = status?;
    if !status.cut && status.code != Some(0) {
        return Err(status.failure());
    }
    let porcelain = parse_porcelain(command::whole_fields(&status.stdout));
    // A checkout without a commit has no HEAD to diff against, and no counts.
    let counts = match diff {
        Ok(diff) if diff.cut || diff.code == Some(0) => {
            numstat::by_path(command::whole_fields(&diff.stdout))
        }
        Ok(_) | Err(_) => HashMap::new(),
    };
    let head = match &porcelain.head {
        Some(hash) => Some(commit::read_one(&root, hash, deadline).await?.commit),
        None => None,
    };
    let mut files: Vec<FileStatus> = Vec::new();
    for entry in &porcelain.entries {
        // An answer carries paths as text.
        let relative = read_file::git_path(&entry.path).filter(|path| path.to_str().is_some());
        let Some(relative) = relative else {
            continue;
        };
        files.push(FileStatus {
            path: root.join(relative),
            change: entry.change,
            counts: counts.get(&entry.path).copied().flatten(),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let completeness = if status.cut || files.len() > MAX_STATUS_FILES {
        Completeness::Truncated
    } else {
        Completeness::Complete
    };
    files.truncate(MAX_STATUS_FILES);
    let upstream = match (porcelain.upstream, porcelain.ahead_behind) {
        (Some(name), Some(AheadBehind { ahead, behind })) => Some(Upstream {
            name,
            ahead,
            behind,
        }),
        _ => None,
    };
    let checkout = root.clone();
    let (root, files, remote, kind) =
        blocking::run(deadline, Scope::Folder(&checkout), move || {
            count_untracked(&access, &mut files);
            let remote = repository::linked_remote(&root);
            let kind = if repository::is_linked_worktree(&root) {
                CheckoutKind::LinkedWorktree
            } else {
                CheckoutKind::MainWorktree
            };
            (root, files, remote, kind)
        })
        .await?;
    let remote = match remote {
        Some(remote) => Some(remote_state(&root, remote, head.as_ref(), deadline).await),
        None => None,
    };
    Ok(CheckoutState {
        root,
        remote,
        kind,
        branch: BranchState {
            name: porcelain.branch,
            head,
            upstream,
        },
        files,
        completeness,
    })
}

/// `remote` of the checkout at `root`, with the commits of `head` it lacks. When git cannot list
/// them, every commit may be unpushed, so that the app links to none.
async fn remote_state(
    root: &Path,
    remote: ConfigRemote,
    head: Option<&Commit>,
    deadline: Instant,
) -> Remote {
    let unpushed = match head {
        Some(head) => commit::unpushed(root, &head.hash, &remote.name, deadline).await,
        None => Ok(Unpushed {
            commits: Vec::new(),
            completeness: Completeness::Complete,
        }),
    };
    let unpushed = unpushed.unwrap_or_else(|error| {
        tracing::warn!(%error, "cannot list the commits a remote lacks");
        Unpushed::UNKNOWN
    });
    Remote {
        repository: Repository::from_remote(&remote.url),
        name: remote.name,
        unpushed: unpushed.commits,
        unpushed_completeness: unpushed.completeness,
    }
}

/// The settings that turn off each filter the config of the checkout at `root` defines, so that
/// git runs none. A filter `git -c` cannot name refuses the checkout.
async fn filters_off(root: &Path, deadline: Instant) -> Result<Vec<String>, GitError> {
    let output = Git::new(root)
        .args(LIST_FILTERS)
        .run(deadline, MAX_CONFIG_BYTES)
        .await?;
    if output.code == Some(NO_SUCH_KEY) {
        return Ok(Vec::new());
    }
    let listed = output.into_success()?;
    let mut settings: Vec<String> = Vec::new();
    for name in filter_names(&listed) {
        let name = match std::str::from_utf8(name) {
            Ok(name) if !name.contains(SETTING_SEPARATOR) => name,
            Ok(_) | Err(_) => {
                return Err(GitError::Filter {
                    checkout: root.to_owned(),
                    name: String::from_utf8_lossy(name).into_owned(),
                });
            }
        };
        let dot = char::from(KEY_SEPARATOR);
        for (key, value) in FILTER_OFF {
            settings.push(format!(
                "{FILTER_SECTION}{dot}{name}{dot}{key}{SETTING_SEPARATOR}{value}"
            ));
        }
    }
    Ok(settings)
}

/// The filter names in `git config -z --get-regexp` output: each key is
/// `filter.<name>.<variable>`.
fn filter_names(listed: &[u8]) -> BTreeSet<&[u8]> {
    let mut names: BTreeSet<&[u8]> = BTreeSet::new();
    for entry in listed.split(|byte| *byte == FIELD_END) {
        let key = entry
            .split(|byte| *byte == VALUE_START)
            .next()
            .unwrap_or(entry);
        let rest = key
            .strip_prefix(FILTER_SECTION.as_bytes())
            .and_then(|rest| rest.strip_prefix(&[KEY_SEPARATOR]));
        let Some(rest) = rest else {
            continue;
        };
        let name = rest
            .iter()
            .rposition(|byte| *byte == KEY_SEPARATOR)
            .and_then(|end| rest.get(..end));
        if let Some(name) = name {
            names.insert(name);
        }
    }
    names
}

/// What `git status --porcelain=v2 -z --branch` output says. An entry that does not parse, as
/// from a later git, is left out.
fn parse_porcelain(written: &[u8]) -> Porcelain {
    let mut porcelain = Porcelain::default();
    let mut fields = written.split(|byte| *byte == FIELD_END);
    while let Some(field) = fields.next() {
        let mut words = field.splitn(2, |byte| *byte == WORD_END);
        let (Some(kind), Some(rest)) = (words.next(), words.next()) else {
            continue;
        };
        let kind: Option<EntryKind> = std::str::from_utf8(kind)
            .ok()
            .and_then(|kind| kind.parse().ok());
        let Some(kind) = kind else {
            continue;
        };
        match kind {
            EntryKind::Header => read_header(&mut porcelain, rest),
            EntryKind::Ordinary | EntryKind::RenamedOrCopied | EntryKind::Unmerged => {
                if kind == EntryKind::RenamedOrCopied {
                    let _original_path = fields.next();
                }
                let mut parts = rest.splitn(kind.fields(), |byte| *byte == WORD_END);
                let codes = parts.next();
                let path = parts.last();
                if let (Some(codes), Some(path)) = (codes, path)
                    && let Some(change) = change_of(kind, codes)
                {
                    porcelain.entries.push(Entry {
                        path: path.to_vec(),
                        change,
                    });
                }
            }
            EntryKind::Untracked => porcelain.entries.push(Entry {
                path: rest.to_vec(),
                change: FileChange::Untracked,
            }),
            EntryKind::Ignored => {}
        }
    }
    porcelain
}

fn read_header(porcelain: &mut Porcelain, rest: &[u8]) {
    let rest = String::from_utf8_lossy(rest);
    let Some((key, value)) = rest.split_once(char::from(WORD_END)) else {
        return;
    };
    let Ok(key) = key.parse::<BranchHeader>() else {
        return;
    };
    match key {
        // `(initial)` in a checkout without a commit.
        BranchHeader::Commit => porcelain.head = CommitHash::new(value),
        BranchHeader::Name => porcelain.branch = (value != DETACHED).then(|| value.to_owned()),
        BranchHeader::Upstream => porcelain.upstream = Some(value.to_owned()),
        BranchHeader::AheadBehind => {
            porcelain.ahead_behind =
                value
                    .split_once(char::from(WORD_END))
                    .and_then(|(ahead, behind)| {
                        Some(AheadBehind {
                            ahead: ahead.strip_prefix(AHEAD_SIGN)?.parse().ok()?,
                            behind: behind.strip_prefix(BEHIND_SIGN)?.parse().ok()?,
                        })
                    });
        }
    }
}

/// How an entry's file differs from the last commit, by its `XY`.
fn change_of(kind: EntryKind, codes: &[u8]) -> Option<FileChange> {
    let codes = std::str::from_utf8(codes).ok()?;
    let index: Code = codes.get(..1)?.parse().ok()?;
    let tree: Code = codes.get(1..2)?.parse().ok()?;
    let either = |code: Code| index == code || tree == code;
    let change = if kind == EntryKind::Unmerged {
        FileChange::Conflicted
    } else if either(Code::Deleted) {
        FileChange::Deleted
    } else if either(Code::Renamed) {
        FileChange::Renamed
    } else if either(Code::Added) || index == Code::Copied {
        FileChange::Added
    } else {
        FileChange::Modified
    };
    Some(change)
}

/// Counts the lines of each untracked text file of `files`, read with the checks of
/// `agent.read_file`, until [`MAX_COUNTED_BYTES`] are read.
fn count_untracked(access: &Access, files: &mut [FileStatus]) {
    let mut left = MAX_COUNTED_BYTES;
    let untracked = files
        .iter_mut()
        .filter(|file| file.change == FileChange::Untracked);
    for file in untracked {
        // An image reads whole, to be left uncounted.
        let is_image = file
            .path
            .to_str()
            .is_some_and(|path| ImageMediaType::from_file_name(path).is_some());
        if is_image || access.check_before_open(&file.path).is_err() {
            continue;
        }
        // Not followed: an untracked link counts nothing.
        let Ok(metadata) = std::fs::symlink_metadata(&file.path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES.min(left) {
            continue;
        }
        left -= metadata.len();
        // git counts no lines in a file with a NUL byte.
        if let Ok(Content::Text(text)) = read_file::read(access, &file.path)
            && !text.contains('\0')
        {
            let lines = u32::try_from(text.lines().count()).unwrap_or(u32::MAX);
            file.counts = Some(LineCounts {
                added: lines,
                removed: 0,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::Timestamp;

    use super::*;
    use crate::git::test_repo::TestRepo;

    const HEAD_HASH: &str = "96fd0396a5ffa4370ae53fddb83ac55bdaedd5c5";
    const BLOB: &str = "1a2f0420651fb28f622f39efa0f7852a66d1eaef";
    const ZERO: &str = "0000000000000000000000000000000000000000";

    fn entry(path: &str, change: FileChange) -> Entry {
        Entry {
            path: path.as_bytes().to_vec(),
            change,
        }
    }

    #[test]
    fn porcelain_gives_the_branch_and_each_entry() {
        let written = format!(
            "# branch.oid {HEAD_HASH}\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +1 -2\x001 .M N... 100644 100644 100644 {BLOB} {BLOB} src/a b.rs\x001 .D N... 100644 100644 000000 {BLOB} {BLOB} gone.bin\x002 R. N... 100644 100644 100644 {BLOB} {BLOB} R100 new name.txt\0old name.txt\x001 A. N... 000000 100644 100644 {ZERO} {BLOB} staged.txt\0u UU N... 100644 100644 100644 100644 {BLOB} {BLOB} {BLOB} both.txt\0? deep/er/f.txt\0! target/out\0"
        );

        let porcelain = parse_porcelain(command::whole_fields(written.as_bytes()));

        assert_eq!(
            porcelain,
            Porcelain {
                head: CommitHash::new(HEAD_HASH),
                branch: Some("main".to_owned()),
                upstream: Some("origin/main".to_owned()),
                ahead_behind: Some(AheadBehind {
                    ahead: 1,
                    behind: 2,
                }),
                entries: vec![
                    entry("src/a b.rs", FileChange::Modified),
                    entry("gone.bin", FileChange::Deleted),
                    entry("new name.txt", FileChange::Renamed),
                    entry("staged.txt", FileChange::Added),
                    entry("both.txt", FileChange::Conflicted),
                    entry("deep/er/f.txt", FileChange::Untracked),
                ],
            }
        );
    }

    #[test]
    fn a_checkout_without_a_commit_or_on_a_detached_head_has_no_branch() {
        let initial = parse_porcelain(b"# branch.oid (initial)\0# branch.head main");
        let detached = parse_porcelain(
            format!("# branch.oid {HEAD_HASH}\0# branch.head (detached)\0# stash 2").as_bytes(),
        );

        assert_eq!(initial.head, None);
        assert_eq!(initial.branch, Some("main".to_owned()));
        assert_eq!(detached.head, CommitHash::new(HEAD_HASH));
        assert_eq!(detached.branch, None);
    }

    #[test]
    fn each_filter_the_config_names_is_listed_once() {
        let listed = b"filter.lfs.clean\ngit-lfs clean -- %f\0filter.lfs.required\ntrue\0filter.a.b.smudge\ncat\0filter.bare\0filter.flag.clean\0";

        let names: Vec<&[u8]> = filter_names(listed).into_iter().collect();

        assert_eq!(names, [b"a.b".as_slice(), b"flag", b"lfs"]);
    }

    async fn git_status(repo: &TestRepo, touched: &[PathBuf]) -> Vec<CheckoutState> {
        status(
            repo.root.clone(),
            touched.to_vec(),
            Some(repo.home().to_owned()),
        )
        .await
        .unwrap()
        .checkouts
    }

    fn file(path: PathBuf, change: FileChange, counts: Option<(u32, u32)>) -> FileStatus {
        FileStatus {
            path,
            change,
            counts: counts.map(|(added, removed)| LineCounts { added, removed }),
        }
    }

    #[tokio::test]
    async fn a_checkout_lists_its_changed_files_by_path_with_their_counts() {
        let repo = TestRepo::new();
        repo.write("a.txt", "one\ntwo\n");
        repo.write("old.txt", "x\ny\n");
        repo.write("gone.txt", "gone\n");
        let head = repo.commit("Start\n\nThe body.\n");
        repo.write("a.txt", "one\nTWO\nthree\n");
        repo.git(&["mv", "old.txt", "new.txt"]);
        std::fs::remove_file(repo.root.join("gone.txt")).unwrap();
        repo.write("staged.txt", "staged\n");
        repo.git(&["add", "staged.txt"]);
        repo.write("dir/untracked.txt", "first\nsecond");
        std::fs::write(repo.root.join("blob.dat"), b"\0\x01").unwrap();

        let checkouts = git_status(&repo, &[]).await;

        let [state] = checkouts.as_slice() else {
            panic!("one checkout, not {checkouts:?}");
        };
        assert_eq!(state.root, repo.root);
        assert_eq!(state.remote, None);
        assert_eq!(state.kind, CheckoutKind::MainWorktree);
        assert_eq!(state.branch.name.as_deref(), Some("main"));
        let commit = state.branch.head.as_ref().unwrap();
        assert_eq!((&commit.hash, commit.body.as_str()), (&head, "The body."));
        assert_eq!(state.branch.upstream, None);
        let root = &repo.root;
        assert_eq!(
            state.files,
            [
                file(root.join("a.txt"), FileChange::Modified, Some((2, 1))),
                file(root.join("blob.dat"), FileChange::Untracked, None),
                file(
                    root.join("dir").join("untracked.txt"),
                    FileChange::Untracked,
                    Some((2, 0))
                ),
                file(root.join("gone.txt"), FileChange::Deleted, Some((0, 1))),
                file(root.join("new.txt"), FileChange::Renamed, Some((0, 0))),
                file(root.join("staged.txt"), FileChange::Added, Some((1, 0))),
            ]
        );
        assert_eq!(state.completeness, Completeness::Complete);
    }

    #[tokio::test]
    async fn a_checkout_without_a_commit_counts_its_untracked_files() {
        let repo = TestRepo::new();
        repo.write("notes.txt", "one\ntwo\n");

        let checkouts = git_status(&repo, &[]).await;

        let [state] = checkouts.as_slice() else {
            panic!("one checkout, not {checkouts:?}");
        };
        assert_eq!(state.branch.name.as_deref(), Some("main"));
        assert_eq!(state.branch.head, None);
        assert_eq!(
            state.files,
            [file(
                repo.root.join("notes.txt"),
                FileChange::Untracked,
                Some((2, 0))
            )]
        );
    }

    #[tokio::test]
    async fn a_clone_has_an_upstream_and_a_worktree_or_a_detached_head_says_so() {
        let origin = TestRepo::new();
        origin.write("a.txt", "a\n");
        origin.commit("Start");
        let clone = origin.clone_beside("clone");
        clone.write("b.txt", "b\n");
        let local = clone.commit("Local");
        origin.write("c.txt", "c\n");
        let upstream_head = origin.commit("Upstream");
        clone.git(&["fetch", "-q"]);
        let feature = origin.worktree_beside("feature", "feature");
        origin.git(&["checkout", "-q", "--detach"]);

        let cloned = git_status(&clone, &[]).await;
        let worktree = git_status(&feature, &[]).await;
        let detached = git_status(&origin, &[]).await;

        assert_eq!(
            cloned[0].branch.upstream,
            Some(Upstream {
                name: "origin/main".to_owned(),
                ahead: 1,
                behind: 1,
            })
        );
        let remote = cloned[0].remote.as_ref().unwrap();
        assert_eq!(remote.name, "origin");
        assert_eq!(remote.unpushed, [local], "the cloned commit is on origin");
        assert_eq!(remote.unpushed_completeness, Completeness::Complete);
        assert_eq!(cloned[0].kind, CheckoutKind::MainWorktree);
        assert_eq!(worktree[0].kind, CheckoutKind::LinkedWorktree);
        assert_eq!(worktree[0].branch.name.as_deref(), Some("feature"));
        assert_eq!(detached[0].branch.name, None);
        let head = detached[0].branch.head.as_ref().map(|head| &head.hash);
        assert_eq!(head, Some(&upstream_head));
    }

    #[tokio::test]
    async fn a_remote_whose_commits_git_cannot_list_may_lack_any_commit() {
        let url = "git@github.com:acme/app.git";
        let remote = ConfigRemote {
            name: "origin".to_owned(),
            url: url.to_owned(),
        };
        let head = Commit {
            hash: CommitHash::new(HEAD_HASH).unwrap(),
            title: "Start".to_owned(),
            body: String::new(),
            author: "Ann Example".to_owned(),
            time: Timestamp::from_unix_millis(0),
        };
        let deadline = Instant::now() + Duration::from_secs(5);

        let state = remote_state(Path::new("/nowhere"), remote, Some(&head), deadline).await;

        assert_eq!(
            state,
            Remote {
                name: "origin".to_owned(),
                repository: Repository::from_remote(url),
                unpushed: Vec::new(),
                unpushed_completeness: Completeness::Truncated,
            }
        );
    }

    #[tokio::test]
    async fn a_conflict_shows_and_a_checkout_the_agent_worked_in_comes_after_its_own() {
        let repo = TestRepo::new();
        repo.write("f.txt", "base\n");
        repo.commit("Base");
        repo.git(&["checkout", "-q", "-b", "side"]);
        repo.write("f.txt", "side\n");
        repo.commit("Side");
        repo.git(&["checkout", "-q", "main"]);
        repo.write("f.txt", "main\n");
        repo.commit("Main");
        assert!(
            !repo.try_git(&["merge", "-q", "side"]),
            "the merge conflicts"
        );
        let lib = repo.beside("lib");
        lib.write("x.txt", "x\n");

        let checkouts = git_status(&repo, &[lib.root.join("x.txt")]).await;

        let roots: Vec<&Path> = checkouts.iter().map(|state| state.root.as_path()).collect();
        assert_eq!(roots, [repo.root.as_path(), lib.root.as_path()]);
        let changes: Vec<FileChange> = checkouts[0].files.iter().map(|file| file.change).collect();
        assert_eq!(changes, [FileChange::Conflicted]);
        assert_eq!(checkouts[1].files[0].change, FileChange::Untracked);
    }

    /// A path that is not UTF-8 cannot go in an answer, which carries paths as text.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_whose_name_is_not_utf8_is_left_out() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let repo = TestRepo::new();
        repo.write("notes.txt", "one\n");
        let latin1 = OsStr::from_bytes(b"caf\xe9.txt");
        std::fs::write(repo.root.join(latin1), "two\n").unwrap();

        let checkouts = git_status(&repo, &[]).await;

        assert!(serde_json::to_vec(&checkouts).is_ok());
        let paths: Vec<&Path> = checkouts[0]
            .files
            .iter()
            .map(|file| file.path.as_path())
            .collect();
        assert_eq!(paths, [repo.root.join("notes.txt")]);
    }

    #[test]
    fn porcelain_words_are_pinned() {
        let kinds: Vec<&str> = EntryKind::VARIANTS
            .iter()
            .map(|&kind| kind.into())
            .collect();
        let headers: Vec<&str> = BranchHeader::VARIANTS
            .iter()
            .map(|&header| header.into())
            .collect();
        let codes: Vec<&str> = Code::VARIANTS.iter().map(|&code| code.into()).collect();

        assert_eq!(kinds, ["#", "1", "2", "u", "?", "!"]);
        assert_eq!(
            headers,
            ["branch.oid", "branch.head", "branch.upstream", "branch.ab"]
        );
        assert_eq!(codes, [".", "M", "T", "A", "D", "R", "C", "U"]);
    }
}
