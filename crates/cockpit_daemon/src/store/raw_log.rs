use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use cockpit_protocol::Timestamp;
use serde::Serialize;

use crate::store::state_dir::FILE_MODE;
use crate::store::store_error::StoreError;

/// `raw/<agent-id>.jsonl`: every line exchanged with one agent's `claude`, for debugging.
#[derive(Debug)]
pub(crate) struct RawLog {
    path: PathBuf,
    file: File,
    /// Set after a failed write, so a full disk logs one warning, not one per line.
    failed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Direction {
    ToClaude,
    FromClaude,
    Stderr,
}

#[derive(Serialize)]
struct Entry<'line> {
    at: Timestamp,
    direction: Direction,
    line: &'line str,
}

impl RawLog {
    pub(crate) fn open(path: &Path) -> Result<Self, StoreError> {
        use std::os::unix::fs::OpenOptionsExt;

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(FILE_MODE)
            .open(path)
            .map_err(|source| StoreError::Write {
                path: path.to_owned(),
                source,
            })?;
        Ok(Self {
            path: path.to_owned(),
            file,
            failed: false,
        })
    }

    /// Appends a line. A failure is logged once, and never stops the agent.
    pub(crate) fn append(&mut self, at: Timestamp, direction: Direction, line: &str) {
        use std::io::Write;

        let entry = Entry {
            at,
            direction,
            line: line.trim_end_matches('\n'),
        };
        let written = serde_json::to_vec(&entry)
            .map_err(io::Error::other)
            .and_then(|mut bytes| {
                bytes.push(b'\n');
                self.file.write_all(&bytes)
            });
        match written {
            Ok(()) => self.failed = false,
            Err(error) if !self.failed => {
                self.failed = true;
                tracing::warn!(path = %self.path.display(), %error, "cannot write the raw log");
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn appends_one_json_object_per_line() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("1.jsonl");
        let mut log = RawLog::open(&path).unwrap();

        log.append(
            Timestamp::from_unix_millis(5),
            Direction::ToClaude,
            "{\"type\":\"user\"}\n",
        );
        log.append(
            Timestamp::from_unix_millis(6),
            Direction::Stderr,
            "not json",
        );

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "{\"at\":5,\"direction\":\"to_claude\",\"line\":\"{\\\"type\\\":\\\"user\\\"}\"}\n{\"at\":6,\"direction\":\"stderr\",\"line\":\"not json\"}\n"
        );
    }

    #[test]
    fn direction_words_are_pinned() {
        let directions = [
            Direction::ToClaude,
            Direction::FromClaude,
            Direction::Stderr,
        ];

        let words: Vec<Value> = directions
            .iter()
            .map(|direction| serde_json::to_value(direction).unwrap())
            .collect();

        assert_eq!(words, ["to_claude", "from_claude", "stderr"]);
    }
}
