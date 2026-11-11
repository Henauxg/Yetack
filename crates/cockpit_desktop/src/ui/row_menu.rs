//! The menu a right-click on a sidebar row opens, at the pointer: the agent's mark, its unread
//! dot, its name, its notifications and its working folder in two submenus, and its close.
//!
//! One highlight runs through the menu and its open submenu, moved by the pointer and the arrows.
//! While the menu is open, a letter or a digit picks the item that shows it.

use std::time::Duration;

use cockpit_protocol::{AgentId, Mark};
use dioxus::prelude::*;
use strum::VariantArray;

use crate::bridge::HIGHLIGHTED_CLASS;
use crate::connection::Cockpit;
use crate::keymap::{Command, Keymap};
use crate::path_style;
use crate::prefs::NotificationLevel;
use crate::state::actions::Direction;
use crate::state::menu_keys::{MenuKey, next};
use crate::state::{
    AppState, FolderAction, Popover, actions, notification_levels, to_handle, unread,
    working_folder,
};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::menu::KeyedMenu;
use crate::ui::notification_levels as levels_ui;
use crate::ui::presence::{Presence, use_presence};

/// How long the pointer may cross other rows on its way into the open submenu before the submenu
/// follows the row under the pointer.
const SUBMENU_DELAY: Duration = Duration::from_millis(200);

/// A row of the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    Mark,
    Unread,
    Rename,
    Notifications,
    Folder,
    Close,
}

/// The menu's rows, in order.
const ITEMS: [Item; 6] = [
    Item::Mark,
    Item::Unread,
    Item::Rename,
    Item::Notifications,
    Item::Folder,
    Item::Close,
];

impl Item {
    fn submenu(self) -> Option<Submenu> {
        match self {
            Self::Notifications => Some(Submenu::Notifications),
            Self::Folder => Some(Submenu::Folder),
            Self::Mark | Self::Unread | Self::Rename | Self::Close => None,
        }
    }

    /// What the row does when picked. A row that opens a submenu does nothing itself.
    fn pick(self) -> Option<Pick> {
        match self {
            Self::Mark => Some(Pick::Mark),
            Self::Unread => Some(Pick::Unread),
            Self::Rename => Some(Pick::Rename),
            Self::Close => Some(Pick::Close),
            Self::Notifications | Self::Folder => None,
        }
    }

    /// The key that picks it while the menu is open.
    fn key(self) -> Option<char> {
        match self {
            Self::Mark => Some('h'),
            Self::Unread => Some('u'),
            Self::Rename => Some('r'),
            Self::Close => Some('w'),
            Self::Notifications | Self::Folder => None,
        }
    }

    fn index(self) -> usize {
        ITEMS.iter().position(|&item| item == self).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Submenu {
    Notifications,
    Folder,
}

impl Submenu {
    /// The menu row that opens it.
    fn trigger(self) -> Item {
        match self {
            Self::Notifications => Item::Notifications,
            Self::Folder => Item::Folder,
        }
    }
}

/// What a click, Enter or a key picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Mark,
    Unread,
    Rename,
    Close,
    Level(NotificationLevel),
    Folder(FolderAction),
}

impl Pick {
    /// The key that picks it, for a submenu's row.
    fn key(self) -> Option<char> {
        match self {
            Self::Level(NotificationLevel::All) => Some('1'),
            Self::Level(NotificationLevel::NeedsYou) => Some('2'),
            Self::Level(NotificationLevel::Off) => Some('3'),
            Self::Folder(FolderAction::Fork) => Some('f'),
            Self::Folder(FolderAction::Move) => Some('m'),
            Self::Mark | Self::Unread | Self::Rename | Self::Close => None,
        }
    }
}

/// A submenu's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubRow {
    pick: Pick,
    enabled: bool,
}

/// What the menu shows, read as it opens.
#[derive(Debug, Clone, PartialEq)]
struct RowMenuShown {
    agent: AgentId,
    x: i32,
    y: i32,
    mark: Mark,
    unread: bool,
    notifications: NotificationLevel,
    /// The agent's working folder, as paths show.
    folder: String,
    /// Why the fork or the move cannot run now.
    fork_blocked: Option<&'static str>,
    move_blocked: Option<&'static str>,
}

