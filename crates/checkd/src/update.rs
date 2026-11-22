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
//!
//! With `--update-signer` it takes only a binary whose SHA-256 is signed by
//! the key of that certificate, and none signed before the one it has: so
//! whoever can change the binaries nuthatch serves cannot change the agent.
//!
//! A new binary is on trial (`<name>.trial`) until it connects to an
//! engine. One that has not within ten minutes, or was started three times
//! without, goes back to the binary before it (`<name>.old`), and that
//! binary is refused for a day (`<name>.refused`).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// How long a new binary has to connect, how often it may be started
/// without, and how long the one that did not is refused.
const TRIAL: u64 = 10 * 60;
const TRIAL_STARTS: u32 = 3;
const REFUSED_FOR: u64 = 24 * 3600;

/// Set once the engine has welcomed this agent (worker.rs).
pub static CONNECTED: AtomicBool = AtomicBool::new(false);

/// `--update-from`, `--update-pin` and `--update-signer`.
#[derive(Clone, Debug)]
pub struct Update {
    pub from: Url,
    /// SHA-256 of nuthatch's public key (its SubjectPublicKeyInfo).
    pub pin: Option<[u8; 32]>,
    pub signer: Option<Signer>,
}

/// The key that signs the binaries, from a certificate.
#[derive(Clone, Debug)]
pub struct Signer {
    /// SHA-256 of its SubjectPublicKeyInfo, as nuthatch knows it.
    pub id: [u8; 32],
    algorithm: &'static dyn ring::signature::VerificationAlgorithm,
    key: Vec<u8>,
}

impl Signer {
    /// Whether `signature` signs this binary: its file name, SHA-256 and
    /// the time it was signed.
    fn signs(&self, file: &str, sum: &str, time: u64, signature: &[u8]) -> bool {
        ring::signature::UnparsedPublicKey::new(self.algorithm, &self.key).verify(statement(file, sum, time).as_bytes(), signature).is_ok()
    }
}

/// What is signed for a binary (nuthatch's signing.ts makes the same).
pub fn statement(file: &str, sum: &str, time: u64) -> String {
    format!("checkd-agent update\n{file}\n{sum}\n{time}\n")
}

/// `--update-signer`: a PEM certificate with an Ed25519, ECDSA P-256 or
/// P-384, or RSA key. Only its key counts, not who issued it or until when.
pub fn read_signer(path: &Path) -> Result<Signer, String> {
    use rustls::pki_types::pem::PemObject;
    let cert = CertificateDer::from_pem_file(path).map_err(|err| format!("--update-signer: cannot read {}: {err}", path.display()))?;
    signer_of(cert.as_ref()).ok_or_else(|| format!("--update-signer: {} holds no Ed25519, ECDSA P-256 or P-384, or RSA key", path.display()))
}

fn signer_of(cert: &[u8]) -> Option<Signer> {
    use ring::signature as s;
    const ED25519: &[u8] = &[0x06, 0x03, 0x2b, 0x65, 0x70];
    const EC: &[u8] = &[0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
    const P256: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
    const P384: &[u8] = &[0x06, 0x05, 0x2b, 0x81, 0x04, 0x00, 0x22];
    const RSA: &[u8] = &[0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
    let spki = public_key_info(cert)?;
    let (_, _, inner) = element(spki)?;
    let (0x30, whole, algorithm) = element(inner)? else { return None };
    let (0x03, _, bits) = element(&inner[whole.len()..])? else { return None };
    let (&0, key) = bits.split_first()? else { return None };
    let algorithm: &'static dyn s::VerificationAlgorithm = if algorithm == ED25519 {
        &s::ED25519
    } else if let Some(curve) = algorithm.strip_prefix(EC) {
        if curve == P256 {
            &s::ECDSA_P256_SHA256_ASN1
        } else if curve == P384 {
            &s::ECDSA_P384_SHA384_ASN1
        } else {
            return None;
        }
    } else if algorithm.starts_with(RSA) {
        &s::RSA_PKCS1_2048_8192_SHA256
    } else {
        return None;
    };
    let mut id = [0u8; 32];
    id.copy_from_slice(ring::digest::digest(&ring::digest::SHA256, spki).as_ref());
    Some(Signer { id, algorithm, key: key.to_vec() })
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
    unhex(text).and_then(|bytes| <[u8; 32]>::try_from(bytes).ok()).ok_or_else(bad)
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok()).collect()
}

