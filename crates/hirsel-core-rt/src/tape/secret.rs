use super::{Args, Request, put_varint};

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
        KeyOp::Sign { alg, msg } => {
            head.push(alg.case());
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
