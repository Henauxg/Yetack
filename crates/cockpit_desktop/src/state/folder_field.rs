//! The new agent dialog's folder field, as the "Open file" dialog's path field: the folders of the
//! folder it names listed under it, filtered by its last segment, Tab to complete, and Backspace
//! on an empty segment to go up.
//!
//! The list opens once the field is typed in, and Esc closes it. The field follows the keyboard
//! alone: its text lives in the drafts, and a pick that changes it returns the new text for the
//! window to put in the field.

use std::path::PathBuf;

use cockpit_protocol::{DirEntry, EntryKind};

use super::actions::Direction;
use super::app_state::AppState;
use super::drafts::Drafts;
use super::open_file;
use super::ui_state::{Dialog, FolderList, NewAgentForm};
use crate::machine::MachineId;
use crate::path_style::PathStyle;

/// A row under the folder field, in the order it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderRow<'list> {
    /// "..": the folder above.
    Up,
    Folder(&'list DirEntry),
}

/// The folder the text lists: its folder part, when it is absolute. The daemon refuses any other,
/// as `agent.start` does.
pub fn listed_folder(style: PathStyle, text: &str) -> Option<PathBuf> {
    let (folder_part, _) = style.split(text);
    style
        .is_absolute(folder_part)
        .then(|| PathBuf::from(folder_part))
}

/// The rules of the paths the field names: those of the machine the agent starts on.
pub fn style(state: &AppState) -> PathStyle {
    let machine = match &state.ui.dialog {
        Some(Dialog::NewAgent(form)) => form.machine,
        _ => MachineId::THIS,
    };
    state
        .machines
        .get(machine)
        .unwrap_or(&state.machines.this)
        .daemon
        .path_style()
}

/// The rows for `text`: ".." while the last segment is empty, then the listed folders the segment
/// keeps.
pub fn rows<'list>(style: PathStyle, list: &'list FolderList, text: &str) -> Vec<FolderRow<'list>> {
    let (_, segment) = style.split(text);
    let mut rows = Vec::new();
    if segment.is_empty() && style.parent(&list.folder.to_string_lossy()).is_some() {
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
    let style = style(state);
    let (_, segment) = style.split(&text);
    let filtering = !segment.is_empty();
    drafts.new_agent.folder_highlighted = filtering.then_some(0);
    let wanted = listed_folder(style, &text);
    drafts.new_agent.folder = text;
    let shown = list(state).map(|list| &list.folder);
    match (shown, wanted) {
        (Some(shown), Some(wanted)) => {
            !style.same_path(&shown.to_string_lossy(), &wanted.to_string_lossy())
        }
        (None, None) => false,
        (Some(_), None) | (None, Some(_)) => true,
    }
}

/// Makes the list show the folder the typed text names, and closes it when the text names none.
pub fn follow(state: &mut AppState, drafts: &Drafts) {
    let style = style(state);
    let folders_unknown = form(state)
        .and_then(|form| state.machines.get(form.machine))
        .is_some_and(|known| known.folders_unknown);
    let Some(form) = form_mut(state) else {
        return;
    };
    let listed = listed_folder(style, &drafts.new_agent.folder).filter(|_| !folders_unknown);
    let Some(folder) = listed else {
        form.folders = None;
        return;
    };
    let shown = |list: &FolderList| {
        style.same_path(&list.folder.to_string_lossy(), &folder.to_string_lossy())
    };
    if !form.folders.as_ref().is_some_and(shown) {
        form.folders = Some(FolderList::new(folder));
    }
}

/// Moves the highlight over the rows.
pub fn move_highlight(state: &AppState, drafts: &mut Drafts, direction: Direction) {
    let Some(list) = list(state) else {
        return;
    };
    let count = rows(style(state), list, &drafts.new_agent.folder).len();
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
    if index < rows(style(state), list, &drafts.new_agent.folder).len() {
        drafts.new_agent.folder_highlighted = Some(index);
    }
}