/// The binary nuthatch serves for this machine.
fn wanted_file() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("checkd-agent-linux-x86_64"),
        ("linux", "aarch64") => Some("checkd-agent-linux-aarch64"),
        ("windows", "x86_64") => Some("checkd-agent-windows-x86_64.exe"),
        _ => None,
    }
}

/// A file beside the binary: `checkd-agent.<tag>`, and for one that is run,
/// with the binary's extension (`checkd-agent.new.exe`), which Windows wants.
fn beside(exe: &Path, tag: &str, runs: bool) -> PathBuf {
    let stem = exe.file_stem().unwrap_or_default().to_string_lossy();
    let extension = match exe.extension() {
        Some(e) if runs => format!(".{}", e.to_string_lossy()),
        _ => String::new(),
    };
    exe.with_file_name(format!("{stem}.{tag}{extension}"))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The words of a small file beside the binary.
fn words(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).map(|text| text.split_whitespace().map(str::to_string).collect()).unwrap_or_default()
}

/// The binary refused after a trial, while it still is.
fn refused(exe: &Path) -> Option<String> {
    match words(&beside(exe, "refused", false)).as_slice() {
        [sum, at] if at.parse::<u64>().is_ok_and(|at| now() < at + REFUSED_FOR) => Some(sum.clone()),
        _ => None,
    }
}

/// How a start goes with a binary on trial.
#[derive(Debug, PartialEq)]
enum Trial {
    /// None on trial.
    None,
    /// On trial until then (a time in seconds).
    Until(u64),
    /// It went back to the binary before: to be started anew.
    WentBack,
}

/// Counts this start of a binary on trial, and goes back when it is over.
fn start_trial(exe: &Path) -> Result<Trial, String> {
    let file = beside(exe, "trial", false);
    let found = words(&file);
    let [began, starts, sum] = found.as_slice() else {
        if file.exists() {
            let _ = std::fs::remove_file(&file);
        }
        return Ok(Trial::None);
    };
    let (began, starts) = (began.parse::<u64>().unwrap_or(0), starts.parse::<u32>().unwrap_or(TRIAL_STARTS) + 1);
    if starts > TRIAL_STARTS || now() >= began + TRIAL {
        go_back(exe, sum)?;
        return Ok(Trial::WentBack);
    }
    std::fs::write(&file, format!("{began} {starts} {sum}\n")).map_err(|err| format!("cannot write {}: {err}", file.display()))?;
    Ok(Trial::Until(began + TRIAL))
}

/// The binary on trial connected: it stays.
fn end_trial(exe: &Path) {
    let _ = std::fs::remove_file(beside(exe, "trial", false));
    let _ = std::fs::remove_file(beside(exe, "old", true));
}

