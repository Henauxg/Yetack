//! A change of some parts of an agent's details, so that a subagent's tool call sends that
//! subagent alone rather than the whole list.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::details::{
    AgentDetails, BackgroundTask, ChangedFile, ContextUsage, McpServer, Subagent, TaskListItem,
    WaitingPrompt,
};
use crate::ids::ItemId;

/// One part of an agent's details that changed, with its new value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "part", rename_all = "snake_case")]
pub enum DetailsChange {
    Tasks {
        tasks: Vec<TaskListItem>,
    },
    BackgroundTasks {
        background_tasks: Vec<BackgroundTask>,
    },
    ChangedFiles {
        changed_files: Vec<ChangedFile>,
    },
    Context {
        context: Option<ContextUsage>,
    },
    McpServers {
        mcp_servers: Vec<McpServer>,
    },
    WaitingPrompts {
        waiting_prompts: Vec<WaitingPrompt>,
    },
    /// Takes out the subagents that `removed` names, then puts each of `changed` in place of the
    /// entry with its item, or at the end of the list when none has it.
    Subagents {
        changed: Vec<Subagent>,
        removed: Vec<ItemId>,
    },
}

impl AgentDetails {
    /// The changes that turn `older` into these details: each part that differs, and of the
    /// subagents only those that differ or left. Empty when the two are equal.
    #[must_use]
    pub fn changes_since(&self, older: &Self) -> Vec<DetailsChange> {
        let Self {
            tasks,
            background_tasks,
            changed_files,
            context,
            mcp_servers,
            subagents,
            waiting_prompts,
        } = self;
        let mut changes = Vec::new();
        if *tasks != older.tasks {
            changes.push(DetailsChange::Tasks {
                tasks: tasks.clone(),
            });
        }
        if *background_tasks != older.background_tasks {
            changes.push(DetailsChange::BackgroundTasks {
                background_tasks: background_tasks.clone(),
            });
        }
        if *changed_files != older.changed_files {
            changes.push(DetailsChange::ChangedFiles {
                changed_files: changed_files.clone(),
            });
        }
        if *context != older.context {
            changes.push(DetailsChange::Context {
                context: context.clone(),
            });
        }
        if *mcp_servers != older.mcp_servers {
            changes.push(DetailsChange::McpServers {
                mcp_servers: mcp_servers.clone(),
            });
        }
        if *waiting_prompts != older.waiting_prompts {
            changes.push(DetailsChange::WaitingPrompts {
                waiting_prompts: waiting_prompts.clone(),
            });
        }
        changes.extend(subagent_changes(&older.subagents, subagents));
        changes
    }

    /// Applies a change that [`AgentDetails::changes_since`] made.
    pub fn apply(&mut self, change: DetailsChange) {
        match change {
            DetailsChange::Tasks { tasks } => self.tasks = tasks,
            DetailsChange::BackgroundTasks { background_tasks } => {
                self.background_tasks = background_tasks;
            }
            DetailsChange::ChangedFiles { changed_files } => self.changed_files = changed_files,
            DetailsChange::Context { context } => self.context = context,
            DetailsChange::McpServers { mcp_servers } => self.mcp_servers = mcp_servers,
            DetailsChange::WaitingPrompts { waiting_prompts } => {
                self.waiting_prompts = waiting_prompts;
            }
            DetailsChange::Subagents { changed, removed } => {
                let removed: HashSet<ItemId> = removed.into_iter().collect();
                self.subagents
                    .retain(|subagent| !removed.contains(&subagent.item));
                for subagent in changed {
                    let held = self
                        .subagents
                        .iter_mut()
                        .find(|held| held.item == subagent.item);
                    match held {
                        Some(held) => *held = subagent,
                        None => self.subagents.push(subagent),
                    }
                }
            }
        }
    }
}

