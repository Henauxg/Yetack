//! The agent list: a status filter, compact one-line rows in the order the user sets, a panel per
//! machine that says how it answers, and the foot's buttons.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use cockpit_protocol::{AgentId, AgentStatus, AgentSummary, MAX_TITLE_CHARS, Mark, Timestamp};
use dioxus::prelude::*;

use crate::bridge::{self, ROW_TITLE_FIELD_ID};
use crate::connection::{Clock, Cockpit};
use crate::keymap::{Command, Keymap};
use crate::machine::MachineId;
use crate::prefs::{AccountColor, NotificationLevel, StatusFilter};
use crate::state::accounts::{self, AccountLabel, SignInLine};
use crate::state::grouping::GroupKey;
use crate::state::machine_status::{self, StatusLine, Tone};
use crate::state::machines::MachineName;
use crate::state::projects::ProjectKey;
use crate::state::sidebar_keys::SidebarItem;
use crate::state::{
    AgentEntry, AppState, Dialog, TitleEdit, TitlePlace, actions, grouping, machines,
    notification_levels, project_menu, projects, settings, to_handle,
};
use crate::ui::accounts::{AccountLetter, AccountsButton};
use crate::ui::app_update::ReleaseNotice;
use crate::ui::branch_card::CardHover;
use crate::ui::grouping::GroupSwitch;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::machines::{machine_icon, run_action};
use crate::ui::notification_levels::{self as levels_ui, AppBell, LevelIcon};
use crate::ui::presence::use_presence;
use crate::ui::project_menu::ProjectNameField;
use crate::ui::projects_tab;
use crate::ui::reorder::{
    self, GROUP_END_CLASS, GROUP_HEAD_CLASS, KEPT_GROUP_ID_PREFIX, LIST_ID, PROJECT_DRAG_CLASS,
    ROW_CLASS,
};
use crate::ui::splitter::SIDEBAR_ID;
use crate::voice_desk::VoiceDesk;
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
    /// The "to handle" mark you put on the agent.
    mark: Mark,
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
            mark: summary.mark,
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

/// The room, in pixels, between the sidebar and a row's branch card.
const CARD_GAP: u16 = 8;

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

/// The mark's name, in the filter.
const TO_HANDLE: &str = "To handle";

/// The row's flag, as its tooltip says it.
fn mark_label(mark: Mark) -> &'static str {
    match mark {
        Mark::Unmarked => "",
        Mark::ToHandle => "To handle, until you send it a message",
        Mark::ToHandleKept => "To handle, until you clear the mark",
    }
}

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
                MachineStrip {}
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
            SidebarFoot {}
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
    /// What the panel says while the machine does not answer or has an update to show.
    line: Option<ShownLine>,
    /// It answers no request: its panel's edge turns red.
    down: bool,
    rows: Vec<AgentRow>,
}

/// The machine as a header or a machine line names it: "Local" with a laptop, or its name with a
/// server.
struct MachineLook {
    name: String,
    icon: IconKind,
}

impl MachineLook {
    fn new(state: &AppState, machine: MachineId) -> Self {
        let MachineName { name, local } = MachineName::of(state, machine);
        Self {
            name,
            icon: machine_icon(local),
        }
    }
}

impl MachineGroup {
    fn new(state: &AppState, machine: MachineId, rows: Vec<AgentRow>) -> Self {
        let MachineLook { name, icon } = MachineLook::new(state, machine);
        let agents = state
            .agents
            .values()
            .filter(|entry| MachineId::of(entry.id()) == machine);
        let (mut count, mut needs_you, mut to_handle) = (0, 0, 0);
        for entry in agents {
            count += 1;
            needs_you += usize::from(entry.summary.status == AgentStatus::NeedsYou);
            to_handle += usize::from(entry.summary.mark.is_to_handle());
        }
        let status = state.machines.get(machine).map(machine_status::status);
        let down = status
            .as_ref()
            .is_some_and(|status| status.tone == Tone::Down);
        let line = status.and_then(|status| {
            Some(ShownLine {
                name: None,
                tone: status.tone,
                line: status.line?,
            })
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
            line,
            down,
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
        line,
        down,
        rows,
        ..
    } = group;
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
            MachineLineView { machine, line }
            if !folded {
                for row in rows {
                    AgentRowView { key: "{row.id.get()}", row }
                }
            }
        }
    }
}

