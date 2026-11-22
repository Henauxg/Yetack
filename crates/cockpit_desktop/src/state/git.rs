//! What the daemon read of git for an agent: the shown file's last committed text, its blame, its
//! history and the commits shown, and the state of the agent's checkouts. Each is read when
//! something shows it, and read again after the agent's step that may change it, when the agent
//! comes back on screen, and on F5. The old value stays on screen until the new one arrives.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cockpit_protocol::{
    AgentId, AgentParams, CheckoutState, Commit, CommitHash, Completeness, FileChange, GitBlame,
    GitCommitFile, GitFile, GitFileParams, GitIgnore, GitShowParams, GitStatus, LogEntry, Request,
    ResponseBody, Upstream,
};
use strum::IntoStaticStr;
use url::Url;

use super::app_state::{AgentEntry, AppState, FileRead, Loadable};
use super::requests::{Outgoing, Purpose};
use super::ui_state::{Popover, ShownCommit, ViewMode, is_markdown};
use crate::machine::MachineId;
use crate::path_style::PathStyle;
use crate::render;

/// A value the daemon reads, which stays while it is read again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept<T> {
    pub value: Loadable<T>,
    /// Read it again at the next chance.
    stale: bool,
    /// A request is on its way.
    reading: bool,
}

impl<T> Default for Kept<T> {
    fn default() -> Self {
        Self {
            value: Loadable::NotRequested,
            stale: false,
            reading: false,
        }
    }
}

impl<T> Kept<T> {
    /// Whether it is to be asked for now: never asked, or stale, with no request on its way.
    fn wanted(&self) -> bool {
        !self.reading && (matches!(self.value, Loadable::NotRequested) || self.stale)
    }

    fn start(&mut self) {
        self.reading = true;
        self.stale = false;
        if matches!(self.value, Loadable::NotRequested) {
            self.value = Loadable::Loading;
        }
    }

    /// A read again that failed, as on a dropped connection, keeps what shows.
    fn arrived(&mut self, value: Loadable<T>) {
        self.reading = false;
        if matches!(value, Loadable::Failed(_)) && matches!(self.value, Loadable::Loaded(_)) {
            return;
        }
        self.value = value;
    }

    fn mark_stale(&mut self) {
        if !matches!(self.value, Loadable::NotRequested) {
            self.stale = true;
        }
    }
}

/// The git of one file the file view showed, by its absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileGit {
    pub committed: Kept<GitFile>,
    pub blame: Kept<GitBlame>,
    pub log: Kept<Vec<LogEntry>>,
    /// The commits shown, which never change.
    pub commits: BTreeMap<CommitHash, Loadable<GitCommitFile>>,
}

/// What the app holds of an agent's git.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentGit {
    pub files: BTreeMap<PathBuf, FileGit>,
    pub status: Kept<GitStatus>,
}

impl AgentGit {
    /// Everything read again: the agent may have committed, switched branches or pulled. A
    /// commit's change that did not load is asked for again.
    pub fn refresh(&mut self) {
        self.status.mark_stale();
        for file in self.files.values_mut() {
            file.committed.mark_stale();
            file.blame.mark_stale();
            file.log.mark_stale();
            file.commits
                .retain(|_, shown| !matches!(shown, Loadable::Failed(_)));
        }
    }

    /// The files of a closed file view are dropped.
    pub fn forget_files(&mut self) {
        self.files.clear();
    }

    /// The checkout that holds `path`, absolute: the one with the longest top folder.
    pub fn checkout_of(&self, path: &Path) -> Option<&CheckoutState> {
        let status = self.status.value.loaded()?;
        let text = path.to_string_lossy();
        status
            .checkouts
            .iter()
            .filter(|checkout| {
                let root = checkout.root.to_string_lossy();
                PathStyle::of(&checkout.root)
                    .strip_prefix(&text, &root)
                    .is_some()
            })
            .max_by_key(|checkout| checkout.root.as_os_str().len())
    }
}

/// A connection to `machine`'s daemon came: what its agents' git showed is read again, and what
/// failed while it was away is asked for again.
pub fn connected(state: &mut AppState, machine: MachineId) {
    for (agent, entry) in &mut state.agents {
        if MachineId::of(*agent) == machine {
            entry.git.refresh();
        }
    }
}

/// The shown file's absolute path, in the agent's folder's style.
pub fn absolute(entry: &AgentEntry, path: &Path) -> PathBuf {
    let folder = &entry.summary.folder;
    let style = PathStyle::of(folder);
    PathBuf::from(style.resolve(&folder.to_string_lossy(), &path.to_string_lossy()))
}

/// A step of an agent, by what of its git it may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// An edit, which changes only files.
    Edit,
    /// A Bash call, which may also commit or switch branches.
    Command,
}

/// The selected agent's `step` finished.
pub fn step_finished(entry: &mut AgentEntry, step: Step) {
    match step {
        Step::Command => entry.git.refresh(),
        Step::Edit => entry.git.status.mark_stale(),
    }
}

