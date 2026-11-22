//! The file view's git parts: the line above the diff, the blame of the hovered line and its
//! commit's card, the file's history, and a commit's change to the file.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use cockpit_protocol::{
    AgentId, Commit, CommitHash, CommitTexts, GitBlame, GitCommitFile, LogEntry, MAX_LOG_COMMITS,
    OnRemote,
};
use dioxus::prelude::*;

use crate::bridge::HIGHLIGHTED_CLASS;
use crate::connection::Cockpit;
use crate::format;
use crate::render::{self, Language, MAX_RENDERED_LINES, diff_lines};
use crate::state::git::{self, DiffBase, GitHubLinks};
use crate::state::menu_keys::MenuKey;
use crate::state::{AppState, FileRead, Loadable, Popover, ViewMode};
use crate::ui::code::{LineMarks, LineNumbers, diff_row, lines_not_shown};
use crate::ui::file_view::{FILE_DIFF_CONTEXT, FILE_LOADING_CLASS};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::menu::KeyedMenu;
use crate::ui::presence::use_presence;

/// How long the blame's card stays after the pointer leaves the line's text, so that the pointer
/// can reach the card.
const CARD_GRACE: Duration = Duration::from_millis(250);
/// How long the pointer rests on a line's blame text before its card shows, when none shows yet,
/// so that a pointer on its way elsewhere opens none.
const CARD_DELAY: Duration = Duration::from_millis(400);

/// What the hovered line's blame text points at, and where the pointer was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlameHover {
    /// The index of the line's commit in the blame, `None` for a line not committed yet.
    pub commit: Option<usize>,
    pub x: i32,
    pub y: i32,
    /// Tells this hover from the next one, so a late hide leaves a newer card alone.
    pub serial: u64,
}

/// The blame text of one line, which the style sheet shows while the line is hovered.
#[derive(Clone, PartialEq)]
pub struct BlameCell {
    pub text: String,
    pub commit: Option<usize>,
    pub hover: Signal<Option<BlameHover>>,
    /// The pointer is over the card: it stays.
    pub on_card: Signal<bool>,
    /// The serial of the blame text the pointer is on, whose card may show next.
    pub entered: Signal<Option<u64>>,
}

impl BlameCell {
    pub fn render(self) -> Element {
        let Self {
            text,
            commit,
            mut hover,
            on_card,
            mut entered,
        } = self;
        rsx! {
            span {
                class: "line-blame",
                // The style sheet draws the text from the attribute, so that a selection of the
                // file, its copy and its find leave it out.
                "data-text": "{text}",
                onmouseenter: move |event: MouseEvent| {
                    let point = event.client_coordinates();
                    let serial = next_serial();
                    // Whole pixels are enough to place a card.
                    #[expect(clippy::cast_possible_truncation, reason = "a window's pixels fit in i32")]
                    let (x, y) = (point.x.round() as i32, point.y.round() as i32);
                    let shown = BlameHover { commit, x, y, serial };
                    entered.set(Some(serial));
                    // From one line's card to the next, the card follows at once.
                    if hover.peek().is_some() {
                        hover.set(Some(shown));
                        return;
                    }
                    spawn(async move {
                        tokio::time::sleep(CARD_DELAY).await;
                        if *entered.peek() == Some(serial) {
                            hover.set(Some(shown));
                        }
                    });
                },
                onmouseleave: move |_| {
                    entered.set(None);
                    hide_later(hover, on_card);
                },
            }
        }
    }
}

/// A serial no hover had before, so that a late hide of an older hover never takes a newer card.
fn next_serial() -> u64 {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    SERIAL.fetch_add(1, Ordering::Relaxed)
}

/// Hides the card after `CARD_GRACE`, unless the pointer reached it or another line took over.
fn hide_later(mut hover: Signal<Option<BlameHover>>, on_card: Signal<bool>) {
    let shown = hover.peek().as_ref().map(|shown| shown.serial);
    spawn(async move {
        tokio::time::sleep(CARD_GRACE).await;
        if !*on_card.peek() && hover.peek().as_ref().map(|shown| shown.serial) == shown {
            hover.set(None);
        }
    });
}

/// "Robin Hale, 3 weeks ago · Add the lexer", the text a line's blame shows.
pub fn blame_text(commit: &Commit, now: SystemTime) -> String {
    format!(
        "{}, {} · {}",
        commit.author,
        format::ago(commit.time, now),
        commit.title
    )
}

