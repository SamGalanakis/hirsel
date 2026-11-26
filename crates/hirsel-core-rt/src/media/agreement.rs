//! TLS agreement constructors consume a share only after admitting both charges.
use super::{
    schedule::{Schedule, admit, before_issue, owned, refusal, track, track_as},
    take,
};
use crate::{
    Context,
    provider::{Buffer, IoError, Outcome},
    secret::{agreement, key_share::Share},
};
use hirsel_core_net::{
    packet_epoch::{IssueError, SCHEDULE_CHARGE, tls_suite, tls13_labels, traffic_profile},
    packet_schedule,
    tls_kdf::{Side, Tls13Handshake, TlsHash, dtls12::Dtls12Keys},
};

const REGISTRY_CHARGE: u64 = 48;

/// # Safety
/// The running context owns the share and pins the public byte operands.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_tls13(
    context: *mut Context,
    profile: u64,
    suite: u64,
    side: u64,
    share: u64,
    peer: *const Buffer,
    hello_hash: *const Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: the checked caller owns the share and lends both operands.
    unsafe { crate::process::deny_describe_capability(context) };
    let (peer, hello_hash) =
        unsafe { (crate::tls::bytes(&*peer), crate::tls::bytes(&*hello_hash)) };
    let Some((format, algorithm)) = traffic_profile(profile, suite) else {
        return before_issue(IssueError::Invalid);
    };
    let hash = TlsHash::for_aead(algorithm);
    if hello_hash.len() != hash.output_len() {
        return before_issue(IssueError::Invalid);
    }
    let request = Constructor {
        context,
        side,
        share,
        peer,
        site,
    };
    // SAFETY: the call pins the public inputs and owns its KeyShare.
    unsafe {
        request.issue(
            [profile as u8, suite as u8],
            &[&[0, profile as u8, suite as u8], hello_hash],
            |side, shared| {
                let labels = tls13_labels(format).unwrap();
                let handshake = shared
                    .map(|shared| Tls13Handshake::new(hash, labels, shared, hello_hash).unwrap());
                packet_schedule::Schedule::tls13(format, algorithm, side, handshake)
            },
        )
    }
}

/// # Safety
/// The running task lends the public carriers and transfers the KeyShare on issue.
/// A null session_hash encodes None.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_schedule_dtls12(
    context: *mut Context,
    suite: u64,
    side: u64,
    epoch: u16,
    share: u64,
    peer: *const Buffer,
    client: *const Buffer,
    server: *const Buffer,
    session_hash: *const Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: the checked caller pins the shared byte carriers and owns the share.
    unsafe { crate::process::deny_describe_capability(context) };
    let Some(algorithm) = tls_suite(suite) else {
        return before_issue(IssueError::Invalid);
    };
    let bytes = |carrier: *const Buffer| unsafe { crate::tls::bytes(&*carrier) };
    let (Ok(client), Ok(server)) = (
        <&[u8; 32]>::try_from(bytes(client)),
        <&[u8; 32]>::try_from(bytes(server)),
    ) else {
        return before_issue(IssueError::Invalid);
    };
    let session = (!session_hash.is_null()).then(|| bytes(session_hash));
    if session.is_some_and(|bytes| bytes.len() != TlsHash::for_aead(algorithm).output_len()) {
        return before_issue(IssueError::Invalid);
    }
    let request = Constructor {
        context,
        side,
        share,
        peer: bytes(peer),
        site,
    };
    let fields = [[1, suite as u8], epoch.to_le_bytes()];
    // SAFETY: the constructor pins the input carriers and uniquely owns its share.
    unsafe {
        request.issue(
            [4, suite as u8],
            &[
                &fields[0],
                &fields[1],
                client,
                server,
                session.unwrap_or(&[]),
            ],
            |side, shared| {
                let keys = shared.map(|shared| {
                    Dtls12Keys::new(algorithm, shared, *client, *server, session).unwrap()
                });
                packet_schedule::Schedule::dtls12(epoch, algorithm, side, keys)
            },
        )
    }
}

struct Constructor<'a> {
    context: *mut Context,
    side: u64,
    share: u64,
    peer: &'a [u8],
    site: u64,
}

impl Constructor<'_> {
    /// # Safety
    /// The caller pins the public inputs and owns the share. Pre-issue failures
    /// return the holder. The registry ticket stays live through the verdict.
    unsafe fn issue(
        self,
        profile: [u8; 2],
        fields: &[&[u8]],
        make: impl FnOnce(Side, Option<&[u8; 32]>) -> packet_schedule::Schedule<super::schedule::Paid>,
    ) -> Outcome {
        let Self {
            context,
            side,
            share,
            peer,
            site,
        } = self;
        let requested = SCHEDULE_CHARGE + REGISTRY_CHARGE;
        let mut schedule_charge = match admit(unsafe { &*context }, requested) {
            Ok(charge) => charge,
            Err(available) => {
                return before_issue(IssueError::Capacity {
                    requested,
                    available,
                });
            }
        };
        let registry_charge = track_as(
            unsafe { &*context },
            schedule_charge.partition(REGISTRY_CHARGE),
            crate::media::REGISTRY_ENTRY,
        );
        let Share {
            mut key,
            ticket,
            origin,
            charge,
            tape,
            ..
        } = unsafe { take::<Share>(share) };
        #[cfg(feature = "record")]
        let origin = tape.arrival().map_or(origin, |ordinal| (2, ordinal));
        #[cfg(not(feature = "record"))]
        let _ = tape;
        drop(charge);
        let mut pending = Some((ticket, registry_charge));
        let opaque = origin.0 != 0;
        let canonical = {
            #[cfg(feature = "record")]
            {
                let mut bytes = vec![side as u8, origin.0];
                bytes.extend_from_slice(&origin.1.to_le_bytes());
                for field in fields.iter().copied().chain(std::iter::once(peer)) {
                    bytes.extend_from_slice(&(field.len() as u64).to_le_bytes());
                    bytes.extend_from_slice(field);
                }
                bytes
            }
            #[cfg(not(feature = "record"))]
            {
                let _ = fields;
                []
            }
        };
        let side_wire = side as u8;
        let side = if side == 0 {
            Side::Client
        } else {
            Side::Server
        };
        // SAFETY: the call pins the public arguments, ticket, and exclusive share through issue.
        let (answer, mut shared) = unsafe {
            agreement::decide(
                context,
                &canonical,
                side,
                &mut pending,
                || key.take().and_then(|key| key.agree(peer)),
                site,
            )
        };
        if answer.status != 0 || answer.error != IoError::OK {
            return answer;
        }
        match answer.value {
            agreement::INVALID => return refusal(IssueError::Invalid),
            agreement::ISSUED => return refusal(IssueError::Issued),
            agreement::ACCEPTED => {}
            _ => unreachable!("validated agreement verdict"),
        }
        // On virtual replay the registry is skipped, and only the agreement and
        // its deterministic derivations run again.
        if let Some(key) = key {
            shared = key.agree(peer);
            if shared.is_none() {
                return refusal(IssueError::Invalid);
            }
        }
        owned(Schedule::new(
            make(side, shared.as_deref()),
            answer.written,
            opaque,
            track(unsafe { &*context }, schedule_charge),
            profile,
            side_wire,
        ))
    }
}
