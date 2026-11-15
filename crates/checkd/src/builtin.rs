//! Checks checkd makes itself, without a plugin: a command line that
//! begins with `checkd:` starts no process.
//!
//!     checkd:ping -H <host> [-w <ms>] [-c <ms>]
//!     checkd:tcp  -H <host> -p <port> [-w <s>] [-c <s>]
//!     checkd:http -H <host> [-p <port>] [-u <path>] [-S] [-e <codes>] [-s <text>] [-w <s>] [-c <s>]
//!     checkd:cert -H <host> [-p <port>] [-w <days>] [-c <days>]
//!
//! and some of the machine they run on (`local.rs`), for an agent or the
//! engine's own host:
//!
//!     checkd:disk -p <path> [-w <% used>] [-c <% used>]
//!     checkd:memory [-w <% used>] [-c <% used>]
//!     checkd:load [-w <1,5,15>] [-c <1,5,15>]
//!     checkd:cpu [-w <% busy>] [-c <% busy>]
//!     checkd:service -s <name>
//!
//! and one that is not a check but a way to tell someone, for a
//! notification command:
//!
//!     checkd:notify -u <url> -t <title> -m <message> [-f json|ntfy|slack] [-p <priority>]
//!
//! They answer as a plugin does: a line of text with performance data
//! and one of the four states. The reference knows nothing of them: a
//! config that uses them is a config for checkd only. See
//! `docs/builtin.md`.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use checkd_core::engine::{JobResult, Ms, Outcome};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

const OK: i32 = 0;
const WARNING: i32 = 1;
const CRITICAL: i32 = 2;
const UNKNOWN: i32 = 3;
const WORDS: [&str; 4] = ["OK", "WARNING", "CRITICAL", "UNKNOWN"];

/// Whether a command line is one of checkd's own checks.
pub fn is_builtin(line: &str) -> bool {
    checkd_core::cmdline::own(line)
}

/// What a check found: the state, the text after it, the performance data.
type Found = (i32, String, String);

fn answer(name: &str, found: Found) -> JobResult {
    let (code, text, perfdata) = found;
    let bar = if perfdata.is_empty() { String::new() } else { format!("|{perfdata}") };
    JobResult {
        outcome: Outcome::Exited(code),
        stdout: format!("{name} {} - {text}{bar}\n", WORDS[code.clamp(0, 3) as usize]),
        stderr: String::new(),
    }
}

/// The options of a check: `-H value`, and `-S` alone.
struct Options(BTreeMap<String, String>);

impl Options {
    fn read(words: &[String]) -> Result<Options, String> {
        let mut options = BTreeMap::new();
        let mut at = 0;
        while at < words.len() {
            let flag = &words[at];
            if !flag.starts_with('-') || flag.len() != 2 {
                return Err(format!("'{flag}' is not an option"));
            }
            if flag == "-S" {
                options.insert(flag.clone(), String::new());
                at += 1;
                continue;
            }
            let value = words.get(at + 1).ok_or_else(|| format!("{flag} needs a value"))?;
            options.insert(flag.clone(), value.clone());
            at += 2;
        }
        Ok(Options(options))
    }

    fn text(&self, flag: &str) -> Option<&str> {
        self.0.get(flag).map(String::as_str)
    }

    fn has(&self, flag: &str) -> bool {
        self.0.contains_key(flag)
    }

    fn number(&self, flag: &str) -> Result<Option<f64>, String> {
        match self.text(flag) {
            None => Ok(None),
            Some(text) => text.parse().map(Some).map_err(|_| format!("{flag} takes a number, not '{text}'")),
        }
    }

    fn host(&self) -> Result<&str, String> {
        self.text("-H").filter(|host| !host.is_empty()).ok_or_else(|| "-H <host> is needed".to_string())
    }

    fn port(&self, default: Option<u16>) -> Result<u16, String> {
        match self.text("-p") {
            Some(text) => text.parse().map_err(|_| format!("-p takes a port, not '{text}'")),
            None => default.ok_or_else(|| "-p <port> is needed".to_string()),
        }
    }
}

/// The state a measured value gives: over `-c` critical, over `-w` warning.
fn over(value: f64, warn: Option<f64>, crit: Option<f64>) -> i32 {
    if crit.is_some_and(|c| value > c) {
        CRITICAL
    } else if warn.is_some_and(|w| value > w) {
        WARNING
    } else {
        OK
    }
}

