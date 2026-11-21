//! `agent.git_blame`: who last changed each line of a file's last committed text.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use cockpit_protocol::{BlameRun, CommitHash, GitBlame};
use tokio::time::Instant;

use crate::git::command::{END_OF_OPTIONS, Git, HEAD, NO_TEXTCONV};
use crate::git::{GitError, blob, checkout, commit};
use crate::read_file::MAX_FILE_BYTES;

/// How long a blame may take, with the commits it names. It walks the file's history.
const BLAME_TIME: Duration = Duration::from_secs(10);
/// Blames a commit's file in groups of lines, each starting with `<hash> <line in that commit>
/// <line in the file> <count>`, in any order. Without textconv, the lines are the blob's.
const BLAME: [&str; 3] = ["blame", "--incremental", NO_TEXTCONV];
const WORD_END: char = ' ';
const LINE_END: u8 = b'\n';
const MAX_BLAME_BYTES: usize = 16 * 1024 * 1024;

/// The blame of the file `path` names, absolute or relative to `folder`, in the last commit of
/// its checkout. `touched` and `home` are as for `agent.read_file`.
pub(crate) async fn blame(
    folder: PathBuf,
    touched: Vec<PathBuf>,
    home: Option<PathBuf>,
    path: PathBuf,
) -> Result<GitBlame, GitError> {
    let deadline = Instant::now() + BLAME_TIME;
    let asked = folder.join(&path);
    let located = checkout::locate_in_task(folder, touched, home, path).await?;
    let root = located.root.as_path();
    let Some(head) = commit::resolve(root, HEAD, deadline).await? else {
        return Err(GitError::NoCommit(located.root));
    };
    let Some(blob) = blob::find(root, &head, &located.path, deadline).await? else {
        return Err(GitError::NotCommitted(asked));
    };
    if blob.is_larger_than(MAX_FILE_BYTES) {
        return Err(GitError::FileTooLarge(MAX_FILE_BYTES));
    }
    // An explicit commit: blame without one reads the working tree.
    let written = Git::new(root)
        .args(BLAME)
        .arg(head.as_str())
        .arg(END_OF_OPTIONS)
        .arg(&located.path)
        .run(deadline, MAX_BLAME_BYTES)
        .await?
        .into_success()?;
    let (hashes, runs) = runs_of(&written)?;
    let mut records = commit::read(root, &hashes, deadline).await?;
    let mut commits = Vec::with_capacity(hashes.len());
    for hash in &hashes {
        let record = records
            .remove(hash)
            .ok_or(GitError::Unparsable("the commits of a blame"))?;
        commits.push(record.commit);
    }
    Ok(GitBlame {
        head,
        commits,
        runs,
    })
}

/// A group of lines that `git blame --incremental` gives one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Group {
    commit: CommitHash,
    start: u32,
    count: u32,
}

/// The runs of `git blame --incremental` output, in the order of the text and without gaps, and
/// the commits they name, in the order they first name them. Neighbouring groups of one commit
/// make one run.
fn runs_of(written: &[u8]) -> Result<(Vec<CommitHash>, Vec<BlameRun>), GitError> {
    let unparsable = || GitError::Unparsable("the lines of a blame");
    let mut groups: Vec<Group> = written
        .split(|byte| *byte == LINE_END)
        .filter_map(group_start)
        .collect();
    groups.sort_by_key(|group| group.start);
    let mut hashes: Vec<CommitHash> = Vec::new();
    let mut index_of: HashMap<CommitHash, u32> = HashMap::new();
    let mut runs: Vec<BlameRun> = Vec::new();
    let mut next_line: u32 = 1;
    for group in groups {
        if group.start != next_line || group.count == 0 {
            return Err(unparsable());
        }
        next_line = group
            .start
            .checked_add(group.count)
            .ok_or_else(unparsable)?;
        let index = match index_of.get(&group.commit) {
            Some(index) => *index,
            None => {
                let index = u32::try_from(hashes.len()).map_err(|_| unparsable())?;
                index_of.insert(group.commit.clone(), index);
                hashes.push(group.commit);
                index
            }
        };
        match runs.last_mut() {
            Some(last) if last.commit == index => {
                last.count = last.count.saturating_add(group.count);
            }
            _ => runs.push(BlameRun {
                start: group.start,
                count: group.count,
                commit: index,
            }),
        }
    }
    Ok((hashes, runs))
}

