//! Ctrl+F: a find in the selected agent's chat, and one in its file view. The page's script
//! (`ui/find.rs`) searches what the page shows and highlights the matches. The state keeps which
//! finds are open, and the drafts what each one looks for and what the page found.

use std::collections::BTreeMap;

use cockpit_protocol::AgentId;
use serde::{Deserialize, Serialize};

use super::app_state::AppState;
use super::drafts::Drafts;
use super::terminals;

/// A selection longer than this, in characters, is no text to look for: Ctrl+F leaves the find's
/// text as it was.
const MAX_SEEDED_CHARS: usize = 200;

/// The part of the window a find searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindPane {
    #[default]
    Chat,
    /// The selected agent's file view.
    File,
}

/// What a find looks for, as typed, and what the page found for it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FindDraft {
    pub query: String,
    pub match_case: bool,
    /// What the page last reported. Until it reports on a new text, the bar keeps the count of the
    /// one before, rather than blinking empty at each key.
    pub found: Option<Found>,
}

impl FindDraft {
    /// The page's report, when it is about the text the find holds now: a report on its way
    /// while the user typed is about an older text.
    pub fn apply_report(&mut self, report: &FindReport) {
        if report.query == self.query && report.match_case == self.match_case {
            self.found = Some(report.found);
        }
    }

    fn set_query(&mut self, query: &str) {
        query.clone_into(&mut self.query);
    }
}

/// The matches of a find, as the page counts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
pub struct Found {
    /// The current match, counted from 1. `None` when nothing matches.
    pub current: Option<u32>,
    /// Every match, the ones in folded calls included.
    pub total: u32,
    /// The matches in calls that are folded, which the page does not hold yet.
    pub folded: u32,
    /// The page stopped counting at its limit: there are more than `total`.
    pub capped: bool,
}

/// The page's report on a find. It names what it searched for, so that a report about an older
/// text is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FindReport {
    pub pane: FindPane,
    pub agent: AgentId,
    pub query: String,
    pub match_case: bool,
    #[serde(flatten)]
    pub found: Found,
}

/// Each agent's finds: the chat's, and its file view's.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Finds {
    pub chat: BTreeMap<AgentId, FindDraft>,
    pub file: BTreeMap<AgentId, FindDraft>,
}

impl Finds {
    pub fn get(&self, pane: FindPane, agent: AgentId) -> Option<&FindDraft> {
        self.of(pane).get(&agent)
    }

    pub fn get_mut(&mut self, pane: FindPane, agent: AgentId) -> &mut FindDraft {
        let finds = match pane {
            FindPane::Chat => &mut self.chat,
            FindPane::File => &mut self.file,
        };
        finds.entry(agent).or_default()
    }

    /// Takes the page's report, for the find it names.
    pub fn report(&mut self, report: &FindReport) {
        let finds = match report.pane {
            FindPane::Chat => &mut self.chat,
            FindPane::File => &mut self.file,
        };
        if let Some(draft) = finds.get_mut(&report.agent) {
            draft.apply_report(report);
        }
    }

    /// Keeps the finds of the agents `keep` keeps.
    pub fn retain(&mut self, keep: impl Fn(AgentId) -> bool) {
        self.chat.retain(|&agent, _| keep(agent));
        self.file.retain(|&agent, _| keep(agent));
    }

    fn of(&self, pane: FindPane) -> &BTreeMap<AgentId, FindDraft> {
        match pane {
            FindPane::Chat => &self.chat,
            FindPane::File => &self.file,
        }
    }
}

/// A request for the focus in a find's field, which its bar follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FindFocus {
    pub pane: FindPane,
    /// Counts the requests, so that a second Ctrl+F selects the field's text again.
    pub serial: u64,
}

/// Whether `agent`'s find in `pane` is open. The file's find waits while the terminal's tab is
/// over the file: it takes no key, and it finds again once the file shows.
pub fn is_open(state: &AppState, pane: FindPane, agent: AgentId) -> bool {
    match pane {
        FindPane::Chat => state.ui.chat_finds.contains(&agent),
        FindPane::File => {
            state.ui.file_view(agent).is_some_and(|view| view.find)
                && !terminals::file_view_shows_terminal(state, agent)
        }
    }
}

