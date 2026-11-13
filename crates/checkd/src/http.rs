//! The HTTP API, off unless `--http <address>` is given.
//!
//! - `GET /api/v1/<table>` answers like a Livestatus `GET` of that table,
//!   as a JSON list of objects. Query parameters: `columns` (separated by
//!   commas), `filter` (Livestatus filter lines, separated by `;;`, as
//!   `state = 2;;host_name = web`), `limit`.
//! - `POST /api/v1/command` takes the body as one external command, as the
//!   command file would (`NAME;argument;...`, without the time).
//! - `GET` or `POST /api/v1/push/<token>` is a result for the service
//!   whose `_PUSH_TOKEN` the token is (`crate::push`).
//! - `GET /metrics` gives the engine's own health in Prometheus' text
//!   format.
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

/// Starts listening. Requests come out of the answer.
pub async fn listen(addr: &str) -> Result<mpsc::UnboundedReceiver<Ask>, String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|err| format!("cannot listen for HTTP at {addr}: {err}"))?;
    let token = std::env::var("CHECKD_HTTP_TOKEN").ok().filter(|t| !t.is_empty());
    let (asks, inbox) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let asks = asks.clone();
            let token = token.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut reader = BufReader::new(read);
                let mut first = String::new();
                if reader.read_line(&mut first).await.is_err() {
                    return;
                }
                let mut parts = first.split_whitespace();
                let (Some(method), Some(target)) = (parts.next(), parts.next()) else { return };
                let (method, target) = (method.to_string(), target.to_string());
                let mut length = 0usize;
                let mut bearer = None;
                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
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
                if !body.is_empty() && reader.read_exact(&mut body).await.is_err() {
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

/// The Livestatus request a `GET /api/v1/<table>` stands for.
pub fn livestatus_request(table: &str, query: &[(String, String)]) -> String {
    let mut request = format!("GET {table}\n");
    for (key, value) in query {
        match key.as_str() {
            "columns" => request.push_str(&format!("Columns: {}\n", value.replace(',', " "))),
            "filter" => {
                for filter in value.split(";;").filter(|f| !f.trim().is_empty()) {
                    request.push_str(&format!("Filter: {}\n", filter.trim()));
                }
            }
            "limit" => request.push_str(&format!("Limit: {value}\n")),
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
