//! The agent list: a status filter, compact two-line rows in the order the user sets, and the
//! connection's state.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime};

use cockpit_protocol::{AgentId, AgentStatus, AgentSummary, MAX_TITLE_CHARS, Mark, Timestamp};
use dioxus::prelude::*;

use crate::bridge::{self, ROW_TITLE_FIELD_ID};
use crate::connection::{Clock, Cockpit};
use crate::daemon_label::DaemonPlace;
use crate::keymap::{Command, Keymap};
use crate::machine::MachineId;
use crate::prefs::{AccountColor, NotificationLevel, StatusFilter};
use crate::state::accounts::{self, AccountLabel, SignInLine};
use crate::state::grouping::GroupKey;
use crate::state::no_daemon::Absence;
use crate::state::projects::ProjectKey;
use crate::state::sidebar_keys::SidebarItem;
use crate::state::{
    AgentEntry, AppState, ConnectionState, Dialog, TitleEdit, TitlePlace, actions, daemon_update,
    grouping, machines, notification_levels, projects, settings, to_handle,
};
use crate::ui::accounts::{AccountLetter, AccountsButton};
use crate::ui::grouping::GroupSwitch;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::machines::THIS_COMPUTER;
use crate::ui::notification_levels::{self as levels_ui, AppBell, LevelIcon};
use crate::ui::presence::{Shown, use_presence};
use crate::ui::projects_tab;
use crate::ui::reorder::{
    self, GROUP_END_CLASS, GROUP_HEAD_CLASS, KEPT_GROUP_ID_PREFIX, LIST_ID, PROJECT_DRAG_CLASS,
    ROW_CLASS,
};
use crate::ui::splitter::SIDEBAR_ID;
use crate::{APP_NAME, format};

#[derive(Debug, Clone, PartialEq)]
struct AgentRow {
    id: AgentId,
    title: String,
    status_class: String,
    activity: String,
    pending: u16,
    unread: bool,
    selected: bool,
    /// "3/5".
    tasks: Option<String>,
    time: Option<RowTime>,
    /// The title edit, while the agent is renamed in this row.
    edit: Option<TitleEdit>,
    /// The account, with two accounts or more.
    account: Option<AccountLabel>,
    /// The activity says the account is not signed in.
    warns: bool,
    /// You marked the agent as one to handle.
    to_handle: bool,
    /// The machine the agent runs on, when nothing above the row says it.
    machine: Option<RowMachine>,
    notifications: NotificationLevel,
    /// The shortcut that closes the selected agent, for the close button's tooltip.
    close_keys: String,
}

impl AgentRow {
    fn new(state: &AppState, entry: &AgentEntry, edit: Option<TitleEdit>) -> Self {
        let summary = &entry.summary;
        let sign_in = accounts::sign_in_line(state, entry);
        Self {
            id: summary.id,
            title: summary.title.clone(),
            status_class: format::agent_status_class(summary),
            warns: matches!(sign_in, Some(SignInLine::NotSignedIn(_))),
            activity: sign_in.map_or_else(|| activity_line(entry), |line| line.text().to_owned()),
            pending: summary.pending_count,
            unread: state.is_unread(entry.id()),
            selected: state.ui.selected == Some(entry.id()),
            account: accounts::label(state, MachineId::of(entry.id()), summary.account),
            tasks: summary
                .task_progress
                .map(|progress| format!("{}/{}", progress.done, progress.total)),
            time: row_time(summary),
            to_handle: summary.mark == Mark::ToHandle,
            machine: grouping::machine_shown_on_row(state, entry.id())
                .and_then(|machine| RowMachine::new(state, machine)),
            notifications: notification_levels::agent_level(state, entry.id()),
            close_keys: state.prefs.keys.shown(Command::CloseAgent),
            edit,
        }
    }
}

/// A machine on a row or a project's header: its short tag, the whole name on hover.
#[derive(Debug, Clone, PartialEq)]
struct RowMachine {
    name: String,
    tag: String,
}

impl RowMachine {
    fn new(state: &AppState, machine: MachineId) -> Option<Self> {
        Some(Self {
            name: state.machines.get(machine)?.daemon.machine.clone(),
            tag: grouping::machine_tag(state, machine)?,
        })
    }
}

/// What the time next to a row's title counts, from the daemon's clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowTime {
    /// How long the running turn has run.
    Turn(Timestamp),
    /// How long the agent has waited for you.
    Waiting(Timestamp),
    /// How long ago the agent last did something.
    LastActive(Timestamp),
}

/// `None` from a daemon that does not send the times.
fn row_time(summary: &AgentSummary) -> Option<RowTime> {
    match summary.status {
        AgentStatus::Working => summary.turn_started_at.map(RowTime::Turn),
        AgentStatus::NeedsYou => summary.last_activity_at.map(RowTime::Waiting),
        AgentStatus::Idle | AgentStatus::Exited => {
            summary.last_activity_at.map(RowTime::LastActive)
        }
    }
}

