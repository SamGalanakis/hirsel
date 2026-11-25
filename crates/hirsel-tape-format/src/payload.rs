use super::Corrupt;

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
            | TLS_HELLO
            | TLS_ACCEPT
            | SECRET_MAC
            | MEDIA_P256_KEY
            | MEDIA_KEY_SHARE
            | MEDIA_CAPTURE
            | CRED_FETCH
            | RESOLVE
            | STDIN_READ
            | NEXT_ENTRIES
            | STAT
            | STAT_FILE
            | EXEC_SPAWN
            | DATAGRAM_RECV
            | UDP_OPEN
            | UDP_BIND
            | UDP_BIND_GROUP
            | UDP_RECV
            | UDP_LOCAL_ADDR
            | PORT_NEXT
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

/// A successful TLS_HELLO carries the normalised server name (TLS-9): 0 to
/// 253 bytes of `a`-`z`, `0`-`9`, `-`, `_` and `.`, empty for no name.
pub fn tls_hello_payload(bytes: &[u8]) -> Result<&[u8], Corrupt> {
    let valid = |&b: &u8| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.');
    if bytes.len() > 253 || !bytes.iter().all(valid) {
        return Err(Corrupt);
    }
    Ok(bytes)
}

/// A successful TLS_ACCEPT carries one byte: 0 for no protocol, or the
/// chosen entry's index in the server's list plus one (at most 16 entries).
pub fn tls_accept_payload(bytes: &[u8]) -> Result<Option<u8>, Corrupt> {
    match bytes {
        [0] => Ok(None),
        [n @ 1..=16] => Ok(Some(n - 1)),
        _ => Err(Corrupt),
    }
}

pub(super) fn address(bytes: &[u8]) -> Result<(), Corrupt> {
    let [family, rest @ ..] = bytes else {
        return Err(Corrupt);
    };
    if bytes.len() != 19
        || *family > 1
        || (*family == 0 && rest[..8].iter().chain(&rest[12..16]).any(|&n| n != 0))
    {
        return Err(Corrupt);
    }
    Ok(())
}
