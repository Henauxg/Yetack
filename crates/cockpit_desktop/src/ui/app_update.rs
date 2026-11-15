//! The app's own update: the note at the foot of the sidebar, the rows of Settings' About, and the
//! card that offers a newer release and shows its update.

use std::cmp::Ordering;

use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;
use strum::VariantArray;

use crate::connection::{Clock, Cockpit};
use crate::state::app_update::{
    self, AppStep, InstallKind, NoteLine, Release, ReleaseNote, RunPhase, UpdateRun,
};
use crate::ui::accounts::CommandBox;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::presence::use_presence;
use crate::ui::settings::APP_ICON;
use crate::update_script::{APP_VERSION, app_commit};
use crate::{browser, format, self_update};

const TITLE_ID: &str = "app-update-title";
const CHECK_LABEL_ID: &str = "setting-release-check";

/// What a restart does to the agents, as the offer and the ready card say it.
const AGENTS_RESTART: &str = "Local's agents stop with the app and resume after the restart.";

/// Asks the release check to look now: the setting turned on.
#[derive(Clone, Copy)]
pub struct ReleaseChecks(Coroutine<()>);

/// Starts the release check, and provides what asks it to look now.
pub fn use_release_checks(cockpit: Cockpit) -> ReleaseChecks {
    let checks = use_coroutine(move |asked: UnboundedReceiver<()>| {
        self_update::check_releases(cockpit.state, asked)
    });
    use_context_provider(|| ReleaseChecks(checks))
}

/// "Update and restart", or "Try again": the update runs beside the window, and the card shows it.
fn start_update(cockpit: Cockpit) {
    let mut signal = cockpit.state;
    let (started, demo) = {
        let mut state = signal.write();
        (app_update::start(&mut state), state.app_update.demo)
    };
    let Some(started) = started else {
        return;
    };
    if demo {
        spawn_forever(self_update::play_demo(signal, started));
    } else {
        spawn_forever(self_update::run(signal, started));
    }
}

fn open_page(cockpit: Cockpit, url: String) {
    spawn_forever(async move {
        if let Err(error) = browser::open(&url).await {
            cockpit.update(|state| state.ui.report(format!("Cannot open {url}: {error}")));
        }
    });
}

/// Starts the new app, then closes the window as its close button does, which stops the agents
/// that run in the app. The demo only closes the card.
fn restart_now(cockpit: Cockpit) {
    let (app, demo) = {
        let state = cockpit.state.read();
        let Some(UpdateRun {
            phase: RunPhase::Ready(app),
            ..
        }) = &state.app_update.run
        else {
            return;
        };
        (app.clone(), state.app_update.demo)
    };
    if demo {
        cockpit.update(|state| state.ui.close_dialog());
        return;
    }
    match self_update::start_new_app(&app) {
        Ok(()) => dioxus::desktop::window().close(),
        Err(error) => cockpit.update(|state| {
            let message = format!("cannot start {}: {error}", app.display());
            app_update::restart_failed(state, message);
            app_update::open_card(state);
        }),
    }
}

/// "Released 3 days ago."
fn released_sentence(release: &Release, now: std::time::SystemTime) -> Option<String> {
    let text = app_update::released(release.published?, now);
    let mut letters = text.chars();
    let first = letters.next()?.to_uppercase();
    Some(format!("{first}{}.", letters.as_str()))
}

