//! The app's own update: the latest release GitHub names, whether the app offers it, how this app
//! was installed, and the steps of the Windows zip's update, which downloads the release, checks
//! it and puts it in place of the app.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use cockpit_protocol::{ReleaseVersion, Timestamp};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::Deserialize;
use strum::{EnumString, IntoStaticStr, VariantArray};

use super::app_state::AppState;
use super::daemon_probe::LinuxCpu;
use super::ui_state::Dialog;
use crate::update_script::APP_VERSION;
use crate::version_order;

/// The app's repository on GitHub, as `owner/name`. A macro, so that `concat!` builds its
/// addresses.
macro_rules! repository {
    () => {
        "Henauxg/yetack"
    };
}
pub(crate) use repository;

/// The repository's release pages.
pub const RELEASES_URL: &str = concat!("https://github.com/", repository!(), "/releases");

/// Where every file of a release downloads from. A release names its files' addresses, and the
/// app downloads none from anywhere else.
pub const DOWNLOAD_PREFIX: &str =
    concat!("https://github.com/", repository!(), "/releases/download/");

/// The most a release's files may weigh together.
pub const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

/// The file of a release that lists the checksum of each other file.
pub const CHECKSUMS_NAME: &str = "SHA256SUMS";

/// How many lines of the release notes the offer shows.
const NOTE_LINES: usize = 8;

/// The time between two checks for a new release.
pub const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// The shortest wait before the first check, so that it does not slow the start.
const FIRST_CHECK: Duration = Duration::from_secs(10);

/// How this app was installed, which decides how it updates. The build names it in
/// `YETACK_PACKAGE`: the release's Windows zip, its `.deb`, its `.rpm`, its tarball, or the flake.
/// Any other build is one from source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
#[strum(serialize_all = "snake_case")]
pub enum InstallKind {
    Zip,
    Deb,
    Rpm,
    Tarball,
    Nix,
    Source,
}

impl InstallKind {
    pub fn of_this_build() -> Self {
        Self::from_package(option_env!("YETACK_PACKAGE"))
    }

    fn from_package(package: Option<&str>) -> Self {
        package
            .and_then(|word| word.parse().ok())
            .unwrap_or(Self::Source)
    }

    /// Only the zip replaces itself: apt, dnf or Nix own the packages' files, the tarball's
    /// `install.sh` puts its own in place, and a build from source is the user's own.
    pub fn updates_itself(self) -> bool {
        self == Self::Zip
    }

    /// The card's lead under its title: "You run 0.3.0, from Nix."
    pub fn lead(self, app_version: &str) -> String {
        match self {
            Self::Zip => format!("You run {app_version}."),
            Self::Deb => format!("You run {app_version}, from the .deb."),
            Self::Rpm => format!("You run {app_version}, from the .rpm."),
            Self::Tarball => format!("You run {app_version}, from the tarball."),
            Self::Nix => format!("You run {app_version}, from Nix."),
            Self::Source => format!("You run {app_version}, built from source."),
        }
    }

    /// What the About section says under a newer release, after when it came out.
    pub fn about_line(self) -> &'static str {
        match self {
            Self::Zip => "The update downloads it, checks it, and restarts the app.",
            Self::Deb => "Install its .deb to update.",
            Self::Rpm => "Install its .rpm to update.",
            Self::Tarball => "Run its tarball's install.sh to update.",
            Self::Nix => "Move to it in your Nix configuration to update.",
            Self::Source => "Build it from your checkout to update.",
        }
    }
}

/// The command that installs a release's `.deb`, from the folder it was downloaded to.
pub fn apt_command(version: &ReleaseVersion) -> String {
    format!("sudo apt install ./yetack_{version}_amd64.deb")
}

/// The command that installs a release's `.rpm`, from the folder it was downloaded to.
pub fn dnf_command(version: &ReleaseVersion) -> String {
    format!("sudo dnf install ./yetack-{version}-1.x86_64.rpm")
}

/// The commands that unpack a release's tarball and install it, from the folder it was
/// downloaded to.
pub fn tarball_command(version: &ReleaseVersion) -> String {
    let folder = format!("yetack-desktop-{version}-x86_64-linux");
    format!("tar xzf {folder}.tar.gz && ./{folder}/install.sh")
}

/// The commands that build a release in a checkout of the repository.
pub fn build_command(version: &ReleaseVersion) -> String {
    format!("git fetch --tags && git checkout v{version} && just build && just linux-daemon")
}

/// The release's Windows app alone, which the Windows zip's update downloads.
pub fn app_asset_name(version: &ReleaseVersion) -> String {
    format!("yetack-desktop-{version}-windows-x64.exe")
}

/// The release's static Linux daemon, which the Windows zip carries beside the app.
pub fn daemon_asset_name(version: &ReleaseVersion) -> String {
    LinuxCpu::X86_64.release_file_name(version)
}

/// A release, as GitHub's API names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Without the tag's leading `v`.
    pub version: ReleaseVersion,
    pub published: Option<Timestamp>,
    /// Markdown.
    pub notes: String,
    /// Its page, as GitHub names it. `page()` checks it.
    pub html_url: String,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    pub size: u64,
}

/// The fields of GitHub's answer the app reads.
#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    published_at: Option<String>,
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

/// Whether `text` is made of the characters a version has: letters, digits, `.` and `-`. The
/// card shows the version inside commands to copy, so nothing else may get in.
fn is_version_text(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|letter| letter.is_ascii_alphanumeric() || letter == '.' || letter == '-')
}

