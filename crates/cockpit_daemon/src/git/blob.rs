//! A file's content in a commit, read by its blob's hash, which no filter or textconv touches.

use std::path::Path;

use cockpit_protocol::CommitHash;
use tokio::time::Instant;

use crate::git::GitError;
use crate::git::command::{END_OF_OPTIONS, FIELD_END, Git};
use crate::read_file::{MAX_FILE_BYTES, NUL_ENDED};

/// Lists the entries of a commit's tree at the paths given, each as `<mode> <type> <object>
/// <size>`, a tab, then its path.
const LIST_ENTRY: [&str; 3] = ["ls-tree", "--long", NUL_ENDED];
const BLOB_TYPE: &str = "blob";
const PATH_START: u8 = b'\t';
/// Writes a blob as it is stored.
const READ_BLOB: [&str; 2] = ["cat-file", BLOB_TYPE];
const MAX_ENTRY_BYTES: usize = 64 * 1024;

/// A blob's object hash, as git writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlobId(String);

impl BlobId {
    /// `None` unless `text` is 40 or 64 hexadecimal digits, so that it never reaches git as an
    /// option.
    fn new(text: &str) -> Option<Self> {
        let is_hash =
            matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit());
        is_hash.then(|| Self(text.to_owned()))
    }
}

/// A file in a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Blob {
    id: BlobId,
    /// `None` when the blob is not in the repository, as in a partial clone, which git does not
    /// fetch from here.
    pub(super) size: Option<u64>,
}

impl Blob {
    pub(super) fn is_larger_than(&self, max_bytes: u64) -> bool {
        self.size.is_some_and(|size| size > max_bytes)
    }
}

/// What the file view shows of a blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Content {
    Text(String),
    /// Binary, or larger than [`MAX_FILE_BYTES`].
    NotText,
}

/// The file `commit` holds at `path`, from the checkout's top folder. `None` when it holds none
/// there, or something else, such as a folder or a submodule.
pub(super) async fn find(
    root: &Path,
    commit: &CommitHash,
    path: &str,
    deadline: Instant,
) -> Result<Option<Blob>, GitError> {
    let listed = Git::new(root)
        .args(LIST_ENTRY)
        .arg(END_OF_OPTIONS)
        .arg(commit.as_str())
        .arg(path)
        .run(deadline, MAX_ENTRY_BYTES)
        .await?
        .into_success()?;
    Ok(entry_at(&listed, path))
}

/// Reads `blob`, unless it is larger than [`MAX_FILE_BYTES`].
pub(super) async fn read(root: &Path, blob: &Blob, deadline: Instant) -> Result<Content, GitError> {
    if blob.is_larger_than(MAX_FILE_BYTES) {
        return Ok(Content::NotText);
    }
    let max_bytes = usize::try_from(MAX_FILE_BYTES).unwrap_or(usize::MAX);
    let bytes = Git::new(root)
        .args(READ_BLOB)
        .arg(&blob.id.0)
        .run(deadline, max_bytes)
        .await?
        .into_success()?;
    Ok(content_of(bytes))
}

/// The blob of `git ls-tree --long -z` output whose path is `path`.
fn entry_at(listed: &[u8], path: &str) -> Option<Blob> {
    for entry in listed.split(|byte| *byte == FIELD_END) {
        let mut parts = entry.splitn(2, |byte| *byte == PATH_START);
        let (Some(fields), Some(named)) = (parts.next(), parts.next()) else {
            continue;
        };
        if named != path.as_bytes() {
            continue;
        }
        let mut fields = std::str::from_utf8(fields).ok()?.split_ascii_whitespace();
        let (Some(_mode), Some(kind), Some(id), Some(size)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return None;
        };
        if kind != BLOB_TYPE {
            return None;
        }
        return Some(Blob {
            id: BlobId::new(id)?,
            size: size.parse().ok(),
        });
    }
    None
}

/// `bytes` as text: not when they hold a NUL byte, or are not UTF-8.
fn content_of(bytes: Vec<u8>) -> Content {
    if bytes.contains(&0) {
        return Content::NotText;
    }
    String::from_utf8(bytes).map_or(Content::NotText, Content::Text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOB: &str = "dd8c897f039f57aa91e16efa6dfde386c4255206";

    #[test]
    fn the_entry_at_the_path_is_a_blob_with_its_size() {
        let listed = format!(
            "100644 blob {BLOB}      14\tsrc/main.rs\x00040000 tree {BLOB}       -\tsrc\x00160000 commit {BLOB}       -\tvendor/lib\0"
        );

        let main = entry_at(listed.as_bytes(), "src/main.rs");

        assert_eq!(
            main,
            Some(Blob {
                id: BlobId(BLOB.to_owned()),
                size: Some(14),
            })
        );
        assert_eq!(entry_at(listed.as_bytes(), "src"), None);
        assert_eq!(entry_at(listed.as_bytes(), "vendor/lib"), None);
        assert_eq!(entry_at(listed.as_bytes(), "src/main"), None);
        assert_eq!(entry_at(b"", "src/main.rs"), None);
    }

    /// A partial clone lacks the blobs it has not fetched.
    #[test]
    fn a_blob_missing_from_the_repository_has_no_size() {
        let listed = format!("100644 blob {BLOB}     BAD\tnotes.txt\0");

        let missing = entry_at(listed.as_bytes(), "notes.txt");

        assert_eq!(missing.map(|blob| blob.size), Some(None));
    }

    #[test]
    fn a_nul_byte_or_bytes_that_are_not_utf8_are_not_text() {
        assert_eq!(
            content_of(b"one\n".to_vec()),
            Content::Text("one\n".to_owned())
        );
        assert_eq!(content_of(b"PK\0\x03".to_vec()), Content::NotText);
        assert_eq!(content_of(vec![0xff, 0xfe]), Content::NotText);
    }
}
