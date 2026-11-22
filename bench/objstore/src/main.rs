mod socket;
mod tls;

use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use socket::{CHUNK, Epoll, Ring};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tls::{AnyResult, Buffered};

const BODY: usize = 8 * 1024 * 1024;
const BYTE: u8 = 0xa5;
const PUT: &[u8] = b"PUT /object HTTP/1.1\r\nHost: localhost\r\nContent-Length: 8388608\r\n\r\n";
const GET: &[u8] = b"GET /object HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n";
const STORED: &[u8] = b"HTTP/1.1 204 No Content\r\n\r\n";
const OBJECT: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 8388608\r\n\r\n";
const MODES: &[&str] = &[
    "epoll",
    "ring",
    "uring",
    "uring-ring",
    "aws-lc",
    "ktls",
    "ktls-send",
];

struct Buffers {
    tx: Box<[u8]>,
    rx: Box<[u8]>,
}

impl Buffers {
    fn new() -> Self {
        Self {
            tx: vec![BYTE; CHUNK].into_boxed_slice(),
            rx: vec![0; CHUNK].into_boxed_slice(),
        }
    }
}

fn head(wire: &mut impl Read, header: &mut [u8]) -> io::Result<Option<(usize, usize)>> {
    let mut filled = 0;
    loop {
        if filled == header.len() {
            return Err(io::Error::other("HTTP head exceeds benchmark limit"));
        }
        let n = match wire.read(&mut header[filled..]) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && filled == 0 => return Ok(None),
            result => result?,
        };
        if n == 0 {
            return if filled == 0 {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        // Only search the new bytes and up to three bytes from the preceding read.
        let search = filled.saturating_sub(3);
        filled += n;
        if let Some(end) = header[search..filled]
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
        {
            return Ok(Some((search + end + 4, filled)));
        }
    }
}

fn check_bytes(bytes: &[u8], verify: bool) -> io::Result<()> {
    if verify && bytes.iter().any(|&byte| byte != BYTE) {
        return Err(io::Error::other("incorrect object payload"));
    }
    Ok(())
}

fn read_body(
    wire: &mut impl Read,
    buf: &mut [u8],
    initial: &[u8],
    total: usize,
    verify: bool,
) -> io::Result<()> {
    check_bytes(initial, verify)?;
    let mut remaining = total
        .checked_sub(initial.len())
        .ok_or_else(|| io::Error::other("extra body bytes"))?;
    while remaining != 0 {
        let count = remaining.min(buf.len());
        wire.read_exact(&mut buf[..count])?;
        check_bytes(&buf[..count], verify)?;
        remaining -= count;
    }
    Ok(())
}

fn write_body(wire: &mut impl Write, buf: &[u8]) -> io::Result<()> {
    for _ in 0..BODY / CHUNK {
        wire.write_all(buf)?;
    }
    Ok(())
}

fn serve_connection(
    wire: &mut (impl Read + Write),
    buf: &mut Buffers,
    verify: bool,
) -> io::Result<()> {
    let mut header = [0; 4096];
    while let Some((end, filled)) = head(wire, &mut header)? {
        let request = &header[..end];
        let tail = &header[end..filled];
        if request.starts_with(b"PUT /object HTTP/1.1\r\n")
            && request
                .windows(25)
                .any(|w| w.eq_ignore_ascii_case(b"Content-Length: 8388608\r\n"))
        {
            read_body(wire, &mut buf.rx, tail, BODY, verify)?;
            wire.write_all(STORED)?;
        } else if request.starts_with(b"GET /object HTTP/1.1\r\n") && tail.is_empty() {
            wire.write_all(OBJECT)?;
            write_body(wire, &buf.tx)?;
        } else {
            return Err(io::Error::other("unexpected benchmark request"));
        }
    }
    Ok(())
}

fn serve(listener: TcpListener, config: Option<Arc<ServerConfig>>, verify: bool) -> AnyResult<()> {
    let mut buf = Buffers::new();
    println!("{}", listener.local_addr()?.port());
    for stream in listener.incoming() {
        let mut stream = stream?;
        stream.set_nodelay(true)?;
        let result = if let Some(ref config) = config {
            let mut session = ServerConnection::new(config.clone())?;
            session.set_buffer_limit(None);
            session.complete_io(&mut stream).and_then(|_| {
                let mut wire = StreamOwned::new(session, stream);
                serve_connection(&mut wire, &mut buf, verify)
            })
        } else {
            serve_connection(&mut stream, &mut buf, verify)
        };
        if let Err(e) = result {
            eprintln!("server connection: {e}");
        }
    }
    Ok(())
}

fn request(
    wire: &mut (impl Read + Write),
    buf: &mut Buffers,
    put: bool,
    verify: bool,
) -> io::Result<()> {
    if put {
        wire.write_all(PUT)?;
        write_body(wire, &buf.tx)?;
    } else {
        wire.write_all(GET)?;
    }
    let mut header = [0; 4096];
    let (end, filled) = head(wire, &mut header)?.ok_or(io::ErrorKind::UnexpectedEof)?;
    let expected = if put { STORED } else { OBJECT };
    if &header[..end] != expected {
        return Err(io::Error::other("unexpected benchmark response"));
    }
    read_body(
        wire,
        &mut buf.rx,
        &header[end..filled],
        if put { 0 } else { BODY },
        verify,
    )
}

fn requests(
    wire: &mut (impl Read + Write),
    buf: &mut Buffers,
    put: bool,
    count: usize,
    verify: bool,
) -> io::Result<()> {
    request(wire, buf, put, verify)?;
    println!("READY");
    io::stdin().read_exact(&mut [0])?;
    let start = Instant::now();
    for _ in 0..count {
        request(wire, buf, put, verify)?;
    }
    println!("DONE {}", start.elapsed().as_nanos());
    io::stdin().read_exact(&mut [0])?;
    Ok(())
}

fn run_client(
    stream: TcpStream,
    cert: Option<CertificateDer<'static>>,
    mode: &str,
    put: bool,
    count: usize,
    verify: bool,
) -> AnyResult<(u64, u64)> {
    stream.set_nodelay(true)?;
    let mut buf = Buffers::new();
    let mut epoll = Epoll::new(stream)?;
    let ktls = matches!(mode, "ktls" | "ktls-send");
    let mut session = cert
        .map(|c| tls::client(c, mode == "aws-lc", ktls))
        .transpose()?;
    if let Some(connection) = session.take() {
        let mut wire = Buffered {
            session: connection,
            wire: &mut epoll,
        };
        wire.handshake()?;
        if ktls {
            tls::install_ktls(wire.wire.stream.as_raw_fd(), wire.session)
                .map_err(|error| format!("kTLS setup: {error}"))?;
        } else {
            session = Some(wire.session);
        }
    }
    if ktls || matches!(mode, "uring" | "uring-ring") {
        // SAFETY: the ring is dropped before buf and neither box is resized or
        // moved out while the registered addresses are in use.
        let mut ring =
            unsafe { Ring::new(epoll.stream, &mut buf.rx, &mut buf.tx, mode != "ktls-send")? };
        match session {
            Some(session) => requests(
                &mut Buffered {
                    session,
                    wire: &mut ring,
                },
                &mut buf,
                put,
                count,
                verify,
            )?,
            None => requests(&mut ring, &mut buf, put, count, verify)?,
        };
        Ok((ring.zc_bytes, ring.copied_bytes))
    } else {
        match session {
            Some(session) => requests(
                &mut Buffered {
                    session,
                    wire: &mut epoll,
                },
                &mut buf,
                put,
                count,
                verify,
            )?,
            None => requests(&mut epoll, &mut buf, put, count, verify)?,
        };
        Ok((0, 0))
    }
}

fn read_cert(dir: &Path) -> AnyResult<CertificateDer<'static>> {
    Ok(fs::read(dir.join("cert.der"))?.into())
}

