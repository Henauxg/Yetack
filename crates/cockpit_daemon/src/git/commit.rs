//! Commits as the app shows them, read with `git log`.

use std::collections::HashMap;
use std::iter::Peekable;
use std::path::Path;

use cockpit_protocol::{
    Commit, CommitHash, Completeness, MAX_COMMIT_BODY_BYTES, MAX_UNPUSHED_COMMITS, Timestamp,
};
use tokio::time::Instant;

use crate::git::GitError;
use crate::git::command::{END_OF_OPTIONS, FIELD_END, Git, LOG, NO_COLOR, NO_SIGNATURE_CHECK};
use crate::read_file::NUL_ENDED;

/// Each commit's hash, parents, author, author time in seconds and message, each ended by a NUL
/// byte with `-z`.
pub(super) const COMMIT_FORMAT: &str = "--format=%H%x00%P%x00%an%x00%at%x00%B";
/// Writes the answer in UTF-8, whatever encoding the config names: git encodes the whole answer
/// in it, and UTF-16 puts NUL bytes inside every field.
pub(super) const UTF8_OUTPUT: &str = "--encoding=UTF-8";
/// Names the commit a revision points to, or nothing with [`NO_SUCH_REVISION`].
const VERIFY: [&str; 3] = ["rev-parse", "--verify", "--quiet"];
/// Peels a revision to the commit it names, and fails for any other object.
const TO_COMMIT: &str = "^{commit}";
/// `git rev-parse --verify --quiet`'s exit code for a revision that names no commit.
const NO_SUCH_REVISION: i32 = 1;
/// Shows the commits named on standard input, one hash a line, and no others.
const GIVEN_COMMITS: [&str; 2] = ["--no-walk=unsorted", "--stdin"];
const PARENT_SEPARATOR: char = ' ';
const LINE_END: char = '\n';
const MAX_HASH_BYTES: usize = 1024;
/// The most bytes of the commits one blame names, messages whole. A blame of thousands of commits
/// fits.
const MAX_COMMITS_BYTES: usize = 32 * 1024 * 1024;
const MILLIS_PER_SECOND: u64 = 1_000;
/// Lists the commits the revisions given reach, newest first.
const REV_LIST: &str = "rev-list";
/// Leaves out the commits that the revisions after it reach.
const NOT: &str = "--not";
/// What git reads as a pattern in a `--remotes=` name.
const PATTERN_CHARACTERS: [char; 4] = ['*', '?', '[', '\\'];
/// The hashes of [`MAX_UNPUSHED_COMMITS`] and one more, a line each, with room to spare.
const MAX_UNPUSHED_BYTES: usize = 16 * 1024;

/// A commit, with the first of its parents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommitRecord {
    pub(super) commit: Commit,
    /// `None` for a first commit.
    pub(super) first_parent: Option<CommitHash>,
}

/// The commit `revision` names: HEAD, or a hash. `None` when it names none, as HEAD in a checkout
/// without a commit.
pub(super) async fn resolve(
    root: &Path,
    revision: &str,
    deadline: Instant,
) -> Result<Option<CommitHash>, GitError> {
    let output = Git::new(root)
        .args(VERIFY)
        .arg(format!("{revision}{TO_COMMIT}"))
        .run(deadline, MAX_HASH_BYTES)
        .await?;
    if output.code == Some(NO_SUCH_REVISION) {
        return Ok(None);
    }
    let written = output.into_success()?;
    let hash = std::str::from_utf8(&written)
        .ok()
        .and_then(|text| CommitHash::new(text.trim()));
    hash.map(Some).ok_or(GitError::Unparsable("a commit hash"))
}

/// The commits `hashes` name, by hash, with their messages.
pub(super) async fn read(
    root: &Path,
    hashes: &[CommitHash],
    deadline: Instant,
) -> Result<HashMap<CommitHash, CommitRecord>, GitError> {
    // Without a commit to read, `git log` would read HEAD.
    if hashes.is_empty() {
        return Ok(HashMap::new());
    }
    let mut input = String::new();
    for hash in hashes {
        input.push_str(hash.as_str());
        input.push(LINE_END);
    }
    let written = Git::new(root)
        .args([
            LOG,
            NUL_ENDED,
            NO_COLOR,
            NO_SIGNATURE_CHECK,
            UTF8_OUTPUT,
            COMMIT_FORMAT,
        ])
        .args(GIVEN_COMMITS)
        .input(input.into_bytes())
        .run(deadline, MAX_COMMITS_BYTES)
        .await?
        .into_success()?;
    let mut fields = fields(&written);
    let mut records: HashMap<CommitHash, CommitRecord> = HashMap::new();
    while fields.peek().is_some() {
        let record = next_record(&mut fields)?;
        records.insert(record.commit.hash.clone(), record);
    }
    Ok(records)
}

