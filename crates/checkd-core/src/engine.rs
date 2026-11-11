//! The engine: what is checked when, and what a result does.
//!
//! The engine is driven from outside. A driver tells it the time
//! ([`Engine::advance`]) and the outcome of the commands it asked for
//! ([`Engine::job_done`]); the engine answers with [`Effect`]s: lines for
//! the log and commands to run. Times are milliseconds since the epoch.
//!
//! # Scheduling
//!
//! Every host and service that is checked has one check waiting. When its
//! time comes the check runs unless
//!
//! - the time is outside the object's check period: the check waits for
//!   the next valid time;
//! - the last check is younger than the cache horizon: the check is put
//!   off by the retry interval (host) or the check interval (service);
//! - an execution dependency holds it back: it is put off by the check
//!   interval.
//!
//! After a result the next check is one interval after the last one was
//! due, and the time it took to start, at most a millisecond: the retry
//! interval while the object is in a soft problem state, the check
//! interval otherwise. So checks keep their place and drift as the
//! reference's do. A check that started more than a second late counts
//! from its start. An object is never checked twice at once (see the
//! README of `runs/` for where the reference differs). A passive result
//! leaves the planned check where it is, unless it leaves a soft problem.
//! Checks a command forces run whatever the check period, the cache and
//! the dependencies say, in the order they were asked for.
//!
//! A host is also checked on demand. That moves its waiting check to
//! now; it does nothing while a check of the host is running. It happens
//!
//! - when a service result is a problem that is new, has changed, or
//!   comes while the service is in a soft state;
//! - at every OK result of a service whose host is not up;
//! - for the parents and children that are up, when a host stops being
//!   up, and for those that are not up, when a host recovers.
//!
//! # States
//!
//! A service that fails goes through soft states, one per result, until
//! `max_check_attempts` results have failed; then the state is hard. An
//! OK result ends that at once. While its host is not up a failing
//! service goes to the hard state directly. Hosts work the same way,
//! with two differences in what is logged: a host that recovers on the
//! result that would have made it hard is logged as a hard recovery, and
//! a hard problem that changes between DOWN and UNREACHABLE is logged
//! with attempt 1.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use checkd_config::maincfg::MainConfig;
use checkd_config::model::{Checkable, Objects, ServiceId};
use checkd_config::values::Opts;

use crate::flap::{self, History};
use crate::notify::{self, Comment, ContactEntry, Downtime, Kind, NoticeState, Timer};
use crate::output::{self, Output};
use crate::settings::Settings;
use crate::timeperiod::{Periods, Zone};

/// Milliseconds since the epoch.
pub type Ms = i64;

/// A host or a service, by its place in the engine's lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Obj {
    Host(usize),
    Service(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    HostCheck,
    ServiceCheck,
    HostHandler,
    ServiceHandler,
    Notification,
    /// The command that processes a performance data file.
    PerfdataProcessing,
}

/// A command for the driver to run.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: u64,
    pub kind: JobKind,
    pub obj: Obj,
    /// The command line, macros filled in.
    pub line: String,
    /// How long it may run.
    pub timeout: Ms,
}

/// How a command ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Exited(i32),
    /// Ended by this signal.
    Signaled(i32),
    /// Still running at the timeout, and stopped by the driver.
    TimedOut,
    /// Could not be started. The text says why, in the words of a plugin
    /// that had written them to standard error.
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct JobResult {
    pub outcome: Outcome,
    pub stdout: String,
    pub stderr: String,
}

/// What the engine wants done.
#[derive(Clone, Debug)]
pub enum Effect {
    /// A line for the log. The driver puts the time in front.
    Log(String),
    Run(Job),
    /// A line for the performance data file of hosts (`host` true) or of
    /// services.
    Perfdata { host: bool, line: String },
    /// A result's performance data, for a metrics store: with the export on
    /// only. `service` is empty for a host; `time` in seconds.
    Sample { host: String, service: String, time: i64, perfdata: String },
}

/// The state of one host or service, and the settings its checks go by.
#[derive(Clone, Debug)]
pub struct Item {
    pub cfg: Checkable,
    pub(crate) check_ms: Ms,
    pub(crate) retry_ms: Ms,
    /// Host: 0 up, 1 down, 2 unreachable. Service: 0 OK, 1 warning,
    /// 2 critical, 3 unknown.
    pub state: u8,
    /// The state before the last result.
    pub last_state: u8,
    pub last_hard_state: u8,
    pub hard: bool,
    pub attempt: u32,
    /// Whether there has been a result yet.
    pub checked: bool,
    pub output: Output,
    /// When the last finished check was started. 0 before the first.
    pub last_check: Ms,
    pub next_check: Option<Ms>,
    generation: u64,
    pub running: bool,
    pub(crate) history: History,
    pub flapping: bool,
    pub percent_change: f64,
    pub last_state_change: Ms,
    pub last_hard_state_change: Ms,
    /// When the object was last found in each state. 0 for never.
    pub last_time: [Ms; 4],
    /// Seconds between the time a check was due and its start.
    pub latency: f64,
    /// Seconds the last check ran.
    pub execution_time: f64,
    pub event_id: u64,
    pub last_event_id: u64,
    pub problem_id: String,
    pub last_problem_id: String,
    pub notice: NoticeState,
    /// When the last result came in. 0 before the first.
    pub last_update: Ms,
    /// The waiting check was forced by a command: it runs whatever the
    /// check period, the cache horizon and the dependencies say.
    pub forced: bool,
    /// When the check that ran last was due.
    pub due_at: Ms,
    /// Whether the last result was a passive one.
    pub passive_result: bool,
    /// What commands changed, as the bits `MOD_*`: shown by Livestatus,
    /// and kept even when a later command sets the value back.
    pub modified: u32,
}

pub const MOD_NOTIFICATIONS: u32 = 1;
pub const MOD_ACTIVE_CHECKS: u32 = 2;
pub const MOD_PASSIVE_CHECKS: u32 = 4;
pub const MOD_EVENT_HANDLER: u32 = 8;
pub const MOD_FLAP_DETECTION: u32 = 16;
pub const MOD_NORMAL_INTERVAL: u32 = 1024;
pub const MOD_RETRY_INTERVAL: u32 = 2048;
pub const MOD_MAX_ATTEMPTS: u32 = 4096;
pub const MOD_CUSTOM_VARIABLE: u32 = 32768;

