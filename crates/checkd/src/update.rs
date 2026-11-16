//! An agent that replaces itself (`--update-from`): every few minutes it
//! asks nuthatch, with its token, whether it is to be replaced, and if so
//! fetches the binary nuthatch names, checks it against the SHA-256 given
//! with it, runs it once (`--version`), puts it in place of its own and
//! ends, for its service manager to start the new one.
//!
//! It runs as the agent's own user, so the binary must be in a directory
//! that user may write: no root is needed after the install, and an agent
//! taken over can change no more than itself.
//!
//! nuthatch is reached over HTTPS, its certificate checked by the SHA-256
//! of its public key (`--update-pin`, as curl's `--pinnedpubkey`) or, with
//! no pin, by the usual authorities; plain HTTP only to this machine.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// How often it asks.
pub const EVERY: Duration = Duration::from_secs(5 * 60);
/// The most an answer, and a binary, may be.
const MOST_ANSWER: usize = 64 << 10;
const MOST_BINARY: usize = 128 << 20;

/// `--update-from` and `--update-pin`.
#[derive(Clone, Debug)]
pub struct Update {
    pub from: Url,
    /// SHA-256 of nuthatch's public key (its SubjectPublicKeyInfo).
    pub pin: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Url {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    pub fn read(text: &str) -> Result<Url, String> {
        let bad = || format!("--update-from: '{text}' is not an http(s)://host[:port]/path address");
        let (secure, rest) = match (text.strip_prefix("https://"), text.strip_prefix("http://")) {
            (Some(rest), _) => (true, rest),
            (_, Some(rest)) => (false, rest),
            _ => return Err(bad()),
        };
        let at = rest.find('/').ok_or_else(bad)?;
        let (authority, path) = (&rest[..at], &rest[at..]);
        if path.contains(['?', '#']) || path.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(bad());
        }
        let (host, port) = match authority.strip_prefix('[') {
            Some(v6) => {
                let (host, after) = v6.split_once(']').ok_or_else(bad)?;
                (host.to_string(), after.strip_prefix(':'))
            }
            None => match authority.rsplit_once(':') {
                Some((host, port)) => (host.to_string(), Some(port)),
                None => (authority.to_string(), None),
            },
        };
        let port = match port {
            Some(port) => port.parse().map_err(|_| bad())?,
            None => if secure { 443 } else { 80 },
        };
        if host.is_empty() || host.contains('@') {
            return Err(bad());
        }
        if !secure && !["127.0.0.1", "localhost", "::1"].contains(&host.as_str()) {
            return Err(format!("--update-from: plain http only to this machine, not {host}"));
        }
        Ok(Url { secure, host, port, path: path.to_string() })
    }

    /// The same server, another path.
    fn at(&self, path: &str) -> Url {
        Url { path: path.to_string(), ..self.clone() }
    }
}

pub fn read_pin(text: &str) -> Result<[u8; 32], String> {
    let bad = || format!("--update-pin takes the SHA-256 of nuthatch's public key in hex, not '{text}'");
    if text.len() != 64 {
        return Err(bad());
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
    }
    Ok(out)
}

/// The binary nuthatch serves for this machine.
fn wanted_file() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("checkd-agent-linux-x86_64"),
        ("linux", "aarch64") => Some("checkd-agent-linux-aarch64"),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

/// Asks every `EVERY`, the first time soon after the start; returns when
/// the binary was replaced, for the agent to end. A failure is said and
/// tried again at the next asking.
pub async fn watch(update: Update, token: String, say: fn(&str)) {
    let Ok(exe) = std::env::current_exe() else {
        say("Updates: cannot tell where this program is");
        return std::future::pending().await;
    };
    let Some(file) = wanted_file() else {
        say("Updates: none for this system");
        return std::future::pending().await;
    };
    // Spread out, so that hosts started together do not ask together.
    let spread = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_millis() as u64).unwrap_or(0);
    tokio::time::sleep(Duration::from_secs(30) + Duration::from_millis(spread * 60)).await;
    loop {
        match once(&update, &token, &exe, file).await {
            Ok(Some(sum)) => {
                say(&format!("Updated: {} is now {sum}; ending, to be started anew", exe.display()));
                return;
            }
            Ok(None) => {}
            Err(why) => say(&format!("Updates: {why}")),
        }
        tokio::time::sleep(EVERY).await;
    }
}