impl Release {
    /// Reads GitHub's answer for a release. A tag that is not a version does not read.
    pub fn from_github(json: &str) -> Result<Self, serde_json::Error> {
        use serde::de::Error as _;

        let read: GitHubRelease = serde_json::from_str(json)?;
        let version = read.tag_name.strip_prefix('v').unwrap_or(&read.tag_name);
        if !is_version_text(version) {
            return Err(serde_json::Error::custom(format!(
                "its tag {:?} is not a version",
                read.tag_name
            )));
        }
        let version = ReleaseVersion::new(version.to_owned());
        let published = read.published_at.as_deref().and_then(|text| {
            let moment: jiff::Timestamp = text.parse().ok()?;
            u64::try_from(moment.as_millisecond())
                .ok()
                .map(Timestamp::from_unix_millis)
        });
        Ok(Self {
            version,
            published,
            notes: read.body.unwrap_or_default(),
            html_url: read.html_url,
            assets: read.assets,
        })
    }

    /// Whether the release is newer than `version`. A version the app cannot read is not.
    pub fn is_newer_than(&self, version: &str) -> bool {
        version_order::compare(self.version.as_str(), version) == Some(Ordering::Greater)
    }

    /// Its page, when it is on the repository's GitHub. The list of releases otherwise.
    pub fn page(&self) -> &str {
        if self.html_url.starts_with(RELEASES_URL) {
            &self.html_url
        } else {
            RELEASES_URL
        }
    }

    /// The files the Windows zip's update downloads: the app, the Linux daemon beside it, and the
    /// checksums. `None` when one is missing, downloads from elsewhere, or they weigh too much.
    pub fn windows_assets(&self) -> Option<WindowsAssets> {
        let asset = |name: &str| {
            self.assets
                .iter()
                .find(|asset| asset.name == name && asset.url.starts_with(DOWNLOAD_PREFIX))
                .cloned()
        };
        let assets = WindowsAssets {
            app: asset(&app_asset_name(&self.version))?,
            daemon: asset(&daemon_asset_name(&self.version))?,
            checksums: asset(CHECKSUMS_NAME)?,
        };
        (assets.total_size() <= MAX_DOWNLOAD_BYTES).then_some(assets)
    }
}

/// The files of a release that the Windows zip's update downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsAssets {
    pub app: ReleaseAsset,
    pub daemon: ReleaseAsset,
    pub checksums: ReleaseAsset,
}

impl WindowsAssets {
    /// What the release says the files weigh.
    pub fn total_size(&self) -> u64 {
        self.app
            .size
            .saturating_add(self.daemon.size)
            .saturating_add(self.checksums.size)
    }
}

/// A release's `SHA256SUMS`: a line `<hex>  <name>` per file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Checksums(BTreeMap<String, String>);

impl Checksums {
    /// Reads the lines `sha256sum` writes, in text or binary mode (`*` before the name). A line
    /// without 64 hex digits is left out.
    pub fn parse(text: &str) -> Self {
        let mut sums = BTreeMap::new();
        for line in text.lines() {
            let Some((sum, name)) = line.split_once(' ') else {
                continue;
            };
            let name = name.trim_start_matches(' ');
            let name = name.strip_prefix('*').unwrap_or(name).trim_end();
            let is_sum = sum.len() == 64 && sum.chars().all(|digit| digit.is_ascii_hexdigit());
            if is_sum && !name.is_empty() {
                sums.insert(name.to_owned(), sum.to_ascii_lowercase());
            }
        }
        Self(sums)
    }

    /// The checksum of `name`, in lowercase hex.
    pub fn of(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

impl From<&str> for Checksums {
    fn from(text: &str) -> Self {
        Self::parse(text)
    }
}

/// A step of the Windows zip's update, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, VariantArray)]
pub enum AppStep {
    Download,
    Check,
    Replace,
    Restart,
}

/// Where a step of the update stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepMark {
    Done,
    Now,
    Failed,
    ToCome,
}

/// Tells the runs of the update apart, so that a cancelled run's late news changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunId(u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunPhase {
    Running(AppStep),
    /// The new app is in place, at this path, and runs from the next start.
    Ready(PathBuf),
    Failed {
        step: AppStep,
        message: String,
    },
}

/// The Windows zip's update of the app, from "Update and restart".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateRun {
    pub id: RunId,
    pub version: ReleaseVersion,
    pub downloaded: u64,
    pub total: u64,
    pub phase: RunPhase,
}

impl UpdateRun {
    /// Whether "Cancel" still stops it: once the files move, they go to the end.
    pub fn offers_cancel(&self) -> bool {
        matches!(self.phase, RunPhase::Running(step) if step < AppStep::Replace)
    }

    /// The step under way or the one that failed. Ready waits for the restart.
    pub fn current_step(&self) -> AppStep {
        match self.phase {
            RunPhase::Running(step) | RunPhase::Failed { step, .. } => step,
            RunPhase::Ready(_) => AppStep::Restart,
        }
    }

    /// Why the run failed, when it did.
    pub fn failure(&self) -> Option<&str> {
        match &self.phase {
            RunPhase::Failed { message, .. } => Some(message),
            RunPhase::Running(_) | RunPhase::Ready(_) => None,
        }
    }

    /// Where `step` stands: done before the current step, failed or under way at it.
    pub fn mark(&self, step: AppStep) -> StepMark {
        match step.cmp(&self.current_step()) {
            Ordering::Less => StepMark::Done,
            Ordering::Equal if self.failure().is_some() => StepMark::Failed,
            Ordering::Equal => StepMark::Now,
            Ordering::Greater => StepMark::ToCome,
        }
    }

    /// The share done, for the sidebar's bar: the download is most of it.
    pub fn percent(&self) -> u8 {
        let downloaded = (self.downloaded.min(self.total) * 90)
            .checked_div(self.total)
            .unwrap_or(0);
        let percent = match self.phase {
            RunPhase::Running(AppStep::Download) => downloaded,
            RunPhase::Running(AppStep::Check) => 92,
            RunPhase::Running(AppStep::Replace | AppStep::Restart) | RunPhase::Ready(_) => 96,
            RunPhase::Failed { .. } => 0,
        };
        u8::try_from(percent).unwrap_or(100)
    }
}