/// The note at the foot of the sidebar: a newer release, its update under way, or how it ended.
/// It folds open and shut, so the foot's buttons move with it.
#[component]
pub fn ReleaseNoteLine() -> Element {
    let cockpit: Cockpit = use_context();
    let note = app_update::note(&cockpit.state.read());
    let Some(shown) = use_presence(note) else {
        return rsx! {};
    };
    let link = |label: &'static str, action: fn(Cockpit)| {
        rsx! {
            button { class: "link-button", onclick: move |_| action(cockpit), "{label}" }
        }
    };
    let cross = |title: &'static str, action: fn(&mut crate::state::AppState)| {
        rsx! {
            button {
                class: "icon-button tiny release-note-close",
                title,
                aria_label: title,
                onclick: move |_| cockpit.update(action),
                Icon { kind: IconKind::Close, size: 12 }
            }
        }
    };
    let open: fn(Cockpit) = |cockpit| cockpit.update(app_update::open_card);
    rsx! {
        div {
            class: if shown.leaving { "release-note-fold leaving" } else { "release-note-fold" },
            inert: shown.leaving.then_some(true),
            div { class: "release-note-clip",
                div { class: "release-note", role: "status",
                    span { class: "release-dot", aria_hidden: true }
                    match shown.value {
                        ReleaseNote::Out(version) => rsx! {
                            span { class: "release-note-text", "{version} is out" }
                            {link("Update", open)}
                            {cross("Skip this version", app_update::skip)}
                        },
                        ReleaseNote::Progress { text, percent } => rsx! {
                            span { class: "release-note-text", "{text}" }
                            {link("Show", open)}
                            span {
                                class: "update-bar",
                                role: "progressbar",
                                aria_valuenow: "{percent}",
                                aria_valuemin: "0",
                                aria_valuemax: "100",
                                span { style: format!("transform: scaleX({})", f32::from(percent) / 100.0) }
                            }
                        },
                        ReleaseNote::Ready(version) => rsx! {
                            span { class: "release-note-text", "{version} is ready" }
                            {link("Restart", restart_now)}
                        },
                        ReleaseNote::Failed => rsx! {
                            span { class: "release-note-text", "Update failed" }
                            {link("Show", open)}
                            {cross("Dismiss", app_update::dismiss_failure)}
                        },
                    }
                }
            }
        }
    }
}

/// Settings' About: the newer release with its buttons, and the setting of the check.
#[component]
pub fn AboutRelease() -> Element {
    let cockpit: Cockpit = use_context();
    let checks: ReleaseChecks = use_context();
    let Clock(now) = use_context::<Clock>();
    let (offered, check) = {
        let state = cockpit.state.read();
        (
            app_update::offered(&state).cloned(),
            state.prefs.updates.check,
        )
    };
    let shown = use_presence(offered);
    let set_check = move |on: bool| {
        cockpit.update(|state| app_update::set_check(state, on));
        if on && !check {
            checks.0.send(());
        }
    };
    rsx! {
        if let Some(shown) = shown {
            div {
                class: if shown.leaving { "release-row-fold leaving" } else { "release-row-fold" },
                inert: shown.leaving.then_some(true),
                div { class: "release-row-clip",
                    ReleaseRow { release: shown.value, now: now() }
                }
            }
        }
        div { class: "setting-row",
            div { class: "setting-text",
                span { id: CHECK_LABEL_ID, class: "setting-label", "Look for new releases" }
                span { class: "setting-help",
                    "Once a day, the app asks GitHub for its latest release. GitHub sees your IP address, as it does for any visit."
                }
            }
            div { class: "segmented", role: "group", aria_labelledby: CHECK_LABEL_ID,
                for (on, label) in [(true, "On"), (false, "Off")] {
                    button {
                        key: "{label}",
                        class: if on == check { "segment chosen" } else { "segment" },
                        aria_pressed: on == check,
                        onclick: move |_| set_check(on),
                        "{label}"
                    }
                }
            }
        }
    }
}

