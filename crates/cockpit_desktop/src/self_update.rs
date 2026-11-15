//! The app's own update, beside the window. The app asks GitHub for its latest release about ten
//! seconds after it starts, then once a day.
//!
//! The Windows zip updates itself: it downloads the release's app and Linux daemon beside its own
//! files, as `<name>.new`, and checks them against the release's `SHA256SUMS`. Windows lets a
//! program rename its running file but not write over it, so the app then moves each of its files
//! aside to `<name>.old` and the new one into its place. The new app, started with
//! `--after-update <pid>`, waits for this one to end, and deletes what the update left.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use dioxus::prelude::{ReadableExt, Signal, UnboundedReceiver, WritableExt};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tokio::task::JoinError;
use tokio::time::Instant;

use crate::demo;
use crate::server_daemon::{self, CARRIED_FILE_NAME};
use crate::state::AppState;
use crate::state::app_update::{
    self, AppStep, CHECKSUMS_NAME, Checksums, InstallKind, MAX_DOWNLOAD_BYTES, Release,
    ReleaseAsset, StartedRun, WindowsAssets,
};
use crate::update_script::APP_VERSION;

const LATEST_URL: &str = "https://api.github.com/repos/Henauxg/yetack/releases/latest";
const GITHUB_JSON: &str = "application/vnd.github+json";

/// GitHub's answer is a few kilobytes, more with long release notes.
const MAX_ANSWER_BYTES: u64 = 4 * 1024 * 1024;

/// The app the update started passes the pid of the one that closes: `--after-update 1234`.
pub const AFTER_UPDATE_FLAG: &str = "--after-update";

const NEW_SUFFIX: &str = ".new";
const OLD_SUFFIX: &str = ".old";

/// How long a connection may stay silent before the request fails.
const SILENCE: Duration = Duration::from_secs(30);

/// How many bytes the download reads between two reports of its progress.
const PROGRESS_STEP: u64 = 256 * 1024;

const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error(transparent)]
    Http(#[from] ureq::Error),
    #[error("GitHub's answer does not read as a release: {0}")]
    Answer(#[from] serde_json::Error),
    #[error("the check stopped: {0}")]
    Stopped(#[from] JoinError),
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("cannot download {name}: {source}")]
    Http { name: String, source: ureq::Error },
    #[error("cannot write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("the release's files weigh more than {} MB", MAX_DOWNLOAD_BYTES / 1024 / 1024)]
    TooLarge,
    #[error("the download was cancelled")]
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum CheckFileError {
    #[error("{CHECKSUMS_NAME} lists no checksum for {0}.")]
    Unlisted(String),
    #[error("The download of {0} does not match the checksum the release lists.")]
    Mismatch(String),
    #[error("cannot read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
}

#[derive(Debug, thiserror::Error)]
pub enum ReplaceError {
    #[error("cannot move {}: {source}", path.display())]
    Moved { path: PathBuf, source: io::Error },
    #[error(
        "cannot move {}: {source}. Putting the files back failed too, at {}: {restore}",
        path.display(),
        restored.display()
    )]
    NotRestored {
        path: PathBuf,
        source: io::Error,
        restored: PathBuf,
        restore: io::Error,
    },
}

/// The client every request goes through: the system's TLS and its root certificates, HTTPS
/// only, and a name that says which app asks.
fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

    let tls = TlsConfig::builder()
        .provider(TlsProvider::NativeTls)
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .https_only(true)
        .user_agent(format!("yetack-desktop/{APP_VERSION}"))
        .timeout_connect(Some(SILENCE))
        .timeout_per_read(Some(SILENCE))
        .build()
        .into()
}