/// What the app knows of its own releases, and the update under way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppUpdate {
    pub install: InstallKind,
    /// The newest release the last check found, when it is newer than this app.
    pub release: Option<Release>,
    /// The release whose sidebar note "Not now" hides until the app's next start. A newer one
    /// shows again.
    pub not_now: Option<ReleaseVersion>,
    pub run: Option<UpdateRun>,
    runs: u32,
    /// A cancelled run whose task still deletes its downloads. No run starts until it ends, so
    /// that it deletes none of the next run's files.
    stopping: Option<RunId>,
    /// A run that no task plays, which a demo screen shows. "Cancel" ends it at once.
    without_task: Option<RunId>,
    /// The demo's update plays made-up steps and never touches the network.
    pub demo: bool,
}

impl AppUpdate {
    pub fn new() -> Self {
        Self {
            install: InstallKind::of_this_build(),
            release: None,
            not_now: None,
            run: None,
            runs: 0,
            stopping: None,
            without_task: None,
            demo: false,
        }
    }
}

impl Default for AppUpdate {
    fn default() -> Self {
        Self::new()
    }
}

/// How long the release check waits after the start: until a day after the last check, which
/// `last_check` gives in Unix seconds, and at least `FIRST_CHECK`. A last check the clock puts
/// after `now` counts for nothing.
pub fn first_check_delay(now: SystemTime, last_check: Option<u64>) -> Duration {
    let since_last = last_check
        .and_then(|seconds| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds)))
        .and_then(|last| now.duration_since(last).ok());
    match since_last {
        Some(since_last) => DAY.saturating_sub(since_last).max(FIRST_CHECK),
        None => FIRST_CHECK,
    }
}

/// A check got GitHub's answer at `now`: the next start waits a day from then.
pub fn checked(state: &mut AppState, now: SystemTime) {
    if let Ok(since_epoch) = now.duration_since(SystemTime::UNIX_EPOCH) {
        state.prefs.updates.last_check = Some(since_epoch.as_secs());
    }
}

/// A check found `release`. The app keeps it only when it is newer than this app, and keeps the
/// release of an update under way.
pub fn found(state: &mut AppState, release: Release) {
    if state.app_update.run.is_some() {
        return;
    }
    state.app_update.release = release.is_newer_than(APP_VERSION).then_some(release);
}

/// The newer release the app tells about: the check is on, and the user did not skip it.
pub fn offered(state: &AppState) -> Option<&Release> {
    let release = state.app_update.release.as_ref()?;
    let skipped = state.prefs.updates.skipped.as_ref() == Some(&release.version);
    (state.prefs.updates.check && !skipped).then_some(release)
}

/// The files of the update this app can run: it is the zip, and the release has its Windows
/// files.
fn update_assets(update: &AppUpdate) -> Option<WindowsAssets> {
    if !update.install.updates_itself() {
        return None;
    }
    update.release.as_ref()?.windows_assets()
}

/// Whether this app updates itself to the release it knows, or only tells how to.
pub fn can_update(state: &AppState) -> bool {
    update_assets(&state.app_update).is_some()
}

/// What the note at the sidebar's foot says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarNotice {
    /// "0.4.0 is out", with "Update" and the cross that skips it.
    Out(ReleaseVersion),
    /// "Downloading 0.4.0", with "Show" and a bar.
    Progress { text: String, percent: u8 },
    /// "0.4.0 is ready", with "Restart".
    Ready(ReleaseVersion),
    /// "Update failed", with "Show".
    Failed,
}

pub fn notice(state: &AppState) -> Option<SidebarNotice> {
    let update = &state.app_update;
    if let Some(run) = &update.run {
        let version = &run.version;
        return Some(match &run.phase {
            RunPhase::Running(AppStep::Download) => SidebarNotice::Progress {
                text: format!("Downloading {version}"),
                percent: run.percent(),
            },
            RunPhase::Running(_) => SidebarNotice::Progress {
                text: format!("Checking {version}"),
                percent: run.percent(),
            },
            RunPhase::Ready(_) => SidebarNotice::Ready(version.clone()),
            RunPhase::Failed { .. } if update.not_now.as_ref() == Some(version) => return None,
            RunPhase::Failed { .. } => SidebarNotice::Failed,
        });
    }
    let release = offered(state)?;
    let hidden = update.not_now.as_ref() == Some(&release.version);
    (!hidden).then(|| SidebarNotice::Out(release.version.clone()))
}

pub fn open_card(state: &mut AppState) {
    state.ui.dialog = Some(Dialog::AppUpdate);
}

/// "Not now": the card closes, and the note of this release hides until the next start.
pub fn not_now(state: &mut AppState) {
    state.app_update.not_now = state
        .app_update
        .release
        .as_ref()
        .map(|release| release.version.clone());
    close_card(state);
}

/// The note's cross, or "Skip this version": the app tells of this release no more, and of the
/// next one again.
pub fn skip(state: &mut AppState) {
    if let Some(release) = &state.app_update.release {
        state.prefs.updates.skipped = Some(release.version.clone());
    }
    close_card(state);
}

/// The note's cross after a failure: the note of this release hides until the next start, and
/// the card offers the release again. A failed restart stays: its new app is in place already.
pub fn dismiss_failure(state: &mut AppState) {
    let update = &mut state.app_update;
    let Some(run) = &update.run else {
        return;
    };
    update.not_now = Some(run.version.clone());
    if matches!(run.phase, RunPhase::Failed { step, .. } if step < AppStep::Restart) {
        update.run = None;
    }
}

