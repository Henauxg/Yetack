//! The file view's list of its open files: a button at the end of the tabs opens it, a filter
//! narrows it, and the keys or the pointer pick a file to show or one to close.
//!
//! The filter follows the keyboard alone: its text lives in the drafts.

use std::path::{Path, PathBuf};

use cockpit_protocol::AgentId;

use super::actions::{self, Direction};
use super::app_state::AppState;
use super::drafts::{Drafts, FileListDraft};
use super::ui_state::{Popover, TabMenuOrigin};
use super::{tab_menu, terminals};
use crate::path_style::PathStyle;

/// The open files the filter leaves, in the order of the tabs. The filter matches the path as the
/// row shows it, whatever the case.
pub fn rows(state: &AppState, drafts: &Drafts, agent: AgentId) -> Vec<PathBuf> {
    let (Some(entry), Some(view)) = (state.agents.get(&agent), state.ui.file_view(agent)) else {
        return Vec::new();
    };
    let query = drafts.file_list.query.to_lowercase();
    let folder = &entry.summary.folder;
    view.tabs(&entry.changed_paths())
        .into_iter()
        .filter(|tab| shown_text(folder, tab).to_lowercase().contains(&query))
        .collect()
}

/// A tab's path without the folders above the agent's folder: a file outside it shows from the
/// folder beside it, such as another worktree.
fn shown_text(folder: &Path, tab: &Path) -> String {
    let style = PathStyle::of(folder);
    let folder = folder.to_string_lossy();
    let tab = tab.to_string_lossy();
    let above = style.parent(&folder).unwrap_or_default();
    match style.strip_prefix(&tab, above) {
        Some(rest) if style.is_absolute(&tab) => rest.to_owned(),
        Some(_) | None => tab.into_owned(),
    }
}

/// Opens the list with the shown file highlighted, or closes it.
pub fn toggle(state: &mut AppState, drafts: &mut Drafts, agent: AgentId) {
    if state.ui.popover == Some(Popover::FileList) {
        state.ui.popover = None;
        return;
    }
    if state.ui.file_view(agent).is_none() {
        return;
    }
    state.ui.popover = Some(Popover::FileList);
    drafts.file_list = FileListDraft::default();
    drafts.file_list.highlighted = shown_row(state, drafts, agent).unwrap_or(0);
}

/// The filter changed: the highlight goes to the first file it leaves, or back to the shown file
/// once the filter is empty.
pub fn filter(state: &AppState, drafts: &mut Drafts, agent: AgentId, query: String) {
    drafts.file_list.query = query;
    drafts.file_list.highlighted = if drafts.file_list.query.is_empty() {
        shown_row(state, drafts, agent).unwrap_or(0)
    } else {
        0
    };
}

pub fn move_highlight(state: &AppState, drafts: &mut Drafts, agent: AgentId, direction: Direction) {
    let count = rows(state, drafts, agent).len();
    drafts.file_list.highlighted = actions::moved(drafts.file_list.highlighted, count, direction);
}

/// The pointer moved onto the row at `index`: it takes the highlight. A row of a list the filter
/// has since narrowed changes nothing.
pub fn point(state: &AppState, drafts: &mut Drafts, agent: AgentId, index: usize) {
    if index < rows(state, drafts, agent).len() {
        drafts.file_list.highlighted = index;
    }
}

/// Shows the file at `index`, or the highlighted one, and closes the list.
pub fn pick(state: &mut AppState, drafts: &Drafts, agent: AgentId, index: Option<usize>) {
    let index = index.unwrap_or(drafts.file_list.highlighted);
    if let Some(path) = rows(state, drafts, agent).into_iter().nth(index) {
        actions::show_file(state, agent, path);
    }
}

/// Closes the file at `index`, or the highlighted one. The list stays open while the file view
/// holds a file, and the highlight stays on the same row, or the last one.
pub fn close(state: &mut AppState, drafts: &mut Drafts, agent: AgentId, index: Option<usize>) {
    let index = index.unwrap_or(drafts.file_list.highlighted);
    let Some(path) = rows(state, drafts, agent).into_iter().nth(index) else {
        return;
    };
    actions::close_tab(state, agent, &path);
    after_close(state, drafts, agent, &path);
}

/// After a close: the list stays open while the file view holds a file. The highlight goes to
/// `path`'s row while it is there, else it stays on the same row, or the last one.
pub fn after_close(state: &mut AppState, drafts: &mut Drafts, agent: AgentId, path: &Path) {
    if state.ui.file_view(agent).is_none() {
        state.ui.popover = None;
        return;
    }
    // Closing the shown file shows another one, which closes popovers.
    state.ui.popover = Some(Popover::FileList);
    let rows = rows(state, drafts, agent);
    let last = rows.len().saturating_sub(1);
    drafts.file_list.highlighted = rows
        .iter()
        .position(|row| row == path)
        .unwrap_or(drafts.file_list.highlighted.min(last));
}

/// Opens the menu of the row at `index`, at `x`, `y` in the window's pixels. The row takes the
/// highlight.
pub fn open_menu(
    state: &mut AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    index: usize,
    x: i32,
    y: i32,
) {
    let Some(path) = rows(state, drafts, agent).into_iter().nth(index) else {
        return;
    };
    drafts.file_list.highlighted = index;
    tab_menu::open(state, agent, &path, x, y, TabMenuOrigin::List);
}

