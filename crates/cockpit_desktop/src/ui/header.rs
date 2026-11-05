//! The center header: the agent's title, status and folder, the usage chip and the changes chip.
//! It stays on one line; when narrow, the title shortens, then the chips shrink to icons.

use std::path::PathBuf;
use std::time::SystemTime;

use cockpit_protocol::{
    AgentId, ChangedFile, ContextUsage, PlanUsage, RateLimitStatus, Timestamp, UsageWindow,
    WindowUsage,
};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::state::{AgentEntry, Popover, actions, file_edits};
use crate::ui::icons::{Icon, IconKind};

/// The shades of the context categories, darker for smaller parts.
const CATEGORY_SHADES: [&str; 5] = ["#ececec", "#a6a6a6", "#6b6b6b", "#4a4a4a", "#333333"];

/// The circumferences of the narrow usage icon's rings.
const OUTER_RING: f64 = 62.83;
const INNER_RING: f64 = 31.42;

#[component]
pub fn Header() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let Some(entry) = state.selected() else {
        return rsx! {
            header { class: "center-header",
                div { class: "header-title",
                    h1 { "No agent" }
                    span { class: "header-folder", "Ctrl+N starts one." }
                }
            }
        };
    };
    let agent = entry.id();
    let summary = &entry.summary;
    let status_class = format::status_class(summary.status);
    let status_label = format::status_label(summary.status);
    let plan = plan_percent(&state.plan_usage);
    let context = context_percent(entry);
    let files = changed_files(entry);
    let (added, removed) = line_totals(&files);
    let usage_open = state.ui.popover == Some(Popover::Usage);
    let changes_open = state.ui.popover == Some(Popover::Changes);
    let plan_text = plan.map_or_else(|| "?".to_owned(), |percent| format!("{percent}%"));
    let context_text = context.map_or_else(|| "?".to_owned(), |percent| format!("{percent}%"));
    let usage_label = format!("Usage: plan {plan_text}, context {context_text}");
    let changes_label = format!(
        "Changes: {}, {added} lines added, {removed} removed",
        format::count(files.len(), "file", "files")
    );
    let file_count = files.len();
    let folder = summary.folder.display().to_string();
    rsx! {
        header { class: "center-header",
            div { class: "header-title",
                div { class: "title-line",
                    h1 { "{summary.title}" }
                    span { class: "status-pill {status_class}", title: status_label,
                        span { class: "status-dot", aria_hidden: true }
                        span { class: "pill-label", "{status_label}" }
                    }
                }
                span { class: "header-folder mono", "{folder}" }
            }
            div { class: "chip-anchor",
                button {
                    class: if usage_open { "chip open" } else { "chip" },
                    aria_expanded: usage_open,
                    aria_label: "{usage_label}",
                    onclick: move |_| cockpit.change(|state| actions::toggle_usage(state, agent)),
                    span { class: "chip-full",
                        span { span { class: "chip-key", "Plan" } " {plan_text}" }
                        span { class: "chip-separator", aria_hidden: true }
                        span { span { class: "chip-key", "Context" } " {context_text}" }
                        Icon { kind: IconKind::ChevronDown, size: 12 }
                    }
                    span { class: "chip-compact", dangerous_inner_html: rings(plan, context) }
                }
                if usage_open {
                    UsageCard {}
                }
            }
            div { class: "chip-anchor",
                button {
                    class: if changes_open { "chip open" } else { "chip" },
                    aria_expanded: changes_open,
                    aria_label: "{changes_label}",
                    onclick: move |_| cockpit.update(|state| state.ui.toggle_popover(Popover::Changes)),
                    Icon { kind: IconKind::FileChanges, size: 14 }
                    span { class: "chip-full mono",
                        span { class: "added", "+{added}" }
                        " "
                        span { class: "removed", "-{removed}" }
                        Icon { kind: IconKind::ChevronDown, size: 12 }
                    }
                    if file_count > 0 {
                        span { class: "chip-compact chip-count", aria_hidden: true, "{file_count}" }
                    }
                }
                if changes_open {
                    ChangesList { agent, files: files.clone(), folder: summary.folder.clone() }
                }
            }
        }
    }
}

