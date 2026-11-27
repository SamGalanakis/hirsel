//! Native failure coordinates. Each marker is a ULEB128 byte distance from
//! the preceding marker, shifted left once. The low bit is one for a newline
//! and zero for a UTF-8 continuation byte. Markers denote the byte after the
//! event, so an offset landing on a newline is still on its original line.
pub const TAG: u8 = 0xff;
pub const POINTS_TAG: u8 = 0xfe;
/// Size of one `SourceFile` directory row in bytes (`path: (u64, u64)`, `source: (u64, u64)`, `start: u64`).
pub const SOURCE_ROW_BYTES: usize = 40;
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

fn try_take_uleb(bytes: &mut &[u8]) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let (&byte, remaining) = bytes.split_first()?;
        *bytes = remaining;
        if shift >= 64 || (shift == 63 && byte > 1) {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Some(value);
        }
        shift += 7;
    }
}

/// Encode `(path, start, encoded_positions_or_utf8_source)` rows into the
/// self-contained binary format carried by `.hirsel-mod` section 8 (`sites`).
/// Empty slice encodes to an empty vector.
pub fn encode_source_table(rows: &[(&str, u64, &[u8])]) -> Vec<u8> {
    if rows.is_empty() {
        return Vec::new();
    }
    let count = rows.len();
    let mut body = vec![0u8; SOURCE_ROW_BYTES * count];
    for (index, &(path, start, positions)) in rows.iter().enumerate() {
        let row = SOURCE_ROW_BYTES * index;
        body[row + 32..row + SOURCE_ROW_BYTES].copy_from_slice(&start.to_le_bytes());
        for (view, content) in [path.as_bytes(), positions].into_iter().enumerate() {
            let field = row + 16 * view;
            let offset = body.len() as u64;
            body[field..field + 8].copy_from_slice(&offset.to_le_bytes());
            body[field + 8..field + 16].copy_from_slice(&(content.len() as u64).to_le_bytes());
            body.extend_from_slice(content);
        }
    }
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&(count as u64).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Validate `.hirsel-mod` section 8 (`sites`), returning `(file_count, body_bytes)`
/// where `body_bytes` begins with `file_count` 40-byte `SourceFile` rows whose
/// offsets are relative to the start of `body_bytes`.
pub fn validate_source_table(bytes: &[u8]) -> Result<(usize, &[u8]), &'static str> {
    if bytes.is_empty() {
        return Ok((0, &[]));
    }
    if bytes.len() < 8 {
        return Err("truncated source table header");
    }
    let count = usize::try_from(u64::from_le_bytes(bytes[..8].try_into().unwrap()))
        .map_err(|_| "source file count overflow")?;
    let body = &bytes[8..];
    if count == 0 {
        return if body.is_empty() {
            Ok((0, &[]))
        } else {
            Err("trailing bytes in empty source table")
        };
    }
    let rows_bytes = count
        .checked_mul(SOURCE_ROW_BYTES)
        .ok_or("source row table overflow")?;
    if body.len() < rows_bytes {
        return Err("truncated source file rows");
    }
    let word = |slice: &[u8], at: usize| u64::from_le_bytes(slice[at..at + 8].try_into().unwrap());
    let mut prev_start = 0u64;
    for i in 0..count {
        let row = &body[i * SOURCE_ROW_BYTES..(i + 1) * SOURCE_ROW_BYTES];
        let path_off = usize::try_from(word(row, 0)).map_err(|_| "path offset overflow")?;
        let path_len = usize::try_from(word(row, 8)).map_err(|_| "path length overflow")?;
        let src_off = usize::try_from(word(row, 16)).map_err(|_| "source offset overflow")?;
        let src_len = usize::try_from(word(row, 24)).map_err(|_| "source length overflow")?;
        let start = word(row, 32);
        if start == 0 || start >= SITE_END || (i > 0 && start <= prev_start) {
            return Err("invalid or unsorted source site start");
        }
        prev_start = start;
        let path_end = path_off
            .checked_add(path_len)
            .ok_or("path range overflow")?;
        if path_off < rows_bytes || path_end > body.len() {
            return Err("path range outside source table");
        }
        if std::str::from_utf8(&body[path_off..path_end]).is_err() {
            return Err("non-utf8 source path");
        }
        let src_end = src_off
            .checked_add(src_len)
            .ok_or("source range overflow")?;
        if src_off < rows_bytes || src_end > body.len() {
            return Err("source range outside source table");
        }
        let src = &body[src_off..src_end];
        if let Some(mut markers) = src.strip_prefix(&[TAG]) {
            while !markers.is_empty() {
                try_take_uleb(&mut markers).ok_or("malformed line markers")?;
            }
        } else if let Some(points) = src.strip_prefix(&[POINTS_TAG]) {
            validate_points(points)?;
        } else if std::str::from_utf8(src).is_err() {
            return Err("non-utf8 raw source text");
        }
    }
    Ok((count, body))
}

fn validate_points(points: &[u8]) -> Result<(), &'static str> {
    if points.len() < 4 {
        return Err("truncated point table header");
    }
    let count = u32::from_le_bytes(points[..4].try_into().unwrap()) as usize;
    let dir_end = 4usize
        .checked_add(
            count
                .checked_mul(BLOCK_ROW_BYTES)
                .ok_or("point directory overflow")?,
        )
        .ok_or("point directory overflow")?;
    if points.len() < dir_end {
        return Err("truncated point directory");
    }
    let (rows, _) = points[4..dir_end].as_chunks::<BLOCK_ROW_BYTES>();
    let mut prev_block = None;
    for row in rows {
        let block = u32::from_le_bytes(row[..4].try_into().unwrap());
        if prev_block.is_some_and(|prev| block <= prev) {
            return Err("unsorted point blocks");
        }
        prev_block = Some(block);
        let offset = usize::try_from(u64::from_le_bytes(row[4..].try_into().unwrap()))
            .map_err(|_| "point block offset overflow")?;
        let mask_end = offset.checked_add(8).ok_or("point block mask overflow")?;
        if offset < dir_end || mask_end > points.len() {
            return Err("point block outside table");
        }
        let mask = u64::from_le_bytes(points[offset..mask_end].try_into().unwrap());
        let mut rest = &points[mask_end..];
        for _ in 0..mask.count_ones() {
            let (&code, tail) = rest.split_first().ok_or("truncated point block stream")?;
            rest = tail;
            if code == 0 {
                let marker = try_take_uleb(&mut rest).ok_or("malformed point delta")?;
                if marker & 1 == 1 {
                    try_take_uleb(&mut rest).ok_or("malformed point column")?;
                }
            }
        }
    }
    Ok(())
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
