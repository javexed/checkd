//! Running a command: the plugin, event handler or notification command
//! an engine or a worker is given.
//!
//! The command line is started directly or through `/bin/sh`, as
//! `checkd_core::cmdline` decides, in a process group of its own, so that
//! it can be stopped at its timeout with all it started. Its output is
//! kept up to a limit and read on while it lasts.

use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use checkd_core::cmdline::{self, Start};
use checkd_core::engine::{Job, JobResult, Ms, Outcome};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

/// How much of what a command prints is kept, on each of standard output
/// and standard error. The rest is read and dropped, so that the command
/// is not held up.
const KEEP: usize = 1024 * 1024;

/// How long output is still read after a command has ended. Something
/// the command started may have kept its output open.
const GRACE: Duration = Duration::from_millis(500);

/// The process groups of the commands that run, by job: for stopping them
/// all, and for counting them. Kept by the tasks that run the commands.
#[derive(Clone, Default)]
pub struct Running(Arc<Mutex<BTreeMap<u64, i32>>>);

impl Running {
    pub fn count(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    /// Stops every command that runs, with all it started.
    pub fn stop_all(&self) {
        for group in self.0.lock().unwrap().values() {
            stop_group(*group);
        }
    }

    fn add(&self, id: u64, group: i32) {
        self.0.lock().unwrap().insert(id, group);
    }

    fn remove(&self, id: u64) {
        self.0.lock().unwrap().remove(&id);
    }
}

/// Starts a command. Its result comes back on `done`.
///
/// Starting a process takes the thread that does it until the new process
/// runs its program, which takes longer the more processes there are; on
/// the engine's one thread that would hold every check up behind it. So
/// the process is started on a thread of the runtime's pool, many at once,
/// and the engine's thread goes on at once.
pub fn start(job: Job, done: mpsc::UnboundedSender<(u64, JobResult)>, running: &Running) {
    // One of checkd's own checks starts no process.
    if crate::builtin::is_builtin(&job.line) {
        tokio::spawn(async move {
            let result = crate::builtin::run(&job.line, job.timeout).await;
            let _ = done.send((job.id, result));
        });
        return;
    }
    let running = running.clone();
    tokio::spawn(async move {
        let line = job.line.clone();
        let env = job.env.clone();
        let spawned = tokio::task::spawn_blocking(move || spawn(&line, &env)).await;
        let (child, group) = match spawned {
            Ok(Ok(started)) => started,
            Ok(Err(result)) => {
                let _ = done.send((job.id, result));
                return;
            }
            Err(err) => {
                let result = JobResult { outcome: Outcome::Failed(err.to_string()), stdout: String::new(), stderr: String::new() };
                let _ = done.send((job.id, result));
                return;
            }
        };
        if let Some(group) = group {
            running.add(job.id, group);
        }
        let result = finish(child, group, job.timeout).await;
        running.remove(job.id);
        let _ = done.send((job.id, result));
    });
}

/// Starts the process of a command line. A command that cannot be started
/// gives its result at once.
fn spawn(line: &str, more_env: &[(String, String)]) -> Result<(Child, Option<i32>), JobResult> {
    let (program, mut command) = match cmdline::plan(line) {
        Start::Direct { env, argv } => {
            let program = argv.first().cloned().unwrap_or_default();
            let mut command = Command::new(&program);
            command.args(argv.iter().skip(1)).envs(env);
            (program, command)
        }
        Start::Shell => {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(line);
            ("/bin/sh".to_string(), command)
        }
    };
    // A group of its own, so that the command can be stopped together
    // with everything it started.
    command
        .envs(more_env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    match command.spawn() {
        Ok(child) => {
            let group = child.id().map(|pid| pid as i32);
            Ok((child, group))
        }
        Err(err) => {
            // The reference tries to start the program in a process of
            // its own, which says this and ends with the error's number.
            let errno = err.raw_os_error().unwrap_or(libc::ENOENT);
            Err(JobResult {
                outcome: Outcome::Exited(errno),
                stdout: String::new(),
                stderr: format!("execvp({program}, ...) failed. errno is {errno}: {}", error_text(errno)),
            })
        }
    }
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
    // On the heap, and only once the command runs: a buffer in the task
    // itself would be reserved for every check that waits to start.
    let mut buffer = vec![0u8; 8192];
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
pub fn stop_group(group: i32) {
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

#[cfg(test)]
mod load {
    use super::*;
    use checkd_core::engine::{JobKind, Obj};

    /// How many quick commands a second the runner gets through on its own.
    /// `cargo test --release -p checkd runner_rate -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn runner_rate() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            for n in [500usize, 2000, 8000] {
                let (done, mut results) = mpsc::unbounded_channel();
                let began = std::time::Instant::now();
                let running = Running::default();
                for id in 0..n as u64 {
                    let job = Job { id, kind: JobKind::ServiceCheck, obj: Obj::Service(0), line: "/bin/true".into(), timeout: 60_000 , about: None, env: Vec::new() };
                    start(job, done.clone(), &running);
                }
                let started = began.elapsed();
                for _ in 0..n {
                    results.recv().await.unwrap();
                }
                let all = began.elapsed();
                println!("{n} commands: started in {:.2} s, all ended in {:.2} s: {:.0} a second", started.as_secs_f64(), all.as_secs_f64(), n as f64 / all.as_secs_f64());
            }
        });
    }
}

#[cfg(test)]
mod memory {
    use super::*;
    use checkd_core::engine::{JobKind, Obj};

    fn rss_mb() -> i64 {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find(|l| l.starts_with("VmRSS:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|n| n.parse::<i64>().ok())
            .unwrap_or(0)
            / 1024
    }

    /// Whether the runner holds on to memory command by command.
    /// `cargo test --release -p checkd runner_memory -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn runner_memory() {
        let runtime = tokio::runtime::Builder::new_current_thread().max_blocking_threads(32).enable_all().build().unwrap();
        runtime.block_on(async {
            let (done, mut results) = mpsc::unbounded_channel();
            let running = Running::default();
            let mut marks = Vec::new();
            for round in 0..10u64 {
                for n in 0..20_000u64 {
                    let job = Job { id: round * 20_000 + n, kind: JobKind::ServiceCheck, obj: Obj::Service(0), line: "/bin/true".into(), timeout: 60_000 , about: None, env: Vec::new() };
                    start(job, done.clone(), &running);
                    if n % 500 == 499 {
                        for _ in 0..500 {
                            results.recv().await.unwrap();
                        }
                    }
                }
                marks.push(rss_mb());
            }
            println!("MB after each 20,000 commands: {marks:?}");
        });
    }
}

#[cfg(test)]
mod size {
    use super::*;

    /// A check that waits to start costs about the size of its task: kept
    /// small, so that tens of thousands can wait.
    #[test]
    fn a_waiting_task_is_small() {
        let (done, _results) = mpsc::unbounded_channel::<(u64, JobResult)>();
        let job = Job { id: 1, kind: checkd_core::engine::JobKind::ServiceCheck, obj: checkd_core::engine::Obj::Service(0), line: "x".into(), timeout: 1 , about: None, env: Vec::new() };
        let running = Running::default();
        let task = async move {
            let line = job.line.clone();
            let spawned = tokio::task::spawn_blocking(move || spawn(&line, &[])).await;
            if let Ok(Ok((child, group))) = spawned {
                let result = finish(child, group, job.timeout).await;
                running.remove(job.id);
                let _ = done.send((job.id, result));
            }
        };
        let size = std::mem::size_of_val(&task);
        assert!(size < 2048, "a waiting task takes {size} bytes");
    }
}
