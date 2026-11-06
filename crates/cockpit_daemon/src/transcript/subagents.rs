//! The agent map's subagents: each starts with its call, counts the calls of its own lines, and
//! ends with its call's result or its task's end.
//!
//! Claude Code may send no task events for a subagent, so a subagent without them still shows
//! its calls and its time, with no tokens.

use std::collections::BTreeSet;

use claude_protocol::{AgentInput, TaskUsage};
use cockpit_protocol::{
    BackgroundTaskId, BackgroundTaskStatus, ItemId, Subagent, SubagentRun, SubagentStatus,
    SubagentToolCall, Timestamp,
};

use crate::transcript::tools;

/// Subagents kept in the details; the oldest ended ones go first.
const KEPT_SUBAGENTS: usize = 200;

/// A subagent's call, as it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Started {
    pub(super) item: ItemId,
    pub(super) parent: Option<ItemId>,
    /// The call's summary, for a call whose input gives no description.
    pub(super) summary: String,
    pub(super) input: AgentInput,
    pub(super) turn: u32,
    pub(super) now: Timestamp,
}

/// How a subagent ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Ended {
    pub(super) status: SubagentStatus,
    pub(super) error: Option<String>,
    pub(super) now: Option<Timestamp>,
}

pub(super) fn start(subagents: &mut Vec<Subagent>, started: Started) {
    let Started {
        item,
        parent,
        summary,
        input,
        turn,
        now,
    } = started;
    let description = if input.description.trim().is_empty() {
        summary
    } else {
        tools::one_line(&input.description)
    };
    let run = if input.run_in_background {
        SubagentRun::Background
    } else {
        SubagentRun::Foreground
    };
    subagents.retain(|subagent| subagent.item != item);
    subagents.push(Subagent {
        item,
        parent,
        description,
        agent_type: input
            .subagent_type
            .map(|kind| tools::one_line(&kind))
            .filter(|kind| !kind.is_empty()),
        run,
        status: SubagentStatus::Running,
        started_at: now,
        ended_at: None,
        tokens: None,
        tool_calls: 0,
        last_tool: None,
        error: None,
        turn,
        task: None,
    });
    prune(subagents);
}

pub(super) fn find_mut(subagents: &mut [Subagent], item: ItemId) -> Option<&mut Subagent> {
    subagents.iter_mut().find(|subagent| subagent.item == item)
}

pub(super) fn find_task_mut<'list>(
    subagents: &'list mut [Subagent],
    task: &BackgroundTaskId,
) -> Option<&'list mut Subagent> {
    subagents
        .iter_mut()
        .find(|subagent| subagent.task.as_ref() == Some(task))
}

/// A call made by the subagent of item `owner`. False when no such subagent runs.
pub(super) fn note_call(subagents: &mut [Subagent], owner: ItemId, call: SubagentToolCall) -> bool {
    let Some(subagent) = find_mut(subagents, owner) else {
        return false;
    };
    subagent.tool_calls = subagent.tool_calls.saturating_add(1);
    subagent.last_tool = Some(call);
    true
}

/// Claude Code's report of the subagent's usage and its latest tool. True when it changed what
/// the map shows.
pub(super) fn note_progress(
    subagent: &mut Subagent,
    usage: Option<TaskUsage>,
    last_tool: Option<String>,
) -> bool {
    let before = subagent.clone();
    if let Some(usage) = usage {
        subagent.tokens = Some(usage.total_tokens);
        // The subagent's own lines may be counted already, or may never come.
        subagent.tool_calls = subagent.tool_calls.max(usage.tool_uses);
    }
    let named = last_tool.filter(|tool| !tool.is_empty());
    if let Some(tool) = named
        && subagent.last_tool.as_ref().map(|call| &call.tool) != Some(&tool)
    {
        subagent.last_tool = Some(SubagentToolCall {
            item: None,
            tool,
            summary: String::new(),
        });
    }
    *subagent != before
}

/// Ends the subagent of `item`, and the foreground subagents under it that still run, which
/// cannot outlive it. False when nothing ran.
pub(super) fn end(subagents: &mut [Subagent], item: ItemId, ended: &Ended) -> bool {
    let Some(subagent) = find_mut(subagents, item) else {
        return false;
    };
    if !subagent.status.is_running() {
        return false;
    }
    finish(subagent, ended);
    let mut ended_items = vec![item];
    let mut changed = true;
    while let Some(parent) = ended_items.pop() {
        for child in subagents.iter_mut() {
            let under = child.parent == Some(parent);
            if under && child.run == SubagentRun::Foreground && child.status.is_running() {
                finish(
                    child,
                    &Ended {
                        status: SubagentStatus::Stopped,
                        error: None,
                        now: ended.now,
                    },
                );
                ended_items.push(child.item);
                changed = true;
            }
        }
    }
    changed
}

