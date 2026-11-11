//! The keys of a menu that takes the focus as it opens: the arrows move one highlight among the
//! rows that can run, Enter picks the highlighted row, and a row's letter picks it.

use super::actions::Direction;

/// A key the menu takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuKey {
    Up,
    Down,
    Right,
    Left,
    Enter,
    Char(char),
}

/// A row of a menu without submenus, as the keys see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyedRow {
    /// The letter that picks it, in lower case.
    pub key: char,
    pub enabled: bool,
}

/// What a key did in a menu without submenus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuStep {
    pub highlighted: Option<usize>,
    /// The row the key picks.
    pub picked: Option<usize>,
}

/// After `key`, with the row `highlighted` lit: the highlight, and the row the key picks. A row
/// that cannot run is never highlighted nor picked.
pub fn step(rows: &[KeyedRow], highlighted: Option<usize>, key: &MenuKey) -> MenuStep {
    let enabled: Vec<bool> = rows.iter().map(|row| row.enabled).collect();
    let stay = MenuStep {
        highlighted,
        picked: None,
    };
    match key {
        MenuKey::Up => MenuStep {
            highlighted: next(&enabled, highlighted, Direction::Up),
            picked: None,
        },
        MenuKey::Down => MenuStep {
            highlighted: next(&enabled, highlighted, Direction::Down),
            picked: None,
        },
        MenuKey::Left | MenuKey::Right => stay,
        MenuKey::Enter => MenuStep {
            highlighted,
            picked: highlighted.filter(|&at| enabled.get(at) == Some(&true)),
        },
        MenuKey::Char(letter) => {
            let picked = rows
                .iter()
                .position(|row| row.enabled && row.key == *letter);
            MenuStep {
                highlighted: picked.or(highlighted),
                picked,
            }
        }
    }
}

/// The next row that can run after `from`: with none highlighted, the first or the last. The
/// highlight stays at the ends.
pub fn next(enabled: &[bool], from: Option<usize>, direction: Direction) -> Option<usize> {
    let mut runnable = enabled
        .iter()
        .enumerate()
        .filter(|&(_, &can_run)| can_run)
        .map(|(index, _)| index);
    let found = match direction {
        Direction::Down => runnable.find(|&index| from.is_none_or(|from| index > from)),
        Direction::Up => runnable.rfind(|&index| from.is_none_or(|from| index < from)),
    };
    found.or(from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "Close" (w), a disabled "Close the others" (o), then "Copy the path" (c).
    fn rows() -> [KeyedRow; 3] {
        [
            KeyedRow {
                key: 'w',
                enabled: true,
            },
            KeyedRow {
                key: 'o',
                enabled: false,
            },
            KeyedRow {
                key: 'c',
                enabled: true,
            },
        ]
    }

    fn press(highlighted: Option<usize>, keys: &[MenuKey]) -> MenuStep {
        keys.iter().fold(
            MenuStep {
                highlighted,
                picked: None,
            },
            |last, key| step(&rows(), last.highlighted, key),
        )
    }

    #[test]
    fn the_arrows_skip_a_row_that_cannot_run_and_stop_at_the_ends() {
        assert_eq!(press(None, &[MenuKey::Down]).highlighted, Some(0));
        assert_eq!(press(None, &[MenuKey::Up]).highlighted, Some(2));
        assert_eq!(press(Some(0), &[MenuKey::Down]).highlighted, Some(2));
        assert_eq!(press(Some(2), &[MenuKey::Up]).highlighted, Some(0));
        assert_eq!(press(Some(2), &[MenuKey::Down]).highlighted, Some(2));
        assert_eq!(press(Some(0), &[MenuKey::Up]).highlighted, Some(0));
    }

    #[test]
    fn enter_picks_the_highlighted_row() {
        let picked = press(None, &[MenuKey::Down, MenuKey::Down, MenuKey::Enter]);

        assert_eq!(picked.picked, Some(2));
        assert_eq!(press(None, &[MenuKey::Enter]).picked, None);
        assert_eq!(press(Some(1), &[MenuKey::Enter]).picked, None);
    }

    #[test]
    fn a_letter_picks_its_row_unless_it_cannot_run() {
        let copy = press(Some(0), &[MenuKey::Char('c')]);

        assert_eq!(copy.picked, Some(2));
        assert_eq!(copy.highlighted, Some(2));
        assert_eq!(press(Some(0), &[MenuKey::Char('o')]).picked, None);
        assert_eq!(
            press(Some(0), &[MenuKey::Char('z')]),
            MenuStep {
                highlighted: Some(0),
                picked: None,
            }
        );
    }

    #[test]
    fn left_and_right_do_nothing_without_a_submenu() {
        let after = press(Some(2), &[MenuKey::Left, MenuKey::Right]);

        assert_eq!(
            after,
            MenuStep {
                highlighted: Some(2),
                picked: None,
            }
        );
    }
}
