//! The files pane: the selected agent's folder as a tree, which lists a folder when it opens, with
//! what the agent changed and read.
//!
//! - "All" shows the folder's own entries, then, under "Also worked in", the folders outside it
//!   that hold a file the agent changed or read, as a git worktree beside it.
//! - "Changed" and "Read" show only those files, under their folders, all open.
//! - A search shows the project's files whose name holds the text, as Ctrl+O finds them.
//!
//! A listing is read again when the agent finishes a step that can change files, when the agent
//! comes back on screen, and on Refresh. The old entries stay on screen until the new ones arrive.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use cockpit_protocol::{
    AgentId, ChangedFile, Completeness, DirListing, EntryKind, FileChange, GitIgnore, ItemBody,
    ItemId, ToolCallStatus, TranscriptItem,
};
use serde_json::Value;

use super::actions::{self, Direction};
use super::app_state::{AgentEntry, AppState, Loadable};
use super::drafts::Drafts;
use super::file_edits::{self, BASH_TOOL, FILE_FIELDS, READ_TOOL};
use super::git::{self, Step};
use super::ui_state::{FileSearch, Popover, ViewMode};
use crate::machine::MachineId;
use crate::path_style::PathStyle;
use crate::prefs::{self, ClearedFolders, PaneState};

/// The rows a filter keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TreeFilter {
    #[default]
    All,
    Changed,
    Read,
}

/// One agent's tree. Every path is absolute.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileTree {
    /// The folders opened where folders start closed: the listed ones of "All".
    pub expanded: BTreeSet<PathBuf>,
    /// The folders closed where folders start open: those of "Changed" and "Read", and those
    /// under a folder outside every checkout, which only shows the files the agent touched.
    pub closed: BTreeSet<PathBuf>,
    pub listings: BTreeMap<PathBuf, Listing>,
    pub filter: TreeFilter,
    /// The row the keys and the pointer moved to.
    pub highlighted: Option<PathBuf>,
    /// The row `reveal` highlighted, until the pane scrolls to it. It may wait for a listing.
    pub reveal: Option<PathBuf>,
    /// The text of the search field, with the files found.
    pub search: Option<FileSearch>,
    /// The row whose menu is open.
    pub menu: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub entries: Loadable<DirListing>,
    /// The folder may have changed since it was listed: it lists again while it shows.
    pub stale: bool,
    /// A listing is on its way. Another waits for it, so that an older answer never comes last.
    pub reading: bool,
    /// The daemon said the folder does not exist, as a worktree removed since the agent worked in
    /// it: "Also worked in" leaves it out.
    pub gone: Option<Gone>,
}

/// When the daemon said a folder does not exist. The folder shows again, and lists again, once a
/// chat item after `after` has read or written a file in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gone {
    /// The agent's last chat item then. `None` when it had none.
    pub after: Option<ItemId>,
}

/// What the agent did to the files, by absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Footprint {
    pub changed: BTreeMap<PathBuf, LineCounts>,
    pub read: BTreeSet<PathBuf>,
    /// How each file that differs from its last commit differs, when the daemon read git.
    pub git: BTreeMap<PathBuf, FileChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCounts {
    pub added: u32,
    pub removed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// The entry's absolute path. A section or a note takes its folder's.
    pub path: PathBuf,
    pub name: String,
    pub depth: usize,
    pub kind: RowKind,
    pub marks: Marks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    Folder {
        open: bool,
        /// It shows open until closed, else closed until opened.
        starts_open: bool,
    },
    File,
    /// The head of the folders outside the agent's.
    Section,
    /// A folder's entries on their way.
    Loading,
    /// Why a folder shows no entries, or that it shows only some.
    Note,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Marks {
    /// The lines the agent changed in the file.
    pub change: Option<LineCounts>,
    /// The agent read the file.
    pub read: bool,
    /// The folder holds a file the agent changed, or one that differs from its last commit.
    pub holds_change: bool,
    /// How the file differs from its last commit.
    pub git: Option<FileChange>,
    /// The checkout ignores the entry or a folder above it.
    pub ignored: bool,
}

/// The head of the folders outside the agent's.
pub const OUTSIDE_SECTION: &str = "Also worked in";

/// The note under a folder whose listing left entries out.
const TRUNCATED_NOTE: &str = "More entries not shown";

/// The entry that marks a git checkout's top folder: a folder, or a file in a worktree.
const GIT_ENTRY: &str = ".git";

impl RowKind {
    /// Whether the keys and the pointer can highlight the row.
    pub fn is_entry(&self) -> bool {
        matches!(self, Self::Folder { .. } | Self::File)
    }
}

/// What `entry` changed and read, from its details and its transcript, and what differs from
/// the last commit in its checkouts, as the files pane shows it: without the folders outside the
/// agent's that no longer exist or that `cleared` hides, until a later chat item works in them.
pub fn footprint(entry: &AgentEntry, cleared: Option<&ClearedFolders>) -> Footprint {
    let Changes { changed, git } = changes(entry);
    let mut footprint = Footprint {
        changed,
        read: read_files(entry.transcript.items()),
        git,
    };
    let gone_marks = entry
        .tree
        .listings
        .values()
        .filter_map(|listing| listing.gone.map(|gone| gone.after));
    // The earliest mark: the chat items after it tell which folders the agent worked in since.
    let earliest: Option<Option<ItemId>> = cleared
        .map(|cleared| cleared.after)
        .into_iter()
        .chain(gone_marks)
        .min();
    let Some(since) = earliest else {
        return footprint;
    };
    let folder = &entry.summary.folder;
    let style = PathStyle::of(folder);
    let key = |path: &Path| style.key(&path.to_string_lossy());
    let cleared_keys: HashSet<String> = cleared
        .into_iter()
        .flat_map(|cleared| &cleared.folders)
        .map(|root| key(root))
        .collect();
    let last_touched: HashMap<String, ItemId> =
        touched_roots(folder, entry.transcript.items_after(since));
    // Hidden until a chat item after `mark` touches a file in it.
    let hidden_since = |root_key: &str, mark: Option<ItemId>| {
        last_touched
            .get(root_key)
            .is_none_or(|touched| Some(*touched) <= mark)
    };
    let hidden: HashSet<String> = outside_roots(&entry.tree, folder, &footprint)
        .into_iter()
        .map(|root| (key(&root), root))
        .filter(|(root_key, root)| {
            let gone = entry
                .tree
                .listings
                .get(root)
                .and_then(|listing| listing.gone)
                .is_some_and(|gone| hidden_since(root_key, gone.after));
            let cleared = cleared.is_some_and(|cleared| {
                cleared_keys.contains(root_key) && hidden_since(root_key, cleared.after)
            });
            gone || cleared
        })
        .map(|(root_key, _)| root_key)
        .collect();
    if hidden.is_empty() {
        return footprint;
    }
    let folder_text = folder.to_string_lossy();
    // A file right in a folder above the agent's lists under that folder, which holds the agent's
    // folder too: the agent's own files stay.
    let shown = |path: &PathBuf| {
        style
            .strip_prefix(&path.to_string_lossy(), &folder_text)
            .is_some()
            || !hidden.contains(&key(&outside_root(folder, path)))
    };
    footprint.changed.retain(|path, _| shown(path));
    footprint.read.retain(shown);
    footprint.git.retain(|path, _| shown(path));
    footprint
}

/// The folders `agent`'s "Also worked in" no longer lists.
pub fn cleared(state: &AppState, agent: AgentId) -> Option<&ClearedFolders> {
    let machine = state.daemon_machine(agent)?;
    state
        .prefs
        .files_pane
        .cleared(machine, MachineId::daemon_agent(agent))
}

/// [`footprint`] of `agent`, with the folders the user cleared.
pub fn footprint_of(state: &AppState, entry: &AgentEntry) -> Footprint {
    footprint(entry, cleared(state, entry.id()))
}

/// Clears "Also worked in": its folders no longer show until a later chat item reads or changes a
/// file in one.
pub fn clear_outside(state: &mut AppState, agent: AgentId) {
    let Some(entry) = state.agents.get(&agent) else {
        return;
    };
    let Some(machine) = state.daemon_machine(agent).map(str::to_owned) else {
        return;
    };
    let all = footprint(entry, None);
    // A folder cleared before stays hidden, though its files may have left the app's copy of
    // the chat.
    let mut folders = cleared(state, agent)
        .map(|before| before.folders.clone())
        .unwrap_or_default();
    folders.extend(outside_roots(&entry.tree, &entry.summary.folder, &all));
    let cleared = ClearedFolders {
        folders,
        after: entry.transcript.last_id(),
    };
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.tree.menu = None;
    }
    state
        .prefs
        .files_pane
        .set_cleared(&machine, MachineId::daemon_agent(agent), cleared);
}

/// The folders of "Also worked in" to list whether they are open or not, since a listing tells
/// whether a folder still exists: those never listed, those the daemon said were gone that the
/// agent worked in since, and the gone ones a refresh asks about.
pub fn roots_to_check(tree: &FileTree, folder: &Path, footprint: &Footprint) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = outside_roots(tree, folder, footprint)
        .into_iter()
        .filter(|root| match tree.listings.get(root) {
            None => true,
            Some(listing) => listing.gone.is_some() && !listing.reading,
        })
        .collect();
    for (path, listing) in &tree.listings {
        let asked = listing.gone.is_some() && listing.entries == Loadable::NotRequested;
        if asked && !listing.reading && !roots.contains(path) {
            roots.push(path.clone());
        }
    }
    roots
}

/// The folders "Also worked in" lists the files that the ended tool calls among `items` read or
/// wrote under, by [`PathStyle::key`], each with the last item that touched it. A call that still
/// runs may not have made its folder yet, so a listing then would find the folder gone.
fn touched_roots<'items>(
    folder: &Path,
    items: impl Iterator<Item = &'items TranscriptItem>,
) -> HashMap<String, ItemId> {
    let style = PathStyle::of(folder);
    let folder_text = folder.to_string_lossy();
    let mut roots = HashMap::new();
    for item in items {
        let (ItemBody::ToolCall(call) | ItemBody::Subagent(call)) = &item.body else {
            continue;
        };
        if call.status == ToolCallStatus::Running {
            continue;
        }
        let path = FILE_FIELDS
            .iter()
            .find_map(|field| match call.input.get(field) {
                Some(Value::String(path)) => Some(path),
                _ => None,
            });
        if let Some(path) = path
            && style.is_absolute(path)
            && style.strip_prefix(path, &folder_text).is_none()
        {
            let root = outside_root(folder, Path::new(path));
            // The items come in order: the last one stays.
            roots.insert(style.key(&root.to_string_lossy()), item.id);
        }
    }
    roots
}

/// The changed files, with their line counts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes {
    pub changed: BTreeMap<PathBuf, LineCounts>,
    /// How each file that differs from its last commit differs, when the daemon read git.
    pub git: BTreeMap<PathBuf, FileChange>,
}

/// The lines added and removed, all files together, in `u64` so that no count the daemon sends
/// overflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineTotals {
    pub added: u64,
    pub removed: u64,
}

impl Changes {
    pub fn totals(&self) -> LineTotals {
        let added = self
            .changed
            .values()
            .map(|counts| u64::from(counts.added))
            .sum();
        let removed = self
            .changed
            .values()
            .map(|counts| u64::from(counts.removed))
            .sum();
        LineTotals { added, removed }
    }
}

/// The files the agent changed, with its counts, and the files that differ from their last
/// commit, with git's counts, which are more exact: they count every change, whoever made it. A
/// binary file counts no line.
pub fn changes(entry: &AgentEntry) -> Changes {
    let mut changed: BTreeMap<PathBuf, LineCounts> = entry
        .details
        .loaded()
        .into_iter()
        .flat_map(|details| details.changed_files.iter())
        .map(|file| (file.path.clone(), counts(file)))
        .collect();
    let mut git = BTreeMap::new();
    let Some(status) = entry.git.status.value.loaded() else {
        return Changes { changed, git };
    };
    let style = PathStyle::of(&entry.summary.folder);
    // A checkout lists up to `MAX_STATUS_FILES`: each is looked up, not compared with each other.
    let mut names: HashMap<String, PathBuf> = HashMap::new();
    for path in changed.keys() {
        names
            .entry(style.key(&path.to_string_lossy()))
            .or_insert_with(|| path.clone());
    }
    for file in status.checkouts.iter().flat_map(|checkout| &checkout.files) {
        // The agent's name for the file wins, so that the file shows once.
        let key = names
            .entry(style.key(&file.path.to_string_lossy()))
            .or_insert_with(|| file.path.clone())
            .clone();
        let counts = file.counts.map_or(
            LineCounts {
                added: 0,
                removed: 0,
            },
            |counts| LineCounts {
                added: counts.added,
                removed: counts.removed,
            },
        );
        changed.insert(key.clone(), counts);
        git.insert(key, file.change);
    }
    Changes { changed, git }
}

fn counts(file: &ChangedFile) -> LineCounts {
    LineCounts {
        added: file.lines_added,
        removed: file.lines_removed,
    }
}

/// The files of the finished `Read` calls, subagents' included. Claude Code reads by absolute
/// path: another path is left out.
fn read_files<'items>(items: impl Iterator<Item = &'items TranscriptItem>) -> BTreeSet<PathBuf> {
    let mut read = BTreeSet::new();
    for item in items {
        let (ItemBody::ToolCall(call) | ItemBody::Subagent(call)) = &item.body else {
            continue;
        };
        if call.tool != READ_TOOL || call.status != ToolCallStatus::Done {
            continue;
        }
        let path = FILE_FIELDS
            .iter()
            .find_map(|field| match call.input.get(field) {
                Some(Value::String(path)) => Some(path),
                _ => None,
            });
        if let Some(path) = path
            && PathStyle::of(Path::new(path)).is_absolute(path)
        {
            read.insert(PathBuf::from(path));
        }
    }
    read
}