/// Puts the binary before back in place of the one on trial, `sum`, and
/// refuses that one for a while.
fn go_back(exe: &Path, sum: &str) -> Result<(), String> {
    let old = beside(exe, "old", true);
    let trial = beside(exe, "trial", false);
    if !old.exists() {
        let _ = std::fs::remove_file(&trial);
        return Err(format!("no {} to go back to", old.display()));
    }
    // Windows renames a running binary but does not replace it.
    #[cfg(windows)]
    {
        let bad = beside(exe, "bad", true);
        let _ = std::fs::remove_file(&bad);
        std::fs::rename(exe, &bad).map_err(|err| format!("cannot move {}: {err}", exe.display()))?;
        if let Err(err) = std::fs::rename(&old, exe) {
            let _ = std::fs::rename(&bad, exe);
            return Err(format!("cannot put {} back: {err}", old.display()));
        }
    }
    #[cfg(not(windows))]
    std::fs::rename(&old, exe).map_err(|err| format!("cannot put {} back: {err}", old.display()))?;
    let _ = std::fs::write(beside(exe, "refused", false), format!("{sum} {}\n", now()));
    let _ = std::fs::remove_file(&trial);
    Ok(())
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
    // Left by a going back on Windows, which cannot remove a running binary.
    let _ = std::fs::remove_file(beside(&exe, "bad", true));
    match start_trial(&exe) {
        Ok(Trial::None) => {}
        Ok(Trial::WentBack) => {
            say("Updates: this binary did not connect in time; went back to the one before, ending, to be started anew");
            return;
        }
        Ok(Trial::Until(until)) => loop {
            if CONNECTED.load(Ordering::Relaxed) {
                end_trial(&exe);
                say("Updates: this binary connected, and stays");
                break;
            }
            if now() >= until {
                let sum = words(&beside(&exe, "trial", false)).get(2).cloned().unwrap_or_default();
                match go_back(&exe, &sum) {
                    Ok(()) => {
                        say("Updates: this binary did not connect in time; went back to the one before, ending, to be started anew");
                        return;
                    }
                    Err(why) => {
                        say(&format!("Updates: {why}"));
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        },
        Err(why) => say(&format!("Updates: {why}")),
    }
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
    // Linux says nothing of its system, as agents did before Windows ones asked.
    let os = if std::env::consts::OS == "linux" { String::new() } else { format!("&os={}", std::env::consts::OS) };
    let signer = update.signer.as_ref().map(|s| format!("&signer={}", hex(&s.id))).unwrap_or_default();
    let refused = refused(exe);
    let refusing = refused.as_ref().map(|sum| format!("&refused={sum}")).unwrap_or_default();
    let ask = update.from.at(&format!("{}?arch={arch}&have={have}{os}{signer}{refusing}", update.from.path));
    let answer = get(&ask, update.pin, Some(token), MOST_ANSWER).await?;
    let answer = String::from_utf8_lossy(&answer);
    let parts: Vec<&str> = answer.split_whitespace().collect();
    let (named, sum, signed) = match parts.as_slice() {
        ["none"] => return Ok(None),
        ["update", named, sum] => (*named, *sum, None),
        ["update", named, sum, time, signature] => (*named, *sum, Some((*time, *signature))),
        _ => return Err("nuthatch's answer is not understood".into()),
    };
    if named != file || sum.len() != 64 || !sum.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err(format!("nuthatch names {named}, not {file}"));
    }
    if refused.as_deref() == Some(sum) {
        return Ok(None);
    }
    // Signed by the key it was given, and later than the binary it has.
    let mut signed_at = None;
    if let Some(signer) = &update.signer {
        let Some((time, signature)) = signed else {
            return Err("nuthatch's binary is not signed".into());
        };
        let (Ok(time), Some(signature)) = (time.parse::<u64>(), unhex(signature)) else {
            return Err("nuthatch's answer is not understood".into());
        };
        if !signer.signs(file, sum, time, &signature) {
            return Err("nuthatch's binary is not signed by the key this agent was given".into());
        }
        if words(&beside(exe, "signed", false)).first().and_then(|t| t.parse::<u64>().ok()).is_some_and(|last| time <= last) {
            return Err("nuthatch's binary was signed before the one this agent has".into());
        }
        signed_at = Some(time);
    }
    // The binaries are beside the updates: .../agent/bin/<file>.
    let base = update.from.path.find("/agent/update/").map(|at| &update.from.path[..at]).unwrap_or("");
    let binary = get(&update.from.at(&format!("{base}/agent/bin/{file}")), update.pin, None, MOST_BINARY).await?;
    if sha256(&binary) != sum {
        return Err("the download does not match its checksum".into());
    }
    replace(exe, &binary).await?;
    // On trial from its first start; and what it was signed at, for the next.
    let trial = beside(exe, "trial", false);
    std::fs::write(&trial, format!("{} 0 {sum}\n", now())).map_err(|err| format!("cannot write {}: {err}", trial.display()))?;
    if let Some(time) = signed_at {
        let _ = std::fs::write(beside(exe, "signed", false), format!("{time}\n"));
    }
    Ok(Some(sum.to_string()))
}

/// Puts the new binary in place of the running one, once it has run,
/// and keeps the running one beside it (`.old`) to go back to.
async fn replace(exe: &Path, binary: &[u8]) -> Result<(), String> {
    let fresh = beside(exe, "new", true);
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
    put_in_place(exe, &fresh)
}

/// Puts `fresh` in place of the binary, which may be running, keeping that
/// one beside it as `.old`.
fn put_in_place(exe: &Path, fresh: &Path) -> Result<(), String> {
    let old = beside(exe, "old", true);
    let _ = std::fs::remove_file(&old);
    let fail = |err: std::io::Error| {
        let _ = std::fs::remove_file(fresh);
        format!("cannot put it in place of {}: {err}", exe.display())
    };
    // Windows renames a running binary but does not replace it; elsewhere
    // the binary is replaced at once, so that there always is one.
    #[cfg(windows)]
    {
        std::fs::rename(exe, &old).map_err(fail)?;
        if let Err(err) = std::fs::rename(fresh, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(fail(err));
        }
    }
    #[cfg(not(windows))]
    {
        std::fs::hard_link(exe, &old).or_else(|_| std::fs::copy(exe, &old).map(|_| ())).map_err(fail)?;
        std::fs::rename(fresh, exe).map_err(fail)?;
    }
    Ok(())
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

    // Scripts stand in for binaries here; Windows runs none.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_binary_that_does_not_run_is_not_put_in_place() {
        let dir = std::env::temp_dir().join(format!("checkd-update-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("checkd-agent");
        std::fs::write(&exe, b"the old one").unwrap();
        assert!(replace(&exe, b"#!/bin/sh\nexit 3\n").await.unwrap_err().contains("does not run"));
        assert_eq!(std::fs::read(&exe).unwrap(), b"the old one");
        assert!(!beside(&exe, "new", true).exists());
        replace(&exe, b"#!/bin/sh\necho checkd-agent 9.9.9\n").await.unwrap();
        assert!(std::fs::read(&exe).unwrap().starts_with(b"#!/bin/sh"));
        assert_eq!(std::fs::read(beside(&exe, "old", true)).unwrap(), b"the old one");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_beside_the_binary() {
        assert_eq!(beside(Path::new("/b/checkd-agent"), "new", true), Path::new("/b/checkd-agent.new"));
        assert_eq!(beside(Path::new("/b/checkd-agent.exe"), "new", true), Path::new("/b/checkd-agent.new.exe"));
        assert_eq!(beside(Path::new("/b/checkd-agent.exe"), "trial", false), Path::new("/b/checkd-agent.trial"));
    }

    fn trial_dir(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("checkd-trial-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("checkd-agent");
        std::fs::write(&exe, b"new").unwrap();
        std::fs::write(beside(&exe, "old", true), b"old").unwrap();
        (dir, exe)
    }

    #[test]
    fn a_binary_on_trial_that_connects_stays() {
        let (dir, exe) = trial_dir("stays");
        assert_eq!(start_trial(&exe).unwrap(), Trial::None);
        std::fs::write(beside(&exe, "trial", false), format!("{} 0 {}\n", now(), "ab".repeat(32))).unwrap();
        assert!(matches!(start_trial(&exe).unwrap(), Trial::Until(until) if until >= now() + TRIAL - 2));
        assert_eq!(words(&beside(&exe, "trial", false))[1], "1");
        end_trial(&exe);
        assert!(!beside(&exe, "trial", false).exists() && !beside(&exe, "old", true).exists());
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_binary_on_trial_too_long_or_started_too_often_goes_back() {
        let sum = "cd".repeat(32);
        for (name, began, starts) in [("late", now() - TRIAL - 1, 0), ("often", now(), TRIAL_STARTS)] {
            let (dir, exe) = trial_dir(name);
            std::fs::write(beside(&exe, "trial", false), format!("{began} {starts} {sum}\n")).unwrap();
            assert_eq!(start_trial(&exe).unwrap(), Trial::WentBack);
            assert_eq!(std::fs::read(&exe).unwrap(), b"old");
            assert!(!beside(&exe, "trial", false).exists() && !beside(&exe, "old", true).exists());
            assert_eq!(refused(&exe), Some(sum.clone()));
            std::fs::write(beside(&exe, "refused", false), format!("{sum} {}\n", now() - REFUSED_FOR)).unwrap();
            assert_eq!(refused(&exe), None);
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn signatures_by_the_key_of_the_certificate() {
        use ring::signature::{EcdsaKeyPair, Ed25519KeyPair, KeyPair as _, ECDSA_P256_SHA256_ASN1_SIGNING};
        let rng = ring::rand::SystemRandom::new();
        let sum = "ef".repeat(32);
        let file = "checkd-agent-linux-x86_64";
        let message = statement(file, &sum, 1_800_000_000);
        assert_eq!(message, format!("checkd-agent update\n{file}\n{sum}\n1800000000\n"));

        let ed = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).unwrap();
        let cert = rcgen::CertificateParams::new(vec!["signer".into()]).unwrap().self_signed(&ed).unwrap();
        let signer = signer_of(cert.der()).unwrap();
        let ring_ed = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&ed.serialize_der()).unwrap();
        assert_eq!(signer.key, ring_ed.public_key().as_ref());
        let signature = ring_ed.sign(message.as_bytes());
        assert!(signer.signs(file, &sum, 1_800_000_000, signature.as_ref()));
        assert!(!signer.signs(file, &sum, 1_800_000_001, signature.as_ref()));
        assert!(!signer.signs("checkd-agent-linux-aarch64", &sum, 1_800_000_000, signature.as_ref()));
        use rcgen::PublicKeyData;
        assert_eq!(&signer.id[..], ring::digest::digest(&ring::digest::SHA256, &ed.subject_public_key_info()).as_ref());

        let ec = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = rcgen::CertificateParams::new(vec!["signer".into()]).unwrap().self_signed(&ec).unwrap();
        let signer = signer_of(cert.der()).unwrap();
        let ring_ec = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &ec.serialize_der(), &rng).unwrap();
        let signature = ring_ec.sign(&rng, message.as_bytes()).unwrap();
        assert!(signer.signs(file, &sum, 1_800_000_000, signature.as_ref()));
        // Another key's signature is not taken.
        assert!(!signer.signs(file, &sum, 1_800_000_000, ring_ed.sign(message.as_bytes()).as_ref()));

        let dir = std::env::temp_dir().join(format!("checkd-signer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("signer.crt"), cert.pem()).unwrap();
        assert_eq!(read_signer(&dir.join("signer.crt")).unwrap().id, signer.id);
        assert!(read_signer(&dir.join("none.crt")).unwrap_err().contains("cannot read"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn signatures_made_by_nuthatch() {
        // Beside this file, so that checkd-agent's build of it finds them too.
        let certs = [
            ("ed25519", include_str!("../testdata/signed/ed25519.pem")),
            ("p384", include_str!("../testdata/signed/p384.pem")),
            ("rsa", include_str!("../testdata/signed/rsa.pem")),
        ];
        let sigs = include_str!("../testdata/signed/sigs.txt");
        let dir = std::env::temp_dir().join(format!("checkd-signed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sum = "ab".repeat(32);
        let mut seen = 0;
        for line in sigs.lines() {
            let (name, signature) = line.split_once(' ').unwrap();
            let pem = certs.iter().find(|(n, _)| *n == name).unwrap().1;
            std::fs::write(dir.join(format!("{name}.pem")), pem).unwrap();
            let signer = read_signer(&dir.join(format!("{name}.pem"))).unwrap();
            let signature = unhex(signature).unwrap();
            assert!(signer.signs("checkd-agent-linux-x86_64", &sum, 1_800_000_000, &signature), "{name}");
            assert!(!signer.signs("checkd-agent-linux-x86_64", &sum, 1_800_000_001, &signature), "{name}");
            seen += 1;
        }
        assert_eq!(seen, 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // A running binary is put aside and back on Windows, where it cannot be
    // replaced while it runs.
    #[cfg(windows)]
    #[test]
    fn a_running_binary_is_replaced_and_put_back_on_windows() {
        let dir = std::env::temp_dir().join(format!("checkd-running-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("checkd-agent.exe");
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        std::fs::copy(Path::new(&system).join(r"System32\PING.EXE"), &exe).unwrap();
        let running = std::fs::read(&exe).unwrap();
        let mut child = std::process::Command::new(&exe).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
        let fresh = beside(&exe, "new", true);
        std::fs::write(&fresh, b"the new one").unwrap();
        put_in_place(&exe, &fresh).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"the new one");
        assert_eq!(std::fs::read(beside(&exe, "old", true)).unwrap(), running);
        assert!(!fresh.exists());
        // And back, as a binary on trial does, here the new one not running.
        std::fs::write(beside(&exe, "trial", false), format!("0 0 {}\n", "ab".repeat(32))).unwrap();
        assert_eq!(start_trial(&exe).unwrap(), Trial::WentBack);
        assert_eq!(std::fs::read(&exe).unwrap(), running);
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
