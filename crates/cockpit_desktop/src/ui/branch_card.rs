//! The card of an agent's branch, which a short hover over its sidebar row or over the header's
//! branch line shows: the branch, what it has to push and to pull, the uncommitted files, the
//! last commit, and whether the folder is a worktree.

use std::time::{Duration, SystemTime};

use cockpit_protocol::{AgentId, CheckoutKind, CheckoutState, FileChange};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::state::{AgentEntry, BranchCard, Loadable, file_tree, git};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::use_presence;
use crate::{bridge, format, path_style};

/// How long the pointer rests on a row before its card shows.
const SHOW_AFTER: Duration = Duration::from_millis(600);
/// How long the card stays after the pointer leaves the row or the card, so that the pointer can
/// go from one to the other.
const HIDE_AFTER: Duration = Duration::from_millis(100);

/// The pointer's part in the card, which the rows, the header and the card share.
#[derive(Clone, Copy)]
pub struct CardHover {
    pointer: Signal<Pointer>,
}

/// Provides [`CardHover`] to the window.
pub fn use_card_hover() {
    use_context_provider(|| CardHover {
        pointer: Signal::new(Pointer::default()),
    });
}

impl CardHover {
    /// The pointer came onto `agent`'s row or branch line, at `x`, `y`: its card shows after a
    /// rest, or at once when another card shows already.
    pub fn enter(mut self, cockpit: Cockpit, agent: AgentId, x: i32, y: i32) {
        let this_move = self.pointer.write().onto_row();
        let card = BranchCard { agent, x, y };
        if cockpit.state.peek().ui.branch_card.is_some() {
            cockpit.update(|state| state.ui.branch_card = Some(card));
            return;
        }
        spawn(async move {
            tokio::time::sleep(SHOW_AFTER).await;
            if self.pointer.peek().shows(this_move) {
                cockpit.update(|state| state.ui.branch_card = Some(card));
            }
        });
    }

    /// The pointer left a row or the branch line: the card goes, unless the pointer comes onto
    /// the card or a row meanwhile.
    pub fn leave(mut self, cockpit: Cockpit) {
        let this_move = self.pointer.write().off();
        self.hide_after(this_move, cockpit);
    }

    fn enter_card(mut self) {
        self.pointer.write().onto_card();
    }

    fn leave_card(mut self, cockpit: Cockpit) {
        let this_move = self.pointer.write().off_card();
        self.hide_after(this_move, cockpit);
    }

    fn hide_after(self, this_move: u64, cockpit: Cockpit) {
        spawn(async move {
            tokio::time::sleep(HIDE_AFTER).await;
            if self.pointer.peek().hides(this_move) {
                cockpit.update(|state| state.ui.branch_card = None);
            }
        });
    }
}

/// Where the pointer is, for the card's delayed show and hide.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Pointer {
    /// Counts the pointer's moves onto and off a row or the card: a delayed show or hide runs
    /// only if no move came after it.
    moves: u64,
    over_card: bool,
}

impl Pointer {
    fn moved(&mut self) -> u64 {
        self.moves = self.moves.wrapping_add(1);
        self.moves
    }

    /// The pointer is off the card too, even when the card went from under it: Dioxus removes
    /// an element without a mouseleave.
    fn onto_row(&mut self) -> u64 {
        self.over_card = false;
        self.moved()
    }

    fn onto_card(&mut self) {
        self.over_card = true;
        self.moved();
    }

    fn off(&mut self) -> u64 {
        self.moved()
    }

    fn off_card(&mut self) -> u64 {
        self.over_card = false;
        self.moved()
    }

    /// Whether the show that waits since `this_move` runs.
    fn shows(self, this_move: u64) -> bool {
        self.moves == this_move
    }

    /// Whether the hide that waits since `this_move` runs.
    fn hides(self, this_move: u64) -> bool {
        self.moves == this_move && !self.over_card
    }
}

/// What the card shows.
#[derive(Debug, Clone, PartialEq)]
struct Shown {
    card: BranchCard,
    title: String,
    folder: String,
    status: CardStatus,
}

#[derive(Debug, Clone, PartialEq)]
enum CardStatus {
    Reading,
    Outside,
    Branch {
        label: String,
        github: Option<String>,
        checkout: Box<CheckoutState>,
    },
}

#[component]
pub fn BranchCardView() -> Element {
    let cockpit: Cockpit = use_context();
    let hover: CardHover = use_context();
    let open = {
        let state = cockpit.state.read();
        state.ui.branch_card.and_then(|card| {
            let entry = state.agents.get(&card.agent)?;
            let status = card_status(entry, state.reaches_agent(card.agent))?;
            Some(Shown {
                card,
                title: entry.summary.title.clone(),
                folder: path_style::shown(&entry.summary.folder),
                status,
            })
        })
    };
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    let Shown {
        card,
        title,
        folder,
        status,
    } = shown.value;
    rsx! {
        div {
            class: if shown.leaving { "branch-card leaving" } else { "branch-card" },
            style: "--card-x: {card.x}px; --card-y: {card.y}px",
            inert: shown.leaving.then_some(true),
            onmouseenter: move |_| hover.enter_card(),
            onmouseleave: move |_| hover.leave_card(cockpit),
            span { class: "branch-card-title", "{title}" }
            span { class: "branch-card-folder mono", title: "{folder}", "{folder}" }
            div { class: "branch-card-divider" }
            match status {
                CardStatus::Reading => rsx! {
                    span { class: "skeleton skeleton-line", style: "width: 60%" }
                    span { class: "skeleton skeleton-line", style: "width: 80%" }
                },
                CardStatus::Outside => rsx! {
                    span { class: "branch-card-note", "Not in a git checkout." }
                },
                CardStatus::Branch { label, github, checkout } => rsx! {
                    BranchFacts { agent: card.agent, label, github, checkout: *checkout }
                },
            }
        }
    }
}

