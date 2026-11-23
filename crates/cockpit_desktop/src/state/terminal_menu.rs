//! The menu a right-click in a terminal opens at the pointer: the link under the pointer first,
//! then the clipboard, then the terminal's own actions.

use cockpit_protocol::{AgentId, TerminalId};
use serde::Deserialize;

use super::app_state::AppState;
use super::ui_state::Popover;
use super::{tab_menu, terminals};
use crate::prefs::TerminalPlace;

/// The link under the pointer, as the page found it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalLink {
    Web(String),
    File { path: String, line: u32 },
}

impl TerminalLink {
    /// The text "Copy the address" or "Copy the path" copies.
    pub fn text(&self) -> String {
        match self {
            Self::Web(url) => url.clone(),
            Self::File { path, line } => format!("{path}:{line}"),
        }
    }
}

/// Where the menu opened, in the window's pixels, and what was under the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuSpot {
    pub agent: AgentId,
    pub terminal: TerminalId,
    pub x: i32,
    pub y: i32,
    /// The terminal has a selection to copy.
    pub selection: bool,
    pub link: Option<TerminalLink>,
}

/// A row of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalItem {
    OpenLink,
    CopyLink,
    Copy,
    Paste,
    SelectAll,
    Clear,
    NewTerminal,
    MoveTo(TerminalPlace),
    Close,
}

impl TerminalItem {
    /// `link` is the one under the pointer, which names the link's rows.
    pub fn label(self, link: Option<&TerminalLink>) -> &'static str {
        let file = matches!(link, Some(TerminalLink::File { .. }));
        match self {
            Self::OpenLink if file => "Open in the file view",
            Self::OpenLink => "Open in the browser",
            Self::CopyLink if file => tab_menu::COPY_PATH_LABEL,
            Self::CopyLink => "Copy the address",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            // ghostty-web selects the rows on screen, not the ones scrolled away.
            Self::SelectAll => "Select the screen",
            Self::Clear => "Clear the screen",
            Self::NewTerminal => "New terminal",
            Self::MoveTo(TerminalPlace::Bottom) => "Move to the bottom panel",
            Self::MoveTo(TerminalPlace::Right) => "Move to the right pane",
            Self::MoveTo(TerminalPlace::FileView) => "Move to a file view tab",
            Self::Close => "Close this terminal",
        }
    }

    /// The letter that picks it while the menu is open.
    pub fn key(self) -> char {
        match self {
            Self::OpenLink => 'o',
            Self::CopyLink => 'd',
            Self::Copy => 'c',
            Self::Paste => 'v',
            Self::SelectAll => 's',
            Self::Clear => 'l',
            Self::NewTerminal => 'n',
            Self::MoveTo(TerminalPlace::Bottom) => 'b',
            Self::MoveTo(TerminalPlace::Right) => 'r',
            Self::MoveTo(TerminalPlace::FileView) => 'f',
            Self::Close => 'x',
        }
    }
}

/// A row, whether it can run, and whether a line divides it from the row above.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalRow {
    pub item: TerminalItem,
    pub enabled: bool,
    pub divided: bool,
}

/// A right-click in a terminal.
pub fn open(state: &mut AppState, spot: MenuSpot) {
    state.ui.terminal_menu = Some(spot);
    state.ui.popover = Some(Popover::TerminalMenu);
}

pub fn close_menu(state: &mut AppState) {
    state
        .ui
        .popover
        .take_if(|popover| *popover == Popover::TerminalMenu);
    state.ui.terminal_menu = None;
}

/// Closes the menu when `gone` says its terminal went, so that its rows reach no other terminal
/// that takes the same id.
pub fn close_if(state: &mut AppState, gone: impl FnOnce(&MenuSpot) -> bool) {
    if state.ui.terminal_menu.as_ref().is_some_and(gone) {
        close_menu(state);
    }
}

/// The open menu and its rows, while its terminal still shows. A terminal that ended, or one
/// that a restarted daemon numbered the same, takes no row.
pub fn target(state: &AppState) -> Option<(MenuSpot, Vec<TerminalRow>)> {
    if state.ui.popover != Some(Popover::TerminalMenu) {
        return None;
    }
    let spot = state
        .ui
        .terminal_menu
        .clone()
        .filter(|spot| terminals::shows(state, spot.agent, spot.terminal))?;
    let rows = rows(&spot, terminals::place(state, spot.agent));
    Some((spot, rows))
}

