//! Native failure coordinates. Each marker is a ULEB128 byte distance from
//! the preceding marker, shifted left once. The low bit is one for a newline
//! and zero for a UTF-8 continuation byte. Markers denote the byte after the
//! event, so an offset landing on a newline is still on its original line.
pub const TAG: u8 = 0xff;
pub const POINTS_TAG: u8 = 0xfe;
pub const POINTS_TABLE_SYMBOL: &str = "hirsel_core_annex_source_table_v2";
pub const TABLE_SYMBOL: &str = "hirsel_core_relative_source_table_v2";
/// Every linked site word is below this bound, so code materialises it as a
/// sign-extended 32-bit immediate. A word is the file's first site plus a
/// byte offset or annex position ordinal. Zero means no site.
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

/// Encode exact positions in site order. Each file picks the byte width of a
/// packed row and the number of low bits holding its column.
pub fn encode_points(points: &[(u32, u32)]) -> Vec<u8> {
    let (max_line, max_column) = points
        .iter()
        .fold((0u32, 0u32), |(lines, columns), &(line, column)| {
            (lines.max(line), columns.max(column))
        });
    let column_bits = u32::BITS - max_column.leading_zeros();
    let line_bits = u32::BITS - max_line.leading_zeros();
    let width = (line_bits + column_bits).div_ceil(8).max(1) as usize;
    let mut output = Vec::with_capacity(3 + points.len() * width);
    output.extend([POINTS_TAG, width as u8, column_bits as u8]);
    for &(line, column) in points {
        let packed = (u64::from(line) << column_bits) | u64::from(column);
        output.extend_from_slice(&packed.to_le_bytes()[..width]);
    }
    output
}

/// Resolve a linked site's ordinal in a generated point table after its tag.
pub fn point_position(points: &[u8], ordinal: usize) -> (usize, usize) {
    let width = usize::from(points[0]);
    let column_bits = points[1];
    let start = 2 + ordinal * width;
    let mut bytes = [0; 8];
    bytes[..width].copy_from_slice(&points[start..start + width]);
    let packed = u64::from_le_bytes(bytes);
    let column_mask = (1u64 << column_bits) - 1;
    (
        (packed >> column_bits) as usize,
        (packed & column_mask) as usize,
    )
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

    #[test]
    fn finds_packed_annex_positions_by_ordinal() {
        for points in [
            &[(1, 1), (8, 12), (15, 15)][..],
            &[(1, 128), (257, 1025)],
            &[(1, u32::MAX), (u32::MAX, 1), (u32::MAX, u32::MAX)],
        ] {
            let encoded = encode_points(points);
            for (ordinal, &(line, column)) in points.iter().enumerate().rev() {
                assert_eq!(
                    point_position(&encoded[1..], ordinal),
                    (line as usize, column as usize)
                );
            }
        }
    }
}
