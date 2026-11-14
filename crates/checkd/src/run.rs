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

use std::ffi::CStr;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use checkd_config::maincfg::PathMap;
use checkd_config::{Options, load};
use checkd_core::engine::{Effect, Engine, JobKind, JobResult, Ms};
use checkd_core::snapshot::Snapshot;
use checkd_core::timeperiod::Zone;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use crate::otel::{self, Telemetry};
use crate::runner::{Running, start};
use crate::store::{Sqlite, Store};


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
    /// The remote workers' file.
    pub workers: Option<PathBuf>,
    /// Livestatus over TCP for any client, at this address (without
    /// failover; with it, the address is for the other node).
    pub livestatus_tcp: Option<String>,
    /// Where the state is kept while a PostgreSQL store cannot be
    /// written. Filled in by `run` when not given.
    pub journal: Option<PathBuf>,
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

/// How one run of a configuration ended.
enum Ended {
    Exit(ExitCode),
    /// The configuration is read again. With no store, the state goes
    /// along here.
    Reload(Box<Snapshot>),
}

/// How `serve` ended.
enum End {
    /// With the lease lost, or not.
    Stopped(bool),
    Reload,
}

/// The command file's lines, read by a thread that lives as long as the
/// program: a reload goes on reading the same pipe.
type Pipe = (mpsc::UnboundedSender<String>, mpsc::UnboundedReceiver<String>);

pub fn run(mut args: RunArgs) -> ExitCode {
    let mut pipe: Option<Pipe> = None;
    let mut carried = None;
    loop {
        match run_once(&mut args, &mut pipe, carried.take()) {
            Ended::Exit(code) => return code,
            Ended::Reload(snapshot) => carried = Some(snapshot),
        }
    }
}

