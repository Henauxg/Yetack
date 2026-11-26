//! The yetack command line: the daemon, the SSH bridge, and client commands.

#[cfg(unix)]
mod bridge;
mod cli;
mod client;

#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::path::Path;
use std::process::ExitCode;

#[cfg(unix)]
use cockpit_daemon::{DaemonConfig, HOME_ENV, MCP_SUBCOMMAND, McpCommand, STATE_DIR_FLAG};
use tokio::runtime::Runtime;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, PROGRAM};
#[cfg(unix)]
use crate::cli::{Command, SERVE_SUBCOMMAND, SSH_FLAG};

/// The log level of `serve`, unless `RUST_LOG` says otherwise.
const SERVE_LOG_LEVEL: &str = "info";
/// The log level of the other commands, whose output is their result.
const CLIENT_LOG_LEVEL: &str = "warn";

fn main() -> ExitCode {
    use clap::Parser;

    let cli = Cli::parse();
    #[cfg(unix)]
    let serving = matches!(cli.command, Command::Serve);
    #[cfg(windows)]
    let serving = false;
    let level = if serving {
        SERVE_LOG_LEVEL
    } else {
        CLIENT_LOG_LEVEL
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{PROGRAM}: cannot start the runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    #[cfg(unix)]
    let bridging = matches!(cli.command, Command::Bridge | Command::Mcp);
    #[cfg(windows)]
    let bridging = false;
    let result = runtime.block_on(run(cli));
    if bridging {
        // The stdin reader blocks a thread until its next byte; waiting for it would hang.
        runtime.shutdown_background();
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{PROGRAM}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        #[cfg(unix)]
        Command::Serve => {
            use anyhow::Context;

            if cli.target.is_remote() {
                anyhow::bail!(
                    "{SERVE_SUBCOMMAND} runs the daemon on this machine: --{SSH_FLAG} does not apply"
                );
            }
            let config = DaemonConfig::from_env(cli.target.state_dir.clone())
                .with_context(|| format!("{HOME_ENV} is not set"))?;
            match mcp_command(&config.state_dir) {
                Some(mcp) => {
                    cockpit_daemon::serve_with_agent_tools(config, mcp, shutdown_signal()?).await?;
                }
                None => cockpit_daemon::serve(config, shutdown_signal()?).await?,
            }
            Ok(())
        }
        #[cfg(unix)]
        Command::Bridge => bridge::run(&cli.target.local_socket()?).await,
        #[cfg(unix)]
        Command::Mcp => {
            if cli.target.is_remote() {
                anyhow::bail!(
                    "mcp serves an agent of the daemon on this machine: --{SSH_FLAG} does not apply"
                );
            }
            let agent = cockpit_mcp::agent_from_env()?;
            cockpit_mcp::serve(cli.target.transport()?, agent).await?;
            Ok(())
        }
        command => client::run(&cli.target, command).await,
    }
}

/// This program's `--state-dir <state_dir> mcp`, which serves the agents of the daemon of
/// `state_dir`. `None` when the program or the folder has no path that `claude` can run.
#[cfg(unix)]
fn mcp_command(state_dir: &Path) -> Option<McpCommand> {
    let program = std::env::current_exe()
        .inspect_err(|error| {
            tracing::warn!(%error, "cannot find this program: agents cannot read terminals");
        })
        .ok()?;
    // The server runs in the agent's folder.
    let state_dir = std::path::absolute(state_dir)
        .inspect_err(|error| {
            tracing::warn!(%error, "cannot resolve the state folder: agents cannot read terminals");
        })
        .ok()?;
    let args = vec![
        OsString::from(format!("--{STATE_DIR_FLAG}")),
        state_dir.into_os_string(),
        OsString::from(MCP_SUBCOMMAND),
    ];
    Some(McpCommand { program, args })
}

/// Completes on SIGTERM, as systemd stops a service, or on Ctrl+C.
#[cfg(unix)]
fn shutdown_signal() -> anyhow::Result<impl Future<Output = ()>> {
    use tokio::signal::unix;
    use tokio::signal::unix::SignalKind;

    let mut terminate = unix::signal(SignalKind::terminate())?;
    let mut interrupt = unix::signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
    })
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    /// `claude` runs the server in the agent's folder, where a relative state folder means
    /// another folder.
    #[test]
    fn the_mcp_command_names_the_state_folder_from_the_root() {
        let command = mcp_command(Path::new("state")).unwrap();

        let state = command.args.get(1).map(Path::new).unwrap();
        assert!(state.is_absolute(), "{state:?}");
        assert!(state.ends_with("state"));
        assert_eq!(
            command.args.first().and_then(|arg| arg.to_str()),
            Some("--state-dir")
        );
        assert_eq!(
            command.args.last().and_then(|arg| arg.to_str()),
            Some("mcp")
        );
    }
}
