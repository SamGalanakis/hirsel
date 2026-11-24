//! dimpl peer for `interop.py`: one DTLS 1.2 endpoint on loopback UDP with the line
//! contract `interop.py` reads from `openssl s_server`/`s_client`. Built against a dimpl
//! clone at e341a90 in /tmp; never vendored.
//!
//! Usage: interop_dimpl client|server PORT CERT.der KEY.der
//! A server binds 127.0.0.1:PORT; a client binds an ephemeral port and sends to PORT.
//! Stdout: `connected`, `cipher <suite>`, `profile <name>`, `keymat <hex>`,
//! `peercert <hex>`, `data <hex>` for each application record, `closed`.
//! Stdin: each line is sent as one application record, newline included.

use std::io::{BufRead, Write};
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use dimpl::crypto::Dtls12CipherSuite;
use dimpl::{Config, Dtls, DtlsCertificate, Output, SrtpProfile};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn say(line: String) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}").unwrap();
    out.flush().unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let client = match args[1].as_str() {
        "client" => true,
        "server" => false,
        other => panic!("role {other}"),
    };
    let port: u16 = args[2].parse().unwrap();
    let certificate = DtlsCertificate {
        certificate: std::fs::read(&args[3]).unwrap(),
        private_key: std::fs::read(&args[4]).unwrap(),
    };
    let config = Config::builder()
        .dtls12_cipher_suites(&[Dtls12CipherSuite::ECDHE_ECDSA_AES128_GCM_SHA256])
        .srtp_profiles(&[SrtpProfile::AEAD_AES_128_GCM])
        .build()
        .unwrap();
    let peer: SocketAddr = ([127, 0, 0, 1], port).into();
    let socket = if client {
        UdpSocket::bind("127.0.0.1:0").unwrap()
    } else {
        UdpSocket::bind(peer).unwrap()
    };
    say(format!("bound {}", socket.local_addr().unwrap().port()));
    let mut dtls = Dtls::new_12(Arc::new(config), certificate, Instant::now());
    dtls.set_active(client);
    let mut to = if client { Some(peer) } else { None };

    let (lines, input) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().split(b'\n') {
            let mut line = line.unwrap();
            line.push(b'\n');
            if lines.send(line).is_err() {
                return;
            }
        }
    });

    let end = Instant::now() + Duration::from_secs(60);
    let mut buf = vec![0u8; 4096];
    let mut datagram = vec![0u8; 65536];
    let mut connected = false;
    let mut wake;
    loop {
        loop {
            match dtls.poll_output(&mut buf) {
                Output::Packet(p) => {
                    if let Some(a) = to {
                        socket.send_to(p, a).unwrap();
                    }
                }
                Output::BufferTooSmall { needed } => buf.resize(needed, 0),
                Output::Timeout(t) => {
                    wake = t;
                    break;
                }
                Output::Connected => {
                    connected = true;
                    say("connected".into());
                }
                Output::PeerCert(der) => say(format!("peercert {}", hex(der))),
                Output::KeyingMaterial(km, profile) => {
                    say(format!("profile {profile}"));
                    say(format!("keymat {}", hex(&km)));
                }
                Output::ApplicationData(data) => say(format!("data {}", hex(data))),
                Output::CloseNotify => {
                    say("closed".into());
                    return;
                }
                _ => {}
            }
        }
        let now = Instant::now();
        if now >= end {
            say("timeout".into());
            std::process::exit(2);
        }
        if connected {
            while let Ok(line) = input.try_recv() {
                dtls.send_application_data(&line).unwrap();
            }
        }
        let wait = wake
            .saturating_duration_since(now)
            .clamp(Duration::from_millis(1), Duration::from_millis(20));
        socket.set_read_timeout(Some(wait)).unwrap();
        match socket.recv_from(&mut datagram) {
            Ok((n, from)) => {
                to.get_or_insert(from);
                dtls.handle_timeout(Instant::now()).unwrap();
                dtls.handle_packet(&datagram[..n]).unwrap();
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                dtls.handle_timeout(Instant::now()).unwrap();
            }
            Err(e) => panic!("{e}"),
        }
    }
}
