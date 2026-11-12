//! What the floating blocks show: tasks, background work, changed files, context usage, MCP
//! servers and slash commands.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use claude_protocol::{
    ContextUsage as ClaudeContextUsage, InitMcpServer, InitializeResponse,
    McpServerStatus as ClaudeMcpServer, McpTransport as ClaudeMcpTransport, OpenWord, Todo,
};
use cockpit_protocol::{
    BackgroundTask, BackgroundTaskKind, BackgroundTaskStatus, ChangedFile, CommandKind,
    ContextCategory, ContextUsage, McpServer, McpServerStatus, McpTool, McpTransport, SlashCommand,
    TaskListItem, TaskProgress, TaskStatus,
};
use strum::EnumString;

use crate::transcript::tools::ChangedLines;
use crate::transcript::words;

/// The task types of `task_started` that run a subagent. Any other runs a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
#[strum(serialize_all = "snake_case")]
enum AgentTaskType {
    LocalAgent,
    RemoteAgent,
}

/// Claude Code names an MCP tool `mcp__<server>__<tool>`.
const MCP_TOOL_PREFIX: &str = "mcp__";
const MCP_TOOL_SEPARATOR: &str = "__";

/// Finished background tasks kept in the block; older ones go.
const KEPT_FINISHED_TASKS: usize = 20;

pub(crate) fn task_list(todos: &[Todo]) -> Vec<TaskListItem> {
    todos
        .iter()
        .map(|todo| TaskListItem {
            content: todo.content.clone(),
            status: words::todo_status(&todo.status),
            active_form: todo
                .active_form
                .clone()
                .unwrap_or_else(|| todo.content.clone()),
        })
        .collect()
}

/// "3/5", or `None` without a task list.
pub(crate) fn task_progress(tasks: &[TaskListItem]) -> Option<TaskProgress> {
    if tasks.is_empty() {
        return None;
    }
    let done = tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Completed)
        .count();
    Some(TaskProgress {
        done: u16::try_from(done).unwrap_or(u16::MAX),
        total: u16::try_from(tasks.len()).unwrap_or(u16::MAX),
    })
}

/// What the agent does now, from the task in progress.
pub(crate) fn task_in_progress(tasks: &[TaskListItem]) -> Option<String> {
    tasks
        .iter()
        .find(|task| task.status == TaskStatus::InProgress)
        .map(|task| task.active_form.clone())
}

pub(crate) fn background_kind(task_type: Option<&str>) -> BackgroundTaskKind {
    match task_type.map(str::parse::<AgentTaskType>) {
        Some(Ok(_)) => BackgroundTaskKind::Agent,
        Some(Err(_)) | None => BackgroundTaskKind::Command,
    }
}

/// Keeps the running tasks and the latest finished ones.
pub(crate) fn prune_background_tasks(tasks: &mut Vec<BackgroundTask>) {
    let finished = tasks
        .iter()
        .filter(|task| task.status != BackgroundTaskStatus::Running)
        .count();
    let mut excess = finished.saturating_sub(KEPT_FINISHED_TASKS);
    tasks.retain(|task| {
        if excess > 0 && task.status != BackgroundTaskStatus::Running {
            excess -= 1;
            return false;
        }
        true
    });
}

pub(crate) fn add_changed_file(files: &mut Vec<ChangedFile>, path: &Path, lines: ChangedLines) {
    match files.iter_mut().find(|file| file.path == path) {
        Some(file) => {
            file.lines_added = file.lines_added.saturating_add(lines.added);
            file.lines_removed = file.lines_removed.saturating_add(lines.removed);
        }
        None => files.push(ChangedFile {
            path: path.to_owned(),
            lines_added: lines.added,
            lines_removed: lines.removed,
        }),
    }
}

/// Deferred tools are not in the context, so they are left out.
pub(crate) fn context_usage(usage: &ClaudeContextUsage) -> ContextUsage {
    let categories: Vec<ContextCategory> = usage
        .categories
        .iter()
        .filter(|category| !category.is_deferred)
        .map(|category| ContextCategory {
            name: category.name.clone(),
            tokens: category.tokens,
        })
        .collect();
    ContextUsage {
        tokens: usage.total_tokens,
        max_tokens: usage.max_tokens,
        percent: context_percent(usage),
        categories,
    }
}

