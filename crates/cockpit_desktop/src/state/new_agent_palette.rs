//! The "/" palette over the new agent dialog's first message. The agent has not started yet, so it
//! has no list of commands: the palette lists those of an agent of the same machine and account.
//! An agent in the dialog's folder has the very list the new agent will have; one in another folder
//! may lack this folder's own commands, or have some that do not work here.

use cockpit_protocol::{AgentId, SlashCommand};

use super::actions::{self, Direction, PaletteEntry};
use super::app_state::{AgentEntry, AppState};
use super::drafts::{Drafts, Palette};
use super::requests::Outgoing;
use super::ui_state::{Dialog, PaletteOrigin};
use super::utf16_offset::Utf16Offset;
use super::{accounts, folder_field};
use crate::machine::MachineId;

/// The agent whose commands the palette lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSource {
    pub agent: AgentId,
    pub place: SourcePlace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePlace {
    /// The dialog's folder: its list is the new agent's.
    SameFolder,
    /// Another folder: the new agent's list may differ.
    OtherFolder,
}

/// The open palette. Its source is chosen as it opens, so that the list stays the same under the
/// highlight: a change of folder, account or machine takes the focus out of the first message,
/// which closes the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPalette {
    pub filter: Palette,
    pub source: Option<CommandSource>,
}

/// The agent of the dialog's machine and account whose commands the palette lists: one in the
/// dialog's `folder` first, then one whose commands the app has, then the latest one active.
pub fn source(state: &AppState, folder: &str) -> Option<CommandSource> {
    let Some(Dialog::NewAgent(form)) = &state.ui.dialog else {
        return None;
    };
    let account = accounts::new_agent_account(state, form);
    let style = folder_field::style(state);
    let folder = folder.trim();
    let folder = style.is_absolute(folder).then(|| style.resolve(folder, ""));
    let in_folder = |entry: &AgentEntry| {
        folder.as_deref().is_some_and(|folder| {
            let agent_folder = style.resolve(&entry.summary.folder.to_string_lossy(), "");
            style.same_path(&agent_folder, folder)
        })
    };
    let best = state
        .agents
        .values()
        .filter(|entry| {
            MachineId::of(entry.id()) == form.machine && entry.summary.account == account
        })
        .max_by_key(|entry| {
            (
                in_folder(entry),
                !entry.commands.is_empty(),
                entry.summary.last_activity_at,
            )
        })?;
    let place = if in_folder(best) {
        SourcePlace::SameFolder
    } else {
        SourcePlace::OtherFolder
    };
    Some(CommandSource {
        agent: best.id(),
        place,
    })
}

/// The first message changed, with the caret at `caret`. The "/" word the caret is in opens the
/// palette and filters it, unless Esc closed it on that word; anywhere else, the palette closes.
/// Only the drafts change: returns the source agent as the palette opens, for `ask_commands`.
///
/// The field's own input event writes the draft before the page script's message arrives: a
/// message whose text is no longer the draft's is stale, and changes nothing.
pub fn typed(
    state: &AppState,
    drafts: &mut Drafts,
    text: &str,
    caret: Utf16Offset,
) -> Option<AgentId> {
    if !matches!(state.ui.dialog, Some(Dialog::NewAgent(_))) || drafts.new_agent.prompt != text {
        return None;
    }
    let caret = caret.byte_offset(text);
    let word = actions::slash_word_at(text, caret);
    let new_agent = &mut drafts.new_agent;
    if new_agent.palette_dismissed != word.map(|word| word.start) {
        new_agent.palette_dismissed = None;
    }
    let Some(word) = word.filter(|word| new_agent.palette_dismissed != Some(word.start)) else {
        new_agent.palette = None;
        return None;
    };
    let query = text.get(word.start + 1..caret).unwrap_or_default();
    let filter = Palette {
        query: query.to_owned(),
        word: Some(word),
        ..Palette::default()
    };
    match &mut new_agent.palette {
        // A message the palette already follows keeps the highlight.
        Some(open) if open.filter.word == filter.word && open.filter.query == filter.query => None,
        Some(open) => {
            open.filter = filter;
            None
        }
        None => {
            let source = source(state, &new_agent.folder);
            new_agent.palette = Some(OpenPalette { filter, source });
            source.map(|source| source.agent)
        }
    }
}

/// Asks for the agent's commands when the app has none and has not asked yet.
pub fn ask_commands(state: &mut AppState, agent: AgentId) -> Vec<Outgoing> {
    actions::commands_request(state, agent)
        .into_iter()
        .collect()
}

