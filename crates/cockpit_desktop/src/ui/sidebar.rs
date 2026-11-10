//! The agent list: a status filter, compact two-line rows in the order the user sets, and the
//! connection's state.

use std::time::{Duration, Instant, SystemTime};

use cockpit_protocol::{AgentId, AgentStatus, AgentSummary, MAX_TITLE_CHARS, Timestamp};
use dioxus::prelude::*;

use crate::bridge::{self, ROW_TITLE_FIELD_ID};
use crate::connection::{Clock, Cockpit};
use crate::daemon_label::DaemonPlace;
use crate::machine::MachineId;
use crate::prefs::StatusFilter;
use crate::state::accounts::{self, AccountLabel, SignInLine};
use crate::state::no_daemon::Absence;
use crate::state::sidebar_keys::SidebarItem;
use crate::state::{
    AgentEntry, AppState, ConnectionState, Dialog, TitleEdit, TitlePlace, actions, daemon_update,
    machines,
};
use crate::ui::accounts::{AccountTag, AccountsButton};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::machines::THIS_COMPUTER;
use crate::ui::reorder::{self, LIST_ID, ROW_CLASS};
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
            edit,
        }
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

/// What the agent does now, or what its status says when it reports nothing.
pub fn activity_line(entry: &AgentEntry) -> String {
    if let Some(activity) = &entry.summary.activity {
        return activity.clone();
    }
    match entry.summary.status {
        AgentStatus::Exited => "Process ended".to_owned(),
        _ => format::agent_status_label(&entry.summary),
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
    filter.status().map_or("All", format::status_label)
}

/// What the list says when the filter leaves no row.
fn empty_list_text(filter: StatusFilter) -> &'static str {
    match filter {
        StatusFilter::All => "No agent yet. Ctrl+N starts one.",
        StatusFilter::NeedsYou => "No agent needs you.",
        StatusFilter::Working => "No agent is working.",
        StatusFilter::Idle => "No agent is idle.",
        StatusFilter::Exited => "No agent has exited.",
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
    let row = |entry: &AgentEntry| {
        let edit = state
            .ui
            .title_edit
            .as_ref()
            .filter(|edit| edit.agent == entry.id() && edit.place == TitlePlace::Sidebar)
            .cloned();
        AgentRow::new(&state, entry, edit)
    };
    let groups: Vec<MachineGroup> = state
        .sidebar_groups()
        .into_iter()
        .map(|group| {
            MachineGroup::new(
                &state,
                group.machine,
                group.rows.into_iter().map(row).collect(),
            )
        })
        .collect();
    let rows: Vec<AgentRow> = if groups.is_empty() {
        state.sidebar_agents().into_iter().map(row).collect()
    } else {
        Vec::new()
    };
    rsx! {
        nav { class: "sidebar", aria_label: "Agents",
            div { class: "sidebar-head",
                span { class: "app-name", "{APP_NAME}" }
                button {
                    class: "icon-button",
                    title: "Find an agent (Ctrl+K)",
                    aria_label: "Find an agent (Ctrl+K)",
                    onclick: move |_| cockpit.update_with_drafts(actions::toggle_switcher),
                    Icon { kind: IconKind::Search, size: 16 }
                }
                button {
                    class: "icon-button raised",
                    title: "New agent (Ctrl+N)",
                    aria_label: "New agent (Ctrl+N)",
                    onclick: move |_| cockpit.update_with_drafts(actions::open_new_agent),
                    Icon { kind: IconKind::Plus, size: 16 }
                }
            }
            if !chips.is_empty() {
                StatusFilters { chips }
            }
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
                    p { class: "sidebar-empty", "{empty_list_text(filter)}" }
                }
                for row in rows {
                    AgentRowView { key: "{row.id.get()}", row }
                }
                for group in groups {
                    MachineGroupView { key: "{group.key}", group }
                }
            }
            ConnectionFooter {}
        }
    }
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