/// One asking: the new binary's SHA-256 when it was put in place.
async fn once(update: &Update, token: &str, exe: &Path, file: &str) -> Result<Option<String>, String> {
    let have = sha256(&std::fs::read(exe).map_err(|err| format!("cannot read {}: {err}", exe.display()))?);
    let arch = std::env::consts::ARCH;
    let ask = update.from.at(&format!("{}?arch={arch}&have={have}", update.from.path));
    let answer = get(&ask, update.pin, Some(token), MOST_ANSWER).await?;
    let answer = String::from_utf8_lossy(&answer);
    let words: Vec<&str> = answer.split_whitespace().collect();
    let (named, sum) = match words.as_slice() {
        ["none"] => return Ok(None),
        ["update", named, sum] => (*named, *sum),
        _ => return Err("nuthatch's answer is not understood".into()),
    };
    if named != file || sum.len() != 64 || !sum.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err(format!("nuthatch names {named}, not {file}"));
    }
    // The binaries are beside the updates: .../agent/bin/<file>.
    let base = update.from.path.find("/agent/update/").map(|at| &update.from.path[..at]).unwrap_or("");
    let binary = get(&update.from.at(&format!("{base}/agent/bin/{file}")), update.pin, None, MOST_BINARY).await?;
    if sha256(&binary) != sum {
        return Err("the download does not match its checksum".into());
    }
    replace(exe, &binary).await?;
    Ok(Some(sum.to_string()))
}

/// Puts the new binary in place of the running one, once it has run.
async fn replace(exe: &Path, binary: &[u8]) -> Result<(), String> {
    let fresh = sibling(exe, ".new");
    std::fs::write(&fresh, binary).map_err(|err| format!("cannot write {}: {err}", fresh.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755)).map_err(|err| err.to_string())?;
    }
    let tried = tokio::time::timeout(Duration::from_secs(15), tokio::process::Command::new(&fresh).arg("--version").kill_on_drop(true).output()).await;
    let works = matches!(&tried, Ok(Ok(out)) if out.status.success() && out.stdout.starts_with(b"checkd-agent "));
    if !works {
        let _ = std::fs::remove_file(&fresh);
        return Err("the new binary does not run here".into());
    }
    std::fs::rename(&fresh, exe).map_err(|err| format!("cannot put it in place of {}: {err}", exe.display()))
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// One GET, its body when the answer is 200.
async fn get(url: &Url, pin: Option<[u8; 32]>, token: Option<&str>, most: usize) -> Result<Vec<u8>, String> {
    let work = async {
        let plain = TcpStream::connect((url.host.as_str(), url.port)).await.map_err(|err| format!("{}:{}: {err}", url.host, url.port))?;
        let mut stream: Box<dyn Stream> = if url.secure {
            let name = ServerName::try_from(url.host.clone()).map_err(|err| format!("'{}': {err}", url.host))?;
            Box::new(connector(pin)?.connect(name, plain).await.map_err(|err| format!("{}: {err}", url.host))?)
        } else {
            Box::new(plain)
        };
        let host = if url.host.contains(':') { format!("[{}]", url.host) } else { url.host.clone() };
        let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        let request = format!("GET {} HTTP/1.1\r\nHost: {host}:{}\r\nUser-Agent: checkd-agent\r\n{auth}Connection: close\r\n\r\n", url.path, url.port);
        stream.write_all(request.as_bytes()).await.map_err(|err| err.to_string())?;
        let mut all = Vec::new();
        let mut buffer = vec![0u8; 64 << 10];
        loop {
            match stream.read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => all.extend_from_slice(&buffer[..n]),
                // A server may end a TLS connection without saying so.
                Err(_) if !all.is_empty() => break,
                Err(err) => return Err(err.to_string()),
            }
            if all.len() > most + MOST_ANSWER {
                return Err("the answer is too long".into());
            }
        }
        body(&all, most)
    };
    tokio::time::timeout(Duration::from_secs(300), work).await.map_err(|_| "no answer in time".to_string())?
}

/// The body of a whole HTTP/1.1 answer, when its status is 200.
fn body(all: &[u8], most: usize) -> Result<Vec<u8>, String> {
    let end = all.windows(4).position(|w| w == b"\r\n\r\n").ok_or("the answer has no end of its head")?;
    let head = String::from_utf8_lossy(&all[..end]);
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or("");
    let code = status.split_whitespace().nth(1).unwrap_or("");
    if code != "200" {
        return Err(format!("nuthatch answered {}", if code.is_empty() { "something else" } else { code }));
    }
    let header = |name: &str| {
        head.split("\r\n").skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim().to_string())
        })
    };
    let rest = &all[end + 4..];
    let out = if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        unchunk(rest)?
    } else if let Some(length) = header("content-length") {
        let length: usize = length.parse().map_err(|_| "a wrong Content-Length")?;
        rest.get(..length).ok_or("the answer was cut short")?.to_vec()
    } else {
        rest.to_vec()
    };
    if out.len() > most {
        return Err("the answer is too long".into());
    }
    Ok(out)
}

