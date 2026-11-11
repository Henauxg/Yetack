//! A file view tab's menu, opened by a right-click on the tab: close it, the others, the ones on
//! its left or its right, or all of them.

use std::path::{Path, PathBuf};

use cockpit_protocol::AgentId;

use super::actions;
use super::app_state::AppState;
use super::ui_state::Popover;

/// The tabs a close takes, around the tab whose menu is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabGroup {
    This,
    Others,
    Left,
    Right,
    All,
}

pub fn open(state: &mut AppState, agent: AgentId, path: &Path, x: i32, y: i32) {
    let Some(view) = state.ui.file_views.get_mut(&agent) else {
        return;
    };
    view.menu = Some(path.to_owned());
    state.ui.popover = Some(Popover::TabMenu { agent, x, y });
}

pub fn close_menu(state: &mut AppState) {
    if let Some(Popover::TabMenu { agent, .. }) = state.ui.popover {
        state.ui.popover = None;
        if let Some(view) = state.ui.file_views.get_mut(&agent) {
            view.menu = None;
        }
    }
}

/// The agent's tabs, in their order.
pub fn tabs(state: &AppState, agent: AgentId) -> Vec<PathBuf> {
    let (Some(entry), Some(view)) = (state.agents.get(&agent), state.ui.file_view(agent)) else {
        return Vec::new();
    };
    view.tabs(&entry.changed_paths())
}

/// The tabs `group` takes around `path`. None when `path` is no tab.
pub fn taken(tabs: &[PathBuf], path: &Path, group: TabGroup) -> Vec<PathBuf> {
    let Some(at) = tabs.iter().position(|tab| tab == path) else {
        return Vec::new();
    };
    let (left, rest) = tabs.split_at(at);
    let right = rest.get(1..).unwrap_or_default();
    match group {
        TabGroup::This => vec![path.to_owned()],
        TabGroup::Others => left.iter().chain(right).cloned().collect(),
        TabGroup::Left => left.to_vec(),
        TabGroup::Right => right.to_vec(),
        TabGroup::All => tabs.to_vec(),
    }
}

/// Closes the tabs `group` takes around `path`. When the shown file goes with them and `path`
/// stays, `path` shows in its place.
pub fn close(state: &mut AppState, agent: AgentId, path: &Path, group: TabGroup) {
    close_menu(state);
    let closing = taken(&tabs(state, agent), path, group);
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
        assert_eq!(around_c(TabGroup::Left), ["a", "b"].map(PathBuf::from));
        assert_eq!(around_c(TabGroup::Right), [PathBuf::from("d")]);
        assert_eq!(around_c(TabGroup::All), tabs);
        assert!(taken(&tabs, Path::new("z"), TabGroup::All).is_empty());
    }

    #[test]
    fn closing_the_others_shows_the_tab_in_place_of_the_shown_one() {
        let mut state = with_tabs("b.rs");

        close(&mut state, agent(), Path::new("c.rs"), TabGroup::Others);

        assert_eq!(names(&state), (vec!["c.rs".into()], Some("c.rs".into())));
    }

    #[test]
    fn closing_on_the_right_keeps_the_shown_tab_on_the_left() {
        let mut state = with_tabs("a.rs");

        close(&mut state, agent(), Path::new("b.rs"), TabGroup::Right);

        assert_eq!(
            names(&state),
            (vec!["a.rs".into(), "b.rs".into()], Some("a.rs".into()))
        );
    }

    #[test]
    fn closing_on_the_left_of_the_shown_tab_keeps_it_shown() {
        let mut state = with_tabs("c.rs");

        close(&mut state, agent(), Path::new("c.rs"), TabGroup::Left);

        assert_eq!(
            names(&state),
            (vec!["c.rs".into(), "d.rs".into()], Some("c.rs".into()))
        );
    }

    #[test]
    fn closing_all_closes_the_file_view_and_the_menu() {
        let mut state = with_tabs("b.rs");
        open(&mut state, agent(), Path::new("c.rs"), 10, 20);

        close(&mut state, agent(), Path::new("c.rs"), TabGroup::All);

        assert_eq!(names(&state), (Vec::new(), None));
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn closing_one_tab_shows_its_neighbour_only_when_it_was_shown() {
        let mut other = with_tabs("b.rs");
        let mut shown = with_tabs("b.rs");

        close(&mut other, agent(), Path::new("c.rs"), TabGroup::This);
        close(&mut shown, agent(), Path::new("b.rs"), TabGroup::This);

        assert_eq!(other.ui.file_view(agent()).unwrap().path, Path::new("b.rs"));
        assert_eq!(shown.ui.file_view(agent()).unwrap().path, Path::new("c.rs"));
    }

    #[test]
    fn closing_on_the_right_of_a_tab_left_of_the_shown_one_shows_it() {
        let mut state = with_tabs("d.rs");

        close(&mut state, agent(), Path::new("b.rs"), TabGroup::Right);

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

        close(&mut state, agent(), Path::new("a.rs"), TabGroup::Others);
        let after_close = names(&state).0;
        actions::open_file(&mut state, agent(), PathBuf::from("x.rs"), None);

        assert_eq!(after_close, ["a.rs"]);
        assert!(names(&state).0.contains(&"x.rs".to_owned()));
    }

    #[test]
    fn closing_the_file_view_closes_its_tab_menu() {
        let mut state = with_tabs("b.rs");
        open(&mut state, agent(), Path::new("b.rs"), 10, 20);

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
}
