//! `agent.git_file`, a file's text in the last commit, and `agent.git_show`, a file before and
//! after one commit.

use std::path::{Path, PathBuf};
use std::time::Duration;

use cockpit_protocol::{
    CommitHash, CommitTexts, Completeness, GitCommitFile, GitFile, MAX_LINE_BYTES, OnRemote,
};
use tokio::time::Instant;

use crate::git::blob::{self, Blob, Content};
use crate::git::command::HEAD;
use crate::git::commit::Unpushed;
use crate::git::{GitError, checkout, commit};
use crate::repository;

/// How long the reads of one file's objects may take. The file view waits for them to show the
/// file's changes.
const READ_TIME: Duration = Duration::from_secs(5);
/// What an answer adds around a commit's texts, with room to spare.
const ANSWER_ENVELOPE_BYTES: usize = 64 * 1024;

/// The file `path` names, absolute or relative to `folder`, in the last commit of its checkout.
/// `touched` and `home` are as for `agent.read_file`.
pub(crate) async fn file(
    folder: PathBuf,
    touched: Vec<PathBuf>,
    home: Option<PathBuf>,
    path: PathBuf,
) -> Result<GitFile, GitError> {
    let deadline = Instant::now() + READ_TIME;
    let located = match checkout::locate_in_task(folder, touched, home, path).await {
        Ok(located) => located,
        Err(GitError::Outside(_)) => return Ok(GitFile::Outside),
        Err(error) => return Err(error),
    };
    let root = located.root.as_path();
    let head = match commit::resolve(root, HEAD, deadline).await {
        Ok(Some(head)) => head,
        Ok(None) => return Ok(GitFile::New { head: None }),
        // git does not take the folder for a checkout, or is not there.
        Err(GitError::NotInstalled | GitError::Failed(_)) => return Ok(GitFile::Outside),
        Err(error) => return Err(error),
    };
    let (record, blob) = tokio::join!(
        commit::read_one(root, &head, deadline),
        blob::find(root, &head, &located.path, deadline),
    );
    let head = record?.commit;
    let Some(blob) = blob? else {
        return Ok(GitFile::New { head: Some(head) });
    };
    match blob::read(root, &blob, deadline).await? {
        Content::Text(text) => Ok(GitFile::Committed { head, text }),
        Content::NotText => Ok(GitFile::NotText { head }),
    }
}

/// The file `path` names in `commit` and in its first parent, for the commit's diff of it.
pub(crate) async fn show(
    folder: PathBuf,
    touched: Vec<PathBuf>,
    home: Option<PathBuf>,
    path: PathBuf,
    commit: CommitHash,
) -> Result<GitCommitFile, GitError> {
    let deadline = Instant::now() + READ_TIME;
    let located = checkout::locate_in_task(folder, touched, home, path).await?;
    let root = located.root.as_path();
    let Some(hash) = commit::resolve(root, commit.as_str(), deadline).await? else {
        return Err(GitError::NoSuchCommit(commit));
    };
    let record = commit::read_one(root, &hash, deadline).await?;
    let before = async {
        match &record.first_parent {
            Some(parent) => blob::find(root, parent, &located.path, deadline).await,
            None => Ok(None),
        }
    };
    let (before, after) = tokio::join!(before, blob::find(root, &hash, &located.path, deadline));
    let (before, after) = (before?, after?);
    let (before, after) = tokio::join!(
        read_side(root, before.as_ref(), deadline),
        read_side(root, after.as_ref(), deadline),
    );
    let texts = match (before?, after?) {
        (Some(Content::NotText), _) | (_, Some(Content::NotText)) => None,
        (before, after) => Some((text_of(before), text_of(after))),
    };
    let shown = texts.filter(|(before, after)| fit_an_answer(before.as_deref(), after.as_deref()));
    let texts = match shown {
        Some((before, after)) => CommitTexts::Text { before, after },
        None => CommitTexts::NotText,
    };
    let on_remote = on_remote(root, &hash, deadline).await;
    Ok(GitCommitFile {
        commit: record.commit,
        texts,
        on_remote,
    })
}