#[component]
fn ReleaseRow(release: Release, now: std::time::SystemTime) -> Element {
    let cockpit: Cockpit = use_context();
    let (install, running) = {
        let state = cockpit.state.read();
        (state.app_update.install, state.app_update.run.is_some())
    };
    let help = match released_sentence(&release, now) {
        Some(released) => format!("{released} {}", install.about_line()),
        None => install.about_line().to_owned(),
    };
    let updates = install.updates_itself() && release.windows_assets().is_some();
    let page = release.page().to_owned();
    rsx! {
        div { class: "setting-row release-row",
            div { class: "setting-text",
                span { class: "setting-label", "Yetack {release.version} is out" }
                span { class: "setting-help", "{help}" }
            }
            div { class: "about-actions",
                button {
                    class: "button",
                    onclick: move |_| open_page(cockpit, page.clone()),
                    "What's new"
                }
                if running {
                    button {
                        class: "button primary",
                        onclick: move |_| cockpit.update(app_update::open_card),
                        "Show"
                    }
                } else if updates {
                    StartButton { label: "Update and restart" }
                } else {
                    button {
                        class: "button primary",
                        onclick: move |_| cockpit.update(app_update::open_card),
                        "See how"
                    }
                }
            }
        }
    }
}

/// The card of `Dialog::AppUpdate`: the offer, the update under way, ready or failed.
#[component]
pub fn AppUpdateCard() -> Element {
    let cockpit: Cockpit = use_context();
    let update = cockpit.state.read().app_update.clone();
    let body = match (update.run, update.release) {
        (Some(run), _) => match run.phase.clone() {
            RunPhase::Running(_) => rsx! {
                RunningCard { run }
            },
            RunPhase::Ready(_) => rsx! {
                ReadyCard { run }
            },
            RunPhase::Failed { step, message } => rsx! {
                FailedCard { run, step, message }
            },
        },
        (None, Some(release)) => rsx! {
            OfferCard { release, install: update.install }
        },
        (None, None) => return rsx! {},
    };
    rsx! {
        div {
            class: "no-daemon-card update-card",
            role: "dialog",
            aria_labelledby: TITLE_ID,
            onclick: move |event| event.stop_propagation(),
            {body}
        }
    }
}

/// The card's head: the app's icon, then the title over what it means.
#[component]
fn CardHead(title: String, lead: String) -> Element {
    rsx! {
        div { class: "update-card-head",
            span { class: "update-card-icon app-mark", aria_hidden: true,
                img { src: APP_ICON.as_str(), alt: "" }
            }
            div { class: "update-card-titles",
                h1 { id: TITLE_ID, "{title}" }
                p { class: "update-card-lead", "{lead}" }
            }
        }
    }
}

#[component]
fn Callout(text: String) -> Element {
    rsx! {
        div { class: "update-callout",
            span { class: "callout-icon", aria_hidden: true,
                Icon { kind: IconKind::Info, size: 16 }
            }
            span { "{text}" }
        }
    }
}

/// This app's build beside the release.
#[component]
fn VersionPair(release: Release) -> Element {
    let Clock(now) = use_context::<Clock>();
    let commit = app_commit();
    let released = release
        .published
        .map(|published| app_update::released(published, now()));
    rsx! {
        div { class: "version-pair",
            div { class: "version-box behind",
                span { class: "version-who", "This app" }
                span { class: "version-number", "{APP_VERSION}" }
                if let Some(commit) = commit {
                    span { class: "version-meta mono", "{commit.short()}" }
                }
            }
            span { class: "version-arrow", aria_hidden: true,
                Icon { kind: IconKind::ArrowRight, size: 18 }
            }
            div { class: "version-box",
                span { class: "version-who", "Latest release" }
                span { class: "version-number", "{release.version}" }
                if let Some(released) = released {
                    span { class: "version-meta mono", "{released}" }
                }
            }
        }
    }
}

/// A block of the notes: a heading, a list, or a paragraph.
#[derive(Debug, Clone, PartialEq)]
enum NoteBlock {
    Heading(String),
    List(Vec<String>),
    Text(String),
}