fn context_percent(usage: &ClaudeContextUsage) -> u8 {
    let percent = match usage.percentage {
        Some(percent) => percent,
        None if usage.max_tokens > 0 => {
            f64::from(usage.total_tokens) * 100.0 / f64::from(usage.max_tokens)
        }
        None => 0.0,
    };
    // Clamped to 0..=100 first, so the cast keeps the value.
    percent.round().clamp(0.0, 100.0) as u8
}

pub(crate) fn mcp_server(server: &ClaudeMcpServer, calls: &HashMap<String, u32>) -> McpServer {
    let transport = server.config.as_ref().and_then(|config| {
        let command = config.command.as_ref().map(|command| {
            let mut line = command.clone();
            for arg in &config.args {
                line.push(' ');
                line.push_str(arg);
            }
            line
        });
        match (config.transport.as_ref(), command, config.url.clone()) {
            (None | Some(OpenWord::Known(ClaudeMcpTransport::Stdio)), Some(command), _) => {
                Some(McpTransport::Stdio { command })
            }
            (Some(OpenWord::Known(ClaudeMcpTransport::Sse)), _, Some(url)) => {
                Some(McpTransport::Sse { url })
            }
            (_, _, Some(url)) => Some(McpTransport::Http { url }),
            (_, Some(command), None) => Some(McpTransport::Stdio { command }),
            (_, None, None) => None,
        }
    });
    let tools: Vec<McpTool> = server
        .tools
        .iter()
        .map(|tool| McpTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            read_only: tool.annotations.read_only.unwrap_or(false),
            destructive: tool.annotations.destructive.unwrap_or(false),
            open_world: tool.annotations.open_world.unwrap_or(false),
            calls: calls
                .get(&mcp_tool_name(&server.name, &tool.name))
                .copied()
                .unwrap_or(0),
        })
        .collect();
    McpServer {
        name: server.name.clone(),
        status: words::mcp_status(&server.status),
        error: server.error.clone(),
        version: server
            .server_info
            .as_ref()
            .map(|info| info.version.clone())
            .filter(|version| !version.is_empty()),
        transport,
        scope: server.scope.as_ref().and_then(words::mcp_scope),
        tools,
    }
}

/// `system/init` lists every server with its status: the list follows it, and keeps what an
/// earlier `mcp_status` said of each server.
pub(crate) fn merge_init_servers(servers: &mut Vec<McpServer>, init: &[InitMcpServer]) {
    let mut merged = Vec::with_capacity(init.len());
    for listed in init {
        let status = listed
            .status
            .as_ref()
            .map_or(McpServerStatus::Pending, words::mcp_status);
        let server = match servers.iter().position(|server| server.name == listed.name) {
            Some(index) => {
                let mut server = servers.swap_remove(index);
                server.status = status;
                server
            }
            None => McpServer {
                name: listed.name.clone(),
                status,
                error: None,
                version: None,
                transport: None,
                scope: None,
                tools: Vec::new(),
            },
        };
        merged.push(server);
    }
    *servers = merged;
}

/// Updates each tool's call count.
pub(crate) fn count_mcp_calls(servers: &mut [McpServer], calls: &HashMap<String, u32>) {
    for server in servers {
        for tool in &mut server.tools {
            tool.calls = calls
                .get(&mcp_tool_name(&server.name, &tool.name))
                .copied()
                .unwrap_or(0);
        }
    }
}

pub(crate) fn is_mcp_tool(tool: &str) -> bool {
    tool.starts_with(MCP_TOOL_PREFIX)
}

fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("{MCP_TOOL_PREFIX}{server}{MCP_TOOL_SEPARATOR}{tool}")
}

pub(crate) fn commands(
    response: &InitializeResponse,
    skills: &BTreeSet<String>,
) -> Vec<SlashCommand> {
    response
        .commands
        .iter()
        .map(|command| SlashCommand {
            name: command.name.clone(),
            description: command.description.clone(),
            kind: command_kind(&command.name, skills),
        })
        .collect()
}

/// From `system/init`, which names the commands without describing them.
pub(crate) fn commands_from_names(
    names: &[String],
    skills: &BTreeSet<String>,
) -> Vec<SlashCommand> {
    names
        .iter()
        .map(|name| SlashCommand {
            name: name.clone(),
            description: String::new(),
            kind: command_kind(name, skills),
        })
        .collect()
}

