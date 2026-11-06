//! The new agent dialog's folder field, as the "Open file" dialog's path field: the folders of the
//! folder it names listed under it, filtered by its last segment, Tab to complete, and Backspace
//! on an empty segment to go up.
//!
//! The list opens once the field is typed in, and Esc closes it. The field follows the keyboard
//! alone: its text lives in the drafts, and a pick that changes it returns the new text for the
//! window to put in the field.

use std::path::{Path, PathBuf};

use cockpit_protocol::DirEntry;

use super::actions::Direction;
use super::app_state::AppState;
use super::drafts::Drafts;
use super::open_file;
use super::ui_state::{Dialog, FolderList, NewAgentForm};

/// A row under the folder field, in the order it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderRow<'list> {
    /// "..": the folder above.
    Up,
    Folder(&'list DirEntry),
}

/// The folder the text lists: its folder part, when it is absolute. The daemon refuses any other,
/// as `agent.start` does.
pub fn listed_folder(text: &str) -> Option<PathBuf> {
    let folder = Path::new(open_file::split(text).0);
    folder.is_absolute().then(|| folder.to_owned())
}

/// The rows for `text`: ".." while the last segment is empty, then the listed folders the segment
/// keeps.
pub fn rows<'list>(list: &'list FolderList, text: &str) -> Vec<FolderRow<'list>> {
    let (_, segment) = open_file::split(text);
    let mut rows = Vec::new();
    if segment.is_empty() && list.folder.parent().is_some() {
        rows.push(FolderRow::Up);
    }
    if let Some(listing) = list.listing.loaded() {
        let kept = open_file::matching(&listing.entries, segment);
        rows.extend(kept.into_iter().map(FolderRow::Folder));
    }
    rows
}

fn form(state: &AppState) -> Option<&NewAgentForm> {
    match &state.ui.dialog {
        Some(Dialog::NewAgent(form)) => Some(form),
        _ => None,
    }
}

fn form_mut(state: &mut AppState) -> Option<&mut NewAgentForm> {
    match &mut state.ui.dialog {
        Some(Dialog::NewAgent(form)) => Some(form),
        _ => None,
    }
}

/// The list under the field, while it is open.
pub fn list(state: &AppState) -> Option<&FolderList> {
    form(state)?.folders.as_ref()
}

/// Keeps the typed text. The first row is highlighted while a segment filters the list. Returns
/// whether the list must follow the text, which names another folder or none.
pub fn type_folder(state: &AppState, drafts: &mut Drafts, text: String) -> bool {
    let filtering = !open_file::split(&text).1.is_empty();
    drafts.new_agent.folder_highlighted = filtering.then_some(0);
    let wanted = listed_folder(&text);
    drafts.new_agent.folder = text;
    let shown = list(state).map(|list| &list.folder);
    shown != wanted.as_ref()
}

/// Makes the list show the folder the typed text names, and closes it when the text names none.
pub fn follow(state: &mut AppState, drafts: &Drafts) {
    let Some(form) = form_mut(state) else {
        return;
    };
    let Some(folder) = listed_folder(&drafts.new_agent.folder) else {
        form.folders = None;
        return;
    };
    if form
        .folders
        .as_ref()
        .is_none_or(|list| list.folder != folder)
    {
        form.folders = Some(FolderList::new(folder));
    }
}

/// Moves the highlight over the rows.
pub fn move_highlight(state: &AppState, drafts: &mut Drafts, direction: Direction) {
    let Some(list) = list(state) else {
        return;
    };
    let count = rows(list, &drafts.new_agent.folder).len();
    let Some(last) = count.checked_sub(1) else {
        return;
    };
    let highlighted = &mut drafts.new_agent.folder_highlighted;
    *highlighted = Some(match (direction, *highlighted) {
        (Direction::Down, None) => 0,
        (Direction::Down, Some(index)) => (index + 1).min(last),
        (Direction::Up, None) => last,
        (Direction::Up, Some(index)) => index.saturating_sub(1).min(last),
    });
}

/// The pointer moved onto the row at `index`: it takes the highlight, as the keys do. A row of a
/// list the text has since narrowed changes nothing.
pub fn point(state: &AppState, drafts: &mut Drafts, index: usize) {
    let Some(list) = list(state) else {
        return;
    };
    if index < rows(list, &drafts.new_agent.folder).len() {
        drafts.new_agent.folder_highlighted = Some(index);
    }
}

