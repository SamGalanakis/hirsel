//! The OS provider's rustls session. Socket reads and writes never block.
use crate::{
    net::{SecureStream, tls_failure},
    os_net,
    provider::*,
};
use hirsel_core_net::{Name, TlsTrust, scram::Binding};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, RootCertStore,
    client::{Resumption, Tls12Resumption},
    pki_types::ServerName,
};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, IoSlice, Read, Write},
    sync::{Arc, OnceLock},
};

const RECORD: usize = 16 * 1024;
const CACHED_ROOT_BUNDLES: usize = 16;
const CACHED_CLIENT_CONFIGS: usize = 16;
const TICKETS_PER_CONFIG: usize = 64;
pub(crate) const CACHED_PEM_BYTES: usize = 64 * 1024;

struct TrustedRoots {
    pem: Box<[u8]>,
    pin: TlsTrust,
    store: Arc<RootCertStore>,
}

#[derive(Default)]
pub(crate) struct Streams {
    pub sessions: BTreeMap<u64, Box<dyn SecureStream>>,
    pub system: Arc<OnceLock<Arc<RootCertStore>>>,
    custom_roots: VecDeque<TrustedRoots>,
    clients: VecDeque<(Arc<RootCertStore>, Arc<ClientConfig>)>,
}

impl Streams {
    pub(crate) fn custom_roots(&self, pem: &[u8]) -> Option<(TlsTrust, Arc<RootCertStore>)> {
        self.custom_roots
            .iter()
            .find(|roots| &*roots.pem == pem)
            .map(|roots| (roots.pin, roots.store.clone()))
    }

    pub(crate) fn remember_roots(&mut self, pem: &[u8], pin: TlsTrust, store: Arc<RootCertStore>) {
        if self.custom_roots.len() == CACHED_ROOT_BUNDLES {
            self.custom_roots.pop_front();
        }
        self.custom_roots.push_back(TrustedRoots {
            pem: pem.into(),
            pin,
            store,
        });
    }

    pub(crate) fn client_config(
        &mut self,
        roots: Arc<RootCertStore>,
        alpn: Vec<Vec<u8>>,
        cacheable: bool,
    ) -> Result<Arc<ClientConfig>, Outcome> {
        if cacheable
            && let Some((_, config)) = self
                .clients
                .iter()
                .find(|(store, config)| Arc::ptr_eq(store, &roots) && config.alpn_protocols == alpn)
        {
            return Ok(config.clone());
        }
        let mut config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(protocol_error)?
                .with_root_certificates(roots.clone())
                .with_no_client_auth();
        config.resumption = if cacheable {
            Resumption::in_memory_sessions(TICKETS_PER_CONFIG)
                .tls12_resumption(Tls12Resumption::Disabled)
        } else {
            Resumption::disabled()
        };
        config.alpn_protocols = alpn;
        let config = Arc::new(config);
        if cacheable {
            if self.clients.len() == CACHED_CLIENT_CONFIGS {
                self.clients.pop_front();
            }
            self.clients.push_back((roots, config.clone()));
        }
        Ok(config)
    }
}

pub(crate) struct Stream {
    connection: ClientConnection,
    read_error: Option<Outcome>,
    write_error: Option<Outcome>,
}

impl Stream {
    pub(crate) fn new(name: Name, config: Arc<ClientConfig>) -> Result<Self, Outcome> {
        let server_name = match &name {
            Name::Ip(ip) => ServerName::from(*ip),
            Name::Dns(dns) => {
                ServerName::try_from(dns.to_string()).map_err(|_| tls_failure(1, 0))?
            }
        };
        let connection = ClientConnection::new(config, server_name).map_err(protocol_error)?;
        Ok(Self {
            connection,
            read_error: None,
            write_error: None,
        })
    }

    /// Pending carries the desired direction, false for read or true for write.
    pub(crate) fn handshake(&mut self, fd: i32) -> Result<Result<(), bool>, Outcome> {
        let mut io = SocketIo(fd);
        loop {
            if !flush(&mut self.connection, &mut io).map_err(transport)? {
                return Ok(Err(true));
            }
            if !self.connection.is_handshaking() {
                return Ok(Ok(()));
            }
            match self.connection.read_tls(&mut io) {
                Ok(0) => return Err(tls_failure(5, 0)),
                Ok(_) => process(&mut self.connection, &mut io)?,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(Err(false)),
                Err(error) => return Err(transport(error)),
            }
        }
    }

