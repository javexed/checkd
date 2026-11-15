//! The store: where the engine's state is kept over a restart, and where
//! the log goes with a time index for reports.
//!
//! The driver hands the store a snapshot of the engine after a batch of
//! events. The store compares it with what it last wrote and writes only
//! what changed, in one transaction, with the log lines of the batch. A
//! batch with a notification, a change of state or a command is written
//! before the commands it asks for are started, so that a notification is
//! on record before it is sent.
//!
//! Two backends take the same SQL: SQLite, one file, the default; and
//! PostgreSQL (`postgres.rs`). The tables can change between versions.
//! The views whose names start with `v1_` are the documented ones; see
//! `docs/store.md`.

use std::collections::BTreeMap;

use checkd_core::snapshot::{AckState, CommentState, DowntimeState, ObjState, Snapshot, Setting};

/// The version of the tables is 1 + the number of migrations. An older
/// store is migrated at start; a newer one is refused.
///
/// What takes a store from version n + 1 to n + 2, at index n. Each step
/// is SQL that SQLite and PostgreSQL both take, written out in full (no
/// `{REAL}` or `{ID}`; `BIGINT`, not `INTEGER`); a step that changes what
/// a view reads drops the view, and the schema makes it again. A step,
/// once released, is never changed: a change is a new step, with a test
/// from a store of the version before it.
pub const MIGRATIONS: &[&[&str]] = &[];

