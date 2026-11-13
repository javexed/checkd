//! Sends what it reads to a Unix socket and prints what comes back: for
//! putting Livestatus queries to checkd in the runs, where the
//! reference's own tool of that name is not at hand (`runs/real-chroot`).
//!
//!     unixcat <socket> < query

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

fn main() -> std::io::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_default();
    let mut request = Vec::new();
    std::io::stdin().read_to_end(&mut request)?;
    let mut stream = UnixStream::connect(&path)?;
    stream.write_all(&request)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer)?;
    std::io::stdout().write_all(&answer)
}
