//! `/proc` files kept open between samples. A read at offset 0 makes the kernel write the file
//! afresh, for about a tenth of what opening it costs.
//!
//! A file opened under `/proc/<pid>` stays bound to that process: once the process ends, a read
//! fails or comes back empty, even when another process has taken the pid.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

/// Room for a whole `stat` line or children list in one read.
const CHUNK: usize = 8192;

/// Reads the whole file from its start into `buffer`. The kernel writes a `/proc` file in one go
/// when it fits the buffer, so a short read is the end of it.
pub(super) fn read_again(file: &File, buffer: &mut Vec<u8>) -> io::Result<()> {
    buffer.clear();
    let mut offset: u64 = 0;
    loop {
        let start = buffer.len();
        buffer.resize(start + CHUNK, 0);
        let read = file.read_at(buffer.get_mut(start..).unwrap_or_default(), offset);
        let read = match read {
            Ok(read) => read,
            Err(error) => {
                buffer.truncate(start);
                return Err(error);
            }
        };
        buffer.truncate(start + read);
        if read < CHUNK {
            return Ok(());
        }
        offset += u64::try_from(read).unwrap_or(u64::MAX);
    }
}

/// One of the machine's files, such as `/proc/stat`, opened at its first read and kept.
#[derive(Debug)]
pub(super) struct KeptFile {
    path: &'static str,
    file: Option<File>,
}

impl KeptFile {
    pub(super) fn new(path: &'static str) -> Self {
        Self { path, file: None }
    }

    pub(super) fn path(&self) -> &'static str {
        self.path
    }

    /// Reads the file into `buffer`, opening it again when the kept one fails.
    pub(super) fn read(&mut self, buffer: &mut Vec<u8>) -> io::Result<()> {
        if let Some(file) = &self.file
            && read_again(file, buffer).is_ok()
        {
            return Ok(());
        }
        self.file = None;
        let file = File::open(self.path)?;
        read_again(&file, buffer)?;
        self.file = Some(file);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    #[test]
    fn a_kept_proc_file_shows_what_changed_since_it_was_opened() {
        // The child is listed under the thread that starts it: this test's.
        let file = File::open("/proc/thread-self/children").unwrap();
        let mut before = Vec::new();
        read_again(&file, &mut before).unwrap();
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();

        let mut after = Vec::new();
        let read = read_again(&file, &mut after);

        let _ = child.kill();
        let _ = child.wait();
        read.unwrap();
        let text = String::from_utf8(after).unwrap();
        assert!(
            text.split_whitespace()
                .any(|pid| pid == child.id().to_string()),
            "{text:?}"
        );
    }

    #[test]
    fn a_file_longer_than_one_read_comes_whole_each_time() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("long");
        let content: Vec<u8> = (0..CHUNK * 2 + 100)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        std::fs::write(&path, &content).unwrap();
        let file = File::open(&path).unwrap();
        let mut buffer = Vec::new();

        read_again(&file, &mut buffer).unwrap();
        let first = buffer.clone();
        read_again(&file, &mut buffer).unwrap();

        assert_eq!(first, content);
        assert_eq!(buffer, content);
    }
}