/// The tree's rows, top to bottom. `folder` is the agent's.
pub fn rows(tree: &FileTree, folder: &Path, footprint: &Footprint) -> Vec<TreeRow> {
    let roots: Vec<PathBuf> = std::iter::once(folder.to_owned())
        .chain(outside_roots(tree, folder, footprint))
        .collect();
    let style = PathStyle::of(folder);
    let builder = Builder {
        tree,
        footprint,
        style,
        roots: &roots,
        index: MarkIndex::new(footprint, style),
    };
    let mut rows = Vec::new();
    if let Some(search) = &tree.search {
        builder.search_rows(folder, search, &mut rows);
        return rows;
    }
    match tree.filter {
        TreeFilter::All => {
            builder.listed_rows(folder, 0, false, &mut rows);
            let outside = outside_roots(tree, folder, footprint);
            if !outside.is_empty() {
                rows.push(section(folder));
            }
            for root in outside {
                builder.root_row(&root, &mut rows);
            }
        }
        TreeFilter::Changed => {
            let paths: Vec<&Path> = footprint.changed.keys().map(PathBuf::as_path).collect();
            builder.picked_rows(folder, &paths, &mut rows);
        }
        TreeFilter::Read => {
            let paths: Vec<&Path> = footprint.read.iter().map(PathBuf::as_path).collect();
            builder.picked_rows(folder, &paths, &mut rows);
        }
    }
    rows
}

fn section(folder: &Path) -> TreeRow {
    TreeRow {
        path: folder.to_owned(),
        name: OUTSIDE_SECTION.to_owned(),
        depth: 0,
        kind: RowKind::Section,
        marks: Marks::default(),
    }
}

/// The folders outside the agent's that hold a file it changed or read, by path. On Windows, one
/// folder written in two cases or with both separators shows once. It takes the spelling that
/// `tree` listed, else the footprint's first, so that its row stays open or closed when the agent
/// writes the folder another way.
fn outside_roots(tree: &FileTree, folder: &Path, footprint: &Footprint) -> Vec<PathBuf> {
    let style = PathStyle::of(folder);
    let folder_text = folder.to_string_lossy();
    let mut roots: HashMap<String, PathBuf> = HashMap::new();
    for path in footprint.changed.keys().chain(&footprint.read) {
        if style
            .strip_prefix(&path.to_string_lossy(), &folder_text)
            .is_some()
        {
            continue;
        }
        let root = listed_spelling(tree, style, outside_root(folder, path));
        match roots.entry(style.key(&root.to_string_lossy())) {
            Entry::Vacant(vacant) => {
                vacant.insert(root);
            }
            Entry::Occupied(mut kept) => {
                if !tree.listings.contains_key(kept.get()) && tree.listings.contains_key(&root) {
                    kept.insert(root);
                }
            }
        }
    }
    let mut roots: Vec<PathBuf> = roots.into_values().collect();
    roots.sort();
    roots
}

/// `root` as `tree` listed it, if it did. The listing's key may write it with other separators,
/// which only a Windows host's `PathBuf` takes as the same path.
fn listed_spelling(tree: &FileTree, style: PathStyle, root: PathBuf) -> PathBuf {
    let text = root.to_string_lossy();
    let listed = tree
        .listings
        .keys()
        .find(|listed| style.same_path(&listed.to_string_lossy(), &text))
        .cloned();
    listed.unwrap_or(root)
}

/// `found`, a folder outside the agent's, as `roots` writes it: its row takes that spelling, and on
/// Windows a file in it may write it another way.
fn kept_root(style: PathStyle, roots: &[PathBuf], found: PathBuf) -> PathBuf {
    let kept = roots
        .iter()
        .find(|root| style.same_path(&root.to_string_lossy(), &found.to_string_lossy()));
    match kept {
        Some(root) => root.clone(),
        None => found,
    }
}

/// Whether the tree shows only the files the agent touched in `root`, a folder outside the
/// agent's. It does when the daemon did not list `root`, and when the listing holds no `.git`
/// entry, so that a folder such as `/etc` or the home folder does not bury those files. A
/// checkout beside the agent's folder, such as a worktree, lists in full.
fn touched_only(tree: &FileTree, root: &Path) -> bool {
    match tree.listings.get(root).map(|listing| &listing.entries) {
        Some(Loadable::Failed(_)) => true,
        Some(Loadable::Loaded(listing)) => {
            !listing.entries.iter().any(|entry| entry.name == GIT_ENTRY)
        }
        Some(Loadable::NotRequested | Loadable::Loading) | None => false,
    }
}

/// The folder at the top level of "All" that shows `full`, an absolute path: the agent's folder,
/// or a folder outside it that holds a file the agent changed or read. None for a path the tree
/// never shows: one outside these folders, or one the agent did not touch in a folder the daemon
/// refuses to list.
fn root_showing(
    tree: &FileTree,
    folder: &Path,
    footprint: &Footprint,
    full: &Path,
) -> Option<PathBuf> {
    let style = PathStyle::of(folder);
    let full_text = full.to_string_lossy();
    let holds = |root: &Path| {
        style
            .strip_prefix(&full_text, &root.to_string_lossy())
            .is_some()
    };
    if holds(folder) {
        return Some(folder.to_owned());
    }
    let root = outside_roots(tree, folder, footprint)
        .into_iter()
        .find(|root| holds(root))?;
    let touched = footprint
        .changed
        .keys()
        .chain(&footprint.read)
        .any(|touched| style.same_path(&touched.to_string_lossy(), &full_text));
    (touched || !touched_only(tree, &root)).then_some(root)
}

/// Whether the agent's tree can show `path`, a path of the file view: "Show in the files pane"
/// needs it.
pub fn can_show(state: &AppState, agent: AgentId, path: &Path) -> bool {
    let Some(entry) = state.agents.get(&agent) else {
        return false;
    };
    let folder = &entry.summary.folder;
    let full = PathStyle::of(folder).resolve(&folder.to_string_lossy(), &path.to_string_lossy());
    root_showing(
        &entry.tree,
        folder,
        &footprint_of(state, entry),
        Path::new(&full),
    )
    .is_some()
}

struct Builder<'tree> {
    tree: &'tree FileTree,
    footprint: &'tree Footprint,
    style: PathStyle,
    /// The agent's folder and the folders outside it, each shown once at the top level.
    roots: &'tree [PathBuf],
    index: MarkIndex,
}

/// The footprint by [`PathStyle::key`], which each row looks up: git can list thousands of files.
struct MarkIndex {
    changed: HashMap<String, LineCounts>,
    read: HashSet<String>,
    git: HashMap<String, FileChange>,
    /// The folders that hold a changed file.
    holding: HashSet<String>,
}

impl MarkIndex {
    fn new(footprint: &Footprint, style: PathStyle) -> Self {
        let key = |path: &PathBuf| style.key(&path.to_string_lossy());
        let mut changed = HashMap::new();
        let mut holding = HashSet::new();
        for (path, counts) in &footprint.changed {
            changed.entry(key(path)).or_insert(*counts);
            holding.extend(style.folder_keys(&path.to_string_lossy()));
        }
        let mut git = HashMap::new();
        for (path, change) in &footprint.git {
            git.entry(key(path)).or_insert(*change);
        }
        Self {
            changed,
            read: footprint.read.iter().map(key).collect(),
            git,
            holding,
        }
    }
}

impl Builder<'_> {
    fn marks(&self, path: &Path, folder: bool, ignored: bool) -> Marks {
        let key = self.style.key(&path.to_string_lossy());
        Marks {
            change: self.index.changed.get(&key).copied(),
            read: self.index.read.contains(&key),
            holds_change: folder && self.index.holding.contains(&key),
            ignored,
            git: self.index.git.get(&key).copied(),
        }
    }

    fn path_in(&self, folder: &Path, name: &str) -> PathBuf {
        PathBuf::from(self.style.join(&folder.to_string_lossy(), name))
    }

    /// The entries of `folder`, and those of its open folders under them.
    fn listed_rows(&self, folder: &Path, depth: usize, ignored: bool, rows: &mut Vec<TreeRow>) {
        let note = |name: &str| TreeRow {
            path: folder.to_owned(),
            name: name.to_owned(),
            depth,
            kind: RowKind::Note,
            marks: Marks::default(),
        };
        let listing = self
            .tree
            .listings
            .get(folder)
            .map(|listing| &listing.entries);
        let listing = match listing {
            Some(Loadable::Loaded(listing)) => listing,
            Some(Loadable::Failed(error)) => {
                rows.push(note(error));
                return;
            }
            Some(Loadable::NotRequested | Loadable::Loading) | None => {
                rows.push(TreeRow {
                    kind: RowKind::Loading,
                    ..note("")
                });
                return;
            }
        };
        for entry in &listing.entries {
            let listed = self.path_in(folder, &entry.name);
            let entry_ignored = ignored || entry.ignore == GitIgnore::Ignored;
            let is_folder = entry.kind == EntryKind::Folder;
            // A root shows open only at the top level, so that no entry shows twice. Its row here
            // takes the root's path: on Windows, the listing may write the folder in another case.
            let root = self.roots.iter().find(|root| {
                is_folder
                    && self
                        .style
                        .same_path(&root.to_string_lossy(), &listed.to_string_lossy())
            });
            let path = root.cloned().unwrap_or(listed);
            let open = is_folder && root.is_none() && self.tree.expanded.contains(&path);
            rows.push(TreeRow {
                marks: self.marks(&path, is_folder, entry_ignored),
                path: path.clone(),
                name: entry.name.clone(),
                depth,
                kind: if is_folder {
                    RowKind::Folder {
                        open,
                        starts_open: false,
                    }
                } else {
                    RowKind::File
                },
            });
            if open {
                self.listed_rows(&path, depth + 1, entry_ignored, rows);
            }
        }
        if listing.completeness == Completeness::Truncated {
            rows.push(note(TRUNCATED_NOTE));
        }
    }

    /// A folder outside the agent's, named by its own name, with its entries when it is open.
    fn root_row(&self, root: &Path, rows: &mut Vec<TreeRow>) {
        let open = self.tree.expanded.contains(root);
        rows.push(TreeRow {
            path: root.to_owned(),
            name: root_label(root),
            depth: 0,
            kind: RowKind::Folder {
                open,
                starts_open: false,
            },
            marks: self.marks(root, true, false),
        });
        if !open {
            return;
        }
        if !touched_only(self.tree, root) {
            self.listed_rows(root, 1, false, rows);
            return;
        }
        // The files the agent touched there show instead, as under "Read".
        let root_text = root.to_string_lossy();
        let touched = self.footprint.changed.keys().chain(&self.footprint.read);
        let relatives: Vec<String> = touched
            .filter_map(|path| {
                self.style
                    .strip_prefix(&path.to_string_lossy(), &root_text)
                    .filter(|relative| !relative.is_empty())
                    .map(str::to_owned)
            })
            .collect();
        let mut node = Node::default();
        for relative in &relatives {
            node.insert(&self.style.names(relative).collect::<Vec<_>>());
        }
        self.node_rows(root, &node, 1, rows);
    }

    /// `paths` under their folders, which start open: the agent's folder's first, then each
    /// folder outside it.
    fn picked_rows(&self, folder: &Path, paths: &[&Path], rows: &mut Vec<TreeRow>) {
        let folder_text = folder.to_string_lossy();
        let mut groups: BTreeMap<Option<PathBuf>, Vec<String>> = BTreeMap::new();
        for path in paths {
            let text = path.to_string_lossy();
            let (root, relative) = match self.style.strip_prefix(&text, &folder_text) {
                Some(relative) if !relative.is_empty() => (None, relative.to_owned()),
                Some(_) | None => {
                    let root = kept_root(self.style, self.roots, outside_root(folder, path));
                    let relative = self
                        .style
                        .strip_prefix(&text, &root.to_string_lossy())
                        .unwrap_or(&text)
                        .to_owned();
                    (Some(root), relative)
                }
            };
            groups.entry(root).or_default().push(relative);
        }
        let mut sectioned = false;
        for (root, relatives) in groups {
            let mut node = Node::default();
            for relative in &relatives {
                node.insert(&self.style.names(relative).collect::<Vec<_>>());
            }
            match root {
                None => self.node_rows(folder, &node, 0, rows),
                Some(root) => {
                    if !sectioned {
                        rows.push(section(folder));
                        sectioned = true;
                    }
                    let open = !self.tree.closed.contains(&root);
                    rows.push(TreeRow {
                        path: root.clone(),
                        name: root_label(&root),
                        depth: 0,
                        kind: RowKind::Folder {
                            open,
                            starts_open: true,
                        },
                        marks: self.marks(&root, true, false),
                    });
                    if open {
                        self.node_rows(&root, &node, 1, rows);
                    }
                }
            }
        }
    }

    /// `node`'s folders and files under `folder`. A folder that holds one folder and no file
    /// shares its row: "src", then "ui" under it, show as "src/ui", with the path of "ui".
    fn node_rows(&self, folder: &Path, node: &Node<'_>, depth: usize, rows: &mut Vec<TreeRow>) {
        for ((_, name), child) in &node.folders {
            let mut path = self.path_in(folder, name);
            let mut shown = (*name).to_owned();
            let mut child = child;
            while let Some((inner, only)) = child.only_folder() {
                path = self.path_in(&path, inner);
                shown = self.style.join(&shown, inner);
                child = only;
            }
            let open = !self.tree.closed.contains(&path);
            rows.push(TreeRow {
                marks: self.marks(&path, true, false),
                path: path.clone(),
                name: shown,
                depth,
                kind: RowKind::Folder {
                    open,
                    starts_open: true,
                },
            });
            if open {
                self.node_rows(&path, child, depth + 1, rows);
            }
        }
        for (_, name) in &node.files {
            let path = self.path_in(folder, name);
            rows.push(TreeRow {
                marks: self.marks(&path, false, false),
                path,
                name: (*name).to_owned(),
                depth,
                kind: RowKind::File,
            });
        }
    }

    /// The files found, one row each, named by their path from the agent's folder.
    fn search_rows(&self, folder: &Path, search: &FileSearch, rows: &mut Vec<TreeRow>) {
        let note = |name: &str| TreeRow {
            path: folder.to_owned(),
            name: name.to_owned(),
            depth: 0,
            kind: RowKind::Note,
            marks: Marks::default(),
        };
        match &search.found {
            Loadable::NotRequested | Loadable::Loading => rows.push(TreeRow {
                kind: RowKind::Loading,
                ..note("")
            }),
            Loadable::Failed(error) => rows.push(note(error)),
            Loadable::Loaded(found) if found.is_empty() => rows.push(note("No file found")),
            Loadable::Loaded(found) => {
                for path in found {
                    let path = PathBuf::from(
                        self.style
                            .resolve(&folder.to_string_lossy(), &path.to_string_lossy()),
                    );
                    rows.push(TreeRow {
                        marks: self.marks(&path, false, false),
                        name: file_edits::shown_path(folder, &path),
                        path,
                        depth: 0,
                        kind: RowKind::File,
                    });
                }
            }
        }
    }
}

