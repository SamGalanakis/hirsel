//! `tls.send_secret`'s message: plain `wire` bytes with the secret and
//! length insertions that `marks` names (TLS-5, "The sink"). The runtime and
//! the evaluator check the marks from the arguments alone and build the
//! message in memory the program never sees.

/// Inserts `secrets[a]` unchanged.
pub const RAW: u64 = 0;
/// Inserts `secrets[a]` form-urlencoded (WHATWG URL 5.2).
pub const FORM: u64 = 1;
/// Inserts the decimal length of the range `a..b`.
pub const DECIMAL: u64 = 2;
/// Inserts the lowercase hexadecimal length of the range `a..b`.
pub const HEX: u64 = 3;

/// Whether `marks` is well formed for `wire_len` plain bytes and `secrets`
/// elements: groups of four words, offsets in order and inside `wire`, every
/// index naming an element, `b` zero for secret insertions, and no length
/// range holding the offset of a length group.
pub fn well_formed(marks: &[u64], wire_len: usize, secrets: usize) -> bool {
    if !marks.len().is_multiple_of(4) {
        return false;
    }
    let wire_len = wire_len as u64;
    let mut last = 0;
    for group in marks.chunks_exact(4) {
        let [at, kind, a, b] = [group[0], group[1], group[2], group[3]];
        if at < last || at > wire_len {
            return false;
        }
        last = at;
        let fits = match kind {
            RAW | FORM => a < secrets as u64 && b == 0,
            DECIMAL | HEX => {
                a <= b
                    && b <= wire_len
                    && !marks.chunks_exact(4).any(|other| {
                        matches!(other[1], DECIMAL | HEX) && (a..=b).contains(&other[0])
                    })
            }
            _ => false,
        };
        if !fits {
            return false;
        }
    }
    true
}

fn form_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .map(|&byte| if keeps(byte) || byte == b' ' { 1 } else { 3 })
        .sum()
}

fn keeps(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'*' | b'-' | b'.' | b'_')
}

fn digits(mut n: u64, base: u64) -> ([u8; 20], usize) {
    let mut out = [0u8; 20];
    let mut len = 0;
    loop {
        out[len] = b"0123456789abcdef"[(n % base) as usize];
        len += 1;
        n /= base;
        if n == 0 {
            break;
        }
    }
    out[..len].reverse();
    (out, len)
}

fn insertion_len(secrets: &[&[u8]], kind: u64, a: u64) -> usize {
    match kind {
        RAW => secrets[a as usize].len(),
        _ => form_len(secrets[a as usize]),
    }
}

/// The length of the range `a..b`: its plain bytes and the secret
/// insertions whose offset lies in `a..=b`.
fn range_len(secrets: &[&[u8]], marks: &[u64], a: u64, b: u64) -> u64 {
    let inserted: usize = marks
        .chunks_exact(4)
        .filter(|group| matches!(group[1], RAW | FORM) && (a..=b).contains(&group[0]))
        .map(|group| insertion_len(secrets, group[1], group[2]))
        .sum();
    (b - a) + inserted as u64
}

fn group_len(secrets: &[&[u8]], marks: &[u64], group: &[u64]) -> usize {
    match group[1] {
        RAW | FORM => insertion_len(secrets, group[1], group[2]),
        kind => {
            let base = if kind == DECIMAL { 10 } else { 16 };
            digits(range_len(secrets, marks, group[2], group[3]), base).1
        }
    }
}

/// The message's length, for reserving its storage exactly once. `marks`
/// must be well formed.
pub fn message_len(wire: &[u8], secrets: &[&[u8]], marks: &[u64]) -> usize {
    wire.len()
        + marks
            .chunks_exact(4)
            .map(|group| group_len(secrets, marks, group))
            .sum::<usize>()
}

/// Appends the message to `out`, which the caller reserved with
/// `message_len` so that no secret byte is left behind by a reallocation.
/// `marks` must be well formed.
pub fn compose(wire: &[u8], secrets: &[&[u8]], marks: &[u64], out: &mut Vec<u8>) {
    let mut from = 0;
    for group in marks.chunks_exact(4) {
        let at = group[0] as usize;
        out.extend_from_slice(&wire[from..at]);
        from = at;
        match group[1] {
            RAW => out.extend_from_slice(secrets[group[2] as usize]),
            FORM => {
                for &byte in secrets[group[2] as usize] {
                    if keeps(byte) {
                        out.push(byte);
                    } else if byte == b' ' {
                        out.push(b'+');
                    } else {
                        let hex = b"0123456789ABCDEF";
                        out.extend_from_slice(&[
                            b'%',
                            hex[(byte >> 4) as usize],
                            hex[(byte & 15) as usize],
                        ]);
                    }
                }
            }
            kind => {
                let base = if kind == DECIMAL { 10 } else { 16 };
                let (digits, len) = digits(range_len(secrets, marks, group[2], group[3]), base);
                out.extend_from_slice(&digits[..len]);
            }
        }
    }
    out.extend_from_slice(&wire[from..]);
}
