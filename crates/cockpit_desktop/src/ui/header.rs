//! The center header: the agent's title, status and folder, the usage chip, the load chip and the
//! changes chip. It stays on one line; when narrow, the title shortens, then the chips shrink to
//! icons.

use std::time::SystemTime;

use cockpit_protocol::{
    ChangedFile, ContextUsage, MAX_TITLE_CHARS, RateLimitStatus, Timestamp, UsageWindow,
    WindowUsage,
};
use dioxus::prelude::*;
use strum::{EnumString, IntoStaticStr, VariantArray};

use crate::connection::{Clock, Cockpit};
use crate::state::accounts::{self, Billing, PlanChip};
use crate::state::{AgentEntry, Loadable, Popover, TitlePlace, actions, title_emoji};
use crate::ui::accounts::{AccountDot, AccountTag, PlanAccounts};
use crate::ui::changes_list::{self, ChangesList};
use crate::ui::emoji_picker::EmojiPicker;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::load_card::{LoadCard, LoadChip};
use crate::ui::presence::Presence;
use crate::{bridge, format, path_style};

const AT_LIMIT_PERCENT: u8 = 100;

/// How close a plan window is to its limit, which colors its bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowLevel {
    Normal,
    CloseToLimit,
    AtLimit,
}

impl WindowLevel {
    /// From 80 %, or when Claude Code warns, the window is close to its limit; at 100 %, or when
    /// Claude Code refuses, it is at its limit.
    fn new(usage: &WindowUsage) -> Self {
        let percent = usage.utilization.map(format::percent).unwrap_or(0);
        match usage.status {
            RateLimitStatus::Rejected => Self::AtLimit,
            _ if percent >= AT_LIMIT_PERCENT => Self::AtLimit,
            RateLimitStatus::AllowedWarning => Self::CloseToLimit,
            _ if percent >= format::CLOSE_TO_LIMIT_PERCENT => Self::CloseToLimit,
            RateLimitStatus::Allowed => Self::Normal,
        }
    }

    fn bar_class(self) -> &'static str {
        match self {
            Self::Normal => "bar-fill accent",
            Self::CloseToLimit => "bar-fill warning",
            Self::AtLimit => "bar-fill rejected",
        }
    }
}

/// A part of the context, by the name Claude Code gives it. Each part keeps its color whatever
/// parts the agent reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString, IntoStaticStr, VariantArray)]
#[strum(ascii_case_insensitive)]
enum ContextPart {
    #[strum(serialize = "System prompt")]
    SystemPrompt,
    #[strum(serialize = "System tools")]
    SystemTools,
    #[strum(serialize = "MCP tools")]
    McpTools,
    #[strum(serialize = "MCP server instructions")]
    McpServerInstructions,
    #[strum(serialize = "Custom agents")]
    CustomAgents,
    #[strum(serialize = "Memory files")]
    MemoryFiles,
    #[strum(serialize = "Skills")]
    Skills,
    #[strum(serialize = "Messages")]
    Messages,
    #[strum(serialize = "Autocompact buffer")]
    AutocompactBuffer,
    #[strum(serialize = "Free space")]
    FreeSpace,
}

impl ContextPart {
    /// The class that colors the part in the bar and the legend.
    fn class(self) -> &'static str {
        match self {
            Self::SystemPrompt => "context-system-prompt",
            Self::SystemTools => "context-system-tools",
            Self::McpTools => "context-mcp-tools",
            Self::McpServerInstructions => "context-mcp-instructions",
            Self::CustomAgents => "context-custom-agents",
            Self::MemoryFiles => "context-memory-files",
            Self::Skills => "context-skills",
            Self::Messages => "context-messages",
            Self::AutocompactBuffer => "context-autocompact",
            Self::FreeSpace => "context-free",
        }
    }
}

/// The class of a part Claude Code names in a way this app does not know: a neutral grey.
const OTHER_PART_CLASS: &str = "context-other";