/// The requests the agent's git needs now: what the file view and the header show for the
/// selected `agent`.
pub fn requests(state: &mut AppState, agent: AgentId) -> Vec<Outgoing> {
    let mut requests = Vec::new();
    let view = state.ui.file_view(agent);
    let history_open = matches!(
        state.ui.popover,
        Some(Popover::FileHistory { agent: shown, .. }) if shown == agent
    );
    let Some(entry) = state.agents.get_mut(&agent) else {
        return requests;
    };
    if entry.git.status.wanted() {
        entry.git.status.start();
        requests.push(outgoing(
            Request::AgentGitStatus(AgentParams { agent }),
            Purpose::Git(GitPurpose::Status { agent }),
        ));
    }
    let Some(view) = view else {
        return requests;
    };
    let is_text = matches!(
        entry.files.get(&view.path),
        Some(Loadable::Loaded(FileRead::Text { .. }))
    );
    if !is_text {
        return requests;
    }
    let path = absolute(entry, &view.path);
    // Only the shown file's git is kept, as only its text is: a file's can be large.
    entry.git.files.retain(|kept, _| *kept == path);
    let file = entry.git.files.entry(path.clone()).or_default();
    let params = GitFileParams {
        agent,
        path: path.clone(),
    };
    if file.committed.wanted() {
        file.committed.start();
        requests.push(outgoing(
            Request::AgentGitFile(params.clone()),
            Purpose::Git(GitPurpose::File {
                agent,
                path: path.clone(),
            }),
        ));
    }
    // The blame maps onto the committed text: it waits while that text is read again.
    let committed = !file.committed.reading
        && matches!(
            file.committed.value,
            Loadable::Loaded(GitFile::Committed { .. })
        );
    // A view left in Preview on another kind of file shows the whole file.
    let whole_file = view.mode == ViewMode::WholeFile
        || (view.mode == ViewMode::Preview && !is_markdown(&view.path));
    if whole_file && committed && file.blame.wanted() {
        file.blame.start();
        requests.push(outgoing(
            Request::AgentGitBlame(params.clone()),
            Purpose::Git(GitPurpose::Blame {
                agent,
                path: path.clone(),
            }),
        ));
    }
    if history_open && file.log.wanted() {
        file.log.start();
        requests.push(outgoing(
            Request::AgentGitLog(params),
            Purpose::Git(GitPurpose::Log {
                agent,
                path: path.clone(),
            }),
        ));
    }
    if let Some(commit) = view
        .commit
        .as_ref()
        .filter(|shown| shown.path == view.path)
        .map(|shown| shown.hash.clone())
        && !file.commits.contains_key(&commit)
    {
        file.commits.insert(commit.clone(), Loadable::Loading);
        requests.push(outgoing(
            Request::AgentGitShow(GitShowParams {
                agent,
                path: path.clone(),
                commit: commit.clone(),
            }),
            Purpose::Git(GitPurpose::Show {
                agent,
                path,
                commit,
            }),
        ));
    }
    requests
}

/// The state of another agent's checkouts, for the card a sidebar row shows on hover, and for
/// the row's menu.
pub fn card_requests(state: &mut AppState) -> Vec<Outgoing> {
    let menu_agent = match state.ui.popover {
        Some(Popover::RowMenu { agent, .. }) => Some(agent),
        _ => None,
    };
    let Some(agent) = state.ui.branch_card.map(|card| card.agent).or(menu_agent) else {
        return Vec::new();
    };
    if !state.reaches_agent(agent) {
        return Vec::new();
    }
    let Some(entry) = state.agents.get_mut(&agent) else {
        return Vec::new();
    };
    if !entry.git.status.wanted() {
        return Vec::new();
    }
    entry.git.status.start();
    vec![outgoing(
        Request::AgentGitStatus(AgentParams { agent }),
        Purpose::Git(GitPurpose::Status { agent }),
    )]
}

/// Opens or closes the shown file's history, with its top right corner at `x`, `y`: under its
/// button's right edge.
pub fn toggle_history(state: &mut AppState, agent: AgentId, x: i32, y: i32) {
    let open = matches!(
        state.ui.popover,
        Some(Popover::FileHistory { agent: shown, .. }) if shown == agent
    );
    state.ui.popover = (!open).then_some(Popover::FileHistory { agent, x, y });
}

/// Shows `hash`'s change to the shown file, in place of its mode. `None` goes back to the mode.
pub fn show_commit(state: &mut AppState, agent: AgentId, hash: Option<CommitHash>) {
    if matches!(state.ui.popover, Some(Popover::FileHistory { .. })) {
        state.ui.popover = None;
    }
    if let Some(view) = state.ui.file_views.get_mut(&agent) {
        view.commit = hash.map(|hash| ShownCommit {
            path: view.path.clone(),
            hash,
        });
    }
}

/// Shows the shown file's uncommitted changes: its diff.
pub fn show_uncommitted(state: &mut AppState, agent: AgentId) {
    show_commit(state, agent, None);
    if let Some(view) = state.ui.file_views.get_mut(&agent) {
        view.mode = ViewMode::Diff;
    }
}

/// The menu item and the link that open a page on GitHub.
pub const OPEN_ON_GITHUB_LABEL: &str = "Open on GitHub";
/// The menu item that opens a file's history.
pub const SHOW_HISTORY_LABEL: &str = "Show its history";

/// What the app may link to on GitHub for one checkout: its repository's pages, but no commit
/// that GitHub may lack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubLinks {
    /// The repository's page: `https://github.com/acme/app`.
    page: String,
    /// The remote's name, which starts its branches' names as an upstream: `origin`.
    remote: String,
    unpushed: Vec<CommitHash>,
    unpushed_completeness: Completeness,
}