/// The id of a machine header, which the arrow keys give the focus to.
fn head_id(machine: MachineId) -> String {
    format!("machine-head-{}", machine.get())
}

/// The id of the sidebar element the arrow keys give the focus to.
pub fn item_id(item: SidebarItem) -> String {
    match item {
        SidebarItem::Agent(agent) => reorder::row_id(agent),
        SidebarItem::Machine(machine) => head_id(machine),
        SidebarItem::Project(project) => project_head_id(&project),
    }
}

/// An idle row says "now" for this long after the agent's last activity.
const JUST_NOW: Duration = Duration::from_secs(60);

/// The short text next to the title, and its tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RowTimeText {
    text: String,
    tooltip: String,
}

/// An idle row counts in minutes: a count of seconds ticking beside a quiet agent distracts.
fn row_time_text(time: RowTime, now: SystemTime) -> RowTimeText {
    if let RowTime::LastActive(moment) = time
        && format::since(moment, now) < JUST_NOW
    {
        return RowTimeText {
            text: "now".to_owned(),
            tooltip: "Last active less than a minute ago".to_owned(),
        };
    }
    let (moment, describe): (Timestamp, fn(&str) -> String) = match time {
        RowTime::Turn(moment) => (moment, |text| format!("Turn running: {text}")),
        RowTime::Waiting(moment) => (moment, |text| format!("Waiting for you: {text}")),
        RowTime::LastActive(moment) => (moment, |text| format!("Last active {text} ago")),
    };
    let text = format::elapsed(format::since(moment, now));
    let tooltip = describe(&text);
    RowTimeText { text, tooltip }
}

/// What the agent does now, or what its status says when it reports nothing. An idle agent with
/// nothing running says nothing: its hollow dot shows it is idle.
pub fn activity_line(entry: &AgentEntry) -> String {
    let summary = &entry.summary;
    if let Some(activity) = &summary.activity {
        return activity.clone();
    }
    match summary.status {
        AgentStatus::Exited => "Process ended".to_owned(),
        AgentStatus::Idle if summary.background_running == 0 => String::new(),
        _ => format::agent_status_label(summary),
    }
}

/// A chip of the status filter.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FilterChip {
    filter: StatusFilter,
    count: usize,
    chosen: bool,
}

fn filter_label(filter: StatusFilter) -> &'static str {
    match filter {
        StatusFilter::ToHandle => TO_HANDLE,
        _ => filter.status().map_or("All", format::status_label),
    }
}

/// The class that gives a chip its color.
fn chip_class(filter: StatusFilter) -> &'static str {
    match filter {
        StatusFilter::ToHandle => TO_HANDLE_CLASS,
        _ => filter.status().map_or("", format::status_class),
    }
}

/// The mark's name, in the filter and the row's tooltip.
const TO_HANDLE: &str = "To handle";

const TO_HANDLE_CLASS: &str = "to-handle";

/// The bell of an agent whose level is not "All". Always there, so that the style sheet can grow
/// it in and shrink it out, and it keeps its last bell while it shrinks.
#[component]
fn RowBell(level: NotificationLevel) -> Element {
    let shown = use_presence((level != NotificationLevel::All).then_some(level));
    let bell = shown.map_or(NotificationLevel::Off, |shown| shown.value);
    let on = level != NotificationLevel::All;
    rsx! {
        span {
            class: if on { "row-bell on" } else { "row-bell" },
            title: if on { levels_ui::agent_label(level) } else { "" },
            aria_hidden: !on,
            LevelIcon { level: bell, size: 12 }
            if on {
                span { class: "visually-hidden", "{levels_ui::agent_label(level)}" }
            }
        }
    }
}

/// What the list says when the filter leaves no row.
fn empty_list_text(filter: StatusFilter, keys: &Keymap) -> String {
    match filter {
        StatusFilter::All => format!(
            "No agent yet. {} starts one.",
            keys.shown(Command::NewAgent)
        ),
        StatusFilter::NeedsYou => "No agent needs you.".to_owned(),
        StatusFilter::Working => "No agent is working.".to_owned(),
        StatusFilter::Idle => "No agent is idle.".to_owned(),
        StatusFilter::Exited => "No agent has exited.".to_owned(),
        StatusFilter::ToHandle => format!(
            "No agent is marked to handle. {} marks the selected one.",
            keys.shown(Command::MarkToHandle)
        ),
    }
}

