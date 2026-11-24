//! Protocol state holds keys by direction and packet space.
use crate::{
    ImportKind, ImportUse,
    packet_epoch::{
        DirectionKeys, Epoch, ImportError, IssueError, Sealer, tls13_labels, traffic_profile,
    },
    packet_nonce::NonceFormat,
    secret_aead::AeadAlg,
    srtp_cipher::{SrtpKeys, SrtpProfile},
    tls_kdf::{
        Side, Tls13Direction, Tls13Handshake, Tls13PacketKeys, Tls13Traffic, TlsHash,
        dtls12::Dtls12Keys,
    },
};

const HANDSHAKE: usize = 1;
const APPLICATION: usize = 2;

#[derive(Clone, Copy)]
pub enum ImportProfile {
    Traffic(NonceFormat, AeadAlg),
    Srtp(SrtpProfile),
}

impl ImportProfile {
    pub fn from_case(kind: ImportKind, profile: u64, suite: u64) -> Option<Self> {
        match kind {
            ImportKind::Traffic => traffic_profile(profile, suite)
                .map(|(format, algorithm)| Self::Traffic(format, algorithm)),
            ImportKind::Srtp => SrtpProfile::from_case(profile).map(Self::Srtp),
        }
    }

    pub fn import<C>(
        self,
        authority: ImportUse,
        bytes: Option<&[u8]>,
    ) -> Result<Schedule<C>, ImportError> {
        match self {
            Self::Traffic(format, algorithm) => {
                Schedule::import_traffic(format, algorithm, authority, bytes)
            }
            Self::Srtp(profile) => Schedule::import_srtp(profile, authority, bytes),
        }
    }
}

pub struct Schedule<C> {
    spaces: [Option<Box<Epoch<C>>>; 5],
    kind: Kind,
}

enum Kind {
    Fixed,
    Tls13 {
        format: NonceFormat,
        algorithm: AeadAlg,
        side: Side,
        handshake: Option<Tls13Handshake>,
        application: Option<Tls13Traffic>,
    },
    Dtls12 {
        algorithm: AeadAlg,
        local: Side,
        keys: Option<Dtls12Keys>,
        srtp_issued: bool,
    },
    TrafficImport {
        local: bool,
        traffic: Option<Tls13Direction>,
    },
}

pub struct SealerPair<'a, C> {
    epochs: [&'a mut Epoch<C>; 2],
    domain: u64,
}

impl<'a, C> SealerPair<'a, C> {
    pub fn issue(self, floors: [u64; 2], charges: [(C, C); 2]) -> [Sealer<'a, C>; 2] {
        let [first, second] = self.epochs;
        let [first_charge, second_charge] = charges;
        [
            first.issue_sealer(self.domain, floors[0], first_charge),
            second.issue_sealer(self.domain, floors[1], second_charge),
        ]
    }
}

impl<C> Schedule<C> {
    pub fn single(space: usize, epoch: Epoch<C>) -> Self {
        let mut spaces = std::array::from_fn(|_| None);
        spaces[space] = Some(Box::new(epoch));
        Self {
            spaces,
            kind: Kind::Fixed,
        }
    }

    pub fn tls13(
        format: NonceFormat,
        algorithm: AeadAlg,
        side: Side,
        handshake: Option<Tls13Handshake>,
    ) -> Self {
        let handshake_format = if format == NonceFormat::QuicMultipath {
            NonceFormat::Quic
        } else {
            format
        };
        let mut schedule = Self::single(
            HANDSHAKE,
            Epoch::traffic(
                handshake_format,
                algorithm,
                side,
                handshake.as_ref().map(Tls13Handshake::traffic),
            ),
        );
        schedule.kind = Kind::Tls13 {
            format,
            algorithm,
            side,
            handshake,
            application: None,
        };
        schedule
    }

    pub fn dtls12(epoch: u16, algorithm: AeadAlg, local: Side, keys: Option<Dtls12Keys>) -> Self {
        let packet_keys = |side| {
            keys.as_ref().map_or(DirectionKeys::Opaque, |keys| {
                DirectionKeys::Live(Tls13PacketKeys::new(keys.payload(side), keys.iv(side)))
            })
        };
        let mut schedule = Self::single(
            APPLICATION,
            Epoch::new(
                NonceFormat::Dtls12 { epoch },
                algorithm,
                packet_keys(local),
                packet_keys(local.peer()),
            ),
        );
        schedule.kind = Kind::Dtls12 {
            algorithm,
            local,
            keys,
            srtp_issued: false,
        };
        schedule
    }

