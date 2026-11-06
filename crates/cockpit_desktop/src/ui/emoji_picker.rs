//! The grid of emoji that can start an agent's title, under the header's title. The arrow keys
//! move the focus in it, Enter picks the focused emoji, and Esc closes it as every popover.

use std::rc::Rc;

use cockpit_protocol::AgentId;
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::state::actions;
use crate::state::title_emoji::TITLE_EMOJI;

/// The emoji in a row of the grid.
const COLUMNS: usize = 8;

/// The emoji, then "No emoji" on a row of its own.
const PLACES: usize = TITLE_EMOJI.len() + 1;

/// A key that moves the focus in the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GridKey {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
}

impl GridKey {
    fn from_key(key: &Key) -> Option<Self> {
        match key {
            Key::ArrowLeft => Some(Self::Left),
            Key::ArrowRight => Some(Self::Right),
            Key::ArrowUp => Some(Self::Up),
            Key::ArrowDown => Some(Self::Down),
            Key::Home => Some(Self::Home),
            Key::End => Some(Self::End),
            _ => None,
        }
    }
}

/// The place `key` moves the focus to from `index`, among `count` places laid out `COLUMNS` to a
/// row. Nothing moves past an edge: Up on the first row stays where it is.
fn moved_in_grid(index: usize, key: GridKey, count: usize) -> usize {
    let last = count.saturating_sub(1);
    match key {
        GridKey::Left => index.saturating_sub(1),
        GridKey::Right => index.saturating_add(1).min(last),
        GridKey::Up => index.checked_sub(COLUMNS).unwrap_or(index),
        GridKey::Down => index.saturating_add(COLUMNS).min(last),
        GridKey::Home => 0,
        GridKey::End => last,
    }
}

/// `current` is the emoji the title starts with, which the grid marks and focuses first.
#[component]
pub fn EmojiPicker(agent: AgentId, current: Option<String>) -> Element {
    let cockpit: Cockpit = use_context();
    let start = current
        .as_deref()
        .and_then(|current| TITLE_EMOJI.iter().position(|&emoji| emoji == current))
        .unwrap_or(0);
    let mut focused = use_signal(|| start);
    let mut handles: Signal<Vec<Option<Rc<MountedData>>>> = use_signal(|| vec![None; PLACES]);
    let mut focus = move |index: usize| {
        focused.set(index);
        let handle = handles.peek().get(index).cloned().flatten();
        if let Some(handle) = handle {
            spawn(async move {
                // The picker may have closed already: nothing to focus then.
                let _ = handle.set_focus(true).await;
            });
        }
    };
    let mut mounted = move |index: usize, handle: Rc<MountedData>| {
        if let Some(slot) = handles.write().get_mut(index) {
            *slot = Some(Rc::clone(&handle));
        }
        if index == start {
            focus(index);
        }
    };
    let pick = move |emoji: Option<&'static str>| {
        cockpit.change(|state| actions::set_title_emoji(state, agent, emoji));
    };
    let none_index = TITLE_EMOJI.len();
    rsx! {
        div {
            class: "popover emoji-picker",
            role: "dialog",
            aria_label: "Emoji of the title",
            onkeydown: move |event: KeyboardEvent| {
                if let Some(key) = GridKey::from_key(&event.key()) {
                    event.prevent_default();
                    focus(moved_in_grid(focused(), key, PLACES));
                }
            },
            div {
                class: "emoji-grid",
                role: "group",
                aria_label: "Emoji",
                style: "grid-template-columns: repeat({COLUMNS}, auto)",
                for (index, emoji) in TITLE_EMOJI.into_iter().enumerate() {
                    button {
                        key: "{index}",
                        class: if current.as_deref() == Some(emoji) { "emoji-choice chosen" } else { "emoji-choice" },
                        aria_pressed: current.as_deref() == Some(emoji),
                        tabindex: if index == focused() { "0" } else { "-1" },
                        onmounted: move |event| mounted(index, event.data()),
                        onfocus: move |_| focused.set(index),
                        onclick: move |_| pick(Some(emoji)),
                        "{emoji}"
                    }
                }
            }
            button {
                class: if current.is_none() { "emoji-none chosen" } else { "emoji-none" },
                aria_pressed: current.is_none(),
                tabindex: if focused() == none_index { "0" } else { "-1" },
                onmounted: move |event| mounted(none_index, event.data()),
                onfocus: move |_| focused.set(none_index),
                onclick: move |_| pick(None),
                "No emoji"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arrows_move_in_the_grid_and_stop_at_its_edges() {
        let last_row = TITLE_EMOJI.len() - COLUMNS;
        let none = TITLE_EMOJI.len();
        let moved = |index, key| moved_in_grid(index, key, PLACES);

        assert_eq!(moved(0, GridKey::Right), 1);
        assert_eq!(moved(0, GridKey::Left), 0);
        assert_eq!(moved(3, GridKey::Up), 3);
        assert_eq!(moved(3, GridKey::Down), 3 + COLUMNS);
        // From the last row of emoji, Down reaches "No emoji", and Up goes back to the grid.
        assert_eq!(moved(last_row + 5, GridKey::Down), none);
        assert_eq!(moved(none, GridKey::Up), none - COLUMNS);
        assert_eq!(moved(none, GridKey::Right), none);
        assert_eq!(moved(9, GridKey::Home), 0);
        assert_eq!(moved(9, GridKey::End), none);
    }

    #[test]
    fn only_the_arrows_home_and_end_move_the_focus() {
        assert_eq!(GridKey::from_key(&Key::ArrowDown), Some(GridKey::Down));
        assert_eq!(GridKey::from_key(&Key::End), Some(GridKey::End));
        assert_eq!(GridKey::from_key(&Key::Enter), None);
        assert_eq!(GridKey::from_key(&Key::Escape), None);
    }
}
