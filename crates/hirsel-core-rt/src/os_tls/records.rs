use super::packet_cipher::PacketCipher;
use super::*;
use hirsel_core_net::{TxSlot, TxState, secret_aead::AeadAlg};
#[cfg(all(test, target_arch = "x86_64"))]
use ring::aead::{self, Aad, LessSafeKey, UnboundKey};
use rustls::{
    AlertDescription, ConnectionTrafficSecrets, ExtractedSecrets,
    client::ClientConnectionData,
    crypto::cipher::{Iv, Nonce},
    kernel::KernelConnection,
};
use std::mem::MaybeUninit;
use std::ops::Range;

const ALERT: u8 = 21;
const HANDSHAKE: u8 = 22;
const APPLICATION: u8 = 23;
const NEW_SESSION_TICKET: u8 = 4;
const KEY_UPDATE: u8 = 24;
const TAG: usize = 16;
const WIRE: usize = HEADER + RECORD + 1 + TAG;
const MAX_CIPHERTEXT: usize = RECORD + 256;
pub(crate) const TX_RECORDS: usize = 16;
const CONTROL_HEADROOM: usize = 64;
pub(super) const TX_SLOT: usize = TX_RECORDS * WIRE + CONTROL_HEADROOM;
const CONTROL_LIMIT: u8 = 32;
const HANDSHAKE_HEADER: usize = 4;
const MAX_HANDSHAKE: usize = 0xffff;

