//! The transport of `remote.tls@1`: TLS over a TCP or a Unix stream socket, so one
//! authentication story covers both (RUN-13). Client and server configurations follow
//! rustls 0.23.46 (`ClientConfig::builder_with_provider`, `ServerConfig::with_cert_resolver`,
//! `WebPkiClientVerifier`) with the ring provider, as the program-facing TLS provider does
//! (hirsel-rt `net_tls.rs`); the material loaders are hirsel-run-schema's.
//!
//! A connection ([`serve`]) owns one socket and its TLS session on its own thread: it writes
//! the frames handed to it and reads frames as they arrive, so a caller never blocks on the
//! network. A reader thread blocks on the socket and hands each read to the connection
//! through the same queue as its commands, so the connection wakes for whichever comes
//! first and a queued frame is written at once.
use crate::frame::Frame;
use crate::wire::Wire;
use hirsel_run_schema::Endpoint;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// The bound on connecting and on a TLS handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);

/// A connected stream socket of either family.
pub enum Socket {
    Tcp(TcpStream),
    Unix(UnixStream),
}
impl Socket {
    pub fn connect(endpoint: &Endpoint) -> io::Result<Self> {
        Ok(match endpoint {
            Endpoint::Tcp { address, service } => {
                let port: u16 = service
                    .parse()
                    .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
                let mut last = io::Error::from(io::ErrorKind::NotFound);
                let mut connected = None;
                for addr in std::net::ToSocketAddrs::to_socket_addrs(&(address.as_str(), port))? {
                    match TcpStream::connect_timeout(&addr, HANDSHAKE) {
                        Ok(stream) => {
                            connected = Some(stream);
                            break;
                        }
                        Err(error) => last = error,
                    }
                }
                let stream = connected.ok_or(last)?;
                stream.set_nodelay(true)?;
                Self::Tcp(stream)
            }
            Endpoint::Unix(path) => Self::Unix(UnixStream::connect(path)?),
        })
    }
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_read_timeout(timeout),
            Self::Unix(s) => s.set_read_timeout(timeout),
        }
    }
    fn try_clone(&self) -> io::Result<Self> {
        Ok(match self {
            Self::Tcp(s) => Self::Tcp(s.try_clone()?),
            Self::Unix(s) => Self::Unix(s.try_clone()?),
        })
    }
    fn shutdown(&self) {
        let _ = match self {
            Self::Tcp(s) => s.shutdown(std::net::Shutdown::Both),
            Self::Unix(s) => s.shutdown(std::net::Shutdown::Both),
        };
    }
}
impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(buf),
            Self::Unix(s) => s.read(buf),
        }
    }
}
impl Write for Socket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(buf),
            Self::Unix(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.flush(),
            Self::Unix(s) => s.flush(),
        }
    }
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}
fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_owned())
}

/// A caller's TLS client: the host's name, the roots it is verified against and the
/// identity presented when the host asks for one, all read once.
pub struct ClientTls {
    config: Arc<ClientConfig>,
    name: ServerName<'static>,
}
impl ClientTls {
    /// `roots` is a PEM bundle, or None for the built-in webpki set; `identity` a PEM chain
    /// and key.
    pub fn new(
        server_name: &str,
        roots: Option<&[u8]>,
        identity: Option<(&[u8], &[u8])>,
    ) -> io::Result<Self> {
        let name = hirsel_run_schema::tls::server_name(server_name)
            .ok_or_else(|| invalid("the server name"))?;
        let roots = match roots {
            None => RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            },
            Some(pem) => RootCertStore {
                roots: hirsel_run_schema::tls::pem_roots(pem)
                    .ok_or_else(|| invalid("the trust anchors"))?,
            },
        };
        let builder = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|_| invalid("TLS protocol versions"))?
            .with_root_certificates(roots);
        let config = match identity {
            None => builder.with_no_client_auth(),
            Some((chain, key)) => {
                let identity = hirsel_run_schema::tls::identity(chain, key)
                    .ok_or_else(|| invalid("the client identity"))?;
                builder.with_client_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(
                    identity,
                )))
            }
        };
        let mut config = config;
        config.resumption = rustls::client::Resumption::disabled();
        Ok(Self {
            config: Arc::new(config),
            name,
        })
    }
    /// Connect to `endpoint` and complete the handshake.
    pub fn connect(&self, endpoint: &Endpoint) -> io::Result<Tls> {
        let mut socket = Socket::connect(endpoint)?;
        socket.set_read_timeout(Some(HANDSHAKE))?;
        let mut session = ClientConnection::new(self.config.clone(), self.name.clone())
            .map_err(io::Error::other)?;
        while session.is_handshaking() {
            session.complete_io(&mut socket)?;
        }
        Ok(Tls {
            session: rustls::Connection::Client(session),
            socket,
        })
    }
}

