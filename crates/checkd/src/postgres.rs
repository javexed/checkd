//! The PostgreSQL store, behind the `postgres` feature.
//!
//! The client runs in a thread of its own with its own runtime. The
//! engine's loop hands it a batch of statements and waits until they are
//! committed, so what is written before a notification is on record before
//! the notification goes out.
//!
//! The connection is given as `postgres:<connection>`, in the keyword form
//! (`host=db.example user=checkd dbname=checkd`) or as a URL. A password is
//! not to be written there, where `ps` shows it: it is taken from the
//! environment, `PGPASSWORD`, or from the file `CHECKD_STORE_PASSWORD_FILE`
//! names.
//!
//! A database on another machine is reached with TLS only, checked against
//! the usual public roots. `sslmode=disable` is taken for a database on
//! this machine only.

#[cfg(not(feature = "postgres"))]
pub fn open(_conninfo: &str) -> Result<crate::store::Store, String> {
    Err("--store postgres:...: this checkd was built without the PostgreSQL store (the postgres feature)".to_string())
}

#[cfg(feature = "postgres")]
pub use with_feature::{Postgres, connect_to, open};

#[cfg(feature = "postgres")]
mod with_feature {
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::mpsc;

    use tokio_postgres::config::{Host, SslMode};
    use tokio_postgres::types::{ToSql, Type};

    use crate::store::{Backend, Row, SCHEMA, Statement, Store, Value};

    /// The lease a write must still hold: the node's name and the lease's
    /// counter when it took it.
    pub type Fence = Arc<std::sync::Mutex<Option<(String, i64)>>>;

    enum Ask {
        Apply(Vec<(String, Vec<Value>)>, Option<(String, i64)>, mpsc::Sender<Result<(), String>>),
        Query(String, Vec<Value>, mpsc::Sender<Result<Vec<Row>, String>>),
    }

    pub struct Postgres {
        asks: mpsc::Sender<Ask>,
        name: String,
        fence: Fence,
        /// How long a write may take.
        limit: std::time::Duration,
    }

    /// The answer of a write that a newer holder of the lease has fenced
    /// off.
    pub const FENCED: &str = "fenced";

    /// How long a write or a query may take before it counts as failed.
    const ANSWER: std::time::Duration = std::time::Duration::from_secs(15);

    impl Postgres {
        fn ask<T>(&self, make: impl FnOnce(mpsc::Sender<Result<T, String>>) -> Ask) -> Result<T, String> {
            self.ask_within(ANSWER, make)
        }

        fn ask_within<T>(
            &self,
            limit: std::time::Duration,
            make: impl FnOnce(mpsc::Sender<Result<T, String>>) -> Ask,
        ) -> Result<T, String> {
            let (reply, answer) = mpsc::channel();
            self.asks
                .send(make(reply))
                .map_err(|_| format!("{}: the database client has stopped", self.name))?;
            answer
                .recv_timeout(limit)
                .map_err(|_| format!("{}: no answer within {} seconds", self.name, limit.as_secs_f64()))?
        }
    }

    /// The text of a statement in PostgreSQL's form: `$1` for `?1`.
    fn dollars(sql: &str) -> String {
        sql.replace('?', "$")
    }

    fn is_local(config: &tokio_postgres::Config) -> bool {
        config.get_hosts().iter().all(|host| match host {
            Host::Tcp(name) => matches!(name.as_str(), "localhost" | "127.0.0.1" | "::1"),
            Host::Unix(_) => true,
        })
    }

    pub fn open(conninfo: &str) -> Result<Store, String> {
        let backend = connect_to(conninfo, true)?;
        Store::new(Box::new(backend))
    }