impl Item {
    fn new(cfg: &Checkable, state: u8, settings: &Settings) -> Item {
        let ms = |units: f64| (units * settings.interval_length * 1000.0).round() as Ms;
        Item {
            check_ms: ms(cfg.check_interval),
            retry_ms: ms(cfg.retry_interval),
            state,
            last_state: state,
            last_hard_state: state,
            hard: true,
            // A problem the object starts in counts as one that has had
            // all its attempts.
            attempt: if state == 0 {
                1
            } else {
                cfg.max_check_attempts.max(1)
            },
            checked: false,
            output: Output::default(),
            last_check: 0,
            next_check: None,
            generation: 0,
            running: false,
            history: History::new(state),
            flapping: false,
            percent_change: 0.0,
            last_state_change: 0,
            last_hard_state_change: 0,
            last_time: [0; 4],
            latency: 0.0,
            execution_time: 0.0,
            event_id: 0,
            last_event_id: 0,
            problem_id: String::new(),
            last_problem_id: String::new(),
            notice: NoticeState {
                next: Some(0),
                ..NoticeState::default()
            },
            last_update: 0,
            forced: false,
            due_at: 0,
            passive_result: false,
            modified: 0,
            cfg: cfg.clone(),
        }
    }

    fn max(&self) -> u32 {
        self.cfg.max_check_attempts.max(1)
    }

    /// In a problem state that has not had all its attempts yet.
    fn soft_problem(&self) -> bool {
        self.state != 0 && !self.hard
    }

    pub fn state_type(&self) -> &'static str {
        if self.hard { "HARD" } else { "SOFT" }
    }
}

pub struct HostEntry {
    pub name: String,
    /// The host groups it is in, by name.
    pub groups: Vec<String>,
    pub services: Vec<usize>,
    pub parents: Vec<usize>,
    pub children: Vec<usize>,
    pub item: Item,
}

pub struct ServiceEntry {
    pub host: usize,
    pub host_name: String,
    pub description: String,
    pub groups: Vec<String>,
    pub volatile: bool,
    pub item: Item,
}

/// One execution dependency, seen from the object that depends.
struct Dep {
    master: Obj,
    period: Option<String>,
    inherits: bool,
    criteria: Opts,
}

struct Running {
    obj: Obj,
    kind: JobKind,
    started: Ms,
    timeout: Ms,
}

pub const HOST_STATES: [&str; 3] = ["UP", "DOWN", "UNREACHABLE"];
pub const SERVICE_STATES: [&str; 4] = ["OK", "WARNING", "CRITICAL", "UNKNOWN"];
const HOST_LETTERS: [char; 3] = ['o', 'd', 'u'];
const SERVICE_LETTERS: [char; 4] = ['o', 'w', 'c', 'u'];

pub struct Engine {
    pub(crate) objects: Objects,
    pub(crate) settings: Settings,
    pub(crate) zone: Box<dyn Zone>,
    pub(crate) hosts: Vec<HostEntry>,
    pub(crate) services: Vec<ServiceEntry>,
    pub(crate) host_index: BTreeMap<String, usize>,
    pub(crate) service_index: BTreeMap<ServiceId, usize>,
    host_deps: Vec<Vec<Dep>>,
    service_deps: Vec<Vec<Dep>>,
    /// Checks that wait: time, a number that keeps equal times in the
    /// order they were asked for, the object and its generation. An
    /// entry whose generation is not the object's any more is dropped.
    /// Checks to come: when, services before hosts, the order asked.
    queue: BinaryHeap<Reverse<(Ms, u8, u64, Obj, u64)>>,
    asked: u64,
    running: BTreeMap<u64, Running>,
    jobs: u64,
    events: u64,
    random: u64,
    /// When the engine started.
    pub(crate) start: Ms,
    pub(crate) user_macros: BTreeMap<u32, String>,
    /// The path macros: `MAINCONFIGFILE` and the like.
    pub(crate) paths: BTreeMap<&'static str, String>,
    pub(crate) contacts: Vec<ContactEntry>,
    pub(crate) contact_index: BTreeMap<String, usize>,
    /// For each host and service: its escalations, by their place in the
    /// configuration's lists.
    pub(crate) host_escalations: Vec<Vec<usize>>,
    pub(crate) service_escalations: Vec<Vec<usize>>,
    pub(crate) comments: BTreeMap<u64, Comment>,
    pub(crate) next_comment: u64,
    pub(crate) downtimes: BTreeMap<u64, Downtime>,
    pub(crate) next_downtime: u64,
    pub(crate) timers: BTreeSet<(Ms, u64, Timer)>,
    pub(crate) timer_seq: u64,
    host_checks_run: u64,
    service_checks_run: u64,
    /// Whether results give `Effect::Sample`.
    pub(crate) export: bool,
}

