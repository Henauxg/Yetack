//! The app's own update, beside the window. The app asks GitHub for its latest release about ten
//! seconds after it starts, or a day after the last check when that is later, then once a day.
//!
//! The Windows zip updates itself: it downloads the release's app and Linux daemon beside its own
//! files, as `<name>.new`, and checks them against the release's `SHA256SUMS`. Windows lets a
//! program rename its running file but not write over it, so the app then moves each of its files
//! aside to `<name>.old` and the new one into its place. The new app, started with
//! `--after-update <pid>`, waits for this one to end, and deletes what the update left.

use std::fmt::Display;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use dioxus::prelude::{ReadableExt, Signal, UnboundedReceiver, WritableExt};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::Instant;

use crate::state::AppState;
use crate::state::app_update::{
    self, AppStep, CHECKSUMS_NAME, CheckAsk, Checksums, DAY, InstallKind, MAX_DOWNLOAD_BYTES,
    Release, ReleaseAsset, RunId, StartedRun, WindowsAssets,
};
use crate::state::daemon_probe::LinuxCpu;
use crate::update_script::APP_VERSION;
use crate::{command_line, demo, server_daemon};

const LATEST_URL: &str = concat!(
    "https://api.github.com/repos/",
    app_update::repository!(),
    "/releases/latest"
);
const GITHUB_JSON: &str = "application/vnd.github+json";

/// GitHub's answer is a few kilobytes, more with long release notes. A release's `SHA256SUMS`
/// is smaller still.
pub const MAX_ANSWER_BYTES: u64 = 4 * 1024 * 1024;

/// The app the update started passes the pid of the one that closes: `--after-update 1234`.
pub const AFTER_UPDATE_FLAG: &str = "--after-update";

const NEW_SUFFIX: &str = ".new";
/// What an update moved aside keeps its name with this after it, until the next start.
pub const OLD_SUFFIX: &str = ".old";

/// How long a connection may stay silent before the request fails.
const SILENCE: Duration = Duration::from_secs(30);

/// How long the download of one file may take, however slowly its bytes come.
pub const DOWNLOAD_TIME: Duration = Duration::from_secs(30 * 60);

/// How many bytes the download reads between two reports of its progress.
const PROGRESS_STEP: u64 = 256 * 1024;

/// The most bytes one read asks for. A "Cancel" stops the download before the next read.
const READ_CHUNK: usize = 64 * 1024;

