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
use crate::path_style::PathStyle;
use crate::prefs;

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
    /// The open folders.
    pub expanded: BTreeSet<PathBuf>,
    pub listings: BTreeMap<PathBuf, Listing>,
    pub filter: TreeFilter,
    /// The row the keys and the pointer moved to.
    pub highlighted: Option<PathBuf>,
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

impl FileTree {
    /// Whether its folders open and close: only "All" lists folders, and the others show every
    /// folder open.
    pub fn folds(&self) -> bool {
        self.search.is_none() && self.filter == TreeFilter::All
    }
}

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
                    RowKind::Folder { open }
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
            kind: RowKind::Folder { open },
            marks: self.marks(root, true, false),
        });
        if !open {
            return;
        }
        let refused = matches!(
            self.tree.listings.get(root).map(|listing| &listing.entries),
            Some(Loadable::Failed(_))
        );
        if !refused {
            self.listed_rows(root, 1, false, rows);
            return;
        }
        // The daemon lists no folder outside every checkout: the files the agent touched there
        // show instead, as under "Read".
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

    /// `paths` under their folders, all open: the agent's folder's first, then each folder
    /// outside it.
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
                    rows.push(TreeRow {
                        path: root.clone(),
                        name: root_label(&root),
                        depth: 0,
                        kind: RowKind::Folder { open: true },
                        marks: self.marks(&root, true, false),
                    });
                    self.node_rows(&root, &node, 1, rows);
                }
            }
        }
    }

    fn node_rows(&self, folder: &Path, node: &Node<'_>, depth: usize, rows: &mut Vec<TreeRow>) {
        for ((_, name), child) in &node.folders {
            let path = self.path_in(folder, name);
            rows.push(TreeRow {
                marks: self.marks(&path, true, false),
                path: path.clone(),
                name: (*name).to_owned(),
                depth,
                kind: RowKind::Folder { open: true },
            });
            self.node_rows(&path, child, depth + 1, rows);
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

/// Opens the pane, or closes it. It shows every agent's tree, each with its own folders.
pub fn toggle_pane(state: &mut AppState) {
    state.prefs.files_pane.open = !state.prefs.files_pane.open;
}

/// The header's changes chip: opens the pane on the agent's changes, or on all its files when it
/// changed none, or closes the pane.
pub fn toggle_from_changes(state: &mut AppState, agent: AgentId) {
    if state.prefs.files_pane.open {
        state.prefs.files_pane.open = false;
        return;
    }
    state.prefs.files_pane.open = true;
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

/// Opens a folder, or closes it. A folder opened again lists again. Under "Changed", "Read" or a
/// search every folder stays open.
pub fn toggle_folder(state: &mut AppState, agent: AgentId, path: &Path) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let tree = &mut entry.tree;
    if tree.folds() && !tree.expanded.remove(path) {
        tree.expanded.insert(path.to_owned());
        if let Some(listing) = tree.listings.get_mut(path) {
            listing.stale = true;
        }
    }
    tree.highlighted = Some(path.to_owned());
}

pub fn collapse_all(state: &mut AppState, agent: AgentId) {
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.tree.expanded.clear();
    }
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
    let folds = state
        .agents
        .get(&agent)
        .is_some_and(|entry| entry.tree.folds());
    let target = match (direction, &row.kind) {
        (Sideways::Right, RowKind::Folder { open: false })
        | (Sideways::Left, RowKind::Folder { open: true })
            if folds =>
        {
            toggle_folder(state, agent, &highlighted);
            return;
        }
        (Sideways::Right, RowKind::Folder { open: true }) => rows
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
        RowKind::Folder { .. } => toggle_folder(state, agent, &row.path),
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

/// Shows the file view's file in the tree: its folders open and its row highlighted. A file the
/// filter hides shows under "All".
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
    let root = outside_roots(&folder, &marks)
        .into_iter()
        .find(|root| {
            style
                .strip_prefix(&full_text, &root.to_string_lossy())
                .is_some()
        })
        .unwrap_or(folder);
    let mut above = style.parent(&full_text).map(str::to_owned);
    while let Some(parent) = above {
        if style
            .strip_prefix(&parent, &root.to_string_lossy())
            .is_none()
        {
            break;
        }
        let parent_path = PathBuf::from(&parent);
        if parent_path != entry.summary.folder {
            entry.tree.expanded.insert(parent_path);
        }
        above = style
            .parent(&parent)
            .filter(|next| *next != parent)
            .map(str::to_owned);
    }
    entry.tree.highlighted = Some(full);
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
                    RowKind::Folder { open: false } => "▸",
                    RowKind::Folder { open: true } => "▾",
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
            vec![entry("Cargo.toml", EntryKind::File, GitIgnore::Kept)],
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
                "  ▾ crates",
                "    ▾ lexer",
                "      ▾ src",
                "        · token.rs",
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
    fn folders_stay_open_outside_the_all_view() {
        let (mut state, agent) = listed_agent();
        let src = Path::new("/work/project-1/src");
        state.agents.get_mut(&agent).unwrap().details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: PathBuf::from("/work/project-1/src/lib.rs"),
                lines_added: 2,
                lines_removed: 0,
            }],
            ..AgentDetails::default()
        });
        set_filter(&mut state, agent, TreeFilter::Changed);
        highlight(&mut state, agent, src);

        step_sideways(&mut state, agent, Sideways::Left);
        toggle_folder(&mut state, agent, src);

        assert!(state.agents[&agent].tree.expanded.is_empty());
        assert_eq!(highlighted(&state, agent), Some(src));
    }

    #[test]
    fn the_files_chip_opens_the_pane_on_the_changes_then_closes_it() {
        let (mut state, agent) = listed_agent();

        toggle_from_changes(&mut state, agent);
        assert!(state.prefs.files_pane.open);
        assert_eq!(state.agents[&agent].tree.filter, TreeFilter::All);

        toggle_from_changes(&mut state, agent);
        assert!(!state.prefs.files_pane.open);

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
    fn a_folder_outside_every_checkout_shows_the_files_the_agent_touched_there() {
        let mut tree = tree();
        let outside = PathBuf::from("/etc");
        tree.expanded.insert(outside.clone());
        tree.listings.insert(
            outside,
            Listing {
                entries: Loadable::Failed(
                    "the folder is outside the agent's folder and the checkouts it worked in"
                        .to_owned(),
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
}
