//! Native failure coordinates. Each marker is a ULEB128 byte distance from
//! the preceding marker, shifted left once. The low bit is one for a newline
//! and zero for a UTF-8 continuation byte. Markers denote the byte after the
//! event, so an offset landing on a newline is still on its original line.
pub const TAG: u8 = 0xff;
pub const POINTS_TAG: u8 = 0xfe;
/// The linked entry checks the native runtime ABI through its source table decoder.
pub const POINTS_TABLE_SYMBOL: &str = "hirsel_core_annex_source_table_v4";
pub const TABLE_SYMBOL: &str = "hirsel_core_relative_source_table_v3";
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
        let marker = ((after - previous) << 1) | usize::from(newline);
        previous = after;
        push_uleb(&mut output, marker as u64);
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

const POINTS_PER_BLOCK: usize = 64;
const BLOCK_OFFSET_BYTES: usize = size_of::<u64>();

/// Encode sorted, unique positions in site order. A directory gives the
/// offset of each block relative to the byte after the tag. Inside a block,
/// a ULEB128 word with low bit zero advances the column by the upper bits.
/// A word with low bit one advances the line and takes the next word as its
/// absolute column. Each block starts from line and column zero.
pub fn encode_points(points: &[(u32, u32)]) -> Vec<u8> {
    let directory = points.len().div_ceil(POINTS_PER_BLOCK) * BLOCK_OFFSET_BYTES;
    let mut output = Vec::with_capacity(1 + directory + points.len());
    output.resize(1 + directory, 0);
    output[0] = POINTS_TAG;
    for (block, points) in points.chunks(POINTS_PER_BLOCK).enumerate() {
        let offset = (output.len() - 1) as u64;
        let slot = 1 + block * BLOCK_OFFSET_BYTES;
        output[slot..slot + BLOCK_OFFSET_BYTES].copy_from_slice(&offset.to_le_bytes());
        let (mut last_line, mut last_column) = (0, 0);
        for &(line, column) in points {
            if line == last_line {
                push_uleb(&mut output, u64::from(column - last_column) << 1);
            } else {
                push_uleb(&mut output, (u64::from(line - last_line) << 1) | 1);
                push_uleb(&mut output, u64::from(column));
            }
            (last_line, last_column) = (line, column);
        }
    }
    output
}

/// Resolve a linked site's ordinal in a generated point table after its tag.
pub fn point_position(points: &[u8], ordinal: usize) -> (usize, usize) {
    let slot = ordinal / POINTS_PER_BLOCK * BLOCK_OFFSET_BYTES;
    let offset = u64::from_le_bytes(points[slot..slot + BLOCK_OFFSET_BYTES].try_into().unwrap());
    let mut bytes = &points[offset as usize..];
    let (mut line, mut column) = (0, 0);
    for _ in 0..=ordinal % POINTS_PER_BLOCK {
        let marker = take_uleb(&mut bytes);
        let delta = marker >> 1;
        if marker & 1 == 0 {
            column += delta;
        } else {
            line += delta;
            column = take_uleb(&mut bytes);
        }
    }
    (line as usize, column as usize)
}

fn push_uleb(output: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        output.push((value as u8) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn take_uleb(bytes: &mut &[u8]) -> u64 {
    let mut value = 0;
    let mut shift = 0;
    loop {
        let (&byte, remaining) = bytes.split_first().unwrap();
        *bytes = remaining;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return value;
        }
        shift += 7;
    }
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
    fn finds_annex_positions_by_ordinal() {
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