/// How long the new app waits for the one that started it to end.
#[cfg(windows)]
const EXIT_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error(transparent)]
    Http(#[from] ureq::Error),
    #[error("GitHub's answer does not read as a release: {0}")]
    Answer(#[from] serde_json::Error),
    #[error("the check stopped: {0}")]
    Stopped(#[from] JoinError),
}

impl CheckError {
    /// Why the check failed, as Settings' About says it after "The check failed: ".
    pub fn reason(&self) -> String {
        match self {
            Self::Http(ureq::Error::StatusCode(code)) => {
                format!("GitHub answered with error {code}")
            }
            Self::Http(ureq::Error::Timeout(_)) => "GitHub did not answer in time".to_owned(),
            Self::Http(
                ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Io(_),
            ) => "cannot reach GitHub".to_owned(),
            Self::Http(_) | Self::Answer(_) | Self::Stopped(_) => self.to_string(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("Cannot download {name}: {source}.")]
    Http { name: String, source: ureq::Error },
    #[error("Cannot write {}: {source}.", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("The release's files weigh more than {} MB.", MAX_DOWNLOAD_BYTES / 1024 / 1024)]
    TooLarge,
    #[error("The download was cancelled.")]
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum CheckFileError {
    #[error("{CHECKSUMS_NAME} lists no checksum for {0}.")]
    Unlisted(String),
    #[error("The download of {0} does not match the checksum the release lists.")]
    Mismatch(String),
    #[error("Cannot read {}: {source}.", path.display())]
    Read { path: PathBuf, source: io::Error },
}

/// A file that did not move, and why.
#[derive(Debug, thiserror::Error)]
#[error("{}: {source}", path.display())]
pub struct MoveError {
    pub path: PathBuf,
    pub source: io::Error,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplaceError {
    #[error("Cannot move {0}.")]
    Moved(MoveError),
    /// Some files are neither in their place nor back: the update deletes none of them, since
    /// one may be the only copy of the app.
    #[error(
        "Cannot move {failed}. Putting the files back failed too, at {restore}. Nothing was deleted. {whereabouts}"
    )]
    NotRestored {
        failed: MoveError,
        restore: MoveError,
        /// Where each file moved so far is now.
        whereabouts: String,
    },
}

/// The client every request goes through: rustls with the system's root certificates, HTTPS
/// only, and a name that says which app asks. `body_time` bounds the reading of a whole answer.
pub fn http_client(body_time: Option<Duration>) -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .https_only(true)
        .user_agent(format!("yetack-desktop/{APP_VERSION}"))
        .timeout_connect(Some(SILENCE))
        .timeout_per_read(Some(SILENCE))
        .timeout_recv_body(body_time)
        .build()
        .into()
}

fn fetch_latest() -> Result<Release, CheckError> {
    let answer = http_client(None)
        .get(LATEST_URL)
        .header("Accept", GITHUB_JSON)
        .call()?
        .into_body()
        .into_with_config()
        .limit(MAX_ANSWER_BYTES)
        .read_to_string()?;
    Ok(Release::from_github(&answer)?)
}

pub async fn latest_release() -> Result<Release, CheckError> {
    tokio::task::spawn_blocking(fetch_latest).await?
}

/// Checks for a newer release after `app_update::first_check_delay`, then once a day while the
/// setting is on, and at once when `asked` says so: "Check now", or the setting turned on. The
/// demo makes one up and never touches the network. A failed check goes to the log and to About,
/// and the next round tries again.
pub async fn check_releases(mut state: Signal<AppState>, mut asked: UnboundedReceiver<CheckAsk>) {
    use futures_util::StreamExt;

    let (demo, last_check) = {
        let state = state.peek();
        (state.app_update.demo, state.prefs.updates.last_check)
    };
    let first = if demo {
        demo::APP_RELEASE_DELAY
    } else {
        app_update::first_check_delay(SystemTime::now(), last_check)
    };
    let mut next = Instant::now() + first;
    loop {
        let mut ask = tokio::select! {
            () = tokio::time::sleep_until(next) => None,
            ask = asked.next() => match ask {
                Some(ask) => Some(ask),
                None => return,
            },
        };
        next = Instant::now() + DAY;
        if ask.is_none() && !state.peek().prefs.updates.check {
            continue;
        }
        app_update::check_started(&mut state.write());
        let latest = if demo {
            Ok(demo::app_release())
        } else {
            latest_release().await
        };
        // The check that just ended answers the asks that came while it ran.
        while let Ok(more) = asked.try_recv() {
            if more == CheckAsk::Now {
                ask = Some(more);
            }
        }
        let mut state = state.write();
        match latest {
            Ok(release) => {
                tracing::info!("the latest release of the app is {}", release.version);
                app_update::answered(&mut state, release, SystemTime::now(), ask);
            }
            Err(error) => {
                tracing::warn!("cannot check for a new release of the app: {error}");
                app_update::check_failed(&mut state, error.reason());
            }
        }
    }
}

/// Where the app's files are: its program, and the Linux daemon beside it that it sends to
/// servers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub app: PathBuf,
    pub daemon: PathBuf,
}

impl Placement {
    pub fn of_this_app() -> io::Result<Self> {
        let app = std::env::current_exe()?;
        let folder = app
            .parent()
            .ok_or_else(|| io::Error::other("the app's program has no folder"))?;
        Ok(Self {
            daemon: folder.join(LinuxCpu::X86_64.daemon_file_name()),
            app,
        })
    }

    fn targets(&self) -> [&Path; 2] {
        [&self.app, &self.daemon]
    }

    /// Deletes the downloads of an update that ended before it put them in place.
    pub fn remove_new_files(&self) {
        for target in self.targets() {
            remove_if_there(&with_suffix(target, NEW_SUFFIX));
        }
    }

    /// Tidies what an earlier update left. A file moved aside goes back to its place when nothing
    /// took it. The downloads it did not use go, and so do the files moved aside that something
    /// took the place of.
    pub fn remove_leftovers(&self) {
        for target in self.targets() {
            let old = with_suffix(target, OLD_SUFFIX);
            if target.exists() || !old.exists() {
                continue;
            }
            match fs::rename(&old, target) {
                Ok(()) => tracing::info!("put {} back as {}", old.display(), target.display()),
                Err(error) => tracing::warn!("cannot put {} back: {error}", old.display()),
            }
        }
        self.remove_new_files();
        for target in self.targets() {
            if target.exists() {
                remove_if_there(&with_suffix(target, OLD_SUFFIX));
            }
        }
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn remove_if_there(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => tracing::info!("deleted {}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!("cannot delete {}: {error}", path.display()),
    }
}

/// Tidies what an earlier update left beside this app.
pub fn remove_leftovers() {
    remove_leftovers_of(InstallKind::of_this_build(), Placement::of_this_app);
}

/// Only the zip updates itself, so only its folder holds such files: `find` says where they are.
fn remove_leftovers_of(install: InstallKind, find: impl FnOnce() -> io::Result<Placement>) {
    if !install.updates_itself() {
        return;
    }
    match find() {
        Ok(placement) => placement.remove_leftovers(),
        Err(error) => tracing::warn!("cannot find the app's program: {error}"),
    }
}

/// Downloads the release's checksums, then its app and daemon beside the app's files, and reports
/// the bytes read so far to `progress`. A closed `progress` cancels the download.
fn download(
    placement: &Placement,
    assets: &WindowsAssets,
    progress: &mpsc::UnboundedSender<u64>,
) -> Result<Checksums, DownloadError> {
    let client = http_client(Some(DOWNLOAD_TIME));
    download_from(placement, assets, progress, |asset| {
        open_body(&client, asset)
    })
}

/// The body of `asset`'s download, which may not weigh more than `MAX_DOWNLOAD_BYTES`.
fn open_body(
    client: &ureq::Agent,
    asset: &ReleaseAsset,
) -> Result<ureq::BodyReader<'static>, DownloadError> {
    let response = client
        .get(&asset.url)
        .call()
        .map_err(|source| http_error(asset, source))?;
    Ok(response
        .into_body()
        .into_with_config()
        .limit(MAX_DOWNLOAD_BYTES)
        .reader())
}

fn http_error(asset: &ReleaseAsset, source: ureq::Error) -> DownloadError {
    DownloadError::Http {
        name: asset.name.clone(),
        source,
    }
}

fn report(progress: &mpsc::UnboundedSender<u64>, done: u64) -> Result<(), DownloadError> {
    progress.send(done).map_err(|_| DownloadError::Cancelled)
}

/// `download`, with the bodies `open` gives. A failure deletes the files written so far.
fn download_from<R: Read>(
    placement: &Placement,
    assets: &WindowsAssets,
    progress: &mpsc::UnboundedSender<u64>,
    mut open: impl FnMut(&ReleaseAsset) -> Result<R, DownloadError>,
) -> Result<Checksums, DownloadError> {
    if progress.is_closed() {
        return Err(DownloadError::Cancelled);
    }
    let mut sums = Vec::new();
    open(&assets.checksums)?
        .take(MAX_ANSWER_BYTES)
        .read_to_end(&mut sums)
        .map_err(|error| http_error(&assets.checksums, ureq::Error::Io(error)))?;
    let mut done = u64::try_from(sums.len()).unwrap_or(u64::MAX);
    report(progress, done)?;
    let checksums = Checksums::from(String::from_utf8_lossy(&sums).as_ref());
    let files = [
        (&assets.app, &placement.app),
        (&assets.daemon, &placement.daemon),
    ];
    for (asset, target) in files {
        let path = with_suffix(target, NEW_SUFFIX);
        let written = open(asset)
            .and_then(|mut body| write_file(&mut body, asset, &path, &mut done, progress));
        if let Err(error) = written {
            placement.remove_new_files();
            return Err(error);
        }
    }
    Ok(checksums)
}

/// Writes `body`, the download of `asset`, to a new file at `path`. It adds the bytes to `done`
/// and reports them to `progress`, and a closed `progress` stops it before its next read.
fn write_file(
    body: &mut impl Read,
    asset: &ReleaseAsset,
    path: &Path,
    done: &mut u64,
    progress: &mpsc::UnboundedSender<u64>,
) -> Result<(), DownloadError> {
    let write_error = |source: io::Error| DownloadError::Write {
        path: path.to_owned(),
        source,
    };
    let mut file = File::create(path).map_err(write_error)?;
    let mut chunk = vec![0; READ_CHUNK];
    let mut reported = *done;
    loop {
        if progress.is_closed() {
            return Err(DownloadError::Cancelled);
        }
        let read = match body.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(http_error(asset, ureq::Error::Io(error))),
        };
        *done = done.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if *done > MAX_DOWNLOAD_BYTES {
            return Err(DownloadError::TooLarge);
        }
        file.write_all(chunk.get(..read).unwrap_or_default())
            .map_err(write_error)?;
        if *done - reported >= PROGRESS_STEP {
            reported = *done;
            report(progress, *done)?;
        }
    }
    file.sync_all().map_err(write_error)?;
    report(progress, *done)
}

/// The file's SHA-256, in lowercase hex.
pub fn sha256_of(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0; 256 * 1024];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        hasher.update(chunk.get(..read).unwrap_or_default());
    }
    Ok(server_daemon::hex(&hasher.finalize()))
}

