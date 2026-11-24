//! TLS agreement constructors consume a share only after admitting both charges.
use super::{
    schedule::{Schedule, admit, before_issue, owned, refusal, track},
    take,
};
use crate::{
    Context,
    provider::{Buffer, IoError, Outcome},
    secret::{agreement, key_share::Share},
};
use hirsel_core_net::{
    packet_epoch::{Epoch, IssueError, SCHEDULE_CHARGE, tls13_labels, traffic_profile},
    packet_nonce::NonceFormat,
    tls_kdf::{Side, Tls13Handshake, TlsHash},
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
    let registry_charge = track(
        unsafe { &*context },
        schedule_charge.partition(REGISTRY_CHARGE),
    );
    let Share {
        mut key,
        ticket,
        origin,
        charge,
    } = unsafe { take::<Share>(share) };
    drop(charge);
    let mut pending = Some((ticket, registry_charge));
    let opaque = origin.0 != 0;
    let canonical = {
        #[cfg(feature = "record")]
        {
            let mut fields = vec![0, profile as u8, suite as u8, side as u8, origin.0];
            fields.extend_from_slice(&origin.1.to_le_bytes());
            fields.extend_from_slice(&hello_hash);
            fields.extend_from_slice(peer);
            fields
        }
        #[cfg(not(feature = "record"))]
        {
            []
        }
    };
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
    let labels = tls13_labels(format).unwrap();
    let handshake =
        shared.map(|shared| Tls13Handshake::new(hash, labels, &shared, hello_hash).unwrap());
    let format = if format == NonceFormat::QuicMultipath {
        NonceFormat::Quic
    } else {
        format
    };
    let epoch = Epoch::traffic(
        format,
        algorithm,
        side,
        handshake.as_ref().map(Tls13Handshake::traffic),
    );
    owned(Schedule {
        epoch,
        space: 1,
        origin: if opaque { answer.written } else { 0 },
        _handshake: handshake,
        _charge: track(unsafe { &*context }, schedule_charge),
    })
}
