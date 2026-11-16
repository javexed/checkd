//! Remote workers, the engine's side: the job server mod_gearman needed a
//! separate daemon for.
//!
//! Workers connect out to the engine over TLS, say who they are with a
//! token of their own, and get jobs for the pools their entry in the
//! configuration lets them serve. A pool is what mod_gearman calls a queue,
//! and the routing settings keep mod_gearman's names (see `docs/workers.md`).
//!
//! - A check goes to its pool, or runs on the engine's host. A pool with no
//!   worker connected that serves it gives the check a result at once:
//!   UNKNOWN unless `orphan_return` says otherwise, with an output that
//!   names the pool.
//! - Any worker of a pool with a free slot takes the next job. A job that
//!   waits longer than `max_age` gets such a result too. A job held by a
//!   worker that goes away goes to another worker once.
//! - The engine never runs two checks of one host or service at once, so
//!   no two jobs for one are ever out.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use checkd_core::engine::{Engine, Job, JobKind, JobResult, Ms, Obj, Outcome};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

/// One worker's entry: its name, its token, and the pools it may serve.
#[derive(Clone, Debug)]
pub struct Credential {
    pub name: String,
    pub token: String,
    pub pools: Vec<String>,
    /// From the agents directory, whose file is read again at each hello.
    pub agent: bool,
}

/// The routing and the workers, from the file given with `--workers`.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub listen: String,
    /// A listener for workers through an HTTPS proxy or tunnel: WebSocket,
    /// with the same TLS inside.
    pub websocket: Option<String>,
    pub cert: PathBuf,
    pub key: PathBuf,
    pub hostgroups: Vec<String>,
    pub servicegroups: Vec<String>,
    pub localhostgroups: Vec<String>,
    pub localservicegroups: Vec<String>,
    pub queue_custom_variable: Option<String>,
    /// A default pool for host checks, `host`, and for service checks,
    /// `service`.
    pub hosts: bool,
    pub services: bool,
    /// Event handlers go to the pool `eventhandler`; with
    /// `route_eventhandler_like_checks`, to their object's checks' pool.
    /// With neither, they run on the engine's host.
    pub eventhandler: bool,
    pub route_eventhandler_like_checks: bool,
    /// Notification commands go to the pool `notification`.
    pub notifications: bool,
    pub orphan_return: u8,
    /// Milliseconds a job may wait.
    pub max_age: Ms,
    pub workers: Vec<Credential>,
    /// `agents=`: a worker per `<host>.token` file, also looked up when
    /// one says hello, so that a token added or removed counts at once.
    pub agents: Option<PathBuf>,
}

fn list(value: &str) -> Vec<String> {
    value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

fn flag(value: &str) -> bool {
    matches!(value.trim(), "yes" | "on" | "1" | "true")
}

/// Reads the workers' file: `key=value` lines, `#` for comments, and a line
/// per worker: `worker=<name> token=<file> pools=<pool>,<pool>`.
pub fn read_config(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("cannot read {}: {err}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut config = Config {
        listen: "0.0.0.0:4730".into(),
        websocket: None,
        orphan_return: 3,
        max_age: 60_000,
        ..Config::default()
    };
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = || format!("{}:{}: not understood: {line}", path.display(), n + 1);
        let (key, value) = line.split_once('=').ok_or_else(bad)?;
        let value = value.trim();
        match key.trim() {
            "listen" => config.listen = value.to_string(),
            "websocket" => config.websocket = Some(value.to_string()),
            "cert" => config.cert = dir.join(value),
            "key" => config.key = dir.join(value),
            "hostgroups" => config.hostgroups = list(value),
            "servicegroups" => config.servicegroups = list(value),
            "localhostgroups" => config.localhostgroups = list(value),
            "localservicegroups" => config.localservicegroups = list(value),
            "queue_custom_variable" => {
                config.queue_custom_variable = Some(value.trim_start_matches('_').to_uppercase())
            }
            "hosts" => config.hosts = flag(value),
            "services" => config.services = flag(value),
            "eventhandler" => config.eventhandler = flag(value),
            "route_eventhandler_like_checks" => config.route_eventhandler_like_checks = flag(value),
            "notifications" => config.notifications = flag(value),
            "orphan_return" => config.orphan_return = value.parse::<u8>().map_err(|_| bad())?.min(3),
            "max-age" | "max_age" => config.max_age = value.parse::<f64>().map_err(|_| bad())? as Ms * 1000,
            "worker" => {
                let mut words = value.split_whitespace();
                let name = words.next().ok_or_else(bad)?.to_string();
                let (mut token, mut pools) = (None, Vec::new());
                for word in words {
                    match word.split_once('=') {
                        Some(("token", file)) => {
                            let file = dir.join(file);
                            let t = std::fs::read_to_string(&file)
                                .map_err(|err| format!("cannot read the token {}: {err}", file.display()))?;
                            token = Some(t.trim().to_string());
                        }
                        Some(("pools", p)) => pools = list(p),
                        _ => return Err(bad()),
                    }
                }
                config.workers.push(Credential {
                    name,
                    token: token.ok_or_else(bad)?,
                    pools,
                    agent: false,
                });
            }
            "agents" => {
                let agents = dir.join(value);
                config.workers.extend(read_agents(&agents));
                config.agents = Some(agents);
            }
            _ => return Err(bad()),
        }
    }
    Ok(config)
}

