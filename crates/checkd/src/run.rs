//! `checkd run`: the engine on the real clock, with real programs.
//!
//! The engine decides what is checked when and what a result means. This
//! file does what the engine cannot: it keeps the time, starts the
//! commands the engine asks for, stops the ones that run too long, and
//! writes the log.

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
}

pub fn run(args: RunArgs) -> ExitCode {
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

    let log_path = args
        .log_file
        .clone()
        .or_else(|| main.get("log_file").map(PathBuf::from))
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
    let (mut engine, warnings) = Engine::new(objects, &main, Box::new(Local), now());
    for warning in &warnings {
        eprintln!("warning: {warning}");
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
    match runtime.block_on(serve(&mut engine, &mut log, &args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

/// Runs the engine until a signal says stop or the time is up.
async fn serve(engine: &mut Engine, log: &mut Log, args: &RunArgs) -> Result<(), String> {
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
    // The process group of every command that is running.
    let mut running: BTreeMap<u64, i32> = BTreeMap::new();
    let mut effects = Vec::new();
    engine.start(&mut effects);
    // The engine has now planned every first check. The file moves them.
    if let Some(path) = &args.first_checks {
        first_checks(engine, path)?;
    }

    let pid = std::process::id();
    log.line(&format!(
        "checkd {} starting... (PID={pid})",
        env!("CARGO_PKG_VERSION")
    ));

    let why = loop {
        for effect in effects.drain(..) {
            match effect {
                Effect::Log(line) => log.line(&line),
                Effect::Run(job) => {
                    let id = job.id;
                    if otel::BUILT_IN {
                        let latency = match job.kind {
                            JobKind::HostCheck | JobKind::ServiceCheck => {
                                engine.item(job.obj).latency
                            }
                            JobKind::HostHandler | JobKind::ServiceHandler => 0.0,
                        };
                        telemetry.job_started(&job, &engine.log_name(job.obj), latency);
                    }
                    if let Some(group) = start(job, done.clone()) {
                        running.insert(id, group);
                    }
                }
            }
        }
        let wait = match engine.next_wakeup() {
            Some(at) => Duration::from_millis((at - now()).max(0) as u64),
            None => Duration::from_secs(3600),
        };
        let flush = telemetry.next_flush();
        tokio::select! {
            _ = tokio::time::sleep(wait) => engine.advance(now(), &mut effects),
            Some((id, result)) = results.recv() => {
                running.remove(&id);
                let read = otel::nanos();
                let outcome = result.outcome.clone();
                engine.job_done(now(), id, result, &mut effects);
                telemetry.job_done(id, &outcome, read);
            }
            _ = tokio::time::sleep(flush.unwrap_or_default()), if flush.is_some() => telemetry.flush(),
            _ = terminate.recv() => break "Caught SIGTERM",
            _ = interrupt.recv() => break "Caught SIGINT",
            _ = &mut over => break "The time given with --for is up",
        }
    };

    log.line(&format!("{why}, shutting down..."));
    for group in running.values() {
        stop_group(*group);
    }
    telemetry.shutdown().await;
    log.line(&format!("Successfully shutdown... (PID={pid})"));
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