fn main() -> AnyResult<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["cert", dir] => {
            let dir = Path::new(dir);
            fs::create_dir(dir)?;
            let rcgen::CertifiedKey { cert, signing_key } =
                rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
            fs::write(dir.join("cert.der"), cert.der())?;
            fs::write(dir.join("cert.pem"), cert.pem())?;
            fs::write(dir.join("key.der"), signing_key.serialize_der())?;
        }
        ["serve", mode @ ("plain" | "tls"), dir, measure @ ("verify" | "measure")] => {
            let config = if *mode == "tls" {
                let dir = Path::new(dir);
                Some(tls::server(read_cert(dir)?, PrivatePkcs8KeyDer::from(fs::read(dir.join("key.der"))?).into())?)
            } else {
                None
            };
            serve(TcpListener::bind(("127.0.0.1", 0))?, config, *measure == "verify")?;
        }
        ["client", mode, method @ ("put" | "get"), port, dir, count, measure @ ("verify" | "measure")]
            if MODES.contains(mode) =>
        {
            let cert = if matches!(*mode, "epoll" | "uring") { None } else { Some(read_cert(Path::new(dir))?) };
            let stream = TcpStream::connect(("127.0.0.1", port.parse()?))?;
            let (zc, copied) = run_client(stream, cert, mode, *method == "put", count.parse()?, *measure == "verify")?;
            println!("SEND_ZC {zc} {copied}");
        }
        _ => return Err("expected cert DIR, serve plain|tls DIR verify|measure, or client epoll|ring|uring|uring-ring|aws-lc|ktls|ktls-send put|get PORT DIR COUNT verify|measure".into()),
    }
    Ok(())
}