/// Loads the configuration and runs it until it is stopped, or told to
/// read the configuration again.
fn run_once(args: &mut RunArgs, pipe: &mut Option<Pipe>, carried: Option<Box<Snapshot>>) -> Ended {
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
        return Ended::Exit(ExitCode::from(1));
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
            return Ended::Exit(ExitCode::from(2));
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
    // Loading the configuration frees much more than it keeps; the C
    // allocator holds on to that unless told: about a third of the memory
    // of a large configuration.
    // SAFETY: a plain call into the C library.
    unsafe {
        libc::malloc_trim(0);
    }
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
    // A PostgreSQL store has a journal on this machine for when it cannot
    // be written: beside the retention file unless given, else in the
    // state directory systemd gives. Not in failover, where only the
    // leader may write and a journal of a node that lost the lease would
    // be wrong to replay.
    if !spec.starts_with("postgres:") || args.failover.is_some() {
        args.journal = None;
    } else if args.journal.is_none() {
        args.journal = default_journal(retention.as_deref(), std::env::var_os("STATE_DIRECTORY").as_deref());
        if args.journal.is_none() {
            eprintln!(
                "Warning: no journal for the PostgreSQL store: while it cannot be written, what waits stays in memory and a restart loses it. Give --journal or a state_retention_file"
            );
        }
    }
    // With a journal on disk, checkd starts from it even while the
    // database does not answer, and connects when it does.
    let deferred = args.journal.as_deref().is_some_and(Path::exists);
    let mut store = match open_store_with(&spec, deferred) {
        Ok(store) => store,
        Err(message) => {
            eprintln!("{message}");
            return Ended::Exit(ExitCode::from(2));
        }
    };
    if args.failover.is_some() && !spec.starts_with("postgres:") {
        eprintln!("--failover needs a PostgreSQL store: --store postgres:<connection>");
        return Ended::Exit(ExitCode::from(2));
    }
    // A node in failover reads the state when it takes the lease.
    if let Some(store) = store.as_mut().filter(|_| args.failover.is_none()) {
        match restore(&mut engine, store, args.import.as_deref().or(retention.as_deref()), args.journal.as_deref()) {
            Ok(lines) => {
                for line in lines {
                    log.line(&line);
                }
            }
            Err(message) => {
                eprintln!("{message}");
                return Ended::Exit(ExitCode::from(2));
            }
        }
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        // Threads that start commands: enough to start thousands a
        // second, few enough to keep memory down.
        .max_blocking_threads(32)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("cannot start: {err}");
            return Ended::Exit(ExitCode::from(2));
        }
    };
    let hash = config_hash;
    // The command file is opened once, with the first configuration's
    // name for it.
    if pipe.is_none() {
        let (lines, commands) = mpsc::unbounded_channel::<String>();
        if let Some(path) = &args.command_file {
            if let Err(message) = command_file(path, lines.clone()) {
                eprintln!("{message}");
                return Ended::Exit(ExitCode::from(2));
            }
        }
        *pipe = Some((lines, commands));
    }
    let commands = &mut pipe.as_mut().expect("made above").1;
    // Without a store, the state of before a reload is the engine's own.
    if let (None, Some(snapshot)) = (&store, &carried) {
        engine.restore(snapshot);
    }
    match runtime.block_on(serve(&mut engine, &mut log, args, store.as_mut(), hash, commands)) {
        Ok(End::Stopped(false)) => Ended::Exit(ExitCode::SUCCESS),
        Ok(End::Stopped(true)) => Ended::Exit(ExitCode::from(LOST_LEASE)),
        Ok(End::Reload) => Ended::Reload(Box::new(engine.snapshot())),
        Err(message) => {
            eprintln!("{message}");
            Ended::Exit(ExitCode::from(2))
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
    commands: &mut mpsc::UnboundedReceiver<String>,
) -> Result<End, String> {
    let seconds = args.seconds;
    let signals = |kind| signal(kind).map_err(|err| format!("cannot wait for signals: {err}"));
    let mut terminate = signals(SignalKind::terminate())?;
    let mut interrupt = signals(SignalKind::interrupt())?;
    let mut hangup = signals(SignalKind::hangup())?;
    // Who asked for the configuration to be read again.
    let mut reload_asked: Option<&str> = None;
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
    let spool = engine.settings().check_result_path.clone().map(PathBuf::from);
    // Remote workers: the file is read now, so that a mistake in it shows
    // at once; a standby listens for workers only once it leads.
    #[cfg(feature = "workers")]
    let workers_config = match &args.workers {
        Some(path) => Some(crate::workers::read_config(path)?),
        None => None,
    };
    #[cfg(not(feature = "workers"))]
    if args.workers.is_some() {
        return Err("--workers: this checkd was built without remote workers (the workers feature)".into());
    }

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
                commands,
                spool.as_deref(),
                &mut |line: &str| log.line(line),
                &mut stop,
            )
            .await?;
            let Some((counter, events)) = waited else {
                log.line("Caught a signal while a standby, shutting down...");
                return Ok(End::Stopped(false));
            };
            lease_events = Some(events);
            node = Some((setup.node.clone(), setup.hold));
            if let Some(store) = store.as_deref_mut() {
                store.set_fence(&setup.node, counter);
                for line in restore(engine, store, None, None)? {
                    log.line(&line);
                }
            }
        }
    }

    let tcp = match (&args.failover, &args.livestatus_tcp) {
        (Some(failover), _) => Some(failover.livestatus_tcp.clone()),
        (None, Some(addr)) => {
            // Livestatus has no login and no encryption: anyone who reaches
            // the port reads everything and can send commands.
            let local = addr.parse::<std::net::SocketAddr>().is_ok_and(|a| a.ip().is_loopback());
            if !local && std::env::var("CHECKD_LIVESTATUS_ALLOW_NETWORK").as_deref() != Ok("1") {
                return Err(format!(
                    "--livestatus-tcp {addr}: Livestatus over TCP has no login and no encryption, and takes commands; \
                     listen on 127.0.0.1, or set CHECKD_LIVESTATUS_ALLOW_NETWORK=1 for a network you trust"
                ));
            }
            Some(addr.clone())
        }
        (None, None) => None,
    };
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
    // The log, for the Livestatus log table and the state history: what
    // the store holds of it, so that a restart loses none, and then what
    // is written from now on. The newest HISTORY lines are kept.
    let mut history: Vec<(i64, String)> = Vec::new();
    if livestatus.is_some() || args.http.is_some() {
        if let Some(store) = store.as_deref_mut() {
            match store.recent_log(HISTORY) {
                Ok(lines) => history = lines,
                Err(err) => log.line(&format!("Warning: the log could not be read from the store: {err}")),
            }
        }
    }
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
    #[cfg(feature = "workers")]
    let mut pools = match workers_config {
        Some(config) => {
            let events = crate::workers::listen(&config).await?;
            log.line(&format!("Listening for workers at {}", config.listen));
            Some((crate::workers::Pools::new(config), events))
        }
        None => None,
    };
    #[cfg(not(feature = "workers"))]
    let mut pools: Option<((), ())> = None;
    let mut worker_tick = tokio::time::interval(Duration::from_secs(1));
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
    let running = Running::default();
    let mut effects = Vec::new();
    // Livestatus requests that wait for something to happen, and what has
    // happened since they were last looked at.
    let (mut pushes, push_notes) = crate::push::Pushes::new(engine, now());
    for note in push_notes {
        log.line(&note);
    }
    if pushes.any() && http.is_none() {
        log.line("Warning: services have a _PUSH_TOKEN, and no pushes can come: checkd was started without --http");
    }
    let mut push_tick = tokio::time::interval(Duration::from_secs(5));
    let (recurring, recurring_notes) = crate::recurring::Recurring::new(engine);
    for note in recurring_notes {
        log.line(&note);
    }
    let mut recurring_tick = tokio::time::interval(Duration::from_secs(15));
    let (mut digests, digest_notes) = crate::digest::Digests::new(engine);
    for note in digest_notes {
        log.line(&note);
    }
    // Where the result of a digest's command goes: nowhere. The engine
    // knows nothing of that command.
    let (unheard, _) = mpsc::unbounded_channel();
    let mut parked: Vec<Parked> = Vec::new();
    let mut happened = 0u8;
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
        lost |= save(engine, store, &mut unsaved, log, &mut store_failing, args.journal.as_deref());
    }

    let why = loop {
        // The tasks that wait for running commands get their turn here,
        // even when the engine always has something due: on this one
        // thread they run only while the loop waits, and under load it
        // may never have to.
        tokio::task::yield_now().await;
        if lost {
            break "Another node holds the lease now";
        }
        if let Some(who) = reload_asked.take() {
            if args.failover.is_some() {
                log.line(&format!("{who}: a node in failover does not read its configuration again; restart it"));
            } else {
                // Only a configuration that can be used replaces the one
                // that runs.
                let options = Options { path_map: args.path_map.clone() };
                let loaded = load(&args.main, &options);
                if loaded.objects.is_some() && loaded.diags.errors.is_empty() {
                    log.line(&format!("{who}, restarting..."));
                    break RELOAD;
                }
                log.line(&format!(
                    "{who}: the configuration has {} errors and is not taken; the one that runs stays",
                    loaded.diags.errors.len()
                ));
                for error in loaded.diags.errors.iter().take(20) {
                    log.line(&format!("Error: {error}"));
                }
            }
        }
        let batch: Vec<Effect> = effects.drain(..).collect();
        let mut jobs = Vec::new();
        for effect in batch {
            match effect {
                Effect::Log(line) => {
                    happened |= LOGGED;
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
        // Those that waited for what just happened are answered, with the
        // state as it is after it; and those whose time is up.
        if !parked.is_empty() {
            let at = now();
            let (due, stay): (Vec<Parked>, Vec<Parked>) =
                parked.into_iter().partition(|p| p.until <= at || fires(p.trigger, happened));
            parked = stay;
            for p in due {
                let source = checkd_livestatus::Source { engine, now: at, log: &history, program: &program, window: None };
                let _ = p.ask.reply.send(checkd_livestatus::handle(&source, &p.ask.text));
            }
        }
        happened = 0;
        // What happened is on record before anything it asks for is run.
        // A store that fails is tried again on the timer only, so that it
        // does not hold up every event.
        if !unsaved.is_empty() && store_failing {
            // While the store fails, each batch goes to the journal, which
            // is on this machine and quick; the store is tried on the timer.
            if let Some(path) = args.journal.as_deref() {
                if journal_write(engine, path, &mut unsaved).is_err() {
                    // Said in the log by `save` when it first failed.
                }
            }
        }
        if !unsaved.is_empty() && !store_failing {
            if let Some(store) = store.as_deref_mut() {
                lost |= save(engine, store, &mut unsaved, log, &mut store_failing, args.journal.as_deref());
                if lost {
                    // Fenced: nothing of this batch is run.
                    continue;
                }
                next_save = now() + SAVE_EVERY;
            }
        }
        // Notifications held for a digest that are due: one alone joins
        // the jobs, several are done with and one command runs for them.
        for release in digests.due(now()) {
            match release {
                crate::digest::Release::Single(job) => jobs.push(job),
                crate::digest::Release::Together { done: ids, digest, log: line } => {
                    log.line(&line);
                    counts.notifications += 1;
                    for id in ids {
                        let result = JobResult { outcome: checkd_core::engine::Outcome::Exited(0), stdout: String::new(), stderr: String::new() };
                        let _ = done.send((id, result));
                    }
                    start(digest, unheard.clone(), &running);
                }
            }
        }
        for job in jobs {
            let Some(job) = digests.hold(job, now()) else { continue };
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
            #[cfg(feature = "workers")]
            if let Some((pools, _)) = pools.as_mut() {
                if let crate::workers::Route::Pool(pool) = crate::workers::route(engine, pools.config(), &job) {
                    for given in pools.submit(job, pool, now()) {
                        let _ = done.send((given.id, given.result));
                    }
                    continue;
                }
            }
            let _ = id;
            start(job, done.clone(), &running);
        }
        let wait = match engine.next_wakeup() {
            Some(at) => Duration::from_millis((at - now()).max(0) as u64),
            None => Duration::from_secs(3600),
        };
        let flush = telemetry.next_flush();
        let save_in = Duration::from_millis((next_save - now()).max(0) as u64);
        let digest_in = digests.next().map(|at| Duration::from_millis((at - now()).max(0) as u64));
        let park_in = parked.iter().map(|p| p.until).min().map(|at| Duration::from_millis((at - now()).max(0) as u64));
        tokio::select! {
            _ = tokio::time::sleep(park_in.unwrap_or_default()), if park_in.is_some() => {}
            _ = tokio::time::sleep(digest_in.unwrap_or_default()), if digest_in.is_some() => {}
            _ = tokio::time::sleep(save_in), if store.is_some() => {
                if let Some(store) = store.as_deref_mut() {
                    lost |= save(engine, store, &mut unsaved, log, &mut store_failing, args.journal.as_deref());
                }
                next_save = now() + SAVE_EVERY;
            }
            _ = tokio::time::sleep(wait) => engine.advance(now(), &mut effects),
            Some((id, result)) = results.recv() => {
                happened |= RESULT;
                let read = otel::nanos();
                let outcome = result.outcome.clone();
                engine.job_done(now(), id, result, &mut effects);
                telemetry.job_done(id, &outcome, read);
            }
            Some(note) = notes.recv() => log.line(&note),
            Some(line) = commands.recv() => {
                happened |= COMMANDED;
                counts.commands += 1;
                if restart_asked(&line) {
                    reload_asked = Some("RESTART_PROGRAM");
                } else {
                    engine.command(&line, now(), &mut effects);
                }
            }
            _ = recurring_tick.tick(), if recurring.any() => {
                for line in recurring.due(engine, now()) {
                    happened |= COMMANDED;
                    counts.commands += 1;
                    engine.command(&line, now(), &mut effects);
                }
            }
            _ = push_tick.tick(), if pushes.any_expected() => {
                for line in pushes.overdue(now()) {
                    happened |= COMMANDED;
                    counts.commands += 1;
                    engine.command(&line, now(), &mut effects);
                }
            }
            Some(ask) = async { http.as_mut()?.recv().await }, if http.is_some() => {
                if let Some(token) = ask.path.strip_prefix("/api/v1/push/") {
                    let param = |name: &str| ask.query.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
                    let output = param("output").unwrap_or(&ask.body);
                    let reply = match pushes.take(token, param("state").unwrap_or(""), output, now()) {
                        Ok(Some(line)) => {
                            happened |= COMMANDED;
                            counts.commands += 1;
                            engine.command(&line, now(), &mut effects);
                            crate::http::Reply::text(200, "ok\n")
                        }
                        Ok(None) => crate::http::Reply::text(404, "not found\n"),
                        Err(why) => crate::http::Reply::text(400, format!("{why}\n")),
                    };
                    let _ = ask.reply.send(reply);
                    continue;
                }
                #[cfg(feature = "workers")]
                let pool_stats = pools.as_ref().map(|(pools, _)| pools.stats(now())).unwrap_or_default();
                #[cfg(not(feature = "workers"))]
                let pool_stats = Vec::new();
                let reply = answer_http(engine, &ask, &history, &program, &counts, running.count(), store_failing, &pool_stats);
                if ask.method == "POST" && ask.path == "/api/v1/command" && ask.authorized {
                    happened |= COMMANDED;
                    counts.commands += 1;
                    let line = format!("[{}] {}", now().div_euclid(1000), ask.body.trim());
                    if restart_asked(&line) {
                        reload_asked = Some("RESTART_PROGRAM");
                    } else {
                        engine.command(&line, now(), &mut effects);
                    }
                }
                let _ = ask.reply.send(reply);
            }
            _ = reaper.tick(), if spool.is_some() => {
                if let Some(dir) = &spool {
                    if read_spool(engine, dir, &mut effects) > 0 {
                        happened |= RESULT;
                    }
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
                // One that waits is kept until what it waits for happens.
                // Beyond a number of them, a request is answered at once.
                if let Some(wait) = checkd_livestatus::wait_of(&ask.text).filter(|_| parked.len() < PARKED) {
                    let until = now() + wait.timeout_ms.min(LONGEST_WAIT) as Ms;
                    parked.push(Parked { ask, trigger: wait.trigger, until });
                    continue;
                }
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
                    happened |= COMMANDED;
                    counts.commands += 1;
                    if restart_asked(command) {
                        reload_asked = Some("RESTART_PROGRAM");
                    } else {
                        engine.command(command, now(), &mut effects);
                    }
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
            Some(event) = worker_event(&mut pools) => {
                #[cfg(feature = "workers")]
                if let Some((pools, _)) = pools.as_mut() {
                    let (given, notes) = pools.event(event, now());
                    for note in notes {
                        log.line(&note);
                    }
                    for given in given {
                        let _ = done.send((given.id, given.result));
                    }
                }
                #[cfg(not(feature = "workers"))]
                let _ = event;
            }
            _ = worker_tick.tick(), if pools.is_some() => {
                #[cfg(feature = "workers")]
                if let Some((pools, _)) = pools.as_mut() {
                    for given in pools.expire(now()) {
                        let _ = done.send((given.id, given.result));
                    }
                }
            }
            _ = hangup.recv() => reload_asked = Some("Caught SIGHUP"),
            _ = terminate.recv() => break "Caught SIGTERM",
            _ = interrupt.recv() => break "Caught SIGINT",
            _ = &mut over => break "The time given with --for is up",
        }
    };

    if digests.held() > 0 {
        log.line(&format!("Warning: {} notifications held for a digest are not sent: checkd ends before their wait is over", digests.held()));
    }
    // Nobody is left waiting: what waits is answered with what is known.
    for p in parked.drain(..) {
        let source = checkd_livestatus::Source { engine, now: now(), log: &history, program: &program, window: None };
        let _ = p.ask.reply.send(checkd_livestatus::handle(&source, &p.ask.text));
    }
    let reloading = why == RELOAD;
    if !reloading {
        log.line(&format!("{why}, shutting down..."));
    }
    running.stop_all();
    if let Some(store) = store.as_deref_mut().filter(|_| !lost) {
        lost |= save(engine, store, &mut unsaved, log, &mut store_failing, args.journal.as_deref());
        if !store_failing && !lost {
            log.line(&format!("State saved to {}", store.describe()));
        }
    }
    telemetry.shutdown().await;
    let _ = &held_notifications;
    log.line(&format!("Successfully shutdown... (PID={pid})"));
    Ok(if reloading && !lost { End::Reload } else { End::Stopped(lost) })
}

/// What the loop ends with to read the configuration again.
const RELOAD: &str = "reload";

/// Whether an external command line is RESTART_PROGRAM: the configuration
/// is read again, as on SIGHUP.
fn restart_asked(line: &str) -> bool {
    let command = line.trim().split_once("] ").map_or(line.trim(), |(_, rest)| rest);
    command.split(';').next().map(str::trim) == Some("RESTART_PROGRAM")
}

/// The next event of the remote workers, or never without them.
#[cfg(feature = "workers")]
async fn worker_event(
    pools: &mut Option<(crate::workers::Pools, mpsc::UnboundedReceiver<crate::workers::Event>)>,
) -> Option<crate::workers::Event> {
    match pools.as_mut() {
        Some((_, events)) => events.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(not(feature = "workers"))]
async fn worker_event(_: &mut Option<((), ())>) -> Option<()> {
    std::future::pending().await
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
    pools: &[(String, usize, usize, usize, f64)],
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
        ("GET", "/metrics") => Reply::text(200, metrics(engine, program, counts, running, store_failing, pools)),
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
    pools: &[(String, usize, usize, usize, f64)],
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
    if !pools.is_empty() {
        let label = |pool: &str| format!("{{pool=\"{}\"}}", pool.replace('\\', "\\\\").replace('"', "\\\""));
        let each = |pick: fn(&(String, usize, usize, usize, f64)) -> f64| -> Vec<(String, f64)> {
            pools.iter().map(|p| (label(&p.0), pick(p))).collect()
        };
        metric("checkd_pool_workers", "gauge", "Remote workers connected that serve the pool.", each(|p| p.1 as f64));
        metric("checkd_pool_jobs_waiting", "gauge", "Jobs waiting for a worker of the pool.", each(|p| p.2 as f64));
        metric("checkd_pool_jobs_running", "gauge", "Jobs held by workers of the pool.", each(|p| p.3 as f64));
        metric("checkd_pool_oldest_waiting_seconds", "gauge", "How long the oldest job waiting has waited.", each(|p| p.4));
    }
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
fn read_spool(engine: &mut Engine, dir: &Path, effects: &mut Vec<Effect>) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let mut names: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_none() && p.with_extension("ok").exists())
        .collect();
    names.sort();
    let taken = names.len();
    for path in names {
        if let Ok(text) = std::fs::read_to_string(&path) {
            engine.check_result_file(&text, now(), effects);
        }
        let _ = std::fs::remove_file(path.with_extension("ok"));
        let _ = std::fs::remove_file(&path);
    }
    taken
}

/// A Livestatus request that waits (`WaitTrigger`), and until when.
struct Parked {
    ask: crate::livestatus::Ask,
    trigger: checkd_livestatus::request::Trigger,
    until: Ms,
}

/// How many requests may wait at a time, and the longest one may.
const PARKED: usize = 64;
const LONGEST_WAIT: u64 = 300_000;

/// What can happen between two looks at the waiting requests.
const RESULT: u8 = 1;
const LOGGED: u8 = 2;
const COMMANDED: u8 = 4;

/// Whether what happened is what a trigger waits for.
///
/// States, downtimes and comments change by a result that is logged or
/// by a command, so those triggers fire on either: sometimes when nothing
/// of theirs changed, never too late. A result that changes nothing fires
/// `check` and `all` only; `check` fires on a command too, which may bring
/// a passive result. `program` fires on nothing: the request is answered
/// when its time is up, or when checkd stops or reloads.
fn fires(trigger: checkd_livestatus::request::Trigger, happened: u8) -> bool {
    use checkd_livestatus::request::Trigger;
    let on = match trigger {
        Trigger::All => RESULT | LOGGED | COMMANDED,
        Trigger::Check => RESULT | COMMANDED,
        Trigger::Log => LOGGED,
        Trigger::Command => COMMANDED,
        Trigger::State | Trigger::Downtime | Trigger::Comment => LOGGED | COMMANDED,
        Trigger::Program => 0,
    };
    happened & on != 0
}

#[cfg(test)]
mod wait_tests {
    use super::*;
    use checkd_livestatus::request::Trigger;

    #[test]
    fn a_trigger_fires_on_what_can_change_what_it_is_about() {
        assert!(fires(Trigger::Check, RESULT) && fires(Trigger::Check, COMMANDED) && !fires(Trigger::Check, LOGGED));
        assert!(fires(Trigger::State, LOGGED) && fires(Trigger::State, COMMANDED) && !fires(Trigger::State, RESULT));
        assert!(fires(Trigger::Log, LOGGED) && !fires(Trigger::Log, RESULT | COMMANDED));
        assert!(fires(Trigger::All, RESULT) && !fires(Trigger::All, 0));
        assert!(!fires(Trigger::Program, RESULT | LOGGED | COMMANDED));
    }
}

/// How many log lines the Livestatus log table keeps in memory.
const HISTORY: usize = 200_000;

/// How often the state is written when nothing in the log calls for it.
const SAVE_EVERY: Ms = 5000;

/// Opens the store a `--store` value names.
/// Opens a store. `deferred`: a PostgreSQL database that does not answer
/// yet is connected to when it does.
fn open_store_with(spec: &str, deferred: bool) -> Result<Option<Store>, String> {
    if let (true, Some(conninfo)) = (deferred, spec.strip_prefix("postgres:")) {
        return crate::postgres::open_deferred(conninfo).map(Some);
    }
    open_store(spec)
}

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

/// Where the journal of a PostgreSQL store goes when `--journal` is not
/// given: beside the retention file, else in the first of systemd's state
/// directories (`StateDirectory=`), else nowhere.
fn default_journal(retention: Option<&Path>, state_directory: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    const NAME: &str = "checkd-journal.sqlite";
    if let Some(dir) = retention.and_then(Path::parent) {
        return Some(dir.join(NAME));
    }
    let dirs = state_directory?.to_str()?;
    let first = dirs.split(':').next().filter(|dir| !dir.is_empty())?;
    Some(Path::new(first).join(NAME))
}

#[cfg(test)]
mod journal_tests {
    use super::default_journal;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_journal_goes_beside_the_retention_file_first() {
        let got = default_journal(Some(Path::new("/var/lib/checkd/retention.dat")), Some(OsStr::new("/elsewhere")));
        assert_eq!(got, Some(PathBuf::from("/var/lib/checkd/checkd-journal.sqlite")));
    }

    #[test]
    fn without_a_retention_file_it_goes_in_the_state_directory() {
        let got = default_journal(None, Some(OsStr::new("/var/lib/checkd:/var/lib/other")));
        assert_eq!(got, Some(PathBuf::from("/var/lib/checkd/checkd-journal.sqlite")));
        assert_eq!(default_journal(None, Some(OsStr::new(""))), None);
        assert_eq!(default_journal(None, None), None);
    }
}

/// Puts the saved state back into the engine: from the store, or, when it
/// holds none, from a retention file. The lines are for the log.
pub(crate) fn restore(engine: &mut Engine, store: &mut Store, import: Option<&Path>, journal: Option<&Path>) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    // A journal is newer than the store: it is written only while the
    // store cannot be, and removed once the store has what it holds.
    if let Some(path) = journal.filter(|path| path.exists()) {
        let mut kept = Store::new(Box::new(Sqlite::open(&path.to_string_lossy())?))?;
        if let Some(snapshot) = kept.load()? {
            let dropped = engine.restore(&snapshot);
            lines.push(format!(
                "Read the state of {} hosts and services from the journal {}, written while {} could not be; it goes to the store when the store can be written",
                snapshot.objects.len() - dropped.min(snapshot.objects.len()),
                path.display(),
                store.describe()
            ));
            return Ok(lines);
        }
    }
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
///
/// With a journal, what cannot be written to the store goes there, a
/// local SQLite file, so that a restart in that time loses nothing; the
/// journal's log lines go to the store with the next write that works,
/// and the journal is then removed.
fn save(engine: &Engine, store: &mut Store, unsaved: &mut Vec<(i64, String)>, log: &mut Log, failing: &mut bool, journal: Option<&Path>) -> bool {
    let snapshot = engine.snapshot();
    let second = now().div_euclid(1000);
    let journaled = journal.filter(|path| path.exists());
    let mut lines: Vec<(i64, String)> = match journaled {
        Some(path) => Sqlite::open(&path.to_string_lossy())
            .and_then(|db| Store::new(Box::new(db)))
            .and_then(|mut kept| kept.recent_log(100_000))
            .unwrap_or_default(),
        None => Vec::new(),
    };
    lines.extend(unsaved.iter().cloned());
    match store.write(&snapshot, &lines, second) {
        Err(message) if message == "fenced" => {
            log.line("A write was refused: another node holds the lease now");
            true
        }
        Ok(()) => {
            if *failing {
                log.line(&format!("The store {} can be written again", store.describe()));
            }
            if let Some(path) = journaled {
                for suffix in ["", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
                }
                log.line(&format!("What the journal {} held is in the store now", path.display()));
            }
            *failing = false;
            unsaved.clear();
            false
        }
        Err(message) => {
            if !*failing {
                log.line(&format!("Warning: the store cannot be written: {message}"));
            }
            let first = !*failing;
            *failing = true;
            if let Some(path) = journal {
                match journal_write(engine, path, unsaved) {
                    Ok(()) => {
                        if first {
                            log.line(&format!("The state is kept in the journal {} until then", path.display()));
                        }
                        return false;
                    }
                    Err(err) => {
                        if first {
                            log.line(&format!("Warning: the journal {} cannot be written either: {err}", path.display()));
                        }
                    }
                }
            }
            // Lines wait for the next try, up to a limit.
            if unsaved.len() > 100_000 {
                let extra = unsaved.len() - 100_000;
                unsaved.drain(..extra);
            }
            false
        }
    }
}

/// Writes the state and the lines not yet saved to the journal; the lines
/// are then saved.
fn journal_write(engine: &Engine, path: &Path, unsaved: &mut Vec<(i64, String)>) -> Result<(), String> {
    let mut kept = Store::new(Box::new(Sqlite::open(&path.to_string_lossy())?))?;
    kept.write(&engine.snapshot(), unsaved, now().div_euclid(1000))?;
    unsaved.clear();
    Ok(())
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