/// Names under a folder: its folders, then its files, each by name without case first.
#[derive(Default)]
struct Node<'name> {
    folders: BTreeMap<(String, &'name str), Node<'name>>,
    files: BTreeSet<(String, &'name str)>,
}

impl<'name> Node<'name> {
    /// The one folder this node holds, when it holds no file and no other folder.
    fn only_folder(&self) -> Option<(&'name str, &Self)> {
        let mut folders = self.folders.iter();
        match (folders.next(), folders.next()) {
            (Some(((_, name), node)), None) if self.files.is_empty() => Some((name, node)),
            _ => None,
        }
    }

    fn insert(&mut self, names: &[&'name str]) {
        match names {
            [] => {}
            [file] => {
                self.files.insert((file.to_lowercase(), file));
            }
            [first, rest @ ..] => self
                .folders
                .entry((first.to_lowercase(), first))
                .or_default()
                .insert(rest),
        }
    }
}

/// The folder a file outside the agent's folder shows under: the first folder below what the two
/// paths share. Beside "/work/app", "/work/app_feature/src/lib.rs" shows under
/// "/work/app_feature", as "src/lib.rs".
pub fn outside_root(folder: &Path, path: &Path) -> PathBuf {
    let style = PathStyle::of(folder);
    let path = path.to_string_lossy();
    let mut root = path.as_ref();
    let mut rest_names = 0;
    // The longest folder above the path that holds the agent's folder too.
    while style
        .strip_prefix(&folder.to_string_lossy(), root)
        .is_none()
    {
        let Some(parent) = style.parent(root) else {
            break;
        };
        root = parent;
        rest_names += 1;
    }
    // A file right in the shared folder shows under it, not under itself.
    if rest_names >= 2 {
        let rest = style.strip_prefix(&path, root).unwrap_or_default();
        if let Some(first) = style.first_name(rest) {
            return PathBuf::from(style.join(root, first));
        }
    }
    PathBuf::from(root)
}

/// A folder's own name, as "app_feature/".
pub fn root_label(root: &Path) -> String {
    let style = PathStyle::of(root);
    let root = root.to_string_lossy();
    match style.file_name(&root) {
        Some(name) => style.folder_text(name),
        None => style.shown(&root).into_owned(),
    }
}

/// The folders whose listing the tree shows: the agent's, and each open folder whose folders
/// above it are open, up to the agent's folder or a folder outside it.
pub fn shown_folders(tree: &FileTree, folder: &Path, footprint: &Footprint) -> Vec<PathBuf> {
    if tree.search.is_some() || tree.filter != TreeFilter::All {
        return Vec::new();
    }
    let style = PathStyle::of(folder);
    let outside = outside_roots(tree, folder, footprint);
    let shown = |path: &Path| {
        let mut current = path.to_string_lossy().into_owned();
        loop {
            let current_path = Path::new(&current);
            if current_path == folder {
                return true;
            }
            if !tree.expanded.contains(current_path) {
                return false;
            }
            if outside.iter().any(|root| root == current_path) {
                return true;
            }
            match style.parent(&current) {
                Some(parent) if parent != current => current = parent.to_owned(),
                Some(_) | None => return false,
            }
        }
    };
    std::iter::once(folder.to_owned())
        .chain(tree.expanded.iter().filter(|path| shown(path)).cloned())
        .collect()
}

/// Whether the agent's files pane shows: as the agent last left it, else as Settings has it for
/// new agents.
pub fn pane_open(state: &AppState, agent: AgentId) -> bool {
    pane(state, agent) == PaneState::Open
}

fn pane(state: &AppState, agent: AgentId) -> PaneState {
    state
        .daemon_machine(agent)
        .map_or(state.prefs.files_pane.new_agents, |machine| {
            state
                .prefs
                .files_pane
                .agent(machine, MachineId::daemon_agent(agent))
        })
}

/// Opens the selected agent's pane, or closes it. Each agent keeps its own.
pub fn toggle_pane(state: &mut AppState) {
    if let Some(agent) = state.ui.selected {
        let toggled = pane(state, agent).toggled();
        set_pane(state, agent, toggled);
    }
}

/// The header's changes chip: opens the agent's pane on its changes, or on all its files when it
/// changed none, or closes the pane.
pub fn toggle_from_changes(state: &mut AppState, agent: AgentId) {
    if pane_open(state, agent) {
        set_pane(state, agent, PaneState::Closed);
        return;
    }
    set_pane(state, agent, PaneState::Open);
    if let Some(entry) = state.agents.get_mut(&agent) {
        let changed = !changes(entry).changed.is_empty();
        entry.tree.search = None;
        entry.tree.filter = if changed {
            TreeFilter::Changed
        } else {
            TreeFilter::All
        };
    }
}

/// Selects `agent` and opens its pane on its changed files, the uncommitted ones included.
pub fn open_on_changes(state: &mut AppState, agent: AgentId) {
    if state.ui.selected != Some(agent) {
        actions::select_agent(state, agent);
    }
    set_pane(state, agent, PaneState::Open);
    set_filter(state, agent, TreeFilter::Changed);
}

pub fn set_filter(state: &mut AppState, agent: AgentId, filter: TreeFilter) {
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.tree.filter = filter;
        entry.tree.search = None;
    }
}

/// Opens a folder's row, or closes it. A folder opened again lists again. The row says which way
/// its folder starts: a folder can show twice, inside a folder outside every checkout and at the
/// top level, each with its own way.
pub fn toggle_folder(state: &mut AppState, agent: AgentId, row: &TreeRow) {
    let RowKind::Folder { starts_open, .. } = row.kind else {
        return;
    };
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let tree = &mut entry.tree;
    let path = &row.path;
    if starts_open {
        if !tree.closed.remove(path) {
            tree.closed.insert(path.clone());
        }
    } else if !tree.expanded.remove(path) {
        tree.expanded.insert(path.clone());
        if let Some(listing) = tree.listings.get_mut(path) {
            listing.stale = true;
        }
    }
    tree.highlighted = Some(path.clone());
}

/// Closes the folders of every view: those opened under "All", and those that start open.
pub fn collapse_all(state: &mut AppState, agent: AgentId) {
    let cleared = cleared(state, agent).cloned();
    if let Some(entry) = state.agents.get_mut(&agent) {
        let starting_open = starting_open(
            &entry.tree,
            &entry.summary.folder,
            &footprint(entry, cleared.as_ref()),
        );
        entry.tree.expanded.clear();
        entry.tree.closed.extend(starting_open);
    }
}

/// Whether "Close every folder" has a folder to close, in any view.
pub fn can_collapse(tree: &FileTree, folder: &Path, footprint: &Footprint) -> bool {
    !tree.expanded.is_empty()
        || starting_open(tree, folder, footprint)
            .iter()
            .any(|folder| !tree.closed.contains(folder))
}

/// The folders that start open, by the paths their rows take: each folder outside the agent's
/// that holds a file it changed or read, and the folders between such a file and the agent's
/// folder or that folder outside it.
fn starting_open(tree: &FileTree, folder: &Path, footprint: &Footprint) -> BTreeSet<PathBuf> {
    let style = PathStyle::of(folder);
    let folder_text = folder.to_string_lossy();
    let roots = outside_roots(tree, folder, footprint);
    let mut folders = BTreeSet::new();
    for path in footprint.changed.keys().chain(&footprint.read) {
        let text = path.to_string_lossy();
        let (root, relative) = match style.strip_prefix(&text, &folder_text) {
            Some(relative) if !relative.is_empty() => (folder.to_string_lossy(), relative),
            Some(_) | None => {
                let root = kept_root(style, &roots, outside_root(folder, path));
                let root_text = root.to_string_lossy().into_owned();
                let relative = style.strip_prefix(&text, &root_text).unwrap_or(&text);
                folders.insert(root);
                (root_text.into(), relative)
            }
        };
        let names: Vec<&str> = style.names(relative).collect();
        let mut current = root.into_owned();
        for name in names.iter().take(names.len().saturating_sub(1)) {
            current = style.join(&current, name);
            folders.insert(PathBuf::from(&current));
        }
    }
    folders
}

/// Lists every folder again, as they show. A search keeps its files: typing again searches again.
pub fn refresh(tree: &mut FileTree) {
    list_again(tree);
    // Only a refresh asks again whether the folders that were gone exist: an agent's step does
    // not, so that a dozen removed worktrees do not list after each of its commands.
    for listing in tree.listings.values_mut() {
        if listing.gone.is_some() {
            listing.entries = Loadable::NotRequested;
        }
    }
}

/// Marks the listings to read again while they show. A failed listing starts again, but for a
/// folder the daemon said was gone: [`roots_to_check`] would list it even while hidden.
fn list_again(tree: &mut FileTree) {
    for listing in tree.listings.values_mut() {
        match listing.entries {
            Loadable::Loaded(_) | Loadable::Loading => listing.stale = true,
            Loadable::Failed(_) if listing.gone.is_some() => listing.stale = true,
            Loadable::Failed(_) => listing.entries = Loadable::NotRequested,
            Loadable::NotRequested => {}
        }
    }
}

/// `agent` finished `item`. A step that can change files lists the folders again.
pub fn item_finished(state: &mut AppState, agent: AgentId, item: &TranscriptItem) {
    let (ItemBody::ToolCall(call) | ItemBody::Subagent(call)) = &item.body else {
        return;
    };
    let bash = call.tool == BASH_TOOL;
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    match call.status {
        ToolCallStatus::Done if call.edit.is_some() || bash => {
            list_again(&mut entry.tree);
            let step = if bash { Step::Command } else { Step::Edit };
            git::step_finished(entry, step);
        }
        // A command that fails may have committed first, as `git commit && cargo test` does.
        ToolCallStatus::Failed if bash => git::step_finished(entry, Step::Command),
        ToolCallStatus::Done | ToolCallStatus::Failed | ToolCallStatus::Running => {}
    }
}

/// The search field's text: an empty one shows the tree again.
pub fn set_search(state: &mut AppState, agent: AgentId, text: &str) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let name = text.trim();
    entry.tree.search = (!name.is_empty()).then(|| FileSearch {
        name: name.to_owned(),
        found: Loadable::NotRequested,
    });
}

/// The rows of `agent`'s tree as the pane shows them.
pub fn shown_rows(state: &AppState, agent: AgentId) -> Vec<TreeRow> {
    state.agents.get(&agent).map_or_else(Vec::new, |entry| {
        rows(
            &entry.tree,
            &entry.summary.folder,
            &footprint_of(state, entry),
        )
    })
}

/// Moves the highlight to the next or previous entry, from the first or the last when no entry
/// is highlighted.
pub fn move_highlight(state: &mut AppState, agent: AgentId, direction: Direction) {
    let rows = shown_rows(state, agent);
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let entries: Vec<&TreeRow> = rows.iter().filter(|row| row.kind.is_entry()).collect();
    let at = entry
        .tree
        .highlighted
        .as_ref()
        .and_then(|highlighted| entries.iter().position(|row| &row.path == highlighted));
    let next = match (direction, at) {
        (Direction::Down, Some(at)) => entries.get(at + 1).or(entries.get(at)),
        (Direction::Up, Some(at)) => entries.get(at.saturating_sub(1)),
        (Direction::Down, None) => entries.first(),
        (Direction::Up, None) => entries.last(),
    };
    if let Some(next) = next {
        entry.tree.highlighted = Some(next.path.clone());
    }
}

/// Right: opens a closed folder, or goes to an open one's first entry. Left: closes an open
/// folder, or goes to the folder above.
pub fn step_sideways(state: &mut AppState, agent: AgentId, direction: Sideways) {
    let rows = shown_rows(state, agent);
    let Some(highlighted) = state
        .agents
        .get(&agent)
        .and_then(|entry| entry.tree.highlighted.clone())
    else {
        return;
    };
    let Some(at) = rows.iter().position(|row| row.path == highlighted) else {
        return;
    };
    let Some(row) = rows.get(at) else {
        return;
    };
    let target = match (direction, &row.kind) {
        (Sideways::Right, RowKind::Folder { open: false, .. })
        | (Sideways::Left, RowKind::Folder { open: true, .. }) => {
            toggle_folder(state, agent, row);
            return;
        }
        (Sideways::Right, RowKind::Folder { open: true, .. }) => rows
            .get(at + 1)
            .filter(|next| next.depth > row.depth && next.kind.is_entry()),
        (Sideways::Left, _) => rows.get(..at).and_then(|above| {
            above
                .iter()
                .rev()
                .find(|above| above.depth < row.depth && above.kind.is_entry())
        }),
        (Sideways::Right, _) => None,
    };
    if let Some(target) = target
        && let Some(entry) = state.agents.get_mut(&agent)
    {
        entry.tree.highlighted = Some(target.path.clone());
    }
}

/// A sideways arrow key in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sideways {
    Left,
    Right,
}

pub fn highlight(state: &mut AppState, agent: AgentId, path: &Path) {
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.tree.highlighted = Some(path.to_owned());
    }
}

/// Opens a file in the file view, or opens or closes a folder.
pub fn activate(state: &mut AppState, agent: AgentId, row: &TreeRow) {
    match row.kind {
        RowKind::File => {
            highlight(state, agent, &row.path);
            actions::open_file(state, agent, row.path.clone(), None);
        }
        RowKind::Folder { .. } => toggle_folder(state, agent, row),
        RowKind::Section | RowKind::Loading | RowKind::Note => {}
    }
}

/// Opens a file the agent changed on its diff, whatever its kind.
pub fn open_diff(state: &mut AppState, agent: AgentId, path: &Path) {
    actions::open_file(state, agent, path.to_owned(), None);
    actions::set_view_mode(state, agent, ViewMode::Diff);
}

