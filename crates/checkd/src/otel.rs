//! OpenTelemetry export: how checkd itself is doing.
//!
//! This is about the engine, not about the hosts and services it checks.
//! It sends metrics and, for a sample of the checks, one trace per check
//! to a collector, as OTLP over HTTP with JSON bodies. Nothing is sent
//! unless an endpoint is given, with `--otlp-endpoint` or in
//! `OTEL_EXPORTER_OTLP_ENDPOINT`.
//!
//! The bodies are written here by hand and sent over a plain socket: a
//! build with this feature carries no further dependency. The price is
//! that only `http://` endpoints can be used. A collector that wants TLS
//! is reached through one on the same machine that does not.
//!
//! A trace carries the name of the host and of the service that was
//! checked. Whoever reads the collector reads those.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use checkd_core::engine::{Job, JobKind, Outcome};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Upper bounds of the histogram buckets, in seconds.
const BOUNDS: [f64; 13] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];

/// Finished spans are sent this often,
const SPANS_EVERY: Duration = Duration::from_secs(5);
/// or sooner when this many wait.
const SPANS_AT_ONCE: usize = 512;
/// Spans that wait beyond this number are dropped.
const SPANS_KEPT: usize = 4096;
/// Bodies that wait for the collector. Beyond this they are dropped.
const BODIES_KEPT: usize = 16;
/// How long one request to the collector may take.
const REQUEST: Duration = Duration::from_secs(10);
/// How long the last bodies may take at shutdown.
const LAST_WORDS: Duration = Duration::from_secs(2);

/// Where to send, and as what.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    /// `host:port` to connect to.
    address: String,
    /// The value of the `Host` header.
    host: String,
    /// The path before `/v1/metrics` and `/v1/traces`, without a slash
    /// at its end.
    base: String,
    /// Further request headers.
    headers: Vec<(String, String)>,
}

impl Target {
    fn parse(endpoint: &str, headers: &str) -> Result<Target, String> {
        let rest = match endpoint.split_once("://") {
            Some(("http", rest)) => rest,
            Some(("https", _)) => {
                return Err(format!(
                    "the OTLP endpoint {endpoint} wants TLS, which this build cannot do: \
                     give an http:// endpoint, such as a collector on this machine"
                ));
            }
            _ => return Err(format!("the OTLP endpoint {endpoint} is not an http:// URL")),
        };
        let (authority, path) = match rest.find('/') {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, ""),
        };
        if authority.is_empty() || authority.contains('@') {
            return Err(format!("the OTLP endpoint {endpoint} names no host"));
        }
        // A port is what follows the last colon, unless that colon is
        // inside the brackets of an IPv6 address.
        let has_port = match authority.rfind(':') {
            Some(colon) => authority.rfind(']').is_none_or(|bracket| bracket < colon),
            None => false,
        };
        let address = if has_port {
            authority.to_string()
        } else {
            format!("{authority}:80")
        };
        let mut list = Vec::new();
        for pair in headers.split(',').map(str::trim).filter(|pair| !pair.is_empty()) {
            let (name, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("OTEL_EXPORTER_OTLP_HEADERS: '{pair}' is not name=value"))?;
            let (name, value) = (name.trim(), value.trim());
            let bad = |text: &str| text.chars().any(|c| c.is_control());
            if name.is_empty() || bad(name) || bad(value) || name.contains(':') {
                return Err(format!("OTEL_EXPORTER_OTLP_HEADERS: '{pair}' cannot be sent"));
            }
            list.push((name.to_string(), value.to_string()));
        }
        Ok(Target {
            address,
            host: authority.to_string(),
            base: path.trim_end_matches('/').to_string(),
            headers: list,
        })
    }
}

/// A body on its way to the collector.
struct Body {
    /// `/v1/metrics` or `/v1/traces`.
    path: &'static str,
    json: String,
}

#[derive(Default)]
struct Histogram {
    /// One count for each bound, and one for what lies above the last.
    buckets: [u64; BOUNDS.len() + 1],
    count: u64,
    sum: f64,
}