/// Takes the row at `index`, or else the highlighted one: ".." goes up, and a folder completes the
/// text and lists. Returns the field's new text, or `None` when no row was taken.
pub fn take(state: &mut AppState, drafts: &mut Drafts, index: Option<usize>) -> Option<String> {
    let style = style(state);
    let list = list(state)?;
    let text = &drafts.new_agent.folder;
    let index = index.or(drafts.new_agent.folder_highlighted)?;
    let new_text = match rows(style, list, text).get(index).copied()? {
        FolderRow::Up => up(style, text)?,
        FolderRow::Folder(entry) => {
            open_file::completed(style, text, &entry.name, EntryKind::Folder)
        }
    };
    set_text(state, drafts, new_text.clone());
    Some(new_text)
}

/// What Backspace makes of the field: on an empty last segment, the folder above's text. `None`
/// when the key should delete as usual.
pub fn text_above(state: &AppState, drafts: &Drafts) -> Option<String> {
    let style = style(state);
    let text = &drafts.new_agent.folder;
    let (_, segment) = style.split(text);
    if !segment.is_empty() {
        return None;
    }
    up(style, text)
}

/// The text naming the folder above the one `text` lists. The field lists absolute paths only.
fn up(style: PathStyle, text: &str) -> Option<String> {
    let (folder_part, _) = style.split(text);
    if !style.is_absolute(folder_part) {
        return None;
    }
    let folder = style.resolve(folder_part, "");
    Some(style.folder_text(style.parent(&folder)?))
}

/// Puts a folder picked in the system's folder dialog in the field's draft, as a click on a recent
/// folder does. The list under the field, of the folder typed before, closes. Returns whether the
/// dialog was still open to take it.
pub fn pick(state: &mut AppState, drafts: &mut Drafts, folder: String) -> bool {
    let Some(form) = form_mut(state) else {
        return false;
    };
    form.folders = None;
    drafts.new_agent.folder = folder;
    drafts.new_agent.folder_highlighted = None;
    true
}