fn finish(subagent: &mut Subagent, ended: &Ended) {
    subagent.status = ended.status;
    subagent.ended_at = ended.now.or(subagent.ended_at);
    if ended.status == SubagentStatus::Failed {
        subagent.error = ended.error.as_deref().map(tools::one_line);
    }
}

/// Ends every subagent that still runs. True when one ran.
pub(super) fn end_running(subagents: &mut [Subagent], ended: &Ended) -> bool {
    let mut changed = false;
    for subagent in subagents.iter_mut() {
        if subagent.status.is_running() {
            finish(subagent, ended);
            changed = true;
        }
    }
    changed
}

/// Ends the foreground subagents of the agent's turn that still run. A foreground subagent under
/// a background one belongs to that one, which outlives the turn. True when one ran.
pub(super) fn end_turn(subagents: &mut [Subagent], ended: &Ended) -> bool {
    // A subagent starts from its parent's lines, so its parent comes before it.
    let mut in_background = BTreeSet::new();
    let mut changed = false;
    for subagent in subagents.iter_mut() {
        let under_background = subagent
            .parent
            .is_some_and(|parent| in_background.contains(&parent));
        if subagent.run == SubagentRun::Background || under_background {
            in_background.insert(subagent.item);
        } else if subagent.status.is_running() {
            finish(subagent, ended);
            changed = true;
        }
    }
    changed
}

/// Marks waiting for you the running subagents in `waiting`, and running again the others.
/// True when one changed.
pub(super) fn mark_waiting(subagents: &mut [Subagent], waiting: &BTreeSet<ItemId>) -> bool {
    let mut changed = false;
    for subagent in subagents.iter_mut() {
        let status = match subagent.status {
            SubagentStatus::Running | SubagentStatus::NeedsYou => {
                if waiting.contains(&subagent.item) {
                    SubagentStatus::NeedsYou
                } else {
                    SubagentStatus::Running
                }
            }
            SubagentStatus::Done | SubagentStatus::Failed | SubagentStatus::Stopped => continue,
        };
        if subagent.status != status {
            subagent.status = status;
            changed = true;
        }
    }
    changed
}

pub(super) fn status_from_task(status: BackgroundTaskStatus) -> Option<SubagentStatus> {
    match status {
        BackgroundTaskStatus::Running => None,
        BackgroundTaskStatus::Done => Some(SubagentStatus::Done),
        BackgroundTaskStatus::Failed => Some(SubagentStatus::Failed),
        BackgroundTaskStatus::Stopped => Some(SubagentStatus::Stopped),
    }
}