/// Checks each downloaded file against the checksum the release lists for its name.
fn check(
    placement: &Placement,
    assets: &WindowsAssets,
    checksums: &Checksums,
) -> Result<(), CheckFileError> {
    let files = [
        (&assets.app.name, &placement.app),
        (&assets.daemon.name, &placement.daemon),
    ];
    for (name, target) in files {
        let expected = checksums
            .of(name)
            .ok_or_else(|| CheckFileError::Unlisted(name.clone()))?;
        let path = with_suffix(target, NEW_SUFFIX);
        let actual = sha256_of(&path).map_err(|source| CheckFileError::Read { path, source })?;
        if actual != expected {
            return Err(CheckFileError::Mismatch(name.clone()));
        }
    }
    Ok(())
}

/// The update's check: a download that does not pass is deleted with the other.
fn check_step(
    placement: &Placement,
    assets: &WindowsAssets,
    checksums: &Checksums,
) -> Result<(), CheckFileError> {
    let checked = check(placement, assets, checksums);
    if checked.is_err() {
        placement.remove_new_files();
    }
    checked
}

/// What a target had before its new file took the place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Aside {
    /// Its file moved aside, to `.old`.
    MovedAside,
    NoFile,
}

/// A target that `put_in_place` moved, which a rollback puts back.
#[derive(Debug, Clone, Copy)]
struct Moved<'path> {
    target: &'path Path,
    aside: Aside,
}

/// Puts each target's `.new` file in its place, and moves the target aside to `.old` first. When
/// a move fails, the files moved so far go back, and nothing changed. `rename` moves a file.
fn put_in_place(
    targets: &[&Path],
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), ReplaceError> {
    let mut moved: Vec<Moved<'_>> = Vec::new();
    for &target in targets {
        let Err(failed) = move_in(target, &rename, &mut moved) else {
            continue;
        };
        return Err(match roll_back(&moved, &rename) {
            Ok(()) => ReplaceError::Moved(failed),
            Err(restore) => ReplaceError::NotRestored {
                failed,
                restore,
                whereabouts: whereabouts(&moved),
            },
        });
    }
    Ok(())
}

