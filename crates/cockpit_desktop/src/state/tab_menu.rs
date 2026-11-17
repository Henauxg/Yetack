//! A file view tab's menu, opened by a right-click on the tab or on its row in the list of open
//! files: close it, the others, the ones before or after it, or all of them; copy its path; put it
//! in the message; or show it in the files pane.
//!
//! From the list, the tabs before and after are the rows the list shows above and below, and a
//! close keeps the list open.

use std::path::{Path, PathBuf};

use cockpit_protocol::AgentId;

use super::app_state::AppState;
use super::drafts::Drafts;
use super::ui_state::{Popover, TabMenuOrigin};
use super::{actions, file_edits, file_list, file_tree, terminals};
use crate::path_style::{self, PathStyle};

/// The tabs a close takes, around the tab whose menu is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabGroup {
    This,
    Others,
    /// On the left of the tab, or above its row in the list.
    Before,
    /// On the right of the tab, or below its row in the list.
    After,
    All,
}

/// A row of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabItem {
    Close(TabGroup),
    CopyPath,
    CopyRelativePath,
    Mention,
    ShowInPane,
}

impl TabItem {
    pub fn label(self, origin: TabMenuOrigin) -> &'static str {
        let list = origin == TabMenuOrigin::List;
        match self {
            Self::Close(TabGroup::This) => "Close",
            Self::Close(TabGroup::Others) => "Close the others",
            Self::Close(TabGroup::Before) if list => "Close the files above",
            Self::Close(TabGroup::Before) => "Close the tabs on the left",
            Self::Close(TabGroup::After) if list => "Close the files below",
            Self::Close(TabGroup::After) => "Close the tabs on the right",
            Self::Close(TabGroup::All) => "Close all",
            Self::CopyPath => "Copy the path",
            Self::CopyRelativePath => "Copy the relative path",
            Self::Mention => "Mention in the message",
            Self::ShowInPane => "Show in the files pane",
        }
    }

    /// The letter that picks it while the menu is open.
    pub fn key(self, origin: TabMenuOrigin) -> char {
        let list = origin == TabMenuOrigin::List;
        match self {
            Self::Close(TabGroup::This) => 'w',
            Self::Close(TabGroup::Others) => 'o',
            Self::Close(TabGroup::Before) if list => 'u',
            Self::Close(TabGroup::Before) => 'l',
            Self::Close(TabGroup::After) if list => 'd',
            Self::Close(TabGroup::After) => 'r',
            Self::Close(TabGroup::All) => 'a',
            Self::CopyPath => 'c',
            Self::CopyRelativePath => 'e',
            Self::Mention => 'm',
            Self::ShowInPane => 'f',
        }
    }
}

/// A row of the menu, and whether it can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabRow {
    pub item: TabItem,
    pub enabled: bool,
}

/// What the open menu is about.
#[derive(Debug, Clone, PartialEq)]
pub struct TabTarget {
    pub agent: AgentId,
    pub x: i32,
    pub y: i32,
    pub origin: TabMenuOrigin,
    /// The tab, as the file view keeps it: relative to the agent's folder when it is inside it.
    pub path: PathBuf,
    pub absolute: PathBuf,
    /// The path as paths show.
    pub full: String,
    /// The path from the agent's folder, for a file inside it.
    pub relative: Option<String>,
    pub rows: Vec<TabRow>,
}

pub fn open(
    state: &mut AppState,
    agent: AgentId,
    path: &Path,
    x: i32,
    y: i32,
    origin: TabMenuOrigin,
) {
    let Some(view) = state.ui.file_views.get_mut(&agent) else {
        return;
    };
    view.menu = Some(path.to_owned());
    state.ui.popover = Some(Popover::TabMenu {
        agent,
        x,
        y,
        origin,
    });
}

/// Closes the menu: opened from the list of open files, it goes back to the list.
pub fn close_menu(state: &mut AppState) {
    if let Some(Popover::TabMenu { agent, .. }) = state.ui.popover {
        state.ui.close_popover();
        if let Some(view) = state.ui.file_views.get_mut(&agent) {
            view.menu = None;
        }
    }
}