/// Ctrl+F: opens the find of the pane that has the focus, the file view's while the selected
/// agent has one that shows a file, and gives its field the focus with its text selected. A
/// selection on one line becomes the text to look for.
pub fn open(state: &mut AppState, drafts: &mut Drafts, pane: FindPane, selection: &str) {
    let Some(agent) = state.ui.selected else {
        return;
    };
    let pane = if terminals::file_view_shows_terminal(state, agent) {
        FindPane::Chat
    } else {
        pane
    };
    let pane = match state.ui.file_views.get_mut(&agent) {
        Some(view) if pane == FindPane::File => {
            view.find = true;
            FindPane::File
        }
        Some(_) | None => {
            state.ui.chat_finds.insert(agent);
            FindPane::Chat
        }
    };
    if let Some(text) = seed(selection) {
        drafts.finds.get_mut(pane, agent).set_query(text);
    }
    state.ui.find_focus = FindFocus {
        pane,
        serial: state.ui.find_focus.serial.wrapping_add(1),
    };
}

/// Closes `agent`'s find in `pane`. Returns whether it was open.
pub fn close(state: &mut AppState, pane: FindPane, agent: AgentId) -> bool {
    match pane {
        FindPane::Chat => state.ui.chat_finds.remove(&agent),
        FindPane::File if !is_open(state, pane, agent) => false,
        FindPane::File => state
            .ui
            .file_views
            .get_mut(&agent)
            .is_some_and(|view| std::mem::take(&mut view.find)),
    }
}

/// Esc with nothing else open: closes the selected agent's find in the pane that has the focus.
/// Returns whether it closed one.
pub fn close_on_escape(state: &mut AppState, pane: FindPane) -> bool {
    state
        .ui
        .selected
        .is_some_and(|agent| close(state, pane, agent))
}

/// The text a selection gives a find: its text trimmed, when it holds one line.
fn seed(selection: &str) -> Option<&str> {
    let text = selection.trim();
    let one_line = !text.contains(['\n', '\r']);
    (one_line && !text.is_empty() && text.chars().count() <= MAX_SEEDED_CHARS).then_some(text)
}

/// What a find looks for, ready to count its matches in a text. The page's script counts the same
/// way: case-insensitive unless `match_case`, without overlaps.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Needle {
    /// In lower case, unless `match_case`.
    text: String,
    match_case: bool,
}

impl Needle {
    /// `None` for an empty text, which matches nothing.
    pub fn new(query: &str, match_case: bool) -> Option<Self> {
        if query.is_empty() {
            return None;
        }
        let text = if match_case {
            query.to_owned()
        } else {
            query.to_lowercase()
        };
        Some(Self { text, match_case })
    }

    /// The needle of `agent`'s find in `pane`, while that find is open.
    pub fn of_open_find(
        state: &AppState,
        drafts: &Drafts,
        pane: FindPane,
        agent: AgentId,
    ) -> Option<Self> {
        if !is_open(state, pane, agent) {
            return None;
        }
        let draft = drafts.finds.get(pane, agent)?;
        Self::new(&draft.query, draft.match_case)
    }

    /// The matches in `haystack`.
    pub fn count(&self, haystack: &str) -> usize {
        if self.needs_lowercase() {
            return self.count_lowercase(&haystack.to_lowercase());
        }
        if self.match_case {
            return haystack.matches(self.text.as_str()).count();
        }
        count_ascii_folded(haystack.as_bytes(), self.text.as_bytes())
    }

    /// Whether counting takes the haystack in lower case (`count_lowercase`): a text past ASCII,
    /// whatever its case.
    pub fn needs_lowercase(&self) -> bool {
        !self.match_case && !self.text.is_ascii()
    }

    /// The matches in `haystack`, which is in lower case.
    pub fn count_lowercase(&self, haystack: &str) -> usize {
        haystack.matches(self.text.as_str()).count()
    }
}

