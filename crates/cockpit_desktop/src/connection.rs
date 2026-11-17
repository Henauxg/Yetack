//! The connection task: a coroutine that owns the daemon session, applies its updates to the
//! state, and runs the requests the components ask for.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use chrono::NaiveDate;
use cockpit_client::{
    Backoff, RequestError, Requester, Session, SessionError, SessionUpdate, Transport,
};
use cockpit_daemon::PROGRAM;
use cockpit_protocol::{AgentId, Catchup, Request, TerminalChange, TerminalUpdate};
use dioxus::desktop::DesktopContext;
use dioxus::prelude::*;
use tokio::sync::mpsc;

use crate::config::{DaemonConfig, MachineConfig, save_daemon, save_machines};
use crate::daemon_label::{DaemonLabel, DaemonPlace};
use crate::demo::LocalScene;
use crate::in_app_host::InAppHost;
use crate::machine::MachineId;
use crate::notifications::Notifier;
use crate::prefs::NotificationLevel;
use crate::reach::{self, Places, Reached, Unreached};
use crate::service_update::{self, BusyCheck, SystemService};
use crate::state::daemon_probe::{LinuxCpu, ProbeTarget};
use crate::state::daemon_start::{self, StartRun};
use crate::state::daemon_update::{self, OutputLine, OutputStream};
use crate::state::machine_load::{LOAD_POLL, LoadView, ShownLoad};
use crate::state::machines::{self, Adding, MachineChange};
use crate::state::no_daemon::{self, Absence, Chosen, DaemonChoice, SERVICE_OFFERED};
use crate::state::server_setup::{self, InstallReport, SetupRun};
use crate::state::settings::{self, StartDaemon};
use crate::state::update_offer::{self, VersionGap};
use crate::state::update_plan::{
    BusyAgents, DaemonSource, JobCheck, SendProgress, ServerPlan, ServicePlan, UpdatePlan,
    UpdateRun, UpdateStep,
};
use crate::state::{
    self, AgentEntry, AppState, ConnectionState, Dialog, Drafts, MachineState, Outgoing, Purpose,
    TitlePlace, Utf16Offset, actions, file_reread, notification_levels,
};
use crate::terminal_feed::{self, FeedSender, Output};
use crate::update_script::APP_VERSION;
use crate::{bridge, demo, format, release_daemon, server_daemon, update_script};

/// What the window was started with.
#[derive(Debug, Clone, PartialEq)]
pub struct Startup {
    /// The state to start from. It holds this computer's daemon as the config names it.
    pub state: AppState,
    /// The other machines the config names, reached over SSH.
    pub machines: Vec<(MachineId, MachineConfig)>,
    pub places: Places,
    /// The daemon the app runs itself, which the window's close stops.
    pub in_app: InAppHost,
    /// Whether the app can raise notifications at all: the demo never does. The levels in the
    /// preferences decide which ones it raises.
    pub notifications: bool,
    /// Answers requests from the sample state instead of a daemon.
    pub demo: bool,
    /// What the demo shows of where Local's agents run (`--demo-local`).
    pub demo_local: Option<LocalScene>,
    /// Where the preferences are saved, `None` without a home folder.
    pub prefs_file: Option<PathBuf>,
}

/// The requests of one machine's connection task.
type Requests = mpsc::UnboundedReceiver<ToDaemon>;

/// Takes the requests a machine's task finds for another machine, to the router.
type Router = mpsc::UnboundedSender<Outgoing>;

/// How long a machine that does not answer waits before the app tries it again.
const REMOTE_RETRY: Duration = Duration::from_secs(10);

/// What components ask of the connection task.
pub enum ToDaemon {
    Send(Outgoing),
    /// A choice on the "No daemon answers" screen.
    Choose(Chosen),
    /// A server added or removed in the "Machines" dialog, or tried again.
    Machines(MachineChange),
    /// Runs or cancels the update of a machine's daemon, beside its connection: a daemon of an
    /// older protocol keeps refusing the app meanwhile.
    Update(UpdateCommand),
    /// The notification levels changed, or the window got the focus back: each machine's notifier
    /// closes what they no longer let through.
    Notifications,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCommand {
    /// Reads the raw logs of this computer's resting agents, then starts the update as
    /// `daemon_update::jobs_checked` says.
    CheckJobs(MachineId, JobCheck),
    Start(MachineId, UpdateRun),
    /// Asks a server's running script to stop before it installs.
    Cancel(MachineId, ServerPlan),
    /// Reads where a server's daemon is and who owns it: the host, and the daemon's path or
    /// command.
    Probe(MachineId, ProbeTarget),
    /// A step of a new server's setup, from "Add a server".
    Setup(SetupRun),
    /// "Start the daemon" on a server where none runs.
    StartDaemon(MachineId, StartRun),
    /// "Add it to my crontab" on a server's row in the Machines dialog, for the daemon named.
    BootStart(MachineId, StartRun),
}

/// What components hold to read and change the state and to reach the daemon. The drafts are a
/// signal of their own: typing changes them alone, so the views that read the state, the
/// transcript first, do not render again. The machine's load is one too: it changes every few
/// seconds, and only the views that show it render again.
#[derive(Clone, Copy)]
pub struct Cockpit {
    pub state: Signal<AppState>,
    pub drafts: Signal<Drafts>,
    pub load: Signal<ShownLoad>,
    daemon: Coroutine<ToDaemon>,
}

impl Cockpit {
    /// Changes the state, then sends the requests the change returns and the ones the open views
    /// now need.
    pub fn change(&self, change: impl FnOnce(&mut AppState) -> Vec<Outgoing>) {
        let mut signal = self.state;
        let requests = {
            let mut state = signal.write();
            let mut requests = change(&mut state);
            requests.extend(state::needed_requests(&mut state));
            state::keep_first_answers(&mut state, &mut requests);
            requests
        };
        for request in requests {
            self.daemon.send(ToDaemon::Send(request));
        }
    }

    /// Sends a terminal's keys or size without writing the state, which would render the window
    /// again for each key. Only `Purpose::TerminalKeys` may skip the state.
    pub fn send_terminal_keys(&self, request: Outgoing) {
        debug_assert!(matches!(request.purpose, Purpose::TerminalKeys));
        self.daemon.send(ToDaemon::Send(request));
    }

    /// Adds or removes a server, or tries one again.
    pub fn change_machines(&self, change: MachineChange) {
        self.daemon.send(ToDaemon::Machines(change));
    }

    /// A choice in the Settings dialog of where Local's agents run from the next start, which the
    /// connection task writes to the config.
    pub fn choose_start(&self, choice: StartDaemon) {
        let mut signal = self.state;
        let next = settings::choose_start(&mut signal.write(), choice);
        if let Some(next) = next {
            self.change_machines(MachineChange::LocalDaemon(next));
        }
    }

    /// Starts the update of `machine`'s daemon, from a card, "Update again" or "Retry". Busy
    /// agents open the confirmation first. Before a restart of this computer's service, the app
    /// reads which agents may hold a scheduled job, beside the window. A click during that read does
    /// nothing.
    pub fn update_daemon(&self, machine: MachineId) {
        let mut signal = self.state;
        let check = daemon_update::jobs_to_check(&mut signal.write(), machine);
        match check {
            Some(check) => {
                self.daemon
                    .send(ToDaemon::Update(UpdateCommand::CheckJobs(machine, check)));
            }
            None => self.begin_update(machine, |state| daemon_update::start(state, machine)),
        }
    }

    /// "Update when they are idle", on the confirmation.
    pub fn update_daemon_when_idle(&self, machine: MachineId) {
        self.begin_update(machine, |state| {
            daemon_update::wait_for_idle(state, machine)
        });
    }

    /// "Update now": the restart ends the busy agents' turns.
    pub fn update_daemon_now(&self, machine: MachineId) {
        self.begin_update(machine, |state| daemon_update::start_now(state, machine));
    }

    fn begin_update(
        &self,
        machine: MachineId,
        begin: impl FnOnce(&mut AppState) -> Option<UpdateRun>,
    ) {
        let mut signal = self.state;
        let run = begin(&mut signal.write());
        if let Some(run) = run {
            self.daemon
                .send(ToDaemon::Update(UpdateCommand::Start(machine, run)));
        }
    }

    pub fn cancel_daemon_update(&self, machine: MachineId) {
        let mut signal = self.state;
        let plan = daemon_update::ask_cancel(&mut signal.write(), machine);
        if let Some(plan) = plan {
            self.daemon
                .send(ToDaemon::Update(UpdateCommand::Cancel(machine, plan)));
        }
    }

    /// Reads where `machine`'s daemon is installed and who owns it, when its update screens need
    /// it and the app has not looked yet.
    pub fn probe_daemon(&self, machine: MachineId) {
        let mut signal = self.state;
        // A write tells every reader, so it waits until a probe is due.
        if !daemon_update::probe_due(&signal.peek(), machine) {
            return;
        }
        let target = daemon_update::probe_to_run(&mut signal.write(), machine);
        if let Some(target) = target {
            self.daemon
                .send(ToDaemon::Update(UpdateCommand::Probe(machine, target)));
        }
    }

    /// "Add the server": the server joins the app, or without a path, the app looks for its
    /// daemon first.
    pub fn add_server(&self) {
        let mut signal = self.state;
        let adding = machines::add(&mut signal.write());
        match adding {
            Some(Adding::Add(change)) => self.change_machines(change),
            Some(Adding::Setup(run)) => self.run_setup(run),
            None => {}
        }
    }

