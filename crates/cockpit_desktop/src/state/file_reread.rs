//! Reading the files the file views show again, so that they follow the files' changes:
//!
//! - when the file comes back on screen: its agent selected again, the window back in front;
//! - when its agent finishes an edit of it while it shows;
//! - on Reload or F5, for changes the transcript does not show.
//!
//! The old text stays on screen until the new one arrives, and the lines the new one changed are
//! kept for the view to highlight.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cockpit_protocol::{AgentId, ItemBody, Timestamp, ToolCallStatus, TranscriptItem};

use super::app_state::{AppState, FileFreshness, FileRead, FileUpdate, Loadable, Reread};
use super::file_edits;
use crate::path_style::PathStyle;
use crate::render;

/// Reads the files `agent`'s views show again.
pub fn reread(state: &mut AppState, agent: AgentId) {
    for path in state.ui.shown_files(agent) {
        reread_path(state, agent, &path);
    }
}

/// Reads `path` again. A file that did not read reads from the start, and so does a name no file
/// was found for, once the search for it that runs ends. A name that several files end with keeps
/// its list to pick from.
fn reread_path(state: &mut AppState, agent: AgentId, path: &Path) {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return;
    };
    let Some(held) = entry.files.get_mut(path) else {
        return;
    };
    let freshness = entry.file_freshness.entry(path.to_owned()).or_default();
    match held {
        Loadable::Loaded(FileRead::Text { .. } | FileRead::Image(_) | FileRead::Pdf(_)) => {
            freshness.reread = match freshness.reread {
                Reread::Idle | Reread::Wanted => Reread::Wanted,
                Reread::Reading { .. } => Reread::Reading { again: true },
            };
        }
        Loadable::Loaded(FileRead::NotFound(not_found)) if not_found.search.is_some() => {
            not_found.look_again = true;
        }
        Loadable::Loaded(FileRead::Missing(_) | FileRead::NotFound(_)) | Loadable::Failed(_) => {
            *held = Loadable::NotRequested;
        }
        // The answer on its way may predate the change: one more read follows it.
        Loadable::Loading => freshness.reread = Reread::Reading { again: true },
        Loadable::Loaded(FileRead::Choices(_) | FileRead::IgnoredChoices { .. })
        | Loadable::NotRequested => {}
    }
}

/// `agent` finished `item`. An edit of a file one of its views shows reads the file again while
/// it is on screen: the agent is the selected one. Off screen, coming back reads it.
pub fn item_finished(state: &mut AppState, agent: AgentId, item: &TranscriptItem) {
    if state.ui.selected != Some(agent) {
        return;
    }
    let (ItemBody::ToolCall(call) | ItemBody::Subagent(call)) = &item.body else {
        return;
    };
    let Some(edit) = call
        .edit
        .as_ref()
        .filter(|_| call.status == ToolCallStatus::Done)
    else {
        return;
    };
    let Some(entry) = state.agents.get(&agent) else {
        return;
    };
    let folder = &entry.summary.folder;
    let edited = file_edits::relative_path(folder, &edit.path);
    let style = PathStyle::of(folder);
    let shown: Vec<PathBuf> = state
        .ui
        .shown_files(agent)
        .into_iter()
        .filter(|path| style.same_path(&edited.to_string_lossy(), &path.to_string_lossy()))
        .collect();
    for path in shown {
        reread_path(state, agent, &path);
    }
}

/// The window gained or lost the focus. Back in front, it reads the shown file again.
pub fn window_focused(state: &mut AppState, focused: bool) {
    let back = focused && !state.ui.window_focused;
    state.ui.window_focused = focused;
    if back {
        state.ui.focus_returns = state.ui.focus_returns.wrapping_add(1);
    }
    if back && let Some(agent) = state.ui.selected {
        reread(state, agent);
    }
}