fn close_card(state: &mut AppState) {
    if state.ui.dialog == Some(Dialog::AppUpdate) {
        state.ui.close_dialog();
    }
}

pub fn set_check(state: &mut AppState, check: bool) {
    state.prefs.updates.check = check;
}

/// An update that starts: its run and what it downloads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedRun {
    pub id: RunId,
    pub assets: WindowsAssets,
}

/// "Update and restart", or "Try again" after a failure: the run starts at its download, and the
/// card shows it. Nothing starts while a run goes on, stops or is ready, once the new app is in
/// place, or when `can_update` says no.
pub fn start(state: &mut AppState) -> Option<StartedRun> {
    open_card(state);
    let update = &mut state.app_update;
    let busy = update.run.as_ref().is_some_and(
        |run| !matches!(run.phase, RunPhase::Failed { step, .. } if step < AppStep::Restart),
    );
    if busy || update.stopping.is_some() {
        return None;
    }
    let assets = update_assets(update)?;
    let version = update.release.as_ref()?.version.clone();
    update.runs = update.runs.wrapping_add(1);
    let id = RunId(update.runs);
    update.run = Some(UpdateRun {
        id,
        version,
        downloaded: 0,
        total: assets.total_size(),
        phase: RunPhase::Running(AppStep::Download),
    });
    Some(StartedRun { id, assets })
}

/// The run `id` has no task: a demo screen shows it. Its "Cancel" ends it at once, since no task
/// deletes its downloads.
pub fn mark_without_task(state: &mut AppState, id: RunId) {
    state.app_update.without_task = Some(id);
}

/// The run `id`, while it is the one under way.
fn running(state: &mut AppState, id: RunId) -> Option<&mut UpdateRun> {
    state
        .app_update
        .run
        .as_mut()
        .filter(|run| run.id == id && matches!(run.phase, RunPhase::Running(_)))
}

/// Whether the run `id` still goes on: "Cancel" ends it.
pub fn is_current(state: &AppState, id: RunId) -> bool {
    state
        .app_update
        .run
        .as_ref()
        .is_some_and(|run| run.id == id && matches!(run.phase, RunPhase::Running(_)))
}

/// The download of the run `id` reached `bytes`.
pub fn downloaded(state: &mut AppState, id: RunId, bytes: u64) {
    if let Some(run) = running(state, id) {
        run.downloaded = bytes;
    }
}

pub fn step_started(state: &mut AppState, id: RunId, step: AppStep) {
    if let Some(run) = running(state, id) {
        run.phase = RunPhase::Running(step);
    }
}

/// The new app is at `app`, in place of this one.
pub fn ready(state: &mut AppState, id: RunId, app: PathBuf) {
    if let Some(run) = running(state, id) {
        run.downloaded = run.total;
        run.phase = RunPhase::Ready(app);
    }
}

pub fn failed(state: &mut AppState, id: RunId, step: AppStep, message: String) {
    if let Some(run) = running(state, id) {
        run.phase = RunPhase::Failed { step, message };
    }
}

/// The restart of a ready update failed: the new app is in place all the same.
pub fn restart_failed(state: &mut AppState, message: String) {
    if let Some(run) = &mut state.app_update.run
        && matches!(run.phase, RunPhase::Ready(_))
    {
        run.phase = RunPhase::Failed {
            step: AppStep::Restart,
            message,
        };
    }
}

/// "Cancel": the run ends before anything moved, and the card offers the release again once its
/// task deleted the downloads.
pub fn cancel(state: &mut AppState) {
    let update = &mut state.app_update;
    if let Some(run) = update.run.take_if(|run| run.offers_cancel())
        && update.without_task != Some(run.id)
    {
        update.stopping = Some(run.id);
    }
}

/// Whether a cancelled run still deletes its downloads.
pub fn is_stopping(state: &AppState) -> bool {
    state.app_update.stopping.is_some()
}

/// The task of the run `id` ended, and left none of its files behind.
pub fn ended(state: &mut AppState, id: RunId) {
    if state.app_update.stopping == Some(id) {
        state.app_update.stopping = None;
    }
}

/// How long ago a release came out, in whole days.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Released {
    Today,
    Yesterday,
    DaysAgo(u64),
}

pub fn released(published: Timestamp, now: SystemTime) -> Released {
    let age = published
        .to_system_time()
        .and_then(|moment| now.duration_since(moment).ok())
        .unwrap_or_default();
    match age.as_secs() / DAY.as_secs() {
        0 => Released::Today,
        1 => Released::Yesterday,
        days => Released::DaysAgo(days),
    }
}

/// A block of the release notes, as plain text: a heading, a list's items, or a paragraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteBlock {
    Heading(String),
    List(Vec<String>),
    Text(String),
}

/// The kind of line the notes' reader is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Heading,
    Item,
    Text,
}

/// Gathers the notes' lines into blocks, and stops at `NOTE_LINES` of them.
#[derive(Default)]
struct NoteReader {
    blocks: Vec<NoteBlock>,
    lines: usize,
    current: Option<(LineKind, String)>,
}

impl NoteReader {
    fn start(&mut self, kind: LineKind) {
        self.finish();
        self.current = Some((kind, String::new()));
    }

    /// Ends the current line. A blank one counts for nothing.
    fn finish(&mut self) {
        let Some((kind, text)) = self.current.take() else {
            return;
        };
        if text.trim().is_empty() || self.is_full() {
            return;
        }
        self.lines += 1;
        match (kind, self.blocks.last_mut()) {
            (LineKind::Item, Some(NoteBlock::List(items))) => items.push(text),
            (LineKind::Item, _) => self.blocks.push(NoteBlock::List(vec![text])),
            (LineKind::Heading, _) => self.blocks.push(NoteBlock::Heading(text)),
            (LineKind::Text, _) => self.blocks.push(NoteBlock::Text(text)),
        }
    }

