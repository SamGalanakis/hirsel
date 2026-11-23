use super::*;
use rustls::unbuffered::{ConnectionState, EncryptError};

/// Drives the handshake for both versions and keeps rustls in charge of TLS 1.2
/// application records. TLS 1.3 only uses its receive slot after the handshake.
pub(super) struct RustlsStream {
    pub(super) connection: UnbufferedClientConnection,
    pub(super) rx: Slot,
    tx: Slot,
    offered: usize,
    plaintext: Slot,
    read_end: Option<Outcome>,
    write_error: Option<Outcome>,
}

impl RustlsStream {
    pub(super) fn new(connection: UnbufferedClientConnection) -> Self {
        Self {
            connection,
            rx: Slot::bulk(),
            tx: Slot::new(MAX_WIRE * 2),
            offered: 0,
            plaintext: Slot::new(0),
            read_end: None,
            write_error: None,
        }
    }

    pub(super) fn advance(
        &mut self,
        socket: &mut SocketIo,
        handshake: bool,
    ) -> Result<Result<(), bool>, Outcome> {
        loop {
            let status = self.connection.process_tls_records(
                &mut self.rx.bytes[self.rx.start..self.rx.start + self.offered],
            );
            Slot::consume_cursor(&mut self.rx.start, &mut self.rx.end, status.discard);
            self.offered -= status.discard;
            match status.state {
                Ok(ConnectionState::EncodeTlsData(mut data)) => {
                    self.tx.compact();
                    self.tx.end += data
                        .encode(&mut self.tx.bytes[self.tx.end..])
                        .map_err(|_| tls_failure(4, 0))?;
                }
                Ok(ConnectionState::TransmitTlsData(data)) => {
                    let flushed = self.tx.flush(socket).map_err(transport)?;
                    if handshake && !flushed {
                        return Ok(Err(true));
                    }
                    // After TLS 1.2's handshake, our slot orders queued control
                    // records with later writes without blocking the read half.
                    data.done();
                }
                Ok(ConnectionState::WriteTraffic(_)) if handshake => return Ok(Ok(())),
                Ok(ConnectionState::ReadTraffic(mut data)) if !handshake => {
                    let record = data.next_record().unwrap().map_err(protocol_error)?;
                    let discard = record.discard;
                    if self.plaintext.bytes.is_empty() {
                        self.plaintext = Slot::new(RECORD);
                    }
                    let len = record.payload.len();
                    self.plaintext.bytes[..len].copy_from_slice(record.payload);
                    self.plaintext.end = len;
                    self.rx.consume(discard);
                    self.offered -= discard;
                    return Ok(Ok(()));
                }
                Ok(ConnectionState::PeerClosed | ConnectionState::Closed) => {
                    if handshake {
                        return Err(tls_failure(5, 0));
                    }
                    self.read_end = Some(count(0));
                    return Ok(Ok(()));
                }
                Ok(ConnectionState::BlockedHandshake | ConnectionState::WriteTraffic(_)) => {
                    if !self.offer_record(socket)? {
                        return Ok(Err(false));
                    }
                }
                Ok(_) => return Err(tls_failure(4, 0)),
                Err(error) => {
                    self.try_alert(socket);
                    return Err(protocol_error(error));
                }
            }
        }
    }

