//! The HTTP API, off unless `--http <address>` is given.
//!
//! - `GET /api/v1/<table>` answers like a Livestatus `GET` of that table,
//!   as a JSON list of objects. Query parameters: `columns` (separated by
//!   commas), `filter` (Livestatus filter lines, separated by `;;`, as
//!   `state = 2;;host_name = web`), `limit`.
//! - `POST /api/v1/command` takes the body as one external command, as the
//!   command file would (`NAME;argument;...`, without the time).
//! - `GET` or `POST /api/v1/push/<token>` is a result for the service
//!   whose `_CHECKD_PUSH_TOKEN` the token is (`crate::push`).
//! - `GET /metrics` gives the engine's own health in Prometheus' text
//!   format.
//!
//! Without `CHECKD_HTTP_TOKEN` the API listens on the loopback only, and
//! a read names its columns and is refused what holds a secret or says
//! what is run: custom variables (push tokens among them), command
//! lines, a contact's addresses.
//!
//! With `CHECKD_HTTP_TOKEN` set, every request needs it as
//! `Authorization: Bearer <token>`, but for a push, which has a token of
//! its own. Without it, commands are refused: the API is then read-only.
//!
//! A request goes to the engine's loop, which answers it between events,
//! as it does Livestatus requests. Plain HTTP only: put it behind a proxy
//! that does TLS where the network is not trusted.

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

/// A request for the engine's loop.
pub struct Ask {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: String,
    pub authorized: bool,
    pub reply: oneshot::Sender<Reply>,
}

pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: String,
}

impl Reply {
    pub fn json(status: u16, body: String) -> Reply {
        Reply {
            status,
            content_type: "application/json",
            body,
        }
    }

    pub fn text(status: u16, body: impl Into<String>) -> Reply {
        Reply {
            status,
            content_type: "text/plain; charset=utf-8",
            body: body.into(),
        }
    }
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'+' => out.push(b' '),
            b'%' if at + 2 < bytes.len() => {
                if let Ok(n) = u8::from_str_radix(&text[at + 1..at + 3], 16) {
                    out.push(n);
                    at += 2;
                } else {
                    out.push(b'%');
                }
            }
            b => out.push(b),
        }
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The longest a request's first line and each header line may be.
const MOST_HEAD: u64 = 64 * 1024;
/// How long a client has for each part of its request.
const TAKES_AT_MOST: std::time::Duration = std::time::Duration::from_secs(15);

/// Reads one line of a request's head. False at the end of what was sent,
/// on an error, and for a line longer than a head line may be.
async fn read_head_line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R, line: &mut String) -> bool {
    let mut limited = reader.take(MOST_HEAD);
    match limited.read_line(line).await {
        Ok(0) | Err(_) => false,
        Ok(_) => line.ends_with('\n'),
    }
}

