//! What the file view shows for a name it found no file for, and for several files git ignores
//! that end with it. The card lists where the lookup looked, and offers the next step: the search
//! of all the ignored files, Stop while it runs, or a question to the agent.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use cockpit_protocol::{AgentId, FoundFile, IgnoredSearch, IgnoredSearched};
use dioxus::prelude::*;

use crate::bridge::{self, COMPOSER_ID};
use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::path_style::PathStyle;
use crate::state::{IgnoredSearchRun, NotFound, actions, file_edits};
use crate::ui::icons::{Icon, IconKind};

/// Where the lookup looked, and what it says about it. The card keeps its four checks in every
/// state, so that it does not jump: only the last one changes.
#[derive(Debug)]
struct Card {
    lead: String,
    checks: [Check; 4],
    /// The tile shows a file instead of the search icon: the search of all the ignored files is
    /// done.
    nowhere: bool,
    next: Next,
}

#[derive(Debug)]
struct Check {
    mark: Mark,
    text: String,
}

impl Check {
    fn done(text: &str) -> Self {
        Self {
            mark: Mark::Done,
            text: text.to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    Done,
    NotYet,
    Running,
    /// Done in part: the search ran out of time.
    Partly,
}

/// The card's main button. After a search cut short, "Ask the agent" comes beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    SearchAll,
    Stop,
    Ask,
    SearchLonger,
}

const AGENT_FILES: &str = "The agent's folder and files";
const TRACKED: &str = "Files git tracks";
const NEAR_IGNORED: &str = "Top two levels of each git-ignored folder";
const ALL_IGNORED: &str = "All git-ignored files";

impl Card {
    fn of(not_found: &NotFound, now: SystemTime) -> Self {
        let (lead, last, nowhere, next) = match (not_found.search, not_found.searched) {
            (Some(IgnoredSearchRun { level, since, .. }), _) => {
                let ran = format::duration(format::since(since, now));
                let limit = level
                    .full_search_time()
                    .map(|limit| format!(" of {}", format::duration(limit)))
                    .unwrap_or_default();
                let last = Check {
                    mark: Mark::Running,
                    text: format!("{ALL_IGNORED} · {ran}{limit}"),
                };
                (
                    "Searching the ignored files…".to_owned(),
                    last,
                    false,
                    Next::Stop,
                )
            }
            (None, IgnoredSearched::Quick) => {
                let last = Check {
                    mark: Mark::NotYet,
                    text: ALL_IGNORED.to_owned(),
                };
                ("Not found yet".to_owned(), last, false, Next::SearchAll)
            }
            (None, IgnoredSearched::All { files }) => {
                let last = Check {
                    mark: Mark::Done,
                    text: all_ignored(files),
                };
                (
                    "No file has this name here".to_owned(),
                    last,
                    true,
                    Next::Ask,
                )
            }
            (None, IgnoredSearched::CutShort { files, seconds }) => {
                let lead = format!(
                    "Not found in the first {}",
                    format::duration(Duration::from_secs(seconds.into()))
                );
                let last = Check {
                    mark: Mark::Partly,
                    text: format!(
                        "{} git-ignored files, not all",
                        format::grouped(files.into())
                    ),
                };
                (lead, last, false, Next::SearchLonger)
            }
        };
        Self {
            lead,
            checks: [
                Check::done(AGENT_FILES),
                Check::done(TRACKED),
                Check::done(NEAR_IGNORED),
                last,
            ],
            nowhere,
            next,
        }
    }
}

/// "All 127,739 git-ignored files", as the search of all of them counted.
fn all_ignored(files: u32) -> String {
    match files {
        0 => "Git ignores no file here".to_owned(),
        1 => "The one git-ignored file".to_owned(),
        files => format!("All {} git-ignored files", format::grouped(files.into())),
    }
}

/// How long `level` may run, as the card says it.
fn search_time(level: IgnoredSearch) -> String {
    level
        .full_search_time()
        .map(format::duration)
        .unwrap_or_default()
}

impl Next {
    fn label(self) -> String {
        match self {
            Self::SearchAll => "Search all ignored files".to_owned(),
            Self::Stop => "Stop".to_owned(),
            Self::Ask => "Ask the agent where it is".to_owned(),
            Self::SearchLonger => format!("Search for {}", search_time(IgnoredSearch::Longer)),
        }
    }

