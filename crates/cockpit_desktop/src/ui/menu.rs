//! A menu at the pointer that answers the keys: it takes the focus as it opens, the arrows move
//! one highlight that the pointer moves too, Enter or a row's letter picks a row, and Esc closes
//! the menu and gives the focus back.

use dioxus::prelude::*;

use crate::bridge::{self, HIGHLIGHTED_CLASS};
use crate::state::menu_keys::{self, KeyedRow, MenuKey};

/// The key a key press is to a menu. Digits come from the key's place, since some layouts
/// (AZERTY) need Shift to type them.
pub fn menu_key(key: &Key, code: Code) -> Option<MenuKey> {
    let digit = match code {
        Code::Digit1 | Code::Numpad1 => Some('1'),
        Code::Digit2 | Code::Numpad2 => Some('2'),
        Code::Digit3 | Code::Numpad3 => Some('3'),
        Code::Digit4 | Code::Numpad4 => Some('4'),
        Code::Digit5 | Code::Numpad5 => Some('5'),
        Code::Digit6 | Code::Numpad6 => Some('6'),
        Code::Digit7 | Code::Numpad7 => Some('7'),
        Code::Digit8 | Code::Numpad8 => Some('8'),
        Code::Digit9 | Code::Numpad9 => Some('9'),
        _ => None,
    };
    if let Some(digit) = digit {
        return Some(MenuKey::Char(digit));
    }
    match key {
        Key::ArrowUp => Some(MenuKey::Up),
        Key::ArrowDown => Some(MenuKey::Down),
        Key::ArrowRight => Some(MenuKey::Right),
        Key::ArrowLeft => Some(MenuKey::Left),
        Key::Enter => Some(MenuKey::Enter),
        Key::Character(text) if text == " " => Some(MenuKey::Enter),
        Key::Character(text) => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(letter), None) => Some(MenuKey::Char(letter.to_ascii_lowercase())),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The menu's frame, at `x`, `y` in the window's pixels: it takes the focus as it opens and passes
/// on the keys it takes. Esc reaches the app through the page's script, which closes the menu:
/// the focus then goes to `focus_back`, or to where the menu took it from.
#[component]
pub fn KeyedMenu(
    x: i32,
    y: i32,
    focus_back: Option<&'static str>,
    on_key: EventHandler<MenuKey>,
    children: Element,
) -> Element {
    rsx! {
        div {
            class: "popover menu row-menu keyed-menu",
            role: "menu",
            tabindex: -1,
            "data-own-keys": true,
            style: "--menu-x: {x}px; --menu-y: {y}px",
            onmounted: move |event| async move {
                // The menu may have closed already: nothing to focus then.
                let _ = event.data().set_focus(true).await;
            },
            onkeydown: move |event: KeyboardEvent| {
                let modifiers = event.modifiers();
                if modifiers.ctrl() || modifiers.alt() || modifiers.meta() || event.is_composing() {
                    return;
                }
                match event.key() {
                    // The menu keeps the focus while it is open, or its keys would stop working.
                    Key::Tab => {
                        event.prevent_default();
                        return;
                    }
                    Key::Escape => {
                        bridge::give_focus_back(focus_back);
                        return;
                    }
                    _ => {}
                }
                let Some(key) = menu_key(&event.key(), event.code()) else {
                    return;
                };
                event.prevent_default();
                on_key.call(key);
            },
            {children}
        }
    }
}

/// A row of a menu without submenus.
#[derive(Debug, Clone, PartialEq)]
pub struct FlatRow {
    pub label: &'static str,
    /// The letter that picks it, in lower case.
    pub key: char,
    pub enabled: bool,
    /// What the row does, or why it cannot run.
    pub title: String,
    /// A line divides it from the row above.
    pub divided: bool,
}

/// A menu without submenus. A pick by the pointer, Enter or a letter gives the focus back, then
/// calls `on_pick` with the row's index, which may move the focus elsewhere.
#[component]
pub fn FlatMenu(
    x: i32,
    y: i32,
    rows: Vec<FlatRow>,
    focus_back: Option<&'static str>,
    on_pick: EventHandler<usize>,
) -> Element {
    let mut highlighted = use_signal(|| None::<usize>);
    let keyed: Vec<KeyedRow> = rows
        .iter()
        .map(|row| KeyedRow {
            key: row.key,
            enabled: row.enabled,
        })
        .collect();
    let pick = move |index: usize| {
        bridge::give_focus_back(focus_back);
        on_pick.call(index);
    };
    let now = highlighted();
    rsx! {
        KeyedMenu {
            x,
            y,
            focus_back,
            on_key: move |key: MenuKey| {
                let step = menu_keys::step(&keyed, *highlighted.peek(), &key);
                if step.highlighted != *highlighted.peek() {
                    highlighted.set(step.highlighted);
                }
                if let Some(index) = step.picked {
                    pick(index);
                }
            },
            for (index, row) in rows.into_iter().enumerate() {
                if row.divided {
                    div { class: "menu-divider" }
                }
                button {
                    class: if now == Some(index) { "menu-item {HIGHLIGHTED_CLASS}" } else { "menu-item" },
                    role: "menuitem",
                    tabindex: -1,
                    disabled: !row.enabled,
                    title: "{row.title}",
                    onmouseenter: move |_| {
                        let pointed = row.enabled.then_some(index);
                        if *highlighted.peek() != pointed {
                            highlighted.set(pointed);
                        }
                    },
                    onclick: move |_| pick(index),
                    span { class: "menu-label", "{row.label}" }
                    span { class: "menu-hint", "{row.key.to_ascii_uppercase()}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_reads_as_its_lower_case_letter() {
        assert_eq!(
            menu_key(&Key::Character("W".to_owned()), Code::KeyZ),
            Some(MenuKey::Char('w'))
        );
        assert_eq!(
            menu_key(&Key::Character(" ".to_owned()), Code::Space),
            Some(MenuKey::Enter)
        );
        assert_eq!(menu_key(&Key::Tab, Code::Tab), None);
    }

    #[test]
    fn a_digit_key_reads_as_its_digit_on_a_layout_that_needs_shift_for_digits() {
        // An AZERTY keyboard gives "&" and "é" on the keys of 1 and 2 without Shift.
        assert_eq!(
            menu_key(&Key::Character("&".to_owned()), Code::Digit1),
            Some(MenuKey::Char('1'))
        );
        assert_eq!(
            menu_key(&Key::Character("é".to_owned()), Code::Digit2),
            Some(MenuKey::Char('2'))
        );
        assert_eq!(
            menu_key(&Key::Character("3".to_owned()), Code::Numpad3),
            Some(MenuKey::Char('3'))
        );
        assert_eq!(
            menu_key(&Key::Character("'".to_owned()), Code::Digit4),
            Some(MenuKey::Char('4'))
        );
    }
}