    /// Extend the slice rustls sees by at most one wire record. In particular,
    /// a call that processes Finished cannot consume an application-key record.
    /// Keep previously offered bytes when rustls is joining handshake fragments.
    fn offer_record(&mut self, socket: &mut SocketIo) -> Result<bool, Outcome> {
        loop {
            let next = &self.rx.bytes[self.rx.start + self.offered..self.rx.end];
            if next.len() >= HEADER {
                let payload = payload_len(next);
                let size = if payload >= MAX_PAYLOAD {
                    HEADER
                } else {
                    HEADER + payload
                };
                if next.len() >= size {
                    self.offered += size;
                    return Ok(true);
                }
            }
            match self.rx.read(socket) {
                Ok(0) => {
                    return Err(tls_failure(
                        if self.connection.is_handshaking() {
                            5
                        } else {
                            6
                        },
                        0,
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                    return Err(tls_failure(4, 0));
                }
                Err(error) => return Err(transport(error)),
            }
        }
    }

    fn try_alert(&mut self, socket: &mut SocketIo) {
        let status = self
            .connection
            .process_tls_records(&mut self.rx.bytes[self.rx.start..self.rx.start + self.offered]);
        if let Ok(ConnectionState::EncodeTlsData(mut alert)) = status.state {
            self.tx.compact();
            if let Ok(n) = alert.encode(&mut self.tx.bytes[self.tx.end..]) {
                self.tx.end += n;
                let _ = self.tx.flush(socket);
            }
        }
    }

    fn read_ready(&mut self, socket: &mut SocketIo) -> bool {
        let _ = self.tx.flush(socket).map_err(|error| {
            self.write_error = Some(transport(error));
        });
        if self.plaintext.end != 0 || self.read_end.is_some() {
            return true;
        }
        let ready = match self.advance(socket, false) {
            Ok(Ok(())) => true,
            Ok(Err(_)) => false,
            Err(error) => {
                self.read_end = Some(error);
                true
            }
        };
        self.rx.release();
        ready
    }

    fn encode(&mut self, plaintext: Option<&[u8]>) -> Result<(), Outcome> {
        // Only the receive callback offers new socket bytes. If it left a
        // fragmented handshake buffered, keep rustls's slice offsets intact.
        let status = self
            .connection
            .process_tls_records(&mut self.rx.bytes[self.rx.start..self.rx.start + self.offered]);
        let Ok(ConnectionState::WriteTraffic(mut traffic)) = status.state else {
            return Err(tls_failure(4, 0));
        };
        let output = &mut self.tx.bytes[self.tx.end..];
        self.tx.end += match plaintext {
            Some(bytes) => traffic.encrypt(bytes, output),
            None => traffic.queue_close_notify(output),
        }
        .map_err(|error| match error {
            EncryptError::EncryptExhausted => tls_failure(6, 0),
            _ => tls_failure(4, 0),
        })?;
        Ok(())
    }
}

impl SecureStream for RustlsStream {
    fn receive(&mut self, fd: i32, plaintext: &mut [u8]) -> Option<Outcome> {
        if plaintext.is_empty() {
            return Some(self.read_end.unwrap_or_else(|| count(0)));
        }
        if !self.read_ready(&mut SocketIo(fd)) {
            return None;
        }
        if let Some(end) = self.read_end {
            return Some(end);
        }
        let n = plaintext
            .len()
            .min(self.plaintext.end - self.plaintext.start);
        plaintext[..n].copy_from_slice(&self.plaintext.bytes[self.plaintext.start..][..n]);
        self.plaintext.consume(n);
        Some(count(n))
    }

    fn ready(&mut self, fd: i32) -> Option<Outcome> {
        self.read_ready(&mut SocketIo(fd)).then(|| count(0))
    }

    fn send(&mut self, fd: i32, prefix: &[u8], tail: &[u8]) -> Option<Outcome> {
        let mut socket = SocketIo(fd);
        let mut committed = 0;
        let result = (|| {
            if let Some(error) = self.write_error {
                return Err(error);
            }
            for record in [prefix, tail]
                .into_iter()
                .flat_map(|part| part.chunks(RECORD))
            {
                if !self.tx.flush(&mut socket).map_err(transport)? {
                    return Ok(false);
                }
                self.encode(Some(record))?;
                committed += record.len();
            }
            self.tx.flush(&mut socket).map_err(transport)
        })();
        if let Err(error) = result {
            self.write_error = Some(error);
            if committed == 0 {
                return Some(error);
            }
        }
        (committed != 0 || matches!(result, Ok(true))).then(|| count(committed))
    }

    fn shutdown(&mut self, fd: i32) {
        if self.write_error.is_none() {
            self.tx.compact();
            let _ = self.encode(None);
            let _ = self.tx.flush(&mut SocketIo(fd));
        }
    }
}