fn part_class(name: &str) -> &'static str {
    name.parse::<ContextPart>()
        .map_or(OTHER_PART_CLASS, ContextPart::class)
}

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
    let account = summary.account;
    let account_label = accounts::label(&state, account);
    let chip = accounts::plan_chip(&state, account);
    let plan = match chip {
        PlanChip::Percent(percent) => percent,
        PlanChip::Environment | PlanChip::NotSignedIn => None,
    };
    let context = context_percent(entry);
    let files = changed_files(entry);
    let (added, removed) = changes_list::line_totals(&files);
    let usage_open = state.ui.popover == Some(Popover::Usage);
    let load_open = state.ui.popover == Some(Popover::Load);
    let emoji_open = state.ui.popover == Some(Popover::TitleEmoji);
    let (title_emoji, title_rest) = match title_emoji::split(&summary.title) {
        Some((emoji, rest)) => (Some(emoji.to_owned()), rest.to_owned()),
        None => (None, summary.title.clone()),
    };
    let changes_open = state.ui.popover == Some(Popover::Changes);
    let plan_text = plan.map_or_else(|| "?".to_owned(), |percent| format!("{percent}%"));
    let context_text = context.map_or_else(|| "?".to_owned(), |percent| format!("{percent}%"));
    let plan_words = match chip {
        PlanChip::Percent(_) => format!("plan {plan_text}"),
        PlanChip::Environment => "API key".to_owned(),
        PlanChip::NotSignedIn => "not signed in".to_owned(),
    };
    let account_words = account_label
        .as_ref()
        .map(|label| format!(" of {}", label.name))
        .unwrap_or_default();
    let usage_label = format!("Usage: {plan_words}{account_words}, context {context_text}");
    let changes_label = format!(
        "Changes: {}, {added} lines added, {removed} removed",
        format::count(files.len(), "file", "files")
    );
    let file_count = files.len();
    let folder = path_style::shown(&summary.folder);
    let title_edit = state
        .ui
        .title_edit
        .as_ref()
        .filter(|edit| edit.agent == agent && edit.place == TitlePlace::Header)
        .map(|edit| (edit.title.clone(), edit.select_all));
    let start_rename = move || {
        cockpit.change_with_drafts(|state, drafts| {
            actions::start_rename(state, drafts, agent);
            Vec::new()
        });
    };
    let toggle_emoji = move || {
        cockpit.update(|state| state.ui.toggle_popover(Popover::TitleEmoji));
    };
    rsx! {
        header { class: "center-header",
            div { class: "header-title",
                div { class: "title-line",
                    if let Some((title, select_all)) = title_edit {
                        input {
                            id: bridge::HEADER_TITLE_FIELD_ID,
                            class: "title-field",
                            r#type: "text",
                            aria_label: "Agent title",
                            // Follows the keyboard alone: a render that lags behind typing would
                            // put an older title back.
                            initial_value: "{title}",
                            maxlength: "{MAX_TITLE_CHARS}",
                            onmounted: move |_| bridge::focus_title_field(bridge::HEADER_TITLE_FIELD_ID, select_all),
                            oninput: move |event: FormEvent| cockpit.type_title(event.value()),
                            onkeydown: move |event: KeyboardEvent| {
                                if event.key() == Key::Enter {
                                    cockpit.change_with_drafts(actions::finish_rename);
                                }
                            },
                            onfocus: move |_| cockpit.title_field_focused(),
                            onblur: move |_| cockpit.title_field_blurred(TitlePlace::Header),
                        }
                    } else {
                        h1 {
                            ondoubleclick: move |_| start_rename(),
                            if let Some(emoji) = &title_emoji {
                                span {
                                    class: "title-emoji",
                                    title: "Change the emoji",
                                    onclick: move |_| toggle_emoji(),
                                    "{emoji}"
                                }
                            }
                            "{title_rest}"
                        }
                        button {
                            class: "icon-button tiny rename-button",
                            aria_label: "Rename",
                            title: "Rename",
                            onclick: move |_| start_rename(),
                            Icon { kind: IconKind::Pencil, size: 13 }
                        }
                        button {
                            class: if emoji_open { "icon-button tiny rename-button active" } else { "icon-button tiny rename-button" },
                            aria_label: "Set the title's emoji",
                            aria_expanded: emoji_open,
                            title: "Set the title's emoji",
                            onclick: move |_| toggle_emoji(),
                            Icon { kind: IconKind::Smile, size: 13 }
                        }
                    }
                    span { class: "status-pill {status_class}", title: status_label,
                        span { class: "status-dot", aria_hidden: true }
                        span { class: "pill-label", "{status_label}" }
                    }
                    if let Some(label) = account_label.clone() {
                        AccountTag { label }
                    }
                }
                span { class: "header-folder", "{folder}" }
                Presence { open: emoji_open,
                    EmojiPicker { agent, current: title_emoji.clone() }
                }
            }
            div { class: "chip-anchor",
                button {
                    class: if usage_open { "chip open" } else { "chip" },
                    aria_expanded: usage_open,
                    aria_label: "{usage_label}",
                    onclick: move |_| cockpit.change(|state| actions::toggle_usage(state, agent)),
                    span { class: "chip-full",
                        if let Some(label) = &account_label {
                            AccountDot { color: label.color }
                        }
                        match chip {
                            PlanChip::Percent(_) => rsx! {
                                span { span { class: "chip-key", "Plan" } " {plan_text}" }
                            },
                            PlanChip::Environment => rsx! {
                                span { class: "chip-key", "API key" }
                            },
                            PlanChip::NotSignedIn => rsx! {
                                span { span { class: "chip-key", "Plan" } " " span { class: "warning", "?" } }
                            },
                        }
                        span { class: "chip-separator", aria_hidden: true }
                        span { span { class: "chip-key", "Context" } " {context_text}" }
                        Icon { kind: IconKind::ChevronDown, size: 12 }
                    }
                    span { class: "chip-compact", dangerous_inner_html: rings(plan, context) }
                }
                Presence { open: usage_open, UsageCard {} }
            }
            div { class: "chip-anchor",
                LoadChip { open: load_open }
                Presence { open: load_open, LoadCard {} }
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
                Presence { open: changes_open,
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
        r##"<svg width="22" height="22" viewBox="0 0 26 26" aria-hidden="true"><circle cx="13" cy="13" r="10" fill="none" stroke="#323232" stroke-width="3"></circle><circle cx="13" cy="13" r="10" fill="none" stroke="#e0e0e0" stroke-width="3" stroke-linecap="round" stroke-dasharray="{outer:.1} {OUTER_RING}" transform="rotate(-90 13 13)"></circle><circle cx="13" cy="13" r="5" fill="none" stroke="#323232" stroke-width="2.5"></circle><circle cx="13" cy="13" r="5" fill="none" stroke="#f9f9f9" stroke-width="2.5" stroke-dasharray="{inner:.1} {INNER_RING}" transform="rotate(-90 13 13)"></circle></svg>"##
    )
}

#[component]
fn UsageCard() -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let now = now();
    let state = cockpit.state.read();
    let Some(entry) = state.selected() else {
        return rsx! {};
    };
    let agent_account = entry.summary.account;
    let several = accounts::shows_labels(&state);
    let windows: Vec<(UsageWindow, WindowUsage)> = accounts::plan_usage(&state, agent_account)
        .map(|usage| {
            usage
                .windows
                .iter()
                .map(|(window, usage)| (*window, *usage))
                .collect()
        })
        .unwrap_or_default();
    let billed_to_key = accounts::billing(&state, agent_account) == Billing::Environment;
    // Before the connection, the plan's windows are unknown, not missing.
    let plan_loading = !state.is_connected();
    let summary = &entry.summary;
    let context_loading = entry.context_loading
        || matches!(entry.details, Loadable::NotRequested | Loadable::Loading);
    let context = entry
        .details
        .loaded()
        .and_then(|details| details.context.clone());
    let context_unknown = context.is_none() && context_loading;
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
        div { class: if several { "popover usage-card accounts-card" } else { "popover usage-card" },
            if several {
                PlanAccounts { agent_account }
            } else {
                div { class: "card-section",
                    span { class: "section-title", "Plan, all agents" }
                    if windows.is_empty() && plan_loading {
                        for width in ["104px", "92px"] {
                            div { class: "card-row", aria_hidden: true,
                                span { class: "skeleton skeleton-line", style: "width: {width}" }
                                span { class: "skeleton skeleton-line", style: "width: 30px" }
                            }
                            span { class: "skeleton", style: "height: 6px", aria_hidden: true }
                        }
                    } else if windows.is_empty() {
                        span { class: "muted small", "Claude Code has not reported the plan usage yet." }
                    }
                    for (window, usage) in windows {
                        WindowRow { window, usage, now }
                    }
                }
            }
            div { class: "card-divider" }
            div { class: "card-section",
                span { class: "section-title loading-note",
                    "This agent"
                    if context_loading {
                        span { class: "spinner", role: "status", aria_label: "Loading the context usage" }
                    }
                }
                div { class: "card-row",
                    span { "Context" }
                    if context_unknown {
                        span { class: "skeleton skeleton-line", style: "width: 120px", aria_hidden: true }
                    } else {
                        span { "{context_line}" }
                    }
                }
                if let Some(context) = context {
                    ContextBar { context }
                } else if context_unknown {
                    span { class: "skeleton", style: "height: 8px", aria_hidden: true }
                }
                div { class: "card-row",
                    span { "Prompt cache" }
                    span { class: if cache_low { "warning" } else { "" }, "{cache_text}" }
                }
                div { class: "card-row", span { "Turns" } span { "{turns}" } }
                div { class: "card-row", span { "Cost at API prices" } span { "{cost}" } }
                if billed_to_key {
                    span { class: "muted small", "Billed to the API key at these prices." }
                } else {
                    span { class: "muted small", "An estimate. The plan does not charge per token." }
                }
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
    let bar_class = WindowLevel::new(&usage).bar_class();
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

/// A part of the context bar and its legend.
#[derive(Debug, Clone, PartialEq)]
struct BarPart {
    class: &'static str,
    tokens: u32,
    label: String,
}

#[component]
fn ContextBar(context: ContextUsage) -> Element {
    let parts: Vec<BarPart> = context
        .categories
        .iter()
        .map(|category| BarPart {
            class: part_class(&category.name),
            tokens: category.tokens,
            label: format!(
                "{} {}",
                category.name,
                format::tokens(u64::from(category.tokens))
            ),
        })
        .collect();
    let reported: u64 = parts.iter().map(|part| u64::from(part.tokens)).sum();
    // Room the parts leave in the window, when Claude Code does not report the free space.
    let rest = u64::from(context.max_tokens).saturating_sub(reported);
    rsx! {
        div { class: "context-bar", aria_hidden: true,
            for part in parts.clone() {
                div {
                    class: "context-part {part.class}",
                    style: "flex-grow: {part.tokens}",
                    title: "{part.label}",
                }
            }
            if rest > 0 {
                div { class: "context-rest", style: "flex-grow: {rest}" }
            }
        }
        div { class: "context-legend",
            for part in parts {
                span { title: "{part.label}",
                    span { class: "legend-swatch {part.class}" }
                    span { class: "legend-label", "{part.label}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude Code writes these names: a rename here would leave a part grey.
    #[test]
    fn the_context_part_names_are_pinned() {
        let names: Vec<&str> = ContextPart::VARIANTS
            .iter()
            .map(|&part| part.into())
            .collect();

        assert_eq!(
            names,
            [
                "System prompt",
                "System tools",
                "MCP tools",
                "MCP server instructions",
                "Custom agents",
                "Memory files",
                "Skills",
                "Messages",
                "Autocompact buffer",
                "Free space",
            ]
        );
        assert_eq!(part_class("memory files"), "context-memory-files");
        assert_eq!(part_class("Deferred tools"), OTHER_PART_CLASS);
    }

    #[test]
    fn a_plan_window_turns_amber_near_its_limit_and_red_at_it() {
        let level = |utilization: f64, status: RateLimitStatus| {
            WindowLevel::new(&WindowUsage {
                utilization: Some(utilization),
                resets_at: None,
                status,
            })
        };

        assert_eq!(level(0.42, RateLimitStatus::Allowed), WindowLevel::Normal);
        assert_eq!(level(0.79, RateLimitStatus::Allowed), WindowLevel::Normal);
        assert_eq!(
            level(0.8, RateLimitStatus::Allowed),
            WindowLevel::CloseToLimit
        );
        assert_eq!(
            level(0.3, RateLimitStatus::AllowedWarning),
            WindowLevel::CloseToLimit
        );
        assert_eq!(
            level(1.0, RateLimitStatus::AllowedWarning),
            WindowLevel::AtLimit
        );
        assert_eq!(level(0.5, RateLimitStatus::Rejected), WindowLevel::AtLimit);
    }
}
