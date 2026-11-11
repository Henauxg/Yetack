//! The agents-cockpit desktop app: a window that watches and steers the agents of one daemon.

// A GUI program on Windows, so that starting it opens no console window. A debug build keeps the
// console, which shows its log.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod bridge;
mod browser;
mod config;
mod connection;
mod daemon_label;
mod demo;
mod fonts;
mod format;
mod in_app_host;
mod keymap;
mod logging;
mod machine;
mod notifications;
mod path_style;
mod prefs;
mod reach;
mod render;
#[cfg(unix)]
mod service;
mod state;
mod ui;
mod update_script;
#[cfg(target_os = "linux")]
mod webview_settings;

use std::process::ExitCode;

use dioxus::LaunchBuilder;
use dioxus::desktop::tao::event::Event;
use dioxus::desktop::tao::window::Icon;
use dioxus::desktop::{LogicalSize, WindowBuilder};

use crate::config::{Config, ConfigError, Environment, MachineConfig};
use crate::connection::Startup;
use crate::daemon_label::DaemonLabel;
use crate::in_app_host::InAppHost;
use crate::machine::MachineId;
use crate::prefs::Prefs;
use crate::reach::Places;
use crate::state::{AppState, MachineState};

const STYLESHEET: &str = include_str!("style.css");

/// The name the window title, the sidebar and the notifications show.
const APP_NAME: &str = "Agents Cockpit";

/// The window's icon, for window managers that read it from the window. A Wayland desktop takes
/// the icon from the desktop entry instead, which `flake.nix` installs with the same artwork.
pub(crate) const ICON_PNG: &[u8] = include_bytes!("../assets/agents-cockpit-256.png");

/// The theme's background, so the window does not flash white before the page draws.
const BACKGROUND: (u8, u8, u8, u8) = (24, 24, 24, 255);

fn main() -> anyhow::Result<ExitCode> {
    #[cfg(unix)]
    if let Some(state_dir) = service::requested(&std::env::args_os().skip(1).collect::<Vec<_>>()) {
        return Ok(service::run(state_dir));
    }
    // SAFETY: no other thread runs yet: nothing above starts one, and the signal watcher, the
    // daemon's threads and the window start below.
    #[cfg(target_os = "linux")]
    unsafe {
        webview_settings::turn_off_accelerated_compositing()
    };
    let environment = Environment::from_process();
    logging::init(&environment);
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let demo = demo::is_requested(&arguments);
    // Logged as well: a GUI program on Windows has no stderr for the error `main` returns.
    let log = |error: &ConfigError| tracing::error!("{}", state::error_chain(error));
    let config = Config::load(&environment).inspect_err(log)?;
    let places = Places::new(&environment).inspect_err(log)?;
    let label = reach::label(&config.daemon);
    let mut state = if demo {
        demo::state(label)
    } else {
        AppState::new(label)
    };
    if let Some(screen) = demo::update_screen(&arguments) {
        demo::show_update_screen(&mut state, screen);
    }
    if demo::showcase_requested(&arguments) {
        demo::showcase(&mut state);
    }
    let machines: Vec<(MachineId, MachineConfig)> =
        (1..).map(MachineId::new).zip(config.machines).collect();
    if !demo {
        for (machine, remote) in &machines {
            state.machines.others.insert(
                *machine,
                MachineState::new(DaemonLabel::ssh(remote.name.clone())),
            );
        }
    }
    // The demo's agents take the ids of real ones: its choices, a muted agent first, stay out of
    // the real preferences.
    let prefs_file = prefs::file(&environment).filter(|_| !demo);
    if let Some(file) = &prefs_file {
        state.prefs = Prefs::load(file);
    }
    state::notification_levels::start_from_config(&mut state, config.notifications.enabled);
    let in_app = InAppHost::default();
    in_app_host::stop_on_signals(in_app.clone());
    in_app_host::stop_on_panic(in_app.clone());
    let startup = Startup {
        state,
        daemon: config.daemon,
        machines,
        places,
        in_app: in_app.clone(),
        notifications: !demo,
        demo,
        prefs_file,
    };
    let window = WindowBuilder::new()
        .with_title(APP_NAME)
        .with_inner_size(LogicalSize::new(1440.0, 900.0))
        .with_min_inner_size(LogicalSize::new(720.0, 480.0));
    let mut desktop = dioxus::desktop::Config::new()
        .with_window(window)
        .with_menu(None)
        .with_background_color(BACKGROUND)
        .with_custom_head(format!(
            "<style>{}{STYLESHEET}</style>",
            fonts::font_faces()
        ))
        .with_custom_event_handler(move |event, _| {
            // The event loop ends the process right after this event, without dropping anything.
            if let Event::LoopDestroyed = event {
                in_app.shutdown();
            }
        });
    match dioxus::desktop::icon_from_memory::<Icon>(ICON_PNG) {
        Ok(icon) => desktop = desktop.with_icon(icon),
        Err(error) => tracing::warn!("cannot load the window icon: {error}"),
    }
    LaunchBuilder::desktop()
        .with_cfg(desktop)
        .with_context(startup)
        .launch(ui::App);
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_decodes() {
        assert!(dioxus::desktop::icon_from_memory::<Icon>(ICON_PNG).is_ok());
    }
}