impl MachineGroup {
    fn new(state: &AppState, machine: MachineId, rows: Vec<AgentRow>) -> Self {
        let known = state.machines.get(machine);
        let label = known.map(|known| &known.daemon);
        let this_computer =
            machine == MachineId::THIS && label.is_none_or(|label| label.place != DaemonPlace::Ssh);
        let name = if this_computer {
            THIS_COMPUTER.to_owned()
        } else {
            label.map_or_else(String::new, |label| label.machine.clone())
        };
        let agents = state
            .agents
            .values()
            .filter(|entry| MachineId::of(entry.id()) == machine);
        let (count, needs_you) = agents.fold((0, 0), |(count, needs_you), entry| {
            let waits = entry.summary.status == AgentStatus::NeedsYou;
            (count + 1, needs_you + usize::from(waits))
        });
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
            icon: if this_computer {
                IconKind::Laptop
            } else {
                IconKind::Server
            },
            folded: state.ui.folded_machines.contains(&machine),
            count,
            needs_you,
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
                        chip.filter.status().map_or("", format::status_class),
                    ),
                    aria_pressed: chip.chosen,
                    title: "{filter_label(chip.filter)}: {chip.count}",
                    aria_label: "{filter_label(chip.filter)}: {chip.count}",
                    onclick: move |_| cockpit.update(|state| actions::set_status_filter(state, chip.filter)),
                    if chip.filter.status().is_some() {
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
                RowSecondLine { row: row.clone() }
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
                title: "Close the agent (Ctrl+Shift+W)",
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
            span { class: "row-line",
                span { class: "status-dot", aria_hidden: true }
                span {
                    class: "row-title",
                    ondoubleclick: move |_| cockpit.change_with_drafts(|state, drafts| {
                        actions::start_rename_in_sidebar(state, drafts, agent);
                        Vec::new()
                    }),
                    "{row.title}"
                }
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
            RowSecondLine { row: row.clone() }
        }
    }
}

/// The account's tag, then what the agent does.
#[component]
fn RowSecondLine(row: AgentRow) -> Element {
    rsx! {
        span { class: "row-line row-second",
            if let Some(label) = row.account {
                AccountTag { label }
            }
            span { class: if row.warns { "row-activity warning" } else { "row-activity" }, "{row.activity}" }
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

#[component]
fn ConnectionFooter() -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let state = cockpit.state.read();
    let machine = &state.machines.this.daemon.machine;
    let (class, line, detail) = match &state.machines.this.connection {
        ConnectionState::Connected => (
            "connection connected",
            state.machines.this.daemon.connected_line(),
            None,
        ),
        ConnectionState::Connecting { attempt } => {
            let mut line = format!("Connecting to {machine}");
            if *attempt > 1 {
                line.push_str(&format!(", attempt {attempt}"));
            }
            ("connection connecting", line, None)
        }
        ConnectionState::Disconnected { error, retry_at } => {
            // Reading the clock refreshes the countdown every second.
            let _tick = now();
            let wait = retry_at.saturating_duration_since(Instant::now());
            let detail = format!("{error}. Retrying in {}.", format::duration(wait));
            (
                "connection disconnected",
                format!("Not connected to {machine}"),
                Some(detail),
            )
        }
        ConnectionState::NoDaemon(screen) => match screen.absence {
            Absence::NothingRuns => ("connection waiting", "No daemon yet".to_owned(), None),
            Absence::Failed => (
                "connection disconnected",
                format!("{} does not answer", screen.subject),
                Some(screen.error.clone()),
            ),
        },
    };
    // With several machines, each panel says how its machine answers.
    let panels = !state.machines.others.is_empty();
    rsx! {
        div { class: "sidebar-foot",
            div { class: "foot-buttons",
                AccountsButton {}
                button {
                    class: "foot-accounts",
                    title: "The machines the app reaches",
                    onclick: move |_| cockpit.update(machines::open_machines),
                    Icon { kind: IconKind::Server, size: 13 }
                    "Machines"
                }
            }
            if !panels {
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
            "Idle"
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
