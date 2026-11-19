//! `/reload-skills` and `/reload-plugins`. Claude Code runs headless here, so these commands
//! do nothing as prompts: the app sends the daemon's request instead, and a note says what the
//! reload found.

use cockpit_protocol::{
    AgentId, AgentParams, AgentStatus, COMMAND_PREFIX, CommandChanges, Request, ResponseBody,
};
use strum::{Display, EnumString, IntoStaticStr, VariantArray};

use super::app_state::AppState;
use super::requests::{Outgoing, Purpose};
use crate::format;
use crate::machine::MachineId;

/// The most command names a note lists before "and 3 more".
const NOTE_NAMES: usize = 5;

/// A reload, by the name the palette and the composer know it by.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Display, EnumString, IntoStaticStr, VariantArray,
)]
#[strum(ascii_case_insensitive)]
pub enum Reload {
    #[strum(serialize = "reload-skills")]
    Skills,
    #[strum(serialize = "reload-plugins")]
    Plugins,
}

impl Reload {
    /// The command's name, without its `/`.
    pub fn name(self) -> &'static str {
        self.into()
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Skills => "Read the skill folders again, to find new skills",
            Self::Plugins => "Read the plugins again, with their commands and skills",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Skills => "Reload skills",
            Self::Plugins => "Reload plugins",
        }
    }

    fn request(self, agent: AgentId) -> Request {
        let params = AgentParams { agent };
        match self {
            Self::Skills => Request::AgentReloadSkills(params),
            Self::Plugins => Request::AgentReloadPlugins(params),
        }
    }
}

/// The reload a message asks for: the command alone, as `/reload-skills`.
pub fn typed(text: &str) -> Option<Reload> {
    text.trim().strip_prefix(COMMAND_PREFIX)?.parse().ok()
}

/// The reload's request, or a note that says why an exited agent cannot reload.
pub fn start(state: &mut AppState, agent: AgentId, reload: Reload) -> Option<Outgoing> {
    let entry = state.agents.get(&agent)?;
    if entry.summary.status == AgentStatus::Exited {
        state.ui.report(format!(
            "{} needs the agent's Claude Code to run: resume the agent first.",
            reload.label()
        ));
        return None;
    }
    Some(Outgoing {
        request: reload.request(agent),
        purpose: Purpose::Reload { reload },
        to: MachineId::THIS,
    })
}

/// Shows what the reload found, or why it failed.
pub fn answered(state: &mut AppState, reload: Reload, answer: Result<&ResponseBody, &str>) {
    match answer.map(done_note) {
        Ok(Some(note)) => state.ui.inform(note),
        Ok(None) => state
            .ui
            .report(format!("{} gave an unexpected answer.", reload.label())),
        Err(message) => state
            .ui
            .report(format!("{} failed: {message}", reload.label())),
    }
}

/// "Reloaded skills: 14 skills. New: /ship-check."
fn done_note(body: &ResponseBody) -> Option<String> {
    let (counts, changes) = match body {
        ResponseBody::SkillsReloaded { skills, changes } => {
            let counts = format!(
                "Reloaded skills: {}",
                format::count(widen(*skills), "skill", "skills")
            );
            (counts, changes)
        }
        ResponseBody::PluginsReloaded {
            plugins,
            errors,
            changes,
        } => {
            let mut counts = format!(
                "Reloaded plugins: {}",
                format::count(widen(*plugins), "plugin", "plugins")
            );
            if *errors > 0 {
                counts = format!(
                    "{counts}, {}",
                    format::count(widen(*errors), "error", "errors")
                );
            }
            (counts, changes)
        }
        _ => return None,
    };
    let CommandChanges { added, removed } = changes;
    let mut note = format!("{counts}.");
    if !added.is_empty() {
        note = format!("{note} New: {}.", listed(added));
    }
    if !removed.is_empty() {
        note = format!("{note} Removed: {}.", listed(removed));
    }
    Some(note)
}

fn widen(count: u32) -> usize {
    usize::try_from(count).unwrap_or(usize::MAX)
}

