//! Projects, which the sidebar can group agents by.
//!
//! An agent belongs to, in this order:
//!
//! 1. the project it was moved to by hand, from its row's menu or by a drag, if that project
//!    still exists;
//! 2. the first kept project with a rule that holds its folder: its repository, or a folder;
//! 3. a temporary project of its repository, which the app makes up and never saves: it goes
//!    away with its last agent;
//! 4. no project.
//!
//! A temporary project becomes a kept one when the user keeps it, or moves an agent into it.
//! Only then can it be renamed, colored, or given more rules.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use cockpit_protocol::{AgentId, normalized_repository};
use serde::{Deserialize, Deserializer};
use strum::VariantArray;

use super::app_state::{AgentEntry, AppState};
use super::grouping;
use crate::machine::MachineId;
use crate::path_style::PathStyle;
use crate::prefs::{AccountColor, Project, ProjectId, ProjectRule};

/// The words of a project key as the page names a group's header: `kept:3`, `repo:<remote>` and
/// `none`.
const KEPT_PREFIX: &str = "kept:";
const REPOSITORY_PREFIX: &str = "repo:";
const NONE_WORD: &str = "none";
/// What a folder rule ends with to hold every path that starts with the rest.
const WILDCARD: char = '*';

/// A project the sidebar groups agents by.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProjectKey {
    Kept(ProjectId),
    /// The temporary project of a repository no kept project claims.
    Repository(String),
    /// "No project".
    None,
}

impl fmt::Display for ProjectKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kept(id) => write!(formatter, "{KEPT_PREFIX}{}", id.get()),
            Self::Repository(repository) => write!(formatter, "{REPOSITORY_PREFIX}{repository}"),
            Self::None => formatter.write_str(NONE_WORD),
        }
    }
}

impl FromStr for ProjectKey {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text == NONE_WORD {
            return Ok(Self::None);
        }
        if let Some(repository) = text.strip_prefix(REPOSITORY_PREFIX) {
            return Ok(Self::Repository(repository.to_owned()));
        }
        text.strip_prefix(KEPT_PREFIX)
            .and_then(|id| id.parse().ok())
            .map(|id| Self::Kept(ProjectId::new(id)))
            .ok_or_else(|| format!("not a project: {text}"))
    }
}

/// The page sends a header's key as its text.
impl<'de> Deserialize<'de> for ProjectKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// The kind of rule the Projects tab's field adds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Repository,
    Folder,
}

impl RuleKind {
    pub fn rule(self, text: String) -> ProjectRule {
        match self {
            Self::Repository => ProjectRule::Repository(text),
            Self::Folder => ProjectRule::Folder(text),
        }
    }
}

/// How a project shows: its name, its color, and whether it is temporary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLook {
    pub name: String,
    pub color: Option<AccountColor>,
    pub temporary: bool,
}

pub fn project_of(state: &AppState, entry: &AgentEntry) -> ProjectKey {
    if let Some(chosen) = moved_to(state, entry.id()) {
        return chosen;
    }
    let summary = &entry.summary;
    let kept = state.prefs.projects.list.iter().find(|project| {
        project
            .rules
            .iter()
            .any(|rule| holds(rule, &summary.folder, summary.repository.as_deref()))
    });
    match (kept, &summary.repository) {
        (Some(project), _) => ProjectKey::Kept(project.id),
        (None, Some(repository)) => ProjectKey::Repository(repository.clone()),
        (None, None) => ProjectKey::None,
    }
}

/// The project an agent started in `folder` on `machine` joins by itself, when the app can tell
/// before it starts: from a kept project's folder rule, or from the repository of an agent
/// already in that folder. `None` when only the new agent's repository can tell.
pub fn for_new_agent(state: &AppState, machine: MachineId, folder: &Path) -> Option<ProjectKey> {
    let repository = state
        .agents
        .values()
        .filter(|entry| MachineId::of(entry.id()) == machine && entry.summary.folder == folder)
        .find_map(|entry| entry.summary.repository.as_deref());
    let kept = state.prefs.projects.list.iter().find(|project| {
        project
            .rules
            .iter()
            .any(|rule| holds(rule, folder, repository))
    });
    match (kept, repository) {
        (Some(project), _) => Some(ProjectKey::Kept(project.id)),
        (None, Some(repository)) => Some(ProjectKey::Repository(repository.to_owned())),
        (None, None) => None,
    }
}

