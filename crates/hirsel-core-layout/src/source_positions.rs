//! Native failure coordinates. Each marker is a ULEB128 byte distance from
//! the preceding marker, shifted left once. The low bit is one for a newline
//! and zero for a UTF-8 continuation byte. Markers denote the byte after the
//! event, so an offset landing on a newline is still on its original line.
pub const TAG: u8 = 0xff;
pub const POINTS_TAG: u8 = 0xfe;
/// The linked entry checks the native runtime ABI through its source table decoder.
pub const POINTS_TABLE_SYMBOL: &str = "hirsel_core_annex_source_table_v6";
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

const POINTS_PER_BLOCK: u32 = 64;
const BLOCK_ROW_BYTES: usize = size_of::<u32>() + size_of::<u64>();
const INLINE_COLUMNS: u64 = 64;

/// Encode coordinates ordered by their original, unique ordinals. The
/// directory names only nonempty blocks of 64 ordinals. Each block's
/// presence mask is followed by the coordinates for its set bits. A byte from 1
/// through 63 advances the column on the same line. Bytes 64 through 255
/// advance the line by the upper two bits and give the one-based column
/// in the lower six bits plus one. Zero introduces a ULEB128 word whose low
/// bit selects a column delta or a line delta with a following absolute column.
/// Each block starts from line and column zero.
pub fn encode_points(points: &[(u32, (u32, u32))]) -> Vec<u8> {
    let blocks = points.chunk_by(|a, b| a.0 / POINTS_PER_BLOCK == b.0 / POINTS_PER_BLOCK);
    let count = blocks.clone().count();
    let header = 1 + size_of::<u32>();
    let directory = count * BLOCK_ROW_BYTES;
    let mut output = vec![0; header + directory];
    output[0] = POINTS_TAG;
    output[1..header].copy_from_slice(&(count as u32).to_le_bytes());
    for (row, points) in blocks.enumerate() {
        let block = points[0].0 / POINTS_PER_BLOCK;
        let offset = (output.len() - 1) as u64;
        let slot = header + row * BLOCK_ROW_BYTES;
        output[slot..slot + 4].copy_from_slice(&block.to_le_bytes());
        output[slot + 4..slot + BLOCK_ROW_BYTES].copy_from_slice(&offset.to_le_bytes());
        let mask = points.iter().fold(0u64, |mask, (ordinal, _)| {
            mask | (1 << (ordinal % POINTS_PER_BLOCK))
        });
        output.extend_from_slice(&mask.to_le_bytes());
        let (mut last_line, mut last_column) = (0, 0);
        for &(_, (line, column)) in points {
            let lines = u64::from(line - last_line);
            let column_value = u64::from(column);
            if lines == 0 {
                let delta = u64::from(column - last_column);
                if (1..INLINE_COLUMNS).contains(&delta) {
                    output.push(delta as u8);
                } else {
                    output.push(0);
                    push_uleb(&mut output, delta << 1);
                }
            } else if lines <= 3 && (1..=INLINE_COLUMNS).contains(&column_value) {
                output.push((lines * INLINE_COLUMNS + column_value - 1) as u8);
            } else {
                output.push(0);
                push_uleb(&mut output, (lines << 1) | 1);
                push_uleb(&mut output, column_value);
            }
            (last_line, last_column) = (line, column);
        }
    }
    output
}

/// Resolve a linked site's ordinal in a generated point table after its tag.
pub fn point_position(points: &[u8], ordinal: usize) -> Option<(usize, usize)> {
    let ordinal = u32::try_from(ordinal).ok()?;
    let count = u32::from_le_bytes(points[..4].try_into().unwrap()) as usize;
    let (rows, _) = points[4..4 + count * BLOCK_ROW_BYTES].as_chunks::<BLOCK_ROW_BYTES>();
    let row = rows
        .binary_search_by_key(&(ordinal / POINTS_PER_BLOCK), |row| {
            u32::from_le_bytes(row[..4].try_into().unwrap())
        })
        .ok()?;
    let offset = u64::from_le_bytes(rows[row][4..].try_into().unwrap()) as usize;
    let mask = u64::from_le_bytes(points[offset..offset + 8].try_into().unwrap());
    let bit = 1u64 << (ordinal % POINTS_PER_BLOCK);
    if mask & bit == 0 {
        return None;
    }
    let mut bytes = &points[offset + 8..];
    let (mut line, mut column) = (0, 0);
    for _ in 0..=(mask & (bit - 1)).count_ones() {
        let (&code, rest) = bytes.split_first().unwrap();
        bytes = rest;
        let code = u64::from(code);
        if code == 0 {
            let marker = take_uleb(&mut bytes);
            let delta = marker >> 1;
            if marker & 1 == 0 {
                column += delta;
            } else {
                line += delta;
                column = take_uleb(&mut bytes);
            }
        } else if code < INLINE_COLUMNS {
            column += code;
        } else {
            line += code / INLINE_COLUMNS;
            column = code % INLINE_COLUMNS + 1;
        }
    }
    Some((line as usize, column as usize))
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
            let points = points
                .iter()
                .copied()
                .enumerate()
                .map(|(ordinal, point)| (ordinal as u32, point))
                .collect::<Vec<_>>();
            let encoded = encode_points(&points);
            for &(ordinal, (line, column)) in points.iter().rev() {
                assert_eq!(
                    point_position(&encoded[1..], ordinal as usize),
                    Some((line as usize, column as usize))
                );
            }
        }
    }
}