/// Starts listening. Requests come out of the answer.
pub async fn listen(addr: &str) -> Result<mpsc::UnboundedReceiver<Ask>, String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|err| format!("cannot listen for HTTP at {addr}: {err}"))?;
    let token = std::env::var("CHECKD_HTTP_TOKEN").ok().filter(|t| !t.is_empty());
    if token.is_none() && !loopback(addr) {
        return Err(format!(
            "--http {addr}: without CHECKD_HTTP_TOKEN the HTTP API listens on this machine only (127.0.0.1 or [::1]); set a token to listen elsewhere"
        ));
    }
    let (asks, inbox) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let asks = asks.clone();
            let token = token.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                // A request is its head and at most a megabyte of body, and
                // is here within a quarter of a minute: a client that sends
                // without end, or not at all, is let go.
                let mut reader = BufReader::new(read.take(MOST_HEAD + (1 << 20)));
                let mut first = String::new();
                let head = tokio::time::timeout(TAKES_AT_MOST, read_head_line(&mut reader, &mut first)).await;
                if !matches!(head, Ok(true)) {
                    return;
                }
                let mut parts = first.split_whitespace();
                let (Some(method), Some(target)) = (parts.next(), parts.next()) else { return };
                let (method, target) = (method.to_string(), target.to_string());
                let mut length = 0usize;
                let mut bearer = None;
                loop {
                    let mut line = String::new();
                    match tokio::time::timeout(TAKES_AT_MOST, read_head_line(&mut reader, &mut line)).await {
                        Ok(true) => {}
                        Ok(false) if line.is_empty() => break,
                        _ => return,
                    }
                    let line = line.trim_end();
                    if line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        let value = value.trim();
                        match name.to_ascii_lowercase().as_str() {
                            "content-length" => length = value.parse().unwrap_or(0),
                            "authorization" => bearer = value.strip_prefix("Bearer ").map(str::to_string),
                            _ => {}
                        }
                    }
                }
                let mut body = vec![0u8; length.min(1 << 20)];
                if !body.is_empty() && !matches!(tokio::time::timeout(TAKES_AT_MOST, reader.read_exact(&mut body)).await, Ok(Ok(_))) {
                    return;
                }
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let query: Vec<(String, String)> = query
                    .split('&')
                    .filter(|p| !p.is_empty())
                    .map(|p| {
                        let (k, v) = p.split_once('=').unwrap_or((p, ""));
                        (decode(k), decode(v))
                    })
                    .collect();
                let authorized = token.is_some() && bearer.as_deref() == token.as_deref();
                // A push carries its own token, in its path.
                let reply = if token.is_some() && !authorized && !path.starts_with("/api/v1/push/") {
                    Reply::text(401, "a bearer token is needed\n")
                } else {
                    let (send, answer) = oneshot::channel();
                    let ask = Ask {
                        method,
                        path: path.to_string(),
                        query,
                        body: String::from_utf8_lossy(&body).into_owned(),
                        authorized,
                        reply: send,
                    };
                    if asks.send(ask).is_err() {
                        return;
                    }
                    match answer.await {
                        Ok(reply) => reply,
                        Err(_) => return,
                    }
                };
                let reason = match reply.status {
                    200 => "OK",
                    400 => "Bad Request",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    404 => "Not Found",
                    405 => "Method Not Allowed",
                    _ => "Error",
                };
                let head = format!(
                    "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    reply.content_type,
                    reply.body.len()
                );
                let _ = write.write_all(head.as_bytes()).await;
                let _ = write.write_all(reply.body.as_bytes()).await;
                let _ = write.shutdown().await;
            });
        }
    });
    Ok(inbox)
}

/// Whether an address to listen at is on this machine only.
pub fn loopback(addr: &str) -> bool {
    match addr.parse::<std::net::SocketAddr>() {
        Ok(at) => at.ip().is_loopback(),
        Err(_) => addr.starts_with("localhost:"),
    }
}

/// A column that holds a secret or says what is run.
fn telling(column: &str) -> bool {
    ["custom_variable", "check_command", "event_handler", "command_line", "notification_commands"].iter().any(|part| column.contains(part))
        || column == "email"
        || column == "pager"
        || column.strip_prefix("address").is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit())
        || column.ends_with("_email")
        || column.ends_with("_pager")
}

/// Why a read without a token is refused, if it is: it must name its
/// columns, and neither they nor its filters may be ones that tell.
pub fn refused_without_token(table: &str, query: &[(String, String)]) -> Option<String> {
    if table == "commands" {
        return Some("the commands table needs a token: set CHECKD_HTTP_TOKEN".into());
    }
    let columns: Vec<&str> = query.iter().filter(|(k, _)| k == "columns").flat_map(|(_, v)| v.split(',')).map(str::trim).collect();
    if columns.is_empty() {
        return Some("without a token, name the columns: ?columns=name,state".into());
    }
    let filtered = query
        .iter()
        .filter(|(k, _)| k == "filter")
        .flat_map(|(_, v)| v.split(";;"))
        .filter_map(|f| f.split_whitespace().next());
    for column in columns.into_iter().chain(filtered) {
        if telling(column) {
            return Some(format!("the column {column} needs a token: set CHECKD_HTTP_TOKEN"));
        }
    }
    None
}

/// A value of the query string without what would end its line.
fn one_line(value: &str) -> String {
    value.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect()
}

