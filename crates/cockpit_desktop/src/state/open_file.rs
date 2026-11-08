//! The "Open file" dialog (Ctrl+O), as VS Code's remote "Open File": a path field over the entries
//! of the folder it names. What the field's text lists, which entries its last segment keeps, and
//! what the keys and clicks make of it.
//!
//! The field follows the keyboard alone: its text lives in the drafts, and a pick that changes it
//! returns the new text for the window to put in the field.

use std::path::{Path, PathBuf};

use cockpit_protocol::{Completeness, DirEntry, EntryKind};

use super::actions::{self, Direction};
use super::app_state::{AppState, Loadable};
use super::drafts::{Drafts, OpenFileDraft};
use super::ui_state::{Dialog, FileSearch, OpenFileView};
use crate::path_style::PathStyle;

/// The shortest bare name searched for in the project: one letter would match nearly every file.
const MIN_SEARCH_CHARS: usize = 2;

/// A row of the dialog's list, in the order it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row<'dialog> {
    /// "..": the folder above.
    Up,
    Entry(&'dialog DirEntry),
    /// A file of the project whose name holds the bare name typed.
    Match(&'dialog Path),
}

/// The folder the text lists: its folder part, absolute or relative to `agent_folder`, with its
/// ".." resolved, so that its parent is the folder above.
pub fn listed_folder(agent_folder: &Path, text: &str) -> PathBuf {
    let style = PathStyle::of(agent_folder);
    let (folder_part, _) = style.split(text);
    PathBuf::from(style.resolve(&agent_folder.to_string_lossy(), folder_part))
}

/// The name a text asks the project's files for: a bare name, without a separator, long enough.
pub fn searched_name(style: PathStyle, text: &str) -> Option<&str> {
    let name = text.trim();
    let long_enough = name.chars().count() >= MIN_SEARCH_CHARS;
    let bare = !text.contains(|character| style.is_separator(character));
    (bare && long_enough).then_some(name)
}

/// The text naming the folder above `folder`, the one `text` lists: "/work/app/src/" gives
/// "/work/app/". A relative text stays relative while the folder above is in `agent_folder`.
/// `None` at the top.
pub fn up(agent_folder: &Path, folder: &Path, text: &str) -> Option<String> {
    let style = PathStyle::of(agent_folder);
    let folder = folder.to_string_lossy();
    let above = style.parent(&folder)?;
    let (folder_part, _) = style.split(text);
    let relative = !style.is_absolute(folder_part)
        && !folder_part.starts_with(|character| style.is_separator(character));
    match style.strip_prefix(above, &agent_folder.to_string_lossy()) {
        Some("") if relative => Some(String::new()),
        Some(inside) if relative => Some(style.folder_text(inside)),
        _ => Some(style.folder_text(above)),
    }
}

/// The text after taking `name` from the listed folder. A folder's ends with a separator, so that
/// it lists.
pub fn completed(style: PathStyle, text: &str, name: &str, kind: EntryKind) -> String {
    let (folder_part, _) = style.split(text);
    let folder_part = style.shown(folder_part);
    if kind == EntryKind::Folder {
        format!("{folder_part}{name}{}", style.separator())
    } else {
        format!("{folder_part}{name}")
    }
}

/// `folder` as the field shows it, ending with a separator.
fn as_folder_text(folder: &Path) -> String {
    PathStyle::of(folder).folder_text(&folder.to_string_lossy())
}

/// The rules of the paths the dialog lists: its agent's folder's.
pub fn style(view: &OpenFileView) -> PathStyle {
    PathStyle::of(&view.folder)
}

fn state_style(state: &AppState) -> PathStyle {
    view(state).map_or_else(|| state.selected_machine_state().daemon.path_style(), style)
}

/// The rows for `text`: ".." while the last segment is empty, the listed entries the segment
/// keeps, then the project's files for a bare name. The entries whose name starts with the segment
/// come first, then those that hold it, whatever the case.
pub fn rows<'view>(view: &'view OpenFileView, text: &str) -> Vec<Row<'view>> {
    let style = style(view);
    let (_, segment) = style.split(text);
    let mut rows = Vec::new();
    if segment.is_empty() && style.parent(&view.folder.to_string_lossy()).is_some() {
        rows.push(Row::Up);
    }
    if let Some(listing) = view.listing.loaded() {
        rows.extend(
            matching(&listing.entries, segment)
                .into_iter()
                .map(Row::Entry),
        );
    }
    if let Some(FileSearch {
        found: Loadable::Loaded(matches),
        ..
    }) = &view.search
    {
        rows.extend(matches.iter().map(|path| Row::Match(path)));
    }
    rows
}

