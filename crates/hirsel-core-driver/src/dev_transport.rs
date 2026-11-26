use crate::call::websocket::{
    MESSAGE_LIMIT, MESSAGE_LIMIT_MIB, UPGRADE_HEAD_LIMIT, upgrade_head_end,
};
use crate::call::{base64, random, sha1};
use std::{
    io::{self, Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

use super::{Error, STOP};

const PROTOCOL: &str = "hirsel.4";
const HANDSHAKE: Duration = Duration::from_secs(5);

struct Reader {
    stream: TcpStream,
    buffered: Vec<u8>,
}

fn accept_key(key: &str) -> String {
    base64(&sha1(
        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
    ))
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.split("\r\n").skip(1).find_map(|line| {
        let (part, value) = line.split_once(':')?;
        part.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

impl Reader {
    fn new(stream: TcpStream) -> Result<Self, Error> {
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(HANDSHAKE))?;
        stream.set_write_timeout(Some(HANDSHAKE))?;
        Ok(Self {
            stream,
            buffered: Vec::new(),
        })
    }

    fn fill(&mut self) -> Result<(), Error> {
        let mut data = [0; 8192];
        let count = self.stream.read(&mut data)?;
        if count == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        self.buffered.extend_from_slice(&data[..count]);
        Ok(())
    }

    fn head(&mut self) -> Result<String, Error> {
        loop {
            if let Some(end) = upgrade_head_end(&self.buffered) {
                return Ok(String::from_utf8(self.buffered.drain(..end).collect())?);
            }
            if self.buffered.len() >= UPGRADE_HEAD_LIMIT {
                return Err(
                    format!("WebSocket request head exceeds {UPGRADE_HEAD_LIMIT} bytes").into(),
                );
            }
            self.fill()?;
        }
    }

    fn need(&mut self, size: usize) -> Result<(), Error> {
        while self.buffered.len() < size {
            self.fill()?;
        }
        Ok(())
    }

    fn message(&mut self, other: &Sender) -> Result<Option<Vec<u8>>, Error> {
        let mut partial: Option<Vec<u8>> = None;
        loop {
            self.need(2)?;
            let (op, len) = (self.buffered[0], self.buffered[1]);
            let fin = op & 128 != 0;
            let control = op & 8 != 0;
            if op & 0x70 != 0
                || (len & 128 != 0) == other.mask
                || (control && (!fin || len & 127 > 125))
            {
                return Err("malformed WebSocket frame".into());
            }
            let (mut start, size) = match len & 127 {
                126 => {
                    self.need(4)?;
                    (
                        4,
                        u64::from(u16::from_be_bytes(self.buffered[2..4].try_into()?)),
                    )
                }
                127 => {
                    self.need(10)?;
                    (10, u64::from_be_bytes(self.buffered[2..10].try_into()?))
                }
                size => (2, u64::from(size)),
            };
            if size > (MESSAGE_LIMIT - partial.as_ref().map_or(0, Vec::len)) as u64 {
                return Err(format!("WebSocket message exceeds {MESSAGE_LIMIT_MIB} MiB").into());
            }
            let size = size as usize;
            let mask = if len & 128 != 0 {
                self.need(start + 4)?;
                let mask: [u8; 4] = self.buffered[start..start + 4].try_into()?;
                start += 4;
                Some(mask)
            } else {
                None
            };
            self.need(start + size)?;
            let mut payload: Vec<_> = self.buffered.drain(..start + size).skip(start).collect();
            if let Some(mask) = mask {
                for (i, byte) in payload.iter_mut().enumerate() {
                    *byte ^= mask[i % 4];
                }
            }
            match op & 15 {
                0 if let Some(message) = &mut partial => {
                    message.append(&mut payload);
                    if fin {
                        return Ok(partial);
                    }
                }
                2 if partial.is_none() => {
                    if fin {
                        return Ok(Some(payload));
                    }
                    partial = Some(payload);
                }
                8 => {
                    other.frame(8, &payload)?;
                    return Ok(None);
                }
                9 => other.frame(10, &payload)?,
                10 => {}
                _ => return Err("expected a binary WebSocket message".into()),
            }
        }
    }
}

struct Sender {
    stream: Mutex<TcpStream>,
    mask: bool,
}

impl Sender {
    fn frame(&self, opcode: u8, payload: &[u8]) -> Result<(), Error> {
        let flags = if self.mask { 128 } else { 0 };
        let mut head = vec![128 | opcode];
        match payload.len() {
            len if len < 126 => head.push(flags | len as u8),
            len if len <= u16::MAX as usize => {
                head.push(flags | 126);
                head.extend_from_slice(&(len as u16).to_be_bytes());
            }
            len => {
                head.push(flags | 127);
                head.extend_from_slice(&(len as u64).to_be_bytes());
            }
        }
        let mut writer = self.stream.lock().unwrap();
        if self.mask {
            let mask = random::<4>()?;
            head.extend_from_slice(&mask);
            head.extend(
                payload
                    .iter()
                    .enumerate()
                    .map(|(i, byte)| byte ^ mask[i % 4]),
            );
            writer.write_all(&head)?;
        } else {
            writer.write_all(&head)?;
            writer.write_all(payload)?;
        }
        Ok(())
    }
}

pub(super) struct Socket {
    sender: Arc<Sender>,
    pub(super) incoming: mpsc::Receiver<Vec<u8>>,
    reader: Option<JoinHandle<()>>,
}

impl Socket {
    fn new(mut reader: Reader, mask: bool) -> Result<Self, Error> {
        reader.stream.set_read_timeout(None)?;
        let sender = Arc::new(Sender {
            stream: Mutex::new(reader.stream.try_clone()?),
            mask,
        });
        let output = Arc::clone(&sender);
        let (messages, incoming) = mpsc::sync_channel(1);
        let task = thread::Builder::new().spawn(move || {
            while let Ok(Some(message)) = reader.message(&output) {
                if messages.send(message).is_err() {
                    break;
                }
            }
        })?;
        Ok(Self {
            sender,
            incoming,
            reader: Some(task),
        })
    }

    pub(super) fn accept(
        stream: TcpStream,
        backend: SocketAddr,
    ) -> Result<Option<(Self, String)>, Error> {
        let mut reader = Reader::new(stream)?;
        let request = reader.head()?;
        if !header(&request, "Upgrade").is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        {
            relay_http(reader, backend, request)?;
            return Ok(None);
        }
        let path = request
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|line| line.strip_suffix(" HTTP/1.1"))
            .filter(|path| path.starts_with('/') && !path.contains(char::is_whitespace))
            .ok_or("malformed WebSocket request")?;
        let host = header(&request, "Host").ok_or("missing Host")?;
        if let Some(origin) = header(&request, "Origin")
            && origin.strip_prefix("http://") != Some(host)
        {
            return Err("WebSocket Origin must match the development server".into());
        }
        if !header(&request, "Sec-WebSocket-Protocol")
            .is_some_and(|value| value.split(',').any(|part| part.trim() == PROTOCOL))
            || header(&request, "Sec-WebSocket-Version") != Some("13")
            || !header(&request, "Connection").is_some_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case("upgrade"))
            })
        {
            return Err("expected a hirsel.4 WebSocket upgrade".into());
        }
        let key = header(&request, "Sec-WebSocket-Key").ok_or("missing WebSocket key")?;
        write!(
            reader.stream,
            "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {}\r\nSec-WebSocket-Protocol: {PROTOCOL}\r\n\r\n",
            accept_key(key)
        )?;
        Ok(Some((Self::new(reader, false)?, path.to_owned())))
    }

    pub(super) fn open(backend: SocketAddr, path: &str) -> Result<Self, Error> {
        let mut reader = Reader::new(TcpStream::connect_timeout(&backend, HANDSHAKE)?)?;
        let key = base64(&random::<16>()?);
        write!(
            reader.stream,
            "GET {path} HTTP/1.1\r\nHost: {backend}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: {PROTOCOL}\r\n\r\n"
        )?;
        let response = reader.head()?;
        if !response.starts_with("HTTP/1.1 101 ")
            || header(&response, "Sec-WebSocket-Accept") != Some(accept_key(&key).as_str())
            || header(&response, "Sec-WebSocket-Protocol") != Some(PROTOCOL)
        {
            return Err("the place refused the hirsel.4 WebSocket upgrade".into());
        }
        Self::new(reader, true)
    }

    pub(super) fn send(&self, raw: &[u8]) -> Result<(), Error> {
        self.sender.frame(2, raw)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = self.sender.stream.lock().unwrap().shutdown(Shutdown::Both);
        // An unread message must release a reader blocked on a full channel.
        drop(std::mem::replace(&mut self.incoming, mpsc::channel().1));
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn relay_http(mut client: Reader, addr: SocketAddr, request: String) -> Result<(), Error> {
    let mut server = TcpStream::connect_timeout(&addr, HANDSHAKE)?;
    server.set_read_timeout(Some(HANDSHAKE))?;
    server.set_write_timeout(Some(HANDSHAKE))?;
    for line in request.split("\r\n").take_while(|line| !line.is_empty()) {
        if !line
            .split_once(':')
            .is_some_and(|(name, _)| name.eq_ignore_ascii_case("Connection"))
        {
            write!(server, "{line}\r\n")?;
        }
    }
    server.write_all(b"Connection: close\r\n\r\n")?;
    server.write_all(&client.buffered)?;
    let upload = server.try_clone()?;
    let mut download = client.stream.try_clone()?;
    thread::scope(|scope| {
        scope.spawn(|| {
            let _ = io::copy(&mut client.stream, &mut &upload);
            let _ = upload.shutdown(Shutdown::Write);
        });
        let mut bytes = [0; 8192];
        while !STOP.load(std::sync::atomic::Ordering::Relaxed) {
            let count = server.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            download.write_all(&bytes[..count])?;
        }
        let _ = download.shutdown(Shutdown::Both);
        Ok(())
    })
}