/// The Livestatus request a `GET /api/v1/<table>` stands for.
pub fn livestatus_request(table: &str, query: &[(String, String)]) -> String {
    let mut request = format!("GET {table}\n");
    for (key, value) in query {
        match key.as_str() {
            // A value is one line: it cannot bring header lines of its own.
            "columns" => request.push_str(&format!("Columns: {}\n", one_line(value).replace(',', " "))),
            "filter" => {
                for filter in value.split(";;").filter(|f| !f.trim().is_empty()) {
                    request.push_str(&format!("Filter: {}\n", one_line(filter).trim()));
                }
            }
            "limit" => request.push_str(&format!("Limit: {}\n", one_line(value))),
            _ => {}
        }
    }
    request.push_str("OutputFormat: json\nColumnHeaders: on\n\n");
    request
}

/// A Livestatus JSON answer with its header row, as a list of objects.
pub fn as_objects(answer: &str) -> Option<String> {
    let checkd_livestatus::json::Json::List(rows) = checkd_livestatus::json::parse(answer)? else {
        return None;
    };
    let mut rows = rows.into_iter();
    let header: Vec<String> = match rows.next()? {
        checkd_livestatus::json::Json::List(names) => names.iter().map(|n| n.as_str().to_string()).collect(),
        _ => return None,
    };
    let mut out = String::from("[");
    for (n, row) in rows.enumerate() {
        if n > 0 {
            out.push_str(",\n");
        }
        out.push('{');
        for (k, (name, value)) in header.iter().zip(row.as_list()).enumerate() {
            if k > 0 {
                out.push(',');
            }
            checkd_livestatus::value::json_string(name, &mut out);
            out.push(':');
            write_json(value, &mut out);
        }
        out.push('}');
    }
    out.push_str("]\n");
    Some(out)
}

fn write_json(value: &checkd_livestatus::json::Json, out: &mut String) {
    use checkd_livestatus::json::Json;
    match value {
        Json::Null => out.push_str("null"),
        Json::Num(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                out.push_str(&format!("{}", *n as i64));
            } else {
                out.push_str(&format!("{n}"));
            }
        }
        Json::Str(s) => checkd_livestatus::value::json_string(s, out),
        Json::List(items) => {
            out.push('[');
            for (n, item) in items.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                write_json(item, out);
            }
            out.push(']');
        }
        Json::Obj(items) => {
            out.push('{');
            for (n, (k, v)) in items.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                checkd_livestatus::value::json_string(k, out);
                out.push(':');
                write_json(v, out);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_read_without_a_token_may_ask() {
        let q = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>();
        assert_eq!(refused_without_token("hosts", &q(&[("columns", "name,state,address")])), None);
        for asked in [vec![], vec![("columns", "name,custom_variables")], vec![("columns", "name"), ("filter", "custom_variable_values ~ x")], vec![("columns", "host_name,check_command")]] {
            assert!(refused_without_token("services", &q(&asked)).is_some(), "{asked:?}");
        }
        assert!(refused_without_token("contacts", &q(&[("columns", "name,email")])).is_some());
        assert!(refused_without_token("contacts", &q(&[("columns", "name,address3")])).is_some());
        assert!(refused_without_token("commands", &q(&[("columns", "name")])).is_some());
        assert!(loopback("127.0.0.1:6559") && loopback("[::1]:6559") && loopback("localhost:6559"));
        assert!(!loopback("0.0.0.0:6559") && !loopback("192.0.2.4:6559"));
    }

    #[test]
    fn requests_and_objects() {
        let query = vec![
            ("columns".to_string(), "name,state".to_string()),
            ("filter".to_string(), "state = 2;;name ~ web".to_string()),
        ];
        assert_eq!(
            livestatus_request("hosts", &query),
            "GET hosts\nColumns: name state\nFilter: state = 2\nFilter: name ~ web\nOutputFormat: json\nColumnHeaders: on\n\n"
        );
        let objects = as_objects("[[\"name\",\"state\"],\n[\"web\",2]]\n").unwrap();
        assert_eq!(objects, "[{\"name\":\"web\",\"state\":2}]\n");
        assert_eq!(decode("a%20b+c"), "a b c");
    }
}
