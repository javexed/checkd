//! WebSocket (RFC 6455) as a plain byte stream, for remote workers behind
//! an HTTPS proxy or tunnel: binary frames carry checkd's own TLS, so the
//! proxy sees neither tokens nor command lines. Only what that needs: no
//! extensions, no text frames.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// The largest frame taken; checkd's TLS records are far smaller.
const MAX_FRAME: u64 = 1 << 20;
/// The largest frame sent.
const SEND: usize = 16 * 1024;
/// The largest HTTP head read in a handshake.
const MAX_HEAD: usize = 16 * 1024;

pub struct WsStream<S> {
    inner: S,
    /// A client masks what it sends.
    client: bool,
    /// Bytes read from `inner`, not yet a whole frame.
    raw: Vec<u8>,
    /// A data frame's payload, for the reader, from `taken` on.
    data: Vec<u8>,
    taken: usize,
    /// The other side closed.
    closed: bool,
    /// Whole frames to send, from `sent` on.
    out: Vec<u8>,
    sent: usize,
}

fn bad(text: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, text.into())
}

fn random<const N: usize>() -> [u8; N] {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; N];
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    bytes
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                text.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// What the server answers to a client's key.
fn accept_of(key: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, format!("{key}{GUID}").as_bytes());
    base64(digest.as_ref())
}