/// Makes the tables of a new store, or brings an older one up to date, in
/// one transaction per store: the tables are made after the migrations,
/// so that the views see the new columns.
fn prepare(backend: &mut dyn Backend, steps: &[&[&'static str]]) -> Result<(), String> {
    let current = 1 + steps.len() as i64;
    backend.apply(&[Statement { sql: "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)", values: vec![] }])?;
    let rows = backend.query("SELECT value FROM meta WHERE key = 'schema_version'")?;
    let found = match rows.first().and_then(|row| row.first()) {
        None => None,
        Some(Value::Text(text)) => match text.parse::<i64>() {
            Ok(n) if n >= 1 => Some(n),
            _ => return Err(format!("{}: the store's version {text:?} is not a version", backend.describe())),
        },
        Some(other) => return Err(format!("{}: the store's version {other:?} is not a version", backend.describe())),
    };
    match found {
        // A new store.
        None => {
            backend.create()?;
            backend.apply(&[Statement {
                sql: "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                values: vec![current.to_string().into()],
            }])
        }
        Some(n) if n > current => Err(format!(
            "{}: the store is of version {n}, from a newer checkd; this checkd knows version {current}",
            backend.describe()
        )),
        Some(n) => {
            if n < current {
                let mut statements: Vec<Statement> =
                    steps[(n - 1) as usize..].iter().flat_map(|step| step.iter()).map(|sql| Statement { sql, values: vec![] }).collect();
                statements.push(Statement { sql: "UPDATE meta SET value = ?1 WHERE key = 'schema_version'", values: vec![current.to_string().into()] });
                backend.apply(&statements).map_err(|err| format!("migrating the store from version {n} to {current}: {err}"))?;
            }
            backend.create()
        }
    }
}

/// A value bound to a statement.
#[derive(Clone, Debug)]
pub enum Value {
    Int(i64),
    Real(f64),
    Text(String),
    Null,
}

impl From<i64> for Value {
    fn from(v: i64) -> Value {
        Value::Int(v)
    }
}
impl From<u64> for Value {
    fn from(v: u64) -> Value {
        Value::Int(v as i64)
    }
}
impl From<u32> for Value {
    fn from(v: u32) -> Value {
        Value::Int(v as i64)
    }
}
impl From<u8> for Value {
    fn from(v: u8) -> Value {
        Value::Int(v as i64)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Value {
        Value::Int(v as i64)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Value {
        Value::Real(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Value {
        Value::Text(v.to_string())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Value {
        Value::Text(v)
    }
}
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Value {
        v.map(Into::into).unwrap_or(Value::Null)
    }
}

/// One statement with its values. `?1`, `?2` ... in the text.
pub struct Statement {
    pub sql: &'static str,
    pub values: Vec<Value>,
}

/// A row read back.
pub type Row = Vec<Value>;

/// What a backend has to do.
pub trait Backend {
    /// Runs the statements in one transaction.
    fn apply(&mut self, statements: &[Statement]) -> Result<(), String>;
    /// Runs a query and gives its rows.
    fn query(&mut self, sql: &str) -> Result<Vec<Row>, String>;
    /// Makes the tables and views when they are not there.
    fn create(&mut self) -> Result<(), String>;
    fn describe(&self) -> String;
    /// From now on, writes are made only while this node holds the lease
    /// with this counter. Only PostgreSQL has a lease.
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    fn set_fence(&mut self, _holder: &str, _counter: i64) {}
}

/// A store: a backend and what was last written to it.
pub struct Store {
    backend: Box<dyn Backend>,
    objects: BTreeMap<(String, String), ObjState>,
    comments: BTreeMap<u64, CommentState>,
    downtimes: BTreeMap<u64, DowntimeState>,
    meta: Option<(u64, u64, bool)>,
    program: Option<Vec<Setting>>,
    /// Trend readings not yet written: the new ones, and for each trend
    /// the time before which its readings are no longer kept.
    trend_added: Vec<(String, i64, f64)>,
    trend_kept_from: BTreeMap<String, i64>,
    /// Whether the tables were made and the version checked: at once for
    /// `new`, at the first use that reaches the database for `deferred`.
    ready: bool,
}

impl Store {
    pub fn new(backend: Box<dyn Backend>) -> Result<Store, String> {
        let mut store = Store::deferred(backend);
        store.ensure()?;
        Ok(store)
    }

    /// A store whose database need not answer yet: the tables are made
    /// and the version checked when it first does.
    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    pub fn deferred(backend: Box<dyn Backend>) -> Store {
        Store {
            backend,
            objects: BTreeMap::new(),
            comments: BTreeMap::new(),
            downtimes: BTreeMap::new(),
            meta: None,
            program: None,
            trend_added: Vec::new(),
            trend_kept_from: BTreeMap::new(),
            ready: false,
        }
    }

    fn ensure(&mut self) -> Result<(), String> {
        if self.ready {
            return Ok(());
        }
        prepare(self.backend.as_mut(), MIGRATIONS)?;
        self.ready = true;
        Ok(())
    }

    pub fn describe(&self) -> String {
        self.backend.describe()
    }

    #[cfg_attr(not(feature = "postgres"), allow(dead_code))]
    pub fn set_fence(&mut self, holder: &str, counter: i64) {
        self.backend.set_fence(holder, counter);
    }

    /// What the store holds, or `None` when it holds no state yet.
    pub fn load(&mut self) -> Result<Option<Snapshot>, String> {
        self.ensure()?;
        let meta = self.backend.query("SELECT key, value FROM meta")?;
        let get = |name: &str| {
            meta.iter()
                .find(|row| matches!(&row[0], Value::Text(key) if key == name))
                .map(|row| text(&row[1]))
        };
        if get("saved_at").is_none() {
            return Ok(None);
        }
        let mut snapshot = Snapshot {
            next_comment: get("next_comment").and_then(|v| v.parse().ok()).unwrap_or(0),
            next_downtime: get("next_downtime").and_then(|v| v.parse().ok()).unwrap_or(0),
            notifications_enabled: get("notifications_enabled").is_none_or(|v| v != "0"),
            ..Snapshot::default()
        };
        for row in self.backend.query(&format!("SELECT {OBJECT_COLUMNS} FROM objects"))? {
            snapshot.objects.push(object_from(&row));
        }
        // The other settings commands changed, by object.
        let mut settings: BTreeMap<(String, String), Vec<Setting>> = BTreeMap::new();
        for row in self.backend.query("SELECT host, service, name, value, configured FROM settings ORDER BY name")? {
            settings.entry((text(&row[0]), text(&row[1]))).or_default().push(Setting {
                name: text(&row[2]),
                value: text(&row[3]),
                configured: text(&row[4]),
            });
        }
        for object in &mut snapshot.objects {
            let key = (object.host.clone(), object.service.clone().unwrap_or_default());
            object.settings = settings.remove(&key).unwrap_or_default();
        }
        // The program's: `program.<name>` = `<value>\t<what the configuration said>`.
        for row in &meta {
            let (Value::Text(key), Value::Text(value)) = (&row[0], &row[1]) else { continue };
            if let (Some(name), Some((value, configured))) = (key.strip_prefix("program."), value.split_once('\t')) {
                snapshot.program.push(Setting { name: name.to_string(), value: value.to_string(), configured: configured.to_string() });
            }
        }
        for row in self.backend.query(&format!("SELECT {COMMENT_COLUMNS} FROM comments"))? {
            snapshot.comments.push(comment_from(&row));
        }
        for row in self.backend.query(&format!("SELECT {DOWNTIME_COLUMNS} FROM downtimes"))? {
            snapshot.downtimes.push(downtime_from(&row));
        }
        // What was read is what the store holds now.
        self.objects = snapshot
            .objects
            .iter()
            .map(|o| ((o.host.clone(), o.service.clone().unwrap_or_default()), o.clone()))
            .collect();
        self.comments = snapshot.comments.iter().map(|c| (c.id, c.clone())).collect();
        self.downtimes = snapshot.downtimes.iter().map(|d| (d.id, d.clone())).collect();
        self.meta = Some((snapshot.next_comment, snapshot.next_downtime, snapshot.notifications_enabled));
        self.program = Some(snapshot.program.clone());
        Ok(Some(snapshot))
    }

    /// The newest lines of the log, at most `limit`, oldest first: what
    /// the Livestatus log table and the state history start from after a
    /// restart.
    pub fn recent_log(&mut self, limit: usize) -> Result<Vec<(i64, String)>, String> {
        self.ensure()?;
        let rows = self.backend.query(&format!("SELECT time, line FROM log ORDER BY id DESC LIMIT {limit}"))?;
        let mut lines: Vec<(i64, String)> = rows
            .iter()
            .map(|row| (if let Value::Int(time) = &row[0] { *time } else { 0 }, text(&row[1])))
            .collect();
        lines.reverse();
        Ok(lines)
    }

    /// The readings of every trend, by its line, oldest first, from
    /// `since` (milliseconds) on. Older ones are removed: a trend keeps
    /// at most 60 days, and one no longer configured is gone after that.
    pub fn trend_readings(&mut self, since: i64) -> Result<Vec<(String, i64, f64)>, String> {
        self.ensure()?;
        self.backend.apply(&[Statement { sql: "DELETE FROM trend_samples WHERE at < ?1", values: vec![since.into()] }])?;
        let rows = self.backend.query(&format!("SELECT line, at, value FROM trend_samples WHERE at >= {since} ORDER BY line, at"))?;
        Ok(rows.iter().map(|row| (text(&row[0]), int(&row[1]), real(&row[2]))).collect())
    }

    /// Trend readings to write with the next write: new ones, and for a
    /// trend the time before which its readings are dropped. Kept until
    /// a write works, up to `MOST_TRENDS` new readings.
    pub fn note_trends(&mut self, added: Vec<(String, i64, f64)>, kept_from: Vec<(String, i64)>) {
        const MOST_TRENDS: usize = 100_000;
        let room = MOST_TRENDS.saturating_sub(self.trend_added.len());
        self.trend_added.extend(added.into_iter().take(room));
        for (line, at) in kept_from {
            let entry = self.trend_kept_from.entry(line).or_insert(at);
            *entry = (*entry).max(at);
        }
    }

    /// Writes what changed since the last write, and the log lines.
    /// `now` is in seconds.
    pub fn write(&mut self, snapshot: &Snapshot, log: &[(i64, String)], now: i64) -> Result<(), String> {
        self.ensure()?;
        let mut statements = Vec::new();
        let mut objects = Vec::new();
        for object in &snapshot.objects {
            let key = (object.host.clone(), object.service.clone().unwrap_or_default());
            let before = self.objects.get(&key);
            if before != Some(object) {
                statements.push(object_upsert(object, now));
                if before.map(|o| &o.settings) != Some(&object.settings) {
                    statements.push(Statement {
                        sql: "DELETE FROM settings WHERE host = ?1 AND service = ?2",
                        values: vec![key.0.as_str().into(), key.1.as_str().into()],
                    });
                    for setting in &object.settings {
                        statements.push(Statement {
                            sql: "INSERT INTO settings (host, service, name, value, configured) VALUES (?1, ?2, ?3, ?4, ?5)",
                            values: vec![
                                key.0.as_str().into(),
                                key.1.as_str().into(),
                                setting.name.as_str().into(),
                                setting.value.as_str().into(),
                                setting.configured.as_str().into(),
                            ],
                        });
                    }
                }
                objects.push((key, object.clone()));
            }
        }
        let comments: BTreeMap<u64, CommentState> =
            snapshot.comments.iter().map(|c| (c.id, c.clone())).collect();
        for (id, comment) in &comments {
            if self.comments.get(id) != Some(comment) {
                statements.push(comment_upsert(comment));
            }
        }
        for id in self.comments.keys().filter(|id| !comments.contains_key(id)) {
            statements.push(Statement {
                sql: "DELETE FROM comments WHERE id = ?1",
                values: vec![(*id).into()],
            });
        }
        let downtimes: BTreeMap<u64, DowntimeState> =
            snapshot.downtimes.iter().map(|d| (d.id, d.clone())).collect();
        for (id, downtime) in &downtimes {
            if self.downtimes.get(id) != Some(downtime) {
                statements.push(downtime_upsert(downtime));
            }
        }
        for id in self.downtimes.keys().filter(|id| !downtimes.contains_key(id)) {
            statements.push(Statement {
                sql: "DELETE FROM downtimes WHERE id = ?1",
                values: vec![(*id).into()],
            });
        }
        let meta = (snapshot.next_comment, snapshot.next_downtime, snapshot.notifications_enabled);
        let mut pairs = vec![("saved_at", now.to_string())];
        if self.meta != Some(meta) {
            pairs.push(("next_comment", meta.0.to_string()));
            pairs.push(("next_downtime", meta.1.to_string()));
            pairs.push(("notifications_enabled", (meta.2 as u8).to_string()));
        }
        if self.program.as_ref() != Some(&snapshot.program) {
            statements.push(Statement { sql: "DELETE FROM meta WHERE key LIKE 'program.%'", values: vec![] });
            for setting in &snapshot.program {
                statements.push(Statement {
                    sql: "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    values: vec![format!("program.{}", setting.name).into(), format!("{}\t{}", setting.value, setting.configured).into()],
                });
            }
        }
        for (key, value) in pairs {
            statements.push(Statement {
                sql: "INSERT INTO meta (key, value) VALUES (?1, ?2) \
                      ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                values: vec![key.into(), value.into()],
            });
        }
        for (second, line) in log {
            let (kind, host, service, contact) = log_parts(line);
            statements.push(Statement {
                sql: "INSERT INTO log (time, kind, host, service, contact, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                values: vec![(*second).into(), kind.into(), host.into(), service.into(), contact.into(), line.as_str().into()],
            });
        }
        for (line, at, value) in &self.trend_added {
            statements.push(Statement {
                sql: "INSERT INTO trend_samples (line, at, value) VALUES (?1, ?2, ?3)",
                values: vec![line.as_str().into(), (*at).into(), (*value).into()],
            });
        }
        for (line, at) in &self.trend_kept_from {
            statements.push(Statement { sql: "DELETE FROM trend_samples WHERE line = ?1 AND at < ?2", values: vec![line.as_str().into(), (*at).into()] });
        }
        self.backend.apply(&statements)?;
        self.trend_added.clear();
        self.trend_kept_from.clear();
        for (key, object) in objects {
            self.objects.insert(key, object);
        }
        self.comments = comments;
        self.downtimes = downtimes;
        self.meta = Some(meta);
        self.program = Some(snapshot.program.clone());
        Ok(())
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::Text(text) => text.clone(),
        Value::Int(n) => n.to_string(),
        Value::Real(x) => x.to_string(),
        Value::Null => String::new(),
    }
}

fn int(value: &Value) -> i64 {
    match value {
        Value::Int(n) => *n,
        Value::Real(x) => *x as i64,
        Value::Text(text) => text.parse().unwrap_or(0),
        Value::Null => 0,
    }
}

fn real(value: &Value) -> f64 {
    match value {
        Value::Real(x) => *x,
        Value::Int(n) => *n as f64,
        Value::Text(text) => text.parse().unwrap_or(0.0),
        Value::Null => 0.0,
    }
}

fn maybe_int(value: &Value) -> Option<i64> {
    match value {
        Value::Null => None,
        other => Some(int(other)),
    }
}

fn service_of(value: &Value) -> Option<String> {
    let name = text(value);
    (!name.is_empty()).then_some(name)
}

const OBJECT_COLUMNS: &str = "host, service, state, last_state, last_hard_state, hard, attempt, checked, \
     output, long_output, perfdata, last_check, last_state_change, last_hard_state_change, \
     last_time_0, last_time_1, last_time_2, last_time_3, flapping, percent_change, history, \
     event_id, last_event_id, problem_id, last_problem_id, notification_number, \
     last_notification, next_notification, notified_state, notified, notification_id, forced, \
     ack_author, ack_comment, ack_sticky, ack_persistent, ack_comment_id, \
     notifications_enabled, active_checks_enabled, config_notifications_enabled, \
     config_active_checks_enabled";

fn object_upsert(o: &ObjState, now: i64) -> Statement {
    let history: Vec<String> = o.history.iter().map(u8::to_string).collect();
    let ack = o.ack.as_ref();
    Statement {
        sql: "INSERT INTO objects (host, service, state, last_state, last_hard_state, hard, attempt, checked, \
              output, long_output, perfdata, last_check, last_state_change, last_hard_state_change, \
              last_time_0, last_time_1, last_time_2, last_time_3, flapping, percent_change, history, \
              event_id, last_event_id, problem_id, last_problem_id, notification_number, \
              last_notification, next_notification, notified_state, notified, notification_id, forced, \
              ack_author, ack_comment, ack_sticky, ack_persistent, ack_comment_id, \
              notifications_enabled, active_checks_enabled, config_notifications_enabled, \
              config_active_checks_enabled, updated) \
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
              ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, \
              ?36, ?37, ?38, ?39, ?40, ?41, ?42) \
              ON CONFLICT (host, service) DO UPDATE SET state = excluded.state, \
              last_state = excluded.last_state, last_hard_state = excluded.last_hard_state, \
              hard = excluded.hard, attempt = excluded.attempt, checked = excluded.checked, \
              output = excluded.output, long_output = excluded.long_output, \
              perfdata = excluded.perfdata, last_check = excluded.last_check, \
              last_state_change = excluded.last_state_change, \
              last_hard_state_change = excluded.last_hard_state_change, \
              last_time_0 = excluded.last_time_0, last_time_1 = excluded.last_time_1, \
              last_time_2 = excluded.last_time_2, last_time_3 = excluded.last_time_3, \
              flapping = excluded.flapping, percent_change = excluded.percent_change, \
              history = excluded.history, event_id = excluded.event_id, \
              last_event_id = excluded.last_event_id, problem_id = excluded.problem_id, \
              last_problem_id = excluded.last_problem_id, \
              notification_number = excluded.notification_number, \
              last_notification = excluded.last_notification, \
              next_notification = excluded.next_notification, \
              notified_state = excluded.notified_state, notified = excluded.notified, \
              notification_id = excluded.notification_id, forced = excluded.forced, \
              ack_author = excluded.ack_author, ack_comment = excluded.ack_comment, \
              ack_sticky = excluded.ack_sticky, ack_persistent = excluded.ack_persistent, \
              ack_comment_id = excluded.ack_comment_id, \
              notifications_enabled = excluded.notifications_enabled, \
              active_checks_enabled = excluded.active_checks_enabled, \
              config_notifications_enabled = excluded.config_notifications_enabled, \
              config_active_checks_enabled = excluded.config_active_checks_enabled, \
              updated = excluded.updated",
        values: vec![
            o.host.as_str().into(),
            o.service.clone().unwrap_or_default().into(),
            o.state.into(),
            o.last_state.into(),
            o.last_hard_state.into(),
            o.hard.into(),
            o.attempt.into(),
            o.checked.into(),
            o.output.as_str().into(),
            o.long_output.as_str().into(),
            o.perfdata.as_str().into(),
            o.last_check.into(),
            o.last_state_change.into(),
            o.last_hard_state_change.into(),
            o.last_time[0].into(),
            o.last_time[1].into(),
            o.last_time[2].into(),
            o.last_time[3].into(),
            o.flapping.into(),
            o.percent_change.into(),
            history.join(",").into(),
            o.event_id.into(),
            o.last_event_id.into(),
            o.problem_id.as_str().into(),
            o.last_problem_id.as_str().into(),
            o.notification_number.into(),
            o.last_notification.into(),
            o.next_notification.into(),
            o.notified_state.into(),
            o.notified.into(),
            o.notification_id.as_str().into(),
            o.forced.into(),
            ack.map(|a| a.author.clone()).into(),
            ack.map(|a| a.comment.clone()).into(),
            ack.map(|a| a.sticky).into(),
            ack.map(|a| a.persistent).into(),
            ack.map(|a| a.comment_id).into(),
            o.notifications_enabled.into(),
            o.active_checks_enabled.into(),
            o.config_notifications_enabled.into(),
            o.config_active_checks_enabled.into(),
            now.into(),
        ],
    }
}

fn object_from(r: &Row) -> ObjState {
    let history = text(&r[20])
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    let ack = match &r[32] {
        Value::Null => None,
        author => Some(AckState {
            author: text(author),
            comment: text(&r[33]),
            sticky: int(&r[34]) != 0,
            persistent: int(&r[35]) != 0,
            comment_id: int(&r[36]) as u64,
        }),
    };
    ObjState {
        host: text(&r[0]),
        service: service_of(&r[1]),
        state: int(&r[2]) as u8,
        last_state: int(&r[3]) as u8,
        last_hard_state: int(&r[4]) as u8,
        hard: int(&r[5]) != 0,
        attempt: int(&r[6]) as u32,
        checked: int(&r[7]) != 0,
        output: text(&r[8]),
        long_output: text(&r[9]),
        perfdata: text(&r[10]),
        last_check: int(&r[11]),
        last_state_change: int(&r[12]),
        last_hard_state_change: int(&r[13]),
        last_time: [int(&r[14]), int(&r[15]), int(&r[16]), int(&r[17])],
        flapping: int(&r[18]) != 0,
        percent_change: real(&r[19]),
        history,
        event_id: int(&r[21]) as u64,
        last_event_id: int(&r[22]) as u64,
        problem_id: text(&r[23]),
        last_problem_id: text(&r[24]),
        notification_number: int(&r[25]) as u32,
        last_notification: int(&r[26]),
        next_notification: maybe_int(&r[27]),
        notified_state: int(&r[28]) as u8,
        notified: int(&r[29]) != 0,
        notification_id: text(&r[30]),
        forced: int(&r[31]) != 0,
        ack,
        notifications_enabled: int(&r[37]) != 0,
        active_checks_enabled: int(&r[38]) != 0,
        config_notifications_enabled: int(&r[39]) != 0,
        config_active_checks_enabled: int(&r[40]) != 0,
        settings: Vec::new(),
    }
}

const COMMENT_COLUMNS: &str = "id, host, service, entry_type, persistent, entry_time, author, text, expires";

fn comment_upsert(c: &CommentState) -> Statement {
    Statement {
        sql: "INSERT INTO comments (id, host, service, entry_type, persistent, entry_time, author, text, expires) \
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
              ON CONFLICT (id) DO UPDATE SET host = excluded.host, service = excluded.service, \
              entry_type = excluded.entry_type, persistent = excluded.persistent, \
              entry_time = excluded.entry_time, author = excluded.author, text = excluded.text, \
              expires = excluded.expires",
        values: vec![
            c.id.into(),
            c.host.as_str().into(),
            c.service.clone().unwrap_or_default().into(),
            c.entry_type.into(),
            c.persistent.into(),
            c.entry.into(),
            c.author.as_str().into(),
            c.text.as_str().into(),
            c.expires.into(),
        ],
    }
}

fn comment_from(r: &Row) -> CommentState {
    CommentState {
        id: int(&r[0]) as u64,
        host: text(&r[1]),
        service: service_of(&r[2]),
        entry_type: int(&r[3]) as u8,
        persistent: int(&r[4]) != 0,
        entry: int(&r[5]),
        author: text(&r[6]),
        text: text(&r[7]),
        expires: maybe_int(&r[8]),
    }
}

const DOWNTIME_COLUMNS: &str = "id, host, service, start_time, end_time, fixed, trigger_id, duration, \
     author, comment, entry_time, triggered_at, active, comment_id";

fn downtime_upsert(d: &DowntimeState) -> Statement {
    Statement {
        sql: "INSERT INTO downtimes (id, host, service, start_time, end_time, fixed, trigger_id, duration, \
              author, comment, entry_time, triggered_at, active, comment_id) \
              VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14) \
              ON CONFLICT (id) DO UPDATE SET host = excluded.host, service = excluded.service, \
              start_time = excluded.start_time, end_time = excluded.end_time, fixed = excluded.fixed, \
              trigger_id = excluded.trigger_id, duration = excluded.duration, author = excluded.author, \
              comment = excluded.comment, entry_time = excluded.entry_time, \
              triggered_at = excluded.triggered_at, active = excluded.active, \
              comment_id = excluded.comment_id",
        values: vec![
            d.id.into(),
            d.host.as_str().into(),
            d.service.clone().unwrap_or_default().into(),
            d.start.into(),
            d.end.into(),
            d.fixed.into(),
            d.trigger.into(),
            d.duration.into(),
            d.author.as_str().into(),
            d.comment.as_str().into(),
            d.entry.into(),
            d.triggered.into(),
            d.active.into(),
            d.comment_id.into(),
        ],
    }
}

fn downtime_from(r: &Row) -> DowntimeState {
    DowntimeState {
        id: int(&r[0]) as u64,
        host: text(&r[1]),
        service: service_of(&r[2]),
        start: int(&r[3]),
        end: int(&r[4]),
        fixed: int(&r[5]) != 0,
        trigger: int(&r[6]) as u64,
        duration: int(&r[7]),
        author: text(&r[8]),
        comment: text(&r[9]),
        entry: int(&r[10]),
        triggered: maybe_int(&r[11]),
        active: int(&r[12]) != 0,
        comment_id: int(&r[13]) as u64,
    }
}

/// The kind of a log line, and the host, service and contact it is
/// about, where it names them.
pub fn log_parts(line: &str) -> (String, Option<String>, Option<String>, Option<String>) {
    let Some((label, rest)) = line.split_once(": ") else {
        return (String::new(), None, None, None);
    };
    let fields: Vec<&str> = rest.split(';').collect();
    let field = |n: usize| fields.get(n).map(|s| s.to_string());
    let (host, service, contact) = match label {
        "HOST ALERT" | "HOST FLAPPING ALERT" | "HOST DOWNTIME ALERT" | "HOST EVENT HANDLER"
        | "GLOBAL HOST EVENT HANDLER" | "INITIAL HOST STATE" | "CURRENT HOST STATE"
        | "HOST NOTIFICATION SUPPRESSED" | "HOST INFO" => (field(0), None, None),
        "SERVICE ALERT" | "SERVICE FLAPPING ALERT" | "SERVICE DOWNTIME ALERT"
        | "SERVICE EVENT HANDLER" | "GLOBAL SERVICE EVENT HANDLER" | "INITIAL SERVICE STATE"
        | "CURRENT SERVICE STATE" | "SERVICE NOTIFICATION SUPPRESSED" | "SERVICE INFO" => {
            (field(0), field(1), None)
        }
        "HOST NOTIFICATION" | "HOST CONTACT NOTIFICATION SUPPRESSED" => (field(1), None, field(0)),
        "SERVICE NOTIFICATION" | "SERVICE CONTACT NOTIFICATION SUPPRESSED" => {
            (field(1), field(2), field(0))
        }
        _ => (None, None, None),
    };
    let kind = if label.len() <= 48 && label.chars().all(|c| c.is_ascii_uppercase() || c == ' ') {
        label.to_string()
    } else {
        String::new()
    };
    (kind, host, service, contact)
}

/// Tables and views, in SQL that SQLite and PostgreSQL both take. `{REAL}`
/// and `{ID}` stand for what differs.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS objects (
    host TEXT NOT NULL,
    service TEXT NOT NULL DEFAULT '',
    state INTEGER NOT NULL,
    last_state INTEGER NOT NULL,
    last_hard_state INTEGER NOT NULL,
    hard INTEGER NOT NULL,
    attempt INTEGER NOT NULL,
    checked INTEGER NOT NULL,
    output TEXT NOT NULL,
    long_output TEXT NOT NULL,
    perfdata TEXT NOT NULL,
    last_check BIGINT NOT NULL,
    last_state_change BIGINT NOT NULL,
    last_hard_state_change BIGINT NOT NULL,
    last_time_0 BIGINT NOT NULL,
    last_time_1 BIGINT NOT NULL,
    last_time_2 BIGINT NOT NULL,
    last_time_3 BIGINT NOT NULL,
    flapping INTEGER NOT NULL,
    percent_change {REAL} NOT NULL,
    history TEXT NOT NULL,
    event_id BIGINT NOT NULL,
    last_event_id BIGINT NOT NULL,
    problem_id TEXT NOT NULL,
    last_problem_id TEXT NOT NULL,
    notification_number INTEGER NOT NULL,
    last_notification BIGINT NOT NULL,
    next_notification BIGINT,
    notified_state INTEGER NOT NULL,
    notified INTEGER NOT NULL,
    notification_id TEXT NOT NULL,
    forced INTEGER NOT NULL,
    ack_author TEXT,
    ack_comment TEXT,
    ack_sticky INTEGER,
    ack_persistent INTEGER,
    ack_comment_id BIGINT,
    notifications_enabled INTEGER NOT NULL,
    active_checks_enabled INTEGER NOT NULL,
    config_notifications_enabled INTEGER NOT NULL,
    config_active_checks_enabled INTEGER NOT NULL,
    updated BIGINT NOT NULL,
    PRIMARY KEY (host, service)
);
CREATE TABLE IF NOT EXISTS comments (
    id BIGINT PRIMARY KEY,
    host TEXT NOT NULL,
    service TEXT NOT NULL,
    entry_type INTEGER NOT NULL,
    persistent INTEGER NOT NULL,
    entry_time BIGINT NOT NULL,
    author TEXT NOT NULL,
    text TEXT NOT NULL,
    expires BIGINT
);
CREATE TABLE IF NOT EXISTS downtimes (
    id BIGINT PRIMARY KEY,
    host TEXT NOT NULL,
    service TEXT NOT NULL,
    start_time BIGINT NOT NULL,
    end_time BIGINT NOT NULL,
    fixed INTEGER NOT NULL,
    trigger_id BIGINT NOT NULL,
    duration BIGINT NOT NULL,
    author TEXT NOT NULL,
    comment TEXT NOT NULL,
    entry_time BIGINT NOT NULL,
    triggered_at BIGINT,
    active INTEGER NOT NULL,
    comment_id BIGINT NOT NULL
);
CREATE TABLE IF NOT EXISTS log (
    id {ID},
    time BIGINT NOT NULL,
    kind TEXT NOT NULL,
    host TEXT,
    service TEXT,
    contact TEXT,
    line TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS log_time ON log (time);
CREATE INDEX IF NOT EXISTS log_object ON log (host, service, time);
CREATE TABLE IF NOT EXISTS trend_samples (
    line TEXT NOT NULL,
    at BIGINT NOT NULL,
    value {REAL} NOT NULL
);
CREATE INDEX IF NOT EXISTS trend_samples_line ON trend_samples (line, at);
CREATE TABLE IF NOT EXISTS settings (
    host TEXT NOT NULL,
    service TEXT NOT NULL,
    name TEXT NOT NULL,
    value TEXT NOT NULL,
    configured TEXT NOT NULL,
    PRIMARY KEY (host, service, name)
);

CREATE VIEW IF NOT EXISTS v1_host_status AS
    SELECT host, CASE state WHEN 0 THEN 'UP' WHEN 1 THEN 'DOWN' ELSE 'UNREACHABLE' END AS state,
           CASE hard WHEN 1 THEN 'HARD' ELSE 'SOFT' END AS state_type, attempt,
           output, long_output, perfdata,
           last_check / 1000 AS last_check, last_state_change / 1000 AS last_state_change,
           last_hard_state_change / 1000 AS last_hard_state_change,
           flapping, percent_change, ack_author IS NOT NULL AS acknowledged,
           notification_number, notifications_enabled, active_checks_enabled
    FROM objects WHERE service = '';
CREATE VIEW IF NOT EXISTS v1_service_status AS
    SELECT host, service,
           CASE state WHEN 0 THEN 'OK' WHEN 1 THEN 'WARNING' WHEN 2 THEN 'CRITICAL' ELSE 'UNKNOWN' END AS state,
           CASE hard WHEN 1 THEN 'HARD' ELSE 'SOFT' END AS state_type, attempt,
           output, long_output, perfdata,
           last_check / 1000 AS last_check, last_state_change / 1000 AS last_state_change,
           last_hard_state_change / 1000 AS last_hard_state_change,
           flapping, percent_change, ack_author IS NOT NULL AS acknowledged,
           notification_number, notifications_enabled, active_checks_enabled
    FROM objects WHERE service <> '';
CREATE VIEW IF NOT EXISTS v1_state_history AS
    SELECT time, kind, host, NULLIF(service, '') AS service, line FROM log
    WHERE kind IN ('HOST ALERT', 'SERVICE ALERT', 'INITIAL HOST STATE', 'INITIAL SERVICE STATE',
                   'CURRENT HOST STATE', 'CURRENT SERVICE STATE',
                   'HOST FLAPPING ALERT', 'SERVICE FLAPPING ALERT',
                   'HOST DOWNTIME ALERT', 'SERVICE DOWNTIME ALERT');
CREATE VIEW IF NOT EXISTS v1_notifications AS
    SELECT time, kind, host, service, contact, line FROM log
    WHERE kind IN ('HOST NOTIFICATION', 'SERVICE NOTIFICATION');
CREATE VIEW IF NOT EXISTS v1_downtimes AS
    SELECT id, host, NULLIF(service, '') AS service, start_time, end_time,
           fixed, trigger_id, duration, author, comment, entry_time, triggered_at, active
    FROM downtimes;
CREATE VIEW IF NOT EXISTS v1_acknowledgements AS
    SELECT host, NULLIF(service, '') AS service, ack_author AS author, ack_comment AS comment,
           ack_sticky AS sticky, ack_persistent AS persistent
    FROM objects WHERE ack_author IS NOT NULL;
CREATE VIEW IF NOT EXISTS v1_comments AS
    SELECT id, host, NULLIF(service, '') AS service,
           CASE entry_type WHEN 1 THEN 'user' WHEN 2 THEN 'downtime' WHEN 3 THEN 'flapping'
                           ELSE 'acknowledgement' END AS kind,
           persistent, entry_time, author, text, expires
    FROM comments;
CREATE VIEW IF NOT EXISTS v1_log AS
    SELECT time, kind, host, NULLIF(service, '') AS service, contact, line FROM log;
";

/// SQLite: one file, written ahead.
pub struct Sqlite {
    conn: rusqlite::Connection,
    path: String,
}

impl Sqlite {
    pub fn open(path: &str) -> Result<Sqlite, String> {
        let conn = rusqlite::Connection::open(path)
            .map_err(|err| format!("cannot open the store {path}: {err}"))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .and_then(|_| conn.pragma_update(None, "synchronous", "NORMAL"))
            .map_err(|err| format!("{path}: {err}"))?;
        Ok(Sqlite {
            conn,
            path: path.to_string(),
        })
    }
}

fn to_sql(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::Int(n) => rusqlite::types::Value::Integer(*n),
        Value::Real(x) => rusqlite::types::Value::Real(*x),
        Value::Text(t) => rusqlite::types::Value::Text(t.clone()),
        Value::Null => rusqlite::types::Value::Null,
    }
}

impl Backend for Sqlite {
    fn apply(&mut self, statements: &[Statement]) -> Result<(), String> {
        let path = self.path.clone();
        let fail = |err: rusqlite::Error| format!("{path}: {err}");
        let tx = self.conn.transaction().map_err(fail)?;
        for statement in statements {
            let mut prepared = tx.prepare_cached(statement.sql).map_err(fail)?;
            let values: Vec<rusqlite::types::Value> = statement.values.iter().map(to_sql).collect();
            prepared.execute(rusqlite::params_from_iter(values)).map_err(fail)?;
        }
        tx.commit().map_err(fail)
    }

    fn query(&mut self, sql: &str) -> Result<Vec<Row>, String> {
        let path = self.path.clone();
        let fail = |err: rusqlite::Error| format!("{path}: {err}");
        let mut prepared = self.conn.prepare(sql).map_err(fail)?;
        let columns = prepared.column_count();
        let rows = prepared
            .query_map([], |row| {
                (0..columns)
                    .map(|n| {
                        Ok(match row.get::<_, rusqlite::types::Value>(n)? {
                            rusqlite::types::Value::Integer(n) => Value::Int(n),
                            rusqlite::types::Value::Real(x) => Value::Real(x),
                            rusqlite::types::Value::Text(t) => Value::Text(t),
                            rusqlite::types::Value::Blob(b) => Value::Text(String::from_utf8_lossy(&b).into()),
                            rusqlite::types::Value::Null => Value::Null,
                        })
                    })
                    .collect::<Result<Vec<Value>, rusqlite::Error>>()
            })
            .map_err(fail)?;
        rows.collect::<Result<Vec<Row>, _>>().map_err(fail)
    }

    fn create(&mut self) -> Result<(), String> {
        let sql = SCHEMA.replace("{REAL}", "REAL").replace("{ID}", "INTEGER PRIMARY KEY");
        self.conn
            .execute_batch(&sql)
            .map_err(|err| format!("{}: {err}", self.path))
    }

    fn describe(&self) -> String {
        format!("sqlite:{}", self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(host: &str, service: Option<&str>, state: u8) -> ObjState {
        ObjState {
            host: host.to_string(),
            service: service.map(str::to_string),
            state,
            hard: true,
            attempt: 1,
            output: format!("{host} output; with a semicolon"),
            history: vec![0, 0, state],
            next_notification: Some(5000),
            notifications_enabled: true,
            active_checks_enabled: true,
            config_notifications_enabled: true,
            config_active_checks_enabled: true,
            ..ObjState::default()
        }
    }

    #[test]
    fn an_older_store_is_migrated_and_a_newer_one_refused() {
        let dir = std::env::temp_dir().join(format!("checkd-store-migrate-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("old.sqlite");
        let _ = std::fs::remove_file(&path);
        let path = path.to_string_lossy().into_owned();
        let version = |backend: &mut Sqlite| backend.query("SELECT value FROM meta WHERE key = 'schema_version'").unwrap()[0][0].clone();

        // A store of version 1, with a row in it.
        let mut backend = Sqlite::open(&path).unwrap();
        prepare(&mut backend, &[]).unwrap();
        assert!(matches!(version(&mut backend), Value::Text(v) if v == "1"));
        backend
            .apply(&[Statement { sql: "INSERT INTO settings (host, service, name, value, configured) VALUES ('h', '', 'n', 'v', 'c')", values: vec![] }])
            .unwrap();

        // Two releases later: two steps, run once, the row kept.
        const STEPS: &[&[&str]] = &[&["ALTER TABLE settings ADD COLUMN note TEXT NOT NULL DEFAULT ''"], &["UPDATE settings SET note = 'moved'"]];
        prepare(&mut backend, STEPS).unwrap();
        assert!(matches!(version(&mut backend), Value::Text(v) if v == "3"));
        let rows = backend.query("SELECT value, note FROM settings").unwrap();
        assert!(matches!(&rows[..], [row] if matches!((&row[0], &row[1]), (Value::Text(v), Value::Text(n)) if v == "v" && n == "moved")));
        prepare(&mut backend, STEPS).unwrap();

        // A step that fails leaves the store as it was.
        let broken: &[&[&str]] = &[STEPS[0], STEPS[1], &["ALTER TABLE nowhere ADD COLUMN x TEXT"]];
        let err = prepare(&mut backend, broken).unwrap_err();
        assert!(err.contains("from version 3 to 4"), "{err}");
        assert!(matches!(version(&mut backend), Value::Text(v) if v == "3"));

        // This checkd, one release back, does not touch it.
        let err = prepare(&mut backend, &[]).unwrap_err();
        assert!(err.contains("version 3, from a newer checkd"), "{err}");
    }

    #[test]
    fn trend_readings_are_kept() {
        let dir = std::env::temp_dir().join(format!("checkd-store-trends-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("trends.sqlite");
        let _ = std::fs::remove_file(&path);
        let path = path.to_string_lossy().into_owned();
        let empty = Snapshot::default();
        let line = "checkd:trend nas/disk /";

        let mut store = Store::new(Box::new(Sqlite::open(&path).unwrap())).unwrap();
        store.note_trends((1..=5).map(|n| (line.to_string(), n * 1000, n as f64)).collect(), vec![]);
        store.note_trends(vec![("checkd:trend old/disk /".to_string(), 10, 1.0)], vec![]);
        store.write(&empty, &[], 1).unwrap();
        // The oldest two dropped by the trend itself.
        store.note_trends(vec![(line.to_string(), 6000, 6.0)], vec![(line.to_string(), 3000)]);
        store.write(&empty, &[], 2).unwrap();

        // After a restart: what is kept, oldest first; what is too old is gone.
        let mut store = Store::new(Box::new(Sqlite::open(&path).unwrap())).unwrap();
        let readings = store.trend_readings(100).unwrap();
        assert_eq!(readings.iter().map(|(_, at, v)| (*at, *v)).collect::<Vec<_>>(), vec![(3000, 3.0), (4000, 4.0), (5000, 5.0), (6000, 6.0)]);
        assert!(readings.iter().all(|(l, _, _)| l == line));
        assert!(store.backend.query("SELECT line FROM trend_samples WHERE at < 100").unwrap().is_empty());
    }

    #[test]
    fn writes_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("checkd-store-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("one.sqlite");
        let _ = std::fs::remove_file(&path);
        let path = path.to_string_lossy().into_owned();

        let mut store = Store::new(Box::new(Sqlite::open(&path).unwrap())).unwrap();
        assert_eq!(store.load().unwrap(), None);
        let mut snapshot = Snapshot {
            objects: vec![object("a", None, 1), object("a", Some("s"), 2)],
            comments: vec![CommentState {
                id: 3,
                host: "a".into(),
                service: Some("s".into()),
                entry_type: 4,
                persistent: true,
                entry: 100,
                author: "ann".into(),
                text: "looking".into(),
                expires: None,
            }],
            downtimes: vec![DowntimeState {
                id: 2,
                host: "a".into(),
                service: None,
                start: 10,
                end: 20,
                fixed: false,
                trigger: 0,
                duration: 5,
                author: "bob".into(),
                comment: "work".into(),
                entry: 9,
                triggered: Some(12),
                active: true,
                comment_id: 4,
            }],
            next_comment: 4,
            next_downtime: 2,
            notifications_enabled: false,
            program: vec![Setting { name: "enable_event_handlers".into(), value: "0".into(), configured: "1".into() }],
        };
        snapshot.objects[1].settings = vec![
            Setting { name: "check_interval".into(), value: "7".into(), configured: "5".into() },
            Setting { name: "modified".into(), value: "4".into(), configured: "0".into() },
        ];
        snapshot.objects[1].ack = Some(AckState {
            author: "ann".into(),
            comment: "looking".into(),
            sticky: true,
            persistent: true,
            comment_id: 3,
        });
        let log = vec![(100, "SERVICE NOTIFICATION: ann;a;s;CRITICAL;n-svc;down".to_string())];
        store.write(&snapshot, &log, 100).unwrap();
        store.write(&snapshot, &[(101, "second".to_string()), (101, "third".to_string())], 101).unwrap();
        assert_eq!(store.recent_log(2).unwrap(), vec![(101, "second".to_string()), (101, "third".to_string())]);
        assert_eq!(store.recent_log(10).unwrap().len(), 3);
        // A second write of the same state writes nothing but the time.
        store.write(&snapshot, &[], 101).unwrap();
        drop(store);

        let mut store = Store::new(Box::new(Sqlite::open(&path).unwrap())).unwrap();
        let mut back = store.load().unwrap().unwrap();
        back.objects.sort_by(|a, b| (&a.host, &a.service).cmp(&(&b.host, &b.service)));
        assert_eq!(back, snapshot);

        // A comment taken away is taken out of the store.
        snapshot.comments.clear();
        store.write(&snapshot, &[], 102).unwrap();
        let back = store.load().unwrap().unwrap();
        assert!(back.comments.is_empty());

        let rows = Sqlite::open(&path)
            .unwrap()
            .query("SELECT host, service, contact FROM v1_notifications")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(text(&rows[0][2]), "ann");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parts_of_log_lines() {
        let (kind, host, service, contact) = log_parts("SERVICE ALERT: a;s;CRITICAL;HARD;3;out");
        assert_eq!(kind, "SERVICE ALERT");
        assert_eq!((host.as_deref(), service.as_deref(), contact), (Some("a"), Some("s"), None));
        let (_, host, service, contact) = log_parts("HOST NOTIFICATION: ann;d;DOWN;n-host;out");
        assert_eq!((host.as_deref(), service, contact.as_deref()), (Some("d"), None, Some("ann")));
        let (kind, ..) = log_parts("Warning: something: else");
        assert_eq!(kind, "");
    }
}