    /// "Install on <server>", in the setup of a new server.
    pub fn install_server(&self) {
        let mut signal = self.state;
        let run = server_setup::install(&mut signal.write());
        if let Some(run) = run {
            self.run_setup(run);
        }
    }

    /// "Try again", once a setup failed.
    pub fn retry_server_setup(&self) {
        let mut signal = self.state;
        let run = server_setup::retry(&mut signal.write());
        if let Some(run) = run {
            self.run_setup(run);
        }
    }

    /// "Add it to my crontab", once the setup's install started the daemon without systemd.
    pub fn add_boot_start_on_server(&self) {
        let mut signal = self.state;
        let run = server_setup::add_boot_start(&mut signal.write());
        if let Some(run) = run {
            self.run_setup(run);
        }
    }

    /// "Check again", once the setup's install is done.
    pub fn check_server_again(&self) {
        let mut signal = self.state;
        let run = server_setup::check_again(&mut signal.write());
        if let Some(run) = run {
            self.run_setup(run);
        }
    }

    /// Runs a step of a server's setup beside the window: the Machines dialog may close before it
    /// ends.
    fn run_setup(&self, run: SetupRun) {
        self.daemon
            .send(ToDaemon::Update(UpdateCommand::Setup(run)));
    }

    /// "Start the daemon" on a server where none runs. Once it runs, the app connects again.
    pub fn start_daemon(&self, machine: MachineId) {
        let mut signal = self.state;
        let run = daemon_start::start(&mut signal.write(), machine);
        if let Some(run) = run {
            self.daemon
                .send(ToDaemon::Update(UpdateCommand::StartDaemon(machine, run)));
        }
    }

    /// "Add it to my crontab" on a server's row in the Machines dialog.
    pub fn add_boot_start_on_machine(&self, machine: MachineId) {
        let mut signal = self.state;
        let run = daemon_start::add_boot_start(&mut signal.write(), machine);
        if let Some(run) = run {
            self.daemon
                .send(ToDaemon::Update(UpdateCommand::BootStart(machine, run)));
        }
    }

    /// Tries a machine that does not answer again now. The update screens look at its daemon
    /// again too: the user may have updated it by hand.
    pub fn retry_machine(&self, machine: MachineId) {
        let mut signal = self.state;
        daemon_update::forget_probe(&mut signal.write(), machine);
        let retry = retry_message(&mut signal.write(), machine);
        if let Some(retry) = retry {
            self.daemon.send(retry);
        }
    }

    /// A click on one of the "No daemon answers" screen's buttons.
    pub fn choose_daemon(&self, choice: DaemonChoice) {
        let mut signal = self.state;
        let chosen = no_daemon::choose(&mut signal.write(), choice);
        if let Some(chosen) = chosen {
            self.daemon.send(ToDaemon::Choose(chosen));
        }
    }

    /// Sets the notification level of `agent`, or the app's for `None`.
    pub fn set_notifications(&self, agent: Option<AgentId>, level: NotificationLevel) {
        let mut signal = self.state;
        match agent {
            Some(agent) => notification_levels::set_agent(&mut signal.write(), agent, level),
            None => notification_levels::set_app(&mut signal.write(), level),
        }
        self.daemon.send(ToDaemon::Notifications);
    }

    /// The window gained or lost the focus. Back in front, it closes the app's notifications.
    pub fn set_window_focused(&self, focused: bool) {
        self.update(|state| file_reread::window_focused(state, focused));
        if focused {
            self.daemon.send(ToDaemon::Notifications);
        }
    }

    pub fn update(&self, change: impl FnOnce(&mut AppState)) {
        self.change(|state| {
            change(state);
            Vec::new()
        });
    }

    /// Changes the state and the drafts, then sends requests as `change` does.
    pub fn change_with_drafts(
        &self,
        change: impl FnOnce(&mut AppState, &mut Drafts) -> Vec<Outgoing>,
    ) {
        let mut drafts = self.drafts;
        self.change(|state| change(state, &mut drafts.write()));
    }

    pub fn update_with_drafts(&self, change: impl FnOnce(&mut AppState, &mut Drafts)) {
        self.change_with_drafts(|state, drafts| {
            change(state, drafts);
            Vec::new()
        });
    }

    /// Changes the drafts alone: the state is read without being written.
    pub fn with_drafts<R>(&self, change: impl FnOnce(&AppState, &mut Drafts) -> R) -> R {
        let mut drafts = self.drafts;
        let state = self.state.peek();
        change(&state, &mut drafts.write())
    }

    /// Typing in the composer changes the drafts, and the state only when the palette opens or
    /// closes.
    pub fn type_in_composer(&self, agent: AgentId, text: String, caret: Utf16Offset) {
        let palette = self.with_drafts(|state, drafts| {
            actions::composer_input(state, drafts, agent, text, caret)
        });
        if let Some(palette) = palette {
            self.change(|state| actions::change_palette(state, agent, palette));
        }
    }

    /// Typing in a title field, the header's or a sidebar row's, changes the drafts alone.
    pub fn type_title(&self, text: String) {
        self.with_drafts(|state, drafts| actions::rename_text(state, drafts, text));
    }

    /// A title field at `place` lost the focus: the edit ends, unless the window lost it.
    pub fn title_field_blurred(self, place: TitlePlace) {
        bridge::save_on_blur(
            move || {
                self.change_with_drafts(|state, drafts| {
                    actions::finish_rename_at(state, drafts, place)
                });
            },
            move || self.update(|state| actions::set_title_kept_open(state, true)),
        );
    }

    /// A title field took the focus, as it opened or when the window got it back.
    pub fn title_field_focused(self) {
        let kept_open = self
            .state
            .peek()
            .ui
            .title_edit
            .as_ref()
            .is_some_and(|edit| edit.kept_open);
        if kept_open {
            self.update(|state| actions::set_title_kept_open(state, false));
        }
    }
}

/// What tries `machine` again now. This computer, while it shows its screen, goes through the
/// choice its connection task waits for. Otherwise the connection task starts a new attempt.
fn retry_message(state: &mut AppState, machine: MachineId) -> Option<ToDaemon> {
    let on_its_screen = machine == MachineId::THIS
        && matches!(state.machines.this.connection, ConnectionState::NoDaemon(_));
    if on_its_screen {
        no_daemon::choose(state, DaemonChoice::Retry).map(ToDaemon::Choose)
    } else {
        Some(ToDaemon::Machines(MachineChange::Retry(machine)))
    }
}

/// The current time, which ticks every second for the timers on screen.
#[derive(Clone, Copy)]
pub struct Clock(pub Signal<SystemTime>);

const TICK: Duration = Duration::from_secs(1);

pub fn use_clock() -> Clock {
    let mut now = use_signal(SystemTime::now);
    use_future(move || async move {
        loop {
            tokio::time::sleep(TICK).await;
            now.set(SystemTime::now());
        }
    });
    use_today(now);
    use_context_provider(|| Clock(now))
}

/// The local day, which changes at midnight. A component that reads it renders again at midnight,
/// and not on each tick of the clock.
#[derive(Clone, Copy)]
pub struct Today(pub Memo<NaiveDate>);

fn use_today(now: Signal<SystemTime>) -> Today {
    let today = use_memo(move || format::local_day(now()));
    use_context_provider(|| Today(today))
}

/// Starts the connection, and provides the `Cockpit` to every component.
pub fn use_cockpit(startup: Startup) -> Cockpit {
    let Startup {
        state: initial,
        machines,
        places,
        in_app,
        notifications,
        demo,
        demo_local,
        prefs_file: _,
    } = startup;
    let state = use_signal(move || initial);
    let drafts = use_signal(move || {
        if demo {
            demo::drafts()
        } else {
            Drafts::default()
        }
    });
    let load = use_signal(ShownLoad::default);
    let (feed, feed_out) = use_hook(terminal_feed::channel);
    use_context_provider(|| feed_out);
    let daemon = use_coroutine(move |requests: UnboundedReceiver<ToDaemon>| {
        let live = Live {
            places: places.clone(),
            in_app: in_app.clone(),
            notify: notifications,
        };
        let machines = machines.clone();
        let feed = feed.clone();
        async move {
            if demo {
                run_demo(state, drafts, load, requests, demo_local).await;
            } else {
                route(state, drafts, load, live, machines, requests, feed).await;
            }
        }
    });
    use_context_provider(|| Cockpit {
        state,
        drafts,
        load,
        daemon,
    })
}

/// Starts a connection task per machine, and hands each request to the task of the machine its
/// agent is on, or to the one it names when it is about no agent.
async fn route(
    mut state: Signal<AppState>,
    mut drafts: Signal<Drafts>,
    load: Signal<ShownLoad>,
    live: Live,
    machines: Vec<(MachineId, MachineConfig)>,
    mut requests: UnboundedReceiver<ToDaemon>,
    feed: FeedSender,
) {
    use futures_util::StreamExt;

    let notify = live.notify;
    let config_file = live.places.config_file.clone();
    let (router, mut rerouted) = mpsc::unbounded_channel();
    let (retries, mut retried) = mpsc::unbounded_channel();
    let (this, this_requests) = mpsc::unbounded_channel();
    let this_task = Task {
        machine: MachineId::THIS,
        notify,
        router: router.clone(),
        updates: retries.clone(),
        feed: feed.clone(),
    };
    spawn(run(state, drafts, load, live, this_requests, this_task));
    let mut servers = Servers {
        tasks: BTreeMap::from([(MachineId::THIS, this)]),
        configs: Vec::new(),
        next: 1,
        router,
        retries,
        feed,
    };
    for (machine, remote) in machines {
        servers.start(state, drafts, load, notify, machine, remote);
    }
    loop {
        let mut message = tokio::select! {
            message = requests.next() => match message {
                Some(message) => message,
                None => return,
            },
            Some(outgoing) = rerouted.recv() => ToDaemon::Send(outgoing),
            Some(retry) = retried.recv() => retry,
        };
        let machine = match &mut message {
            ToDaemon::Send(outgoing) => outgoing.machine(),
            ToDaemon::Choose(_) => MachineId::THIS,
            ToDaemon::Machines(MachineChange::Retry(machine)) => *machine,
            ToDaemon::Machines(MachineChange::Add(remote)) => {
                let Some(machine) = servers.next_machine() else {
                    state.write().ui.report(
                        "No more servers can be added until the app starts again.".to_owned(),
                    );
                    continue;
                };
                servers.start(state, drafts, load, notify, machine, remote.clone());
                servers.save(state, &config_file);
                continue;
            }
            ToDaemon::Machines(MachineChange::Edit(machine, remote)) => {
                servers.restart(state, drafts, load, notify, *machine, remote.clone());
                servers.save(state, &config_file);
                continue;
            }
            ToDaemon::Machines(MachineChange::Remove(machine)) => {
                let machine = *machine;
                // Its task ends with its requests.
                servers.tasks.remove(&machine);
                servers.configs.retain(|(id, _)| *id != machine);
                machines::forget(&mut state.write(), &mut drafts.write(), machine);
                servers.save(state, &config_file);
                continue;
            }
            ToDaemon::Machines(MachineChange::LocalDaemon(next)) => {
                settings::save_start(&mut state.write(), next.clone(), |next| {
                    save_daemon(&config_file, next).map_err(|error| state::error_chain(&error))
                });
                continue;
            }
            ToDaemon::Update(command) => {
                servers.start_update_command(state, command.clone());
                continue;
            }
            ToDaemon::Notifications => {
                for task in servers.tasks.values() {
                    // A task that ended has no notifier.
                    let _ = task.send(ToDaemon::Notifications);
                }
                continue;
            }
        };
        servers.forward(machine, message);
    }
}

/// The connection tasks of the machines, and the servers the config names.
struct Servers {
    tasks: BTreeMap<MachineId, mpsc::UnboundedSender<ToDaemon>>,
    configs: Vec<(MachineId, MachineConfig)>,
    /// The number the next server takes. A removed server's number is not taken again.
    next: u16,
    router: Router,
    /// Takes an update's retry to the task the machine has when the update ends, which an edit
    /// may have replaced, and an update a task starts once its agents are idle.
    retries: mpsc::UnboundedSender<ToDaemon>,
    feed: FeedSender,
}

impl Servers {
    /// The number the next server takes, while app ids have room for it.
    fn next_machine(&self) -> Option<MachineId> {
        Some(MachineId::new(self.next)).filter(|machine| *machine <= MachineId::LAST)
    }