    pub(crate) fn handshake_result(
        &self,
        protocol: &mut [u8],
        peer: &mut [u8; 32],
    ) -> (usize, Binding) {
        let chosen = self.connection.alpn_protocol().unwrap_or_default();
        protocol[..chosen.len()].copy_from_slice(chosen);
        let leaf = &self.connection.peer_certificates().unwrap()[0];
        peer.copy_from_slice(sha256(leaf.as_ref()).as_ref());
        (
            chosen.len(),
            certificate_binding(leaf.as_ref()).unwrap_or_default(),
        )
    }
}

impl SecureStream for Stream {
    fn receive(&mut self, fd: i32, plaintext: &mut [u8]) -> Option<Outcome> {
        if let Some(error) = self.read_error {
            return Some(error);
        }
        if plaintext.is_empty() {
            return Some(count(0));
        }
        let mut io = SocketIo(fd);
        loop {
            match self.connection.reader().read(plaintext) {
                Ok(n) => return Some(count(n)),
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    let error = tls_failure(6, 0);
                    self.read_error = Some(error);
                    return Some(error);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Some(transport(error)),
            }
            match self.connection.read_tls(&mut io) {
                Ok(0) => {}
                Ok(_) => {
                    if let Err(error) = process(&mut self.connection, &mut io) {
                        self.read_error = Some(error);
                        return Some(error);
                    }
                    // A receive can help with writes generated by the session.
                    if let Err(error) = flush(&mut self.connection, &mut io) {
                        self.write_error = Some(transport(error));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return None,
                Err(error) => return Some(transport(error)),
            }
        }
    }

    fn send(&mut self, fd: i32, prefix: &[u8], tail: &[u8]) -> Option<Outcome> {
        if let Some(error) = self.write_error {
            return Some(error);
        }
        let mut io = SocketIo(fd);
        if prefix.is_empty() && tail.is_empty() {
            return match flush(&mut self.connection, &mut io) {
                Ok(true) => Some(count(0)),
                Ok(false) => None,
                Err(error) => Some(transport(error)),
            };
        }
        let mut written = 0;
        for part in [prefix, tail] {
            for record in part.chunks(RECORD) {
                match flush(&mut self.connection, &mut io) {
                    Ok(true) => {}
                    Ok(false) => return (written != 0).then(|| count(written)),
                    Err(error) => {
                        let error = transport(error);
                        self.write_error = Some(error);
                        return Some(if written == 0 { error } else { count(written) });
                    }
                }
                match self.connection.writer().write(record) {
                    Ok(n) => {
                        written += n;
                        if n < record.len() {
                            return Some(count(written));
                        }
                    }
                    Err(error) => {
                        let error = transport(error);
                        self.write_error = Some(error);
                        return Some(if written == 0 { error } else { count(written) });
                    }
                }
            }
        }
        if let Err(error) = flush(&mut self.connection, &mut io) {
            self.write_error = Some(transport(error));
        }
        Some(count(written))
    }

    fn shutdown(&mut self, fd: i32) {
        self.connection.send_close_notify();
        let _ = self.connection.write_tls(&mut SocketIo(fd));
    }
}

pub(crate) fn system_roots() -> Arc<RootCertStore> {
    let mut store = RootCertStore::empty();
    store.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    Arc::new(store)
}

pub(crate) fn sha256(bytes: &[u8]) -> ring::digest::Digest {
    ring::digest::digest(&ring::digest::SHA256, bytes)
}

fn count(value: usize) -> Outcome {
    Outcome {
        value: value as u64,
        ..Outcome::default()
    }
}

fn flush(connection: &mut ClientConnection, fd: &mut SocketIo) -> io::Result<bool> {
    while connection.wants_write() {
        match connection.write_tls(fd) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

fn process(connection: &mut ClientConnection, fd: &mut SocketIo) -> Result<(), Outcome> {
    if let Err(error) = connection.process_new_packets() {
        // Send a queued alert if the socket can accept it now.
        let _ = connection.write_tls(fd);
        return Err(protocol_error(error));
    }
    Ok(())
}

fn protocol_error(error: rustls::Error) -> Outcome {
    match error {
        rustls::Error::AlertReceived(alert) => tls_failure(4, u8::from(alert)),
        rustls::Error::InvalidCertificate(error) => tls_failure(
            0,
            match error {
                CertificateError::UnknownIssuer => 0,
                CertificateError::NotValidForName
                | CertificateError::NotValidForNameContext { .. } => 1,
                CertificateError::Expired | CertificateError::ExpiredContext { .. } => 2,
                CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => 3,
                _ => 4,
            },
        ),
        rustls::Error::NoCertificatesPresented | rustls::Error::UnsupportedNameType => {
            tls_failure(0, 4)
        }
        _ => tls_failure(4, 0),
    }
}

fn transport(error: io::Error) -> Outcome {
    let code = error.raw_os_error().unwrap_or(libc::EIO);
    if matches!(code, libc::ECONNRESET | libc::EPIPE | libc::ETIMEDOUT) {
        os_net::signal(5)
    } else {
        os_net::error(code)
    }
}

/// A synchronous borrow of the nonblocking fd in the current socket callback.
struct SocketIo(i32);

impl Read for SocketIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        os_net::retry(|| unsafe { libc::recv(self.0, buf.as_mut_ptr().cast(), buf.len(), 0) })
            .map(|n| n as usize)
            .map_err(io::Error::from_raw_os_error)
    }
}
impl Write for SocketIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        os_net::retry(|| unsafe {
            libc::send(self.0, buf.as_ptr().cast(), buf.len(), libc::MSG_NOSIGNAL)
        })
        .map(|n| n as usize)
        .map_err(io::Error::from_raw_os_error)
    }