impl Engine {
    /// Sets the engine up for a loaded configuration. The second part of
    /// the answer is warnings for the person who starts it.
    pub fn new(
        objects: Objects,
        main: &MainConfig,
        zone: Box<dyn Zone>,
        start: Ms,
    ) -> (Engine, Vec<String>) {
        let (settings, mut warnings) = Settings::from_main(main);
        for period in objects.timeperiods.values() {
            warnings.extend(crate::timeperiod::notes(period));
        }

        let host_index: BTreeMap<String, usize> = objects
            .hosts
            .keys()
            .enumerate()
            .map(|(at, name)| (name.clone(), at))
            .collect();
        let service_index: BTreeMap<ServiceId, usize> = objects
            .services
            .keys()
            .enumerate()
            .map(|(at, id)| (id.clone(), at))
            .collect();

        let mut hosts: Vec<HostEntry> = objects
            .hosts
            .values()
            .map(|host| {
                let state = match host.check.initial_state {
                    'd' => 1,
                    'u' => 2,
                    _ => 0,
                };
                let mut item = Item::new(&host.check, state, &settings);
                // A host counts as in its state since the start.
                item.last_state_change = start;
                item.last_hard_state_change = start;
                HostEntry {
                    name: host.name.clone(),
                    groups: Vec::new(),
                    services: Vec::new(),
                    parents: host
                        .parents
                        .iter()
                        .filter_map(|name| host_index.get(name).copied())
                        .collect(),
                    children: Vec::new(),
                    item,
                }
            })
            .collect();
        for at in 0..hosts.len() {
            for parent in hosts[at].parents.clone() {
                hosts[parent].children.push(at);
            }
        }
        for group in objects.hostgroups.values() {
            for member in &group.members {
                if let Some(&at) = host_index.get(member) {
                    hosts[at].groups.push(group.name.clone());
                }
            }
        }

        let mut services: Vec<ServiceEntry> = Vec::new();
        for service in objects.services.values() {
            let Some(&host) = host_index.get(&service.host) else {
                continue;
            };
            let state = match service.check.initial_state {
                'w' => 1,
                'c' => 2,
                'u' => 3,
                _ => 0,
            };
            hosts[host].services.push(services.len());
            let item = Item::new(&service.check, state, &settings);
            services.push(ServiceEntry {
                host,
                host_name: service.host.clone(),
                description: service.description.clone(),
                groups: Vec::new(),
                volatile: service.is_volatile,
                item,
            });
        }
        for group in objects.servicegroups.values() {
            for member in &group.members {
                if let Some(&at) = service_index.get(member) {
                    services[at].groups.push(group.name.clone());
                }
            }
        }

        let mut host_deps: Vec<Vec<Dep>> = hosts.iter().map(|_| Vec::new()).collect();
        for dep in &objects.host_dependencies {
            if dep.execution_failure.is_none() {
                continue;
            }
            if let (Some(&master), Some(&dependent)) =
                (host_index.get(&dep.master), host_index.get(&dep.dependent))
            {
                host_deps[dependent].push(Dep {
                    master: Obj::Host(master),
                    period: dep.period.clone(),
                    inherits: dep.inherits_parent,
                    criteria: dep.execution_failure.clone(),
                });
            }
        }
        let mut service_deps: Vec<Vec<Dep>> = services.iter().map(|_| Vec::new()).collect();
        for dep in &objects.service_dependencies {
            if dep.execution_failure.is_none() {
                continue;
            }
            if let (Some(&master), Some(&dependent)) = (
                service_index.get(&dep.master),
                service_index.get(&dep.dependent),
            ) {
                service_deps[dependent].push(Dep {
                    master: Obj::Service(master),
                    period: dep.period.clone(),
                    inherits: dep.inherits_parent,
                    criteria: dep.execution_failure.clone(),
                });
            }
        }

        let dir = main.path.parent().map(|dir| dir.to_path_buf());
        let path = |name: &str| -> String {
            match main.get(name) {
                None => String::new(),
                Some(value) => match &dir {
                    Some(dir) => dir.join(value).to_string_lossy().into_owned(),
                    None => value.to_string(),
                },
            }
        };
        let mut paths = BTreeMap::new();
        paths.insert("MAINCONFIGFILE", main.path.to_string_lossy().into_owned());
        paths.insert("LOGFILE", path("log_file"));
        paths.insert("RESOURCEFILE", path("resource_file"));
        paths.insert("TEMPPATH", path("temp_path"));
        paths.insert("COMMANDFILE", path("command_file"));
        paths.insert("TEMPFILE", path("temp_file"));
        paths.insert("STATUSDATAFILE", path("status_file"));
        paths.insert("OBJECTCACHEFILE", path("object_cache_file"));
        paths.insert("RETENTIONDATAFILE", path("state_retention_file"));
        paths.insert("HOSTPERFDATAFILE", path("host_perfdata_file"));
        paths.insert("SERVICEPERFDATAFILE", path("service_perfdata_file"));

        let (contacts, contact_index) = notify::contacts_of(&objects);
        let mut host_escalations: Vec<Vec<usize>> = hosts.iter().map(|_| Vec::new()).collect();
        for (at, escalation) in objects.host_escalations.iter().enumerate() {
            if let Some(&host) = host_index.get(&escalation.object) {
                host_escalations[host].push(at);
            }
        }
        let mut service_escalations: Vec<Vec<usize>> = services.iter().map(|_| Vec::new()).collect();
        for (at, escalation) in objects.service_escalations.iter().enumerate() {
            if let Some(&service) = service_index.get(&escalation.object) {
                service_escalations[service].push(at);
            }
        }

        let engine = Engine {
            user_macros: main.user_macros.clone(),
            contacts,
            contact_index,
            host_escalations,
            service_escalations,
            comments: BTreeMap::new(),
            next_comment: 0,
            downtimes: BTreeMap::new(),
            next_downtime: 0,
            timers: BTreeSet::new(),
            timer_seq: 0,
            host_checks_run: 0,
            service_checks_run: 0,
            export: false,
            objects,
            settings,
            zone,
            hosts,
            services,
            host_index,
            service_index,
            host_deps,
            service_deps,
            queue: BinaryHeap::new(),
            asked: 0,
            running: BTreeMap::new(),
            jobs: 0,
            events: 0,
            random: (start as u64) ^ 0x9e37_79b9_7f4a_7c15,
            start,
            paths,
        };
        (engine, warnings)
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Sets an object's check intervals and attempts from its settings, as
    /// a command that changes them asks.
    pub(crate) fn reset_intervals(&mut self, obj: Obj) {
        let length = self.settings.interval_length;
        let item = self.item_mut(obj);
        item.check_ms = (item.cfg.check_interval * length * 1000.0).round() as Ms;
        item.retry_ms = (item.cfg.retry_interval * length * 1000.0).round() as Ms;
    }

    /// A host's services, in the reverse order of their names, as the
    /// reference goes through them.
    pub(crate) fn services_of(&self, host: usize) -> Vec<Obj> {
        let mut list = self.hosts[host].services.clone();
        list.sort_by(|a, b| self.services[*b].description.cmp(&self.services[*a].description));
        list.into_iter().map(Obj::Service).collect()
    }

    /// Turns notifications on or off for all, as `ENABLE_NOTIFICATIONS`
    /// does.
    pub fn set_notifications(&mut self, on: bool) {
        self.settings.enable_notifications = on;
    }

    /// Shadow mode: no notifications are sent and no event handlers run.
    pub fn quiet(&mut self) {
        self.settings.enable_notifications = false;
        self.settings.enable_event_handlers = false;
    }

    /// Has every result give its performance data as `Effect::Sample`.
    pub fn export_perfdata(&mut self, on: bool) {
        self.export = on;
    }

    /// How many checks are planned.
    pub fn planned_checks(&self) -> usize {
        self.all_objects().into_iter().filter(|o| self.item(*o).next_check.is_some()).count()
    }

    /// The configuration the engine was made from.
    pub fn objects(&self) -> &Objects {
        &self.objects
    }

    pub fn comments(&self) -> impl Iterator<Item = &crate::notify::Comment> {
        self.comments.values()
    }

    pub fn downtimes(&self) -> impl Iterator<Item = &crate::notify::Downtime> {
        self.downtimes.values()
    }

    /// When the engine started, in milliseconds since the epoch.
    pub fn start_time(&self) -> Ms {
        self.start
    }

    /// Whether a timeperiod is valid at a second. A period that is not
    /// there never is; no period at all always is.
    pub fn in_period(&self, period: Option<&str>, second: i64) -> bool {
        match period {
            None => true,
            Some(name) => self.periods().is_valid(name, second),
        }
    }

    /// How many host checks and service checks were started.
    pub fn checks_run(&self) -> (u64, u64) {
        (self.host_checks_run, self.service_checks_run)
    }

    /// The contacts and runtime switches of notifications, by name.
    pub fn contact_names(&self) -> impl Iterator<Item = &str> {
        self.contacts.iter().map(|c| c.cfg.name.as_str())
    }

    pub fn hosts(&self) -> &[HostEntry] {
        &self.hosts
    }

    pub fn services(&self) -> &[ServiceEntry] {
        &self.services
    }

    pub fn host(&self, name: &str) -> Option<Obj> {
        self.host_index.get(name).map(|&at| Obj::Host(at))
    }

    pub fn service(&self, host: &str, description: &str) -> Option<Obj> {
        self.service_index
            .get(&(host.to_string(), description.to_string()))
            .map(|&at| Obj::Service(at))
    }

    pub fn item(&self, obj: Obj) -> &Item {
        match obj {
            Obj::Host(at) => &self.hosts[at].item,
            Obj::Service(at) => &self.services[at].item,
        }
    }

    pub(crate) fn item_mut(&mut self, obj: Obj) -> &mut Item {
        match obj {
            Obj::Host(at) => &mut self.hosts[at].item,
            Obj::Service(at) => &mut self.services[at].item,
        }
    }

    /// `a` for a host, `a;s` for a service: how the log names it.
    pub fn log_name(&self, obj: Obj) -> String {
        match obj {
            Obj::Host(at) => self.hosts[at].name.clone(),
            Obj::Service(at) => {
                let service = &self.services[at];
                format!("{};{}", service.host_name, service.description)
            }
        }
    }

    pub fn state_name(&self, obj: Obj) -> &'static str {
        match obj {
            Obj::Host(at) => HOST_STATES[self.hosts[at].item.state as usize],
            Obj::Service(at) => SERVICE_STATES[self.services[at].item.state as usize],
        }
    }

