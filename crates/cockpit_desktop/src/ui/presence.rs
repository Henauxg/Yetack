//! Keeps a closed dialog or popover on screen while its exit animation plays. Dioxus removes an
//! element at once, so the element stays here, marked `leaving`, for `EXIT`. The state closes at
//! once: a leaving element is inert, so clicks and keys already go to what is under it.

use std::time::Duration;

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

/// At least the stylesheet's `--motion-out`, which every exit animation lasts.
const EXIT: Duration = Duration::from_millis(120);

/// What to draw while a value is open or leaving.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown<T> {
    pub value: T,
    /// Closed: the element plays its exit animation and takes no input.
    pub leaving: bool,
    /// Counts the openings. As a key, it makes a reopened element mount afresh instead of
    /// taking over the leaving one and the text typed in its fields.
    pub opening: u32,
}

#[derive(Debug, Clone, PartialEq)]
enum Phase<T> {
    Hidden,
    Open(T),
    /// `timer` names the wait that ends it.
    Leaving {
        value: T,
        timer: u32,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct PresenceState<T> {
    phase: Phase<T>,
    openings: u32,
    timers: u32,
}

impl<T> Default for PresenceState<T> {
    fn default() -> Self {
        Self {
            phase: Phase::Hidden,
            openings: 0,
            timers: 0,
        }
    }
}

impl<T: Clone> PresenceState<T> {
    /// Follows `current`, the value the state holds now, given the last wait that ended.
    /// Returns the wait to start when the value starts leaving.
    fn update(&mut self, current: Option<T>, ended: u32) -> Option<u32> {
        let phase = std::mem::replace(&mut self.phase, Phase::Hidden);
        let (next, timer) = match (current, phase) {
            (Some(value), Phase::Open(_)) => (Phase::Open(value), None),
            (Some(value), Phase::Hidden | Phase::Leaving { .. }) => {
                self.openings = self.openings.wrapping_add(1);
                (Phase::Open(value), None)
            }
            (None, Phase::Open(value)) => {
                self.timers = self.timers.wrapping_add(1);
                let timer = self.timers;
                (Phase::Leaving { value, timer }, Some(timer))
            }
            (None, Phase::Leaving { timer, .. }) if timer == ended => (Phase::Hidden, None),
            (None, leaving @ Phase::Leaving { .. }) => (leaving, None),
            (None, Phase::Hidden) => (Phase::Hidden, None),
        };
        self.phase = next;
        timer
    }

    fn shown(&self) -> Option<Shown<T>> {
        let (value, leaving) = match &self.phase {
            Phase::Hidden => return None,
            Phase::Open(value) => (value.clone(), false),
            Phase::Leaving { value, .. } => (value.clone(), true),
        };
        Some(Shown {
            value,
            leaving,
            opening: self.openings,
        })
    }
}

/// `current` while it is `Some`, then its last value, leaving, for `EXIT`.
pub fn use_presence<T: Clone + 'static>(current: Option<T>) -> Option<Shown<T>> {
    let mut presence = use_hook(|| CopyValue::new(PresenceState::<T>::default()));
    let mut ended = use_signal(|| 0_u32);
    // Read on every render, so that the end of a wait renders the component again.
    let last_ended = ended();
    let timer = presence.with_mut(|presence| presence.update(current, last_ended));
    if let Some(timer) = timer {
        spawn(async move {
            tokio::time::sleep(EXIT).await;
            ended.set(timer);
        });
    }
    presence.read().shown()
}

/// What to draw of an element that belongs to one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentShown {
    pub agent: AgentId,
    /// Closed: the element plays its exit animation and takes no input.
    pub leaving: bool,
    /// Counts the openings, as [`Shown::opening`] does.
    pub opening: u32,
    /// It opened as its agent came on screen, as after a switch: it draws at once, without its
    /// opening animation, for as long as it stays open.
    pub instant: bool,
}

impl AgentShown {
    /// The element's class: `base`, with `leaving` or `instant` when they apply.
    pub fn class(self, base: &str) -> String {
        match (self.leaving, self.instant) {
            (true, _) => format!("{base} leaving"),
            (false, true) => format!("{base} instant"),
            (false, false) => base.to_owned(),
        }
    }
}

