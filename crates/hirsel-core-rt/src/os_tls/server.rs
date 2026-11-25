//! TLS-9 server handshakes in the OS provider: rustls's `Acceptor`, a
//! TLS 1.3-only configuration with no session state, and the private key
//! parsed in place by ring behind the provider's own `SigningKey`.
use super::*;
use rustls::{
    Error, ServerConfig, ServerConnection, SignatureAlgorithm, SignatureScheme,
    pki_types::{CertificateDer, SubjectPublicKeyInfoDer, alg_id},
    server::{Accepted, AcceptedAlert, ClientHello, NoServerSessionStorage, ResolvesServerCert},
    sign::{CertifiedKey, Signer, SigningKey, public_key_to_spki},
};
use std::fmt;

/// A fatal `internal_error` alert record (RFC 8446 §6), sent in plaintext
/// before any ServerHello: a refusal before issue and `Tls(Key)`.
pub(crate) const INTERNAL_ERROR: [u8; 7] = [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 80];
/// A fatal `unrecognized_name` alert record: a dropped `Hello` (RFC 6066 §3).
pub(crate) const UNRECOGNIZED_NAME: [u8; 7] = [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 112];

/// One nonblocking attempt to send a plaintext alert record.
pub(crate) fn send_alert(fd: i32, record: &[u8; 7]) {
    // SAFETY: a borrowed live socket descriptor and a 7-byte buffer.
    unsafe {
        libc::send(
            fd,
            record.as_ptr().cast(),
            record.len(),
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
        );
    }
}

/// The ClientHello's server name, normalised as TLS-1 normalises a name.
/// rustls lowercases a DNS name and reports none for an IP literal. Written
/// into `out`, with its length; `None` for a name longer than 253 bytes.
pub(crate) fn server_name(accepted: &Accepted, out: &mut [u8; 253]) -> Option<usize> {
    let hello: ClientHello<'_> = accepted.client_hello();
    let name = hello.server_name().unwrap_or_default().as_bytes();
    let name = name.strip_suffix(b".").unwrap_or(name);
    out.get_mut(..name.len())?.copy_from_slice(name);
    Some(name.len())
}

/// Read until the ClientHello is complete: `Ok(None)` waits for more bytes.
pub(crate) fn read_hello(
    acceptor: &mut rustls::server::Acceptor,
    fd: i32,
) -> Result<Option<Accepted>, Outcome> {
    let socket = &mut SocketIo(fd);
    loop {
        match acceptor.accept() {
            Ok(Some(accepted)) => return Ok(Some(accepted)),
            Ok(None) => {}
            Err((error, alert)) => {
                write_alert(fd, alert);
                return Err(server_error(error));
            }
        }
        match acceptor.read_tls(socket) {
            Ok(0) => return Err(tls_failure(5, 0)),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(io_failure(error)),
        }
    }
}

/// A server-side rustls failure as a `TlsFailure`.
pub(crate) fn server_error(error: Error) -> Outcome {
    match error {
        Error::AlertReceived(alert) => tls_failure(4, u8::from(alert)),
        Error::NoApplicationProtocol => tls_failure(4, 120),
        _ => tls_failure(4, 0),
    }
}

/// Best effort, nonblocking: whatever alert rustls queued.
pub(crate) fn write_alert(fd: i32, mut alert: AcceptedAlert) {
    let _ = alert.write_all(&mut SocketIo(fd));
}