pub(crate) fn mark_skills(commands: &mut [SlashCommand], skills: &BTreeSet<String>) {
    for command in commands {
        command.kind = command_kind(&command.name, skills);
    }
}

fn command_kind(name: &str, skills: &BTreeSet<String>) -> CommandKind {
    if skills.contains(name) {
        CommandKind::Skill
    } else {
        CommandKind::Command
    }
}

#[cfg(test)]
mod tests {
    use claude_protocol::McpStatus;
    use cockpit_protocol::{BackgroundTaskId, McpScope, Timestamp};
    use serde_json::json;

    use super::*;

    fn finished_task(id: u32) -> BackgroundTask {
        BackgroundTask {
            id: BackgroundTaskId::new(format!("task-{id}")),
            kind: BackgroundTaskKind::Command,
            description: String::new(),
            status: BackgroundTaskStatus::Done,
            started_at: Timestamp::from_unix_millis(0),
            ended_at: Some(Timestamp::from_unix_millis(1)),
            tool_calls: 0,
            call: None,
        }
    }

    #[test]
    fn agent_task_types_are_pinned() {
        assert_eq!(
            background_kind(Some("local_agent")),
            BackgroundTaskKind::Agent
        );
        assert_eq!(
            background_kind(Some("remote_agent")),
            BackgroundTaskKind::Agent
        );
        assert_eq!(
            background_kind(Some("local_bash")),
            BackgroundTaskKind::Command
        );
    }

    #[test]
    fn pruning_keeps_running_tasks_and_the_latest_finished_ones() {
        let mut tasks: Vec<BackgroundTask> = (0..30).map(finished_task).collect();
        tasks[0].status = BackgroundTaskStatus::Running;

        prune_background_tasks(&mut tasks);

        assert_eq!(tasks.len(), KEPT_FINISHED_TASKS + 1);
        assert_eq!(tasks[0].status, BackgroundTaskStatus::Running);
        assert_eq!(tasks.last().unwrap().id.as_str(), "task-29");
    }

    #[test]
    fn mcp_status_becomes_a_server_with_its_call_counts() {
        let status: McpStatus = (&json!({
            "mcpServers": [{
                "name": "notes",
                "status": "connected",
                "scope": "project",
                "config": {"type": "stdio", "command": "notes-server", "args": ["--fast"]},
                "serverInfo": {"name": "notes", "version": "1.2.0"},
                "tools": [{"name": "search", "annotations": {"readOnly": true}}]
            }, {
                "name": "remote",
                "status": "something-new",
                "config": {"type": "http", "url": "https://mcp.example.test"}
            }]
        }))
            .try_into()
            .unwrap();
        let calls = HashMap::from([("mcp__notes__search".to_owned(), 3)]);

        let servers: Vec<McpServer> = status
            .mcp_servers
            .iter()
            .map(|server| mcp_server(server, &calls))
            .collect();

        assert_eq!(
            servers[0],
            McpServer {
                name: "notes".to_owned(),
                status: McpServerStatus::Connected,
                error: None,
                version: Some("1.2.0".to_owned()),
                transport: Some(McpTransport::Stdio {
                    command: "notes-server --fast".to_owned()
                }),
                scope: Some(McpScope::Project),
                tools: vec![McpTool {
                    name: "search".to_owned(),
                    description: None,
                    read_only: true,
                    destructive: false,
                    open_world: false,
                    calls: 3,
                }],
            }
        );
        assert_eq!(servers[1].status, McpServerStatus::Pending);
        assert_eq!(
            servers[1].transport,
            Some(McpTransport::Http {
                url: "https://mcp.example.test".to_owned()
            })
        );
    }

    #[test]
    fn context_usage_leaves_out_deferred_tools() {
        let usage: ClaudeContextUsage = (&json!({
            "categories": [
                {"name": "System prompt", "tokens": 3000},
                {"name": "System tools (deferred)", "tokens": 9000, "isDeferred": true},
                {"name": "Messages", "tokens": 7000}
            ],
            "totalTokens": 10000,
            "maxTokens": 200000
        }))
            .try_into()
            .unwrap();

        let usage = context_usage(&usage);

        assert_eq!(usage.percent, 5);
        let names: Vec<&str> = usage
            .categories
            .iter()
            .map(|category| category.name.as_str())
            .collect();
        assert_eq!(names, ["System prompt", "Messages"]);
    }
}