/// Opens a row's menu at `x`, `y` in the window's pixels.
pub fn open_menu(state: &mut AppState, agent: AgentId, path: &Path, x: i32, y: i32) {
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.tree.menu = Some(path.to_owned());
        entry.tree.highlighted = Some(path.to_owned());
    }
    state.ui.popover = Some(Popover::TreeMenu { agent, x, y });
}

/// A row of the menu of a tree row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeMenuItem {
    /// Opens a file, or opens or closes a folder.
    Open,
    OpenDiff,
    ShowHistory,
    Mention,
    CopyPath,
    CopyRelativePath,
    OpenOnGitHub,
    ShowInFileManager,
}

impl TreeMenuItem {
    /// The letter that picks it while the menu is open.
    pub fn key(self) -> char {
        match self {
            Self::Open => 'o',
            Self::OpenDiff => 'd',
            Self::ShowHistory => 'h',
            Self::Mention => 'm',
            Self::CopyPath => 'c',
            Self::CopyRelativePath => 'e',
            Self::OpenOnGitHub => 'g',
            Self::ShowInFileManager => 'x',
        }
    }
}

/// Where the entry of a menu stands, beyond its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MenuPlace {
    /// It is in the agent's folder: its path from there can be copied.
    pub in_folder: bool,
    /// It is a file with a git history.
    pub history: bool,
    /// Its checkout is on GitHub.
    pub on_github: bool,
    /// It is on this computer.
    pub local: bool,
}

/// The menu's rows for `row`: a file the agent changed also opens on its diff, and an entry in the
/// agent's folder also copies its path from that folder.
pub fn menu_items(row: &TreeRow, place: MenuPlace) -> Vec<TreeMenuItem> {
    let file = row.kind == RowKind::File;
    let diff = file && row.marks.change.is_some();
    let mut items = vec![TreeMenuItem::Open];
    if diff {
        items.push(TreeMenuItem::OpenDiff);
    }
    if file && place.history {
        items.push(TreeMenuItem::ShowHistory);
    }
    items.extend([TreeMenuItem::Mention, TreeMenuItem::CopyPath]);
    if place.in_folder {
        items.push(TreeMenuItem::CopyRelativePath);
    }
    if place.on_github {
        items.push(TreeMenuItem::OpenOnGitHub);
    }
    if place.local {
        items.push(TreeMenuItem::ShowInFileManager);
    }
    items
}

/// Adds `path` at the end of the agent's message, apart from the text before it: from the
/// agent's folder when the file is in it.
pub fn mention(state: &mut AppState, drafts: &mut Drafts, agent: AgentId, path: &Path) {
    let Some(entry) = state.agents.get(&agent) else {
        return;
    };
    let text = file_edits::shown_path(&entry.summary.folder, path);
    let draft = drafts.draft_mut(agent);
    if !draft.text.is_empty() && !draft.text.ends_with(char::is_whitespace) {
        draft.text.push(' ');
    }
    draft.text.push_str(&text);
    draft.text.push(' ');
    drafts.draft_written(agent);
    state.ui.popover = None;
}

/// Shows `path` in the file view, then opens the agent's files pane on it. A file already shown
/// stays as it is: opening it again would read it again and reset its mode and its scroll.
pub fn show_in_pane(state: &mut AppState, agent: AgentId, path: &Path) {
    if state
        .ui
        .file_view(agent)
        .is_none_or(|view| view.path != path)
    {
        actions::open_file(state, agent, path.to_owned(), None);
    }
    set_pane(state, agent, PaneState::Open);
    reveal(state, agent);
}

/// Shows the file view's file in the tree: its folders open and its row highlighted, for the pane
/// to scroll to once it shows. A file the filter hides shows under "All". A file the tree never
/// shows changes nothing.
pub fn reveal(state: &mut AppState, agent: AgentId) {
    let Some(shown) = state.ui.file_view(agent).map(|view| view.path.clone()) else {
        return;
    };
    let cleared = cleared(state, agent).cloned();
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let folder = entry.summary.folder.clone();
    let style = PathStyle::of(&folder);
    let full = PathBuf::from(style.resolve(&folder.to_string_lossy(), &shown.to_string_lossy()));
    let marks = footprint(entry, cleared.as_ref());
    let Some(root) = root_showing(&entry.tree, &folder, &marks, &full) else {
        return;
    };
    let Some(full) = in_root_spelling(style, &root, &full) else {
        return;
    };
    let full_text = full.to_string_lossy().into_owned();
    let is_revealed = |path: &PathBuf| style.same_path(&path.to_string_lossy(), &full_text);
    let kept = match entry.tree.filter {
        TreeFilter::All => true,
        TreeFilter::Changed => marks.changed.keys().any(is_revealed),
        TreeFilter::Read => marks.read.iter().any(is_revealed),
    };
    if !kept {
        entry.tree.filter = TreeFilter::All;
    }
    entry.tree.search = None;
    let mut above = style.parent(&full_text).map(str::to_owned);
    while let Some(parent) = above {
        if style
            .strip_prefix(&parent, &root.to_string_lossy())
            .is_none()
        {
            break;
        }
        let parent_path = PathBuf::from(&parent);
        entry.tree.closed.remove(&parent_path);
        if parent_path != entry.summary.folder {
            entry.tree.expanded.insert(parent_path);
        }
        above = style
            .parent(&parent)
            .filter(|next| *next != parent)
            .map(str::to_owned);
    }
    entry.tree.highlighted = Some(full.clone());
    entry.tree.reveal = Some(full);
}

/// `path` with its start written as `root` writes it. `root` is a top folder of the tree that holds
/// `path`, and the rows under it start with its spelling, which the file view's path may not share
/// on Windows.
fn in_root_spelling(style: PathStyle, root: &Path, path: &Path) -> Option<PathBuf> {
    let path = path.to_string_lossy();
    let root = root.to_string_lossy();
    let relative = style.strip_prefix(&path, &root)?;
    let written = style
        .names(relative)
        .fold(root.into_owned(), |written, name| {
            style.join(&written, name)
        });
    Some(PathBuf::from(written))
}

/// Whether the pane scrolls to the row `reveal` highlighted: it is still highlighted, and shows
/// among `rows` now.
pub fn reveal_due(tree: &FileTree, rows: &[TreeRow]) -> bool {
    tree.reveal.as_ref().is_some_and(|revealed| {
        tree.highlighted.as_ref() == Some(revealed)
            && rows
                .iter()
                .any(|row| &row.path == revealed && row.kind.is_entry())
    })
}

/// The pane scrolled to the row `reveal` highlighted.
pub fn revealed(state: &mut AppState, agent: AgentId) {
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.tree.reveal = None;
    }
}

/// Keeps the pane's width, in pixels, where its splitter left it.
pub fn set_width(state: &mut AppState, width: f64) {
    let rounded = width.round();
    let bounds = f64::from(prefs::MIN_FILES_WIDTH)..=f64::from(prefs::MAX_FILES_WIDTH);
    if !bounds.contains(&rounded) {
        tracing::warn!("ignored a files pane width of {width} pixels");
        return;
    }
    // Within the bounds, the cast keeps the value.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the bounds above fit in u16"
    )]
    let pixels = rounded as u16;
    state.prefs.files_pane.width = Some(pixels);
}

pub fn reset_width(state: &mut AppState) {
    state.prefs.files_pane.width = None;
}

fn set_pane(state: &mut AppState, agent: AgentId, pane: PaneState) {
    let Some(machine) = state.daemon_machine(agent).map(str::to_owned) else {
        return;
    };
    state
        .prefs
        .files_pane
        .set_agent(&machine, MachineId::daemon_agent(agent), pane);
}

/// Keeps the panes of the agents a snapshot of `machine` holds: the others closed while the app
/// was away.
pub fn keep_listed_panes(state: &mut AppState, machine: MachineId) {
    let Some(name) = state
        .machines
        .get(machine)
        .map(|known| known.daemon.machine.clone())
    else {
        return;
    };
    let listed: BTreeSet<AgentId> = state
        .agents
        .keys()
        .filter(|&&agent| MachineId::of(agent) == machine)
        .map(|&agent| MachineId::daemon_agent(agent))
        .collect();
    state
        .prefs
        .files_pane
        .keep_agents(&name, |agent| listed.contains(&agent));
}

/// Drops the panes of a machine's agents before the app forgets the machine.
pub fn forget_machine_panes(state: &mut AppState, machine: MachineId) {
    if let Some(known) = state.machines.get(machine) {
        let name = known.daemon.machine.clone();
        state.prefs.files_pane.forget_machine(&name);
    }
}

/// Drops the pane of an agent the daemon removed.
pub fn forget_pane(state: &mut AppState, agent: AgentId) {
    let Some(machine) = state.daemon_machine(agent).map(str::to_owned) else {
        return;
    };
    state
        .prefs
        .files_pane
        .forget_agent(&machine, MachineId::daemon_agent(agent));
}

#[cfg(test)]
mod tests {
    use cockpit_client::RequestError;
    use cockpit_protocol::{
        AgentDetails, AgentStatus, CheckoutKind, DirEntry, EntryLink, ErrorBody, ErrorCode, ItemId,
        Method, ResponseBody, ToolCall,
    };
    use serde_json::json;

    use super::*;
    use crate::state::Purpose;

    const FOLDER: &str = "/work/app";

    fn entry(name: &str, kind: EntryKind, ignore: GitIgnore) -> DirEntry {
        DirEntry {
            name: name.to_owned(),
            kind,
            size: None,
            link: EntryLink::Direct,
            ignore,
        }
    }

    fn listing(path: &str, entries: Vec<DirEntry>) -> (PathBuf, Listing) {
        (
            PathBuf::from(path),
            Listing {
                entries: Loadable::Loaded(DirListing {
                    path: PathBuf::from(path),
                    entries,
                    completeness: Completeness::Complete,
                }),
                stale: false,
                reading: false,
                gone: None,
            },
        )
    }

    fn changed(path: &str, added: u32, removed: u32) -> (PathBuf, LineCounts) {
        (PathBuf::from(path), LineCounts { added, removed })
    }