/// The open menu, with its rows.
pub fn target(state: &AppState, drafts: &Drafts) -> Option<TabTarget> {
    let Some(Popover::TabMenu {
        agent,
        x,
        y,
        origin,
    }) = state.ui.popover
    else {
        return None;
    };
    let entry = state.agents.get(&agent)?;
    let path = state.ui.file_view(agent)?.menu.clone()?;
    let around = group_tabs(state, drafts, agent, origin);
    let at = around.iter().position(|tab| *tab == path)?;
    let before = at > 0;
    let after = at + 1 < around.len();
    let folder = &entry.summary.folder;
    let style = PathStyle::of(folder);
    let absolute = PathBuf::from(style.resolve(&folder.to_string_lossy(), &path.to_string_lossy()));
    let relative = file_edits::shown_path(folder, &absolute);
    let relative = (!style.is_absolute(&relative)).then_some(relative);
    let row = |item: TabItem, enabled: bool| TabRow { item, enabled };
    let mut rows = vec![
        row(TabItem::Close(TabGroup::This), true),
        row(TabItem::Close(TabGroup::Others), before || after),
        row(TabItem::Close(TabGroup::Before), before),
        row(TabItem::Close(TabGroup::After), after),
        row(TabItem::Close(TabGroup::All), true),
        row(TabItem::CopyPath, true),
    ];
    if relative.is_some() {
        rows.push(row(TabItem::CopyRelativePath, true));
    }
    rows.extend([
        row(TabItem::Mention, true),
        row(
            TabItem::ShowInPane,
            file_tree::can_show(state, agent, &path),
        ),
    ]);
    Some(TabTarget {
        agent,
        x,
        y,
        origin,
        full: path_style::shown(&absolute),
        relative,
        path,
        absolute,
        rows,
    })
}

/// The agent's tabs, in their order.
pub fn tabs(state: &AppState, agent: AgentId) -> Vec<PathBuf> {
    let (Some(entry), Some(view)) = (state.agents.get(&agent), state.ui.file_view(agent)) else {
        return Vec::new();
    };
    view.tabs(&entry.changed_paths())
}

/// The tabs a group is taken from: every tab, or the rows the list shows.
fn group_tabs(
    state: &AppState,
    drafts: &Drafts,
    agent: AgentId,
    origin: TabMenuOrigin,
) -> Vec<PathBuf> {
    match origin {
        TabMenuOrigin::Tabs => tabs(state, agent),
        TabMenuOrigin::List => file_list::rows(state, drafts, agent),
    }
}

/// The tabs `group` takes around `path`. None when `path` is no tab.
pub fn taken(tabs: &[PathBuf], path: &Path, group: TabGroup) -> Vec<PathBuf> {
    let Some(at) = tabs.iter().position(|tab| tab == path) else {
        return Vec::new();
    };
    let (before, rest) = tabs.split_at(at);
    let after = rest.get(1..).unwrap_or_default();
    match group {
        TabGroup::This => vec![path.to_owned()],
        TabGroup::Others => before.iter().chain(after).cloned().collect(),
        TabGroup::Before => before.to_vec(),
        TabGroup::After => after.to_vec(),
        TabGroup::All => tabs.to_vec(),
    }
}

/// Closes the tabs `group` takes around `path`. When the shown file goes with them and `path`
/// stays, `path` shows in its place. From the list, the list stays open on `path`'s row.
pub fn close(
    state: &mut AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    path: &Path,
    group: TabGroup,
    origin: TabMenuOrigin,
) {
    let closing = taken(&group_tabs(state, drafts, agent, origin), path, group);
    close_menu(state);
    let terminal_shown = state.ui.terminal_tab_shown.contains(&agent);
    let shown = |state: &AppState| state.ui.file_view(agent).map(|view| view.path.clone());
    if shown(state).is_some_and(|shown| closing.contains(&shown))
        && !closing.iter().any(|tab| tab == path)
    {
        actions::open_file(state, agent, path.to_owned(), None);
    }
    // The shown tab goes last: closing it shows a neighbour, which would read that file first.
    let shown_now = shown(state);
    for tab in closing
        .iter()
        .filter(|tab| Some(*tab) != shown_now.as_ref())
    {
        actions::close_tab(state, agent, tab);
    }
    if let Some(shown_now) = shown_now.filter(|shown_now| closing.contains(shown_now)) {
        actions::close_tab(state, agent, &shown_now);
    }
    // Files closed behind the terminal's tab leave the terminal shown.
    if terminal_shown {
        terminals::show_tab(state, agent);
    }
    if origin == TabMenuOrigin::List {
        file_list::after_close(state, drafts, agent, path);
    }
}
#[cfg(test)]
mod tests {
    use cockpit_protocol::AgentStatus;

