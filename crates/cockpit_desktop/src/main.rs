//! The agents-cockpit desktop app: a window that watches and steers the agents of one daemon.

mod bridge;
mod config;
mod connection;
mod demo;
mod format;
mod notifications;
mod render;
mod state;
mod ui;

use dioxus::LaunchBuilder;
use dioxus::desktop::{LogicalSize, WindowBuilder};
use tracing_subscriber::EnvFilter;

use crate::config::{Config, Environment};
use crate::connection::Startup;
use crate::state::AppState;

const STYLESHEET: &str = include_str!("style.css");

/// The name the window title, the sidebar and the notifications show.
const APP_NAME: &str = "agents-cockpit";

/// The theme's background, so the window does not flash white before the page draws.
const BACKGROUND: (u8, u8, u8, u8) = (24, 24, 24, 255);

/// Log level when `RUST_LOG` is not set.
const DEFAULT_LOG: &str = "warn";

fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG));
    tracing_subscriber::fmt().with_env_filter(filter).init();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let demo = demo::is_requested(&arguments);
    let environment = Environment::from_process();
    let config = Config::load(&environment)?;
    let state = if demo {
        demo::state(config.daemon_label())
    } else {
        AppState::new(config.daemon_label())
    };
    let startup = Startup {
        state,
        transport: config.transport(&environment)?,
        notifications: config.notifications.enabled && !demo,
        demo,
    };
    let window = WindowBuilder::new()
        .with_title(APP_NAME)
        .with_inner_size(LogicalSize::new(1440.0, 900.0))
        .with_min_inner_size(LogicalSize::new(720.0, 480.0));
    let desktop = dioxus::desktop::Config::new()
        .with_window(window)
        .with_menu(None)
        .with_background_color(BACKGROUND)
        .with_custom_head(format!("<style>{STYLESHEET}</style>"));
    LaunchBuilder::desktop()
        .with_cfg(desktop)
        .with_context(startup)
        .launch(ui::App);
    Ok(())
}