/// The subagents of `newer` that `older` does not hold as they are, and those of `older` that
/// leave. An entry keeps its place while every entry before it does: past the first one out of
/// place, each entry leaves and comes back at the end, in its new order.
fn subagent_changes(older: &[Subagent], newer: &[Subagent]) -> Option<DetailsChange> {
    let newer_items: HashSet<ItemId> = newer.iter().map(|subagent| subagent.item).collect();
    let kept: Vec<&Subagent> = older
        .iter()
        .filter(|subagent| newer_items.contains(&subagent.item))
        .collect();
    let in_place = newer
        .iter()
        .zip(&kept)
        .take_while(|(new, old)| new.item == old.item)
        .count();
    let changed: Vec<Subagent> = newer
        .iter()
        .enumerate()
        .filter(|(index, subagent)| *index >= in_place || kept.get(*index) != Some(subagent))
        .map(|(_, subagent)| subagent.clone())
        .collect();
    let staying: HashSet<ItemId> = newer
        .iter()
        .take(in_place)
        .map(|subagent| subagent.item)
        .collect();
    let removed: Vec<ItemId> = older
        .iter()
        .map(|subagent| subagent.item)
        .filter(|item| !staying.contains(item))
        .collect();
    if changed.is_empty() && removed.is_empty() {
        return None;
    }
    Some(DetailsChange::Subagents { changed, removed })
}

#[cfg(test)]
mod tests {
    use crate::details::{SubagentRun, SubagentStatus, SubagentToolCall, TaskStatus};
    use crate::ids::BackgroundTaskId;
    use crate::time::Timestamp;

    use super::*;

    fn subagent(item: u32, status: SubagentStatus) -> Subagent {
        Subagent {
            item: ItemId::new(item),
            parent: None,
            description: format!("Survey module {item}"),
            agent_type: Some("Explore".to_owned()),
            run: SubagentRun::Background,
            status,
            started_at: Timestamp::from_unix_millis(1_000),
            ended_at: None,
            tokens: Some(1_200),
            tool_calls: 3,
            last_tool: Some(SubagentToolCall {
                item: Some(ItemId::new(item + 1)),
                tool: "Read".to_owned(),
                summary: "src/cart.rs".to_owned(),
            }),
            error: None,
            turn: 2,
            task: Some(BackgroundTaskId::new(format!("task-{item}"))),
        }
    }

    fn with_subagents(items: &[u32]) -> AgentDetails {
        AgentDetails {
            subagents: items
                .iter()
                .map(|&item| subagent(item, SubagentStatus::Done))
                .collect(),
            ..AgentDetails::default()
        }
    }

    fn task(content: &str) -> TaskListItem {
        TaskListItem {
            content: content.to_owned(),
            status: TaskStatus::InProgress,
            active_form: format!("{content}ing"),
        }
    }

    /// Applies the changes from `older` to `newer`, and checks that they give `newer`.
    fn applied(older: &AgentDetails, newer: &AgentDetails) -> Vec<DetailsChange> {
        let changes = newer.changes_since(older);
        let mut details = older.clone();
        for change in changes.clone() {
            details.apply(change);
        }
        assert_eq!(&details, newer, "the changes {changes:?}");
        changes
    }

    #[test]
    fn equal_details_change_nothing() {
        let details = with_subagents(&[10, 20, 30]);

        assert_eq!(details.changes_since(&details.clone()), []);
    }

    #[test]
    fn a_subagents_tool_call_changes_that_subagent_alone() {
        let items: Vec<u32> = (1..=200).map(|index| index * 10).collect();
        let older = with_subagents(&items);
        let mut newer = older.clone();
        let busy = &mut newer.subagents[120];
        busy.tool_calls += 1;
        busy.last_tool = Some(SubagentToolCall {
            item: Some(ItemId::new(5_000)),
            tool: "Grep".to_owned(),
            summary: "fn total".to_owned(),
        });
        let busy = busy.clone();

        let changes = applied(&older, &newer);

        assert_eq!(
            changes,
            [DetailsChange::Subagents {
                changed: vec![busy],
                removed: Vec::new(),
            }]
        );
    }

    #[test]
    fn a_new_subagent_goes_at_the_end_and_a_pruned_one_leaves() {
        let older = with_subagents(&[10, 20, 30]);
        let mut newer = with_subagents(&[20, 30]);
        newer.subagents.push(subagent(40, SubagentStatus::Running));

        let changes = applied(&older, &newer);

        assert_eq!(
            changes,
            [DetailsChange::Subagents {
                changed: vec![subagent(40, SubagentStatus::Running)],
                removed: vec![ItemId::new(10)],
            }]
        );
    }