/// Takes the row at `index`, or else the highlighted one: ".." goes up, and a folder completes the
/// text and lists. Returns the field's new text, or `None` when no row was taken.
pub fn take(state: &mut AppState, drafts: &mut Drafts, index: Option<usize>) -> Option<String> {
    let list = list(state)?;
    let text = &drafts.new_agent.folder;
    let index = index.or(drafts.new_agent.folder_highlighted)?;
    let new_text = match rows(list, text).get(index).copied()? {
        FolderRow::Up => up(text)?,
        FolderRow::Folder(entry) => open_file::completed(text, &entry.name, true),
    };
    set_text(state, drafts, new_text.clone());
    Some(new_text)
}

/// What Backspace makes of the field: on an empty last segment, the folder above's text. `None`
/// when the key should delete as usual.
pub fn text_above(drafts: &Drafts) -> Option<String> {
    let text = &drafts.new_agent.folder;
    if !open_file::split(text).1.is_empty() {
        return None;
    }
    up(text)
}

/// The text naming the folder above the one `text` lists. The field lists absolute paths only.
fn up(text: &str) -> Option<String> {
    listed_folder(text)?;
    let root = Path::new("/");
    open_file::up(root, &open_file::listed_folder(root, text), text)
}

/// Puts `text` in the field's draft, and makes the list follow it.
pub fn set_text(state: &mut AppState, drafts: &mut Drafts, text: String) {
    drafts.new_agent.folder = text;
    drafts.new_agent.folder_highlighted = None;
    follow(state, drafts);
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{DirListing, EntryKind};

    use super::*;
    use crate::state::Loadable;
    use crate::state::app_state::tests::state_with;

    fn folder(name: &str) -> DirEntry {
        DirEntry {
            name: name.to_owned(),
            kind: EntryKind::Folder,
            size: None,
            symlink: false,
        }
    }

    fn listing(path: &str, names: &[&str]) -> Loadable<DirListing> {
        Loadable::Loaded(DirListing {
            path: PathBuf::from(path),
            entries: names.iter().map(|name| folder(name)).collect(),
            truncated: false,
        })
    }

    /// The dialog open with the field emptied.
    fn opened() -> (AppState, Drafts) {
        let mut state = state_with(Vec::new());
        let mut drafts = Drafts::default();
        crate::state::actions::open_new_agent(&mut state, &mut drafts);
        (state, drafts)
    }

    /// Types `text` as the window does: the list follows when asked to.
    fn type_text(state: &mut AppState, drafts: &mut Drafts, text: &str) {
        if type_folder(state, drafts, text.to_owned()) {
            follow(state, drafts);
        }
    }

    /// Answers the list's folder with `names`.
    fn answer(state: &mut AppState, names: &[&str]) {
        let list = form_mut(state)
            .and_then(|form| form.folders.as_mut())
            .unwrap();
        list.listing = listing(&list.folder.display().to_string(), names);
    }

    fn row_names(state: &AppState, drafts: &Drafts) -> Vec<String> {
        rows(list(state).unwrap(), &drafts.new_agent.folder)
            .into_iter()
            .map(|row| match row {
                FolderRow::Up => "..".to_owned(),
                FolderRow::Folder(entry) => entry.name.clone(),
            })
            .collect()
    }

    #[test]
    fn only_an_absolute_folder_part_lists() {
        assert_eq!(
            listed_folder("/home/dev/pro"),
            Some(PathBuf::from("/home/dev"))
        );
        assert_eq!(listed_folder("/"), Some(PathBuf::from("/")));
        assert_eq!(listed_folder("/home"), Some(PathBuf::from("/")));
        assert_eq!(listed_folder("~/projects/"), None);
        assert_eq!(listed_folder("projects/"), None);
        assert_eq!(listed_folder("projects"), None);
        assert_eq!(listed_folder(""), None);
    }

    #[test]
    fn typing_opens_the_list_and_the_last_segment_filters_it() {
        let (mut state, mut drafts) = opened();
        assert_eq!(list(&state), None, "the list waits for typing");

        type_text(&mut state, &mut drafts, "/home/dev/");

        assert_eq!(
            list(&state),
            Some(&FolderList::new(PathBuf::from("/home/dev")))
        );
        answer(&mut state, &[".config", "notes", "projects", "scratch"]);
        assert_eq!(
            row_names(&state, &drafts),
            ["..", ".config", "notes", "projects", "scratch"]
        );
        assert_eq!(drafts.new_agent.folder_highlighted, None);

        // The same folder: the list stays, filtered.
        assert!(!type_folder(&state, &mut drafts, "/home/dev/S".to_owned()));
        assert_eq!(row_names(&state, &drafts), ["scratch", "notes", "projects"]);
        assert_eq!(drafts.new_agent.folder_highlighted, Some(0));

        // A text that names no absolute folder closes it.
        type_text(&mut state, &mut drafts, "~/projects/");
        assert_eq!(list(&state), None);
    }

    #[test]
    fn tab_completes_a_folder_which_then_lists() {
        let (mut state, mut drafts) = opened();
        type_text(&mut state, &mut drafts, "/home/dev/pro");
        answer(&mut state, &["notes", "projects"]);

        let text = take(&mut state, &mut drafts, None);

        assert_eq!(text.as_deref(), Some("/home/dev/projects/"));
        assert_eq!(drafts.new_agent.folder, "/home/dev/projects/");
        assert_eq!(drafts.new_agent.folder_highlighted, None);
        let shown = list(&state).unwrap();
        assert_eq!(shown.folder, PathBuf::from("/home/dev/projects"));
        assert_eq!(shown.listing, Loadable::NotRequested);

        // Nothing highlighted: nothing taken, and Tab moves the focus as usual.
        answer(&mut state, &["calc-parser"]);
        assert_eq!(take(&mut state, &mut drafts, None), None);
        // A click takes its row.
        let text = take(&mut state, &mut drafts, Some(1));
        assert_eq!(text.as_deref(), Some("/home/dev/projects/calc-parser/"));
    }

    #[test]
    fn backspace_and_the_up_row_go_to_the_folder_above() {
        let (mut state, mut drafts) = opened();
        type_text(&mut state, &mut drafts, "/home/dev/pro");
        // Backspace in a segment deletes as usual.
        assert_eq!(text_above(&drafts), None);

        type_text(&mut state, &mut drafts, "/home/dev/projects/");
        assert_eq!(text_above(&drafts).as_deref(), Some("/home/dev/"));

        let text = take(&mut state, &mut drafts, Some(0));
        assert_eq!(text.as_deref(), Some("/home/dev/"));
        assert_eq!(list(&state).unwrap().folder, PathBuf::from("/home/dev"));

        type_text(&mut state, &mut drafts, "/");
        answer(&mut state, &["home"]);
        assert_eq!(
            row_names(&state, &drafts),
            ["home"],
            "the root has no folder above"
        );
        assert_eq!(text_above(&drafts), None);
    }

    #[test]
    fn backspace_deletes_as_usual_in_a_text_the_field_does_not_list() {
        let (mut state, mut drafts) = opened();
        for text in ["~/projects/", "projects/", "projects/../"] {
            type_text(&mut state, &mut drafts, text);
            assert_eq!(text_above(&drafts), None, "{text}");
        }
    }

    #[test]
    fn the_keys_move_the_highlight_and_esc_closes_the_list_first() {
        let (mut state, mut drafts) = opened();
        type_text(&mut state, &mut drafts, "/home/dev/");
        answer(&mut state, &["notes", "projects"]);

        move_highlight(&state, &mut drafts, Direction::Up);
        assert_eq!(drafts.new_agent.folder_highlighted, Some(2));
        move_highlight(&state, &mut drafts, Direction::Down);
        assert_eq!(drafts.new_agent.folder_highlighted, Some(2));
        move_highlight(&state, &mut drafts, Direction::Up);
        assert_eq!(drafts.new_agent.folder_highlighted, Some(1));

        assert!(state.ui.close_overlays());
        assert_eq!(list(&state), None);
        assert!(matches!(state.ui.dialog, Some(Dialog::NewAgent(_))));
        assert!(state.ui.close_overlays());
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn the_pointer_moves_the_highlight_within_the_listed_rows() {
        let (mut state, mut drafts) = opened();
        type_text(&mut state, &mut drafts, "/home/dev/");
        answer(&mut state, &["notes", "projects", "scratch"]);

        point(&state, &mut drafts, 3);
        assert_eq!(drafts.new_agent.folder_highlighted, Some(3));
        move_highlight(&state, &mut drafts, Direction::Up);
        assert_eq!(drafts.new_agent.folder_highlighted, Some(2));

        // A row the narrowed list no longer has: the pointer was on the old list.
        type_text(&mut state, &mut drafts, "/home/dev/s");
        point(&state, &mut drafts, 1);
        point(&state, &mut drafts, 3);
        assert_eq!(drafts.new_agent.folder_highlighted, Some(1));
        assert_eq!(
            take(&mut state, &mut drafts, None).as_deref(),
            Some("/home/dev/notes/")
        );
    }
}