/// The shown file's row, when the filter leaves it.
fn shown_row(state: &AppState, drafts: &Drafts, agent: AgentId) -> Option<usize> {
    let shown = terminals::shown_file(state, agent)?;
    rows(state, drafts, agent)
        .iter()
        .position(|row| row == shown)
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::AgentStatus;

    use super::*;
    use crate::state::{state_with, summary};

    const AGENT_ID: u32 = 1;

    /// An agent whose file view opened `files` in order, and shows the last one.
    fn opened(files: &[&str]) -> (AppState, Drafts) {
        let mut state = state_with(vec![summary(AGENT_ID, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        for file in files {
            actions::open_file(&mut state, agent(), PathBuf::from(file), None);
        }
        (state, Drafts::default())
    }

    fn agent() -> AgentId {
        AgentId::new(AGENT_ID)
    }

    fn shown(state: &AppState) -> &Path {
        &state.ui.file_view(agent()).unwrap().path
    }

    #[test]
    fn the_list_opens_on_the_shown_file_and_the_filter_narrows_it() {
        let (mut state, mut drafts) = opened(&["src/lexer.rs", "src/State.rs", "tests/lexer.rs"]);
        actions::open_file(&mut state, agent(), PathBuf::from("src/State.rs"), None);

        toggle(&mut state, &mut drafts, agent());
        assert_eq!(state.ui.popover, Some(Popover::FileList));
        assert_eq!(drafts.file_list.highlighted, 1);

        filter(&state, &mut drafts, agent(), "LEXER".to_owned());
        assert_eq!(
            rows(&state, &drafts, agent()),
            [
                PathBuf::from("src/lexer.rs"),
                PathBuf::from("tests/lexer.rs")
            ]
        );
        assert_eq!(drafts.file_list.highlighted, 0);

        filter(&state, &mut drafts, agent(), String::new());
        assert_eq!(drafts.file_list.highlighted, 1);

        toggle(&mut state, &mut drafts, agent());
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn the_keys_and_the_pointer_move_the_highlight_within_the_rows() {
        let (mut state, mut drafts) = opened(&["a.rs", "b.rs", "c.rs"]);
        toggle(&mut state, &mut drafts, agent());

        move_highlight(&state, &mut drafts, agent(), Direction::Down);
        assert_eq!(drafts.file_list.highlighted, 2);
        move_highlight(&state, &mut drafts, agent(), Direction::Up);
        assert_eq!(drafts.file_list.highlighted, 1);

        point(&state, &mut drafts, agent(), 0);
        assert_eq!(drafts.file_list.highlighted, 0);
        point(&state, &mut drafts, agent(), 3);
        assert_eq!(drafts.file_list.highlighted, 0);
    }

    #[test]
    fn a_pick_shows_the_file_and_closes_the_list() {
        let (mut state, mut drafts) = opened(&["a.rs", "b.rs", "c.rs"]);
        toggle(&mut state, &mut drafts, agent());
        filter(&state, &mut drafts, agent(), "b".to_owned());

        pick(&mut state, &drafts, agent(), None);

        assert_eq!(shown(&state), Path::new("b.rs"));
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn closing_a_file_keeps_the_list_open_until_the_last_one() {
        let (mut state, mut drafts) = opened(&["a.rs", "b.rs", "c.rs"]);
        toggle(&mut state, &mut drafts, agent());

        close(&mut state, &mut drafts, agent(), None);
        assert_eq!(
            rows(&state, &drafts, agent()),
            [PathBuf::from("a.rs"), PathBuf::from("b.rs")]
        );
        assert_eq!(shown(&state), Path::new("b.rs"));
        assert_eq!(state.ui.popover, Some(Popover::FileList));
        assert_eq!(drafts.file_list.highlighted, 1);

        close(&mut state, &mut drafts, agent(), Some(0));
        assert_eq!(rows(&state, &drafts, agent()), [PathBuf::from("b.rs")]);
        assert_eq!(drafts.file_list.highlighted, 0);

        close(&mut state, &mut drafts, agent(), None);
        assert!(state.ui.file_view(agent()).is_none());
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn the_filter_skips_the_folders_above_a_file_outside_the_agent_folder() {
        let (mut state, mut drafts) = opened(&["src/lexer.rs", "/work/project-1_fix/src/lexer.rs"]);
        toggle(&mut state, &mut drafts, agent());

        filter(&state, &mut drafts, agent(), "work".to_owned());
        assert!(rows(&state, &drafts, agent()).is_empty());

        filter(&state, &mut drafts, agent(), "1_fix/src".to_owned());
        assert_eq!(
            rows(&state, &drafts, agent()),
            [PathBuf::from("/work/project-1_fix/src/lexer.rs")]
        );
    }

    #[test]
    fn the_list_closes_with_the_file_view() {
        let (mut state, mut drafts) = opened(&["a.rs"]);
        toggle(&mut state, &mut drafts, agent());

        actions::close_file_view(&mut state, agent());

        assert_eq!(state.ui.popover, None);
    }
}