impl Histogram {
    fn add(&mut self, value: f64) {
        let at = BOUNDS.iter().position(|&bound| value <= bound).unwrap_or(BOUNDS.len());
        self.buckets[at] += 1;
        self.count += 1;
        self.sum += value;
    }
}

/// A command that is running, for the span it may become.
struct Open {
    kind: JobKind,
    started: Instant,
    /// The span, when this command was picked for a trace.
    span: Option<Span>,
}

struct Span {
    name: String,
    /// When the check was due, in nanoseconds since the epoch.
    due: u128,
    /// When the command was started.
    spawned: u128,
    latency: f64,
}

struct On {
    target: Target,
    resource: String,
    scope: String,
    /// The share of checks that get a trace: 0 to 1.
    ratio: f64,
    metrics_every: Duration,
    next_metrics: Instant,
    next_spans: Instant,
    began: u128,
    random: u64,
    open: BTreeMap<u64, Open>,
    /// Checks and event handlers that ended, by kind and outcome.
    ended: BTreeMap<(&'static str, &'static str), u64>,
    latency: BTreeMap<&'static str, Histogram>,
    duration: BTreeMap<&'static str, Histogram>,
    spans: Vec<String>,
    spans_dropped: u64,
    bodies_dropped: u64,
    to_sender: Option<mpsc::Sender<Body>>,
    sender: Option<JoinHandle<()>>,
}

/// The export, or nothing when no endpoint was given.
pub struct Telemetry(Option<Box<On>>);

/// Whether this build can export at all.
pub const BUILT_IN: bool = true;

