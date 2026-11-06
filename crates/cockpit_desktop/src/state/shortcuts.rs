//! The keyboard shortcuts of R4-parts, resolved against the state. The page's script decides
//! which key presses reach here (`bridge`), so typing in a field is never taken over.

use cockpit_protocol::{AgentId, ApprovalDecision, PlanDecision};
use serde::Deserialize;

use super::actions::{self, Direction};
use super::agent_map;
use super::app_state::{AgentEntry, AppState};
use super::drafts::Drafts;
use super::open_file;
use super::requests::Outgoing;
use super::transcript::WaitingKind;
use super::ui_state::{Dialog, PaletteOrigin, Popover};
use super::utf16_offset::Utf16Offset;

/// `KeyboardEvent.key` values.
const KEY_ESCAPE: &str = "Escape";
const KEY_ENTER: &str = "Enter";
const KEY_TAB: &str = "Tab";
const KEY_UP: &str = "ArrowUp";
const KEY_DOWN: &str = "ArrowDown";
const KEY_F2: &str = "F2";

/// The letters that answer an approval or a plan.
const KEY_YES: &str = "y";
const KEY_NO: &str = "n";

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
    let letter = press.key.to_lowercase();
    if command && !press.alt && !press.shift {
        match letter.as_str() {
            "k" => actions::toggle_switcher(state, drafts),
            "n" => actions::open_new_agent(state, drafts),
            "o" => open_file::toggle(state, drafts),
            "m" => agent_map::toggle(state),
            _ => {}
        }
        return Vec::new();
    }
    // Ctrl+Shift+W asks to close the selected agent, and confirms when asked.
    if command && !press.alt && press.shift && letter == "w" {
        return match (&state.ui.dialog, state.ui.selected) {
            (Some(Dialog::CloseAgent(agent)), _) => actions::close_agent(state, drafts, *agent),
            (None, Some(agent)) => actions::request_close(state, drafts, agent),
            (Some(_), _) | (None, None) => Vec::new(),
        };
    }
    // Esc only closes what is open: the Stop button alone stops an agent, so a stray Esc never
    // cuts a turn short.
    if press.key == KEY_ESCAPE {
        // A palette opened by typing stays closed for the rest of its "/" word.
        if state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed))
            && let (Some(agent), Some(word)) = (state.ui.selected, drafts.palette.word)
        {
            drafts.palette_dismissed.insert(agent, word.start);
        }
        state.ui.close_overlays();
        return Vec::new();
    }
    // The map takes its keys from anywhere, the composer behind it included.
    if matches!(state.ui.dialog, Some(Dialog::AgentMap(_))) {
        if command || press.alt {
            return Vec::new();
        }
        return agent_map::press(state, &press.key);
    }
    // The script passes F2 on from outside text fields, and from the title fields.
    let plain = !command && !press.alt && !press.shift;
    if press.key == KEY_F2 && plain && state.ui.dialog.is_none() {
        actions::rename_key(state, drafts);
        return Vec::new();
    }
    // Alt+Up and Alt+Down move the selected agent in the sidebar, from anywhere but a text field
    // other than the composer.
    let moves = press.alt && !command && !press.shift;
    if moves && state.ui.dialog.is_none() && press.target != KeyTarget::Field {
        match press.key.as_str() {
            KEY_UP => return actions::move_selected(state, Direction::Up),
            KEY_DOWN => return actions::move_selected(state, Direction::Down),
            _ => {}
        }
    }
    if command || press.alt {
        return Vec::new();
    }
    let Some(agent) = state.ui.selected else {
        return Vec::new();
    };
    match press.target {
        KeyTarget::Composer => composer_key(state, drafts, agent, press),
        KeyTarget::Page if state.ui.dialog.is_none() => card_key(state, drafts, agent, &press.key),
        KeyTarget::Page | KeyTarget::Field => Vec::new(),
    }
}

