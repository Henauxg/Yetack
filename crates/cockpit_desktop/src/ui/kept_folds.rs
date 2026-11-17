//! What a click decided for each fold of the chat, by agent and fold. A row the chat draws again
//! keeps it: a row moves into a run of work when the next work row comes, and Dioxus mounts it
//! afresh there.

use std::collections::BTreeMap;

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

use crate::ui::find_folds::FoldKey;

/// The decisions of the folds, which the app provides to the chat and to the agent map: a call
/// opened in one shows open in the other.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct KeptFolds(Signal<BTreeMap<(AgentId, FoldKey), bool>>);

impl KeptFolds {
    /// Opens `fold`, as a click on it would.
    pub(super) fn open(self, agent: AgentId, fold: FoldKey) {
        let Self(mut folds) = self;
        folds.write().insert((agent, fold), true);
    }
}

/// Provides the kept folds to the components under the caller: the chat's rows, the agent map,
/// and the links that take the chat to a row, which open the fold that holds it.
pub(super) fn use_kept_folds() {
    let folds = use_signal(BTreeMap::new);
    use_context_provider(|| KeptFolds(folds));
}

/// A fold's decision: open or closed after a click, `None` before one.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct KeptFold {
    folds: Option<KeptFolds>,
    /// The decision of a click since the fold was drawn. Without a provider, the fold's only one.
    clicked: Signal<Option<bool>>,
    key: (AgentId, FoldKey),
    decided: Option<bool>,
}

impl KeptFold {
    pub(super) fn decided(self) -> Option<bool> {
        self.decided
    }

    /// Whether a click decided since the fold was drawn: only then does its body animate as it
    /// opens. A fold drawn again shows the decision it keeps at once.
    pub(super) fn clicked(self) -> bool {
        self.clicked.read().is_some()
    }

    /// `base`, with `instant` when the fold shows the decision it keeps, which turns the body's
    /// opening animation off.
    pub(super) fn body_class(self, base: &str) -> String {
        if self.clicked() {
            base.to_owned()
        } else {
            format!("{base} instant")
        }
    }

    pub(super) fn set(self, open: bool) {
        if let Some(KeptFolds(mut folds)) = self.folds {
            folds.write().insert(self.key, open);
        }
        let mut clicked = self.clicked;
        clicked.set(Some(open));
    }
}

/// `fold`'s decision in `agent`'s chat. A memo, so that a click on another fold does not draw
/// this row again.
pub(super) fn use_kept_fold(agent: AgentId, fold: FoldKey) -> KeptFold {
    let folds = try_use_context::<KeptFolds>();
    let clicked = use_signal(|| None::<bool>);
    let key = (agent, fold);
    let decided = use_memo(use_reactive((&key,), move |(key,)| match folds {
        Some(KeptFolds(folds)) => folds.read().get(&key).copied(),
        None => clicked(),
    }))();
    KeptFold {
        folds,
        clicked,
        key,
        decided,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use cockpit_protocol::ItemId;
    use dioxus::dioxus_core::NoOpMutations;

    use super::*;

    thread_local! {
        static SHOWN: RefCell<Option<Signal<bool>>> = const { RefCell::new(None) };
        static FOLD: RefCell<Option<KeptFold>> = const { RefCell::new(None) };
        static DRAWN: Cell<Option<(Option<bool>, bool)>> = const { Cell::new(None) };
    }

    /// The app's kept folds, and a fold the test draws and takes away.
    #[component]
    fn Folds() -> Element {
        use_kept_folds();
        let shown = use_signal(|| true);
        SHOWN.with(|slot| slot.replace(Some(shown)));
        rsx! {
            if shown() {
                Fold {}
            }
        }
    }

    #[component]
    fn Fold() -> Element {
        let fold = use_kept_fold(AgentId::new(1), FoldKey::Item(ItemId::new(7)));
        FOLD.with(|slot| slot.replace(Some(fold)));
        DRAWN.with(|drawn| drawn.set(Some((fold.decided(), fold.clicked()))));
        rsx! {}
    }

    /// A row that moves into a run of work is drawn again: its fold stays open, and shows open at
    /// once instead of unfolding again.
    #[test]
    fn a_fold_drawn_again_keeps_its_decision_without_its_click() {
        let mut dom = VirtualDom::new(Folds);
        dom.rebuild_in_place();
        let mut shown = SHOWN
            .with(|slot| *slot.borrow())
            .expect("the root provides its switch");
        let fold = FOLD.with(|slot| *slot.borrow()).expect("the fold is drawn");
        let drawn = || DRAWN.with(Cell::get);

        dom.in_runtime(|| fold.set(true));
        dom.render_immediate(&mut NoOpMutations);
        let clicked = drawn();
        dom.in_runtime(|| shown.set(false));
        dom.render_immediate(&mut NoOpMutations);
        dom.in_runtime(|| shown.set(true));
        dom.render_immediate(&mut NoOpMutations);

        assert_eq!(clicked, Some((Some(true), true)));
        assert_eq!(drawn(), Some((Some(true), false)));
    }
}