impl Telemetry {
    /// Reads where to send from the option and the environment. Must be
    /// called inside the runtime: it starts the task that sends.
    pub fn start(endpoint: Option<&str>) -> Result<Telemetry, String> {
        let env = |name: &str| std::env::var(name).ok().filter(|value| !value.trim().is_empty());
        let Some(endpoint) = endpoint.map(str::to_string).or_else(|| env("OTEL_EXPORTER_OTLP_ENDPOINT"))
        else {
            return Ok(Telemetry(None));
        };
        let headers = env("OTEL_EXPORTER_OTLP_HEADERS").unwrap_or_default();
        let target = Target::parse(endpoint.trim(), &headers)?;

        let number = |name: &str, default: f64| -> Result<f64, String> {
            match env(name) {
                Some(text) => text
                    .trim()
                    .parse()
                    .map_err(|_| format!("{name} is '{text}', which is not a number")),
                None => Ok(default),
            }
        };
        let ratio = number("OTEL_TRACES_SAMPLER_ARG", 0.01)?.clamp(0.0, 1.0);
        let every = number("OTEL_METRIC_EXPORT_INTERVAL", 60_000.0)?.max(1000.0);
        let metrics_every = Duration::from_millis(every as u64);

        let mut attributes = vec![
            ("service.name".to_string(), env("OTEL_SERVICE_NAME").unwrap_or("checkd".into())),
            ("service.version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
        ];
        for pair in env("OTEL_RESOURCE_ATTRIBUTES").unwrap_or_default().split(',') {
            if let Some((name, value)) = pair.split_once('=') {
                let name = name.trim();
                if !name.is_empty() && name != "service.name" && name != "service.version" {
                    attributes.push((name.to_string(), value.trim().to_string()));
                }
            }
        }
        let resource = format!(
            "{{\"attributes\":[{}]}}",
            attributes
                .iter()
                .map(|(name, value)| text_attribute(name, value))
                .collect::<Vec<_>>()
                .join(",")
        );
        let scope = format!(
            "{{\"name\":\"checkd\",\"version\":{}}}",
            quoted(env!("CARGO_PKG_VERSION"))
        );

        let (to_sender, bodies) = mpsc::channel(BODIES_KEPT);
        let sender = tokio::spawn(send_all(target.clone(), bodies));
        let now = Instant::now();
        Ok(Telemetry(Some(Box::new(On {
            target,
            resource,
            scope,
            ratio,
            metrics_every,
            next_metrics: now + metrics_every,
            next_spans: now + SPANS_EVERY,
            began: nanos(),
            random: seed(),
            open: BTreeMap::new(),
            ended: BTreeMap::new(),
            latency: BTreeMap::new(),
            duration: BTreeMap::new(),
            spans: Vec::new(),
            spans_dropped: 0,
            bodies_dropped: 0,
            to_sender: Some(to_sender),
            sender: Some(sender),
        }))))
    }

    /// A line for the person who starts checkd, when the export is on.
    pub fn describe(&self) -> Option<String> {
        let on = self.0.as_ref()?;
        Some(format!(
            "OpenTelemetry: metrics every {} s and a trace for {} in 100 checks go to http://{}{}",
            on.metrics_every.as_secs(),
            on.ratio * 100.0,
            on.target.host,
            on.target.base,
        ))
    }

    /// A command was started. `name` is the host, or `host;service`.
    /// `latency` is how many seconds after its time a check was started.
    pub fn job_started(&mut self, job: &Job, name: &str, latency: f64) {
        let Some(on) = &mut self.0 else { return };
        let check = matches!(job.kind, JobKind::HostCheck | JobKind::ServiceCheck);
        if check {
            on.latency.entry(kind_name(job.kind)).or_default().add(latency);
        }
        let span = (on.ratio > 0.0 && on.chance() < on.ratio).then(|| {
            let spawned = nanos();
            Span {
                name: name.to_string(),
                due: spawned.saturating_sub((latency.max(0.0) * 1e9) as u128),
                spawned,
                latency,
            }
        });
        on.open.insert(
            job.id,
            Open {
                kind: job.kind,
                started: Instant::now(),
                span,
            },
        );
    }

    /// The command's result is in. `read` is when it came in, in
    /// nanoseconds since the epoch; by now the engine has applied it.
    pub fn job_done(&mut self, id: u64, outcome: &Outcome, read: u128) {
        let Some(on) = &mut self.0 else { return };
        let Some(open) = on.open.remove(&id) else { return };
        let kind = kind_name(open.kind);
        let (how, code) = match outcome {
            Outcome::Exited(code) => ("exited", Some(*code)),
            Outcome::Signaled(signal) => ("signaled", Some(*signal)),
            Outcome::TimedOut => ("timed_out", None),
            Outcome::Failed(_) => ("failed", None),
        };
        *on.ended.entry((kind, how)).or_default() += 1;
        on.duration
            .entry(kind)
            .or_default()
            .add(open.started.elapsed().as_secs_f64());

        let Some(span) = open.span else { return };
        if on.spans.len() >= SPANS_KEPT {
            on.spans_dropped += 1;
            return;
        }
        let (host, service) = match span.name.split_once(';') {
            Some((host, service)) => (host, Some(service)),
            None => (span.name.as_str(), None),
        };
        let mut attributes = vec![
            text_attribute("checkd.kind", kind),
            text_attribute("checkd.host", host),
            text_attribute("checkd.outcome", how),
            format!(
                "{{\"key\":\"checkd.latency\",\"value\":{{\"doubleValue\":{}}}}}",
                number(span.latency)
            ),
        ];
        if let Some(service) = service {
            attributes.push(text_attribute("checkd.service", service));
        }
        if let Some(code) = code {
            let key = if how == "exited" { "checkd.exit_code" } else { "checkd.signal" };
            attributes.push(format!(
                "{{\"key\":\"{key}\",\"value\":{{\"intValue\":\"{code}\"}}}}"
            ));
        }
        let trace = format!("{:016x}{:016x}", on.next_random(), on.next_random());
        let id = format!("{:016x}", on.next_random());
        let event =
            |name: &str, at: u128| format!("{{\"timeUnixNano\":\"{at}\",\"name\":\"{name}\"}}");
        on.spans.push(format!(
            "{{\"traceId\":\"{trace}\",\"spanId\":\"{id}\",\"name\":{},\"kind\":1,\
             \"startTimeUnixNano\":\"{}\",\"endTimeUnixNano\":\"{}\",\
             \"attributes\":[{}],\"events\":[{},{}],\"status\":{{}}}}",
            quoted(&format!("{} {}", kind.replace('_', " "), span.name)),
            span.due,
            nanos().max(read),
            attributes.join(","),
            event("spawned", span.spawned),
            event("result read", read.max(span.spawned)),
        ));
        if on.spans.len() >= SPANS_AT_ONCE {
            on.send_spans();
        }
    }

    /// How long until `flush` wants to be called.
    pub fn next_flush(&self) -> Option<Duration> {
        let on = self.0.as_ref()?;
        let at = if on.spans.is_empty() {
            on.next_metrics
        } else {
            on.next_metrics.min(on.next_spans)
        };
        Some(at.saturating_duration_since(Instant::now()))
    }

    /// Sends what is due.
    pub fn flush(&mut self) {
        let Some(on) = &mut self.0 else { return };
        let now = Instant::now();
        if now >= on.next_spans {
            on.send_spans();
        }
        if now >= on.next_metrics {
            on.send_metrics();
        }
    }

    /// Sends what is left and waits a short while for it to arrive.
    pub async fn shutdown(&mut self) {
        let Some(mut on) = self.0.take() else { return };
        on.send_spans();
        on.send_metrics();
        on.to_sender = None;
        if let Some(sender) = on.sender.take() {
            if tokio::time::timeout(LAST_WORDS, sender).await.is_err() {
                eprintln!("warning: OpenTelemetry: the last data did not reach the collector in time");
            }
        }
    }
}

impl On {
    fn next_random(&mut self) -> u64 {
        // xorshift64*: ids only have to differ, not to be secret.
        let mut x = self.random;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.random = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A number from 0 up to, not including, 1.
    fn chance(&mut self) -> f64 {
        (self.next_random() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn send(&mut self, path: &'static str, json: String) {
        let Some(to_sender) = &self.to_sender else { return };
        if to_sender.try_send(Body { path, json }).is_err() {
            if self.bodies_dropped == 0 {
                eprintln!(
                    "warning: OpenTelemetry: the collector does not keep up, data is being dropped"
                );
            }
            self.bodies_dropped += 1;
        }
    }

    fn send_spans(&mut self) {
        self.next_spans = Instant::now() + SPANS_EVERY;
        if self.spans.is_empty() {
            return;
        }
        let json = format!(
            "{{\"resourceSpans\":[{{\"resource\":{},\"scopeSpans\":[{{\"scope\":{},\"spans\":[{}]}}]}}]}}",
            self.resource,
            self.scope,
            self.spans.join(","),
        );
        self.spans.clear();
        self.send("/v1/traces", json);
    }

    fn send_metrics(&mut self) {
        self.next_metrics = Instant::now() + self.metrics_every;
        let json = self.metrics(nanos());
        self.send("/v1/metrics", json);
    }

    /// Everything counted since the start, as one body.
    fn metrics(&self, now: u128) -> String {
        let began = self.began;
        let times = format!("\"startTimeUnixNano\":\"{began}\",\"timeUnixNano\":\"{now}\"");
        let mut metrics = Vec::new();

        let sum = |name: &str, what: &str, unit: &str, points: Vec<String>| {
            format!(
                "{{\"name\":\"{name}\",\"description\":{},\"unit\":\"{unit}\",\"sum\":{{\"dataPoints\":[{}],\
                 \"aggregationTemporality\":2,\"isMonotonic\":true}}}}",
                quoted(what),
                points.join(","),
            )
        };
        let int_point = |attributes: Vec<String>, value: u64| {
            format!("{{\"attributes\":[{}],{times},\"asInt\":\"{value}\"}}", attributes.join(","))
        };

        let ended = |handlers: bool| -> Vec<String> {
            self.ended
                .iter()
                .filter(|((kind, _), _)| kind.ends_with("handler") == handlers)
                .map(|((kind, how), count)| {
                    int_point(
                        vec![text_attribute("checkd.kind", kind), text_attribute("checkd.outcome", how)],
                        *count,
                    )
                })
                .collect()
        };
        let checks = ended(false);
        if !checks.is_empty() {
            metrics.push(sum("checkd.checks", "Checks that have ended", "{check}", checks));
        }
        let handlers = ended(true);
        if !handlers.is_empty() {
            metrics.push(sum(
                "checkd.event_handlers",
                "Event handlers that have ended",
                "{command}",
                handlers,
            ));
        }

        let histogram = |name: &str, what: &str, by_kind: &BTreeMap<&'static str, Histogram>| {
            let points: Vec<String> = by_kind
                .iter()
                .map(|(kind, h)| {
                    format!(
                        "{{\"attributes\":[{}],{times},\"count\":\"{}\",\"sum\":{},\
                         \"bucketCounts\":[{}],\"explicitBounds\":[{}]}}",
                        text_attribute("checkd.kind", kind),
                        h.count,
                        number(h.sum),
                        h.buckets.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(","),
                        BOUNDS.iter().map(|b| number(*b)).collect::<Vec<_>>().join(","),
                    )
                })
                .collect();
            format!(
                "{{\"name\":\"{name}\",\"description\":{},\"unit\":\"s\",\"histogram\":{{\"dataPoints\":[{}],\
                 \"aggregationTemporality\":2}}}}",
                quoted(what),
                points.join(","),
            )
        };
        if !self.latency.is_empty() {
            metrics.push(histogram(
                "checkd.check.latency",
                "How long after its time a check was started",
                &self.latency,
            ));
        }
        if !self.duration.is_empty() {
            metrics.push(histogram(
                "checkd.command.duration",
                "How long a check or an event handler ran",
                &self.duration,
            ));
        }

        metrics.push(format!(
            "{{\"name\":\"checkd.commands.running\",\"description\":{},\"unit\":\"{{command}}\",\
             \"gauge\":{{\"dataPoints\":[{{\"timeUnixNano\":\"{now}\",\"asInt\":\"{}\"}}]}}}}",
            quoted("Checks and event handlers that are running"),
            self.open.len(),
        ));
        metrics.push(sum(
            "checkd.otel.dropped",
            "Spans and request bodies that were dropped instead of sent",
            "{item}",
            vec![
                int_point(vec![text_attribute("checkd.item", "span")], self.spans_dropped),
                int_point(vec![text_attribute("checkd.item", "body")], self.bodies_dropped),
            ],
        ));

        format!(
            "{{\"resourceMetrics\":[{{\"resource\":{},\"scopeMetrics\":[{{\"scope\":{},\"metrics\":[{}]}}]}}]}}",
            self.resource,
            self.scope,
            metrics.join(","),
        )
    }
}

fn kind_name(kind: JobKind) -> &'static str {
    match kind {
        JobKind::HostCheck => "host_check",
        JobKind::ServiceCheck => "service_check",
        JobKind::HostHandler => "host_event_handler",
        JobKind::ServiceHandler => "service_event_handler",
        JobKind::Notification => "notification",
        JobKind::PerfdataProcessing => "perfdata_processing",
    }
}

/// Nanoseconds since the epoch, by the system's clock.
pub fn nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or(0)
}

/// Something to start the ids from that differs from run to run.
fn seed() -> u64 {
    let mut bytes = [0u8; 8];
    let read = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut bytes));
    let seed = match read {
        Ok(()) => u64::from_le_bytes(bytes),
        Err(_) => nanos() as u64 ^ ((std::process::id() as u64) << 32),
    };
    // The generator stays at 0 once it is there.
    seed | 1
}