    fn title(self) -> String {
        match self {
            Self::SearchAll => format!(
                "Search every file git ignores here, for up to {}",
                search_time(IgnoredSearch::All)
            ),
            Self::Stop => "Stop waiting for the search".to_owned(),
            Self::Ask => "Put the question in the message to the agent".to_owned(),
            Self::SearchLonger => "Search the git-ignored files again, for longer".to_owned(),
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::SearchAll | Self::SearchLonger => "button primary small",
            Self::Stop | Self::Ask => "button small",
        }
    }
}

/// `path`, a name the view found no file for, with where the lookup looked and what to do next.
#[component]
pub fn NotFoundCard(
    agent: AgentId,
    folder: PathBuf,
    path: PathBuf,
    not_found: NotFound,
) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context();
    let card = Card::of(&not_found, now());
    let name = PathStyle::of(&folder)
        .shown(&path.to_string_lossy())
        .into_owned();
    let next = card.next;
    let act = {
        let path = path.clone();
        move |_| match next {
            Next::SearchAll => search(cockpit, agent, &path, IgnoredSearch::All),
            Next::SearchLonger => search(cockpit, agent, &path, IgnoredSearch::Longer),
            Next::Stop => cockpit.update(|state| actions::stop_search(state, agent, &path)),
            Next::Ask => ask_where(cockpit, agent, &path),
        }
    };
    rsx! {
        div { class: "file-lookup",
            div { class: "pdf-tile", aria_hidden: true,
                Icon { kind: if card.nowhere { IconKind::File } else { IconKind::Search }, size: 26 }
            }
            span { class: "lookup-name", title: "{name}", "{name}" }
            span { class: "lookup-lead", role: "status", "{card.lead}" }
            ul { class: "lookup-checks", aria_label: "Where the file view looked",
                for (index, check) in card.checks.into_iter().enumerate() {
                    li { key: "{index}",
                        CheckMark { mark: check.mark }
                        span { "{check.text}" }
                    }
                }
            }
            div { class: "lookup-actions",
                // One element whatever the step, so that the focus stays on it when Search
                // turns into Stop.
                button {
                    class: next.class(),
                    title: next.title(),
                    onclick: act,
                    "{next.label()}"
                }
                if next == Next::SearchLonger {
                    button {
                        class: "button small",
                        title: Next::Ask.title(),
                        onclick: move |_| ask_where(cockpit, agent, &path),
                        "Ask the agent"
                    }
                }
            }
        }
    }
}

fn search(cockpit: Cockpit, agent: AgentId, path: &Path, level: IgnoredSearch) {
    cockpit.change(|state| actions::search_ignored(state, agent, path, level));
}

#[component]
fn CheckMark(mark: Mark) -> Element {
    match mark {
        Mark::Done => rsx! {
            span { class: "lookup-mark done", aria_label: "Done", Icon { kind: IconKind::Check, size: 13 } }
        },
        Mark::NotYet => rsx! {
            span { class: "lookup-mark", aria_label: "Not yet", Icon { kind: IconKind::Circle, size: 13 } }
        },
        Mark::Running => rsx! {
            span { class: "lookup-mark", aria_label: "Running", span { class: "spinner" } }
        },
        Mark::Partly => rsx! {
            span { class: "lookup-mark partly", aria_label: "Not all", Icon { kind: IconKind::Warning, size: 13 } }
        },
    }
}

/// Puts the question in the agent's message, and the focus in the composer.
fn ask_where(cockpit: Cockpit, agent: AgentId, path: &Path) {
    cockpit.update_with_drafts(|state, drafts| {
        actions::ask_where_file_is(state, drafts, agent, path);
    });
    bridge::focus_field_at_end(COMPOSER_ID);
}

