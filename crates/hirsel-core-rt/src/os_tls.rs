//! The OS provider's TLS sessions. Socket reads and writes never block.
mod gcm;
mod records;
mod rustls_stream;

use crate::{
    net::{SecureStream, tls_failure},
    os_net,
    provider::*,
};
use hirsel_core_net::{Name, TlsTrust, scram::Binding};
#[cfg(test)]
pub(crate) use records::{TX_RECORDS, TX_TEST_LIMIT};
use rustls::{
    CertificateError, ClientConfig, ProtocolVersion, RootCertStore,
    client::{Resumption, Tls12Resumption, UnbufferedClientConnection},
    pki_types::ServerName,
};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, Read, Write},
    sync::{Arc, OnceLock},
};

const RECORD: usize = 16 * 1024;
const HEADER: usize = 5;
const MAX_PAYLOAD: usize = RECORD + 2048;
const MAX_WIRE: usize = HEADER + MAX_PAYLOAD;
/// The shared slot size: one send batch of whole records, or one receive.
const BULK: usize = records::TX_SLOT;
/// Released bulk slots each thread keeps for its next sends and receives.
const FREE_BULK: usize = 2;
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
        config.enable_secret_extraction = true;
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

pub(crate) struct Stream(rustls_stream::RustlsStream);

impl Stream {
    pub(crate) fn new(name: Name, config: Arc<ClientConfig>) -> Result<Self, Outcome> {
        let server_name = match &name {
            Name::Ip(ip) => ServerName::from(*ip),
            Name::Dns(dns) => {
                ServerName::try_from(dns.to_string()).map_err(|_| tls_failure(1, 0))?
            }
        };
        let connection =
            UnbufferedClientConnection::new(config, server_name).map_err(protocol_error)?;
        Ok(Self(rustls_stream::RustlsStream::new(connection)))
    }

    /// Pending carries the desired direction, false for read or true for write.
    pub(crate) fn handshake(&mut self, fd: i32) -> Result<Result<(), bool>, Outcome> {
        let result = self.0.advance(&mut SocketIo(fd), true);
        self.0.rx.release();
        result
    }

    pub(crate) fn finish(
        self,
        protocol: &mut [u8],
        peer: &mut [u8; 32],
    ) -> Result<(usize, Binding, Box<dyn SecureStream>), Outcome> {
        let chosen = self.0.connection.alpn_protocol().unwrap_or_default();
        let len = chosen.len();
        protocol[..len].copy_from_slice(chosen);
        let leaf = &self.0.connection.peer_certificates().unwrap()[0];
        peer.copy_from_slice(sha256(leaf.as_ref()).as_ref());
        let binding = certificate_binding(leaf.as_ref()).unwrap_or_default();
        let stream: Box<dyn SecureStream> =
            if self.0.connection.protocol_version() == Some(ProtocolVersion::TLSv1_3) {
                let (secrets, kernel) = self
                    .0
                    .connection
                    .dangerous_into_kernel_connection()
                    .map_err(protocol_error)?;
                Box::new(records::Records::new(secrets, kernel, self.0.rx)?)
            } else {
                Box::new(self.0)
            };
        Ok((len, binding, stream))
    }
}

thread_local! {
    static FREE: std::cell::RefCell<Vec<Box<[u8]>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// One runtime-owned byte slot, with a cursor for nonblocking socket I/O.
///
/// A bulk slot holds its bytes only while they hold data: it takes them from
/// the thread's free list when it is written and returns them once empty, so
/// an idle connection keeps no slot bytes.
struct Slot {
    bytes: Box<[u8]>,
    start: usize,
    end: usize,
}

impl Slot {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: vec![0; capacity].into_boxed_slice(),
            start: 0,
            end: 0,
        }
    }

    fn bulk() -> Self {
        Self::new(0)
    }

    fn acquire(&mut self) {
        if self.bytes.is_empty() {
            self.bytes = FREE
                .with_borrow_mut(Vec::pop)
                .unwrap_or_else(|| vec![0; BULK].into_boxed_slice());
        }
    }

    /// Gives an empty bulk slot's bytes back to the thread's free list.
    fn release(&mut self) {
        if self.start == self.end && self.bytes.len() == BULK {
            let bytes = std::mem::take(&mut self.bytes);
            let _ = FREE.try_with(|free| {
                let mut free = free.borrow_mut();
                if free.len() < FREE_BULK {
                    free.push(bytes);
                }
            });
            self.start = 0;
            self.end = 0;
        }
    }

    fn consume(&mut self, n: usize) {
        Self::consume_cursor(&mut self.start, &mut self.end, n);
    }

    fn consume_cursor(start: &mut usize, end: &mut usize, n: usize) {
        *start += n;
        if *start == *end {
            *start = 0;
            *end = 0;
        }
    }

    fn compact(&mut self) {
        if self.start != 0 {
            self.bytes.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
    }

    fn read(&mut self, socket: &mut SocketIo) -> io::Result<usize> {
        self.acquire();
        self.compact();
        let space = &mut self.bytes[self.end..];
        if space.is_empty() {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let n = socket.read(space)?;
        self.end += n;
        Ok(n)
    }

    fn flush(&mut self, socket: &mut SocketIo) -> io::Result<bool> {
        while self.start < self.end {
            match socket.write(&self.bytes[self.start..self.end]) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                Ok(n) => self.consume(n),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.start = self.end;
        self.release();
    }
}

fn payload_len(header: &[u8]) -> usize {
    usize::from(u16::from_be_bytes([header[3], header[4]]))
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
