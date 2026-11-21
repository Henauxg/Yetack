//! The one way the git requests run git, and the words of git's command line that several of
//! them use.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::process::Stdio;

use tokio::io::AsyncRead;
use tokio::process::Command;
use tokio::time::Instant;

use crate::git::GitError;
#[cfg(windows)]
use crate::process::program;
use crate::read_file::{CONFIG_OPTION, GIT_FOLDER_OPTION, GIT_PROGRAM, NO_MONITOR};

/// Before the subcommand: no pager, no optional lock, so that the agent's own git never waits on
/// the index lock, and every path after `--` taken as written, without pathspec magic or
/// wildcards.
const GLOBAL_OPTIONS: [&str; 3] = ["--no-pager", "--no-optional-locks", "--literal-pathspecs"];
/// Keeps `git diff` from refreshing the index, which `--no-optional-locks` does not: writing the
/// index takes its lock and runs the `post-index-change` hook, a program the checkout can hold.
const NO_INDEX_REFRESH: [&str; 2] = [CONFIG_OPTION, "diff.autoRefreshIndex=false"];
/// Set, it keeps a partial clone from fetching a missing object, from git 2.44.
const NO_LAZY_FETCH_ENV: &str = "GIT_NO_LAZY_FETCH";
/// Empty, it allows no transport, so that no fetch runs. A fetch runs programs the config names,
/// such as `remote.<name>.uploadpack`, and a git before 2.44 fetches a partial clone's missing
/// objects.
const ALLOW_PROTOCOL_ENV: &str = "GIT_ALLOW_PROTOCOL";
/// The most bytes of git's error output that a message keeps.
const MAX_ERROR_BYTES: usize = 4 * 1024;

pub(super) const END_OF_OPTIONS: &str = "--";
pub(super) const HEAD: &str = "HEAD";
pub(super) const LOG: &str = "log";
pub(super) const NO_COLOR: &str = "--no-color";
pub(super) const NO_TEXTCONV: &str = "--no-textconv";
pub(super) const NO_EXT_DIFF: &str = "--no-ext-diff";
/// A signature check runs `gpg.program`, which `log.showSignature` turns on.
pub(super) const NO_SIGNATURE_CHECK: &str = "--no-show-signature";
/// A submodule's status or diff runs git in it, with its own config.
pub(super) const NO_SUBMODULES: &str = "--ignore-submodules=all";
pub(super) const NUMSTAT: &str = "--numstat";
pub(super) const FIELD_END: u8 = 0;

/// A git command in one checkout.
pub(super) struct Git {
    command: Command,
    input: Option<Vec<u8>>,
}

impl Git {
    /// git in the checkout whose top folder is `root`.
    pub(super) fn new(root: &Path) -> Self {
        Self::with_config(root, &[])
    }

    /// git in the checkout whose top folder is `root`, with each `key=value` of `config` set over
    /// the checkout's own.
    pub(super) fn with_config(root: &Path, config: &[String]) -> Self {
        let mut command = Command::new(GIT_PROGRAM);
        command
            .args(GLOBAL_OPTIONS)
            .args(NO_MONITOR)
            .args(NO_INDEX_REFRESH);
        for setting in config {
            command.arg(CONFIG_OPTION).arg(setting);
        }
        command
            .arg(GIT_FOLDER_OPTION)
            .arg(root)
            .env(NO_LAZY_FETCH_ENV, "1")
            .env(ALLOW_PROTOCOL_ENV, "")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        program::hide_console(&mut command);
        Self {
            command,
            input: None,
        }
    }

    pub(super) fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.command.arg(arg);
        self
    }

    pub(super) fn args(mut self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Self {
        self.command.args(args);
        self
    }

    /// Writes `input` to git's standard input.
    pub(super) fn input(mut self, input: Vec<u8>) -> Self {
        self.command.stdin(Stdio::piped());
        self.input = Some(input);
        self
    }

    /// Runs git until it exits, `deadline` passes, or it writes more than `max_bytes`, which stops
    /// it.
    pub(super) async fn run(
        self,
        deadline: Instant,
        max_bytes: usize,
    ) -> Result<GitOutput, GitError> {
        use tokio::io::AsyncWriteExt;

        let Self { mut command, input } = self;
        let mut child = command.spawn().map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => GitError::NotInstalled,
            _ => GitError::Start(error),
        })?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Err(GitError::Start(io::Error::other(
                "git's output is not piped",
            )));
        };
        let stdin = child.stdin.take();
        let ran = tokio::time::timeout_at(deadline, async {
            let feed = async move {
                if let (Some(mut stdin), Some(input)) = (stdin, input) {
                    // git may stop reading early: its exit code and error output say why.
                    let _ = stdin.write_all(&input).await;
                }
            };
            let read = async {
                let read = read_up_to(stdout, max_bytes).await;
                if matches!(read, Ok((_, true))) {
                    // Stopped, git ends its error output too.
                    let _ = child.start_kill();
                }
                read
            };
            let ((), read, errors) = tokio::join!(feed, read, read_start(stderr, MAX_ERROR_BYTES));
            (read, errors, child.wait().await)
        })
        .await;
        let Ok((read, errors, status)) = ran else {
            return Err(GitError::TimedOut);
        };
        let (stdout, cut) = read.map_err(GitError::Read)?;
        let status = status.map_err(GitError::Read)?;
        Ok(GitOutput {
            code: status.code(),
            stdout,
            cut,
            limit: max_bytes,
            stderr: errors.unwrap_or_default(),
        })
    }
}

