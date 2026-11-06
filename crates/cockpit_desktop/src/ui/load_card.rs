//! The header's Load chip, and its card: the server's CPU, memory and load average, each agent's
//! share, what the other processes use, and the selected agent's heaviest commands. Both read the
//! load's own signal, so a new sample renders them and nothing else.

use cockpit_protocol::{AgentId, AgentLoad, MachineLoad};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::format;
use crate::state::machine_load::{self, AgentLine, LoadLevel, LoadView};
use crate::state::{AppState, Popover};
use crate::ui::icons::{Icon, IconKind};

/// The class of a value the daemon could not give.
const UNKNOWN_CLASS: &str = "load-unknown";

#[component]
pub fn LoadChip(open: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let view = cockpit.load.read();
    let (value, short, class, label) = match &*view {
        LoadView::Known(load) => {
            let percent = machine_load::cpu_percent(&load.server);
            let text = format!("{percent}%");
            let class = LoadLevel::from_percent(percent).class();
            let label = format!("Load: the server's CPU is {percent}% busy");
            (text.clone(), text, class, label)
        }
        LoadView::Waiting => (
            "…".to_owned(),
            "…".to_owned(),
            UNKNOWN_CLASS,
            "Load: measuring".to_owned(),
        ),
        LoadView::Unknown(_) => (
            "unknown".to_owned(),
            "?".to_owned(),
            UNKNOWN_CLASS,
            "Load unknown".to_owned(),
        ),
    };
    rsx! {
        button {
            class: if open { "chip open" } else { "chip" },
            aria_expanded: open,
            aria_label: "{label}",
            onclick: move |_| cockpit.update(|state| state.ui.toggle_popover(Popover::Load)),
            span { class: "chip-full",
                span { span { class: "chip-key", "Load" } " " span { class: "load-value {class}", "{value}" } }
                Icon { kind: IconKind::ChevronDown, size: 12 }
            }
            span { class: "chip-compact load-value {class}", "{short}" }
        }
    }
}

#[component]
pub fn LoadCard() -> Element {
    let cockpit: Cockpit = use_context();
    let view = cockpit.load.read();
    let state = cockpit.state.read();
    let body = match &*view {
        LoadView::Known(load) => known_load(cockpit, load, &state),
        LoadView::Waiting => rsx! {
            div { class: "card-section",
                span { class: "section-title loading-note",
                    "Server"
                    span { class: "spinner", role: "status", aria_label: "Measuring the load" }
                }
                for width in ["160px", "120px", "140px"] {
                    div { class: "load-line", aria_hidden: true,
                        span { class: "skeleton skeleton-line", style: "width: 54px" }
                        span { class: "skeleton", style: "height: 6px" }
                        span { class: "skeleton skeleton-line", style: "width: {width}" }
                    }
                }
            }
        },
        LoadView::Unknown(code) => rsx! {
            div { class: "card-section",
                span { class: "section-title", "Load unknown" }
                span { class: "muted small", {machine_load::unknown_note(*code)} }
            }
        },
    };
    rsx! {
        div { class: "popover load-card", {body} }
    }
}

fn known_load(cockpit: Cockpit, load: &MachineLoad, state: &AppState) -> Element {
    let server = &load.server;
    let cores = f64::from(server.cores);
    let cpu_percent = machine_load::cpu_percent(server);
    let cpu = format!("{:.0} / {} cores", server.cpu.get(), server.cores);
    let memory_percent = machine_load::memory_percent(server);
    let memory = format::memory_of(server.memory_used_bytes, server.memory_total_bytes);
    // Windows has no load average.
    let average = server.load_average.map(|average| {
        let percent = machine_load::percent_of(average.one_minute, cores);
        let text = format!(
            "{:.1}, {:.1}, {:.1}",
            average.one_minute, average.five_minutes, average.fifteen_minutes
        );
        (percent, text)
    });
    let lines = machine_load::agent_lines(load, state);
    let others_percent = machine_load::share_of_server(load.others.cpu, server);
    let others_class = LoadLevel::from_percent(others_percent).class();
    let others_cores = format::cores(load.others.cpu);
    let others_memory = format::memory(load.others.memory_bytes);
    let selected = state.ui.selected.and_then(|agent| {
        let measured = load.agents.iter().find(|found| found.agent == agent)?;
        let title = state.agents.get(&agent)?.summary.title.clone();
        Some((measured.clone(), title))
    });
    rsx! {
        div { class: "card-section",
            span { class: "section-title", "Server" }
            LoadBar { label: "CPU", percent: cpu_percent, value: cpu }
            LoadBar { label: "Memory", percent: memory_percent, value: memory }
            if let Some((percent, value)) = average {
                LoadBar { label: "Load avg", percent, value, title: "Over 1, 5 and 15 minutes" }
            }
        }
        div { class: "card-divider" }
        div { class: "card-section",
            span { class: "section-title", "Your agents" }
            if lines.is_empty() {
                span { class: "muted small", "No agent runs a process." }
            }
            div { class: "load-agents",
                for line in lines {
                    {agent_line(cockpit, line)}
                }
            }
        }
        div { class: "card-divider" }
        div { class: "card-section",
            span { class: "section-title", "Other processes on the server" }
            div { class: "load-row",
                span { class: "load-row-title muted", title: "Other users, services, and your processes outside this daemon", "Everything else" }
                div { class: "bar small", div { class: "bar-fill {others_class}", style: "width: {others_percent}%" } }
                span { class: "load-figure", "{others_cores} cores" }
                span { class: "load-figure", "{others_memory}" }
            }
        }
        if let Some((measured, title)) = selected {
            div { class: "card-divider" }
            Commands { measured, title }
        }
    }
}