/// A string as JSON writes it.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A number as JSON writes it. JSON has no word for what is not finite.
fn number(value: f64) -> String {
    if value.is_finite() { format!("{value}") } else { "0".to_string() }
}

fn text_attribute(key: &str, value: &str) -> String {
    format!("{{\"key\":{},\"value\":{{\"stringValue\":{}}}}}", quoted(key), quoted(value))
}

/// Sends the bodies as they come, until there will be no more.
async fn send_all(target: Target, mut bodies: mpsc::Receiver<Body>) {
    // Whether the last request failed: a collector that is away is
    // reported once, and once more when it is back.
    let mut failing = false;
    while let Some(body) = bodies.recv().await {
        let sent = match tokio::time::timeout(REQUEST, post(&target, &body)).await {
            Ok(result) => result,
            Err(_) => Err("no answer in time".to_string()),
        };
        match sent {
            Ok(()) if failing => {
                eprintln!("OpenTelemetry: the collector takes data again");
                failing = false;
            }
            Ok(()) => {}
            Err(why) if !failing => {
                eprintln!(
                    "warning: OpenTelemetry: cannot send to http://{}{}{}: {why}. \
                     Data is dropped until this works again",
                    target.host, target.base, body.path
                );
                failing = true;
            }
            Err(_) => {}
        }
    }
}