/// Why a key did not load. A host randomness failure is not a key verdict.
pub(crate) enum KeyLoad {
    Invalid,
    Random(&'static str),
}

enum Pair {
    Ed25519(ring::signature::Ed25519KeyPair),
    P256(ring::signature::EcdsaKeyPair),
    Rsa(ring::signature::RsaKeyPair),
}

/// `secret.sign`'s three kinds, read from the document's algorithm
/// identifier. ring's loaders borrow the document and never copy it; any
/// other algorithm, P-384 among them, is no key of ours.
pub(crate) struct ServerKey {
    pair: Arc<Pair>,
    spki: SubjectPublicKeyInfoDer<'static>,
}

const ED25519_OID: &[u8] = b"\x2b\x65\x70";
const EC_OID: &[u8] = b"\x2a\x86\x48\xce\x3d\x02\x01";
const RSA_OID: &[u8] = b"\x2a\x86\x48\x86\xf7\x0d\x01\x01\x01";

fn pkcs8_algorithm(document: &[u8]) -> Option<&[u8]> {
    let (info, _) = der_field(document, 0x30)?;
    let (_, rest) = der_field(info, 0x02)?;
    let (algorithm, _) = der_field(rest, 0x30)?;
    let (oid, _) = der_field(algorithm, 0x06)?;
    Some(oid)
}

impl ServerKey {
    pub(crate) fn load(document: &[u8]) -> Result<Self, KeyLoad> {
        use ring::signature::{self, KeyPair};
        let (pair, spki) = match pkcs8_algorithm(document).ok_or(KeyLoad::Invalid)? {
            ED25519_OID => {
                let pair = signature::Ed25519KeyPair::from_pkcs8_maybe_unchecked(document)
                    .map_err(|_| KeyLoad::Invalid)?;
                let spki = public_key_to_spki(&alg_id::ED25519, pair.public_key());
                (Pair::Ed25519(pair), spki)
            }
            EC_OID => {
                let pair = signature::EcdsaKeyPair::from_pkcs8(
                    &signature::ECDSA_P256_SHA256_ASN1_SIGNING,
                    document,
                    &ring::rand::SystemRandom::new(),
                )
                .map_err(|error| {
                    // ring 0.17.14 exposes the nonce-key RNG failure as a diagnostic.
                    if error.to_string() == "RNG failed" {
                        KeyLoad::Random("tls.accept: P-256 key loading: RNG failed")
                    } else {
                        KeyLoad::Invalid
                    }
                })?;
                let spki = public_key_to_spki(&alg_id::ECDSA_P256, pair.public_key());
                (Pair::P256(pair), spki)
            }
            RSA_OID => {
                // ring refuses a modulus outside 2048 to 4096 bits.
                let pair =
                    signature::RsaKeyPair::from_pkcs8(document).map_err(|_| KeyLoad::Invalid)?;
                let spki = public_key_to_spki(&alg_id::RSA_ENCRYPTION, pair.public_key());
                (Pair::Rsa(pair), spki)
            }
            _ => return Err(KeyLoad::Invalid),
        };
        Ok(Self {
            pair: Arc::new(pair),
            spki,
        })
    }
}

impl fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServerKey")
    }
}

/// TLS 1.3 schemes only (RFC 8446 §4.2.3): RSA signs with PSS.
const RSA_SCHEMES: [SignatureScheme; 3] = [
    SignatureScheme::RSA_PSS_SHA512,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA256,
];

impl SigningKey for ServerKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        let scheme = match &*self.pair {
            Pair::Ed25519(_) => Some(SignatureScheme::ED25519),
            Pair::P256(_) => Some(SignatureScheme::ECDSA_NISTP256_SHA256),
            Pair::Rsa(_) => RSA_SCHEMES.into_iter().find(|s| offered.contains(s)),
        }
        .filter(|scheme| offered.contains(scheme))?;
        Some(Box::new(ServerSigner {
            pair: self.pair.clone(),
            scheme,
        }))
    }

    fn public_key(&self) -> Option<SubjectPublicKeyInfoDer<'_>> {
        Some(self.spki.clone())
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        match &*self.pair {
            Pair::Ed25519(_) => SignatureAlgorithm::ED25519,
            Pair::P256(_) => SignatureAlgorithm::ECDSA,
            Pair::Rsa(_) => SignatureAlgorithm::RSA,
        }
    }
}

struct ServerSigner {
    pair: Arc<Pair>,
    scheme: SignatureScheme,
}

impl fmt::Debug for ServerSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ServerSigner")
    }
}