    fn push(&mut self, text: &str) {
        if let Some((_, line)) = &mut self.current {
            line.push_str(text);
        }
    }

    fn is_full(&self) -> bool {
        self.lines >= NOTE_LINES
    }
}

/// The first lines of `markdown`, as plain text: its headings, lists and paragraphs, without
/// links, emphasis or code marks.
pub fn note_blocks(markdown: &str) -> Vec<NoteBlock> {
    let mut reader = NoteReader::default();
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Heading { .. }) => reader.start(LineKind::Heading),
            Event::Start(Tag::Item) => reader.start(LineKind::Item),
            Event::Start(Tag::Paragraph | Tag::CodeBlock(_)) if reader.current.is_none() => {
                reader.start(LineKind::Text);
            }
            Event::End(
                TagEnd::Heading(_) | TagEnd::Item | TagEnd::Paragraph | TagEnd::CodeBlock,
            ) => reader.finish(),
            Event::Text(text) | Event::Code(text) => reader.push(&text.replace('\n', " ")),
            Event::SoftBreak | Event::HardBreak => reader.push(" "),
            _ => {}
        }
        if reader.is_full() {
            break;
        }
    }
    reader.finish();
    reader.blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_label::DaemonLabel;

    /// A made-up answer, with the fields GitHub sends and more the app does not read.
    const RELEASE_JSON: &str = r#"{
  "url": "https://api.github.com/repos/Henauxg/yetack/releases/1",
  "html_url": "https://github.com/Henauxg/yetack/releases/tag/v9.1.0",
  "tag_name": "v9.1.0",
  "name": "Yetack 9.1.0",
  "draft": false,
  "prerelease": false,
  "published_at": "2026-11-12T10:00:00Z",
  "body": "- Made-up change one.\n- Made-up change two.",
  "assets": [
    {
      "name": "yetack-desktop-9.1.0-windows-x64.exe",
      "size": 14000000,
      "browser_download_url": "https://github.com/Henauxg/yetack/releases/download/v9.1.0/yetack-desktop-9.1.0-windows-x64.exe",
      "content_type": "application/octet-stream"
    },
    {
      "name": "yetack-9.1.0-x86_64-linux",
      "size": 9000000,
      "browser_download_url": "https://github.com/Henauxg/yetack/releases/download/v9.1.0/yetack-9.1.0-x86_64-linux"
    },
    {
      "name": "SHA256SUMS",
      "size": 400,
      "browser_download_url": "https://github.com/Henauxg/yetack/releases/download/v9.1.0/SHA256SUMS"
    }
  ]
}"#;

    fn release(version: &str) -> Release {
        let mut release = Release::from_github(RELEASE_JSON).unwrap();
        release.version = ReleaseVersion::new(version.to_owned());
        for asset in &mut release.assets {
            asset.name = asset.name.replace("9.1.0", version);
            asset.url = asset.url.replace("9.1.0", version);
        }
        release
    }

    fn version(text: &str) -> ReleaseVersion {
        ReleaseVersion::new(text.to_owned())
    }

    fn zip_state() -> AppState {
        let mut state = AppState::new(DaemonLabel::in_app());
        state.app_update.install = InstallKind::Zip;
        state
    }

    #[test]
    fn reads_githubs_answer() {
        let release = Release::from_github(RELEASE_JSON).unwrap();

        assert_eq!(release.version.as_str(), "9.1.0");
        assert_eq!(
            release.published,
            Some(Timestamp::from_unix_millis(1_794_477_600_000))
        );
        assert_eq!(
            release.notes,
            "- Made-up change one.\n- Made-up change two."
        );
        assert_eq!(
            release.page(),
            "https://github.com/Henauxg/yetack/releases/tag/v9.1.0"
        );
        assert_eq!(release.assets.len(), 3);
        assert_eq!(release.assets[1].name, "yetack-9.1.0-x86_64-linux");
        assert_eq!(release.assets[1].size, 9_000_000);
    }

    #[test]
    fn an_answer_without_a_body_or_a_date_still_reads() {
        let json = r#"{"tag_name": "9.2.0", "html_url": "https://example.org/elsewhere", "body": null, "published_at": "not a date"}"#;

        let release = Release::from_github(json).unwrap();

        assert_eq!(release.version.as_str(), "9.2.0");
        assert_eq!(release.notes, "");
        assert_eq!(release.published, None);
        assert!(release.assets.is_empty());
        assert_eq!(release.page(), RELEASES_URL);
        assert!(Release::from_github(r#"{"name": "no tag"}"#).is_err());
    }

    #[test]
    fn the_install_kind_comes_from_the_package_word() {
        let words: Vec<&str> = InstallKind::VARIANTS
            .iter()
            .map(|&kind| kind.into())
            .collect();

        assert_eq!(words, ["zip", "deb", "rpm", "tarball", "nix", "source"]);
        assert_eq!(InstallKind::from_package(Some("zip")), InstallKind::Zip);
        assert_eq!(InstallKind::from_package(Some("deb")), InstallKind::Deb);
        assert_eq!(InstallKind::from_package(Some("nix")), InstallKind::Nix);
        assert_eq!(InstallKind::from_package(Some("rpm")), InstallKind::Rpm);
        assert_eq!(
            InstallKind::from_package(Some("tarball")),
            InstallKind::Tarball
        );
        assert_eq!(InstallKind::from_package(Some("snap")), InstallKind::Source);
        assert_eq!(InstallKind::from_package(None), InstallKind::Source);
    }

    #[test]
    fn each_install_kind_says_how_it_updates() {
        let leads: Vec<String> = InstallKind::VARIANTS
            .iter()
            .map(|kind| kind.lead("0.3.0"))
            .collect();
        let about: Vec<&str> = InstallKind::VARIANTS
            .iter()
            .map(|kind| kind.about_line())
            .collect();

        assert_eq!(
            leads,
            [
                "You run 0.3.0.",
                "You run 0.3.0, from the .deb.",
                "You run 0.3.0, from the .rpm.",
                "You run 0.3.0, from the tarball.",
                "You run 0.3.0, from Nix.",
                "You run 0.3.0, built from source.",
            ]
        );
        assert_eq!(
            about,
            [
                "The update downloads it, checks it, and restarts the app.",
                "Install its .deb to update.",
                "Install its .rpm to update.",
                "Run its tarball's install.sh to update.",
                "Move to it in your Nix configuration to update.",
                "Build it from your checkout to update.",
            ]
        );
        assert!(InstallKind::Zip.updates_itself());
        assert!(!InstallKind::Deb.updates_itself());
        assert!(!InstallKind::Tarball.updates_itself());
        let version = ReleaseVersion::new("0.4.0".to_owned());
        assert_eq!(
            dnf_command(&version),
            "sudo dnf install ./yetack-0.4.0-1.x86_64.rpm"
        );
        assert_eq!(
            tarball_command(&version),
            "tar xzf yetack-desktop-0.4.0-x86_64-linux.tar.gz && ./yetack-desktop-0.4.0-x86_64-linux/install.sh"
        );
        assert_eq!(
            apt_command(&version),
            "sudo apt install ./yetack_0.4.0_amd64.deb"
        );
        assert_eq!(
            build_command(&version),
            "git fetch --tags && git checkout v0.4.0 && just build && just linux-daemon"
        );
    }

    #[test]
    fn only_a_newer_release_counts() {
        let mut state = zip_state();

        found(&mut state, release("0.0.1"));
        assert_eq!(state.app_update.release, None);

        found(&mut state, release(APP_VERSION));
        assert_eq!(state.app_update.release, None);

        found(&mut state, release("not a version"));
        assert_eq!(state.app_update.release, None);

        found(&mut state, release("999.0.0"));
        assert_eq!(
            offered(&state).map(|release| release.version.as_str()),
            Some("999.0.0")
        );
        assert_eq!(notice(&state), Some(SidebarNotice::Out(version("999.0.0"))));
    }

    #[test]
    fn a_skipped_release_hides_until_a_newer_one() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));
        open_card(&mut state);

        skip(&mut state);

        assert_eq!(state.prefs.updates.skipped, Some(version("999.0.0")));
        assert_eq!(state.ui.dialog, None);
        assert_eq!(offered(&state), None);
        assert_eq!(notice(&state), None);

        found(&mut state, release("999.1.0"));
        assert_eq!(notice(&state), Some(SidebarNotice::Out(version("999.1.0"))));
    }

    #[test]
    fn not_now_hides_the_note_and_keeps_the_offer() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));
        open_card(&mut state);

        not_now(&mut state);

        assert_eq!(state.ui.dialog, None);
        assert_eq!(notice(&state), None);
        assert!(offered(&state).is_some());
        assert_eq!(state.prefs.updates.skipped, None);
    }

    #[test]
    fn not_now_hides_only_its_version() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));
        not_now(&mut state);
        assert_eq!(notice(&state), None);

        found(&mut state, release("999.1.0"));

        assert_eq!(notice(&state), Some(SidebarNotice::Out(version("999.1.0"))));
    }

    #[test]
    fn a_tag_with_other_characters_does_not_read() {
        let answer = |tag: &str| {
            format!(
                r#"{{"tag_name": "{tag}", "html_url": "https://github.com/Henauxg/yetack/releases"}}"#
            )
        };

        assert!(Release::from_github(&answer("v1.2.0-rc.1")).is_ok());
        assert!(Release::from_github(&answer("v1.2.0-x;whoami")).is_err());
        assert!(Release::from_github(&answer("v1.2.0 && echo")).is_err());
        assert!(Release::from_github(&answer("v")).is_err());
    }

    #[test]
    fn the_check_turned_off_offers_nothing() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));

        set_check(&mut state, false);

        assert_eq!(offered(&state), None);
        assert_eq!(notice(&state), None);
    }

    #[test]
    fn checksums_read_text_and_binary_lines_and_skip_the_rest() {
        let sum = "AB".repeat(32);
        let text = format!(
            "{sum}  yetack-desktop-9.1.0-windows-x64.exe\n{}  yetack-9.1.0-x86_64-linux\r\n{} *yetack.service\nnot a line\n1234  short\n",
            "cd".repeat(32),
            "ef".repeat(32),
        );

        let sums = Checksums::from(text.as_str());

        assert_eq!(
            sums.of("yetack-desktop-9.1.0-windows-x64.exe"),
            Some("ab".repeat(32).as_str())
        );
        assert_eq!(
            sums.of("yetack-9.1.0-x86_64-linux"),
            Some("cd".repeat(32).as_str())
        );
        assert_eq!(sums.of("yetack.service"), Some("ef".repeat(32).as_str()));
        assert_eq!(sums.of("short"), None);
        assert_eq!(sums.of("missing"), None);
    }

    #[test]
    fn the_windows_files_must_download_from_the_repository() {
        let good = release("9.1.0");
        let assets = good.windows_assets().unwrap();
        assert_eq!(assets.app.name, "yetack-desktop-9.1.0-windows-x64.exe");
        assert_eq!(assets.daemon.name, "yetack-9.1.0-x86_64-linux");
        assert_eq!(assets.total_size(), 23_000_400);

        let mut elsewhere = good.clone();
        elsewhere.assets[0].url =
            "https://example.org/Henauxg/yetack/releases/download/v9.1.0/app.exe".to_owned();
        assert_eq!(elsewhere.windows_assets(), None);

        let mut lookalike = good.clone();
        lookalike.assets[0].url =
            "https://github.com/Henauxg/yetack-fork/releases/download/v9.1.0/app.exe".to_owned();
        assert_eq!(lookalike.windows_assets(), None);

        let mut missing = good.clone();
        missing.assets.remove(2);
        assert_eq!(missing.windows_assets(), None);

        let mut heavy = good;
        heavy.assets[0].size = MAX_DOWNLOAD_BYTES;
        assert_eq!(heavy.windows_assets(), None);
    }

    #[test]
    fn an_update_downloads_checks_and_gets_ready() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));

        let started = start(&mut state).unwrap();
        assert_eq!(state.ui.dialog, Some(Dialog::AppUpdate));
        assert_eq!(
            state
                .app_update
                .run
                .as_ref()
                .map(|run| run.version.as_str()),
            Some("999.0.0")
        );
        assert!(start(&mut state).is_none(), "one run at a time");
        let id = started.id;

        downloaded(&mut state, id, 11_500_200);
        let run = state.app_update.run.clone().unwrap();
        assert_eq!(run.percent(), 45);
        assert!(run.offers_cancel());
        assert_eq!(
            notice(&state),
            Some(SidebarNotice::Progress {
                text: "Downloading 999.0.0".to_owned(),
                percent: 45
            })
        );

        step_started(&mut state, id, AppStep::Check);
        assert_eq!(
            notice(&state),
            Some(SidebarNotice::Progress {
                text: "Checking 999.0.0".to_owned(),
                percent: 92
            })
        );

        step_started(&mut state, id, AppStep::Replace);
        assert!(!state.app_update.run.as_ref().unwrap().offers_cancel());
        cancel(&mut state);
        assert!(
            is_current(&state, id),
            "a cancel during the move changes nothing"
        );

        ready(&mut state, id, PathBuf::from("/made-up/yetack-desktop.exe"));
        let run = state.app_update.run.clone().unwrap();
        assert_eq!(
            run.phase,
            RunPhase::Ready(PathBuf::from("/made-up/yetack-desktop.exe"))
        );
        assert_eq!(run.current_step(), AppStep::Restart);
        assert_eq!(run.downloaded, run.total);
        assert_eq!(
            notice(&state),
            Some(SidebarNotice::Ready(version("999.0.0")))
        );

        found(&mut state, release("999.5.0"));
        assert_eq!(
            state
                .app_update
                .release
                .as_ref()
                .map(|release| release.version.as_str()),
            Some("999.0.0"),
            "a ready update keeps its release"
        );
    }

    #[test]
    fn a_failed_check_offers_another_try() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));
        let first = start(&mut state).unwrap().id;
        step_started(&mut state, first, AppStep::Check);

        failed(
            &mut state,
            first,
            AppStep::Check,
            "The download does not match.".to_owned(),
        );

        let run = state.app_update.run.clone().unwrap();
        assert_eq!(run.current_step(), AppStep::Check);
        assert_eq!(notice(&state), Some(SidebarNotice::Failed));
        let again = start(&mut state).unwrap().id;
        assert_ne!(again, first);
        step_started(&mut state, first, AppStep::Replace);
        assert_eq!(
            state.app_update.run.as_ref().unwrap().phase,
            RunPhase::Running(AppStep::Download),
            "the earlier run's news changes nothing"
        );
    }

    #[test]
    fn cancel_ends_the_download_and_its_late_news() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));
        let id = start(&mut state).unwrap().id;

        cancel(&mut state);

        assert_eq!(state.app_update.run, None);
        assert!(!is_current(&state, id));
        downloaded(&mut state, id, 5);
        ready(&mut state, id, PathBuf::from("/made-up/app.exe"));
        assert_eq!(state.app_update.run, None);
        assert_eq!(notice(&state), Some(SidebarNotice::Out(version("999.0.0"))));
        assert!(is_stopping(&state));
        assert!(
            start(&mut state).is_none(),
            "no run starts while the cancelled one deletes its files"
        );

        ended(&mut state, id);

        assert!(!is_stopping(&state));
        assert!(start(&mut state).is_some());
    }

    #[test]
    fn only_the_zip_with_its_files_starts() {
        let mut state = zip_state();
        state.app_update.install = InstallKind::Deb;
        found(&mut state, release("999.0.0"));
        assert!(start(&mut state).is_none());

        state.app_update.install = InstallKind::Zip;
        if let Some(release) = &mut state.app_update.release {
            release.assets.clear();
        }
        assert!(start(&mut state).is_none());
        assert_eq!(state.app_update.run, None);
    }

    #[test]
    fn a_failed_restart_and_its_dismissal() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));
        let id = start(&mut state).unwrap().id;
        ready(&mut state, id, PathBuf::from("/made-up/app.exe"));

        restart_failed(&mut state, "cannot start".to_owned());
        assert_eq!(
            state.app_update.run.as_ref().unwrap().phase,
            RunPhase::Failed {
                step: AppStep::Restart,
                message: "cannot start".to_owned()
            }
        );

        assert!(
            start(&mut state).is_none(),
            "the new app is in place already"
        );

        dismiss_failure(&mut state);
        assert!(state.app_update.run.is_some());
        assert_eq!(notice(&state), None);
    }

    #[test]
    fn a_dismissed_download_failure_offers_the_release_again() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));
        let id = start(&mut state).unwrap().id;
        failed(&mut state, id, AppStep::Download, "offline".to_owned());

        dismiss_failure(&mut state);

        assert_eq!(state.app_update.run, None);
        assert_eq!(notice(&state), None);
        assert!(offered(&state).is_some());
    }

    #[test]
    fn released_counts_whole_days() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100 * 86_400);
        let days_before = |days: u64| Timestamp::from_unix_millis((100 - days) * 86_400_000);

        assert_eq!(released(days_before(0), now), Released::Today);
        assert_eq!(released(days_before(1), now), Released::Yesterday);
        assert_eq!(released(days_before(3), now), Released::DaysAgo(3));
    }

    #[test]
    fn the_notes_read_as_plain_lines() {
        let markdown = "## Yetack 0.4.0\n\n- The **app** tells you when a release is out.\n* Find with `Ctrl+F` in [the transcript](https://example.org).\n\nTested with Claude Code\n2.1.330.\n\n<b>raw html</b>\n";

        assert_eq!(
            note_blocks(markdown),
            [
                NoteBlock::Heading("Yetack 0.4.0".to_owned()),
                NoteBlock::List(vec![
                    "The app tells you when a release is out.".to_owned(),
                    "Find with Ctrl+F in the transcript.".to_owned(),
                ]),
                NoteBlock::Text("Tested with Claude Code 2.1.330.".to_owned()),
                NoteBlock::Text("raw html".to_owned()),
            ]
        );
    }

    #[test]
    fn the_notes_group_their_bullets() {
        let markdown = "## Yetack 9.1.0\n\n- One.\n- Two.\n\nTested.\n\n- Three.\n";

        assert_eq!(
            note_blocks(markdown),
            [
                NoteBlock::Heading("Yetack 9.1.0".to_owned()),
                NoteBlock::List(vec!["One.".to_owned(), "Two.".to_owned()]),
                NoteBlock::Text("Tested.".to_owned()),
                NoteBlock::List(vec!["Three.".to_owned()]),
            ]
        );
    }

    #[test]
    fn the_notes_keep_their_first_lines() {
        let markdown: String = (1..=20).map(|line| format!("- Change {line}\n")).collect();

        let blocks = note_blocks(&markdown);

        let expected: Vec<String> = (1..=NOTE_LINES)
            .map(|line| format!("Change {line}"))
            .collect();
        assert_eq!(blocks, [NoteBlock::List(expected)]);
    }

    #[test]
    fn the_first_check_waits_a_day_from_the_last_one() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(100 * 86_400);
        let seconds_before = |seconds: u64| Some(100 * 86_400 - seconds);
        let hour = Duration::from_secs(60 * 60);

        assert_eq!(first_check_delay(now, None), FIRST_CHECK);
        assert_eq!(first_check_delay(now, seconds_before(0)), DAY);
        assert_eq!(first_check_delay(now, seconds_before(3600)), DAY - hour);
        assert_eq!(
            first_check_delay(now, seconds_before(86_400 - 3)),
            FIRST_CHECK,
            "never sooner than the first check's wait"
        );
        assert_eq!(
            first_check_delay(now, seconds_before(3 * 86_400)),
            FIRST_CHECK
        );
        assert_eq!(
            first_check_delay(now, Some(101 * 86_400)),
            FIRST_CHECK,
            "a last check after now counts for nothing"
        );
        assert_eq!(first_check_delay(now, Some(u64::MAX)), FIRST_CHECK);
    }

    #[test]
    fn a_check_saves_its_time() {
        let mut state = zip_state();

        checked(
            &mut state,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_794_477_600),
        );

        assert_eq!(state.prefs.updates.last_check, Some(1_794_477_600));
    }

    #[test]
    fn each_step_is_marked_where_it_stands() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));
        let id = start(&mut state).unwrap().id;
        let marks = |state: &AppState| -> Vec<StepMark> {
            let run = state.app_update.run.as_ref().unwrap();
            AppStep::VARIANTS
                .iter()
                .map(|&step| run.mark(step))
                .collect()
        };

        step_started(&mut state, id, AppStep::Check);
        assert_eq!(
            marks(&state),
            [
                StepMark::Done,
                StepMark::Now,
                StepMark::ToCome,
                StepMark::ToCome
            ]
        );

        failed(&mut state, id, AppStep::Check, "No match.".to_owned());
        assert_eq!(
            marks(&state),
            [
                StepMark::Done,
                StepMark::Failed,
                StepMark::ToCome,
                StepMark::ToCome
            ]
        );
        assert_eq!(
            state.app_update.run.as_ref().unwrap().failure(),
            Some("No match.")
        );
    }

    #[test]
    fn only_the_zip_with_the_windows_files_can_update() {
        let mut state = zip_state();
        assert!(!can_update(&state));

        found(&mut state, release("999.0.0"));
        assert!(can_update(&state));

        state.app_update.install = InstallKind::Nix;
        assert!(!can_update(&state));

        state.app_update.install = InstallKind::Zip;
        if let Some(release) = &mut state.app_update.release {
            release.assets.pop();
        }
        assert!(!can_update(&state));
    }

    #[test]
    fn a_run_without_a_task_ends_at_its_cancel() {
        let mut state = zip_state();
        found(&mut state, release("999.0.0"));
        let id = start(&mut state).unwrap().id;
        mark_without_task(&mut state, id);

        cancel(&mut state);

        assert_eq!(state.app_update.run, None);
        assert!(!is_stopping(&state));
        assert!(start(&mut state).is_some());
    }

    #[test]
    fn the_asset_names_carry_the_version() {
        let version = version("9.1.0");

        assert_eq!(
            app_asset_name(&version),
            "yetack-desktop-9.1.0-windows-x64.exe"
        );
        assert_eq!(daemon_asset_name(&version), "yetack-9.1.0-x86_64-linux");
        assert_eq!(RELEASES_URL, "https://github.com/Henauxg/yetack/releases");
        assert_eq!(
            DOWNLOAD_PREFIX,
            "https://github.com/Henauxg/yetack/releases/download/"
        );
    }
}