/// The text of a line not committed yet.
pub const NOT_COMMITTED: &str = "Not committed yet";

/// What a line's blame shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineBlame {
    pub text: String,
    /// The index of the line's commit in the blame, `None` for a line not committed yet.
    pub commit: Option<usize>,
}

/// For each line of `text`, the file on disk, its blame. Empty while nothing says who changed the
/// lines.
pub fn line_blames(
    base: &DiffBase,
    blame: Option<&GitBlame>,
    text: &str,
    now: SystemTime,
) -> Vec<LineBlame> {
    let (committed, blame) = match (base, blame) {
        (DiffBase::Git { new: true, .. }, _) => {
            return text
                .lines()
                .map(|_| LineBlame {
                    text: NOT_COMMITTED.to_owned(),
                    commit: None,
                })
                .collect();
        }
        (DiffBase::Git { text, .. }, Some(blame)) => (text, blame),
        (DiffBase::Git { .. } | DiffBase::Waiting | DiffBase::Edits { .. }, _) => {
            return Vec::new();
        }
    };
    let texts: Vec<String> = blame
        .commits
        .iter()
        .map(|commit| blame_text(commit, now))
        .collect();
    git::line_commits(blame, committed, text)
        .into_iter()
        .map(|commit| {
            let text = commit
                .and_then(|index| texts.get(index).cloned())
                .unwrap_or_else(|| NOT_COMMITTED.to_owned());
            LineBlame { text, commit }
        })
        .collect()
}

/// "Dev, 3 days ago (Wednesday 12 Nov 2026, 13:38:05)", under the title of a commit's card.
fn card_meta(commit: &Commit, now: SystemTime) -> String {
    let date = format::full_time(commit.time).unwrap_or_default();
    format!(
        "{}, {} ({date})",
        commit.author,
        format::ago(commit.time, now)
    )
}

/// The card of the hovered line's commit: its title, author, date and message, a link to its
/// change to the file, and its page on GitHub.
#[component]
pub fn BlameCard(
    agent: AgentId,
    hover: Signal<Option<BlameHover>>,
    on_card: Signal<bool>,
    commits: Vec<Commit>,
    github: Option<GitHubLinks>,
) -> Element {
    let cockpit: Cockpit = use_context();
    let mut on_card = on_card;
    let shown = hover();
    let Some(shown) = use_presence(shown) else {
        return rsx! {};
    };
    let BlameHover { commit, x, y, .. } = shown.value;
    let style = format!("--card-x: {x}px; --card-y: {y}px");
    let class = if shown.leaving {
        "blame-card leaving"
    } else {
        "blame-card"
    };
    let body = match commit.and_then(|index| commits.get(index)) {
        Some(commit) => {
            let hash = commit.hash.clone();
            let meta = card_meta(commit, SystemTime::now());
            let page = github
                .as_ref()
                .and_then(|links| links.commit_page(&commit.hash));
            rsx! {
                span { class: "blame-card-title", "{commit.title}" }
                span { class: "blame-card-meta", "{meta} · " span { class: "mono", "{commit.hash.short()}" } }
                if !commit.body.is_empty() {
                    span { class: "blame-card-body", "{commit.body}" }
                }
                div { class: "blame-card-actions",
                    button {
                        class: "link-button",
                        onclick: move |_| {
                            let hash = hash.clone();
                            cockpit.update(|state| git::show_commit(state, agent, Some(hash)));
                        },
                        "Show this commit's change"
                    }
                    if let Some(page) = page {
                        button {
                            class: "link-button",
                            onclick: move |_| cockpit.open_in_browser(page.clone()),
                            Icon { kind: IconKind::External, size: 12 }
                            {git::OPEN_ON_GITHUB_LABEL}
                        }
                    }
                }
            }
        }
        None => rsx! {
            span { class: "blame-card-title", "{NOT_COMMITTED}" }
            div { class: "blame-card-actions",
                button {
                    class: "link-button",
                    onclick: move |_| cockpit.update(|state| git::show_uncommitted(state, agent)),
                    "Show the uncommitted changes"
                }
            }
        },
    };
    rsx! {
        div {
            class,
            style,
            inert: shown.leaving.then_some(true),
            onmouseenter: move |_| on_card.set(true),
            onmouseleave: move |_| {
                on_card.set(false);
                hide_later(hover, on_card);
            },
            {body}
        }
    }
}

