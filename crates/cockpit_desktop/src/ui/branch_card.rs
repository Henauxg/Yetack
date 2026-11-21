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
const HIDE_AFTER: Duration = Duration::from_millis(250);

/// The pointer's part in the card, which the rows, the header and the card share.
#[derive(Clone, Copy)]
pub struct CardHover {
    /// Counts the pointer's moves onto and off a row or the card: a delayed show or hide runs
    /// only if no move came after it.
    moves: Signal<u64>,
    /// The pointer is over the card.
    over_card: Signal<bool>,
}

/// Provides [`CardHover`] to the window.
pub fn use_card_hover() {
    use_context_provider(|| CardHover {
        moves: Signal::new(0),
        over_card: Signal::new(false),
    });
}

impl CardHover {
    fn next(mut self) -> u64 {
        let next = self.moves.peek().wrapping_add(1);
        self.moves.set(next);
        next
    }

    /// The pointer came onto `agent`'s row or branch line, at `x`, `y`: its card shows after a
    /// rest, or at once when another card shows already.
    pub fn enter(self, cockpit: Cockpit, agent: AgentId, x: i32, y: i32) {
        let this_move = self.next();
        let card = BranchCard { agent, x, y };
        if cockpit.state.peek().ui.branch_card.is_some() {
            cockpit.update(|state| state.ui.branch_card = Some(card));
            return;
        }
        spawn(async move {
            tokio::time::sleep(SHOW_AFTER).await;
            if *self.moves.peek() == this_move {
                cockpit.update(|state| state.ui.branch_card = Some(card));
            }
        });
    }

    /// The pointer left a row, the branch line or the card: the card goes, unless the pointer
    /// comes back onto one meanwhile.
    pub fn leave(self, cockpit: Cockpit) {
        let this_move = self.next();
        spawn(async move {
            tokio::time::sleep(HIDE_AFTER).await;
            if *self.moves.peek() == this_move && !*self.over_card.peek() {
                cockpit.update(|state| state.ui.branch_card = None);
            }
        });
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
    let mut over_card = hover.over_card;
    rsx! {
        div {
            class: if shown.leaving { "branch-card leaving" } else { "branch-card" },
            style: "--card-x: {card.x}px; --card-y: {card.y}px",
            inert: shown.leaving.then_some(true),
            onmouseenter: move |_| {
                over_card.set(true);
                hover.next();
            },
            onmouseleave: move |_| {
                over_card.set(false);
                hover.leave(cockpit);
            },
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
}