fn fetch_latest() -> Result<Release, CheckError> {
    let answer = agent()
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

/// Checks for a newer release `FIRST_CHECK` after the start, then once a day, and at once when
/// `asked` says the setting turned on. The demo makes one up and never touches the network. A
/// failed check goes to the log, and the next round tries again.
pub async fn check_releases(mut state: Signal<AppState>, mut asked: UnboundedReceiver<()>) {
    use futures_util::StreamExt;

    let demo = state.peek().app_update.demo;
    let first = if demo {
        demo::APP_RELEASE_DELAY
    } else {
        FIRST_CHECK
    };
    let mut next = Instant::now() + first;
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(next) => {}
            ask = asked.next() => {
                if ask.is_none() {
                    return;
                }
            }
        }
        next = Instant::now() + CHECK_EVERY;
        if !state.peek().prefs.updates.check {
            continue;
        }
        let release = if demo {
            demo::app_release()
        } else {
            match latest_release().await {
                Ok(release) => release,
                Err(error) => {
                    tracing::warn!("cannot check for a new release of the app: {error}");
                    continue;
                }
            }
        };
        tracing::info!("the latest release of the app is {}", release.version);
        app_update::found(&mut state.write(), release);
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
            daemon: folder.join(CARRIED_FILE_NAME),
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

    /// Deletes what an earlier update left: the files it moved aside, and downloads it did not
    /// use. A file moved aside stays while nothing took its place: it is the only copy.
    pub fn remove_leftovers(&self) {
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

/// Deletes what an earlier update left beside this app. Only the zip updates itself, so only its
/// folder holds such files.
pub fn remove_leftovers() {
    if !InstallKind::of_this_build().updates_itself() {
        return;
    }
    match Placement::of_this_app() {
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
    let agent = agent();
    let mut done = 0;
    let mut sums = Vec::new();
    fetch(&agent, &assets.checksums, &mut sums, &mut done, progress)?;
    let checksums = Checksums::parse(&String::from_utf8_lossy(&sums));
    let files = [
        (&assets.app, &placement.app),
        (&assets.daemon, &placement.daemon),
    ];
    for (asset, target) in files {
        let path = with_suffix(target, NEW_SUFFIX);
        let written = File::create(&path).and_then(|mut file| {
            let fetched = fetch(&agent, asset, &mut file, &mut done, progress);
            file.sync_all()?;
            Ok(fetched)
        });
        match written {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                placement.remove_new_files();
                return Err(error);
            }
            Err(source) => {
                placement.remove_new_files();
                return Err(DownloadError::Write { path, source });
            }
        }
    }
    Ok(checksums)
}

/// Copies `asset` into `out`, adding its bytes to `done`.
fn fetch(
    agent: &ureq::Agent,
    asset: &ReleaseAsset,
    out: &mut impl Write,
    done: &mut u64,
    progress: &mpsc::UnboundedSender<u64>,
) -> Result<(), DownloadError> {
    let http = |source| DownloadError::Http {
        name: asset.name.clone(),
        source,
    };
    let response = agent.get(&asset.url).call().map_err(http)?;
    let mut body = response
        .into_body()
        .into_with_config()
        .limit(MAX_DOWNLOAD_BYTES)
        .reader();
    let mut chunk = vec![0; 64 * 1024];
    let mut reported = *done;
    loop {
        let read = match body.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(http(ureq::Error::Io(error))),
        };
        *done = done.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if *done > MAX_DOWNLOAD_BYTES {
            return Err(DownloadError::TooLarge);
        }
        let bytes = chunk.get(..read).unwrap_or_default();
        out.write_all(bytes)
            .map_err(|source| DownloadError::Write {
                path: PathBuf::from(&asset.name),
                source,
            })?;
        if *done - reported >= PROGRESS_STEP {
            reported = *done;
            progress.send(*done).map_err(|_| DownloadError::Cancelled)?;
        }
    }
    progress.send(*done).map_err(|_| DownloadError::Cancelled)
}

/// The file's SHA-256, in lowercase hex.
fn sha256_of(path: &Path) -> io::Result<String> {
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

/// Puts each target's `.new` file in its place, and moves the target aside to `.old` first. When
/// a move fails, the files moved so far go back, and nothing changed.
fn put_in_place(targets: &[&Path]) -> Result<(), ReplaceError> {
    // Each target moved so far, and whether it had a file to move aside.
    let mut moved: Vec<(&Path, bool)> = Vec::new();
    for &target in targets {
        let new = with_suffix(target, NEW_SUFFIX);
        let old = with_suffix(target, OLD_SUFFIX);
        let fail = |path: &Path, source: io::Error, moved: &[(&Path, bool)]| match roll_back(moved)
        {
            Ok(()) => ReplaceError::Moved {
                path: path.to_owned(),
                source,
            },
            Err((restored, restore)) => ReplaceError::NotRestored {
                path: path.to_owned(),
                source,
                restored,
                restore,
            },
        };
        match fs::remove_file(&old) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(fail(&old, error, &moved)),
        }
        let aside = match fs::rename(target, &old) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(fail(target, error, &moved)),
        };
        moved.push((target, aside));
        if let Err(error) = fs::rename(&new, target) {
            let restored = !aside || fs::rename(&old, target).is_ok();
            if restored {
                moved.pop();
            }
            return Err(fail(&new, error, &moved));
        }
    }
    Ok(())
}

/// Puts back the files `put_in_place` moved, last first: the new file goes back to `.new`, and
/// the old one to its place. Returns the first file it could not put back.
fn roll_back(moved: &[(&Path, bool)]) -> Result<(), (PathBuf, io::Error)> {
    for &(target, aside) in moved.iter().rev() {
        let new = with_suffix(target, NEW_SUFFIX);
        if fs::metadata(target).is_ok() {
            fs::rename(target, &new).map_err(|error| (target.to_owned(), error))?;
        }
        if aside {
            let old = with_suffix(target, OLD_SUFFIX);
            fs::rename(&old, target).map_err(|error| (old, error))?;
        }
    }
    Ok(())
}

/// Runs the update `started` to its end: downloads, checks, then puts the files in place. A
/// "Cancel" before the move ends it and deletes the downloads.
pub async fn run(mut state: Signal<AppState>, started: StartedRun) {
    let id = started.id;
    run_steps(state, started).await;
    app_update::ended(&mut state.write(), id);
}

