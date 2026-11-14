//! `checkd worker`: runs the engine's jobs on another machine.
//!
//! The worker connects out to the engine over TLS, trusting only the
//! certificate authority given with `--ca`, and says who it is with its
//! name and token. It then runs the jobs it gets, as many at once as
//! `--max-jobs` allows, and sends their results back. It runs a job the way
//! the engine runs one on its own host (`runner.rs`): in a process group of
//! its own, stopped at its timeout.
//!
//! - With several `--engine` addresses (the two nodes of a failover pair),
//!   it tries them in turn; only the node that leads listens for workers.
//! - When the connection breaks, the jobs still running are stopped, since
//!   the engine hands them to another worker, and the worker connects again
//!   after a pause that grows to 30 seconds.
//! - `--restrict-path <dir>` (more than once for more): only programs under
//!   these directories are run, and never a line that needs a shell. Any
//!   other job is answered with UNKNOWN and says why.
//! - `--load-limit1/5/15 <n>`: while the machine's load average is above
//!   the limit, the worker takes no new jobs.

use std::path::PathBuf;
use std::time::Duration;

use checkd_core::cmdline::{self, Start};
use checkd_core::engine::{Job, JobKind, JobResult, Obj, Outcome};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

pub const USAGE: &str = "\
usage: checkd worker --engine <host:port> --name <name> --token-file <file>
                     --ca <file> [options]

options:
  --engine <host:port>  the engine to take jobs from; more than once for
                        the nodes of a failover pair
  --server-name <name>  the name in the engine's certificate, when it is
                        not the host of --engine
  --name <name>         this worker's name in the engine's workers file
  --token-file <file>   the file that holds its token
  --ca <file>           the certificate authority of the engine's
                        certificate
  --max-jobs <n>        jobs run at once (8)
  --restrict-path <dir> run only programs under this directory, and no
                        line that needs a shell; more than once for more
  --load-limit1 <n>, --load-limit5 <n>, --load-limit15 <n>
                        take no new jobs while the load average over 1, 5
                        or 15 minutes is above <n>
";

pub struct WorkerArgs {
    pub engines: Vec<String>,
    pub server_name: Option<String>,
    pub name: String,
    pub token_file: PathBuf,
    pub ca: PathBuf,
    pub max_jobs: usize,
    pub restrict_path: Vec<String>,
    pub load_limits: [Option<f64>; 3],
}

pub fn parse(mut args: impl Iterator<Item = String>) -> Result<WorkerArgs, String> {
    let mut out = WorkerArgs {
        engines: Vec::new(),
        server_name: None,
        name: String::new(),
        token_file: PathBuf::new(),
        ca: PathBuf::new(),
        max_jobs: 8,
        restrict_path: Vec::new(),
        load_limits: [None; 3],
    };
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{arg} needs {what}"));
        let number = |text: String| text.parse::<f64>().map_err(|_| format!("'{text}' is not a number"));
        match arg.as_str() {
            "--engine" => out.engines.push(value("host:port")?),
            "--server-name" => out.server_name = Some(value("a host name")?),
            "--name" => out.name = value("a name")?,
            "--token-file" => out.token_file = PathBuf::from(value("a file name")?),
            "--ca" => out.ca = PathBuf::from(value("a file name")?),
            "--max-jobs" => out.max_jobs = number(value("a number")?)?.max(1.0) as usize,
            "--restrict-path" => out.restrict_path.push(value("a directory")?.trim_end_matches(['/', '\\']).to_string() + "/"),
            "--load-limit1" => out.load_limits[0] = Some(number(value("a number")?)?),
            "--load-limit5" => out.load_limits[1] = Some(number(value("a number")?)?),
            "--load-limit15" => out.load_limits[2] = Some(number(value("a number")?)?),
            "--help" | "-h" => return Err(USAGE.to_string()),
            _ => return Err(format!("'{arg}' is not an option of worker\n\n{USAGE}")),
        }
    }
    if out.engines.is_empty() || out.name.is_empty() || out.token_file.as_os_str().is_empty() || out.ca.as_os_str().is_empty() {
        return Err(format!("worker needs --engine, --name, --token-file and --ca\n\n{USAGE}"));
    }
    Ok(out)
}

fn say(line: &str) {
    let second = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("[{second}] {line}");
}

/// Why a job is not run, when `--restrict-path` is given.
pub fn refused(line: &str, allowed: &[String]) -> Option<String> {
    if allowed.is_empty() {
        return None;
    }
    match cmdline::plan(line) {
        Start::Shell => Some("the command line needs a shell, which --restrict-path does not allow".into()),
        Start::Direct { argv, .. } => {
            let program = argv.first().cloned().unwrap_or_default();
            let path = comparable(&program);
            let clean = !path.split('/').any(|part| part == "..");
            if clean && allowed.iter().any(|dir| path.starts_with(comparable(dir).as_str())) {
                None
            } else {
                Some(format!("{program} is not under a directory of --restrict-path"))
            }
        }
    }
}