/// The entries a last segment keeps: those whose name starts with it first, then those that hold
/// it, whatever the case.
pub fn matching<'entries>(entries: &'entries [DirEntry], segment: &str) -> Vec<&'entries DirEntry> {
    let needle = segment.to_lowercase();
    let named = |entry: &&DirEntry| entry.name.to_lowercase();
    let starting = entries
        .iter()
        .filter(|entry| named(entry).starts_with(&needle));
    let holding = entries.iter().filter(|entry| {
        let name = named(entry);
        !name.starts_with(&needle) && name.contains(&needle)
    });
    starting.chain(holding).collect()
}

fn view(state: &AppState) -> Option<&OpenFileView> {
    match &state.ui.dialog {
        Some(Dialog::OpenFile(view)) => Some(view),
        _ => None,
    }
}

fn view_mut(state: &mut AppState) -> Option<&mut OpenFileView> {
    match &mut state.ui.dialog {
        Some(Dialog::OpenFile(view)) => Some(view),
        _ => None,
    }
}

/// Ctrl+O: opens the dialog on the selected agent's folder, or closes it. Another dialog stays.
pub fn toggle(state: &mut AppState, drafts: &mut Drafts) {
    match &state.ui.dialog {
        Some(Dialog::OpenFile(_)) => state.ui.dialog = None,
        Some(_) => {}
        None => open(state, drafts),
    }
}

/// Opens the dialog on the selected agent's folder.
pub fn open(state: &mut AppState, drafts: &mut Drafts) {
    let Some(entry) = state.selected() else {
        return;
    };
    let (agent, folder) = (entry.id(), entry.summary.folder.clone());
    drafts.open_file = OpenFileDraft {
        path: as_folder_text(&folder),
        highlighted: None,
        chosen: false,
    };
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::OpenFile(OpenFileView::new(agent, folder)));
}

/// What a new text in the field asks of the state, which typing leaves alone otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Typed {
    /// The text names another folder, or no longer a bare name: the dialog follows it at once.
    pub follow: bool,
    /// The text is a new bare name: the dialog searches for it after a pause.
    pub search: bool,
}

/// Keeps the typed text. The first row is highlighted while a segment filters the list.
pub fn type_path(state: &AppState, drafts: &mut Drafts, text: String) -> Typed {
    let style = state_style(state);
    let (_, segment) = style.split(&text);
    let filtering = !segment.is_empty();
    drafts.open_file.highlighted = filtering.then_some(0);
    drafts.open_file.chosen = false;
    drafts.open_file.path = text;
    let Some(view) = view(state) else {
        return Typed::default();
    };
    let Some(agent_folder) = agent_folder(state, view) else {
        return Typed::default();
    };
    let text = &drafts.open_file.path;
    let searched = view.search.as_ref().map(|search| search.name.as_str());
    let wanted = searched_name(style, text);
    let listed = listed_folder(agent_folder, text);
    let moved = !style.same_path(&listed.to_string_lossy(), &view.folder.to_string_lossy());
    Typed {
        follow: moved || (wanted.is_none() && searched.is_some()),
        search: wanted.is_some() && wanted != searched,
    }
}

fn agent_folder<'state>(state: &'state AppState, view: &OpenFileView) -> Option<&'state Path> {
    state
        .agents
        .get(&view.agent)
        .map(|entry| entry.summary.folder.as_path())
}

/// Whether `follow` searches for the text's bare name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameSearch {
    Start,
    /// Typing goes on: the search waits for a pause.
    Wait,
}

