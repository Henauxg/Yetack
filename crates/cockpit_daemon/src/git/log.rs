//! `agent.git_log`: the last commits that changed a file.

use std::path::PathBuf;
use std::time::Duration;

use cockpit_protocol::{Commit, LineCounts, LogEntry, MAX_LOG_COMMITS};
use tokio::time::Instant;

use crate::git::command::{
    END_OF_OPTIONS, Git, HEAD, LOG, NO_COLOR, NO_EXT_DIFF, NO_SIGNATURE_CHECK, NO_SUBMODULES,
    NO_TEXTCONV, NUMSTAT,
};
use crate::git::commit::{COMMIT_FORMAT, UTF8_OUTPUT};
use crate::git::{GitError, checkout, commit, numstat};
use crate::read_file::NUL_ENDED;

/// How long a log may take. It walks the history until it finds enough commits of the file.
const LOG_TIME: Duration = Duration::from_secs(10);
/// The file's own history, as the protocol says: `log.follow` would follow it across renames.
const NO_FOLLOW: &str = "log.follow=false";
/// With each commit, the counts of its diff from its parent, without textconv or an external
/// diff. A merge shows no diff, so it has no counts.
const LOG_OPTIONS: [&str; 11] = [
    LOG,
    NUL_ENDED,
    NO_COLOR,
    NO_SIGNATURE_CHECK,
    NO_TEXTCONV,
    NO_EXT_DIFF,
    "--no-renames",
    NO_SUBMODULES,
    NUMSTAT,
    UTF8_OUTPUT,
    COMMIT_FORMAT,
];
const MAX_LOG_BYTES: usize = 16 * 1024 * 1024;

/// The last [`MAX_LOG_COMMITS`] commits that changed the file `path` names, absolute or relative
/// to `folder`, newest first, without their bodies. Empty in a checkout without a commit.
/// `touched` and `home` are as for `agent.read_file`.
pub(crate) async fn log(
    folder: PathBuf,
    touched: Vec<PathBuf>,
    home: Option<PathBuf>,
    path: PathBuf,
) -> Result<Vec<LogEntry>, GitError> {
    let deadline = Instant::now() + LOG_TIME;
    let located = checkout::locate_in_task(folder, touched, home, path, deadline).await?;
    let root = located.root.as_path();
    let Some(head) = commit::resolve(root, HEAD, deadline).await? else {
        return Ok(Vec::new());
    };
    let written = Git::with_config(root, &[NO_FOLLOW.to_owned()])
        .args(LOG_OPTIONS)
        .arg(format!("--max-count={MAX_LOG_COMMITS}"))
        .arg(head.as_str())
        .arg(END_OF_OPTIONS)
        .arg(&located.path)
        .run(deadline, MAX_LOG_BYTES)
        .await?
        .into_success()?;
    entries_of(&written)
}