    /// Whether the engine checks this object at all.
    pub fn is_checked(&self, obj: Obj) -> bool {
        let item = self.item(obj);
        let on = match obj {
            Obj::Host(_) => self.settings.execute_host_checks,
            Obj::Service(_) => self.settings.execute_service_checks,
        };
        on && item.cfg.active_checks_enabled && item.cfg.check_command.is_some()
    }

    /// Whether the object has checks planned: its active checks are on
    /// and it has a check command. While checks are stopped for all hosts
    /// or services, they are planned and passed over when they come up.
    fn is_planned(&self, obj: Obj) -> bool {
        let cfg = &self.item(obj).cfg;
        cfg.active_checks_enabled && cfg.check_command.is_some()
    }

    /// The command line of the object's check as it would be now.
    pub fn check_line(&self, obj: Obj, now: Ms) -> Option<String> {
        let command = self.item(obj).cfg.check_command.clone()?;
        self.command_line(&command, obj, now)
    }

    /// Starts the engine: the initial states go to the log if that is
    /// asked for, and every object that is checked gets its first check.
    /// The first checks are spread over each object's check interval.
    pub fn start(&mut self, out: &mut Vec<Effect>) {
        self.plan_perfdata_processing();
        if self.settings.log_initial_states {
            for at in 0..self.hosts.len() {
                let obj = Obj::Host(at);
                let line = format!(
                    "{}{}",
                    self.state_line("INITIAL HOST STATE", obj),
                    self.hosts[at].item.output.short
                );
                out.push(Effect::Log(line));
            }
            for at in 0..self.services.len() {
                let obj = Obj::Service(at);
                let line = format!("{}(null)", self.state_line("INITIAL SERVICE STATE", obj));
                out.push(Effect::Log(line));
            }
        }
        let start = self.start;
        let count = self.hosts.len().max(1) as Ms;
        for at in 0..self.hosts.len() {
            let obj = Obj::Host(at);
            if self.is_planned(obj) {
                let spread = self.hosts[at].item.check_ms * at as Ms / count;
                self.schedule(obj, start + spread);
            }
        }
        let count = self.services.len().max(1) as Ms;
        for at in 0..self.services.len() {
            let obj = Obj::Service(at);
            if self.is_planned(obj) {
                let spread = self.services[at].item.check_ms * at as Ms / count;
                self.schedule(obj, start + spread);
            }
        }
    }

    /// Moves an object's waiting check to a time, as a command asks. A
    /// check that is not forced does not move to a later time than the one
    /// it has.
    pub(crate) fn check_at(&mut self, obj: Obj, at: Ms, forced: bool) {
        if self.item(obj).cfg.check_command.is_none() {
            return;
        }
        if !forced {
            if let Some(next) = self.item(obj).next_check {
                if next <= at {
                    return;
                }
            }
        }
        // Checks a command asks for are made in the order they were
        // asked for.
        self.schedule_ranked(obj, at, Some(1));
        self.item_mut(obj).forced = forced;
    }

    /// Moves the waiting check of an object. For runs that have to follow
    /// another engine's times.
    pub fn set_first_check(&mut self, obj: Obj, at: Ms) {
        if self.is_checked(obj) {
            self.schedule(obj, at);
        }
    }

    /// The check that comes up next, and when.
    pub fn next_due(&mut self) -> Option<(Ms, Obj)> {
        self.next_check_due()?;
        self.queue.peek().map(|&Reverse((at, _, _, obj, _))| (at, obj))
    }

    /// When the next check is due, if ever.
    fn next_check_due(&mut self) -> Option<Ms> {
        while let Some(&Reverse((at, _, _, obj, generation))) = self.queue.peek() {
            if self.item(obj).generation == generation {
                return Some(at);
            }
            self.queue.pop();
        }
        None
    }

