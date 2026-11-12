//! The `statehist` table: for each host and service, the spans of time it
//! spent in each state, worked out from the log.
//!
//! As the reference answers, learned from its answers on a config written
//! for the purpose:
//!
//! - the spans lie in the window the query's `time` filters give: from its
//!   lower bound, and up to its upper bound (less a second for `<`);
//! - before the first line about an object, it counts as in the state that
//!   line gives;
//! - a span ends where the state changes, soft states included, and where a
//!   downtime of the object starts or stops.

use crate::tables::Source;
use crate::value::Value;

/// The time the spans are worked out for: seconds since the epoch.
#[derive(Clone, Copy, Debug)]
pub struct Window {
    pub from: i64,
    pub until: i64,
}

impl Window {
    pub fn all() -> Window {
        Window {
            from: 0,
            until: i64::MAX,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Span {
    pub host: String,
    pub service: String,
    pub state: u8,
    pub from: i64,
    pub until: i64,
    pub in_downtime: bool,
    pub output: String,
}

/// A line of the log about one object's state or downtime.
enum Event {
    State(u8, String),
    Downtime(bool),
}

fn event(text: &str) -> Option<(String, String, Event)> {
    let (kind, rest) = text.split_once(": ")?;
    let fields: Vec<&str> = rest.split(';').collect();
    let service_kind = kind.contains("SERVICE");
    let (host, service, after) = if service_kind {
        (fields.first()?.to_string(), fields.get(1)?.to_string(), 2)
    } else {
        (fields.first()?.to_string(), String::new(), 1)
    };
    let state_of = |name: &str| -> Option<u8> {
        Some(match (service_kind, name) {
            (false, "UP") | (true, "OK") => 0,
            (false, "DOWN") | (true, "WARNING") => 1,
            (false, "UNREACHABLE") | (true, "CRITICAL") => 2,
            (true, "UNKNOWN") => 3,
            _ => return None,
        })
    };
    let event = match kind {
        "HOST ALERT" | "SERVICE ALERT" | "INITIAL HOST STATE" | "INITIAL SERVICE STATE" | "CURRENT HOST STATE"
        | "CURRENT SERVICE STATE" => Event::State(
            state_of(fields.get(after)?)?,
            fields.get(after + 3).map(|s| s.to_string()).unwrap_or_default(),
        ),
        "HOST DOWNTIME ALERT" | "SERVICE DOWNTIME ALERT" => Event::Downtime(*fields.get(after)? == "STARTED"),
        _ => return None,
    };
    Some((host, service, event))
}

/// The spans of every object in a window, in the order of the objects'
/// first lines.
pub fn spans(source: &Source, window: &Window) -> Vec<Span> {
    let window = source.window.unwrap_or(*window);
    let mut order: Vec<(String, String)> = Vec::new();
    let mut open: std::collections::BTreeMap<(String, String), Span> = std::collections::BTreeMap::new();
    let mut done: Vec<Span> = Vec::new();
    for (time, text) in source.log {
        if *time > window.until {
            break;
        }
        let Some((host, service, event)) = event(text) else { continue };
        let key = (host.clone(), service.clone());
        let at = (*time).max(window.from);
        match open.get_mut(&key) {
            None => {
                let (state, output, in_downtime) = match event {
                    Event::State(state, output) => (state, output, false),
                    Event::Downtime(on) => (0, String::new(), on),
                };
                order.push(key.clone());
                open.insert(
                    key,
                    Span {
                        host,
                        service,
                        state,
                        from: window.from,
                        until: window.until,
                        in_downtime,
                        output,
                    },
                );
            }
            Some(span) => {
                let (state, output, in_downtime) = match &event {
                    Event::State(state, output) => (*state, output.clone(), span.in_downtime),
                    Event::Downtime(on) => (span.state, span.output.clone(), *on),
                };
                if state == span.state && in_downtime == span.in_downtime {
                    span.output = output;
                    continue;
                }
                if at > span.from {
                    let mut ended = span.clone();
                    ended.until = at;
                    done.push(ended);
                }
                span.from = at;
                span.state = state;
                span.output = output;
                span.in_downtime = in_downtime;
            }
        }
    }
    let mut all: Vec<Span> = Vec::new();
    for key in order {
        let mut mine: Vec<Span> = done.iter().filter(|s| (s.host.clone(), s.service.clone()) == key).cloned().collect();
        if let Some(mut last) = open.remove(&key) {
            last.until = window.until.min(source.now.div_euclid(1000));
            mine.push(last);
        }
        all.extend(mine);
    }
    // Only of what is configured now, as the reference has it: the log's
    // lines about a host or service that was taken out give no spans.
    let engine = source.engine;
    all.retain(|span| {
        if span.service.is_empty() { engine.host(&span.host).is_some() } else { engine.service(&span.host, &span.service).is_some() }
    });
    all.sort_by(|a, b| (&a.host, &a.service, a.from).cmp(&(&b.host, &b.service, b.from)));
    all
}

pub fn column(_source: &Source, span: &Span, column: &str) -> Option<Value> {
    let duration = span.until - span.from;
    let in_state = |wanted: u8| Value::Int(if span.state == wanted { duration } else { 0 });
    // By the number of the state, for a host as for a service: the
    // reference counts a host's DOWN (1) as warning and its UNREACHABLE
    // (2) as critical.
    let (warning, critical, unknown) = (1, 2, 3);
    Some(match column {
        "host_name" => Value::str(&span.host),
        "service_description" => Value::str(&span.service),
        "state" => Value::Int(span.state as i64),
        "from" | "time" => Value::Int(span.from),
        "until" => Value::Int(span.until),
        "duration" => Value::Int(duration),
        "duration_ok" => in_state(0),
        "duration_warning" => in_state(warning),
        "duration_critical" => in_state(critical),
        "duration_unknown" => in_state(unknown),
        "duration_unmonitored" => Value::Int(0),
        "in_downtime" => Value::bool(span.in_downtime),
        "log_output" => Value::str(&span.output),
        _ => return None,
    })
}