/// An element that belongs to one agent, open for the agent `open` names, while the window
/// draws `on_screen`'s layout. A switch shows the other agent's element at once, and only the
/// agent on screen sees its element leave.
///
/// The app's `switching` class cannot do it for an animation: it lasts one frame, and the
/// opening animation starts as the class goes.
pub fn use_agent_presence(on_screen: Option<AgentId>, open: Option<AgentId>) -> Option<AgentShown> {
    let mut openings = use_hook(|| CopyValue::new(SwitchOpenings::default()));
    let shown = use_presence(open);
    let instant = openings.with_mut(|openings| openings.update(on_screen, shown.as_ref()));
    let shown = shown.filter(|shown| !shown.leaving || Some(shown.value) == on_screen)?;
    Some(AgentShown {
        agent: shown.value,
        leaving: shown.leaving,
        opening: shown.opening,
        instant,
    })
}

/// Whether an agent's element opened as its agent came on screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SwitchOpenings {
    /// The agent the last render drew, `None` before the first render.
    drawn: Option<Option<AgentId>>,
    /// The element's last opening.
    opening: u32,
    with_agent: bool,
}

impl SwitchOpenings {
    /// Follows a render that draws `on_screen` and shows `shown`. True when `shown` is open and
    /// opened with its agent, the first render included.
    fn update(&mut self, on_screen: Option<AgentId>, shown: Option<&Shown<AgentId>>) -> bool {
        let switched = self.drawn != Some(on_screen);
        self.drawn = Some(on_screen);
        let Some(shown) = shown.filter(|shown| !shown.leaving) else {
            return false;
        };
        if shown.opening != self.opening {
            self.opening = shown.opening;
            self.with_agent = switched;
        }
        self.with_agent
    }
}

/// Shows `children` while `open`, and during their exit animation after.
#[component]
pub fn Presence(open: bool, children: Element) -> Element {
    let Some(shown) = use_presence(open.then_some(())) else {
        return rsx! {};
    };
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            {children}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app draws agent 1, whose element is closed, then switches to agent 2, whose element
    /// is open: that element draws at once while it stays open. Closed and opened again on the
    /// same agent, it plays its animation.
    #[test]
    fn an_element_that_opens_with_its_agent_draws_at_once() {
        let mut openings = SwitchOpenings::default();
        let first = Some(AgentId::new(1));
        let second = Some(AgentId::new(2));
        let open = |opening: u32| Shown {
            value: AgentId::new(2),
            leaving: false,
            opening,
        };

        let closed = openings.update(first, None);
        let on_switch = openings.update(second, Some(&open(1)));
        let later = openings.update(second, Some(&open(1)));
        let closing = openings.update(second, None);
        let reopened = openings.update(second, Some(&open(2)));

        assert!(!closed);
        assert!(on_switch, "it opened as its agent came on screen");
        assert!(later, "it stays so while it is open");
        assert!(!closing);
        assert!(!reopened, "opened on the agent on screen, it animates");
    }

    #[test]
    fn a_closed_value_leaves_until_its_wait_ends() {
        let mut presence = PresenceState::default();

        assert_eq!(presence.update(Some("menu"), 0), None);
        let opened = presence.shown().unwrap();
        assert!(!opened.leaving);

        let timer = presence.update(None, 0);
        assert_eq!(timer, Some(1));
        let leaving = presence.shown().unwrap();
        assert_eq!((leaving.value, leaving.leaving), ("menu", true));

        assert_eq!(presence.update(None, 1), None);
        assert_eq!(presence.shown(), None);
    }

    #[test]
    fn a_value_reopened_while_leaving_stays_and_counts_a_new_opening() {
        let mut presence = PresenceState::default();
        presence.update(Some("first"), 0);
        presence.update(None, 0);

        presence.update(Some("second"), 0);
        let reopened = presence.shown().unwrap();
        assert_eq!(
            (reopened.value, reopened.leaving, reopened.opening),
            ("second", false, 2)
        );

        // The first wait ends after the second closing: only the second wait hides it.
        assert_eq!(presence.update(None, 0), Some(2));
        presence.update(None, 1);
        assert!(presence.shown().is_some_and(|shown| shown.leaving));
        presence.update(None, 2);
        assert_eq!(presence.shown(), None);
    }

    #[test]
    fn the_exit_lasts_at_least_the_stylesheet_exit_animation() {
        const STYLESHEET: &str = include_str!("../style.css");
        const TOKEN: &str = "--motion-out:";

        let declared = STYLESHEET
            .lines()
            .find_map(|line| line.trim().strip_prefix(TOKEN))
            .and_then(|value| value.trim().strip_suffix("ms;"))
            .and_then(|millis| millis.parse::<u64>().ok());

        assert!(
            declared.is_some_and(|millis| Duration::from_millis(millis) <= EXIT),
            "`{TOKEN}` is {declared:?} ms, over the {EXIT:?} the element stays"
        );
    }
}