    /// When the engine next has something to do on its own, if ever: a
    /// check, or the start or end of a downtime.
    pub fn next_wakeup(&mut self) -> Option<Ms> {
        match (self.next_check_due(), self.next_timer()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Does what is due at `now`.
    pub fn advance(&mut self, now: Ms, out: &mut Vec<Effect>) {
        loop {
            let check = self.next_check_due().filter(|&at| at <= now);
            let timer = self.next_timer().filter(|&at| at <= now);
            match (check, timer) {
                (None, None) => break,
                (Some(c), Some(t)) if t <= c => self.run_timers(t, out),
                (None, Some(t)) => self.run_timers(t, out),
                _ => {
                    let Reverse((at, _, _, obj, _)) = self.queue.pop().unwrap();
                    self.item_mut(obj).next_check = None;
                    self.due(obj, at, now, out);
                }
            }
        }
    }

    fn schedule(&mut self, obj: Obj, at: Ms) {
        self.schedule_ranked(obj, at, None);
    }

    /// Plans a check. `rank` puts checks due at one moment in order;
    /// without it a service goes before a host.
    fn schedule_ranked(&mut self, obj: Obj, at: Ms, rank: Option<u8>) {
        self.asked += 1;
        let asked = self.asked;
        let item = self.item_mut(obj);
        item.generation += 1;
        item.next_check = Some(at);
        let generation = item.generation;
        // A service and its host that are due at one moment: the service
        // goes first, so that its result is read while the host still
        // counts as it did. The reference's log shows that order.
        let rank = rank.unwrap_or(match obj {
            Obj::Service(_) => 0,
            Obj::Host(_) => 1,
        });
        self.queue.push(Reverse((at, rank, asked, obj, generation)));
    }

    pub(crate) fn periods(&self) -> Periods<'_> {
        Periods::new(&self.objects.timeperiods, &*self.zone)
    }

    /// A check's time has come.
    fn due(&mut self, obj: Obj, at: Ms, now: Ms, out: &mut Vec<Effect>) {
        // Checks stopped for all hosts or all services: this one is
        // passed over and comes up again an interval on.
        let stopped = match obj {
            Obj::Host(_) => !self.settings.execute_host_checks,
            Obj::Service(_) => !self.settings.execute_service_checks,
        };
        let cfg = &self.item(obj).cfg;
        let forced = self.item(obj).forced;
        if stopped && !forced && cfg.active_checks_enabled && cfg.check_command.is_some() {
            let item = self.item(obj);
            let wait = first_set(item.check_ms, item.retry_ms);
            if wait > 0 {
                self.schedule(obj, now + wait);
            }
            return;
        }
        let can = self.item(obj).cfg.active_checks_enabled && self.item(obj).cfg.check_command.is_some();
        if !(self.is_checked(obj) || (forced && can)) || self.item(obj).running {
            return;
        }
        let second = now.div_euclid(1000);
        let forced = std::mem::take(&mut self.item_mut(obj).forced);
        let item = self.item(obj);

        if let Some(period) = item.cfg.check_period.as_ref().filter(|_| !forced) {
            let periods = self.periods();
            if !periods.is_valid(period, second) {
                if let Some(next) = periods.next_valid(period, second) {
                    self.schedule(obj, next * 1000);
                }
                return;
            }
        }

        let horizon = match obj {
            Obj::Host(_) => self.settings.cached_host_check_horizon,
            Obj::Service(_) => self.settings.cached_service_check_horizon,
        };
        if !forced && item.checked && second - item.last_check.div_euclid(1000) < horizon {
            let wait = match obj {
                Obj::Host(_) => first_set(item.retry_ms, item.check_ms),
                Obj::Service(_) => first_set(item.check_ms, item.retry_ms),
            };
            if wait > 0 {
                self.schedule(obj, now + wait);
            }
            return;
        }

        if !forced && self.held_back(obj, second, 0) {
            let wait = first_set(item.check_ms, item.retry_ms);
            if wait > 0 {
                self.schedule(obj, now + wait);
            }
            return;
        }

        let Some(line) = self.check_line(obj, now) else {
            return;
        };
        let (kind, timeout) = match obj {
            Obj::Host(_) => (JobKind::HostCheck, self.settings.host_check_timeout),
            Obj::Service(_) => (JobKind::ServiceCheck, self.settings.service_check_timeout),
        };
        let own = self.item(obj).cfg.check_timeout;
        let timeout = if own > 0 { own } else { timeout } as Ms * 1000;
        let item = self.item_mut(obj);
        item.running = true;
        // A check a command forced has no latency.
        item.latency = if forced { 0.0 } else { (now - at).max(0) as f64 / 1000.0 };
        item.due_at = at;
        match obj {
            Obj::Host(_) => self.host_checks_run += 1,
            Obj::Service(_) => self.service_checks_run += 1,
        }
        self.run(obj, kind, line, timeout, now, out);
    }

    pub(crate) fn run_job(&mut self, obj: Obj, kind: JobKind, line: String, timeout: Ms, now: Ms, out: &mut Vec<Effect>) {
        self.run(obj, kind, line, timeout, now, out);
    }

    fn run(&mut self, obj: Obj, kind: JobKind, line: String, timeout: Ms, now: Ms, out: &mut Vec<Effect>) {
        self.jobs += 1;
        let id = self.jobs;
        self.running.insert(
            id,
            Running {
                obj,
                kind,
                started: now,
                timeout,
            },
        );
        out.push(Effect::Run(Job {
            id,
            kind,
            obj,
            line,
            timeout,
        }));
    }

    /// Whether an execution dependency keeps the object from being
    /// checked now.
    fn held_back(&self, obj: Obj, second: i64, depth: usize) -> bool {
        if depth > 32 {
            return false;
        }
        let deps = match obj {
            Obj::Host(at) => &self.host_deps[at],
            Obj::Service(at) => &self.service_deps[at],
        };
        for dep in deps {
            if let Some(period) = &dep.period {
                if !self.periods().is_valid(period, second) {
                    continue;
                }
            }
            let master = self.item(dep.master);
            let state = if master.hard || self.settings.soft_state_dependencies {
                master.state
            } else {
                master.last_hard_state
            };
            let letter = if !master.checked && dep.criteria.has('p') {
                'p'
            } else {
                match dep.master {
                    Obj::Host(_) => HOST_LETTERS[state as usize],
                    Obj::Service(_) => SERVICE_LETTERS[state as usize],
                }
            };
            if dep.criteria.has(letter) {
                return true;
            }
            if dep.inherits && self.held_back(dep.master, second, depth + 1) {
                return true;
            }
        }
        false
    }

    /// Takes the outcome of a command the engine asked for.
    pub fn job_done(&mut self, now: Ms, id: u64, result: JobResult, out: &mut Vec<Effect>) {
        let Some(job) = self.running.remove(&id) else {
            return;
        };
        match (job.kind, job.obj) {
            (JobKind::ServiceCheck, Obj::Service(at)) => {
                let (state, output) = self.read_result(&job, &result, out);
                self.services[at].item.passive_result = false;
                self.service_result(at, state, output, job.started, now, out);
            }
            (JobKind::HostCheck, Obj::Host(at)) => {
                let (state, output) = self.read_result(&job, &result, out);
                self.hosts[at].item.passive_result = false;
                // Codes 0 and 1 mean the host is there.
                self.host_result(at, state <= 1, output, job.started, now, out);
            }
            _ => {}
        }
    }

    /// The plugin state (0 to 3) and output of a finished check.
    fn read_result(&self, job: &Running, result: &JobResult, out: &mut Vec<Effect>) -> (u8, Output) {
        let host = matches!(job.obj, Obj::Host(_));
        let mut output = output::parse(&result.stdout);
        if result.stdout.trim().is_empty() {
            output.short = output::no_output(&result.stderr, host);
        }
        match &result.outcome {
            Outcome::Exited(code @ 0..=3) => (*code as u8, output),
            Outcome::Exited(code) => {
                // For a host the reference says of 126 what it says of
                // 127. The missing space is the reference's.
                let hint = match code {
                    126 if !host => "Make sure the plugin you're trying to run is executable.",
                    126 | 127 => " Make sure the plugin you're trying to run actually exists.",
                    _ => "",
                };
                let what = match job.obj {
                    Obj::Host(at) => format!("host '{}'", self.hosts[at].name),
                    Obj::Service(at) => format!(
                        "service '{}' on host '{}'",
                        self.services[at].description, self.services[at].host_name
                    ),
                };
                out.push(Effect::Log(format!(
                    "Warning: Return code of {code} for check of {what} was out of bounds.{hint}"
                )));
                let note = match code {
                    126 if !host => " - plugin may not be executable",
                    126 | 127 => " - plugin may be missing",
                    _ => "",
                };
                let output = Output {
                    short: format!("(Return code of {code} is out of bounds{note})"),
                    ..Output::default()
                };
                (2, output)
            }
            Outcome::Signaled(_) => (3, output),
            Outcome::TimedOut => {
                let seconds = job.timeout as f64 / 1000.0;
                let (state, what) = if host {
                    (2, "Host")
                } else {
                    (self.settings.service_check_timeout_state, "Service")
                };
                let output = Output {
                    short: format!("({what} check timed out after {seconds:.2} seconds)"),
                    ..Output::default()
                };
                (state, output)
            }
            Outcome::Failed(why) => {
                let output = Output {
                    short: output::no_output(why, host),
                    ..Output::default()
                };
                (2, output)
            }
        }
    }

    pub(crate) fn service_result(
        &mut self,
        at: usize,
        new: u8,
        output: Output,
        started: Ms,
        now: Ms,
        out: &mut Vec<Effect>,
    ) {
        let obj = Obj::Service(at);
        let host = self.services[at].host;
        let host_up = self.hosts[host].item.state == 0;
        let volatile = self.services[at].volatile;
        let log_retries = self.settings.log_service_retries;

        let item = &mut self.services[at].item;
        // A problem that was made hard only by the host being down starts
        // again as a soft one once the host is back up. The host is not
        // asked about at that result.
        let mut restarted = false;
        if item.notice.forced && host_up && new != 0 && item.state != 0 {
            item.hard = false;
            item.attempt = 0;
            item.notice.forced = false;
            restarted = true;
        }
        let (was, was_hard, attempt) = (item.state, item.hard, item.attempt);
        let hard_before = item.last_hard_state;
        let max = item.max();
        let first = !item.checked;
        let new_text = first || item.output.short != output.short || item.output.long != output.long;
        item.running = false;
        item.checked = true;
        item.last_check = started;
        item.last_update = now;
        item.execution_time = (now - started) as f64 / 1000.0;
        item.last_state = was;
        item.output = output;
        // The state it leaves was last found now too.
        item.last_time[was as usize] = now;
        item.last_time[new as usize] = now;

        // `step` is a move in the state machine: it is logged as an alert
        // and the event handlers run.
        let mut step = false;
        let mut info = false;
        let ask_host;
        if new == 0 {
            if was != 0 {
                step = true;
                item.hard = was_hard;
                item.attempt = if was_hard { attempt } else { attempt + 1 };
            } else {
                item.hard = true;
                item.attempt = 1;
            }
            ask_host = !host_up;
        } else {
            ask_host = new != was || (was != 0 && !was_hard && !restarted);
            if was == 0 {
                step = true;
                item.attempt = 1;
                item.hard = max <= 1 || !host_up;
                info = !host_up;
            } else if !was_hard {
                step = true;
                item.attempt = (attempt + 1).min(max);
                item.hard = item.attempt >= max || !host_up;
                info = !host_up;
            } else if new != was || volatile {
                step = true;
            }
        }
        item.state = new;
        // Gone hard only because the host is not up.
        if new != was || new == 0 {
            item.notice.forced = false;
        }
        if info && item.attempt < max {
            item.notice.forced = true;
        }

        if new != was {
            item.last_state_change = now;
            self.events += 1;
            let item = &mut self.services[at].item;
            item.last_event_id = item.event_id;
            item.event_id = self.events;
            self.problem_ids(obj, was, new);
        } else if first {
            self.services[at].item.last_state_change = now;
        }
        let item = &mut self.services[at].item;
        if item.hard && item.state != item.last_hard_state {
            item.last_hard_state = item.state;
            item.last_hard_state_change = now;
        }
        let stalked = !step && new_text && item.cfg.stalking_options.has(SERVICE_LETTERS[new as usize]);

        if info {
            out.push(Effect::Log(format!(
                "SERVICE INFO: {}; Service switch to hard down state due to host down.",
                self.log_name(obj)
            )));
        }
        let item = &self.services[at].item;
        // A soft problem that turns hard in the state it was last hard in
        // is not a change of hard state, and the reference writes no
        // alert for it.
        let same_hard = item.hard && !was_hard && was != 0 && hard_before == new;
        if (step || stalked) && (item.hard || log_retries) && !same_hard {
            let line = format!("{}{}", self.state_line("SERVICE ALERT", obj), item.output.short);
            out.push(Effect::Log(line));
        }

        // A result that leaves the service in a soft problem state does
        // not go into the flap history.
        if !item.soft_problem() {
            self.record_flap(obj, now, out);
        }
        let changed = new != was || self.services[at].item.hard != was_hard;
        if step || self.services[at].item.hard {
            self.after_result(obj, was, changed, now, out);
        }
        if step {
            self.handlers(obj, now, out);
        }
        let item = &mut self.services[at].item;
        if item.state == 0 {
            item.hard = true;
            item.attempt = 1;
        }

        // Every service result adds the host's state to the host's history.
        self.record_flap(Obj::Host(host), now, out);
        if ask_host {
            self.check_host_now(host, now, true);
        }
        self.perfdata(obj, now, out);
        self.schedule_next(obj, started, now);
    }

    fn host_result(
        &mut self,
        at: usize,
        up: bool,
        output: Output,
        started: Ms,
        now: Ms,
        out: &mut Vec<Effect>,
    ) {
        let parents = &self.hosts[at].parents;
        let cut_off = !parents.is_empty() && parents.iter().all(|&p| self.hosts[p].item.state != 0);
        let new: u8 = if up {
            0
        } else if cut_off {
            2
        } else {
            1
        };
        self.host_state(at, new, false, output, started, now, out);
    }

    /// What a result does to a host whose new state is known. A passive
    /// result is hard at once unless `passive_host_checks_are_soft`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn host_state(
        &mut self,
        at: usize,
        new: u8,
        passive: bool,
        output: Output,
        started: Ms,
        now: Ms,
        out: &mut Vec<Effect>,
    ) {
        let obj = Obj::Host(at);
        let passive_hard = passive && !self.settings.passive_host_checks_are_soft;
        let log_retries = self.settings.log_host_retries;

        let item = &mut self.hosts[at].item;
        let (was, was_hard, attempt) = (item.state, item.hard, item.attempt);
        let max = item.max();
        let first = !item.checked;
        let new_text = first || item.output.short != output.short || item.output.long != output.long;
        item.running = false;
        item.checked = true;
        item.last_check = started;
        item.last_update = now;
        item.execution_time = (now - started) as f64 / 1000.0;
        item.last_state = was;
        item.output = output;
        item.last_time[was as usize] = now;
        item.last_time[new as usize] = now;

        let mut step = false;
        if new == 0 {
            if was != 0 {
                step = true;
                if was_hard {
                    item.attempt = 1;
                } else if attempt + 1 >= max {
                    // Recovered on the result that would have made the
                    // problem hard: written as a hard recovery.
                    item.hard = true;
                    item.attempt = max;
                } else {
                    item.attempt = attempt + 1;
                }
            } else {
                item.hard = true;
                item.attempt = 1;
            }
        } else if was == 0 {
            step = true;
            item.attempt = 1;
            item.hard = max <= 1;
        } else if !was_hard {
            step = true;
            item.attempt = (attempt + 1).min(max);
            item.hard = item.attempt >= max;
        } else {
            // A host that stays in a hard problem is at attempt 1 from its
            // next result on.
            step = new != was;
            item.attempt = 1;
        }
        if passive_hard {
            step = new != was;
            item.hard = true;
            item.attempt = 1;
        }
        item.state = new;
        // A host's state counts as changed when its hard state does.
        if item.hard && item.last_hard_state != new {
            item.last_state_change = now;
        }

        if new != was {
            self.problem_ids(obj, was, new);
        }
        if step {
            self.events += 1;
            let item = &mut self.hosts[at].item;
            item.last_event_id = item.event_id;
            item.event_id = self.events;
        }
        let item = &mut self.hosts[at].item;
        if item.hard && item.state != item.last_hard_state {
            item.last_hard_state = item.state;
            item.last_hard_state_change = now;
        }
        let stalked = !step && new_text && item.cfg.stalking_options.has(HOST_LETTERS[new as usize]);

        let item = &self.hosts[at].item;
        if (step || stalked) && (item.hard || log_retries) {
            let line = format!("{}{}", self.state_line("HOST ALERT", obj), item.output.short);
            out.push(Effect::Log(line));
        }
        // For a host the reference runs the event handler before it looks
        // at flapping. For a service it is the other way round.
        if step {
            self.handlers(obj, now, out);
        }
        // A host's notification is tried before its flapping is looked
        // at; a service's after.
        let changed = new != was || self.hosts[at].item.hard != was_hard;
        self.after_result(obj, was, changed, now, out);
        self.record_flap(obj, now, out);
        let item = &mut self.hosts[at].item;
        if item.state == 0 {
            item.hard = true;
            item.attempt = 1;
        }

        // The hosts around it are looked at again when a host stops
        // being up, and when it comes back: the parents and children that
        // still count as it did before, and, when it stops being up, the
        // children that are down too, which may now be cut off.
        if (was == 0) != (new == 0) {
            let host = &self.hosts[at];
            let as_before = |other: &usize| (self.hosts[*other].item.state == 0) == (was == 0);
            let around: Vec<usize> = host
                .parents
                .iter()
                .filter(|other| as_before(other))
                .chain(host.children.iter().filter(|other| was == 0 || as_before(other)))
                .copied()
                .collect();
            for other in around {
                self.check_host_now(other, now, false);
            }
        }
        self.perfdata(obj, now, out);
        self.schedule_next(obj, started, now);
    }

