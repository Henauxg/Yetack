//! The agents-cockpit command line: the daemon, the SSH bridge, and client commands.

#[cfg(unix)]
mod bridge;
mod cli;
mod client;

use std::process::ExitCode;

#[cfg(unix)]
use cockpit_daemon::{DaemonConfig, HOME_ENV};
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
    let bridging = matches!(cli.command, Command::Bridge);
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
            cockpit_daemon::serve(config, shutdown_signal()?).await?;
            Ok(())
        }
        #[cfg(unix)]
        Command::Bridge => bridge::run(&cli.target.local_socket()?).await,
        command => client::run(&cli.target, command).await,
    }
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
