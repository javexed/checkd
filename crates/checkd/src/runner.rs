//! Running a command: the plugin, event handler or notification command
//! an engine or a worker is given.
//!
//! The command line is started directly or through `/bin/sh`, as
//! `checkd_core::cmdline` decides, in a process group of its own, so that
//! it can be stopped at its timeout with all it started. Its output is
//! kept up to a limit and read on while it lasts.

use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
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

/// Starts a command. Its result comes back on `done`. The answer is the
/// command's process group, when it could be started.
pub fn start(job: Job, done: mpsc::UnboundedSender<(u64, JobResult)>) -> Option<i32> {
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
