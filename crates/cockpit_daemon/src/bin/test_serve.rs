//! The daemon in a process of its own, for the integration tests: a client inside the daemon's
//! process would fail the peer check. It reads the same environment as `agents-cockpit serve`.

use std::path::PathBuf;
use std::process::ExitCode;

use cockpit_daemon::{DaemonConfig, HOME_ENV};
use tokio::signal::unix;
use tokio::signal::unix::SignalKind;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::new("debug"))
        .init();
    let Some(state_dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: test-serve <state folder>");
        return ExitCode::FAILURE;
    };
    let Some(config) = DaemonConfig::from_env(Some(state_dir)) else {
        eprintln!("{HOME_ENV} is not set");
        return ExitCode::FAILURE;
    };
    let Ok(mut terminate) = unix::signal(SignalKind::terminate()) else {
        eprintln!("cannot handle SIGTERM");
        return ExitCode::FAILURE;
    };
    let shutdown = async move {
        terminate.recv().await;
    };
    match cockpit_daemon::serve(config, shutdown).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("test-serve: {error}");
            ExitCode::FAILURE
        }
    }
}