impl GitHubLinks {
    /// `None` unless `checkout`'s remote is on GitHub.
    pub fn of(checkout: &CheckoutState) -> Option<Self> {
        let remote = checkout.remote.as_ref()?;
        let path = remote.repository.as_str().strip_prefix(GITHUB_HOST)?;
        Some(Self {
            page: format!("https://{GITHUB_HOST}{path}"),
            remote: remote.name.clone(),
            unpushed: remote.unpushed.clone(),
            unpushed_completeness: remote.unpushed_completeness,
        })
    }

    /// The links for the commits `blame` names. `None` when `checkout`'s state is of another HEAD
    /// than the blame, as while both are read again: the state lists the unpushed commits of its
    /// own HEAD only.
    pub fn of_blame(checkout: &CheckoutState, blame: &GitBlame) -> Option<Self> {
        if checkout.branch.head.as_ref()?.hash != blame.head {
            return None;
        }
        Self::of(checkout)
    }

    /// Whether GitHub shows the commit `hash`: a branch of the remote held it at the last fetch.
    fn shows(&self, hash: &CommitHash) -> bool {
        self.unpushed_completeness == Completeness::Complete && !self.unpushed.contains(hash)
    }

    /// The page of the commit `hash`, when GitHub shows it.
    pub fn commit_page(&self, hash: &CommitHash) -> Option<String> {
        self.shows(hash).then(|| self.held_commit_page(hash))
    }

    /// The page of the commit `hash`, which the daemon said the remote holds.
    pub fn held_commit_page(&self, hash: &CommitHash) -> String {
        format!("{}/commit/{hash}", self.page)
    }

    /// The page of `upstream`'s branch, when it is one of the remote's.
    fn branch_page(&self, upstream: &Upstream) -> Option<String> {
        let branch = upstream
            .name
            .strip_prefix(self.remote.as_str())?
            .strip_prefix('/')?;
        let tree: &str = PageKind::Tree.into();
        page_at(&self.page, std::iter::once(tree).chain(branch.split('/')))
    }
}

/// The kind of a page on GitHub. Its address names the kind after the repository, as in
/// `github.com/acme/app/blob/...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "lowercase")]
pub enum PageKind {
    /// A file's page.
    Blob,
    /// A folder's or a branch's page.
    Tree,
}

/// The page of `path`, absolute, on GitHub, at the commit its checkout is on. `None` outside a
/// checkout on GitHub, for a file the last commit lacks, and while GitHub may lack that commit.
pub fn github_file_page(entry: &AgentEntry, path: &Path, kind: PageKind) -> Option<String> {
    let checkout = entry.git.checkout_of(path)?;
    let new = match kind {
        PageKind::Blob => is_new(checkout, path),
        PageKind::Tree => may_be_new_folder(checkout, path),
    };
    if new || listed_as_ignored(entry, path) {
        return None;
    }
    let links = GitHubLinks::of(checkout)?;
    let head = checkout.branch.head.as_ref()?;
    if !links.shows(&head.hash) {
        return None;
    }
    let relative = path_in_checkout(checkout, path)?;
    let kind: &str = kind.into();
    let names = [kind, head.hash.as_str()]
        .into_iter()
        .chain(relative.split('/'));
    page_at(&links.page, names)
}

/// `page` with `names` after it, each escaped, so that a `#`, a `?` or a `%` in a file's or a
/// branch's name stays in that name.
fn page_at<'name>(page: &str, names: impl IntoIterator<Item = &'name str>) -> Option<String> {
    let mut url = Url::parse(page).ok()?;
    url.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .extend(names.into_iter().filter(|name| !name.is_empty()));
    Some(url.into())
}

/// Whether the agent's checkouts have a history for `path`: it is in one of them, and not new.
pub fn has_history(entry: &AgentEntry, path: &Path) -> bool {
    let committed = entry
        .git
        .checkout_of(path)
        .is_some_and(|checkout| checkout.branch.head.is_some() && !is_new(checkout, path));
    committed && !listed_as_ignored(entry, path)
}

/// Whether the folder `path`, absolute, may be new since the last commit: it holds a new file,
/// and no file the last commit has. Git does not list the files that did not change.
fn may_be_new_folder(checkout: &CheckoutState, path: &Path) -> bool {
    let style = PathStyle::of(&checkout.root);
    let folder = path.to_string_lossy();
    let mut new = false;
    for file in &checkout.files {
        let inside = style
            .strip_prefix(&file.path.to_string_lossy(), &folder)
            .is_some_and(|rest| !rest.is_empty());
        match file.change {
            _ if !inside => {}
            FileChange::Added | FileChange::Untracked => new = true,
            FileChange::Modified | FileChange::Deleted => return false,
            FileChange::Renamed | FileChange::Conflicted => {}
        }
    }
    new
}

/// Whether a listing of the files pane shows `path`, absolute, or a folder above it, as one its
/// checkout ignores: git has no history of it.
fn listed_as_ignored(entry: &AgentEntry, path: &Path) -> bool {
    let style = PathStyle::of(path);
    let text = path.to_string_lossy();
    entry.tree.listings.iter().any(|(folder, listing)| {
        let Loadable::Loaded(listing) = &listing.entries else {
            return false;
        };
        let name = style
            .strip_prefix(&text, &folder.to_string_lossy())
            .and_then(|relative| style.first_name(relative));
        name.is_some_and(|name| {
            listing.entries.iter().any(|listed| {
                listed.ignore == GitIgnore::Ignored && style.same_name(&listed.name, name)
            })
        })
    })
}