/// The commands the open palette lists, filtered by its query.
pub fn entries<'state>(state: &'state AppState, drafts: &Drafts) -> Vec<&'state SlashCommand> {
    let Some(open) = &drafts.new_agent.palette else {
        return Vec::new();
    };
    let commands = open
        .source
        .and_then(|source| state.agents.get(&source.agent))
        .map_or(&[][..], |entry| entry.commands.as_slice());
    let filter = &open.filter;
    let how = actions::command_match(filter);
    let entries = actions::palette_entries(commands, &filter.query, how, PaletteOrigin::Typed);
    entries
        .into_iter()
        .filter_map(|entry| match entry {
            PaletteEntry::Command(command) => Some(command),
            // The agent has not started: there is nothing to reload.
            PaletteEntry::Action(_) | PaletteEntry::Reload(_) => None,
        })
        .collect()
}

/// The source's commands are on their way.
pub fn loading(state: &AppState, drafts: &Drafts) -> bool {
    drafts
        .new_agent
        .palette
        .as_ref()
        .and_then(|open| open.source)
        .and_then(|source| state.agents.get(&source.agent))
        .is_some_and(AgentEntry::commands_loading)
}

/// The page script's message may come after a key: the palette takes its "/" word from the
/// draft, which the field's input event keeps current. A word typed further filters anew, and the
/// highlight goes back to the first entry; a word that is gone closes the palette.
fn follow_draft(drafts: &mut Drafts) {
    let new_agent = &mut drafts.new_agent;
    let Some(start) = new_agent
        .palette
        .as_ref()
        .and_then(|open| open.filter.word)
        .map(|word| word.start)
    else {
        return;
    };
    let text = &new_agent.prompt;
    let word = actions::slash_word_at(text, start + 1).filter(|word| word.start == start);
    let Some(word) = word else {
        new_agent.palette = None;
        return;
    };
    if let Some(open) = &mut new_agent.palette
        && open.filter.word != Some(word)
    {
        open.filter = Palette {
            query: text.get(start + 1..word.end).unwrap_or_default().to_owned(),
            word: Some(word),
            ..Palette::default()
        };
    }
}

pub fn move_highlight(state: &AppState, drafts: &mut Drafts, direction: Direction) {
    follow_draft(drafts);
    let count = entries(state, drafts).len();
    if let Some(open) = &mut drafts.new_agent.palette {
        open.filter.highlighted = actions::moved(open.filter.highlighted, count, direction);
        open.filter.moved = true;
    }
}

/// The pointer moved onto the entry at `index`: it takes the highlight.
pub fn point(state: &AppState, drafts: &mut Drafts, index: usize) {
    let count = entries(state, drafts).len();
    if let Some(open) = &mut drafts.new_agent.palette
        && index < count
    {
        open.filter.highlighted = index;
    }
}

/// The text of the first message once a command took the place of its "/" word, with the caret
/// after the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub text: String,
    pub caret: Utf16Offset,
}

/// Puts the command at `index`, or the highlighted one, in place of the "/" word, and closes the
/// palette. The field follows the keyboard alone, so the caller writes the text into it. `None`
/// when no command is there.
pub fn pick(state: &AppState, drafts: &mut Drafts, index: Option<usize>) -> Option<Written> {
    follow_draft(drafts);
    let filter = &drafts.new_agent.palette.as_ref()?.filter;
    let index = index.unwrap_or(filter.highlighted);
    let word = filter.word?;
    let name = entries(state, drafts).get(index)?.name.clone();
    let (text, caret) = actions::with_command_at(&drafts.new_agent.prompt, word, &name)?;
    drafts.new_agent.prompt.clone_from(&text);
    drafts.new_agent.palette = None;
    Some(Written { text, caret })
}

/// Esc closes the palette for the rest of its "/" word. False when it was closed.
pub fn dismiss(drafts: &mut Drafts) -> bool {
    let Some(open) = drafts.new_agent.palette.take() else {
        return false;
    };
    drafts.new_agent.palette_dismissed = open.filter.word.map(|word| word.start);
    true
}

/// The first message lost the focus: the palette closes, and opens again as you type in its word.
pub fn close(drafts: &mut Drafts) {
    drafts.new_agent.palette = None;
}

/// A key the palette takes while it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteKey {
    Up,
    Down,
    Enter,
    Tab,
    Escape,
}

/// What a key did in the open palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyResult {
    /// The palette took the key, and nothing else happens.
    Taken,
    /// A command took the place of the "/" word: the caller writes it into the field.
    Written(Written),
    /// Enter with no command to pick: the agent starts with the message as typed.
    Start,
}