fn note_blocks(markdown: &str) -> Vec<NoteBlock> {
    let mut blocks: Vec<NoteBlock> = Vec::new();
    for line in app_update::note_lines(markdown) {
        match (line, blocks.last_mut()) {
            (NoteLine::Bullet(text), Some(NoteBlock::List(items))) => items.push(text),
            (NoteLine::Bullet(text), _) => blocks.push(NoteBlock::List(vec![text])),
            (NoteLine::Heading(text), _) => blocks.push(NoteBlock::Heading(text)),
            (NoteLine::Text(text), _) => blocks.push(NoteBlock::Text(text)),
        }
    }
    blocks
}

/// The first lines of the release notes, which fade at the box's foot, and the link to the rest.
#[component]
fn ReleaseNotes(release: Release) -> Element {
    let cockpit: Cockpit = use_context();
    let blocks = note_blocks(&release.notes);
    let page = release.page().to_owned();
    rsx! {
        div { class: "release-notes",
            if !blocks.is_empty() {
                div { class: "release-notes-text",
                    for block in blocks {
                        match block {
                            NoteBlock::Heading(text) => rsx! {
                                b { "{text}" }
                            },
                            NoteBlock::List(items) => rsx! {
                                ul {
                                    for item in items {
                                        li { "{item}" }
                                    }
                                }
                            },
                            NoteBlock::Text(text) => rsx! {
                                p { "{text}" }
                            },
                        }
                    }
                }
            }
            button {
                class: "release-notes-link",
                onclick: move |_| open_page(cockpit, page.clone()),
                "The full notes on GitHub"
                Icon { kind: IconKind::External, size: 12 }
            }
        }
    }
}

/// "Skip this version", at the left of an offer's buttons.
#[component]
fn SkipButton() -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        button {
            class: "details-link",
            onclick: move |_| cockpit.update(app_update::skip),
            "Skip this version"
        }
    }
}

#[component]
fn NotNowButton(primary: bool) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        button {
            class: if primary { "button primary" } else { "button" },
            onclick: move |_| cockpit.update(app_update::not_now),
            "Not now"
        }
    }
}

#[component]
fn ReleasePageButton(page: String) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        button {
            class: "button primary",
            onclick: move |_| open_page(cockpit, page.clone()),
            "Open the release page"
        }
    }
}

/// The offer of a newer release, as this app's install takes it.
#[component]
fn OfferCard(release: Release, install: InstallKind) -> Element {
    let version = release.version.clone();
    let page = release.page().to_owned();
    let has_files = release.windows_assets().is_some();
    rsx! {
        CardHead { title: format!("Yetack {version} is out"), lead: install.lead(APP_VERSION) }
        if install == InstallKind::Zip {
            VersionPair { release: release.clone() }
        }
        if matches!(install, InstallKind::Zip | InstallKind::Deb) {
            ReleaseNotes { release: release.clone() }
        }
        match install {
            InstallKind::Zip if has_files => rsx! {
                Callout {
                    text: format!(
                        "Downloads {version} from GitHub and checks it against the release's checksums, then restarts the app. {AGENTS_RESTART} Your servers keep running: update their daemons after, from the sidebar."
                    ),
                }
            },
            InstallKind::Zip => rsx! {
                Callout {
                    text: format!(
                        "The {version} release has no Windows app to download. Its page may have one later."
                    ),
                }
            },
            InstallKind::Deb => rsx! {
                div { class: "owner-steps",
                    p {
                        "apt installed this app, so it does not replace itself. Download the {version} "
                        code { ".deb" }
                        " from the release page, then run:"
                    }
                    CommandBox { command: app_update::apt_command(&version) }
                }
            },
            InstallKind::Nix => rsx! {
                Callout {
                    text: format!(
                        "Nix installed this app, so it does not replace itself. Move it to {version} in your flake or your system configuration."
                    ),
                }
            },
            InstallKind::Source => rsx! {
                div { class: "owner-steps",
                    p { "In your checkout, build the release, then start the app again:" }
                    CommandBox { command: app_update::build_command(&version) }
                }
            },
        }
        div { class: "update-card-actions",
            SkipButton {}
            match install {
                InstallKind::Zip if has_files => rsx! {
                    NotNowButton { primary: false }
                    StartButton { label: "Update and restart" }
                },
                InstallKind::Zip | InstallKind::Deb => rsx! {
                    NotNowButton { primary: false }
                    ReleasePageButton { page }
                },
                InstallKind::Nix | InstallKind::Source => rsx! {
                    NotNowButton { primary: true }
                },
            }
        }
    }
}