/// Whether `path`, absolute, is new in `checkout` since its last commit. Git's name for it may
/// differ in case and separators on Windows.
fn is_new(checkout: &CheckoutState, path: &Path) -> bool {
    let style = PathStyle::of(&checkout.root);
    let text = path.to_string_lossy();
    checkout.files.iter().any(|file| {
        matches!(file.change, FileChange::Added | FileChange::Untracked)
            && style.same_path(&file.path.to_string_lossy(), &text)
    })
}

/// The branch of the agent's folder's checkout, as the header, the row menu and the branch card
/// say it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchSummary<'a> {
    /// The branch's name, or "detached at 9f8e7d6".
    pub branch: String,
    /// The branch, then what it has to push and to pull, then its uncommitted files: "fix/columns
    /// · 2 to push · 7 uncommitted".
    pub words: String,
    /// The page on GitHub of the branch's upstream.
    pub github: Option<String>,
    pub checkout: &'a CheckoutState,
}

/// The branch of `entry`'s folder's checkout. `None` before the daemon said, or outside git.
pub fn branch_summary(entry: &AgentEntry) -> Option<BranchSummary<'_>> {
    let checkout = entry.git.checkout_of(&entry.summary.folder)?;
    let state = &checkout.branch;
    let branch = match (&state.name, &state.head) {
        (Some(name), _) => name.clone(),
        (None, Some(head)) => format!("detached at {}", head.hash.short()),
        (None, None) => "no commit yet".to_owned(),
    };
    let mut parts = vec![branch.clone()];
    if let Some(upstream) = &state.upstream {
        if upstream.ahead > 0 {
            parts.push(format!("{} to push", upstream.ahead));
        }
        if upstream.behind > 0 {
            parts.push(format!("{} to pull", upstream.behind));
        }
    }
    if !checkout.files.is_empty() {
        let more = match checkout.completeness {
            Completeness::Complete => "",
            Completeness::Truncated => "+",
        };
        parts.push(format!("{}{more} uncommitted", checkout.files.len()));
    }
    let github = GitHubLinks::of(checkout)
        .zip(state.upstream.as_ref())
        .and_then(|(links, upstream)| links.branch_page(upstream));
    Some(BranchSummary {
        branch,
        words: parts.join(" · "),
        github,
        checkout,
    })
}

/// The host whose repositories the app links to.
const GITHUB_HOST: &str = "github.com/";

/// `path`, absolute, relative to `checkout`'s top folder with `/` between its names, as a web
/// page names it.
pub fn path_in_checkout(checkout: &CheckoutState, path: &Path) -> Option<String> {
    let style = PathStyle::of(&checkout.root);
    let text = path.to_string_lossy();
    let root = checkout.root.to_string_lossy();
    let relative = style.strip_prefix(&text, &root)?;
    let names: Vec<&str> = style.names(relative).collect();
    Some(names.join("/"))
}

/// For each line of `current`, the file on disk, the index in `blame.commits` of the commit that
/// last changed it. `None` for a line not committed yet. `committed` is the text `blame` is of.
pub fn line_commits(blame: &GitBlame, committed: &str, current: &str) -> Vec<Option<usize>> {
    let mut by_line: Vec<Option<usize>> = vec![None; committed.lines().count()];
    for run in &blame.runs {
        let commit = usize::try_from(run.commit).ok();
        let start = usize::try_from(run.start.saturating_sub(1)).unwrap_or(usize::MAX);
        let count = usize::try_from(run.count).unwrap_or(0);
        for line in by_line.iter_mut().skip(start).take(count) {
            *line = commit;
        }
    }
    render::line_origins(committed, current)
        .into_iter()
        .map(|origin| origin.and_then(|line| by_line.get(line).copied().flatten()))
        .collect()
}

/// What the shown file's diff compares the file with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffBase {
    /// The daemon has not said yet.
    Waiting,
    /// The last commit's text: empty for a file new since. `head` is `None` in a checkout without
    /// a commit.
    Git {
        head: Option<Commit>,
        text: String,
        new: bool,
    },
    /// The text before the agent's edits. `outside` when the file is in no git checkout.
    Edits { outside: bool },
}

impl DiffBase {
    pub fn of(file: Option<&FileGit>) -> Self {
        let Some(file) = file else {
            return Self::Waiting;
        };
        match &file.committed.value {
            Loadable::NotRequested | Loadable::Loading => Self::Waiting,
            Loadable::Loaded(GitFile::Committed { head, text }) => Self::Git {
                head: Some(head.clone()),
                text: text.clone(),
                new: false,
            },
            Loadable::Loaded(GitFile::New { head }) => Self::Git {
                head: head.clone(),
                text: String::new(),
                new: true,
            },
            Loadable::Loaded(GitFile::Outside) => Self::Edits { outside: true },
            Loadable::Loaded(GitFile::NotText { .. }) | Loadable::Failed(_) => {
                Self::Edits { outside: false }
            }
        }
    }
}

fn outgoing(request: Request, purpose: Purpose) -> Outgoing {
    Outgoing {
        request,
        purpose,
        to: MachineId::THIS,
    }
}

