//! The keyboard shortcuts of R4-parts, resolved against the state. The page's script decides
//! which key presses reach here (`bridge`), so typing in a field is never taken over.

use cockpit_protocol::{AgentId, ApprovalDecision, PlanDecision, PromptDelivery};
use serde::Deserialize;

use super::actions::{self, Direction, Step};
use super::app_state::{AgentEntry, AppState};
use super::drafts::Drafts;
use super::find::{self, FindPane};
use super::requests::Outgoing;
use super::transcript::WaitingKind;
use super::ui_state::{Dialog, PaletteOrigin, Popover};
use super::utf16_offset::Utf16Offset;
use super::{accounts, agent_map, file_reread, file_tree, open_file, quotes, terminals, to_handle};
use crate::keymap::{Chord, Command};
use crate::prefs::EscStop;

/// `KeyboardEvent.key` values.
const KEY_ESCAPE: &str = "Escape";
pub(super) const KEY_ENTER: &str = "Enter";
const KEY_TAB: &str = "Tab";
pub(super) const KEY_UP: &str = "ArrowUp";
pub(super) const KEY_DOWN: &str = "ArrowDown";
pub(super) const KEY_LEFT: &str = "ArrowLeft";
pub(super) const KEY_RIGHT: &str = "ArrowRight";
/// Reloads the page in the webview: the page script takes it everywhere, whatever runs it.
pub const PAGE_RELOAD_KEY: &str = "F5";

/// The letters that answer an approval or a plan.
pub(super) const KEY_YES: &str = "y";
pub(super) const KEY_NO: &str = "n";
pub const ANSWER_KEYS: [&str; 2] = [KEY_YES, KEY_NO];

/// A key press the page's script passes on.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct KeyPress {
    pub key: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
    pub target: KeyTarget,
    /// The composer's text when the key went to it, which may hold a keystroke the draft has not
    /// received yet.
    #[serde(default)]
    pub composer_text: String,
    /// The pane the focus is in, whose find Ctrl+F opens and Esc closes.
    #[serde(default)]
    pub pane: FindPane,
    /// The page's selection, with a command's shortcut: Ctrl+F looks for it.
    #[serde(default)]
    pub selection: String,
    /// The key went to a find's field: Esc there closes the find.
    #[serde(default)]
    pub in_find: bool,
}

/// Esc with nothing left open: the selected agent's turn stops, at once or after the dialog that
/// Settings asks for. An agent with no turn to stop is left alone.
fn stop_on_escape(state: &mut AppState) -> Vec<Outgoing> {
    let Some(agent) = state.ui.selected else {
        return Vec::new();
    };
    if !actions::has_turn(state, agent) {
        return Vec::new();
    }
    match state.prefs.chat.esc_stop {
        EscStop::Ask => {
            state.ui.dialog = Some(Dialog::StopAgent(agent));
            Vec::new()
        }
        EscStop::AtOnce => actions::interrupt(state, agent),
    }
}

/// Runs the command a shortcut is bound to. The page script passes a shortcut on only where its
/// command reaches (`Command::reach`).
fn run(
    state: &mut AppState,
    drafts: &mut Drafts,
    command: Command,
    press: &KeyPress,
) -> Vec<Outgoing> {
    let target = press.target;
    let no_dialog = state.ui.dialog.is_none();
    match command {
        Command::Switcher => actions::toggle_switcher(state, drafts),
        Command::NewAgent => actions::open_new_agent(state, drafts),
        Command::OpenFile => open_file::toggle(state, drafts),
        Command::FilesPane => file_tree::toggle_pane(state),
        Command::Terminal => {
            terminals::toggle_pane(state);
        }
        Command::AgentMap => agent_map::toggle(state),
        Command::Find if no_dialog => find::open(state, drafts, press.pane, &press.selection),
        // Asks to close the selected agent, and confirms when asked.
        Command::CloseAgent => {
            return match (&state.ui.dialog, state.ui.selected) {
                (Some(Dialog::CloseAgent(agent)), _) => actions::close_agent(state, drafts, *agent),
                (None, Some(agent)) => actions::request_close(state, drafts, agent),
                (Some(_), _) | (None, None) => Vec::new(),
            };
        }
        Command::MarkToHandle if no_dialog => return to_handle::cycle_selected(state),
        Command::Rename if no_dialog => actions::rename_key(state, drafts),
        Command::ReloadFile => {
            if let Some(agent) = state.ui.selected {
                file_reread::reread(state, agent);
                if let Some(entry) = state.agents.get_mut(&agent) {
                    file_tree::refresh(&mut entry.tree);
                }
            }
        }
        // From anywhere but a text field other than the composer.
        Command::MoveUp if no_dialog && target != KeyTarget::Field => {
            return actions::move_selected(state, Direction::Up);
        }
        Command::MoveDown if no_dialog && target != KeyTarget::Field => {
            return actions::move_selected(state, Direction::Down);
        }
        Command::Find
        | Command::MarkToHandle
        | Command::Rename
        | Command::MoveUp
        | Command::MoveDown => {}
    }
    Vec::new()
}

/// Where the key went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyTarget {
    Composer,
    /// Another text field: only the shortcuts with Ctrl, and Esc.
    Field,
    /// No text field has the focus.
    Page,
}