/// Keeps every running subagent and the latest ended ones, `KEPT_SUBAGENTS` in all.
fn prune(subagents: &mut Vec<Subagent>) {
    let mut excess = subagents.len().saturating_sub(KEPT_SUBAGENTS);
    subagents.retain(|subagent| {
        if excess > 0 && !subagent.status.is_running() {
            excess -= 1;
            return false;
        }
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_unix_millis(millis)
    }

    fn started(item: u32, parent: Option<u32>, run_in_background: bool) -> Started {
        Started {
            item: ItemId::new(item),
            parent: parent.map(ItemId::new),
            summary: format!("Call {item}"),
            input: AgentInput {
                description: String::new(),
                prompt: "Look around".to_owned(),
                subagent_type: Some(" ".to_owned()),
                run_in_background,
            },
            turn: 2,
            now: at(1_000),
        }
    }

    fn statuses(subagents: &[Subagent]) -> Vec<SubagentStatus> {
        subagents.iter().map(|subagent| subagent.status).collect()
    }

    #[test]
    fn a_call_without_a_description_shows_its_summary() {
        let mut subagents = Vec::new();

        start(&mut subagents, started(5, None, true));

        let [subagent] = subagents.as_slice() else {
            panic!("expected one subagent, got {subagents:?}");
        };
        assert_eq!(subagent.description, "Call 5");
        assert_eq!(subagent.agent_type, None);
        assert_eq!(subagent.run, SubagentRun::Background);
        assert_eq!(subagent.turn, 2);
    }

    #[test]
    fn a_long_type_is_cut_to_one_line() {
        let mut subagents = Vec::new();
        let mut call = started(5, None, false);
        call.input.subagent_type = Some(format!("Explore\n{}", "x".repeat(10_000)));
        let mut long = started(6, None, false);
        long.input.subagent_type = Some("y".repeat(10_000));

        start(&mut subagents, call);
        start(&mut subagents, long);

        assert_eq!(subagents[0].agent_type.as_deref(), Some("Explore"));
        let cut = subagents[1].agent_type.as_deref().unwrap_or_default();
        assert!(cut.len() < 1_000, "{} bytes", cut.len());
    }

    #[test]
    fn an_end_stops_the_foreground_subagents_under_it_and_leaves_the_background_ones() {
        let mut subagents = Vec::new();
        start(&mut subagents, started(1, None, false));
        start(&mut subagents, started(2, Some(1), false));
        start(&mut subagents, started(3, Some(2), false));
        start(&mut subagents, started(4, Some(1), true));

        let ended = Ended {
            status: SubagentStatus::Failed,
            error: Some("\nAPI error 529: overloaded\nRetry later".to_owned()),
            now: Some(at(9_000)),
        };
        assert!(end(&mut subagents, ItemId::new(1), &ended));

        assert_eq!(
            statuses(&subagents),
            [
                SubagentStatus::Failed,
                SubagentStatus::Stopped,
                SubagentStatus::Stopped,
                SubagentStatus::Running
            ]
        );
        assert_eq!(
            subagents[0].error.as_deref(),
            Some("API error 529: overloaded")
        );
        assert_eq!(subagents[2].ended_at, Some(at(9_000)));
        assert!(!end(&mut subagents, ItemId::new(1), &ended), "ended once");
    }

    #[test]
    fn progress_counts_the_most_calls_seen_and_names_a_new_tool() {
        let mut subagents = Vec::new();
        start(&mut subagents, started(1, None, true));
        let call = SubagentToolCall {
            item: Some(ItemId::new(7)),
            tool: "Read".to_owned(),
            summary: "src/main.rs".to_owned(),
        };
        assert!(note_call(&mut subagents, ItemId::new(1), call.clone()));
        let subagent = &mut subagents[0];

        let usage = TaskUsage {
            total_tokens: 12_000,
            tool_uses: 3,
            duration: None,
        };
        assert!(note_progress(
            subagent,
            Some(usage),
            Some("Read".to_owned())
        ));
        assert_eq!(subagent.tokens, Some(12_000));
        assert_eq!(subagent.tool_calls, 3);
        assert_eq!(
            subagent.last_tool,
            Some(call),
            "the same tool keeps its call"
        );

        assert!(!note_progress(subagent, Some(usage), None));
        assert!(note_progress(subagent, None, Some("Bash".to_owned())));
        assert_eq!(
            subagent
                .last_tool
                .as_ref()
                .map(|call| (call.item, call.tool.as_str())),
            Some((None, "Bash"))
        );
    }

    #[test]
    fn waiting_follows_the_pending_answers_of_running_subagents() {
        let mut subagents = Vec::new();
        start(&mut subagents, started(1, None, false));
        start(&mut subagents, started(2, None, false));
        end_running(
            &mut subagents[1..],
            &Ended {
                status: SubagentStatus::Done,
                error: None,
                now: None,
            },
        );

        assert!(mark_waiting(
            &mut subagents,
            &BTreeSet::from([ItemId::new(1), ItemId::new(2)])
        ));
        assert_eq!(
            statuses(&subagents),
            [SubagentStatus::NeedsYou, SubagentStatus::Done]
        );
        assert!(mark_waiting(&mut subagents, &BTreeSet::new()));
        assert_eq!(subagents[0].status, SubagentStatus::Running);
        assert!(!mark_waiting(&mut subagents, &BTreeSet::new()));
    }

    #[test]
    fn pruning_keeps_the_running_subagents_and_the_latest_ended_ones() {
        let mut subagents = Vec::new();
        start(&mut subagents, started(0, None, true));
        let ended = Ended {
            status: SubagentStatus::Done,
            error: None,
            now: None,
        };
        for item in 1..u32::try_from(KEPT_SUBAGENTS).unwrap() {
            start(&mut subagents, started(item, None, false));
            end(&mut subagents, ItemId::new(item), &ended);
        }

        start(&mut subagents, started(1_000, None, false));

        assert_eq!(subagents.len(), KEPT_SUBAGENTS);
        assert_eq!(subagents[0].item, ItemId::new(0), "running, so kept");
        assert_eq!(
            subagents[1].item,
            ItemId::new(2),
            "the oldest ended one went"
        );
    }
}