/// An answer to one of the git requests. A failure, as from a daemon older than them, leaves
/// what it would show out.
pub fn arrived(state: &mut AppState, purpose: GitPurpose, result: Result<ResponseBody, String>) {
    let agent = purpose.agent();
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    match purpose {
        GitPurpose::Status { .. } => {
            let value = match result {
                Ok(ResponseBody::GitStatus(status)) => Loadable::Loaded(status),
                other => failed(other),
            };
            entry.git.status.arrived(value);
        }
        GitPurpose::File { path, .. } => {
            let Some(file) = entry.git.files.get_mut(&path) else {
                return;
            };
            let value = match result {
                Ok(ResponseBody::GitFile(committed)) => Loadable::Loaded(committed),
                other => failed(other),
            };
            file.committed.arrived(value);
        }
        GitPurpose::Blame { path, .. } => {
            let Some(file) = entry.git.files.get_mut(&path) else {
                return;
            };
            let value = match result {
                Ok(ResponseBody::GitBlame(blame)) => Loadable::Loaded(blame),
                other => failed(other),
            };
            file.blame.arrived(value);
        }
        GitPurpose::Log { path, .. } => {
            let Some(file) = entry.git.files.get_mut(&path) else {
                return;
            };
            let value = match result {
                Ok(ResponseBody::GitLog { entries }) => Loadable::Loaded(entries),
                other => failed(other),
            };
            file.log.arrived(value);
        }
        GitPurpose::Show { path, commit, .. } => {
            let Some(file) = entry.git.files.get_mut(&path) else {
                return;
            };
            let value = match result {
                Ok(ResponseBody::GitCommitFile(shown)) => Loadable::Loaded(shown),
                other => failed(other),
            };
            file.commits.insert(commit, value);
        }
    }
}

fn failed<T>(result: Result<ResponseBody, String>) -> Loadable<T> {
    match result {
        Ok(other) => Loadable::Failed(format!("unexpected answer: {other:?}")),
        Err(message) => Loadable::Failed(message),
    }
}

/// What a git request is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitPurpose {
    Status {
        agent: AgentId,
    },
    File {
        agent: AgentId,
        path: PathBuf,
    },
    Blame {
        agent: AgentId,
        path: PathBuf,
    },
    Log {
        agent: AgentId,
        path: PathBuf,
    },
    Show {
        agent: AgentId,
        path: PathBuf,
        commit: CommitHash,
    },
}

impl GitPurpose {
    fn agent(&self) -> AgentId {
        match self {
            Self::Status { agent }
            | Self::File { agent, .. }
            | Self::Blame { agent, .. }
            | Self::Log { agent, .. }
            | Self::Show { agent, .. } => *agent,
        }
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{
        AgentStatus, BlameRun, BranchState, CheckoutKind, Remote, Repository, Timestamp,
    };

    use super::*;

    fn commit(hash: char) -> Commit {
        Commit {
            hash: CommitHash::new(&hash.to_string().repeat(40)).unwrap(),
            title: format!("Commit {hash}"),
            body: String::new(),
            author: "Dev".to_owned(),
            time: Timestamp::from_unix_millis(0),
        }
    }

    /// `acme/app` on GitHub as `origin`, which lacks the commits `unpushed` names.
    fn github_remote(unpushed: &[char], completeness: Completeness) -> Option<Remote> {
        Some(Remote {
            name: "origin".to_owned(),
            repository: Repository::from_remote("github.com/acme/app"),
            unpushed: unpushed.iter().map(|hash| commit(*hash).hash).collect(),
            unpushed_completeness: completeness,
        })
    }

    #[test]
    fn each_line_on_disk_takes_the_commit_of_its_committed_line() {
        let blame = GitBlame {
            head: commit('b').hash,
            commits: vec![commit('a'), commit('b')],
            runs: vec![
                BlameRun {
                    start: 1,
                    count: 2,
                    commit: 0,
                },
                BlameRun {
                    start: 3,
                    count: 1,
                    commit: 1,
                },
            ],
        };
        let committed = "one\ntwo\nthree\n";
        let current = "one\nnew\ntwo\nthree\n";

        assert_eq!(
            line_commits(&blame, committed, current),
            [Some(0), None, Some(0), Some(1)]
        );
    }

    #[test]
    fn the_diff_base_follows_what_the_daemon_said() {
        let mut file = FileGit::default();
        let waiting = DiffBase::of(Some(&file));
        file.committed.arrived(Loadable::Loaded(GitFile::Outside));
        let outside = DiffBase::of(Some(&file));
        let mut asked_an_older_daemon = FileGit::default();
        asked_an_older_daemon.committed.arrived(Loadable::Failed(
            "unknown method `agent.git_file`".to_owned(),
        ));
        let older_daemon = DiffBase::of(Some(&asked_an_older_daemon));

        assert_eq!(waiting, DiffBase::Waiting);
        assert_eq!(outside, DiffBase::Edits { outside: true });
        assert_eq!(older_daemon, DiffBase::Edits { outside: false });
    }

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn lib_path() -> PathBuf {
        PathBuf::from("/work/project-1/src/lib.rs")
    }

    /// The selected agent of `/work/project-1`, its file view on `src/lib.rs`, read.
    fn viewing_state() -> AppState {
        use crate::state::app_state::ConnectionState;
        use crate::state::app_state::tests::{state_with, summary};
        use crate::state::ui_state::FileView;

        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.this.connection = ConnectionState::Connected;
        state.ui.selected = Some(agent());
        let path = PathBuf::from("src/lib.rs");
        state
            .ui
            .file_views
            .insert(agent(), FileView::new(path.clone(), None, ViewMode::Diff));
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.summary.folder = PathBuf::from("/work/project-1");
            entry
                .files
                .insert(path, Loadable::Loaded(FileRead::unedited("one\ntwo\n")));
        }
        state
    }