/// "/a, /b and 2 more".
fn listed(names: &[String]) -> String {
    let shown: Vec<String> = names
        .iter()
        .take(NOTE_NAMES)
        .map(|name| format!("{COMMAND_PREFIX}{name}"))
        .collect();
    let shown = shown.join(", ");
    match names.len().saturating_sub(NOTE_NAMES) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

#[cfg(test)]
mod tests {
    use cockpit_client::RequestError;
    use cockpit_protocol::{ErrorBody, ErrorCode};

    use super::*;
    use crate::state::{Drafts, NoteKind, requests, state_with, summary};

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|&name| name.to_owned()).collect()
    }

    #[test]
    fn the_command_names_are_pinned() {
        let words: Vec<&str> = Reload::VARIANTS
            .iter()
            .map(|&reload| reload.name())
            .collect();

        assert_eq!(words, ["reload-skills", "reload-plugins"]);
    }

    #[test]
    fn only_the_command_alone_asks_for_a_reload() {
        assert_eq!(typed("/reload-skills"), Some(Reload::Skills));
        assert_eq!(typed("  /reload-plugins\n"), Some(Reload::Plugins));
        assert_eq!(typed("/Reload-Skills"), Some(Reload::Skills));
        assert_eq!(typed("/reload-skills now"), None);
        assert_eq!(typed("reload-skills"), None);
        assert_eq!(typed("/reload"), None);
    }

    #[test]
    fn a_note_counts_what_the_reload_found_and_names_what_changed() {
        let skills = ResponseBody::SkillsReloaded {
            skills: 14,
            changes: CommandChanges {
                added: names(&["ship-check"]),
                removed: Vec::new(),
            },
        };
        let one_skill = ResponseBody::SkillsReloaded {
            skills: 1,
            changes: CommandChanges::default(),
        };
        let plugins = ResponseBody::PluginsReloaded {
            plugins: 3,
            errors: 1,
            changes: CommandChanges {
                added: names(&["a", "b", "c", "d", "e", "f", "g"]),
                removed: names(&["old"]),
            },
        };
        let plugins_without_errors = ResponseBody::PluginsReloaded {
            plugins: 1,
            errors: 0,
            changes: CommandChanges::default(),
        };

        let notes =
            [skills, one_skill, plugins, plugins_without_errors].map(|body| done_note(&body));

        assert_eq!(
            notes,
            [
                Some("Reloaded skills: 14 skills. New: /ship-check.".to_owned()),
                Some("Reloaded skills: 1 skill.".to_owned()),
                Some(
                    "Reloaded plugins: 3 plugins, 1 error. New: /a, /b, /c, /d, /e and 2 more. Removed: /old."
                        .to_owned()
                ),
                Some("Reloaded plugins: 1 plugin.".to_owned()),
            ]
        );
        assert_eq!(done_note(&ResponseBody::Done), None);
    }

    #[test]
    fn the_answer_shows_as_an_info_note_and_a_failure_as_an_error() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let mut drafts = Drafts::default();
        let agent = AgentId::new(1);
        let reloaded = ResponseBody::SkillsReloaded {
            skills: 2,
            changes: CommandChanges::default(),
        };
        let refused = RequestError::Failed(ErrorBody::new(
            ErrorCode::AgentExited,
            "agent 1 is not running".to_owned(),
        ));
        let answers = [
            (Reload::Skills, Ok(reloaded)),
            (Reload::Plugins, Err(refused)),
        ];

        for (reload, answer) in answers {
            let Some(Outgoing {
                request, purpose, ..
            }) = start(&mut state, agent, reload)
            else {
                panic!("a running agent reloads");
            };
            requests::apply_response(
                &mut state,
                &mut drafts,
                request.method(),
                purpose,
                answer,
                MachineId::THIS,
            );
        }

        let notes: Vec<(&str, NoteKind)> = state
            .ui
            .errors
            .iter()
            .map(|note| (note.text.as_str(), note.kind))
            .collect();
        assert_eq!(
            notes,
            [
                ("Reloaded skills: 2 skills.", NoteKind::Info),
                (
                    "Reload plugins failed: agent 1 is not running",
                    NoteKind::Error
                ),
            ]
        );
    }
}