fn limit(value: Option<f64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

/// Runs one of checkd's own checks. `timeout` is the check's, as for a
/// plugin: a check still at work then gives the result of one that timed
/// out.
pub async fn run(line: &str, timeout: Ms) -> JobResult {
    // The engine settles the words before it fills in what the moment
    // brings (`checkd_core::macros`), so that plugin output is only ever
    // inside a word. A line a person typed is split here, by its quotes.
    let words = checkd_core::cmdline::own_words(line);
    let kind = words.first().map(String::as_str).unwrap_or("").trim_start_matches("checkd:").to_string();
    let name = kind.to_uppercase();
    // A fixed answer, for a service whose results come from elsewhere:
    // `checkd:dummy 2 "nothing came in time"`, as freshness forces it.
    if kind == "dummy" {
        let code = words.get(1).and_then(|c| c.parse::<i32>().ok()).filter(|c| (0..=3).contains(c));
        return match code {
            Some(code) => JobResult { outcome: Outcome::Exited(code), stdout: format!("{}\n", words[2..].join(" ")), stderr: String::new() },
            None => answer("DUMMY", (UNKNOWN, "checkd:dummy takes a state, 0 to 3, and a text".into(), String::new())),
        };
    }
    let options = match Options::read(&words[1..]) {
        Ok(options) => options,
        Err(why) => return answer(&name, (UNKNOWN, why, String::new())),
    };
    let work = async {
        match kind.as_str() {
            "ping" => ping(&options).await,
            "tcp" => tcp(&options).await,
            "http" => http(&options).await,
            "cert" => cert(&options).await,
            "notify" => notify(&options).await,
            "disk" => disk(&options),
            "memory" => memory(&options),
            "load" => load(&options),
            "cpu" => cpu(&options).await,
            "service" => service(&options).await,
            "aggregate" | "trend" => Err("this one is worked out by the engine that has the hosts and services, not here".to_string()),
            other => Err(format!("checkd has no check '{other}': ping, tcp, http, cert, disk, memory, load, cpu, service, dummy, aggregate, trend, notify")),
        }
    };
    match tokio::time::timeout(Duration::from_millis(timeout.max(0) as u64), work).await {
        Ok(Ok(found)) => answer(&name, found),
        Ok(Err(why)) => answer(&name, (UNKNOWN, why, String::new())),
        Err(_) => JobResult { outcome: Outcome::TimedOut, stdout: String::new(), stderr: String::new() },
    }
}

/// Bytes as a person reads them.
fn size(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{value:.1} {}", units[unit]) }
}

fn disk(options: &Options) -> Result<Found, String> {
    let path = options.text("-p").filter(|p| !p.is_empty()).ok_or("-p <path> is needed")?;
    let (warn, crit) = (options.number("-w")?, options.number("-c")?);
    let space = crate::local::space(path)?;
    let used = space.used_pct();
    Ok((
        over(used, warn, crit),
        format!("{path} {used:.0}% used, {} free of {}", size(space.free), size(space.total)),
        format!("used={used:.1}%;{};{};0;100 free={}B;;;0;{} free_pct={:.1}%;;;0;100", limit(warn), limit(crit), space.free, space.total, 100.0 - used),
    ))
}

fn memory(options: &Options) -> Result<Found, String> {
    let (warn, crit) = (options.number("-w")?, options.number("-c")?);
    let (total, available) = crate::local::memory()?;
    let used = if total == 0 { 0.0 } else { (total - available.min(total)) as f64 * 100.0 / total as f64 };
    Ok((
        over(used, warn, crit),
        format!("{used:.0}% used, {} available of {}", size(available), size(total)),
        format!("used={used:.1}%;{};{};0;100 available={available}B;;;0;{total}", limit(warn), limit(crit)),
    ))
}

/// `-w 4,3,2`: limits over 1, 5 and 15 minutes; one number is all three.
fn triple(options: &Options, flag: &str) -> Result<[Option<f64>; 3], String> {
    let Some(text) = options.text(flag) else { return Ok([None; 3]) };
    let parts: Vec<&str> = text.split(',').collect();
    let number = |t: &str| t.trim().parse::<f64>().map_err(|_| format!("{flag} takes numbers, not '{text}'"));
    match parts.as_slice() {
        [one] => {
            let n = number(one)?;
            Ok([Some(n); 3])
        }
        [a, b, c] => Ok([Some(number(a)?), Some(number(b)?), Some(number(c)?)]),
        _ => Err(format!("{flag} takes one number or three, as 4,3,2")),
    }
}

fn load(options: &Options) -> Result<Found, String> {
    let (warn, crit) = (triple(options, "-w")?, triple(options, "-c")?);
    let now = crate::local::load().ok_or("this system has no load average")?;
    let state = (0..3).map(|i| over(now[i], warn[i], crit[i])).max().unwrap_or(OK);
    let perf: Vec<String> = ["load1", "load5", "load15"]
        .iter()
        .enumerate()
        .map(|(i, name)| format!("{name}={:.2};{};{};0", now[i], limit(warn[i]), limit(crit[i])))
        .collect();
    Ok((
        state,
        format!("load average {:.2}, {:.2}, {:.2} on {} processors", now[0], now[1], now[2], crate::local::cpus()),
        perf.join(" "),
    ))
}

async fn cpu(options: &Options) -> Result<Found, String> {
    let (warn, crit) = (options.number("-w")?, options.number("-c")?);
    let busy = crate::local::cpu_busy().await?;
    Ok((over(busy, warn, crit), format!("{busy:.0}% busy"), format!("busy={busy:.1}%;{};{};0;100", limit(warn), limit(crit))))
}

async fn service(options: &Options) -> Result<Found, String> {
    let name = options.text("-s").filter(|s| !s.is_empty()).ok_or("-s <name> is needed")?;
    Ok(match crate::local::service(name).await? {
        None => (CRITICAL, format!("{name}: no such service"), String::new()),
        Some((true, state)) => (OK, format!("{name} is {state}"), String::new()),
        Some((false, state)) => (CRITICAL, format!("{name} is {state}"), String::new()),
    })
}

/// The addresses of a name, or the address it is.
async fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let found: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await.map_err(|err| format!("{host}: {err}"))?.collect();
    if found.is_empty() { Err(format!("{host} has no address")) } else { Ok(found) }
}