impl RowMenuShown {
    fn rows(&self, submenu: Submenu) -> Vec<SubRow> {
        match submenu {
            Submenu::Notifications => NotificationLevel::VARIANTS
                .iter()
                .map(|&level| SubRow {
                    pick: Pick::Level(level),
                    enabled: true,
                })
                .collect(),
            Submenu::Folder => vec![
                SubRow {
                    pick: Pick::Folder(FolderAction::Fork),
                    enabled: self.fork_blocked.is_none(),
                },
                SubRow {
                    pick: Pick::Folder(FolderAction::Move),
                    enabled: self.move_blocked.is_none(),
                },
            ],
        }
    }
}

/// Where the highlight is, and which submenu is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Cursor {
    /// The highlighted row of the menu.
    item: Option<usize>,
    open: Option<Submenu>,
    /// The highlighted row of the open submenu. While it is set, the arrows move in the submenu.
    sub: Option<usize>,
}

impl Cursor {
    /// After a key: the cursor, and what the key picks.
    fn after_key(self, key: &MenuKey, shown: &RowMenuShown) -> (Self, Option<Pick>) {
        let in_submenu = self.open.zip(self.sub);
        match key {
            MenuKey::Down | MenuKey::Up => {
                let direction = if *key == MenuKey::Down {
                    Direction::Down
                } else {
                    Direction::Up
                };
                let moved = match in_submenu {
                    Some((submenu, at)) => Self {
                        sub: next(&enabled(&shown.rows(submenu)), Some(at), direction),
                        ..self
                    },
                    None => Self {
                        item: next(&[true; ITEMS.len()], self.item, direction),
                        open: None,
                        sub: None,
                    },
                };
                (moved, None)
            }
            MenuKey::Right => (self.opened(shown), None),
            MenuKey::Left => (
                Self {
                    open: None,
                    sub: None,
                    ..self
                },
                None,
            ),
            MenuKey::Enter => match in_submenu {
                Some((submenu, at)) => {
                    let row = shown.rows(submenu).get(at).copied();
                    (self, row.filter(|row| row.enabled).map(|row| row.pick))
                }
                None => {
                    let item = self.item.and_then(|index| ITEMS.get(index)).copied();
                    match item.and_then(Item::pick) {
                        Some(pick) => (self, Some(pick)),
                        None => (self.opened(shown), None),
                    }
                }
            },
            MenuKey::Char(letter) => {
                let sub_rows = self.open.map(|submenu| shown.rows(submenu));
                let in_open = sub_rows
                    .unwrap_or_default()
                    .into_iter()
                    .find(|row| row.enabled && row.pick.key() == Some(*letter))
                    .map(|row| row.pick);
                let in_menu = || {
                    ITEMS
                        .into_iter()
                        .find(|item| item.key() == Some(*letter))
                        .and_then(Item::pick)
                };
                (self, in_open.or_else(in_menu))
            }
        }
    }

    /// The highlighted row's submenu open, its first row that can run highlighted.
    fn opened(self, shown: &RowMenuShown) -> Self {
        let submenu = self
            .item
            .and_then(|index| ITEMS.get(index))
            .and_then(|item| item.submenu());
        match submenu {
            Some(submenu) => Self {
                open: Some(submenu),
                sub: next(&enabled(&shown.rows(submenu)), None, Direction::Down),
                ..self
            },
            None => self,
        }
    }

    /// The pointer on the menu's row `index`. The open submenu stays until `followed`.
    fn pointed(self, index: usize) -> Self {
        Self {
            item: Some(index),
            sub: None,
            ..self
        }
    }

    /// The highlighted row's submenu open, or none.
    fn followed(self) -> Self {
        let open = self
            .item
            .and_then(|index| ITEMS.get(index))
            .and_then(|item| item.submenu());
        if open == self.open {
            return self;
        }
        Self {
            open,
            sub: None,
            ..self
        }
    }

    /// The pointer on the open submenu's row `index`: its trigger stays highlighted too. A row
    /// that cannot run takes no highlight.
    fn pointed_in_submenu(self, index: usize, can_run: bool) -> Self {
        let Some(submenu) = self.open else {
            return self;
        };
        Self {
            item: Some(submenu.trigger().index()),
            open: Some(submenu),
            sub: can_run.then_some(index),
        }
    }
}