fn composer_key(
    state: &mut AppState,
    drafts: &mut Drafts,
    agent: AgentId,
    press: &KeyPress,
) -> Vec<Outgoing> {
    if state.ui.popover == Some(Popover::Palette(PaletteOrigin::Typed)) {
        match press.key.as_str() {
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
        let answered = card_key(state, drafts, agent, &press.key);
        if !answered.is_empty() || press.key != KEY_ENTER {
            return answered;
        }
    }
    if press.key == KEY_ENTER && !press.shift {
        let typed = Some(press.composer_text.clone());
        return actions::send_prompt(state, drafts, agent, typed);
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

/// Y and N answer an approval or a plan; 1 to 9 pick an answer to a question, Enter sends it.
fn card_key(state: &AppState, drafts: &mut Drafts, agent: AgentId, key: &str) -> Vec<Outgoing> {
    let Some(waiting) = state
        .agents
        .get(&agent)
        .and_then(|entry| entry.transcript.waiting())
    else {
        return Vec::new();
    };
    let letter = key.to_lowercase();
    match (waiting.kind, letter.as_str()) {
        (WaitingKind::Approval, KEY_YES) => {
            actions::answer_approval(agent, waiting.pending, ApprovalDecision::Allow)
        }
        (WaitingKind::Approval, KEY_NO) => {
            actions::answer_approval(agent, waiting.pending, ApprovalDecision::Deny)
        }
        (WaitingKind::Plan, KEY_YES) => {
            actions::answer_plan(agent, waiting.pending, PlanDecision::AcceptEdits)
        }
        (WaitingKind::Plan, KEY_NO) => {
            actions::answer_plan(agent, waiting.pending, PlanDecision::KeepPlanning)
        }
        (WaitingKind::Question, _) if key == KEY_ENTER => {
            actions::submit_question(state, drafts, agent, waiting.item)
        }
        (WaitingKind::Question, digit) => {
            if let Ok(number) = digit.parse::<usize>() {
                actions::pick_by_number(state, drafts, agent, waiting.item, number);
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{
        AgentDetails, AgentStatus, Answer, AnswerState, Approval, AskedQuestion, CommandKind,
        ItemBody, ItemId, Method, PendingId, Question, QuestionOption, Request, SlashCommand,
        TranscriptItem,
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
        state.connection = ConnectionState::Connected;
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
        ItemBody::Question(Question {
            pending: PendingId::new("request-3".to_owned()),
            questions: vec![AskedQuestion {
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
                multi_select: false,
            }],
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
    fn escape_closes_what_is_open_and_never_stops_a_working_agent() {
        let mut drafts = Drafts::default();
        let mut state = state(AgentStatus::Working, None);
        state.ui.popover = Some(Popover::Usage);

        let first = handle_key(&mut state, &mut drafts, &press(KEY_ESCAPE, KeyTarget::Page));
        let second = handle_key(&mut state, &mut drafts, &press(KEY_ESCAPE, KeyTarget::Page));
        let from_composer = handle_key(
            &mut state,
            &mut drafts,
            &press(KEY_ESCAPE, KeyTarget::Composer),
        );

        assert!(first.is_empty());
        assert_eq!(state.ui.popover, None);
        assert!(second.is_empty());
        assert!(from_composer.is_empty());
    }

    #[test]
    fn enter_and_tab_in_a_typed_palette_insert_the_command_and_send_nothing() {
        let agent = AgentId::new(1);
        for key in [KEY_ENTER, KEY_TAB] {
            let mut drafts = Drafts::default();
            let mut state = state(AgentStatus::Idle, None);
            if let Some(entry) = state.agents.get_mut(&agent) {
                entry.details = Loadable::Loaded(AgentDetails {
                    commands: vec![SlashCommand {
                        name: "review".to_owned(),
                        description: "Review the changes on the branch".to_owned(),
                        kind: CommandKind::Skill,
                    }],
                    ..AgentDetails::default()
                });
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
                entry.details = Loadable::Loaded(AgentDetails {
                    commands: vec![command("review"), command("rename-branch")],
                    ..AgentDetails::default()
                });
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
            entry.details = Loadable::Loaded(AgentDetails {
                commands: vec![
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
                ],
                ..AgentDetails::default()
            });
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
    fn escape_on_one_agent_leaves_the_palette_of_another_agent_alone() {
        let mut drafts = Drafts::default();
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        state.connection = ConnectionState::Connected;
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
        state.connection = ConnectionState::Connected;
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
    fn alt_up_and_alt_down_move_the_selected_agent_except_from_a_field_or_a_dialog() {
        let mut drafts = Drafts::default();
        let mut state = state_with(vec![
            summary(1, AgentStatus::Idle),
            summary(2, AgentStatus::Idle),
        ]);
        // Equal positions list the newer agent first: agent 1 is at the bottom.
        state.ui.selected = Some(AgentId::new(1));
        state.connection = ConnectionState::Connected;
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

        handle_key(&mut state, &mut drafts, &press(KEY_F2, KeyTarget::Page));
        let from_page = place(&state);
        state.ui.title_edit = None;
        actions::start_rename(&mut state, &mut drafts, AgentId::new(1));
        handle_key(&mut state, &mut drafts, &press(KEY_F2, KeyTarget::Field));
        let from_header = place(&state);
        state.ui.title_edit = None;
        actions::toggle_switcher(&mut state, &mut drafts);
        handle_key(&mut state, &mut drafts, &press(KEY_F2, KeyTarget::Page));

        assert_eq!(from_page, Some(TitlePlace::Sidebar));
        assert_eq!(from_header, Some(TitlePlace::Sidebar));
        assert_eq!(place(&state), None, "nothing behind a dialog");
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
}