/// A runner host's TLS server: its identity and the client trust anchors.
pub struct HostTls {
    config: Arc<ServerConfig>,
}
/// What a client's handshake proved: its end-entity certificate's SHA-256 and DER.
pub struct Peer {
    pub digest: [u8; 32],
    pub certificate: Vec<u8>,
}
impl Peer {
    /// Whether the certificate names `name` (a DNS or IP subject alternative name).
    pub fn names(&self, name: &str) -> bool {
        let der = CertificateDer::from(self.certificate.as_slice());
        let (Some(name), Ok(cert)) = (
            hirsel_run_schema::tls::server_name(name),
            webpki::EndEntityCert::try_from(&der),
        ) else {
            return false;
        };
        cert.verify_is_valid_for_subject_name(&name).is_ok()
    }
}
impl HostTls {
    /// `clients` is the PEM bundle client certificates must chain to, or None to serve
    /// clients without certificates.
    pub fn new(chain: &[u8], key: &[u8], clients: Option<&[u8]>) -> io::Result<Self> {
        let identity = hirsel_run_schema::tls::identity(chain, key)
            .ok_or_else(|| invalid("the host identity"))?;
        let builder = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|_| invalid("TLS protocol versions"))?;
        let builder = match clients {
            None => builder.with_no_client_auth(),
            Some(pem) => {
                let trust = hirsel_run_schema::tls::pem_roots(pem)
                    .ok_or_else(|| invalid("the client trust anchors"))?;
                let verifier = hirsel_run_schema::tls::client_verifier(trust, true)
                    .ok_or_else(|| invalid("the client trust anchors"))?;
                builder.with_client_cert_verifier(verifier)
            }
        };
        let mut config =
            builder.with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(identity)));
        config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
        config.send_tls13_tickets = 0;
        Ok(Self {
            config: Arc::new(config),
        })
    }
    /// Complete a server handshake on an accepted socket: the session and the client's
    /// certificate, when it presented one.
    pub fn accept(&self, socket: Socket) -> io::Result<(Tls, Option<Peer>)> {
        let mut socket = socket;
        socket.set_read_timeout(Some(HANDSHAKE))?;
        let mut session = ServerConnection::new(self.config.clone()).map_err(io::Error::other)?;
        while session.is_handshaking() {
            session.complete_io(&mut socket)?;
        }
        let peer = session
            .peer_certificates()
            .and_then(|chain| chain.first())
            .map(|leaf| Peer {
                digest: Sha256::digest(leaf.as_ref()).into(),
                certificate: leaf.to_vec(),
            });
        Ok((
            Tls {
                session: rustls::Connection::Server(session),
                socket,
            },
            peer,
        ))
    }
}

/// A host's listening socket.
pub enum Listener {
    Tcp(std::net::TcpListener),
    Unix(UnixListener),
}
impl Listener {
    /// Listen at `endpoint`. A Unix socket path whose socket no longer accepts connections
    /// (a previous host's) is replaced; one a live host serves is refused.
    pub fn bind(endpoint: &Endpoint) -> io::Result<Self> {
        Ok(match endpoint {
            Endpoint::Tcp { address, service } => {
                let port: u16 = service.parse().map_err(|_| invalid("the service"))?;
                Self::Tcp(std::net::TcpListener::bind((address.as_str(), port))?)
            }
            Endpoint::Unix(path) => match UnixListener::bind(path) {
                Ok(listener) => Self::Unix(listener),
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                    if UnixStream::connect(path).is_ok() {
                        return Err(error);
                    }
                    std::fs::remove_file(path)?;
                    Self::Unix(UnixListener::bind(path)?)
                }
                Err(error) => return Err(error),
            },
        })
    }
    pub fn accept(&self) -> io::Result<Socket> {
        Ok(match self {
            Self::Tcp(l) => {
                let (stream, _) = l.accept()?;
                stream.set_nodelay(true)?;
                Socket::Tcp(stream)
            }
            Self::Unix(l) => Socket::Unix(l.accept()?.0),
        })
    }
}

