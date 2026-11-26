//! The quinn peer interop.py runs against the Hirsel QUIC endpoint (law Q4, handshake only,
//! and Q16's client pins). Built by interop.py against quinn at the commit it names.
//!
//! Usage: hirsel_interop client PORT SERVER_PIN_HEX SUITE GROUP [CLIENT_CERT_DER CLIENT_KEY_DER]
//!        hirsel_interop server CERT_DER KEY_DER SUITE GROUP
//!
//! SUITE is a rustls suite name (TLS13_AES_128_GCM_SHA256, ...) and GROUP a rustls group name
//! (X25519, secp256r1); the provider offers that one suite and that one group, so a completed
//! handshake used them. The client trusts the server only by the SHA-256 of its leaf, and still
//! verifies its CertificateVerify with rustls.
//!
//! Stdout lines: `bound <port>` (server), `local <quinn's own transport parameters>`,
//! `params <transport parameters quinn read from the peer>`, `connected`, `alpn <protocol>`,
//! `client <sha256 of the client leaf>` (server), then `error <code hex>` when the peer closed
//! or refused the connection, or `closed` when this side closed it.
use std::any::Any;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use quinn::ConnectionError;
use quinn::crypto::rustls::{HandshakeData, QuicClientConfig, QuicServerConfig};
use quinn_proto::crypto::{
    self, ExportKeyingMaterialError, HeaderKey, KeyPair, Keys, PacketKey, Session, UnsupportedVersion,
};
use quinn_proto::transport_parameters::TransportParameters;
use quinn_proto::{ConnectError, ConnectionId, Side, TransportError};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

const ALPN: &[u8] = b"hq-interop";

/// A session that prints the transport parameters rustls decoded from the peer, once.
struct Spy {
    inner: Box<dyn Session>,
    printed: AtomicBool,
}

impl Session for Spy {
    fn initial_keys(&self, dst_cid: ConnectionId, side: Side) -> Keys {
        self.inner.initial_keys(dst_cid, side)
    }
    fn handshake_data(&self) -> Option<Box<dyn Any>> {
        self.inner.handshake_data()
    }
    fn peer_identity(&self) -> Option<Box<dyn Any>> {
        self.inner.peer_identity()
    }
    fn early_crypto(&self) -> Option<(Box<dyn HeaderKey>, Box<dyn PacketKey>)> {
        self.inner.early_crypto()
    }
    fn early_data_accepted(&self) -> Option<bool> {
        self.inner.early_data_accepted()
    }
    fn is_handshaking(&self) -> bool {
        self.inner.is_handshaking()
    }
    fn read_handshake(&mut self, buf: &[u8]) -> Result<bool, TransportError> {
        self.inner.read_handshake(buf)
    }
    fn transport_parameters(&self) -> Result<Option<TransportParameters>, TransportError> {
        let params = self.inner.transport_parameters()?;
        if let Some(p) = &params {
            if !self.printed.swap(true, Ordering::SeqCst) {
                println!("params {p:?}");
            }
        }
        Ok(params)
    }
    fn write_handshake(&mut self, buf: &mut Vec<u8>) -> Option<Keys> {
        self.inner.write_handshake(buf)
    }
    fn next_1rtt_keys(&mut self) -> Option<KeyPair<Box<dyn PacketKey>>> {
        self.inner.next_1rtt_keys()
    }
    fn is_valid_retry(&self, orig_dst_cid: ConnectionId, header: &[u8], payload: &[u8]) -> bool {
        self.inner.is_valid_retry(orig_dst_cid, header, payload)
    }
    fn export_keying_material(&self, output: &mut [u8], label: &[u8], context: &[u8]) -> Result<(), ExportKeyingMaterialError> {
        self.inner.export_keying_material(output, label, context)
    }
}

fn spy(inner: Box<dyn Session>) -> Box<dyn Session> {
    Box::new(Spy { inner, printed: AtomicBool::new(false) })
}

struct SpyClient(Arc<QuicClientConfig>);

impl crypto::ClientConfig for SpyClient {
    fn start_session(self: Arc<Self>, version: u32, server_name: &str, params: &TransportParameters) -> Result<Box<dyn Session>, ConnectError> {
        println!("local {params:?}");
        Ok(spy(self.0.clone().start_session(version, server_name, params)?))
    }
}

struct SpyServer(Arc<QuicServerConfig>);

impl crypto::ServerConfig for SpyServer {
    fn initial_keys(&self, version: u32, dst_cid: ConnectionId) -> Result<Keys, UnsupportedVersion> {
        self.0.initial_keys(version, dst_cid)
    }
    fn retry_tag(&self, version: u32, orig_dst_cid: ConnectionId, packet: &[u8]) -> [u8; 16] {
        self.0.retry_tag(version, orig_dst_cid, packet)
    }
    fn start_session(self: Arc<Self>, version: u32, params: &TransportParameters) -> Box<dyn Session> {
        println!("local {params:?}");
        spy(self.0.clone().start_session(version, params))
    }
}