/// The line above a diff against the last commit.
pub fn since_note(head: Option<&Commit>, new: bool) -> String {
    match (head, new) {
        (_, true) => "New since the last commit.".to_owned(),
        (Some(head), false) => format!(
            "Since {} \u{201c}{}\u{201d}, {}.",
            head.hash.short(),
            head.title,
            format::ago(head.time, SystemTime::now())
        ),
        (None, false) => String::new(),
    }
}

/// The file's uncommitted changes against its last committed `base`.
pub fn git_diff(
    base: &str,
    text: &str,
    note: String,
    language: Option<Language>,
    marks: LineMarks<'_>,
) -> Element {
    let diff = diff_lines(
        base,
        text,
        language,
        FILE_DIFF_CONTEXT,
        1,
        MAX_RENDERED_LINES,
    );
    rsx! {
        if !note.is_empty() {
            p { class: "file-note", "{note}" }
        }
        if diff.rows.is_empty() {
            p { class: "file-note", "No uncommitted change." }
        } else {
            div { class: "diff-rows",
                for row in diff.rows.iter() {
                    {diff_row(row, LineNumbers::Shown, marks)}
                }
            }
            {lines_not_shown(diff.more)}
        }
    }
}

/// "3 uncommitted changes", for the foot. Its own component, so that the count runs again only
/// when a text changes.
#[component]
pub fn ChangeCount(base: String, text: String) -> Element {
    let count = render::change_count(&base, &text);
    let words = match count {
        0 => "No uncommitted change".to_owned(),
        count => format::count(count, "uncommitted change", "uncommitted changes"),
    };
    rsx! { "{words}" }
}

/// Whether the file view shows the history button, and whether it is enabled. Git is asked only
/// about a file shown as text: the button waits, disabled, for its answer.
pub fn history_button(base: &DiffBase, is_text: bool) -> Option<bool> {
    let has_history = matches!(base, DiffBase::Git { head: Some(_), .. });
    let waiting = is_text && *base == DiffBase::Waiting;
    (has_history || waiting).then_some(has_history)
}

/// The history button, which opens the list of the commits that changed the file.
#[component]
pub fn HistoryButton(agent: AgentId, open: bool, enabled: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let mut mounted = use_signal(|| None::<Rc<MountedData>>);
    rsx! {
        button {
            class: if open { "icon-button framed toggled" } else { "icon-button framed" },
            aria_label: "History of this file",
            aria_expanded: open,
            title: if enabled { "History of this file" } else { "The file is in no git checkout" },
            disabled: !enabled,
            onmounted: move |event| mounted.set(Some(event.data())),
            // A click from the keyboard has no pointer position: the list goes under the button.
            onclick: move |event: MouseEvent| async move {
                let pointer = event.client_coordinates();
                let mut corner = (pointer.x, pointer.y);
                let button = mounted.cloned();
                if let Some(button) = button
                    && let Ok(rect) = button.get_client_rect().await
                {
                    corner = (rect.max_x(), rect.max_y());
                }
                // Whole pixels are enough to place a menu.
                #[expect(clippy::cast_possible_truncation, reason = "a window's pixels fit in i32")]
                let (x, y) = (corner.0.round() as i32, corner.1.round() as i32);
                cockpit.update(|state| git::toggle_history(state, agent, x, y));
            },
            Icon { kind: IconKind::Clock, size: 15 }
        }
    }
}

/// What the history list shows.
#[derive(Debug, Clone, PartialEq)]
struct HistoryShown {
    agent: AgentId,
    x: i32,
    y: i32,
    /// The file differs from its last commit: "Uncommitted changes" comes first.
    uncommitted: bool,
    log: Loadable<Vec<LogEntry>>,
}

/// The commits that changed the shown file, newest first, one line each.
#[component]
pub fn FileHistory() -> Element {
    let cockpit: Cockpit = use_context();
    let open = {
        let state = cockpit.state.read();
        match state.ui.popover {
            Some(Popover::FileHistory { agent, x, y }) => history_shown(&state, agent, x, y),
            _ => None,
        }
    };
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    let key = format!("{}-{}", shown.opening, shown.value.agent.get());
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            for key in [key] {
                HistoryList { key: "{key}", shown: shown.value.clone() }
            }
        }
    }
}

