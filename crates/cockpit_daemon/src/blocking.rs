//! A request's blocking file work, which the request stops waiting for at its deadline. A file
//! system that never answers, such as a stuck network mount, would otherwise leave the request
//! unanswered.
//!
//! Nothing stops a thread that the file system holds: it runs on past the deadline until its call
//! returns. The module counts these late threads. Once one scope has [`MAX_LATE_PER_SCOPE`] of
//! them, or all scopes [`MAX_LATE`], new work there is refused at once, so that retries against a
//! stuck mount cannot take every blocking thread tokio has.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use cockpit_protocol::{ErrorBody, ErrorCode};
use tokio::task::JoinError;
use tokio::time::Instant;

/// The most late threads one scope may have.
const MAX_LATE_PER_SCOPE: usize = 8;
/// The most late threads of all scopes. Tokio has 512 blocking threads, and other work needs some.
const MAX_LATE: usize = 128;

/// A task's state, which its caller and its thread both change.
const RUNNING: u8 = 0;
const DONE: u8 = 1;
/// The caller stopped waiting: the thread counts as late until it ends.
const ABANDONED: u8 = 2;

static LATE: Mutex<Late> = Mutex::new(Late::new());

/// Why blocking work gave no result.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BlockingError {
    #[error("the file system did not answer in time")]
    TimedOut,
    #[error("too many earlier reads still wait on the file system")]
    Held,
    #[error("the request's task failed: {0}")]
    Task(#[from] JoinError),
}

impl BlockingError {
    pub(crate) fn to_error_body(&self) -> ErrorBody {
        let code = match self {
            Self::TimedOut | Self::Held => ErrorCode::Timeout,
            Self::Task(_) => ErrorCode::Internal,
        };
        ErrorBody::new(code, self.to_string())
    }
}

/// The work whose late threads count together, since one stuck mount holds them all.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Scope<'folder> {
    /// The reads of an agent's folder, or of a checkout.
    Folder(&'folder Path),
    /// The folder lists of the new agent dialog.
    FolderList,
    /// The lists of past sessions.
    Sessions,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ScopeKey {
    Folder(PathBuf),
    FolderList,
    Sessions,
}

impl From<Scope<'_>> for ScopeKey {
    fn from(scope: Scope<'_>) -> Self {
        match scope {
            Scope::Folder(folder) => Self::Folder(folder.to_owned()),
            Scope::FolderList => Self::FolderList,
            Scope::Sessions => Self::Sessions,
        }
    }
}

/// The threads still running whose callers stopped waiting.
#[derive(Debug)]
struct Late {
    total: usize,
    by_scope: BTreeMap<ScopeKey, usize>,
}

impl Late {
    const fn new() -> Self {
        Self {
            total: 0,
            by_scope: BTreeMap::new(),
        }
    }

    fn is_full(&self, scope: &ScopeKey) -> bool {
        self.total >= MAX_LATE
            || self
                .by_scope
                .get(scope)
                .is_some_and(|count| *count >= MAX_LATE_PER_SCOPE)
    }

    fn add(&mut self, scope: ScopeKey) {
        self.total += 1;
        *self.by_scope.entry(scope).or_default() += 1;
    }

    fn remove(&mut self, scope: &ScopeKey) {
        self.total = self.total.saturating_sub(1);
        if let Some(count) = self.by_scope.get_mut(scope) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.by_scope.remove(scope);
            }
        }
    }
}

/// Marks a task as done when dropped, and no longer counts it if it was late. It drops after the
/// work, when the work panics, and when tokio drops work it never ran.
struct Ending {
    state: Arc<AtomicU8>,
    scope: ScopeKey,
}

impl Drop for Ending {
    fn drop(&mut self) {
        let ended = self
            .state
            .compare_exchange(RUNNING, DONE, Ordering::AcqRel, Ordering::Acquire);
        // The caller counted it as late, under the lock this waits for.
        if ended.is_err()
            && let Ok(mut late) = LATE.lock()
        {
            late.remove(&self.scope);
        }
    }
}

/// Runs `work` on a blocking thread, and stops waiting for it at `deadline`. When `scope`, or the
/// whole daemon, has too many threads that the file system still holds, it refuses the work.
pub(crate) async fn run<T: Send + 'static>(
    deadline: Instant,
    scope: Scope<'_>,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, BlockingError> {
    let scope = ScopeKey::from(scope);
    // A poisoned count is not read: the work then runs uncounted.
    if let Ok(late) = LATE.lock()
        && late.is_full(&scope)
    {
        return Err(BlockingError::Held);
    }
    let state = Arc::new(AtomicU8::new(RUNNING));
    let task = {
        let ending = Ending {
            state: Arc::clone(&state),
            scope: scope.clone(),
        };
        tokio::task::spawn_blocking(move || {
            let _ending = ending;
            work()
        })
    };
    match tokio::time::timeout_at(deadline, task).await {
        Ok(done) => Ok(done?),
        Err(_) => {
            if let Ok(mut late) = LATE.lock()
                && state
                    .compare_exchange(RUNNING, ABANDONED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                late.add(scope);
            }
            Err(BlockingError::TimedOut)
        }
    }
}