fn frame(opcode: u8, payload: &[u8], mask: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode);
    let bit = if mask { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => out.push(bit | n as u8),
        n if n <= 0xffff => {
            out.push(bit | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(bit | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    if mask {
        let key: [u8; 4] = random();
        out.extend_from_slice(&key);
        out.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
    } else {
        out.extend_from_slice(payload);
    }
    out
}

/// A whole frame at the start of `raw`: its opcode, payload and length.
fn parse(raw: &[u8]) -> io::Result<Option<(u8, Vec<u8>, usize)>> {
    if raw.len() < 2 {
        return Ok(None);
    }
    if raw[0] & 0x70 != 0 {
        return Err(bad("a WebSocket extension was not asked for"));
    }
    let opcode = raw[0] & 0x0f;
    let masked = raw[1] & 0x80 != 0;
    let (len, mut at) = match raw[1] & 0x7f {
        126 if raw.len() >= 4 => (u64::from(u16::from_be_bytes([raw[2], raw[3]])), 4),
        127 if raw.len() >= 10 => (u64::from_be_bytes(raw[2..10].try_into().unwrap_or_default()), 10),
        126 | 127 => return Ok(None),
        n => (u64::from(n), 2),
    };
    if len > MAX_FRAME {
        return Err(bad("a WebSocket frame too large"));
    }
    let key = if masked {
        if raw.len() < at + 4 {
            return Ok(None);
        }
        at += 4;
        Some([raw[at - 4], raw[at - 3], raw[at - 2], raw[at - 1]])
    } else {
        None
    };
    let end = at + len as usize;
    if raw.len() < end {
        return Ok(None);
    }
    let mut payload = raw[at..end].to_vec();
    if let Some(key) = key {
        payload.iter_mut().enumerate().for_each(|(i, b)| *b ^= key[i % 4]);
    }
    Ok(Some((opcode, payload, end)))
}

impl<S: AsyncRead + AsyncWrite + Unpin> WsStream<S> {
    fn new(inner: S, client: bool, raw: Vec<u8>) -> Self {
        WsStream { inner, client, raw, data: Vec::new(), taken: 0, closed: false, out: Vec::new(), sent: 0 }
    }

    fn send_out(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.sent < self.out.len() {
            let n = ready!(Pin::new(&mut self.inner).poll_write(cx, &self.out[self.sent..]))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.sent += n;
        }
        self.out.clear();
        self.sent = 0;
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for WsStream<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        loop {
            if this.taken < this.data.len() {
                let n = buf.remaining().min(this.data.len() - this.taken);
                buf.put_slice(&this.data[this.taken..this.taken + n]);
                this.taken += n;
                return Poll::Ready(Ok(()));
            }
            if this.closed {
                return Poll::Ready(Ok(()));
            }
            if let Some((opcode, payload, used)) = parse(&this.raw)? {
                this.raw.drain(..used);
                match opcode {
                    0 | 2 => {
                        this.data = payload;
                        this.taken = 0;
                    }
                    8 => this.closed = true,
                    9 => {
                        // A ping is answered with the next write, or now if it can go.
                        let pong = frame(10, &payload, this.client);
                        this.out.extend_from_slice(&pong);
                        let _ = this.send_out(cx);
                    }
                    10 => {}
                    _ => return Poll::Ready(Err(bad("a WebSocket text or unknown frame"))),
                }
                continue;
            }
            let mut chunk = [0u8; 8192];
            let mut read = ReadBuf::new(&mut chunk);
            ready!(Pin::new(&mut this.inner).poll_read(cx, &mut read))?;
            if read.filled().is_empty() {
                this.closed = true;
            } else {
                this.raw.extend_from_slice(read.filled());
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for WsStream<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        ready!(this.send_out(cx))?;
        let n = buf.len().min(SEND);
        this.out = frame(2, &buf[..n], this.client);
        this.sent = 0;
        // Sent whole later if not now: the frame is ours once taken.
        let _ = this.send_out(cx);
        Poll::Ready(Ok(n))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.send_out(cx))?;
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.send_out(cx))?;
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Reads an HTTP head, up to and with its blank line, a byte at a time so
/// that nothing after it is taken.
async fn head<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<String> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        if bytes.len() >= MAX_HEAD {
            return Err(bad("an HTTP head too long"));
        }
        let mut one = [0u8];
        if stream.read(&mut one).await? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        bytes.push(one[0]);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
}

/// The client's side: asks `host` (the `Host` header) for `path`.
pub async fn client<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, host: &str, path: &str) -> Result<WsStream<S>, String> {
    let key = base64(&random::<16>());
    let ask = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: checkd\r\n\r\n"
    );
    stream.write_all(ask.as_bytes()).await.map_err(|err| err.to_string())?;
    let answer = head(&mut stream).await.map_err(|err| format!("WebSocket: {err}"))?;
    let status = answer.lines().next().unwrap_or("");
    if status.split_whitespace().nth(1) != Some("101") {
        return Err(format!("WebSocket: the server answered {status}"));
    }
    if header(&answer, "Sec-WebSocket-Accept") != Some(accept_of(&key).as_str()) {
        return Err("WebSocket: the server's answer does not match the key".into());
    }
    Ok(WsStream::new(stream, true, Vec::new()))
}

/// The server's side: answers one upgrade, on any path.
pub async fn server<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S) -> Result<WsStream<S>, String> {
    let ask = head(&mut stream).await.map_err(|err| err.to_string())?;
    let websocket = ask.starts_with("GET ") && header(&ask, "Upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let Some(key) = header(&ask, "Sec-WebSocket-Key").filter(|_| websocket) else {
        let _ = stream
            .write_all(b"HTTP/1.1 426 Upgrade Required\r\nUpgrade: websocket\r\nConnection: close\r\nContent-Length: 0\r\n\r\n")
            .await;
        return Err("not a WebSocket request".into());
    };
    let answer = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        accept_of(key)
    );
    stream.write_all(answer.as_bytes()).await.map_err(|err| err.to_string())?;
    Ok(WsStream::new(stream, false, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_accept_of_the_rfc_example() {
        assert_eq!(accept_of("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }

    #[tokio::test]
    async fn bytes_go_both_ways_in_frames_of_any_size_and_pings_are_answered() {
        let (a, b) = tokio::io::duplex(1 << 16);
        let (client, server) = tokio::join!(client(a, "engine.example.test", "/workers"), server(b));
        let (mut client, mut server) = (client.unwrap(), server.unwrap());
        let big: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        let sending = big.clone();
        let writer = tokio::spawn(async move {
            client.write_all(&sending).await.unwrap();
            client.write_all(b"tail").await.unwrap();
            client.flush().await.unwrap();
            let mut back = [0u8; 4];
            client.read_exact(&mut back).await.unwrap();
            back
        });
        let mut got = vec![0u8; big.len() + 4];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(&got[..big.len()], &big[..]);
        assert_eq!(&got[big.len()..], b"tail");
        // A ping from the server's side, then data: the client reads past it.
        server.out.extend_from_slice(&frame(9, b"hi", false));
        server.write_all(b"back").await.unwrap();
        server.flush().await.unwrap();
        assert_eq!(&writer.await.unwrap(), b"back");
    }

    #[tokio::test]
    async fn a_plain_http_request_is_turned_away() {
        let (mut a, b) = tokio::io::duplex(4096);
        let serving = tokio::spawn(server(b));
        a.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
        let mut answer = String::new();
        a.read_to_string(&mut answer).await.unwrap();
        assert!(answer.starts_with("HTTP/1.1 426"));
        assert!(serving.await.unwrap().is_err());
    }
}