    /// A client of the database in a thread of its own. With `now`, the
    /// first connection must succeed; without it, the client connects when
    /// it is first asked, and again whenever its connection is lost.
    pub fn connect_to(conninfo: &str, now: bool) -> Result<Postgres, String> {
        let mut config = tokio_postgres::Config::from_str(conninfo)
            .map_err(|err| format!("--store postgres: the connection is not understood: {err}"))?;
        if config.get_password().is_none() {
            if let Ok(password) = std::env::var("PGPASSWORD") {
                config.password(password);
            } else if let Ok(file) = std::env::var("CHECKD_STORE_PASSWORD_FILE") {
                let password = std::fs::read_to_string(&file)
                    .map_err(|err| format!("cannot read the password file {file}: {err}"))?;
                config.password(password.trim_end_matches(['\n', '\r']));
            }
        }
        // A database that does not answer must not hold the checks up.
        if config.get_connect_timeout().is_none() {
            config.connect_timeout(std::time::Duration::from_secs(5));
        }
        let local = is_local(&config);
        let tls = match (config.get_ssl_mode(), local) {
            (SslMode::Disable, true) => false,
            // A network the site trusts, said so outright.
            (SslMode::Disable, false) if std::env::var("CHECKD_STORE_ALLOW_PLAIN").as_deref() == Ok("1") => false,
            (SslMode::Disable, false) => {
                return Err("--store postgres: a database on another machine is reached with TLS only; \
                            sslmode=disable is taken for a database on this machine, or with \
                            CHECKD_STORE_ALLOW_PLAIN=1 for a network you trust"
                    .to_string());
            }
            (_, true) => false,
            (_, false) => {
                config.ssl_mode(SslMode::Require);
                true
            }
        };
        let hosts: Vec<String> = config
            .get_hosts()
            .iter()
            .map(|host| match host {
                Host::Tcp(name) => name.clone(),
                Host::Unix(path) => path.display().to_string(),
            })
            .collect();
        let name = format!(
            "postgres:{}/{}",
            hosts.join(","),
            config.get_dbname().unwrap_or("")
        );

        let (asks, inbox) = mpsc::channel::<Ask>();
        let (ready, started) = mpsc::channel::<Result<(), String>>();
        let label = name.clone();
        std::thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(err) => {
                    let _ = ready.send(Err(format!("{label}: {err}")));
                    return;
                }
            };
            runtime.block_on(serve(config, tls, label, inbox, ready, now));
        });
        started
            .recv()
            .map_err(|_| format!("{name}: the database client did not start"))??;
        Ok(Postgres {
            asks,
            name,
            fence: Arc::new(std::sync::Mutex::new(None)),
            limit: ANSWER,
        })
    }

    async fn connect(config: &tokio_postgres::Config, tls: bool) -> Result<tokio_postgres::Client, String> {
        if tls {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let tls_config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|err| err.to_string())?
                .with_root_certificates(roots)
                .with_no_client_auth();
            let connector = tokio_postgres_rustls::MakeRustlsConnect::new(tls_config);
            let (client, connection) = config.connect(connector).await.map_err(|err| err.to_string())?;
            tokio::spawn(connection);
            Ok(client)
        } else {
            let (client, connection) = config
                .connect(tokio_postgres::NoTls)
                .await
                .map_err(|err| err.to_string())?;
            tokio::spawn(connection);
            Ok(client)
        }
    }

    async fn serve(
        config: tokio_postgres::Config,
        tls: bool,
        name: String,
        inbox: mpsc::Receiver<Ask>,
        ready: mpsc::Sender<Result<(), String>>,
        now: bool,
    ) {
        let mut client: Option<tokio_postgres::Client> = None;
        if now {
            match connect(&config, tls).await {
                Ok(c) => client = Some(c),
                Err(err) => {
                    let _ = ready.send(Err(format!("{name}: cannot connect: {err}")));
                    return;
                }
            }
        }
        let _ = ready.send(Ok(()));
        let mut cache = HashMap::new();
        // The loop waits on a channel of the standard library: the thread
        // has nothing else to do.
        while let Ok(ask) = inbox.recv() {
            if client.as_ref().is_none_or(|c| c.is_closed()) {
                cache.clear();
                client = connect(&config, tls).await.ok();
            }
            let Some(live) = client.as_mut() else {
                let err = format!("{name}: cannot connect");
                match ask {
                    Ask::Apply(_, _, reply) => drop(reply.send(Err(err))),
                    Ask::Query(_, _, reply) => drop(reply.send(Err(err))),
                }
                continue;
            };
            match ask {
                Ask::Apply(statements, fence, reply) => {
                    let result = apply(live, &mut cache, &statements, fence.as_ref())
                        .await
                        .map_err(|err| if err == FENCED { err } else { format!("{name}: {err}") });
                    let _ = reply.send(result);
                }
                Ask::Query(sql, values, reply) => {
                    let result = query(live, &sql, &values).await.map_err(|err| format!("{name}: {err}"));
                    let _ = reply.send(result);
                }
            }
        }
    }

    async fn apply(
        client: &mut tokio_postgres::Client,
        cache: &mut HashMap<String, tokio_postgres::Statement>,
        statements: &[(String, Vec<Value>)],
        fence: Option<&(String, i64)>,
    ) -> Result<(), String> {
        for (sql, values) in statements {
            if !values.is_empty() && !cache.contains_key(sql) {
                let prepared = client.prepare(sql).await.map_err(|err| err.to_string())?;
                cache.insert(sql.clone(), prepared);
            }
        }
        let tx = client.transaction().await.map_err(|err| err.to_string())?;
        // A write by a node whose lease was taken over is not made.
        if let Some((holder, counter)) = fence {
            let held = tx
                .execute(
                    "UPDATE checkd_lease SET written = now() WHERE name = 'leader' AND holder = $1 AND counter = $2",
                    &[holder, counter],
                )
                .await
                .map_err(|err| err.to_string())?;
            if held != 1 {
                let _ = tx.rollback().await;
                return Err(FENCED.to_string());
            }
        }
        for (sql, values) in statements {
            if values.is_empty() {
                tx.batch_execute(sql).await.map_err(|err| err.to_string())?;
                continue;
            }
            let prepared = &cache[sql];
            // Each value goes in as the column's type wants it.
            let typed: Vec<Box<dyn ToSql + Sync + Send>> = values
                .iter()
                .zip(prepared.params())
                .map(|(value, ty)| -> Box<dyn ToSql + Sync + Send> {
                    match (value, ty) {
                        (Value::Null, &Type::TEXT) => Box::new(None::<String>),
                        (Value::Null, &Type::FLOAT8) => Box::new(None::<f64>),
                        (Value::Null, _) => Box::new(None::<i64>),
                        (Value::Int(n), &Type::TEXT) => Box::new(n.to_string()),
                        (Value::Int(n), &Type::FLOAT8) => Box::new(*n as f64),
                        (Value::Int(n), _) => Box::new(*n),
                        (Value::Real(x), _) => Box::new(*x),
                        (Value::Text(t), _) => Box::new(t.clone()),
                    }
                })
                .collect();
            let params: Vec<&(dyn ToSql + Sync)> = typed.iter().map(|v| v.as_ref() as &(dyn ToSql + Sync)).collect();
            tx.execute(prepared, &params).await.map_err(|err| err.to_string())?;
        }
        tx.commit().await.map_err(|err| err.to_string())
    }

    async fn query(client: &tokio_postgres::Client, sql: &str, values: &[Value]) -> Result<Vec<Row>, String> {
        let owned: Vec<Box<dyn ToSql + Sync + Send>> = values
            .iter()
            .map(|value| -> Box<dyn ToSql + Sync + Send> {
                match value {
                    Value::Int(n) => Box::new(*n),
                    Value::Real(x) => Box::new(*x),
                    Value::Text(t) => Box::new(t.clone()),
                    Value::Null => Box::new(None::<String>),
                }
            })
            .collect();
        let params: Vec<&(dyn ToSql + Sync)> = owned.iter().map(|v| v.as_ref() as &(dyn ToSql + Sync)).collect();
        let rows = client.query(sql, &params).await.map_err(|err| err.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            let mut values = Vec::new();
            for (n, column) in row.columns().iter().enumerate() {
                let value = match *column.type_() {
                    Type::INT8 => row.try_get::<_, Option<i64>>(n).map(|v| v.map(Value::Int)),
                    Type::INT4 => row.try_get::<_, Option<i32>>(n).map(|v| v.map(|n| Value::Int(n as i64))),
                    Type::FLOAT8 => row.try_get::<_, Option<f64>>(n).map(|v| v.map(Value::Real)),
                    Type::BOOL => row.try_get::<_, Option<bool>>(n).map(|v| v.map(|b| Value::Int(b as i64))),
                    _ => row.try_get::<_, Option<String>>(n).map(|v| v.map(Value::Text)),
                }
                .map_err(|err| err.to_string())?;
                values.push(value.unwrap_or(Value::Null));
            }
            out.push(values);
        }
        Ok(out)
    }

    /// The tables of failover: the lease, and the nodes that take part.
    const LEASE_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS checkd_lease (
    name TEXT PRIMARY KEY,
    holder TEXT NOT NULL,
    counter BIGINT NOT NULL,
    expires TIMESTAMPTZ NOT NULL,
    written TIMESTAMPTZ
);
CREATE TABLE IF NOT EXISTS checkd_nodes (
    name TEXT PRIMARY KEY,
    config_hash TEXT NOT NULL,
    peer TEXT NOT NULL,
    livestatus TEXT NOT NULL,
    seen TIMESTAMPTZ NOT NULL
);
";

    impl Postgres {
        /// Runs a query with values, outside a transaction, and waits at
        /// most `limit` for the answer.
        pub fn query_within(&self, sql: &str, values: Vec<Value>, limit: std::time::Duration) -> Result<Vec<Row>, String> {
            self.ask_within(limit, |reply| Ask::Query(dollars(sql), values, reply))
        }

        /// Makes the tables when they are not there.
        pub fn make_tables(&mut self) -> Result<(), String> {
            Backend::create(self)
        }
    }

    impl Backend for Postgres {
        fn apply(&mut self, statements: &[Statement]) -> Result<(), String> {
            let batch = statements
                .iter()
                .map(|s| (dollars(s.sql), s.values.clone()))
                .collect();
            let fence = self.fence.lock().map(|f| f.clone()).unwrap_or(None);
            self.ask_within(self.limit, |reply| Ask::Apply(batch, fence, reply))
        }

        fn query(&mut self, sql: &str) -> Result<Vec<Row>, String> {
            self.ask(|reply| Ask::Query(dollars(sql), Vec::new(), reply))
        }

        fn set_fence(&mut self, holder: &str, counter: i64) {
            if let Ok(mut fence) = self.fence.lock() {
                *fence = Some((holder.to_string(), counter));
            }
            // A leader does not wait long for the database: it has to stop
            // in time when it cannot renew its lease.
            self.limit = std::time::Duration::from_secs(2);
        }

        fn create(&mut self) -> Result<(), String> {
            let sql = SCHEMA
                .replace("{REAL}", "DOUBLE PRECISION")
                .replace("{ID}", "BIGSERIAL PRIMARY KEY")
                .replace(" INTEGER", " BIGINT")
                .replace("CREATE VIEW IF NOT EXISTS", "CREATE OR REPLACE VIEW")
                + LEASE_SCHEMA;
            let batch = sql
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| (s.to_string(), Vec::new()))
                .collect();
            self.ask(|reply| Ask::Apply(batch, None, reply))
        }

        fn describe(&self) -> String {
            self.name.clone()
        }
    }
}