/// One TLS session over its socket.
pub struct Tls {
    session: rustls::Connection,
    socket: Socket,
}

/// What a connection thread reports.
pub enum Event {
    Frame(Frame),
    /// The connection ended: closed by either side, failed, or a frame did not decode.
    Closed,
}
/// What a connection thread is asked to do.
pub enum Command {
    Send(Frame),
    Close,
}

/// What wakes a connection thread: a command, or what its reader read.
enum Input {
    Command(Command),
    Read(Vec<u8>),
    /// The socket's read side ended or failed.
    End,
}

/// The sending end of a connection's queue. Dropping it closes the connection.
pub struct Commands(Sender<Input>);
impl Commands {
    /// Queue `command`; false once the connection has ended.
    pub fn send(&self, command: Command) -> bool {
        self.0.send(Input::Command(command)).is_ok()
    }
}
impl Drop for Commands {
    fn drop(&mut self) {
        let _ = self.0.send(Input::Command(Command::Close));
    }
}
/// A connection's queue, which [`serve`] takes.
pub struct Queue {
    inputs: Sender<Input>,
    received: Receiver<Input>,
}
/// A new connection queue and its sending end.
pub fn queue() -> (Commands, Queue) {
    let (inputs, received) = channel();
    (Commands(inputs.clone()), Queue { inputs, received })
}

/// Run a connection until it closes: write every frame its [`Commands`] hand over, and
/// report every frame that arrives, then [`Event::Closed`], through `report`. `report`
/// returns false once nobody listens, which also ends the connection.
pub fn serve(tls: Tls, wire: Arc<Wire>, queue: Queue, mut report: impl FnMut(Event) -> bool) {
    let Tls {
        session,
        mut socket,
    } = tls;
    let Queue { inputs, received } = queue;
    let reader = socket
        .set_read_timeout(None)
        .and_then(|()| socket.try_clone());
    let reader = reader.and_then(|reader| {
        std::thread::Builder::new()
            .name("runner connection reader".into())
            .spawn(move || read(reader, inputs))
    });
    if reader.is_ok() {
        match session {
            rustls::Connection::Client(mut session) => {
                pump(&mut session, &mut socket, &wire, &received, &mut report)
            }
            rustls::Connection::Server(mut session) => {
                pump(&mut session, &mut socket, &wire, &received, &mut report)
            }
        }
    }
    // Ends the reader's blocking read too.
    socket.shutdown();
    report(Event::Closed);
}

/// The reader thread: every read of the socket, until it ends or nobody listens.
fn read(mut socket: Socket, inputs: Sender<Input>) {
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let input = match socket.read(&mut chunk) {
            Ok(0) => Input::End,
            Ok(n) => Input::Read(chunk[..n].to_vec()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => Input::End,
        };
        let end = matches!(input, Input::End);
        if inputs.send(input).is_err() || end {
            return;
        }
    }
}