    fn start(
        &mut self,
        mut state: Signal<AppState>,
        drafts: Signal<Drafts>,
        load: Signal<ShownLoad>,
        notify: bool,
        machine: MachineId,
        remote: MachineConfig,
    ) {
        let label = DaemonLabel::ssh(remote.name.clone());
        state
            .write()
            .machines
            .others
            .entry(machine)
            .or_insert_with(|| MachineState::new(label))
            .server = Some(remote.clone());
        self.spawn_task(state, drafts, load, notify, machine, remote.clone());
        self.configs.push((machine, remote));
        self.next = self.next.max(machine.get().saturating_add(1));
    }

    /// Reaches the server with its new config: its task ends with its requests, and a new one
    /// connects. A new name takes the preferences the old one had.
    fn restart(
        &mut self,
        mut state: Signal<AppState>,
        drafts: Signal<Drafts>,
        load: Signal<ShownLoad>,
        notify: bool,
        machine: MachineId,
        remote: MachineConfig,
    ) {
        let Some((_, config)) = self.configs.iter_mut().find(|(id, _)| *id == machine) else {
            return;
        };
        config.clone_from(&remote);
        self.tasks.remove(&machine);
        machines::apply_edit(&mut state.write(), machine, &remote);
        self.spawn_task(state, drafts, load, notify, machine, remote);
    }

    /// Starts the task that connects to the server and carries its requests.
    fn spawn_task(
        &mut self,
        state: Signal<AppState>,
        drafts: Signal<Drafts>,
        load: Signal<ShownLoad>,
        notify: bool,
        machine: MachineId,
        remote: MachineConfig,
    ) {
        let (sender, requests) = mpsc::unbounded_channel();
        spawn(run_remote(
            state,
            drafts,
            load,
            Task {
                machine,
                notify,
                router: self.router.clone(),
                updates: self.retries.clone(),
                feed: self.feed.clone(),
            },
            remote,
            requests,
        ));
        self.tasks.insert(machine, sender);
    }

    /// Hands `message` to the machine's task.
    fn forward(&self, machine: MachineId, message: ToDaemon) {
        if let Some(task) = self.tasks.get(&machine) {
            // A task that ended takes no more requests.
            let _ = task.send(message);
        }
    }

    /// Runs an update or a cancel next to the machine's connection task.
    fn start_update_command(&self, mut state: Signal<AppState>, command: UpdateCommand) {
        match command {
            UpdateCommand::CheckJobs(machine, check) => {
                if !self.tasks.contains_key(&machine) {
                    daemon_update::job_check_dropped(&mut state.write(), machine);
                    return;
                }
                spawn(check_jobs(state, machine, check, self.retries.clone()));
            }
            UpdateCommand::Start(machine, run) => {
                if !self.tasks.contains_key(&machine) {
                    return;
                }
                spawn(run_update(state, machine, run, self.retries.clone()));
            }
            UpdateCommand::Cancel(machine, plan) => {
                spawn(cancel_update(state, machine, plan));
            }
            UpdateCommand::Probe(machine, target) => {
                let mut state = state;
                let updates = self.retries.clone();
                spawn(async move {
                    // GitHub may now say whether the release's daemons can be downloaded. It
                    // answers beside the probe, so the wait is the longer of the two.
                    let (read, carried) = tokio::join!(
                        server_daemon::probe(&target.host, &target.binary),
                        server_daemon::carried_daemons(),
                    );
                    let mut state = state.write();
                    daemon_update::carry(&mut state, machine, carried);
                    daemon_update::probed(&mut state, machine, &target, read);
                    // A wait for idle agents may have waited for this probe.
                    if let Some(run) = daemon_update::start_when_idle(&mut state, machine) {
                        let _ = updates.send(ToDaemon::Update(UpdateCommand::Start(machine, run)));
                    }
                });
            }
            UpdateCommand::Setup(run) => {
                spawn(run_setup(state, run, self.retries.clone()));
            }
            UpdateCommand::StartDaemon(machine, run) => {
                spawn(start_daemon(state, machine, run, self.retries.clone()));
            }
            UpdateCommand::BootStart(machine, run) => {
                spawn(add_boot_start(state, machine, run));
            }
        }
    }