/// The button that starts the update. It waits while a cancelled run deletes its downloads.
#[component]
fn StartButton(label: &'static str) -> Element {
    let cockpit: Cockpit = use_context();
    let stopping = app_update::is_stopping(&cockpit.state.read());
    rsx! {
        button {
            class: "button primary",
            disabled: stopping,
            onclick: move |_| start_update(cockpit),
            if stopping { "Cancelling…" } else { "{label}" }
        }
    }
}

/// The steps of an update, each marked done, under way, failed or to come.
#[component]
fn Steps(run: UpdateRun, failure: Option<String>) -> Element {
    let current = run.current_step();
    let failed = failure.is_some();
    rsx! {
        ol { class: "update-steps",
            for &step in AppStep::VARIANTS {
                StepRow {
                    key: "{step:?}",
                    mark: match step.cmp(&current) {
                        Ordering::Less => StepMark::Done,
                        Ordering::Equal if failed => StepMark::Failed,
                        Ordering::Equal => StepMark::Now,
                        Ordering::Greater => StepMark::ToCome,
                    },
                    label: step_label(step, &run.version),
                    detail: step_detail(step, &run, failure.as_deref()),
                }
            }
        }
    }
}

fn step_label(step: AppStep, version: &str) -> String {
    match step {
        AppStep::Download => format!("Download {version}"),
        AppStep::Check => "Check it against the release's checksums".to_owned(),
        AppStep::Replace => "Put it in place of this app".to_owned(),
        AppStep::Restart => "Restart".to_owned(),
    }
}

/// The line under a step: the bytes of the download, or why the step failed.
fn step_detail(step: AppStep, run: &UpdateRun, failure: Option<&str>) -> Option<String> {
    let current = run.current_step();
    if step == current
        && let Some(failure) = failure
    {
        return Some(failure.to_owned());
    }
    match (step, step.cmp(&current)) {
        (AppStep::Download, Ordering::Equal) => Some(format!(
            "{} of {}",
            format::bytes(run.downloaded),
            format::bytes(run.total)
        )),
        (AppStep::Download, Ordering::Less) => Some(format::bytes(run.total)),
        _ => None,
    }
}

/// Where a step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepMark {
    Done,
    Now,
    Failed,
    ToCome,
}

#[component]
fn StepRow(mark: StepMark, label: String, detail: Option<String>) -> Element {
    let (class, icon) = match mark {
        StepMark::Done => (
            "done",
            rsx! {
                Icon { kind: IconKind::Check, size: 14 }
            },
        ),
        StepMark::Now => (
            "now",
            rsx! {
                span { class: "spinner", role: "status", aria_label: "In progress" }
            },
        ),
        StepMark::Failed => (
            "failed",
            rsx! {
                Icon { kind: IconKind::Close, size: 14 }
            },
        ),
        StepMark::ToCome => (
            "",
            rsx! {
                Icon { kind: IconKind::Circle, size: 12 }
            },
        ),
    };
    rsx! {
        li { class,
            span { class: "update-mark", {icon} }
            span { class: "step-text",
                "{label}"
                if let Some(detail) = detail {
                    span { class: "step-detail",
                        span { "{detail}" }
                    }
                }
            }
        }
    }
}

