//! Native failure coordinates. Each marker is a ULEB128 byte distance from
//! the preceding marker, shifted left once. The low bit is one for a newline
//! and zero for a UTF-8 continuation byte. Markers denote the byte after the
//! event, so an offset landing on a newline is still on its original line.
pub const TAG: u8 = 0xff;
pub const TABLE_SYMBOL: &str = "hirsel_core_relative_source_table_v2";
/// Every linked site word is below this bound, so code materialises it as a
/// sign-extended 32-bit immediate. A word is the file's first site plus the
/// byte offset into that file; 0 means no site.
pub const SITE_END: u64 = 1 << 31;

pub fn encode(text: &str) -> Vec<u8> {
    let mut output = vec![TAG];
    let mut previous = 0;
    for (index, byte) in text.bytes().enumerate() {
        let newline = byte == b'\n';
        if !newline && byte & 0xc0 != 0x80 {
            continue;
        }
        let after = index + 1;
        let mut marker = ((after - previous) << 1) | usize::from(newline);
        previous = after;
        while marker >= 0x80 {
            output.push((marker as u8) | 0x80);
            marker >>= 7;
        }
        output.push(marker as u8);
    }
    output
}

/// One-based line and Unicode scalar column at a byte boundary in the file.
/// The generated stream starts after its tag.
pub fn position(markers: &[u8], offset: usize) -> (usize, usize) {
    let (mut line, mut start, mut continuations, mut at) = (1, 0, 0, 0);
    let (mut marker, mut shift) = (0, 0);
    for &byte in markers {
        marker |= usize::from(byte & 0x7f) << shift;
        if byte >= 0x80 {
            shift += 7;
            continue;
        }
        at += marker >> 1;
        if at > offset {
            break;
        }
        if marker & 1 == 1 {
            line += 1;
            start = at;
            continuations = 0;
        } else {
            continuations += 1;
        }
        marker = 0;
        shift = 0;
    }
    (line, offset - start - continuations + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances_by_scalars_and_resets_after_lf() {
        let data = encode(&format!("{}λ\t𐀀\r\nβ", "a".repeat(68)));
        for (offset, expected) in [
            (68, (1, 69)),
            (70, (1, 70)),
            (71, (1, 71)),
            (76, (1, 73)),
            (77, (2, 1)),
            (79, (2, 2)),
        ] {
            assert_eq!(position(&data[1..], offset), expected);
        }
    }
}