fn enabled(rows: &[SubRow]) -> Vec<bool> {
    rows.iter().map(|row| row.enabled).collect()
}

/// The tooltip of a row whose action also has a shortcut outside the menu.
fn shortcut_title(item: Item, keys: &Keymap) -> String {
    let command = match item {
        Item::Mark => Command::MarkToHandle,
        Item::Rename => Command::Rename,
        Item::Close => Command::CloseAgent,
        Item::Unread | Item::Notifications | Item::Folder => return String::new(),
    };
    format!("{} does this for the selected agent", keys.shown(command))
}

/// The row menu, at the pointer that opened it. It acts on that row's agent, selected or not.
/// The style sheet keeps it inside the window.
#[component]
pub fn RowMenu() -> Element {
    let cockpit: Cockpit = use_context();
    let open = {
        let state = cockpit.state.read();
        match state.ui.popover {
            Some(Popover::RowMenu { agent, x, y }) => {
                state.agents.get(&agent).map(|entry| RowMenuShown {
                    agent,
                    x,
                    y,
                    mark: entry.summary.mark,
                    unread: state.is_unread(agent),
                    notifications: notification_levels::agent_level(&state, agent),
                    folder: path_style::shown(&entry.summary.folder),
                    fork_blocked: working_folder::blocked(&state, agent, FolderAction::Fork),
                    move_blocked: working_folder::blocked(&state, agent, FolderAction::Move),
                })
            }
            _ => None,
        }
    };
    let Some(shown) = use_presence(open) else {
        return rsx! {};
    };
    // A menu opened on another row starts with nothing highlighted.
    let key = format!(
        "{}-{}-{}-{}",
        shown.opening,
        shown.value.agent.get(),
        shown.value.x,
        shown.value.y
    );
    rsx! {
        div {
            class: if shown.leaving { "presence leaving" } else { "presence" },
            inert: shown.leaving.then_some(true),
            for key in [key] {
                MenuBody { key: "{key}", shown: shown.value.clone() }
            }
        }
    }
}