    pub fn check_srtp(&self) -> Result<(), IssueError> {
        match self.kind {
            Kind::Dtls12 {
                srtp_issued: false, ..
            } => Ok(()),
            Kind::Dtls12 {
                srtp_issued: true, ..
            } => Err(IssueError::Issued),
            _ => Err(IssueError::Invalid),
        }
    }

    pub fn srtp(&mut self, profile: SrtpProfile) -> Self {
        let Kind::Dtls12 {
            local,
            keys,
            srtp_issued,
            ..
        } = &mut self.kind
        else {
            unreachable!("the caller validated the parent schedule");
        };
        let directions = keys
            .as_mut()
            .map(|keys| keys.take_srtp(profile, *local).unwrap().map(Some))
            .unwrap_or([None, None]);
        *srtp_issued = true;
        Self::srtp_spaces(profile, directions, [true, true])
    }

    fn import_srtp(
        profile: SrtpProfile,
        authority: ImportUse,
        bytes: Option<&[u8]>,
    ) -> Result<Self, ImportError> {
        let sending = match authority {
            ImportUse::SrtpSend => true,
            ImportUse::SrtpReceive => false,
            _ => return Err(ImportError::Denied),
        };
        let keys = bytes
            .map(|bytes| SrtpKeys::import(profile, authority, bytes))
            .transpose()?;
        let directions = if sending { [keys, None] } else { [None, keys] };
        Ok(Self::srtp_spaces(profile, directions, [sending, !sending]))
    }

    fn srtp_spaces(
        profile: SrtpProfile,
        directions: [Option<SrtpKeys>; 2],
        permitted: [bool; 2],
    ) -> Self {
        let per_space = |keys: Option<SrtpKeys>, allowed: bool| match keys {
            Some(keys) => keys.into_spaces().map(|keys| {
                if let Some((_, payload, iv)) = keys.aead_material() {
                    DirectionKeys::Live(Tls13PacketKeys::new(payload, iv))
                } else {
                    DirectionKeys::Cm(keys)
                }
            }),
            None => std::array::from_fn(|_| {
                if allowed {
                    DirectionKeys::Opaque
                } else {
                    DirectionKeys::Missing
                }
            }),
        };
        let [tx, rx] = directions;
        let [rtp_tx, rtcp_tx] = per_space(tx, permitted[0]);
        let [rtp_rx, rtcp_rx] = per_space(rx, permitted[1]);
        let mut schedule = Self::single(
            3,
            Epoch::srtp(NonceFormat::SrtpRtp, profile, rtp_tx, rtp_rx),
        );
        schedule.spaces[4] = Some(Box::new(Epoch::srtp(
            NonceFormat::SrtpRtcp,
            profile,
            rtcp_tx,
            rtcp_rx,
        )));
        schedule
    }

    fn import_traffic(
        format: NonceFormat,
        algorithm: AeadAlg,
        authority: ImportUse,
        bytes: Option<&[u8]>,
    ) -> Result<Self, ImportError> {
        let labels = tls13_labels(format).ok_or(ImportError::Invalid)?;
        let local = match authority {
            ImportUse::TrafficSend => true,
            ImportUse::TrafficReceive => false,
            _ => return Err(ImportError::Denied),
        };
        let (epoch, traffic) = match bytes {
            Some(bytes) => {
                let traffic =
                    Tls13Direction::new(labels, algorithm, bytes).ok_or(ImportError::Invalid)?;
                let epoch =
                    Epoch::directional(format, algorithm, local, Some(traffic.packet_keys()));
                (epoch, Some(traffic))
            }
            None => (Epoch::directional(format, algorithm, local, None), None),
        };
        let mut schedule = Self::single(APPLICATION, epoch);
        schedule.kind = Kind::TrafficImport { local, traffic };
        Ok(schedule)
    }

    pub fn epoch(&mut self, space: u64) -> Result<&mut Epoch<C>, IssueError> {
        self.space_error(space)?;
        Ok(self.spaces[space as usize].as_deref_mut().unwrap())
    }

    pub fn view_epoch(&self, space: u64) -> Result<&Epoch<C>, IssueError> {
        self.space_error(space)?;
        Ok(self.spaces[space as usize].as_deref().unwrap())
    }