/// Holds reads of `folder` until the next one is refused at once, as on a mount that never
/// answers. Dropping what it returns lets the held reads end.
#[cfg(test)]
pub(crate) async fn hold_folder(folder: &Path) -> Vec<std::sync::mpsc::Sender<()>> {
    use std::sync::mpsc;
    use std::time::Duration;

    let mut releases = Vec::new();
    loop {
        let (release, held) = mpsc::channel::<()>();
        let deadline = Instant::now() + Duration::from_millis(20);
        let late = run(deadline, Scope::Folder(folder), move || held.recv()).await;
        if matches!(late, Err(BlockingError::Held)) {
            return releases;
        }
        releases.push(release);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn work_that_never_ends_times_out_and_quick_work_answers() {
        let folder = Path::new("/blocking-test/times-out");
        let (release, held) = mpsc::channel::<()>();
        let deadline = Instant::now() + Duration::from_millis(100);

        let stuck = run(deadline, Scope::Folder(folder), move || held.recv()).await;
        let quick = run(
            Instant::now() + Duration::from_secs(5),
            Scope::Folder(folder),
            || 7,
        )
        .await;

        assert!(matches!(stuck, Err(BlockingError::TimedOut)), "{stuck:?}");
        assert_eq!(quick.unwrap(), 7);
        assert_eq!(
            BlockingError::TimedOut.to_error_body().code,
            ErrorCode::Timeout
        );
        // Lets the held thread end, so that the test's runtime can shut down.
        drop(release);
    }

    #[tokio::test]
    async fn a_scope_with_too_many_late_threads_refuses_work_until_they_end() {
        let stuck = Path::new("/blocking-test/stuck-mount");
        let other = Path::new("/blocking-test/other-folder");
        let mut releases = Vec::new();
        for _ in 0..MAX_LATE_PER_SCOPE {
            let (release, held) = mpsc::channel::<()>();
            releases.push(release);
            let deadline = Instant::now() + Duration::from_millis(20);
            let late = run(deadline, Scope::Folder(stuck), move || held.recv()).await;
            assert!(matches!(late, Err(BlockingError::TimedOut)), "{late:?}");
        }
        let later = Instant::now() + Duration::from_secs(5);

        let refused = run(later, Scope::Folder(stuck), || 1).await;
        let elsewhere = run(later, Scope::Folder(other), || 2).await;
        drop(releases);
        let mut again = run(later, Scope::Folder(stuck), || 3).await;
        while matches!(again, Err(BlockingError::Held)) && Instant::now() < later {
            tokio::time::sleep(Duration::from_millis(10)).await;
            again = run(later, Scope::Folder(stuck), || 3).await;
        }

        assert!(matches!(refused, Err(BlockingError::Held)), "{refused:?}");
        assert_eq!(elsewhere.unwrap(), 2);
        assert_eq!(again.unwrap(), 3, "the stuck threads ended");
    }

    #[tokio::test]
    async fn a_late_thread_that_panics_is_counted_until_it_ends() {
        let folder = Path::new("/blocking-test/panics-late");
        let key = ScopeKey::Folder(folder.to_owned());
        let counted = || LATE.lock().unwrap().by_scope.contains_key(&key);
        let (release, held) = mpsc::channel::<()>();
        let deadline = Instant::now() + Duration::from_millis(20);

        let late = run(deadline, Scope::Folder(folder), move || {
            let _ = held.recv();
            // Unlike `panic!`, it prints nothing.
            std::panic::resume_unwind(Box::new("the work failed after its deadline"))
        })
        .await;
        let counted_while_held = counted();
        drop(release);
        let until = Instant::now() + Duration::from_secs(5);
        while counted() && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(matches!(late, Err(BlockingError::TimedOut)), "{late:?}");
        assert!(counted_while_held);
        assert!(!counted(), "the thread ended");
    }

    #[test]
    fn past_the_total_every_scope_is_full() {
        let mut late = Late::new();
        for index in 0..MAX_LATE {
            late.add(ScopeKey::Folder(PathBuf::from(format!("/mount/{index}"))));
        }
        let fresh = ScopeKey::Sessions;

        assert!(late.is_full(&fresh));
        late.remove(&ScopeKey::Folder(PathBuf::from("/mount/0")));
        assert!(!late.is_full(&fresh));
        assert!(
            !late
                .by_scope
                .contains_key(&ScopeKey::Folder(PathBuf::from("/mount/0")))
        );
    }

    #[test]
    fn a_time_out_reaches_the_app_as_a_timeout_from_each_request() {
        use crate::browse::ListDirError;
        use crate::git::GitError;
        use crate::read_file::ReadFileError;

        let read = ReadFileError::from(BlockingError::TimedOut);
        let listed = ListDirError::from(ReadFileError::from(BlockingError::TimedOut));
        let git = GitError::from(BlockingError::Held);

        for body in [
            read.to_error_body(),
            listed.to_error_body(),
            git.to_error_body(),
        ] {
            assert_eq!(body.code, ErrorCode::Timeout, "{body:?}");
        }
        assert_eq!(read.to_string(), "the file system did not answer in time");
    }
}