/// Makes the dialog list the folder the typed text names, and drops a search the text no longer
/// asks for.
pub fn follow(state: &mut AppState, drafts: &Drafts, search: NameSearch) {
    let text = drafts.open_file.path.as_str();
    let style = state_style(state);
    let Some(folder) = view(state).and_then(|view| agent_folder(state, view)) else {
        return;
    };
    let folder = listed_folder(folder, text);
    let Some(view) = view_mut(state) else {
        return;
    };
    if !style.same_path(&folder.to_string_lossy(), &view.folder.to_string_lossy()) {
        view.folder = folder;
        view.listing = Loadable::NotRequested;
    }
    match searched_name(style, text) {
        None => view.search = None,
        Some(name) => {
            let searched = view.search.as_ref().map(|search| search.name.as_str());
            if search == NameSearch::Start && searched != Some(name) {
                view.search = Some(FileSearch {
                    name: name.to_owned(),
                    found: Loadable::NotRequested,
                });
            }
        }
    }
}

/// Moves the highlight over the rows.
pub fn move_highlight(state: &AppState, drafts: &mut Drafts, direction: Direction) {
    let Some(view) = view(state) else {
        return;
    };
    let count = rows(view, &drafts.open_file.path).len();
    let Some(last) = count.checked_sub(1) else {
        return;
    };
    let shown = highlighted(view, &drafts.open_file);
    drafts.open_file.chosen = true;
    drafts.open_file.highlighted = Some(match (direction, shown) {
        (Direction::Down, None) => 0,
        (Direction::Down, Some(index)) => (index + 1).min(last),
        (Direction::Up, None) => last,
        (Direction::Up, Some(index)) => index.saturating_sub(1).min(last),
    });
}

/// The pointer moved onto the row at `index`: it takes the highlight and picks the row, as a key
/// does, so that a cut listing shows it too. A row of a list the text has since narrowed changes
/// nothing.
pub fn point(state: &AppState, drafts: &mut Drafts, index: usize) {
    let Some(view) = view(state) else {
        return;
    };
    if index < rows(view, &drafts.open_file.path).len() {
        drafts.open_file.highlighted = Some(index);
        drafts.open_file.chosen = true;
    }
}

/// The row the window highlights, which Enter and Tab take. A cut listing may leave out the file
/// the text names: until a key picks a row, a file row with another name is not highlighted, and
/// Enter opens the typed name.
pub fn highlighted(view: &OpenFileView, draft: &OpenFileDraft) -> Option<usize> {
    let index = draft.highlighted?;
    let cut = view
        .listing
        .loaded()
        .is_some_and(|listing| listing.completeness == Completeness::Truncated);
    if draft.chosen || !cut {
        return Some(index);
    }
    let (_, segment) = style(view).split(&draft.path);
    match rows(view, &draft.path).get(index)? {
        Row::Entry(entry) if entry.kind != EntryKind::Folder && entry.name != segment => None,
        _ => Some(index),
    }
}

/// How a row is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Take {
    /// Enter or a click: a folder lists, a file opens.
    Open,
    /// Tab: the field takes the row's name, and a file waits for Enter.
    Complete,
}

/// Takes the row at `index`, or else the highlighted one. With no row, Enter opens the typed path
/// as a file. Returns the field's new text, when it changes.
pub fn take(
    state: &mut AppState,
    drafts: &mut Drafts,
    index: Option<usize>,
    how: Take,
) -> Option<String> {
    let view = view(state)?;
    let agent = view.agent;
    let text = drafts.open_file.path.clone();
    let style = style(view);
    let (_, segment) = style.split(&text);
    let folder = view.folder.to_string_lossy().into_owned();
    let in_folder = |name: &str| PathBuf::from(style.join(&folder, name));
    let index = index.or_else(|| highlighted(view, &drafts.open_file));
    let row = index.and_then(|index| rows(view, &text).get(index).copied());
    let (new_text, file) = match row {
        None if how == Take::Open && !segment.is_empty() => (None, Some(in_folder(segment))),
        None => (None, None),
        Some(Row::Up) => (text_above_view(state, view, &text), None),
        Some(Row::Entry(entry)) if entry.kind == EntryKind::Folder => {
            let text = completed(style, &text, &entry.name, EntryKind::Folder);
            (Some(text), None)
        }
        Some(Row::Entry(entry)) => match how {
            Take::Open => (None, Some(in_folder(&entry.name))),
            Take::Complete => {
                let text = completed(style, &text, &entry.name, EntryKind::File);
                (Some(text), None)
            }
        },
        Some(Row::Match(path)) => match how {
            Take::Open => (None, Some(path.to_owned())),
            Take::Complete => {
                let shown = style.shown(&path.to_string_lossy()).into_owned();
                (Some(shown), None)
            }
        },
    };
    if let Some(file) = file {
        state.ui.dialog = None;
        actions::open_file(state, agent, file, None);
        return None;
    }
    let new_text = new_text?;
    set_text(state, drafts, new_text.clone());
    Some(new_text)
}