#[component]
pub fn Sidebar() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let filter = state.prefs.sidebar.filter;
    let chips: Vec<FilterChip> = state
        .filter_chips()
        .into_iter()
        .map(|(chip, count)| FilterChip {
            filter: chip,
            count,
            chosen: chip == filter,
        })
        .collect();
    let selected_hidden = state.selected_is_hidden();
    let find_title = format!(
        "Find an agent ({})",
        state.prefs.keys.shown(Command::Switcher)
    );
    let new_title = format!("New agent ({})", state.prefs.keys.shown(Command::NewAgent));
    let row = |entry: &AgentEntry| {
        let edit = state
            .ui
            .title_edit
            .as_ref()
            .filter(|edit| edit.agent == entry.id() && edit.place == TitlePlace::Sidebar)
            .cloned();
        AgentRow::new(&state, entry, edit)
    };
    let groups: Vec<Panel> = state
        .sidebar_groups()
        .into_iter()
        .map(|group| {
            let rows = group.rows.into_iter().map(row).collect();
            match group.key {
                GroupKey::Machine(machine) => {
                    Panel::Machine(MachineGroup::new(&state, machine, rows))
                }
                GroupKey::Project(project) => {
                    Panel::Project(ProjectGroup::new(&state, project, rows))
                }
            }
        })
        .collect();
    let rows: Vec<AgentRow> = if groups.is_empty() {
        state.sidebar_agents().into_iter().map(row).collect()
    } else {
        Vec::new()
    };
    rsx! {
        nav { id: SIDEBAR_ID, class: "sidebar", aria_label: "Agents",
            div { class: "sidebar-head",
                span { class: "app-name", "{APP_NAME}" }
                AppBell {}
                button {
                    class: "icon-button",
                    title: "{find_title}",
                    aria_label: "{find_title}",
                    onclick: move |_| cockpit.update_with_drafts(actions::toggle_switcher),
                    Icon { kind: IconKind::Search, size: 16 }
                }
                button {
                    class: "icon-button raised",
                    title: "{new_title}",
                    aria_label: "{new_title}",
                    onclick: move |_| cockpit.update_with_drafts(actions::open_new_agent),
                    Icon { kind: IconKind::Plus, size: 16 }
                }
            }
            if !chips.is_empty() {
                StatusFilters { chips }
            }
            GroupSwitch {}
            div { id: LIST_ID, class: "agent-list scroll",
                if selected_hidden {
                    p { class: "hidden-note",
                        "The filter hides the selected agent. "
                        button {
                            class: "link-button",
                            onclick: move |_| cockpit.update(|state| actions::set_status_filter(state, StatusFilter::All)),
                            "Show all"
                        }
                    }
                }
                if rows.is_empty() && groups.is_empty() {
                    p { class: "sidebar-empty", "{empty_list_text(filter, &state.prefs.keys)}" }
                }
                for row in rows {
                    AgentRowView { key: "{row.id.get()}", row }
                }
                for panel in groups {
                    match panel {
                        Panel::Machine(group) => rsx! {
                            MachineGroupView { key: "{group.key}", group }
                        },
                        Panel::Project(group) => rsx! {
                            ProjectGroupView { key: "{group.key}", group }
                        },
                    }
                }
            }
            ConnectionFooter {}
        }
    }
}

/// A header and its rows: a machine's or a project's.
enum Panel {
    Machine(MachineGroup),
    Project(ProjectGroup),
}

/// A machine's panel in the sidebar: its header, a note when it does not answer, and its rows.
#[derive(Debug, Clone, PartialEq)]
struct MachineGroup {
    machine: MachineId,
    key: String,
    name: String,
    icon: IconKind,
    folded: bool,
    /// The machine's agents, whatever the filter shows.
    count: usize,
    /// The machine's agents that wait for you, which a folded header still shows.
    needs_you: usize,
    /// The machine's agents marked to handle, which a folded header still shows.
    to_handle: usize,
    /// What the panel says while the machine does not answer.
    trouble: Option<MachineTrouble>,
    rows: Vec<AgentRow>,
}

#[derive(Debug, Clone, PartialEq)]
struct MachineTrouble {
    line: String,
    /// The error, as a tooltip.
    error: Option<String>,
    /// A machine that answered no first connection: its panel's edge turns red.
    down: bool,
    /// This computer has no daemon yet: the panel asks where its agents run.
    asks: bool,
    /// The link that opens the update's screens, for a daemon of another version.
    update_link: Option<&'static str>,
}

/// The machine as a header or a machine line names it: "This computer" with a laptop, or its name
/// with a server.
fn machine_look(state: &AppState, machine: MachineId) -> (String, IconKind) {
    let label = state.machines.get(machine).map(|known| &known.daemon);
    let this_computer =
        machine == MachineId::THIS && label.is_none_or(|label| label.place != DaemonPlace::Ssh);
    if this_computer {
        (THIS_COMPUTER.to_owned(), IconKind::Laptop)
    } else {
        let name = label.map_or_else(String::new, |label| label.machine.clone());
        (name, IconKind::Server)
    }
}