#[component]
fn RunningCard(run: UpdateRun) -> Element {
    let cockpit: Cockpit = use_context();
    let lead = format!(
        "Hiding this card does not stop the download. Once {} is in place, the app asks to restart.",
        run.version
    );
    let cancels = run.offers_cancel();
    rsx! {
        CardHead { title: "Updating Yetack".to_owned(), lead }
        Steps { run, failure: None }
        div { class: "update-card-actions",
            if cancels {
                button {
                    class: "button",
                    onclick: move |_| cockpit.update(app_update::cancel),
                    "Cancel"
                }
            }
            button {
                class: "button primary",
                onclick: move |_| cockpit.update(|state| state.ui.close_dialog()),
                "Hide"
            }
        }
    }
}

#[component]
fn ReadyCard(run: UpdateRun) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        CardHead {
            title: format!("Yetack {} is ready", run.version),
            lead: format!("Restart to run it. {AGENTS_RESTART}"),
        }
        Steps { run, failure: None }
        div { class: "update-card-actions",
            button {
                class: "button",
                onclick: move |_| cockpit.update(|state| state.ui.close_dialog()),
                "Restart later"
            }
            button {
                class: "button primary",
                onclick: move |_| restart_now(cockpit),
                "Restart now"
            }
        }
    }
}

#[component]
fn FailedCard(run: UpdateRun, step: AppStep, message: String) -> Element {
    let cockpit: Cockpit = use_context();
    let lead = match step {
        AppStep::Download | AppStep::Check => {
            format!("Nothing changed: you still run {APP_VERSION}.")
        }
        AppStep::Replace => "The app could not put the new files in place.".to_owned(),
        AppStep::Restart => format!(
            "{} is in place, but the app could not start it. Start the app again to run it.",
            run.version
        ),
    };
    rsx! {
        CardHead { title: "The update failed".to_owned(), lead }
        Steps { run, failure: Some(message) }
        div { class: "update-card-actions",
            button {
                class: if step == AppStep::Restart { "button primary" } else { "button" },
                onclick: move |_| cockpit.update(|state| state.ui.close_dialog()),
                "Close"
            }
            if step != AppStep::Restart {
                StartButton { label: "Try again" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::state::app_update::RunId;

    #[test]
    fn the_notes_group_their_bullets() {
        let markdown = "## Yetack 9.1.0\n\n- One.\n- Two.\n\nTested.\n\n- Three.\n";

        assert_eq!(
            note_blocks(markdown),
            [
                NoteBlock::Heading("Yetack 9.1.0".to_owned()),
                NoteBlock::List(vec!["One.".to_owned(), "Two.".to_owned()]),
                NoteBlock::Text("Tested.".to_owned()),
                NoteBlock::List(vec!["Three.".to_owned()]),
            ]
        );
    }

    #[test]
    fn the_download_step_says_its_bytes_and_a_failed_step_its_reason() {
        let mut run = UpdateRun {
            id: test_run_id(),
            version: "9.1.0".to_owned(),
            downloaded: 5_452_595,
            total: 14_784_921,
            phase: RunPhase::Running(AppStep::Download),
        };
        assert_eq!(
            step_detail(AppStep::Download, &run, None).as_deref(),
            Some("5.2 MB of 14 MB")
        );
        assert_eq!(step_detail(AppStep::Check, &run, None), None);

        run.phase = RunPhase::Ready(PathBuf::from("/made-up/app.exe"));
        assert_eq!(
            step_detail(AppStep::Download, &run, None).as_deref(),
            Some("14 MB")
        );

        run.phase = RunPhase::Failed {
            step: AppStep::Check,
            message: "No match.".to_owned(),
        };
        assert_eq!(
            step_detail(AppStep::Check, &run, Some("No match.")).as_deref(),
            Some("No match.")
        );
        assert_eq!(step_label(AppStep::Download, "9.1.0"), "Download 9.1.0");
    }

    fn test_run_id() -> RunId {
        let mut state = crate::state::AppState::new(crate::daemon_label::DaemonLabel::in_app());
        state.app_update.install = InstallKind::Zip;
        state.app_update.release = Some(crate::demo::app_release());
        app_update::start(&mut state).unwrap().id
    }
}