/// Whether a name can be an agent's: a host name of letters, digits,
/// `.`, `-` and `_`, as a token file's name and as one word of `HELLO`.
pub fn agent_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// The agents of a directory: each `<host>.token` in it is the worker
/// `<host>` with the pool `host_<host>`, which runs that host's services.
/// A file that cannot be read is left out, and so is a missing directory:
/// one bad token must not stop the engine at a reload.
fn read_agents(dir: &Path) -> Vec<Credential> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("Warning: cannot read the agents in {}", dir.display());
        return Vec::new();
    };
    let mut agents: Vec<Credential> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.strip_suffix(".token")?.to_string();
            agent(dir, &name)
        })
        .collect();
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    agents
}

/// The agent of one host, from its token file, if it has one that can be read.
fn agent(dir: &Path, name: &str) -> Option<Credential> {
    if !agent_name(name) {
        return None;
    }
    let file = dir.join(format!("{name}.token"));
    let token = match std::fs::read_to_string(&file) {
        Ok(text) => text.trim().to_string(),
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                eprintln!("Warning: cannot read the token {}: {err}", file.display());
            }
            return None;
        }
    };
    (!token.is_empty()).then(|| Credential {
        name: name.to_string(),
        token,
        pools: vec![format!("host_{name}")],
        agent: true,
    })
}

/// The agent a token belongs to: by its host's `<host>.token`, with the
/// pool `host_<host>`; or by a new `<host>.new` that is not in use yet,
/// with the pool `waiting_<host>`, which no check goes to. nuthatch makes
/// a `.new` token the host's `.token` once its agent has connected, so a
/// command made and never run moves no checks.
fn agent_for(dir: &Path, name: &str, token: &str) -> Option<Credential> {
    if let Some(known) = agent(dir, name).filter(|w| same(&w.token, token)) {
        return Some(known);
    }
    let new = std::fs::read_to_string(dir.join(format!("{name}.new"))).ok()?.trim().to_string();
    (agent_name(name) && !new.is_empty() && same(&new, token)).then(|| Credential {
        name: name.to_string(),
        token: new,
        pools: vec![format!("waiting_{name}")],
        agent: true,
    })
}

/// Where a job runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    Local,
    Pool(String),
}

fn custom(engine: &Engine, obj: Obj, name: &str) -> Option<String> {
    engine
        .item(obj)
        .cfg
        .custom
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty())
}

/// The pool of an object's checks, as mod_gearman picks the queue: its
/// custom variable (or its host's), then the groups that stay local, then the listed
/// service groups, then its host's listed host groups, then the default
/// pool.
///
/// Before the groups, checkd adds one rule of its own: a service whose host
/// has an agent, a worker entry with the pool `host_<host name>`, runs
/// there, unless its custom variable `_CHECKD_RUN_ON` is `engine`; and one
/// whose `_CHECKD_RUN_ON` is `agent` goes there even without an agent. The
/// host's own check stays where it was, so that a host whose agent is gone
/// still shows as up or down.
pub fn check_route(engine: &Engine, config: &Config, obj: Obj) -> Route {
    let (host, service) = match obj {
        Obj::Host(at) => (at, None),
        Obj::Service(at) => (engine.services()[at].host, Some(at)),
    };
    if let Some(var) = &config.queue_custom_variable {
        // A service without the variable takes its host's.
        if let Some(pool) = custom(engine, obj, var).or_else(|| custom(engine, Obj::Host(host), var)) {
            return Route::Pool(pool);
        }
    }
    // `_CHECKD_RUN_ON engine`: a check of the host from outside (a port, a
    // web page, a certificate), which its agent would make from inside.
    // `_CHECKD_RUN_ON agent`: a check of the machine itself (its disks),
    // which the engine would make of its own: without an agent it is the
    // pool's orphan, never run here.
    let run_on = custom(engine, obj, "CHECKD_RUN_ON").map(|v| v.to_ascii_lowercase());
    if service.is_some() && run_on.as_deref() != Some("engine") {
        let pool = format!("host_{}", engine.hosts()[host].name);
        if run_on.as_deref() == Some("agent") || config.workers.iter().any(|w| w.pools.contains(&pool)) {
            return Route::Pool(pool);
        }
    }
    let host_groups = &engine.hosts()[host].groups;
    if let Some(at) = service {
        let groups = &engine.services()[at].groups;
        if groups.iter().any(|g| config.localservicegroups.contains(g)) {
            return Route::Local;
        }
    }
    if host_groups.iter().any(|g| config.localhostgroups.contains(g)) {
        return Route::Local;
    }
    if let Some(at) = service {
        for wanted in &config.servicegroups {
            if engine.services()[at].groups.contains(wanted) {
                return Route::Pool(format!("servicegroup_{wanted}"));
            }
        }
    }
    for wanted in &config.hostgroups {
        if host_groups.contains(wanted) {
            return Route::Pool(format!("hostgroup_{wanted}"));
        }
    }
    match (service, config.hosts, config.services) {
        (None, true, _) => Route::Pool("host".into()),
        (Some(_), _, true) => Route::Pool("service".into()),
        _ => Route::Local,
    }
}

