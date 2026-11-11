//! Performance data sent straight to a metrics store, beside or instead of
//! the performance data files.
//!
//! - `--perfdata-graphite <host:port>`: Graphite's plaintext protocol over
//!   TCP, one line per value: `checkd.<host>.<service>.<label> <value>
//!   <time>`, with `_host_` for a host's own data. Characters that Graphite
//!   takes as separators become `_`.
//! - `--perfdata-influx <url>`: InfluxDB's line protocol, posted to the URL
//!   given (`http://db:8086/write?db=checkd`, or the `/api/v2/write?...`
//!   form), measurement `checkd`, tags `host`, `service` and `label`, fields
//!   `value`, and `warn`, `crit`, `min`, `max` where the plugin gave them.
//!   `CHECKD_INFLUX_TOKEN` is sent as `Authorization: Token ...` when set.
//!   Plain HTTP only.
//!
//! Values are sent in batches, every five seconds. When the store cannot
//! be reached, what waits is dropped beyond 100,000 lines, and that is
//! said once in the log.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

/// One value of a plugin's performance data.
#[derive(Clone, Debug, PartialEq)]
pub struct Value {
    pub label: String,
    pub value: f64,
    pub unit: String,
    pub warn: Option<f64>,
    pub crit: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// Reads performance data as the plugin guidelines write it:
/// `label=value[unit];[warn];[crit];[min];[max]`, separated by spaces, with
/// the label in single quotes when it holds spaces. Parts that cannot be
/// read are skipped.
pub fn parse(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut at = 0;
    while at < chars.len() {
        while at < chars.len() && chars[at].is_whitespace() {
            at += 1;
        }
        if at >= chars.len() {
            break;
        }
        let mut label = String::new();
        if chars[at] == '\'' {
            at += 1;
            while at < chars.len() {
                if chars[at] == '\'' {
                    if chars.get(at + 1) == Some(&'\'') {
                        label.push('\'');
                        at += 2;
                        continue;
                    }
                    at += 1;
                    break;
                }
                label.push(chars[at]);
                at += 1;
            }
        } else {
            while at < chars.len() && chars[at] != '=' && !chars[at].is_whitespace() {
                label.push(chars[at]);
                at += 1;
            }
        }
        if chars.get(at) != Some(&'=') {
            while at < chars.len() && !chars[at].is_whitespace() {
                at += 1;
            }
            continue;
        }
        at += 1;
        let start = at;
        while at < chars.len() && !chars[at].is_whitespace() {
            at += 1;
        }
        let rest: String = chars[start..at].iter().collect();
        let mut parts = rest.split(';');
        let first = parts.next().unwrap_or("");
        let number_end = first
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E'))
            .unwrap_or(first.len());
        let Ok(value) = first[..number_end].parse::<f64>() else { continue };
        let unit = first[number_end..].to_string();
        let mut number = || parts.next().and_then(|p| p.trim().parse::<f64>().ok());
        let (warn, crit, min, max) = (number(), number(), number(), number());
        if !label.is_empty() {
            out.push(Value {
                label,
                value,
                unit,
                warn,
                crit,
                min,
                max,
            });
        }
    }
    out
}

/// What is sent for one result.
#[derive(Clone, Debug)]
pub struct Sample {
    pub host: String,
    /// Empty for a host.
    pub service: String,
    /// Seconds since the epoch.
    pub time: i64,
    pub perfdata: String,
}

fn graphite_part(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() { "_".into() } else { cleaned }
}

pub fn graphite_lines(sample: &Sample) -> Vec<String> {
    let service = if sample.service.is_empty() { "_host_".to_string() } else { graphite_part(&sample.service) };
    parse(&sample.perfdata)
        .into_iter()
        .map(|v| {
            format!(
                "checkd.{}.{service}.{} {} {}",
                graphite_part(&sample.host),
                graphite_part(&v.label),
                v.value,
                sample.time
            )
        })
        .collect()
}

fn influx_tag(text: &str) -> String {
    text.replace('\\', "\\\\").replace(',', "\\,").replace('=', "\\=").replace(' ', "\\ ")
}

pub fn influx_lines(sample: &Sample) -> Vec<String> {
    parse(&sample.perfdata)
        .into_iter()
        .map(|v| {
            let mut fields = format!("value={}", v.value);
            for (name, part) in [("warn", v.warn), ("crit", v.crit), ("min", v.min), ("max", v.max)] {
                if let Some(x) = part {
                    fields.push_str(&format!(",{name}={x}"));
                }
            }
            if !v.unit.is_empty() {
                fields.push_str(&format!(",unit=\"{}\"", v.unit.replace('"', "\\\"")));
            }
            let mut tags = format!("host={}", influx_tag(&sample.host));
            if !sample.service.is_empty() {
                tags.push_str(&format!(",service={}", influx_tag(&sample.service)));
            }
            tags.push_str(&format!(",label={}", influx_tag(&v.label)));
            format!("checkd,{tags} {fields} {}", sample.time * 1_000_000_000)
        })
        .collect()
}

#[derive(Clone, Debug)]
pub enum Target {
    Graphite(String),
    /// Host and port, and the path with its query.
    Influx(String, String),
}

impl Target {
    pub fn graphite(addr: &str) -> Target {
        Target::Graphite(addr.to_string())
    }

