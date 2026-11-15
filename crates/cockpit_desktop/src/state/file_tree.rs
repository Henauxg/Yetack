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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cockpit_protocol::{
    AgentId, ChangedFile, Completeness, DirListing, EntryKind, GitIgnore, ItemBody, ToolCallStatus,
    TranscriptItem,
};
use serde_json::Value;

use super::actions::{self, Direction};
use super::app_state::{AgentEntry, AppState, Loadable};
use super::drafts::Drafts;
use super::file_edits::{self, BASH_TOOL, FILE_FIELDS, READ_TOOL};
use super::ui_state::{FileSearch, Popover, ViewMode};
use crate::machine::MachineId;
use crate::path_style::PathStyle;
use crate::prefs::{self, PaneState};

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
}

/// What the agent did to the files, by absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Footprint {
    pub changed: BTreeMap<PathBuf, LineCounts>,
    pub read: BTreeSet<PathBuf>,
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
    /// The folder holds a file the agent changed.
    pub holds_change: bool,
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

/// What `entry` changed and read, from its details and its transcript.
pub fn footprint(entry: &AgentEntry) -> Footprint {
    let changed = entry
        .details
        .loaded()
        .into_iter()
        .flat_map(|details| details.changed_files.iter())
        .map(|file| (file.path.clone(), counts(file)))
        .collect();
    Footprint {
        changed,
        read: read_files(entry.transcript.items()),
    }
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
        .chain(outside_roots(folder, footprint))
        .collect();
    let builder = Builder {
        tree,
        footprint,
        style: PathStyle::of(folder),
        roots: &roots,
    };
    let mut rows = Vec::new();
    if let Some(search) = &tree.search {
        builder.search_rows(folder, search, &mut rows);
        return rows;
    }
    match tree.filter {
        TreeFilter::All => {
            builder.listed_rows(folder, 0, false, &mut rows);
            let outside = outside_roots(folder, footprint);
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

/// The folders outside the agent's that hold a file it changed or read, by path.
fn outside_roots(folder: &Path, footprint: &Footprint) -> Vec<PathBuf> {
    let style = PathStyle::of(folder);
    let folder_text = folder.to_string_lossy();
    let touched = footprint.changed.keys().chain(&footprint.read);
    let roots: BTreeSet<PathBuf> = touched
        .filter(|path| {
            style
                .strip_prefix(&path.to_string_lossy(), &folder_text)
                .is_none()
        })
        .map(|path| outside_root(folder, path))
        .collect();
    roots.into_iter().collect()
}

/// Whether the tree shows only the files the agent touched in `root`, a folder outside the
/// agent's: its listing holds no `.git` entry, so that a folder such as `/etc` or the home
/// folder does not bury them, or the daemon did not list it. A checkout beside the agent's
/// folder, such as a worktree, lists in full.
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
    let root = outside_roots(folder, footprint)
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
    root_showing(&entry.tree, folder, &footprint(entry), Path::new(&full)).is_some()
}

struct Builder<'tree> {
    tree: &'tree FileTree,
    footprint: &'tree Footprint,
    style: PathStyle,
    /// The agent's folder and the folders outside it, each shown once at the top level.
    roots: &'tree [PathBuf],
}

impl Builder<'_> {
    fn marks(&self, path: &Path, folder: bool, ignored: bool) -> Marks {
        let text = path.to_string_lossy();
        let holds_change = folder
            && self.footprint.changed.keys().any(|changed| {
                self.style
                    .strip_prefix(&changed.to_string_lossy(), &text)
                    .is_some_and(|rest| !rest.is_empty())
            });
        Marks {
            change: self
                .footprint
                .changed
                .iter()
                .find(|(changed, _)| self.same(changed, &text))
                .map(|(_, counts)| *counts),
            read: self
                .footprint
                .read
                .iter()
                .any(|read| self.same(read, &text)),
            holds_change,
            ignored,
        }
    }

    /// Whether `path` names the file at `text`, whatever its separators, and its case on Windows.
    fn same(&self, path: &Path, text: &str) -> bool {
        self.style.same_path(&path.to_string_lossy(), text)
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
            let path = self.path_in(folder, &entry.name);
            let entry_ignored = ignored || entry.ignore == GitIgnore::Ignored;
            let is_folder = entry.kind == EntryKind::Folder;
            // A root shows open only at the top level, so that no entry shows twice.
            let root = self.roots.contains(&path);
            let open = is_folder && !root && self.tree.expanded.contains(&path);
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
                    let root = outside_root(folder, path);
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

/// Summed in `u64`, so that no count the daemon sends overflows.
pub fn line_totals(files: &[ChangedFile]) -> (u64, u64) {
    let added = files.iter().map(|file| u64::from(file.lines_added)).sum();
    let removed = files.iter().map(|file| u64::from(file.lines_removed)).sum();
    (added, removed)
}

/// The folders whose listing the tree shows: the agent's, and each open folder whose folders
/// above it are open, up to the agent's folder or a folder outside it.
pub fn shown_folders(tree: &FileTree, folder: &Path, footprint: &Footprint) -> Vec<PathBuf> {
    if tree.search.is_some() || tree.filter != TreeFilter::All {
        return Vec::new();
    }
    let style = PathStyle::of(folder);
    let outside = outside_roots(folder, footprint);
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
        let changed = entry
            .details
            .loaded()
            .is_some_and(|details| !details.changed_files.is_empty());
        entry.tree.search = None;
        entry.tree.filter = if changed {
            TreeFilter::Changed
        } else {
            TreeFilter::All
        };
    }
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
    if let Some(entry) = state.agents.get_mut(&agent) {
        let starting_open = starting_open(&entry.summary.folder, &footprint(entry));
        entry.tree.expanded.clear();
        entry.tree.closed.extend(starting_open);
    }
}

/// Whether "Close every folder" has a folder to close, in any view.
pub fn can_collapse(tree: &FileTree, folder: &Path, footprint: &Footprint) -> bool {
    !tree.expanded.is_empty()
        || starting_open(folder, footprint)
            .iter()
            .any(|folder| !tree.closed.contains(folder))
}

/// The folders that start open, by the paths their rows take: each folder outside the agent's
/// that holds a file it changed or read, and the folders between such a file and the agent's
/// folder or that folder outside it.
fn starting_open(folder: &Path, footprint: &Footprint) -> BTreeSet<PathBuf> {
    let style = PathStyle::of(folder);
    let folder_text = folder.to_string_lossy();
    let mut folders = BTreeSet::new();
    for path in footprint.changed.keys().chain(&footprint.read) {
        let text = path.to_string_lossy();
        let (root, relative) = match style.strip_prefix(&text, &folder_text) {
            Some(relative) if !relative.is_empty() => (folder.to_string_lossy(), relative),
            Some(_) | None => {
                let root = outside_root(folder, path);
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
    for listing in tree.listings.values_mut() {
        match listing.entries {
            Loadable::Loaded(_) | Loadable::Loading => listing.stale = true,
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
    let changes_files = call.edit.is_some() || call.tool == BASH_TOOL;
    if call.status == ToolCallStatus::Done
        && changes_files
        && let Some(entry) = state.agents.get_mut(&agent)
    {
        refresh(&mut entry.tree);
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
        rows(&entry.tree, &entry.summary.folder, &footprint(entry))
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
    Mention,
    CopyPath,
    CopyRelativePath,
}

impl TreeMenuItem {
    /// The letter that picks it while the menu is open.
    pub fn key(self) -> char {
        match self {
            Self::Open => 'o',
            Self::OpenDiff => 'd',
            Self::Mention => 'm',
            Self::CopyPath => 'c',
            Self::CopyRelativePath => 'e',
        }
    }
}

/// The menu's rows for `row`: a file the agent changed also opens on its diff, and an entry in the
/// agent's folder also copies its path from that folder.
pub fn menu_items(row: &TreeRow, in_folder: bool) -> Vec<TreeMenuItem> {
    let diff = row.kind == RowKind::File && row.marks.change.is_some();
    let mut items = vec![TreeMenuItem::Open];
    if diff {
        items.push(TreeMenuItem::OpenDiff);
    }
    items.extend([TreeMenuItem::Mention, TreeMenuItem::CopyPath]);
    if in_folder {
        items.push(TreeMenuItem::CopyRelativePath);
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
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let folder = entry.summary.folder.clone();
    let style = PathStyle::of(&folder);
    let full = PathBuf::from(style.resolve(&folder.to_string_lossy(), &shown.to_string_lossy()));
    let marks = footprint(entry);
    let Some(root) = root_showing(&entry.tree, &folder, &marks, &full) else {
        return;
    };
    let kept = match entry.tree.filter {
        TreeFilter::All => true,
        TreeFilter::Changed => marks.changed.contains_key(&full),
        TreeFilter::Read => marks.read.contains(&full),
    };
    if !kept {
        entry.tree.filter = TreeFilter::All;
    }
    entry.tree.search = None;
    let full_text = full.to_string_lossy().into_owned();
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
    use cockpit_protocol::{
        AgentDetails, AgentStatus, DirEntry, EntryLink, ItemId, Method, ResponseBody, ToolCall,
    };
    use serde_json::json;

    use super::*;

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
        let file = |path: &str, lines_added: u32, lines_removed: u32| ChangedFile {
            path: PathBuf::from(path),
            lines_added,
            lines_removed,
        };
        let files = [file("a.rs", u32::MAX, 1), file("b.rs", u32::MAX, u32::MAX)];

        assert_eq!(
            line_totals(&files),
            (2 * u64::from(u32::MAX), u64::from(u32::MAX) + 1)
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
        can_collapse(&entry.tree, &entry.summary.folder, &footprint(entry))
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
            },
        );
        let footprint = Footprint {
            changed: [changed("/etc/calc/limits.toml", 1, 0)]
                .into_iter()
                .collect(),
            read: [PathBuf::from("/etc/calc/config.toml")]
                .into_iter()
                .collect(),
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

        let changed_file = menu_items(row("lib.rs"), true);
        let folder_outside = menu_items(row("src"), false);

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
        let mut letters: Vec<char> = changed_file.iter().map(|item| item.key()).collect();
        letters.sort_unstable();
        letters.dedup();
        assert_eq!(
            letters.len(),
            changed_file.len(),
            "each row has its own letter"
        );
    }

    #[test]
    fn on_windows_a_touched_file_in_a_refused_folder_shows_whatever_its_case() {
        let folder = Path::new(r"C:\work\app");
        let footprint = Footprint {
            changed: [changed("c:/WORK/other/a.md", 1, 0)].into_iter().collect(),
            read: BTreeSet::new(),
        };
        let mut tree = FileTree::default();
        for root in outside_roots(folder, &footprint) {
            tree.listings.insert(
                root,
                Listing {
                    entries: Loadable::Failed("outside every checkout".to_owned()),
                    stale: false,
                    reading: false,
                },
            );
        }

        let root = root_showing(&tree, folder, &footprint, Path::new(r"C:\work\other\a.md"));

        assert!(root.is_some());
    }
}
