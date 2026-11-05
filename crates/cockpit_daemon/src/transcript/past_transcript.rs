//! The transcript of a past session, built from its session file as a live agent's is built from
//! `claude`'s stdout.

use std::collections::HashMap;
use std::path::PathBuf;

use claude_protocol::OutputLine;
use cockpit_protocol::{ItemBody, ItemId, TranscriptItem};

use crate::transcript::agent_view::AgentView;

/// The most items a past session shows. The app keeps as many for an agent it does not show.
const MAX_PAST_ITEMS: usize = 2_000;
/// The id of a past session's newest item, whatever the number of items. An agent made from a
/// past session numbers its own items after it, so the two never share an id.
pub(crate) const LAST_PAST_ID: u32 = 2_001;
const LEFT_OUT_NOTICE: &str = "Older messages of this session are not shown";

pub(crate) struct PastTranscript {
    view: AgentView,
    left_out: bool,
}

impl PastTranscript {
    /// `folder` is the session's, which tool summaries show paths relative to.
    pub(crate) fn new(folder: PathBuf) -> Self {
        Self {
            view: AgentView::for_session_file(folder),
            left_out: false,
        }
    }

    /// Reads one line of the session file.
    pub(crate) fn add(&mut self, line: OutputLine) {
        self.view.apply_session_line(line);
        // Trimmed in batches, so it holds at most twice the items it keeps.
        if self.view.items().len() > 2 * MAX_PAST_ITEMS {
            self.view.keep_newest(MAX_PAST_ITEMS);
            self.left_out = true;
        }
    }

    /// The newest items, numbered to end at [`LAST_PAST_ID`], after a notice when older ones are
    /// left out. An item whose parent is left out shows at the top level.
    pub(crate) fn into_items(self) -> Vec<TranscriptItem> {
        let mut items = self.view.into_finished_items();
        let older = items.len().saturating_sub(MAX_PAST_ITEMS);
        let left_out = self.left_out || older > 0;
        items.drain(..older);
        let count = u32::try_from(items.len()).unwrap_or(LAST_PAST_ID);
        let first = LAST_PAST_ID.saturating_sub(count).saturating_add(1);
        let mut ids: HashMap<ItemId, ItemId> = HashMap::new();
        let mut numbered = Vec::with_capacity(items.len().saturating_add(1));
        if left_out {
            numbered.push(TranscriptItem {
                id: ItemId::new(first.saturating_sub(1)),
                parent: None,
                body: ItemBody::Notice {
                    text: LEFT_OUT_NOTICE.to_owned(),
                },
            });
        }
        for (raw, item) in (first..).zip(items) {
            let id = ItemId::new(raw);
            ids.insert(item.id, id);
            numbered.push(TranscriptItem {
                id,
                parent: item.parent.and_then(|parent| ids.get(&parent).copied()),
                body: item.body,
            });
        }
        numbered
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn line(value: Value) -> OutputLine {
        value.to_string().parse().unwrap()
    }

    fn prompt(text: &str) -> OutputLine {
        line(json!({"type": "user", "message": {"role": "user", "content": text}}))
    }

    fn answer(text: &str) -> OutputLine {
        line(json!({"type": "assistant", "message": {"content": [{"type": "text", "text": text}]}}))
    }

    fn ids(items: &[TranscriptItem]) -> Vec<u32> {
        items.iter().map(|item| item.id.get()).collect()
    }

    #[test]
    fn prompts_answers_and_tool_calls_come_out_in_order_ending_at_the_last_past_id() {
        let mut past = PastTranscript::new(PathBuf::from("/srv/work/shop"));

        past.add(prompt("List the tests"));
        past.add(line(json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls tests"}}
        ]}})));
        past.add(line(
            json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": "cart.rs"}
        ]}, "toolUseResult": {"stdout": "cart.rs"}}),
        ));
        past.add(line(
            json!({"type": "user", "message": {"role": "user", "content": [
                {"type": "text", "text": "[Request interrupted by user]"}
            ]}}),
        ));
        past.add(answer("One test file."));
        past.add(line(json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "t2", "name": "Bash", "input": {"command": "cargo test"}}
        ]}})));
        let items = past.into_items();

        assert_eq!(ids(&items), [1997, 1998, 1999, 2000, 2001]);
        let ItemBody::UserMessage(first) = &items[0].body else {
            panic!("expected the prompt first, got {items:?}");
        };
        assert_eq!(first.text, "List the tests");
        let ItemBody::ToolCall(listed) = &items[1].body else {
            panic!("expected a tool call");
        };
        assert_eq!(listed.output.as_deref(), Some("cart.rs"));
        assert_eq!(
            items[2].body,
            ItemBody::Notice {
                text: "Stopped".to_owned()
            }
        );
        assert_eq!(
            items[3].body,
            ItemBody::AssistantText {
                markdown: "One test file.".to_owned()
            }
        );
        let ItemBody::ToolCall(unfinished) = &items[4].body else {
            panic!("expected a tool call");
        };
        assert_eq!(
            unfinished.status,
            cockpit_protocol::ToolCallStatus::Failed,
            "a call the file never answers did not finish"
        );
    }

    #[test]
    fn a_session_files_commands_read_as_typed_and_their_output_as_notices() {
        let mut past = PastTranscript::new(PathBuf::from("/srv/work/shop"));

        past.add(prompt(
            "<command-name>/effort</command-name>\n\
             <command-message>effort</command-message>\n<command-args>high</command-args>",
        ));
        past.add(line(json!({
            "type": "system", "subtype": "local_command",
            "content": "<local-command-stdout>Set effort level to high</local-command-stdout>"
        })));
        past.add(prompt(
            "<command-name>/model</command-name>\n<command-args>sonnet</command-args>",
        ));
        past.add(prompt(
            "<local-command-stdout>Set model to sonnet</local-command-stdout>",
        ));
        let items = past.into_items();

        let bodies: Vec<&ItemBody> = items.iter().map(|item| &item.body).collect();
        let user = |text: &str| {
            ItemBody::UserMessage(cockpit_protocol::UserMessage {
                text: text.to_owned(),
                images: Vec::new(),
                queued: false,
            })
        };
        let notice = |text: &str| ItemBody::Notice {
            text: text.to_owned(),
        };
        assert_eq!(
            bodies,
            [
                &user("/effort high"),
                &notice("Set effort level to high"),
                &user("/model sonnet"),
                &notice("Set model to sonnet"),
            ]
        );
    }

    #[test]
    fn a_long_session_keeps_its_newest_items_under_a_notice() {
        let mut past = PastTranscript::new(PathBuf::from("/srv/work/shop"));

        for index in 0..(2 * MAX_PAST_ITEMS + 10) {
            past.add(answer(&format!("Answer {index}")));
        }
        let items = past.into_items();

        assert_eq!(items.len(), MAX_PAST_ITEMS + 1);
        assert_eq!(
            items[0],
            TranscriptItem {
                id: ItemId::new(1),
                parent: None,
                body: ItemBody::Notice {
                    text: LEFT_OUT_NOTICE.to_owned()
                },
            }
        );
        assert_eq!(items[1].id, ItemId::new(2));
        assert_eq!(
            items[1].body,
            ItemBody::AssistantText {
                markdown: "Answer 2010".to_owned()
            }
        );
        assert_eq!(
            items.last().map(|item| item.id),
            Some(ItemId::new(LAST_PAST_ID))
        );
    }
}
