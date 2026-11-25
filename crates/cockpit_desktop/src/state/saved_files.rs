//! Each agent's files and docks, kept in the preferences so that they open again after a restart:
//! the shown file and its mode, the files opened besides the changed ones, the changed files the
//! user closed, the dock each terminal sits in, and the order and the pins of the tabs.

use std::collections::BTreeSet;

use cockpit_protocol::AgentId;

use super::app_state::AppState;
use super::ui_state::FileView;
use crate::machine::MachineId;
use crate::prefs::SavedFiles;

/// Writes each agent's files into the preferences, where they differ.
pub fn save(state: &mut AppState) {
    let mut changes = Vec::new();
    for &agent in state.agents.keys() {
        let Some(machine) = state.daemon_machine(agent) else {
            continue;
        };
        let files = saved_now(state, agent);
        let kept = state
            .prefs
            .file_view
            .agent(machine, MachineId::daemon_agent(agent));
        let differs = match kept {
            Some(kept) => *kept != files,
            None => !files.is_empty(),
        };
        if differs {
            changes.push((machine.to_owned(), agent, files));
        }
    }
    for (machine, agent, files) in changes {
        state
            .prefs
            .file_view
            .set_agent(&machine, MachineId::daemon_agent(agent), files);
    }
}

fn saved_now(state: &AppState, agent: AgentId) -> SavedFiles {
    let layout = state.ui.layouts.get(&agent).cloned().unwrap_or_default();
    match state.ui.file_view(agent) {
        Some(view) => SavedFiles {
            shown: Some(view.path.clone()),
            mode: view.mode,
            opened: view.opened.clone(),
            closed: view.closed.clone(),
            layout,
        },
        None => SavedFiles {
            layout,
            closed: state
                .ui
                .closed_files
                .get(&agent)
                .cloned()
                .unwrap_or_default(),
            ..SavedFiles::default()
        },
    }
}

/// Opens the files the preferences keep for an agent the app has just listed, unless its view is
/// open already.
pub fn restore(state: &mut AppState, agent: AgentId) {
    let Some(saved) = state.daemon_machine(agent).and_then(|machine| {
        state
            .prefs
            .file_view
            .agent(machine, MachineId::daemon_agent(agent))
            .cloned()
    }) else {
        return;
    };
    if !state.ui.layouts.contains_key(&agent) && !saved.layout.is_empty() {
        state.ui.layouts.insert(agent, saved.layout);
    }
    if state.ui.file_views.contains_key(&agent) {
        return;
    }
    let Some(path) = saved.shown else {
        state.ui.closed_files.insert(agent, saved.closed);
        return;
    };
    let mut view = FileView::new(path, None, saved.mode);
    view.opened = saved.opened;
    view.closed = saved.closed;
    view.mode_settled = true;
    state.ui.file_views.insert(agent, view);
}

/// Keeps the files of the agents a snapshot of `machine` holds: the others closed while the app
/// was away.
pub fn keep_listed(state: &mut AppState, machine: MachineId) {
    let Some(name) = state
        .machines
        .get(machine)
        .map(|known| known.daemon.machine.clone())
    else {
        return;
    };
    let listed: BTreeSet<AgentId> = state
        .agents
        .keys()
        .filter(|&&agent| MachineId::of(agent) == machine)
        .map(|&agent| MachineId::daemon_agent(agent))
        .collect();
    state
        .prefs
        .file_view
        .keep_agents(&name, |agent| listed.contains(&agent));
}

/// Drops the files of a machine's agents before the app forgets the machine.
pub fn forget_machine(state: &mut AppState, machine: MachineId) {
    if let Some(known) = state.machines.get(machine) {
        let name = known.daemon.machine.clone();
        state.prefs.file_view.forget_machine(&name);
    }
}

