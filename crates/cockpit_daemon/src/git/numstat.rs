//! The line counts of `--numstat -z`: a field `<added>\t<removed>\t<path>` for each file, `-` for
//! both counts of a binary file. A rename's path is empty, and its two paths follow as fields of
//! their own.

use std::collections::HashMap;

use cockpit_protocol::LineCounts;

use crate::git::command::FIELD_END;

const COUNT_END: u8 = b'\t';
const BINARY: &[u8] = b"-";
/// What `git log` writes before a commit's first count.
const LINE_END: u8 = b'\n';

/// One file's field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Numstat<'output> {
    /// `None` for a binary file.
    pub(super) counts: Option<LineCounts>,
    /// Empty for a rename.
    pub(super) path: &'output [u8],
}

/// Whether a field of `git log -z --numstat` holds counts rather than a commit's hash.
pub(super) fn holds_counts(field: &[u8]) -> bool {
    field.contains(&COUNT_END)
}

/// The counts of one field. `None` when it holds none.
pub(super) fn parse(field: &[u8]) -> Option<Numstat<'_>> {
    let field = field.strip_prefix(&[LINE_END]).unwrap_or(field);
    let mut parts = field.splitn(3, |byte| *byte == COUNT_END);
    let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let counts = if added == BINARY && removed == BINARY {
        None
    } else {
        Some(LineCounts {
            added: count(added)?,
            removed: count(removed)?,
        })
    };
    Some(Numstat { counts, path })
}

fn count(field: &[u8]) -> Option<u32> {
    let count: u64 = std::str::from_utf8(field).ok()?.parse().ok()?;
    Some(u32::try_from(count).unwrap_or(u32::MAX))
}

/// The counts of `git diff --numstat -z` output by path, a rename's under its new path.
pub(super) fn by_path(output: &[u8]) -> HashMap<Vec<u8>, Option<LineCounts>> {
    let mut fields = output.split(|byte| *byte == FIELD_END);
    let mut counts: HashMap<Vec<u8>, Option<LineCounts>> = HashMap::new();
    while let Some(field) = fields.next() {
        let Some(line) = parse(field) else {
            continue;
        };
        let path = if line.path.is_empty() {
            let _old_path = fields.next();
            fields.next()
        } else {
            Some(line.path)
        };
        if let Some(path) = path {
            counts.insert(path.to_vec(), line.counts);
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(added: u32, removed: u32) -> Option<LineCounts> {
        Some(LineCounts { added, removed })
    }

    #[test]
    fn a_field_holds_counts_or_marks_a_binary_file() {
        assert_eq!(
            parse(b"\n12\t3\tsrc/a b.rs"),
            Some(Numstat {
                counts: counts(12, 3),
                path: b"src/a b.rs",
            })
        );
        assert_eq!(
            parse(b"-\t-\tlogo.png"),
            Some(Numstat {
                counts: None,
                path: b"logo.png",
            })
        );
        assert_eq!(parse(b"9f8e7d6c5b4a39281706f5e4d3c2b1a098765432"), None);
        assert_eq!(parse(b"x\t1\ta.rs"), None);
        assert!(holds_counts(b"\n1\t0\ta.rs"));
        assert!(!holds_counts(b"9f8e7d6c5b4a39281706f5e4d3c2b1a098765432"));
    }

    #[test]
    fn a_rename_counts_under_its_new_path() {
        let output =
            b"1\t0\ta.txt\0-\t-\tb.bin\x000\t2\t\0old name.txt\0new name.txt\x004\t0\tstaged.txt";

        let by_path = by_path(output);

        assert_eq!(by_path.len(), 4);
        assert_eq!(by_path.get(b"a.txt".as_slice()), Some(&counts(1, 0)));
        assert_eq!(by_path.get(b"b.bin".as_slice()), Some(&None));
        assert_eq!(by_path.get(b"new name.txt".as_slice()), Some(&counts(0, 2)));
        assert_eq!(by_path.get(b"staged.txt".as_slice()), Some(&counts(4, 0)));
    }
}