/// The project the agent was moved to by hand, while that project exists.
fn moved_to(state: &AppState, agent: AgentId) -> Option<ProjectKey> {
    let key = grouping::agent_key(state, agent)?;
    let chosen = *state
        .prefs
        .projects
        .agents
        .get(&key.machine)?
        .get(&key.agent)?;
    match chosen {
        None => Some(ProjectKey::None),
        Some(id) => kept(state, id).map(|_| ProjectKey::Kept(id)),
    }
}

fn holds(rule: &ProjectRule, folder: &Path, repository: Option<&str>) -> bool {
    match rule {
        ProjectRule::Repository(wanted) => repository == Some(wanted.as_str()),
        ProjectRule::Folder(wanted) => {
            let text = folder.to_string_lossy();
            match wanted.strip_suffix(WILDCARD) {
                Some(start) => text.starts_with(start),
                None => PathStyle::of(folder).strip_prefix(&text, wanted).is_some(),
            }
        }
    }
}

pub fn kept(state: &AppState, id: ProjectId) -> Option<&Project> {
    state
        .prefs
        .projects
        .list
        .iter()
        .find(|project| project.id == id)
}

pub fn look(state: &AppState, key: &ProjectKey) -> ProjectLook {
    match key {
        ProjectKey::Kept(id) => kept(state, *id).map_or_else(
            || ProjectLook {
                name: String::new(),
                color: None,
                temporary: false,
            },
            |project| ProjectLook {
                name: project.name.clone(),
                color: Some(project.color),
                temporary: false,
            },
        ),
        ProjectKey::Repository(repository) => ProjectLook {
            name: repository_name(repository).to_owned(),
            color: Some(color_of(repository)),
            temporary: true,
        },
        ProjectKey::None => ProjectLook {
            name: "No project".to_owned(),
            color: None,
            temporary: false,
        },
    }
}

/// A repository's last name: `billing` for `github.com/acme/billing`.
pub fn repository_name(repository: &str) -> &str {
    repository
        .rsplit(['/', ':'])
        .find(|name| !name.is_empty())
        .unwrap_or(repository)
}

/// A temporary project's color, the same for a repository every time.
fn color_of(repository: &str) -> AccountColor {
    // FNV-1a: stable across runs and versions, unlike the standard hasher.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in repository.bytes() {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    let count = u32::try_from(AccountColor::VARIANTS.len()).unwrap_or(1);
    let index = usize::try_from(hash % count).unwrap_or(0);
    AccountColor::VARIANTS
        .get(index)
        .copied()
        .unwrap_or(AccountColor::Blue)
}

/// The projects the sidebar heads when it groups by project, in its order: the kept ones, the
/// temporary ones by name, then "No project". A project shows while it has an agent, whatever
/// the status filter hides.
pub fn shown_projects(state: &AppState) -> Vec<ProjectKey> {
    let mut used: Vec<ProjectKey> = Vec::new();
    for entry in state.agents.values() {
        let key = project_of(state, entry);
        if !used.contains(&key) {
            used.push(key);
        }
    }
    let mut keys: Vec<ProjectKey> = state
        .prefs
        .projects
        .list
        .iter()
        .map(|project| ProjectKey::Kept(project.id))
        .filter(|key| used.contains(key))
        .collect();
    let mut temporary: Vec<ProjectKey> = used
        .iter()
        .filter(|key| matches!(key, ProjectKey::Repository(_)))
        .cloned()
        .collect();
    temporary.sort_by_key(|key| look(state, key).name.to_lowercase());
    keys.extend(temporary);
    if used.contains(&ProjectKey::None) {
        keys.push(ProjectKey::None);
    }
    keys
}

/// The projects an agent can move to, as its row's menu lists them: every kept project in
/// its order, the temporary ones by name, then "No project".
pub fn move_targets(state: &AppState) -> Vec<ProjectKey> {
    let mut targets: Vec<ProjectKey> = state
        .prefs
        .projects
        .list
        .iter()
        .map(|project| ProjectKey::Kept(project.id))
        .collect();
    targets.extend(
        shown_projects(state)
            .into_iter()
            .filter(|key| matches!(key, ProjectKey::Repository(_))),
    );
    targets.push(ProjectKey::None);
    targets
}