    pub fn check_sealer_pair(
        &mut self,
        first: u64,
        second: u64,
        domain: u64,
    ) -> Result<SealerPair<'_, C>, IssueError> {
        if first == second {
            return Err(IssueError::Invalid);
        }
        self.view_epoch(first)?.check_sealer(domain)?;
        self.view_epoch(second)?.check_sealer(domain)?;
        let [first, second] = self
            .spaces
            .get_disjoint_mut([first as usize, second as usize])
            .unwrap();
        Ok(SealerPair {
            epochs: [
                first.as_deref_mut().unwrap(),
                second.as_deref_mut().unwrap(),
            ],
            domain,
        })
    }

    fn space_error(&self, space: u64) -> Result<(), IssueError> {
        self.spaces
            .get(space as usize)
            .and_then(Option::as_ref)
            .map(|_| ())
            .ok_or(IssueError::Invalid)
    }

    pub fn into_ledger(self) -> impl Iterator<Item = C> {
        self.spaces
            .into_iter()
            .flatten()
            .flat_map(|epoch| epoch.into_ledger())
    }

    pub fn check_application(&self, hash: &[u8]) -> Result<(), IssueError> {
        let Kind::Tls13 { algorithm, .. } = self.kind else {
            return Err(IssueError::Invalid);
        };
        if self.spaces[APPLICATION].is_some()
            || hash.len() != TlsHash::for_aead(algorithm).output_len()
        {
            return Err(IssueError::Invalid);
        }
        Ok(())
    }

    pub fn application(&mut self, hash: &[u8]) {
        let Kind::Tls13 {
            format,
            algorithm,
            side,
            handshake,
            application,
        } = &mut self.kind
        else {
            unreachable!("application admitted on a TLS 1.3 schedule")
        };
        *application = handshake
            .as_mut()
            .map(|handshake| handshake.application(hash).unwrap());
        self.spaces[APPLICATION] = Some(Box::new(Epoch::traffic(
            *format,
            *algorithm,
            *side,
            application.as_ref(),
        )));
    }

    pub fn check_update(&self, local: bool) -> Result<(), IssueError> {
        match &self.kind {
            Kind::TrafficImport {
                local: permitted, ..
            } if local == *permitted => Ok(()),
            Kind::Tls13 { .. } if self.spaces[APPLICATION].is_some() => Ok(()),
            _ => Err(IssueError::Invalid),
        }
    }

    pub fn key_update(&mut self, local: bool) -> impl Iterator<Item = C> {
        let keys = match &mut self.kind {
            Kind::Tls13 {
                side,
                algorithm,
                application,
                ..
            } => application.as_mut().map(|traffic| {
                let sending = if local { *side } else { side.peer() };
                traffic.update_packet_keys(sending, *algorithm)
            }),
            Kind::TrafficImport { traffic, .. } => traffic.as_mut().map(Tls13Direction::key_update),
            Kind::Fixed | Kind::Dtls12 { .. } => unreachable!("checked key update"),
        };
        self.spaces[APPLICATION]
            .as_mut()
            .unwrap()
            .key_update(local, keys)
    }

    pub fn check_retire(&self, space: u64) -> Result<(), IssueError> {
        if space != HANDSHAKE as u64 || !matches!(self.kind, Kind::Tls13 { .. }) {
            return Err(IssueError::Invalid);
        }
        if self.spaces[HANDSHAKE].as_ref().unwrap().retired() {
            return Err(IssueError::Spent);
        }
        Ok(())
    }

    pub fn retire(&mut self) -> impl Iterator<Item = C> {
        if let Kind::Tls13 {
            handshake: Some(handshake),
            ..
        } = &mut self.kind
        {
            handshake.retire();
        }
        self.spaces[HANDSHAKE].as_mut().unwrap().retire_entries()
    }

    pub fn finished_size(&self, transcript_hash: &[u8]) -> Result<usize, IssueError> {
        let (algorithm, dtls12) = match self.kind {
            Kind::Tls13 { algorithm, .. } => (algorithm, false),
            Kind::Dtls12 { algorithm, .. } => (algorithm, true),
            _ => return Err(IssueError::Invalid),
        };
        let size = TlsHash::for_aead(algorithm).output_len();
        if transcript_hash.len() != size {
            return Err(IssueError::Invalid);
        }
        if dtls12 {
            Ok(12)
        } else {
            self.check_retire(HANDSHAKE as u64)?;
            Ok(size)
        }
    }

    pub fn verify_finished(&self, of: Side, hash: &[u8], received: &[u8], size: usize) -> bool {
        use subtle::ConstantTimeEq;
        let mut computed = [0u8; 48];
        let computed = &mut computed[..size];
        self.finished(of, hash, computed);
        bool::from(computed.ct_eq(received))
    }

    pub fn finished(&self, of: Side, transcript_hash: &[u8], out: &mut [u8]) {
        match &self.kind {
            Kind::Tls13 {
                handshake: Some(handshake),
                ..
            } => {
                handshake.finished(of, transcript_hash, out).unwrap();
            }
            Kind::Dtls12 {
                keys: Some(keys), ..
            } => {
                out.copy_from_slice(&keys.finished(of, transcript_hash).unwrap());
            }
            _ => unreachable!("Finished uses a live schedule with admitted operands"),
        }
    }
}