#[cfg(test)]
thread_local! {
    pub(crate) static TX_TEST_LIMIT: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

#[cfg(all(test, target_arch = "x86_64"))]
pub(super) fn check_fallback_seal() {
    let key = [0x42; 32];
    let iv = [0x5a; 12];
    let plain = b"fallback";
    let header = [23, 3, 3, 0, 25];
    for (algorithm, secret) in [
        (
            &aead::AES_256_GCM,
            ConnectionTrafficSecrets::Aes256Gcm {
                key: key.into(),
                iv: iv.into(),
            },
        ),
        (
            &aead::CHACHA20_POLY1305,
            ConnectionTrafficSecrets::Chacha20Poly1305 {
                key: key.into(),
                iv: iv.into(),
            },
        ),
    ] {
        let mut sending = TrafficKey::new((255, secret)).unwrap();
        assert!(!sending.key.writes_into());
        let opening = LessSafeKey::new(UnboundKey::new(algorithm, &key).unwrap());
        let mut slot = Slot::new(HEADER + usize::from(header[4]));
        for seq in 255u64..=257 {
            sending.seal(&mut slot, APPLICATION, plain).unwrap();
            assert_eq!(slot.end, slot.bytes.len());
            assert_eq!(slot.bytes[..HEADER], header);
            let mut nonce = iv;
            for (byte, seq_byte) in nonce[4..].iter_mut().zip(seq.to_be_bytes()) {
                *byte ^= seq_byte;
            }
            let opened = opening
                .open_in_place(
                    aead::Nonce::assume_unique_for_key(nonce),
                    Aad::from(header),
                    &mut slot.bytes[HEADER..slot.end],
                )
                .unwrap();
            assert_eq!(opened, b"fallback\x17");
            slot.consume(slot.end);
        }
    }
}

struct TrafficKey {
    key: PacketCipher,
    iv: Iv,
    seq: u64,
}

impl TrafficKey {
    /// A key that seals and opens AES-GCM out of place where the CPU allows.
    fn new((seq, secret): (u64, ConnectionTrafficSecrets)) -> Result<Self, Outcome> {
        let (algorithm, key, iv) = match secret {
            ConnectionTrafficSecrets::Aes128Gcm { key, iv } => (AeadAlg::Aes128Gcm, key, iv),
            ConnectionTrafficSecrets::Aes256Gcm { key, iv } => (AeadAlg::Aes256Gcm, key, iv),
            ConnectionTrafficSecrets::Chacha20Poly1305 { key, iv } => {
                (AeadAlg::ChaCha20Poly1305, key, iv)
            }
            _ => return Err(tls_failure(4, 0)),
        };
        let key = PacketCipher::new(algorithm, key.as_ref()).map_err(|_| tls_failure(4, 0))?;
        Ok(Self { key, iv, seq })
    }

    fn nonce(&mut self) -> [u8; 12] {
        let nonce = Nonce::new(&self.iv, self.seq);
        // Callers stop or rekey at the suite limit, before a sequence can wrap.
        self.seq += 1;
        nonce.0
    }

    fn seal(&mut self, slot: &mut Slot, kind: u8, input: &[u8]) -> Result<(), Outcome> {
        slot.acquire();
        let end = slot.end + HEADER + input.len() + 1 + TAG;
        self.seal_into(&mut slot.bytes[slot.end..end], kind, input)?;
        slot.end = end;
        Ok(())
    }

    /// Seals `input` as one record filling `record`, which holds exactly its
    /// header, payload, content type and tag.
    fn seal_into(&mut self, record: &mut [u8], kind: u8, input: &[u8]) -> Result<(), Outcome> {
        let (header, body) = record.split_at_mut(HEADER);
        let [hi, lo] = (body.len() as u16).to_be_bytes();
        header.copy_from_slice(&[APPLICATION, 3, 3, hi, lo]);
        let nonce = self.nonce();
        self.key
            .seal_record(nonce, header, input, kind, body)
            .map_err(|_| tls_failure(4, 0))
    }

    /// Opens a whole record in place, header first. Returns its content type
    /// and the length of its payload, which starts after the header.
    fn open(&mut self, record: &mut [u8], limit: u64) -> Result<(u8, usize), AlertDescription> {
        if self.seq >= limit {
            return Err(AlertDescription::UnexpectedMessage);
        }
        let (header, body) = record.split_at_mut(HEADER);
        let nonce = self.nonce();
        let plaintext = self
            .key
            .open(nonce, header, body)
            .map_err(|_| AlertDescription::BadRecordMac)?;
        if plaintext.len() > RECORD + 1 {
            return Err(AlertDescription::RecordOverflow);
        }
        let content = plaintext
            .iter()
            .rposition(|&byte| byte != 0)
            .ok_or(AlertDescription::UnexpectedMessage)?;
        Ok((plaintext[content], content))
    }

    /// Whether `open_into` can open a record of `body` ciphertext bytes, tag
    /// included, into `out`.
    fn opens_into(&self, body: usize, out: &[u8]) -> bool {
        self.key.writes_into() && body >= TAG && out.len() >= body - TAG
    }

    /// Opens a whole record, header first, into `out`, which takes its whole
    /// decrypted body (payload, content type and padding) and must hold it,
    /// as `opens_into` checks. Returns the content type and the payload
    /// length. A record that fails to open leaves `out` zeroed.
    fn open_into(
        &mut self,
        record: &[u8],
        out: &mut [u8],
        limit: u64,
    ) -> Result<(u8, usize), AlertDescription> {
        if self.seq >= limit {
            return Err(AlertDescription::UnexpectedMessage);
        }
        let (header, body) = record.split_at(HEADER);
        let plaintext = &mut out[..body.len() - TAG];
        let nonce = self.nonce();
        // SAFETY: MaybeUninit<u8> has the same layout. With an exact-size
        // destination and a complete record, all paths write the body.
        let destination = unsafe {
            std::slice::from_raw_parts_mut(
                plaintext.as_mut_ptr().cast::<MaybeUninit<u8>>(),
                plaintext.len(),
            )
        };
        self.key
            .open_into(nonce, header, body, destination)
            .map_err(|_| AlertDescription::BadRecordMac)?;
        if plaintext.len() > RECORD + 1 {
            return Err(AlertDescription::RecordOverflow);
        }
        let content = plaintext
            .iter()
            .rposition(|&byte| byte != 0)
            .ok_or(AlertDescription::UnexpectedMessage)?;
        Ok((plaintext[content], content))
    }
}

/// The length of the whole record at the front of `input`, once it is all
/// there.
fn whole_record(input: &[u8]) -> Result<Option<usize>, AlertDescription> {
    if input.len() < HEADER {
        return Ok(None);
    }
    let len = payload_len(input);
    if len > MAX_CIPHERTEXT {
        return Err(AlertDescription::RecordOverflow);
    }
    if input[0] != APPLICATION {
        return Err(AlertDescription::UnexpectedMessage);
    }
    Ok((input.len() >= HEADER + len).then_some(HEADER + len))
}

/// What an opened record held.
pub(crate) enum Opened {
    /// Application data, at this range of the record's buffer.
    Data(Range<usize>),
    /// A ticket, a key update or an empty record.
    Control,
    /// The peer's close_notify.
    Closed,
}

/// TLS 1.3 records sealed and opened in buffers the caller owns, so opened
/// plaintext stays where it was received and the caller zeroes it (TLS-7's
/// credential exchange). The handshake is rustls'; it holds no key bytes.
pub(crate) struct OwnedRecords {
    handshake: PostHandshake,
    tx_key: TrafficKey,
    rx_key: TrafficKey,
    limit: u64,
}

impl OwnedRecords {
    pub(crate) fn new(connection: UnbufferedClientConnection) -> Result<Self, Outcome> {
        let (secrets, kernel) = connection
            .dangerous_into_kernel_connection()
            .map_err(protocol_error)?;
        let limit = kernel
            .negotiated_cipher_suite()
            .tls13()
            .ok_or(tls_failure(4, 0))?
            .common
            .confidentiality_limit;
        Ok(Self {
            handshake: PostHandshake {
                kernel,
                fragment: Vec::new(),
            },
            tx_key: TrafficKey::new(secrets.tx)?,
            rx_key: TrafficKey::new(secrets.rx)?,
            limit,
        })
    }

    /// Appends `input` to `out` sealed as application records.
    pub(crate) fn seal(&mut self, out: &mut Vec<u8>, input: &[u8]) -> Result<(), Outcome> {
        for chunk in input.chunks(RECORD) {
            if self.tx_key.seq >= self.limit {
                return Err(tls_failure(4, 0));
            }
            let start = out.len();
            out.resize(start + HEADER + chunk.len() + 1 + TAG, 0);
            self.tx_key
                .seal_into(&mut out[start..], APPLICATION, chunk)?;
        }
        Ok(())
    }

    /// Opens the record at the front of `wire` in place. None while it is
    /// incomplete; else the record's length and what it held. A local
    /// failure is `Protocol(0)` and a peer's alert `Protocol(code)`.
    pub(crate) fn open(&mut self, wire: &mut [u8]) -> Result<Option<(usize, Opened)>, Outcome> {
        let local = |_: AlertDescription| tls_failure(4, 0);
        let Some(len) = whole_record(wire).map_err(local)? else {
            return Ok(None);
        };
        let (kind, content) = self
            .rx_key
            .open(&mut wire[..len], self.limit)
            .map_err(local)?;
        let payload = &wire[HEADER..HEADER + content];
        if kind != HANDSHAKE && !self.handshake.fragment.is_empty() {
            return Err(tls_failure(4, 0));
        }
        let opened = match kind {
            APPLICATION if payload.is_empty() => Opened::Control,
            APPLICATION => Opened::Data(HEADER..HEADER + content),
            HANDSHAKE if !payload.is_empty() => {
                // A requested update is owed only before our next record,
                // and the exchange sends none after its request.
                if self.handshake.consume(payload).map_err(local)?.is_some() {
                    self.rx_key = self
                        .handshake
                        .kernel
                        .update_rx_secret()
                        .map_err(protocol_error)
                        .and_then(TrafficKey::new)?;
                }
                Opened::Control
            }
            ALERT => match payload {
                [1, 0] => Opened::Closed,
                [1 | 2, code] => return Err(tls_failure(4, *code)),
                _ => return Err(tls_failure(4, 0)),
            },
            _ => return Err(tls_failure(4, 0)),
        };
        Ok(Some((len, opened)))
    }
}

/// Sealed records on their way to the socket. Records seal straight into
/// the bytes they are sent from: a bulk slot written with `send(2)`, or,
/// when the socket's io_uring ring sends zero-copy, a ring transmit slot. A
/// stream picks the zero-copy path while it has nothing queued, and keeps it
/// until it is drained again, so the two never interleave on the wire.
struct Tx {
    plain: Slot,
    zc: Option<ZcTx>,
}

/// Zero-copy transmit: one slot in the kernel (`flight`) while the next
/// batch seals into another (`filling`), so sealing overlaps the send.
struct ZcTx {
    poller: Arc<hirsel_core_net::Poller>,
    tag: u64,
    filling: Option<TxSlot>,
    end: usize,
    flight: Option<Flight>,
}

struct Flight {
    slot: TxSlot,
    sent: usize,
    end: usize,
}

impl Tx {
    fn idle(&self) -> bool {
        self.plain.start == self.plain.end
            && self
                .zc
                .as_ref()
                .is_none_or(|zc| zc.flight.is_none() && zc.end == 0)
    }

    /// Choose the send path for the next bytes. Only an idle stream changes
    /// path, so queued bytes always leave before later ones.
    fn choose(&mut self, zc: ZeroCopy) {
        if !self.idle() {
            return;
        }
        self.zc = zc.map(|(poller, tag)| ZcTx {
            poller,
            tag,
            filling: None,
            end: 0,
            flight: None,
        });
    }

    fn seal(&mut self, key: &mut TrafficKey, kind: u8, input: &[u8]) -> Result<(), Outcome> {
        let wire = HEADER + input.len() + 1 + TAG;
        let Some(zc) = &mut self.zc else {
            return key.seal(&mut self.plain, kind, input);
        };
        if zc.filling.is_none() {
            zc.filling = Some(
                zc.poller
                    .tx_slot(TX_SLOT)
                    .ok_or_else(|| transport(io::Error::from_raw_os_error(libc::ENOBUFS)))?,
            );
        }
        let bytes = zc.filling.as_mut().unwrap().bytes();
        key.seal_into(&mut bytes[zc.end..zc.end + wire], kind, input)?;
        zc.end += wire;
        Ok(())
    }

    /// Move queued bytes on. Zero-copy: account the flight's answer, resend
    /// what it left, and submit the filled slot once the flight is done.
    fn pump(&mut self, socket: &mut SocketIo) -> io::Result<()> {
        let Some(zc) = &mut self.zc else {
            self.plain.flush(socket)?;
            return Ok(());
        };
        loop {
            if let Some(flight) = &mut zc.flight {
                match flight.slot.state() {
                    TxState::Sending | TxState::Filling => return Ok(()),
                    // An abandoned send's cancel (`cancel_send`) leaves the
                    // rest of its slot for the next send, which resumes it.
                    TxState::Sent(error)
                        if [-libc::ECANCELED, -libc::EINTR, -libc::EAGAIN].contains(&error) =>
                    {
                        return flight.slot.send(zc.tag, flight.sent..flight.end);
                    }
                    TxState::Sent(error) if error < 0 => {
                        return Err(io::Error::from_raw_os_error(-error));
                    }
                    TxState::Sent(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                    TxState::Sent(n) => {
                        flight.sent += n as usize;
                        if flight.sent < flight.end {
                            return flight.slot.send(zc.tag, flight.sent..flight.end);
                        }
                        // The ring keeps the bytes until their notification.
                        zc.flight = None;
                    }
                }
            }
            if zc.end == 0 {
                return Ok(());
            }
            let mut slot = zc.filling.take().unwrap();
            let end = std::mem::take(&mut zc.end);
            slot.send(zc.tag, 0..end)?;
            zc.flight = Some(Flight { slot, sent: 0, end });
        }
    }

    /// Whether every queued byte has reached the socket.
    fn flush(&mut self, socket: &mut SocketIo) -> io::Result<bool> {
        self.pump(socket)?;
        Ok(self.idle())
    }

    /// Whether the next batch of records has room: the plain slot drained,
    /// or no zero-copy slot still waiting behind the flight.
    fn room(&mut self, socket: &mut SocketIo) -> io::Result<bool> {
        self.pump(socket)?;
        Ok(match &self.zc {
            None => self.plain.start == self.plain.end,
            Some(zc) => zc.end == 0,
        })
    }

    fn release(&mut self) {
        self.plain.release();
    }

    /// Ask the kernel to hand back the send in flight, keeping its unsent
    /// bytes for the next send, so the abandoned socket can close at once.
    fn cancel(&self) {
        if let Some(flight) = self.zc.as_ref().and_then(|zc| zc.flight.as_ref()) {
            flight.slot.cancel();
        }
    }
}

impl Drop for ZcTx {
    fn drop(&mut self) {
        // A closed stream drops its unsent bytes, as a plain slot does; a
        // send parked in the kernel would otherwise hold the socket open.
        if let Some(flight) = &self.flight {
            flight.slot.cancel();
        }
    }
}

pub(super) struct Records {
    handshake: PostHandshake,
    tx_key: TrafficKey,
    rx_key: TrafficKey,
    tx_limit: u64,
    rx_limit: u64,
    tx: Tx,
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
            tx: Tx {
                plain: Slot::bulk(),
                zc: None,
            },
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
        self.tx
            .seal(&mut self.tx_key, HANDSHAKE, &[KEY_UPDATE, 0, 0, 1, 0])?;
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
            if !self.tx.flush(socket).map_err(transport)? {
                return Ok(false);
            }
        }
        self.tx.release();
        Ok(true)
    }

    /// Returns the receive slot once no record bytes or plaintext remain in it.
    fn release_rx(&mut self) {
        if self.plaintext.is_empty() {
            self.rx.release();
        }
    }

    /// Opens the next record. None while it would wait; else how many
    /// payload bytes it delivered straight into `direct`. Application data
    /// opens there when its whole decrypted body fits, and otherwise in the
    /// receive slot, as `plaintext`. Past the delivered bytes, `direct` holds
    /// nothing a record carried: anything else opened there is zeroed.
    fn read_record(
        &mut self,
        socket: &mut SocketIo,
        allow_read: bool,
        direct: &mut [u8],
    ) -> Result<Option<usize>, ReadEnd> {
        let (start, end) = loop {
            let input = &self.rx.bytes[self.rx.start..self.rx.end];
            if let Some(len) = whole_record(input).map_err(ReadEnd::Local)? {
                break (self.rx.start, self.rx.start + len);
            }
            if !allow_read {
                return Ok(None);
            }
            match self.rx.read(socket) {
                Ok(0) => return Err(ReadEnd::Transport(tls_failure(6, 0))),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) => return Err(ReadEnd::Transport(transport(error))),
            }
        };

        let into = self.rx_key.opens_into(end - start - HEADER, direct);
        let delivered = self.dispatch(start..end, into.then_some(&mut *direct));
        if into {
            // The content type, padding and any control payload.
            let opened = end - start - HEADER - TAG;
            direct[*delivered.as_ref().unwrap_or(&0)..opened].fill(0);
        }
        let delivered = delivered?;
        self.rx.consume(end - start);
        Ok(Some(delivered))
    }

    /// Opens the whole record at `record` in the receive slot, or into
    /// `direct`, and acts on it. Returns the payload bytes left in `direct`.
    fn dispatch(
        &mut self,
        record: Range<usize>,
        direct: Option<&mut [u8]>,
    ) -> Result<usize, ReadEnd> {
        let start = record.start;
        let into = direct.is_some();
        let (kind, content, payload): (u8, usize, &[u8]) = match direct {
            Some(out) => {
                let (kind, content) = self
                    .rx_key
                    .open_into(&self.rx.bytes[record], out, self.rx_limit)
                    .map_err(ReadEnd::Local)?;
                (kind, content, &out[..content])
            }
            None => {
                let (kind, content) = self
                    .rx_key
                    .open(&mut self.rx.bytes[record], self.rx_limit)
                    .map_err(ReadEnd::Local)?;
                let payload = &self.rx.bytes[start + HEADER..start + HEADER + content];
                (kind, content, payload)
            }
        };
        let mut delivered = 0;
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
                    if into {
                        delivered = content;
                    } else {
                        self.plaintext = start + HEADER..start + HEADER + content;
                    }
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
        Ok(delivered)
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
                .tx
                .seal(&mut self.tx_key, ALERT, &[level, alert])
                .is_ok()
        {
            let _ = self.tx.flush(socket);
        }
    }

    /// Opens records until plaintext waits in the slot, the read ends or
    /// payload lands in `direct`. None while it would wait; else how many
    /// bytes landed in `direct`.
    fn read_ready(
        &mut self,
        socket: &mut SocketIo,
        allow_read: bool,
        direct: &mut [u8],
    ) -> Option<usize> {
        while self.plaintext.is_empty() && self.read_end.is_none() {
            match self.read_record(socket, allow_read, direct) {
                Ok(Some(delivered)) => {
                    if self.reply_update
                        && let Err(error) = self.flush(socket)
                    {
                        self.write_end = Some(error);
                    }
                    if delivered != 0 {
                        return Some(delivered);
                    }
                }
                Ok(None) => return None,
                Err(end) => self.stop_read(socket, end),
            }
        }
        Some(0)
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
        while copied < destination.len() {
            let Some(delivered) =
                self.read_ready(&mut socket, copied == 0, &mut destination[copied..])
            else {
                break;
            };
            copied += delivered;
            if delivered != 0 {
                continue;
            }
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
        self.release_rx();
        (copied != 0).then(|| count(copied))
    }

    fn ready(&mut self, fd: i32) -> Option<Outcome> {
        let ready = self.read_ready(&mut SocketIo(fd), true, &mut []);
        self.release_rx();
        ready.map(|_| count(0))
    }

    fn send(&mut self, fd: i32, zc: ZeroCopy, prefix: &[u8], tail: &[u8]) -> Option<Outcome> {
        let mut socket = SocketIo(fd);
        let mut committed = 0;
        self.tx.choose(zc);
        let result = (|| {
            if let Some(error) = self.write_end {
                return Err(error);
            }
            if self.reply_update && !self.flush(&mut socket)? {
                return Ok(false);
            }
            if !self.tx.room(&mut socket).map_err(transport)? {
                return Ok(false);
            }
            let mut queued = 0;
            for record in [prefix, tail]
                .into_iter()
                .flat_map(|part| part.chunks(RECORD))
            {
                if self.tx_key.seq >= self.tx_limit - 1 {
                    if !self.tx.room(&mut socket).map_err(transport)? {
                        return Ok(false);
                    }
                    self.update_tx()?;
                    queued = 0;
                }
                self.tx.seal(&mut self.tx_key, APPLICATION, record)?;
                committed += record.len();
                queued += 1;
                if queued == TX_RECORDS {
                    if !self.tx.room(&mut socket).map_err(transport)? {
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

    fn cancel_send(&mut self) {
        self.tx.cancel();
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