/// Where a job runs.
pub fn route(engine: &Engine, config: &Config, job: &Job) -> Route {
    match job.kind {
        JobKind::HostCheck | JobKind::ServiceCheck => check_route(engine, config, job.obj),
        JobKind::HostHandler | JobKind::ServiceHandler if config.route_eventhandler_like_checks => {
            check_route(engine, config, job.obj)
        }
        JobKind::HostHandler | JobKind::ServiceHandler if config.eventhandler => Route::Pool("eventhandler".into()),
        JobKind::Notification if config.notifications => Route::Pool("notification".into()),
        _ => Route::Local,
    }
}

/// What a connection tells the engine's loop.
pub enum Event {
    Joined {
        id: u64,
        name: String,
        pools: Vec<String>,
        slots: usize,
        send: mpsc::UnboundedSender<Vec<u8>>,
    },
    Slots(u64, usize),
    Result(u64, u64, JobResult),
    Left(u64),
}

/// Listens for workers. Their events come out of the answer.
pub async fn listen(config: &Config) -> Result<mpsc::UnboundedReceiver<Event>, String> {
    let acceptor = crate::tls::acceptor(&config.cert, &config.key)?;
    let listener = TcpListener::bind(&config.listen)
        .await
        .map_err(|err| format!("cannot listen for workers at {}: {err}", config.listen))?;
    // Behind an HTTPS proxy or tunnel: WebSocket, with the same TLS inside.
    let websocket = match &config.websocket {
        Some(at) => Some(
            TcpListener::bind(at)
                .await
                .map_err(|err| format!("cannot listen for workers' WebSockets at {at}: {err}"))?,
        ),
        None => None,
    };
    let (events, inbox) = mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        acceptor,
        events,
        workers: config.workers.clone(),
        agents: config.agents.clone(),
        next_id: AtomicU64::new(0),
    });
    let direct = shared.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let shared = direct.clone();
            tokio::spawn(async move { shared.take(stream).await });
        }
    });
    if let Some(websocket) = websocket {
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = websocket.accept().await else { continue };
                let shared = shared.clone();
                tokio::spawn(async move {
                    if let Ok(Ok(stream)) = tokio::time::timeout(std::time::Duration::from_secs(10), crate::ws::server(stream)).await {
                        shared.take(stream).await
                    }
                });
            }
        });
    }
    Ok(inbox)
}

struct Shared {
    acceptor: tokio_rustls::TlsAcceptor,
    events: mpsc::UnboundedSender<Event>,
    workers: Vec<Credential>,
    agents: Option<PathBuf>,
    next_id: AtomicU64,
}