/// What a git command wrote, and how it ended.
#[derive(Debug)]
pub(super) struct GitOutput {
    /// `None` when a signal ended git.
    pub(super) code: Option<i32>,
    pub(super) stdout: Vec<u8>,
    /// git wrote more than `limit` bytes, and `stdout` holds the start of it.
    pub(super) cut: bool,
    limit: usize,
    stderr: Vec<u8>,
}

impl GitOutput {
    /// What git wrote, when it succeeded within the limit.
    pub(super) fn into_success(self) -> Result<Vec<u8>, GitError> {
        if self.cut {
            return Err(GitError::TooLarge(self.limit));
        }
        if self.code != Some(0) {
            return Err(self.failure());
        }
        Ok(self.stdout)
    }

    /// The error of a run that failed: git's last line of error output.
    pub(super) fn failure(&self) -> GitError {
        let errors = String::from_utf8_lossy(&self.stderr);
        let last_line = errors
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty());
        let message = match (last_line, self.code) {
            (Some(line), _) => line.to_owned(),
            (None, Some(code)) => format!("exit code {code}"),
            (None, None) => "ended by a signal".to_owned(),
        };
        GitError::Failed(message)
    }
}

/// The start of `output` that holds whole fields: up to its last NUL byte. A cut answer ends in
/// the middle of one.
pub(super) fn whole_fields(output: &[u8]) -> &[u8] {
    match output.iter().rposition(|byte| *byte == FIELD_END) {
        Some(end) => output.get(..end).unwrap_or_default(),
        None => &[],
    }
}

/// Reads `reader` to its end, or until it passes `max_bytes`: the bytes, at most `max_bytes`, and
/// whether it passed.
async fn read_up_to(
    reader: impl AsyncRead + Unpin,
    max_bytes: usize,
) -> io::Result<(Vec<u8>, bool)> {
    use tokio::io::AsyncReadExt;

    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes: Vec<u8> = Vec::new();
    reader.take(limit).read_to_end(&mut bytes).await?;
    let cut = bytes.len() > max_bytes;
    bytes.truncate(max_bytes);
    Ok((bytes, cut))
}

/// The first `keep` bytes of `reader`, which it reads to its end, so that git never waits to write
/// its warnings.
async fn read_start(mut reader: impl AsyncRead + Unpin, keep: usize) -> io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;

    let mut kept: Vec<u8> = Vec::new();
    (&mut reader)
        .take(u64::try_from(keep).unwrap_or(u64::MAX))
        .read_to_end(&mut kept)
        .await?;
    tokio::io::copy(&mut reader, &mut tokio::io::sink()).await?;
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::git::test_repo::TestRepo;

    #[test]
    fn whole_fields_end_at_the_last_nul_byte() {
        assert_eq!(whole_fields(b"1\t0\ta.txt\x002\t1\tb.t"), b"1\t0\ta.txt");
        assert_eq!(whole_fields(b"1\t0\ta.txt\x00"), b"1\t0\ta.txt");
        assert_eq!(whole_fields(b"1\t0\ta.t"), b"");
    }

    #[test]
    fn a_failure_names_gits_last_line_of_error_output() {
        let output = |stderr: &str, code: Option<i32>| GitOutput {
            code,
            stdout: Vec::new(),
            cut: false,
            limit: 0,
            stderr: stderr.as_bytes().to_vec(),
        };
        let warned = "warning: in the working copy of 'a.txt', LF will be replaced by CRLF\nfatal: not a git repository (or any of the parent directories): .git\n\n";

        let named = output(warned, Some(128)).failure().to_string();
        let silent = output("", Some(3)).failure().to_string();

        assert_eq!(
            named,
            "git failed: fatal: not a git repository (or any of the parent directories): .git"
        );
        assert_eq!(silent, "git failed: exit code 3");
    }

    #[tokio::test]
    async fn an_answer_past_the_limit_stops_git_and_a_deadline_too() {
        let repo = TestRepo::new();
        repo.write("large.txt", &"line\n".repeat(100_000));
        repo.commit("Add a large file");
        let read_blob = ["cat-file", "blob", "HEAD:large.txt"];
        let later = Instant::now() + Duration::from_secs(10);

        let cut = Git::new(&repo.root)
            .args(read_blob)
            .run(later, 1024)
            .await
            .unwrap();
        let late = Git::new(&repo.root)
            .args(read_blob)
            .run(Instant::now(), 1024)
            .await;

        assert!(cut.cut);
        assert_eq!(cut.stdout.len(), 1024);
        assert!(matches!(cut.into_success(), Err(GitError::TooLarge(1024))));
        assert!(matches!(late, Err(GitError::TimedOut)), "{late:?}");
    }
}