/// Whether the remote that the checkout at `root` links to holds `hash`. When git cannot say, it
/// may lack it.
async fn on_remote(root: &Path, hash: &CommitHash, deadline: Instant) -> OnRemote {
    let folder = root.to_owned();
    let remote = match tokio::task::spawn_blocking(move || repository::linked_remote(&folder)).await
    {
        Ok(Some(remote)) => remote,
        Ok(None) => return OnRemote::MayLack,
        Err(error) => {
            tracing::warn!(%error, "cannot read a checkout's remote");
            return OnRemote::MayLack;
        }
    };
    // The remote holds the commit when it holds every commit the commit reaches.
    match commit::unpushed(root, hash, &remote.name, deadline).await {
        Ok(Unpushed {
            commits,
            completeness: Completeness::Complete,
        }) if commits.is_empty() => OnRemote::Held,
        Ok(_) => OnRemote::MayLack,
        Err(error) => {
            tracing::warn!(%error, "cannot tell whether a remote holds a commit");
            OnRemote::MayLack
        }
    }
}

/// One side of a commit's diff: `None` where the file is not.
async fn read_side(
    root: &Path,
    blob: Option<&Blob>,
    deadline: Instant,
) -> Result<Option<Content>, GitError> {
    match blob {
        Some(blob) => blob::read(root, blob, deadline).await.map(Some),
        None => Ok(None),
    }
}

fn text_of(content: Option<Content>) -> Option<String> {
    match content {
        Some(Content::Text(text)) => Some(text),
        Some(Content::NotText) | None => None,
    }
}