impl Shared {
    /// One worker's connection, from its TLS handshake to its end.
    async fn take<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static>(&self, stream: S) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (events, workers, agents) = (self.events.clone(), &self.workers, self.agents.clone());
        let Ok(Ok(stream)) = tokio::time::timeout(std::time::Duration::from_secs(10), self.acceptor.accept(stream)).await else {
            return;
        };
        let (read, mut write) = tokio::io::split(stream);
        let mut reader = BufReader::new(read);
        let mut hello = String::new();
        // A line of a few words: no more is read before the token is known.
        // And within a while of the handshake: a peer that says nothing holds no task.
        let said = tokio::time::timeout(std::time::Duration::from_secs(10), (&mut reader).take(1024).read_line(&mut hello)).await;
        if !matches!(said, Ok(Ok(_))) {
            return;
        }
        let words: Vec<&str> = hello.split_whitespace().collect();
        let known = match words.as_slice() {
            ["HELLO", name, token, slots] => workers
                .iter()
                .find(|w| w.name == *name && !w.agent && same(&w.token, token))
                .cloned()
                // An agent's token is read now, not at the last reload.
                .or_else(|| agents.as_deref().and_then(|dir| agent_for(dir, name, token)))
                .map(|w| (w, slots.parse::<usize>().unwrap_or(1))),
            _ => None,
        };
        let Some((worker, slots)) = known else {
            let _ = write.write_all(b"REFUSED unknown worker or wrong token\n").await;
            return;
        };
        // An agent is asked for its inventory, which nuthatch reads beside its token.
        let inventory = agents.clone().filter(|_| worker.agent).map(|dir| dir.join(format!("{}.inventory.json", worker.name)));
        let welcome = format!("WELCOME {}{}\n", worker.pools.join(","), if inventory.is_some() { " inventory" } else { "" });
        if write.write_all(welcome.as_bytes()).await.is_err() {
            return;
        }
        let (send, mut outbox) = mpsc::unbounded_channel::<Vec<u8>>();
        let _ = events.send(Event::Joined {
            id,
            name: worker.name.clone(),
            pools: worker.pools.clone(),
            slots,
            send,
        });
        let writer = tokio::spawn(async move {
            while let Some(bytes) = outbox.recv().await {
                if write.write_all(&bytes).await.is_err() {
                    return;
                }
            }
        });
        // An agent whose token file goes or changes is let go
        // within seconds, reload or not.
        let own_file = agents.clone().filter(|_| worker.agent);
        let mut check = tokio::time::interval(std::time::Duration::from_secs(5));
        let mut line = String::new();
        let mut quiet_until = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            // read_line keeps what it read when another branch wins,
            // in `line`, which lives across turns.
            let n = tokio::select! {
                read = reader.read_line(&mut line) => match read {
                    Ok(n) => n,
                    Err(_) => break,
                },
                _ = tokio::time::sleep_until(quiet_until) => break,
                _ = check.tick() => {
                    if let Some(dir) = &own_file {
                        if !agent_for(dir, &worker.name, &worker.token).is_some_and(|now| now.pools == worker.pools) {
                            break;
                        }
                    }
                    continue;
                }
            };
            if n == 0 {
                break;
            }
            quiet_until = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            let line = std::mem::take(&mut line);
            let words: Vec<&str> = line.split_whitespace().collect();
            match words.as_slice() {
                ["PING"] => {}
                ["INVENTORY", len] => {
                    let Some(file) = &inventory else { break };
                    let Ok(len) = len.parse::<usize>() else { break };
                    if len > MAX_INVENTORY {
                        break;
                    }
                    let mut body = vec![0u8; len];
                    if reader.read_exact(&mut body).await.is_err() {
                        break;
                    }
                    if let Err(err) = keep_inventory(file, &body) {
                        eprintln!("Warning: cannot keep the inventory of agent {}: {err}", worker.name);
                    }
                }
                ["SLOTS", n] => {
                    let _ = events.send(Event::Slots(id, n.parse().unwrap_or(0)));
                }
                ["RESULT", job, kind, code, out_len, err_len] => {
                    let (Ok(out_len), Ok(err_len)) = (out_len.parse::<usize>(), err_len.parse::<usize>()) else {
                        break;
                    };
                    let mut out = vec![0u8; out_len.min(1 << 21)];
                    let mut err = vec![0u8; err_len.min(1 << 21)];
                    if reader.read_exact(&mut out).await.is_err() || reader.read_exact(&mut err).await.is_err() {
                        break;
                    }
                    let code: i32 = code.parse().unwrap_or(3);
                    let stderr = String::from_utf8_lossy(&err).into_owned();
                    let outcome = match *kind {
                        "exit" => Outcome::Exited(code),
                        "signal" => Outcome::Signaled(code),
                        "timeout" => Outcome::TimedOut,
                        _ => Outcome::Failed(stderr.clone()),
                    };
                    let result = JobResult {
                        outcome,
                        stdout: String::from_utf8_lossy(&out).into_owned(),
                        stderr,
                    };
                    let _ = events.send(Event::Result(id, job.parse().unwrap_or(0), result));
                }
                _ => break,
            }
        }
        writer.abort();
        let _ = events.send(Event::Left(id));
    }
}

