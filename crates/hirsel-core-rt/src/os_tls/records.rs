use super::*;
use ring::aead::{self, Aad, LessSafeKey, UnboundKey};
use rustls::{
    AlertDescription, ConnectionTrafficSecrets, ExtractedSecrets,
    client::ClientConnectionData,
    crypto::cipher::{Iv, Nonce},
    kernel::KernelConnection,
};
use std::ops::Range;

const ALERT: u8 = 21;
const HANDSHAKE: u8 = 22;
const APPLICATION: u8 = 23;
const NEW_SESSION_TICKET: u8 = 4;
const KEY_UPDATE: u8 = 24;
const TAG: usize = 16;
const WIRE: usize = HEADER + RECORD + 1 + TAG;
pub(crate) const TX_RECORDS: usize = 16;
const CONTROL_HEADROOM: usize = 64;
const CONTROL_LIMIT: u8 = 32;
const HANDSHAKE_HEADER: usize = 4;
const MAX_HANDSHAKE: usize = 0xffff;

#[cfg(test)]
thread_local! {
    pub(crate) static TX_TEST_LIMIT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

struct TrafficKey {
    key: LessSafeKey,
    iv: Iv,
    seq: u64,
}

impl TrafficKey {
    fn new((seq, secret): (u64, ConnectionTrafficSecrets)) -> Result<Self, Outcome> {
        let (algorithm, key, iv) = match secret {
            ConnectionTrafficSecrets::Aes128Gcm { key, iv } => (&aead::AES_128_GCM, key, iv),
            ConnectionTrafficSecrets::Aes256Gcm { key, iv } => (&aead::AES_256_GCM, key, iv),
            ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv } => {
                (&aead::CHACHA20_POLY1305, key, iv)
            }
            _ => return Err(tls_failure(4, 0)),
        };
        let key = LessSafeKey::new(
            UnboundKey::new(algorithm, key.as_ref()).map_err(|_| tls_failure(4, 0))?,
        );
        Ok(Self { key, iv, seq })
    }

    fn nonce(&mut self) -> aead::Nonce {
        let nonce = Nonce::new(&self.iv, self.seq);
        // Callers stop or rekey at the suite limit, before a sequence can wrap.
        self.seq += 1;
        aead::Nonce::assume_unique_for_key(nonce.0)
    }

    fn seal(&mut self, slot: &mut Slot, kind: u8, input: &[u8]) -> Result<(), Outcome> {
        let payload = input.len() + 1 + TAG;
        let end = slot.end + HEADER + payload;
        let (header, body) = slot.bytes[slot.end..end].split_at_mut(HEADER);
        let [hi, lo] = (payload as u16).to_be_bytes();
        header.copy_from_slice(&[APPLICATION, 3, 3, hi, lo]);
        let (plain, tag) = body.split_at_mut(input.len() + 1);
        plain[..input.len()].copy_from_slice(input);
        plain[input.len()] = kind;
        let nonce = self.nonce();
        let sealed = self
            .key
            .seal_in_place_separate_tag(nonce, Aad::from(header), plain)
            .map_err(|_| tls_failure(4, 0))?;
        tag.copy_from_slice(sealed.as_ref());
        slot.end = end;
        Ok(())
    }
}

pub(super) struct Records {
    handshake: PostHandshake,
    tx_key: TrafficKey,
    rx_key: TrafficKey,
    tx_limit: u64,
    rx_limit: u64,
    tx: Slot,
    rx: Slot,
    /// Decrypted payload of the last record, before the unread ciphertext.
    plaintext: Range<usize>,
    read_end: Option<Outcome>,
    write_end: Option<Outcome>,
    reply_update: bool,
    consecutive_updates: u8,
    consecutive_empty: u8,
}