/// The group a line of `git blame --incremental` output starts. `None` for the lines about its
/// commit that follow, each of which starts with a word such as `author`.
fn group_start(line: &[u8]) -> Option<Group> {
    let line = std::str::from_utf8(line).ok()?.trim_end();
    let mut words = line.split(WORD_END);
    let (Some(hash), Some(source), Some(start), Some(count), None) = (
        words.next(),
        words.next(),
        words.next(),
        words.next(),
        words.next(),
    ) else {
        return None;
    };
    let _source_line: u32 = source.parse().ok()?;
    Some(Group {
        commit: CommitHash::new(hash)?,
        start: start.parse().ok()?,
        count: count.parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::ErrorCode;

    use super::*;
    use crate::git::test_repo::TestRepo;

    const FIRST: &str = "2900a3c40cd49528625c63252bf0414b34bbbb03";
    const SECOND: &str = "7e3dd57776ee829c9d96b8a4a78ec95c46d4db4e";

    fn hash(text: &str) -> CommitHash {
        CommitHash::new(text).unwrap()
    }

    fn run(start: u32, count: u32, commit: u32) -> BlameRun {
        BlameRun {
            start,
            count,
            commit,
        }
    }

    #[test]
    fn groups_in_any_order_make_runs_in_the_order_of_the_text() {
        let written = format!(
            "{SECOND} 2 2 1\nauthor Ann Example\nauthor-time 1700000000\nsummary {FIRST} 1 1 1\nprevious {FIRST} notes.txt\nfilename notes.txt\n{FIRST} 1 1 1\nauthor Bob Example\nsummary Start\nboundary\nfilename notes.txt\n{FIRST} 3 3 2\nfilename notes.txt\n{SECOND} 5 5 1\nfilename notes.txt\n{SECOND} 6 6 3\nfilename notes.txt\n"
        );

        let (hashes, runs) = runs_of(written.as_bytes()).unwrap();

        assert_eq!(hashes, [hash(FIRST), hash(SECOND)]);
        assert_eq!(
            runs,
            [run(1, 1, 0), run(2, 1, 1), run(3, 2, 0), run(5, 4, 1)]
        );
    }

    #[test]
    fn a_gap_or_an_overlap_does_not_parse() {
        let gap = format!("{FIRST} 1 1 1\n{FIRST} 3 3 1\n");
        let overlap = format!("{FIRST} 1 1 2\n{SECOND} 2 2 1\n");

        assert!(runs_of(gap.as_bytes()).is_err());
        assert!(runs_of(overlap.as_bytes()).is_err());
        assert_eq!(runs_of(b"").unwrap(), (Vec::new(), Vec::new()));
    }

    async fn git_blame(repo: &TestRepo, path: &str) -> Result<GitBlame, GitError> {
        blame(
            repo.root.clone(),
            Vec::new(),
            Some(repo.home().to_owned()),
            PathBuf::from(path),
        )
        .await
    }

    #[tokio::test]
    async fn each_line_of_the_last_commit_names_the_commit_that_changed_it() {
        let repo = TestRepo::new();
        repo.write("notes.txt", "one\ntwo\nthree\n");
        let first = repo.commit("Start\n\nThree lines.\n");
        repo.write("notes.txt", "one\nTWO\nthree\nfour\n");
        let second = repo.commit("Shout two");
        repo.write("notes.txt", "not committed\n");

        let blamed = git_blame(&repo, "notes.txt").await.unwrap();

        assert_eq!(blamed.head, second);
        let hashes: Vec<&CommitHash> = blamed.commits.iter().map(|commit| &commit.hash).collect();
        assert_eq!(hashes, [&first, &second]);
        assert_eq!(blamed.commits[0].body, "Three lines.");
        assert_eq!(
            blamed.runs,
            [run(1, 1, 0), run(2, 1, 1), run(3, 1, 0), run(4, 1, 1)]
        );
    }

    #[tokio::test]
    async fn a_file_not_in_the_last_commit_has_no_blame() {
        let repo = TestRepo::new();
        repo.write("notes.txt", "one\n");
        let no_commit = git_blame(&repo, "notes.txt").await;
        repo.commit("Start");
        repo.write("new.txt", "new\n");
        let loose = repo.home().join("loose.txt");
        std::fs::write(&loose, "loose\n").unwrap();

        let new = git_blame(&repo, "new.txt").await;
        let outside = git_blame(&repo, loose.to_str().unwrap()).await;

        assert!(
            matches!(no_commit, Err(GitError::NoCommit(_))),
            "{no_commit:?}"
        );
        let error = new.unwrap_err().to_error_body();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(
            error.message,
            format!(
                "{} is not in the last commit",
                repo.root.join("new.txt").display()
            )
        );
        assert!(matches!(outside, Err(GitError::Outside(_))), "{outside:?}");
    }

    #[tokio::test]
    async fn an_empty_file_has_no_runs() {
        let repo = TestRepo::new();
        repo.write("empty.txt", "");
        let head = repo.commit("Add an empty file");

        let blamed = git_blame(&repo, "empty.txt").await.unwrap();

        assert_eq!(
            blamed,
            GitBlame {
                head,
                commits: Vec::new(),
                runs: Vec::new(),
            }
        );
    }
}