/// Puts `text` in the field's draft, and makes the list follow it.
pub fn set_text(state: &mut AppState, drafts: &mut Drafts, text: String) {
    drafts.new_agent.folder = text;
    drafts.new_agent.folder_highlighted = None;
    follow(state, drafts);
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{Completeness, DirListing, EntryLink};

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::state::app_state::tests::state_with;
    use crate::state::{ConnectionState, Loadable, MachineState};

    fn folder(name: &str) -> DirEntry {
        DirEntry {
            name: name.to_owned(),
            kind: EntryKind::Folder,
            size: None,
            link: EntryLink::Direct,
        }
    }

    fn listing(path: &str, names: &[&str]) -> Loadable<DirListing> {
        Loadable::Loaded(DirListing {
            path: PathBuf::from(path),
            entries: names.iter().map(|name| folder(name)).collect(),
            completeness: Completeness::Complete,
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
        rows(style(state), list(state).unwrap(), &drafts.new_agent.folder)
            .into_iter()
            .map(|row| match row {
                FolderRow::Up => "..".to_owned(),
                FolderRow::Folder(entry) => entry.name.clone(),
            })
            .collect()
    }

    #[test]
    fn the_list_follows_whether_the_new_agents_machine_lists_folders() {
        let devbox = MachineId::new(1);
        let (mut state, mut drafts) = opened();
        let mut known = MachineState::new(DaemonLabel::ssh("devbox".to_owned()));
        known.connection = ConnectionState::Connected;
        state.machines.others.insert(devbox, known);
        state.machines.this.folders_unknown = true;
        crate::state::actions::pick_new_agent_machine(&mut state, &mut drafts, devbox);

        type_text(&mut state, &mut drafts, "/srv/");
        let on_devbox = list(&state).map(|list| list.folder.clone());
        if let Some(known) = state.machines.others.get_mut(&devbox) {
            known.folders_unknown = true;
        }
        state.machines.this.folders_unknown = false;
        type_text(&mut state, &mut drafts, "/srv/shop/");
        let older_devbox = list(&state).cloned();

        assert_eq!(on_devbox, Some(PathBuf::from("/srv")));
        assert_eq!(
            older_devbox, None,
            "a daemon without folders.list is not asked"
        );
    }

    #[test]
    fn only_an_absolute_folder_part_lists() {
        let listed_folder = |text| listed_folder(PathStyle::Unix, text);
        assert_eq!(
            listed_folder("/home/dev/pro"),
            Some(PathBuf::from("/home/dev"))
        );
        assert_eq!(listed_folder("/"), Some(PathBuf::from("/")));
        assert_eq!(listed_folder("/home"), Some(PathBuf::from("/")));
        assert_eq!(listed_folder("~/projects/"), None);
        assert_eq!(listed_folder("projects/"), None);
        assert_eq!(listed_folder("projects"), None);
        assert_eq!(listed_folder(r"C:\Users\"), None);
        assert_eq!(listed_folder(""), None);
    }

    #[test]
    fn on_windows_a_drive_lists_and_has_no_folder_above() {
        let windows = PathStyle::Windows;
        assert_eq!(
            listed_folder(windows, r"C:\Users\me\pro"),
            Some(PathBuf::from(r"C:\Users\me\"))
        );
        assert_eq!(
            listed_folder(windows, "c:/Users/"),
            Some(PathBuf::from("c:/Users/"))
        );
        assert_eq!(listed_folder(windows, r"C:\"), Some(PathBuf::from(r"C:\")));
        assert_eq!(listed_folder(windows, "/home/dev/"), None);
        assert_eq!(listed_folder(windows, r"\Users\"), None);
        assert_eq!(listed_folder(windows, "C:"), None);

        assert_eq!(up(windows, r"C:\Users\me\").as_deref(), Some(r"C:\Users\"));
        assert_eq!(up(windows, "C:/Users/").as_deref(), Some(r"C:\"));
        assert_eq!(up(windows, r"C:\"), None);
        assert_eq!(up(windows, "C:/"), None);
        assert_eq!(up(windows, r"Users\"), None);
        assert_eq!(up(windows, r"C:\Users\me\..\").as_deref(), Some(r"C:\"));

        let list = FolderList::new(PathBuf::from(r"C:\"));
        let text = r"C:\";
        assert!(
            rows(windows, &list, text).is_empty(),
            "the drive has no folder above"
        );
        let list = FolderList::new(PathBuf::from(r"C:\Users"));
        assert_eq!(rows(windows, &list, r"C:\Users\"), [FolderRow::Up]);
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
    fn a_folder_picked_in_the_system_dialog_closes_the_list_of_another_folder() {
        let (mut state, mut drafts) = opened();
        type_text(&mut state, &mut drafts, "/home/dev/pro");
        answer(&mut state, &["notes", "projects"]);

        assert!(pick(
            &mut state,
            &mut drafts,
            "/srv/data/reports".to_owned()
        ));

        assert_eq!(drafts.new_agent.folder, "/srv/data/reports");
        assert_eq!(list(&state), None);
        assert_eq!(
            take(&mut state, &mut drafts, None),
            None,
            "Enter keeps the folder picked"
        );
        assert_eq!(drafts.new_agent.folder, "/srv/data/reports");

        state.ui.close_dialog();
        assert!(!pick(&mut state, &mut drafts, "/srv/other".to_owned()));
        assert_eq!(drafts.new_agent.folder, "/srv/data/reports");
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
        assert_eq!(text_above(&state, &drafts), None);

        type_text(&mut state, &mut drafts, "/home/dev/projects/");
        assert_eq!(text_above(&state, &drafts).as_deref(), Some("/home/dev/"));
        type_text(&mut state, &mut drafts, "/home/dev/../");
        assert_eq!(text_above(&state, &drafts).as_deref(), Some("/"));
        type_text(&mut state, &mut drafts, "/home/dev/projects/");

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
        assert_eq!(text_above(&state, &drafts), None);
    }

    #[test]
    fn backspace_deletes_as_usual_in_a_text_the_field_does_not_list() {
        let (mut state, mut drafts) = opened();
        for text in ["~/projects/", "projects/", "projects/../"] {
            type_text(&mut state, &mut drafts, text);
            assert_eq!(text_above(&state, &drafts), None, "{text}");
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
