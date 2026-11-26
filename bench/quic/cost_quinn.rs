//! The quinn side of the QUIC cost bench (bench/quic/cost.py): the receiver both senders
//! send to, and the quinn sender the Hirsel sender is compared with.
//!
//! Usage: cost_quinn receive CERT_DER KEY_DER
//!        cost_quinn send PORT PIN BYTES
//!
//! Both offer only TLS13_AES_128_GCM_SHA256 and X25519 and keep every datagram at 1200
//! bytes (no MTU discovery), as the Hirsel sender does; quinn-udp sends GSO trains on Linux.
//! The receiver accepts one connection and one unidirectional stream, reads it to the end
//! without copying, prints `received <bytes> <datagrams>` (quinn's count of UDP datagrams
//! it received on the connection), and waits for the sender to close. The sender opens one
//! unidirectional stream, prints READY, queues BYTES as shared 64 KiB chunks with FIN, waits
//! until the receiver has acknowledged all of it, and prints `DONE <bytes>`.
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

const ALPN: &[u8] = b"hq-interop";
const MTU: u16 = 1200;
const CHUNK: usize = 65536;

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

fn provider() -> Arc<CryptoProvider> {
    let mut p = ring::default_provider();
    p.cipher_suites.retain(|s| s.suite() == rustls::CipherSuite::TLS13_AES_128_GCM_SHA256);
    p.kx_groups.retain(|g| g.name() == rustls::NamedGroup::X25519);
    Arc::new(p)
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    t.initial_mtu(MTU).min_mtu(MTU).mtu_discovery_config(None);
    Arc::new(t)
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

async fn receive(args: &[String]) {
    let cert = CertificateDer::from(std::fs::read(&args[0]).unwrap());
    let key = PrivateKeyDer::try_from(std::fs::read(&args[1]).unwrap()).unwrap();
    let mut tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls).unwrap()));
    config.transport_config(transport());
    let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
    println!("bound {}", endpoint.local_addr().unwrap().port());
    let conn = endpoint.accept().await.unwrap().await.unwrap();
    let mut recv = conn.accept_uni().await.unwrap();
    let mut bytes = 0usize;
    while let Some(chunk) = recv.read_chunk(usize::MAX, true).await.unwrap() {
        bytes += chunk.bytes.len();
    }
    println!("received {bytes} {}", conn.stats().udp_rx.datagrams);
    conn.closed().await;
    endpoint.wait_idle().await;
}

async fn send(args: &[String]) {
    let port: u16 = args[0].parse().unwrap();
    let total: usize = args[2].parse().unwrap();
    assert!(total > 0 && total % CHUNK == 0, "BYTES must be a positive multiple of {CHUNK}");
    let p = provider();
    let mut tls = rustls::ClientConfig::builder_with_provider(p.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { pin: unhex(&args[1]), provider: p }))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls).unwrap()));
    config.transport_config(transport());
    let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let to = SocketAddr::from(([127, 0, 0, 1], port));
    let conn = endpoint.connect_with(config, to, "localhost").unwrap().await.unwrap();
    let mut stream = conn.open_uni().await.unwrap();
    let chunk = Bytes::from((0..CHUNK).map(|i| (i.wrapping_mul(31).wrapping_add(7) & 0xff) as u8).collect::<Vec<u8>>());
    println!("READY");
    for _ in 0..total / CHUNK {
        stream.write_chunk(chunk.clone()).await.unwrap();
    }
    stream.finish().unwrap();
    stream.stopped().await.unwrap();
    println!("DONE {total}");
    conn.close(0u32.into(), b"");
    endpoint.wait_idle().await;
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args[0].as_str() {
        "receive" => receive(&args[1..]).await,
        "send" => send(&args[1..]).await,
        other => panic!("unknown role {other}"),
    }
}
