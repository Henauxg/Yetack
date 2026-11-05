//! `mcp.remove`: runs `claude mcp remove` in the agent's folder, which decides the project and
//! local scopes.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use claude_protocol::McpScope;
use cockpit_protocol::{ErrorBody, ErrorCode};
use tokio::process::Command;

const MCP_SUBCOMMAND: &str = "mcp";
const REMOVE_SUBCOMMAND: &str = "remove";
const SCOPE_FLAG: &str = "--scope";
/// Ends the options, so a server name starting with `-` stays a name.
const END_OF_OPTIONS: &str = "--";
const TIMEOUT: Duration = Duration::from_secs(30);
/// How much of `claude`'s output an error quotes.
const OUTPUT_TAIL_BYTES: usize = 2 * 1024;

pub(crate) async fn remove(
    program: &Path,
    folder: &Path,
    server: &str,
    scope: McpScope,
) -> Result<(), ErrorBody> {
    let mut command = Command::new(program);
    command
        .args(remove_args(server, scope))
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(TIMEOUT, command.output())
        .await
        .map_err(|_| {
            ErrorBody::new(
                ErrorCode::Timeout,
                format!("claude {MCP_SUBCOMMAND} {REMOVE_SUBCOMMAND} did not finish in time"),
            )
        })?
        .map_err(|error| {
            ErrorBody::new(
                ErrorCode::ClaudeFailed,
                format!("cannot run {}: {error}", program.display()),
            )
        })?;
    if output.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stdout).into_owned();
    }
    let tail_start = text.len().saturating_sub(OUTPUT_TAIL_BYTES);
    let tail = text
        .char_indices()
        .find(|(index, _)| *index >= tail_start)
        .map_or("", |(index, _)| text.get(index..).unwrap_or_default());
    Err(ErrorBody::new(
        ErrorCode::ClaudeFailed,
        format!(
            "claude {MCP_SUBCOMMAND} {REMOVE_SUBCOMMAND} failed: {}",
            tail.trim()
        ),
    ))
}

fn remove_args(server: &str, scope: McpScope) -> Vec<OsString> {
    vec![
        OsString::from(MCP_SUBCOMMAND),
        OsString::from(REMOVE_SUBCOMMAND),
        OsString::from(format!("{SCOPE_FLAG}={scope}")),
        OsString::from(END_OF_OPTIONS),
        OsString::from(server),
    ]
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn the_server_name_comes_after_the_end_of_options() {
        let args = remove_args("-x", McpScope::Project);

        assert_eq!(
            args,
            ["mcp", "remove", "--scope=project", "--", "-x"].map(OsString::from)
        );
    }

    #[tokio::test]
    async fn a_failure_quotes_what_claude_said() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let program = temp.path().join("claude");
        std::fs::write(
            &program,
            "#!/bin/sh\necho \"No MCP server named $5\" >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&program, Permissions::from_mode(0o755)).unwrap();
        wait_until_it_runs(&program).await;

        let result = remove(&program, temp.path(), "notes", McpScope::Local).await;

        let error = result.unwrap_err();
        assert_eq!(error.code, ErrorCode::ClaudeFailed);
        assert_eq!(
            error.message,
            "claude mcp remove failed: No MCP server named notes"
        );
    }

    /// A process another test forks while a script is written keeps it open for writing until
    /// it runs its own program, and running the script fails with "Text file busy" until then.
    /// Once the script ran, no process can hold it open for writing again.
    async fn wait_until_it_runs(program: &Path) {
        for _ in 0..200 {
            let run = Command::new(program)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
            match run {
                Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok(_) | Err(_) => return,
            }
        }
    }
}