impl MachineGroup {
    fn new(state: &AppState, machine: MachineId, rows: Vec<AgentRow>) -> Self {
        let known = state.machines.get(machine);
        let (name, icon) = machine_look(state, machine);
        let agents = state
            .agents
            .values()
            .filter(|entry| MachineId::of(entry.id()) == machine);
        let (mut count, mut needs_you, mut to_handle) = (0, 0, 0);
        for entry in agents {
            count += 1;
            needs_you += usize::from(entry.summary.status == AgentStatus::NeedsYou);
            to_handle += usize::from(entry.summary.mark == Mark::ToHandle);
        }
        let trouble = known.and_then(|known| {
            if let Some(line) = daemon_update::update_line(known) {
                return Some(MachineTrouble {
                    line: line.text.to_owned(),
                    error: None,
                    down: line.down,
                    asks: false,
                    update_link: Some(line.link),
                });
            }
            match &known.connection {
                ConnectionState::Connected => None,
                ConnectionState::Connecting { .. } => Some(MachineTrouble {
                    line: "Connecting…".to_owned(),
                    error: None,
                    down: false,
                    asks: false,
                    update_link: None,
                }),
                ConnectionState::Disconnected { error, .. } => Some(MachineTrouble {
                    line: "Not connected. The app tries again on its own.".to_owned(),
                    error: Some(error.clone()),
                    down: true,
                    asks: false,
                    update_link: None,
                }),
                ConnectionState::NoDaemon(_) => Some(MachineTrouble {
                    line: "No daemon yet.".to_owned(),
                    error: None,
                    down: false,
                    asks: true,
                    update_link: None,
                }),
            }
        });
        Self {
            machine,
            key: format!("{machine:?}"),
            name,
            icon,
            folded: state.ui.folded_machines.contains(&machine),
            count,
            needs_you,
            to_handle,
            trouble,
            rows,
        }
    }
}

#[component]
fn MachineGroupView(group: MachineGroup) -> Element {
    let cockpit: Cockpit = use_context();
    let MachineGroup {
        machine,
        name,
        icon,
        folded,
        count,
        needs_you,
        to_handle,
        trouble,
        rows,
        ..
    } = group;
    let down = trouble.as_ref().is_some_and(|trouble| trouble.down);
    let fold_label = if folded {
        format!("Show the agents of {name}")
    } else {
        format!("Fold the agents of {name}")
    };
    rsx! {
        div { class: if down { "machine-group down" } else { "machine-group" },
            button {
                id: head_id(machine),
                class: "machine-head",
                "data-head-machine": "{machine.get()}",
                aria_expanded: !folded,
                title: "{fold_label}",
                onclick: move |_| cockpit.update(|state| actions::toggle_machine(state, machine)),
                Icon {
                    kind: if folded { IconKind::ChevronRight } else { IconKind::ChevronDown },
                    size: 13,
                }
                span { class: "machine-icon", Icon { kind: icon, size: 14 } }
                span { class: "machine-name", "{name}" }
                if folded && needs_you > 0 {
                    span { class: "machine-needs-you", title: "Agents that wait for you", "{needs_you}" }
                }
                if folded && to_handle > 0 {
                    span { class: "machine-to-handle", title: "Agents marked to handle",
                        Icon { kind: IconKind::Flag, size: 11 }
                        "{to_handle}"
                    }
                }
                span { class: "machine-count", "{count}" }
            }
            if let Some(trouble) = trouble {
                p { class: "machine-trouble", title: trouble.error.clone().unwrap_or_default(),
                    "{trouble.line} "
                    if let Some(link) = trouble.update_link {
                        button {
                            class: "link-button",
                            onclick: move |_| cockpit.update(|state| daemon_update::open(state, machine)),
                            "{link}"
                        }
                    }
                    if trouble.down {
                        button {
                            class: "link-button",
                            onclick: move |_| cockpit.retry_machine(machine),
                            "Retry now"
                        }
                    }
                    if trouble.asks {
                        button {
                            class: "link-button",
                            onclick: move |_| cockpit.update(|state| state.ui.dialog = Some(Dialog::NoDaemon)),
                            "Choose how agents run here"
                        }
                    }
                }
            }
            if !folded {
                for row in rows {
                    AgentRowView { key: "{row.id.get()}", row }
                }
            }
        }
    }
}

/// A project's panel in the sidebar: its header, with Keep for a temporary project, and its rows.
#[derive(Debug, Clone, PartialEq)]
struct ProjectGroup {
    project: ProjectKey,
    key: String,
    name: String,
    /// The color's CSS value, `None` for "No project".
    color: Option<&'static str>,
    temporary: bool,
    folded: bool,
    /// The project's agents, whatever the filter shows.
    count: usize,
    needs_you: usize,
    to_handle: usize,
    /// The machine all its agents run on, when the header names it.
    machine: Option<RowMachine>,
    items: Vec<ProjectItem>,
}