impl Records {
    pub(super) fn new(
        secrets: ExtractedSecrets,
        kernel: KernelConnection<ClientConnectionData>,
        rx: Slot,
    ) -> Result<Self, Outcome> {
        let limit = kernel
            .negotiated_cipher_suite()
            .tls13()
            .unwrap()
            .common
            .confidentiality_limit;
        let tx_limit = limit;
        #[cfg(test)]
        let tx_limit = TX_TEST_LIMIT.take().unwrap_or(tx_limit);
        Ok(Self {
            handshake: PostHandshake {
                kernel,
                fragment: Vec::new(),
            },
            tx_key: TrafficKey::new(secrets.tx)?,
            rx_key: TrafficKey::new(secrets.rx)?,
            tx_limit,
            rx_limit: limit,
            tx: Slot::new(TX_RECORDS * WIRE + CONTROL_HEADROOM),
            rx,
            plaintext: 0..0,
            read_end: None,
            write_end: None,
            reply_update: false,
            consecutive_updates: 0,
            consecutive_empty: 0,
        })
    }

    fn update_tx(&mut self) -> Result<(), Outcome> {
        self.tx_key
            .seal(&mut self.tx, HANDSHAKE, &[KEY_UPDATE, 0, 0, 1, 0])?;
        self.tx_key = TrafficKey::new(
            self.handshake
                .kernel
                .update_tx_secret()
                .map_err(protocol_error)?,
        )?;
        self.reply_update = false;
        Ok(())
    }

    fn flush(&mut self, socket: &mut SocketIo) -> Result<bool, Outcome> {
        if let Some(error) = self.write_end {
            return Err(error);
        }
        if !self.tx.flush(socket).map_err(transport)? {
            return Ok(false);
        }
        if self.reply_update {
            self.update_tx()?;
            return self.tx.flush(socket).map_err(transport);
        }
        Ok(true)
    }

    fn read_record(&mut self, socket: &mut SocketIo, allow_read: bool) -> Result<bool, ReadEnd> {
        let (start, end) = loop {
            let input = &self.rx.bytes[self.rx.start..self.rx.end];
            if input.len() >= HEADER {
                let len = payload_len(input);
                if len >= MAX_PAYLOAD {
                    return Err(ReadEnd::Local(AlertDescription::RecordOverflow));
                }
                if input[0] != APPLICATION {
                    return Err(ReadEnd::Local(AlertDescription::UnexpectedMessage));
                }
                if input.len() >= HEADER + len {
                    break (self.rx.start, self.rx.start + HEADER + len);
                }
            }
            if !allow_read {
                return Ok(false);
            }
            match self.rx.read(socket) {
                Ok(0) => return Err(ReadEnd::Transport(tls_failure(6, 0))),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => return Err(ReadEnd::Transport(transport(error))),
            }
        };

        if self.rx_key.seq >= self.rx_limit {
            return Err(ReadEnd::Local(AlertDescription::UnexpectedMessage));
        }
        let (header, body) = self.rx.bytes[start..end].split_at_mut(HEADER);
        let nonce = self.rx_key.nonce();
        let plaintext = self
            .rx_key
            .key
            .open_in_place(nonce, Aad::from(header), body)
            .map_err(|_| ReadEnd::Local(AlertDescription::BadRecordMac))?;
        if plaintext.len() > RECORD + 1 {
            return Err(ReadEnd::Local(AlertDescription::RecordOverflow));
        }
        let content = plaintext
            .iter()
            .rposition(|&byte| byte != 0)
            .ok_or(ReadEnd::Local(AlertDescription::UnexpectedMessage))?;
        let (payload, kind) = plaintext.split_at(content);
        let kind = kind[0];
        if kind != HANDSHAKE && !self.handshake.fragment.is_empty() {
            return Err(ReadEnd::Local(AlertDescription::UnexpectedMessage));
        }
        match kind {
            APPLICATION => {
                self.consecutive_updates = 0;
                if payload.is_empty() {
                    self.consecutive_empty += 1;
                    if self.consecutive_empty > CONTROL_LIMIT {
                        return Err(ReadEnd::Local(AlertDescription::UnexpectedMessage));
                    }
                } else {
                    self.consecutive_empty = 0;
                    self.plaintext = start + HEADER..start + HEADER + content;
                }
            }
            HANDSHAKE if !payload.is_empty() => {
                self.consecutive_empty = 0;
                if let Some(reply) = self.handshake.consume(payload).map_err(ReadEnd::Local)? {
                    self.consecutive_updates += 1;
                    if self.consecutive_updates > CONTROL_LIMIT {
                        return Err(ReadEnd::Local(AlertDescription::UnexpectedMessage));
                    }
                    self.rx_key = self
                        .handshake
                        .kernel
                        .update_rx_secret()
                        .map_err(protocol_error)
                        .and_then(TrafficKey::new)
                        .map_err(ReadEnd::Transport)?;
                    self.reply_update |= reply;
                }
            }
            ALERT => match payload {
                [1 | 2, code] => return Err(ReadEnd::Peer(*code)),
                _ => return Err(ReadEnd::Local(AlertDescription::DecodeError)),
            },
            _ => return Err(ReadEnd::Local(AlertDescription::UnexpectedMessage)),
        }
        self.rx.consume(end - start);
        Ok(true)
    }