    fn write_vectored(&mut self, buf: &[IoSlice<'_>]) -> io::Result<usize> {
        // IoSlice's Unix ABI is iovec. sendmsg borrows the array for this call.
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = buf.as_ptr().cast_mut().cast();
        msg.msg_iovlen = buf.len();
        os_net::retry(|| unsafe { libc::sendmsg(self.0, &msg, libc::MSG_NOSIGNAL) })
            .map(|n| n as usize)
            .map_err(io::Error::from_raw_os_error)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// X.509 Certificate contains TBSCertificate, signatureAlgorithm and
/// signatureValue. rustls has already verified the leaf; select the digest
/// from the outer AlgorithmIdentifier, including the explicit PSS hash.
fn certificate_binding(leaf: &[u8]) -> Option<Binding> {
    use ring::digest::{SHA256, SHA384, SHA512};
    let (certificate, _) = der_field(leaf, 0x30)?;
    let (_, rest) = der_field(certificate, 0x30)?;
    let (signature_algorithm, _) = der_field(rest, 0x30)?;
    let (oid, parameters) = der_field(signature_algorithm, 0x06)?;
    const RSA: &[u8] = b"\x2a\x86\x48\x86\xf7\x0d\x01\x01";
    const ECDSA_SHA2: &[u8] = b"\x2a\x86\x48\xce\x3d\x04\x03";
    const SHA2: &[u8] = b"\x60\x86\x48\x01\x65\x03\x04\x02";
    let algorithm = match (oid.strip_prefix(RSA), oid.strip_prefix(ECDSA_SHA2)) {
        (Some([0x04 | 0x05 | 0x0b]), _) | (_, Some([2])) => &SHA256,
        (Some([0x0c]), _) | (_, Some([3])) => &SHA384,
        (Some([0x0d]), _) | (_, Some([4])) => &SHA512,
        (Some([0x0a]), _) => {
            let (parameters, _) = der_field(parameters, 0x30)?;
            if parameters.first() != Some(&0xa0) {
                // RSASSA-PSS-params defaults the hash to SHA-1.
                &SHA256
            } else {
                let (hash, _) = der_field(parameters, 0xa0)?;
                let (hash, _) = der_field(hash, 0x30)?;
                let (oid, _) = der_field(hash, 0x06)?;
                match oid.strip_prefix(SHA2) {
                    Some([1]) => &SHA256,
                    Some([2]) => &SHA384,
                    Some([3]) => &SHA512,
                    _ if oid == b"\x2b\x0e\x03\x02\x1a" => &SHA256,
                    _ => return None,
                }
            }
        }
        _ if oid == b"\x2a\x86\x48\xce\x3d\x04\x01" => &SHA256,
        _ => return None,
    };
    Binding::from_bytes(ring::digest::digest(algorithm, leaf).as_ref())
}

fn der_field(bytes: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let [actual, length, tail @ ..] = bytes else {
        return None;
    };
    if *actual != tag {
        return None;
    }
    if *length < 0x80 {
        return tail.split_at_checked(usize::from(*length));
    }
    let size = usize::from(*length & 0x7f);
    if size == 0 || size > size_of::<usize>() {
        return None;
    }
    let (encoded, content) = tail.split_at_checked(size)?;
    let length = encoded
        .iter()
        .fold(0usize, |n, &b| (n << 8) | usize::from(b));
    content.split_at_checked(length)
}