/// A machine's line, as its panel or the strip shows it.
#[derive(Debug, Clone, PartialEq)]
struct ShownLine {
    /// The machine's name, in the strip, where no header says it.
    name: Option<String>,
    tone: Tone,
    line: StatusLine,
}

/// What a machine says, with what to do about it: under its panel's head, or in the strip. The
/// line folds open and shut, so the rows under it move with it.
#[component]
fn MachineLineView(machine: MachineId, line: Option<ShownLine>) -> Element {
    let cockpit: Cockpit = use_context();
    let Some(shown) = use_presence(line) else {
        return rsx! {};
    };
    let ShownLine { name, tone, line } = shown.value;
    rsx! {
        div {
            class: if shown.leaving { "machine-trouble-fold leaving" } else { "machine-trouble-fold" },
            inert: shown.leaving.then_some(true),
            div { class: "machine-trouble-clip",
                p {
                    class: "machine-trouble {tone.class()}",
                    title: line.error.unwrap_or_default(),
                    if tone == Tone::Busy {
                        span { class: "spinner", aria_hidden: true }
                    } else {
                        span { class: "trouble-mark", aria_hidden: true }
                    }
                    if let Some(name) = name {
                        span { class: "trouble-machine", "{name}" }
                    }
                    match &line.progress {
                        Some(progress) => rsx! {
                            span { class: "trouble-text", "{progress.text}" }
                        },
                        None => rsx! {
                            span { class: "trouble-text", "{line.text}" }
                        },
                    }
                    for action in line.actions {
                        button {
                            key: "{action.label()}",
                            class: "link-button",
                            onclick: move |_| run_action(cockpit, machine, action),
                            "{action.label()}"
                        }
                    }
                    if let Some(progress) = &line.progress {
                        span {
                            class: "update-bar",
                            role: "progressbar",
                            aria_valuenow: "{progress.percent}",
                            aria_valuemin: "0",
                            aria_valuemax: "100",
                            span {
                                style: format!("transform: scaleX({})", f32::from(progress.percent) / 100.0),
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The machines with something to say, at the top of a sidebar without machine panels: a line
/// each, as its panel would say it. Gone while every machine answers.
#[component]
fn MachineStrip() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let mut lines = machine_status::strip_lines(&state);
    // A line per machine the app knows, so that each leaves on its own.
    let machines: Vec<MachineId> = state.machines.all().map(|(machine, _)| machine).collect();
    let shown = use_presence((!lines.is_empty()).then_some(()));
    let Some(shown) = shown else {
        return rsx! {};
    };
    let mut line_of = |machine: MachineId| {
        let at = lines.iter().position(|line| line.machine == machine)?;
        let line = lines.remove(at);
        Some(ShownLine {
            name: Some(line.name),
            tone: line.tone,
            line: line.line,
        })
    };
    let strip: Vec<(MachineId, Option<ShownLine>)> = machines
        .into_iter()
        .map(|machine| (machine, line_of(machine)))
        .collect();
    rsx! {
        div {
            class: if shown.leaving { "machine-strip leaving" } else { "machine-strip" },
            inert: shown.leaving.then_some(true),
            role: "status",
            div { class: "machine-strip-clip",
                div { class: "machine-strip-box",
                    for (machine, line) in strip {
                        MachineLineView { key: "{machine.get()}", machine, line }
                    }
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
                to_handle += usize::from(entry.summary.mark.is_to_handle());
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
                let MachineLook { name, icon } = MachineLook::new(state, machine);
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
            folded: state.prefs.projects.folded.contains(&project),
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
    let renaming = match &project {
        ProjectKey::Kept(id) => {
            (cockpit.state.read().ui.project_rename == Some(*id)).then_some(*id)
        }
        ProjectKey::Repository(_) | ProjectKey::None => None,
    };
    let menu_project = project.clone();
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
                if let Some(id) = renaming {
                    // A div: a button would take the field's Space and Enter. Without the header's
                    // class, which a drag starts from.
                    div { class: "project-head-field",
                        span { class: swatch_class, style: swatch_style.clone(), aria_hidden: true }
                        ProjectNameField { id, name: name.clone() }
                    }
                } else {
                    button {
                        id: head,
                        class: GROUP_HEAD_CLASS,
                        "data-head-project": "{key}",
                        aria_expanded: !folded,
                        title: "{fold_label} (right-click for the project's menu)",
                        onclick: move |_| cockpit.update(|state| grouping::toggle_folded(state, &toggled)),
                        oncontextmenu: move |event: MouseEvent| {
                            event.prevent_default();
                            let point = event.client_coordinates();
                            // Whole pixels are enough to place a menu.
                            #[expect(clippy::cast_possible_truncation, reason = "a window's pixels fit in i32")]
                            let (x, y) = (point.x.round() as i32, point.y.round() as i32);
                            let project = menu_project.clone();
                            cockpit.update(|state| project_menu::open(state, project, x, y));
                        },
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
    if row.mark.is_to_handle() {
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
    let hover: CardHover = use_context();
    let voice: VoiceDesk = use_context();
    let agent = row.id;
    // A memo: the live words change the voice state several times a second.
    let dictating = use_memo(move || voice.state.read().dictating() == Some(agent))();
    rsx! {
        button {
            id: reorder::row_id(agent),
            class,
            aria_current: row.selected,
            "data-row-agent": "{agent.get()}",
            onclick: move |_| cockpit.update(|state| actions::select_agent(state, agent)),
            // The branch card shows beside the sidebar, level with the row.
            onmouseenter: move |event: MouseEvent| {
                let width = cockpit.state.peek().prefs.sidebar.width();
                let top = event.client_coordinates().y - event.element_coordinates().y;
                // Whole pixels are enough to place a card.
                #[expect(clippy::cast_possible_truncation, reason = "a window's pixels fit in i32")]
                let y = top.round() as i32;
                hover.enter(cockpit, agent, i32::from(width) + i32::from(CARD_GAP), y);
            },
            onmouseleave: move |_| hover.leave(cockpit),
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                let point = event.client_coordinates();
                // Whole pixels are enough to place a menu.
                #[expect(clippy::cast_possible_truncation, reason = "a window's pixels fit in i32")]
                let (x, y) = (point.x.round() as i32, point.y.round() as i32);
                cockpit.update(|state| to_handle::open_menu(state, agent, x, y));
            },
            span { class: "row-line",
                if dictating {
                    span {
                        class: "row-mic",
                        title: "Dictating into this agent",
                        Icon { kind: IconKind::Mic, size: 10 }
                        span { class: "visually-hidden", "Dictating" }
                    }
                } else {
                    span { class: "status-dot", aria_hidden: true }
                }
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
                    class: if row.mark == Mark::ToHandleKept { "row-flag kept" } else { "row-flag" },
                    title: mark_label(row.mark),
                    aria_hidden: !row.mark.is_to_handle(),
                    Icon { kind: IconKind::Flag, size: 12 }
                    if row.mark.is_to_handle() {
                        span { class: "visually-hidden", {mark_label(row.mark)} }
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

/// The note of a newer release, then Accounts, Machines and the Settings gear. Each machine's
/// panel, or the strip above the list, says how its machine answers.
#[component]
fn SidebarFoot() -> Element {
    let cockpit: Cockpit = use_context();
    let settings_open = matches!(cockpit.state.read().ui.dialog, Some(Dialog::Settings(_)));
    rsx! {
        div { class: "sidebar-foot",
            ReleaseNotice {}
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
            quiet_since: None,
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