#[component]
fn MenuBody(shown: RowMenuShown) -> Element {
    let cockpit: Cockpit = use_context();
    let mut cursor = use_signal(Cursor::default);
    // Counts the pointer's moves, so that a submenu change it delayed is dropped once it moves
    // again.
    let mut moves = use_hook(|| CopyValue::new(0_u32));
    let RowMenuShown {
        agent,
        x,
        y,
        mark,
        unread,
        notifications,
        ref folder,
        fork_blocked,
        move_blocked,
    } = shown;
    let folder = folder.clone();
    let act = move |pick: Pick| run(cockpit, agent, mark, unread, pick);
    let keys = cockpit.state.read().prefs.keys.clone();
    let mut cancel_delay = move || {
        let next = moves.peek().wrapping_add(1);
        moves.set(next);
        next
    };
    let mut point = move |index: usize| {
        let before = *cursor.peek();
        let pointed = before.pointed(index);
        if pointed == before {
            return;
        }
        let this_move = cancel_delay();
        let followed = pointed.followed();
        if before.open.is_none() || followed == pointed {
            cursor.set(followed);
            return;
        }
        // Another submenu is open: the pointer may be crossing this row on its way to it.
        cursor.set(pointed);
        spawn(async move {
            tokio::time::sleep(SUBMENU_DELAY).await;
            if *moves.peek() == this_move {
                cursor.with_mut(|cursor| *cursor = cursor.followed());
            }
        });
    };
    let shown_for_keys = shown.clone();
    let mut point_in_submenu = move |index: usize, can_run: bool| {
        cancel_delay();
        let pointed = cursor.peek().pointed_in_submenu(index, can_run);
        if pointed != *cursor.peek() {
            cursor.set(pointed);
        }
    };
    let now = cursor();
    let row_class = |index: usize, extra: &str| {
        if now.item == Some(index) {
            format!("menu-item{extra} {HIGHLIGHTED_CLASS}")
        } else {
            format!("menu-item{extra}")
        }
    };
    let sub_class = |submenu: Submenu, index: usize| {
        if now.open == Some(submenu) && now.sub == Some(index) {
            format!("menu-item {HIGHLIGHTED_CLASS}")
        } else {
            "menu-item".to_owned()
        }
    };
    let action_row = move |item: Item, label: &'static str, extra: &'static str| {
        let index = item.index();
        let hint = item.key().map(|key| key.to_ascii_uppercase());
        rsx! {
            button {
                class: row_class(index, extra),
                role: "menuitem",
                tabindex: -1,
                title: shortcut_title(item, &keys),
                onmouseenter: move |_| point(index),
                onclick: move |_| {
                    if let Some(pick) = item.pick() {
                        act(pick);
                    }
                },
                span { class: "menu-label", "{label}" }
                if let Some(hint) = hint {
                    span { class: "menu-hint", "{hint}" }
                }
            }
        }
    };
    let trigger_row = move |submenu: Submenu, label: &'static str, value: Option<&'static str>| {
        let index = submenu.trigger().index();
        let open = now.open == Some(submenu);
        rsx! {
            button {
                class: row_class(index, ""),
                role: "menuitem",
                tabindex: -1,
                aria_haspopup: "menu",
                aria_expanded: open,
                onmouseenter: move |_| point(index),
                onclick: move |_| {
                    cancel_delay();
                    cursor.with_mut(|cursor| *cursor = cursor.pointed(index).followed());
                },
                span { class: "menu-label", "{label}" }
                if let Some(value) = value {
                    span { class: "menu-value", "{value}" }
                }
                Icon { kind: IconKind::ChevronRight, size: 14 }
            }
        }
    };
    let level_row = move |index: usize, level: NotificationLevel| {
        let pick = Pick::Level(level);
        let chosen = level == notifications;
        let hint = pick.key().unwrap_or(' ');
        rsx! {
            button {
                key: "{index}",
                class: sub_class(Submenu::Notifications, index),
                role: "menuitemradio",
                tabindex: -1,
                aria_checked: chosen,
                onmouseenter: move |_| point_in_submenu(index, /* can_run */ true),
                onclick: move |_| act(pick),
                span { class: "menu-check",
                    if chosen {
                        Icon { kind: IconKind::Check, size: 14 }
                    }
                }
                span { class: "menu-label", "{levels_ui::choice_label(level)}" }
                span { class: "menu-hint", "{hint}" }
            }
        }
    };
    let folder_row = move |index: usize, action: FolderAction| {
        let pick = Pick::Folder(action);
        let (label, blocked, about) = match action {
            FolderAction::Fork => (
                "Fork into another folder…",
                fork_blocked,
                "A new agent carries on this conversation in another folder",
            ),
            FolderAction::Move => (
                "Move to another folder…",
                move_blocked,
                "This agent runs in another folder, on the same conversation",
            ),
        };
        let hint = pick
            .key()
            .map(|key| key.to_ascii_uppercase())
            .unwrap_or(' ');
        rsx! {
            button {
                class: sub_class(Submenu::Folder, index),
                role: "menuitem",
                tabindex: -1,
                disabled: blocked.is_some(),
                title: blocked.unwrap_or(about),
                onmouseenter: move |_| point_in_submenu(index, blocked.is_none()),
                onclick: move |_| act(pick),
                span { class: "menu-label", "{label}" }
                span { class: "menu-hint", "{hint}" }
            }
        }
    };
    rsx! {
        KeyedMenu {
            x,
            y,
            focus_back: None,
            on_key: move |key: MenuKey| {
                cancel_delay();
                let (moved, pick) = cursor.peek().after_key(&key, &shown_for_keys);
                if moved != *cursor.peek() {
                    cursor.set(moved);
                }
                if let Some(pick) = pick {
                    act(pick);
                }
            },
            {action_row(Item::Mark, if mark == Mark::ToHandle { "Done handling" } else { "Mark to handle" }, "")}
            {action_row(Item::Unread, if unread { "Mark as read" } else { "Mark as unread" }, "")}
            {action_row(Item::Rename, "Rename", "")}
            div { class: "menu-divider" }
            div { class: "submenu-anchor",
                {trigger_row(Submenu::Notifications, "Notifications", Some(levels_ui::short_label(notifications)))}
                Presence { open: now.open == Some(Submenu::Notifications),
                    div {
                        class: "popover menu submenu",
                        role: "menu",
                        aria_label: "Notifications",
                        for (index, level) in NotificationLevel::VARIANTS.iter().copied().enumerate() {
                            {level_row(index, level)}
                        }
                    }
                }
            }
            div { class: "submenu-anchor",
                {trigger_row(Submenu::Folder, "Working folder", None)}
                Presence { open: now.open == Some(Submenu::Folder),
                    // These act on the folder `claude` runs in, not on the sidebar.
                    div {
                        class: "popover menu submenu",
                        role: "menu",
                        aria_label: "Working folder",
                        span { class: "menu-folder mono", title: "{folder}",
                            span { class: "file-path-text", "{folder}" }
                        }
                        div { class: "menu-divider" }
                        {folder_row(0, FolderAction::Fork)}
                        {folder_row(1, FolderAction::Move)}
                    }
                }
            }
            div { class: "menu-divider" }
            {action_row(Item::Close, "Close", " danger")}
        }
    }
}