    /// Writes the servers to the config. A config the app cannot write keeps them for this run.
    fn save(&self, mut state: Signal<AppState>, file: &Path) {
        let configs: Vec<MachineConfig> = self
            .configs
            .iter()
            .map(|(_, config)| config.clone())
            .collect();
        if let Err(error) = save_machines(file, &configs) {
            state.write().ui.report(format!(
                "The servers hold for this run only: {}",
                state::error_chain(&error)
            ));
        }
    }
}

/// A machine's connection task: its machine, and where the requests for other machines go.
#[derive(Clone)]
struct Task {
    machine: MachineId,
    notify: bool,
    router: Router,
    /// Starts the machine's waiting update beside the task, once its agents are idle.
    updates: mpsc::UnboundedSender<ToDaemon>,
    /// Takes terminal output to the page.
    feed: FeedSender,
}

/// Runs the update and reports it to the machine's state, which outlives the connection's
/// attempts. Once it succeeds, the machine's connection task tries again at once. On a server,
/// the window's closing kills ssh, and the update goes on there.
async fn run_update(
    mut state: Signal<AppState>,
    machine: MachineId,
    run: UpdateRun,
    retries: mpsc::UnboundedSender<ToDaemon>,
) {
    let UpdateRun { plan, busy } = run;
    let retry_now = match plan {
        UpdatePlan::Server(plan) => match update_server(state, machine, &plan, busy).await {
            ServerUpdateEnd::ScriptEnded(code) => {
                daemon_update::finished(&mut state.write(), machine, code)
            }
            ServerUpdateEnd::NotSent => {
                daemon_update::send_failed(&mut state.write(), machine);
                false
            }
        },
        UpdatePlan::Service(plan) => {
            let check = match busy {
                BusyAgents::Refuse => Some(BusyCheck {
                    resting: update_offer::resting_agents(&state.peek(), machine),
                    runs_something: Box::new(move || {
                        daemon_update::runs_something_now(&state.peek(), machine)
                    }),
                }),
                BusyAgents::Interrupt => None,
            };
            let restarted = service_update::restart(&SystemService, &plan, check, |step| {
                daemon_update::step_started(&mut state.write(), machine, step);
            })
            .await;
            daemon_update::restart_ended(&mut state.write(), machine, restarted)
        }
    };
    if !retry_now {
        return;
    }
    let retry = retry_message(&mut state.write(), machine);
    if let Some(retry) = retry {
        // The window closed: nothing tries the machine again.
        let _ = retries.send(retry);
    }
}

/// Starts the daemon on a server where none runs, then connects again through `retries` once it
/// runs. A failure stays on the machine's line, with the script's last word.
async fn start_daemon(
    mut state: Signal<AppState>,
    machine: MachineId,
    run: StartRun,
    retries: mpsc::UnboundedSender<ToDaemon>,
) {
    let mut lines = Vec::new();
    let code =
        server_daemon::start_daemon(&run.host, &run.binary, |batch| lines.extend(batch)).await;
    let result = server_setup::script_result(code, lines).map(drop);
    let ran = result.is_ok();
    {
        let mut state = state.write();
        daemon_start::started(&mut state, machine, result);
        if ran && let Some(retry) = retry_message(&mut state, machine) {
            let _ = retries.send(retry);
        }
    }
    if !ran {
        return;
    }
    // A daemon started on its own may need a start at boot, which the dialog then offers.
    let mut lines = Vec::new();
    let code =
        server_daemon::check_needs(&run.host, Some(&run.binary), |batch| lines.extend(batch)).await;
    if let Ok(said) = server_setup::script_result(code, lines) {
        let report = InstallReport::read(said.iter().map(String::as_str));
        daemon_start::offer_boot_start(&mut state.write(), machine, &run, report);
    }
}

/// "Add it to my crontab" on a server's row, then the report again.
async fn add_boot_start(mut state: Signal<AppState>, machine: MachineId, run: StartRun) {
    let mut lines = Vec::new();
    let code =
        server_daemon::add_boot_start(&run.host, Some(&run.binary), |batch| lines.extend(batch))
            .await;
    let result = server_setup::script_result(code, lines);
    daemon_start::boot_start_added(&mut state.write(), machine, &run, result);
}

/// Runs a step of a server's setup. A server whose daemon runs joins the app through `added`.
async fn run_setup(
    mut state: Signal<AppState>,
    run: SetupRun,
    added: mpsc::UnboundedSender<ToDaemon>,
) {
    let change = match run {
        SetupRun::Look { host } => {
            let (found, carried) = tokio::join!(
                server_daemon::probe(&host, PROGRAM),
                server_daemon::carried_daemons(),
            );
            server_setup::looked(&mut state.write(), &host, found, &carried)
        }
        SetupRun::Install { host, cpu } => install_on(state, &host, cpu).await,
        SetupRun::Check { host } => {
            let mut lines = Vec::new();
            let code = server_daemon::check_needs(&host, None, |batch| lines.extend(batch)).await;
            let result = server_setup::script_result(code, lines);
            server_setup::checked(&mut state.write(), &host, result);
            None
        }
        SetupRun::AddBootStart { host } => {
            let mut lines = Vec::new();
            let code =
                server_daemon::add_boot_start(&host, None, |batch| lines.extend(batch)).await;
            let result = server_setup::script_result(code, lines);
            server_setup::checked(&mut state.write(), &host, result);
            None
        }
    };
    if let Some(change) = change {
        // The window closed: nothing adds the server.
        let _ = added.send(ToDaemon::Machines(change));
    }
}

/// The daemon file to send: a carried one, or the release's once it is downloaded.
async fn daemon_file(source: &DaemonSource) -> Result<PathBuf, String> {
    match source {
        DaemonSource::Carried(file) => Ok(file.clone()),
        DaemonSource::Release(cpu) => release_daemon::for_cpu(*cpu).await,
    }
}

/// Sends the daemon this app carries for `cpu` to `host` and installs it there. Returns the server
/// to add once its daemon runs.
async fn install_on(
    mut state: Signal<AppState>,
    host: &str,
    cpu: LinuxCpu,
) -> Option<MachineChange> {
    let Some(source) = server_daemon::carried_daemons().await.for_cpu(cpu) else {
        server_setup::send_failed(
            &mut state.write(),
            host,
            "This app carries no daemon to send.".to_owned(),
        );
        return None;
    };
    let file = match daemon_file(&source).await {
        Ok(file) => file,
        Err(error) => {
            server_setup::send_failed(&mut state.write(), host, error);
            return None;
        }
    };
    let sent = server_daemon::send(host, &file, |progress| {
        server_setup::sending(&mut state.write(), host, progress);
    })
    .await;
    let sent = match sent {
        Ok(sent) => sent,
        Err(error) => {
            server_setup::send_failed(&mut state.write(), host, error);
            return None;
        }
    };
    let mut said = Vec::new();
    let code = server_daemon::install(host, &sent, |lines| {
        let mut state = state.write();
        for line in lines {
            server_setup::output(&mut state, host, &line.text);
            said.push(line.text);
        }
    })
    .await;
    server_setup::installed(&mut state.write(), host, code, &said)
}

/// How a server's update ended.
enum ServerUpdateEnd {
    /// The script ran, and ended with this exit code: `None` when ssh lost it.
    ScriptEnded(Option<i32>),
    /// The send of the daemon failed, so the script did not run.
    NotSent,
}

/// Sends the daemon this app carries, if the plan has one, then runs the update script.
async fn update_server(
    mut state: Signal<AppState>,
    machine: MachineId,
    plan: &ServerPlan,
    busy: BusyAgents,
) -> ServerUpdateEnd {
    let file = match &plan.daemon {
        Some(source) => {
            if let DaemonSource::Release(cpu) = source {
                let line = OutputLine {
                    stream: OutputStream::Stdout,
                    text: format!(
                        "Downloading the {APP_VERSION} daemon for Linux on {} from the release",
                        cpu.word()
                    ),
                };
                daemon_update::output(&mut state.write(), machine, vec![line]);
            }
            match daemon_file(source).await {
                Ok(file) => Some(file),
                Err(error) => {
                    let line = OutputLine {
                        stream: OutputStream::Stderr,
                        text: format!(
                            "{error} The update builds the daemon on {} instead.",
                            plan.host
                        ),
                    };
                    let mut state = state.write();
                    daemon_update::output(&mut state, machine, vec![line]);
                    daemon_update::builds_instead(&mut state, machine);
                    None
                }
            }
        }
        None => None,
    };
    let sent = match &file {
        Some(file) => {
            let sent = server_daemon::send(&plan.host, file, |progress| {
                daemon_update::sending(&mut state.write(), machine, progress);
            })
            .await;
            match sent {
                Ok(sent) => Some(sent),
                Err(error) => {
                    let line = OutputLine {
                        stream: OutputStream::Stderr,
                        text: format!("Could not send the daemon: {error}"),
                    };
                    daemon_update::output(&mut state.write(), machine, vec![line]);
                    return ServerUpdateEnd::NotSent;
                }
            }
        }
        None => None,
    };
    let code = update_script::update(plan, busy, sent.as_ref(), |lines| {
        daemon_update::output(&mut state.write(), machine, lines);
    })
    .await;
    ServerUpdateEnd::ScriptEnded(code)
}

/// Reads which of this computer's resting agents may hold a scheduled job, then starts the update,
/// or opens the confirmation that names them.
async fn check_jobs(
    mut state: Signal<AppState>,
    machine: MachineId,
    check: JobCheck,
    retries: mpsc::UnboundedSender<ToDaemon>,
) {
    let JobCheck { plan, agents } = check;
    let holders = service_update::read_job_holders(&SystemService, &plan, agents).await;
    let run = daemon_update::jobs_checked(&mut state.write(), machine, holders);
    if let Some(run) = run {
        run_update(state, machine, run, retries).await;
    }
}

async fn cancel_update(mut state: Signal<AppState>, machine: MachineId, plan: ServerPlan) {
    if let Err(error) = update_script::cancel(&plan).await {
        let said = format!("Could not cancel the update: {error}");
        daemon_update::cancel_failed(&mut state.write(), machine, said);
    }
}

/// Reaches a machine over SSH, and tries again after each failure, as long as the window lives.
async fn run_remote(
    mut state: Signal<AppState>,
    drafts: Signal<Drafts>,
    load: Signal<ShownLoad>,
    task: Task,
    remote: MachineConfig,
    mut requests: Requests,
) {
    let machine = task.machine;
    let label = DaemonLabel::ssh(remote.name.clone());
    loop {
        if let Some(known) = state.write().machines.get_mut(machine) {
            known.connection = ConnectionState::Connecting { attempt: 1 };
            known.update_plan = plan_for_machine(&remote);
        }
        let reached = Reached {
            transport: Transport::Ssh {
                host: remote.host.clone(),
                remote_command: remote.remote_command.clone(),
            },
            label: label.clone(),
            note: None,
        };
        let served = serve(state, drafts, load, reached, task.clone(), &mut requests);
        let (error, gap) = match served.await {
            Served::Closed => return,
            Served::NoAnswer { unreached, gap, .. } => (unreached.error, gap),
        };
        let retry_at = Instant::now() + REMOTE_RETRY;
        {
            let mut state = state.write();
            if let Some(known) = state.machines.get_mut(machine) {
                known.connection = ConnectionState::Disconnected { error, retry_at };
            }
            daemon_update::attempt_failed(&mut state, machine, gap);
        }
        let waited = refuse_requests(
            state,
            drafts,
            machine,
            &mut requests,
            tokio::time::sleep(REMOTE_RETRY),
        );
        if !waited.await {
            return;
        }
    }
}

/// Answers the requests that come until `wait` ends with a failure, since no daemon answers them.
/// Returns false when the requests end, with the window.
async fn refuse_requests(
    mut state: Signal<AppState>,
    mut drafts: Signal<Drafts>,
    machine: MachineId,
    requests: &mut Requests,
    wait: impl Future<Output = ()>,
) -> bool {
    tokio::pin!(wait);
    loop {
        tokio::select! {
            () = &mut wait => return true,
            message = requests.recv() => match message {
                Some(ToDaemon::Machines(MachineChange::Retry(_))) => return true,
                Some(ToDaemon::Machines(_) | ToDaemon::Update(_) | ToDaemon::Notifications) => {}
                // A key needs no note while the machine is away: the app shows that.
                Some(ToDaemon::Send(Outgoing {
                    purpose: Purpose::TerminalKeys,
                    ..
                })) => {}
                Some(ToDaemon::Send(Outgoing { request, purpose, .. })) => {
                    let mut state = state.write();
                    state::apply_response(
                        &mut state,
                        &mut drafts.write(),
                        request.method(),
                        purpose,
                        Err(RequestError::NotConnected),
                        machine,
                    );
                }
                Some(ToDaemon::Choose(_)) => {}
                None => return false,
            },
        }
    }
}

/// Shows `machine`'s load, unless the chip already shows it: setting a signal renders its
/// readers again.
fn show_load(mut load: Signal<ShownLoad>, machine: MachineId, view: LoadView) {
    let shown = ShownLoad { machine, view };
    if *load.peek() != shown {
        load.set(shown);
    }
}

/// Answers every request from the sample state, in order, and makes the live agent's
/// conversation grow.
async fn run_demo(
    mut state: Signal<AppState>,
    mut drafts: Signal<Drafts>,
    load: Signal<ShownLoad>,
    mut requests: UnboundedReceiver<ToDaemon>,
    local: Option<LocalScene>,
) {
    use futures_util::StreamExt;

    let mut ticks = tokio::time::interval(demo::LIVE_TICK);
    let mut load_ticks = tokio::time::interval(LOAD_POLL);
    let mut tick: u32 = 0;
    // The agents closed, which Undo brings back.
    let mut closed: Vec<AgentEntry> = Vec::new();
    loop {
        tokio::select! {
            _ = load_ticks.tick() => {
                let machine = state.peek().selected_machine();
                let answer = demo::answer_from(&state.peek(), machine, &Request::MachineLoad);
                show_load(load, machine, LoadView::from_result(answer));
            }
            outgoing = requests.next() => {
                let outgoing = match outgoing {
                    Some(ToDaemon::Send(outgoing)) => outgoing,
                    Some(ToDaemon::Update(UpdateCommand::Start(machine, run))) => {
                        spawn(play_demo_update(state, machine, run.plan));
                        continue;
                    }
                    // The demo's agents have no raw logs: none holds a job.
                    Some(ToDaemon::Update(UpdateCommand::CheckJobs(machine, _))) => {
                        let run = daemon_update::jobs_checked(&mut state.write(), machine, Vec::new());
                        if let Some(run) = run {
                            spawn(play_demo_update(state, machine, run.plan));
                        }
                        continue;
                    }
                    Some(ToDaemon::Machines(MachineChange::Edit(machine, config))) => {
                        machines::apply_edit(&mut state.write(), machine, &config);
                        continue;
                    }
                    Some(ToDaemon::Update(UpdateCommand::Probe(machine, target))) => {
                        let read = Ok(demo::probe_answer());
                        daemon_update::probed(&mut state.write(), machine, &target, read);
                        continue;
                    }
                    Some(ToDaemon::Update(UpdateCommand::Setup(run))) => {
                        spawn(demo::play_setup(state, run));
                        continue;
                    }
                    // The demo's start works after a pause, and the server connects.
                    Some(ToDaemon::Update(UpdateCommand::StartDaemon(machine, run))) => {
                        spawn(demo::play_start(state, machine, run));
                        continue;
                    }
                    Some(ToDaemon::Update(UpdateCommand::BootStart(machine, run))) => {
                        spawn(demo::play_boot_start(state, machine, run));
                        continue;
                    }
                    // The demo writes no config.
                    Some(ToDaemon::Machines(MachineChange::LocalDaemon(next))) => {
                        settings::save_start(&mut state.write(), next, |_| demo::save_local(local));
                        continue;
                    }
                    Some(
                        ToDaemon::Choose(_)
                        | ToDaemon::Machines(_)
                        | ToDaemon::Update(UpdateCommand::Cancel(..))
                        | ToDaemon::Notifications,
                    ) => continue,
                    None => return,
                };
                let mut queue = VecDeque::from([outgoing]);
                while let Some(mut outgoing) = queue.pop_front() {
                    let machine = outgoing.machine();
                    let Outgoing { request, purpose, .. } = outgoing;
                    tokio::time::sleep(demo::latency(&request)).await;
                    if let Purpose::Close { entry } = &purpose {
                        closed.push(entry.as_ref().clone());
                    }
                    let reopened = demo::reopen(&mut state.write(), &mut closed, &request);
                    let result = reopened
                        .unwrap_or_else(|| demo::answer_from(&state.read(), machine, &request));
                    let mut state = state.write();
                    if result.is_ok() {
                        demo::apply(&mut state, &request);
                    }
                    let method = request.method();
                    state::apply_response(
                        &mut state,
                        &mut drafts.write(),
                        method,
                        purpose,
                        result,
                        machine,
                    );
                    queue.extend(state::needed_requests(&mut state));
                }
            }
            _ = ticks.tick() => {
                demo::live_tick(&mut state.write(), tick);
                tick = tick.wrapping_add(1);
            }
        }
    }
}

/// Plays a successful update on the demo's screens: the script's lines or the service's steps,
/// then the first connection.
async fn play_demo_update(mut state: Signal<AppState>, machine: MachineId, plan: UpdatePlan) {
    let reconnects = match plan {
        UpdatePlan::Server(plan) => {
            let lines = if plan.daemon.is_some() {
                for fifth in 1..=5 {
                    tokio::time::sleep(demo::UPDATE_LINE_PAUSE / 2).await;
                    let sent = SendProgress {
                        sent: demo::SENT_SIZE * fifth / 5,
                        total: demo::SENT_SIZE,
                    };
                    daemon_update::sending(&mut state.write(), machine, sent);
                }
                demo::SENT_OUTPUT
            } else {
                demo::UPDATE_OUTPUT
            };
            for line in lines {
                tokio::time::sleep(demo::UPDATE_LINE_PAUSE).await;
                let line = OutputLine {
                    stream: OutputStream::Stdout,
                    text: (*line).to_owned(),
                };
                daemon_update::output(&mut state.write(), machine, vec![line]);
            }
            daemon_update::finished(&mut state.write(), machine, Some(0))
        }
        UpdatePlan::Service(_) => {
            tokio::time::sleep(demo::UPDATE_LINE_PAUSE).await;
            daemon_update::step_started(&mut state.write(), machine, UpdateStep::Start);
            tokio::time::sleep(demo::UPDATE_LINE_PAUSE).await;
            daemon_update::restart_ended(&mut state.write(), machine, Ok(()))
        }
    };
    if reconnects {
        tokio::time::sleep(demo::UPDATE_LINE_PAUSE).await;
        demo::finish_update(&mut state.write(), machine);
    }
}

/// What the connection task needs to reach a daemon.
struct Live {
    places: Places,
    in_app: InAppHost,
    notify: bool,
}

/// How a session ended.
enum Served {
    /// The window is gone.
    Closed,
    /// The first connection failed: the error says why.
    NoAnswer {
        unreached: Unreached,
        absence: Absence,
        /// The daemon refused the app for its protocol.
        gap: Option<VersionGap>,
    },
}

/// Reaches the config's daemon. When that fails before a first connection, shows the "No daemon
/// answers" screen and reaches the daemon the user picks there.
async fn run(
    mut state: Signal<AppState>,
    drafts: Signal<Drafts>,
    load: Signal<ShownLoad>,
    live: Live,
    mut requests: Requests,
    task: Task,
) {
    let Live {
        places,
        in_app,
        notify: _,
    } = live;
    let mut daemon = state.peek().machines.configured.clone().unwrap_or_default();
    // The tick of the last choice on the screen.
    let mut every_time = None;
    loop {
        let (unreached, absence, gap) = match reach::reach(&daemon, &places, &in_app).await {
            Ok(reached) => {
                {
                    let mut state = state.write();
                    state.machines.this.daemon = reached.label.clone();
                    state.machines.this.update_plan =
                        plan_for_this(&daemon, reached.label.place, &places);
                    state.machines.this.connection = ConnectionState::Connecting { attempt: 1 };
                    // An update under way keeps its screen until the new daemon answers.
                    if state.machines.this.update.is_none() {
                        state
                            .ui
                            .dialog
                            .take_if(|dialog| matches!(dialog, Dialog::NoDaemon));
                    }
                    if let Some(note) = reached.note {
                        state.ui.inform(note.to_owned());
                    }
                }
                let served = serve(state, drafts, load, reached, task.clone(), &mut requests);
                match served.await {
                    Served::Closed => return,
                    Served::NoAnswer {
                        unreached,
                        absence,
                        gap,
                    } => (unreached, absence, gap),
                }
            }
            Err(unreached) => (unreached, Absence::Failed, None),
        };
        let plan = plan_for_this(&daemon, unreached.label.place, &places);
        show_no_daemon(state, unreached, absence, every_time);
        {
            let mut state = state.write();
            state.machines.this.update_plan = plan;
            daemon_update::attempt_failed(&mut state, MachineId::THIS, gap);
        }
        let Some(chosen) = next_choice(state, drafts, &mut requests).await else {
            return;
        };
        every_time = Some(chosen.every_time);
        // The Settings dialog may have changed it since the start.
        let (next, error) = reach::settle(
            chosen,
            &daemon,
            &mut state.write().machines.configured,
            &places.config_file,
        );
        if let Some(error) = error {
            state
                .write()
                .ui
                .report(format!("Cannot keep the choice for next time: {error}"));
        }
        daemon = next;
    }
}

/// How an update of this computer's daemon runs, which the app reached at `place` for `daemon`:
/// the script over SSH, when the app knows its commit, or a restart of the background service,
/// where the app runs it. That covers another daemon at the socket of a state folder, and the one
/// the daemon in the app gave way to. The daemon in the app needs none: it runs this app's code.
fn plan_for_this(daemon: &DaemonConfig, place: DaemonPlace, places: &Places) -> Option<UpdatePlan> {
    match place {
        DaemonPlace::Ssh => match daemon {
            DaemonConfig::Ssh {
                host,
                remote_command,
                source_dir,
            } => server_plan(host, remote_command, source_dir.clone()),
            DaemonConfig::Local { .. } | DaemonConfig::InApp => None,
        },
        DaemonPlace::Service | DaemonPlace::Socket if SERVICE_OFFERED => {
            let state_dir = match daemon {
                DaemonConfig::Local { socket, .. } => {
                    reach::service_state_dir(socket.as_deref(), places)?
                }
                DaemonConfig::InApp => places.daemon_state.clone(),
                DaemonConfig::Ssh { .. } => return None,
            };
            Some(UpdatePlan::Service(ServicePlan {
                state_dir,
                log_folder: places.app_state.clone(),
            }))
        }
        DaemonPlace::Service | DaemonPlace::Socket | DaemonPlace::InApp => None,
    }
}

fn plan_for_machine(remote: &MachineConfig) -> Option<UpdatePlan> {
    server_plan(
        &remote.host,
        &remote.remote_command,
        remote.source_dir.clone(),
    )
}

/// The script's plan on a server, when the app knows the commit it was built from.
fn server_plan(host: &str, remote_command: &str, source_dir: Option<String>) -> Option<UpdatePlan> {
    Some(UpdatePlan::Server(ServerPlan {
        host: host.to_owned(),
        remote_command: remote_command.to_owned(),
        source_dir,
        commit: update_script::app_commit()?,
        carried: server_daemon::carried_daemons_now(),
        daemon: None,
    }))
}

fn show_no_daemon(
    mut state: Signal<AppState>,
    unreached: Unreached,
    absence: Absence,
    every_time: Option<bool>,
) {
    let Unreached { label, error } = unreached;
    let mut state = state.write();
    no_daemon::show(&mut state, &label, error, absence, every_time);
    state.machines.this.daemon = label;
}

/// Waits for a choice on the screen. No daemon answers the requests that come meanwhile.
async fn next_choice(
    mut state: Signal<AppState>,
    mut drafts: Signal<Drafts>,
    requests: &mut Requests,
) -> Option<Chosen> {
    loop {
        match requests.recv().await? {
            ToDaemon::Choose(chosen) => return Some(chosen),
            ToDaemon::Machines(_) | ToDaemon::Update(_) | ToDaemon::Notifications => {}
            ToDaemon::Send(Outgoing {
                purpose: Purpose::TerminalKeys,
                ..
            }) => {}
            ToDaemon::Send(Outgoing {
                request, purpose, ..
            }) => {
                let refused = Err(RequestError::NotConnected);
                let mut state = state.write();
                state::apply_response(
                    &mut state,
                    &mut drafts.write(),
                    request.method(),
                    purpose,
                    refused,
                    MachineId::THIS,
                );
            }
        }
    }
}

/// Runs a session with the daemon reached, which reconnects after every drop once it connected.
async fn serve(
    mut state: Signal<AppState>,
    mut drafts: Signal<Drafts>,
    load: Signal<ShownLoad>,
    reached: Reached,
    task: Task,
    requests: &mut Requests,
) -> Served {
    let Reached {
        transport, label, ..
    } = reached;
    let machine = task.machine;
    let mut session = Session::start(transport.clone(), Backoff::default());
    let mut requester = session.requester().clone();
    let (opened_sender, mut opened) = mpsc::unbounded_channel::<AgentId>();
    let mut notifier = task.notify.then(|| Notifier::new(opened_sender));
    let mut load_ticks = tokio::time::interval(LOAD_POLL);
    let (load_sender, mut load_answers) = mpsc::unbounded_channel();
    // One question at a time: a slow daemon does not pile them up.
    let mut load_asked = false;
    let mut connected_once = false;
    loop {
        let finish_due = state::next_finish_due(&state.peek(), machine);
        tokio::select! {
            () = sleep_until(finish_due) => {
                let alerts = state::release_finishes(&mut state.write(), machine, Instant::now());
                if let Some(notifier) = &mut notifier {
                    notifier.update(alerts, &state.peek());
                }
            }
            // The load shows the selected agent's machine.
            _ = load_ticks.tick() => {
                let (shown, answers) = {
                    let state = state.peek();
                    let answers = state.machines.get(machine).is_some_and(MachineState::is_connected);
                    (state.selected_machine() == machine, answers)
                };
                if !shown {
                } else if !answers {
                    show_load(load, machine, LoadView::Waiting);
                } else if !load_asked {
                    load_asked = true;
                    let requester = requester.clone();
                    let answers = load_sender.clone();
                    spawn(async move {
                        let answer = requester.request(Request::MachineLoad).await;
                        // The connection task ended: nobody shows the load any more.
                        let _ = answers.send(answer);
                    });
                }
            }
            Some(answer) = load_answers.recv() => {
                load_asked = false;
                let answer = answer.map(|mut body| {
                    machine.answer_to_app(&mut body);
                    body
                });
                if state.peek().selected_machine() == machine {
                    show_load(load, machine, LoadView::from_result(answer));
                }
            }
            update = session.next_update() => {
                let Some(mut update) = update else {
                    return Served::Closed;
                };
                machine.update_to_app(&mut update);
                if let SessionUpdate::Terminal(TerminalUpdate {
                    agent,
                    terminal,
                    change: TerminalChange::Output { offset, data },
                }) = update
                {
                    task.feed.send(Output { agent, terminal, offset, data });
                    continue;
                }
                match &update {
                    SessionUpdate::Connected(welcome) => {
                        crate::perf::connection(match welcome.catchup {
                            Catchup::Snapshot(_) => "connected: snapshot, every transcript reloads",
                            Catchup::Replay => "connected: replay, transcripts kept",
                        });
                    }
                    SessionUpdate::Disconnected { error, .. } => {
                        crate::perf::connection(format_args!("disconnected: {error}"));
                    }
                    SessionUpdate::Connecting { .. }
                    | SessionUpdate::Event(_)
                    | SessionUpdate::Terminal(_) => {}
                }
                match &update {
                    SessionUpdate::Connected(_) => connected_once = true,
                    SessionUpdate::Disconnected { error: error @ SessionError::Connect(_), .. }
                        if !connected_once =>
                    {
                        return Served::NoAnswer {
                            absence: no_daemon::absence(label.place, error),
                            gap: update_offer::protocol_gap(label.place, error),
                            unreached: Unreached {
                                label,
                                error: state::error_chain(error),
                            },
                        };
                    }
                    SessionUpdate::Connecting { .. }
                    | SessionUpdate::Disconnected { .. }
                    | SessionUpdate::Event(_)
                    | SessionUpdate::Terminal(_) => {}
                }
                let applying = Instant::now();
                let (alerts, follow_up, idle_update) = {
                    let mut state = state.write();
                    let mut drafts = drafts.write();
                    let alerts =
                        state::apply_update(&mut state, &mut drafts, update, Instant::now(), machine);
                    let idle_update = daemon_update::start_when_idle(&mut state, machine);
                    let wait_probe = daemon_update::probe_for_wait(&mut state, machine);
                    (alerts, state::needed_requests(&mut state), (idle_update, wait_probe))
                };
                crate::perf::event(applying.elapsed());
                let (idle_update, wait_probe) = idle_update;
                // The window closed: nothing runs the update or the probe.
                if let Some(run) = idle_update {
                    let _ = task.updates.send(ToDaemon::Update(UpdateCommand::Start(machine, run)));
                }
                if let Some(target) = wait_probe {
                    let _ = task.updates.send(ToDaemon::Update(UpdateCommand::Probe(machine, target)));
                }
                send_all(state, drafts, &requester, &task, follow_up);
                if let Some(notifier) = &mut notifier {
                    notifier.update(alerts, &state.peek());
                }
            }
            outgoing = requests.recv() => match outgoing {
                Some(ToDaemon::Send(outgoing)) => {
                    send(state, drafts, requester.clone(), task.clone(), outgoing);
                }
                // The session waits between its own attempts: "Retry now" starts a new one.
                Some(ToDaemon::Machines(MachineChange::Retry(_))) => {
                    let answers = state
                        .peek()
                        .machines
                        .get(machine)
                        .is_some_and(MachineState::is_connected);
                    if !answers {
                        session = Session::start(transport.clone(), Backoff::default());
                        requester = session.requester().clone();
                    }
                }
                Some(ToDaemon::Notifications) => {
                    if let Some(notifier) = &mut notifier {
                        notifier.update(Vec::new(), &state.peek());
                    }
                }
                // The screen shows only before a first connection.
                Some(ToDaemon::Choose(_) | ToDaemon::Machines(_) | ToDaemon::Update(_)) => {}
                None => return Served::Closed,
            },
            Some(agent) = opened.recv() => {
                focus_window();
                let follow_up = {
                    let mut state = state.write();
                    actions::select_agent(&mut state, agent);
                    state::needed_requests(&mut state)
                };
                send_all(state, drafts, &requester, &task, follow_up);
            }
        }
    }
}

/// Waits until `due`, or forever without one.
async fn sleep_until(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due.into()).await,
        None => std::future::pending().await,
    }
}