fn unchunk(mut rest: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line = rest.windows(2).position(|w| w == b"\r\n").ok_or("a chunk was cut short")?;
        let size_text = String::from_utf8_lossy(&rest[..line]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "a wrong chunk size")?;
        rest = &rest[line + 2..];
        if size == 0 {
            return Ok(out);
        }
        out.extend_from_slice(rest.get(..size).ok_or("a chunk was cut short")?);
        rest = rest.get(size + 2..).ok_or("a chunk was cut short")?;
    }
}

fn connector(pin: Option<[u8; 32]>) -> Result<tokio_rustls::TlsConnector, String> {
    let Some(pin) = pin else { return crate::tls::public_connector() };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|err| err.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { pin, provider }))
        .with_no_client_auth();
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

/// A certificate taken by the SHA-256 of its public key, whatever signed
/// it; the handshake's signatures are still checked, so the server must
/// hold the key.
#[derive(Debug)]
struct Pinned {
    pin: [u8; 32],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, end: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        let spki = public_key_info(end.as_ref()).ok_or(rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
        let found = ring::digest::digest(&ring::digest::SHA256, spki);
        if found.as_ref() == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure))
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// One DER element at the start of `data`: its tag, the whole element,
/// and its contents.
fn element(data: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *data.first()?;
    let first = *data.get(1)?;
    let (length, start) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 {
            return None;
        }
        let mut length = 0usize;
        for i in 0..n {
            length = (length << 8) | *data.get(2 + i)? as usize;
        }
        (length, 2 + n)
    };
    let whole = data.get(..start.checked_add(length)?)?;
    Some((tag, whole, &whole[start..]))
}

/// The SubjectPublicKeyInfo of a certificate, whole, as curl hashes it.
fn public_key_info(cert: &[u8]) -> Option<&[u8]> {
    let (0x30, _, certificate) = element(cert)? else { return None };
    let (0x30, _, mut tbs) = element(certificate)? else { return None };
    // [0] version, if there; then serial, signature, issuer, validity,
    // subject; then the key.
    if tbs.first() == Some(&0xa0) {
        tbs = &tbs[element(tbs)?.1.len()..];
    }
    for _ in 0..5 {
        tbs = &tbs[element(tbs)?.1.len()..];
    }
    let (0x30, whole, _) = element(tbs)? else { return None };
    Some(whole)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_pins() {
        let url = Url::read("https://192.0.2.7:4731/agent/update/site/web1").unwrap();
        assert_eq!((url.secure, url.host.as_str(), url.port, url.path.as_str()), (true, "192.0.2.7", 4731, "/agent/update/site/web1"));
        assert_eq!(Url::read("https://[2001:db8::1]/agent/update/s/h").unwrap().host, "2001:db8::1");
        assert_eq!(Url::read("http://127.0.0.1:8092/agent/update/s/h").unwrap().port, 8092);
        assert!(Url::read("http://192.0.2.7/agent/update/s/h").unwrap_err().contains("only to this machine"));
        assert!(Url::read("ftp://x/y").is_err() && Url::read("https://x").is_err() && Url::read("https://x/y?z").is_err());
        assert_eq!(read_pin(&"ab".repeat(32)).unwrap(), [0xab; 32]);
        assert!(read_pin("ab").is_err() && read_pin(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn answers_are_read() {
        assert_eq!(body(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nnone\nmore", 100).unwrap(), b"none\n");
        assert_eq!(body(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nnone\r\n1\r\n\n\r\n0\r\n\r\n", 100).unwrap(), b"none\n");
        assert!(body(b"HTTP/1.1 404 Not Found\r\n\r\nnot found\n", 100).unwrap_err().contains("404"));
        assert!(body(b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nshort", 100).is_err());
        assert!(body(b"HTTP/1.1 200 OK\r\n\r\ntoo long", 3).is_err());
    }

    #[test]
    fn the_public_key_of_a_certificate() {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["nuthatch".into()]).unwrap().self_signed(&key).unwrap();
        use rcgen::PublicKeyData;
        assert_eq!(public_key_info(cert.der()).unwrap(), key.subject_public_key_info().as_slice());
        assert!(public_key_info(b"\x30\x03\x02\x01\x00").is_none());
    }

    #[tokio::test]
    async fn a_binary_that_does_not_run_is_not_put_in_place() {
        let dir = std::env::temp_dir().join(format!("checkd-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("checkd-agent");
        std::fs::write(&exe, b"the old one").unwrap();
        assert!(replace(&exe, b"#!/bin/sh\nexit 3\n").await.unwrap_err().contains("does not run"));
        assert_eq!(std::fs::read(&exe).unwrap(), b"the old one");
        assert!(!sibling(&exe, ".new").exists());
        replace(&exe, b"#!/bin/sh\necho checkd-agent 9.9.9\n").await.unwrap();
        assert!(std::fs::read(&exe).unwrap().starts_with(b"#!/bin/sh"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