/// Runs what the menu picked, on the menu's agent. `mark` and `unread` are what the menu
/// showed: a toggle does what its label said, even if the agent changed since.
fn run(cockpit: Cockpit, agent: AgentId, mark: Mark, unread: bool, pick: Pick) {
    let close_menu = |state: &mut AppState| state.ui.popover = None;
    match pick {
        Pick::Mark => {
            cockpit.change(|state| to_handle::set(state, agent, to_handle::toggled(mark)))
        }
        Pick::Unread if unread => cockpit.update(|state| unread::mark_read(state, agent)),
        Pick::Unread => cockpit.update(|state| unread::mark_unread(state, agent)),
        Pick::Rename => cockpit.change_with_drafts(|state, drafts| {
            close_menu(state);
            actions::start_rename_in_sidebar(state, drafts, agent);
            Vec::new()
        }),
        Pick::Close => cockpit.change_with_drafts(|state, drafts| {
            close_menu(state);
            actions::request_close(state, drafts, agent)
        }),
        Pick::Level(level) => cockpit.set_notifications(Some(agent), level),
        Pick::Folder(action) => cockpit.update_with_drafts(|state, drafts| {
            working_folder::open(state, drafts, agent, action);
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown() -> RowMenuShown {
        RowMenuShown {
            agent: AgentId::new(1),
            x: 10,
            y: 20,
            mark: Mark::Unmarked,
            unread: false,
            notifications: NotificationLevel::All,
            folder: "/work/project".to_owned(),
            fork_blocked: None,
            move_blocked: None,
        }
    }

    fn press(cursor: Cursor, keys: &[MenuKey], shown: &RowMenuShown) -> (Cursor, Option<Pick>) {
        keys.iter().fold((cursor, None), |(cursor, _), key| {
            cursor.after_key(key, shown)
        })
    }

    #[test]
    fn the_arrows_walk_the_menu_and_stop_at_its_ends() {
        let shown = shown();

        let (first, _) = press(Cursor::default(), &[MenuKey::Down], &shown);
        let (top, _) = press(first, &[MenuKey::Up], &shown);
        let (last, _) = press(Cursor::default(), &[MenuKey::Up], &shown);
        let (bottom, _) = press(last, &[MenuKey::Down], &shown);

        assert_eq!(first.item, Some(0));
        assert_eq!(top.item, Some(0));
        assert_eq!(last.item, Some(ITEMS.len() - 1));
        assert_eq!(bottom.item, Some(ITEMS.len() - 1));
    }

    #[test]
    fn right_opens_a_submenu_on_its_first_row_and_left_closes_it() {
        let shown = shown();
        let on_notifications = Cursor {
            item: Some(Item::Notifications.index()),
            ..Cursor::default()
        };

        let (opened, _) = press(on_notifications, &[MenuKey::Right], &shown);
        let (moved, _) = press(opened, &[MenuKey::Down], &shown);
        let (closed, _) = press(moved, &[MenuKey::Left], &shown);
        let (on_mark, _) = press(
            on_notifications,
            &[MenuKey::Up, MenuKey::Up, MenuKey::Right],
            &shown,
        );

        assert_eq!(opened.open, Some(Submenu::Notifications));
        assert_eq!(opened.sub, Some(0));
        assert_eq!(moved.sub, Some(1));
        assert_eq!(moved.item, on_notifications.item);
        assert_eq!(closed, on_notifications);
        assert_eq!(on_mark.open, None);
    }

    #[test]
    fn enter_picks_the_highlighted_row_or_opens_its_submenu() {
        let shown = shown();
        let on = |item: Item| Cursor {
            item: Some(item.index()),
            ..Cursor::default()
        };

        let (_, close) = press(on(Item::Close), &[MenuKey::Enter], &shown);
        let (opened, nothing) = press(on(Item::Notifications), &[MenuKey::Enter], &shown);
        let (_, off) = press(
            opened,
            &[MenuKey::Down, MenuKey::Down, MenuKey::Enter],
            &shown,
        );

        assert_eq!(close, Some(Pick::Close));
        assert_eq!(nothing, None);
        assert_eq!(opened.open, Some(Submenu::Notifications));
        assert_eq!(off, Some(Pick::Level(NotificationLevel::Off)));
    }

    #[test]
    fn the_keys_skip_a_folder_action_that_cannot_run() {
        let shown = RowMenuShown {
            move_blocked: Some("The agent is working"),
            ..shown()
        };
        let on_folder = Cursor {
            item: Some(Item::Folder.index()),
            ..Cursor::default()
        };

        let (opened, _) = press(on_folder, &[MenuKey::Right, MenuKey::Down], &shown);
        let (_, by_letter) = press(opened, &[MenuKey::Char('m')], &shown);
        let (_, fork) = press(opened, &[MenuKey::Char('f')], &shown);

        assert_eq!(opened.sub, Some(0));
        assert_eq!(by_letter, None);
        assert_eq!(fork, Some(Pick::Folder(FolderAction::Fork)));
    }

    #[test]
    fn a_submenu_letter_works_only_while_its_submenu_is_open() {
        let shown = shown();
        let open = Cursor {
            item: Some(Item::Notifications.index()),
            open: Some(Submenu::Notifications),
            sub: None,
        };

        let (_, closed_two) = press(Cursor::default(), &[MenuKey::Char('2')], &shown);
        let (_, open_two) = press(open, &[MenuKey::Char('2')], &shown);
        let (_, close) = press(open, &[MenuKey::Char('w')], &shown);
        let (_, mark) = press(Cursor::default(), &[MenuKey::Char('h')], &shown);

        assert_eq!(closed_two, None);
        assert_eq!(open_two, Some(Pick::Level(NotificationLevel::NeedsYou)));
        assert_eq!(close, Some(Pick::Close));
        assert_eq!(mark, Some(Pick::Mark));
    }

    #[test]
    fn u_or_enter_on_its_row_marks_the_agent_unread() {
        let shown = shown();
        let on_unread = Cursor {
            item: Some(Item::Unread.index()),
            ..Cursor::default()
        };

        let (_, by_letter) = press(Cursor::default(), &[MenuKey::Char('u')], &shown);
        let (_, by_enter) = press(on_unread, &[MenuKey::Enter], &shown);

        assert_eq!(by_letter, Some(Pick::Unread));
        assert_eq!(by_enter, Some(Pick::Unread));
    }

    #[test]
    fn the_pointer_in_a_submenu_keeps_its_trigger_highlighted() {
        // The pointer crossed "Close" on its way to the open submenu.
        let crossing = Cursor {
            item: Some(Item::Close.index()),
            open: Some(Submenu::Folder),
            sub: None,
        };

        let on_move = crossing.pointed_in_submenu(1, true);
        let on_blocked_fork = crossing.pointed_in_submenu(0, false);

        assert_eq!(on_move.item, Some(Item::Folder.index()));
        assert_eq!(on_move.sub, Some(1));
        assert_eq!(on_blocked_fork.sub, None);
        assert_eq!(crossing.followed().open, None);
    }
}
