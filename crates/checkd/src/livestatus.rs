//! The Livestatus server: a Unix socket that answers Livestatus requests
//! from the engine's state.
//!
//! Each connection is read in a task of its own. A request, the lines up
//! to an empty line or the end of what the client sent, goes to the
//! engine's loop, which answers it between events and hands a `COMMAND`
//! to the engine. With `KeepAlive: on` the connection waits for the next
//! request; otherwise it is closed after the answer.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

/// A request from a client, and where its answer goes.
pub struct Ask {
    pub text: String,
    pub reply: oneshot::Sender<checkd_livestatus::Response>,
}

/// The socket path of a Livestatus `broker_module` line, if the line is
/// one: `broker_module=/usr/lib/.../livestatus.o /var/run/live debug=1`.
pub fn socket_of_broker_module(line: &str) -> Option<PathBuf> {
    let mut words = line.split_whitespace();
    let module = words.next()?;
    let name = Path::new(module).file_name()?.to_string_lossy().to_lowercase();
    if !name.starts_with("livestatus") {
        return None;
    }
    words.find(|w| !w.contains('=')).map(PathBuf::from)
}

/// Starts listening at a Unix socket, at a TCP address, or both.
/// Requests come out of the answer's receiver.
pub fn listen(path: Option<&Path>, tcp: Option<&str>) -> Result<mpsc::UnboundedReceiver<Ask>, String> {
    let (asks, inbox) = mpsc::unbounded_channel();
    if let Some(path) = path {
        if path.exists() {
            std::fs::remove_file(path)
                .map_err(|err| format!("cannot remove the old socket {}: {err}", path.display()))?;
        }
        let listener = UnixListener::bind(path)
            .map_err(|err| format!("cannot make the Livestatus socket {}: {err}", path.display()))?;
        // For checkd's user and group, whatever the umask: a web interface
        // whose user is in checkd's group connects, others do not.
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
                .map_err(|err| format!("cannot set the mode of the Livestatus socket {}: {err}", path.display()))?;
        }
        let asks = asks.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                let (read, write) = stream.into_split();
                tokio::spawn(serve(read, write, asks.clone()));
            }
        });
    }
    if let Some(addr) = tcp {
        let std_listener = std::net::TcpListener::bind(addr)
            .map_err(|err| format!("cannot listen for Livestatus at {addr}: {err}"))?;
        std_listener.set_nonblocking(true).map_err(|err| err.to_string())?;
        let listener = tokio::net::TcpListener::from_std(std_listener).map_err(|err| err.to_string())?;
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else { continue };
                let (read, write) = stream.into_split();
                tokio::spawn(serve(read, write, asks.clone()));
            }
        });
    }
    Ok(inbox)
}

/// The longest a request may be.
const MOST_REQUEST: usize = 1 << 20;

/// One connection: requests in, answers out.
async fn serve<R, W>(read: R, mut write: W, asks: mpsc::UnboundedSender<Ask>)
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(read);
    loop {
        let mut text = String::new();
        let mut ended = false;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => {
                    ended = true;
                    break;
                }
                Ok(_) => {
                    // No request is a megabyte long: one that goes on is let go.
                    if text.len() + line.len() > MOST_REQUEST {
                        return;
                    }
                    if line.trim_end_matches(['\n', '\r']).is_empty() {
                        if text.is_empty() {
                            continue;
                        }
                        break;
                    }
                    text.push_str(&line);
                }
            }
        }
        if text.trim().is_empty() {
            return;
        }
        let (reply, answer) = oneshot::channel();
        if asks.send(Ask { text, reply }).is_err() {
            return;
        }
        let Ok(response) = answer.await else { return };
        if write.write_all(&response.bytes).await.is_err() {
            return;
        }
        let _ = write.flush().await;
        if !response.keep_alive || ended {
            let _ = write.shutdown().await;
            return;
        }
    }
}