fn changed_files(entry: &AgentEntry) -> Vec<ChangedFile> {
    entry
        .details
        .loaded()
        .map(|details| details.changed_files.clone())
        .unwrap_or_default()
}

/// Summed in `u64`, so that no count the daemon sends overflows.
fn line_totals(files: &[ChangedFile]) -> (u64, u64) {
    let added = files.iter().map(|file| u64::from(file.lines_added)).sum();
    let removed = files.iter().map(|file| u64::from(file.lines_removed)).sum();
    (added, removed)
}

/// The five-hour window, which the chip shows; another window when Claude Code reported only
/// that one.
fn plan_percent(plan_usage: &PlanUsage) -> Option<u8> {
    let five_hour = plan_usage
        .windows
        .get(&UsageWindow::FiveHour)
        .and_then(|usage| usage.utilization);
    let any = || {
        plan_usage
            .windows
            .values()
            .find_map(|usage| usage.utilization)
    };
    five_hour.or_else(any).map(format::percent)
}

fn context_percent(entry: &AgentEntry) -> Option<u8> {
    let reported = entry
        .details
        .loaded()
        .and_then(|details| details.context.as_ref())
        .map(|context| context.percent);
    reported.or(entry.summary.context_percent)
}

/// The narrow chip: the plan on the outer ring, the agent's context on the inner one.
fn rings(plan: Option<u8>, context: Option<u8>) -> String {
    let outer = f64::from(plan.unwrap_or(0)) / 100.0 * OUTER_RING;
    let inner = f64::from(context.unwrap_or(0)) / 100.0 * INNER_RING;
    format!(
        r##"<svg width="22" height="22" viewBox="0 0 26 26" aria-hidden="true"><circle cx="13" cy="13" r="10" fill="none" stroke="#262626" stroke-width="3"></circle><circle cx="13" cy="13" r="10" fill="none" stroke="#d4d4d4" stroke-width="3" stroke-linecap="round" stroke-dasharray="{outer:.1} {OUTER_RING}" transform="rotate(-90 13 13)"></circle><circle cx="13" cy="13" r="5" fill="none" stroke="#262626" stroke-width="2.5"></circle><circle cx="13" cy="13" r="5" fill="none" stroke="#ececec" stroke-width="2.5" stroke-dasharray="{inner:.1} {INNER_RING}" transform="rotate(-90 13 13)"></circle></svg>"##
    )
}

#[component]
fn UsageCard() -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let now = now();
    let state = cockpit.state.read();
    let windows: Vec<(UsageWindow, WindowUsage)> = state
        .plan_usage
        .windows
        .iter()
        .map(|(window, usage)| (*window, *usage))
        .collect();
    let Some(entry) = state.selected() else {
        return rsx! {};
    };
    let summary = &entry.summary;
    let context = entry
        .details
        .loaded()
        .and_then(|details| details.context.clone());
    let context_line = match (&context, summary.context_percent) {
        (Some(context), _) => format!(
            "{} of {}, {}%",
            format::tokens(u64::from(context.tokens)),
            format::tokens(u64::from(context.max_tokens)),
            context.percent
        ),
        (None, Some(percent)) => format!("{percent}%"),
        (None, None) => "Not reported yet".to_owned(),
    };
    let (cache_text, cache_low) = cache_left(summary.cache_expires_at, now);
    let cost = format::cost(summary.cost);
    let turns = summary.turns;
    rsx! {
        div { class: "popover usage-card",
            div { class: "card-section",
                span { class: "section-title", "Plan, all agents" }
                if windows.is_empty() {
                    span { class: "muted small", "Claude Code has not reported the plan usage yet." }
                }
                for (window, usage) in windows {
                    WindowRow { window, usage, now }
                }
            }
            div { class: "card-divider" }
            div { class: "card-section",
                span { class: "section-title", "This agent" }
                div { class: "card-row", span { "Context" } span { "{context_line}" } }
                if let Some(context) = context {
                    ContextBar { context }
                }
                div { class: "card-row",
                    span { "Prompt cache" }
                    span { class: if cache_low { "warning" } else { "" }, "{cache_text}" }
                }
                div { class: "card-row", span { "Turns" } span { "{turns}" } }
                div { class: "card-row", span { "Cost at API prices" } span { "{cost}" } }
                span { class: "muted small", "An estimate. The plan does not charge per token." }
            }
        }
    }
}