/// Sends each request to its machine: the task's own at once, another's through the router.
fn send_all(
    state: Signal<AppState>,
    drafts: Signal<Drafts>,
    requester: &Requester,
    task: &Task,
    requests: Vec<Outgoing>,
) {
    let (own, others) = split_by_machine(task.machine, requests);
    for outgoing in own {
        send(state, drafts, requester.clone(), task.clone(), outgoing);
    }
    for outgoing in others {
        // The router ends with the window.
        let _ = task.router.send(outgoing);
    }
}

/// The requests for `machine`, then those for the other machines.
fn split_by_machine(machine: MachineId, requests: Vec<Outgoing>) -> (Vec<Outgoing>, Vec<Outgoing>) {
    let mut own = Vec::new();
    let mut others = Vec::new();
    for mut outgoing in requests {
        if outgoing.machine() == machine {
            own.push(outgoing);
        } else {
            others.push(outgoing);
        }
    }
    (own, others)
}

/// Sends a request to the task's machine and applies its answer, without holding up the other
/// requests.
fn send(
    mut state: Signal<AppState>,
    mut drafts: Signal<Drafts>,
    requester: Requester,
    task: Task,
    outgoing: Outgoing,
) {
    let machine = task.machine;
    spawn(async move {
        let Outgoing {
            mut request,
            purpose,
            ..
        } = outgoing;
        let method = request.method();
        MachineId::request_to_daemon(&mut request);
        let sent = Instant::now();
        let result = requester.request(request).await.map(|mut body| {
            machine.answer_to_app(&mut body);
            body
        });
        if purpose == Purpose::TerminalKeys {
            if let Err(error) = result {
                tracing::debug!(%error, "a terminal did not take a key");
            }
            return;
        }
        crate::perf::answered(method, sent.elapsed(), &result);
        let applying = Instant::now();
        let follow_up = {
            let mut state = state.write();
            state::apply_response(
                &mut state,
                &mut drafts.write(),
                method,
                purpose,
                result,
                machine,
            );
            state::needed_requests(&mut state)
        };
        crate::perf::applied(method, applying.elapsed());
        send_all(state, drafts, &requester, &task, follow_up);
    });
}