/// The commit `hash` names, with its message.
pub(super) async fn read_one(
    root: &Path,
    hash: &CommitHash,
    deadline: Instant,
) -> Result<CommitRecord, GitError> {
    let mut records = read(root, std::slice::from_ref(hash), deadline).await?;
    records
        .remove(hash)
        .ok_or(GitError::Unparsable("a commit's fields"))
}

/// The commits of a checkout's HEAD that a remote's branches lack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Unpushed {
    /// Newest first, at most [`MAX_UNPUSHED_COMMITS`].
    pub(super) commits: Vec<CommitHash>,
    pub(super) completeness: Completeness,
}

impl Unpushed {
    /// Nothing known: every commit may be unpushed.
    pub(super) const UNKNOWN: Self = Self {
        commits: Vec::new(),
        completeness: Completeness::Truncated,
    };
}

/// The commits of `head` and its ancestors that none of the branches of the remote named `remote`
/// held at the last fetch.
pub(super) async fn unpushed(
    root: &Path,
    head: &CommitHash,
    remote: &str,
    deadline: Instant,
) -> Result<Unpushed, GitError> {
    // `--remotes` reads the name as a pattern, which would match other remotes' branches too.
    if remote.is_empty() || remote.contains(PATTERN_CHARACTERS) {
        return Ok(Unpushed::UNKNOWN);
    }
    let written = Git::new(root)
        .arg(REV_LIST)
        .arg(format!("--max-count={}", MAX_UNPUSHED_COMMITS + 1))
        .arg(head.as_str())
        .arg(NOT)
        .arg(format!("--remotes={remote}"))
        // Without it, git refuses a hash that also names a file in the checkout.
        .arg(END_OF_OPTIONS)
        .run(deadline, MAX_UNPUSHED_BYTES)
        .await?
        .into_success()?;
    unpushed_listed(&written)
}

/// The commits `git rev-list` wrote, a hash a line.
fn unpushed_listed(written: &[u8]) -> Result<Unpushed, GitError> {
    let text = std::str::from_utf8(written).map_err(|_| GitError::Unparsable("a commit hash"))?;
    let mut commits: Vec<CommitHash> = text
        .lines()
        .map(|line| CommitHash::new(line).ok_or(GitError::Unparsable("a commit hash")))
        .collect::<Result<_, _>>()?;
    let completeness = if commits.len() > MAX_UNPUSHED_COMMITS {
        commits.truncate(MAX_UNPUSHED_COMMITS);
        Completeness::Truncated
    } else {
        Completeness::Complete
    };
    Ok(Unpushed {
        commits,
        completeness,
    })
}

/// The NUL-ended fields of `git log -z` output.
pub(super) fn fields(output: &[u8]) -> Peekable<impl Iterator<Item = &[u8]>> {
    // An empty answer has no field, rather than one empty field.
    let whole = (!output.is_empty()).then(|| output.strip_suffix(&[FIELD_END]).unwrap_or(output));
    whole
        .into_iter()
        .flat_map(|whole| whole.split(|byte| *byte == FIELD_END))
        .peekable()
}

/// The next commit of `fields`, as [`COMMIT_FORMAT`] writes it.
pub(super) fn next_record<'output>(
    fields: &mut impl Iterator<Item = &'output [u8]>,
) -> Result<CommitRecord, GitError> {
    record(fields).ok_or(GitError::Unparsable("a commit's fields"))
}

fn record<'output>(fields: &mut impl Iterator<Item = &'output [u8]>) -> Option<CommitRecord> {
    let hash = CommitHash::new(text(fields.next()?)?)?;
    let first_parent = match text(fields.next()?)?.split(PARENT_SEPARATOR).next() {
        Some("") | None => None,
        Some(parent) => Some(CommitHash::new(parent)?),
    };
    let author = String::from_utf8_lossy(fields.next()?).into_owned();
    let seconds: u64 = text(fields.next()?)?.parse().ok()?;
    let millis = seconds.checked_mul(MILLIS_PER_SECOND)?;
    let (title, body) = split_message(&String::from_utf8_lossy(fields.next()?));
    Some(CommitRecord {
        commit: Commit {
            hash,
            title,
            body,
            author,
            time: Timestamp::from_unix_millis(millis),
        },
        first_parent,
    })
}

fn text(field: &[u8]) -> Option<&str> {
    std::str::from_utf8(field).ok()
}

/// A message's first line, and the rest without the blank lines around it, at most
/// [`MAX_COMMIT_BODY_BYTES`].
fn split_message(message: &str) -> (String, String) {
    let (title, mut rest) = message.split_once(LINE_END).unwrap_or((message, ""));
    while let Some((line, after)) = rest.split_once(LINE_END) {
        if !line.trim().is_empty() {
            break;
        }
        rest = after;
    }
    let body = cut(rest.trim_end(), MAX_COMMIT_BODY_BYTES);
    (title.trim_end().to_owned(), body.to_owned())
}