    /// A call seen again starts its subagent again, at the end of the list.
    #[test]
    fn a_subagent_started_again_moves_to_the_end() {
        let older = with_subagents(&[10, 20, 30, 40]);
        let mut newer = with_subagents(&[10, 30, 40]);
        newer.subagents.push(subagent(20, SubagentStatus::Running));

        let changes = applied(&older, &newer);

        let DetailsChange::Subagents { changed, removed } = &changes[0] else {
            panic!("expected subagents, got {changes:?}");
        };
        assert!(
            !changed
                .iter()
                .any(|subagent| subagent.item == ItemId::new(10)),
            "the first one stays in place: {changed:?}"
        );
        assert!(removed.contains(&ItemId::new(20)));
    }

    #[test]
    fn a_change_of_another_part_sends_that_part_alone() {
        let older = AgentDetails {
            tasks: vec![task("Fix the total")],
            ..with_subagents(&[10, 20])
        };
        let newer = AgentDetails {
            tasks: vec![task("Fix the total"), task("Test the total")],
            ..older.clone()
        };

        let changes = applied(&older, &newer);

        assert_eq!(
            changes,
            [DetailsChange::Tasks {
                tasks: newer.tasks.clone(),
            }]
        );
    }

    #[test]
    fn the_changes_of_each_step_lead_through_every_step() {
        let mut steps: Vec<AgentDetails> = vec![AgentDetails::default()];
        let mut details = AgentDetails::default();
        for item in [10, 20, 30] {
            details
                .subagents
                .push(subagent(item, SubagentStatus::Running));
            steps.push(details.clone());
        }
        details.subagents[1].status = SubagentStatus::NeedsYou;
        details.context = Some(ContextUsage {
            tokens: 4_000,
            max_tokens: 200_000,
            percent: 2,
            categories: Vec::new(),
        });
        details.tasks = vec![task("Read the cart")];
        details.changed_files = vec![ChangedFile {
            path: "src/cart.rs".into(),
            lines_added: 3,
            lines_removed: 1,
        }];
        steps.push(details.clone());
        details.subagents[1].status = SubagentStatus::Done;
        details.subagents.remove(0);
        steps.push(details.clone());
        let restarted = details.subagents.remove(0);
        details
            .subagents
            .push(subagent(40, SubagentStatus::Running));
        details.subagents.push(restarted);
        steps.push(details.clone());
        details = AgentDetails::default();
        steps.push(details);

        let mut followed = AgentDetails::default();
        for pair in steps.windows(2) {
            for change in pair[1].changes_since(&pair[0]) {
                followed.apply(change);
            }
            assert_eq!(followed, pair[1]);
        }
    }

    /// Numbers that look random and are the same at every run.
    struct Draws(u64);

    impl Draws {
        /// A number from 0 to `bound`, excluded, or 0 when `bound` is 0.
        fn below(&mut self, bound: usize) -> usize {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let draw = usize::try_from(self.0 >> 33).unwrap();
            draw.checked_rem(bound).unwrap_or(0)
        }
    }

    /// Each step starts, ends, prunes, starts again or changes a few subagents, as the daemon
    /// does, or changes the tasks.
    #[test]
    fn the_changes_lead_through_many_random_steps() {
        let mut draws = Draws(7);
        let mut next_item = 1;
        let mut details = AgentDetails::default();
        let mut followed = AgentDetails::default();

        for step in 0..2_000 {
            let older = details.clone();
            for _ in 0..=draws.below(3) {
                let count = details.subagents.len();
                let index = draws.below(count);
                match draws.below(5) {
                    0 | 1 => {
                        details
                            .subagents
                            .push(subagent(next_item, SubagentStatus::Running));
                        next_item += 1;
                    }
                    2 if count > 0 => {
                        details.subagents.remove(index);
                    }
                    3 if count > 0 => {
                        let again = details.subagents.remove(index);
                        details.subagents.push(again);
                    }
                    4 if count > 0 => {
                        details.subagents[index].tool_calls += 1;
                        details.subagents[index].status = SubagentStatus::Done;
                    }
                    _ => details.tasks = vec![task(&format!("Step {step}"))],
                }
            }
            for change in details.changes_since(&older) {
                followed.apply(change);
            }

            assert_eq!(followed, details, "at step {step}");
        }
    }
}
