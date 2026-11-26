//! Nonce fields and sender counter rules for the TLS-8 AEAD profiles.
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NonceFormat {
    Quic,
    QuicMultipath,
    Dtls13,
    Tls13,
    Dtls12 {
        epoch: u16,
    },
    SrtpRtp,
    SrtpRtcp,
    /// RFC 9605 §4.4.3: the salt XOR the 64-bit CTR at bytes 4..12.
    Sframe,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CounterError {
    Range,
    Reused,
}

impl NonceFormat {
    pub fn valid_domain(self, domain: u64) -> bool {
        match self {
            Self::QuicMultipath | Self::SrtpRtp | Self::SrtpRtcp => u32::try_from(domain).is_ok(),
            _ => domain == 0,
        }
    }

    fn sequence_max(self, sending: bool) -> u64 {
        match self {
            Self::Quic | Self::QuicMultipath => (1 << 62) - 1,
            Self::SrtpRtp => (1 << 48) - 1,
            Self::SrtpRtcp => (1 << 31) - 1,
            Self::Dtls12 { .. } if sending => (1 << 48) - 1,
            _ => u64::MAX,
        }
    }

    fn nonce(self, base: &[u8; 12], domain: u64, seq: u64) -> [u8; 12] {
        let mut nonce = *base;
        let seq = seq.to_be_bytes();
        let domain = (domain as u32).to_be_bytes();
        let xor = |a: &mut [u8], b: &[u8]| {
            for (a, b) in a.iter_mut().zip(b) {
                *a ^= *b;
            }
        };
        match self {
            Self::QuicMultipath => {
                xor(&mut nonce[..4], &domain);
                xor(&mut nonce[4..], &seq);
            }
            Self::SrtpRtp => {
                xor(&mut nonce[2..6], &domain);
                xor(&mut nonce[6..], &seq[2..]);
            }
            Self::SrtpRtcp => {
                xor(&mut nonce[2..6], &domain);
                xor(&mut nonce[8..], &seq[4..]);
            }
            _ => xor(&mut nonce[4..], &seq),
        }
        nonce
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Nonce {
    Aead([u8; 12]),
    Cm([u8; 16]),
}

pub(super) enum Base {
    Aead(Zeroizing<[u8; 12]>),
    Cm(Zeroizing<[u8; 14]>),
}

impl Base {
    fn nonce(&self, format: NonceFormat, domain: u64, seq: u64) -> Nonce {
        match self {
            Self::Aead(base) => Nonce::Aead(format.nonce(base, domain, seq)),
            Self::Cm(salt) => Nonce::Cm(cm_nonce(format, salt, domain as u32, seq)),
        }
    }
}

pub(super) fn cm_nonce(format: NonceFormat, salt: &[u8; 14], ssrc: u32, seq: u64) -> [u8; 16] {
    let start = match format {
        NonceFormat::SrtpRtp => 8,
        NonceFormat::SrtpRtcp => 10,
        _ => unreachable!("CM is confined to RTP and RTCP"),
    };
    let mut nonce = [0; 16];
    nonce[..14].copy_from_slice(salt);
    for (byte, word) in nonce[4..8].iter_mut().zip(ssrc.to_be_bytes()) {
        *byte ^= word;
    }
    for (byte, word) in nonce[start..14]
        .iter_mut()
        .zip(&seq.to_be_bytes()[start - 6..])
    {
        *byte ^= word;
    }
    nonce
}

/// A sender's key lives independently of its owner's issuance ledger.
pub struct SenderCounter {
    format: NonceFormat,
    base: Base,
    domain: u64,
    floor: u64,
    last: Option<u64>,
}

impl SenderCounter {
    pub fn new(format: NonceFormat, base: [u8; 12], domain: u64, floor: u64) -> Option<Self> {
        Self::with_base(format, Base::Aead(Zeroizing::new(base)), domain, floor)
    }

    pub(super) fn with_base(
        format: NonceFormat,
        base: Base,
        domain: u64,
        floor: u64,
    ) -> Option<Self> {
        format.valid_domain(domain).then_some(Self {
            format,
            base,
            domain,
            floor,
            last: None,
        })
    }

    /// Public sender state carried in a crossing snapshot.
    pub fn snapshot(&self) -> (u64, u64, Option<u64>) {
        (self.domain, self.floor, self.last)
    }

    /// Restore public counter state. Replay captures the packet operation,
    /// so it uses the nonce format for validation and never uses the nonce bytes.
    pub fn from_snapshot(
        format: NonceFormat,
        domain: u64,
        floor: u64,
        highest: Option<u64>,
    ) -> Option<Self> {
        let mut counter = Self::new(format, [0; 12], domain, floor)?;
        if let Some(highest) = highest {
            counter.packet_nonce(highest).ok()?;
            counter.commit(highest);
        }
        Some(counter)
    }

    /// Validation does not advance the counter. The adapter commits only
    /// after it validates lengths and reserves the caller's destination and
    /// any AAD scratch, at the same point where the operation issues.
    pub fn check(&self, seq: u64) -> Result<[u8; 12], CounterError> {
        self.packet_nonce(seq).map(|nonce| match nonce {
            Nonce::Aead(bytes) => bytes,
            Nonce::Cm(_) => unreachable!("use packet_nonce for CM"),
        })
    }

    pub fn packet_nonce(&self, seq: u64) -> Result<Nonce, CounterError> {
        if seq > self.format.sequence_max(true) {
            return Err(CounterError::Range);
        }
        if seq < self.floor || self.last.is_some_and(|last| seq <= last) {
            return Err(CounterError::Reused);
        }
        let wire_seq = match self.format {
            NonceFormat::Dtls12 { epoch } => (u64::from(epoch) << 48) | seq,
            _ => seq,
        };
        Ok(self.base.nonce(self.format, self.domain, wire_seq))
    }

    pub fn commit(&mut self, seq: u64) {
        self.last = Some(seq);
    }
}

/// Openers authenticate a caller-supplied sequence without retaining it.
pub struct ReceiverNonce {
    format: NonceFormat,
    base: Base,
}

impl ReceiverNonce {
    pub fn new(format: NonceFormat, base: [u8; 12]) -> Self {
        Self::with_base(format, Base::Aead(Zeroizing::new(base)))
    }

    /// Replay uses the nonce format for validation and takes the packet output from tape.
    pub fn from_snapshot(format: NonceFormat) -> Self {
        Self::new(format, [0; 12])
    }

    pub(super) fn with_base(format: NonceFormat, base: Base) -> Self {
        Self { format, base }
    }

    pub fn nonce(&self, domain: u64, seq: u64) -> Result<[u8; 12], CounterError> {
        self.packet_nonce(domain, seq).map(|nonce| match nonce {
            Nonce::Aead(bytes) => bytes,
            Nonce::Cm(_) => unreachable!("use packet_nonce for CM"),
        })
    }

    pub fn packet_nonce(&self, domain: u64, seq: u64) -> Result<Nonce, CounterError> {
        if !self.format.valid_domain(domain) || seq > self.format.sequence_max(false) {
            return Err(CounterError::Range);
        }
        Ok(self.base.nonce(self.format, domain, seq))
    }
}
