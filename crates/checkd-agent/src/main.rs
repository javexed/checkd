//! `checkd-agent`: one host's checks, run on that host for a checkd engine.
//!
//! It is `checkd worker` built on its own. The engine sends a host's
//! service checks to the worker whose entry has the pool
//! `host_<host name>` (`workers.rs`), so an agent named after its host
//! runs that host's checks there.

#![allow(dead_code)]

#[path = "../../checkd/src/builtin.rs"]
mod builtin;
#[path = "../../checkd/src/local.rs"]
mod local;
#[path = "../../checkd/src/runner.rs"]
mod runner;
#[path = "../../checkd/src/tls.rs"]
mod tls;
#[path = "../../checkd/src/worker.rs"]
mod worker;
#[path = "../../checkd/src/ws.rs"]
mod ws;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("--version") {
        println!("checkd-agent {}", env!("CHECKD_VERSION"));
        return ExitCode::SUCCESS;
    }
    match worker::parse(args) {
        Ok(args) => worker::run(args),
        Err(text) => {
            eprintln!("{}", text.replace("checkd worker", "checkd-agent"));
            ExitCode::from(2)
        }
    }
}