/// Drops the files of an agent the daemon removed.
pub fn forget(state: &mut AppState, agent: AgentId) {
    state.ui.closed_files.remove(&agent);
    state.ui.layouts.remove(&agent);
    let Some(machine) = state.daemon_machine(agent).map(str::to_owned) else {
        return;
    };
    state
        .prefs
        .file_view
        .forget_agent(&machine, MachineId::daemon_agent(agent));
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use cockpit_protocol::{AgentDetails, AgentStatus, ChangedFile};

    use super::*;
    use crate::state::{Loadable, ViewMode, actions, state_with, summary};

    const AGENT: u32 = 1;

    fn agent() -> AgentId {
        AgentId::new(AGENT)
    }

    /// An agent that changed src/cart.rs and src/tax.rs.
    fn state() -> AppState {
        let mut state = state_with(vec![summary(AGENT, AgentStatus::Idle)]);
        state.ui.selected = Some(agent());
        let entry = state.agents.get_mut(&agent()).unwrap();
        entry.summary.folder = PathBuf::from("/work/shop");
        let changed = |path: &str| ChangedFile {
            path: PathBuf::from(path),
            lines_added: 1,
            lines_removed: 0,
        };
        entry.details = Loadable::Loaded(AgentDetails {
            changed_files: vec![
                changed("/work/shop/src/cart.rs"),
                changed("/work/shop/src/tax.rs"),
            ],
            ..AgentDetails::default()
        });
        state
    }

    /// The app starts again with the preferences `state` saved.
    fn restarted(state: &mut AppState) -> AppState {
        save(state);
        let mut restarted = self::state();
        restarted.prefs = state.prefs.clone();
        restore(&mut restarted, agent());
        restarted
    }

    fn tabs(state: &AppState) -> Vec<PathBuf> {
        let changed = state.agents[&agent()].changed_paths();
        state
            .ui
            .file_view(agent())
            .map(|view| view.tabs(&changed))
            .unwrap_or_default()
    }

    #[test]
    fn the_files_open_again_as_they_were_after_a_restart() {
        let mut state = state();
        actions::open_file(&mut state, agent(), PathBuf::from("src/cart.rs"), None);
        actions::open_file(&mut state, agent(), PathBuf::from("notes.md"), None);
        actions::set_view_mode(&mut state, agent(), ViewMode::WholeFile);
        actions::close_tab(&mut state, agent(), Path::new("src/tax.rs"));

        let restarted = restarted(&mut state);

        let view = restarted.ui.file_view(agent()).unwrap();
        assert_eq!(view.path, PathBuf::from("notes.md"));
        assert_eq!(view.mode, ViewMode::WholeFile);
        assert!(view.mode_settled);
        assert_eq!(tabs(&restarted), tabs(&state));
        assert_eq!(
            tabs(&restarted),
            [PathBuf::from("src/cart.rs"), PathBuf::from("notes.md")]
        );
    }

    #[test]
    fn closing_the_view_keeps_the_changed_files_closed() {
        let mut state = state();
        actions::open_file(&mut state, agent(), PathBuf::from("src/cart.rs"), None);

        actions::close_file_view(&mut state, agent());
        actions::open_file(&mut state, agent(), PathBuf::from("notes.md"), None);

        assert_eq!(tabs(&state), [PathBuf::from("notes.md")]);
    }

    #[test]
    fn a_closed_view_stays_closed_and_its_files_too_after_a_restart() {
        let mut state = state();
        actions::open_file(&mut state, agent(), PathBuf::from("src/cart.rs"), None);
        actions::close_file_view(&mut state, agent());

        let mut restarted = restarted(&mut state);
        let reopened = restarted.ui.file_view(agent()).is_some();
        actions::open_file(&mut restarted, agent(), PathBuf::from("notes.md"), None);

        assert!(!reopened);
        assert_eq!(tabs(&restarted), [PathBuf::from("notes.md")]);
    }

    #[test]
    fn a_close_that_fails_keeps_the_agents_files() {
        use cockpit_client::RequestError;

        use crate::machine::MachineId;
        use crate::state::{Drafts, apply_response};

        let mut state = state();
        actions::open_file(&mut state, agent(), PathBuf::from("notes.md"), None);
        save(&mut state);

        let closing = actions::close_agent(&mut state, &mut Drafts::default(), agent());
        save(&mut state);
        for outgoing in closing {
            let method = outgoing.request.method();
            apply_response(
                &mut state,
                &mut Drafts::default(),
                method,
                outgoing.purpose,
                Err(RequestError::Disconnected),
                MachineId::THIS,
            );
        }
        save(&mut state);

        let shown = state.ui.file_view(agent()).map(|view| view.path.clone());
        assert_eq!(shown, Some(PathBuf::from("notes.md")));
        assert!(!state.prefs.file_view.agents.is_empty());
    }

    #[test]
    fn an_agent_with_nothing_to_keep_leaves_no_entry() {
        let mut state = state();
        state.agents.get_mut(&agent()).unwrap().details = Loadable::Loaded(AgentDetails::default());
        actions::open_file(&mut state, agent(), PathBuf::from("notes.md"), None);
        save(&mut state);
        actions::close_tab(&mut state, agent(), Path::new("notes.md"));

        save(&mut state);

        assert!(state.prefs.file_view.agents.is_empty());
    }
}
