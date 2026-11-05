//! The changes chip's popover: the files the agent changed, one line each. A file inside the
//! agent's folder shows relative to it. A file outside it, as in another git worktree beside it,
//! shows under the folder that holds it, named once at the head of its group.

use std::collections::BTreeMap;
use std::path::{Component, MAIN_SEPARATOR, Path, PathBuf};

use cockpit_protocol::{AgentId, ChangedFile};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::format;
use crate::state::actions;
use crate::ui::icons::{Icon, IconKind};

/// The files under one folder: the agent's own, or one outside it.
#[derive(Debug, Clone, PartialEq)]
struct ChangeGroup {
    /// `None` for the agent's folder.
    root: Option<PathBuf>,
    rows: Vec<ChangeRow>,
}

#[derive(Debug, Clone, PartialEq)]
struct ChangeRow {
    /// What a click opens, and what the tooltip shows.
    path: PathBuf,
    name: String,
    /// The file's folder under its group's folder, ending with a separator; empty right in it.
    folder: String,
    lines_added: u32,
    lines_removed: u32,
}

impl ChangeRow {
    fn new(file: &ChangedFile, relative: &Path) -> Self {
        let name = relative.file_name().map_or_else(
            || relative.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let folder = relative
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(|parent| format!("{}{MAIN_SEPARATOR}", parent.display()))
            .unwrap_or_default();
        Self {
            path: file.path.clone(),
            name,
            folder,
            lines_added: file.lines_added,
            lines_removed: file.lines_removed,
        }
    }
}

/// The agent's folder first, then each folder outside it, by path. Within a group the files sort
/// by path, so that the files of a folder sit together.
fn group_changes(folder: &Path, files: &[ChangedFile]) -> Vec<ChangeGroup> {
    let mut groups: BTreeMap<Option<PathBuf>, Vec<ChangeRow>> = BTreeMap::new();
    for file in files {
        let (root, relative) = match file.path.strip_prefix(folder) {
            Ok(relative) if !relative.as_os_str().is_empty() => (None, relative.to_owned()),
            Ok(_) | Err(_) => {
                let root = outside_root(folder, &file.path);
                let relative = file
                    .path
                    .strip_prefix(&root)
                    .map_or_else(|_| file.path.clone(), Path::to_owned);
                (Some(root), relative)
            }
        };
        groups
            .entry(root)
            .or_default()
            .push(ChangeRow::new(file, &relative));
    }
    let mut sorted = Vec::with_capacity(groups.len());
    for (root, mut rows) in groups {
        rows.sort_by(|left, right| (&left.folder, &left.name).cmp(&(&right.folder, &right.name)));
        sorted.push(ChangeGroup { root, rows });
    }
    sorted
}

/// The folder a file outside the agent's folder shows under: the first folder below what the two
/// paths share. Beside "/work/app", "/work/app_feature/src/lib.rs" shows under
/// "/work/app_feature", as "src/lib.rs".
fn outside_root(folder: &Path, path: &Path) -> PathBuf {
    let shared = folder
        .components()
        .zip(path.components())
        .take_while(|(left, right)| left == right)
        .count();
    let mut components = path.components();
    let mut root: PathBuf = components.by_ref().take(shared).collect();
    let rest: Vec<Component<'_>> = components.collect();
    // A file right in the shared folder shows under it, not under itself.
    if let [first, _, ..] = rest.as_slice() {
        root.push(first);
    }
    root
}

/// A group's head: its folder's own name, as "app_feature/".
fn root_label(root: &Path) -> String {
    match root.file_name() {
        Some(name) => format!("{}{MAIN_SEPARATOR}", name.to_string_lossy()),
        None => root.display().to_string(),
    }
}

/// Summed in `u64`, so that no count the daemon sends overflows.
pub fn line_totals(files: &[ChangedFile]) -> (u64, u64) {
    let added = files.iter().map(|file| u64::from(file.lines_added)).sum();
    let removed = files.iter().map(|file| u64::from(file.lines_removed)).sum();
    (added, removed)
}

#[component]
pub fn ChangesList(agent: AgentId, files: Vec<ChangedFile>, folder: PathBuf) -> Element {
    let groups = group_changes(&folder, &files);
    let (added, removed) = line_totals(&files);
    let count = format::count(files.len(), "file", "files");
    // A head per group only when there are several: alone, the agent's folder needs none.
    let heads = groups.len() > 1;
    rsx! {
        div { class: "popover changes-list",
            if files.is_empty() {
                span { class: "changes-empty", "No file changed yet." }
            } else {
                div { class: "changes-total",
                    span { "{count}" }
                    span { class: "mono added", "+{added}" }
                    span { class: "mono removed", "-{removed}" }
                }
                div { class: "changes-scroll scroll",
                    for group in groups {
                        {
                            let root = group.root.clone().unwrap_or_else(|| folder.clone());
                            rsx! {
                                div {
                                    key: "{root.display()}",
                                    class: if heads { "changes-group headed" } else { "changes-group" },
                                    role: "group",
                                    aria_label: "{root.display()}",
                                    if heads {
                                        div { class: "changes-root", title: "{root.display()}",
                                            Icon { kind: IconKind::Folder, size: 13 }
                                            span { class: "mono changes-root-name", "{root_label(&root)}" }
                                        }
                                    }
                                    // Keyed by position: the daemon's list does not promise one
                                    // entry per path.
                                    for (index, row) in group.rows.into_iter().enumerate() {
                                        ChangeLine { key: "{index}", agent, row }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ChangeLine(agent: AgentId, row: ChangeRow) -> Element {
    let cockpit: Cockpit = use_context();
    let path = row.path.clone();
    rsx! {
        button {
            class: "change-row",
            title: "{row.path.display()}",
            onclick: move |_| {
                let path = path.clone();
                cockpit.update(|state| actions::open_file(state, agent, path, None));
            },
            span { class: "change-icon", Icon { kind: IconKind::File, size: 14 } }
            span { class: "mono change-name", "{row.name}" }
            // Cut at its start: the end of a folder tells files of the same name apart.
            span { class: "mono change-folder",
                span { class: "change-folder-text", "{row.folder}" }
            }
            span { class: "mono change-counts",
                span { class: if row.lines_added == 0 { "added zero" } else { "added" }, "+{row.lines_added}" }
                span { class: if row.lines_removed == 0 { "removed zero" } else { "removed" }, "-{row.lines_removed}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, lines_added: u32, lines_removed: u32) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            lines_added,
            lines_removed,
        }
    }

    /// A group's root, and each row's folder and name.
    type ShownGroup<'groups> = (Option<&'groups str>, Vec<(&'groups str, &'groups str)>);

    fn shown(groups: &[ChangeGroup]) -> Vec<ShownGroup<'_>> {
        groups
            .iter()
            .map(|group| {
                let root = group.root.as_deref().and_then(Path::to_str);
                let rows = group
                    .rows
                    .iter()
                    .map(|row| (row.folder.as_str(), row.name.as_str()))
                    .collect();
                (root, rows)
            })
            .collect()
    }

    #[test]
    fn files_in_the_agent_folder_show_relative_to_it_sorted_by_path() {
        let files = [
            file("/work/app/tests/columns.rs", 8, 0),
            file("/work/app/src/error.rs", 2, 0),
            file("/work/app/README.md", 1, 1),
        ];

        let groups = group_changes(Path::new("/work/app"), &files);

        assert_eq!(
            shown(&groups),
            [(
                None,
                vec![
                    ("", "README.md"),
                    ("src/", "error.rs"),
                    ("tests/", "columns.rs")
                ]
            )]
        );
        assert_eq!(
            groups
                .first()
                .and_then(|group| group.rows.first())
                .map(|row| &row.path),
            Some(&PathBuf::from("/work/app/README.md"))
        );
    }

    #[test]
    fn files_of_a_worktree_beside_the_folder_group_under_it() {
        let files = [
            file(
                "/work/app_GH42Q-report-columns/crates/lexer/src/token.rs",
                31,
                4,
            ),
            file("/work/app/src/lexer.rs", 1, 1),
            file(
                "/work/app_GH42Q-report-columns/crates/lexer/tests/columns.rs",
                24,
                0,
            ),
        ];

        let groups = group_changes(Path::new("/work/app"), &files);

        assert_eq!(
            shown(&groups),
            [
                (None, vec![("src/", "lexer.rs")]),
                (
                    Some("/work/app_GH42Q-report-columns"),
                    vec![
                        ("crates/lexer/src/", "token.rs"),
                        ("crates/lexer/tests/", "columns.rs")
                    ]
                ),
            ]
        );
    }

    #[test]
    fn files_of_several_folders_outside_get_one_group_each() {
        let files = [
            file("/work/app_bench/benches/lexer.rs", 12, 2),
            file("/work/app_columns/src/token.rs", 3, 0),
            file("/work/notes.md", 1, 0),
            file("/etc/calc/config.toml", 1, 1),
        ];

        let groups = group_changes(Path::new("/work/app"), &files);

        assert_eq!(
            shown(&groups),
            [
                (Some("/etc"), vec![("calc/", "config.toml")]),
                (Some("/work"), vec![("", "notes.md")]),
                (Some("/work/app_bench"), vec![("benches/", "lexer.rs")]),
                (Some("/work/app_columns"), vec![("src/", "token.rs")]),
            ]
        );
    }

    #[test]
    fn a_group_head_names_its_folder_once() {
        assert_eq!(
            root_label(Path::new("/work/app_GH42Q-report-columns")),
            "app_GH42Q-report-columns/"
        );
        assert_eq!(root_label(Path::new("/")), "/");
    }

    #[test]
    fn line_totals_hold_counts_past_u32() {
        let files = [file("a.rs", u32::MAX, 1), file("b.rs", u32::MAX, u32::MAX)];

        assert_eq!(
            line_totals(&files),
            (2 * u64::from(u32::MAX), u64::from(u32::MAX) + 1)
        );
    }
}
