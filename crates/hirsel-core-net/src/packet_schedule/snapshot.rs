//! Restore public schedule and packet state for keyless replay.
use super::{
    DirectionKeys, Epoch, ImportProfile, ImportUse, Kind, NonceFormat, Schedule, Snapshot, Space,
    SrtpProfile, traffic_profile,
};
use crate::{
    SframeDirection, packet_epoch::tls_suite, packet_protection::Algorithm, secret_aead::AeadAlg,
    tls_kdf::Side,
};

impl Snapshot {
    pub fn from_parts(
        application: bool,
        retired: u8,
        exported: bool,
        domains: Vec<(Space, u64)>,
    ) -> Option<Self> {
        if retired >> Space::ALL.len() != 0
            || domains
                .windows(2)
                .any(|w| (w[0].0.tag(), w[0].1) >= (w[1].0.tag(), w[1].1))
        {
            return None;
        }
        Some(Self {
            application,
            retired: Space::ALL.map(|space| retired & (1 << space.tag()) != 0),
            exported,
            domains,
        })
    }
}

impl<C> Schedule<C> {
    /// Restore the spaces, issuance ledger and transition guards with no key material.
    pub fn from_snapshot(
        [family, suite]: [u8; 2],
        side: u8,
        snapshot: Snapshot,
        send_block: Option<(u64, u8)>,
        uncharged: impl Fn() -> C,
    ) -> Option<Self> {
        let suite = u64::from(suite);
        let peer = match side {
            0 => Some(Side::Client),
            1 => Some(Side::Server),
            _ => None,
        };
        let mut schedule = match (family, side) {
            (0..=3, 0..=1) => {
                let (format, algorithm) = traffic_profile(family.into(), suite)?;
                let mut schedule = Self::tls13(format, algorithm, peer?, None);
                if snapshot.application {
                    schedule.spaces[Space::Application.index()] =
                        Some(Box::new(Epoch::traffic(format, algorithm, peer?, None)));
                }
                schedule
            }
            (0..=3, 2..=3) => {
                let (format, algorithm) = traffic_profile(family.into(), suite)?;
                let authority = if side == 2 {
                    ImportUse::TrafficSend
                } else {
                    ImportUse::TrafficReceive
                };
                Self::import_traffic(format, algorithm, authority, None).ok()?
            }
            // The epoch only contributes to the nonce bytes, which replay does not use.
            (4, 0..=1) => Self::dtls12(0, tls_suite(suite)?, peer?, None),
            (5, 0..=3) => {
                let permitted = [side != 3, side != 2];
                Self::srtp_spaces(SrtpProfile::from_case(suite)?, [None, None], permitted)
            }
            (6, 0..=1) if suite == 0 => Self::single(
                Space::Initial,
                Epoch::new(
                    NonceFormat::Quic,
                    AeadAlg::Aes128Gcm,
                    DirectionKeys::Opaque,
                    DirectionKeys::Opaque,
                ),
            ),
            (7, 2..=3) if suite == 0 => {
                let direction = match (side, send_block) {
                    (2, Some((kid, bits)))
                        if bits <= SframeDirection::MAX_BITS && kid & ((1 << bits) - 1) == 0 =>
                    {
                        SframeDirection::Send { kid, bits }
                    }
                    (3, None) => SframeDirection::Receive,
                    _ => return None,
                };
                ImportProfile::Sframe
                    .import(ImportUse::Sframe(direction), None)
                    .ok()?
            }
            _ => return None,
        };
        let application =
            schedule.spaces[Space::Application.index()].is_some() || matches!(family, 5 | 7);
        if snapshot.application != application || (send_block.is_some() && family != 7) {
            return None;
        }
        if snapshot.exported {
            let Kind::Dtls12 { srtp_issued, .. } = &mut schedule.kind else {
                return None;
            };
            *srtp_issued = true;
        }
        for space in Space::ALL {
            if snapshot.retired[space.index()] {
                schedule.check_retire(space).ok()?;
                drop(schedule.retire());
            }
        }
        for (space, domain) in snapshot.domains {
            if let Kind::Sframe(state) = &mut schedule.kind {
                if space != Space::Application {
                    return None;
                }
                state.restore_domain(domain, uncharged()).ok()?;
            } else {
                schedule
                    .epoch(space)
                    .ok()?
                    .restore_domain(domain, uncharged())
                    .ok()?;
            }
        }
        Some(schedule)
    }
}

/// The nonce range and cipher parameters preserved by a packet object's profile and space.
pub fn snapshot_packet_profile(
    [family, suite]: [u8; 2],
    space: Space,
) -> Option<(NonceFormat, Algorithm)> {
    let suite = u64::from(suite);
    Some(match (family, space) {
        (0..=3, Space::Handshake | Space::Application) => {
            let (mut format, algorithm) = traffic_profile(family.into(), suite)?;
            if format == NonceFormat::QuicMultipath && space == Space::Handshake {
                format = NonceFormat::Quic;
            }
            (format, Algorithm::Aead(algorithm))
        }
        (4, Space::Application) => (
            NonceFormat::Dtls12 { epoch: 0 },
            Algorithm::Aead(tls_suite(suite)?),
        ),
        (5, Space::Rtp | Space::Rtcp) => {
            let format = if space == Space::Rtp {
                NonceFormat::SrtpRtp
            } else {
                NonceFormat::SrtpRtcp
            };
            let algorithm = SrtpProfile::from_case(suite)?
                .algorithm()
                .map_or(Algorithm::Cm, Algorithm::Aead);
            (format, algorithm)
        }
        (6, Space::Initial) if suite == 0 => {
            (NonceFormat::Quic, Algorithm::Aead(AeadAlg::Aes128Gcm))
        }
        (7, Space::Application) if suite == 0 => {
            (NonceFormat::Sframe, Algorithm::Aead(AeadAlg::Aes128Gcm))
        }
        _ => return None,
    })
}