    fn stop_read(&mut self, socket: &mut SocketIo, end: ReadEnd) {
        let error = match end {
            ReadEnd::Local(alert) => {
                self.send_alert(socket, 2, alert.into());
                tls_failure(4, 0)
            }
            ReadEnd::Peer(0) => {
                self.send_alert(socket, 1, 0);
                count(0)
            }
            ReadEnd::Peer(code) => tls_failure(4, code),
            ReadEnd::Transport(error) => error,
        };
        // SAFETY: the provider owns this live socket for the callback.
        unsafe { libc::shutdown(socket.0, libc::SHUT_RDWR) };
        self.read_end = Some(error);
        self.write_end = Some(if error.error == IoError::OK {
            os_net::signal(5)
        } else {
            error
        });
    }

    fn send_alert(&mut self, socket: &mut SocketIo, level: u8, alert: u8) {
        if self.write_end.is_none()
            && self.tx_key.seq < self.tx_limit
            && self
                .tx_key
                .seal(&mut self.tx, ALERT, &[level, alert])
                .is_ok()
        {
            let _ = self.tx.flush(socket);
        }
    }

    fn read_ready(&mut self, socket: &mut SocketIo, allow_read: bool) -> bool {
        while self.plaintext.is_empty() && self.read_end.is_none() {
            match self.read_record(socket, allow_read) {
                Ok(true) => {
                    if self.reply_update
                        && let Err(error) = self.flush(socket)
                    {
                        self.write_end = Some(error);
                    }
                }
                Ok(false) => return false,
                Err(end) => self.stop_read(socket, end),
            }
        }
        true
    }
}

enum ReadEnd {
    Local(AlertDescription),
    Peer(u8),
    Transport(Outcome),
}

impl SecureStream for Records {
    fn receive(&mut self, fd: i32, destination: &mut [u8]) -> Option<Outcome> {
        if destination.is_empty() {
            return Some(self.read_end.unwrap_or_else(|| count(0)));
        }
        let mut socket = SocketIo(fd);
        let mut copied = 0;
        while copied < destination.len() && self.read_ready(&mut socket, copied == 0) {
            if let Some(end) = self.read_end {
                return Some(if copied == 0 { end } else { count(copied) });
            }
            let n = (destination.len() - copied).min(self.plaintext.len());
            let end = self.plaintext.start + n;
            destination[copied..copied + n]
                .copy_from_slice(&self.rx.bytes[self.plaintext.start..end]);
            self.plaintext.start = end;
            copied += n;
        }
        (copied != 0).then(|| count(copied))
    }