/// The most an agent's inventory may be.
const MAX_INVENTORY: usize = 1 << 20;

/// Writes an agent's inventory beside its token, whole or not at all. It
/// must be a JSON object; what is in it is the agent's word, for nuthatch
/// to read as such.
fn keep_inventory(file: &Path, body: &[u8]) -> Result<(), String> {
    if body.first() != Some(&b'{') || body.last() != Some(&b'}') || std::str::from_utf8(body).is_err() {
        return Err("not a JSON object".to_string());
    }
    let part = file.with_extension("json.part");
    std::fs::write(&part, body).map_err(|err| err.to_string())?;
    std::fs::rename(&part, file).map_err(|err| err.to_string())
}

/// Compares tokens in a time that does not tell how much of them matched.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() != b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

struct Worker {
    name: String,
    pools: Vec<String>,
    slots: usize,
    send: mpsc::UnboundedSender<Vec<u8>>,
    /// The jobs it holds: id, and the job with its pool and tries.
    running: BTreeMap<u64, Waiting>,
}

#[derive(Clone)]
struct Waiting {
    job: Job,
    pool: String,
    /// When it was taken, and when it was last handed to a worker.
    since: Ms,
    sent: Ms,
    tries: u32,
}

/// The pools and the workers, kept by the engine's loop.
pub struct Pools {
    config: Config,
    workers: BTreeMap<u64, Worker>,
    waiting: BTreeMap<String, VecDeque<Waiting>>,
    turn: u64,
}

/// A result the pools give a job themselves: the pool has no worker, or the
/// job waited too long.
pub struct Given {
    pub id: u64,
    pub result: JobResult,
}