/// The link's rows when there is a link, the clipboard's, Clear, then a new terminal, the places
/// the terminal can move to, and Close.
fn rows(spot: &MenuSpot, place: TerminalPlace) -> Vec<TerminalRow> {
    let row = |item, divided| TerminalRow {
        item,
        enabled: true,
        divided,
    };
    let mut rows = Vec::new();
    if spot.link.is_some() {
        rows.extend([
            row(TerminalItem::OpenLink, false),
            row(TerminalItem::CopyLink, false),
        ]);
    }
    rows.extend([
        TerminalRow {
            item: TerminalItem::Copy,
            enabled: spot.selection,
            divided: spot.link.is_some(),
        },
        row(TerminalItem::Paste, false),
        row(TerminalItem::SelectAll, false),
        row(TerminalItem::Clear, true),
        row(TerminalItem::NewTerminal, true),
    ]);
    rows.extend(
        [
            TerminalPlace::Bottom,
            TerminalPlace::Right,
            TerminalPlace::FileView,
        ]
        .into_iter()
        .filter(|other| *other != place)
        .map(|other| row(TerminalItem::MoveTo(other), false)),
    );
    rows.push(row(TerminalItem::Close, true));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(selection: bool, link: Option<TerminalLink>) -> MenuSpot {
        MenuSpot {
            agent: AgentId::new(1),
            terminal: TerminalId::new(2),
            x: 40,
            y: 60,
            selection,
            link,
        }
    }

    fn items(rows: &[TerminalRow]) -> Vec<(TerminalItem, bool, bool)> {
        rows.iter()
            .map(|row| (row.item, row.enabled, row.divided))
            .collect()
    }

    #[test]
    fn without_a_link_the_clipboard_comes_first_and_copy_waits_for_a_selection() {
        let rows = rows(&spot(false, None), TerminalPlace::Bottom);

        assert_eq!(
            items(&rows),
            [
                (TerminalItem::Copy, false, false),
                (TerminalItem::Paste, true, false),
                (TerminalItem::SelectAll, true, false),
                (TerminalItem::Clear, true, true),
                (TerminalItem::NewTerminal, true, true),
                (TerminalItem::MoveTo(TerminalPlace::Right), true, false),
                (TerminalItem::MoveTo(TerminalPlace::FileView), true, false),
                (TerminalItem::Close, true, true),
            ]
        );
    }

    #[test]
    fn a_link_puts_its_rows_first_and_names_them_by_its_kind() {
        let web = TerminalLink::Web("https://example.org/login".to_owned());
        let file = TerminalLink::File {
            path: "src/main.rs".to_owned(),
            line: 12,
        };

        let rows = rows(&spot(true, Some(web.clone())), TerminalPlace::FileView);

        assert_eq!(
            items(&rows)[..3],
            [
                (TerminalItem::OpenLink, true, false),
                (TerminalItem::CopyLink, true, false),
                (TerminalItem::Copy, true, true),
            ]
        );
        assert_eq!(
            TerminalItem::OpenLink.label(Some(&web)),
            "Open in the browser"
        );
        assert_eq!(
            TerminalItem::OpenLink.label(Some(&file)),
            "Open in the file view"
        );
        assert_eq!(web.text(), "https://example.org/login");
        assert_eq!(file.text(), "src/main.rs:12");
    }

    #[test]
    fn the_rows_letters_differ() {
        let rows = rows(
            &spot(
                true,
                Some(TerminalLink::Web("https://a.example".to_owned())),
            ),
            TerminalPlace::Bottom,
        );
        let mut keys: Vec<char> = rows.iter().map(|row| row.item.key()).collect();
        keys.push(TerminalItem::MoveTo(TerminalPlace::Bottom).key());

        let count = keys.len();
        keys.sort_unstable();
        keys.dedup();

        assert_eq!(keys.len(), count);
    }

    #[test]
    fn the_menu_closes_with_its_terminal_and_a_new_connection() {
        use cockpit_protocol::{AgentStatus, TerminalChange, TerminalInfo, TerminalUpdate};

        use crate::machine::MachineId;
        use crate::state::{state_with, summary};

        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(AgentId::new(1));
        terminals::toggle_pane(&mut state);
        terminals::needed_requests(&mut state);
        let info = |terminal| TerminalInfo {
            terminal: TerminalId::new(terminal),
            title: "zsh".to_owned(),
            exited: None,
        };
        terminals::listed(&mut state, AgentId::new(1), Some(vec![info(2)]), Vec::new());

        open(&mut state, spot(false, None));
        let opened = target(&state).is_some();
        terminals::apply_update(
            &mut state,
            TerminalUpdate {
                agent: AgentId::new(1),
                terminal: TerminalId::new(2),
                change: TerminalChange::Closed,
            },
        );
        let after_close = (state.ui.popover, target(&state).is_some());
        terminals::listed(&mut state, AgentId::new(1), Some(vec![info(2)]), Vec::new());
        open(&mut state, spot(false, None));
        terminals::opened(&mut state, AgentId::new(1), Some(TerminalId::new(3)));
        let after_opening = state.ui.popover;
        open(&mut state, spot(false, None));
        terminals::connected(&mut state, MachineId::THIS);

        assert!(opened);
        assert_eq!(after_close, (None, false));
        assert_eq!(
            after_opening, None,
            "a terminal that opens takes the screen"
        );
        assert_eq!(state.ui.popover, None, "a new connection closes it");
    }
}
