//! Shadow mode: checkd runs beside a production engine on the same
//! configuration, sends no notifications and runs no event handlers, and
//! reports where the states of its hosts and services differ from the
//! production engine's.
//!
//! Every `EVERY` the production engine's Livestatus socket is asked for
//! the state and state type of every host and service. The two engines do
//! not check at the same moments, so a difference is reported only when
//! it has lasted longer than `GRACE` plus twice the object's own check
//! interval, and its end is reported too. Each round is summed up in the
//! log, so that a quiet log can be told from a shadow that does not run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use checkd_core::engine::{Engine, Ms, Obj};
use checkd_livestatus::json::{self, Json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// How often production is asked.
pub const EVERY: Duration = Duration::from_secs(10);

/// How long a difference may last before it counts, beyond the object's
/// own intervals.
const GRACE: Ms = 30_000;

/// The state and state type of each object, by `host` or `host;service`.
pub type States = BTreeMap<String, (u8, bool)>;

async fn ask(socket: &Path, query: &str) -> Result<Vec<Json>, String> {
    let mut stream = UnixStream::connect(socket)
        .await
        .map_err(|err| format!("cannot reach {}: {err}", socket.display()))?;
    stream.write_all(query.as_bytes()).await.map_err(|err| err.to_string())?;
    stream.shutdown().await.map_err(|err| err.to_string())?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.map_err(|err| err.to_string())?;
    match json::parse(&answer) {
        Some(Json::List(rows)) => Ok(rows),
        _ => Err(format!("{} gave an answer that is not understood", socket.display())),
    }
}

/// What production says now.
pub async fn production(socket: PathBuf) -> Result<States, String> {
    let mut states = States::new();
    let hosts = ask(&socket, "GET hosts\nColumns: name state state_type has_been_checked\nOutputFormat: json\n\n").await?;
    for row in hosts {
        let cells = row.as_list();
        if cells.len() == 4 && cells[3].as_num() == 1.0 {
            states.insert(cells[0].as_str().to_string(), (cells[1].as_num() as u8, cells[2].as_num() == 1.0));
        }
    }
    let services = ask(
        &socket,
        "GET services\nColumns: host_name description state state_type has_been_checked\nOutputFormat: json\n\n",
    )
    .await?;
    for row in services {
        let cells = row.as_list();
        if cells.len() == 5 && cells[4].as_num() == 1.0 {
            states.insert(
                format!("{};{}", cells[0].as_str(), cells[1].as_str()),
                (cells[2].as_num() as u8, cells[3].as_num() == 1.0),
            );
        }
    }
    Ok(states)
}

/// What is followed between rounds: since when each object differs, and
/// whether that was reported.
#[derive(Default)]
pub struct Shadow {
    since: BTreeMap<String, (Ms, bool)>,
    failing: bool,
}

impl Shadow {
    /// Compares production's states with the engine's. The answer is the
    /// lines for the log.
    pub fn compare(&mut self, engine: &Engine, theirs: Result<States, String>, now: Ms) -> Vec<String> {
        let mut lines = Vec::new();
        let theirs = match theirs {
            Ok(states) => {
                if self.failing {
                    lines.push("SHADOW: production answers again".to_string());
                }
                self.failing = false;
                states
            }
            Err(message) => {
                if !self.failing {
                    lines.push(format!("SHADOW: production does not answer: {message}"));
                }
                self.failing = true;
                return lines;
            }
        };
        let (mut compared, mut differing) = (0, 0);
        for obj in engine.all_objects() {
            let item = engine.item(obj);
            if !item.checked {
                continue;
            }
            let name = engine.log_name(obj);
            let Some(&(state, hard)) = theirs.get(&name) else { continue };
            compared += 1;
            let names = |state: u8| checkd_core::notify::state_name(obj, state);
            let hard_name = |hard: bool| if hard { "HARD" } else { "SOFT" };
            if state == item.state && hard == item.hard {
                if let Some((_, true)) = self.since.remove(&name) {
                    lines.push(format!("SHADOW AGREES: {name}: {} {}", names(state), hard_name(hard)));
                } else {
                    self.since.remove(&name);
                }
                continue;
            }
            differing += 1;
            let interval = match obj {
                Obj::Host(_) | Obj::Service(_) => {
                    let cfg = &item.cfg;
                    (cfg.check_interval.max(cfg.retry_interval) * engine.settings().interval_length * 1000.0) as Ms
                }
            };
            let entry = self.since.entry(name.clone()).or_insert((now, false));
            if !entry.1 && now - entry.0 > GRACE + 2 * interval {
                entry.1 = true;
                lines.push(format!(
                    "SHADOW DIFFERENCE: {name}: production {} {}, checkd {} {}, for {} seconds",
                    names(state),
                    hard_name(hard),
                    names(item.state),
                    hard_name(item.hard),
                    (now - entry.0) / 1000
                ));
            }
        }
        lines.push(format!("SHADOW: {compared} hosts and services compared, {differing} differ now"));
        lines
    }
}