    use super::*;
    use crate::state::{state_with, summary};

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    /// The agent with the tabs `a.rs` to `d.rs` open, `shown` on screen.
    fn with_tabs(shown: &str) -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        for name in ["a.rs", "b.rs", "c.rs", "d.rs"] {
            actions::open_file(&mut state, agent(), PathBuf::from(name), None);
        }
        actions::open_file(&mut state, agent(), PathBuf::from(shown), None);
        state
    }

    fn names(state: &AppState) -> (Vec<String>, Option<String>) {
        let tabs = tabs(state, agent())
            .iter()
            .map(|tab| tab.display().to_string())
            .collect();
        let shown = state
            .ui
            .file_view(agent())
            .map(|view| view.path.display().to_string());
        (tabs, shown)
    }

    #[test]
    fn each_group_takes_its_tabs_around_the_tab() {
        let tabs: Vec<PathBuf> = ["a", "b", "c", "d"].map(PathBuf::from).to_vec();
        let around_c = |which| taken(&tabs, Path::new("c"), which);

        assert_eq!(around_c(TabGroup::This), [PathBuf::from("c")]);
        assert_eq!(
            around_c(TabGroup::Others),
            ["a", "b", "d"].map(PathBuf::from)
        );
        assert_eq!(around_c(TabGroup::Before), ["a", "b"].map(PathBuf::from));
        assert_eq!(around_c(TabGroup::After), [PathBuf::from("d")]);
        assert_eq!(around_c(TabGroup::All), tabs);
        assert!(taken(&tabs, Path::new("z"), TabGroup::All).is_empty());
    }

    #[test]
    fn closing_the_others_shows_the_tab_in_place_of_the_shown_one() {
        let mut state = with_tabs("b.rs");

        close(
            &mut state,
            &mut Drafts::default(),
            agent(),
            Path::new("c.rs"),
            TabGroup::Others,
            TabMenuOrigin::Tabs,
        );

        assert_eq!(names(&state), (vec!["c.rs".into()], Some("c.rs".into())));
    }

    #[test]
    fn closing_on_the_right_keeps_the_shown_tab_on_the_left() {
        let mut state = with_tabs("a.rs");

        close(
            &mut state,
            &mut Drafts::default(),
            agent(),
            Path::new("b.rs"),
            TabGroup::After,
            TabMenuOrigin::Tabs,
        );

        assert_eq!(
            names(&state),
            (vec!["a.rs".into(), "b.rs".into()], Some("a.rs".into()))
        );
    }

    #[test]
    fn closing_on_the_left_of_the_shown_tab_keeps_it_shown() {
        let mut state = with_tabs("c.rs");

        close(
            &mut state,
            &mut Drafts::default(),
            agent(),
            Path::new("c.rs"),
            TabGroup::Before,
            TabMenuOrigin::Tabs,
        );

        assert_eq!(
            names(&state),
            (vec!["c.rs".into(), "d.rs".into()], Some("c.rs".into()))
        );
    }

    #[test]
    fn closing_all_closes_the_file_view_and_the_menu() {
        let mut state = with_tabs("b.rs");
        open(
            &mut state,
            agent(),
            Path::new("c.rs"),
            10,
            20,
            TabMenuOrigin::Tabs,
        );

        close(
            &mut state,
            &mut Drafts::default(),
            agent(),
            Path::new("c.rs"),
            TabGroup::All,
            TabMenuOrigin::Tabs,
        );

        assert_eq!(names(&state), (Vec::new(), None));
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn closing_one_tab_shows_its_neighbour_only_when_it_was_shown() {
        let mut other = with_tabs("b.rs");
        let mut shown = with_tabs("b.rs");

        close(
            &mut other,
            &mut Drafts::default(),
            agent(),
            Path::new("c.rs"),
            TabGroup::This,
            TabMenuOrigin::Tabs,
        );
        close(
            &mut shown,
            &mut Drafts::default(),
            agent(),
            Path::new("b.rs"),
            TabGroup::This,
            TabMenuOrigin::Tabs,
        );

        assert_eq!(other.ui.file_view(agent()).unwrap().path, Path::new("b.rs"));
        assert_eq!(shown.ui.file_view(agent()).unwrap().path, Path::new("c.rs"));
    }

    #[test]
    fn closing_on_the_right_of_a_tab_left_of_the_shown_one_shows_it() {
        let mut state = with_tabs("d.rs");

        close(
            &mut state,
            &mut Drafts::default(),
            agent(),
            Path::new("b.rs"),
            TabGroup::After,
            TabMenuOrigin::Tabs,
        );

        assert_eq!(
            names(&state),
            (vec!["a.rs".into(), "b.rs".into()], Some("b.rs".into()))
        );
    }

    #[test]
    fn a_changed_file_closed_with_the_others_comes_back_when_opened() {
        use cockpit_protocol::{AgentDetails, ChangedFile};

        use crate::state::Loadable;

        let mut state = with_tabs("a.rs");
        state.agents.get_mut(&agent()).unwrap().details = Loadable::Loaded(AgentDetails {
            changed_files: vec![ChangedFile {
                path: PathBuf::from("/work/project-1/x.rs"),
                lines_added: 1,
                lines_removed: 0,
            }],
            ..AgentDetails::default()
        });

        close(
            &mut state,
            &mut Drafts::default(),
            agent(),
            Path::new("a.rs"),
            TabGroup::Others,
            TabMenuOrigin::Tabs,
        );
        let after_close = names(&state).0;
        actions::open_file(&mut state, agent(), PathBuf::from("x.rs"), None);

        assert_eq!(after_close, ["a.rs"]);
        assert!(names(&state).0.contains(&"x.rs".to_owned()));
    }

    #[test]
    fn closing_the_file_view_closes_its_tab_menu() {
        let mut state = with_tabs("b.rs");
        open(
            &mut state,
            agent(),
            Path::new("b.rs"),
            10,
            20,
            TabMenuOrigin::Tabs,
        );

        actions::close_file_view(&mut state, agent());

        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn showing_the_shown_tab_in_the_files_pane_keeps_its_marked_line() {
        use crate::state::file_tree;

        let mut state = with_tabs("a.rs");
        actions::open_file(&mut state, agent(), PathBuf::from("b.rs"), Some(6));

        file_tree::show_in_pane(&mut state, agent(), Path::new("b.rs"));

        assert_eq!(state.ui.file_view(agent()).unwrap().line, Some(6));
        assert!(file_tree::pane_open(&state, agent()));
    }

    /// The open menu's rows by their labels, a row that cannot run in brackets.
    fn menu(state: &AppState, drafts: &Drafts) -> Vec<String> {
        let target = target(state, drafts).unwrap();
        target
            .rows
            .iter()
            .map(|row| {
                let label = row.item.label(target.origin);
                if row.enabled {
                    label.to_owned()
                } else {
                    format!("({label})")
                }
            })
            .collect()
    }

    #[test]
    fn the_menu_greys_out_a_group_with_no_tab() {
        let mut state = with_tabs("a.rs");
        open(
            &mut state,
            agent(),
            Path::new("a.rs"),
            10,
            20,
            TabMenuOrigin::Tabs,
        );

        assert_eq!(
            menu(&state, &Drafts::default()),
            [
                "Close",
                "Close the others",
                "(Close the tabs on the left)",
                "Close the tabs on the right",
                "Close all",
                "Copy the path",
                "Copy the relative path",
                "Mention in the message",
                "Show in the files pane",
            ]
        );
    }

    #[test]
    fn show_in_the_files_pane_is_greyed_out_for_a_file_the_pane_cannot_show() {
        let mut state = with_tabs("a.rs");
        let elsewhere = Path::new("/opt/tools/config.toml");
        actions::open_file(&mut state, agent(), elsewhere.to_owned(), None);
        open(&mut state, agent(), elsewhere, 10, 20, TabMenuOrigin::Tabs);

        let rows = menu(&state, &Drafts::default());

        assert_eq!(
            rows.last().map(String::as_str),
            Some("(Show in the files pane)")
        );
        assert!(!rows.contains(&"Copy the relative path".to_owned()));
    }

    #[test]
    fn each_menu_gives_each_row_its_own_letter() {
        let items = [
            TabItem::Close(TabGroup::This),
            TabItem::Close(TabGroup::Others),
            TabItem::Close(TabGroup::Before),
            TabItem::Close(TabGroup::After),
            TabItem::Close(TabGroup::All),
            TabItem::CopyPath,
            TabItem::CopyRelativePath,
            TabItem::Mention,
            TabItem::ShowInPane,
        ];
        for origin in [TabMenuOrigin::Tabs, TabMenuOrigin::List] {
            let mut letters: Vec<char> = items.iter().map(|item| item.key(origin)).collect();
            letters.sort_unstable();
            letters.dedup();

            assert_eq!(letters.len(), items.len(), "{origin:?}");
        }
    }

    /// The agent with `src/lexer.rs`, `tests/lexer.rs` and `src/parser.rs` open, and the list of
    /// open files narrowed to `src`.
    fn listed_src() -> (AppState, Drafts) {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        for name in ["src/lexer.rs", "tests/lexer.rs", "src/parser.rs"] {
            actions::open_file(&mut state, agent(), PathBuf::from(name), None);
        }
        let mut drafts = Drafts::default();
        file_list::toggle(&mut state, &mut drafts, agent());
        file_list::filter(&state, &mut drafts, agent(), "src".to_owned());
        (state, drafts)
    }

    #[test]
    fn from_the_list_the_menu_closes_the_rows_shown_below_and_the_list_stays() {
        let (mut state, mut drafts) = listed_src();
        file_list::open_menu(&mut state, &mut drafts, agent(), 0, 10, 20);
        let rows = menu(&state, &drafts);

        close(
            &mut state,
            &mut drafts,
            agent(),
            Path::new("src/lexer.rs"),
            TabGroup::After,
            TabMenuOrigin::List,
        );

        assert_eq!(
            rows.get(2..4).unwrap(),
            ["(Close the files above)", "Close the files below"]
        );
        assert_eq!(names(&state).0, ["src/lexer.rs", "tests/lexer.rs"]);
        assert_eq!(state.ui.popover, Some(Popover::FileList));
        assert_eq!(drafts.file_list.highlighted, 0);
    }

    #[test]
    fn closing_the_shown_file_from_the_list_keeps_the_list_open() {
        let (mut state, mut drafts) = listed_src();
        file_list::open_menu(&mut state, &mut drafts, agent(), 1, 10, 20);

        close(
            &mut state,
            &mut drafts,
            agent(),
            Path::new("src/parser.rs"),
            TabGroup::This,
            TabMenuOrigin::List,
        );

        assert_eq!(names(&state).0, ["src/lexer.rs", "tests/lexer.rs"]);
        assert_eq!(state.ui.popover, Some(Popover::FileList));
        assert_eq!(drafts.file_list.highlighted, 0);
    }

    #[test]
    fn esc_or_a_click_beside_a_menu_from_the_list_goes_back_to_the_list() {
        let (mut state, mut drafts) = listed_src();
        file_list::open_menu(&mut state, &mut drafts, agent(), 1, 10, 20);
        let highlighted = drafts.file_list.highlighted;

        let closed = state.ui.close_overlays();
        let after_escape = state.ui.popover;
        file_list::open_menu(&mut state, &mut drafts, agent(), 1, 10, 20);
        state.ui.close_popover();
        let after_click = state.ui.popover;
        state.ui.close_overlays();

        assert!(closed);
        assert_eq!(highlighted, 1);
        assert_eq!(after_escape, Some(Popover::FileList));
        assert_eq!(after_click, Some(Popover::FileList));
        assert_eq!(state.ui.popover, None, "a second Esc closes the list");
    }

    #[test]
    fn copying_from_the_list_goes_back_to_the_list_and_from_a_tab_closes() {
        let (mut state, mut drafts) = listed_src();
        file_list::open_menu(&mut state, &mut drafts, agent(), 0, 10, 20);

        close_menu(&mut state);
        let from_list = state.ui.popover;
        open(
            &mut state,
            agent(),
            Path::new("src/lexer.rs"),
            10,
            20,
            TabMenuOrigin::Tabs,
        );
        close_menu(&mut state);

        assert_eq!(from_list, Some(Popover::FileList));
        assert_eq!(state.ui.popover, None);
    }
}