impl Signer for ServerSigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        use ring::signature;
        let random = ring::rand::SystemRandom::new();
        // Signing fails only when host randomness does: terminal (TLS-8).
        Ok(match &*self.pair {
            Pair::Ed25519(pair) => pair.sign(message).as_ref().to_vec(),
            Pair::P256(pair) => match pair.sign(&random, message) {
                Ok(signature) => signature.as_ref().to_vec(),
                Err(_) => crate::process::stop(b"tls.accept: P-256 signing: RNG failed"),
            },
            Pair::Rsa(pair) => {
                let padding: &'static dyn signature::RsaEncoding = match self.scheme {
                    SignatureScheme::RSA_PSS_SHA512 => &signature::RSA_PSS_SHA512,
                    SignatureScheme::RSA_PSS_SHA384 => &signature::RSA_PSS_SHA384,
                    _ => &signature::RSA_PSS_SHA256,
                };
                let mut out = vec![0; pair.public().modulus_len()];
                if pair.sign(padding, &random, message, &mut out).is_err() {
                    crate::process::stop(b"tls.accept: RSA signing: RNG failed");
                }
                out
            }
        })
    }

    fn scheme(&self) -> SignatureScheme {
        self.scheme
    }
}

/// Holds the key for the one handshake it was built for. rustls asks once
/// per ClientHello, twice after a HelloRetryRequest, so every call answers
/// the same key; `Handshake::finish` clears it, so the parsed key drops when
/// the handshake ends, never with the configuration the connection keeps
/// after it (TLS-9).
#[derive(Debug)]
pub(crate) struct Single(std::sync::Mutex<Option<Arc<CertifiedKey>>>);

impl ResolvesServerCert for Single {
    fn resolve(&self, _: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.0.lock().unwrap().clone()
    }
}

/// The chain and key `accept` was given, with the key checked against the
/// leaf before any ServerHello: `None` is `Tls(Key)`.
pub(crate) fn certified(
    chain: Vec<CertificateDer<'static>>,
    key: ServerKey,
) -> Option<Arc<CertifiedKey>> {
    let certified = CertifiedKey::new(chain, Arc::new(key));
    certified.keys_match().ok()?;
    Some(Arc::new(certified))
}

/// TLS 1.3 only, no session cache, no tickets, no early data, no client
/// authentication, no key log and no secret extraction (TLS-9).
pub(crate) fn config(
    single: Arc<Single>,
    alpn: Vec<Vec<u8>>,
) -> Result<Arc<ServerConfig>, Outcome> {
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(server_error)?
            .with_no_client_auth()
            .with_cert_resolver(single);
    config.session_storage = Arc::new(NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    config.alpn_protocols = alpn;
    Ok(Arc::new(config))
}

/// A server half's session over rustls's buffered connection.
pub(crate) struct ServerStream {
    pub(crate) connection: ServerConnection,
    read_end: Option<Outcome>,
    write_error: Option<Outcome>,
}

impl ServerStream {
    pub(crate) fn new(connection: ServerConnection) -> Self {
        Self {
            connection,
            read_end: None,
            write_error: None,
        }
    }

    /// Pending carries the desired direction, false for read or true for write.
    pub(crate) fn handshake(&mut self, fd: i32) -> Result<Result<(), bool>, Outcome> {
        let socket = &mut SocketIo(fd);
        loop {
            while self.connection.wants_write() {
                match self.connection.write_tls(socket) {
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        return Ok(Err(true));
                    }
                    Err(error) => return Err(transport(error)),
                }
            }
            if !self.connection.is_handshaking() {
                return Ok(Ok(()));
            }
            match self.connection.read_tls(socket) {
                Ok(0) => return Err(tls_failure(5, 0)),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(Err(false)),
                Err(error) => return Err(io_failure(error)),
            }
            if let Err(error) = self.connection.process_new_packets() {
                let _ = self.connection.write_tls(socket);
                return Err(server_error(error));
            }
        }
    }

    fn flush(&mut self, socket: &mut SocketIo) -> Result<bool, Outcome> {
        while self.connection.wants_write() {
            match self.connection.write_tls(socket) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(transport(error)),
            }
        }
        Ok(true)
    }

    /// True once a receive has plaintext, end of stream or an error.
    fn read_ready(&mut self, socket: &mut SocketIo) -> bool {
        if let Err(error) = self.flush(socket) {
            self.write_error.get_or_insert(error);
        }
        loop {
            if self.read_end.is_some() {
                return true;
            }
            match self.connection.process_new_packets() {
                Ok(state) if state.plaintext_bytes_to_read() != 0 => return true,
                Ok(state) if state.peer_has_closed() => {
                    self.read_end = Some(count(0));
                    return true;
                }
                Ok(_) => {}
                Err(error) => {
                    let _ = self.connection.write_tls(socket);
                    self.read_end = Some(server_error(error));
                    return true;
                }
            }
            match self.connection.read_tls(socket) {
                // The transport ended without close_notify.
                Ok(0) => self.read_end = Some(tls_failure(6, 0)),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return false,
                Err(error) => self.read_end = Some(io_failure(error)),
            }
        }
    }
}