pub fn handle_key(state: &mut AppState, drafts: &mut Drafts, press: &KeyPress) -> Vec<Outgoing> {
    let command = press.ctrl || press.meta;
    let bound = Chord::of(&press.key, command, press.alt, press.shift)
        .and_then(|chord| state.prefs.keys.command(&chord));
    if let Some(bound) = bound {
        return run(state, drafts, bound, press);
    }
    // Esc closes what is open first. Only then does it stop a busy agent, and by default after a
    // dialog, so that an Esc meant to close something never cuts a turn short.
    if press.key == KEY_ESCAPE {
        // The comment field closes first, and alone: a dialog behind it stays.
        if quotes::cancel(drafts, state.ui.selected) {
            return Vec::new();
        }
        // A palette opened by typing stays closed for the rest of its "/" word.
        if state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed))
            && let (Some(agent), Some(word)) = (state.ui.selected, drafts.palette.word)
        {
            drafts.palette_dismissed.insert(agent, word.start);
        }
        if state.ui.close_overlays() {
            return Vec::new();
        }
        // In a text field, such as a search, Esc never stops an agent. A find's own field closes
        // its find.
        if press.target == KeyTarget::Field {
            if press.in_find {
                find::close_on_escape(state, press.pane);
            }
            return Vec::new();
        }
        if find::close_on_escape(state, press.pane) {
            return Vec::new();
        }
        return stop_on_escape(state);
    }
    // The map takes its keys from anywhere, the composer behind it included.
    if matches!(state.ui.dialog, Some(Dialog::AgentMap(_))) {
        if command || press.alt {
            return Vec::new();
        }
        return agent_map::press(state, &press.key);
    }
    // Alt+Enter in the composer queues the message until the turn ends.
    let queues = press.alt && !command && !press.shift && press.key == KEY_ENTER;
    if queues
        && press.target == KeyTarget::Composer
        && state.ui.dialog.is_none()
        && let Some(agent) = state.ui.selected
    {
        let typed = Some(press.composer_text.clone());
        return actions::send_prompt(state, drafts, agent, typed, PromptDelivery::Queue);
    }
    if command || press.alt {
        return Vec::new();
    }
    let Some(agent) = state.ui.selected else {
        return Vec::new();
    };
    match press.target {
        KeyTarget::Composer => composer_key(state, drafts, agent, press),
        KeyTarget::Page if state.ui.dialog.is_none() => page_key(state, drafts, agent, &press.key),
        KeyTarget::Page | KeyTarget::Field => Vec::new(),
    }
}

/// A key outside text fields: it answers the waiting card, or with nothing waiting, Enter
/// presses the sign-in card's "Send the message again".
fn page_key(state: &mut AppState, drafts: &mut Drafts, agent: AgentId, key: &str) -> Vec<Outgoing> {
    let waits = state
        .agents
        .get(&agent)
        .is_some_and(|entry| entry.transcript.waiting().is_some());
    if !waits && key == KEY_ENTER {
        return accounts::send_again(state, agent);
    }
    card_key(state, drafts, agent, key).unwrap_or_default()
}

fn composer_key(
    state: &mut AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    press: &KeyPress,
) -> Vec<Outgoing> {
    if state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed)) {
        match press.key.as_str() {
            // Shift+Enter adds a line, which the page does itself.
            KEY_ENTER if press.shift => return Vec::new(),
            KEY_UP => actions::palette_move(state, drafts, agent, Direction::Up),
            KEY_DOWN => actions::palette_move(state, drafts, agent, Direction::Down),
            // Opened by typing, the palette lists commands alone. A pick puts one in the
            // composer, which already has the focus the pick asks the window for.
            KEY_ENTER | KEY_TAB => {
                sync_composer_text(state, drafts, agent, &press.composer_text);
                let typed_open = state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed));
                if typed_open && actions::palette_has_entries(state, drafts, agent) {
                    return actions::palette_pick(state, drafts, agent, None).requests;
                }
                let loading = state
                    .agents
                    .get(&agent)
                    .is_some_and(AgentEntry::commands_loading);
                if typed_open && loading {
                    return Vec::new();
                }
                // With no command to pick, Enter sends the message as typed.
                if typed_open && press.key == KEY_ENTER {
                    state.ui.popover = None;
                }
            }
            _ => {}
        }
        // The text the key saw closed the palette: only Enter goes on, to send it.
        if press.key != KEY_ENTER {
            return Vec::new();
        }
    }
    if press.composer_text.is_empty() {
        match card_key(state, drafts, agent, &press.key) {
            Some(answered) => return answered,
            None if press.key != KEY_ENTER => return Vec::new(),
            None => {}
        }
    }
    if press.key == KEY_ENTER && !press.shift {
        let typed = Some(press.composer_text.clone());
        return actions::send_prompt(state, drafts, agent, typed, PromptDelivery::Steer);
    }
    Vec::new()
}

/// The draft can lag a keystroke behind the composer: a key that acts on the typed text first
/// gives the draft the text the key saw. The same text leaves the palette's highlight in place.
fn sync_composer_text(state: &mut AppState, drafts: &mut Drafts, agent: AgentId, text: &str) {
    let draft = drafts.draft(agent).map(|draft| draft.text.as_str());
    if draft == Some(text) {
        return;
    }
    // A key press carries no caret: the end of the text stands in for it.
    let caret = Utf16Offset::end_of(text);
    if let Some(change) = actions::composer_input(state, drafts, agent, text.to_owned(), caret) {
        actions::change_palette(state, agent, change);
    }
}