/// A new kept project for the agent, named after its repository or its folder, with the agent
/// moved into it.
pub fn create_for(state: &mut AppState, agent: AgentId) -> Option<ProjectId> {
    let entry = state.agents.get(&agent)?;
    let name = match &entry.summary.repository {
        Some(repository) => repository_name(repository).to_owned(),
        None => {
            let folder = entry.summary.folder.to_string_lossy();
            PathStyle::of(&entry.summary.folder)
                .file_name(&folder)
                .unwrap_or("New project")
                .to_owned()
        }
    };
    let id = create(state, &name);
    move_agent(state, agent, &ProjectKey::Kept(id));
    Some(id)
}

/// Makes the temporary project of `repository` a kept one, at the end of the order, or finds
/// the kept one that already claims it.
pub fn keep(state: &mut AppState, repository: &str) -> ProjectId {
    let claimed = state.prefs.projects.list.iter().find(|project| {
        project
            .rules
            .contains(&ProjectRule::Repository(repository.to_owned()))
    });
    if let Some(project) = claimed {
        return project.id;
    }
    let name = repository_name(repository).to_owned();
    let color = color_of(repository);
    add(
        state,
        name,
        color,
        vec![ProjectRule::Repository(repository.to_owned())],
    )
}

/// A new kept project, with no rule: its agents are the ones moved to it.
pub fn create(state: &mut AppState, name: &str) -> ProjectId {
    let used: Vec<AccountColor> = state
        .prefs
        .projects
        .list
        .iter()
        .map(|project| project.color)
        .collect();
    let color = AccountColor::VARIANTS
        .iter()
        .copied()
        .find(|color| !used.contains(color))
        .unwrap_or(AccountColor::Blue);
    add(state, name.to_owned(), color, Vec::new())
}

fn add(
    state: &mut AppState,
    name: String,
    color: AccountColor,
    rules: Vec<ProjectRule>,
) -> ProjectId {
    let projects = &mut state.prefs.projects;
    let id = ProjectId::new(projects.next_id);
    projects.next_id = projects.next_id.wrapping_add(1);
    projects.list.push(Project {
        id,
        name,
        color,
        rules,
    });
    id
}

/// The kept project behind `key`: a temporary one is kept first. `None` for "No project".
pub fn kept_id(state: &mut AppState, key: &ProjectKey) -> Option<ProjectId> {
    match key {
        ProjectKey::Kept(id) => Some(*id),
        ProjectKey::Repository(repository) => Some(keep(state, repository)),
        ProjectKey::None => None,
    }
}

/// Moves the agent to `target`, which keeps a temporary project. When the rules would put the
/// agent there anyway, no choice is kept, so that a later rule change moves it again.
pub fn move_agent(state: &mut AppState, agent: AgentId, target: &ProjectKey) {
    forget_agent(state, agent);
    let Some(key) = grouping::agent_key(state, agent) else {
        return;
    };
    let chosen = kept_id(state, target);
    let wanted = chosen.map_or(ProjectKey::None, ProjectKey::Kept);
    let by_rules = state
        .agents
        .get(&agent)
        .map(|entry| project_of(state, entry));
    if by_rules.as_ref() != Some(&wanted) {
        state
            .prefs
            .projects
            .agents
            .entry(key.machine)
            .or_default()
            .insert(key.agent, chosen);
    }
}

/// Renames a kept project. An empty name changes nothing.
pub fn rename(state: &mut AppState, id: ProjectId, name: &str) {
    let name = name.trim();
    if name.is_empty() {
        return;
    }
    if let Some(project) = kept_mut(state, id) {
        name.clone_into(&mut project.name);
    }
}

pub fn set_color(state: &mut AppState, id: ProjectId, color: AccountColor) {
    if let Some(project) = kept_mut(state, id) {
        project.color = color;
    }
}