/// The matches of `needle`, in lower case and ASCII, in `haystack`, whatever the case of its
/// ASCII letters. A byte of a character past ASCII never equals an ASCII byte, so a match never
/// starts or ends inside such a character.
fn count_ascii_folded(haystack: &[u8], needle: &[u8]) -> usize {
    let mut count = 0;
    let mut at = 0;
    while let Some(window) = haystack.get(at..at + needle.len()) {
        if window.eq_ignore_ascii_case(needle) {
            count += 1;
            at += needle.len();
        } else {
            at += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::AgentStatus;

    use super::*;
    use crate::state::ui_state::FileView;
    use crate::state::{ViewMode, state_with, summary};

    fn agent() -> AgentId {
        AgentId::new(1)
    }

    fn selected_state() -> AppState {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        state
    }

    fn with_file_view(state: &mut AppState) {
        state.ui.file_views.insert(
            agent(),
            FileView::new(PathBuf::from("src/lexer.rs"), None, ViewMode::WholeFile),
        );
    }

    #[test]
    fn a_needle_counts_without_overlaps_and_whatever_the_case() {
        let needle = Needle::new("Column", false).unwrap();
        assert_eq!(needle.count("column COLUMN Columns colum"), 3);

        let exact = Needle::new("Column", true).unwrap();
        assert_eq!(exact.count("column COLUMN Columns"), 1);

        let doubled = Needle::new("aa", false).unwrap();
        assert_eq!(doubled.count("aaaa"), 2);

        // Past ASCII, the case folds too, and a character is never cut.
        let accented = Needle::new("ÉTÉ", false).unwrap();
        assert_eq!(accented.count("un été, deux étés"), 2);
        let ascii = Needle::new("te", false).unwrap();
        assert_eq!(ascii.count("été"), 0);

        assert_eq!(Needle::new("", false), None);
    }

    #[test]
    fn ctrl_f_opens_the_find_of_the_pane_with_the_focus() {
        let mut state = selected_state();
        let mut drafts = Drafts::default();

        // Without a file view, the file view's focus has no find: the chat's opens.
        open(&mut state, &mut drafts, FindPane::File, "");
        assert!(is_open(&state, FindPane::Chat, agent()));
        assert_eq!(state.ui.find_focus.pane, FindPane::Chat);

        with_file_view(&mut state);
        open(&mut state, &mut drafts, FindPane::File, "");
        assert!(is_open(&state, FindPane::File, agent()));
        assert_eq!(
            state.ui.find_focus,
            FindFocus {
                pane: FindPane::File,
                serial: 2
            }
        );
    }

    #[test]
    fn a_selection_on_one_line_becomes_the_text_to_look_for() {
        let mut state = selected_state();
        let mut drafts = Drafts::default();
        drafts.finds.get_mut(FindPane::Chat, agent()).query = "lexer".to_owned();

        open(
            &mut state,
            &mut drafts,
            FindPane::Chat,
            "  two lines\nof text ",
        );
        assert_eq!(drafts.finds.chat[&agent()].query, "lexer");

        open(
            &mut state,
            &mut drafts,
            FindPane::Chat,
            &"x".repeat(MAX_SEEDED_CHARS + 1),
        );
        assert_eq!(drafts.finds.chat[&agent()].query, "lexer");

        open(&mut state, &mut drafts, FindPane::Chat, "  the column ");
        assert_eq!(drafts.finds.chat[&agent()].query, "the column");
    }

    #[test]
    fn a_report_about_an_older_text_is_dropped() {
        let mut drafts = Drafts::default();
        let draft = drafts.finds.get_mut(FindPane::Chat, agent());
        draft.query = "colu".to_owned();
        let found = Found {
            current: Some(1),
            total: 4,
            folded: 1,
            capped: false,
        };
        let mut report = FindReport {
            pane: FindPane::Chat,
            agent: agent(),
            query: "col".to_owned(),
            match_case: false,
            found,
        };

        drafts.finds.report(&report);
        assert_eq!(drafts.finds.chat[&agent()].found, None);

        report.query = "colu".to_owned();
        drafts.finds.report(&report);
        assert_eq!(drafts.finds.chat[&agent()].found, Some(found));

        // The file view's find of the same agent is another find.
        report.pane = FindPane::File;
        drafts.finds.report(&report);
        assert_eq!(drafts.finds.get(FindPane::File, agent()), None);
    }

    #[test]
    fn esc_closes_the_find_of_the_pane_with_the_focus() {
        let mut state = selected_state();
        let mut drafts = Drafts::default();
        with_file_view(&mut state);
        open(&mut state, &mut drafts, FindPane::Chat, "");

        // The file view has no find open: Esc there closes nothing.
        assert!(!close_on_escape(&mut state, FindPane::File));
        assert!(close_on_escape(&mut state, FindPane::Chat));
        assert!(!is_open(&state, FindPane::Chat, agent()));
        assert!(!close_on_escape(&mut state, FindPane::Chat));
    }

    #[test]
    fn each_agent_keeps_its_own_find() {
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        let mut drafts = Drafts::default();
        state.ui.selected = Some(agent());
        open(&mut state, &mut drafts, FindPane::Chat, "lexer");
        state.ui.selected = Some(AgentId::new(2));

        assert!(!is_open(&state, FindPane::Chat, AgentId::new(2)));
        assert_eq!(
            Needle::of_open_find(&state, &drafts, FindPane::Chat, agent()),
            Needle::new("lexer", false)
        );

        drafts.finds.retain(|kept| kept != agent());
        assert_eq!(drafts.finds.get(FindPane::Chat, agent()), None);
    }
}
