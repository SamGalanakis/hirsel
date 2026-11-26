//! Lower-case hexadecimal encoding shared by store, launch and diagnostic paths.

/// Encode bytes as lower-case hexadecimal.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    write(bytes, &mut out).expect("writing to String");
    out
}

/// Decode hexadecimal in either case. `None` means an odd length or a non-hex digit.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        out.push(digit(pair[0])? * 16 + digit(pair[1])?);
    }
    Some(out)
}

fn digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn write(bytes: &[u8], out: &mut impl std::fmt::Write) -> std::fmt::Result {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        out.write_char(DIGITS[(byte >> 4) as usize] as char)?;
        out.write_char(DIGITS[(byte & 0x0f) as usize] as char)?;
    }
    Ok(())
}