/// A path as `--restrict-path` compares it. Windows takes either slash and
/// ignores case.
#[cfg(windows)]
fn comparable(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

#[cfg(not(windows))]
fn comparable(path: &str) -> String {
    path.to_string()
}

#[cfg(not(unix))]
fn load_average() -> Option<[f64; 3]> {
    None
}

#[cfg(unix)]
fn load_average() -> Option<[f64; 3]> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    let mut numbers = text.split_whitespace().map(|n| n.parse::<f64>().ok());
    Some([numbers.next()??, numbers.next()??, numbers.next()??])
}

fn over_limit(limits: &[Option<f64>; 3]) -> bool {
    let Some(load) = load_average() else { return false };
    limits.iter().zip(load).any(|(limit, now)| limit.is_some_and(|limit| now > limit))
}

pub fn run(args: WorkerArgs) -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread().max_blocking_threads(32).enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("cannot start: {err}");
            return std::process::ExitCode::from(2);
        }
    };
    match runtime.block_on(serve(args)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            std::process::ExitCode::from(2)
        }
    }
}

async fn serve(args: WorkerArgs) -> Result<(), String> {
    let token = std::fs::read_to_string(&args.token_file)
        .map_err(|err| format!("cannot read the token {}: {err}", args.token_file.display()))?
        .trim()
        .to_string();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return Err(format!("{}: a token is one word", args.token_file.display()));
    }
    let connector = crate::tls::connector(&args.ca)?;
    let mut pause = Duration::from_secs(1);
    let mut turn = 0usize;
    let mut terminate = Terminate::new()?;
    loop {
        let engine = args.engines[turn % args.engines.len()].clone();
        turn += 1;
        let session = session(&args, &engine, &token, &connector);
        tokio::select! {
            ended = session => match ended {
                Ok(served) => {
                    say(&format!("The connection to {engine} ended"));
                    if served {
                        pause = Duration::from_secs(1);
                    }
                }
                Err(err) => say(&format!("{engine}: {err}")),
            },
            _ = terminate.recv() => {
                say("Caught SIGTERM, shutting down");
                return Ok(());
            }
            _ = tokio::signal::ctrl_c() => {
                say("Caught SIGINT, shutting down");
                return Ok(());
            }
        }
        // Each engine once quickly, then the pause grows.
        if turn % args.engines.len() == 0 {
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_secs(30));
        }
    }
}

/// SIGTERM on Unix. Windows stops a service or task some other way, and
/// Ctrl-C covers a console.
#[cfg(unix)]
struct Terminate(tokio::signal::unix::Signal);

#[cfg(unix)]
impl Terminate {
    fn new() -> Result<Self, String> {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map(Terminate)
            .map_err(|err| err.to_string())
    }
    async fn recv(&mut self) {
        self.0.recv().await;
    }
}

#[cfg(not(unix))]
struct Terminate;

#[cfg(not(unix))]
impl Terminate {
    fn new() -> Result<Self, String> {
        Ok(Terminate)
    }
    async fn recv(&mut self) {
        std::future::pending::<()>().await
    }
}

