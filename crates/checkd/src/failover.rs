//! Failover: two nodes on one PostgreSQL database, one leading.
//!
//! - **The lease.** A row in the database: who holds it, a counter that goes
//!   up at each takeover, and when it runs out, by the database's clock. The
//!   leader renews it every third of its length. A thread of its own does
//!   this, with its own connection, so that a slow database does not hold
//!   the checks up.
//! - **Fencing.** Every write of the store carries the holder and counter it
//!   was made under, and is not made when the lease has moved on. A node
//!   that finds it has lost the lease stops at once: it writes that to its
//!   log and ends with status 3, and is to be started again, as a standby,
//!   by whatever runs it.
//! - **The standby** takes an expired lease, but only after it has been
//!   connected to the database for one full lease: after an outage, the
//!   leader gets the time to renew first. Until then it forwards Livestatus
//!   connections, external commands and check result files to the leader,
//!   so that clients can point at either node.
//! - **The heartbeat.** Each node answers on a TCP port whether it leads, and
//!   with what counter. A leader that cannot reach the database asks the
//!   other node. If that one leads with a newer counter, it stops. If it can
//!   reach neither, it cannot tell: by default it keeps notifying, at the
//!   risk of duplicate alerts; with `--isolated hold` it holds its
//!   notifications back until the database is back.
//! - Each node writes a hash of its configuration beside its name, and a
//!   node whose hash differs from the other's says so in its log.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::mpsc;

use crate::postgres::Postgres;
use crate::store::Value;

