//! Git's pkt-line framing (gitprotocol-common): four hex digits of length, then the payload;
//! `0000` is a flush, `0001` a delimiter and `0002` a response end.

use super::{Failure, quote_text};
use std::io::Read;

pub(crate) enum Packet {
    Data(Vec<u8>),
    Flush,
    Delim,
    End,
}

pub(crate) fn line(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(format!("{:04x}", text.len() + 5).as_bytes());
    out.extend_from_slice(text.as_bytes());
    out.push(b'\n');
}

pub(crate) fn flush(out: &mut Vec<u8>) {
    out.extend_from_slice(b"0000");
}

pub(crate) fn delim(out: &mut Vec<u8>) {
    out.extend_from_slice(b"0001");
}

/// Reads packets from a response body. `bad` words a framing failure.
pub(crate) struct Reader<R> {
    inner: R,
    bad: fn() -> Failure,
}

impl<R: Read> Reader<R> {
    pub(crate) fn new(inner: R, bad: fn() -> Failure) -> Self {
        Self { inner, bad }
    }

    fn exact(&mut self, buf: &mut [u8]) -> Result<bool, Failure> {
        let mut read = 0;
        while read < buf.len() {
            match self.inner.read(&mut buf[read..]) {
                Ok(0) if read == 0 => return Ok(false),
                Ok(0) => return Err((self.bad)()),
                Ok(n) => read += n,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(super::transport::io_failure(error)),
            }
        }
        Ok(true)
    }

    /// The next packet, or `None` at a clean end of the body.
    pub(crate) fn next(&mut self) -> Result<Option<Packet>, Failure> {
        let mut head = [0u8; 4];
        if !self.exact(&mut head)? {
            return Ok(None);
        }
        let length = std::str::from_utf8(&head)
            .ok()
            .and_then(|text| usize::from_str_radix(text, 16).ok())
            .ok_or_else(self.bad)?;
        match length {
            0 => return Ok(Some(Packet::Flush)),
            1 => return Ok(Some(Packet::Delim)),
            2 => return Ok(Some(Packet::End)),
            3 => return Err((self.bad)()),
            _ => {}
        }
        let mut data = vec![0u8; length - 4];
        if !self.exact(&mut data)? {
            return Err((self.bad)());
        }
        if let Some(text) = data.strip_prefix(b"ERR ") {
            return Err(server_said(text));
        }
        Ok(Some(Packet::Data(data)))
    }
}

/// A refusal carrying a server's text, `not our ref` mapped to its own cause by the caller.
pub(crate) fn server_said(text: &[u8]) -> Failure {
    let text = text.strip_suffix(b"\n").unwrap_or(text);
    Failure::Fetch(format!("the server said: {}", quote_text(text)))
}

/// A data line without its trailing line feed.
pub(crate) fn text(data: &[u8]) -> &[u8] {
    data.strip_suffix(b"\n").unwrap_or(data)
}