    fn git_requests(state: &mut AppState) -> Vec<GitPurpose> {
        requests(state, agent())
            .into_iter()
            .filter_map(|outgoing| match outgoing.purpose {
                Purpose::Git(purpose) => Some(purpose),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn what_shows_is_asked_for_once_then_again_after_a_bash_call() {
        let mut state = viewing_state();

        let first = git_requests(&mut state);
        let while_reading = git_requests(&mut state);
        arrived(
            &mut state,
            GitPurpose::File {
                agent: agent(),
                path: lib_path(),
            },
            Ok(ResponseBody::GitFile(GitFile::Committed {
                head: commit('a'),
                text: "one\n".to_owned(),
            })),
        );
        if let Some(view) = state.ui.file_views.get_mut(&agent()) {
            view.mode = ViewMode::WholeFile;
        }
        let blame = git_requests(&mut state);
        state.ui.popover = Some(Popover::FileHistory {
            agent: agent(),
            x: 0,
            y: 0,
        });
        let log = git_requests(&mut state);
        let shown = commit('a').hash;
        show_commit(&mut state, agent(), Some(shown.clone()));
        let commit_file = git_requests(&mut state);
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry
                .git
                .status
                .arrived(Loadable::Failed("busy".to_owned()));
            if let Some(file) = entry.git.files.get_mut(&lib_path()) {
                file.blame.arrived(Loadable::Failed("busy".to_owned()));
            }
            step_finished(entry, Step::Command);
        }
        let after_bash = git_requests(&mut state);

        let path = lib_path;
        assert_eq!(
            first,
            [
                GitPurpose::Status { agent: agent() },
                GitPurpose::File {
                    agent: agent(),
                    path: path()
                }
            ]
        );
        assert!(while_reading.is_empty(), "nothing twice");
        assert_eq!(
            blame,
            [GitPurpose::Blame {
                agent: agent(),
                path: path()
            }]
        );
        assert_eq!(
            log,
            [GitPurpose::Log {
                agent: agent(),
                path: path()
            }]
        );
        assert_eq!(
            commit_file,
            [GitPurpose::Show {
                agent: agent(),
                path: path(),
                commit: shown
            }]
        );
        assert_eq!(
            after_bash,
            [
                GitPurpose::Status { agent: agent() },
                GitPurpose::File {
                    agent: agent(),
                    path: path()
                }
            ],
            "the checkouts and the shown file's last commit are read again, the blame after"
        );
    }

    #[test]
    fn a_file_the_preview_cannot_show_shows_whole_with_its_blame() {
        let mut state = viewing_state();
        if let Some(view) = state.ui.file_views.get_mut(&agent()) {
            view.mode = ViewMode::Preview;
        }
        git_requests(&mut state);
        arrived(
            &mut state,
            GitPurpose::File {
                agent: agent(),
                path: lib_path(),
            },
            Ok(ResponseBody::GitFile(GitFile::Committed {
                head: commit('a'),
                text: "one\n".to_owned(),
            })),
        );

        let asked = git_requests(&mut state);

        assert_eq!(
            asked,
            [GitPurpose::Blame {
                agent: agent(),
                path: lib_path()
            }]
        );
    }

    #[test]
    fn a_failed_command_reads_git_again_since_it_may_have_committed() {
        use cockpit_protocol::{ItemBody, ItemId, ToolCall, ToolCallStatus, TranscriptItem};

        let mut state = viewing_state();
        git_requests(&mut state);
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry
                .git
                .status
                .arrived(Loadable::Failed("busy".to_owned()));
        }
        let failed = TranscriptItem {
            id: ItemId::new(4),
            parent: None,
            body: ItemBody::ToolCall(ToolCall {
                tool: "Bash".to_owned(),
                summary: "git commit -m Start && cargo test".to_owned(),
                input: serde_json::Value::Null,
                status: ToolCallStatus::Failed,
                output: None,
                exit_code: Some(101),
                edit: None,
            }),
        };

        crate::state::file_tree::item_finished(&mut state, agent(), &failed);

        assert!(git_requests(&mut state).contains(&GitPurpose::Status { agent: agent() }));
    }

    #[test]
    fn a_new_file_has_no_history_nor_page_whatever_its_case_and_separators() {
        use cockpit_protocol::FileStatus;

        let mut entry = AgentEntry::new(crate::state::summary(1, AgentStatus::Idle));
        entry.summary.folder = PathBuf::from(r"C:\work\app");
        entry.git.status.value = Loadable::Loaded(GitStatus {
            checkouts: vec![CheckoutState {
                root: PathBuf::from(r"C:\work\app"),
                remote: github_remote(&[], Completeness::Complete),
                kind: CheckoutKind::MainWorktree,
                branch: BranchState {
                    name: Some("main".to_owned()),
                    head: Some(commit('a')),
                    upstream: None,
                },
                files: vec![FileStatus {
                    path: PathBuf::from("c:/work/app/src/New.rs"),
                    change: FileChange::Untracked,
                    counts: None,
                }],
                completeness: Completeness::Complete,
            }],
        });
        let path = Path::new(r"C:\work\app\src\new.rs");

        assert!(!has_history(&entry, path));
        assert_eq!(github_file_page(&entry, path, PageKind::Blob), None);
        assert!(has_history(&entry, Path::new(r"C:\work\app\src\lib.rs")));
    }

    #[test]
    fn a_commit_that_did_not_load_loads_again_on_refresh() {
        let mut state = viewing_state();
        let shown = commit('a').hash;
        show_commit(&mut state, agent(), Some(shown.clone()));
        git_requests(&mut state);
        arrived(
            &mut state,
            GitPurpose::Show {
                agent: agent(),
                path: lib_path(),
                commit: shown.clone(),
            },
            Err("disconnected".to_owned()),
        );

        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.git.refresh();
        }

        assert!(git_requests(&mut state).contains(&GitPurpose::Show {
            agent: agent(),
            path: lib_path(),
            commit: shown,
        }));
    }