/// Y and N answer an approval or a plan. 1 to 9 pick an option of the question on screen, Left
/// and Right show another question, and Enter sends the answers or shows the next question.
/// `None` when the key does nothing to the waiting card.
fn card_key(
    state: &AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    key: &str,
) -> Option<Vec<Outgoing>> {
    let waiting = state
        .agents
        .get(&agent)
        .and_then(|entry| entry.transcript.waiting())?;
    let letter = key.to_lowercase();
    match (waiting.kind, letter.as_str()) {
        (WaitingKind::Approval, KEY_YES) => Some(actions::answer_approval(
            agent,
            waiting.pending,
            ApprovalDecision::Allow,
        )),
        (WaitingKind::Approval, KEY_NO) => Some(actions::answer_approval(
            agent,
            waiting.pending,
            ApprovalDecision::Deny,
        )),
        (WaitingKind::Plan, KEY_YES) => Some(actions::answer_plan(
            agent,
            waiting.pending,
            PlanDecision::AcceptEdits,
        )),
        (WaitingKind::Plan, KEY_NO) => Some(actions::answer_plan(
            agent,
            waiting.pending,
            PlanDecision::KeepPlanning,
        )),
        (WaitingKind::Question, _) if key == KEY_ENTER => {
            actions::enter_on_question(state, drafts, agent, waiting.item)
        }
        (WaitingKind::Question, _) if key == KEY_LEFT || key == KEY_RIGHT => {
            let step = if key == KEY_RIGHT {
                Step::Next
            } else {
                Step::Previous
            };
            actions::step_question(state, drafts, agent, waiting.item, step);
            Some(Vec::new())
        }
        (WaitingKind::Question, digit) => {
            let number = digit.parse::<usize>().ok()?;
            actions::pick_by_number(state, drafts, agent, waiting.item, number);
            Some(Vec::new())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{
        AgentDetails, AgentStatus, Answer, AnswerState, Approval, AskedQuestion, CommandKind,
        Image, ImageMediaType, ItemBody, ItemId, Method, PendingId, Question, QuestionOption,
        Request, Selection, SignInWait, SlashCommand, TranscriptItem,
    };
    use serde_json::Value;

    use super::*;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::app_state::{ConnectionState, Loadable};
    use crate::state::ui_state::Dialog;

    fn press(key: &str, target: KeyTarget) -> KeyPress {
        KeyPress {
            key: key.to_owned(),
            ctrl: false,
            shift: false,
            alt: false,
            meta: false,
            target,
            composer_text: String::new(),
            pane: FindPane::Chat,
            selection: String::new(),
            in_find: false,
        }
    }

    fn ctrl(key: &str) -> KeyPress {
        KeyPress {
            ctrl: true,
            ..press(key, KeyTarget::Field)
        }
    }

    fn state(status: AgentStatus, body: Option<ItemBody>) -> AppState {
        let mut state = state_with(vec![summary(1, status)]);
        state.machines.this.connection = ConnectionState::Connected;
        state.ui.selected = Some(AgentId::new(1));
        if let (Some(body), Some(entry)) = (body, state.agents.get_mut(&AgentId::new(1))) {
            entry.transcript.append(TranscriptItem {
                id: ItemId::new(3),
                parent: None,
                body,
            });
        }
        state
    }

    fn approval() -> ItemBody {
        ItemBody::Approval(Approval {
            pending: PendingId::new("request-3".to_owned()),
            tool: "Bash".to_owned(),
            summary: "cargo test".to_owned(),
            input: Value::Null,
            edit: None,
            reason: None,
            answer: AnswerState::Waiting,
            tool_call: None,
        })
    }

    fn question() -> ItemBody {
        questions(1)
    }

    /// A card of `count` one-choice questions, each with the options "Unit" and "All".
    fn questions(count: usize) -> ItemBody {
        let asked = AskedQuestion {
            header: "Scope".to_owned(),
            text: "Which tests?".to_owned(),
            options: vec![
                QuestionOption {
                    label: "Unit".to_owned(),
                    description: String::new(),
                },
                QuestionOption {
                    label: "All".to_owned(),
                    description: String::new(),
                },
            ],
            selection: Selection::Single,
        };
        ItemBody::Question(Question {
            pending: PendingId::new("request-3".to_owned()),
            questions: vec![asked; count],
            answer: AnswerState::Waiting,
        })
    }

    fn sent_answer(requests: &[Outgoing]) -> Option<&Answer> {
        match &requests.first()?.request {
            Request::AgentAnswer(params) => Some(&params.answer),
            _ => None,
        }
    }

    #[test]
    fn the_map_takes_its_keys_and_y_answers_its_selected_subagents_approval() {
        use cockpit_protocol::{
            Subagent, SubagentRun, SubagentStatus, Timestamp, ToolCall, ToolCallStatus,
        };

        use crate::state::agent_map::MapRow;

        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, None);
        let subagent = ItemId::new(2);
        let call = ToolCall {
            tool: "Agent".to_owned(),
            summary: "Clean up".to_owned(),
            input: Value::Null,
            status: ToolCallStatus::Running,
            output: None,
            exit_code: None,
            edit: None,
        };
        if let Some(entry) = state.agents.get_mut(&AgentId::new(1)) {
            entry.transcript.append(TranscriptItem {
                id: subagent,
                parent: None,
                body: ItemBody::Subagent(call),
            });
            entry.transcript.append(TranscriptItem {
                id: ItemId::new(3),
                parent: Some(subagent),
                body: approval(),
            });
            entry.details = Loadable::Loaded(AgentDetails {
                subagents: vec![Subagent {
                    item: subagent,
                    parent: None,
                    description: "Clean up".to_owned(),
                    agent_type: None,
                    run: SubagentRun::Foreground,
                    status: SubagentStatus::NeedsYou,
                    started_at: Timestamp::from_unix_millis(0),
                    ended_at: None,
                    tokens: None,
                    tool_calls: 1,
                    last_tool: None,
                    error: None,
                    turn: 1,
                    task: None,
                }],
                ..AgentDetails::default()
            });
        }

        handle_key(&mut state, &mut drafts, &ctrl("m"));
        let on_the_agent = handle_key(&mut state, &mut drafts, &press("y", KeyTarget::Page));
        handle_key(
            &mut state,
            &mut drafts,
            &press("ArrowDown", KeyTarget::Composer),
        );
        let answered = handle_key(&mut state, &mut drafts, &press("y", KeyTarget::Page));

        assert!(on_the_agent.is_empty(), "the agent's row has no card");
        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::AgentMap(view)) if view.selected == MapRow::Subagent(subagent)
        ));
        assert!(matches!(
            sent_answer(&answered),
            Some(Answer::Approval(answer)) if answer.decision == ApprovalDecision::Allow
        ));
        handle_key(&mut state, &mut drafts, &ctrl("m"));
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn ctrl_k_and_ctrl_n_open_their_dialogs() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);

        handle_key(&mut state, &mut drafts, &ctrl("k"));
        assert!(matches!(state.ui.dialog, Some(Dialog::Switcher)));

        handle_key(&mut state, &mut drafts, &ctrl("N"));
        assert!(matches!(state.ui.dialog, Some(Dialog::NewAgent(_))));
    }

    #[test]
    fn escape_closes_what_is_open_then_asks_before_it_stops_a_working_agent() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Working, None);
        state.ui.popover = Some(Popover::Usage);
        let mut escape = |state: &mut AppState, target| {
            handle_key(state, &mut drafts, &press(KEY_ESCAPE, target))
        };

        let closing = escape(&mut state, KeyTarget::Page);
        let popover_after = state.ui.popover;
        let asking = escape(&mut state, KeyTarget::Composer);
        let dialog_after = state.ui.dialog.clone();
        let keeping = escape(&mut state, KeyTarget::Page);

        assert!(closing.is_empty());
        assert_eq!(popover_after, None);
        assert!(asking.is_empty());
        assert_eq!(dialog_after, Some(Dialog::StopAgent(AgentId::new(1))));
        assert!(
            keeping.is_empty(),
            "Esc on the dialog keeps the agent working"
        );
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn ctrl_f_opens_the_chat_find_on_the_selection() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        let find = KeyPress {
            selection: "lexer.rs".to_owned(),
            ..ctrl("f")
        };

        handle_key(&mut state, &mut drafts, &find);

        assert!(find::is_open(&state, FindPane::Chat, AgentId::new(1)));
        let query = drafts.finds.get(FindPane::Chat, AgentId::new(1));
        assert_eq!(query.map(|draft| draft.query.as_str()), Some("lexer.rs"));

        // Behind a dialog, Ctrl+F opens nothing.
        let mut state = self::state(AgentStatus::Idle, None);
        state.ui.dialog = Some(Dialog::Switcher);
        handle_key(&mut state, &mut drafts, &find);
        assert!(!find::is_open(&state, FindPane::Chat, AgentId::new(1)));
    }

    #[test]
    fn escape_closes_the_find_before_it_stops_a_working_agent() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Working, None);
        find::open(&mut state, &mut drafts, FindPane::Chat, "");

        // In another text field, such as the files pane's search, Esc leaves the find open.
        handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Field),
        );
        assert!(find::is_open(&state, FindPane::Chat, AgentId::new(1)));

        handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Composer),
        );
        assert!(!find::is_open(&state, FindPane::Chat, AgentId::new(1)));
        assert_eq!(state.ui.dialog, None);

        handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Composer),
        );
        assert_eq!(state.ui.dialog, Some(Dialog::StopAgent(AgentId::new(1))));
    }

    #[test]
    fn escape_in_the_find_closes_an_open_comment_first_and_never_stops_the_agent() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Working, None);
        find::open(&mut state, &mut drafts, FindPane::Chat, "");
        let passage = quotes::Passage {
            anchor: quotes::QuoteAnchor {
                item: ItemId::new(3),
                start: Utf16Offset::new(0),
                end: Utf16Offset::new(4),
            },
            raw: "word".to_owned(),
            text: "word".to_owned(),
            code: false,
        };
        quotes::start(&mut drafts, AgentId::new(1), passage);
        let escape = KeyPress {
            in_find: true,
            ..press(KEY_ESCAPE, KeyTarget::Field)
        };

        handle_key(&mut state, &mut drafts, &escape);
        let comment_closed = drafts.quote_editor.is_none();
        let find_kept = find::is_open(&state, FindPane::Chat, AgentId::new(1));
        handle_key(&mut state, &mut drafts, &escape);
        let find_closed = !find::is_open(&state, FindPane::Chat, AgentId::new(1));
        handle_key(&mut state, &mut drafts, &escape);

        assert!(comment_closed && find_kept && find_closed);
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn escape_stops_a_busy_agent_at_once_when_settings_say_so() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, None);
        state.prefs.chat.esc_stop = EscStop::AtOnce;

        let sent = handle_key(&mut state, &mut drafts, &press(KEY_ESCAPE, KeyTarget::Page));

        let methods: Vec<Method> = sent
            .iter()
            .map(|outgoing| outgoing.request.method())
            .collect();
        assert_eq!(methods, [Method::AgentInterrupt]);
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn escape_leaves_an_idle_agent_alone() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);

        let sent = handle_key(&mut state, &mut drafts, &press(KEY_ESCAPE, KeyTarget::Page));

        assert!(sent.is_empty());
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn enter_and_tab_in_a_typed_palette_insert_the_command_and_send_nothing() {
        let agent = AgentId::new(1);
        for key in [KEY_ENTER, KEY_TAB] {
            let mut drafts = Drafts::default();
            let mut state = state(AgentStatus::Idle, None);
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.details = Loadable::Loaded(AgentDetails::default());
                entry.commands = vec![SlashCommand {
                    name: "review".to_owned(),
                    description: "Review the changes on the branch".to_owned(),
                    kind: CommandKind::Skill,
                }];
            }
            let typed = actions::composer_input(
                &state,
                &mut drafts,
                agent,
                "/rev".to_owned(),
                Utf16Offset::new(4),
            );
            if let Some(change) = typed {
                actions::change_palette(&mut state, agent, change);
            }
            let enter = KeyPress {
                composer_text: "/rev".to_owned(),
                ..press(key, KeyTarget::Composer)
            };

            let requests = handle_key(&mut state, &mut drafts, &enter);

            assert!(requests.is_empty(), "{key} sends nothing");
            let text = drafts.draft(agent).map(|draft| draft.text.as_str());
            assert_eq!(text, Some("/review "), "{key}");
            assert_eq!(state.ui.popover, None, "{key}");
        }
    }

    /// Shift+Enter adds a line, as it does without the palette: it picks no command.
    #[test]
    fn shift_enter_in_a_typed_palette_picks_nothing() {
        let agent = AgentId::new(1);
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        if let Some(entry) = state.agents.get_mut(&agent) {
            entry.details = Loadable::Loaded(AgentDetails::default());
            entry.commands = vec![SlashCommand {
                name: "review".to_owned(),
                description: "Review the changes on the branch".to_owned(),
                kind: CommandKind::Skill,
            }];
        }
        let typed = actions::composer_input(
            &state,
            &mut drafts,
            agent,
            "/rev".to_owned(),
            Utf16Offset::new(4),
        );
        if let Some(change) = typed {
            actions::change_palette(&mut state, agent, change);
        }
        let shift_enter = KeyPress {
            shift: true,
            composer_text: "/rev".to_owned(),
            ..press(KEY_ENTER, KeyTarget::Composer)
        };

        let requests = handle_key(&mut state, &mut drafts, &shift_enter);

        assert!(requests.is_empty(), "nothing is sent");
        let text = drafts.draft(agent).map(|draft| draft.text.as_str());
        assert_eq!(text, Some("/rev"), "the text stays as typed");
    }

    /// The draft can lag a keystroke behind the field: "/re" in the draft and "/ren" in the field
    /// must pick "rename-branch", not "review".
    #[test]
    fn enter_in_a_typed_palette_picks_from_the_text_the_key_saw() {
        let agent = AgentId::new(1);
        let command = |name: &str| SlashCommand {
            name: name.to_owned(),
            description: String::new(),
            kind: CommandKind::Command,
        };
        let pick = |field: &str, down: bool| {
            let mut drafts = Drafts::default();
            let mut state = state(AgentStatus::Idle, None);
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.details = Loadable::Loaded(AgentDetails::default());
                entry.commands = vec![command("review"), command("rename-branch")];
            }
            let typed = actions::composer_input(
                &state,
                &mut drafts,
                agent,
                "/re".to_owned(),
                Utf16Offset::new(3),
            );
            if let Some(change) = typed {
                actions::change_palette(&mut state, agent, change);
            }
            if down {
                handle_key(
                    &mut state,
                    &mut drafts,
                    &press(KEY_DOWN, KeyTarget::Composer),
                );
            }
            let enter = KeyPress {
                composer_text: field.to_owned(),
                ..press(KEY_ENTER, KeyTarget::Composer)
            };
            let requests = handle_key(&mut state, &mut drafts, &enter);
            let text = drafts.draft(agent).map(|draft| draft.text.clone());
            (requests.is_empty(), text)
        };

        assert_eq!(
            pick("/ren", false),
            (true, Some("/rename-branch ".to_owned()))
        );
        // The same text keeps the highlight the keys moved.
        assert_eq!(
            pick("/re", true),
            (true, Some("/rename-branch ".to_owned()))
        );
    }

    #[test]
    fn escape_keeps_the_palette_closed_for_the_rest_of_its_slash_word() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        let agent = AgentId::new(1);
        let type_text = |state: &mut AppState, drafts: &mut Drafts, text: &str| {
            let caret = Utf16Offset::end_of(text);
            let change = actions::composer_input(state, drafts, agent, text.to_owned(), caret);
            if let Some(change) = change {
                actions::change_palette(state, agent, change);
            }
            state.ui.popover
        };

        let opened = type_text(&mut state, &mut drafts, "run /re");
        handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Composer),
        );
        let same_word = type_text(&mut state, &mut drafts, "run /rev");
        let next_word = type_text(&mut state, &mut drafts, "run /rev then /co");

        let typed = Some(Popover::Palette(PaletteOrigin::Typed));
        assert_eq!((opened, same_word, next_word), (typed, None, typed));
    }

    fn with_review_commands(state: &mut AppState) {
        let command = |name: &str, description: &str, kind| SlashCommand {
            name: name.to_owned(),
            description: description.to_owned(),
            kind,
        };
        if let Some(entry) = state.agents.get_mut(&AgentId::new(1)) {
            entry.details = Loadable::Loaded(AgentDetails::default());
            entry.commands = vec![
                command(
                    "review",
                    "Review the changes on the branch",
                    CommandKind::Skill,
                ),
                command(
                    "mcp",
                    "Manage MCP servers, tools, etc.",
                    CommandKind::Command,
                ),
            ];
        }
    }

    /// Types `text` with the caret at its end.
    fn typed(state: &mut AppState, drafts: &mut Drafts, agent: AgentId, text: &str) {
        typed_at(state, drafts, agent, text, Utf16Offset::end_of(text));
    }

    fn typed_at(
        state: &mut AppState,
        drafts: &mut Drafts,
        agent: AgentId,
        text: &str,
        caret: Utf16Offset,
    ) {
        if let Some(change) = actions::composer_input(state, drafts, agent, text.to_owned(), caret)
        {
            actions::change_palette(state, agent, change);
        }
    }

    fn enter(text: &str) -> KeyPress {
        KeyPress {
            composer_text: text.to_owned(),
            ..press(KEY_ENTER, KeyTarget::Composer)
        }
    }

    #[test]
    fn a_caret_just_before_a_slash_opens_no_palette_and_enter_sends_the_text() {
        let agent = AgentId::new(1);
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        with_review_commands(&mut state);
        let text = "fix /mcp";

        typed_at(&mut state, &mut drafts, agent, text, Utf16Offset::new(4));
        let opened = state.ui.popover;
        let requests = handle_key(&mut state, &mut drafts, &enter(text));

        let draft = drafts.draft(agent).map(|draft| draft.text.as_str());
        assert_eq!((opened, requests.len(), draft), (None, 1, Some("")));
    }

    #[test]
    fn enter_on_a_slash_word_no_command_matches_sends_the_message() {
        let agent = AgentId::new(1);
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        with_review_commands(&mut state);
        let text = "clean up /tmp";

        typed(&mut state, &mut drafts, agent, text);
        let opened = state.ui.popover;
        let requests = handle_key(&mut state, &mut drafts, &enter(text));

        assert_eq!(opened, Some(Popover::Palette(PaletteOrigin::Typed)));
        assert!(matches!(
            requests.first().map(|outgoing| &outgoing.request),
            Some(Request::AgentPrompt(params)) if params.prompt.text == text
        ));
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn enter_on_a_slash_word_waits_while_the_commands_load() {
        let agent = AgentId::new(1);
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);

        typed(&mut state, &mut drafts, agent, "/rev");
        let loading = state.agents.get(&agent).map(AgentEntry::commands_loading);
        let requests = handle_key(&mut state, &mut drafts, &enter("/rev"));

        assert_eq!(loading, Some(true));
        assert!(requests.is_empty());
        let draft = drafts.draft(agent).map(|draft| draft.text.as_str());
        assert_eq!(draft, Some("/rev"));
    }

    #[test]
    fn a_slash_word_inside_a_message_matches_command_names_alone() {
        let agent = AgentId::new(1);
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        with_review_commands(&mut state);
        // "etc" is in the description of /mcp.
        let text = "look at /etc";

        typed(&mut state, &mut drafts, agent, text);
        let requests = handle_key(&mut state, &mut drafts, &enter(text));

        let draft = drafts.draft(agent).map(|draft| draft.text.as_str());
        assert_eq!((requests.len(), draft), (1, Some("")));
    }

    #[test]
    fn a_message_of_one_slash_word_matches_command_descriptions_too() {
        let agent = AgentId::new(1);
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        with_review_commands(&mut state);

        typed(&mut state, &mut drafts, agent, "/etc");
        let requests = handle_key(&mut state, &mut drafts, &enter("/etc"));

        let draft = drafts.draft(agent).map(|draft| draft.text.as_str());
        assert_eq!((requests.len(), draft), (0, Some("/mcp ")));
    }

    #[test]
    fn escape_closes_the_shown_comment_field_alone_and_leaves_another_agents_field() {
        let mut drafts = Drafts::default();
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        state.machines.this.connection = ConnectionState::Connected;
        state.ui.selected = Some(AgentId::new(1));
        let passage = quotes::Passage {
            anchor: quotes::QuoteAnchor {
                item: ItemId::new(3),
                start: Utf16Offset::new(0),
                end: Utf16Offset::new(4),
            },
            raw: "word".to_owned(),
            text: "word".to_owned(),
            code: false,
        };
        quotes::start(&mut drafts, AgentId::new(2), passage.clone());
        state.ui.popover = Some(Popover::Model);

        handle_key(&mut state, &mut drafts, &press(KEY_ESCAPE, KeyTarget::Page));
        let other_field_kept = drafts.quote_editor.is_some();
        let popover_closed = state.ui.popover.is_none();
        quotes::start(&mut drafts, AgentId::new(1), passage);
        state.ui.popover = Some(Popover::Model);
        handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Field),
        );

        assert!(other_field_kept, "the hidden field of agent 2 stays");
        assert!(popover_closed, "Esc closes what shows");
        assert_eq!(drafts.quote_editor, None);
        assert_eq!(
            state.ui.popover,
            Some(Popover::Model),
            "the shown field closes first, and alone"
        );
    }

    #[test]
    fn escape_on_one_agent_leaves_the_palette_of_another_agent_alone() {
        let mut drafts = Drafts::default();
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        state.machines.this.connection = ConnectionState::Connected;
        state.ui.selected = Some(AgentId::new(1));

        typed(&mut state, &mut drafts, AgentId::new(1), "/re");
        handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Composer),
        );
        actions::select_agent(&mut state, AgentId::new(2));
        typed(&mut state, &mut drafts, AgentId::new(2), "/");

        assert_eq!(
            state.ui.popover,
            Some(Popover::Palette(PaletteOrigin::Typed))
        );
    }

    #[test]
    fn each_agent_keeps_the_slash_word_escape_closed() {
        let (first, second) = (AgentId::new(1), AgentId::new(2));
        let mut drafts = Drafts::default();
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        state.machines.this.connection = ConnectionState::Connected;
        let escape = press(KEY_ESCAPE, KeyTarget::Composer);

        state.ui.selected = Some(first);
        typed(&mut state, &mut drafts, first, "/re");
        handle_key(&mut state, &mut drafts, &escape);
        actions::select_agent(&mut state, second);
        typed(&mut state, &mut drafts, second, "/co");
        handle_key(&mut state, &mut drafts, &escape);
        actions::select_agent(&mut state, first);
        typed(&mut state, &mut drafts, first, "/rev");

        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn a_slash_word_closed_with_escape_and_sent_leaves_the_next_message_its_palette() {
        let agent = AgentId::new(1);
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);

        typed(&mut state, &mut drafts, agent, "/xyz");
        handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Composer),
        );
        let sent = handle_key(&mut state, &mut drafts, &enter("/xyz"));
        typed(&mut state, &mut drafts, agent, "/");

        assert_eq!(sent.len(), 1);
        assert_eq!(
            state.ui.popover,
            Some(Popover::Palette(PaletteOrigin::Typed))
        );
    }

    #[test]
    fn escape_closes_the_palette_whatever_opened_it() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        let agent = AgentId::new(1);
        let escape = press(KEY_ESCAPE, KeyTarget::Field);

        actions::toggle_palette(&mut state, &mut drafts, agent);
        handle_key(&mut state, &mut drafts, &escape);
        let after_button = state.ui.popover;
        let typed = actions::composer_input(
            &state,
            &mut drafts,
            agent,
            "/".to_owned(),
            Utf16Offset::new(1),
        );
        if let Some(change) = typed {
            actions::change_palette(&mut state, agent, change);
        }
        handle_key(&mut state, &mut drafts, &escape);

        assert_eq!(after_button, None);
        assert_eq!(state.ui.popover, None);
    }

    #[test]
    fn escape_does_not_stop_an_agent_that_waits_for_you() {
        let mut drafts = Drafts::default();
        let mut waiting = state(AgentStatus::NeedsYou, Some(question()));
        let mut idle = state(AgentStatus::Idle, None);

        // The question's "Other" field is a field like any other.
        let from_field = handle_key(
            &mut waiting,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Field),
        );
        let from_page = handle_key(
            &mut waiting,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Page),
        );
        let from_composer = handle_key(
            &mut idle,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Composer),
        );

        assert!(from_field.is_empty());
        assert!(from_page.is_empty());
        assert!(from_composer.is_empty());
    }

    #[test]
    fn escape_in_a_field_closes_a_menu_then_a_dialog_and_stops_nothing() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Working, None);
        state.ui.popover = Some(Popover::Model);

        let menu = handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Field),
        );
        actions::toggle_switcher(&mut state, &mut drafts);
        let dialog = handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Field),
        );
        let stop = handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Field),
        );

        assert!(menu.is_empty() && dialog.is_empty());
        assert_eq!((state.ui.popover, &state.ui.dialog), (None, &None));
        assert!(stop.is_empty());
    }

    #[test]
    fn y_and_n_answer_an_approval_from_the_page_or_an_empty_composer() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, Some(approval()));

        let allowed = handle_key(&mut state, &mut drafts, &press("y", KeyTarget::Page));
        let denied = handle_key(&mut state, &mut drafts, &press("N", KeyTarget::Composer));

        assert!(matches!(
            sent_answer(&allowed),
            Some(Answer::Approval(answer)) if answer.decision == ApprovalDecision::Allow
        ));
        assert!(matches!(
            sent_answer(&denied),
            Some(Answer::Approval(answer)) if answer.decision == ApprovalDecision::Deny
        ));
    }

    #[test]
    fn letters_in_a_composer_with_text_are_typing() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, Some(approval()));
        let typing = KeyPress {
            composer_text: "no, ".to_owned(),
            ..press("y", KeyTarget::Composer)
        };

        assert!(handle_key(&mut state, &mut drafts, &typing).is_empty());
        assert!(handle_key(&mut state, &mut drafts, &press("y", KeyTarget::Field)).is_empty());
    }

    #[test]
    fn numbers_pick_and_enter_sends_a_question_answer() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, Some(question()));

        handle_key(&mut state, &mut drafts, &press("2", KeyTarget::Page));
        let requests = handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ENTER, KeyTarget::Composer),
        );

        assert!(matches!(
            sent_answer(&requests),
            Some(Answer::Question { answers }) if answers.first().is_some_and(|answer| answer.selected == ["All"])
        ));
    }

    #[test]
    fn enter_that_shows_the_next_question_sends_nothing_from_the_composer() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, Some(questions(2)));
        let image = Image {
            media_type: ImageMediaType::Png,
            data: "iVBORw0KGgo=".to_owned(),
        };
        actions::add_image(&mut drafts, AgentId::new(1), image).unwrap();

        let sent = handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ENTER, KeyTarget::Composer),
        );

        assert!(sent.is_empty());
        let shown = drafts.questions.values().next().map(|draft| draft.shown);
        assert_eq!(shown, Some(1));
    }

    #[test]
    fn enter_sends_the_composer_text_and_shift_enter_does_not() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        let typed = KeyPress {
            composer_text: "Run the linter".to_owned(),
            ..press(KEY_ENTER, KeyTarget::Composer)
        };
        let new_line = KeyPress {
            shift: true,
            ..typed.clone()
        };

        assert!(handle_key(&mut state, &mut drafts, &new_line).is_empty());
        let sent = handle_key(&mut state, &mut drafts, &typed);
        assert!(matches!(
            sent.first().map(|outgoing| &outgoing.request),
            Some(Request::AgentPrompt(params)) if params.prompt.text == "Run the linter"
        ));
    }

    #[test]
    fn enter_steers_a_working_agent_and_alt_enter_queues() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Working, None);
        let delivery = |state: &mut AppState, drafts: &mut Drafts, alt| {
            let press = KeyPress {
                alt,
                composer_text: "Also the docs".to_owned(),
                ..press(KEY_ENTER, KeyTarget::Composer)
            };
            match handle_key(state, drafts, &press).first() {
                Some(Outgoing {
                    request: Request::AgentPrompt(params),
                    ..
                }) => Some(params.delivery),
                _ => None,
            }
        };

        assert_eq!(
            delivery(&mut state, &mut drafts, false),
            Some(PromptDelivery::Steer)
        );
        assert_eq!(
            delivery(&mut state, &mut drafts, true),
            Some(PromptDelivery::Queue)
        );
    }

    #[test]
    fn alt_up_and_alt_down_move_the_selected_agent_except_from_a_field_or_a_dialog() {
        let mut drafts = Drafts::default();
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        // Equal positions list the newer agent first: agent 1 is at the bottom.
        state.ui.selected = Some(AgentId::new(1));
        state.machines.this.connection = ConnectionState::Connected;
        let alt = |key: &str, target| KeyPress {
            alt: true,
            ..press(key, target)
        };

        let from_field = handle_key(&mut state, &mut drafts, &alt(KEY_UP, KeyTarget::Field));
        let from_composer = handle_key(&mut state, &mut drafts, &alt(KEY_UP, KeyTarget::Composer));
        let from_page = handle_key(&mut state, &mut drafts, &alt(KEY_DOWN, KeyTarget::Page));
        actions::toggle_switcher(&mut state, &mut drafts);
        let behind_a_dialog = handle_key(&mut state, &mut drafts, &alt(KEY_DOWN, KeyTarget::Page));

        let method =
            |requests: &[Outgoing]| requests.first().map(|outgoing| outgoing.request.method());
        assert!(from_field.is_empty());
        assert_eq!(method(&from_composer), Some(Method::AgentMove));
        assert_eq!(method(&from_page), Some(Method::AgentMove));
        assert!(behind_a_dialog.is_empty());
    }

    #[test]
    fn f2_opens_the_rows_title_field_and_moves_it_from_the_headers() {
        use crate::state::TitlePlace;

        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        let place = |state: &AppState| state.ui.title_edit.as_ref().map(|edit| edit.place);

        handle_key(&mut state, &mut drafts, &press("F2", KeyTarget::Page));
        let from_page = place(&state);
        state.ui.title_edit = None;
        actions::start_rename(&mut state, &mut drafts, AgentId::new(1));
        handle_key(&mut state, &mut drafts, &press("F2", KeyTarget::Field));
        let from_header = place(&state);
        state.ui.title_edit = None;
        actions::toggle_switcher(&mut state, &mut drafts);
        handle_key(&mut state, &mut drafts, &press("F2", KeyTarget::Page));

        assert_eq!(from_page, Some(TitlePlace::Sidebar));
        assert_eq!(from_header, Some(TitlePlace::Sidebar));
        assert_eq!(place(&state), None, "nothing behind a dialog");
    }

    #[test]
    fn f5_reads_the_shown_file_again_from_anywhere() {
        use std::path::{Path, PathBuf};

        use crate::state::app_state::{FileRead, Reread};

        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        let agent = AgentId::new(1);
        actions::open_file(&mut state, agent, PathBuf::from("notes.md"), None);
        let path = Path::new("notes.md");
        let text = FileRead::unedited("one\n");
        actions::show_read_file(&mut state, agent, path, path, text, None);

        handle_key(&mut state, &mut drafts, &press("F5", KeyTarget::Field));

        assert_eq!(state.agents[&agent].file_freshness.reread, Reread::Wanted);
    }

    #[test]
    fn a_changed_shortcut_runs_its_command_and_the_old_one_nothing() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Idle, None);
        state
            .prefs
            .keys
            .set(Command::Switcher, "Ctrl+j".parse().unwrap())
            .unwrap();

        handle_key(&mut state, &mut drafts, &ctrl("k"));
        let old = state.ui.dialog.clone();
        handle_key(&mut state, &mut drafts, &ctrl("J"));

        assert_eq!(old, None);
        assert!(matches!(state.ui.dialog, Some(Dialog::Switcher)));
    }

    #[test]
    fn ctrl_shift_w_asks_to_close_the_selected_agent_and_confirms_when_asked() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Working, None);
        let close = KeyPress {
            shift: true,
            key: "W".to_owned(),
            ..ctrl("w")
        };

        let asked = handle_key(&mut state, &mut drafts, &close);
        let dialog = state.ui.dialog.clone();
        let confirmed = handle_key(&mut state, &mut drafts, &close);

        assert!(asked.is_empty());
        assert_eq!(dialog, Some(Dialog::CloseAgent(AgentId::new(1))));
        assert!(matches!(
            confirmed.first().map(|outgoing| &outgoing.request),
            Some(Request::AgentClose(params)) if params.agent == AgentId::new(1)
        ));
        assert!(state.agents.is_empty());
    }

    #[test]
    fn key_targets_parse_from_the_script() {
        let targets: Vec<KeyTarget> =
            serde_json::from_str(r#"["composer","field","page"]"#).unwrap();

        assert_eq!(
            targets,
            [KeyTarget::Composer, KeyTarget::Field, KeyTarget::Page]
        );
    }

    #[test]
    fn page_keys_do_nothing_behind_a_dialog() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::NeedsYou, Some(approval()));
        actions::toggle_switcher(&mut state, &mut drafts);

        assert!(handle_key(&mut state, &mut drafts, &press("y", KeyTarget::Page)).is_empty());
    }

    /// An agent whose turn found no login: its message, then the turn's end.
    fn sign_in_turn_state(wait: SignInWait) -> AppState {
        use std::time::Duration;

        use cockpit_protocol::{
            CostUsd, HistoryPage, OlderItems, TokenUsage, TurnEnd, TurnOutcome, UserMessage,
        };

        let mut state = state(AgentStatus::Idle, None);
        let entry = state.agents.get_mut(&AgentId::new(1)).unwrap();
        entry.summary.sign_in_wait = wait;
        let items = [
            ItemBody::UserMessage(UserMessage {
                text: "Export the invoices".to_owned(),
                images: Vec::new(),
                sent_at: None,
            }),
            ItemBody::TurnEnd(TurnEnd {
                duration: Duration::from_millis(900),
                cost: CostUsd::new(0.0),
                usage: TokenUsage::default(),
                outcome: TurnOutcome::NotSignedIn,
                error: Some("The account is not signed in".to_owned()),
            }),
        ];
        let items = items
            .into_iter()
            .zip(10..)
            .map(|(body, id)| TranscriptItem {
                id: ItemId::new(id),
                parent: None,
                body,
            });
        entry.transcript.merge_page(HistoryPage {
            items: items.collect(),
            older_items: OlderItems::None,
        });
        state
    }

    #[test]
    fn enter_outside_text_fields_sends_the_message_again_once_signed_in() {
        let mut drafts = Drafts::default();
        let mut waiting = sign_in_turn_state(SignInWait::Waiting);
        let mut signed_in = sign_in_turn_state(SignInWait::NotWaiting);
        let mut in_composer = sign_in_turn_state(SignInWait::NotWaiting);
        let mut in_field = sign_in_turn_state(SignInWait::NotWaiting);
        let enter = |target| press(KEY_ENTER, target);

        let while_waiting = handle_key(&mut waiting, &mut drafts, &enter(KeyTarget::Page));
        let sent = handle_key(&mut signed_in, &mut drafts, &enter(KeyTarget::Page));
        let again = handle_key(&mut signed_in, &mut drafts, &enter(KeyTarget::Page));
        let from_composer = handle_key(&mut in_composer, &mut drafts, &enter(KeyTarget::Composer));
        let from_field = handle_key(&mut in_field, &mut drafts, &enter(KeyTarget::Field));

        assert_eq!(while_waiting, []);
        assert!(matches!(
            sent.as_slice(),
            [Outgoing { request: Request::AgentPrompt(params), .. }]
                if params.prompt.text == "Export the invoices"
        ));
        assert_eq!(again, [], "the message goes once");
        assert_eq!(from_composer, []);
        assert_eq!(from_field, []);
    }
}
