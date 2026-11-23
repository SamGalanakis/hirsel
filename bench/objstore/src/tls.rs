use crate::socket::checked;
use rustls::client::Resumption;
use rustls::crypto::{CryptoProvider, aws_lc_rs, ring};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{
    ClientConfig, ClientConnection, Connection, ConnectionTrafficSecrets, RootCertStore,
    ServerConfig,
};
use std::io::{self, Read, Write};
use std::os::fd::RawFd;
use std::sync::Arc;

pub type AnyResult<T> = Result<T, Box<dyn std::error::Error>>;

fn provider(aws: bool) -> Arc<CryptoProvider> {
    let mut provider = if aws {
        aws_lc_rs::default_provider()
    } else {
        ring::default_provider()
    };
    provider
        .cipher_suites
        .retain(|suite| suite.suite() == rustls::CipherSuite::TLS13_AES_128_GCM_SHA256);
    Arc::new(provider)
}

pub fn server(
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
) -> AnyResult<Arc<ServerConfig>> {
    let mut cfg = ServerConfig::builder_with_provider(provider(true))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
    cfg.send_tls13_tickets = 0;
    cfg.alpn_protocols.push(b"http/1.1".to_vec());
    Ok(Arc::new(cfg))
}

pub fn client(cert: CertificateDer<'static>, aws: bool, ktls: bool) -> AnyResult<Connection> {
    let mut roots = RootCertStore::empty();
    roots.add(cert)?;
    let mut cfg = ClientConfig::builder_with_provider(provider(aws))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.resumption = Resumption::disabled();
    cfg.enable_secret_extraction = ktls;
    cfg.alpn_protocols.push(b"http/1.1".to_vec());
    Ok(ClientConnection::new(Arc::new(cfg), ServerName::try_from("localhost")?)?.into())
}

pub struct Buffered<'a, T> {
    pub session: Connection,
    pub wire: &'a mut T,
}

impl<T: Read + Write> Buffered<'_, T> {
    pub fn handshake(&mut self) -> io::Result<()> {
        while self.session.is_handshaking() {
            self.session.complete_io(self.wire)?;
        }
        self.flush()
    }
}

impl<T: Read + Write> Read for Buffered<'_, T> {
    fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.session.reader().read(dst) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                result => return result,
            }
            let n = self.session.read_tls(self.wire)?;
            self.session
                .process_new_packets()
                .map_err(io::Error::other)?;
            self.flush()?;
            if n == 0 {
                return self.session.reader().read(dst);
            }
        }
    }
}

impl<T: Write> Write for Buffered<'_, T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut done = 0;
        for record in bytes.chunks(16 * 1024) {
            self.flush()?;
            let n = self.session.writer().write(record)?;
            done += n;
            if n < record.len() {
                break;
            }
        }
        self.flush()?;
        Ok(done)
    }

    fn flush(&mut self) -> io::Result<()> {
        while self.session.wants_write() {
            self.session.write_tls(self.wire)?;
        }
        Ok(())
    }
}

fn set_socket_option(
    fd: RawFd,
    level: libc::c_int,
    name: libc::c_int,
    bytes: &[u8],
) -> io::Result<()> {
    // SAFETY: bytes is initialized for the length copied by setsockopt.
    checked(unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            bytes.as_ptr().cast(),
            bytes.len() as libc::socklen_t,
        )
    })?;
    Ok(())
}

pub fn install_ktls(fd: RawFd, session: Connection) -> AnyResult<()> {
    let secrets = session.dangerous_extract_secrets()?;
    set_socket_option(fd, libc::IPPROTO_TCP, libc::TCP_ULP, b"tls\0")?;
    for (direction, (seq, secret)) in [(libc::TLS_TX, secrets.tx), (libc::TLS_RX, secrets.rx)] {
        let ConnectionTrafficSecrets::Aes128Gcm { key, iv } = secret else {
            return Err("kTLS requires TLS 1.3 AES-128-GCM".into());
        };
        let iv = iv.as_ref();
        // Linux tls12_crypto_info_aes_gcm_128 ABI, also used for TLS 1.3.
        let mut data = [0; 40];
        data[..2].copy_from_slice(&0x0304u16.to_ne_bytes());
        data[2..4].copy_from_slice(&51u16.to_ne_bytes());
        data[4..12].copy_from_slice(&iv[4..]);
        data[12..28].copy_from_slice(key.as_ref());
        data[28..32].copy_from_slice(&iv[..4]);
        data[32..].copy_from_slice(&seq.to_be_bytes());
        set_socket_option(fd, libc::SOL_TLS, direction, &data)?;
    }
    Ok(())
}