/// An error of the system in its own words, without its number.
fn plain(err: &std::io::Error) -> String {
    let text = err.to_string();
    match text.find(" (os error") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// Connects to the first address that takes the connection.
async fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let mut last = String::new();
    for address in resolve(host, port).await? {
        match TcpStream::connect(address).await {
            Ok(stream) => return Ok(stream),
            Err(err) => last = plain(&err),
        }
    }
    Err(last)
}

async fn tcp(options: &Options) -> Result<Found, String> {
    let (host, port) = (options.host()?, options.port(None)?);
    let (warn, crit) = (options.number("-w")?, options.number("-c")?);
    let started = Instant::now();
    // A name that is not known, like a port that is closed, is the
    // check's finding, not a fault of the check.
    if let Err(why) = connect(host, port).await {
        return Ok((CRITICAL, format!("{host} port {port}: {why}"), String::new()));
    }
    let took = started.elapsed().as_secs_f64();
    Ok((
        over(took, warn, crit),
        format!("{took:.3} second response time on {host} port {port}"),
        format!("time={took:.6}s;{};{};0", limit(warn), limit(crit)),
    ))
}

/// A connection, plain or over TLS.
trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

async fn http(options: &Options) -> Result<Found, String> {
    let host = options.host()?;
    let secure = options.has("-S");
    let port = options.port(Some(if secure { 443 } else { 80 }))?;
    let path = options.text("-u").unwrap_or("/");
    let (warn, crit) = (options.number("-w")?, options.number("-c")?);
    let expected: Option<Vec<&str>> = options.text("-e").map(|codes| codes.split(',').map(str::trim).collect());
    let wanted = options.text("-s");

    let started = Instant::now();
    let plain = match connect(host, port).await {
        Ok(stream) => stream,
        Err(why) => return Ok((CRITICAL, format!("{host} port {port}: {why}"), String::new())),
    };
    let mut stream: Box<dyn Stream> = if secure {
        match tls::connect(plain, host).await? {
            (stream, _, None) => Box::new(stream),
            (_, _, Some(why)) => return Ok((CRITICAL, format!("the certificate of {host} is not trusted: {why}"), String::new())),
        }
    } else {
        Box::new(plain)
    };
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: checkd\r\nAccept: */*\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.map_err(|err| err.to_string())?;
    // The head, and as much of the page as a text is looked for in.
    let most = if wanted.is_some() { 1 << 20 } else { 16 << 10 };
    let mut page = Vec::new();
    let mut buffer = vec![0u8; 8192];
    while page.len() < most {
        match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(n) => page.extend_from_slice(&buffer[..n]),
            // A server may end a TLS connection without saying so.
            Err(_) => break,
        }
        if wanted.is_none() && page.windows(2).any(|w| w == b"\r\n") {
            break;
        }
    }
    let took = started.elapsed().as_secs_f64();
    let text = String::from_utf8_lossy(&page);
    let status = text.lines().next().unwrap_or("").trim().to_string();
    let code = status.split_whitespace().nth(1).unwrap_or("");
    if !status.starts_with("HTTP/") || code.len() != 3 {
        return Ok((CRITICAL, format!("{host} port {port} does not answer as a web server"), String::new()));
    }
    let mut state = match (&expected, code.as_bytes()[0]) {
        (Some(codes), _) => if codes.contains(&code) { OK } else { CRITICAL },
        (None, b'2' | b'3') => OK,
        (None, b'4') => WARNING,
        (None, _) => CRITICAL,
    };
    let mut said = format!("{status} - {took:.3} second response time");
    if let Some(wanted) = wanted {
        if !text.contains(wanted) {
            state = CRITICAL;
            said = format!("{status} - '{wanted}' is not on the page");
        }
    }
    Ok((
        state.max(over(took, warn, crit)),
        said,
        format!("time={took:.6}s;{};{};0 size={}B;;;0", limit(warn), limit(crit), page.len()),
    ))
}

/// A text as a JSON string.
fn json(text: &str) -> String {
    let mut out = String::from("\"");
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

/// A URL in its parts: whether it is HTTPS, the host, the port, the path.
fn url_parts(url: &str) -> Result<(bool, String, u16, String), String> {
    let (secure, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(rest), _) => (true, rest),
        (_, Some(rest)) => (false, rest),
        _ => return Err(format!("'{url}' is not an http or https address")),
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') || path.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("'{url}' is not an address to send to"));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            (host, port.parse().map_err(|_| format!("'{port}' is not a port"))?)
        }
        _ => (authority, if secure { 443 } else { 80 }),
    };
    Ok((secure, host.to_string(), port, path.to_string()))
}

/// Tells someone: a notification command that starts no program. The
/// title and the message are sent to an address, as the service there
/// takes them.
async fn notify(options: &Options) -> Result<Found, String> {
    let url = options.text("-u").ok_or("-u <url> is needed")?;
    let title = options.text("-t").unwrap_or("checkd");
    let message = options.text("-m").unwrap_or("");
    let priority = options.text("-p");
    let (secure, host, port, mut path) = url_parts(url)?;
    let body = match options.text("-f").unwrap_or("json") {
        "json" => format!("{{\"title\":{},\"message\":{}}}", json(title), json(message)),
        "slack" => format!("{{\"text\":{}}}", json(&format!("*{title}*\n{message}"))),
        // ntfy: the topic is the last part of the address, and the rest
        // of it is where everything is published.
        "ntfy" => {
            let topic = path.trim_matches('/').to_string();
            if topic.is_empty() || topic.contains('/') {
                return Err("for ntfy, the address ends with the topic: https://ntfy.example.com/<topic>".to_string());
            }
            path = "/".to_string();
            let priority = match priority {
                None => String::new(),
                Some(p) if matches!(p, "1" | "2" | "3" | "4" | "5") => format!(",\"priority\":{p}"),
                Some(p) => return Err(format!("for ntfy, -p is 1 to 5, not '{p}'")),
            };
            format!("{{\"topic\":{},\"title\":{},\"message\":{}{priority}}}", json(&topic), json(title), json(message))
        }
        other => return Err(format!("-f is json, ntfy or slack, not '{other}'")),
    };
    let plain = match connect(&host, port).await {
        Ok(stream) => stream,
        Err(why) => return Ok((CRITICAL, format!("not sent: {host} port {port}: {why}"), String::new())),
    };
    let mut stream: Box<dyn Stream> = if secure {
        match tls::connect(plain, &host).await? {
            (stream, _, None) => Box::new(stream),
            (_, _, Some(why)) => return Ok((CRITICAL, format!("not sent: the certificate of {host} is not trusted: {why}"), String::new())),
        }
    } else {
        Box::new(plain)
    };
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: checkd\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.map_err(|err| err.to_string())?;
    let mut head = vec![0u8; 512];
    let n = stream.read(&mut head).await.unwrap_or(0);
    let status = String::from_utf8_lossy(&head[..n]).lines().next().unwrap_or("").trim().to_string();
    let code = status.split_whitespace().nth(1).unwrap_or("");
    if code.starts_with('2') {
        Ok((OK, format!("sent to {host} ({status})"), String::new()))
    } else if status.is_empty() {
        Ok((CRITICAL, format!("not sent: {host} gave no answer"), String::new()))
    } else {
        Ok((CRITICAL, format!("not sent: {host} answered {status}"), String::new()))
    }
}

async fn cert(options: &Options) -> Result<Found, String> {
    let (host, port) = (options.host()?, options.port(Some(443))?);
    let warn = options.number("-w")?.unwrap_or(21.0);
    let crit = options.number("-c")?.unwrap_or(7.0);
    let plain = match connect(host, port).await {
        Ok(stream) => stream,
        Err(why) => return Ok((CRITICAL, format!("{host} port {port}: {why}"), String::new())),
    };
    let (_, der, distrust) = tls::connect(plain, host).await?;
    let Some(ends) = der::not_after(&der) else {
        return Err(format!("the certificate of {host} cannot be read"));
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let days = (ends - now).div_euclid(86_400);
    let perfdata = format!("days={days};{warn};{crit}");
    let until = der::day(ends);
    if days < 0 {
        return Ok((CRITICAL, format!("the certificate of {host} ended {} days ago ({until})", -days), perfdata));
    }
    if let Some(why) = distrust {
        return Ok((CRITICAL, format!("the certificate of {host} is not trusted: {why}"), perfdata));
    }
    let state = if (days as f64) < crit {
        CRITICAL
    } else if (days as f64) < warn {
        WARNING
    } else {
        OK
    };
    Ok((state, format!("the certificate of {host} ends in {days} days ({until})"), perfdata))
}

async fn ping(options: &Options) -> Result<Found, String> {
    let host = options.host()?.to_string();
    // Thresholds in milliseconds, as the times are said.
    let (warn, crit) = (options.number("-w")?, options.number("-c")?);
    let address = match resolve(&host, 0).await {
        Ok(found) => found[0].ip(),
        Err(why) => return Ok((CRITICAL, why, String::new())),
    };
    let (sent, times) = tokio::task::spawn_blocking(move || icmp::echo(address, 3, Duration::from_secs(1)))
        .await
        .map_err(|err| err.to_string())??;
    let loss = (sent - times.len()) * 100 / sent.max(1);
    if times.is_empty() {
        return Ok((CRITICAL, format!("{host} does not answer: packet loss = 100%"), "pl=100%;;;0;100".to_string()));
    }
    let average = times.iter().sum::<f64>() / times.len() as f64;
    let state = over(average, warn, crit).max(if loss > 0 { WARNING } else { OK });
    Ok((
        state,
        format!("packet loss = {loss}%, RTA = {average:.2} ms"),
        format!("rta={average:.3}ms;{};{};0 pl={loss}%;;;0;100", limit(warn), limit(crit)),
    ))
}

/// No echo requests without Linux's unprivileged ICMP sockets.
#[cfg(not(unix))]
mod icmp {
    use std::net::IpAddr;
    use std::time::Duration;

    pub fn echo(_: IpAddr, _: usize, _: Duration) -> Result<(usize, Vec<f64>), String> {
        Err("checkd:ping is not available on this system".to_string())
    }
}

/// Echo requests, as an ordinary user: Linux gives a process whose group
/// is within `net.ipv4.ping_group_range` a socket for them.
#[cfg(unix)]
mod icmp {
    use std::net::IpAddr;
    use std::time::{Duration, Instant};

    /// Sends `count` requests, each waited for up to `wait`. The answer is
    /// how many went out and the time of each reply, in milliseconds.
    pub fn echo(address: IpAddr, count: usize, wait: Duration) -> Result<(usize, Vec<f64>), String> {
        let (family, protocol, request, reply) = match address {
            IpAddr::V4(_) => (libc::AF_INET, libc::IPPROTO_ICMP, 8u8, 0u8),
            IpAddr::V6(_) => (libc::AF_INET6, libc::IPPROTO_ICMPV6, 128u8, 129u8),
        };
        // SAFETY: plain calls on a socket this function owns; every buffer
        // given is a local of the size said.
        unsafe {
            let fd = libc::socket(family, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, protocol);
            if fd < 0 {
                let err = std::io::Error::last_os_error();
                return Err(format!(
                    "no socket for echo requests ({err}): the user checkd runs as must be within net.ipv4.ping_group_range"
                ));
            }
            let closed = Closed(fd);
            let connected = match address {
                IpAddr::V4(v4) => {
                    let mut to: libc::sockaddr_in = std::mem::zeroed();
                    to.sin_family = libc::AF_INET as libc::sa_family_t;
                    to.sin_addr.s_addr = u32::from_ne_bytes(v4.octets());
                    libc::connect(fd, (&raw const to).cast(), size_of::<libc::sockaddr_in>() as libc::socklen_t)
                }
                IpAddr::V6(v6) => {
                    let mut to: libc::sockaddr_in6 = std::mem::zeroed();
                    to.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                    to.sin6_addr.s6_addr = v6.octets();
                    libc::connect(fd, (&raw const to).cast(), size_of::<libc::sockaddr_in6>() as libc::socklen_t)
                }
            };
            if connected < 0 {
                return Err(format!("{address}: {}", std::io::Error::last_os_error()));
            }
            let mut times = Vec::new();
            let mut sent = 0;
            for sequence in 0..count as u16 {
                // Type, code, checksum and identifier (both the kernel's
                // to fill in), sequence number, and something to carry.
                let mut packet = [0u8; 24];
                packet[0] = request;
                packet[6..8].copy_from_slice(&sequence.to_be_bytes());
                packet[8..].copy_from_slice(b"checkd echo. 123");
                let started = Instant::now();
                if libc::send(fd, packet.as_ptr().cast(), packet.len(), 0) < 0 {
                    // Nothing on the way there: counted as lost.
                    sent += 1;
                    continue;
                }
                sent += 1;
                loop {
                    let left = wait.saturating_sub(started.elapsed());
                    if left.is_zero() {
                        break;
                    }
                    let mut poll = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                    if libc::poll(&mut poll, 1, left.as_millis().max(1) as i32) <= 0 {
                        break;
                    }
                    let mut answer = [0u8; 256];
                    let n = libc::recv(fd, answer.as_mut_ptr().cast(), answer.len(), 0);
                    if n < 0 {
                        // "Unreachable", said by something on the way.
                        break;
                    }
                    if n >= 8 && answer[0] == reply && answer[6..8] == sequence.to_be_bytes() {
                        times.push(started.elapsed().as_secs_f64() * 1000.0);
                        break;
                    }
                }
            }
            drop(closed);
            Ok((sent, times))
        }
    }

    struct Closed(i32);
    impl Drop for Closed {
        fn drop(&mut self) {
            // SAFETY: the descriptor is this value's own.
            unsafe {
                libc::close(self.0);
            }
        }
    }
}

/// As much of reading a certificate as its last day takes.
mod der {
    /// One value: its tag, what is in it, and what comes after it.
    fn value(data: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let (&tag, rest) = data.split_first()?;
        let (&first, rest) = rest.split_first()?;
        let (length, rest) = if first < 0x80 {
            (first as usize, rest)
        } else {
            let bytes = (first & 0x7f) as usize;
            if bytes == 0 || bytes > 4 || rest.len() < bytes {
                return None;
            }
            (rest[..bytes].iter().fold(0usize, |n, b| (n << 8) | *b as usize), &rest[bytes..])
        };
        (rest.len() >= length).then(|| (tag, &rest[..length], &rest[length..]))
    }

    /// When a certificate ends, in seconds since 1970.
    pub fn not_after(certificate: &[u8]) -> Option<i64> {
        let (_, certificate, _) = value(certificate)?;
        let (_, mut fields, _) = value(certificate)?;
        // The version, when it is there; then the serial number, the
        // signature's kind and the issuer, before the two times.
        if fields.first() == Some(&0xa0) {
            fields = value(fields)?.2;
        }
        for _ in 0..3 {
            fields = value(fields)?.2;
        }
        let (_, validity, _) = value(fields)?;
        let (_, _, after) = value(validity)?;
        let (tag, text, _) = value(after)?;
        let text = std::str::from_utf8(text).ok()?;
        let digits = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
        // UTCTime has two digits for the year, GeneralizedTime four.
        let (year, at) = match tag {
            0x17 => {
                let short = digits(0, 2)?;
                (if short >= 50 { 1900 + short } else { 2000 + short }, 2)
            }
            0x18 => (digits(0, 4)?, 4),
            _ => return None,
        };
        let (month, day) = (digits(at, at + 2)?, digits(at + 2, at + 4)?);
        let (hour, minute, second) = (digits(at + 4, at + 6)?, digits(at + 6, at + 8)?, digits(at + 8, at + 10)?);
        Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
    }

    /// Days since 1970-01-01 of a date of the Gregorian calendar.
    fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
        let year = if month <= 2 { year - 1 } else { year };
        let era = year.div_euclid(400);
        let of_era = year - era * 400;
        let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
        era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468
    }

    /// The day a time falls on, as `2027-03-01`.
    pub fn day(seconds: i64) -> String {
        let days = seconds.div_euclid(86_400) + 719_468;
        let era = days.div_euclid(146_097);
        let of_era = days - era * 146_097;
        let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
        let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted = (5 * of_year + 2) / 153;
        let day = of_year - (153 * shifted + 2) / 5 + 1;
        let month = if shifted < 10 { shifted + 3 } else { shifted - 9 };
        let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
        format!("{year:04}-{month:02}-{day:02}")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn dates_there_and_back() {
            assert_eq!(days_from_civil(1970, 1, 1), 0);
            assert_eq!(days_from_civil(2000, 3, 1), 11_017);
            for seconds in [0, 951_868_800, 1_709_164_800, 1_800_000_000, 4_102_444_800] {
                let text = day(seconds);
                let part = |from: usize, to: usize| text[from..to].parse::<i64>().unwrap();
                assert_eq!(days_from_civil(part(0, 4), part(5, 7), part(8, 10)), seconds.div_euclid(86_400), "{text}");
            }
            assert_eq!(day(1_709_164_800), "2024-02-29");
        }

        #[test]
        fn the_end_of_a_certificate() {
            // A certificate cut down to what is read: version, serial,
            // kind of signature, issuer, and the two times.
            let mut tbs = vec![0xa0, 0x03, 0x02, 0x01, 0x02, 0x02, 0x01, 0x05, 0x30, 0x00, 0x30, 0x00];
            let mut validity = vec![0x17, 13];
            validity.extend_from_slice(b"260101000000Z");
            validity.extend_from_slice(&[0x18, 15]);
            validity.extend_from_slice(b"20570301120000Z");
            tbs.extend_from_slice(&[0x30, validity.len() as u8]);
            tbs.extend_from_slice(&validity);
            let mut inner = vec![0x30, tbs.len() as u8];
            inner.extend_from_slice(&tbs);
            let mut certificate = vec![0x30, 0x81, inner.len() as u8];
            certificate.extend_from_slice(&inner);
            let ends = not_after(&certificate).unwrap();
            assert_eq!(day(ends), "2057-03-01");
            assert_eq!(ends.rem_euclid(86_400), 12 * 3600);
            assert_eq!(not_after(&certificate[..20]), None);
        }
    }
}

/// TLS for the checks: a connection is made whoever signed the server's
/// certificate, and what the usual authorities say of it is kept beside
/// it, so that a check can say how long a certificate lasts that it also
/// says is not trusted.
#[cfg(feature = "tls-checks")]
mod tls {
    use std::sync::{Arc, Mutex};

    use rustls::client::WebPkiServerVerifier;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};
    use tokio::net::TcpStream;
    use tokio_rustls::TlsConnector;
    use tokio_rustls::client::TlsStream;

    #[derive(Debug)]
    struct Looking {
        usual: Arc<WebPkiServerVerifier>,
        /// The server's certificate, and why it is not trusted if it is not.
        seen: Mutex<Option<(Vec<u8>, Option<String>)>>,
    }

    impl ServerCertVerifier for Looking {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            intermediates: &[CertificateDer<'_>],
            server_name: &ServerName<'_>,
            ocsp_response: &[u8],
            now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            let distrust = self
                .usual
                .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
                .err()
                .map(|err| err.to_string());
            *self.seen.lock().unwrap() = Some((end_entity.to_vec(), distrust));
            // The check reads the verdict; nothing is sent that trusts it.
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            self.usual.verify_tls12_signature(message, cert, dss)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            self.usual.verify_tls13_signature(message, cert, dss)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.usual.supported_verify_schemes()
        }
    }

    /// The connection, the server's certificate, and why the usual
    /// authorities do not vouch for it, when they do not.
    pub async fn connect(plain: TcpStream, host: &str) -> Result<(TlsStream<TcpStream>, Vec<u8>, Option<String>), String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let usual = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(|err| err.to_string())?;
        let looking = Arc::new(Looking { usual, seen: Mutex::new(None) });
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|err| err.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(looking.clone())
            .with_no_client_auth();
        let name = ServerName::try_from(host.to_string()).map_err(|_| format!("'{host}' is not a name a certificate can be for"))?;
        let stream = TlsConnector::from(Arc::new(config))
            .connect(name, plain)
            .await
            .map_err(|err| format!("{host}: no TLS connection: {err}"))?;
        let (certificate, distrust) = looking.seen.lock().unwrap().take().ok_or("the server showed no certificate")?;
        Ok((stream, certificate, distrust))
    }
}

#[cfg(not(feature = "tls-checks"))]
mod tls {
    use tokio::net::TcpStream;

    pub async fn connect(_: TcpStream, _: &str) -> Result<(TcpStream, Vec<u8>, Option<String>), String> {
        Err("this checkd was built without TLS for its own checks (the feature tls-checks)".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn options() {
        let options = Options::read(&words("-H web -p 8080 -S -w 0.5")).unwrap();
        assert_eq!((options.host().unwrap(), options.port(None).unwrap(), options.has("-S")), ("web", 8080, true));
        assert_eq!(options.number("-w").unwrap(), Some(0.5));
        assert!(Options::read(&words("-H")).is_err());
        assert!(Options::read(&words("web")).is_err());
        assert!(Options::read(&words("-p 80")).unwrap().host().is_err());
        assert!(Options::read(&words("-H a -p many")).unwrap().port(None).is_err());
    }

    #[test]
    fn thresholds() {
        assert_eq!(over(1.0, Some(2.0), Some(3.0)), OK);
        assert_eq!(over(2.5, Some(2.0), Some(3.0)), WARNING);
        assert_eq!(over(3.5, Some(2.0), Some(3.0)), CRITICAL);
        assert_eq!(over(99.0, None, None), OK);
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
    }

    /// A web server for one request, answering with `status`.
    async fn serve(status: &'static str) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await;
            let _ = stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 5\r\n\r\nhello").as_bytes()).await;
        });
        port
    }

    #[test]
    fn a_port_that_is_open_and_one_that_is_not() {
        runtime().block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let open = run(&format!("checkd:tcp -H 127.0.0.1 -p {port}"), 5000).await;
            assert!(matches!(open.outcome, Outcome::Exited(0)), "{open:?}");
            assert!(open.stdout.starts_with("TCP OK - ") && open.stdout.contains("|time="), "{}", open.stdout);
            drop(listener);
            let closed = run(&format!("checkd:tcp -H 127.0.0.1 -p {port}"), 5000).await;
            assert!(matches!(closed.outcome, Outcome::Exited(2)), "{closed:?}");
            assert!(closed.stdout.starts_with("TCP CRITICAL - "));
        });
    }

    #[test]
    fn a_web_server_by_what_it_answers() {
        runtime().block_on(async {
            let state = async |status: &'static str, more: &str| {
                let port = serve(status).await;
                let result = run(&format!("checkd:http -H 127.0.0.1 -p {port} -u /x {more}"), 5000).await;
                match result.outcome {
                    Outcome::Exited(code) => (code, result.stdout),
                    other => panic!("{other:?}"),
                }
            };
            assert_eq!(state("200 OK", "").await.0, 0);
            assert_eq!(state("301 Moved", "").await.0, 0);
            assert_eq!(state("404 Not Found", "").await.0, 1);
            assert_eq!(state("503 Busy", "").await.0, 2);
            assert_eq!(state("404 Not Found", "-e 404,410").await.0, 0);
            assert_eq!(state("200 OK", "-e 204").await.0, 2);
            assert_eq!(state("200 OK", "-s hello").await.0, 0);
            let (code, text) = state("200 OK", "-s goodbye").await;
            assert_eq!(code, 2);
            assert!(text.contains("'goodbye' is not on the page"), "{text}");
            assert!(state("200 OK", "").await.1.starts_with("HTTP OK - HTTP/1.1 200 OK - "));
        });
    }

    /// A server for one request, which gives back what it was sent.
    async fn listen() -> (u16, tokio::sync::oneshot::Receiver<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sent, got) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 4096];
            let n = stream.read(&mut request).await.unwrap_or(0);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
            let _ = sent.send(String::from_utf8_lossy(&request[..n]).into_owned());
        });
        (port, got)
    }

    #[test]
    fn telling_someone() {
        runtime().block_on(async {
            // Plugin output with what a shell would act on is a word like any other.
            let (port, got) = listen().await;
            let line = format!("checkd:notify -u http://127.0.0.1:{port}/hook -t \"web1 is DOWN\" -m 'no route; `rm -rf` | $(x) \"q\"'");
            let result = run(&line, 5000).await;
            assert!(matches!(result.outcome, Outcome::Exited(0)), "{result:?}");
            let request = got.await.unwrap();
            assert!(request.starts_with("POST /hook HTTP/1.1\r\n"), "{request}");
            assert!(request.ends_with(r#"{"title":"web1 is DOWN","message":"no route; `rm -rf` | $(x) \"q\""}"#), "{request}");

            let (port, got) = listen().await;
            let result = run(&format!("checkd:notify -u http://127.0.0.1:{port}/alerts -f ntfy -p 5 -t T -m M"), 5000).await;
            assert!(matches!(result.outcome, Outcome::Exited(0)), "{result:?}");
            let request = got.await.unwrap();
            assert!(request.starts_with("POST / HTTP/1.1\r\n"), "{request}");
            assert!(request.ends_with(r#"{"topic":"alerts","title":"T","message":"M","priority":5}"#), "{request}");

            let (port, got) = listen().await;
            run(&format!("checkd:notify -u http://127.0.0.1:{port}/services/x -f slack -t T -m M"), 5000).await;
            assert!(got.await.unwrap().ends_with(r#"{"text":"*T*\nM"}"#));

            for line in ["checkd:notify -t T", "checkd:notify -u ftp://x/y", "checkd:notify -u http://127.0.0.1:1/ -f carrier-pigeon", "checkd:notify -u http://127.0.0.1:1/ -f ntfy"] {
                let result = run(line, 5000).await;
                assert!(matches!(result.outcome, Outcome::Exited(3)), "{line}: {result:?}");
            }
            // Nobody there: said, as a failure.
            let result = run("checkd:notify -u http://127.0.0.1:1/x -t T", 5000).await;
            assert!(matches!(result.outcome, Outcome::Exited(2)), "{result:?}");
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn checks_of_this_machine() {
        runtime().block_on(async {
            let disk = run("checkd:disk -p / -w 101 -c 102", 5000).await;
            assert!(matches!(disk.outcome, Outcome::Exited(0)), "{disk:?}");
            assert!(disk.stdout.starts_with("DISK OK - / ") && disk.stdout.contains("|used="), "{disk:?}");
            assert!(matches!(run("checkd:disk -p / -w -1", 5000).await.outcome, Outcome::Exited(1)));
            assert!(matches!(run("checkd:disk -p /no/such/place", 5000).await.outcome, Outcome::Exited(3)));
            assert!(matches!(run("checkd:disk", 5000).await.outcome, Outcome::Exited(3)));
            let memory = run("checkd:memory -w 101", 5000).await;
            assert!(memory.stdout.starts_with("MEMORY OK - ") && memory.stdout.contains("|used="), "{memory:?}");
            let load = run("checkd:load -w 1000,1000,1000 -c 2000", 5000).await;
            assert!(load.stdout.contains("load1=") && load.stdout.contains(";2000;0 "), "{load:?}");
            assert!(matches!(run("checkd:load -w 1,2", 5000).await.outcome, Outcome::Exited(3)));
            let cpu = run("checkd:cpu", 5000).await;
            assert!(cpu.stdout.starts_with("CPU OK - ") && cpu.stdout.contains("|busy="), "{cpu:?}");
            assert!(matches!(run("checkd:service -s --now", 5000).await.outcome, Outcome::Exited(3)));
        });
    }

    #[test]
    fn a_fixed_answer() {
        runtime().block_on(async {
            let result = run("checkd:dummy 2 'nothing came in time'", 5000).await;
            assert!(matches!(result.outcome, Outcome::Exited(2)), "{result:?}");
            assert_eq!(result.stdout, "nothing came in time\n");
            assert!(matches!(run("checkd:dummy 7 x", 5000).await.outcome, Outcome::Exited(3)));
        });
    }

    #[test]
    fn what_is_not_a_check() {
        runtime().block_on(async {
            for line in ["checkd:dns -H a", "checkd:tcp -H a", "checkd:tcp -H a -p 1 | cat", "checkd:http web"] {
                let result = run(line, 5000).await;
                assert!(matches!(result.outcome, Outcome::Exited(3)), "{line}: {result:?}");
            }
            assert!(is_builtin(" checkd:tcp -H a -p 1") && !is_builtin("/usr/lib/plugins/check_tcp"));
        });
    }

    #[test]
    fn a_check_that_takes_too_long() {
        runtime().block_on(async {
            // A listener that never answers the request.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let result = run(&format!("checkd:http -H 127.0.0.1 -p {port}"), 200).await;
            assert!(matches!(result.outcome, Outcome::TimedOut), "{result:?}");
        });
    }
}