/// What a project's panel lists under its header.
#[derive(Debug, Clone, PartialEq)]
enum ProjectItem {
    /// Split by machine: the line above that machine's rows.
    Machine(MachineLine),
    Row(Box<AgentRow>),
}

#[derive(Debug, Clone, PartialEq)]
struct MachineLine {
    machine: MachineId,
    name: String,
    icon: IconKind,
    /// The project's agents on the machine, whatever the filter shows.
    count: usize,
}

impl ProjectGroup {
    fn new(state: &AppState, project: ProjectKey, rows: Vec<AgentRow>) -> Self {
        let look = projects::look(state, &project);
        let (mut count, mut needs_you, mut to_handle) = (0, 0, 0);
        let mut on_machine: BTreeMap<MachineId, usize> = BTreeMap::new();
        for entry in state.agents.values() {
            if projects::project_of(state, entry) == project {
                count += 1;
                needs_you += usize::from(entry.summary.status == AgentStatus::NeedsYou);
                to_handle += usize::from(entry.summary.mark == Mark::ToHandle);
                *on_machine.entry(MachineId::of(entry.id())).or_default() += 1;
            }
        }
        let split = grouping::splits_by_machine(state) && on_machine.len() > 1;
        let mut items = Vec::with_capacity(rows.len());
        let mut last_machine = None;
        // The rows come sorted by machine, so that each machine's line shows once.
        for row in rows {
            let machine = MachineId::of(row.id);
            if split && last_machine != Some(machine) {
                last_machine = Some(machine);
                let (name, icon) = machine_look(state, machine);
                items.push(ProjectItem::Machine(MachineLine {
                    machine,
                    name,
                    icon,
                    count: on_machine.get(&machine).copied().unwrap_or_default(),
                }));
            }
            items.push(ProjectItem::Row(Box::new(row)));
        }
        Self {
            key: project.to_string(),
            folded: state.ui.folded_projects.contains(&project),
            machine: grouping::machine_on_project_head(state, &project)
                .and_then(|machine| RowMachine::new(state, machine)),
            project,
            name: look.name,
            color: look.color.map(AccountColor::css),
            temporary: look.temporary,
            count,
            needs_you,
            to_handle,
            items,
        }
    }
}

/// The id of a project's header, which the arrow keys give the focus to.
fn project_head_id(project: &ProjectKey) -> String {
    format!("project-head-{project}")
}

#[component]
fn ProjectGroupView(group: ProjectGroup) -> Element {
    let cockpit: Cockpit = use_context();
    let ProjectGroup {
        project,
        key,
        name,
        color,
        temporary,
        folded,
        count,
        needs_you,
        to_handle,
        machine,
        items,
    } = group;
    let fold_label = if folded {
        format!("Show the agents of {name}")
    } else {
        format!("Fold the agents of {name}")
    };
    let toggled = GroupKey::Project(project.clone());
    let swatch_class = match (color, temporary) {
        (None, _) => "project-swatch none",
        (Some(_), true) => "project-swatch temporary",
        (Some(_), false) => "project-swatch",
    };
    let swatch_style = projects_tab::swatch_style(color);
    let head = project_head_id(&project);
    // Only a kept project has a place the user chose: a drag by its header moves it.
    let (group_id, group_class) = match &project {
        ProjectKey::Kept(id) => (
            Some(format!("{KEPT_GROUP_ID_PREFIX}{}", id.get())),
            format!("machine-group project-group {PROJECT_DRAG_CLASS}"),
        ),
        ProjectKey::Repository(_) | ProjectKey::None => {
            (None, "machine-group project-group".to_owned())
        }
    };
    rsx! {
        div { id: group_id, class: group_class,
            div { class: "project-head-row",
                button {
                    id: head,
                    class: GROUP_HEAD_CLASS,
                    "data-head-project": "{key}",
                    aria_expanded: !folded,
                    title: "{fold_label}",
                    onclick: move |_| cockpit.update(|state| grouping::toggle_folded(state, &toggled)),
                    Icon {
                        kind: if folded { IconKind::ChevronRight } else { IconKind::ChevronDown },
                        size: 13,
                    }
                    span { class: swatch_class, style: swatch_style, aria_hidden: true }
                    span { class: if temporary { "machine-name temporary" } else { "machine-name" }, "{name}" }
                    if let Some(machine) = &machine {
                        span { class: "head-machine", title: "Its agents run on {machine.name}",
                            Icon { kind: IconKind::Server, size: 11 }
                            "{machine.tag}"
                        }
                    }
                    if folded && needs_you > 0 {
                        span { class: "machine-needs-you", title: "Agents that wait for you", "{needs_you}" }
                    }
                    if folded && to_handle > 0 {
                        span { class: "machine-to-handle", title: "Agents marked to handle",
                            Icon { kind: IconKind::Flag, size: 11 }
                            "{to_handle}"
                        }
                    }
                    span { class: "machine-count", "{count}" }
                }
                if let ProjectKey::Repository(repository) = project {
                    button {
                        class: "keep-button",
                        title: "Keep this project when it has no agent left, with its name, color and rules",
                        onclick: move |_| {
                            let repository = repository.clone();
                            cockpit.update(move |state| {
                                projects::keep(state, &repository);
                            });
                        },
                        "Keep"
                    }
                }
            }
            if !folded {
                for item in items {
                    match item {
                        ProjectItem::Machine(line) => rsx! {
                            div {
                                key: "machine-{line.machine.get()}",
                                class: "machine-line",
                                Icon { kind: line.icon, size: 12 }
                                span { class: "machine-line-name", "{line.name}" }
                                span { class: "machine-line-count", "{line.count}" }
                            }
                        },
                        ProjectItem::Row(row) => rsx! {
                            AgentRowView { key: "{row.id.get()}", row: *row }
                        },
                    }
                }
            }
            div { class: GROUP_END_CLASS, "data-end": "{key}", aria_hidden: true }
        }
    }
}