fn history_shown(state: &AppState, agent: AgentId, x: i32, y: i32) -> Option<HistoryShown> {
    let entry = state.agents.get(&agent)?;
    let view = state.ui.file_view(agent)?;
    let path = git::absolute(entry, &view.path);
    let file = entry.git.files.get(&path);
    let uncommitted = match (DiffBase::of(file), entry.files.get(&view.path)) {
        (
            DiffBase::Git { text, .. },
            Some(Loadable::Loaded(FileRead::Text { text: current, .. })),
        ) => render::change_count(&text, current) > 0,
        _ => false,
    };
    Some(HistoryShown {
        agent,
        x,
        y,
        uncommitted,
        log: file.map(|file| file.log.value.clone()).unwrap_or_default(),
    })
}

#[component]
fn HistoryList(shown: HistoryShown) -> Element {
    let cockpit: Cockpit = use_context();
    let mut highlighted = use_signal(|| None::<usize>);
    let HistoryShown {
        agent,
        x,
        y,
        uncommitted,
        log,
    } = shown;
    let entries: Vec<LogEntry> = log.loaded().cloned().unwrap_or_default();
    let loaded = matches!(log, Loadable::Loaded(_));
    let offset = usize::from(uncommitted);
    let count = entries.len() + offset;
    let pick = {
        let entries = entries.clone();
        move |index: usize| match index.checked_sub(offset) {
            None => cockpit.update(|state| git::show_uncommitted(state, agent)),
            Some(at) => {
                if let Some(entry) = entries.get(at) {
                    let hash: CommitHash = entry.commit.hash.clone();
                    cockpit.update(|state| git::show_commit(state, agent, Some(hash)));
                }
            }
        }
    };
    let row_class = move |index: usize, extra: &str| {
        if highlighted() == Some(index) {
            format!("history-row{extra} {HIGHLIGHTED_CLASS}")
        } else {
            format!("history-row{extra}")
        }
    };
    let now = SystemTime::now();
    rsx! {
        KeyedMenu {
            x,
            y,
            focus_back: None,
            class: "history-menu",
            on_key: {
                let pick = pick.clone();
                move |key: MenuKey| {
                    let now = *highlighted.peek();
                    match key {
                        MenuKey::Down if count > 0 => {
                            highlighted.set(Some(now.map_or(0, |at| (at + 1).min(count - 1))));
                        }
                        MenuKey::Up if count > 0 => {
                            highlighted.set(Some(now.map_or(0, |at| at.saturating_sub(1))));
                        }
                        MenuKey::Enter => {
                            if let Some(at) = now {
                                pick(at);
                            }
                        }
                        _ => {}
                    }
                }
            },
            div { class: "history-head", "History of the file" }
            if uncommitted {
                button {
                    class: row_class(0, " uncommitted"),
                    role: "menuitem",
                    tabindex: -1,
                    onmouseenter: move |_| highlighted.set(Some(0)),
                    onclick: {
                        let pick = pick.clone();
                        move |_| pick(0)
                    },
                    span { class: "history-hash" }
                    span { class: "history-title", "Uncommitted changes" }
                    span { class: "history-who", "now" }
                    span { class: "history-counts" }
                }
            }
            match log {
                Loadable::NotRequested | Loadable::Loading => rsx! {
                    for (index, width) in ["70%", "55%", "62%"].iter().enumerate() {
                        div { key: "{index}", class: "history-row skeleton-row", aria_hidden: true,
                            span { class: "skeleton skeleton-line", style: "width: {width}" }
                        }
                    }
                },
                Loadable::Failed(error) => rsx! {
                    p { class: "history-note", "The history did not load: {error}" }
                },
                Loadable::Loaded(_) if entries.is_empty() => rsx! {
                    p { class: "history-note", "No commit changed the file yet." }
                },
                Loadable::Loaded(_) => rsx! {
                    for (at, entry) in entries.iter().enumerate() {
                        {
                            let index = at + offset;
                            let pick = pick.clone();
                            let who = format!("{} · {}", entry.commit.author, format::ago(entry.commit.time, now));
                            rsx! {
                                button {
                                    key: "{entry.commit.hash}",
                                    class: row_class(index, ""),
                                    role: "menuitem",
                                    tabindex: -1,
                                    title: "{entry.commit.title}",
                                    onmouseenter: move |_| highlighted.set(Some(index)),
                                    onclick: move |_| pick(index),
                                    span { class: "history-hash mono", "{entry.commit.hash.short()}" }
                                    span { class: "history-title", "{entry.commit.title}" }
                                    span { class: "history-who", "{who}" }
                                    span { class: "history-counts mono",
                                        if let Some(counts) = entry.counts {
                                            span { class: "added", "+{counts.added}" }
                                            span { class: "removed", "-{counts.removed}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                },
            }
            if loaded && entries.len() >= usize::from(MAX_LOG_COMMITS) {
                p { class: "history-note", "The last {MAX_LOG_COMMITS} commits that changed the file." }
            }
        }
    }
}

/// A commit's change to the file: a line that names the commit, with Back and its GitHub page,
/// then the diff.
#[component]
pub fn CommitChange(
    agent: AgentId,
    shown: Loadable<GitCommitFile>,
    language_path: PathBuf,
    github: Option<GitHubLinks>,
) -> Element {
    let cockpit: Cockpit = use_context();
    let language = Language::from_path(&language_path);
    let back = rsx! {
        button {
            class: "link-button commit-back",
            onclick: move |_| cockpit.update(|state| git::show_commit(state, agent, None)),
            Icon { kind: IconKind::ChevronLeft, size: 13 }
            "Back"
        }
    };
    let shown = match shown {
        Loadable::NotRequested | Loadable::Loading => {
            return rsx! {
                div { class: "commit-bar", {back} }
                div { class: FILE_LOADING_CLASS, aria_hidden: true,
                    for (index, width) in ["80%", "65%", "72%"].iter().enumerate() {
                        div { key: "{index}", class: "skeleton skeleton-line", style: "width: {width}" }
                    }
                }
            };
        }
        Loadable::Failed(error) => {
            return rsx! {
                div { class: "commit-bar", {back} }
                p { class: "file-note", "The commit's change did not load: {error}" }
            };
        }
        Loadable::Loaded(shown) => shown,
    };
    let commit = &shown.commit;
    let ago = format::ago(commit.time, SystemTime::now());
    let page = shown_commit_page(github.as_ref(), &shown);
    let note = commit_note(&shown.texts);
    let (before, after) = match &shown.texts {
        CommitTexts::Text { before, after } => (
            before.clone().unwrap_or_default(),
            after.clone().unwrap_or_default(),
        ),
        CommitTexts::NotText => (String::new(), String::new()),
    };
    rsx! {
        div { class: "commit-bar",
            {back}
            span { class: "mono commit-hash", "{commit.hash.short()}" }
            span { class: "commit-title", title: "{commit.title}", "{commit.title}" }
            span { class: "commit-meta", "{commit.author}, {ago}" }
            if let Some(page) = page {
                button {
                    class: "link-button commit-page",
                    onclick: move |_| cockpit.open_in_browser(page.clone()),
                    Icon { kind: IconKind::External, size: 12 }
                    "GitHub"
                }
            }
        }
        if let Some(note) = note {
            p { class: "file-note", "{note}" }
        }
        if shows_diff(&shown.texts) {
            {git_diff(&before, &after, String::new(), language, LineMarks::marked(None))}
        }
    }
}

/// The shown commit's page on GitHub, when the remote holds it. The checkout's state lists the
/// unpushed commits of HEAD only, and the commit may have left HEAD's history since: the answer
/// says.
fn shown_commit_page(github: Option<&GitHubLinks>, shown: &GitCommitFile) -> Option<String> {
    github
        .filter(|_| shown.on_remote == OnRemote::Held)
        .map(|links| links.held_commit_page(&shown.commit.hash))
}

/// Whether a commit's change to the file shows as a diff: an empty one would say "No uncommitted
/// change".
fn shows_diff(texts: &CommitTexts) -> bool {
    match texts {
        CommitTexts::Text { before, after } => {
            let before = before.as_deref().unwrap_or_default();
            let after = after.as_deref().unwrap_or_default();
            render::change_count(before, after) > 0
        }
        CommitTexts::NotText => false,
    }
}

/// The line above a commit's change to the file, when the diff alone would not say it.
fn commit_note(texts: &CommitTexts) -> Option<&'static str> {
    let CommitTexts::Text { before, after } = texts else {
        return Some("The file is binary or too large to show.");
    };
    match (before, after) {
        (None, Some(_)) => Some("This commit added the file."),
        (Some(_), None) => Some("This commit deleted the file."),
        (None, None) => Some("This commit did not change the file."),
        (Some(_), Some(_)) if !shows_diff(texts) => {
            Some("This commit did not change the file's lines.")
        }
        (Some(_), Some(_)) => None,
    }
}

/// Whether the shown mode's segment is chosen: none while a commit shows.
pub fn mode_chosen(mode: ViewMode, choice: ViewMode, commit_shown: bool) -> bool {
    !commit_shown && choice == mode
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::Timestamp;

    use super::*;

    #[test]
    fn a_commits_card_gives_its_date() {
        // 15 March 2026 at noon UTC: the 15th in every time zone.
        let time = Timestamp::from_unix_millis(1_773_576_000_000);
        let commit = Commit {
            hash: CommitHash::new(&"a".repeat(40)).unwrap(),
            title: "Add the lexer".to_owned(),
            body: String::new(),
            author: "Dev".to_owned(),
            time,
        };
        let now = time
            .to_system_time()
            .and_then(|time| time.checked_add(Duration::from_secs(40 * 24 * 3600)))
            .unwrap();

        let meta = card_meta(&commit, now);

        assert!(meta.starts_with("Dev, "), "{meta}");
        assert!(meta.contains("15 Mar 2026"), "{meta}");
    }

    #[test]
    fn a_shown_commit_links_to_github_only_when_the_remote_holds_it() {
        use cockpit_protocol::{
            BranchState, CheckoutKind, CheckoutState, Completeness, Remote, Repository,
        };

        let hash = CommitHash::new(&"a".repeat(40)).unwrap();
        // The state does not list the commit as unpushed, as when it left HEAD's history.
        let checkout = CheckoutState {
            root: PathBuf::from("/work/app"),
            remote: Some(Remote {
                name: "origin".to_owned(),
                repository: Repository::from_remote("github.com/acme/app"),
                unpushed: Vec::new(),
                unpushed_completeness: Completeness::Complete,
            }),
            kind: CheckoutKind::MainWorktree,
            branch: BranchState {
                name: Some("main".to_owned()),
                head: None,
                upstream: None,
            },
            files: Vec::new(),
            completeness: Completeness::Complete,
        };
        let links = GitHubLinks::of(&checkout);
        let shown = |on_remote| GitCommitFile {
            commit: Commit {
                hash: hash.clone(),
                title: "Amended away".to_owned(),
                body: String::new(),
                author: "Dev".to_owned(),
                time: Timestamp::from_unix_millis(0),
            },
            texts: CommitTexts::NotText,
            on_remote,
        };

        let held = shown_commit_page(links.as_ref(), &shown(OnRemote::Held));
        let lacking = shown_commit_page(links.as_ref(), &shown(OnRemote::MayLack));

        assert_eq!(
            held,
            Some(format!("https://github.com/acme/app/commit/{hash}"))
        );
        assert_eq!(lacking, None);
    }

    #[test]
    fn a_commit_that_left_the_lines_as_they_were_says_so() {
        let shown = |before: Option<&str>, after: Option<&str>| CommitTexts::Text {
            before: before.map(str::to_owned),
            after: after.map(str::to_owned),
        };

        assert_eq!(
            commit_note(&shown(Some("run\n"), Some("run\n"))),
            Some("This commit did not change the file's lines.")
        );
        assert_eq!(commit_note(&shown(Some("run\n"), Some("go\n"))), None);
        assert!(!shows_diff(&shown(None, Some(""))), "an empty file added");
        assert!(shows_diff(&shown(None, Some("run\n"))));
    }

    #[test]
    fn no_two_hovers_share_a_serial() {
        let first = next_serial();
        let second = next_serial();

        assert_ne!(first, second);
    }

    #[test]
    fn the_history_button_waits_only_for_an_answer_that_comes() {
        let committed = DiffBase::Git {
            head: Some(Commit {
                hash: CommitHash::new(&"a".repeat(40)).unwrap(),
                title: "Start".to_owned(),
                body: String::new(),
                author: "Dev".to_owned(),
                time: Timestamp::from_unix_millis(0),
            }),
            text: String::new(),
            new: false,
        };

        assert_eq!(history_button(&DiffBase::Waiting, true), Some(false));
        assert_eq!(history_button(&committed, true), Some(true));
        assert_eq!(
            history_button(&DiffBase::Waiting, false),
            None,
            "git is not asked about an image"
        );
        assert_eq!(
            history_button(&DiffBase::Edits { outside: false }, true),
            None
        );
    }

    #[test]
    fn each_line_of_a_file_new_since_the_last_commit_is_not_committed_yet() {
        let base = DiffBase::Git {
            head: None,
            text: String::new(),
            new: true,
        };

        let blames = line_blames(&base, None, "one\ntwo\n", SystemTime::now());

        let not_committed = LineBlame {
            text: NOT_COMMITTED.to_owned(),
            commit: None,
        };
        assert_eq!(blames, [not_committed.clone(), not_committed]);
    }
}
