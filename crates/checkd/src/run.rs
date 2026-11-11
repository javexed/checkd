//! `checkd run`: the engine on the real clock, with real programs.
//!
//! The engine decides what is checked when and what a result means. This
//! file does what the engine cannot: it keeps the time, starts the
//! commands the engine asks for, stops the ones that run too long, writes
//! the log, and reads the command file.
//!
//! The command file is a named pipe at the configuration's
//! `command_file`, made at the start when it is not there. A thread reads
//! it line by line. It keeps the pipe open for writing too, so that the
//! pipe does not end when the last program that wrote to it lets go.

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use checkd_config::maincfg::PathMap;
use checkd_config::{Options, load};
use checkd_core::cmdline::{self, Start};
use checkd_core::engine::{Effect, Engine, Job, JobKind, JobResult, Ms, Outcome};
use checkd_core::timeperiod::Zone;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::otel::{self, Telemetry};
use crate::store::{Sqlite, Store};

/// How much of what a command prints is kept, on each of standard output
/// and standard error. The rest is read and dropped, so that the command
/// is not held up.
const KEEP: usize = 1024 * 1024;

/// How long output is still read after a command has ended. Something
/// the command started may have kept its output open.
const GRACE: Duration = Duration::from_millis(500);

pub struct RunArgs {
    pub main: PathBuf,
    pub path_map: PathMap,
    /// Where the log goes instead of the configuration's `log_file`.
    /// `-` is standard output.
    pub log_file: Option<PathBuf>,
    /// Stop after this many seconds.
    pub seconds: Option<u64>,
    /// For tests: a file that says when each object is first checked.
    pub first_checks: Option<PathBuf>,
    /// For tests: the time to take the moment of the start for, in
    /// milliseconds since the epoch.
    pub start_at: Option<Ms>,
    /// Where to send OpenTelemetry data instead of where the environment
    /// says.
    pub otlp_endpoint: Option<String>,
    /// The command file, from the configuration. Filled in by `run`.
    pub command_file: Option<PathBuf>,
    /// Where the state is kept: `sqlite:<file>`, `postgres:<connection>`
    /// or `none`. Without it, an SQLite file beside the configuration's
    /// `state_retention_file`.
    pub store: Option<String>,
    /// A `retention.dat` to take the state from when the store is empty.
    /// Without it, the configuration's `state_retention_file`, if there
    /// is one.
    pub import: Option<PathBuf>,
    /// The Livestatus socket. Without it, the socket of a Livestatus
    /// `broker_module` line of the configuration, if there is one.
    pub livestatus: Option<PathBuf>,
    /// Shadow mode: the Livestatus socket of the production engine.
    pub shadow: Option<PathBuf>,
    /// Failover, with a PostgreSQL store.
    pub failover: Option<Failover>,
    /// Where the HTTP API listens, if anywhere.
    pub http: Option<String>,
    /// Where performance data are sent straight to.
    pub perfdata_graphite: Option<String>,
    pub perfdata_influx: Option<String>,
}

/// The options of failover. See `failover.rs`.
#[derive(Clone, Debug, Default)]
pub struct Failover {
    pub node: String,
    pub lease: f64,
    pub peer_listen: String,
    pub peer: Option<String>,
    pub livestatus_tcp: String,
    pub advertise: String,
    pub hold: bool,
}

/// The status checkd ends with when it lost the lease: whatever runs it
/// should start it again, and it comes back as a standby.
const LOST_LEASE: u8 = 3;