/// The chips of the statuses agents have, with their counts: a chip hides the other agents'
/// rows, and never changes their order.
#[component]
fn StatusFilters(chips: Vec<FilterChip>) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        div { class: "status-filters", role: "group", aria_label: "Show agents by status",
            for chip in chips {
                button {
                    key: "{filter_label(chip.filter)}",
                    class: format!(
                        "filter-chip {} {}",
                        if chip.chosen { "chosen" } else { "" },
                        chip_class(chip.filter),
                    ),
                    aria_pressed: chip.chosen,
                    title: "{filter_label(chip.filter)}: {chip.count}",
                    aria_label: "{filter_label(chip.filter)}: {chip.count}",
                    onclick: move |_| cockpit.update(|state| actions::set_status_filter(state, chip.filter)),
                    if chip.filter == StatusFilter::ToHandle {
                        Icon { kind: IconKind::Flag, size: 12 }
                    } else if chip.filter.status().is_some() {
                        span { class: "status-dot", aria_hidden: true }
                    } else {
                        span { class: "filter-word", "{filter_label(chip.filter)}" }
                    }
                    span { class: "filter-count", "{chip.count}" }
                }
            }
        }
    }
}

#[component]
fn AgentRowView(row: AgentRow) -> Element {
    let cockpit: Cockpit = use_context();
    let agent = row.id;
    let mut classes = vec![ROW_CLASS, row.status_class.as_str()];
    if row.selected {
        classes.push("selected");
    }
    if row.unread {
        classes.push("unread");
    }
    if row.to_handle {
        classes.push(TO_HANDLE_CLASS);
    }
    if let Some(TitleEdit {
        title, select_all, ..
    }) = row.edit.clone()
    {
        classes.push("editing");
        let class = classes.join(" ");
        // A div, not a button: a button would take the field's Space and Enter as clicks.
        return rsx! {
            div { id: reorder::row_id(agent), class,
                span { class: "row-line",
                    span { class: "status-dot", aria_hidden: true }
                    if let Some(label) = row.account.clone() {
                        AccountLetter { label }
                    }
                    input {
                        id: ROW_TITLE_FIELD_ID,
                        class: "row-title-field",
                        r#type: "text",
                        aria_label: "Agent title",
                        // Follows the keyboard alone: a render that lags behind typing would put
                        // an older title back.
                        initial_value: "{title}",
                        maxlength: "{MAX_TITLE_CHARS}",
                        onmounted: move |_| bridge::focus_title_field(ROW_TITLE_FIELD_ID, select_all),
                        oninput: move |event: FormEvent| cockpit.type_title(event.value()),
                        onkeydown: move |event: KeyboardEvent| {
                            if event.key() == Key::Enter {
                                cockpit.change_with_drafts(actions::finish_rename);
                            }
                        },
                        onfocus: move |_| cockpit.title_field_focused(),
                        onblur: move |_| cockpit.title_field_blurred(TitlePlace::Sidebar),
                    }
                }
            }
        };
    }
    let class = classes.join(" ");
    // The close button sits beside the row, over its corner: a button cannot hold another.
    rsx! {
        div { class: if row.selected { "row-wrap selected" } else { "row-wrap" },
            AgentRowButton { row: row.clone(), class }
            button {
                class: "row-close",
                aria_label: "Close {row.title}",
                title: "Close the agent ({row.close_keys} when selected)",
                onclick: move |_| cockpit.change_with_drafts(|state, drafts| actions::request_close(state, drafts, agent)),
                Icon { kind: IconKind::Close, size: 12 }
            }
        }
    }
}