/// Whether both texts fit in one answer. A control character takes six bytes in JSON, so two
/// texts of the largest size may not.
fn fit_an_answer(before: Option<&str>, after: Option<&str>) -> bool {
    let json_bytes = |text: Option<&str>| {
        text.map_or(0, |text| {
            serde_json::to_vec(text).map_or(usize::MAX, |json| json.len())
        })
    };
    let room = MAX_LINE_BYTES.saturating_sub(ANSWER_ENVELOPE_BYTES);
    json_bytes(before).saturating_add(json_bytes(after)) <= room
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{Commit, Timestamp};

    use super::*;
    use crate::git::test_repo::{TIME_MILLIS, TestRepo};
    use crate::read_file::MAX_FILE_BYTES;

    async fn git_file(repo: &TestRepo, path: &Path) -> GitFile {
        file(
            repo.root.clone(),
            Vec::new(),
            Some(repo.home().to_owned()),
            path.to_owned(),
        )
        .await
        .unwrap()
    }

    async fn git_show(repo: &TestRepo, path: &str, commit: &CommitHash) -> GitCommitFile {
        let path = repo.root.join(path);
        show(
            repo.root.clone(),
            Vec::new(),
            Some(repo.home().to_owned()),
            path,
            commit.clone(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_committed_file_gives_its_text_and_the_last_commit() {
        let repo = TestRepo::new();
        repo.write("src/main.rs", "fn main() {}\n");
        repo.commit("Start");
        repo.write("src/main.rs", "fn main() {\n    run();\n}\n");
        let head = repo.commit("Run the app\n\nIt did nothing.\n");
        repo.write("src/main.rs", "changed on disk\n");

        let answer = git_file(&repo, &repo.root.join("src").join("main.rs")).await;

        assert_eq!(
            answer,
            GitFile::Committed {
                head: Commit {
                    hash: head,
                    title: "Run the app".to_owned(),
                    body: "It did nothing.".to_owned(),
                    author: "Ann Example".to_owned(),
                    time: Timestamp::from_unix_millis(TIME_MILLIS),
                },
                text: "fn main() {\n    run();\n}\n".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn a_file_since_the_last_commit_is_new_and_one_outside_a_checkout_is_outside() {
        let repo = TestRepo::new();
        repo.write("README.md", "readme\n");
        let empty = git_file(&repo, Path::new("README.md")).await;
        let head = repo.commit("Start");
        repo.write("notes.txt", "notes\n");
        let loose = repo.home().join("loose.txt");
        std::fs::write(&loose, "loose\n").unwrap();

        let new = git_file(&repo, Path::new("notes.txt")).await;
        let outside = git_file(&repo, &loose).await;

        assert_eq!(empty, GitFile::New { head: None });
        let GitFile::New { head: Some(commit) } = new else {
            panic!("a new file, not {new:?}");
        };
        assert_eq!(commit.hash, head);
        assert_eq!(outside, GitFile::Outside);
    }

    #[tokio::test]
    async fn a_binary_or_large_or_deleted_file_reads_from_the_commit() {
        let repo = TestRepo::new();
        std::fs::write(repo.root.join("logo.png"), b"\x89PNG\r\n\x1a\n\0\0").unwrap();
        let large = "line\n".repeat(usize::try_from(MAX_FILE_BYTES).unwrap() / 5 + 1);
        repo.write("large.txt", &large);
        repo.write("gone.txt", "kept in the commit\n");
        repo.commit("Add files");
        std::fs::remove_file(repo.root.join("gone.txt")).unwrap();

        let binary = git_file(&repo, Path::new("logo.png")).await;
        let too_large = git_file(&repo, Path::new("large.txt")).await;
        let deleted = git_file(&repo, Path::new("gone.txt")).await;

        assert!(matches!(binary, GitFile::NotText { .. }), "{binary:?}");
        assert!(
            matches!(too_large, GitFile::NotText { .. }),
            "{too_large:?}"
        );
        let GitFile::Committed { text, .. } = deleted else {
            panic!("a committed file, not {deleted:?}");
        };
        assert_eq!(text, "kept in the commit\n");
    }

    #[tokio::test]
    async fn a_path_that_goes_up_is_refused() {
        let repo = TestRepo::new();

        let refused = file(
            repo.root.clone(),
            Vec::new(),
            Some(repo.home().to_owned()),
            PathBuf::from("../notes.txt"),
        )
        .await;

        assert!(matches!(refused, Err(GitError::GoesUp)), "{refused:?}");
    }

    #[tokio::test]
    async fn a_commit_shows_the_file_before_and_after_it() {
        let repo = TestRepo::new();
        repo.write("notes.txt", "one\n");
        let added = repo.commit("Add notes");
        repo.write("notes.txt", "one\ntwo\n");
        let changed = repo.commit("Add a line");
        std::fs::remove_file(repo.root.join("notes.txt")).unwrap();
        let deleted = repo.commit("Remove notes");

        let first = git_show(&repo, "notes.txt", &added).await;
        let middle = git_show(&repo, "notes.txt", &changed).await;
        let last = git_show(&repo, "notes.txt", &deleted).await;

        assert_eq!(
            first.texts,
            CommitTexts::Text {
                before: None,
                after: Some("one\n".to_owned()),
            }
        );
        assert_eq!(middle.commit.title, "Add a line");
        assert_eq!(
            middle.texts,
            CommitTexts::Text {
                before: Some("one\n".to_owned()),
                after: Some("one\ntwo\n".to_owned()),
            }
        );
        assert_eq!(
            last.texts,
            CommitTexts::Text {
                before: Some("one\ntwo\n".to_owned()),
                after: None,
            }
        );
    }

    #[tokio::test]
    async fn a_shown_commit_says_whether_the_remote_holds_it() {
        let origin = TestRepo::new();
        origin.write("notes.txt", "one\n");
        let cloned = origin.commit("Start");
        let clone = origin.clone_beside("clone");
        clone.write("notes.txt", "one\ntwo\n");
        let local = clone.commit("Add a line");

        let held = git_show(&clone, "notes.txt", &cloned).await;
        let lacking = git_show(&clone, "notes.txt", &local).await;
        let without_remote = git_show(&origin, "notes.txt", &cloned).await;

        assert_eq!(held.on_remote, OnRemote::Held);
        assert_eq!(lacking.on_remote, OnRemote::MayLack);
        assert_eq!(without_remote.on_remote, OnRemote::MayLack);
    }

    #[tokio::test]
    async fn a_binary_side_or_an_unknown_commit_shows_no_text() {
        let repo = TestRepo::new();
        repo.write("data.bin", "text first\n");
        repo.commit("Add data");
        std::fs::write(repo.root.join("data.bin"), b"\0\x01\x02").unwrap();
        let binary = repo.commit("Make it binary");
        let unknown = CommitHash::new(&"a".repeat(40)).unwrap();

        let shown = git_show(&repo, "data.bin", &binary).await;
        let missing = show(
            repo.root.clone(),
            Vec::new(),
            Some(repo.home().to_owned()),
            repo.root.join("data.bin"),
            unknown,
        )
        .await;

        assert_eq!(shown.texts, CommitTexts::NotText);
        assert!(
            matches!(missing, Err(GitError::NoSuchCommit(_))),
            "{missing:?}"
        );
    }

    #[test]
    fn two_texts_of_control_characters_may_not_fit_an_answer() {
        let small = "fn main() {}\n";
        let escaped = "\u{1}".repeat(usize::try_from(MAX_FILE_BYTES).unwrap());

        assert!(fit_an_answer(Some(small), None));
        assert!(!fit_an_answer(Some(&escaped), Some(small)));
    }
}