/// One request. A body that the collector does not take is not sent
/// again: the next metrics carry the same counts and more.
async fn post(target: &Target, body: &Body) -> Result<(), String> {
    let mut stream = TcpStream::connect(&target.address)
        .await
        .map_err(|err| err.to_string())?;
    let mut request = format!(
        "POST {}{} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\nUser-Agent: checkd/{}\r\n",
        target.base,
        body.path,
        target.host,
        body.json.len(),
        env!("CARGO_PKG_VERSION"),
    );
    for (name, value) in &target.headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    let io = |err: std::io::Error| err.to_string();
    stream.write_all(request.as_bytes()).await.map_err(io)?;
    stream.write_all(body.json.as_bytes()).await.map_err(io)?;

    // The status line is all that is looked at.
    let mut answer = Vec::new();
    let mut buffer = [0u8; 1024];
    while !answer.contains(&b'\n') && answer.len() < 4096 {
        match stream.read(&mut buffer).await.map_err(io)? {
            0 => break,
            n => answer.extend_from_slice(&buffer[..n]),
        }
    }
    let line = String::from_utf8_lossy(&answer);
    let line = line.lines().next().unwrap_or("").trim();
    let status = line.split(' ').nth(1).and_then(|code| code.parse::<u16>().ok());
    match status {
        Some(200..=299) => Ok(()),
        Some(_) => Err(format!("the answer was '{line}'")),
        None => Err("the answer was not HTTP".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use checkd_core::engine::Obj;
    use tokio::net::TcpListener;

    #[test]
    fn targets() {
        let target = Target::parse("http://127.0.0.1:4318", "").unwrap();
        assert_eq!(target.address, "127.0.0.1:4318");
        assert_eq!(target.host, "127.0.0.1:4318");
        assert_eq!(target.base, "");

        let target = Target::parse("http://collector/otlp/", "a=b, x-key = secret").unwrap();
        assert_eq!(target.address, "collector:80");
        assert_eq!(target.host, "collector");
        assert_eq!(target.base, "/otlp");
        assert_eq!(
            target.headers,
            vec![("a".to_string(), "b".to_string()), ("x-key".to_string(), "secret".to_string())]
        );

        assert_eq!(Target::parse("http://[::1]:4318", "").unwrap().address, "[::1]:4318");
        assert_eq!(Target::parse("http://[::1]", "").unwrap().address, "[::1]:80");
        assert!(Target::parse("https://collector:4318", "").unwrap_err().contains("TLS"));
        assert!(Target::parse("collector:4318", "").is_err());
        assert!(Target::parse("http://", "").is_err());
        assert!(Target::parse("http://c", "novalue").is_err());
    }

    #[test]
    fn json_strings() {
        assert_eq!(quoted("a\"b\\c\nd\u{1}"), "\"a\\\"b\\\\c\\nd\\u0001\"");
        assert_eq!(number(0.25), "0.25");
        assert_eq!(number(f64::NAN), "0");
    }

    #[test]
    fn buckets() {
        let mut h = Histogram::default();
        for value in [0.001, 0.005, 0.2, 61.0] {
            h.add(value);
        }
        assert_eq!(h.count, 4);
        assert_eq!(h.buckets[0], 2);
        assert_eq!(h.buckets[5], 1);
        assert_eq!(h.buckets[BOUNDS.len()], 1);
    }

    /// Takes one request and answers it with `status`.
    async fn collector(listener: &TcpListener, status: &str) -> String {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let n = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..n]);
            let text = String::from_utf8_lossy(&request).into_owned();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length: usize = head
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if body.len() >= length {
                    let answer = format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\n\r\n{{}}");
                    stream.write_all(answer.as_bytes()).await.unwrap();
                    return text;
                }
            }
            assert!(n > 0, "the request ended early");
        }
    }

    fn job(id: u64, kind: JobKind) -> Job {
        Job {
            id,
            kind,
            obj: Obj::Host(0),
            line: String::new(),
            timeout: 1000,
        }
    }

    #[tokio::test]
    async fn sends_metrics_and_spans() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/base", listener.local_addr().unwrap());
        let mut telemetry = Telemetry::start(Some(&endpoint)).unwrap();
        // Every check gets a trace in this test.
        telemetry.0.as_mut().unwrap().ratio = 1.0;
        assert!(telemetry.describe().unwrap().contains("/base"));

        telemetry.job_started(&job(1, JobKind::ServiceCheck), "web \"1\";disk", 0.25);
        telemetry.job_started(&job(2, JobKind::HostHandler), "web", 0.0);
        telemetry.job_done(1, &Outcome::Exited(2), nanos());
        assert!(telemetry.next_flush().is_some());

        let receive = async {
            let traces = collector(&listener, "200 OK").await;
            let metrics = collector(&listener, "200 OK").await;
            (traces, metrics)
        };
        let ((traces, metrics), ()) = tokio::join!(receive, telemetry.shutdown());

        assert!(traces.starts_with("POST /base/v1/traces HTTP/1.1\r\n"), "{traces}");
        assert!(traces.contains("Content-Type: application/json\r\n"));
        assert!(traces.contains("\"name\":\"service check web \\\"1\\\";disk\""), "{traces}");
        assert!(traces.contains("{\"key\":\"checkd.service\",\"value\":{\"stringValue\":\"disk\"}}"));
        assert!(traces.contains("{\"key\":\"checkd.exit_code\",\"value\":{\"intValue\":\"2\"}}"));
        assert!(traces.contains("\"name\":\"spawned\""));

        assert!(metrics.starts_with("POST /base/v1/metrics HTTP/1.1\r\n"), "{metrics}");
        for wanted in [
            "\"name\":\"checkd.checks\"",
            "\"name\":\"checkd.check.latency\"",
            "\"name\":\"checkd.command.duration\"",
            // The event handler has not ended.
            "\"name\":\"checkd.commands.running\"",
            "\"asInt\":\"1\"",
            "{\"key\":\"checkd.outcome\",\"value\":{\"stringValue\":\"exited\"}}",
        ] {
            assert!(metrics.contains(wanted), "{wanted} is not in {metrics}");
        }
        assert!(!metrics.contains("checkd.event_handlers"));
        // Braces and brackets pair up: a cheap test of the hand-written JSON.
        for body in [&traces, &metrics] {
            let json = body.split_once("\r\n\r\n").unwrap().1;
            let mut depth = 0i32;
            let mut in_text = false;
            let mut escaped = false;
            for c in json.chars() {
                match c {
                    _ if escaped => escaped = false,
                    '\\' if in_text => escaped = true,
                    '"' => in_text = !in_text,
                    '{' | '[' if !in_text => depth += 1,
                    '}' | ']' if !in_text => depth -= 1,
                    _ => {}
                }
                assert!(depth >= 0);
            }
            assert_eq!(depth, 0);
            assert!(!in_text);
        }
    }

    #[tokio::test]
    async fn a_collector_that_refuses() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = Target::parse(&format!("http://{}", listener.local_addr().unwrap()), "").unwrap();
        let body = Body {
            path: "/v1/metrics",
            json: "{}".to_string(),
        };
        let (_, sent) = tokio::join!(collector(&listener, "503 Service Unavailable"), post(&target, &body));
        assert!(sent.unwrap_err().contains("503"));
    }

    #[tokio::test]
    async fn off_without_an_endpoint() {
        // The test does not set the variable, and neither may its caller.
        if std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some() {
            return;
        }
        let mut telemetry = Telemetry::start(None).unwrap();
        assert!(telemetry.describe().is_none());
        assert!(telemetry.next_flush().is_none());
        telemetry.job_started(&job(1, JobKind::HostCheck), "a", 0.0);
        telemetry.job_done(1, &Outcome::TimedOut, nanos());
        telemetry.shutdown().await;
    }
}