#[component]
fn AgentRowButton(row: AgentRow, class: String) -> Element {
    let cockpit: Cockpit = use_context();
    let agent = row.id;
    rsx! {
        button {
            id: reorder::row_id(agent),
            class,
            aria_current: row.selected,
            "data-row-agent": "{agent.get()}",
            onclick: move |_| cockpit.update(|state| actions::select_agent(state, agent)),
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                let point = event.client_coordinates();
                // Whole pixels are enough to place a menu.
                #[expect(clippy::cast_possible_truncation, reason = "a window's pixels fit in i32")]
                let (x, y) = (point.x.round() as i32, point.y.round() as i32);
                cockpit.update(|state| to_handle::open_menu(state, agent, x, y));
            },
            span { class: "row-line",
                span { class: "status-dot", aria_hidden: true }
                if let Some(machine) = &row.machine {
                    // The account and the machine share one chip.
                    span { class: "row-chip",
                        if let Some(label) = row.account.clone() {
                            AccountLetter { label }
                        }
                        span { class: "row-machine", title: "Runs on {machine.name}",
                            Icon { kind: IconKind::Server, size: 10 }
                            span { aria_hidden: true, "{machine.tag}" }
                            span { class: "visually-hidden", "on {machine.name}" }
                        }
                    }
                } else if let Some(label) = row.account.clone() {
                    AccountLetter { label }
                }
                span {
                    class: "row-title",
                    ondoubleclick: move |_| cockpit.change_with_drafts(|state, drafts| {
                        actions::start_rename_in_sidebar(state, drafts, agent);
                        Vec::new()
                    }),
                    "{row.title}"
                }
                // Always there, so that the style sheet can grow it in and shrink it out.
                span {
                    class: "row-flag",
                    title: if row.to_handle { TO_HANDLE } else { "" },
                    aria_hidden: !row.to_handle,
                    Icon { kind: IconKind::Flag, size: 12 }
                    if row.to_handle {
                        span { class: "visually-hidden", "{TO_HANDLE}" }
                    }
                }
                RowBell { level: row.notifications }
                RowActivity { activity: row.activity.clone(), warns: row.warns }
                if let Some(tasks) = &row.tasks {
                    span { class: "row-tasks", title: "Tasks done", "{tasks}" }
                }
                if let Some(time) = row.time {
                    RowTimeView { time }
                }
                if row.pending > 0 {
                    span { class: "pending-badge", "{row.pending}" }
                }
                if row.unread {
                    span { class: "unread-dot", aria_hidden: true }
                    span { class: "visually-hidden", "unread" }
                }
            }
        }
    }
}

/// What the agent does, dimmed after its title, whole on hover. Empty, it still takes the line's
/// free width, which keeps the time at the right edge.
#[component]
fn RowActivity(activity: String, warns: bool) -> Element {
    let class = match (activity.is_empty(), warns) {
        (true, _) => "row-activity empty",
        (false, true) => "row-activity warning",
        (false, false) => "row-activity",
    };
    rsx! {
        span { class, title: "{activity}",
            if activity.is_empty() {
                span { class: "visually-hidden", {format::status_label(AgentStatus::Idle)} }
            } else {
                "{activity}"
            }
        }
    }
}

/// The only part of a row that reads the clock, so its tick renders this text alone.
#[component]
fn RowTimeView(time: RowTime) -> Element {
    let Clock(now) = use_context::<Clock>();
    let RowTimeText { text, tooltip } = row_time_text(time, now());
    rsx! {
        span { class: "row-time", title: "{tooltip}", "{text}" }
    }
}

/// The foot's line while this computer's daemon does not answer. Connected, it shows nothing: the
/// Machines dialog says where the agents run.
#[derive(Debug, Clone, PartialEq)]
struct ConnectionLine {
    class: &'static str,
    line: String,
    detail: Option<String>,
}