/// What the card says of `entry`'s checkout. `None` shows no card: a daemon older than the git
/// requests answers with an error, and no answer comes from a machine the app does not reach.
fn card_status(entry: &AgentEntry, reachable: bool) -> Option<CardStatus> {
    match (&entry.git.status.value, git::branch_summary(entry)) {
        (_, Some(summary)) => Some(CardStatus::Branch {
            label: summary.branch,
            github: summary.github,
            checkout: Box::new(summary.checkout.clone()),
        }),
        (Loadable::Loaded(_), None) => Some(CardStatus::Outside),
        (Loadable::NotRequested | Loadable::Loading, None) if reachable => {
            Some(CardStatus::Reading)
        }
        (Loadable::NotRequested | Loadable::Loading | Loadable::Failed(_), None) => None,
    }
}

#[component]
fn BranchFacts(
    agent: AgentId,
    label: String,
    github: Option<String>,
    checkout: CheckoutState,
) -> Element {
    let cockpit: Cockpit = use_context();
    let branch = &checkout.branch;
    let upstream = match &branch.upstream {
        Some(upstream) => {
            let push = match upstream.ahead {
                0 => "nothing to push".to_owned(),
                ahead => format::count(
                    usize::try_from(ahead).unwrap_or(usize::MAX),
                    "commit to push",
                    "commits to push",
                ),
            };
            let pull = match upstream.behind {
                0 => "none to pull".to_owned(),
                behind => format!("{behind} to pull"),
            };
            format!("{push} to {}, {pull}", upstream.name)
        }
        None => "No upstream branch.".to_owned(),
    };
    let new = checkout
        .files
        .iter()
        .filter(|file| matches!(file.change, FileChange::Added | FileChange::Untracked))
        .count();
    let modified = checkout.files.len() - new;
    let uncommitted = match (modified, new) {
        (0, 0) => None,
        (modified, 0) => Some(format!("{modified} modified")),
        (0, new) => Some(format!("{new} new")),
        (modified, new) => Some(format!("{modified} modified, {new} new")),
    };
    let files = format::count(
        checkout.files.len(),
        "uncommitted file",
        "uncommitted files",
    );
    let last = branch.head.as_ref().map(|head| {
        (
            head.hash.short().to_owned(),
            head.title.clone(),
            format::ago(head.time, SystemTime::now()),
        )
    });
    // A detached HEAD has no branch name to copy.
    let name = branch.name.clone();
    let page = github.clone();
    rsx! {
        span { class: "branch-card-branch",
            Icon { kind: IconKind::Repository, size: 13 }
            span { class: "mono", "{label}" }
        }
        span { class: "branch-card-line", "{upstream}" }
        if let Some(uncommitted) = uncommitted {
            button {
                class: "link-button branch-card-line",
                title: "Show them in the files pane",
                onclick: move |_| cockpit.update(|state| {
                    state.ui.branch_card = None;
                    file_tree::open_on_changes(state, agent);
                }),
                "{files}: {uncommitted}"
            }
        }
        if let Some((hash, title, ago)) = last {
            span { class: "branch-card-line",
                "Last commit "
                span { class: "mono", "{hash}" }
                " \u{201c}{title}\u{201d}, {ago}"
            }
        }
        if checkout.kind == CheckoutKind::LinkedWorktree {
            span { class: "branch-card-note", "A worktree, beside the repository's main checkout." }
        }
        div { class: "branch-card-actions",
            if let Some(name) = name {
                button {
                    class: "link-button",
                    onclick: move |_| bridge::copy_to_clipboard(name.clone()),
                    Icon { kind: IconKind::Copy, size: 12 }
                    "Copy the branch name"
                }
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

#[cfg(test)]
mod tests {
    use cockpit_protocol::AgentStatus;

    use super::*;

    #[test]
    fn the_card_says_nothing_of_a_checkout_it_cannot_know() {
        let mut entry = AgentEntry::new(crate::state::summary(1, AgentStatus::Idle));
        let reading = card_status(&entry, true);
        let away = card_status(&entry, false);
        entry.git.status.value = Loadable::Failed("unknown method `agent.git_status`".to_owned());
        let older_daemon = card_status(&entry, true);

        assert_eq!(reading, Some(CardStatus::Reading));
        assert_eq!(
            away, None,
            "no answer comes from a machine the app does not reach"
        );
        assert_eq!(
            older_daemon, None,
            "with an older daemon, no card shows, as before"
        );
    }

    /// A click on the card's uncommitted files closes the card under the pointer.
    #[test]
    fn a_card_that_goes_from_under_the_pointer_does_not_keep_the_next_one() {
        let mut pointer = Pointer::default();
        pointer.onto_card();

        let rest = pointer.onto_row();
        let shown = pointer.shows(rest);
        let away = pointer.off();

        assert!(shown);
        assert!(pointer.hides(away));
    }

    #[test]
    fn the_card_stays_while_the_pointer_goes_from_the_row_onto_it() {
        let mut pointer = Pointer::default();
        let rest = pointer.onto_row();
        let shown = pointer.shows(rest);
        let away = pointer.off();
        pointer.onto_card();
        let kept = !pointer.hides(away);
        let gone = pointer.off_card();

        assert!(shown);
        assert!(kept);
        assert!(pointer.hides(gone));
    }
}