/// A key pressed in the first message while the palette is open. With no command to pick, Enter
/// starts the agent, unless the commands are still on their way.
pub fn press(state: &AppState, drafts: &mut Drafts, key: PaletteKey) -> KeyResult {
    match key {
        PaletteKey::Up => move_highlight(state, drafts, Direction::Up),
        PaletteKey::Down => move_highlight(state, drafts, Direction::Down),
        PaletteKey::Escape => {
            dismiss(drafts);
        }
        PaletteKey::Enter | PaletteKey::Tab => {
            if let Some(written) = pick(state, drafts, None) {
                return KeyResult::Written(written);
            }
            if key == PaletteKey::Enter && !loading(state, drafts) {
                close(drafts);
                return KeyResult::Start;
            }
        }
    }
    KeyResult::Taken
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::{AccountId, AgentDetails, AgentStatus, CommandKind, Request, Timestamp};

    use super::*;
    use crate::state::app_state::Loadable;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::ui_state::NewAgentForm;

    fn command(name: &str) -> SlashCommand {
        SlashCommand {
            name: name.to_owned(),
            description: String::new(),
            kind: CommandKind::Skill,
        }
    }

    fn agent(id: u32) -> AgentId {
        AgentId::new(id)
    }

    /// Agent 1 in `/work/shop`, agent 2 in `/work/blog`, active later, and agent 3 of another
    /// account in `/work/shop`, active latest of all. The new agent dialog is open on this
    /// machine, with the default account.
    fn state() -> AppState {
        let mut shop = summary(1, AgentStatus::Idle);
        shop.folder = PathBuf::from("/work/shop");
        shop.last_activity_at = Some(Timestamp::from_unix_millis(1_000));
        let mut blog = summary(2, AgentStatus::Idle);
        blog.folder = PathBuf::from("/work/blog");
        blog.last_activity_at = Some(Timestamp::from_unix_millis(2_000));
        let mut other_account = summary(3, AgentStatus::Idle);
        other_account.folder = PathBuf::from("/work/shop");
        other_account.account = AccountId::new(7);
        other_account.last_activity_at = Some(Timestamp::from_unix_millis(3_000));
        let mut state = state_with(vec![shop, blog, other_account]);
        state.ui.dialog = Some(Dialog::NewAgent(NewAgentForm {
            account: Some(AccountId::DEFAULT),
            ..NewAgentForm::default()
        }));
        for (id, name) in [(1, "review"), (2, "release-notes"), (3, "refactor")] {
            set_commands(&mut state, id, &[name]);
        }
        state
    }

    fn set_commands(state: &mut AppState, id: u32, names: &[&str]) {
        if let Some(entry) = state.agents.get_mut(&agent(id)) {
            entry.details = Loadable::Loaded(AgentDetails::default());
            entry.commands = names.iter().map(|name| command(name)).collect();
        }
    }

    /// Types `text` as the field does: its input event writes the draft, then the page script's
    /// message comes with the caret at the end.
    fn type_text(state: &AppState, drafts: &mut Drafts, text: &str) -> Option<AgentId> {
        drafts.new_agent.prompt = text.to_owned();
        typed(state, drafts, text, Utf16Offset::end_of(text))
    }

    fn listed(state: &AppState, drafts: &Drafts) -> Vec<String> {
        entries(state, drafts)
            .iter()
            .map(|command| command.name.clone())
            .collect()
    }

    fn highlighted(drafts: &Drafts) -> Option<usize> {
        drafts
            .new_agent
            .palette
            .as_ref()
            .map(|open| open.filter.highlighted)
    }

    #[test]
    fn the_source_is_an_agent_of_the_account_in_the_folder_else_the_latest() {
        let state = state();

        let in_folder = source(&state, "/work/shop/");
        let elsewhere = source(&state, "/work/new");
        let untyped = source(&state, "");

        assert_eq!(
            in_folder,
            Some(CommandSource {
                agent: agent(1),
                place: SourcePlace::SameFolder
            })
        );
        assert_eq!(
            elsewhere,
            Some(CommandSource {
                agent: agent(2),
                place: SourcePlace::OtherFolder
            }),
            "agent 3 is the latest, but of another account"
        );
        assert_eq!(untyped.map(|source| source.agent), Some(agent(2)));
    }

    #[test]
    fn an_agent_with_commands_comes_before_a_later_one_without() {
        let mut state = state();
        if let Some(entry) = state.agents.get_mut(&agent(2)) {
            entry.commands.clear();
        }

        let chosen = source(&state, "/work/new").map(|source| source.agent);

        assert_eq!(chosen, Some(agent(1)));
    }

    #[test]
    fn an_agent_of_another_machine_is_no_source() {
        let mut state = state();
        for entry in state.agents.values_mut() {
            entry.summary.account = AccountId::new(7);
        }
        let server = MachineId::new(4);
        let mut remote = summary(1, AgentStatus::Idle);
        remote.id = server.app_agent(agent(1));
        remote.folder = PathBuf::from("/work/shop");
        let remote_id = remote.id;
        state.agents.insert(remote_id, AgentEntry::new(remote));

        assert_eq!(source(&state, "/work/shop"), None);
    }

    /// A Windows daemon's folders compare whatever their case, as Windows does.
    #[cfg(windows)]
    #[test]
    fn a_folder_typed_in_another_case_is_the_same_folder_on_windows() {
        use crate::daemon_label::DaemonPlace;

        let mut state = state();
        state.machines.this.daemon.place = DaemonPlace::InApp;
        if let Some(entry) = state.agents.get_mut(&agent(1)) {
            entry.summary.folder = PathBuf::from(r"C:\work\shop");
        }

        let chosen = source(&state, r"c:\WORK\shop");

        assert_eq!(
            chosen.map(|source| source.place),
            Some(SourcePlace::SameFolder)
        );
    }

    #[test]
    fn a_slash_word_opens_the_palette_on_the_source_commands_and_a_pick_replaces_it() {
        let state = state();
        let mut drafts = Drafts::default();
        drafts.new_agent.folder = "/work/shop".to_owned();

        type_text(&state, &mut drafts, "please /rev");
        let shown = listed(&state, &drafts);
        let written = pick(&state, &mut drafts, None);

        assert_eq!(shown, ["review"]);
        assert_eq!(
            written,
            Some(Written {
                text: "please /review ".to_owned(),
                caret: Utf16Offset::new(15),
            })
        );
        assert_eq!(drafts.new_agent.prompt, "please /review ");
        assert_eq!(drafts.new_agent.palette, None);
    }

    /// The page script's message for the last letters comes after Enter: the pick follows the
    /// field's text, and the late message neither changes the text nor opens the palette again.
    #[test]
    fn a_pick_follows_the_text_typed_after_the_last_message() {
        let mut state = state();
        set_commands(&mut state, 1, &["rebase", "review"]);
        let mut drafts = Drafts::default();
        drafts.new_agent.folder = "/work/shop".to_owned();
        type_text(&state, &mut drafts, "/re");

        drafts.new_agent.prompt = "/rev".to_owned();
        let written = pick(&state, &mut drafts, None);
        let late = typed(&state, &mut drafts, "/rev", Utf16Offset::new(4));

        assert_eq!(
            written.map(|written| written.text),
            Some("/review ".to_owned())
        );
        assert_eq!(late, None);
        assert_eq!(drafts.new_agent.palette, None, "the late message is stale");
    }

    #[test]
    fn a_message_the_palette_already_follows_keeps_the_highlight() {
        let mut state = state();
        set_commands(&mut state, 1, &["rebase", "review"]);
        let mut drafts = Drafts::default();
        drafts.new_agent.folder = "/work/shop".to_owned();
        type_text(&state, &mut drafts, "/re");
        move_highlight(&state, &mut drafts, Direction::Down);

        typed(&state, &mut drafts, "/re", Utf16Offset::new(3));
        let kept = highlighted(&drafts);
        type_text(&state, &mut drafts, "/reb");
        let filtered_again = highlighted(&drafts);

        assert_eq!(kept, Some(1));
        assert_eq!(filtered_again, Some(0));
    }

    /// Agent 2 does some work while the palette is open: the list stays agent 1's.
    #[test]
    fn the_source_stays_while_the_palette_is_open() {
        let mut state = state();
        let mut drafts = Drafts::default();
        drafts.new_agent.folder = "/work/new".to_owned();
        if let Some(entry) = state.agents.get_mut(&agent(1)) {
            entry.summary.last_activity_at = Some(Timestamp::from_unix_millis(5_000));
        }
        type_text(&state, &mut drafts, "/re");
        let before = listed(&state, &drafts);

        if let Some(entry) = state.agents.get_mut(&agent(2)) {
            entry.summary.last_activity_at = Some(Timestamp::from_unix_millis(9_000));
        }

        assert_eq!(before, ["review"]);
        assert_eq!(listed(&state, &drafts), before);
    }

    #[test]
    fn the_caret_out_of_the_word_closes_the_palette_and_esc_keeps_it_closed_for_the_word() {
        let state = state();
        let mut drafts = Drafts::default();

        type_text(&state, &mut drafts, "/re");
        let dismissed = dismiss(&mut drafts);
        type_text(&state, &mut drafts, "/rev");
        let after_escape = drafts.new_agent.palette.is_some();
        type_text(&state, &mut drafts, "/rev and ");
        type_text(&state, &mut drafts, "/rev and /re");
        let next_word = drafts.new_agent.palette.is_some();
        type_text(&state, &mut drafts, "/rev and /re then");

        assert!(dismissed);
        assert!(!after_escape, "Esc keeps it closed for its word");
        assert!(next_word, "another word opens it");
        assert_eq!(drafts.new_agent.palette, None, "the caret left the word");
    }

    #[test]
    fn opening_asks_once_for_commands_the_app_does_not_have() {
        let mut state = state();
        let mut drafts = Drafts::default();
        drafts.new_agent.folder = "/work/shop".to_owned();
        if let Some(entry) = state.agents.get_mut(&agent(1)) {
            entry.commands.clear();
        }

        let opened = type_text(&state, &mut drafts, "/r");
        let asked = opened.map(|source| ask_commands(&mut state, source));
        let filtered = type_text(&state, &mut drafts, "/re");
        let asked_again = ask_commands(&mut state, agent(1));

        assert!(matches!(
            asked.as_deref(),
            Some([Outgoing {
                request: Request::AgentCommands(params),
                ..
            }]) if params.agent == agent(1)
        ));
        assert_eq!(filtered, None, "only the opening names the source");
        assert!(asked_again.is_empty(), "once");
        assert!(loading(&state, &drafts));
    }

    #[test]
    fn the_keys_move_the_highlight_within_the_entries() {
        let mut state = state();
        set_commands(&mut state, 1, &["review", "rebase"]);
        let mut drafts = Drafts::default();
        drafts.new_agent.folder = "/work/shop".to_owned();
        type_text(&state, &mut drafts, "/re");

        let pressed = press(&state, &mut drafts, PaletteKey::Down);
        let moved = highlighted(&drafts);
        point(&state, &mut drafts, 5);
        let pointed_out = highlighted(&drafts);

        assert_eq!(pressed, KeyResult::Taken);
        assert_eq!(moved, Some(1));
        assert_eq!(
            pointed_out,
            Some(1),
            "an entry past the list changes nothing"
        );
    }

    #[test]
    fn enter_and_tab_insert_the_command_and_escape_closes_the_palette() {
        let state = state();
        let mut drafts = Drafts::default();
        drafts.new_agent.folder = "/work/shop".to_owned();

        type_text(&state, &mut drafts, "/re");
        let entered = press(&state, &mut drafts, PaletteKey::Enter);
        type_text(&state, &mut drafts, "/review and /re");
        let tabbed = press(&state, &mut drafts, PaletteKey::Tab);
        type_text(&state, &mut drafts, "/review and /review /re");
        let escaped = press(&state, &mut drafts, PaletteKey::Escape);

        assert!(matches!(entered, KeyResult::Written(Written { text, .. }) if text == "/review "));
        assert!(
            matches!(tabbed, KeyResult::Written(Written { text, .. }) if text == "/review and /review ")
        );
        assert_eq!(escaped, KeyResult::Taken);
        assert_eq!(drafts.new_agent.palette, None);
    }

    /// With no command to pick, Enter starts the agent as typed, but not while the commands are
    /// on their way, and Tab never does.
    #[test]
    fn enter_with_no_command_starts_the_agent_unless_commands_load() {
        let mut state = state();
        let mut drafts = Drafts::default();
        drafts.new_agent.folder = "/work/shop".to_owned();

        type_text(&state, &mut drafts, "/zz");
        let tab = press(&state, &mut drafts, PaletteKey::Tab);
        let enter = press(&state, &mut drafts, PaletteKey::Enter);
        if let Some(entry) = state.agents.get_mut(&agent(1)) {
            entry.commands.clear();
            entry.commands_requested = true;
        }
        type_text(&state, &mut drafts, "/zz ");
        type_text(&state, &mut drafts, "/zz /re");
        let while_loading = press(&state, &mut drafts, PaletteKey::Enter);

        assert_eq!(tab, KeyResult::Taken);
        assert_eq!(enter, KeyResult::Start);
        assert_eq!(while_loading, KeyResult::Taken);
    }

    #[test]
    fn a_blur_closes_the_palette_and_typing_in_the_word_opens_it_again() {
        let state = state();
        let mut drafts = Drafts::default();

        type_text(&state, &mut drafts, "/re");
        close(&mut drafts);
        let closed = drafts.new_agent.palette.is_none();
        type_text(&state, &mut drafts, "/rev");

        assert!(closed);
        assert!(drafts.new_agent.palette.is_some(), "a blur is no Esc");
    }
}