fn focus_window() {
    if let Some(desktop) = try_consume_context::<DesktopContext>() {
        desktop.window.set_minimized(false);
        desktop.window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::time::UNIX_EPOCH;

    use cockpit_protocol::{AgentStatus, Method};
    use dioxus::dioxus_core::NoOpMutations;

    use super::*;
    use crate::state::actions::Direction;
    use crate::state::{state_with, summary};

    thread_local! {
        static COCKPIT: RefCell<Option<Cockpit>> = const { RefCell::new(None) };
        static TRANSCRIPT_RENDERS: Cell<u32> = const { Cell::new(0) };
    }

    /// The window's signals around a transcript, with a daemon that answers nothing.
    #[component]
    fn Window() -> Element {
        let state = use_signal(|| {
            let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
            state.ui.selected = Some(AgentId::new(1));
            state
        });
        let drafts = use_signal(Drafts::default);
        let load = use_signal(ShownLoad::default);
        let daemon = use_coroutine(|_: UnboundedReceiver<ToDaemon>| async {});
        let cockpit = use_context_provider(|| Cockpit {
            state,
            drafts,
            load,
            daemon,
        });
        COCKPIT.with(|slot| slot.replace(Some(cockpit)));
        rsx! {
            Transcript {}
        }
    }

    /// Reads the state as the transcript view does, and counts its renders.
    #[component]
    fn Transcript() -> Element {
        let cockpit: Cockpit = use_context();
        let state = cockpit.state.read();
        let _items = state
            .selected()
            .map(|entry| entry.transcript.items().count());
        TRANSCRIPT_RENDERS.with(|renders| renders.set(renders.get() + 1));
        rsx! {}
    }

    #[test]
    fn retry_now_restarts_this_computers_connection_after_a_drop() {
        let mut state = state_with(Vec::new());
        state.machines.this.connection = ConnectionState::Disconnected {
            error: "connection reset".to_owned(),
            retry_at: Instant::now(),
        };

        let retry = retry_message(&mut state, MachineId::THIS);

        assert!(matches!(
            retry,
            Some(ToDaemon::Machines(MachineChange::Retry(machine))) if machine == MachineId::THIS
        ));
    }

    #[test]
    fn retry_on_this_computers_screen_goes_through_its_choice_once() {
        let mut state = state_with(Vec::new());
        no_daemon::show(
            &mut state,
            &DaemonLabel::ssh("build-box".to_owned()),
            "refused".to_owned(),
            Absence::Failed,
            None,
        );

        let first = retry_message(&mut state, MachineId::THIS);
        let second = retry_message(&mut state, MachineId::THIS);

        assert!(matches!(
            first,
            Some(ToDaemon::Choose(Chosen {
                choice: DaemonChoice::Retry,
                ..
            }))
        ));
        assert!(second.is_none(), "the first retry is under way");
    }

    #[test]
    fn a_server_is_tried_again_by_its_connection_task() {
        let devbox = MachineId::new(1);
        let mut state = state_with(Vec::new());

        let retry = retry_message(&mut state, devbox);

        assert!(matches!(
            retry,
            Some(ToDaemon::Machines(MachineChange::Retry(machine))) if machine == devbox
        ));
    }

    /// Absolute on every platform.
    fn places() -> Places {
        let home = std::env::temp_dir().join("made-up-home");
        Places {
            config_file: home.join("config.toml"),
            daemon_state: home.join("state").join("yetack"),
            app_state: home.join("state").join("yetack-desktop"),
        }
    }

    #[test]
    fn this_computer_updates_over_ssh_or_by_restarting_its_service() {
        let places = places();
        let ssh = DaemonConfig::Ssh {
            host: "build-box".to_owned(),
            remote_command: "/opt/yetack".to_owned(),
            source_dir: None,
        };
        let service = DaemonConfig::Local {
            socket: None,
            start_service: true,
        };
        let socket = DaemonConfig::Local {
            socket: None,
            start_service: false,
        };
        let restart = SERVICE_OFFERED.then(|| {
            UpdatePlan::Service(ServicePlan {
                state_dir: places.daemon_state.clone(),
                log_folder: places.app_state.clone(),
            })
        });

        assert_eq!(
            plan_for_this(&ssh, DaemonPlace::Ssh, &places).is_some(),
            update_script::app_commit().is_some(),
            "the script needs the app's commit"
        );
        assert_eq!(
            plan_for_this(&service, DaemonPlace::Service, &places),
            restart
        );
        assert_eq!(
            plan_for_this(&socket, DaemonPlace::Socket, &places),
            restart
        );
        assert_eq!(
            plan_for_this(&DaemonConfig::InApp, DaemonPlace::Socket, &places),
            restart,
            "the daemon the app gave way to"
        );
        assert_eq!(
            plan_for_this(&DaemonConfig::InApp, DaemonPlace::InApp, &places),
            None,
            "the app's own code"
        );
    }

    #[test]
    fn a_socket_outside_a_state_folder_has_no_service_to_restart() {
        let places = places();
        let elsewhere = DaemonConfig::Local {
            socket: Some(places.daemon_state.join("cockpit.sock")),
            start_service: false,
        };

        assert_eq!(
            plan_for_this(&elsewhere, DaemonPlace::Socket, &places),
            None
        );
    }

    /// Servers with a task for `devbox`, and the receiver of the updates' retries.
    fn servers_with(
        devbox: MachineId,
        task: mpsc::UnboundedSender<ToDaemon>,
    ) -> (Servers, mpsc::UnboundedReceiver<ToDaemon>) {
        let (router, _rerouted) = mpsc::unbounded_channel();
        let (retries, retried) = mpsc::unbounded_channel();
        let servers = Servers {
            tasks: BTreeMap::from([(devbox, task)]),
            configs: Vec::new(),
            next: 2,
            router,
            retries,
            feed: terminal_feed::channel().0,
        };
        (servers, retried)
    }

    #[test]
    fn an_update_under_way_lets_a_removed_servers_connection_task_end() {
        let devbox = MachineId::new(1);
        let (task, mut requests) = mpsc::unbounded_channel();
        let (mut servers, _retried) = servers_with(devbox, task);

        let _held_by_the_update = servers.retries.clone();
        servers.tasks.remove(&devbox);

        assert_eq!(
            requests.try_recv().err(),
            Some(mpsc::error::TryRecvError::Disconnected)
        );
    }

    #[test]
    fn an_updates_retry_reaches_the_task_an_edit_started() {
        let devbox = MachineId::new(1);
        let (old_task, mut old_requests) = mpsc::unbounded_channel();
        let (mut servers, mut retried) = servers_with(devbox, old_task);
        let held_by_the_update = servers.retries.clone();
        let (new_task, mut new_requests) = mpsc::unbounded_channel();

        // What an edit does to the tasks.
        servers.tasks.insert(devbox, new_task);
        let retry = ToDaemon::Machines(MachineChange::Retry(devbox));
        held_by_the_update.send(retry).unwrap();
        servers.forward(devbox, retried.try_recv().unwrap());

        assert!(matches!(
            new_requests.try_recv(),
            Ok(ToDaemon::Machines(MachineChange::Retry(machine))) if machine == devbox
        ));
        assert_eq!(
            old_requests.try_recv().err(),
            Some(mpsc::error::TryRecvError::Disconnected)
        );
    }

    #[test]
    fn a_server_past_the_last_number_an_app_id_holds_is_refused() {
        let (router, _rerouted) = mpsc::unbounded_channel();
        let (retries, _retried) = mpsc::unbounded_channel();
        let mut servers = Servers {
            tasks: BTreeMap::new(),
            configs: Vec::new(),
            next: MachineId::LAST.get(),
            router,
            retries,
            feed: terminal_feed::channel().0,
        };

        let last = servers.next_machine();
        servers.next = servers.next.saturating_add(1);

        assert_eq!(last, Some(MachineId::LAST));
        assert_eq!(servers.next_machine(), None);
    }

    #[test]
    fn a_task_hands_the_requests_for_another_machine_to_the_router() {
        let devbox = MachineId::new(1);
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        state.machines.this.connection = ConnectionState::Connected;
        let mut known = MachineState::new(DaemonLabel::ssh("devbox".to_owned()));
        known.connection = ConnectionState::Connected;
        state.machines.others.insert(devbox, known);
        let mut remote = summary(1, AgentStatus::Idle);
        remote.id = devbox.app_agent(AgentId::new(1));
        state
            .agents
            .insert(remote.id, AgentEntry::new(remote.clone()));
        state.ui.selected = Some(remote.id);

        // This computer's task finds them after one of its own events.
        let found = state::needed_requests(&mut state);
        let (own, others) = split_by_machine(MachineId::THIS, found);

        let about_devbox = |outgoing: &Outgoing| outgoing.request.method() == Method::AgentDetails;
        assert!(!own.iter().any(about_devbox), "{own:?}");
        assert!(others.iter().any(about_devbox), "{others:?}");
        assert!(
            others
                .into_iter()
                .all(|mut outgoing| outgoing.machine() == devbox),
            "only devbox's requests leave this computer's task"
        );
    }

    thread_local! {
        static CLOCK: RefCell<Option<Signal<SystemTime>>> = const { RefCell::new(None) };
        static DAY_RENDERS: Cell<u32> = const { Cell::new(0) };
    }

    /// A clock the test sets, and the day it gives.
    #[component]
    fn Clocked() -> Element {
        let now = use_signal(|| UNIX_EPOCH + Duration::from_secs(1_794_480_000));
        use_today(now);
        CLOCK.with(|slot| slot.replace(Some(now)));
        rsx! {
            Day {}
        }
    }

    /// Reads the day, and counts its renders.
    #[component]
    fn Day() -> Element {
        let Today(today) = use_context::<Today>();
        let _today = today();
        DAY_RENDERS.with(|renders| renders.set(renders.get() + 1));
        rsx! {}
    }

    #[test]
    fn a_tick_of_the_clock_renders_what_shows_the_day_only_when_the_day_changes() {
        let mut dom = VirtualDom::new(Clocked);
        dom.rebuild_in_place();
        let mut now = CLOCK
            .with(|slot| *slot.borrow())
            .expect("the component provides the clock");
        let renders = || DAY_RENDERS.with(Cell::get);
        let opened = renders();

        dom.in_runtime(|| now.with_mut(|now| *now += Duration::from_secs(1)));
        dom.render_immediate(&mut NoOpMutations);
        let after_a_tick = renders();
        dom.in_runtime(|| now.with_mut(|now| *now += Duration::from_secs(24 * 3600)));
        dom.render_immediate(&mut NoOpMutations);

        assert_eq!(after_a_tick, opened);
        assert_eq!(renders(), opened + 1);
    }

    #[test]
    fn typing_does_not_render_the_transcript_again() {
        let mut dom = VirtualDom::new(Window);
        dom.rebuild_in_place();
        let cockpit = COCKPIT
            .with(|slot| *slot.borrow())
            .expect("the window provides the cockpit");
        let renders = || TRANSCRIPT_RENDERS.with(Cell::get);
        let agent = AgentId::new(1);

        for text in ["R", "Ru", "Run the tests"] {
            dom.in_runtime(|| {
                cockpit.type_in_composer(agent, text.to_owned(), Utf16Offset::end_of(text))
            });
            dom.render_immediate(&mut NoOpMutations);
        }
        dom.in_runtime(|| {
            cockpit.change_with_drafts(|state, drafts| {
                actions::start_rename(state, drafts, agent);
                Vec::new()
            });
        });
        dom.render_immediate(&mut NoOpMutations);
        let after_opening_the_title = renders();
        for text in ["C", "Ca", "Cart total"] {
            dom.in_runtime(|| cockpit.type_title(text.to_owned()));
            dom.render_immediate(&mut NoOpMutations);
        }
        let after_typing = renders();
        // F2 moves the field to the agent's sidebar row, where typing changes the drafts alone too.
        dom.in_runtime(|| {
            cockpit.change_with_drafts(|state, drafts| {
                actions::rename_key(state, drafts);
                Vec::new()
            });
        });
        dom.render_immediate(&mut NoOpMutations);
        let after_moving_the_title = renders();
        for text in ["Cart total!", "Cart totals"] {
            dom.in_runtime(|| cockpit.type_title(text.to_owned()));
            dom.render_immediate(&mut NoOpMutations);
        }
        let after_typing_in_the_row = renders();
        // The dialogs' fields, through the calls their components make.
        dom.in_runtime(|| cockpit.update_with_drafts(actions::open_new_agent));
        dom.render_immediate(&mut NoOpMutations);
        let after_opening_the_dialog = renders();
        for text in ["/work/c", "/work/calc"] {
            dom.in_runtime(|| {
                cockpit.with_drafts(|_, drafts| drafts.new_agent.folder = text.to_owned());
                cockpit.with_drafts(|_, drafts| drafts.new_agent.prompt = text.to_owned());
                cockpit.with_drafts(|_, drafts| actions::session_query(drafts, text.to_owned()));
                cockpit.with_drafts(|state, drafts| {
                    actions::session_move(state, drafts, Direction::Down);
                });
            });
            dom.render_immediate(&mut NoOpMutations);
        }
        dom.in_runtime(|| cockpit.update_with_drafts(actions::toggle_switcher));
        dom.render_immediate(&mut NoOpMutations);
        let after_opening_the_switcher = renders();
        for text in ["a", "ag"] {
            dom.in_runtime(|| {
                cockpit.with_drafts(|_, drafts| actions::switcher_query(drafts, text.to_owned()));
                cockpit.with_drafts(|state, drafts| {
                    actions::switcher_move(state, drafts, Direction::Down);
                });
            });
            dom.render_immediate(&mut NoOpMutations);
        }
        let after_typing_in_dialogs = renders();
        dom.in_runtime(|| cockpit.update(|state| state.ui.window_focused = false));
        dom.render_immediate(&mut NoOpMutations);

        // The first render, then only the ones the changes of the state call for: opening the
        // title field, moving it to the row, the new agent dialog and the switcher, then the
        // focus.
        assert_eq!(
            (
                after_opening_the_title,
                after_typing,
                after_moving_the_title,
                after_typing_in_the_row
            ),
            (2, 2, 3, 3)
        );
        assert_eq!(
            (
                after_opening_the_dialog,
                after_opening_the_switcher,
                after_typing_in_dialogs,
                renders()
            ),
            (4, 5, 5, 6)
        );
        let typed = dom.in_runtime(|| {
            let drafts = cockpit.drafts.peek();
            let draft = drafts.draft(agent).map(|draft| draft.text.clone());
            (draft, drafts.title.clone())
        });
        assert_eq!(
            typed,
            (Some("Run the tests".to_owned()), "Cart totals".to_owned())
        );
        let dialogs = dom.in_runtime(|| {
            let drafts = cockpit.drafts.peek();
            (
                drafts.new_agent.folder.clone(),
                drafts.new_agent.session_query.clone(),
                drafts.switcher.query.clone(),
            )
        });
        assert_eq!(
            dialogs,
            (
                "/work/calc".to_owned(),
                "/work/calc".to_owned(),
                "ag".to_owned()
            )
        );
    }
}
