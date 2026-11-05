//! The MCP servers block: the list, with hidden servers left out until "Show them", and a
//! server's detail with its actions, tools and removal.

use std::collections::BTreeSet;

use cockpit_protocol::{AgentId, McpServer, McpServerStatus, McpTool, McpTransport};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::format;
use crate::state::{McpView, ToolFilter, actions};
use crate::ui::icons::{Icon, IconKind};

/// The red dot on the folded block or the badge, and the block's one-line summary. A hidden
/// server counts for neither.
pub fn mcp_summary(
    alive: bool,
    servers: &[McpServer],
    hidden: &BTreeSet<String>,
) -> (bool, String) {
    if !alive {
        return (false, "not running".to_owned());
    }
    let visible = || {
        servers
            .iter()
            .filter(|server| !hidden.contains(&server.name))
    };
    let failed = visible()
        .filter(|server| server.status == McpServerStatus::Failed)
        .count();
    if failed > 0 {
        return (true, format!("{failed} failed"));
    }
    let connected = visible()
        .filter(|server| server.status == McpServerStatus::Connected)
        .count();
    (false, format!("{connected} connected"))
}

pub fn filter_tools(tools: &[McpTool], filter: ToolFilter) -> Vec<&McpTool> {
    tools
        .iter()
        .filter(|tool| match filter {
            ToolFilter::All => true,
            ToolFilter::ReadOnly => tool.read_only,
            ToolFilter::Destructive => tool.destructive,
            ToolFilter::Used => tool.calls > 0,
        })
        .collect()
}

const FILTERS: [(ToolFilter, &str); 4] = [
    (ToolFilter::All, "All"),
    (ToolFilter::ReadOnly, "Read-only"),
    (ToolFilter::Destructive, "Destructive"),
    (ToolFilter::Used, "Used here"),
];

#[component]
pub fn McpPanel(
    agent: AgentId,
    alive: bool,
    servers: Vec<McpServer>,
    hidden: BTreeSet<String>,
    view: McpView,
    machine: String,
) -> Element {
    if !alive {
        return rsx! {
            span { class: "block-note", "The agent is not running." }
        };
    }
    let selected = view
        .detail
        .as_ref()
        .and_then(|name| servers.iter().find(|server| &server.name == name))
        .cloned();
    if let Some(server) = selected {
        let is_hidden = hidden.contains(&server.name);
        return rsx! {
            McpDetail { agent, server, hidden: is_hidden, filter: view.filter, machine }
        };
    }
    rsx! {
        McpList { servers, hidden, show_hidden: view.show_hidden }
    }
}

#[component]
fn McpList(servers: Vec<McpServer>, hidden: BTreeSet<String>, show_hidden: bool) -> Element {
    let cockpit = use_context::<Cockpit>();
    let hidden_count = servers
        .iter()
        .filter(|server| hidden.contains(&server.name))
        .count();
    let shown: Vec<McpServer> = servers
        .into_iter()
        .filter(|server| show_hidden || !hidden.contains(&server.name))
        .collect();
    let hidden_text = if hidden_count > 0 {
        format!("{hidden_count} hidden from this view")
    } else {
        "No server hidden".to_owned()
    };
    rsx! {
        div { class: "mcp-list",
            if shown.is_empty() {
                span { class: "block-note", "Claude Code reports no MCP server." }
            }
            for server in shown {
                {
                    let is_hidden = hidden.contains(&server.name);
                    let status_class = format::mcp_status_class(server.status);
                    let detail = if server.status == McpServerStatus::Connected {
                        format::count(server.tools.len(), "tool", "tools")
                    } else {
                        format::mcp_status_label(server.status).to_owned()
                    };
                    let name = server.name.clone();
                    rsx! {
                        button {
                            key: "{server.name}",
                            class: if is_hidden { "mcp-row hidden-server {status_class}" } else { "mcp-row {status_class}" },
                            onclick: move |_| {
                                let name = name.clone();
                                cockpit.update(|state| {
                                    state.ui.mcp.detail = Some(name);
                                    state.ui.mcp.filter = ToolFilter::All;
                                });
                            },
                            span { class: "mcp-dot", "aria-hidden": "true" }
                            span { class: "mono", "{server.name}" }
                            if is_hidden {
                                span { class: "hidden-tag", "hidden" }
                            }
                            span { class: "spacer" }
                            span { class: "mcp-detail", "{detail}" }
                            Icon { kind: IconKind::ChevronRight, size: 12 }
                        }
                    }
                }
            }
            div { class: "mcp-foot",
                span { class: "spacer", "{hidden_text}" }
                if hidden_count > 0 {
                    button {
                        class: "link-button",
                        onclick: move |_| cockpit.update(|state| state.ui.mcp.show_hidden = !state.ui.mcp.show_hidden),
                        if show_hidden { "Hide them" } else { "Show them" }
                    }
                }
            }
        }
    }
}

