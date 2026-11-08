//! The connection task: a coroutine that owns the daemon session, applies its updates to the
//! state, and runs the requests the components ask for.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use cockpit_client::{
    Backoff, RequestError, Requester, Session, SessionError, SessionUpdate, Transport,
};
use cockpit_protocol::{AgentId, Request};
use dioxus::desktop::DesktopContext;
use dioxus::prelude::*;
use tokio::sync::mpsc;

use crate::config::{DaemonConfig, MachineConfig, save_machines};
use crate::daemon_label::DaemonLabel;
use crate::in_app_host::InAppHost;
use crate::machine::MachineId;
use crate::notifications::Notifier;
use crate::reach::{self, Places, Reached, Unreached};
use crate::state::machine_load::{LOAD_POLL, LoadView, ShownLoad};
use crate::state::machines::{self, MachineChange};
use crate::state::no_daemon::{self, Absence, Chosen, DaemonChoice};
use crate::state::{
    self, AgentEntry, AppState, ConnectionState, Dialog, Drafts, MachineState, Outgoing, Purpose,
    TitlePlace, Utf16Offset, actions,
};
use crate::{bridge, demo};

/// What the window was started with.
#[derive(Debug, Clone, PartialEq)]
pub struct Startup {
    pub state: AppState,
    /// This computer's daemon, as the config names it.
    pub daemon: DaemonConfig,
    /// The other machines the config names, reached over SSH.
    pub machines: Vec<(MachineId, MachineConfig)>,
    pub places: Places,
    /// The daemon the app runs itself, which the window's close stops.
    pub in_app: InAppHost,
    pub notifications: bool,
    /// Answers requests from the sample state instead of a daemon.
    pub demo: bool,
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
            requests
        };
        for request in requests {
            self.daemon.send(ToDaemon::Send(request));
        }
    }

    /// Adds or removes a server, or tries one again.
    pub fn change_machines(&self, change: MachineChange) {
        self.daemon.send(ToDaemon::Machines(change));
    }

    /// A click on one of the "No daemon answers" screen's buttons.
    pub fn choose_daemon(&self, choice: DaemonChoice) {
        let mut signal = self.state;
        let chosen = no_daemon::choose(&mut signal.write(), choice);
        if let Some(chosen) = chosen {
            self.daemon.send(ToDaemon::Choose(chosen));
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
    use_context_provider(|| Clock(now))
}

/// Starts the connection, and provides the `Cockpit` to every component.
pub fn use_cockpit(startup: Startup) -> Cockpit {
    let Startup {
        state: initial,
        daemon: configured,
        machines,
        places,
        in_app,
        notifications,
        demo,
        prefs_file: _,
    } = startup;
    let state = use_signal(move || initial);
    let drafts = use_signal(Drafts::default);
    let load = use_signal(ShownLoad::default);
    let daemon = use_coroutine(move |requests: UnboundedReceiver<ToDaemon>| {
        let live = Live {
            configured: configured.clone(),
            places: places.clone(),
            in_app: in_app.clone(),
            notify: notifications,
        };
        let machines = machines.clone();
        async move {
            if demo {
                run_demo(state, drafts, load, requests).await;
            } else {
                route(state, drafts, load, live, machines, requests).await;
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
) {
    use futures_util::StreamExt;

    let notify = live.notify;
    let config_file = live.places.config_file.clone();
    let (router, mut rerouted) = mpsc::unbounded_channel();
    let (this, this_requests) = mpsc::unbounded_channel();
    spawn(run(
        state,
        drafts,
        load,
        live,
        this_requests,
        router.clone(),
    ));
    let mut servers = Servers {
        tasks: BTreeMap::from([(MachineId::THIS, this)]),
        configs: Vec::new(),
        next: 1,
        router,
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
            ToDaemon::Machines(MachineChange::Remove(machine)) => {
                let machine = *machine;
                // Its task ends with its requests.
                servers.tasks.remove(&machine);
                servers.configs.retain(|(id, _)| *id != machine);
                machines::forget(&mut state.write(), &mut drafts.write(), machine);
                servers.save(state, &config_file);
                continue;
            }
        };
        if let Some(task) = servers.tasks.get(&machine) {
            // A task that ended takes no more requests.
            let _ = task.send(message);
        }
    }
}

/// The connection tasks of the machines, and the servers the config names.
struct Servers {
    tasks: BTreeMap<MachineId, mpsc::UnboundedSender<ToDaemon>>,
    configs: Vec<(MachineId, MachineConfig)>,
    /// The number the next server takes. A removed server's number is not taken again.
    next: u16,
    router: Router,
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
            .or_insert_with(|| MachineState::new(label));
        let (sender, requests) = mpsc::unbounded_channel();
        spawn(run_remote(
            state,
            drafts,
            load,
            Task {
                machine,
                notify,
                router: self.router.clone(),
            },
            remote.clone(),
            requests,
        ));
        self.tasks.insert(machine, sender);
        self.configs.push((machine, remote));
        self.next = self.next.max(machine.get().saturating_add(1));
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
        let error = match served.await {
            Served::Closed => return,
            Served::NoAnswer { unreached, .. } => unreached.error,
        };
        let retry_at = Instant::now() + REMOTE_RETRY;
        if let Some(known) = state.write().machines.get_mut(machine) {
            known.connection = ConnectionState::Disconnected { error, retry_at };
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
                Some(ToDaemon::Machines(_)) => {}
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
                let answer = demo::answer(&state.peek(), &Request::MachineLoad);
                show_load(load, MachineId::THIS, LoadView::from_result(answer));
            }
            outgoing = requests.next() => {
                let outgoing = match outgoing {
                    Some(ToDaemon::Send(outgoing)) => outgoing,
                    Some(ToDaemon::Choose(_) | ToDaemon::Machines(_)) => continue,
                    None => return,
                };
                let mut queue = VecDeque::from([outgoing]);
                while let Some(Outgoing { request, purpose, .. }) = queue.pop_front() {
                    tokio::time::sleep(demo::latency(&request)).await;
                    if let Purpose::Close { entry } = &purpose {
                        closed.push(entry.as_ref().clone());
                    }
                    let reopened = demo::reopen(&mut state.write(), &mut closed, &request);
                    let result = reopened.unwrap_or_else(|| demo::answer(&state.read(), &request));
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
                        MachineId::THIS,
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

/// What the connection task needs to reach a daemon.
struct Live {
    configured: DaemonConfig,
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
    router: Router,
) {
    let Live {
        mut configured,
        places,
        in_app,
        notify,
    } = live;
    let mut daemon = configured.clone();
    // The tick of the last choice on the screen.
    let mut every_time = None;
    loop {
        let (unreached, absence) = match reach::reach(&daemon, &places, &in_app).await {
            Ok(reached) => {
                {
                    let mut state = state.write();
                    state.machines.this.daemon = reached.label.clone();
                    state.machines.this.connection = ConnectionState::Connecting { attempt: 1 };
                    state
                        .ui
                        .dialog
                        .take_if(|dialog| matches!(dialog, Dialog::NoDaemon));
                    if let Some(note) = reached.note {
                        state.ui.inform(note.to_owned());
                    }
                }
                let task = Task {
                    machine: MachineId::THIS,
                    notify,
                    router: router.clone(),
                };
                let served = serve(state, drafts, load, reached, task, &mut requests);
                match served.await {
                    Served::Closed => return,
                    Served::NoAnswer { unreached, absence } => (unreached, absence),
                }
            }
            Err(unreached) => (unreached, Absence::Failed),
        };
        show_no_daemon(state, unreached, absence, every_time);
        let Some(chosen) = next_choice(state, drafts, &mut requests).await else {
            return;
        };
        every_time = Some(chosen.every_time);
        let (next, error) = reach::settle(chosen, &daemon, &mut configured, &places.config_file);
        if let Some(error) = error {
            state
                .write()
                .ui
                .report(format!("Cannot keep the choice for next time: {error}"));
        }
        daemon = next;
    }
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
            ToDaemon::Machines(_) => {}
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
        tokio::select! {
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
                match &update {
                    SessionUpdate::Connected(_) => connected_once = true,
                    SessionUpdate::Disconnected { error: error @ SessionError::Connect(_), .. }
                        if !connected_once =>
                    {
                        return Served::NoAnswer {
                            absence: no_daemon::absence(label.place, error),
                            unreached: Unreached {
                                label,
                                error: state::error_chain(error),
                            },
                        };
                    }
                    SessionUpdate::Connecting { .. }
                    | SessionUpdate::Disconnected { .. }
                    | SessionUpdate::Event(_) => {}
                }
                let (alerts, follow_up) = {
                    let mut state = state.write();
                    let mut drafts = drafts.write();
                    let alerts =
                        state::apply_update(&mut state, &mut drafts, update, Instant::now(), machine);
                    (alerts, state::needed_requests(&mut state))
                };
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
                // The screen shows only before a first connection.
                Some(ToDaemon::Choose(_) | ToDaemon::Machines(_)) => {}
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
        let result = requester.request(request).await.map(|mut body| {
            machine.answer_to_app(&mut body);
            body
        });
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
    fn a_server_past_the_last_number_an_app_id_holds_is_refused() {
        let (router, _rerouted) = mpsc::unbounded_channel();
        let mut servers = Servers {
            tasks: BTreeMap::new(),
            configs: Vec::new(),
            next: MachineId::LAST.get(),
            router,
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