/// Adds a rule from what the user typed: a remote in any form, or a folder's path. Empty text
/// and a rule the project has already add nothing.
pub fn add_rule(state: &mut AppState, id: ProjectId, rule: ProjectRule) {
    let rule = match rule {
        ProjectRule::Repository(text) => ProjectRule::Repository(normalized_repository(&text)),
        ProjectRule::Folder(text) => ProjectRule::Folder(text.trim().to_owned()),
    };
    let empty = match &rule {
        ProjectRule::Repository(text) | ProjectRule::Folder(text) => text.is_empty(),
    };
    if empty {
        return;
    }
    if let Some(project) = kept_mut(state, id)
        && !project.rules.contains(&rule)
    {
        project.rules.push(rule);
    }
}

pub fn remove_rule(state: &mut AppState, id: ProjectId, index: usize) {
    if let Some(project) = kept_mut(state, id)
        && index < project.rules.len()
    {
        project.rules.remove(index);
    }
}

/// Deletes a kept project. Its agents go where the rules put them now, and the ones moved to it
/// by hand lose that choice.
pub fn delete(state: &mut AppState, id: ProjectId) {
    let projects = &mut state.prefs.projects;
    projects.list.retain(|project| project.id != id);
    for agents in projects.agents.values_mut() {
        agents.retain(|_, chosen| *chosen != Some(id));
    }
    projects.agents.retain(|_, agents| !agents.is_empty());
}

/// Moves a kept project just above `before`, or to the end of the kept ones.
pub fn reorder(state: &mut AppState, id: ProjectId, before: Option<ProjectId>) {
    let list = &mut state.prefs.projects.list;
    let Some(from) = list.iter().position(|project| project.id == id) else {
        return;
    };
    let project = list.remove(from);
    let at = before
        .and_then(|before| list.iter().position(|project| project.id == before))
        .unwrap_or(list.len());
    list.insert(at, project);
}

/// Drops the choice of an agent the daemon removed, whose id a later agent may take.
pub fn forget_agent(state: &mut AppState, agent: AgentId) {
    let Some(key) = grouping::agent_key(state, agent) else {
        return;
    };
    let projects = &mut state.prefs.projects;
    if let Some(agents) = projects.agents.get_mut(&key.machine) {
        agents.remove(&key.agent);
        if agents.is_empty() {
            projects.agents.remove(&key.machine);
        }
    }
}