/// The shown file's text arrived at `now`, after `old`. A read again that changed it records the
/// update, and the lines it changed.
pub fn arrived(
    freshness: &mut FileFreshness,
    old: Option<&FileRead>,
    new: &FileRead,
    now: Option<Timestamp>,
) {
    let again = match freshness.reread {
        Reread::Reading { again } => Some(again),
        Reread::Idle | Reread::Wanted => None,
    };
    // New edits alone leave the text as it was: nothing to show as updated.
    let lines: Option<BTreeSet<u32>> = match (again.and(old), new) {
        (Some(FileRead::Text { text: old, .. }), FileRead::Text { text: new, .. }) => {
            (old != new).then(|| render::changed_lines(old, new))
        }
        (Some(old), new) => (old != new).then(BTreeSet::new),
        (None, _) => None,
    };
    if let (Some(lines), Some(at)) = (lines, now) {
        let serial = freshness
            .update
            .as_ref()
            .map_or(0, |update| update.serial.wrapping_add(1));
        freshness.update = Some(FileUpdate { at, serial, lines });
    }
    freshness.read_at = now;
    freshness.reread = if again == Some(true) {
        Reread::Wanted
    } else {
        Reread::Idle
    };
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use cockpit_protocol::{AgentStatus, EditTexts, FileEdit, ItemId, ToolCall};
    use serde_json::Value;

    use super::*;
    use crate::state::app_state::FoundAmong;
    use crate::state::{ConnectionState, Purpose, ViewMode, actions, state_with, summary};

    const AGENT_ID: u32 = 1;

    fn agent() -> AgentId {
        AgentId::new(AGENT_ID)
    }

    fn at(millis: u64) -> Option<Timestamp> {
        Some(Timestamp::from_unix_millis(millis))
    }

    /// The agent selected, its file view on `path`, whose `text` arrived.
    fn showing(path: &str, text: &str) -> AppState {
        showing_read(path, FileRead::unedited(text))
    }

    /// The agent selected, its file view on `path`, whose `read` arrived.
    fn showing_read(path: &str, read: FileRead) -> AppState {
        let mut state = state_with(vec![summary(AGENT_ID, AgentStatus::Working)]);
        state.machines.this.connection = ConnectionState::Connected;
        state.ui.selected = Some(agent());
        actions::open_file(&mut state, agent(), PathBuf::from(path), None);
        let path = Path::new(path);
        actions::show_read_file(
            &mut state,
            agent(),
            path,
            path,
            read,
            at(1_000),
            FoundAmong::ListedFiles,
        );
        state
    }

    fn freshness(state: &AppState) -> FileFreshness {
        let path = &state.ui.file_view(agent()).unwrap().path;
        let entry = &state.agents[&agent()];
        entry.file_freshness.get(path).cloned().unwrap_or_default()
    }

    fn edit_of(path: &str, status: ToolCallStatus) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(7),
            parent: None,
            body: ItemBody::ToolCall(ToolCall {
                tool: "Edit".to_owned(),
                summary: path.to_owned(),
                input: Value::Null,
                status,
                output: None,
                exit_code: None,
                edit: Some(FileEdit {
                    path: PathBuf::from(path),
                    old_text: "old".to_owned(),
                    new_text: "new".to_owned(),
                    texts: EditTexts::Kept,
                }),
            }),
        }
    }

    #[test]
    fn a_read_again_keeps_the_text_and_records_the_changed_lines() {
        let mut state = showing("notes.md", "one\ntwo\n");
        assert_eq!(freshness(&state).read_at, at(1_000));

        reread(&mut state, agent());
        assert_eq!(freshness(&state).reread, Reread::Wanted);
        let reads = crate::state::needed_requests(&mut state)
            .into_iter()
            .filter(|outgoing| matches!(outgoing.purpose, Purpose::ReadFile { .. }))
            .count();
        assert_eq!(reads, 1);
        assert_eq!(freshness(&state).reread, Reread::Reading { again: false });
        assert_eq!(
            state.agents[&agent()].files[Path::new("notes.md")],
            Loadable::Loaded(FileRead::unedited("one\ntwo\n"))
        );

        let path = Path::new("notes.md");
        let new = FileRead::unedited("one\nTWO\nthree\n");
        actions::show_read_file(
            &mut state,
            agent(),
            path,
            path,
            new,
            at(2_000),
            FoundAmong::ListedFiles,
        );

        let fresh = freshness(&state);
        assert_eq!(fresh.read_at, at(2_000));
        assert_eq!(fresh.reread, Reread::Idle);
        let update = fresh.update.as_ref().unwrap();
        assert_eq!(
            (update.at, update.serial),
            (Timestamp::from_unix_millis(2_000), 0)
        );
        assert_eq!(update.lines, BTreeSet::from([2, 3]));
    }

    /// A PDF shows no text, but reads again as a text does: the old one stays on screen meanwhile.
    #[test]
    fn a_pdf_reads_again_and_a_changed_one_shows_as_updated() {
        let pdf = |bytes: &[u8]| FileRead::Pdf(Arc::from(bytes));
        let mut state = showing_read("report.pdf", pdf(b"%PDF-1"));

        reread(&mut state, agent());
        let reads = crate::state::needed_requests(&mut state)
            .into_iter()
            .filter(|outgoing| matches!(outgoing.purpose, Purpose::ReadFile { .. }))
            .count();
        let path = Path::new("report.pdf");
        let held = state.agents[&agent()].files[path].clone();
        actions::show_read_file(
            &mut state,
            agent(),
            path,
            path,
            pdf(b"%PDF-2"),
            at(2_000),
            FoundAmong::ListedFiles,
        );

        assert_eq!(reads, 1);
        assert_eq!(held, Loadable::Loaded(pdf(b"%PDF-1")));
        let update = freshness(&state).update.unwrap();
        assert_eq!(update.at, Timestamp::from_unix_millis(2_000));
        assert!(update.lines.is_empty(), "{:?}", update.lines);
    }

    #[test]
    fn a_read_again_keeps_the_mode_the_user_chose() {
        let mut state = showing("notes.md", "one\n");
        if let Some(entry) = state.agents.get_mut(&agent()) {
            entry
                .transcript
                .append(edit_of("notes.md", ToolCallStatus::Done));
        }
        actions::set_view_mode(&mut state, agent(), ViewMode::WholeFile);
        reread(&mut state, agent());
        let _ = crate::state::needed_requests(&mut state);

        let path = Path::new("notes.md");
        let new = FileRead::unedited("two\n");
        actions::show_read_file(
            &mut state,
            agent(),
            path,
            path,
            new,
            at(2_000),
            FoundAmong::ListedFiles,
        );

        assert_eq!(
            state.ui.file_view(agent()).map(|view| view.mode),
            Some(ViewMode::WholeFile)
        );
    }

    #[test]
    fn a_read_again_of_the_same_text_records_no_update() {
        let mut state = showing("notes.md", "one\n");
        reread(&mut state, agent());
        let _ = crate::state::needed_requests(&mut state);

        let path = Path::new("notes.md");
        let same = FileRead::unedited("one\n");
        actions::show_read_file(
            &mut state,
            agent(),
            path,
            path,
            same,
            at(2_000),
            FoundAmong::ListedFiles,
        );

        assert_eq!(freshness(&state).update, None);
        assert_eq!(freshness(&state).read_at, at(2_000));
    }

    #[test]
    fn a_change_while_a_read_is_on_its_way_reads_once_more() {
        let mut state = showing("notes.md", "one\n");
        reread(&mut state, agent());
        let _ = crate::state::needed_requests(&mut state);

        reread(&mut state, agent());
        assert_eq!(freshness(&state).reread, Reread::Reading { again: true });
        let path = Path::new("notes.md");
        let new = FileRead::unedited("two\n");
        actions::show_read_file(
            &mut state,
            agent(),
            path,
            path,
            new,
            at(2_000),
            FoundAmong::ListedFiles,
        );

        assert_eq!(freshness(&state).reread, Reread::Wanted);
    }

    #[test]
    fn an_edit_that_finishes_during_the_first_read_reads_once_more() {
        let mut state = state_with(vec![summary(AGENT_ID, AgentStatus::Working)]);
        state.machines.this.connection = ConnectionState::Connected;
        state.ui.selected = Some(agent());
        actions::open_file(&mut state, agent(), PathBuf::from("notes.md"), None);
        let _ = crate::state::needed_requests(&mut state);

        item_finished(
            &mut state,
            agent(),
            &edit_of("notes.md", ToolCallStatus::Done),
        );
        let path = Path::new("notes.md");
        let first = FileRead::unedited("new\n");
        actions::show_read_file(
            &mut state,
            agent(),
            path,
            path,
            first,
            at(1_000),
            FoundAmong::ListedFiles,
        );

        assert_eq!(freshness(&state).reread, Reread::Wanted);
        assert_eq!(freshness(&state).update, None);
    }

    #[test]
    fn a_read_again_that_brings_only_new_edits_records_no_update() {
        let mut state = showing("notes.md", "one\n");
        reread(&mut state, agent());
        let _ = crate::state::needed_requests(&mut state);

        let path = Path::new("notes.md");
        let edited = FileRead::Text {
            text: "one\n".to_owned(),
            edits: vec![FileEdit {
                path: PathBuf::from("notes.md"),
                old_text: "zero".to_owned(),
                new_text: "one".to_owned(),
                texts: EditTexts::Kept,
            }],
        };
        actions::show_read_file(
            &mut state,
            agent(),
            path,
            path,
            edited,
            at(2_000),
            FoundAmong::ListedFiles,
        );

        assert_eq!(freshness(&state).update, None);
    }

    #[test]
    fn only_a_finished_edit_of_the_shown_file_of_the_selected_agent_reads_it_again() {
        let mut state = showing("src/notes.md", "one\n");

        item_finished(
            &mut state,
            agent(),
            &edit_of("src/other.md", ToolCallStatus::Done),
        );
        item_finished(
            &mut state,
            agent(),
            &edit_of("src/notes.md", ToolCallStatus::Running),
        );
        assert_eq!(freshness(&state).reread, Reread::Idle);

        state.ui.selected = None;
        item_finished(
            &mut state,
            agent(),
            &edit_of("src/notes.md", ToolCallStatus::Done),
        );
        assert_eq!(freshness(&state).reread, Reread::Idle);

        state.ui.selected = Some(agent());
        let absolute = "/work/project-1/src/notes.md";
        item_finished(
            &mut state,
            agent(),
            &edit_of(absolute, ToolCallStatus::Done),
        );
        assert_eq!(freshness(&state).reread, Reread::Wanted);
    }

    #[test]
    fn coming_back_to_the_window_or_the_agent_reads_the_shown_file_again() {
        let mut state = showing("notes.md", "one\n");

        window_focused(&mut state, false);
        assert_eq!(freshness(&state).reread, Reread::Idle);
        window_focused(&mut state, true);
        assert_eq!(freshness(&state).reread, Reread::Wanted);

        let mut state = showing("notes.md", "one\n");
        state.ui.selected = None;
        actions::select_agent(&mut state, agent());
        assert_eq!(freshness(&state).reread, Reread::Wanted);
    }

    #[test]
    fn a_file_that_did_not_read_reads_from_the_start() {
        let mut state = showing("notes.md", "one\n");
        if let Some(held) = state
            .agents
            .get_mut(&agent())
            .and_then(|entry| entry.files.get_mut(Path::new("notes.md")))
        {
            *held = Loadable::Loaded(FileRead::Missing("No such file".to_owned()));
        }

        reread(&mut state, agent());

        assert_eq!(
            state.agents[&agent()].files[Path::new("notes.md")],
            Loadable::NotRequested
        );
    }
}