fn agent_line(cockpit: Cockpit, line: AgentLine) -> Element {
    match line {
        AgentLine::Agent {
            agent,
            title,
            cpu,
            memory_bytes,
            bar_percent,
            heavy,
            selected,
        } => {
            let fill = if heavy {
                LoadLevel::Busy.class()
            } else {
                LoadLevel::Normal.class()
            };
            let cores = format::cores(cpu);
            let memory = format::memory(memory_bytes);
            rsx! {
                button {
                    key: "{agent.get()}",
                    class: if selected { "load-row load-agent selected" } else { "load-row load-agent" },
                    aria_current: selected,
                    title: "Open {title}",
                    onclick: move |_| select(cockpit, agent),
                    span { class: "load-row-title", "{title}" }
                    div { class: "bar small", div { class: "bar-fill {fill}", style: "width: {bar_percent}%" } }
                    span { class: "load-figure", "{cores} cores" }
                    span { class: "load-figure", "{memory}" }
                }
            }
        }
        AgentLine::Idle {
            count,
            memory_bytes,
        } => {
            let memory = format::memory(memory_bytes);
            rsx! {
                div { key: "idle-{count}", class: "load-row",
                    span { class: "load-row-title muted", "{count} idle agents" }
                    div { class: "bar small" }
                    span { class: "load-figure", "0.0 cores" }
                    span { class: "load-figure", "{memory}" }
                }
            }
        }
    }
}

fn select(cockpit: Cockpit, agent: AgentId) {
    cockpit.update(|state| machine_load::select_from_card(state, agent));
}

/// The selected agent's heaviest commands, each with a stop button.
#[component]
fn Commands(measured: AgentLoad, title: String) -> Element {
    let cockpit: Cockpit = use_context();
    let agent = measured.agent;
    rsx! {
        div { class: "card-section",
            span { class: "section-title load-commands-title", "{title}: heaviest commands" }
            if measured.heaviest.is_empty() {
                span { class: "muted small", "It runs no command." }
            }
            for command in measured.heaviest {
                div { key: "{command.pid.get()}", class: "load-command",
                    span { class: "load-command-line mono", title: "{command.command}", "{command.command}" }
                    span { class: "load-figure", "{format::cores(command.cpu)} cores" }
                    span { class: "load-figure", "{format::memory(command.memory_bytes)}" }
                    button {
                        class: "stop-button",
                        aria_label: "Stop {command.command}",
                        title: "Stop this command (SIGTERM to process {command.pid.get()})",
                        onclick: move |_| cockpit.change(|_| machine_load::stop_process(agent, command.pid)),
                        span { class: "stop-square", aria_hidden: true }
                    }
                }
            }
        }
    }
}

/// A line of the server section: its label, a bar colored by how full it is, and the figures.
#[component]
fn LoadBar(
    label: &'static str,
    percent: u8,
    value: String,
    #[props(default)] title: Option<&'static str>,
) -> Element {
    let class = LoadLevel::from_percent(percent).class();
    rsx! {
        div { class: "load-line", title,
            span { class: "load-label", "{label}" }
            div { class: "bar", div { class: "bar-fill {class}", style: "width: {percent}%" } }
            span { class: "load-figure mono", "{value}" }
        }
    }
}