impl Pools {
    pub fn new(config: Config) -> Pools {
        Pools {
            config,
            workers: BTreeMap::new(),
            waiting: BTreeMap::new(),
            turn: 0,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    fn orphan(&self, id: u64, why: String) -> Given {
        Given {
            id,
            result: JobResult {
                outcome: Outcome::Exited(self.config.orphan_return as i32),
                stdout: format!("({why})\n"),
                stderr: String::new(),
            },
        }
    }

    fn served(&self, pool: &str) -> bool {
        self.workers.values().any(|w| w.pools.iter().any(|p| p == pool))
    }

    /// Takes a job for a pool. A result is given at once when no worker
    /// serves the pool.
    pub fn submit(&mut self, job: Job, pool: String, now: Ms) -> Vec<Given> {
        if !self.served(&pool) {
            return vec![self.orphan(job.id, format!("No worker is connected for pool {pool}"))];
        }
        self.waiting.entry(pool.clone()).or_default().push_back(Waiting {
            job,
            pool,
            since: now,
            sent: 0,
            tries: 0,
        });
        self.dispatch(now);
        Vec::new()
    }

    /// Hands waiting jobs to workers with free slots, taking turns.
    fn dispatch(&mut self, now: Ms) {
        loop {
            let mut sent = false;
            let ids: Vec<u64> = self.workers.keys().copied().collect();
            let n = ids.len().max(1);
            for k in 0..ids.len() {
                let id = ids[(self.turn as usize + k) % n];
                let worker = self.workers.get(&id).unwrap();
                if worker.running.len() >= worker.slots {
                    continue;
                }
                let pools = worker.pools.clone();
                let next = pools.iter().find_map(|p| {
                    let queue = self.waiting.get_mut(p)?;
                    queue.pop_front()
                });
                let Some(mut waiting) = next else { continue };
                waiting.sent = now;
                let line = waiting.job.line.as_bytes();
                let mut frame = format!("JOB {} {} {}\n", waiting.job.id, waiting.job.timeout, line.len()).into_bytes();
                frame.extend_from_slice(line);
                let worker = self.workers.get_mut(&id).unwrap();
                if worker.send.send(frame).is_ok() {
                    worker.running.insert(waiting.job.id, waiting);
                    sent = true;
                }
                self.turn = self.turn.wrapping_add(1);
            }
            if !sent {
                return;
            }
        }
    }

    /// What a connection told. The answer is the results to give jobs, and
    /// lines for the log.
    pub fn event(&mut self, event: Event, now: Ms) -> (Vec<Given>, Vec<String>) {
        let mut given = Vec::new();
        let mut notes = Vec::new();
        match event {
            Event::Joined {
                id,
                name,
                pools,
                slots,
                send,
            } => {
                notes.push(format!("Worker {name} joined, for pools {}", pools.join(", ")));
                self.workers.insert(
                    id,
                    Worker {
                        name,
                        pools,
                        slots,
                        send,
                        running: BTreeMap::new(),
                    },
                );
            }
            Event::Slots(id, slots) => {
                if let Some(worker) = self.workers.get_mut(&id) {
                    worker.slots = slots;
                }
            }
            Event::Result(id, job, result) => {
                if let Some(worker) = self.workers.get_mut(&id) {
                    if worker.running.remove(&job).is_some() {
                        given.push(Given { id: job, result });
                    }
                }
            }
            Event::Left(id) => {
                if let Some(worker) = self.workers.remove(&id) {
                    notes.push(format!("Worker {} left", worker.name));
                    for (_, mut waiting) in worker.running {
                        // Once more, to another worker.
                        if waiting.tries == 0 && self.served(&waiting.pool) {
                            waiting.tries += 1;
                            self.waiting.entry(waiting.pool.clone()).or_default().push_front(waiting);
                        } else {
                            let why = format!("The worker that had the job left, in pool {}", waiting.pool);
                            given.push(self.orphan(waiting.job.id, why));
                        }
                    }
                    // Jobs of pools nobody serves any more.
                    let pools: Vec<String> = self.waiting.keys().cloned().collect();
                    for pool in pools {
                        if !self.served(&pool) {
                            for waiting in self.waiting.remove(&pool).unwrap_or_default() {
                                given.push(self.orphan(waiting.job.id, format!("No worker is connected for pool {pool}")));
                            }
                        }
                    }
                }
            }
        }
        self.dispatch(now);
        (given, notes)
    }

    /// Jobs that waited longer than `max_age`, and jobs a worker has held
    /// ten seconds past their timeout without an answer: those are given
    /// up, as a check the engine ran itself would have been stopped.
    pub fn expire(&mut self, now: Ms) -> Vec<Given> {
        let max = self.config.max_age;
        let mut given = Vec::new();
        for worker in self.workers.values_mut() {
            let late: Vec<u64> = worker
                .running
                .iter()
                .filter(|(_, w)| now - w.sent > w.job.timeout + 10_000)
                .map(|(id, _)| *id)
                .collect();
            for id in late {
                worker.running.remove(&id);
                given.push(Given {
                    id,
                    result: JobResult {
                        outcome: Outcome::TimedOut,
                        stdout: String::new(),
                        stderr: String::new(),
                    },
                });
            }
        }
        let mut expired = Vec::new();
        for (pool, queue) in &mut self.waiting {
            while queue.front().is_some_and(|w| now - w.since > max) {
                let waiting = queue.pop_front().unwrap();
                expired.push((waiting.job.id, pool.clone()));
            }
        }
        for (id, pool) in expired {
            given.push(self.orphan(id, format!("The job waited longer than max_age in pool {pool}")));
        }
        given
    }

    /// For `/metrics`: per pool, the workers connected, the jobs waiting,
    /// the jobs running and the age of the oldest job waiting, in seconds.
    pub fn stats(&self, now: Ms) -> Vec<(String, usize, usize, usize, f64)> {
        let mut pools: Vec<String> = self.workers.values().flat_map(|w| w.pools.clone()).collect();
        pools.extend(self.waiting.keys().cloned());
        pools.sort();
        pools.dedup();
        pools
            .into_iter()
            .map(|pool| {
                let workers = self.workers.values().filter(|w| w.pools.contains(&pool)).count();
                let waiting = self.waiting.get(&pool).map_or(0, |q| q.len());
                let running = self.workers.values().flat_map(|w| w.running.values()).filter(|w| w.pool == pool).count();
                let oldest = self
                    .waiting
                    .get(&pool)
                    .and_then(|q| q.front())
                    .map_or(0.0, |w| (now - w.since) as f64 / 1000.0);
                (pool, workers, waiting, running, oldest)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u64) -> Job {
        Job {
            id,
            kind: JobKind::ServiceCheck,
            obj: Obj::Service(0),
            line: "/bin/true".into(),
            timeout: 5_000, about: None, env: Vec::new()
        }
    }

    fn join(pools: &mut Pools, id: u64, pool: &str, slots: usize) -> mpsc::UnboundedReceiver<Vec<u8>> {
        let (send, inbox) = mpsc::unbounded_channel();
        pools.event(
            Event::Joined {
                id,
                name: format!("w{id}"),
                pools: vec![pool.into()],
                slots,
                send,
            },
            0,
        );
        inbox
    }

    fn sent(inbox: &mut mpsc::UnboundedReceiver<Vec<u8>>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(frame) = inbox.try_recv() {
            out.push(String::from_utf8_lossy(&frame).lines().next().unwrap_or("").to_string());
        }
        out
    }

    fn config() -> Config {
        Config {
            orphan_return: 2,
            max_age: 10_000,
            ..Config::default()
        }
    }

    #[test]
    fn an_agents_directory_gives_a_worker_per_token_file() {
        let dir = std::env::temp_dir().join(format!("checkd-agents-{}", std::process::id()));
        let agents = dir.join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join("win1.token"), "abc\n").unwrap();
        std::fs::write(agents.join("bad name.token"), "abc\n").unwrap();
        std::fs::write(agents.join("empty.token"), "\n").unwrap();
        std::fs::write(agents.join("notes.txt"), "abc\n").unwrap();
        std::fs::write(dir.join("workers.cfg"), "agents=agents\n").unwrap();
        std::fs::create_dir_all(agents.join("dir.token")).unwrap();
        let config = read_config(&dir.join("workers.cfg")).unwrap();
        assert_eq!(config.workers.len(), 1);
        assert_eq!(config.workers[0].name, "win1");
        assert_eq!(config.workers[0].token, "abc");
        assert_eq!(config.workers[0].pools, vec!["host_win1".to_string()]);
        // A new token is let in, but to no pool a check goes to.
        std::fs::write(agents.join("lin1.new"), "def\n").unwrap();
        assert_eq!(read_config(&dir.join("workers.cfg")).unwrap().workers.len(), 1);
        assert_eq!(agent_for(&agents, "lin1", "def").unwrap().pools, vec!["waiting_lin1".to_string()]);
        assert!(agent_for(&agents, "lin1", "abc").is_none());
        assert_eq!(agent_for(&agents, "win1", "abc").unwrap().pools, vec!["host_win1".to_string()]);
        // A missing directory is no agents, not an error.
        std::fs::write(dir.join("other.cfg"), "agents=nowhere\n").unwrap();
        assert!(read_config(&dir.join("other.cfg")).unwrap().workers.is_empty());
    }

    #[test]
    fn an_inventory_is_kept_whole_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!("checkd-inventory-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("lin1.inventory.json");
        keep_inventory(&file, br#"{"version":1}"#).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), r#"{"version":1}"#);
        assert!(keep_inventory(&file, b"rm -rf /").is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), r#"{"version":1}"#);
        assert!(!dir.join("lin1.inventory.json.part").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_hosts_agent_takes_its_services_but_not_its_host_check() {
        let dir = std::env::temp_dir().join(format!("checkd-agent-route-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("objects.cfg"),
            "define command {\n command_name ok\n command_line /bin/true\n}\n\
             define timeperiod {\n timeperiod_name always\n alias always\n monday 00:00-24:00\n}\n\
             define host {\n host_name win1\n address 192.0.2.1\n check_command ok\n max_check_attempts 1\n check_period always\n}\n\
             define host {\n host_name lin1\n address 192.0.2.2\n check_command ok\n max_check_attempts 1\n check_period always\n}\n\
             define service {\n host_name win1,lin1\n service_description disk\n check_command ok\n max_check_attempts 1\n check_period always\n}\n\
             define service {\n host_name win1\n service_description rdp\n check_command ok\n max_check_attempts 1\n check_period always\n _CHECKD_RUN_ON engine\n}\n\
             define service {\n host_name lin1\n service_description memory\n check_command ok\n max_check_attempts 1\n check_period always\n _CHECKD_RUN_ON agent\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("naemon.cfg"), "cfg_file=objects.cfg\n").unwrap();
        let loaded = checkd_config::load(&dir.join("naemon.cfg"), &Default::default());
        let (Some(main), Some(objects)) = (loaded.main, loaded.objects) else { panic!("{:?}", loaded.diags.errors) };
        let engine = Engine::new(objects, &main, Box::new(checkd_core::timeperiod::Fixed(0)), 1_800_000_000_000).0;
        let config = Config {
            workers: vec![Credential { name: "win1".into(), token: "t".into(), pools: vec!["host_win1".into()], agent: false }],
            ..config()
        };
        let win1 = engine.hosts().iter().position(|h| h.name == "win1").unwrap();
        let on = |host: usize| engine.services().iter().position(|s| s.host == host).unwrap();
        let lin1 = engine.hosts().iter().position(|h| h.name == "lin1").unwrap();
        assert_eq!(check_route(&engine, &config, Obj::Service(on(win1))), Route::Pool("host_win1".into()));
        assert_eq!(check_route(&engine, &config, Obj::Host(win1)), Route::Local);
        assert_eq!(check_route(&engine, &config, Obj::Service(on(lin1))), Route::Local);
        // A check from outside stays on the engine.
        let rdp = engine.services().iter().position(|s| s.description == "rdp").unwrap();
        assert_eq!(check_route(&engine, &config, Obj::Service(rdp)), Route::Local);
        // A check of the machine itself waits for an agent, never runs here.
        let memory = engine.services().iter().position(|s| s.description == "memory").unwrap();
        assert_eq!(check_route(&engine, &config, Obj::Service(memory)), Route::Pool("host_lin1".into()));
    }

    #[test]
    fn a_pool_without_workers_answers_at_once() {
        let mut pools = Pools::new(config());
        let given = pools.submit(job(1), "dmz".into(), 0);
        assert_eq!(given.len(), 1);
        assert_eq!(given[0].result.outcome, Outcome::Exited(2));
        assert!(given[0].result.stdout.contains("pool dmz"));
    }

    #[test]
    fn jobs_go_to_free_slots_and_wait_for_them() {
        let mut pools = Pools::new(config());
        let mut a = join(&mut pools, 1, "dmz", 1);
        assert!(pools.submit(job(1), "dmz".into(), 0).is_empty());
        assert!(pools.submit(job(2), "dmz".into(), 0).is_empty());
        assert_eq!(sent(&mut a), vec!["JOB 1 5000 9"]);
        let result = JobResult {
            outcome: Outcome::Exited(0),
            stdout: "OK".into(),
            stderr: String::new(),
        };
        let (given, _) = pools.event(Event::Result(1, 1, result), 100);
        assert_eq!(given.len(), 1);
        assert_eq!(sent(&mut a), vec!["JOB 2 5000 9"]);
        // A result for a job the worker does not hold is dropped.
        let stray = JobResult {
            outcome: Outcome::Exited(0),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(pools.event(Event::Result(1, 7, stray), 100).0.is_empty());
    }

    #[test]
    fn a_job_of_a_worker_that_left_goes_to_another_once() {
        let mut pools = Pools::new(config());
        let mut a = join(&mut pools, 1, "dmz", 1);
        let mut b = join(&mut pools, 2, "dmz", 1);
        pools.submit(job(1), "dmz".into(), 0);
        let first = if sent(&mut a).is_empty() { 2 } else { 1 };
        let _ = sent(&mut b);
        let (given, notes) = pools.event(Event::Left(first), 10);
        assert!(given.is_empty());
        assert!(notes[0].contains("left"));
        let other = if first == 1 { &mut b } else { &mut a };
        assert_eq!(sent(other), vec!["JOB 1 5000 9"]);
        // The second time, the job gets a result.
        let (given, _) = pools.event(Event::Left(3 - first), 20);
        assert_eq!(given.len(), 1);
        assert!(given[0].result.stdout.contains("left"));
    }

    #[test]
    fn old_and_late_jobs_are_given_up() {
        let mut pools = Pools::new(config());
        let _a = join(&mut pools, 1, "dmz", 1);
        pools.submit(job(1), "dmz".into(), 0);
        pools.submit(job(2), "dmz".into(), 0);
        let given = pools.expire(10_001);
        assert_eq!(given.iter().map(|g| g.id).collect::<Vec<_>>(), vec![2]);
        let given = pools.expire(15_001);
        assert_eq!(given.len(), 1);
        assert_eq!(given[0].result.outcome, Outcome::TimedOut);
        assert_eq!(pools.stats(15_001), vec![("dmz".to_string(), 1, 0, 0, 0.0)]);
    }

    #[test]
    fn reads_a_workers_file() {
        let dir = std::env::temp_dir().join(format!("checkd-workers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dmz.token"), "s3cret\n").unwrap();
        std::fs::write(
            dir.join("workers.cfg"),
            "# pools\nhostgroups=dmz, lab\nservices=yes\nqueue_custom_variable=_WORKER\nmax-age=30\n\
             notifications=yes\nworker=w1 token=dmz.token pools=hostgroup_dmz,service\n",
        )
        .unwrap();
        let config = read_config(&dir.join("workers.cfg")).unwrap();
        assert_eq!(config.hostgroups, vec!["dmz", "lab"]);
        assert!(config.services && !config.hosts);
        assert_eq!(config.queue_custom_variable.as_deref(), Some("WORKER"));
        assert_eq!(config.max_age, 30_000);
        assert!(config.notifications && !config.eventhandler);
        assert_eq!(config.workers[0].token, "s3cret");
        assert_eq!(config.workers[0].pools, vec!["hostgroup_dmz", "service"]);
        assert!(read_config(&dir.join("dmz.token")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
