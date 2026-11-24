use super::{Args, Provider, Request, put_varint};

pub(super) fn inline_sink(op: u8) -> Option<Provider> {
    use crate::provider::{MEDIA_AGREEMENT, MEDIA_P256_KEY, SCRAM_VERIFY, SECRET_KDF, SECRET_MAC};
    Some(match op {
        SECRET_MAC => crate::secret::MAC_PROVIDER,
        SECRET_KDF => crate::secret::kdf::KDF_PROVIDER,
        MEDIA_AGREEMENT => crate::secret::agreement::AGREEMENT_PROVIDER,
        crate::provider::MEDIA_CAPTURE => crate::media::capture::PROVIDER,
        MEDIA_P256_KEY => crate::secret::p256_generation::PROVIDER,
        crate::provider::MEDIA_KEY_SHARE => crate::secret::key_share::PROVIDER,
        SCRAM_VERIFY => crate::scram::VERIFY_PROVIDER,
        _ => return None,
    })
}

/// # Safety
/// A media capture request pins its descriptor through delivery.
pub(super) unsafe fn designation_delivery(r: &Request) -> bool {
    use crate::provider::{MEDIA_CAPTURE, MEDIA_KEY_SHARE, MEDIA_P256_KEY, SECRET_KDF};
    if r.operation == MEDIA_CAPTURE {
        // SAFETY: the descriptor is pinned and the tag identifies its layout.
        return unsafe { crate::media::capture::creates_child(r) };
    }
    matches!(r.operation, SECRET_KDF | MEDIA_P256_KEY | MEDIA_KEY_SHARE)
}

/// A key operation's canonical arguments: each Secret's origin and policy,
/// never its bytes, then the operation's public operands and the spare.
pub(super) unsafe fn key_args(r: &Request) -> Args {
    use crate::secret::key::KeyOp;
    // SAFETY: the key adapter pins its request for the issue.
    let key = unsafe { &*r.path.cast::<crate::secret::key::KeyRequest<'_>>() };
    let mut head = vec![r.mode];
    let mut texts = Vec::new();
    let mut secret = |head: &mut Vec<u8>, s: &crate::secret::Secret| {
        let (kind, ordinal) = s.origin.wire();
        head.push(kind);
        put_varint(head, ordinal);
        texts.push(s.policy.text().into_bytes());
    };
    secret(&mut head, key.key);
    let mut aead_inputs = None;
    let operand: &[u8] = match &key.op {
        KeyOp::Wrap(s) => {
            secret(&mut head, s);
            &[]
        }
        KeyOp::Unwrap { blob, uses, .. } => {
            texts.push(uses.text().into_bytes());
            blob
        }
        KeyOp::Sign {
            alg, random, msg, ..
        } => {
            head.push(alg.case());
            put_varint(&mut head, *random);
            put_varint(&mut head, (r.buffer.capacity - r.buffer.len) as u64);
            msg
        }
        KeyOp::Aead { input, .. } => {
            head.push(input.alg.case());
            put_varint(&mut head, input.nonce.len() as u64);
            put_varint(&mut head, input.aad.len() as u64);
            aead_inputs = Some([input.nonce, input.aad]);
            input.bytes
        }
    };
    for text in &texts {
        put_varint(&mut head, text.len() as u64);
    }
    put_varint(&mut head, operand.len() as u64);
    let mut parts: Vec<&[u8]> = vec![&head];
    parts.extend(texts.iter().map(Vec::as_slice));
    parts.extend(aead_inputs.into_iter().flatten());
    parts.push(operand);
    Args::of_parts(&parts)
}

/// A derivation records designations and public inputs only.
pub(super) unsafe fn kdf_args(r: &Request) -> Args {
    // SAFETY: the derivation adapter pins its request for issue.
    let kdf = unsafe { &*r.path.cast::<crate::secret::kdf::KdfRequest<'_>>() };
    let (kind, ordinal) = kdf.secret.origin.wire();
    let policy = kdf.secret.policy.text();
    let child = kdf.child.text();
    use hirsel_core_net::secret_kdf::Derivation;
    let mut head = Vec::new();
    // Keep the HMAC argument encoding. New mode tags are disjoint
    // from the origin kinds which begin its arguments.
    if r.mode != 0 {
        head.push(r.mode);
    }
    head.push(kind);
    put_varint(&mut head, ordinal);
    put_varint(&mut head, policy.len() as u64);
    put_varint(&mut head, child.len() as u64);
    put_varint(&mut head, kdf.salt_or_prefix.len() as u64);
    match kdf.derivation.as_ref().unwrap() {
        Derivation::Hmac => {}
        Derivation::Hkdf { len } => put_varint(&mut head, *len as u64),
        Derivation::Argon2id { len, cost, .. } => {
            put_varint(&mut head, *len as u64);
            for value in cost {
                put_varint(&mut head, u64::from(*value));
            }
        }
    }
    Args::of_parts(&[
        &head,
        policy.as_bytes(),
        child.as_bytes(),
        kdf.salt_or_prefix,
        kdf.info_or_data,
    ])
}