/// The files git ignores that end with `name`, for the user to pick one by when it changed and
/// its size. `cut_short`: the search stopped before its end, and other files may have the name.
#[component]
pub fn IgnoredChoices(
    agent: AgentId,
    folder: PathBuf,
    name: PathBuf,
    matches: Vec<FoundFile>,
    cut_short: bool,
    line: Option<u32>,
) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context();
    let now = now();
    let style = PathStyle::of(&folder);
    let lead = choices_lead(matches.len());
    rsx! {
        div { class: "file-lookup top",
            span { class: "lookup-lead", "{lead}" }
            if cut_short {
                span { class: "lookup-note", "{CUT_SHORT_NOTE}" }
            }
            div { class: "lookup-found",
                for found in matches {
                    {
                        let full = style.shown(&found.path.to_string_lossy()).into_owned();
                        let shown = ShownPath::of(&folder, &found.path);
                        let details = details(&found, now);
                        let name = name.clone();
                        let choice = found.path;
                        rsx! {
                            button {
                                key: "{full}",
                                class: "lookup-choice",
                                title: "{full}",
                                onclick: move |_| {
                                    let choice = choice.clone();
                                    cockpit.update(|state| {
                                        actions::pick_ignored_file(state, agent, &name, choice, line);
                                    });
                                },
                                span { class: "lookup-choice-name", "{shown.name}" }
                                span { class: "lookup-choice-place", "{shown.place}" }
                                span { class: "lookup-choice-details", "{details}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What the list of choices says under its count when the search stopped before its end.
const CUT_SHORT_NOTE: &str = "The search stopped before its end: other files may have this name.";

/// "2 git-ignored files have this name".
fn choices_lead(count: usize) -> String {
    let files = format::count(count, "git-ignored file has", "git-ignored files have");
    format!("{files} this name")
}

/// A path as the view shows it, relative to the agent's folder when inside it, cut into the
/// file's name and the folder that holds it.
#[derive(Debug, PartialEq, Eq)]
struct ShownPath {
    name: String,
    /// Ends with the separator, or is empty.
    place: String,
}

impl ShownPath {
    fn of(folder: &Path, path: &Path) -> Self {
        let text = file_edits::shown_path(folder, path);
        let name = PathStyle::of(folder)
            .file_name(&text)
            .unwrap_or(&text)
            .to_owned();
        let place = text
            .strip_suffix(name.as_str())
            .unwrap_or_default()
            .to_owned();
        Self { name, place }
    }
}

/// "Changed 12 min ago · 48 KB".
fn details(found: &FoundFile, now: SystemTime) -> String {
    let changed = found
        .modified
        .map(|modified| format!("Changed {}", format::ago_in_sentence(modified, now)));
    let size = found.bytes.map(format::bytes);
    match (changed, size) {
        (Some(changed), Some(size)) => format!("{changed} · {size}"),
        (Some(text), None) | (None, Some(text)) => text,
        (None, None) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::Timestamp;

    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000 + seconds)
    }

    fn marks(card: &Card) -> Vec<(Mark, &str)> {
        card.checks
            .iter()
            .map(|check| (check.mark, check.text.as_str()))
            .collect()
    }

    #[test]
    fn the_card_says_where_the_lookup_looked_and_what_comes_next() {
        let quick = Card::of(&NotFound::new(IgnoredSearched::Quick), at(0));
        let all = Card::of(
            &NotFound::new(IgnoredSearched::All { files: 127_739 }),
            at(0),
        );
        let cut = Card::of(
            &NotFound::new(IgnoredSearched::CutShort {
                files: 2_100_000,
                seconds: 15,
            }),
            at(0),
        );

        assert_eq!(quick.lead, "Not found yet");
        assert_eq!(
            marks(&quick),
            [
                (Mark::Done, AGENT_FILES),
                (Mark::Done, TRACKED),
                (Mark::Done, NEAR_IGNORED),
                (Mark::NotYet, ALL_IGNORED),
            ]
        );
        assert_eq!(quick.next, Next::SearchAll);
        assert_eq!(all.lead, "No file has this name here");
        assert_eq!(
            marks(&all)[3],
            (Mark::Done, "All 127,739 git-ignored files")
        );
        assert!(all.nowhere);
        assert_eq!(all.next, Next::Ask);
        assert_eq!(cut.lead, "Not found in the first 15 s");
        assert_eq!(
            marks(&cut)[3],
            (Mark::Partly, "2,100,000 git-ignored files, not all")
        );
        assert_eq!(cut.next, Next::SearchLonger);
        assert_eq!(Next::SearchLonger.label(), "Search for 1 min");
    }

    #[test]
    fn a_running_search_shows_its_time_against_its_limit() {
        let not_found = NotFound {
            searched: IgnoredSearched::CutShort {
                files: 10,
                seconds: 15,
            },
            search: Some(IgnoredSearchRun {
                level: IgnoredSearch::Longer,
                since: Timestamp::from_system_time(at(0)).unwrap(),
                number: 2,
            }),
            look_again: false,
            searches: 2,
        };

        let card = Card::of(&not_found, at(12));

        assert_eq!(card.lead, "Searching the ignored files…");
        assert_eq!(
            marks(&card)[3],
            (Mark::Running, "All git-ignored files · 12 s of 1 min")
        );
        assert_eq!(card.next, Next::Stop);
    }

    #[test]
    fn a_choice_shows_its_name_its_folder_and_when_it_changed() {
        let folder = Path::new("/work/shop");
        let found = FoundFile {
            path: PathBuf::from("/work/shop/.atmp/exports/report.csv"),
            modified: Timestamp::from_system_time(at(0)),
            bytes: Some(48 * 1024),
        };

        assert_eq!(
            ShownPath::of(folder, &found.path),
            ShownPath {
                name: "report.csv".to_owned(),
                place: ".atmp/exports/".to_owned(),
            }
        );
        assert_eq!(details(&found, at(12 * 60)), "Changed 12 min ago · 48 KB");
        assert_eq!(all_ignored(0), "Git ignores no file here");
        assert_eq!(choices_lead(1), "1 git-ignored file has this name");
        assert_eq!(choices_lead(2), "2 git-ignored files have this name");
    }
}
