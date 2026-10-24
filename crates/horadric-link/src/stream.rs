//! A TCP connection, under TLS for `https` and `wss`, that HTTPS and the
//! WebSocket both run over. TLS is rustls on ring with Mozilla's roots
//! compiled in, so a server with no certificate store still connects.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

/// How long reaching a server may take, per address it resolves to.
pub const CONNECT: Duration = Duration::from_secs(10);

/// One TLS setup for the whole process: building the root store each
/// time would parse every root again.
fn config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            // The provider is named here rather than installed for the
            // process, so nothing else linked in can swap it.
            let mut config = ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("ring supports TLS 1.2 and 1.3")
            .with_root_certificates(roots)
            .with_no_client_auth();
            // A WebSocket upgrade is HTTP/1.1, so it is all we speak.
            config.alpn_protocols = vec![b"http/1.1".to_vec()];
            Arc::new(config)
        })
        .clone()
}

pub enum Stream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Stream {
    /// Connects to `host:port`, trying each address it resolves to, with
    /// TLS when `secure`. The handshake happens on the first read or write.
    pub fn connect(host: &str, port: u16, secure: bool) -> Result<Stream, String> {
        let tcp = dial(host, port)?;
        let _ = tcp.set_nodelay(true);
        if !secure {
            return Ok(Stream::Plain(tcp));
        }
        let name = ServerName::try_from(host.to_string())
            .map_err(|_| format!("`{host}` is not a name TLS can check"))?;
        let conn = ClientConnection::new(config(), name).map_err(|e| format!("TLS: {e}"))?;
        Ok(Stream::Tls(Box::new(StreamOwned::new(conn, tcp))))
    }

    pub fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => s.get_ref(),
        }
    }

    pub fn set_timeouts(&self, read: Option<Duration>, write: Option<Duration>) {
        let _ = self.tcp().set_read_timeout(read);
        let _ = self.tcp().set_write_timeout(write);
    }

    /// Finishes the TLS handshake now, so a bad certificate is an error
    /// from connecting rather than from the first request.
    pub fn handshake(&mut self) -> io::Result<()> {
        if let Stream::Tls(s) = self {
            while s.conn.is_handshaking() {
                s.conn.complete_io(&mut s.sock)?;
            }
        }
        Ok(())
    }
}

fn dial(host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot find {host}: {e}"))?;
    let mut last = format!("{host} has no address");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT) {
            Ok(s) => return Ok(s),
            Err(e) => last = format!("cannot reach {host} ({addr}): {e}"),
        }
    }
    Err(last)
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// Whether an error only means a read timed out, which on Linux is
/// `WouldBlock` and on Windows `TimedOut`.
pub fn timed_out(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeout_reads_as_one_on_every_system() {
        assert!(timed_out(&io::ErrorKind::WouldBlock.into()));
        assert!(timed_out(&io::ErrorKind::TimedOut.into()));
        assert!(!timed_out(&io::ErrorKind::ConnectionReset.into()));
        assert!(!timed_out(&io::ErrorKind::UnexpectedEof.into()));
    }

    #[test]
    fn the_roots_are_compiled_in() {
        assert!(config().alpn_protocols == [b"http/1.1".to_vec()]);
        assert!(webpki_roots::TLS_SERVER_ROOTS.len() > 100);
    }
}