    pub fn influx(url: &str) -> Result<Target, String> {
        let rest = url
            .strip_prefix("http://")
            .ok_or_else(|| format!("--perfdata-influx: only http:// URLs, not '{url}'"))?;
        let (hostport, path) = rest.split_once('/').unwrap_or((rest, "write"));
        Ok(Target::Influx(hostport.to_string(), format!("/{path}")))
    }
}

/// Starts the sender. Samples go into the answer. Lines for the log come
/// out of `notes`.
pub fn start(targets: Vec<Target>, notes: mpsc::UnboundedSender<String>) -> mpsc::UnboundedSender<Sample> {
    let (samples, mut inbox) = mpsc::unbounded_channel::<Sample>();
    tokio::spawn(async move {
        let mut waiting: Vec<Vec<String>> = targets.iter().map(|_| Vec::new()).collect();
        let mut failing: Vec<bool> = targets.iter().map(|_| false).collect();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                sample = inbox.recv() => {
                    let Some(sample) = sample else { return };
                    for (n, target) in targets.iter().enumerate() {
                        let lines = match target {
                            Target::Graphite(_) => graphite_lines(&sample),
                            Target::Influx(..) => influx_lines(&sample),
                        };
                        waiting[n].extend(lines);
                        if waiting[n].len() > 100_000 {
                            let extra = waiting[n].len() - 100_000;
                            waiting[n].drain(..extra);
                        }
                    }
                }
                _ = tick.tick() => {
                    for (n, target) in targets.iter().enumerate() {
                        if waiting[n].is_empty() {
                            continue;
                        }
                        match send(target, &waiting[n]).await {
                            Ok(()) => {
                                if failing[n] {
                                    let _ = notes.send(format!("Performance data reach {target:?} again"));
                                }
                                failing[n] = false;
                                waiting[n].clear();
                            }
                            Err(err) => {
                                if !failing[n] {
                                    let _ = notes.send(format!("Warning: performance data cannot be sent to {target:?}: {err}"));
                                }
                                failing[n] = true;
                            }
                        }
                    }
                }
            }
        }
    });
    samples
}

async fn send(target: &Target, lines: &[String]) -> Result<(), String> {
    let mut body = lines.join("\n");
    body.push('\n');
    let limit = Duration::from_secs(10);
    match target {
        Target::Graphite(addr) => {
            let mut stream = tokio::time::timeout(limit, TcpStream::connect(addr))
                .await
                .map_err(|_| "no connection within 10 seconds".to_string())?
                .map_err(|err| err.to_string())?;
            stream.write_all(body.as_bytes()).await.map_err(|err| err.to_string())?;
            stream.shutdown().await.map_err(|err| err.to_string())
        }
        Target::Influx(hostport, path) => {
            let mut stream = tokio::time::timeout(limit, TcpStream::connect(hostport))
                .await
                .map_err(|_| "no connection within 10 seconds".to_string())?
                .map_err(|err| err.to_string())?;
            let token = std::env::var("CHECKD_INFLUX_TOKEN")
                .map(|t| format!("Authorization: Token {t}\r\n"))
                .unwrap_or_default();
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: {hostport}\r\nContent-Type: text/plain; charset=utf-8\r\n{token}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(request.as_bytes()).await.map_err(|err| err.to_string())?;
            let mut answer = Vec::new();
            let _ = tokio::time::timeout(limit, stream.read_to_end(&mut answer)).await;
            let status = String::from_utf8_lossy(&answer);
            let code: u16 = status.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
            if (200..300).contains(&code) { Ok(()) } else { Err(format!("answered {}", status.lines().next().unwrap_or(""))) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_perfdata() {
        let values = parse("time=0.5s;1;2;0;10 'disk use'=91% size=3B;;;0 bad junk=x");
        assert_eq!(values.len(), 3);
        assert_eq!(values[0].label, "time");
        assert_eq!((values[0].value, values[0].unit.as_str()), (0.5, "s"));
        assert_eq!((values[0].warn, values[0].crit, values[0].min, values[0].max), (Some(1.0), Some(2.0), Some(0.0), Some(10.0)));
        assert_eq!(values[1].label, "disk use");
        assert_eq!(values[2].min, Some(0.0));
    }

    #[test]
    fn graphite_and_influx() {
        let sample = Sample {
            host: "web.example".into(),
            service: "disk /var".into(),
            time: 1_800_000_000,
            perfdata: "'disk use'=91%;80;90".into(),
        };
        assert_eq!(graphite_lines(&sample), vec!["checkd.web_example.disk__var.disk_use 91 1800000000"]);
        assert_eq!(
            influx_lines(&sample),
            vec!["checkd,host=web.example,service=disk\\ /var,label=disk\\ use value=91,warn=80,crit=90,unit=\"%\" 1800000000000000000"]
        );
    }

    #[test]
    fn sends_to_a_graphite_listener() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            let got = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut text = String::new();
                stream.read_to_string(&mut text).await.unwrap();
                text
            });
            send(&Target::graphite(&addr), &["a.b 1 2".into(), "a.c 3 4".into()]).await.unwrap();
            assert_eq!(got.await.unwrap(), "a.b 1 2\na.c 3 4\n");
        });
    }
}