#[component]
fn ConnectionFooter() -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let state = cockpit.state.read();
    let machine = &state.machines.this.daemon.machine;
    let trouble = match &state.machines.this.connection {
        ConnectionState::Connected => None,
        ConnectionState::Connecting { attempt } => {
            let mut line = format!("Connecting to {machine}");
            if *attempt > 1 {
                line.push_str(&format!(", attempt {attempt}"));
            }
            Some(ConnectionLine {
                class: "connection connecting",
                line,
                detail: None,
            })
        }
        ConnectionState::Disconnected { error, retry_at } => {
            // Reading the clock refreshes the countdown every second.
            let _tick = now();
            let wait = retry_at.saturating_duration_since(Instant::now());
            Some(ConnectionLine {
                class: "connection disconnected",
                line: format!("Not connected to {machine}"),
                detail: Some(format!("{error}. Retrying in {}.", format::duration(wait))),
            })
        }
        ConnectionState::NoDaemon(screen) => Some(match screen.absence {
            Absence::NothingRuns => ConnectionLine {
                class: "connection waiting",
                line: "No daemon yet".to_owned(),
                detail: None,
            },
            Absence::Failed => ConnectionLine {
                class: "connection disconnected",
                line: format!("{} does not answer", screen.subject),
                detail: Some(screen.error.clone()),
            },
        }),
    };
    // With several machines, each panel says how its machine answers.
    let panels = !state.machines.others.is_empty();
    let trouble = use_presence(trouble.filter(|_| !panels));
    let settings_open = matches!(state.ui.dialog, Some(Dialog::Settings(_)));
    rsx! {
        div { class: "sidebar-foot",
            div { class: "foot-buttons",
                AccountsButton {}
                button {
                    class: "foot-accounts",
                    title: "The machines the app reaches",
                    onclick: move |_| cockpit.update(machines::open_machines),
                    Icon { kind: IconKind::Server, size: 14 }
                    "Machines"
                }
                span { class: "spacer" }
                button {
                    class: if settings_open { "icon-button foot-settings pressed" } else { "icon-button foot-settings" },
                    title: "Settings",
                    aria_label: "Settings",
                    onclick: move |_| cockpit.update(settings::open),
                    Icon { kind: IconKind::Gear, size: 16 }
                }
            }
            if let Some(Shown { value: ConnectionLine { class, line, detail }, leaving, .. }) = trouble {
                div {
                    class: if leaving { "connection-trouble leaving" } else { "connection-trouble" },
                    inert: leaving.then_some(true),
                    span { class,
                        span { class: "connection-dot", aria_hidden: true }
                        "{line}"
                    }
                    if let Some(detail) = detail {
                        span { class: "connection-error", "{detail}" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::state::summary;

    fn at(seconds: u64) -> Timestamp {
        Timestamp::from_unix_millis(seconds * 1000)
    }

    #[test]
    fn an_idle_agent_with_background_tasks_says_so_in_the_working_color() {
        let mut idle = summary(1, AgentStatus::Idle);
        idle.background_running = 2;
        let mut working = summary(2, AgentStatus::Working);
        working.background_running = 2;

        assert_eq!(
            activity_line(&AgentEntry::new(idle.clone())),
            "Idle, 2 running in the background"
        );
        assert_eq!(
            format::agent_status_class(&idle),
            "status-idle background-work"
        );
        assert_eq!(format::agent_status_label(&working), "Working");
        assert_eq!(format::agent_status_class(&working), "status-working");
        assert_eq!(
            activity_line(&AgentEntry::new(summary(3, AgentStatus::Idle))),
            ""
        );
    }

    fn timed(status: AgentStatus) -> AgentSummary {
        AgentSummary {
            last_activity_at: Some(at(1_000)),
            turn_started_at: Some(at(700)),
            ..summary(1, status)
        }
    }

    #[test]
    fn a_working_row_counts_its_turn_and_the_others_their_last_activity() {
        assert_eq!(
            row_time(&timed(AgentStatus::Working)),
            Some(RowTime::Turn(at(700)))
        );
        assert_eq!(
            row_time(&timed(AgentStatus::NeedsYou)),
            Some(RowTime::Waiting(at(1_000)))
        );
        assert_eq!(
            row_time(&timed(AgentStatus::Idle)),
            Some(RowTime::LastActive(at(1_000)))
        );
        assert_eq!(
            row_time(&timed(AgentStatus::Exited)),
            Some(RowTime::LastActive(at(1_000)))
        );
    }

    #[test]
    fn a_daemon_without_the_times_shows_none() {
        assert_eq!(row_time(&summary(1, AgentStatus::Working)), None);
        assert_eq!(row_time(&summary(1, AgentStatus::Idle)), None);
    }

    #[test]
    fn the_text_is_short_and_its_tooltip_says_what_it_counts() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_252);
        let shown = |text: &str, tooltip: &str| RowTimeText {
            text: text.to_owned(),
            tooltip: tooltip.to_owned(),
        };

        assert_eq!(
            row_time_text(RowTime::Turn(at(1_000)), now),
            shown("4 min", "Turn running: 4 min")
        );
        assert_eq!(
            row_time_text(RowTime::Waiting(at(1_240)), now),
            shown("12 s", "Waiting for you: 12 s")
        );
        assert_eq!(
            row_time_text(RowTime::LastActive(at(1_000)), now),
            shown("4 min", "Last active 4 min ago")
        );
    }

    #[test]
    fn an_idle_row_counts_no_seconds() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_252);
        let just_now = RowTimeText {
            text: "now".to_owned(),
            tooltip: "Last active less than a minute ago".to_owned(),
        };

        assert_eq!(row_time_text(RowTime::LastActive(at(1_240)), now), just_now);
        assert_eq!(row_time_text(RowTime::LastActive(at(1_300)), now), just_now);
        assert_eq!(
            row_time_text(RowTime::LastActive(at(1_192)), now).text,
            "1 min"
        );
    }
}