fn kept_mut(state: &mut AppState, id: ProjectId) -> Option<&mut Project> {
    state
        .prefs
        .projects
        .list
        .iter_mut()
        .find(|project| project.id == id)
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{AgentStatus, Method, ResponseBody};

    use super::*;
    use crate::prefs::GroupBy;
    use crate::state::actions;
    use crate::state::app_state::tests::{state_with, summary};
    use crate::state::drafts::Drafts;
    use crate::state::requests::{self, Purpose};

    const BILLING: &str = "github.com/acme/billing";
    const GATEWAY: &str = "github.com/acme/api-gateway";

    /// Agents 1 and 2 in the billing repository, 3 in the gateway's, 4 outside git.
    fn agents() -> AppState {
        let mut state = state_with(
            [
                (1, Some(BILLING)),
                (2, Some(BILLING)),
                (3, Some(GATEWAY)),
                (4, None),
            ]
            .into_iter()
            .map(|(id, repository)| {
                let mut agent = summary(id, AgentStatus::Idle);
                agent.repository = repository.map(str::to_owned);
                agent
            })
            .collect(),
        );
        state.prefs.sidebar.group_by = GroupBy::Project;
        state
    }

    fn project(state: &AppState, id: u32) -> ProjectKey {
        project_of(state, &state.agents[&AgentId::new(id)])
    }

    fn order(state: &AppState) -> Vec<AgentId> {
        state
            .sidebar_order()
            .iter()
            .map(|entry| entry.id())
            .collect()
    }

    /// Whether `upper` sits just above `lower` in the sidebar's order.
    fn just_above(state: &AppState, upper: u32, lower: u32) -> bool {
        let order = order(state);
        let at = |id| order.iter().position(|agent| *agent == AgentId::new(id));
        at(upper).map(|at| at + 1) == at(lower)
    }

    /// The app's order puts the gateway's agent 3 between the billing agents 1 and 2. On
    /// screen, the gateway's group comes first: 3, then 1 and 2, then 4.
    fn interleaved() -> AppState {
        let mut state = agents();
        state.prefs.agent_order = [1, 3, 2, 4]
            .into_iter()
            .filter_map(|id| grouping::agent_key(&state, AgentId::new(id)))
            .collect();
        state
    }

    fn shown(state: &AppState) -> Vec<AgentId> {
        state
            .sidebar_groups()
            .iter()
            .flat_map(|group| group.rows.iter().map(|entry| entry.id()))
            .collect()
    }

    #[test]
    fn alt_up_moves_the_agent_above_the_row_above_it_on_screen() {
        let mut state = interleaved();
        state.ui.selected = Some(AgentId::new(2));

        actions::move_selected(&mut state, actions::Direction::Up);

        assert_eq!(shown(&state), [3, 2, 1, 4].map(AgentId::new));
    }

    #[test]
    fn closing_an_agent_selects_the_row_below_it_on_screen() {
        let mut state = interleaved();
        state.ui.selected = Some(AgentId::new(1));

        actions::close_agent(&mut state, &mut Drafts::default(), AgentId::new(1));

        assert_eq!(state.ui.selected, Some(AgentId::new(2)));
    }

    #[test]
    fn a_drop_above_a_row_of_another_project_moves_the_agent_into_it() {
        let mut state = agents();

        actions::drop_agent(&mut state, AgentId::new(4), Some(AgentId::new(1)), None);

        let billing = ProjectKey::Kept(state.prefs.projects.list[0].id);
        assert_eq!(project(&state, 4), billing);
        assert!(just_above(&state, 4, 1));
    }

    #[test]
    fn a_drop_at_a_group_end_moves_the_agent_to_its_bottom() {
        let mut state = agents();

        actions::drop_agent(&mut state, AgentId::new(1), None, Some(&ProjectKey::None));

        assert_eq!(project(&state, 1), ProjectKey::None);
        assert!(just_above(&state, 4, 1));
    }

    #[test]
    fn a_drop_in_the_same_group_keeps_no_choice() {
        let mut state = agents();

        actions::drop_agent(&mut state, AgentId::new(2), Some(AgentId::new(1)), None);

        assert!(state.prefs.projects.agents.is_empty());
        assert!(state.prefs.projects.list.is_empty());
        assert!(just_above(&state, 2, 1));
    }

    #[test]
    fn a_new_agent_s_project_comes_from_the_folder_s_rules_or_its_agents() {
        let mut state = agents();
        let work = create(&mut state, "Work");
        add_rule(
            &mut state,
            work,
            ProjectRule::Folder("/scratch/*".to_owned()),
        );
        let guess = |state: &AppState, folder: &str| {
            for_new_agent(state, MachineId::THIS, Path::new(folder))
        };

        assert_eq!(
            guess(&state, "/work/project-1"),
            Some(ProjectKey::Repository(BILLING.to_owned()))
        );
        assert_eq!(
            guess(&state, "/scratch/notes"),
            Some(ProjectKey::Kept(work))
        );
        assert_eq!(guess(&state, "/elsewhere"), None);
        assert_eq!(
            for_new_agent(&state, MachineId::new(1), Path::new("/work/project-1")),
            None
        );
    }

    #[test]
    fn a_started_agent_joins_the_project_picked_in_the_dialog() {
        let mut state = agents();
        let started = AgentId::new(9);

        requests::apply_response(
            &mut state,
            &mut Drafts::default(),
            Method::AgentStart,
            Purpose::Start {
                project: Some(ProjectKey::Repository(GATEWAY.to_owned())),
            },
            Ok(ResponseBody::AgentStarted { agent: started }),
            MachineId::THIS,
        );
        let mut agent = summary(9, AgentStatus::Idle);
        agent.repository = Some(BILLING.to_owned());
        state.agents.insert(started, AgentEntry::new(agent));

        let gateway = keep(&mut state, GATEWAY);
        assert_eq!(project(&state, 9), ProjectKey::Kept(gateway));
    }

    #[test]
    fn without_kept_projects_each_repository_makes_a_temporary_one() {
        let state = agents();

        assert_eq!(
            project(&state, 1),
            ProjectKey::Repository(BILLING.to_owned())
        );
        assert_eq!(
            project(&state, 3),
            ProjectKey::Repository(GATEWAY.to_owned())
        );
        assert_eq!(project(&state, 4), ProjectKey::None);
        assert_eq!(
            shown_projects(&state),
            [
                ProjectKey::Repository(GATEWAY.to_owned()),
                ProjectKey::Repository(BILLING.to_owned()),
                ProjectKey::None,
            ]
        );
        let look = look(&state, &ProjectKey::Repository(BILLING.to_owned()));
        assert_eq!((look.name.as_str(), look.temporary), ("billing", true));
    }

    #[test]
    fn keeping_a_temporary_project_saves_it_with_its_repository() {
        let mut state = agents();

        let id = keep(&mut state, BILLING);
        let again = keep(&mut state, BILLING);

        assert_eq!(id, again);
        assert_eq!(project(&state, 2), ProjectKey::Kept(id));
        assert_eq!(state.prefs.projects.list.len(), 1);
        assert_eq!(shown_projects(&state).first(), Some(&ProjectKey::Kept(id)));
    }

    #[test]
    fn a_folder_rule_holds_its_folder_and_a_wildcard_its_start() {
        let mut state = agents();
        let id = create(&mut state, "Work");
        add_rule(
            &mut state,
            id,
            ProjectRule::Folder("/work/project-4".to_owned()),
        );
        let other = create(&mut state, "Wildcard");
        add_rule(
            &mut state,
            other,
            ProjectRule::Folder("/work/project-*".to_owned()),
        );

        assert_eq!(project(&state, 4), ProjectKey::Kept(id));
        assert_eq!(project(&state, 3), ProjectKey::Kept(other));
    }

    #[test]
    fn a_rule_typed_in_any_form_matches_the_normalized_repository() {
        let mut state = agents();
        let id = create(&mut state, "Gateway");

        add_rule(
            &mut state,
            id,
            ProjectRule::Repository("git@github.com:acme/api-gateway.git".to_owned()),
        );

        assert_eq!(project(&state, 3), ProjectKey::Kept(id));
    }

    #[test]
    fn a_moved_agent_stays_until_its_project_goes_and_a_move_back_leaves_no_choice() {
        let mut state = agents();
        let id = create(&mut state, "Elsewhere");

        move_agent(&mut state, AgentId::new(1), &ProjectKey::Kept(id));
        let moved = project(&state, 1);
        move_agent(
            &mut state,
            AgentId::new(1),
            &ProjectKey::Repository(BILLING.to_owned()),
        );
        let back = project(&state, 1);
        move_agent(&mut state, AgentId::new(2), &ProjectKey::Kept(id));
        delete(&mut state, id);

        assert_eq!(moved, ProjectKey::Kept(id));
        // Moving into the temporary project kept it, and the rules put the agent there.
        assert!(matches!(back, ProjectKey::Kept(_)));
        assert!(state.prefs.projects.agents.is_empty());
        assert!(matches!(project(&state, 2), ProjectKey::Kept(_)));
    }

    #[test]
    fn a_moved_agent_can_leave_every_project() {
        let mut state = agents();

        move_agent(&mut state, AgentId::new(1), &ProjectKey::None);

        assert_eq!(project(&state, 1), ProjectKey::None);
    }

    #[test]
    fn projects_reorder_and_a_new_one_never_takes_a_deleted_ones_id() {
        let mut state = agents();
        let first = create(&mut state, "First");
        let second = create(&mut state, "Second");

        reorder(&mut state, second, Some(first));
        delete(&mut state, first);
        let third = create(&mut state, "Third");

        let ids: Vec<ProjectId> = state
            .prefs
            .projects
            .list
            .iter()
            .map(|project| project.id)
            .collect();
        assert_eq!(ids, [second, third]);
        assert_ne!(third, first);
    }

    #[test]
    fn a_header_key_reads_back_from_its_text() {
        for key in [
            ProjectKey::Kept(ProjectId::new(3)),
            ProjectKey::Repository(BILLING.to_owned()),
            ProjectKey::None,
        ] {
            assert_eq!(key.to_string().parse::<ProjectKey>(), Ok(key));
        }
        assert!("kept:x".parse::<ProjectKey>().is_err());
    }

    #[test]
    fn a_temporary_project_keeps_its_color() {
        assert_eq!(color_of(BILLING), color_of(BILLING));
        assert_eq!(repository_name("/srv/git/billing"), "billing");
    }
}