/// Trust the server leaf whose SHA-256 is the pin; verify its CertificateVerify as rustls does.
#[derive(Debug)]
struct Pinned {
    pin: Vec<u8>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        if ::ring::digest::digest(&::ring::digest::SHA256, end_entity).as_ref() == self.pin.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure))
        }
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not QUIC".into()))
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn provider(suite: &str, group: &str) -> Arc<CryptoProvider> {
    let mut p = ring::default_provider();
    p.cipher_suites.retain(|s| format!("{:?}", s.suite()) == suite);
    p.kx_groups.retain(|g| format!("{:?}", g.name()) == group);
    assert!(p.cipher_suites.len() == 1 && p.kx_groups.len() == 1, "unknown suite {suite} or group {group}");
    Arc::new(p)
}

/// RFC 9001 §5.2: Initial packets always use AES-128-GCM, whatever suite the handshake picks.
fn initial_suite() -> rustls::quic::Suite {
    ring::cipher_suite::TLS13_AES_128_GCM_SHA256.tls13().unwrap().quic_suite().unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

fn report_error(e: ConnectionError) {
    match e {
        ConnectionError::ConnectionClosed(c) => println!("error {:x}", u64::from(c.error_code)),
        ConnectionError::ApplicationClosed(c) => println!("error application {:x}", c.error_code.into_inner()),
        ConnectionError::TransportError(t) => println!("error {:x}", u64::from(t.code)),
        other => println!("error {other}"),
    }
}

fn report_connected(conn: &quinn::Connection) {
    println!("connected");
    let data = conn.handshake_data().unwrap().downcast::<HandshakeData>().unwrap();
    println!("alpn {}", String::from_utf8_lossy(&data.protocol.unwrap_or_default()));
}

async fn client(args: &[String]) {
    let port: u16 = args[0].parse().unwrap();
    let p = provider(&args[2], &args[3]);
    let builder = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { pin: unhex(&args[1]), provider: p }));
    let mut tls = if args.len() > 5 {
        let cert = CertificateDer::from(std::fs::read(&args[4]).unwrap());
        let key = PrivateKeyDer::try_from(std::fs::read(&args[5]).unwrap()).unwrap();
        builder.with_client_auth_cert(vec![cert], key).unwrap()
    } else {
        builder.with_no_client_auth()
    };
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let quic = QuicClientConfig::with_initial(Arc::new(tls), initial_suite()).unwrap();
    let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(Arc::new(SpyClient(Arc::new(quic)))));
    let to = SocketAddr::from(([127, 0, 0, 1], port));
    match endpoint.connect(to, "localhost").unwrap().await {
        Err(e) => report_error(e),
        Ok(conn) => {
            report_connected(&conn);
            // RFC 8446 §4.4.2.4: a server checks the client's certificate after the client's
            // Finished, so a refused client sees its handshake complete, then the close.
            match tokio::time::timeout(Duration::from_millis(500), conn.closed()).await {
                Ok(e) => report_error(e),
                Err(_) => {
                    conn.close(0u32.into(), b"done");
                    println!("closed");
                }
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
}

async fn server(args: &[String]) {
    let cert = CertificateDer::from(std::fs::read(&args[0]).unwrap());
    let key = PrivateKeyDer::try_from(std::fs::read(&args[1]).unwrap()).unwrap();
    let mut tls = rustls::ServerConfig::builder_with_provider(provider(&args[2], &args[3]))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let quic = QuicServerConfig::with_initial(Arc::new(tls), initial_suite()).unwrap();
    let config = quinn::ServerConfig::with_crypto(Arc::new(SpyServer(Arc::new(quic))));
    let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
    println!("bound {}", endpoint.local_addr().unwrap().port());
    let incoming = endpoint.accept().await.unwrap();
    match incoming.await {
        Err(e) => report_error(e),
        Ok(conn) => {
            report_connected(&conn);
            if let Some(identity) = conn.peer_identity() {
                if let Ok(chain) = identity.downcast::<Vec<CertificateDer<'static>>>() {
                    println!("client {}", hex(::ring::digest::digest(&::ring::digest::SHA256, &chain[0]).as_ref()));
                }
            }
            conn.close(0u32.into(), b"done");
            println!("closed");
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), endpoint.wait_idle()).await;
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args[0].as_str() {
        "client" => client(&args[1..]).await,
        "server" => server(&args[1..]).await,
        other => panic!("unknown role {other}"),
    }
}