/// The entries of `git log -z --numstat` output with [`COMMIT_FORMAT`]: each commit's fields,
/// then a field of counts for each file of its diff.
fn entries_of(written: &[u8]) -> Result<Vec<LogEntry>, GitError> {
    let mut fields = commit::fields(written);
    let mut entries: Vec<LogEntry> = Vec::new();
    while fields.peek().is_some() {
        let record = commit::next_record(&mut fields)?;
        let mut counts: Option<LineCounts> = None;
        let mut binary = false;
        while let Some(field) = fields.next_if(|field| numstat::holds_counts(field)) {
            let line = numstat::parse(field).ok_or(GitError::Unparsable("a log's counts"))?;
            match line.counts {
                Some(added) => {
                    let total = counts.get_or_insert_default();
                    total.added = total.added.saturating_add(added.added);
                    total.removed = total.removed.saturating_add(added.removed);
                }
                None => binary = true,
            }
        }
        entries.push(LogEntry {
            commit: Commit {
                body: String::new(),
                ..record.commit
            },
            counts: counts.filter(|_| !binary),
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_repo::TestRepo;

    const MERGE: &str = "a67dd5fc7355651154a518bad91a2c89a0023f2f";
    const MAIN: &str = "e7df16dff58ce9e2bfb9b8640ba21d09f6db188f";
    const SIDE: &str = "2d20470af6cf7536fd7e5f6e73970045b1aea11c";

    #[test]
    fn each_commit_has_the_counts_of_its_diff_and_a_merge_none() {
        let written = format!(
            "{MERGE}\0{MAIN} {SIDE}\0Ann\x001700000000\0Merge side\n\0{MAIN}\0{SIDE}\0Ann\x001700000000\0Fix\n\nThe body.\n\0\n3\t1\tf.txt\0{SIDE}\0\0Ann\x001700000000\0Add the logo\n\0\n-\t-\tf.txt\0"
        );

        let entries = entries_of(written.as_bytes()).unwrap();

        let shown: Vec<(&str, &str, Option<LineCounts>)> = entries
            .iter()
            .map(|entry| {
                let commit = &entry.commit;
                (commit.title.as_str(), commit.body.as_str(), entry.counts)
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("Merge side", "", None),
                (
                    "Fix",
                    "",
                    Some(LineCounts {
                        added: 3,
                        removed: 1
                    })
                ),
                ("Add the logo", "", None),
            ]
        );
        assert_eq!(entries_of(b"").unwrap(), Vec::new());
    }

    async fn git_log(repo: &TestRepo, path: &str) -> Vec<LogEntry> {
        log(
            repo.root.clone(),
            Vec::new(),
            Some(repo.home().to_owned()),
            PathBuf::from(path),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn the_commits_of_a_file_come_newest_first_with_their_counts() {
        let repo = TestRepo::new();
        let empty = git_log(&repo, "notes.txt").await;
        repo.write("notes.txt", "one\ntwo\n");
        repo.write("other.txt", "other\n");
        let added = repo.commit("Add notes");
        repo.write("other.txt", "changed\n");
        repo.commit("Change the other file");
        repo.write("notes.txt", "one\nTWO\nthree\n");
        let changed = repo.commit("Change notes\n\nWith a body.\n");
        std::fs::write(repo.root.join("logo.png"), b"\0PNG").unwrap();
        repo.commit("Add the logo");

        let notes = git_log(&repo, "notes.txt").await;
        let logo = git_log(&repo, "logo.png").await;

        assert_eq!(empty, Vec::new());
        let shown: Vec<_> = notes
            .iter()
            .map(|entry| {
                (
                    entry.commit.hash.clone(),
                    entry.commit.body.clone(),
                    entry.counts,
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                (
                    changed,
                    String::new(),
                    Some(LineCounts {
                        added: 2,
                        removed: 1
                    })
                ),
                (
                    added,
                    String::new(),
                    Some(LineCounts {
                        added: 2,
                        removed: 0
                    })
                ),
            ]
        );
        assert_eq!(logo.len(), 1);
        assert_eq!(logo[0].counts, None);
    }

    #[tokio::test]
    async fn a_renamed_file_has_no_history_before_its_rename() {
        let repo = TestRepo::new();
        repo.write("old.txt", "one\ntwo\n");
        repo.commit("Add old");
        repo.git(&["config", "log.follow", "true"]);
        repo.git(&["mv", "old.txt", "new.txt"]);
        let renamed = repo.commit("Rename it");

        let entries = git_log(&repo, "new.txt").await;

        let hashes: Vec<_> = entries.iter().map(|entry| &entry.commit.hash).collect();
        assert_eq!(hashes, [&renamed]);
        assert_eq!(
            entries[0].counts,
            Some(LineCounts {
                added: 2,
                removed: 0
            })
        );
    }

    /// git writes a log in the config's encoding, which UTF-16 fills with NUL bytes.
    #[tokio::test]
    async fn commits_read_as_utf8_whatever_the_config_says() {
        let repo = TestRepo::new();
        repo.write("notes.txt", "one\n");
        let head = repo.commit("Add the café's notes");
        repo.git(&["config", "i18n.logOutputEncoding", "UTF-16LE"]);
        let deadline = Instant::now() + LOG_TIME;

        let entries = git_log(&repo, "notes.txt").await;
        let record = commit::read_one(&repo.root, &head, deadline).await.unwrap();

        let titles: Vec<&str> = entries
            .iter()
            .map(|entry| entry.commit.title.as_str())
            .collect();
        assert_eq!(titles, ["Add the café's notes"]);
        assert_eq!(record.commit.title, "Add the café's notes");
    }
}