pub fn run(mut args: RunArgs) -> ExitCode {
    let loaded = load(
        &args.main,
        &Options {
            path_map: args.path_map.clone(),
        },
    );
    for error in &loaded.diags.errors {
        eprintln!("error: {error}");
    }
    let (Some(main), Some(objects)) = (loaded.main, loaded.objects) else {
        eprintln!(
            "{} errors. This configuration cannot be used.",
            loaded.diags.errors.len()
        );
        return ExitCode::from(1);
    };
    // A hash of the resolved objects, for failover to tell whether both
    // nodes run the same configuration.
    let config_hash = {
        let text = checkd_config::dump::dump(&objects);
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text.bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        format!("{hash:016x}")
    };

    let commands_on = main.get("check_external_commands").is_none_or(|value| value.trim() != "0");
    args.command_file = main
        .get("command_file")
        .filter(|path| commands_on && !path.trim().is_empty())
        .map(|path| args.path_map.apply(Path::new(path.trim())));
    // In shadow mode the configuration's files are production's: checkd
    // leaves its command file, its Livestatus socket and its log alone.
    if args.shadow.is_some() {
        args.command_file = None;
    }
    let config_log = if args.shadow.is_some() { None } else { main.get("log_file").map(PathBuf::from) };
    let log_path = args
        .log_file
        .clone()
        .or(config_log)
        .filter(|path| path != Path::new("-"));
    let mut log = match Log::open(log_path.as_deref()) {
        Ok(log) => log,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    // SAFETY: nothing else is running yet that could read the time zone.
    unsafe { tzset() };
    if let Some(at) = args.start_at {
        AHEAD.store(at - now(), Ordering::Relaxed);
    }
    if args.livestatus.is_none() && args.shadow.is_none() {
        args.livestatus = main
            .settings
            .iter()
            .filter(|(name, _)| name == "broker_module")
            .find_map(|(_, value)| crate::livestatus::socket_of_broker_module(value))
            .map(|path| args.path_map.apply(&path));
    }
    for (name, value) in &main.settings {
        if name == "broker_module" && crate::livestatus::socket_of_broker_module(value).is_none() {
            eprintln!("warning: broker_module={value} is not loaded: checkd has no event broker");
        }
    }
    let retention = main
        .get("state_retention_file")
        .filter(|path| !path.trim().is_empty())
        .map(|path| args.path_map.apply(Path::new(path.trim())));
    let keep_state = main.get("retain_state_information").is_none_or(|v| v.trim() != "0");
    let (mut engine, warnings) = Engine::new(objects, &main, Box::new(Local), now());
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    if args.shadow.is_some() {
        // Beside production: nothing is sent and nothing is acted on.
        engine.quiet();
        if args.store.is_none() {
            args.store = Some("none".to_string());
        }
    }
    let spec = match (&args.store, &retention, keep_state) {
        (Some(spec), _, _) => spec.clone(),
        (None, Some(file), true) => {
            let dir = file.parent().unwrap_or(Path::new("."));
            format!("sqlite:{}", dir.join("checkd.sqlite").display())
        }
        _ => "none".to_string(),
    };
    let mut store = match open_store(&spec) {
        Ok(store) => store,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    if args.failover.is_some() && !spec.starts_with("postgres:") {
        eprintln!("--failover needs a PostgreSQL store: --store postgres:<connection>");
        return ExitCode::from(2);
    }
    // A node in failover reads the state when it takes the lease.
    if let Some(store) = store.as_mut().filter(|_| args.failover.is_none()) {
        match restore(&mut engine, store, args.import.as_deref().or(retention.as_deref())) {
            Ok(lines) => {
                for line in lines {
                    log.line(&line);
                }
            }
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(2);
            }
        }
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("cannot start: {err}");
            return ExitCode::from(2);
        }
    };
    let hash = config_hash;
    match runtime.block_on(serve(&mut engine, &mut log, &args, store.as_mut(), hash)) {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::from(LOST_LEASE),
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

/// Runs the engine until a signal says stop or the time is up.
async fn serve(
    engine: &mut Engine,
    log: &mut Log,
    args: &RunArgs,
    mut store: Option<&mut Store>,
    config_hash: String,
) -> Result<bool, String> {
    let seconds = args.seconds;
    let signals = |kind| signal(kind).map_err(|err| format!("cannot wait for signals: {err}"));
    let mut terminate = signals(SignalKind::terminate())?;
    let mut interrupt = signals(SignalKind::interrupt())?;
    let over = async {
        match seconds {
            Some(seconds) => tokio::time::sleep(Duration::from_secs(seconds)).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(over);

    let mut telemetry = Telemetry::start(args.otlp_endpoint.as_deref())?;
    if let Some(line) = telemetry.describe() {
        eprintln!("{line}");
    }

    let (done, mut results) = mpsc::unbounded_channel::<(u64, JobResult)>();
    let (lines, mut commands) = mpsc::unbounded_channel::<String>();
    if let Some(path) = &args.command_file {
        command_file(path, lines)?;
    }
    let spool = engine.settings().check_result_path.clone().map(PathBuf::from);

    // A node in failover waits as a standby until it holds the lease.
    #[cfg(feature = "postgres")]
    let mut lease_events = None;
    #[cfg(feature = "postgres")]
    let mut node: Option<(String, bool)> = None;
    #[cfg(not(feature = "postgres"))]
    let _ = &config_hash;
    if let Some(failover) = &args.failover {
        #[cfg(not(feature = "postgres"))]
        {
            let _ = failover;
            return Err("--failover: this checkd was built without the PostgreSQL store (the postgres feature)".into());
        }
        #[cfg(feature = "postgres")]
        {
            let conninfo = args.store.as_deref().and_then(|s| s.strip_prefix("postgres:")).unwrap_or("").to_string();
            let setup = crate::failover::Setup {
                node: failover.node.clone(),
                conninfo,
                lease: failover.lease,
                peer_listen: failover.peer_listen.clone(),
                peer: failover.peer.clone(),
                livestatus_tcp: failover.livestatus_tcp.clone(),
                advertise: failover.advertise.clone(),
                hold: failover.hold,
                config_hash: config_hash.clone(),
            };
            let role = std::sync::Arc::new(std::sync::Mutex::new(crate::failover::Role::Standby));
            let mut stop = Signals {
                terminate: &mut terminate,
                interrupt: &mut interrupt,
            };
            let waited = crate::failover::standby(
                &setup,
                role,
                args.livestatus.as_deref(),
                &mut commands,
                spool.as_deref(),
                &mut |line: &str| log.line(line),
                &mut stop,
            )
            .await?;
            let Some((counter, events)) = waited else {
                log.line("Caught a signal while a standby, shutting down...");
                return Ok(false);
            };
            lease_events = Some(events);
            node = Some((setup.node.clone(), setup.hold));
            if let Some(store) = store.as_deref_mut() {
                store.set_fence(&setup.node, counter);
                for line in restore(engine, store, None)? {
                    log.line(&line);
                }
            }
        }
    }

    let tcp = args.failover.as_ref().map(|f| f.livestatus_tcp.clone());
    let mut livestatus = if args.livestatus.is_some() || tcp.is_some() {
        Some(crate::livestatus::listen(args.livestatus.as_deref(), tcp.as_deref())?)
    } else {
        None
    };
    let mut program = checkd_livestatus::Program {
        pid: std::process::id(),
        // The versions of the reference that checkd answers like, with its
        // own in them. Thruk chooses what it asks for by these: a
        // Livestatus version that ends in `-naemon` has Naemon's columns
        // and its wrapped_json output, as checkd's has.
        version: format!("1.5.2 (checkd {})", env!("CARGO_PKG_VERSION")),
        livestatus_version: format!("1.5.1-checkd-{}-naemon", env!("CARGO_PKG_VERSION")),
        ..Default::default()
    };
    // The log, for the Livestatus log table.
    let mut history: Vec<(i64, String)> = Vec::new();
    let mut perfdata = Perfdata::open(engine.settings());
    // The check result directory, read on the reaper's interval.
    let mut reaper = tokio::time::interval(Duration::from_secs(
        engine.settings().check_result_reaper_frequency as u64,
    ));
    reaper.tick().await;
    let mut shadow = crate::shadow::Shadow::default();
    let mut shadow_tick = tokio::time::interval(crate::shadow::EVERY);
    let (shadow_done, mut shadow_answers) = mpsc::unbounded_channel();
    let mut lost = false;
    let mut http = match &args.http {
        Some(addr) => Some(crate::http::listen(addr).await?),
        None => None,
    };
    let mut counts = Counts::default();
    // Performance data straight to a metrics store.
    let (notes_out, mut notes) = mpsc::unbounded_channel::<String>();
    let mut targets = Vec::new();
    if let Some(addr) = &args.perfdata_graphite {
        targets.push(crate::perfexport::Target::graphite(addr));
    }
    if let Some(url) = &args.perfdata_influx {
        targets.push(crate::perfexport::Target::influx(url)?);
    }
    let samples = (!targets.is_empty()).then(|| crate::perfexport::start(targets, notes_out));
    engine.export_perfdata(samples.is_some());
    #[cfg_attr(not(feature = "postgres"), allow(unused_mut))]
    let mut held_notifications: Option<bool> = None;
    // The process group of every command that is running.
    let mut running: BTreeMap<u64, i32> = BTreeMap::new();
    let mut effects = Vec::new();
    engine.start(&mut effects);
    // The engine has now planned every first check. The file moves them.
    if let Some(path) = &args.first_checks {
        first_checks(engine, path)?;
    }

    let pid = std::process::id();
    let starting = format!("checkd {} starting... (PID={pid})", env!("CARGO_PKG_VERSION"));
    log.line(&starting);
    if livestatus.is_some() {
        history.push((now().div_euclid(1000), starting));
    }

    // Log lines not yet in the store, and when the store is next written
    // anyway.
    let mut unsaved: Vec<(i64, String)> = Vec::new();
    let mut store_failing = false;
    let mut next_save = now() + SAVE_EVERY;
    if let Some(store) = store.as_deref_mut() {
        lost |= save(engine, store, &mut unsaved, log, &mut store_failing);
    }

    let why = loop {
        if lost {
            break "Another node holds the lease now";
        }
        let batch: Vec<Effect> = effects.drain(..).collect();
        let mut jobs = Vec::new();
        for effect in batch {
            match effect {
                Effect::Log(line) => {
                    log.line(&line);
                    let second = now().div_euclid(1000);
                    if livestatus.is_some() {
                        history.push((second, line.clone()));
                        if history.len() > HISTORY {
                            history.drain(..HISTORY / 10);
                        }
                    }
                    if store.is_some() {
                        unsaved.push((second, line));
                    }
                }
                Effect::Run(job) => jobs.push(job),
                Effect::Perfdata { host, line } => perfdata.write(host, &line),
                Effect::Sample { host, service, time, perfdata } => {
                    if let Some(samples) = &samples {
                        let _ = samples.send(crate::perfexport::Sample { host, service, time, perfdata });
                    }
                }
            }
        }
        // What happened is on record before anything it asks for is run.
        // A store that fails is tried again on the timer only, so that it
        // does not hold up every event.
        if !unsaved.is_empty() && !store_failing {
            if let Some(store) = store.as_deref_mut() {
                lost |= save(engine, store, &mut unsaved, log, &mut store_failing);
                if lost {
                    // Fenced: nothing of this batch is run.
                    continue;
                }
                next_save = now() + SAVE_EVERY;
            }
        }
        for job in jobs {
            if job.kind == JobKind::Notification {
                counts.notifications += 1;
            }
            let id = job.id;
            if otel::BUILT_IN {
                let latency = match job.kind {
                    JobKind::HostCheck | JobKind::ServiceCheck => engine.item(job.obj).latency,
                    JobKind::HostHandler
                    | JobKind::ServiceHandler
                    | JobKind::Notification
                    | JobKind::PerfdataProcessing => 0.0,
                };
                telemetry.job_started(&job, &engine.log_name(job.obj), latency);
            }
            if let Some(group) = start(job, done.clone()) {
                running.insert(id, group);
            }
        }
        let wait = match engine.next_wakeup() {
            Some(at) => Duration::from_millis((at - now()).max(0) as u64),
            None => Duration::from_secs(3600),
        };
        let flush = telemetry.next_flush();
        let save_in = Duration::from_millis((next_save - now()).max(0) as u64);
        tokio::select! {
            _ = tokio::time::sleep(save_in), if store.is_some() => {
                if let Some(store) = store.as_deref_mut() {
                    lost |= save(engine, store, &mut unsaved, log, &mut store_failing);
                }
                next_save = now() + SAVE_EVERY;
            }
            _ = tokio::time::sleep(wait) => engine.advance(now(), &mut effects),
            Some((id, result)) = results.recv() => {
                running.remove(&id);
                let read = otel::nanos();
                let outcome = result.outcome.clone();
                engine.job_done(now(), id, result, &mut effects);
                telemetry.job_done(id, &outcome, read);
            }
            Some(note) = notes.recv() => log.line(&note),
            Some(line) = commands.recv() => {
                counts.commands += 1;
                engine.command(&line, now(), &mut effects);
            }
            Some(ask) = async { http.as_mut()?.recv().await }, if http.is_some() => {
                let reply = answer_http(engine, &ask, &history, &program, &counts, running.len(), store_failing);
                if ask.method == "POST" && ask.path == "/api/v1/command" && ask.authorized {
                    counts.commands += 1;
                    let line = format!("[{}] {}", now().div_euclid(1000), ask.body.trim());
                    engine.command(&line, now(), &mut effects);
                }
                let _ = ask.reply.send(reply);
            }
            _ = reaper.tick(), if spool.is_some() => {
                if let Some(dir) = &spool {
                    read_spool(engine, dir, &mut effects);
                }
            }
            _ = shadow_tick.tick(), if args.shadow.is_some() => {
                let socket = args.shadow.clone().unwrap_or_default();
                let done = shadow_done.clone();
                tokio::spawn(async move {
                    let _ = done.send(crate::shadow::production(socket).await);
                });
            }
            Some(theirs) = shadow_answers.recv() => {
                for line in shadow.compare(engine, theirs, now()) {
                    effects.push(Effect::Log(line));
                }
            }
            Some(ask) = async { livestatus.as_mut()?.recv().await }, if livestatus.is_some() => {
                program.requests += 1;
                program.connections += 1;
                let response = {
                    let source = checkd_livestatus::Source {
                        engine,
                        now: now(),
                        log: &history,
                        program: &program,
                        window: None,
                    };
                    checkd_livestatus::handle(&source, &ask.text)
                };
                if let Some(command) = &response.command {
                    counts.commands += 1;
                    engine.command(command, now(), &mut effects);
                }
                let _ = ask.reply.send(response);
            }
            _ = tokio::time::sleep(flush.unwrap_or_default()), if flush.is_some() => telemetry.flush(),
            Some(event) = lease_event(
                #[cfg(feature = "postgres")]
                &mut lease_events,
            ) => {
                #[cfg(feature = "postgres")]
                {
                    use crate::failover::Event;
                    let hold = node.as_ref().is_some_and(|(_, hold)| *hold);
                    match event {
                        Event::Lost(reason) => {
                            log.line(&format!("Leadership lost: {reason}"));
                            lost = true;
                        }
                        Event::Isolated => {
                            if hold {
                                held_notifications = Some(engine.settings().enable_notifications);
                                engine.set_notifications(false);
                                log.line("Neither the database nor the other node answers: holding notifications back");
                            } else {
                                log.line("Neither the database nor the other node answers: going on, and notifying");
                            }
                        }
                        Event::Reached => {
                            if let Some(was) = held_notifications.take() {
                                engine.set_notifications(was);
                            }
                            log.line("The database answers again");
                        }
                        Event::Note(note) => log.line(&note),
                        _ => {}
                    }
                }
                #[cfg(not(feature = "postgres"))]
                let _ = event;
            }
            _ = terminate.recv() => break "Caught SIGTERM",
            _ = interrupt.recv() => break "Caught SIGINT",
            _ = &mut over => break "The time given with --for is up",
        }
    };

    log.line(&format!("{why}, shutting down..."));
    for group in running.values() {
        stop_group(*group);
    }
    if let Some(store) = store.as_deref_mut().filter(|_| !lost) {
        lost |= save(engine, store, &mut unsaved, log, &mut store_failing);
        if !store_failing && !lost {
            log.line(&format!("State saved to {}", store.describe()));
        }
    }
    telemetry.shutdown().await;
    let _ = &held_notifications;
    log.line(&format!("Successfully shutdown... (PID={pid})"));
    Ok(lost)
}

/// The next event of the lease thread, or never without failover.
async fn lease_event(
    #[cfg(feature = "postgres")] events: &mut Option<mpsc::UnboundedReceiver<crate::failover::Event>>,
) -> Option<LeaseEvent> {
    #[cfg(feature = "postgres")]
    if let Some(events) = events.as_mut() {
        return events.recv().await;
    }
    std::future::pending().await
}

#[cfg(feature = "postgres")]
type LeaseEvent = crate::failover::Event;
#[cfg(not(feature = "postgres"))]
type LeaseEvent = ();

/// The signals that stop a standby.
#[cfg(feature = "postgres")]
struct Signals<'a> {
    terminate: &'a mut tokio::signal::unix::Signal,
    interrupt: &'a mut tokio::signal::unix::Signal,
}

#[cfg(feature = "postgres")]
impl crate::failover::futures_like::Stop for Signals<'_> {
    fn wait(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(async move {
            tokio::select! {
                _ = self.terminate.recv() => {}
                _ = self.interrupt.recv() => {}
            }
        })
    }
}

/// What the driver counts, for `/metrics`.
#[derive(Default)]
struct Counts {
    commands: u64,
    notifications: u64,
}

/// Answers a request of the HTTP API.
fn answer_http(
    engine: &Engine,
    ask: &crate::http::Ask,
    history: &[(i64, String)],
    program: &checkd_livestatus::Program,
    counts: &Counts,
    running: usize,
    store_failing: bool,
) -> crate::http::Reply {
    use crate::http::Reply;
    let source = checkd_livestatus::Source {
        engine,
        now: now(),
        log: history,
        program,
        window: None,
    };
    match (ask.method.as_str(), ask.path.as_str()) {
        ("GET", "/metrics") => Reply::text(200, metrics(engine, program, counts, running, store_failing)),
        ("POST", "/api/v1/command") => {
            if ask.authorized {
                Reply::json(200, "{\"accepted\":true}\n".to_string())
            } else {
                Reply::text(403, "commands need a token: set CHECKD_HTTP_TOKEN\n")
            }
        }
        ("GET", path) if path.starts_with("/api/v1/") => {
            let table = &path["/api/v1/".len()..];
            if !checkd_livestatus::tables::TABLES.contains(&table) {
                return Reply::text(404, format!("no table '{table}'\n"));
            }
            let request = crate::http::livestatus_request(table, &ask.query);
            let response = checkd_livestatus::handle(&source, &request);
            let text = String::from_utf8_lossy(&response.bytes).into_owned();
            match crate::http::as_objects(&text) {
                Some(json) => Reply::json(200, json),
                None => Reply::text(400, text),
            }
        }
        (_, "/metrics") | (_, "/api/v1/command") => Reply::text(405, "method not allowed\n"),
        _ => Reply::text(404, "not found\n"),
    }
}

/// The engine's health, in Prometheus' text format.
fn metrics(
    engine: &Engine,
    program: &checkd_livestatus::Program,
    counts: &Counts,
    running: usize,
    store_failing: bool,
) -> String {
    let (host_checks, service_checks) = engine.checks_run();
    let mut out = String::new();
    let mut metric = |name: &str, kind: &str, help: &str, lines: Vec<(String, f64)>| {
        out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
        for (labels, value) in lines {
            out.push_str(&format!("{name}{labels} {value}\n"));
        }
    };
    metric(
        "checkd_checks_total",
        "counter",
        "Checks started.",
        vec![("{kind=\"host\"}".into(), host_checks as f64), ("{kind=\"service\"}".into(), service_checks as f64)],
    );
    metric("checkd_commands_running", "gauge", "Commands running now.", vec![(String::new(), running as f64)]);
    metric(
        "checkd_checks_waiting",
        "gauge",
        "Checks planned and not yet started.",
        vec![(String::new(), engine.planned_checks() as f64)],
    );
    let items: Vec<_> = engine.all_objects().into_iter().map(|o| engine.item(o)).filter(|i| i.checked).collect();
    let latency = if items.is_empty() { 0.0 } else { items.iter().map(|i| i.latency).sum::<f64>() / items.len() as f64 };
    metric(
        "checkd_check_latency_seconds",
        "gauge",
        "The average latency of the last check of each host and service.",
        vec![(String::new(), latency)],
    );
    metric("checkd_notifications_total", "counter", "Notification commands started.", vec![(String::new(), counts.notifications as f64)]);
    metric("checkd_external_commands_total", "counter", "External commands taken.", vec![(String::new(), counts.commands as f64)]);
    metric(
        "checkd_livestatus_requests_total",
        "counter",
        "Livestatus requests answered.",
        vec![(String::new(), program.requests as f64)],
    );
    let mut states: Vec<(String, f64)> = Vec::new();
    for (kind, names) in [("host", &checkd_core::engine::HOST_STATES[..]), ("service", &checkd_core::engine::SERVICE_STATES[..])] {
        for (state, name) in names.iter().enumerate() {
            let n = engine
                .all_objects()
                .into_iter()
                .filter(|o| matches!(o, checkd_core::engine::Obj::Host(_)) == (kind == "host"))
                .filter(|o| engine.item(*o).checked && engine.item(*o).state as usize == state)
                .count();
            states.push((format!("{{kind=\"{kind}\",state=\"{name}\"}}"), n as f64));
        }
    }
    metric("checkd_objects", "gauge", "Hosts and services by their current state.", states);
    metric("checkd_store_failing", "gauge", "1 while the store cannot be written.", vec![(String::new(), store_failing as u8 as f64)]);
    out
}

/// The performance data files.
struct Perfdata {
    files: [Option<std::fs::File>; 2],
}

impl Perfdata {
    fn open(settings: &checkd_core::settings::Settings) -> Perfdata {
        let open = |place: usize| {
            let p = &settings.perfdata[place];
            let path = p.file.as_ref()?;
            let mut options = OpenOptions::new();
            options.create(true);
            if p.mode == 'w' {
                options.write(true).truncate(true);
            } else {
                options.append(true);
            }
            match options.open(path) {
                Ok(file) => Some(file),
                Err(err) => {
                    eprintln!("warning: cannot open the performance data file {path}: {err}");
                    None
                }
            }
        };
        Perfdata { files: [open(0), open(1)] }
    }

    fn write(&mut self, host: bool, line: &str) {
        if let Some(file) = &mut self.files[(!host) as usize] {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// Takes the files NRDP and the like left in the check result directory:
/// each with an `.ok` file beside it once it is written whole. Both are
/// removed when the result is taken.
fn read_spool(engine: &mut Engine, dir: &Path, effects: &mut Vec<Effect>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut names: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_none() && p.with_extension("ok").exists())
        .collect();
    names.sort();
    for path in names {
        if let Ok(text) = std::fs::read_to_string(&path) {
            engine.check_result_file(&text, now(), effects);
        }
        let _ = std::fs::remove_file(path.with_extension("ok"));
        let _ = std::fs::remove_file(&path);
    }
}

/// How many log lines the Livestatus log table keeps in memory.
const HISTORY: usize = 200_000;

/// How often the state is written when nothing in the log calls for it.
const SAVE_EVERY: Ms = 5000;

/// Opens the store a `--store` value names.
fn open_store(spec: &str) -> Result<Option<Store>, String> {
    if spec == "none" {
        return Ok(None);
    }
    if let Some(path) = spec.strip_prefix("sqlite:") {
        return Store::new(Box::new(Sqlite::open(path)?)).map(Some);
    }
    if let Some(conninfo) = spec.strip_prefix("postgres:") {
        return crate::postgres::open(conninfo).map(Some);
    }
    Err(format!("--store: '{spec}' is not sqlite:<file>, postgres:<connection> or none"))
}

/// `checkd state`: prints what a store holds, one line per thing.
pub fn show_state(spec: &str) -> ExitCode {
    let mut store = match open_store(spec) {
        Ok(Some(store)) => store,
        Ok(None) => {
            eprintln!("state needs a store, not none");
            return ExitCode::from(2);
        }
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let snapshot = match store.load() {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            println!("{}: no state saved yet", store.describe());
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    println!("notifications {}", if snapshot.notifications_enabled { "on" } else { "off" });
    for o in &snapshot.objects {
        let name = match &o.service {
            Some(service) => format!("{};{service}", o.host),
            None => o.host.clone(),
        };
        let state = checkd_core::notify::state_name(
            if o.service.is_some() {
                checkd_core::engine::Obj::Service(0)
            } else {
                checkd_core::engine::Obj::Host(0)
            },
            o.state,
        );
        let mut line = format!(
            "object {name} {state} {} attempt={} notified={} notifications={}",
            if o.hard { "HARD" } else { "SOFT" },
            o.attempt,
            o.notification_number,
            if o.notifications_enabled { "on" } else { "off" },
        );
        if let Some(ack) = &o.ack {
            line.push_str(&format!(
                " ack={}:{}{}",
                ack.author,
                ack.comment,
                if ack.sticky { ":sticky" } else { "" }
            ));
        }
        println!("{line} output={}", o.output);
    }
    for c in &snapshot.comments {
        let name = c.service.as_ref().map_or(c.host.clone(), |s| format!("{};{s}", c.host));
        println!(
            "comment {} {name} type={} persistent={} {}: {}",
            c.id, c.entry_type, c.persistent as u8, c.author, c.text
        );
    }
    for d in &snapshot.downtimes {
        let name = d.service.as_ref().map_or(d.host.clone(), |s| format!("{};{s}", d.host));
        println!(
            "downtime {} {name} {}-{} {} active={} {}: {}",
            d.id,
            d.start,
            d.end,
            if d.fixed { "fixed" } else { "flexible" },
            d.active as u8,
            d.author,
            d.comment
        );
    }
    ExitCode::SUCCESS
}

/// Puts the saved state back into the engine: from the store, or, when it
/// holds none, from a retention file. The lines are for the log.
pub(crate) fn restore(engine: &mut Engine, store: &mut Store, import: Option<&Path>) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let (snapshot, from) = match store.load()? {
        Some(snapshot) => (snapshot, store.describe()),
        None => match import.filter(|path| path.exists()) {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
                let (snapshot, notes) = checkd_core::retention::read(&text);
                for note in notes {
                    lines.push(format!("Retention file {}: {note}", path.display()));
                }
                (snapshot, format!("the retention file {}", path.display()))
            }
            None => return Ok(lines),
        },
    };
    let dropped = engine.restore(&snapshot);
    lines.push(format!(
        "Read the state of {} hosts and services, {} comments and {} downtimes from {from}",
        snapshot.objects.len() - dropped.min(snapshot.objects.len()),
        snapshot.comments.len(),
        snapshot.downtimes.len()
    ));
    if dropped > 0 {
        lines.push(format!("{dropped} saved entries name objects that are not in the configuration and were dropped"));
    }
    Ok(lines)
}

/// Writes the engine's state and the log lines not yet saved. A store that
/// fails is said so in the log once, until it works again.
/// The answer says whether the write was fenced off: another node holds
/// the lease.
fn save(engine: &Engine, store: &mut Store, unsaved: &mut Vec<(i64, String)>, log: &mut Log, failing: &mut bool) -> bool {
    match store.write(&engine.snapshot(), unsaved, now().div_euclid(1000)) {
        Err(message) if message == "fenced" => {
            log.line("A write was refused: another node holds the lease now");
            true
        }
        Ok(()) => {
            if *failing {
                log.line(&format!("The store {} can be written again", store.describe()));
            }
            *failing = false;
            unsaved.clear();
            false
        }
        Err(message) => {
            if !*failing {
                log.line(&format!("Warning: the store cannot be written: {message}"));
            }
            *failing = true;
            // Lines wait for the next try, up to a limit.
            if unsaved.len() > 100_000 {
                let extra = unsaved.len() - 100_000;
                unsaved.drain(..extra);
            }
            false
        }
    }
}

/// Makes the command file if it is not there, and starts the thread that
/// reads it.
fn command_file(path: &Path, lines: mpsc::UnboundedSender<String>) -> Result<(), String> {
    use std::io::BufRead;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;

    match std::fs::metadata(path) {
        Ok(meta) if meta.file_type().is_fifo() => {}
        Ok(_) => {
            return Err(format!(
                "the command file {} is there and is not a named pipe",
                path.display()
            ));
        }
        Err(_) => {
            let name = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| format!("bad command file name {}", path.display()))?;
            // SAFETY: a plain system call with a string that lives
            // through it.
            if unsafe { libc::mkfifo(name.as_ptr(), 0o660) } != 0 {
                return Err(format!(
                    "cannot make the command file {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
    // Read and write: the pipe then never sees its last writer go.
    let pipe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|err| format!("cannot open the command file {}: {err}", path.display()))?;
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(pipe);
        for line in reader.split(b'\n') {
            let Ok(line) = line else { return };
            let line = String::from_utf8_lossy(&line).into_owned();
            if !line.trim().is_empty() && lines.send(line).is_err() {
                return;
            }
        }
    });
    Ok(())
}

/// Starts a command. Its result comes back on `done`. The answer is the
/// command's process group, when it could be started.
fn start(job: Job, done: mpsc::UnboundedSender<(u64, JobResult)>) -> Option<i32> {
    let (program, mut command) = match cmdline::plan(&job.line) {
        Start::Direct { env, argv } => {
            let program = argv.first().cloned().unwrap_or_default();
            let mut command = Command::new(&program);
            command.args(argv.iter().skip(1)).envs(env);
            (program, command)
        }
        Start::Shell => {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(&job.line);
            ("/bin/sh".to_string(), command)
        }
    };
    // A group of its own, so that the command can be stopped together
    // with everything it started.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            // The reference tries to start the program in a process of
            // its own, which says this and ends with the error's number.
            let errno = err.raw_os_error().unwrap_or(libc::ENOENT);
            let result = JobResult {
                outcome: Outcome::Exited(errno),
                stdout: String::new(),
                stderr: format!(
                    "execvp({program}, ...) failed. errno is {errno}: {}",
                    error_text(errno)
                ),
            };
            let _ = done.send((job.id, result));
            return None;
        }
    };
    let group = child.id().map(|pid| pid as i32);
    tokio::spawn(async move {
        let result = finish(child, group, job.timeout).await;
        let _ = done.send((job.id, result));
    });
    group
}

/// Waits for a command to end, or stops it at its timeout.
async fn finish(mut child: Child, group: Option<i32>, timeout: Ms) -> JobResult {
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let work = async {
        let mut ended = None;
        {
            let read = async {
                tokio::join!(keep(&mut out, &mut stdout), keep(&mut err, &mut stderr));
            };
            tokio::pin!(read);
            tokio::select! {
                _ = &mut read => {}
                status = child.wait() => {
                    ended = Some(status);
                    let _ = tokio::time::timeout(GRACE, &mut read).await;
                }
            }
        }
        match ended {
            Some(status) => status,
            None => child.wait().await,
        }
    };
    let limit = Duration::from_millis(timeout.max(0) as u64);
    let outcome = match tokio::time::timeout(limit, work).await {
        Ok(Ok(status)) => match (status.code(), status.signal()) {
            (Some(code), _) => Outcome::Exited(code),
            (None, Some(signal)) => Outcome::Signaled(signal),
            (None, None) => Outcome::Failed("ended in a way that is not understood".to_string()),
        },
        Ok(Err(err)) => Outcome::Failed(err.to_string()),
        Err(_) => {
            if let Some(group) = group {
                stop_group(group);
            }
            let _ = child.wait().await;
            Outcome::TimedOut
        }
    };
    JobResult {
        outcome,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}

/// Reads a pipe to its end and keeps the first `KEEP` bytes.
async fn keep<R: AsyncRead + Unpin>(pipe: &mut Option<R>, into: &mut Vec<u8>) {
    let Some(pipe) = pipe else { return };
    let mut buffer = [0u8; 8192];
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let room = KEEP.saturating_sub(into.len());
                into.extend_from_slice(&buffer[..n.min(room)]);
            }
        }
    }
}

/// Ends every process of a group at once.
fn stop_group(group: i32) {
    if group > 1 {
        // SAFETY: a plain system call. A group that is gone is an error
        // that does not matter here.
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
}

/// The system's words for an error number.
fn error_text(errno: i32) -> String {
    let text = std::io::Error::from_raw_os_error(errno).to_string();
    match text.rfind(" (os error ") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// How far the time checkd goes by is ahead of the system's clock, in
/// milliseconds. Not 0 only in a test run that was given `--start-at`.
static AHEAD: AtomicI64 = AtomicI64::new(0);

/// Milliseconds since the epoch.
fn now() -> Ms {
    let system = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as Ms)
        .unwrap_or(0);
    system + AHEAD.load(Ordering::Relaxed)
}

/// The log: one line per event, with the second it was written in front.
struct Log {
    out: Box<dyn Write>,
}

impl Log {
    fn open(path: Option<&Path>) -> Result<Log, String> {
        let out: Box<dyn Write> = match path {
            None => Box::new(std::io::stdout()),
            Some(path) => Box::new(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .map_err(|err| format!("cannot write the log {}: {err}", path.display()))?,
            ),
        };
        Ok(Log { out })
    }

    fn line(&mut self, text: &str) {
        let line = format!("[{}] {text}\n", now().div_euclid(1000));
        // A log that cannot be written must not stop the checks.
        let _ = self.out.write_all(line.as_bytes());
        let _ = self.out.flush();
    }
}

/// Reads the file given with `--first-checks`: on each line a moment in
/// milliseconds since the epoch, a blank, and `host` or
/// `host;service`. That object is first checked at that moment.
fn first_checks(engine: &mut Engine, path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let bad = || format!("{}:{}: not '<millisecond> <object>'", path.display(), n + 1);
        let (at, name) = line.split_once(' ').ok_or_else(bad)?;
        let at: Ms = at.parse().map_err(|_| bad())?;
        let obj = match name.split_once(';') {
            Some((host, description)) => engine.service(host, description),
            None => engine.host(name),
        };
        let obj = obj.ok_or_else(|| {
            format!("{}:{}: there is no '{name}'", path.display(), n + 1)
        })?;
        engine.set_first_check(obj, at);
    }
    Ok(())
}

unsafe extern "C" {
    fn tzset();
}

/// The time zone of the system, or of `TZ`.
struct Local;

impl Local {
    fn at(time: i64) -> Option<libc::tm> {
        let time = time as libc::time_t;
        // SAFETY: `tm` is plain numbers and a pointer, for which all
        // zeroes is a value, and both pointers are to live values.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            (!libc::localtime_r(&time, &mut tm).is_null()).then_some(tm)
        }
    }
}

impl Zone for Local {
    fn offset(&self, time: i64) -> i64 {
        Local::at(time).map(|tm| tm.tm_gmtoff as i64).unwrap_or(0)
    }

    fn name(&self, time: i64) -> String {
        match Local::at(time) {
            // SAFETY: the name is a string the C library keeps for as
            // long as the program runs.
            Some(tm) if !tm.tm_zone.is_null() => unsafe { CStr::from_ptr(tm.tm_zone) }
                .to_string_lossy()
                .into_owned(),
            _ => "UTC".to_string(),
        }
    }
}