    fn problem_ids(&mut self, obj: Obj, was: u8, new: u8) {
        if was == 0 {
            let id = self.uuid();
            let item = self.item_mut(obj);
            item.last_problem_id = std::mem::replace(&mut item.problem_id, id);
        } else if new == 0 {
            let item = self.item_mut(obj);
            item.last_problem_id = std::mem::take(&mut item.problem_id);
        }
    }

    pub(crate) fn uuid_pub(&mut self) -> String {
        self.uuid()
    }

    fn uuid(&mut self) -> String {
        let mut next = || {
            self.random ^= self.random << 13;
            self.random ^= self.random >> 7;
            self.random ^= self.random << 17;
            self.random
        };
        let (a, b) = (next(), next());
        format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            a >> 32,
            (a >> 16) & 0xffff,
            a & 0xfff,
            0x8000 | ((b >> 48) & 0x3fff),
            b & 0xffff_ffff_ffff
        )
    }

    /// `LABEL: name;STATE;TYPE;attempt;`
    fn state_line(&self, label: &str, obj: Obj) -> String {
        let item = self.item(obj);
        format!(
            "{label}: {};{};{};{};",
            self.log_name(obj),
            self.state_name(obj),
            item.state_type(),
            item.attempt
        )
    }

    /// Adds the object's state to its flap history and says so in the log
    /// when flapping starts or stops.
    fn record_flap(&mut self, obj: Obj, now: Ms, out: &mut Vec<Effect>) {
        let on = self.settings.enable_flap_detection;
        let (global_low, global_high, letters, label, what): (f64, f64, &[char], &str, &str) = match obj {
            Obj::Host(_) => (
                self.settings.low_host_flap_threshold,
                self.settings.high_host_flap_threshold,
                &HOST_LETTERS,
                "HOST FLAPPING ALERT",
                "Host",
            ),
            Obj::Service(_) => (
                self.settings.low_service_flap_threshold,
                self.settings.high_service_flap_threshold,
                &SERVICE_LETTERS,
                "SERVICE FLAPPING ALERT",
                "Service",
            ),
        };
        let item = self.item_mut(obj);
        if !item.cfg.flap_detection_options.has(letters[item.state as usize]) {
            return;
        }
        let percent = item.history.record(item.state);
        item.percent_change = percent;
        if !on || !item.cfg.flap_detection_enabled {
            return;
        }
        let low = if item.cfg.low_flap_threshold > 0.0 { item.cfg.low_flap_threshold } else { global_low };
        let high = if item.cfg.high_flap_threshold > 0.0 { item.cfg.high_flap_threshold } else { global_high };
        let change = flap::judge(item.flapping, percent, low, high);
        let started = matches!(change, flap::Change::Started);
        let text = match change {
            flap::Change::None => return,
            flap::Change::Started => {
                item.flapping = true;
                // The reference writes ">=" for a service and ">" for a host.
                let sign = if matches!(obj, Obj::Host(_)) { ">" } else { ">=" };
                format!("STARTED; {what} appears to have started flapping ({percent:.1}% change {sign} {high:.1}% threshold)")
            }
            flap::Change::Stopped => {
                item.flapping = false;
                format!("STOPPED; {what} appears to have stopped flapping ({percent:.1}% change < {low:.1}% threshold)")
            }
        };
        out.push(Effect::Log(format!("{label}: {};{text}", self.log_name(obj))));
        let kind = if started { Kind::FlappingStart } else { Kind::FlappingStop };
        self.notify(obj, kind, "", "", now, out);
    }

    /// Runs the event handlers for the state the object is in now: the
    /// global one first, then its own.
    fn handlers(&mut self, obj: Obj, now: Ms, out: &mut Vec<Effect>) {
        if !self.settings.enable_event_handlers {
            return;
        }
        let (global, label, kind) = match obj {
            Obj::Host(_) => (
                self.settings.global_host_event_handler.clone(),
                "HOST EVENT HANDLER",
                JobKind::HostHandler,
            ),
            Obj::Service(_) => (
                self.settings.global_service_event_handler.clone(),
                "SERVICE EVENT HANDLER",
                JobKind::ServiceHandler,
            ),
        };
        let item = self.item(obj);
        let own = if item.cfg.event_handler_enabled { item.cfg.event_handler.clone() } else { None };
        let timeout = self.settings.event_handler_timeout as Ms * 1000;
        for (label, command) in [(format!("GLOBAL {label}"), global), (label.to_string(), own)] {
            let Some(command) = command else { continue };
            let Some(line) = self.command_line(&command, obj, now) else {
                continue;
            };
            if self.settings.log_event_handlers {
                out.push(Effect::Log(format!("{}{command}", self.state_line(&label, obj))));
            }
            self.run(obj, kind, line, timeout, now, out);
        }
    }

    /// Moves the host's waiting check to now.
    ///
    /// A host whose last result is young enough to be used again is not
    /// checked. Asked for by a service's result, such a host keeps its
    /// place in the queue. Asked for by a neighbour's change of state, it
    /// loses it: the check comes up now, is passed over, and is planned
    /// again a retry interval on.
    fn check_host_now(&mut self, at: usize, now: Ms, for_service: bool) {
        let obj = Obj::Host(at);
        let item = &self.hosts[at].item;
        if item.running || !self.is_checked(obj) {
            return;
        }
        let age = now.div_euclid(1000) - item.last_check.div_euclid(1000);
        if for_service && item.checked && age < self.settings.cached_host_check_horizon {
            return;
        }
        self.schedule(obj, now);
    }

    fn schedule_next(&mut self, obj: Obj, started: Ms, now: Ms) {
        if !self.is_planned(obj) {
            return;
        }
        let item = self.item(obj);
        // A passive result leaves the planned active check where it is,
        // unless it leaves the object in a soft problem, which is retried.
        if item.passive_result && !item.soft_problem() && item.next_check.is_some() {
            return;
        }
        let wait = if item.soft_problem() {
            first_set(item.retry_ms, item.check_ms)
        } else {
            item.check_ms
        };
        if wait <= 0 {
            // Without a check interval the object is checked again only
            // on demand.
            let item = self.item_mut(obj);
            item.generation += 1;
            item.next_check = None;
            return;
        }
        // The next check is planned from when the last one was due, and the
        // time the check took to start, but at most the millisecond the
        // reference takes, when it started within a second of being due.
        // So checks drift as the reference's do, and not by however late
        // checkd itself was.
        let late = started - item.due_at;
        let base = if (0..1000).contains(&late) { item.due_at + late.min(1) } else { started };
        let mut at = base + wait;
        if at <= now {
            at = now + wait;
        }
        self.schedule(obj, at);
    }
}

fn first_set(first: Ms, second: Ms) -> Ms {
    if first > 0 { first } else { second }
}