/// One connection. The answer says whether the engine took the worker.
async fn session(
    args: &WorkerArgs,
    engine: &str,
    token: &str,
    connector: &tokio_rustls::TlsConnector,
) -> Result<bool, String> {
    let host = match &args.server_name {
        Some(name) => name.clone(),
        None => engine.rsplit_once(':').map_or(engine, |(host, _)| host).trim_matches(['[', ']']).to_string(),
    };
    let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(engine))
        .await
        .map_err(|_| "no connection within 10 seconds".to_string())?
        .map_err(|err| err.to_string())?;
    let stream = tokio::time::timeout(Duration::from_secs(10), connector.connect(crate::tls::server_name(&host)?, tcp))
        .await
        .map_err(|_| "no TLS handshake within 10 seconds".to_string())?
        .map_err(|err| format!("TLS: {err}"))?;
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let slots = if over_limit(&args.load_limits) { 0 } else { args.max_jobs };
    write
        .write_all(format!("HELLO {} {token} {slots}\n", args.name).as_bytes())
        .await
        .map_err(|err| err.to_string())?;
    let mut answer = String::new();
    tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut answer))
        .await
        .map_err(|_| "no answer to HELLO within 10 seconds".to_string())?
        .map_err(|err| err.to_string())?;
    let pools = match answer.trim_end().split_once(' ') {
        Some(("WELCOME", pools)) => pools.to_string(),
        _ if answer.trim_end() == "WELCOME" => String::new(),
        _ => return Err(format!("the engine said: {}", answer.trim_end())),
    };
    say(&format!("Connected to {engine}, for pools {pools}"));

    let (done, mut results) = mpsc::unbounded_channel::<(u64, JobResult)>();
    let running = crate::runner::Running::default();
    let mut ping = tokio::time::interval(Duration::from_secs(10));
    let mut load_tick = tokio::time::interval(Duration::from_secs(5));
    let mut offered = slots;
    // Frames are read by a task of their own: a read cut off half way by
    // another event would lose its part.
    let (frames, mut jobs) = mpsc::unbounded_channel::<Result<(u64, i64, String), String>>();
    let reading = tokio::spawn(async move {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let words: Vec<&str> = line.split_whitespace().collect();
            let parsed = match words.as_slice() {
                ["JOB", id, timeout, len] => match (id.parse::<u64>(), timeout.parse::<i64>(), len.parse::<usize>()) {
                    (Ok(id), Ok(timeout), Ok(len)) if len <= 1 << 20 => Some((id, timeout, len)),
                    _ => None,
                },
                _ => None,
            };
            let Some((id, timeout, len)) = parsed else {
                let _ = frames.send(Err(format!("the engine sent what is not understood: {}", line.trim_end())));
                return;
            };
            let mut bytes = vec![0u8; len];
            if reader.read_exact(&mut bytes).await.is_err() {
                return;
            }
            if frames.send(Ok((id, timeout, String::from_utf8_lossy(&bytes).into_owned()))).is_err() {
                return;
            }
        }
    });
    let outcome = loop {
        tokio::select! {
            frame = jobs.recv() => {
                let Some(frame) = frame else { break Ok(true) };
                let (id, timeout, line) = match frame {
                    Ok(job) => job,
                    Err(err) => break Err(err),
                };
                if let Some(why) = refused(&line, &args.restrict_path) {
                    let _ = done.send((id, JobResult {
                        outcome: Outcome::Exited(3),
                        stdout: format!("(Refused by worker {}: {why})\n", args.name),
                        stderr: String::new(),
                    }));
                    continue;
                }
                let job = Job { id, kind: JobKind::ServiceCheck, obj: Obj::Host(0), line, timeout, about: None, env: Vec::new() };
                crate::runner::start(job, done.clone(), &running);
            }
            Some((id, result)) = results.recv() => {
                let (kind, code) = match &result.outcome {
                    Outcome::Exited(code) => ("exit", *code),
                    Outcome::Signaled(signal) => ("signal", *signal),
                    Outcome::TimedOut => ("timeout", 0),
                    Outcome::Failed(_) => ("failed", 0),
                };
                let stderr = match &result.outcome {
                    Outcome::Failed(why) if result.stderr.is_empty() => why.clone(),
                    _ => result.stderr.clone(),
                };
                let mut frame = format!("RESULT {id} {kind} {code} {} {}\n", result.stdout.len(), stderr.len()).into_bytes();
                frame.extend_from_slice(result.stdout.as_bytes());
                frame.extend_from_slice(stderr.as_bytes());
                if write.write_all(&frame).await.is_err() {
                    break Ok(true);
                }
            }
            _ = ping.tick() => {
                if write.write_all(b"PING\n").await.is_err() {
                    break Ok(true);
                }
            }
            _ = load_tick.tick() => {
                let slots = if over_limit(&args.load_limits) { 0 } else { args.max_jobs };
                if slots != offered {
                    offered = slots;
                    say(&format!("Load average {}: taking {slots} jobs at once", if slots == 0 { "over the limit" } else { "under the limit" }));
                    if write.write_all(format!("SLOTS {slots}\n").as_bytes()).await.is_err() {
                        break Ok(true);
                    }
                }
            }
        }
    };
    reading.abort();
    // The engine gives what was running to another worker.
    running.stop_all();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restrict_path() {
        let allowed = vec!["/usr/lib/nagios/plugins/".to_string()];
        assert_eq!(refused("/usr/lib/nagios/plugins/check_ping -H x", &allowed), None);
        assert!(refused("/bin/rm -rf /", &allowed).is_some());
        assert!(refused("/usr/lib/nagios/plugins/../../../bin/sh", &allowed).is_some());
        assert!(refused("/usr/lib/nagios/plugins/check_x | tee /tmp/y", &allowed).is_some());
        assert_eq!(refused("anything at all", &[]), None);
    }

    #[cfg(windows)]
    #[test]
    fn restrict_path_on_windows() {
        let allowed = vec!["C:\\Program Files\\checkd/".to_string()];
        assert_eq!(refused("'c:\\program files\\checkd\\check_disk.exe' -w 10", &allowed), None);
        assert_eq!(refused("\"C:/Program Files/checkd/check_disk.exe\" -w 10", &allowed), None);
        assert!(refused("'C:\\Program Files\\checkd\\..\\..\\Windows\\cmd.exe'", &allowed).is_some());
        assert!(refused("C:/Windows/System32/cmd.exe", &allowed).is_some());
    }
}