async fn run_steps(mut state: Signal<AppState>, started: StartedRun) {
    let StartedRun { id, assets, .. } = started;
    let mut fail = move |step: AppStep, message: String| {
        tracing::warn!("the app's update failed: {message}");
        app_update::failed(&mut state.write(), id, step, message);
    };
    let placement = match Placement::of_this_app() {
        Ok(placement) => placement,
        Err(error) => {
            fail(
                AppStep::Download,
                format!("cannot find the app's program: {error}"),
            );
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
    // A closed channel stops the download at its next report.
    drop(receiver);
    let downloaded = joined(downloading.await);
    if !app_update::is_current(&state.peek(), id) {
        placement.remove_new_files();
        return;
    }
    let checksums = match downloaded {
        Ok(checksums) => checksums,
        Err(message) => {
            placement.remove_new_files();
            fail(AppStep::Download, message);
            return;
        }
    };
    app_update::step_started(&mut state.write(), id, AppStep::Check);
    let checked = {
        let placement = placement.clone();
        joined(tokio::task::spawn_blocking(move || check(&placement, &assets, &checksums)).await)
    };
    if !app_update::is_current(&state.peek(), id) {
        placement.remove_new_files();
        return;
    }
    if let Err(message) = checked {
        placement.remove_new_files();
        fail(AppStep::Check, message);
        return;
    }
    app_update::step_started(&mut state.write(), id, AppStep::Replace);
    let replaced = {
        let placement = placement.clone();
        joined(tokio::task::spawn_blocking(move || put_in_place(&placement.targets())).await)
    };
    match replaced {
        Ok(()) => {
            tracing::info!(
                "the app's update is in place at {}",
                placement.app.display()
            );
            app_update::ready(&mut state.write(), id, placement.app);
        }
        Err(message) => {
            placement.remove_new_files();
            fail(AppStep::Replace, message);
        }
    }
}

/// The result of a blocking step, with its error as the card says it.
fn joined<T, E: ToString>(result: Result<Result<T, E>, JoinError>) -> Result<T, String> {
    match result {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    }
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
    let flag = arguments
        .iter()
        .position(|argument| argument == AFTER_UPDATE_FLAG)?;
    arguments.get(flag + 1)?.parse().ok()
}

/// Waits up to 30 s for the process `pid` to end: the app that started this one after its update
/// still stops its agents and holds the web view's folder.
#[cfg(windows)]
pub fn wait_for_exit(pid: u32) {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    const EXIT_WAIT_MILLIS: u32 = 30_000;

    // SAFETY: OpenProcess takes no pointer. It returns null for a process that ended already.
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return;
    }
    // SAFETY: `process` is a handle OpenProcess opened with the right to wait on it, and it is
    // closed once, after the wait.
    let waited = unsafe { WaitForSingleObject(process, EXIT_WAIT_MILLIS) };
    // SAFETY: as above.
    unsafe { CloseHandle(process) };
    if waited == WAIT_TIMEOUT {
        tracing::warn!("the app before the update, pid {pid}, still runs after 30 s");
    }
}

/// Only the Windows zip restarts itself after an update.
#[cfg(not(windows))]
pub fn wait_for_exit(_pid: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(folder: &Path) -> Placement {
        Placement {
            app: folder.join("yetack-desktop.exe"),
            daemon: folder.join(CARRIED_FILE_NAME),
        }
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

        put_in_place(&files.targets()).unwrap();

        assert_eq!(read(&files.app), "new app");
        assert_eq!(read(&files.daemon), "new daemon");
        assert_eq!(read(&with_suffix(&files.app, OLD_SUFFIX)), "old app");

        files.remove_leftovers();
        assert_eq!(
            names(temp.path()),
            ["yetack-desktop.exe", CARRIED_FILE_NAME]
        );
    }

    #[test]
    fn a_file_moved_aside_stays_while_nothing_took_its_place() {
        let temp = tempfile::tempdir().unwrap();
        let files = placement(temp.path());
        fs::write(&files.app, "app").unwrap();
        fs::write(with_suffix(&files.app, OLD_SUFFIX), "old app").unwrap();
        fs::write(with_suffix(&files.daemon, OLD_SUFFIX), "only daemon").unwrap();
        fs::write(with_suffix(&files.daemon, NEW_SUFFIX), "partial").unwrap();

        files.remove_leftovers();

        assert_eq!(
            names(temp.path()),
            ["yetack-desktop.exe", "yetack-x86_64-linux.old"]
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

        put_in_place(&files.targets()).unwrap();

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

        let error = put_in_place(&files.targets()).unwrap_err();

        assert!(matches!(error, ReplaceError::Moved { .. }), "{error}");
        assert_eq!(read(&files.app), "old app");
        assert_eq!(read(&files.daemon), "old daemon");
        assert_eq!(read(&with_suffix(&files.app, NEW_SUFFIX)), "new app");
        files.remove_new_files();
        assert_eq!(
            names(temp.path()),
            ["yetack-desktop.exe", CARRIED_FILE_NAME]
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

        let unlisted = check(&files, &assets(), &Checksums::parse("")).unwrap_err();
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
}