/// Write every TLS record the session holds.
fn flush<S: rustls::SideData>(
    session: &mut rustls::ConnectionCommon<S>,
    socket: &mut Socket,
) -> io::Result<()> {
    while session.wants_write() {
        match session.write_tls(socket) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn pump<S: rustls::SideData>(
    session: &mut rustls::ConnectionCommon<S>,
    socket: &mut Socket,
    wire: &Wire,
    received: &Receiver<Input>,
    report: &mut impl FnMut(Event) -> bool,
) {
    // A frame is queued whole and written at once; the wire bounds its size.
    session.set_buffer_limit(None);
    let mut inbound = Inbound {
        buffer: vec![],
        plain: vec![0u8; 64 * 1024],
    };
    // The handshake's last read may also have taken the peer's first records: they are
    // the session's already, and no later read brings them.
    if inbound.receive(session, &[], wire, report) && flush(session, socket).is_ok() {
        while let Ok(input) = received.recv() {
            let open = match input {
                Input::Command(Command::Send(frame)) => {
                    wire.write(&mut session.writer(), &frame).is_ok()
                }
                Input::Command(Command::Close) | Input::End => false,
                Input::Read(bytes) => inbound.receive(session, &bytes, wire, report),
            };
            // What a command wrote, and any record the session answers a read with.
            if !open || flush(session, socket).is_err() {
                break;
            }
        }
    }
    session.send_close_notify();
    let _ = flush(session, socket);
}

/// A connection's received plaintext not yet whole frames.
struct Inbound {
    buffer: Vec<u8>,
    plain: Vec<u8>,
}
impl Inbound {
    /// Take `bytes` read from the socket, and report each whole frame the session then
    /// holds; false when the connection ends.
    fn receive<S: rustls::SideData>(
        &mut self,
        session: &mut rustls::ConnectionCommon<S>,
        bytes: &[u8],
        wire: &Wire,
        report: &mut impl FnMut(Event) -> bool,
    ) -> bool {
        let mut rest = bytes;
        loop {
            if !rest.is_empty() && session.read_tls(&mut rest).is_err() {
                return false;
            }
            if session.process_new_packets().is_err() {
                return false;
            }
            loop {
                match session.reader().read(&mut self.plain) {
                    // The peer's close_notify.
                    Ok(0) => return false,
                    Ok(n) => self.buffer.extend_from_slice(&self.plain[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => return false,
                }
            }
            if rest.is_empty() {
                break;
            }
        }
        // Every whole frame the buffer holds.
        while self.buffer.len() >= 8 {
            let len = u64::from_le_bytes(self.buffer[..8].try_into().unwrap());
            if len > wire.limit() {
                return false;
            }
            let end = 8 + len as usize;
            if self.buffer.len() < end {
                break;
            }
            let frame = wire.decode(&self.buffer[8..end]);
            self.buffer.drain(..end);
            match frame {
                Ok(frame) => {
                    if !report(Event::Frame(frame)) {
                        return false;
                    }
                }
                // A frame that does not decode ends the session: nothing after it can be
                // trusted to be framed.
                Err(_) => return false,
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hirsel_types::serialization::{
        SERIALIZABLE_SCHEMA_VERSION, SchemaId, SchemaNode, SerializableSchema,
    };

    const CERT: &[u8] = include_bytes!("../../hirsel-rt/tests/it/tls-data/server.cert");
    const KEY: &[u8] = include_bytes!("../../hirsel-rt/tests/it/tls-data/server.key");

    /// Moves every record `from` has to write into `to`.
    fn shuttle(
        from: &mut impl std::ops::DerefMut<Target = rustls::ConnectionCommon<impl rustls::SideData>>,
        to: &mut impl std::ops::DerefMut<Target = rustls::ConnectionCommon<impl rustls::SideData>>,
    ) {
        let mut records = vec![];
        while from.wants_write() {
            from.write_tls(&mut records).unwrap();
        }
        let mut rest = &records[..];
        while !rest.is_empty() {
            to.read_tls(&mut rest).unwrap();
            to.process_new_packets().unwrap();
        }
    }

    /// A caller's first record can reach the host in the same read as its handshake
    /// Finished, so the handshake hands the session over already holding it; no later
    /// read brings it again. Here that record is a frame header over the wire's limit,
    /// which must end the session at once: a session that only looks at what it holds
    /// when the socket next has bytes would wait forever.
    #[test]
    fn a_session_acts_on_records_its_handshake_already_read() {
        let client = ClientTls::new("localhost", Some(CERT), None).unwrap();
        let host = HostTls::new(CERT, KEY, None).unwrap();
        let mut c = ClientConnection::new(client.config.clone(), client.name.clone()).unwrap();
        let mut s = ServerConnection::new(host.config.clone()).unwrap();
        while c.is_handshaking() {
            shuttle(&mut c, &mut s);
            shuttle(&mut s, &mut c);
        }
        c.writer().write_all(&u64::MAX.to_le_bytes()).unwrap();
        shuttle(&mut c, &mut s);
        assert!(!s.is_handshaking());

        let unit = SerializableSchema {
            version: SERIALIZABLE_SCHEMA_VERSION,
            root: SchemaId(0),
            nodes: vec![SchemaNode::Tuple(vec![])],
        };
        let wire = Arc::new(Wire::new(&unit, &unit, &unit, 1 << 20).unwrap());
        let (socket, _peer) = UnixStream::pair().unwrap();
        let tls = Tls {
            session: rustls::Connection::Server(s),
            socket: Socket::Unix(socket),
        };
        let (commands, queue) = queue();
        let (events, seen) = channel();
        let served = std::thread::spawn(move || {
            serve(tls, wire, queue, |event| {
                events.send(matches!(event, Event::Closed)).is_ok()
            })
        });
        let closed = seen.recv_timeout(Duration::from_secs(10));
        drop(commands);
        served.join().unwrap();
        assert_eq!(closed, Ok(true));
    }
}