    fn ready(&mut self, fd: i32) -> Option<Outcome> {
        self.read_ready(&mut SocketIo(fd), true).then(|| count(0))
    }

    fn send(&mut self, fd: i32, prefix: &[u8], tail: &[u8]) -> Option<Outcome> {
        let mut socket = SocketIo(fd);
        let mut committed = 0;
        let result = (|| {
            if !self.flush(&mut socket)? {
                return Ok(false);
            }
            let mut queued = 0;
            for record in [prefix, tail]
                .into_iter()
                .flat_map(|part| part.chunks(RECORD))
            {
                if self.tx_key.seq >= self.tx_limit - 1 {
                    if !self.flush(&mut socket)? {
                        return Ok(false);
                    }
                    self.update_tx()?;
                    queued = 0;
                }
                self.tx_key.seal(&mut self.tx, APPLICATION, record)?;
                committed += record.len();
                queued += 1;
                if queued == TX_RECORDS {
                    if !self.flush(&mut socket)? {
                        return Ok(false);
                    }
                    queued = 0;
                }
            }
            self.flush(&mut socket)
        })();
        if let Err(error) = result {
            self.write_end = Some(error);
            if committed == 0 {
                return Some(error);
            }
        }
        (committed != 0 || matches!(result, Ok(true))).then(|| count(committed))
    }

    fn shutdown(&mut self, fd: i32) {
        self.send_alert(&mut SocketIo(fd), 1, 0);
        self.write_end = Some(os_net::signal(5));
    }
}

/// Only tickets can require significant reassembly. Keep the common case in
/// the receive slot and allocate scratch on the first spanning message.
struct PostHandshake {
    kernel: KernelConnection<ClientConnectionData>,
    fragment: Vec<u8>,
}

impl PostHandshake {
    fn consume(&mut self, mut input: &[u8]) -> Result<Option<bool>, AlertDescription> {
        let Self { kernel, fragment } = self;
        while !input.is_empty() {
            let message = if fragment.is_empty() && input.len() >= HANDSHAKE_HEADER {
                let size = Self::message_size(input)?;
                if let Some((message, rest)) = input.split_at_checked(size) {
                    input = rest;
                    message
                } else {
                    fragment.extend_from_slice(input);
                    return Ok(None);
                }
            } else {
                let n = HANDSHAKE_HEADER
                    .saturating_sub(fragment.len())
                    .min(input.len());
                fragment.extend_from_slice(&input[..n]);
                input = &input[n..];
                if fragment.len() < HANDSHAKE_HEADER {
                    return Ok(None);
                }
                let n = (Self::message_size(fragment)? - fragment.len()).min(input.len());
                fragment.extend_from_slice(&input[..n]);
                input = &input[n..];
                if fragment.len() < Self::message_size(fragment)? {
                    return Ok(None);
                }
                &fragment[..]
            };
            let payload = &message[HANDSHAKE_HEADER..];
            match message[0] {
                NEW_SESSION_TICKET => kernel
                    .handle_new_session_ticket(payload)
                    .map_err(|_| AlertDescription::DecodeError)?,
                KEY_UPDATE => {
                    // A change of keys must align with a record boundary.
                    if !input.is_empty() {
                        return Err(AlertDescription::UnexpectedMessage);
                    }
                    let [request @ (0 | 1)] = payload else {
                        return Err(AlertDescription::IllegalParameter);
                    };
                    let reply = *request == 1;
                    fragment.clear();
                    return Ok(Some(reply));
                }
                _ => return Err(AlertDescription::UnexpectedMessage),
            }
            fragment.clear();
        }
        Ok(None)
    }

    fn message_size(header: &[u8]) -> Result<usize, AlertDescription> {
        let len = u32::from_be_bytes([0, header[1], header[2], header[3]]) as usize;
        if len > MAX_HANDSHAKE {
            return Err(AlertDescription::DecodeError);
        }
        Ok(HANDSHAKE_HEADER + len)
    }
}