/// "48 min left", or "expired"; and whether under ten minutes remain.
pub fn cache_left(expires_at: Option<Timestamp>, now: SystemTime) -> (String, bool) {
    const LOW_MINUTES: u64 = 10;
    match expires_at.and_then(|moment| format::minutes_until(moment, now)) {
        Some(minutes) => (format!("{minutes} min left"), minutes < LOW_MINUTES),
        None if expires_at.is_some() => ("expired".to_owned(), false),
        None => ("not started".to_owned(), false),
    }
}

#[component]
fn WindowRow(window: UsageWindow, usage: WindowUsage, now: SystemTime) -> Element {
    let percent = usage.utilization.map(format::percent);
    let shown = percent.map_or_else(|| "?".to_owned(), |percent| format!("{percent}%"));
    let width = percent.unwrap_or(0);
    let bar_class = match usage.status {
        RateLimitStatus::Allowed => "bar-fill",
        RateLimitStatus::AllowedWarning => "bar-fill warning",
        RateLimitStatus::Rejected => "bar-fill rejected",
    };
    let reset = usage
        .resets_at
        .and_then(|moment| format::reset_time(moment, now));
    let label = format::window_label(window);
    rsx! {
        div { class: "card-row", span { "{label}" } span { "{shown}" } }
        div { class: "bar", div { class: bar_class, style: "width: {width}%" } }
        if let Some(reset) = reset {
            span { class: "muted small", "{reset}" }
        }
    }
}

#[component]
fn ContextBar(context: ContextUsage) -> Element {
    let max = f64::from(context.max_tokens.max(1));
    let parts: Vec<(String, String, &'static str)> = context
        .categories
        .iter()
        .zip(CATEGORY_SHADES.iter().cycle())
        .map(|(category, shade)| {
            let width = f64::from(category.tokens) / max * 100.0;
            let label = format!(
                "{} {}",
                category.name,
                format::tokens(u64::from(category.tokens))
            );
            (format!("{width:.1}%"), label, *shade)
        })
        .collect();
    rsx! {
        div { class: "context-bar", aria_hidden: true,
            for (width, _, shade) in parts.clone() {
                div { style: "width: {width}; background: {shade}" }
            }
        }
        div { class: "context-legend",
            for (_, label, shade) in parts {
                span {
                    span { class: "legend-swatch", style: "background: {shade}" }
                    "{label}"
                }
            }
        }
    }
}

#[component]
fn ChangesList(agent: AgentId, files: Vec<ChangedFile>, folder: PathBuf) -> Element {
    let cockpit: Cockpit = use_context();
    let title = if files.is_empty() {
        "No file changed yet.".to_owned()
    } else {
        format!(
            "{} changed by this agent. Pick one to open it.",
            format::count(files.len(), "file", "files")
        )
    };
    rsx! {
        div { class: "popover changes-list",
            span { class: "changes-title", "{title}" }
            // Keyed by position: the daemon's list does not promise one entry per path.
            for (index, file) in files.into_iter().enumerate() {
                {
                    let shown = file_edits::relative_path(&folder, &file.path).display().to_string();
                    let path = file.path.clone();
                    rsx! {
                        button {
                            key: "{index}",
                            class: "file-row",
                            onclick: move |_| {
                                let path = path.clone();
                                cockpit.update(|state| actions::open_file(state, agent, path, None));
                            },
                            span { class: "mono file-path", "{shown}" }
                            span { class: "mono",
                                span { class: "added", "+{file.lines_added}" }
                                " "
                                span { class: "removed", "-{file.lines_removed}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, lines_added: u32, lines_removed: u32) -> ChangedFile {
        ChangedFile {
            path: PathBuf::from(path),
            lines_added,
            lines_removed,
        }
    }

    #[test]
    fn line_totals_hold_counts_past_u32() {
        let files = [file("a.rs", u32::MAX, 1), file("b.rs", u32::MAX, u32::MAX)];

        assert_eq!(
            line_totals(&files),
            (2 * u64::from(u32::MAX), u64::from(u32::MAX) + 1)
        );
    }
}