#[component]
fn McpDetail(
    agent: AgentId,
    server: McpServer,
    hidden: bool,
    filter: ToolFilter,
    machine: String,
) -> Element {
    let cockpit = use_context::<Cockpit>();
    let mut confirming = use_signal(|| false);
    let name = server.name.clone();
    let status = server.status;
    let disabled = status == McpServerStatus::Disabled;
    let run_line = match &server.transport {
        Some(McpTransport::Stdio { command }) => Some(format!("Runs {command}")),
        Some(McpTransport::Http { url } | McpTransport::Sse { url }) => Some(format!("URL {url}")),
        None => None,
    };
    let scope_line = server.scope.map(|scope| match scope {
        cockpit_protocol::McpScope::ClaudeAi => "Comes from your claude.ai account".to_owned(),
        scope => format!("Configured in the {} scope", format::scope_word(scope)),
    });
    let version = server
        .version
        .clone()
        .filter(|_| status == McpServerStatus::Connected);
    let read_only = server.tools.iter().filter(|tool| tool.read_only).count();
    let destructive = server.tools.iter().filter(|tool| tool.destructive).count();
    let tools_summary = format!(
        "{}, {read_only} read-only and {destructive} destructive. \"Used here\" counts the calls of this agent.",
        format::count(server.tools.len(), "tool", "tools")
    );
    let filters: Vec<(ToolFilter, String)> = FILTERS
        .iter()
        .map(|(kind, label)| {
            let count = filter_tools(&server.tools, *kind).len();
            (*kind, format!("{label} {count}"))
        })
        .collect();
    let shown_tools: Vec<McpTool> = filter_tools(&server.tools, filter)
        .into_iter()
        .cloned()
        .collect();
    let no_tools = if status == McpServerStatus::Connected {
        "This server offers no tools."
    } else {
        "Claude Code lists the tools once the server connects."
    };
    let removable_scope = server.scope.filter(|scope| format::removable_scope(*scope));
    let status_class = format::mcp_status_class(status);
    let status_label = format::mcp_status_label(status);
    let toggle_label = if disabled {
        "Enable for this agent"
    } else {
        "Disable for this agent"
    };
    let hide_label = if hidden {
        "Show in my view"
    } else {
        "Hide from my view"
    };
    let (reconnect_name, toggle_name, hide_name, remove_name) =
        (name.clone(), name.clone(), name.clone(), name.clone());
    rsx! {
        div { class: "mcp-detail-view",
            button {
                class: "back-button",
                onclick: move |_| cockpit.update(|state| state.ui.mcp.detail = None),
                Icon { kind: IconKind::ChevronLeft, size: 12 }
                "All servers"
            }
            div { class: "mcp-title-row",
                span { class: "mcp-name mono", "{name}" }
                span { class: "mcp-chip {status_class}", "{status_label}" }
                span { class: "tooltip-anchor",
                    button {
                        class: "icon-button framed",
                        "aria-label": "Reconnect",
                        onclick: move |_| cockpit.change(|_| actions::mcp_reconnect(agent, reconnect_name.clone())),
                        Icon { kind: IconKind::Reconnect, size: 14 }
                    }
                    span { class: "tooltip", "role": "tooltip", "Reconnect" }
                }
                span { class: "tooltip-anchor",
                    button {
                        class: if disabled { "icon-button framed toggled-warning" } else { "icon-button framed" },
                        "aria-label": "{toggle_label}",
                        "aria-pressed": if disabled { "true" } else { "false" },
                        onclick: move |_| cockpit.change(|_| actions::mcp_toggle(agent, toggle_name.clone(), disabled)),
                        Icon { kind: IconKind::Power, size: 14 }
                    }
                    span { class: "tooltip", "role": "tooltip", "{toggle_label}" }
                }
                span { class: "tooltip-anchor",
                    button {
                        class: if hidden { "icon-button framed toggled" } else { "icon-button framed" },
                        "aria-label": "{hide_label}",
                        "aria-pressed": if hidden { "true" } else { "false" },
                        onclick: move |_| cockpit.change(|state| actions::set_mcp_hidden(state, &hide_name, !hidden)),
                        if hidden {
                            Icon { kind: IconKind::Eye, size: 14 }
                        } else {
                            Icon { kind: IconKind::EyeOff, size: 14 }
                        }
                    }
                    span { class: "tooltip", "role": "tooltip", "{hide_label}" }
                }
            }
            div { class: "mcp-facts",
                if let Some(version) = version {
                    span { "{version}" }
                }
                if let Some(run_line) = run_line {
                    span { class: "mono", "{run_line}" }
                }
                if let Some(scope_line) = scope_line {
                    span { "{scope_line}" }
                }
            }
            if status == McpServerStatus::Failed {
                if let Some(error) = &server.error {
                    div { class: "error-box", "{error}" }
                }
            }
            if status == McpServerStatus::NeedsAuth {
                div { class: "warning-box",
                    "Sign in once with /mcp in a terminal on {machine}. Every agent then uses that sign-in."
                }
            }
            if hidden {
                span { class: "block-note",
                    "Hidden in every agent. It still runs, but the list and the red dot ignore it, even when it fails."
                }
            }
            if server.tools.is_empty() {
                span { class: "block-note", "{no_tools}" }
            } else {
                div { class: "mcp-tools",
                    span { class: "block-note", "{tools_summary}" }
                    div { class: "tool-filters", "role": "group", "aria-label": "Filter tools",
                        for (kind, label) in filters {
                            button {
                                key: "{label}",
                                class: if kind == filter { "filter chosen" } else { "filter" },
                                "aria-pressed": if kind == filter { "true" } else { "false" },
                                onclick: move |_| cockpit.update(|state| state.ui.mcp.filter = kind),
                                "{label}"
                            }
                        }
                    }
                    div { class: "tool-list scroll",
                        for tool in shown_tools {
                            ToolRow { key: "{tool.name}", tool }
                        }
                    }
                }
            }
            div { class: "mcp-remove",
                match removable_scope {
                    Some(scope) => {
                        let word = format::scope_word(scope);
                        let remove_name = remove_name.clone();
                        rsx! {
                            if confirming() {
                                span { class: "block-note", "Remove {name} from the {word} config? Every agent that reads this config loses the server." }
                                div { class: "card-actions",
                                    button {
                                        class: "button danger small",
                                        onclick: move |_| {
                                            confirming.set(false);
                                            cockpit.change(|_| actions::mcp_remove(agent, remove_name.clone(), scope));
                                        },
                                        "Remove"
                                    }
                                    button { class: "button small", onclick: move |_| confirming.set(false), "Cancel" }
                                }
                            } else {
                                button { class: "link-button danger", onclick: move |_| confirming.set(true), "Remove from the {word} config" }
                                span { class: "block-note small", "Runs claude mcp remove {name} -s {word}. Every agent that reads this config loses the server." }
                            }
                        }
                    }
                    None => rsx! {
                        span { class: "block-note small",
                            match server.scope {
                                Some(cockpit_protocol::McpScope::ClaudeAi) => "It comes from your claude.ai account, so it cannot be removed here.".to_owned(),
                                Some(scope) => format!("It comes from the {} configuration, so it cannot be removed here.", format::scope_word(scope)),
                                None => "Its scope is unknown, so it cannot be removed here.".to_owned(),
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn ToolRow(tool: McpTool) -> Element {
    let used = (tool.calls > 0).then(|| {
        if tool.calls == 1 {
            "used 1 time".to_owned()
        } else {
            format!("used {} times", tool.calls)
        }
    });
    let chip = if tool.destructive {
        Some(("tool-chip destructive", "destructive"))
    } else if tool.read_only {
        Some(("tool-chip read-only", "read-only"))
    } else {
        None
    };
    rsx! {
        div { class: "tool-row",
            div { class: "tool-row-head",
                span { class: "mono tool-row-name", "{tool.name}" }
                if let Some(used) = used {
                    span { class: "tool-used", "{used}" }
                }
                if let Some((class, label)) = chip {
                    span { class, "{label}" }
                }
            }
            if let Some(description) = &tool.description {
                span { class: "tool-description", "{description}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(name: &str, status: McpServerStatus) -> McpServer {
        McpServer {
            name: name.to_owned(),
            status,
            error: None,
            version: None,
            transport: None,
            scope: None,
            tools: Vec::new(),
        }
    }

    fn tool(name: &str, read_only: bool, destructive: bool, calls: u32) -> McpTool {
        McpTool {
            name: name.to_owned(),
            description: None,
            read_only,
            destructive,
            open_world: false,
            calls,
        }
    }

    #[test]
    fn a_hidden_failing_server_raises_no_alert() {
        let servers = [
            server("search", McpServerStatus::Connected),
            server("tickets", McpServerStatus::Failed),
        ];
        let mut hidden = BTreeSet::new();

        assert_eq!(
            mcp_summary(true, &servers, &hidden),
            (true, "1 failed".to_owned())
        );
        hidden.insert("tickets".to_owned());
        assert_eq!(
            mcp_summary(true, &servers, &hidden),
            (false, "1 connected".to_owned())
        );
        assert_eq!(
            mcp_summary(false, &servers, &hidden),
            (false, "not running".to_owned())
        );
    }

    #[test]
    fn tool_filters_pick_by_annotation_and_use() {
        let tools = [
            tool("read_page", true, false, 2),
            tool("delete_page", false, true, 0),
            tool("ping", false, false, 0),
        ];
        let names = |filter| -> Vec<&str> {
            filter_tools(&tools, filter)
                .iter()
                .map(|tool| tool.name.as_str())
                .collect()
        };

        assert_eq!(names(ToolFilter::All).len(), 3);
        assert_eq!(names(ToolFilter::ReadOnly), ["read_page"]);
        assert_eq!(names(ToolFilter::Destructive), ["delete_page"]);
        assert_eq!(names(ToolFilter::Used), ["read_page"]);
    }
}