    #[test]
    fn an_ignored_entry_or_a_new_folder_has_no_history_nor_page() {
        use cockpit_protocol::{DirEntry, DirListing, EntryKind, EntryLink, FileStatus};

        use crate::state::file_tree::Listing;

        let mut entry = AgentEntry::new(crate::state::summary(1, AgentStatus::Idle));
        entry.summary.folder = PathBuf::from("/work/app");
        let status = |path: &str, change| FileStatus {
            path: PathBuf::from(path),
            change,
            counts: None,
        };
        entry.git.status.value = Loadable::Loaded(GitStatus {
            checkouts: vec![CheckoutState {
                root: PathBuf::from("/work/app"),
                remote: github_remote(&[], Completeness::Complete),
                kind: CheckoutKind::MainWorktree,
                branch: BranchState {
                    name: Some("main".to_owned()),
                    head: Some(commit('a')),
                    upstream: None,
                },
                files: vec![
                    status("/work/app/src/lib.rs", FileChange::Modified),
                    status("/work/app/src/parser/mod.rs", FileChange::Untracked),
                    status("/work/app/src/tokens.rs", FileChange::Untracked),
                ],
                completeness: Completeness::Complete,
            }],
        });
        let folder = |name: &str, ignore| DirEntry {
            name: name.to_owned(),
            kind: EntryKind::Folder,
            size: None,
            link: EntryLink::Direct,
            ignore,
        };
        entry.tree.listings.insert(
            PathBuf::from("/work/app"),
            Listing {
                entries: Loadable::Loaded(DirListing {
                    path: PathBuf::from("/work/app"),
                    entries: vec![
                        folder("src", GitIgnore::Kept),
                        folder("target", GitIgnore::Ignored),
                    ],
                    completeness: Completeness::Complete,
                }),
                stale: false,
                reading: false,
                gone: None,
            },
        );
        let page = |path: &str, kind| github_file_page(&entry, Path::new(path), kind);
        let log = Path::new("/work/app/target/debug/build.log");

        assert!(!has_history(&entry, log), "a file under an ignored folder");
        assert_eq!(
            page("/work/app/target/debug/build.log", PageKind::Blob),
            None
        );
        assert_eq!(page("/work/app/target", PageKind::Tree), None);
        assert_eq!(
            page("/work/app/src/parser", PageKind::Tree),
            None,
            "it holds new files only"
        );
        assert!(
            page("/work/app/src", PageKind::Tree).is_some(),
            "it holds a changed file"
        );
        assert!(
            page("/work/app/docs", PageKind::Tree).is_some(),
            "git lists no file in it"
        );
        assert!(has_history(&entry, Path::new("/work/app/src/lib.rs")));
    }

    #[test]
    fn no_link_leads_to_a_commit_or_a_branch_github_may_lack() {
        let checkout = |remote: Option<Remote>, upstream: &str| CheckoutState {
            root: PathBuf::from("/work/app"),
            remote,
            kind: CheckoutKind::MainWorktree,
            branch: BranchState {
                name: Some("feature".to_owned()),
                head: Some(commit('b')),
                upstream: Some(Upstream {
                    name: upstream.to_owned(),
                    ahead: 1,
                    behind: 0,
                }),
            },
            files: Vec::new(),
            completeness: Completeness::Complete,
        };
        let entry = |checkout: CheckoutState| {
            let mut entry = AgentEntry::new(crate::state::summary(1, AgentStatus::Idle));
            entry.summary.folder = PathBuf::from("/work/app");
            entry.git.status.value = Loadable::Loaded(GitStatus {
                checkouts: vec![checkout],
            });
            entry
        };
        let pushed = checkout(
            github_remote(&['b'], Completeness::Complete),
            "origin/feature",
        );
        let cut_short = checkout(
            github_remote(&['b'], Completeness::Truncated),
            "origin/feature",
        );
        // Its name starts as the linked remote's does.
        let on_a_fork = checkout(
            github_remote(&[], Completeness::Complete),
            "origin-fork/feature",
        );
        let file = Path::new("/work/app/src/lib.rs");

        let links = GitHubLinks::of(&pushed).unwrap();
        let older = links.commit_page(&commit('a').hash);
        let head = links.commit_page(&commit('b').hash);
        let unknown =
            GitHubLinks::of(&cut_short).and_then(|links| links.commit_page(&commit('a').hash));
        let pushed = entry(pushed);
        let branch = branch_summary(&pushed).and_then(|summary| summary.github);
        let on_a_fork = entry(on_a_fork);
        let fork_branch = branch_summary(&on_a_fork).and_then(|summary| summary.github);

        let hash = "a".repeat(40);
        assert_eq!(
            older,
            Some(format!("https://github.com/acme/app/commit/{hash}"))
        );
        assert_eq!(head, None, "the remote lacks the commit to push");
        assert_eq!(unknown, None, "the remote may lack any commit");
        assert_eq!(
            github_file_page(&pushed, file, PageKind::Blob),
            None,
            "the file's page is at HEAD, which the remote lacks"
        );
        assert_eq!(
            branch.as_deref(),
            Some("https://github.com/acme/app/tree/feature")
        );
        assert_eq!(fork_branch, None, "the upstream is on another remote");
        assert!(
            github_file_page(&on_a_fork, file, PageKind::Blob).is_some(),
            "HEAD is on the remote"
        );
    }

