//! RFC 6455 client-role WebSocket framing over the in-process `std.wsock.Local` byte stream.
//!
//! `std.wsock.local_server` runs the exact same RFC 6455 decoder/encoder as TCP:
//! client -> server frames MUST be masked with 4 fresh entropy bytes; server -> client
//! frames are unmasked.
use std::collections::VecDeque;

pub const OP_CONT: u8 = 0x0;
pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

/// An inbound server WebSocket event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WsEvent {
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Close(u16),
    ProtocolError(u16),
}

fn fill_mask() -> [u8; 4] {
    let mut mask = [0u8; 4];
    let mut off = 0;
    while off < 4 {
        // SAFETY: mask slice is valid writable memory of length 4 - off.
        let n = unsafe { crate::host::sys::random(mask.as_mut_ptr().add(off), 4 - off, 0) };
        if n > 0 {
            off += n as usize;
        } else {
            // Fallback only if OS entropy fails momentarily; keep advancing.
            mask[off] = 0xA5 ^ (off as u8);
            off += 1;
        }
    }
    mask
}

/// Encode one client-role (masked) WebSocket frame.
pub fn encode_client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mask = fill_mask();
    let len = payload.len();
    let header_len = if len <= 125 {
        6
    } else if len <= 65535 {
        8
    } else {
        14
    };
    let mut out = Vec::with_capacity(header_len + len);
    out.push(0x80 | (opcode & 0x0f));
    if len <= 125 {
        out.push(0x80 | (len as u8));
    } else if len <= 65535 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(&mask);
    for (i, &b) in payload.iter().enumerate() {
        out.push(b ^ mask[i & 3]);
    }
    out
}

/// Encode a client Close frame with a 2-byte big-endian status code.
pub fn encode_close(code: u16) -> Vec<u8> {
    encode_client_frame(OP_CLOSE, &code.to_be_bytes())
}

/// Encode a client Pong frame echoing `payload`.
pub fn encode_pong(payload: &[u8]) -> Vec<u8> {
    encode_client_frame(OP_PONG, payload)
}

/// Streaming server-to-client WebSocket frame decoder.
pub struct WsDecoder {
    max_frame: usize,
    buf: Vec<u8>,
    cursor: usize,
    frag_opcode: Option<u8>,
    frag_data: Vec<u8>,
    events: VecDeque<WsEvent>,
    failed: bool,
}

impl WsDecoder {
    pub fn new(max_frame: usize) -> Self {
        Self {
            max_frame,
            buf: Vec::new(),
            cursor: 0,
            frag_opcode: None,
            frag_data: Vec::new(),
            events: VecDeque::new(),
            failed: false,
        }
    }

    pub fn set_max_frame(&mut self, max_frame: usize) {
        self.max_frame = max_frame;
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.failed || bytes.is_empty() {
            return;
        }
        if self.cursor > 0 && self.cursor == self.buf.len() {
            self.buf.clear();
            self.cursor = 0;
        } else if self.cursor > 4096 {
            self.buf.drain(..self.cursor);
            self.cursor = 0;
        }
        self.buf.extend_from_slice(bytes);
        self.decode_available();
    }

    pub fn pop(&mut self) -> Option<WsEvent> {
        self.events.pop_front()
    }

    fn fail(&mut self, code: u16) {
        self.failed = true;
        self.events.push_back(WsEvent::ProtocolError(code));
    }

    fn decode_available(&mut self) {
        loop {
            let rem = &self.buf[self.cursor..];
            if rem.len() < 2 {
                return;
            }
            let b0 = rem[0];
            let b1 = rem[1];
            let fin = (b0 & 0x80) != 0;
            let rsv = b0 & 0x70;
            let opcode = b0 & 0x0f;
            let masked = (b1 & 0x80) != 0;
            let len7 = (b1 & 0x7f) as usize;
            if rsv != 0 || masked {
                self.fail(1002);
                return;
            }
            let (payload_len, hdr_len) = match len7 {
                0..=125 => (len7, 2),
                126 => {
                    if rem.len() < 4 {
                        return;
                    }
                    let n = u16::from_be_bytes([rem[2], rem[3]]) as usize;
                    if n < 126 {
                        self.fail(1002);
                        return;
                    }
                    (n, 4)
                }
                127 => {
                    if rem.len() < 10 {
                        return;
                    }
                    let raw = u64::from_be_bytes(rem[2..10].try_into().unwrap());
                    if raw >> 63 != 0 || raw < 65536 {
                        self.fail(1002);
                        return;
                    }
                    let Ok(n) = usize::try_from(raw) else {
                        self.fail(1009);
                        return;
                    };
                    (n, 10)
                }
                _ => unreachable!(),
            };
            let is_control = (opcode & 0x08) != 0;
            if is_control && (!fin || payload_len > 125) {
                self.fail(1002);
                return;
            }
            if !is_control && payload_len > self.max_frame {
                self.fail(1009);
                return;
            }
            if rem.len() < hdr_len + payload_len {
                return;
            }
            let payload = rem[hdr_len..hdr_len + payload_len].to_vec();
            self.cursor += hdr_len + payload_len;
            match opcode {
                OP_PING => self.events.push_back(WsEvent::Ping(payload)),
                OP_PONG => self.events.push_back(WsEvent::Pong(payload)),
                OP_CLOSE => {
                    let code = if payload.len() >= 2 {
                        u16::from_be_bytes([payload[0], payload[1]])
                    } else if payload.is_empty() {
                        1005
                    } else {
                        1002
                    };
                    self.events.push_back(WsEvent::Close(code));
                }
                OP_BINARY => {
                    if self.frag_opcode.is_some() {
                        self.fail(1002);
                        return;
                    }
                    if fin {
                        self.events.push_back(WsEvent::Binary(payload));
                    } else {
                        self.frag_opcode = Some(OP_BINARY);
                        self.frag_data = payload;
                    }
                }
                OP_CONT => {
                    let Some(op) = self.frag_opcode else {
                        self.fail(1002);
                        return;
                    };
                    if self.frag_data.len() + payload.len() > self.max_frame {
                        self.fail(1009);
                        return;
                    }
                    self.frag_data.extend_from_slice(&payload);
                    if fin {
                        self.frag_opcode = None;
                        let full = std::mem::take(&mut self.frag_data);
                        if op == OP_BINARY {
                            self.events.push_back(WsEvent::Binary(full));
                        }
                    }
                }
                OP_TEXT => {
                    self.fail(1003);
                    return;
                }
                _ => {
                    self.fail(1002);
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_masked_client_and_unmasked_server_frames() {
        let payload = vec![0x42u8; 300];
        let encoded = encode_client_frame(OP_BINARY, &payload);
        assert_eq!(encoded[0], 0x82);
        assert_eq!(encoded[1], 0x80 | 126);
        let mask = &encoded[4..8];
        let mut unmasked = Vec::new();
        for (i, &b) in encoded[8..].iter().enumerate() {
            unmasked.push(b ^ mask[i & 3]);
        }
        assert_eq!(unmasked, payload);

        // Unmasked server binary + close frame.
        let mut server = vec![0x82, 126, 0x01, 0x2c];
        server.extend_from_slice(&payload);
        server.extend_from_slice(&[0x88, 0x02, 0x03, 0xe8]); // Close 1000
        let mut dec = WsDecoder::new(262_144);
        dec.push(&server[..7]);
        assert_eq!(dec.pop(), None);
        dec.push(&server[7..]);
        assert_eq!(dec.pop(), Some(WsEvent::Binary(payload)));
        assert_eq!(dec.pop(), Some(WsEvent::Close(1000)));
    }
}
