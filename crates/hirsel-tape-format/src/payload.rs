use super::{Corrupt, op};

pub fn carries_payload(op: u8) -> bool {
    use crate::op::*;
    matches!(
        op,
        READ | READ_AT
            | MKDTEMP
            | MKSTEMP
            | RANDOM
            | LISTEN
            | ACCEPT
            | LOCAL_ADDR
            | PEER_ADDR
            | RECV_BOUNDED
            | TLS_CONNECT
            | SECRET_MAC
            | MEDIA_P256_KEY
            | MEDIA_KEY_SHARE
            | CRED_FETCH
            | RESOLVE
            | STDIN_READ
            | NEXT_ENTRIES
            | STAT
            | STAT_FILE
            | EXEC_SPAWN
            | DATAGRAM_RECV
    )
}

/// A successful TLS_CONNECT carries a length-prefixed ALPN, the fixed peer
/// digest and a length-prefixed channel binding (zero length when absent).
pub fn tls_connect_payload(bytes: &[u8]) -> Result<(&[u8], &[u8; 32], &[u8]), Corrupt> {
    let (&alpn_len, rest) = bytes.split_first().ok_or(Corrupt)?;
    let (alpn, rest) = rest
        .split_at_checked(usize::from(alpn_len))
        .ok_or(Corrupt)?;
    let (peer, rest) = rest.split_first_chunk::<32>().ok_or(Corrupt)?;
    let (&binding_len, binding) = rest.split_first().ok_or(Corrupt)?;
    if !matches!(binding_len, 0 | 32 | 48 | 64) || usize::from(binding_len) != binding.len() {
        return Err(Corrupt);
    }
    Ok((alpn, peer, binding))
}