fn io_failure(error: io::Error) -> Outcome {
    if error.raw_os_error().is_some() {
        transport(error)
    } else {
        // rustls's own read refusals: an oversized or malformed record.
        tls_failure(4, 0)
    }
}

impl SecureStream for ServerStream {
    fn receive(&mut self, fd: i32, plaintext: &mut [u8]) -> Option<Outcome> {
        if plaintext.is_empty() {
            return Some(self.read_end.unwrap_or_else(|| count(0)));
        }
        if !self.read_ready(&mut SocketIo(fd)) {
            return None;
        }
        match self.connection.reader().read(plaintext) {
            Ok(n) if n != 0 => Some(count(n)),
            _ => Some(self.read_end.unwrap_or_else(|| count(0))),
        }
    }

    fn ready(&mut self, fd: i32) -> Option<Outcome> {
        self.read_ready(&mut SocketIo(fd)).then(|| count(0))
    }

    fn send(&mut self, fd: i32, _: ZeroCopy, prefix: &[u8], tail: &[u8]) -> Option<Outcome> {
        let mut socket = SocketIo(fd);
        let mut committed = 0;
        let result = (|| {
            if let Some(error) = self.write_error {
                return Err(error);
            }
            for record in [prefix, tail]
                .into_iter()
                .flat_map(|part| part.chunks(RECORD))
            {
                if !self.flush(&mut socket)? {
                    return Ok(false);
                }
                let n = self
                    .connection
                    .writer()
                    .write(record)
                    .map_err(|_| tls_failure(4, 0))?;
                committed += n;
                if n != record.len() {
                    return Ok(false);
                }
            }
            self.flush(&mut socket)
        })();
        if let Err(error) = result {
            self.write_error = Some(error);
            if committed == 0 {
                return Some(error);
            }
        }
        (committed != 0 || matches!(result, Ok(true))).then(|| count(committed))
    }

    fn shutdown(&mut self, fd: i32) {
        if self.write_error.is_none() {
            self.connection.send_close_notify();
            let _ = self.flush(&mut SocketIo(fd));
        }
    }
}

/// The server's work between issue and delivery of `TLS_ACCEPT`.
pub(crate) struct Handshake {
    pub(crate) stream: ServerStream,
    key: Arc<Single>,
}

impl Handshake {
    /// The finished stream; the key is gone from the configuration it keeps.
    pub(crate) fn finish(self) -> ServerStream {
        drop(self.key.0.lock().unwrap().take());
        self.stream
    }
}

/// Load, check and start: `Err` is the delivered failure, after its alert.
/// Loading a key and building a configuration can take milliseconds of CPU,
/// so the caller runs this off the slot.
pub(crate) fn start(
    fd: i32,
    accepted: Accepted,
    chain: Vec<CertificateDer<'static>>,
    document: &[u8],
    alpn: Vec<Vec<u8>>,
) -> Result<Handshake, Outcome> {
    let refuse_key = || {
        send_alert(fd, &INTERNAL_ERROR);
        tls_failure(8, 0)
    };
    let key = match ServerKey::load(document) {
        Ok(key) => key,
        Err(KeyLoad::Invalid) => return Err(refuse_key()),
        Err(KeyLoad::Random(reason)) => crate::process::stop(reason.as_bytes()),
    };
    let Some(certified) = certified(chain, key) else {
        return Err(refuse_key());
    };
    let key = Arc::new(Single(std::sync::Mutex::new(Some(certified))));
    let config = config(key.clone(), alpn)?;
    match accepted.into_connection(config) {
        Ok(connection) => Ok(Handshake {
            stream: ServerStream::new(connection),
            key,
        }),
        Err((error, alert)) => {
            write_alert(fd, alert);
            Err(server_error(error))
        }
    }
}