/// The start of `text`, at most `max_bytes`, ending on a character boundary.
fn cut(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let end = (0..=max_bytes)
        .rev()
        .find(|end| text.is_char_boundary(*end))
        .unwrap_or(0);
    text.get(..end).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::git::test_repo::TestRepo;

    const FIRST: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a098765432";
    const SECOND: &str = "1234567890abcdef1234567890abcdef12345678";

    #[test]
    fn past_the_limit_the_unpushed_commits_are_cut_short() {
        let few = format!("{FIRST}\n{SECOND}\n");
        let all = format!("{FIRST}\n").repeat(MAX_UNPUSHED_COMMITS);
        let many = format!("{FIRST}\n").repeat(MAX_UNPUSHED_COMMITS + 1);

        let few = unpushed_listed(few.as_bytes()).unwrap();
        let all = unpushed_listed(all.as_bytes()).unwrap();
        let many = unpushed_listed(many.as_bytes()).unwrap();

        assert_eq!(
            few,
            Unpushed {
                commits: vec![
                    CommitHash::new(FIRST).unwrap(),
                    CommitHash::new(SECOND).unwrap()
                ],
                completeness: Completeness::Complete,
            }
        );
        assert_eq!(all.commits.len(), MAX_UNPUSHED_COMMITS);
        assert_eq!(all.completeness, Completeness::Complete);
        assert_eq!(many.commits.len(), MAX_UNPUSHED_COMMITS);
        assert_eq!(many.completeness, Completeness::Truncated);
    }

    #[tokio::test]
    async fn a_remote_name_git_would_read_as_a_pattern_lists_no_commit() {
        let head = CommitHash::new(FIRST).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);

        for name in ["", "orig*", "o?igin", "[o]rigin", "or\\igin"] {
            let listed = unpushed(Path::new("/nowhere"), &head, name, deadline).await;

            assert_eq!(listed.unwrap(), Unpushed::UNKNOWN, "{name:?}");
        }
    }

    #[tokio::test]
    async fn a_file_named_like_head_leaves_the_unpushed_commits_listed() {
        let origin = TestRepo::new();
        origin.commit("Start");
        let clone = origin.clone_beside("clone");
        let local = clone.commit("Local");
        clone.write(local.as_str(), "");
        let deadline = Instant::now() + Duration::from_secs(10);

        let listed = unpushed(&clone.root, &local, "origin", deadline).await;

        assert_eq!(
            listed.unwrap(),
            Unpushed {
                commits: vec![local],
                completeness: Completeness::Complete,
            }
        );
    }

    #[test]
    fn a_record_holds_the_commit_and_its_first_parent() {
        let output = format!(
            "{SECOND}\0{FIRST} {SECOND}\0Ann Example\x001700000000\0Fix the totals\n\nThe sum left out\nthe last line.\n\n\0{FIRST}\0\0Bob Example\x001600000000\0Start\n\0"
        );

        let mut fields = fields(output.as_bytes());
        let merge = next_record(&mut fields).unwrap();
        let first = next_record(&mut fields).unwrap();

        assert_eq!(fields.next(), None);
        assert_eq!(
            merge,
            CommitRecord {
                commit: Commit {
                    hash: CommitHash::new(SECOND).unwrap(),
                    title: "Fix the totals".to_owned(),
                    body: "The sum left out\nthe last line.".to_owned(),
                    author: "Ann Example".to_owned(),
                    time: Timestamp::from_unix_millis(1_700_000_000_000),
                },
                first_parent: CommitHash::new(FIRST),
            }
        );
        assert_eq!(first.first_parent, None);
        assert_eq!(first.commit.title, "Start");
        assert_eq!(first.commit.body, "");
    }

    #[test]
    fn fields_that_do_not_make_a_commit_are_refused() {
        let short = format!("{FIRST}\0\0Ann\0");
        let bad_hash = "not a hash\0\0Ann\x001700000000\0Start\n\0";

        assert!(next_record(&mut fields(short.as_bytes())).is_err());
        assert!(next_record(&mut fields(bad_hash.as_bytes())).is_err());
        assert_eq!(fields(b"").next(), None);
    }

    #[test]
    fn a_long_body_is_cut_on_a_character_boundary() {
        let message = format!("Title\n\n{}", "é".repeat(MAX_COMMIT_BODY_BYTES));

        let (title, body) = split_message(&message);

        assert_eq!(title, "Title");
        assert_eq!(body.len(), MAX_COMMIT_BODY_BYTES);
        assert!(body.chars().all(|character| character == 'é'));
        assert_eq!(cut("aé", 2), "a");
    }
}
