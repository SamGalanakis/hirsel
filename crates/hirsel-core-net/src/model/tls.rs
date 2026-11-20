use super::{Direction, Reply};
use std::collections::VecDeque;

const PROTOCOL_ERROR: TlsFailure = TlsFailure::Protocol(0);
pub(super) const RECORD_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertFailure {
    UnknownIssuer,
    NameMismatch,
    Expired,
    NotYetValid,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsFailure {
    Certificate(CertFailure),
    Name,
    Alpn,
    Roots,
    Protocol(u8),
    Closed,
    Truncated,
}

impl TlsFailure {
    fn reply(self) -> Reply {
        let code = match self {
            Self::Certificate(failure) => (failure as u32) << 8,
            Self::Name => 1,
            Self::Alpn => 2,
            Self::Roots => 3,
            Self::Protocol(alert) => 4 | (u32::from(alert) << 8),
            Self::Closed => 5,
            Self::Truncated => 6,
        };
        Reply {
            code,
            ..Reply::error(9)
        }
    }
}

#[derive(Debug)]
pub struct TlsIdentity {
    /// The peer's selected ALPN bytes, which must occur in the client's offer.
    pub protocol: Option<Vec<u8>>,
    pub peer: [u8; 32],
}

#[derive(Debug)]
pub enum TlsHandshake {
    After {
        delay_ns: u64,
        result: Result<TlsIdentity, TlsFailure>,
    },
    Silent,
}

#[derive(Debug)]
pub enum TlsStep {
    /// Compare the next plaintext from the client, across any send boundaries.
    Expect(Vec<u8>),
    Send(Vec<u8>),
    CloseNotify,
    Truncate,
}

#[derive(Debug)]
pub struct TlsScript {
    pub handshake: TlsHandshake,
    pub steps: Vec<TlsStep>,
}

enum Handshake {
    Waiting(TlsHandshake, Option<u64>),
    Established,
    Failed,
}

pub(super) struct ScriptedPeer {
    handshake: Handshake,
    steps: VecDeque<TlsStep>,
    offset: usize,
    mismatch: bool,
    truncated: bool,
    read_error: Option<TlsFailure>,
}

impl ScriptedPeer {
    pub(super) fn new(script: TlsScript) -> Self {
        Self {
            handshake: Handshake::Waiting(script.handshake, None),
            steps: script.steps.into(),
            offset: 0,
            mismatch: false,
            truncated: false,
            read_error: None,
        }
    }

    pub(super) fn ready_at(&self) -> Option<u64> {
        match self.handshake {
            Handshake::Waiting(_, at) => at,
            _ => None,
        }
    }

    pub(super) fn establish(
        &mut self,
        now: u64,
        key: u64,
        offered: &[impl AsRef<[u8]>],
    ) -> Option<Reply> {
        let Handshake::Waiting(TlsHandshake::After { delay_ns, .. }, at) = &mut self.handshake
        else {
            return None;
        };
        if now < *at.get_or_insert_with(|| now.saturating_add(*delay_ns)) {
            return None;
        }
        let Handshake::Waiting(TlsHandshake::After { result, .. }, _) =
            std::mem::replace(&mut self.handshake, Handshake::Failed)
        else {
            unreachable!()
        };
        let TlsIdentity { protocol, peer } = match result {
            Ok(identity) => identity,
            Err(error) => return Some(error.reply()),
        };
        if protocol
            .as_ref()
            .is_some_and(|selected| !offered.iter().any(|p| p.as_ref() == selected))
        {
            return Some(PROTOCOL_ERROR.reply());
        }
        self.handshake = Handshake::Established;
        Some(Reply {
            value: key,
            bytes: protocol.unwrap_or_default(),
            peer,
            ..Reply::default()
        })
    }

    pub(super) fn established(&self) -> bool {
        matches!(self.handshake, Handshake::Established)
    }

    pub(super) fn read_terminal(&self, drained: bool) -> Option<TlsFailure> {
        self.read_error.or(if self.mismatch {
            Some(PROTOCOL_ERROR)
        } else if self.truncated && drained {
            Some(TlsFailure::Truncated)
        } else {
            None
        })
    }

    pub(super) fn receive_error(&mut self, drained: bool, spare: usize) -> Option<Reply> {
        if spare != 0 {
            self.read_error = self.read_terminal(drained);
        }
        self.read_error.map(TlsFailure::reply)
    }

    pub(super) fn drive(&mut self, [client, server]: &mut [Direction; 2], window: usize) {
        if !self.established() || self.mismatch {
            return;
        }
        loop {
            let done = match self.steps.front() {
                Some(TlsStep::Expect(bytes)) => {
                    let remaining = &bytes[self.offset..];
                    let n = remaining.len().min(client.readable.len());
                    if !client
                        .readable
                        .drain(..n)
                        .eq(remaining[..n].iter().copied())
                    {
                        self.mismatch = true;
                        return;
                    }
                    self.offset += n;
                    if self.offset < bytes.len() && client.fin && client.is_drained() {
                        self.mismatch = true;
                        return;
                    }
                    self.offset == bytes.len()
                }
                Some(TlsStep::Send(bytes)) => {
                    let remaining = &bytes[self.offset..];
                    let n = remaining.len().min(window - server.accepted.len());
                    server.accepted.extend(&remaining[..n]);
                    self.offset += n;
                    self.offset == bytes.len()
                }
                Some(TlsStep::CloseNotify) => {
                    server.fin = true;
                    true
                }
                Some(TlsStep::Truncate) => {
                    self.truncated = true;
                    server.fin = true;
                    self.steps.clear();
                    return;
                }
                None => {
                    self.mismatch = !self.truncated && !client.readable.is_empty();
                    return;
                }
            };
            if !done {
                return;
            }
            self.steps.pop_front();
            self.offset = 0;
        }
    }
}

pub(super) fn unsupported() -> Reply {
    PROTOCOL_ERROR.reply()
}