    #[test]
    fn a_blame_of_another_head_than_the_checkouts_links_no_commit() {
        let checkout = CheckoutState {
            root: PathBuf::from("/work/app"),
            remote: github_remote(&[], Completeness::Complete),
            kind: CheckoutKind::MainWorktree,
            branch: BranchState {
                name: Some("main".to_owned()),
                head: Some(commit('a')),
                upstream: None,
            },
            files: Vec::new(),
            completeness: Completeness::Complete,
        };
        let blame_of = |head: char| GitBlame {
            head: commit(head).hash,
            commits: vec![commit(head)],
            runs: Vec::new(),
        };

        let same = GitHubLinks::of_blame(&checkout, &blame_of('a'));
        let newer = GitHubLinks::of_blame(&checkout, &blame_of('b'));

        let hash = "a".repeat(40);
        assert_eq!(
            same.and_then(|links| links.commit_page(&commit('a').hash)),
            Some(format!("https://github.com/acme/app/commit/{hash}"))
        );
        assert_eq!(
            newer, None,
            "the checkout's state does not say whether the remote holds b"
        );
    }

    #[test]
    fn a_page_on_github_escapes_its_names() {
        let mut entry = AgentEntry::new(crate::state::summary(1, AgentStatus::Idle));
        entry.summary.folder = PathBuf::from("/work/app");
        entry.git.status.value = Loadable::Loaded(GitStatus {
            checkouts: vec![CheckoutState {
                root: PathBuf::from("/work/app"),
                remote: github_remote(&[], Completeness::Complete),
                kind: CheckoutKind::MainWorktree,
                branch: BranchState {
                    name: Some("fix/#12".to_owned()),
                    head: Some(commit('a')),
                    upstream: Some(Upstream {
                        name: "origin/fix/#12".to_owned(),
                        ahead: 0,
                        behind: 0,
                    }),
                },
                files: Vec::new(),
                completeness: Completeness::Complete,
            }],
        });

        let file = github_file_page(
            &entry,
            Path::new("/work/app/docs/C#/50% off.md"),
            PageKind::Blob,
        );
        let branch = branch_summary(&entry).and_then(|summary| summary.github);

        let hash = "a".repeat(40);
        assert_eq!(
            file,
            Some(format!(
                "https://github.com/acme/app/blob/{hash}/docs/C%23/50%25%20off.md"
            ))
        );
        assert_eq!(
            branch.as_deref(),
            Some("https://github.com/acme/app/tree/fix/%2312")
        );
    }

    #[test]
    fn the_history_closes_with_the_file_view() {
        let mut state = viewing_state();
        toggle_history(&mut state, agent(), 10, 20);

        crate::state::actions::close_file_view(&mut state, agent());

        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn an_edit_reads_only_the_checkouts_again() {
        let mut state = viewing_state();
        git_requests(&mut state);
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry
                .git
                .status
                .arrived(Loadable::Failed("busy".to_owned()));
            if let Some(file) = entry.git.files.get_mut(&lib_path()) {
                file.committed.arrived(Loadable::Loaded(GitFile::Outside));
            }
            step_finished(entry, Step::Edit);
        }

        let after_edit = git_requests(&mut state);

        assert_eq!(after_edit, [GitPurpose::Status { agent: agent() }]);
    }

    #[test]
    fn a_failed_read_again_keeps_what_shows_and_a_reconnect_reads_again() {
        let mut state = viewing_state();
        git_requests(&mut state);
        let status = GitStatus {
            checkouts: Vec::new(),
        };
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.git.status.arrived(Loadable::Loaded(status.clone()));
            entry.git.refresh();
        }
        git_requests(&mut state);
        arrived(
            &mut state,
            GitPurpose::Status { agent: agent() },
            Err("not connected".to_owned()),
        );
        let kept = state.agents[&agent()].git.status.value.clone();
        let before_reconnect = git_requests(&mut state);
        connected(&mut state, MachineId::THIS);
        let after_reconnect = git_requests(&mut state);

        assert_eq!(kept, Loadable::Loaded(status));
        assert!(before_reconnect.is_empty());
        assert!(after_reconnect.contains(&GitPurpose::Status { agent: agent() }));
    }

    #[test]
    fn only_the_shown_files_git_is_kept() {
        let mut state = viewing_state();
        git_requests(&mut state);
        let other = PathBuf::from("src/main.rs");
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry.files.insert(
                other.clone(),
                Loadable::Loaded(FileRead::unedited("main\n")),
            );
        }
        if let Some(view) = state.ui.file_views.get_mut(&agent()) {
            view.path = other;
        }

        git_requests(&mut state);

        let kept: Vec<PathBuf> = state.agents[&agent()].git.files.keys().cloned().collect();
        assert_eq!(kept, [PathBuf::from("/work/project-1/src/main.rs")]);
    }
}