/// What Backspace makes of the field: on an empty last segment, the folder above's text. `None`
/// when the key should delete as usual. Changes nothing, so that a key that deletes does not
/// render the window.
pub fn text_above(state: &AppState, drafts: &Drafts) -> Option<String> {
    let text = &drafts.open_file.path;
    let view = view(state)?;
    let (_, segment) = style(view).split(text);
    if !segment.is_empty() {
        return None;
    }
    text_above_view(state, view, text)
}

fn text_above_view(state: &AppState, view: &OpenFileView, text: &str) -> Option<String> {
    up(agent_folder(state, view)?, &view.folder, text)
}

/// Whether the typed text is a bare name the dialog has not searched for yet.
pub fn needs_search(state: &AppState, drafts: &Drafts) -> bool {
    let Some(view) = view(state) else {
        return false;
    };
    let searched = view.search.as_ref().map(|search| search.name.as_str());
    let wanted = searched_name(style(view), &drafts.open_file.path);
    wanted.is_some() && wanted != searched
}

/// Puts `text` in the field's draft, and makes the dialog follow it.
pub fn set_text(state: &mut AppState, drafts: &mut Drafts, text: String) {
    drafts.open_file = OpenFileDraft {
        path: text,
        highlighted: None,
        chosen: false,
    };
    follow(state, drafts, NameSearch::Start);
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentId, AgentStatus, DirListing, EntryLink};

    use super::*;
    use crate::state::app_state::tests::{state_with, summary};

    const AGENT_FOLDER: &str = "/work/project-1";

    /// The agent's folder on a Windows daemon.
    const WINDOWS_FOLDER: &str = r"C:\work\project-1";

    fn entry(name: &str, kind: EntryKind) -> DirEntry {
        DirEntry {
            name: name.to_owned(),
            kind,
            size: (kind == EntryKind::File).then_some(10),
            link: EntryLink::Direct,
        }
    }

    fn listing(folder: &str) -> DirListing {
        DirListing {
            path: PathBuf::from(folder),
            entries: vec![
                entry("src", EntryKind::Folder),
                entry("tests", EntryKind::Folder),
                entry("Cargo.toml", EntryKind::File),
                entry("README.md", EntryKind::File),
                entry("build.rs", EntryKind::File),
            ],
            completeness: Completeness::Complete,
        }
    }

    /// The dialog open on agent 1's folder, listed.
    fn opened() -> (AppState, Drafts) {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(AgentId::new(1));
        let mut drafts = Drafts::default();
        toggle(&mut state, &mut drafts);
        if let Some(view) = view_mut(&mut state) {
            view.listing = Loadable::Loaded(listing(AGENT_FOLDER));
        }
        (state, drafts)
    }

    fn row_names(state: &AppState, drafts: &Drafts) -> Vec<String> {
        let view = view(state).unwrap();
        rows(view, &drafts.open_file.path)
            .into_iter()
            .map(|row| match row {
                Row::Up => "..".to_owned(),
                Row::Entry(entry) => entry.name.clone(),
                Row::Match(path) => path.display().to_string(),
            })
            .collect()
    }

    #[test]
    fn a_typed_path_splits_at_its_last_separator() {
        let split = |text| PathStyle::Unix.split(text);
        assert_eq!(split("/work/app/src/ma"), ("/work/app/src/", "ma"));
        assert_eq!(split("/work/app/"), ("/work/app/", ""));
        assert_eq!(split("main.rs"), ("", "main.rs"));
        assert_eq!(
            listed_folder(Path::new(AGENT_FOLDER), "src/ma"),
            PathBuf::from("/work/project-1/src")
        );
        assert_eq!(
            listed_folder(Path::new(AGENT_FOLDER), "/etc/"),
            PathBuf::from("/etc")
        );
        assert_eq!(
            listed_folder(Path::new(AGENT_FOLDER), "lexer"),
            PathBuf::from(AGENT_FOLDER)
        );
        let windows = Path::new(WINDOWS_FOLDER);
        assert_eq!(
            listed_folder(windows, "src/ma"),
            PathBuf::from(r"C:\work\project-1\src")
        );
        assert_eq!(listed_folder(windows, r"..\..\..\"), PathBuf::from(r"C:\"));
        assert_eq!(
            listed_folder(windows, "D:/data/"),
            PathBuf::from(r"D:\data")
        );
    }

    #[test]
    fn going_up_and_completing_rewrite_the_folder_part() {
        let up = |text: &str| {
            let folder = listed_folder(Path::new(AGENT_FOLDER), text);
            super::up(Path::new(AGENT_FOLDER), &folder, text)
        };
        assert_eq!(up("/work/app/src/").as_deref(), Some("/work/app/"));
        assert_eq!(up("/work/").as_deref(), Some("/"));
        assert_eq!(up("src/lib/").as_deref(), Some("src/"));
        assert_eq!(up("src/").as_deref(), Some(""));
        assert_eq!(up("").as_deref(), Some("/work/"));
        assert_eq!(up("../").as_deref(), Some("/"));
        assert_eq!(
            up("/work/project-1/src/").as_deref(),
            Some("/work/project-1/")
        );
        assert_eq!(up("/"), None);
        let unix = PathStyle::Unix;
        assert_eq!(
            completed(unix, "/work/app/sr", "src", EntryKind::Folder),
            "/work/app/src/"
        );
        assert_eq!(
            completed(unix, "/work/app/RE", "README.md", EntryKind::File),
            "/work/app/README.md"
        );
        assert_eq!(searched_name(unix, "lexer"), Some("lexer"));
        assert_eq!(searched_name(unix, "l"), None);
        assert_eq!(searched_name(unix, "src/lexer"), None);
        assert_eq!(searched_name(unix, r"src\lexer"), Some(r"src\lexer"));
    }

    #[test]
    fn on_windows_going_up_stops_at_the_drive_and_completing_shows_backslashes() {
        let up = |text: &str| {
            let folder = listed_folder(Path::new(WINDOWS_FOLDER), text);
            super::up(Path::new(WINDOWS_FOLDER), &folder, text)
        };
        assert_eq!(up(r"C:\work\app\src\").as_deref(), Some(r"C:\work\app\"));
        assert_eq!(up("C:/work/").as_deref(), Some(r"C:\"));
        assert_eq!(up(r"src\lib\").as_deref(), Some(r"src\"));
        assert_eq!(up("src/").as_deref(), Some(""));
        assert_eq!(up("").as_deref(), Some(r"C:\work\"));
        assert_eq!(
            up(r"c:\WORK\Project-1\src\").as_deref(),
            Some(r"c:\WORK\Project-1\")
        );
        assert_eq!(up(r"C:\"), None);
        assert_eq!(up("C:/"), None);
        let windows = PathStyle::Windows;
        assert_eq!(
            completed(windows, "C:/work/sr", "src", EntryKind::Folder),
            r"C:\work\src\"
        );
        assert_eq!(searched_name(windows, r"src\lexer"), None);
        assert_eq!(searched_name(windows, "src/lexer"), None);
    }

    #[test]
    fn the_last_segment_filters_the_entries_starting_ones_first() {
        let (state, mut drafts) = opened();
        assert_eq!(drafts.open_file.path, "/work/project-1/");
        assert_eq!(
            row_names(&state, &drafts),
            ["..", "src", "tests", "Cargo.toml", "README.md", "build.rs"]
        );

        type_path(&state, &mut drafts, "/work/project-1/r".to_owned());

        assert_eq!(
            row_names(&state, &drafts),
            ["README.md", "src", "Cargo.toml", "build.rs"]
        );
        assert_eq!(drafts.open_file.highlighted, Some(0));
    }

    #[test]
    fn enter_on_a_folder_lists_it_and_on_a_file_opens_it() {
        let (mut state, mut drafts) = opened();
        type_path(&state, &mut drafts, "/work/project-1/sr".to_owned());

        let text = take(&mut state, &mut drafts, None, Take::Open);

        assert_eq!(text.as_deref(), Some("/work/project-1/src/"));
        let view = view(&state).unwrap();
        assert_eq!(view.folder, PathBuf::from("/work/project-1/src"));
        assert_eq!(view.listing, Loadable::NotRequested);

        if let Some(view) = view_mut(&mut state) {
            view.listing = Loadable::Loaded(DirListing {
                entries: vec![entry("lexer.rs", EntryKind::File)],
                ..listing("/work/project-1/src")
            });
        }
        let text = take(&mut state, &mut drafts, Some(1), Take::Open);

        assert_eq!(text, None);
        assert_eq!(state.ui.dialog, None);
        let opened = state.ui.file_view(AgentId::new(1)).unwrap();
        assert_eq!(opened.path, PathBuf::from("src/lexer.rs"));
    }

    #[test]
    fn on_a_windows_daemon_the_dialog_lists_and_opens_with_backslashes() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(AgentId::new(1));
        if let Some(entry) = state.agents.get_mut(&AgentId::new(1)) {
            entry.summary.folder = PathBuf::from(WINDOWS_FOLDER);
        }
        let mut drafts = Drafts::default();
        toggle(&mut state, &mut drafts);
        assert_eq!(drafts.open_file.path, r"C:\work\project-1\");
        if let Some(view) = view_mut(&mut state) {
            view.listing = Loadable::Loaded(listing(WINDOWS_FOLDER));
        }

        // The same folder typed another way lists nothing new.
        let typed = type_path(&state, &mut drafts, "c:/WORK/project-1/sr".to_owned());
        assert!(!typed.follow);
        let text = take(&mut state, &mut drafts, None, Take::Open);

        assert_eq!(text.as_deref(), Some(r"c:\WORK\project-1\src\"));
        let shown = view(&state).unwrap();
        assert_eq!(shown.folder, PathBuf::from(r"c:\WORK\project-1\src"));
        if let Some(view) = view_mut(&mut state) {
            view.listing = Loadable::Loaded(DirListing {
                entries: vec![entry("lexer.rs", EntryKind::File)],
                ..listing(r"C:\work\project-1\src")
            });
        }
        take(&mut state, &mut drafts, Some(1), Take::Open);

        let opened = state.ui.file_view(AgentId::new(1)).unwrap();
        assert_eq!(opened.path, PathBuf::from(r"src\lexer.rs"));
    }

    #[test]
    fn tab_completes_a_name_and_backspace_goes_up() {
        let (mut state, mut drafts) = opened();
        type_path(&state, &mut drafts, "/work/project-1/READ".to_owned());

        let text = take(&mut state, &mut drafts, None, Take::Complete);

        assert_eq!(text.as_deref(), Some("/work/project-1/README.md"));
        assert!(view(&state).is_some(), "Tab keeps the dialog open");
        // Backspace on a segment deletes as usual.
        assert_eq!(text_above(&state, &drafts), None);

        type_path(&state, &mut drafts, "/work/project-1/src/".to_owned());
        follow(&mut state, &drafts, NameSearch::Wait);
        let text = text_above(&state, &drafts);

        assert_eq!(text.as_deref(), Some("/work/project-1/"));
    }

    #[test]
    fn the_up_row_of_the_agent_folder_typed_as_an_empty_text_lists_the_folder_above() {
        let (mut state, mut drafts) = opened();
        set_text(&mut state, &mut drafts, String::new());
        if let Some(view) = view_mut(&mut state) {
            view.listing = Loadable::Loaded(listing(AGENT_FOLDER));
        }
        assert_eq!(
            row_names(&state, &drafts).first().map(String::as_str),
            Some("..")
        );

        let text = take(&mut state, &mut drafts, Some(0), Take::Open);

        assert_eq!(text.as_deref(), Some("/work/"));
        assert_eq!(view(&state).unwrap().folder, PathBuf::from("/work"));
    }

    #[test]
    fn the_up_row_of_a_parent_folder_typed_relative_lists_the_folder_above_it() {
        let (mut state, mut drafts) = opened();
        set_text(&mut state, &mut drafts, "../".to_owned());
        assert_eq!(view(&state).unwrap().folder, PathBuf::from("/work"));

        let text = take(&mut state, &mut drafts, Some(0), Take::Open);

        assert_eq!(text.as_deref(), Some("/"));
        assert_eq!(view(&state).unwrap().folder, PathBuf::from("/"));
    }

    #[test]
    fn enter_on_a_full_name_a_cut_listing_left_out_opens_that_file() {
        let (mut state, mut drafts) = opened();
        if let Some(view) = view_mut(&mut state) {
            view.listing = Loadable::Loaded(DirListing {
                completeness: Completeness::Truncated,
                ..listing(AGENT_FOLDER)
            });
        }
        type_path(&state, &mut drafts, "/work/project-1/main.rs".to_owned());

        take(&mut state, &mut drafts, None, Take::Open);

        let opened = state.ui.file_view(AgentId::new(1)).unwrap();
        assert_eq!(opened.path, PathBuf::from("main.rs"));
    }

    #[test]
    fn enter_in_a_cut_listing_opens_the_typed_name_unless_a_key_picked_a_row() {
        let (mut state, mut drafts) = opened();
        let cut = |state: &mut AppState| {
            if let Some(view) = view_mut(state) {
                view.listing = Loadable::Loaded(DirListing {
                    completeness: Completeness::Truncated,
                    ..listing(AGENT_FOLDER)
                });
            }
        };
        cut(&mut state);
        type_path(&state, &mut drafts, "/work/project-1/build".to_owned());

        take(&mut state, &mut drafts, None, Take::Open);

        let shown = state.ui.file_view(AgentId::new(1)).unwrap();
        assert_eq!(shown.path, PathBuf::from("build"));

        let (mut state, mut drafts) = opened();
        cut(&mut state);
        type_path(&state, &mut drafts, "/work/project-1/build".to_owned());
        move_highlight(&state, &mut drafts, Direction::Down);

        take(&mut state, &mut drafts, None, Take::Open);

        let shown = state.ui.file_view(AgentId::new(1)).unwrap();
        assert_eq!(shown.path, PathBuf::from("build.rs"));

        // A folder's row still lists the folder.
        let (mut state, mut drafts) = opened();
        cut(&mut state);
        type_path(&state, &mut drafts, "/work/project-1/sr".to_owned());

        let text = take(&mut state, &mut drafts, None, Take::Open);

        assert_eq!(text.as_deref(), Some("/work/project-1/src/"));
    }

    #[test]
    fn a_cut_listing_highlights_only_a_row_enter_opens() {
        let cut = |folder: &str| {
            Loadable::Loaded(DirListing {
                path: PathBuf::from(folder),
                entries: vec![
                    entry("Cargo.toml", EntryKind::File),
                    entry("README.md", EntryKind::File),
                ],
                completeness: Completeness::Truncated,
            })
        };
        let shown = |state: &AppState, drafts: &Drafts| {
            highlighted(view(state).unwrap(), &drafts.open_file)
        };
        let (mut state, mut drafts) = opened();
        if let Some(view) = view_mut(&mut state) {
            view.listing = cut(AGENT_FOLDER);
        }
        type_path(&state, &mut drafts, "/work/project-1/Car".to_owned());
        assert_eq!(row_names(&state, &drafts), ["Cargo.toml"]);
        assert_eq!(shown(&state, &drafts), None);

        let mut typed = (state.clone(), drafts.clone());
        take(&mut typed.0, &mut typed.1, None, Take::Open);
        let opened_file = typed.0.ui.file_view(AgentId::new(1)).unwrap();
        assert_eq!(opened_file.path, PathBuf::from("Car"));

        move_highlight(&state, &mut drafts, Direction::Down);
        assert_eq!(shown(&state, &drafts), Some(0));
        take(&mut state, &mut drafts, None, Take::Open);
        let opened_file = state.ui.file_view(AgentId::new(1)).unwrap();
        assert_eq!(opened_file.path, PathBuf::from("Cargo.toml"));

        // The listing arrives after the text: the same rule.
        let (mut state, mut drafts) = opened();
        let typed = type_path(&state, &mut drafts, "/work/project-1/src/Car".to_owned());
        assert!(typed.follow);
        follow(&mut state, &drafts, NameSearch::Wait);
        if let Some(view) = view_mut(&mut state) {
            view.listing = cut("/work/project-1/src");
        }
        assert_eq!(row_names(&state, &drafts), ["Cargo.toml"]);
        assert_eq!(shown(&state, &drafts), None);

        // The row named as typed stays highlighted.
        type_path(
            &state,
            &mut drafts,
            "/work/project-1/src/README.md".to_owned(),
        );
        assert_eq!(shown(&state, &drafts), Some(0));
    }

    #[test]
    fn the_pointer_moves_the_highlight_within_the_rows_and_picks_a_row_of_a_cut_listing() {
        let (mut state, mut drafts) = opened();

        point(&state, &mut drafts, 5);
        assert_eq!(drafts.open_file.highlighted, Some(5));
        move_highlight(&state, &mut drafts, Direction::Up);
        assert_eq!(drafts.open_file.highlighted, Some(4));

        // A row the narrowed list no longer has: the pointer was on the old list.
        type_path(&state, &mut drafts, "/work/project-1/r".to_owned());
        point(&state, &mut drafts, 4);
        assert_eq!(drafts.open_file.highlighted, Some(0));

        // In a cut listing, the row under the pointer shows and opens, as one a key picked.
        if let Some(view) = view_mut(&mut state) {
            view.listing = Loadable::Loaded(DirListing {
                completeness: Completeness::Truncated,
                ..listing(AGENT_FOLDER)
            });
        }
        type_path(&state, &mut drafts, "/work/project-1/Car".to_owned());
        assert_eq!(highlighted(view(&state).unwrap(), &drafts.open_file), None);
        point(&state, &mut drafts, 0);
        assert_eq!(
            highlighted(view(&state).unwrap(), &drafts.open_file),
            Some(0)
        );
        take(&mut state, &mut drafts, None, Take::Open);
        let opened_file = state.ui.file_view(AgentId::new(1)).unwrap();
        assert_eq!(opened_file.path, PathBuf::from("Cargo.toml"));
    }

    #[test]
    fn a_bare_name_asks_for_a_search_after_the_folder() {
        let (mut state, mut drafts) = opened();

        let typed = type_path(&state, &mut drafts, "lex".to_owned());
        assert_eq!(
            typed,
            Typed {
                follow: false,
                search: true
            }
        );
        assert!(needs_search(&state, &drafts));
        follow(&mut state, &drafts, NameSearch::Start);
        assert!(!needs_search(&state, &drafts));
        let shown = view_mut(&mut state).unwrap();
        assert_eq!(
            shown.search,
            Some(FileSearch {
                name: "lex".to_owned(),
                found: Loadable::NotRequested,
            })
        );
        shown.search = Some(FileSearch {
            name: "lex".to_owned(),
            found: Loadable::Loaded(vec![PathBuf::from("/work/project-1/src/lexer.rs")]),
        });
        assert_eq!(row_names(&state, &drafts), ["/work/project-1/src/lexer.rs"]);

        // A path with a separator drops the search at once.
        let typed = type_path(&state, &mut drafts, "src/".to_owned());
        assert!(typed.follow && !typed.search);
        follow(&mut state, &drafts, NameSearch::Wait);
        assert_eq!(view(&state).unwrap().search, None);
    }

    #[test]
    fn the_keys_move_the_highlight_within_the_rows() {
        let (state, mut drafts) = opened();

        move_highlight(&state, &mut drafts, Direction::Down);
        assert_eq!(drafts.open_file.highlighted, Some(0));
        move_highlight(&state, &mut drafts, Direction::Up);
        assert_eq!(drafts.open_file.highlighted, Some(0));
        drafts.open_file.highlighted = None;
        move_highlight(&state, &mut drafts, Direction::Up);
        assert_eq!(drafts.open_file.highlighted, Some(5));
        move_highlight(&state, &mut drafts, Direction::Down);
        assert_eq!(drafts.open_file.highlighted, Some(5));
    }
}