/// How failover is set up for this node.
#[derive(Clone, Debug)]
pub struct Setup {
    pub node: String,
    pub conninfo: String,
    /// Seconds.
    pub lease: f64,
    /// Where this node answers the heartbeat, and where the other does.
    pub peer_listen: String,
    pub peer: Option<String>,
    /// The address others reach this node's Livestatus at over TCP.
    pub livestatus_tcp: String,
    /// The host name others reach this node at.
    pub advertise: String,
    /// Hold notifications back when isolated, instead of going on.
    pub hold: bool,
    pub config_hash: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Role {
    Standby,
    Leader(i64),
}

/// What the lease thread tells the node.
#[derive(Debug)]
pub enum Event {
    Acquired(i64),
    Lost(String),
    /// Neither the database nor the other node can be reached.
    Isolated,
    /// The database answers again.
    Reached,
    /// The leader's Livestatus address over TCP, as far as is known.
    LeaderAt(Option<String>),
    Note(String),
}

pub type Shared<T> = Arc<Mutex<T>>;

fn port_of(addr: &str) -> &str {
    addr.rsplit_once(':').map_or(addr, |(_, port)| port)
}

/// Asks a node over the heartbeat whether it leads.
pub fn ask_peer(addr: &str) -> Option<Role> {
    let target = addr.to_socket_addrs().ok()?.next()?;
    let mut stream = TcpStream::connect_timeout(&target, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let _ = stream.write_all(b"STATUS\n");
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let line = line.trim();
    if let Some(counter) = line.strip_prefix("LEADER ") {
        return counter.parse().ok().map(Role::Leader);
    }
    (line == "STANDBY").then_some(Role::Standby)
}

/// Answers the heartbeat: `LEADER <counter>` or `STANDBY`.
pub async fn serve_heartbeat(addr: String, role: Shared<Role>) -> Result<(), String> {
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|err| format!("cannot listen for the heartbeat at {addr}: {err}"))?;
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else { continue };
            let answer = match *role.lock().unwrap() {
                Role::Leader(counter) => format!("LEADER {counter}\n"),
                Role::Standby => "STANDBY\n".to_string(),
            };
            let _ = stream.write_all(answer.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    Ok(())
}

/// How long a lease operation may wait for the database: not so long
/// that the lease runs out while waiting.
static LIMIT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();

fn rows(db: &Postgres, sql: &str, values: Vec<Value>) -> Result<Vec<Vec<Value>>, String> {
    db.query_within(sql, values, *LIMIT.get().unwrap_or(&Duration::from_secs(5)))
}

fn int(value: &Value) -> i64 {
    match value {
        Value::Int(n) => *n,
        Value::Text(t) => t.parse().unwrap_or(0),
        _ => 0,
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::Text(t) => t.clone(),
        Value::Int(n) => n.to_string(),
        _ => String::new(),
    }
}

/// Starts the thread that holds the lease. Its events come out of the
/// answer.
pub fn start(setup: Setup, role: Shared<Role>) -> Result<mpsc::UnboundedReceiver<Event>, String> {
    let mut db = crate::postgres::connect_to(&setup.conninfo, false)?;
    let (events, inbox) = mpsc::unbounded_channel();
    let _ = LIMIT.set(Duration::from_secs_f64((setup.lease / 3.0).clamp(1.0, 5.0)));
    std::thread::spawn(move || {
        let period = Duration::from_secs_f64((setup.lease / 3.0).max(1.0));
        let mut connected_since: Option<Instant> = None;
        let mut isolated = false;
        let mut said_mismatch = false;
        let mut tables = false;
        let mut renewed_at = Instant::now();
        loop {
            let current = *role.lock().unwrap();
            match current {
                Role::Standby => {
                    // The database is reached when this answers.
                    if !tables {
                        tables = db.make_tables().is_ok();
                    }
                    let reached = (if tables { Ok(()) } else { Err(String::new()) }).and_then(|_| {
                        rows(
                            &db,
                            "INSERT INTO checkd_lease (name, holder, counter, expires) \
                             VALUES ('leader', '', 0, now() - interval '1 second') \
                             ON CONFLICT (name) DO NOTHING RETURNING counter",
                            vec![],
                        )?;
                        register(&db, &setup)?;
                        rows(
                            &db,
                            "SELECT l.holder, n.livestatus FROM checkd_lease l \
                             LEFT JOIN checkd_nodes n ON n.name = l.holder \
                             WHERE l.name = 'leader' AND l.expires > now()",
                            vec![],
                        )
                    });
                    match reached {
                        Ok(leader) => {
                            let since = *connected_since.get_or_insert_with(Instant::now);
                            let at = leader.first().map(|row| text(&row[1])).filter(|a| !a.is_empty());
                            let _ = events.send(Event::LeaderAt(at));
                            if !said_mismatch {
                                if let Some(note) = mismatch(&db, &setup) {
                                    said_mismatch = true;
                                    let _ = events.send(Event::Note(note));
                                }
                            }
                            // A full lease after connecting, before taking an
                            // expired one.
                            if leader.is_empty() && since.elapsed().as_secs_f64() >= setup.lease {
                                let taken = rows(
                                    &db,
                                    "UPDATE checkd_lease SET holder = $1, counter = counter + 1, \
                                     expires = now() + make_interval(secs => $2) \
                                     WHERE name = 'leader' AND expires < now() RETURNING counter",
                                    vec![Value::Text(setup.node.clone()), Value::Real(setup.lease)],
                                );
                                if let Ok(taken) = taken {
                                    if let Some(row) = taken.first() {
                                        let counter = int(&row[0]);
                                        *role.lock().unwrap() = Role::Leader(counter);
                                        let _ = events.send(Event::Acquired(counter));
                                    }
                                }
                            }
                        }
                        Err(_) => connected_since = None,
                    }
                }
                Role::Leader(counter) => {
                    let renewed = rows(
                        &db,
                        "UPDATE checkd_lease SET expires = now() + make_interval(secs => $3) \
                         WHERE name = 'leader' AND holder = $1 AND counter = $2 RETURNING counter",
                        vec![Value::Text(setup.node.clone()), Value::Int(counter), Value::Real(setup.lease)],
                    );
                    match renewed {
                        Ok(found) if found.is_empty() => {
                            let _ = events.send(Event::Lost("the lease is held by another node now".into()));
                            return;
                        }
                        Ok(_) => {
                            renewed_at = Instant::now();
                            let _ = register(&db, &setup);
                            if isolated {
                                isolated = false;
                                let _ = events.send(Event::Reached);
                            }
                        }
                        Err(_) => {
                            // Without the database: ask the other node.
                            match setup.peer.as_deref().and_then(ask_peer) {
                                Some(Role::Leader(theirs)) if theirs > counter => {
                                    let _ = events.send(Event::Lost(format!(
                                        "the other node leads, with lease {theirs}"
                                    )));
                                    return;
                                }
                                Some(_) => {
                                    // The other node answers, and reaches the
                                    // database or soon will: it takes the lease
                                    // once it runs out. This node stops before
                                    // that, by its own clock, so that the two
                                    // never lead at once.
                                    if renewed_at.elapsed().as_secs_f64() > setup.lease * 0.6 {
                                        let _ = events.send(Event::Lost(
                                            "the lease could not be renewed, and the other node can take it".into(),
                                        ));
                                        return;
                                    }
                                    if isolated {
                                        isolated = false;
                                        let _ = events.send(Event::Reached);
                                    }
                                }
                                None => {
                                    if !isolated {
                                        isolated = true;
                                        let _ = events.send(Event::Isolated);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            std::thread::sleep(period);
        }
    });
    Ok(inbox)
}

fn register(db: &Postgres, setup: &Setup) -> Result<(), String> {
    let peer = format!("{}:{}", setup.advertise, port_of(&setup.peer_listen));
    let livestatus = format!("{}:{}", setup.advertise, port_of(&setup.livestatus_tcp));
    rows(
        db,
        "INSERT INTO checkd_nodes (name, config_hash, peer, livestatus, seen) VALUES ($1, $2, $3, $4, now()) \
         ON CONFLICT (name) DO UPDATE SET config_hash = $2, peer = $3, livestatus = $4, seen = now() \
         RETURNING name",
        vec![
            Value::Text(setup.node.clone()),
            Value::Text(setup.config_hash.clone()),
            Value::Text(peer),
            Value::Text(livestatus),
        ],
    )
    .map(|_| ())
}

fn mismatch(db: &Postgres, setup: &Setup) -> Option<String> {
    let others = rows(db, "SELECT name, config_hash FROM checkd_nodes WHERE name <> $1", vec![Value::Text(setup.node.clone())]).ok()?;
    let differing: Vec<String> = others
        .iter()
        .filter(|row| text(&row[1]) != setup.config_hash)
        .map(|row| text(&row[0]))
        .collect();
    (!differing.is_empty()).then(|| {
        format!(
            "Warning: the configuration of node(s) {} is not the same as this one's",
            differing.join(", ")
        )
    })
}

/// Sends a request to the leader's Livestatus over TCP, and gives back
/// what it answers.
async fn to_leader(at: &str, request: &[u8]) -> Option<Vec<u8>> {
    let mut stream = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(at))
        .await
        .ok()?
        .ok()?;
    stream.write_all(request).await.ok()?;
    stream.shutdown().await.ok()?;
    let mut answer = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut answer)).await;
    Some(answer)
}

/// While a standby: what comes in for the engine goes to the leader.
pub struct Forward {
    pub leader: Shared<Option<String>>,
}

impl Forward {
    /// Livestatus connections to this node's socket are passed through to
    /// the leader's TCP port.
    pub fn livestatus(&self, socket: &Path) -> Result<(), String> {
        if socket.exists() {
            let _ = std::fs::remove_file(socket);
        }
        let listener = UnixListener::bind(socket)
            .map_err(|err| format!("cannot make the Livestatus socket {}: {err}", socket.display()))?;
        let leader = self.leader.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else { continue };
                let at = leader.lock().unwrap().clone();
                tokio::spawn(async move {
                    let Some(at) = at else { return };
                    if let Ok(mut upstream) = tokio::net::TcpStream::connect(&at).await {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    }
                });
            }
        });
        Ok(())
    }

    /// An external command, sent to the leader as a Livestatus `COMMAND`.
    pub async fn command(&self, line: &str) -> bool {
        let Some(at) = self.leader.lock().unwrap().clone() else { return false };
        to_leader(&at, format!("COMMAND {}\n\n", line.trim()).as_bytes()).await.is_some()
    }

    /// A check result file, sent to the leader as the passive result it
    /// holds.
    pub async fn result_file(&self, text: &str, now: i64) -> bool {
        let get = |key: &str| {
            text.lines()
                .find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
                .map(str::to_string)
        };
        let (Some(host), Some(code)) = (get("host_name"), get("return_code")) else { return false };
        let output = get("output").unwrap_or_default();
        let line = match get("service_description") {
            Some(service) => format!("[{now}] PROCESS_SERVICE_CHECK_RESULT;{host};{service};{code};{output}"),
            None => format!("[{now}] PROCESS_HOST_CHECK_RESULT;{host};{code};{output}"),
        };
        self.command(&line).await
    }
}

/// Waits as a standby until this node takes the lease, forwarding to the
/// leader meanwhile. The answer is the lease's counter and the lease
/// thread's events, or `None` when a signal said stop first.
#[allow(clippy::too_many_arguments)]
pub async fn standby(
    setup: &Setup,
    role: Shared<Role>,
    socket: Option<&Path>,
    commands: &mut mpsc::UnboundedReceiver<String>,
    spool: Option<&Path>,
    log: &mut dyn FnMut(&str),
    stop: &mut dyn futures_like::Stop,
) -> Result<Option<(i64, mpsc::UnboundedReceiver<Event>)>, String> {
    let mut events = start(setup.clone(), role.clone())?;
    serve_heartbeat(setup.peer_listen.clone(), role.clone()).await?;
    let forward = Forward {
        leader: Arc::new(Mutex::new(None)),
    };
    if let Some(socket) = socket {
        forward.livestatus(socket)?;
    }
    log(&format!(
        "Node {} is a standby: waiting for the lease, and forwarding to the leader meanwhile",
        setup.node
    ));
    let mut reaper = tokio::time::interval(Duration::from_secs(10));
    let mut said_leader: Option<String> = None;
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(Event::Acquired(counter)) => {
                    log(&format!("Node {} took the lease, number {counter}", setup.node));
                    return Ok(Some((counter, events)));
                }
                Some(Event::LeaderAt(at)) => {
                    if at != said_leader {
                        if let Some(at) = &at {
                            log(&format!("The leader answers Livestatus at {at}"));
                        }
                        said_leader = at.clone();
                    }
                    *forward.leader.lock().unwrap() = at;
                }
                Some(Event::Note(note)) => log(&note),
                Some(_) => {}
                None => return Err("the lease thread has stopped".into()),
            },
            Some(line) = commands.recv() => {
                if !forward.command(&line).await {
                    log(&format!("Warning: no leader to forward the command to: {}", line.trim()));
                }
            }
            _ = reaper.tick(), if spool.is_some() => {
                if let Some(dir) = spool {
                    forward_spool(&forward, dir).await;
                }
            }
            _ = stop.wait() => return Ok(None),
        }
    }
}

async fn forward_spool(forward: &Forward, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let names: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_none() && p.with_extension("ok").exists())
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    for path in names {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if forward.result_file(&text, now).await {
            let _ = std::fs::remove_file(path.with_extension("ok"));
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Something to wait on that says stop.
pub mod futures_like {
    use std::future::Future;
    use std::pin::Pin;

    pub trait Stop {
        fn wait(&mut self) -> Pin<Box<dyn Future<Output = ()> + '_>>;
    }
}