/// Moves `target` aside and its `.new` file into its place, and adds it to `moved` once it moved
/// aside.
fn move_in<'path>(
    target: &'path Path,
    rename: &impl Fn(&Path, &Path) -> io::Result<()>,
    moved: &mut Vec<Moved<'path>>,
) -> Result<(), MoveError> {
    let new = with_suffix(target, NEW_SUFFIX);
    let old = with_suffix(target, OLD_SUFFIX);
    match fs::remove_file(&old) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => return Err(MoveError { path: old, source }),
    }
    let aside = match rename(target, &old) {
        Ok(()) => Aside::MovedAside,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Aside::NoFile,
        Err(source) => {
            return Err(MoveError {
                path: target.to_owned(),
                source,
            });
        }
    };
    moved.push(Moved { target, aside });
    rename(&new, target).map_err(|source| MoveError { path: new, source })
}

/// Puts back the files `put_in_place` moved, last first: the new file goes back to `.new`, and
/// the old one to its place. Stops at the first file it cannot move.
fn roll_back(
    moved: &[Moved<'_>],
    rename: &impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), MoveError> {
    for &Moved { target, aside } in moved.iter().rev() {
        if fs::metadata(target).is_ok() {
            let new = with_suffix(target, NEW_SUFFIX);
            rename(target, &new).map_err(|source| MoveError {
                path: target.to_owned(),
                source,
            })?;
        }
        if aside == Aside::MovedAside {
            let old = with_suffix(target, OLD_SUFFIX);
            rename(&old, target).map_err(|source| MoveError { path: old, source })?;
        }
    }
    Ok(())
}

/// Says where the old and the new file of each target in `moved` are, after a rollback that
/// stopped.
fn whereabouts(moved: &[Moved<'_>]) -> String {
    let mut sentences = Vec::new();
    for &Moved { target, aside } in moved {
        let new = with_suffix(target, NEW_SUFFIX);
        let old = with_suffix(target, OLD_SUFFIX);
        let name = target.file_name().unwrap_or(target.as_os_str()).display();
        let old_at = match aside {
            Aside::NoFile => None,
            Aside::MovedAside if old.exists() => Some(old.as_path()),
            Aside::MovedAside => target.exists().then_some(target),
        };
        let new_at = if new.exists() {
            Some(new.as_path())
        } else {
            let in_place = aside == Aside::NoFile || old.exists();
            (in_place && target.exists()).then_some(target)
        };
        let sentence = match (old_at, new_at) {
            (Some(old_at), Some(new_at)) => format!(
                "The {name} from before the update is at {}, and the new one at {}.",
                old_at.display(),
                new_at.display()
            ),
            (Some(old_at), None) => {
                format!(
                    "The {name} from before the update is at {}.",
                    old_at.display()
                )
            }
            (None, Some(new_at)) => format!("The new {name} is at {}.", new_at.display()),
            (None, None) => continue,
        };
        sentences.push(sentence);
    }
    sentences.join(" ")
}

/// The update's move. A failure deletes the downloads once the files are back, and keeps every
/// file when some are not: one of them may be the only copy of the app.
fn replace_step(
    placement: &Placement,
    rename: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<(), ReplaceError> {
    let replaced = put_in_place(&placement.targets(), rename);
    if let Err(ReplaceError::Moved(_)) = replaced {
        placement.remove_new_files();
    }
    replaced
}

/// Runs the update `started` to its end: downloads, checks, then puts the files in place. A
/// "Cancel" before the move ends it and deletes the downloads.
pub async fn run(mut state: Signal<AppState>, started: StartedRun) {
    let id = started.id;
    run_steps(state, started).await;
    app_update::ended(&mut state.write(), id);
}

async fn run_steps(mut state: Signal<AppState>, started: StartedRun) {
    let StartedRun { id, assets } = started;
    let placement = match Placement::of_this_app() {
        Ok(placement) => placement,
        Err(error) => {
            let message = format!("Cannot find the app's program: {error}.");
            fail(state, id, AppStep::Download, message);
            return;
        }
    };
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let downloading = {
        let placement = placement.clone();
        let assets = assets.clone();
        tokio::task::spawn_blocking(move || download(&placement, &assets, &sender))
    };
    while let Some(bytes) = receiver.recv().await {
        if !app_update::is_current(&state.peek(), id) {
            break;
        }
        app_update::downloaded(&mut state.write(), id, bytes);
    }
    // A closed channel stops the download before its next read.
    drop(receiver);
    let ControlFlow::Continue(checksums) =
        finish_step(state, id, AppStep::Download, &placement, downloading).await
    else {
        return;
    };
    app_update::step_started(&mut state.write(), id, AppStep::Check);
    let checking = {
        let placement = placement.clone();
        tokio::task::spawn_blocking(move || check_step(&placement, &assets, &checksums))
    };
    let ControlFlow::Continue(()) =
        finish_step(state, id, AppStep::Check, &placement, checking).await
    else {
        return;
    };
    app_update::step_started(&mut state.write(), id, AppStep::Replace);
    let replacing = {
        let placement = placement.clone();
        tokio::task::spawn_blocking(move || {
            replace_step(&placement, |from, to| fs::rename(from, to))
        })
    };
    let ControlFlow::Continue(()) =
        finish_step(state, id, AppStep::Replace, &placement, replacing).await
    else {
        return;
    };
    tracing::info!(
        "the app's update is in place at {}",
        placement.app.display()
    );
    app_update::ready(&mut state.write(), id, placement.app);
}

/// Waits for `task`, the blocking work of `step`, and gives its result. `Break` when the run ends
/// there: a "Cancel" came, and the downloads go, or the step failed, and the card says why. The
/// steps before the move delete their downloads when they fail.
async fn finish_step<T, E: Display>(
    state: Signal<AppState>,
    id: RunId,
    step: AppStep,
    placement: &Placement,
    task: JoinHandle<Result<T, E>>,
) -> ControlFlow<(), T> {
    let before_the_move = step < AppStep::Replace;
    let result = match task.await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(error) => {
            if before_the_move {
                placement.remove_new_files();
            }
            Err(format!("The step stopped: {error}."))
        }
    };
    if before_the_move && !app_update::is_current(&state.peek(), id) {
        placement.remove_new_files();
        return ControlFlow::Break(());
    }
    match result {
        Ok(value) => ControlFlow::Continue(value),
        Err(message) => {
            fail(state, id, step, message);
            ControlFlow::Break(())
        }
    }
}

fn fail(mut state: Signal<AppState>, id: RunId, step: AppStep, message: String) {
    tracing::warn!("the app's update failed: {message}");
    app_update::failed(&mut state.write(), id, step, message);
}

/// The demo's update: made-up bytes, then the checks, ending ready. Nothing is downloaded.
pub async fn play_demo(mut state: Signal<AppState>, started: StartedRun) {
    let id = started.id;
    play_demo_steps(state, started).await;
    app_update::ended(&mut state.write(), id);
}

async fn play_demo_steps(mut state: Signal<AppState>, started: StartedRun) {
    let id = started.id;
    let total = started.assets.total_size();
    for tenth in 1..=10 {
        tokio::time::sleep(demo::APP_UPDATE_PAUSE).await;
        if !app_update::is_current(&state.peek(), id) {
            return;
        }
        app_update::downloaded(&mut state.write(), id, total * tenth / 10);
    }
    for step in [AppStep::Check, AppStep::Replace] {
        app_update::step_started(&mut state.write(), id, step);
        tokio::time::sleep(demo::APP_UPDATE_PAUSE * 3).await;
        if !app_update::is_current(&state.peek(), id) {
            return;
        }
    }
    app_update::ready(&mut state.write(), id, PathBuf::from(demo::APP_PROGRAM));
}

/// Starts the new app at `app`. It waits for this process to end before it opens its window.
pub fn start_new_app(app: &Path) -> io::Result<()> {
    let mut command = Command::new(app);
    command
        .arg(AFTER_UPDATE_FLAG)
        .arg(std::process::id().to_string());
    // The new app outlives this one, which never waits for it.
    drop(command.spawn()?);
    Ok(())
}

/// The pid that `--after-update` names.
pub fn after_update(arguments: &[String]) -> Option<u32> {
    command_line::flag_value(arguments, AFTER_UPDATE_FLAG)?
        .parse()
        .ok()
}

/// Waits up to `EXIT_WAIT` for the process `pid` to end: the app that started this one after its
/// update still stops its agents and holds the web view's folder.
#[cfg(windows)]
pub fn wait_for_exit(pid: u32) {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    let millis = u32::try_from(EXIT_WAIT.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: OpenProcess takes no pointer. It returns null for a process that ended already.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return;
    }
    // SAFETY: `process` is a handle OpenProcess opened with the right to wait on it, and it is
    // closed once, after the wait.
    let waited = unsafe { WaitForSingleObject(process, millis) };
    // SAFETY: as above.
    unsafe { CloseHandle(process) };
    if waited == WAIT_TIMEOUT {
        tracing::warn!(
            "the app before the update, pid {pid}, still runs after {} s",
            EXIT_WAIT.as_secs()
        );
    }
}

/// Only the Windows zip restarts itself after an update.
#[cfg(not(windows))]
pub fn wait_for_exit(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_check_names_its_reason_in_plain_words() {
        let reason = |error: ureq::Error| CheckError::Http(error).reason();

        assert_eq!(
            reason(ureq::Error::StatusCode(403)),
            "GitHub answered with error 403"
        );
        assert_eq!(reason(ureq::Error::HostNotFound), "cannot reach GitHub");
        assert_eq!(reason(ureq::Error::ConnectionFailed), "cannot reach GitHub");
        assert_eq!(
            reason(ureq::Error::Io(io::ErrorKind::ConnectionReset.into())),
            "cannot reach GitHub"
        );
        assert_eq!(
            reason(ureq::Error::Timeout(ureq::Timeout::Global)),
            "GitHub did not answer in time"
        );
        let answer = CheckError::from(Release::from_github("{}").unwrap_err());
        assert!(
            answer
                .reason()
                .starts_with("GitHub's answer does not read as a release: "),
            "{}",
            answer.reason()
        );
    }

    fn placement(folder: &Path) -> Placement {
        Placement {
            app: folder.join("yetack-desktop.exe"),
            daemon: folder.join(LinuxCpu::X86_64.daemon_file_name()),
        }
    }

    fn rename(from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_owned(),
            url: format!("https://github.com/Henauxg/yetack/releases/download/v9.1.0/{name}"),
            size: 10,
        }
    }

    fn assets() -> WindowsAssets {
        WindowsAssets {
            app: asset("yetack-desktop-9.1.0-windows-x64.exe"),
            daemon: asset("yetack-9.1.0-x86_64-linux"),
            checksums: asset(CHECKSUMS_NAME),
        }
    }

    #[test]
    fn the_new_files_take_the_old_ones_place() {
        let temp = tempfile::tempdir().unwrap();
        let files = placement(temp.path());
        fs::write(&files.app, "old app").unwrap();
        fs::write(&files.daemon, "old daemon").unwrap();
        fs::write(with_suffix(&files.app, NEW_SUFFIX), "new app").unwrap();
        fs::write(with_suffix(&files.daemon, NEW_SUFFIX), "new daemon").unwrap();

        put_in_place(&files.targets(), rename).unwrap();

        assert_eq!(read(&files.app), "new app");
        assert_eq!(read(&files.daemon), "new daemon");
        assert_eq!(read(&with_suffix(&files.app, OLD_SUFFIX)), "old app");

        files.remove_leftovers();
        assert_eq!(
            names(temp.path()),
            ["yetack-desktop.exe", "yetack-x86_64-linux"]
        );
    }

    #[test]
    fn a_file_moved_aside_goes_back_while_nothing_took_its_place() {
        let temp = tempfile::tempdir().unwrap();
        let files = placement(temp.path());
        fs::write(&files.app, "app").unwrap();
        fs::write(with_suffix(&files.app, OLD_SUFFIX), "old app").unwrap();
        fs::write(with_suffix(&files.daemon, OLD_SUFFIX), "only daemon").unwrap();
        fs::write(with_suffix(&files.daemon, NEW_SUFFIX), "partial").unwrap();

        files.remove_leftovers();

        assert_eq!(
            names(temp.path()),
            ["yetack-desktop.exe", "yetack-x86_64-linux"]
        );
        assert_eq!(read(&files.daemon), "only daemon");
    }

    #[test]
    fn the_start_puts_back_an_old_file_whose_place_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let files = placement(temp.path());
        fs::write(with_suffix(&files.app, OLD_SUFFIX), "old app").unwrap();
        fs::write(with_suffix(&files.app, NEW_SUFFIX), "new app").unwrap();
        fs::write(&files.daemon, "daemon").unwrap();

        files.remove_leftovers();

        assert_eq!(read(&files.app), "old app");
        assert_eq!(
            names(temp.path()),
            ["yetack-desktop.exe", "yetack-x86_64-linux"]
        );
    }

    #[test]
    fn a_missing_daemon_takes_the_new_one_and_an_old_leftover_goes() {
        let temp = tempfile::tempdir().unwrap();
        let files = placement(temp.path());
        fs::write(&files.app, "old app").unwrap();
        fs::write(with_suffix(&files.app, OLD_SUFFIX), "older app").unwrap();
        fs::write(with_suffix(&files.app, NEW_SUFFIX), "new app").unwrap();
        fs::write(with_suffix(&files.daemon, NEW_SUFFIX), "new daemon").unwrap();

        put_in_place(&files.targets(), rename).unwrap();

        assert_eq!(read(&files.daemon), "new daemon");
        assert_eq!(read(&with_suffix(&files.app, OLD_SUFFIX)), "old app");
        assert!(!with_suffix(&files.daemon, OLD_SUFFIX).exists());
    }

    #[test]
    fn a_failed_move_puts_the_moved_files_back() {
        let temp = tempfile::tempdir().unwrap();
        let files = placement(temp.path());
        fs::write(&files.app, "old app").unwrap();
        fs::write(&files.daemon, "old daemon").unwrap();
        fs::write(with_suffix(&files.app, NEW_SUFFIX), "new app").unwrap();
        // The daemon's download is missing, so its move fails after the app's.

        let error = put_in_place(&files.targets(), rename).unwrap_err();

        assert!(matches!(error, ReplaceError::Moved(_)), "{error}");
        assert_eq!(read(&files.app), "old app");
        assert_eq!(read(&files.daemon), "old daemon");
        assert_eq!(read(&with_suffix(&files.app, NEW_SUFFIX)), "new app");
        files.remove_new_files();
        assert_eq!(
            names(temp.path()),
            ["yetack-desktop.exe", "yetack-x86_64-linux"]
        );
    }

    #[test]
    fn the_downloads_must_match_their_checksums() {
        let temp = tempfile::tempdir().unwrap();
        let files = placement(temp.path());
        fs::write(with_suffix(&files.app, NEW_SUFFIX), "abc").unwrap();
        fs::write(with_suffix(&files.daemon, NEW_SUFFIX), "").unwrap();
        // The SHA-256 of "abc" and of nothing.
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let sums = |app: &str| {
            Checksums::parse(&format!(
                "{app}  yetack-desktop-9.1.0-windows-x64.exe\n{empty}  yetack-9.1.0-x86_64-linux\n"
            ))
        };

        assert!(check(&files, &assets(), &sums(abc)).is_ok());

        let wrong = check(&files, &assets(), &sums(empty)).unwrap_err();
        assert_eq!(
            wrong.to_string(),
            "The download of yetack-desktop-9.1.0-windows-x64.exe does not match the checksum the release lists."
        );

        let unlisted = check(&files, &assets(), &Checksums::from("")).unwrap_err();
        assert_eq!(
            unlisted.to_string(),
            "SHA256SUMS lists no checksum for yetack-desktop-9.1.0-windows-x64.exe."
        );
    }

    #[test]
    fn after_update_names_a_pid() {
        let arguments = |words: &[&str]| -> Vec<String> {
            words.iter().map(|word| (*word).to_owned()).collect()
        };

        assert_eq!(
            after_update(&arguments(&["--after-update", "4242"])),
            Some(4242)
        );
        assert_eq!(after_update(&arguments(&["--after-update"])), None);
        assert_eq!(after_update(&arguments(&["--after-update", "soon"])), None);
        assert_eq!(after_update(&arguments(&["--demo"])), None);
    }

    /// A `rename` that refuses to move `from` to `to`, and moves every other file.
    fn refusing(refused: &[(PathBuf, PathBuf)]) -> impl Fn(&Path, &Path) -> io::Result<()> {
        move |from: &Path, to: &Path| {
            let pair = (from.to_owned(), to.to_owned());
            if refused.contains(&pair) {
                return Err(io::Error::other("refused"));
            }
            fs::rename(from, to)
        }
    }

    fn with_all_files(folder: &Path) -> Placement {
        let files = placement(folder);
        fs::write(&files.app, "old app").unwrap();
        fs::write(&files.daemon, "old daemon").unwrap();
        fs::write(with_suffix(&files.app, NEW_SUFFIX), "new app").unwrap();
        fs::write(with_suffix(&files.daemon, NEW_SUFFIX), "new daemon").unwrap();
        files
    }

    #[test]
    fn a_rollback_that_fails_deletes_nothing_and_says_where_the_files_are() {
        let temp = tempfile::tempdir().unwrap();
        let files = with_all_files(temp.path());
        let app_new = with_suffix(&files.app, NEW_SUFFIX);
        let app_old = with_suffix(&files.app, OLD_SUFFIX);
        let refused = [
            (with_suffix(&files.daemon, NEW_SUFFIX), files.daemon.clone()),
            (app_old.clone(), files.app.clone()),
        ];

        let error = replace_step(&files, refusing(&refused)).unwrap_err();

        assert!(matches!(error, ReplaceError::NotRestored { .. }), "{error}");
        assert!(
            !files.app.exists(),
            "the rollback stopped before the old app came back"
        );
        assert_eq!(read(&app_new), "new app");
        assert_eq!(read(&app_old), "old app");
        let message = error.to_string();
        let said = |path: &Path| message.contains(&path.display().to_string());
        assert!(said(&app_new) && said(&app_old), "{message}");
        assert!(message.contains("Nothing was deleted."), "{message}");

        files.remove_leftovers();

        assert_eq!(read(&files.app), "old app", "the next start puts it back");
        assert_eq!(read(&files.daemon), "old daemon");
        assert_eq!(
            names(temp.path()),
            ["yetack-desktop.exe", "yetack-x86_64-linux"]
        );
    }

    #[test]
    fn a_move_that_fails_and_comes_back_deletes_the_downloads() {
        let temp = tempfile::tempdir().unwrap();
        let files = with_all_files(temp.path());
        let refused = [(with_suffix(&files.daemon, NEW_SUFFIX), files.daemon.clone())];

        let error = replace_step(&files, refusing(&refused)).unwrap_err();

        assert!(matches!(error, ReplaceError::Moved(_)), "{error}");
        assert_eq!(read(&files.app), "old app");
        assert_eq!(read(&files.daemon), "old daemon");
        assert_eq!(
            names(temp.path()),
            ["yetack-desktop.exe", "yetack-x86_64-linux"]
        );
    }

    #[test]
    fn a_failure_on_the_first_file_moves_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let files = with_all_files(temp.path());
        // A folder where the app's old file goes cannot be deleted as a file.
        let app_old = with_suffix(&files.app, OLD_SUFFIX);
        fs::create_dir(&app_old).unwrap();
        fs::write(app_old.join("inside"), "").unwrap();

        let error = put_in_place(&files.targets(), rename).unwrap_err();

        assert!(matches!(error, ReplaceError::Moved(_)), "{error}");
        assert_eq!(read(&files.app), "old app");
        assert_eq!(read(&files.daemon), "old daemon");
        assert_eq!(read(&with_suffix(&files.app, NEW_SUFFIX)), "new app");
        assert_eq!(read(&with_suffix(&files.daemon, NEW_SUFFIX)), "new daemon");
    }

    #[test]
    fn a_failed_check_deletes_the_downloads() {
        let temp = tempfile::tempdir().unwrap();
        let files = with_all_files(temp.path());

        let error = check_step(&files, &assets(), &Checksums::from("")).unwrap_err();

        assert!(matches!(error, CheckFileError::Unlisted(_)), "{error}");
        assert_eq!(
            names(temp.path()),
            ["yetack-desktop.exe", "yetack-x86_64-linux"]
        );
    }

    /// A body that counts the reads asked of it, and ends at once.
    struct CountedReads(usize);

    impl Read for CountedReads {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            self.0 += 1;
            Ok(0)
        }
    }

    /// A body whose connection breaks.
    struct Broken;

    impl Read for Broken {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::ConnectionReset))
        }
    }

    #[test]
    fn a_cancel_stops_the_download_before_its_next_read() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("app.exe.new");
        let (progress, receiver) = mpsc::unbounded_channel();
        drop(receiver);
        let mut body = CountedReads(0);
        let mut done = 0;

        let error = write_file(&mut body, &assets().app, &path, &mut done, &progress).unwrap_err();

        assert!(matches!(error, DownloadError::Cancelled), "{error}");
        assert_eq!(body.0, 0);
    }

    #[test]
    fn a_download_past_the_limit_stops() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("app.exe.new");
        let (progress, _receiver) = mpsc::unbounded_channel();
        let mut done = MAX_DOWNLOAD_BYTES - 3;

        let error = write_file(
            &mut io::Cursor::new(b"ten bytes!".to_vec()),
            &assets().app,
            &path,
            &mut done,
            &progress,
        )
        .unwrap_err();

        assert!(matches!(error, DownloadError::TooLarge), "{error}");
        assert_eq!(
            error.to_string(),
            "The release's files weigh more than 200 MB."
        );
    }

    #[test]
    fn a_download_writes_its_file_and_reports_its_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("app.exe.new");
        let (progress, mut receiver) = mpsc::unbounded_channel();
        let mut done = 400;

        write_file(
            &mut io::Cursor::new(b"new app".to_vec()),
            &assets().app,
            &path,
            &mut done,
            &progress,
        )
        .unwrap();

        assert_eq!(read(&path), "new app");
        assert_eq!(done, 407);
        assert_eq!(receiver.try_recv().ok(), Some(407));
    }

    #[test]
    fn a_write_error_names_the_file_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing folder").join("app.exe.new");
        let (progress, _receiver) = mpsc::unbounded_channel();

        let error = write_file(&mut Broken, &assets().app, &path, &mut 0, &progress).unwrap_err();

        assert!(
            matches!(&error, DownloadError::Write { path: named, .. } if *named == path),
            "{error}"
        );
    }

    #[test]
    fn a_failed_download_deletes_the_files_written_so_far() {
        let temp = tempfile::tempdir().unwrap();
        let files = placement(temp.path());
        fs::write(&files.app, "old app").unwrap();
        let (progress, _receiver) = mpsc::unbounded_channel();
        let mut opened = Vec::new();

        let error = download_from(&files, &assets(), &progress, |asset| {
            opened.push(asset.name.clone());
            if asset.name.ends_with("x86_64-linux") {
                return Err(DownloadError::Http {
                    name: asset.name.clone(),
                    source: ureq::Error::StatusCode(404),
                });
            }
            Ok(io::Cursor::new(b"bytes".to_vec()))
        })
        .unwrap_err();

        assert!(matches!(error, DownloadError::Http { .. }), "{error}");
        assert_eq!(
            opened,
            [
                CHECKSUMS_NAME,
                "yetack-desktop-9.1.0-windows-x64.exe",
                "yetack-9.1.0-x86_64-linux"
            ]
        );
        assert_eq!(names(temp.path()), ["yetack-desktop.exe"]);
    }

    #[test]
    fn a_broken_connection_fails_the_download_and_deletes_its_file() {
        let temp = tempfile::tempdir().unwrap();
        let files = placement(temp.path());
        let (progress, _receiver) = mpsc::unbounded_channel();

        let error = download_from(&files, &assets(), &progress, |asset| {
            let body: Box<dyn Read> = if asset.name == CHECKSUMS_NAME {
                Box::new(io::Cursor::new(Vec::new()))
            } else {
                Box::new(Broken)
            };
            Ok(body)
        })
        .unwrap_err();

        assert!(
            error
                .to_string()
                .starts_with("Cannot download yetack-desktop-9.1.0-windows-x64.exe: "),
            "{error}"
        );
        assert!(names(temp.path()).is_empty());
    }

    #[test]
    fn only_the_zip_tidies_its_folder_at_the_start() {
        let temp = tempfile::tempdir().unwrap();
        let files = placement(temp.path());
        fs::write(&files.app, "app").unwrap();
        fs::write(with_suffix(&files.app, OLD_SUFFIX), "old app").unwrap();

        for install in [
            InstallKind::Deb,
            InstallKind::Rpm,
            InstallKind::Tarball,
            InstallKind::Nix,
            InstallKind::Source,
        ] {
            remove_leftovers_of(install, || Ok(files.clone()));
            assert_eq!(
                names(temp.path()),
                ["yetack-desktop.exe", "yetack-desktop.exe.old"],
                "{install:?}"
            );
        }

        remove_leftovers_of(InstallKind::Zip, || Ok(files.clone()));
        assert_eq!(names(temp.path()), ["yetack-desktop.exe"]);
    }
}
