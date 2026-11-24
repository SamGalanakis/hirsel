use super::*;

/// Whether a reply variant answers the call's operation (core.md, Handles).
pub(super) fn answers(vocab: Vocab, op: u64, reply: u64) -> bool {
    match vocab {
        Vocab::Exec => unreachable!("Exec replies use their own matrix"),
        Vocab::Stream => matches!(reply, R_DONE | R_WRITE_FAILED),
        Vocab::Dir => {
            (reply == R_FAILED && op != DIR_RELEASED)
                || match op {
                    0 | DIR_MKSTEMP => reply == R_FILE,
                    1 | 2 => reply == R_DIR,
                    3 | 14 => reply == R_STAT,
                    9 => reply == R_ENTRIES,
                    10 | 11 | 19 => reply == R_COUNT,
                    12 | 13 => matches!(reply, R_DONE | R_WRITE_FAILED),
                    16 => reply == R_LOCKED,
                    DIR_ZERO_EXTENSION => reply == R_ZERO_EXTENSION,
                    _ => reply == R_DONE,
                }
        }
        Vocab::Net => {
            (reply == R_FAILED && op != NET_RELEASED)
                || match op {
                    NET_CONNECT | NET_ACCEPT => reply == R_SOCKET,
                    NET_LISTEN => reply == R_LISTENER,
                    NET_RESOLVE | NET_RECV | NET_SEND => reply == R_COUNT,
                    NET_HANDSHAKE => reply == R_SECURED,
                    NET_SEND_SECRET | NET_READY => reply == R_DONE,
                    NET_LOCAL_ADDR | NET_PEER_ADDR => reply == R_ADDRESS,
                    _ => false,
                }
        }
    }
}
