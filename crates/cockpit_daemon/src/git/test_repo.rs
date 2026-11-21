//! Small git checkouts for the tests, made with the git command line in a temporary folder.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cockpit_protocol::CommitHash;
use tempfile::TempDir;

use crate::read_file::{self, GIT_PROGRAM};

/// The author time of every test commit.
pub(super) const TIME_MILLIS: u64 = 1_700_000_000_000;
const TIME: &str = "1700000000 +0000";
const TIME_ENV: [&str; 2] = ["GIT_AUTHOR_DATE", "GIT_COMMITTER_DATE"];
/// Every git command a test runs ignores these settings of the user's own.
const SETTINGS: [&str; 10] = [
    "-c",
    "user.name=Ann Example",
    "-c",
    "user.email=ann@example.invalid",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "core.autocrlf=false",
    "-c",
    "init.defaultBranch=main",
];

/// A checkout in a temporary folder that stands for the home folder, so that a checkout above
/// it, as when the system's temporary folder sits in one, never counts.
pub(super) struct TestRepo {
    /// The folder that holds the checkouts. The first checkout owns it.
    _temp: Option<TempDir>,
    home: PathBuf,
    /// The top folder, with symlinks resolved.
    pub(super) root: PathBuf,
}

impl TestRepo {
    /// A checkout `app`, without a commit.
    pub(super) fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = read_file::canonical(temp.path()).unwrap();
        let repo = Self {
            root: home.join("app"),
            home,
            _temp: Some(temp),
        };
        repo.init();
        repo
    }

    /// Another checkout without a commit, beside this one.
    pub(super) fn beside(&self, name: &str) -> Self {
        let repo = self.sibling(name);
        repo.init();
        repo
    }

    /// A clone of this checkout, beside it.
    pub(super) fn clone_beside(&self, name: &str) -> Self {
        let repo = self.sibling(name);
        git_at(&self.home, &["clone", "-q"], &[&self.root, &repo.root]);
        repo
    }

    /// A clone of this checkout beside it that has fetched no file's content, and no working
    /// tree.
    pub(super) fn partial_clone_beside(&self, name: &str) -> Self {
        let repo = self.sibling(name);
        let path = self.root.to_str().unwrap().replace('\\', "/");
        // A local clone without a `file://` URL takes no filter.
        let url = format!("file:///{}", path.trim_start_matches('/'));
        let clone = [
            "clone",
            "-q",
            "--no-checkout",
            "--filter=blob:none",
            url.as_str(),
        ];
        git_at(&self.home, &clone, &[&repo.root]);
        repo
    }

    /// A linked worktree of this checkout on a new branch, beside it.
    pub(super) fn worktree_beside(&self, name: &str, branch: &str) -> Self {
        let repo = self.sibling(name);
        git_at(
            &self.root,
            &["worktree", "add", "-q", "-b", branch],
            &[&repo.root],
        );
        repo
    }

    fn sibling(&self, name: &str) -> Self {
        Self {
            _temp: None,
            home: self.home.clone(),
            root: self.home.join(name),
        }
    }

    fn init(&self) {
        std::fs::create_dir_all(&self.root).unwrap();
        self.git(&["init", "-q"]);
    }

    pub(super) fn home(&self) -> &Path {
        &self.home
    }

    /// Runs git in the checkout, which must succeed, and gives what it wrote.
    pub(super) fn git(&self, args: &[&str]) -> String {
        git_at(&self.root, args, &[])
    }

    /// Runs git in the checkout, and tells whether it succeeded.
    pub(super) fn try_git(&self, args: &[&str]) -> bool {
        run(&self.root, args, &[]).status.success()
    }

    /// Writes `text` at `relative`, creating its folders.
    pub(super) fn write(&self, relative: &str, text: &str) {
        let path = self.root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Commits every change, and gives the commit.
    pub(super) fn commit(&self, message: &str) -> CommitHash {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.head()
    }

    pub(super) fn head(&self) -> CommitHash {
        CommitHash::new(self.git(&["rev-parse", "HEAD"]).trim()).unwrap()
    }
}

/// Runs git in `folder` with `args` then `paths`, which must succeed, and gives what it wrote.
fn git_at(folder: &Path, args: &[&str], paths: &[&Path]) -> String {
    let output = run(folder, args, paths);
    assert!(
        output.status.success(),
        "git {args:?} {paths:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn run(folder: &Path, args: &[&str], paths: &[&Path]) -> Output {
    let mut git = Command::new(GIT_PROGRAM);
    git.args(SETTINGS)
        .args(args)
        .args(paths)
        .current_dir(folder);
    for variable in TIME_ENV {
        git.env(variable, TIME);
    }
    git.output().expect("the git tests need git")
}