    /// Each row as its indent, a sign for its kind, and its name: "▸" a closed folder, "▾" an
    /// open one, "·" a file, "#" a section, "…" a loading row, "!" a note.
    fn shown(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let sign = match row.kind {
                    RowKind::Folder { open: false, .. } => "▸",
                    RowKind::Folder { open: true, .. } => "▾",
                    RowKind::File => "·",
                    RowKind::Section => "#",
                    RowKind::Loading => "…",
                    RowKind::Note => "!",
                };
                format!("{}{sign} {}", "  ".repeat(row.depth), row.name)
            })
            .collect()
    }

    fn tree() -> FileTree {
        FileTree {
            listings: [
                listing(
                    FOLDER,
                    vec![
                        entry("src", EntryKind::Folder, GitIgnore::Kept),
                        entry("target", EntryKind::Folder, GitIgnore::Ignored),
                        entry("README.md", EntryKind::File, GitIgnore::Kept),
                    ],
                ),
                listing(
                    "/work/app/src",
                    vec![
                        entry("lexer.rs", EntryKind::File, GitIgnore::Kept),
                        entry("lib.rs", EntryKind::File, GitIgnore::Kept),
                    ],
                ),
                listing(
                    "/work/app/target",
                    vec![entry("debug", EntryKind::Folder, GitIgnore::Kept)],
                ),
            ]
            .into_iter()
            .collect(),
            ..FileTree::default()
        }
    }

    #[test]
    fn all_shows_the_listed_entries_and_the_open_folders_under_them() {
        let mut tree = tree();
        tree.expanded.insert(PathBuf::from("/work/app/src"));
        tree.expanded.insert(PathBuf::from("/work/app/target"));
        tree.expanded
            .insert(PathBuf::from("/work/app/target/debug"));

        let rows = rows(&tree, Path::new(FOLDER), &Footprint::default());

        assert_eq!(
            shown(&rows),
            [
                "▾ src",
                "  · lexer.rs",
                "  · lib.rs",
                "▾ target",
                "  ▾ debug",
                "    … ",
                "· README.md",
            ]
        );
        let ignored: Vec<&str> = rows
            .iter()
            .filter(|row| row.marks.ignored)
            .map(|row| row.name.as_str())
            .collect();
        assert_eq!(ignored, ["target", "debug"]);
    }

    #[test]
    fn rows_carry_what_the_agent_changed_and_read() {
        let mut tree = tree();
        tree.expanded.insert(PathBuf::from("/work/app/src"));
        let footprint = Footprint {
            changed: [changed("/work/app/src/lexer.rs", 1, 1)]
                .into_iter()
                .collect(),
            read: [PathBuf::from("/work/app/src/lib.rs")]
                .into_iter()
                .collect(),
            git: BTreeMap::new(),
        };

        let rows = rows(&tree, Path::new(FOLDER), &footprint);

        let marks = |name: &str| {
            rows.iter()
                .find(|row| row.name == name)
                .map(|row| row.marks)
                .unwrap()
        };
        assert!(marks("src").holds_change);
        assert!(!marks("target").holds_change);
        assert_eq!(
            marks("lexer.rs").change,
            Some(LineCounts {
                added: 1,
                removed: 1
            })
        );
        assert!(marks("lib.rs").read);
        assert!(!marks("lexer.rs").read);
    }

    #[test]
    fn a_root_listed_inside_another_shows_closed_so_no_row_shows_twice() {
        let mut tree = tree();
        tree.listings.extend([listing(
            "/work",
            vec![
                entry(".git", EntryKind::Folder, GitIgnore::Ignored),
                entry("app", EntryKind::Folder, GitIgnore::Kept),
                entry("notes.md", EntryKind::File, GitIgnore::Kept),
            ],
        )]);
        tree.expanded.insert(PathBuf::from("/work"));
        tree.expanded.insert(PathBuf::from(FOLDER));
        let footprint = Footprint {
            changed: BTreeMap::new(),
            read: [PathBuf::from("/work/notes.md")].into_iter().collect(),
            git: BTreeMap::new(),
        };

        let rows = rows(&tree, Path::new(FOLDER), &footprint);

        assert_eq!(
            shown(&rows),
            [
                "▸ src",
                "▸ target",
                "· README.md",
                "# Also worked in",
                "▾ work/",
                "  ▸ .git",
                "  ▸ app",
                "  · notes.md",
            ]
        );
        let mut paths: Vec<&PathBuf> = rows
            .iter()
            .filter(|row| row.kind.is_entry())
            .map(|row| &row.path)
            .collect();
        let count = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), count, "a path shows twice: {rows:?}");
    }

    #[test]
    fn on_windows_a_root_listed_inside_another_in_another_case_opens_at_the_top_level() {
        let folder = Path::new(r"C:\work\app");
        let mut tree = FileTree::default();
        tree.listings.extend([
            listing(
                r"C:\work",
                vec![
                    entry(".git", EntryKind::Folder, GitIgnore::Ignored),
                    entry("app", EntryKind::Folder, GitIgnore::Kept),
                    entry("other", EntryKind::Folder, GitIgnore::Kept),
                    entry("notes.md", EntryKind::File, GitIgnore::Kept),
                ],
            ),
            listing(
                r"C:\Work\Other",
                vec![entry("a.md", EntryKind::File, GitIgnore::Kept)],
            ),
        ]);
        tree.expanded.insert(PathBuf::from(r"C:\work"));
        let footprint = Footprint {
            changed: [changed(r"C:\Work\Other\a.md", 1, 0)].into_iter().collect(),
            read: [PathBuf::from(r"C:\work\notes.md")].into_iter().collect(),
            git: BTreeMap::new(),
        };
        let inner = rows(&tree, folder, &footprint)
            .into_iter()
            .find(|row| row.depth == 1 && row.name == "other")
            .unwrap();

        tree.expanded.insert(inner.path);

        assert_eq!(
            shown(&rows(&tree, folder, &footprint)),
            [
                "… ",
                "# Also worked in",
                r"▾ Other\",
                "  · a.md",
                r"▾ work\",
                "  ▸ .git",
                "  ▸ app",
                "  ▸ other",
                "  · notes.md",
            ]
        );
    }

    #[test]
    fn on_windows_marks_match_whatever_the_separators_and_the_case() {
        let mut tree = FileTree::default();
        tree.listings.extend([listing(
            r"C:\work\app",
            vec![entry("README.md", EntryKind::File, GitIgnore::Kept)],
        )]);
        let footprint = Footprint {
            changed: [changed("c:/WORK/app/README.md", 1, 0)]
                .into_iter()
                .collect(),
            read: [PathBuf::from("C:/work/app/README.md")]
                .into_iter()
                .collect(),
            git: BTreeMap::new(),
        };

        let rows = rows(&tree, Path::new(r"C:\work\app"), &footprint);

        let marks = rows.first().map(|row| row.marks).unwrap();
        assert!(marks.read);
        assert_eq!(
            marks.change,
            Some(LineCounts {
                added: 1,
                removed: 0
            })
        );
    }

    #[test]
    fn folders_outside_the_agents_follow_under_their_section() {
        let mut tree = tree();
        let worktree = "/work/app_columns";
        tree.expanded.insert(PathBuf::from(worktree));
        tree.listings.extend([listing(
            worktree,
            vec![
                entry(".git", EntryKind::File, GitIgnore::Ignored),
                entry("Cargo.toml", EntryKind::File, GitIgnore::Kept),
            ],
        )]);
        let footprint = Footprint {
            changed: [changed("/work/app_columns/src/token.rs", 3, 0)]
                .into_iter()
                .collect(),
            read: [PathBuf::from("/etc/calc/config.toml")]
                .into_iter()
                .collect(),
            git: BTreeMap::new(),
        };

        let rows = rows(&tree, Path::new(FOLDER), &footprint);

        assert_eq!(
            shown(&rows),
            [
                "▸ src",
                "▸ target",
                "· README.md",
                "# Also worked in",
                "▸ etc/",
                "▾ app_columns/",
                "  · .git",
                "  · Cargo.toml",
            ]
        );
        assert_eq!(
            shown_folders(&tree, Path::new(FOLDER), &footprint),
            [PathBuf::from(FOLDER), PathBuf::from(worktree)]
        );
    }

    #[test]
    fn changed_shows_the_changed_files_under_open_folders() {
        let tree = FileTree {
            filter: TreeFilter::Changed,
            ..FileTree::default()
        };
        let footprint = Footprint {
            changed: [
                changed("/work/app/tests/columns.rs", 8, 0),
                changed("/work/app/src/error.rs", 2, 0),
                changed("/work/app/README.md", 1, 1),
                changed("/work/app_GH42Q/crates/lexer/src/token.rs", 31, 4),
            ]
            .into_iter()
            .collect(),
            read: BTreeSet::new(),
            git: BTreeMap::new(),
        };

        let rows = rows(&tree, Path::new(FOLDER), &footprint);

        assert_eq!(
            shown(&rows),
            [
                "▾ src",
                "  · error.rs",
                "▾ tests",
                "  · columns.rs",
                "· README.md",
                "# Also worked in",
                "▾ app_GH42Q/",
                "  ▾ crates/lexer/src",
                "    · token.rs",
            ]
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.name == "token.rs")
                .map(|row| row.path.clone()),
            Some(PathBuf::from("/work/app_GH42Q/crates/lexer/src/token.rs"))
        );
    }

    #[test]
    fn on_windows_changed_files_group_whatever_their_case_and_show_backslashes() {
        let folder = r"C:\work\app";
        let tree = FileTree {
            filter: TreeFilter::Changed,
            ..FileTree::default()
        };
        let footprint = Footprint {
            changed: [
                changed(r"c:\work\APP\src\lexer.rs", 1, 1),
                changed(r"C:\work\app_columns\src\token.rs", 3, 0),
            ]
            .into_iter()
            .collect(),
            read: BTreeSet::new(),
            git: BTreeMap::new(),
        };

        let rows = rows(&tree, Path::new(folder), &footprint);

        assert_eq!(
            shown(&rows),
            [
                "▾ src",
                "  · lexer.rs",
                "# Also worked in",
                r"▾ app_columns\",
                "  ▾ src",
                "    · token.rs",
            ]
        );
    }

    #[test]
    fn on_windows_an_outside_folder_shows_once_whatever_its_case_and_separators() {
        let folder = r"C:\work\app";
        let tree = FileTree {
            filter: TreeFilter::Changed,
            ..FileTree::default()
        };
        let footprint = Footprint {
            changed: [
                changed(r"C:\Work\Other\a.md", 1, 0),
                changed(r"c:/work/OTHER/b.md", 2, 0),
            ]
            .into_iter()
            .collect(),
            read: BTreeSet::new(),
            git: BTreeMap::new(),
        };

        let rows = rows(&tree, Path::new(folder), &footprint);

        assert_eq!(
            shown(&rows),
            ["# Also worked in", r"▾ Other\", "  · a.md", "  · b.md"]
        );
    }

    #[test]
    fn a_search_lists_the_files_found_from_the_agents_folder() {
        let tree = FileTree {
            search: Some(FileSearch {
                name: "lex".to_owned(),
                found: Loadable::Loaded(vec![PathBuf::from("/work/app/src/lexer.rs")]),
            }),
            ..tree()
        };

        let rows = rows(&tree, Path::new(FOLDER), &Footprint::default());

        assert_eq!(shown(&rows), ["· src/lexer.rs"]);
        assert_eq!(
            rows.first().map(|row| row.path.clone()),
            Some(PathBuf::from("/work/app/src/lexer.rs"))
        );
    }

    #[test]
    fn only_open_folders_under_open_folders_list() {
        let mut tree = tree();
        tree.expanded.insert(PathBuf::from("/work/app/src"));
        tree.expanded
            .insert(PathBuf::from("/work/app/target/debug"));

        let folders = shown_folders(&tree, Path::new(FOLDER), &Footprint::default());

        assert_eq!(
            folders,
            [PathBuf::from(FOLDER), PathBuf::from("/work/app/src")]
        );
    }

    #[test]
    fn reads_come_from_finished_read_calls_with_absolute_paths() {
        let read = |path: &str, status: ToolCallStatus| TranscriptItem {
            id: ItemId::new(1),
            parent: None,
            body: ItemBody::ToolCall(ToolCall {
                tool: READ_TOOL.to_owned(),
                summary: path.to_owned(),
                input: json!({ "file_path": path }),
                status,
                output: None,
                exit_code: None,
                edit: None,
            }),
        };
        let items = [
            read("/work/app/src/lib.rs", ToolCallStatus::Done),
            read("src/main.rs", ToolCallStatus::Done),
            read("/work/app/README.md", ToolCallStatus::Running),
        ];

        let files = read_files(items.iter());

        assert_eq!(
            files.into_iter().collect::<Vec<_>>(),
            [PathBuf::from("/work/app/src/lib.rs")]
        );
    }

    #[test]
    fn a_folder_that_holds_only_one_folder_shares_its_row() {
        let tree = FileTree {
            filter: TreeFilter::Changed,
            ..FileTree::default()
        };
        let footprint = Footprint {
            changed: [
                changed("/work/app/web/src/views/login/form.ts", 7, 3),
                changed("/work/app/web/src/views/search/list.ts", 21, 9),
                changed("/work/app/service/seeds/41_users.sql", 41, 21),
                changed("/work/app/service/src/grpc/users.proto", 3, 2),
            ]
            .into_iter()
            .collect(),
            read: BTreeSet::new(),
            git: BTreeMap::new(),
        };

        let rows = rows(&tree, Path::new(FOLDER), &footprint);

        assert_eq!(
            shown(&rows),
            [
                "▾ service",
                "  ▾ seeds",
                "    · 41_users.sql",
                "  ▾ src/grpc",
                "    · users.proto",
                "▾ web/src/views",
                "  ▾ login",
                "    · form.ts",
                "  ▾ search",
                "    · list.ts",
            ]
        );
        let merged = rows.iter().find(|row| row.name == "web/src/views");
        assert_eq!(
            merged.map(|row| (row.path.clone(), row.marks.holds_change)),
            Some((PathBuf::from("/work/app/web/src/views"), true))
        );
    }

    #[test]
    fn a_group_head_names_its_folder_once() {
        assert_eq!(
            root_label(Path::new("/work/app_GH42Q-report-columns")),
            "app_GH42Q-report-columns/"
        );
        assert_eq!(root_label(Path::new("/")), "/");
        assert_eq!(
            root_label(Path::new("C:/work/app_feature")),
            r"app_feature\"
        );
        assert_eq!(root_label(Path::new("C:/")), r"C:\");
    }

    #[test]
    fn line_totals_hold_counts_past_u32() {
        let counts = |added: u32, removed: u32| LineCounts { added, removed };
        let changes = Changes {
            changed: BTreeMap::from([
                (PathBuf::from("a.rs"), counts(u32::MAX, 1)),
                (PathBuf::from("b.rs"), counts(u32::MAX, u32::MAX)),
            ]),
            git: BTreeMap::new(),
        };

        assert_eq!(
            changes.totals(),
            LineTotals {
                added: 2 * u64::from(u32::MAX),
                removed: u64::from(u32::MAX) + 1
            }
        );
    }

    /// The selected agent of `/work/project-1`, with its folder listed: `src` holding `lib.rs`,
    /// and `README.md`.
    fn listed_agent() -> (AppState, AgentId) {
        let agent = AgentId::new(1);
        let mut state = crate::state::state_with(vec![crate::state::summary(1, AgentStatus::Idle)]);
        state.machines.this.connection = crate::state::ConnectionState::Connected;
        state.ui.selected = Some(agent);
        state.agents.get_mut(&agent).unwrap().tree.listings = [
            listing(
                "/work/project-1",
                vec![
                    entry("src", EntryKind::Folder, GitIgnore::Kept),
                    entry("README.md", EntryKind::File, GitIgnore::Kept),
                ],
            ),
            listing(
                "/work/project-1/src",
                vec![entry("lib.rs", EntryKind::File, GitIgnore::Kept)],
            ),
        ]
        .into_iter()
        .collect();
        (state, agent)
    }

    fn highlighted(state: &AppState, agent: AgentId) -> Option<&Path> {
        state.agents[&agent].tree.highlighted.as_deref()
    }

    #[test]
    fn the_arrow_keys_move_open_and_close_folders() {
        let (mut state, agent) = listed_agent();
        let src = Path::new("/work/project-1/src");

        move_highlight(&mut state, agent, Direction::Down);
        assert_eq!(highlighted(&state, agent), Some(src));

        step_sideways(&mut state, agent, Sideways::Right);
        assert!(state.agents[&agent].tree.expanded.contains(src));
        step_sideways(&mut state, agent, Sideways::Right);
        assert_eq!(
            highlighted(&state, agent),
            Some(Path::new("/work/project-1/src/lib.rs"))
        );

        step_sideways(&mut state, agent, Sideways::Left);
        assert_eq!(highlighted(&state, agent), Some(src));
        step_sideways(&mut state, agent, Sideways::Left);
        assert!(state.agents[&agent].tree.expanded.is_empty());

        move_highlight(&mut state, agent, Direction::Down);
        move_highlight(&mut state, agent, Direction::Down);
        assert_eq!(
            highlighted(&state, agent),
            Some(Path::new("/work/project-1/README.md"))
        );
    }

    #[test]
    fn folders_start_open_outside_the_all_view_and_close_on_a_click_or_a_key() {
        let (mut state, agent) = changed_agent();
        let src = Path::new("/work/project-1/src");
        let lines = |state: &AppState| shown(&shown_rows(state, agent));
        let opened = lines(&state);

        highlight(&mut state, agent, src);
        step_sideways(&mut state, agent, Sideways::Left);
        let closed_by_key = lines(&state);
        click(&mut state, agent, src);
        let opened_by_click = lines(&state);

        assert_eq!(opened, ["▾ src", "  · lib.rs"]);
        assert_eq!(closed_by_key, ["▸ src"]);
        assert_eq!(opened_by_click, opened);
        assert!(state.agents[&agent].tree.expanded.is_empty());
        assert_eq!(highlighted(&state, agent), Some(src));
    }

    #[test]
    fn close_every_folder_closes_the_open_folders_of_every_view() {
        let (mut state, agent) = changed_agent();
        let src = Path::new("/work/project-1/src");
        set_filter(&mut state, agent, TreeFilter::All);
        click(&mut state, agent, src);
        set_filter(&mut state, agent, TreeFilter::Changed);

        collapse_all(&mut state, agent);
        let changed = shown(&shown_rows(&state, agent));
        set_filter(&mut state, agent, TreeFilter::All);
        let all = shown(&shown_rows(&state, agent));

        assert_eq!(changed, ["▸ src"]);
        assert_eq!(all, ["▸ src", "· README.md"]);
    }

    #[test]
    fn showing_the_file_views_file_opens_its_closed_folders() {
        let (mut state, agent) = changed_agent();
        collapse_all(&mut state, agent);
        actions::open_file(
            &mut state,
            agent,
            PathBuf::from("/work/project-1/src/lib.rs"),
            None,
        );

        reveal(&mut state, agent);

        assert_eq!(shown(&shown_rows(&state, agent)), ["▾ src", "  · lib.rs"]);
    }

    #[test]
    fn close_every_folder_under_all_closes_the_folders_of_changed_too() {
        let (mut state, agent) = changed_agent();
        set_filter(&mut state, agent, TreeFilter::All);
        let before = collapsible(&state, agent);

        collapse_all(&mut state, agent);
        let after = collapsible(&state, agent);
        set_filter(&mut state, agent, TreeFilter::Changed);

        assert!(before);
        assert!(!after);
        assert_eq!(shown(&shown_rows(&state, agent)), ["▸ src"]);
    }

    #[test]
    fn a_click_opens_the_clicked_copy_of_a_folder_that_shows_twice() {
        let (mut state, agent) = listed_agent();
        let work = PathBuf::from("/work");
        let feature = PathBuf::from("/work/project-1_feature");
        let agent_entry = state.agents.get_mut(&agent).unwrap();
        // The file right in `project-1_feature` keeps its inner copy from sharing a row with `src`.
        agent_entry.details = Loadable::Loaded(AgentDetails {
            changed_files: [
                "/work/notes.txt",
                "/work/project-1_feature/Cargo.toml",
                "/work/project-1_feature/src/x.rs",
            ]
            .into_iter()
            .map(|path| ChangedFile {
                path: PathBuf::from(path),
                lines_added: 1,
                lines_removed: 0,
            })
            .collect(),
            ..AgentDetails::default()
        });
        agent_entry.tree.expanded.insert(work.clone());
        agent_entry.tree.listings.insert(
            work,
            Listing {
                entries: Loadable::Failed("outside every checkout".to_owned()),
                stale: false,
                reading: false,
                gone: None,
            },
        );
        let (path, listing) = listing(
            "/work/project-1_feature",
            vec![entry("src", EntryKind::Folder, GitIgnore::Kept)],
        );
        state
            .agents
            .get_mut(&agent)
            .unwrap()
            .tree
            .listings
            .insert(path, listing);
        let top = shown_rows(&state, agent)
            .into_iter()
            .rfind(|row| row.path == feature)
            .unwrap();

        activate(&mut state, agent, &top);

        let lines = shown(&shown_rows(&state, agent));
        assert!(
            lines.contains(&"  ▾ project-1_feature".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"▾ project-1_feature/".to_owned()),
            "{lines:?}"
        );
    }

    fn collapsible(state: &AppState, agent: AgentId) -> bool {
        let entry = &state.agents[&agent];
        can_collapse(&entry.tree, &entry.summary.folder, &footprint(entry, None))
    }

    /// A click on the first row of `path`, as the pane makes it.
    fn click(state: &mut AppState, agent: AgentId, path: &Path) {
        let row = shown_rows(state, agent)
            .into_iter()
            .find(|row| row.path == path)
            .unwrap();
        activate(state, agent, &row);
    }

    #[test]
    fn a_shared_row_closes_and_opens_again_by_its_last_folder() {
        let (mut state, agent) = changed_agent();
        state.agents.get_mut(&agent).unwrap().details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: PathBuf::from("/work/project-1/src/ui/view.rs"),
                lines_added: 2,
                lines_removed: 0,
            }],
            ..AgentDetails::default()
        });
        let ui = Path::new("/work/project-1/src/ui");

        collapse_all(&mut state, agent);
        let closed = shown(&shown_rows(&state, agent));
        actions::open_file(
            &mut state,
            agent,
            PathBuf::from("/work/project-1/src/ui/view.rs"),
            None,
        );
        reveal(&mut state, agent);
        let revealed = shown(&shown_rows(&state, agent));

        assert_eq!(closed, ["▸ src/ui"]);
        assert_eq!(revealed, ["▾ src/ui", "  · view.rs"]);
        assert!(!state.agents[&agent].tree.closed.contains(ui));
    }

    /// `listed_agent` on "Changed", having changed `src/lib.rs`.
    fn changed_agent() -> (AppState, AgentId) {
        let (mut state, agent) = listed_agent();
        state.agents.get_mut(&agent).unwrap().details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: PathBuf::from("/work/project-1/src/lib.rs"),
                lines_added: 2,
                lines_removed: 0,
            }],
            ..AgentDetails::default()
        });
        set_filter(&mut state, agent, TreeFilter::Changed);
        (state, agent)
    }

    #[test]
    fn changed_lists_every_uncommitted_file_with_gits_counts() {
        use cockpit_protocol::{
            BranchState, CheckoutState, FileStatus, GitStatus, LineCounts as GitCounts,
        };

        let (mut state, agent) = changed_agent();
        let file = |path: &str, change, counts| FileStatus {
            path: PathBuf::from(path),
            change,
            counts,
        };
        let status = GitStatus {
            checkouts: vec![CheckoutState {
                root: PathBuf::from("/work/project-1"),
                remote: None,
                kind: CheckoutKind::MainWorktree,
                branch: BranchState {
                    name: Some("main".to_owned()),
                    head: None,
                    upstream: None,
                },
                files: vec![
                    file(
                        "/work/project-1/src/lib.rs",
                        FileChange::Modified,
                        Some(GitCounts {
                            added: 3,
                            removed: 1,
                        }),
                    ),
                    file("/work/project-1/logo.png", FileChange::Untracked, None),
                ],
                completeness: Completeness::Complete,
            }],
        };
        let entry = state.agents.get_mut(&agent).unwrap();
        let before = changes(entry);
        entry.git.status.value = Loadable::Loaded(status);
        let after = changes(entry);

        let counts = |added, removed| LineCounts { added, removed };
        let agent_counts: Vec<LineCounts> = before.changed.values().copied().collect();
        assert_eq!(
            agent_counts,
            [counts(2, 0)],
            "the agent's counts, without git"
        );
        assert_eq!(
            after.changed,
            BTreeMap::from([
                (PathBuf::from("/work/project-1/logo.png"), counts(0, 0)),
                (PathBuf::from("/work/project-1/src/lib.rs"), counts(3, 1)),
            ]),
            "git's counts win, and a binary file counts no line"
        );
        assert_eq!(
            after.git.get(Path::new("/work/project-1/logo.png")),
            Some(&FileChange::Untracked)
        );
        assert_eq!(
            after.totals(),
            LineTotals {
                added: 3,
                removed: 1
            }
        );
    }

    #[test]
    fn each_agent_keeps_its_pane_and_a_new_one_takes_the_settings_one() {
        let mut state = crate::state::state_with(vec![
            crate::state::summary(1, AgentStatus::Idle),
            crate::state::summary(2, AgentStatus::Idle),
        ]);
        let (first, second) = (AgentId::new(1), AgentId::new(2));
        state.ui.selected = Some(first);

        toggle_pane(&mut state);
        let after_toggle = (pane_open(&state, first), pane_open(&state, second));
        state.prefs.files_pane.new_agents = PaneState::Open;
        let with_open_default = (pane_open(&state, first), pane_open(&state, second));
        state.ui.selected = Some(second);
        toggle_pane(&mut state);
        state.prefs.files_pane.new_agents = PaneState::Closed;
        let set_both = (pane_open(&state, first), pane_open(&state, second));
        forget_pane(&mut state, first);

        assert_eq!(after_toggle, (true, false));
        assert_eq!(with_open_default, (true, true));
        assert_eq!(set_both, (true, false));
        assert!(!pane_open(&state, first));
    }

    #[test]
    fn an_agent_on_another_machine_keeps_its_own_pane() {
        let devbox = MachineId::new(1);
        let mut state = crate::state::state_with(vec![crate::state::summary(1, AgentStatus::Idle)]);
        state.machines.others.insert(
            devbox,
            crate::state::MachineState::new(crate::daemon_label::DaemonLabel::ssh(
                "devbox".to_owned(),
            )),
        );
        let mut remote = crate::state::summary(1, AgentStatus::Idle);
        remote.id = devbox.app_agent(AgentId::new(1));
        state
            .agents
            .insert(remote.id, AgentEntry::new(remote.clone()));
        state.ui.selected = Some(remote.id);

        toggle_pane(&mut state);

        assert!(pane_open(&state, remote.id));
        assert!(!pane_open(&state, AgentId::new(1)));
        assert_eq!(
            state.prefs.files_pane.agent("devbox", AgentId::new(1)),
            PaneState::Open
        );
    }

    #[test]
    fn a_switch_to_an_agent_whose_pane_is_closed_lists_nothing_for_it() {
        let (mut state, agent) = listed_agent();
        let other = AgentId::new(2);
        state.agents.insert(
            other,
            AgentEntry::new(crate::state::summary(2, AgentStatus::Idle)),
        );
        toggle_pane(&mut state);
        state.ui.selected = Some(other);

        let listings: Vec<AgentId> = crate::state::needed_requests(&mut state)
            .into_iter()
            .filter_map(|outgoing| match outgoing.purpose {
                crate::state::Purpose::TreeListing { agent, .. } => Some(agent),
                _ => None,
            })
            .collect();

        assert!(pane_open(&state, agent));
        assert!(listings.is_empty());
    }

    #[test]
    fn the_files_chip_opens_the_pane_on_the_changes_then_closes_it() {
        let (mut state, agent) = listed_agent();

        toggle_from_changes(&mut state, agent);
        assert!(pane_open(&state, agent));
        assert_eq!(state.agents[&agent].tree.filter, TreeFilter::All);

        toggle_from_changes(&mut state, agent);
        assert!(!pane_open(&state, agent));

        state.agents.get_mut(&agent).unwrap().details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: PathBuf::from("/work/project-1/README.md"),
                lines_added: 1,
                lines_removed: 1,
            }],
            ..AgentDetails::default()
        });
        toggle_from_changes(&mut state, agent);
        assert_eq!(state.agents[&agent].tree.filter, TreeFilter::Changed);
    }

    #[test]
    fn a_mention_adds_the_path_from_the_agents_folder_after_the_text() {
        let (mut state, agent) = listed_agent();
        let mut drafts = Drafts::default();
        drafts.draft_mut(agent).text = "Look at".to_owned();

        mention(
            &mut state,
            &mut drafts,
            agent,
            Path::new("/work/project-1/src/lib.rs"),
        );
        mention(&mut state, &mut drafts, agent, Path::new("/etc/hosts"));

        assert_eq!(
            drafts.draft(agent).map(|draft| draft.text.as_str()),
            Some("Look at src/lib.rs /etc/hosts ")
        );
    }

    #[test]
    fn the_open_pane_lists_its_folders_and_lists_them_again_when_stale() {
        let (mut state, agent) = listed_agent();
        let tree_listings = |state: &mut AppState| -> Vec<PathBuf> {
            crate::state::needed_requests(state)
                .into_iter()
                .filter_map(|outgoing| match outgoing.purpose {
                    crate::state::Purpose::TreeListing { folder, .. } => Some(folder),
                    _ => None,
                })
                .collect()
        };
        state.agents.get_mut(&agent).unwrap().tree.listings.clear();

        assert!(tree_listings(&mut state).is_empty());

        toggle_pane(&mut state);
        assert_eq!(
            tree_listings(&mut state),
            [PathBuf::from("/work/project-1")]
        );
        assert!(tree_listings(&mut state).is_empty());

        let tree = &mut state.agents.get_mut(&agent).unwrap().tree;
        tree.listings = listed_agent().0.agents[&agent].tree.listings.clone();
        tree.expanded.insert(PathBuf::from("/work/project-1/src"));
        refresh(tree);
        assert_eq!(
            tree_listings(&mut state),
            [
                PathBuf::from("/work/project-1"),
                PathBuf::from("/work/project-1/src")
            ]
        );
        assert!(matches!(
            state.agents[&agent].tree.listings[Path::new("/work/project-1")].entries,
            Loadable::Loaded(_)
        ));
    }

    #[test]
    fn a_folder_lists_once_at_a_time_and_a_refresh_meanwhile_lists_it_after() {
        let (mut state, agent) = listed_agent();
        let root = PathBuf::from("/work/project-1");
        let tree_listings = |state: &mut AppState| -> Vec<PathBuf> {
            crate::state::needed_requests(state)
                .into_iter()
                .filter_map(|outgoing| match outgoing.purpose {
                    crate::state::Purpose::TreeListing { folder, .. } => Some(folder),
                    _ => None,
                })
                .collect()
        };
        state.agents.get_mut(&agent).unwrap().tree.listings.clear();
        toggle_pane(&mut state);
        assert_eq!(tree_listings(&mut state), std::slice::from_ref(&root));

        refresh(&mut state.agents.get_mut(&agent).unwrap().tree);
        assert!(tree_listings(&mut state).is_empty());

        let answer = |state: &mut AppState| {
            crate::state::apply_response(
                state,
                &mut Drafts::default(),
                Method::AgentListDir,
                crate::state::Purpose::TreeListing {
                    agent,
                    folder: root.clone(),
                },
                Ok(ResponseBody::Directory(DirListing {
                    path: root.clone(),
                    entries: Vec::new(),
                    completeness: Completeness::Complete,
                })),
                crate::machine::MachineId::THIS,
            );
        };
        answer(&mut state);
        assert_eq!(tree_listings(&mut state), std::slice::from_ref(&root));
        refresh(&mut state.agents.get_mut(&agent).unwrap().tree);
        assert!(tree_listings(&mut state).is_empty());
        answer(&mut state);
        assert_eq!(tree_listings(&mut state), std::slice::from_ref(&root));
    }

    #[test]
    fn a_folder_that_does_not_list_shows_the_files_the_agent_touched_there() {
        let mut tree = tree();
        let outside = PathBuf::from("/etc");
        tree.expanded.insert(outside.clone());
        tree.listings.insert(
            outside,
            Listing {
                entries: Loadable::Failed(
                    "cannot read the folder: Permission denied (os error 13)".to_owned(),
                ),
                stale: false,
                reading: false,
                gone: None,
            },
        );
        let footprint = Footprint {
            changed: [changed("/etc/calc/limits.toml", 1, 0)]
                .into_iter()
                .collect(),
            read: [PathBuf::from("/etc/calc/config.toml")]
                .into_iter()
                .collect(),
            git: BTreeMap::new(),
        };

        let rows = rows(&tree, Path::new(FOLDER), &footprint);

        assert_eq!(
            shown(&rows),
            [
                "▸ src",
                "▸ target",
                "· README.md",
                "# Also worked in",
                "▾ etc/",
                "  ▾ calc",
                "    · config.toml",
                "    · limits.toml",
            ]
        );
    }

    /// The daemon lists any folder, but one that is no checkout's top folder, such as `/etc` or
    /// a home folder, would bury the few files the agent touched there.
    #[test]
    fn a_folder_outside_every_checkout_shows_only_the_files_the_agent_touched_there() {
        let mut tree = tree();
        let footprint = Footprint {
            changed: [changed("/etc/calc/limits.toml", 1, 0)]
                .into_iter()
                .collect(),
            read: [PathBuf::from("/work/project-1_feature/src/x.rs")]
                .into_iter()
                .collect(),
            git: BTreeMap::new(),
        };
        for (root, entries) in [
            (
                "/etc",
                vec![
                    entry("calc", EntryKind::Folder, GitIgnore::Kept),
                    entry("hosts", EntryKind::File, GitIgnore::Kept),
                ],
            ),
            (
                "/work/project-1_feature",
                vec![
                    entry(".git", EntryKind::Folder, GitIgnore::Ignored),
                    entry("src", EntryKind::Folder, GitIgnore::Kept),
                    entry("Cargo.toml", EntryKind::File, GitIgnore::Kept),
                ],
            ),
        ] {
            let (path, listing) = listing(root, entries);
            tree.expanded.insert(path.clone());
            tree.listings.insert(path, listing);
        }

        let rows = rows(&tree, Path::new(FOLDER), &footprint);

        assert_eq!(
            shown(&rows),
            [
                "▸ src",
                "▸ target",
                "· README.md",
                "# Also worked in",
                "▾ etc/",
                "  ▾ calc",
                "    · limits.toml",
                "▾ project-1_feature/",
                "  ▸ .git",
                "  ▸ src",
                "  · Cargo.toml",
            ]
        );
    }

    /// `listed_agent`, having changed `/work/other/notes/a.md`, outside its folder.
    fn agent_beside_other() -> (AppState, AgentId) {
        let (mut state, agent) = listed_agent();
        state.agents.get_mut(&agent).unwrap().details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: PathBuf::from("/work/other/notes/a.md"),
                lines_added: 1,
                lines_removed: 0,
            }],
            ..AgentDetails::default()
        });
        (state, agent)
    }

    /// A finished `Read` of `path`.
    fn read_item(id: u32, path: &str) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::ToolCall(ToolCall {
                tool: READ_TOOL.to_owned(),
                summary: path.to_owned(),
                input: json!({ "file_path": path }),
                status: ToolCallStatus::Done,
                output: None,
                exit_code: None,
                edit: None,
            }),
        }
    }

    /// A finished Bash call, which lists the shown folders again.
    fn command_item(id: u32) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body: ItemBody::ToolCall(ToolCall {
                tool: BASH_TOOL.to_owned(),
                summary: "cargo fmt".to_owned(),
                input: json!({ "command": "cargo fmt" }),
                status: ToolCallStatus::Done,
                output: None,
                exit_code: Some(0),
                edit: None,
            }),
        }
    }

    /// The folders the files pane asks the daemon to list now.
    fn tree_listings(state: &mut AppState) -> Vec<PathBuf> {
        crate::state::needed_requests(state)
            .into_iter()
            .filter_map(|outgoing| match outgoing.purpose {
                Purpose::TreeListing { folder, .. } => Some(folder),
                _ => None,
            })
            .collect()
    }

    fn list_failed(state: &mut AppState, agent: AgentId, folder: &Path, code: ErrorCode) {
        crate::state::apply_response(
            state,
            &mut Drafts::default(),
            Method::AgentListDir,
            Purpose::TreeListing {
                agent,
                folder: folder.to_owned(),
            },
            Err(RequestError::Failed(ErrorBody::new(
                code,
                format!("{} cannot be listed", folder.display()),
            ))),
            MachineId::THIS,
        );
    }

    /// The names of the folders under "Also worked in".
    fn outside_rows(state: &AppState, agent: AgentId) -> Vec<String> {
        shown_rows(state, agent)
            .into_iter()
            .skip_while(|row| row.kind != RowKind::Section)
            .filter(|row| row.depth == 0 && row.kind.is_entry())
            .map(|row| row.name)
            .collect()
    }

    #[test]
    fn clearing_a_folder_above_the_agents_keeps_the_agents_own_files() {
        let (mut state, agent) = listed_agent();
        let own = PathBuf::from("/work/project-1/src/lib.rs");
        let entry = state.agents.get_mut(&agent).unwrap();
        entry.details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: own.clone(),
                lines_added: 2,
                lines_removed: 1,
            }],
            ..AgentDetails::default()
        });
        entry.transcript.append(read_item(1, "/work/notes.md"));

        let before = outside_rows(&state, agent);
        clear_outside(&mut state, agent);
        let kept = footprint_of(&state, &state.agents[&agent]);
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read_item(2, "/work/project-1/src/lib.rs"));
        let after_own_read = outside_rows(&state, agent);
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read_item(3, "/work/other/a.md"));
        let after_other_read = outside_rows(&state, agent);

        assert_eq!(before, ["work/"]);
        assert!(kept.changed.contains_key(&own), "{kept:?}");
        assert!(kept.read.is_empty(), "{kept:?}");
        assert!(
            after_own_read.is_empty(),
            "a read in the agent's own folder brings back no folder: {after_own_read:?}"
        );
        assert_eq!(
            after_other_read,
            ["other/"],
            "a folder under the cleared one shows on its own"
        );
    }

    #[test]
    fn on_windows_a_cleared_folder_matches_whatever_its_case_and_separators() {
        let (mut state, agent) = listed_agent();
        let entry = state.agents.get_mut(&agent).unwrap();
        entry.summary.folder = PathBuf::from(r"C:\work\app");
        entry.transcript.append(read_item(1, r"C:\Work\Other\a.md"));
        let machine = state.daemon_machine(agent).unwrap().to_owned();
        state.prefs.files_pane.set_cleared(
            &machine,
            MachineId::daemon_agent(agent),
            ClearedFolders {
                folders: BTreeSet::from([PathBuf::from("c:/work/other")]),
                after: Some(ItemId::new(1)),
            },
        );

        let cleared_rows = outside_rows(&state, agent);
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read_item(2, "c:/WORK/APP/src/lib.rs"));
        let after_own_read = outside_rows(&state, agent);
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read_item(3, r"c:\work\OTHER\b.md"));
        let back = outside_rows(&state, agent);

        assert!(cleared_rows.is_empty(), "{cleared_rows:?}");
        assert!(after_own_read.is_empty(), "{after_own_read:?}");
        assert!(!back.is_empty(), "a read in it brings it back");
    }

    #[test]
    fn clearing_again_keeps_a_folder_cleared_before_hidden() {
        let (mut state, agent) = listed_agent();
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read_item(1, "/work/other/a.md"));
        clear_outside(&mut state, agent);
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read_item(2, "/opt/tools/config.toml"));
        // The oldest item leaves the app's copy, as in a long chat.
        transcript.trim(1);

        let listed = outside_rows(&state, agent);
        clear_outside(&mut state, agent);
        // Scrolling up loads it again.
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read_item(1, "/work/other/a.md"));
        let rows = outside_rows(&state, agent);

        assert_eq!(listed, ["opt/"]);
        assert!(rows.is_empty(), "{rows:?}");
    }

    #[test]
    fn a_closed_folder_that_cannot_be_listed_is_not_listed_again_on_each_step() {
        let (mut state, agent) = agent_beside_other();
        let other = PathBuf::from("/work/other");
        let command = command_item(1);
        toggle_pane(&mut state);

        let asked = tree_listings(&mut state);
        list_failed(&mut state, agent, &other, ErrorCode::FileUnreadable);
        item_finished(&mut state, agent, &command);
        let asked_after_step = tree_listings(&mut state);

        assert!(asked.contains(&other), "{asked:?}");
        assert!(
            !asked_after_step.contains(&other),
            "listed once: {asked_after_step:?}"
        );
    }

    #[test]
    fn a_folder_that_no_longer_exists_leaves_also_worked_in() {
        let (mut state, agent) = agent_beside_other();
        let other = PathBuf::from("/work/other");
        toggle_pane(&mut state);
        let rows_before = shown(&shown_rows(&state, agent));

        let asked = tree_listings(&mut state);
        let asked_again = tree_listings(&mut state);
        list_failed(&mut state, agent, &other, ErrorCode::NotFound);

        assert!(
            rows_before.contains(&format!("# {OUTSIDE_SECTION}")),
            "{rows_before:?}"
        );
        assert_eq!(asked, [other], "its listing tells whether it exists");
        assert!(asked_again.is_empty(), "it lists once");
        let rows = shown(&shown_rows(&state, agent));
        assert!(
            !rows.iter().any(|row| row.contains(OUTSIDE_SECTION)),
            "{rows:?}"
        );
        assert!(
            footprint_of(&state, &state.agents[&agent])
                .changed
                .is_empty()
        );
    }

    #[test]
    fn a_gone_folder_is_asked_about_again_on_a_refresh_or_once_the_agent_works_in_it() {
        let (mut state, agent) = agent_beside_other();
        let other = PathBuf::from("/work/other");
        let command = command_item(1);
        toggle_pane(&mut state);
        tree_listings(&mut state);
        list_failed(&mut state, agent, &other, ErrorCode::NotFound);

        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(command.clone());
        item_finished(&mut state, agent, &command);
        let after_step = tree_listings(&mut state);
        refresh(&mut state.agents.get_mut(&agent).unwrap().tree);
        let after_refresh = tree_listings(&mut state);
        let hidden_meanwhile = outside_rows(&state, agent);
        list_failed(&mut state, agent, &other, ErrorCode::NotFound);
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read_item(2, "/work/other/notes/a.md"));
        let worked_in = outside_rows(&state, agent);
        let asked_again = tree_listings(&mut state);

        assert!(!after_step.contains(&other), "{after_step:?}");
        assert_eq!(after_refresh, std::slice::from_ref(&other));
        assert!(hidden_meanwhile.is_empty(), "{hidden_meanwhile:?}");
        assert_eq!(worked_in, ["other/"]);
        assert_eq!(asked_again, [other]);
    }

    #[test]
    fn an_open_folder_the_daemon_said_was_gone_lists_again_after_a_step() {
        let (mut state, agent) = listed_agent();
        let out = PathBuf::from("/work/project-1/out");
        let entry = state.agents.get_mut(&agent).unwrap();
        entry.tree.expanded.insert(out.clone());
        toggle_pane(&mut state);
        let asked = tree_listings(&mut state);
        list_failed(&mut state, agent, &out, ErrorCode::NotFound);

        // A build makes the folder again.
        item_finished(&mut state, agent, &command_item(1));
        let asked_after_step = tree_listings(&mut state);

        assert!(asked.contains(&out), "{asked:?}");
        assert!(asked_after_step.contains(&out), "{asked_after_step:?}");
    }

    #[test]
    fn a_gone_folder_that_a_write_makes_again_lists_once_the_write_ends() {
        let (mut state, agent) = agent_beside_other();
        let other = PathBuf::from("/work/other");
        let write = |status| TranscriptItem {
            id: ItemId::new(1),
            parent: None,
            body: ItemBody::ToolCall(ToolCall {
                tool: "Write".to_owned(),
                summary: "/work/other/notes/b.md".to_owned(),
                input: json!({ "file_path": "/work/other/notes/b.md", "content": "b" }),
                status,
                output: None,
                exit_code: None,
                edit: None,
            }),
        };
        toggle_pane(&mut state);
        tree_listings(&mut state);
        list_failed(&mut state, agent, &other, ErrorCode::NotFound);

        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(write(ToolCallStatus::Running));
        // The write has not run yet, as while it waits for approval.
        let while_running = tree_listings(&mut state);
        for folder in &while_running {
            list_failed(&mut state, agent, folder, ErrorCode::NotFound);
        }
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.change(write(ToolCallStatus::Done));
        item_finished(&mut state, agent, &write(ToolCallStatus::Done));
        let after_write = tree_listings(&mut state);

        assert_eq!(after_write, [other]);
        assert!(while_running.is_empty(), "{while_running:?}");
    }

    #[test]
    fn a_gone_folder_the_agent_worked_in_that_is_still_gone_hides_and_is_not_asked_again() {
        let (mut state, agent) = agent_beside_other();
        let other = PathBuf::from("/work/other");
        toggle_pane(&mut state);
        tree_listings(&mut state);
        list_failed(&mut state, agent, &other, ErrorCode::NotFound);
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read_item(1, "/work/other/notes/a.md"));
        let asked = tree_listings(&mut state);

        list_failed(&mut state, agent, &other, ErrorCode::NotFound);
        let rows = outside_rows(&state, agent);
        let asked_again = tree_listings(&mut state);
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read_item(2, "/work/project-1/src/lib.rs"));
        let asked_after_more_work = tree_listings(&mut state);

        assert_eq!(asked, std::slice::from_ref(&other));
        assert!(rows.is_empty(), "{rows:?}");
        assert!(asked_again.is_empty(), "{asked_again:?}");
        assert!(
            asked_after_more_work.is_empty(),
            "{asked_after_more_work:?}"
        );
    }

    #[test]
    fn a_lost_connection_keeps_a_folder_gone_and_another_failure_shows_it_again() {
        let (mut state, agent) = agent_beside_other();
        let other = PathBuf::from("/work/other");
        toggle_pane(&mut state);
        tree_listings(&mut state);
        list_failed(&mut state, agent, &other, ErrorCode::NotFound);
        refresh(&mut state.agents.get_mut(&agent).unwrap().tree);
        tree_listings(&mut state);

        crate::state::apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentListDir,
            Purpose::TreeListing {
                agent,
                folder: other.clone(),
            },
            Err(RequestError::Disconnected),
            MachineId::THIS,
        );
        let after_lost_connection = outside_rows(&state, agent);
        let asked_after_lost_connection = tree_listings(&mut state);
        list_failed(&mut state, agent, &other, ErrorCode::FileUnreadable);
        let after_other_failure = outside_rows(&state, agent);

        assert!(
            after_lost_connection.is_empty(),
            "{after_lost_connection:?}"
        );
        assert_eq!(asked_after_lost_connection, [other]);
        assert_eq!(after_other_failure, ["other/"]);
    }

    #[test]
    fn on_windows_a_gone_folder_comes_back_whatever_the_case_of_the_path_worked_in() {
        let (mut state, agent) = listed_agent();
        let other = PathBuf::from(r"C:\Work\Other");
        let entry = state.agents.get_mut(&agent).unwrap();
        entry.summary.folder = PathBuf::from(r"C:\work\app");
        entry.details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: PathBuf::from(r"C:\Work\Other\a.md"),
                lines_added: 1,
                lines_removed: 0,
            }],
            ..AgentDetails::default()
        });
        entry.tree.listings.clear();
        toggle_pane(&mut state);
        let asked = tree_listings(&mut state);
        list_failed(&mut state, agent, &other, ErrorCode::NotFound);
        let gone_rows = outside_rows(&state, agent);
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(TranscriptItem {
            id: ItemId::new(1),
            parent: None,
            body: ItemBody::ToolCall(ToolCall {
                tool: "Write".to_owned(),
                summary: "b.md".to_owned(),
                input: json!({ "file_path": "c:/work/OTHER/b.md", "content": "b" }),
                status: ToolCallStatus::Done,
                output: None,
                exit_code: None,
                edit: None,
            }),
        });
        let back = outside_rows(&state, agent);
        let asked_again = tree_listings(&mut state);

        assert!(asked.contains(&other), "{asked:?}");
        assert!(gone_rows.is_empty(), "{gone_rows:?}");
        assert_eq!(back, [r"Other\"]);
        assert_eq!(asked_again, [other]);
    }

    #[test]
    fn a_cleared_folder_shows_again_once_the_agent_works_in_it() {
        let (mut state, agent) = agent_beside_other();
        let read = read_item;
        let has_section = |state: &AppState| {
            shown(&shown_rows(state, agent))
                .iter()
                .any(|row| row.contains(OUTSIDE_SECTION))
        };
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read(1, "/work/other/notes/a.md"));

        let before = has_section(&state);
        clear_outside(&mut state, agent);
        let after_clear = has_section(&state);
        let saved = cleared(&state, agent).cloned();
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read(2, "/opt/tools/config.toml"));
        let elsewhere = has_section(&state);
        let elsewhere_rows = shown(&shown_rows(&state, agent));
        let transcript = &mut state.agents.get_mut(&agent).unwrap().transcript;
        transcript.append(read(3, "/work/other/notes/b.md"));
        let back = has_section(&state);

        assert!(before);
        assert!(!after_clear);
        assert_eq!(
            saved,
            Some(ClearedFolders {
                folders: BTreeSet::from([PathBuf::from("/work/other")]),
                after: Some(ItemId::new(1)),
            }),
            "kept in the preferences"
        );
        assert!(elsewhere, "a read elsewhere lists its own folder");
        assert!(
            !elsewhere_rows.iter().any(|row| row.contains("other")),
            "{elsewhere_rows:?}"
        );
        assert!(back, "a read in the cleared folder brings it back");
        let rows = shown(&shown_rows(&state, agent));
        assert!(rows.iter().any(|row| row.contains("other")), "{rows:?}");
    }

    #[test]
    fn the_tree_shows_the_agents_folder_and_the_files_it_touched_outside() {
        let (mut state, agent) = agent_beside_other();
        let can = |state: &AppState, path: &str| can_show(state, agent, Path::new(path));

        let inside = can(&state, "src/lib.rs");
        let inside_unlisted = can(&state, "/work/project-1/docs/guide.md");
        let touched = can(&state, "/work/other/notes/a.md");
        let beside_touched = can(&state, "/work/other/notes/b.md");
        let elsewhere = can(&state, "/opt/tools/config.toml");
        state.agents.get_mut(&agent).unwrap().tree.listings.insert(
            PathBuf::from("/work/other"),
            Listing {
                entries: Loadable::Failed("outside every checkout".to_owned()),
                stale: false,
                reading: false,
                gone: None,
            },
        );

        assert!(inside);
        assert!(inside_unlisted);
        assert!(touched);
        assert!(beside_touched, "the daemon may list the folder");
        assert!(!elsewhere);
        assert!(can(&state, "/work/other/notes/a.md"));
        assert!(
            !can(&state, "/work/other/notes/b.md"),
            "the daemon refused to list the folder"
        );
    }

    #[test]
    fn showing_a_file_the_tree_never_shows_changes_nothing() {
        let (mut state, agent) = changed_agent();
        actions::open_file(
            &mut state,
            agent,
            PathBuf::from("/opt/tools/config.toml"),
            None,
        );

        reveal(&mut state, agent);

        let tree = &state.agents[&agent].tree;
        assert_eq!(tree.filter, TreeFilter::Changed);
        assert_eq!(tree.highlighted, None);
        assert_eq!(tree.reveal, None);
    }

    #[test]
    fn the_pane_scrolls_to_a_revealed_file_once_its_folder_is_listed() {
        let (mut state, agent) = listed_agent();
        let src = PathBuf::from("/work/project-1/src");
        let src_listing = state
            .agents
            .get_mut(&agent)
            .unwrap()
            .tree
            .listings
            .remove(&src)
            .unwrap();
        let due =
            |state: &AppState| reveal_due(&state.agents[&agent].tree, &shown_rows(state, agent));
        actions::open_file(&mut state, agent, PathBuf::from("src/lib.rs"), None);

        reveal(&mut state, agent);
        let before_listing = due(&state);
        state
            .agents
            .get_mut(&agent)
            .unwrap()
            .tree
            .listings
            .insert(src, src_listing);
        let listed = due(&state);
        revealed(&mut state, agent);
        let after_scroll = due(&state);

        assert!(!before_listing, "the row waits for its folder's listing");
        assert!(listed);
        assert!(!after_scroll, "the pane scrolls once");
    }

    #[test]
    fn the_pane_does_not_scroll_to_a_revealed_row_the_highlight_left() {
        let (mut state, agent) = listed_agent();
        actions::open_file(&mut state, agent, PathBuf::from("src/lib.rs"), None);

        reveal(&mut state, agent);
        highlight(&mut state, agent, Path::new("/work/project-1/README.md"));

        assert!(!reveal_due(
            &state.agents[&agent].tree,
            &shown_rows(&state, agent)
        ));
    }

    #[test]
    fn a_rows_menu_opens_a_changed_file_on_its_diff_and_copies_a_relative_path_inside() {
        let (state, agent) = changed_agent();
        let rows = shown_rows(&state, agent);
        let row = |name: &str| rows.iter().find(|row| row.name == name).unwrap();

        let in_folder = MenuPlace {
            in_folder: true,
            ..MenuPlace::default()
        };
        let changed_file = menu_items(row("lib.rs"), in_folder);
        let folder_outside = menu_items(row("src"), MenuPlace::default());
        let in_git = menu_items(
            row("lib.rs"),
            MenuPlace {
                in_folder: true,
                history: true,
                on_github: true,
                local: true,
            },
        );

        assert_eq!(
            changed_file,
            [
                TreeMenuItem::Open,
                TreeMenuItem::OpenDiff,
                TreeMenuItem::Mention,
                TreeMenuItem::CopyPath,
                TreeMenuItem::CopyRelativePath,
            ]
        );
        assert_eq!(
            folder_outside,
            [
                TreeMenuItem::Open,
                TreeMenuItem::Mention,
                TreeMenuItem::CopyPath,
            ]
        );
        assert_eq!(
            in_git,
            [
                TreeMenuItem::Open,
                TreeMenuItem::OpenDiff,
                TreeMenuItem::ShowHistory,
                TreeMenuItem::Mention,
                TreeMenuItem::CopyPath,
                TreeMenuItem::CopyRelativePath,
                TreeMenuItem::OpenOnGitHub,
                TreeMenuItem::ShowInFileManager,
            ]
        );
        let mut letters: Vec<char> = in_git.iter().map(|item| item.key()).collect();
        letters.sort_unstable();
        letters.dedup();
        assert_eq!(letters.len(), in_git.len(), "each row has its own letter");
    }

    #[test]
    fn on_windows_a_touched_file_in_a_refused_folder_shows_whatever_its_case() {
        let folder = Path::new(r"C:\work\app");
        let footprint = Footprint {
            changed: [changed("c:/WORK/other/a.md", 1, 0)].into_iter().collect(),
            read: BTreeSet::new(),
            git: BTreeMap::new(),
        };
        let mut tree = FileTree::default();
        for root in outside_roots(&tree, folder, &footprint) {
            tree.listings.insert(
                root,
                Listing {
                    entries: Loadable::Failed("outside every checkout".to_owned()),
                    stale: false,
                    reading: false,
                    gone: None,
                },
            );
        }

        let root = root_showing(&tree, folder, &footprint, Path::new(r"C:\work\other\a.md"));

        assert!(root.is_some());
    }

    /// `listed_agent` in `C:\work\app`, on "Changed", having changed `C:\Work\Other\a.md` and
    /// `c:/work/OTHER/notes/b.md`: one folder outside its own, written in two ways.
    fn agent_beside_other_written_twice() -> (AppState, AgentId) {
        let (mut state, agent) = listed_agent();
        let entry = state.agents.get_mut(&agent).unwrap();
        entry.summary.folder = PathBuf::from(r"C:\work\app");
        entry.tree.listings.clear();
        entry.details = Loadable::Loaded(AgentDetails {
            changed_files: [r"C:\Work\Other\a.md", "c:/work/OTHER/notes/b.md"]
                .into_iter()
                .map(|path| ChangedFile {
                    path: PathBuf::from(path),
                    lines_added: 1,
                    lines_removed: 0,
                })
                .collect(),
            ..AgentDetails::default()
        });
        set_filter(&mut state, agent, TreeFilter::Changed);
        (state, agent)
    }

    #[test]
    fn on_windows_close_every_folder_closes_a_folder_written_in_two_ways() {
        let other = Path::new(r"C:\Work\Other");
        let notes = Path::new(r"C:\Work\Other\notes");
        let (mut by_hand, agent) = agent_beside_other_written_twice();
        let (mut collapsed, _) = agent_beside_other_written_twice();

        click(&mut by_hand, agent, notes);
        click(&mut by_hand, agent, other);
        collapse_all(&mut collapsed, agent);
        click(&mut collapsed, agent, other);

        assert!(!collapsible(&by_hand, agent), "every folder is closed");
        assert_eq!(
            shown(&shown_rows(&collapsed, agent)),
            ["# Also worked in", r"▾ Other\", "  ▸ notes", "  · a.md"]
        );
    }

    #[test]
    fn on_windows_showing_a_file_opens_the_folder_its_row_is_in_however_it_is_written() {
        let (mut state, agent) = agent_beside_other_written_twice();
        click(&mut state, agent, Path::new(r"C:\Work\Other"));
        // As the agent wrote it.
        actions::open_file(
            &mut state,
            agent,
            PathBuf::from("c:/work/OTHER/notes/b.md"),
            None,
        );

        reveal(&mut state, agent);

        let rows = shown_rows(&state, agent);
        assert_eq!(
            shown(&rows),
            [
                "# Also worked in",
                r"▾ Other\",
                "  ▾ notes",
                "    · b.md",
                "  · a.md",
            ]
        );
        assert_eq!(
            highlighted(&state, agent),
            Some(Path::new(r"C:\Work\Other\notes\b.md"))
        );
        assert!(reveal_due(&state.agents[&agent].tree, &rows));
    }

    #[test]
    fn on_windows_showing_a_changed_file_keeps_changed_however_its_row_writes_it() {
        let (mut state, agent) = agent_beside_other_written_twice();
        let row = PathBuf::from(r"C:\Work\Other\notes\b.md");
        actions::open_file(&mut state, agent, row.clone(), None);

        reveal(&mut state, agent);

        assert_eq!(state.agents[&agent].tree.filter, TreeFilter::Changed);
        assert_eq!(highlighted(&state, agent), Some(row.as_path()));
    }

    #[test]
    fn on_windows_a_listed_outside_folder_keeps_its_row_when_written_another_way() {
        let folder = Path::new(r"C:\work\app");
        let other = "c:/work/other";
        let mut tree = FileTree::default();
        tree.expanded.insert(PathBuf::from(other));
        tree.listings.extend([listing(
            other,
            vec![
                entry(".git", EntryKind::File, GitIgnore::Ignored),
                entry("a.md", EntryKind::File, GitIgnore::Kept),
                entry("b.md", EntryKind::File, GitIgnore::Kept),
            ],
        )]);
        // The change comes after the read, and its spelling sorts first.
        let footprint = Footprint {
            changed: [changed(r"C:\Work\Other\a.md", 1, 0)].into_iter().collect(),
            read: [PathBuf::from("c:/work/other/b.md")].into_iter().collect(),
            git: BTreeMap::new(),
        };

        let rows = rows(&tree, folder, &footprint);
        let to_check = roots_to_check(&tree, folder, &footprint);

        assert_eq!(
            shown(&rows),
            [
                "… ",
                "# Also worked in",
                r"▾ other\",
                "  · .git",
                "  · a.md",
                "  · b.md",
            ]
        );
        assert!(to_check.is_empty(), "it is listed already: {to_check:?}");
    }
}
