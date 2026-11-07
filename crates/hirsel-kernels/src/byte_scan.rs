//! Go strings.IndexAny and asciiSet, src/strings/strings.go:184-240,948-967
//! at dc70946c365d0c23dc2fccc2c3ff94c20c77e3b8 (BSD-3-Clause).
//! Byte extents use all 256 entries, including non-ASCII bytes.

/// First byte at or after `start` contained in `needles`, or `source.len()`.
/// An empty set or a start outside the extent has no match.
pub fn find_any(source: &[u8], needles: &[u8], start: u64) -> u64 {
    let Ok(start) = usize::try_from(start) else {
        return source.len() as u64;
    };
    let Some(rest) = source.get(start..) else {
        return source.len() as u64;
    };
    if needles.is_empty() {
        return source.len() as u64;
    }
    // Use the short-string walk for the prefix of a long extent too. Parsers
    // often stop at a nearby delimiter even when the remaining document is
    // long; only a longer unmatched run needs the membership table.
    let prefix = rest.len().min(8);
    if let Some(at) = rest[..prefix]
        .iter()
        .position(|byte| needles.contains(byte))
    {
        return (start + at) as u64;
    }
    if prefix == rest.len() {
        return source.len() as u64;
    }
    if needles.len() <= 3 {
        return short_set(&rest[prefix..], needles).map_or(source.len(), |at| start + prefix + at)
            as u64;
    }
    let mut set = [false; 256];
    for &byte in needles {
        set[usize::from(byte)] = true;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("ssse3") {
        // SAFETY: runtime feature detection admits SSSE3; the helper guards
        // every source load and its fixed lookup tables have sixteen bytes.
        return unsafe { any_set_ssse3(&rest[prefix..], &set) }
            .map_or(source.len(), |at| start + prefix + at) as u64;
    }
    rest[prefix..]
        .iter()
        .position(|&byte| set[usize::from(byte)])
        .map_or(source.len(), |at| start + prefix + at) as u64
}

// aho-corasick 1.1.4, src/packed/teddy/generic.rs:218-285: nibble
// lookup and lane masks. Two eight-bit tables encode exact byte membership,
// instead of Teddy's approximate multi-literal buckets and byte shifts.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn any_set_ssse3(source: &[u8], set: &[bool; 256]) -> Option<usize> {
    use std::arch::x86_64::*;
    let mut lower = [0u8; 16];
    let mut upper = [0u8; 16];
    for (byte, &present) in set.iter().enumerate() {
        if present {
            let row = if byte < 128 { &mut lower } else { &mut upper };
            row[byte & 15] |= 1 << ((byte >> 4) & 7);
        }
    }
    let bit_values: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    // SAFETY: each lookup table has exactly the sixteen bytes being read.
    let (low_table, high_table, bit_table) = unsafe {
        (
            _mm_loadu_si128(lower.as_ptr().cast()),
            _mm_loadu_si128(upper.as_ptr().cast()),
            _mm_loadu_si128(bit_values.as_ptr().cast()),
        )
    };
    let zero = _mm_setzero_si128();
    let nibble_mask = _mm_set1_epi8(15);
    let bit_mask = _mm_set1_epi8(7);
    let mut at = 0;
    while source.len() - at >= 16 {
        // SAFETY: the loop guard proves all sixteen bytes are in source.
        let block = unsafe { _mm_loadu_si128(source.as_ptr().add(at).cast()) };
        let indices = _mm_and_si128(block, nibble_mask);
        let low = _mm_shuffle_epi8(low_table, indices);
        let high = _mm_shuffle_epi8(high_table, indices);
        let halves = _mm_cmplt_epi8(block, zero);
        let memberships = _mm_or_si128(_mm_and_si128(halves, high), _mm_andnot_si128(halves, low));
        let bits = _mm_shuffle_epi8(
            bit_table,
            _mm_and_si128(_mm_srli_epi16::<4>(block), bit_mask),
        );
        let misses = _mm_cmpeq_epi8(_mm_and_si128(memberships, bits), zero);
        let mask = !(_mm_movemask_epi8(misses) as u32) & 0xffff;
        if mask != 0 {
            return Some(at + mask.trailing_zeros() as usize);
        }
        at += 16;
    }
    source[at..]
        .iter()
        .position(|&byte| set[usize::from(byte)])
        .map(|offset| at + offset)
}

// memchr 2.8.3, src/arch/x86_64/sse2/memchr.rs:71-143 and
// src/arch/generic/memchr.rs:290-321 (MIT/Unlicense). One vector/word at a
// time replaces upstream's four-vector unroll; the existing short prefix
// avoids vector setup for nearby parser delimiters.
fn short_set(source: &[u8], needles: &[u8]) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is part of the x86-64 baseline. All vector reads in
        // short_set_sse2 are guarded by the remaining source extent.
        unsafe { short_set_sse2(source, needles) }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        short_set_word(source, needles)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn short_set_sse2(source: &[u8], needles: &[u8]) -> Option<usize> {
    use std::arch::x86_64::*;
    let a = _mm_set1_epi8(needles[0] as i8);
    let b = _mm_set1_epi8(*needles.get(1).unwrap_or(&needles[0]) as i8);
    let c = _mm_set1_epi8(*needles.get(2).unwrap_or(&needles[0]) as i8);
    let mut at = 0;
    while source.len() - at >= 16 {
        // SAFETY: the loop guard proves all sixteen bytes are in source.
        let block = unsafe { _mm_loadu_si128(source.as_ptr().add(at).cast()) };
        let equal = _mm_or_si128(
            _mm_cmpeq_epi8(block, a),
            _mm_or_si128(_mm_cmpeq_epi8(block, b), _mm_cmpeq_epi8(block, c)),
        );
        let mask = _mm_movemask_epi8(equal) as u32;
        if mask != 0 {
            return Some(at + mask.trailing_zeros() as usize);
        }
        at += 16;
    }
    source[at..]
        .iter()
        .position(|byte| needles.contains(byte))
        .map(|offset| at + offset)
}

#[cfg(any(not(target_arch = "x86_64"), test))]
fn short_set_word(source: &[u8], needles: &[u8]) -> Option<usize> {
    let mut at = 0;
    while source.len() - at >= 8 {
        let block = u64::from_le_bytes(source[at..at + 8].try_into().unwrap());
        let mut mask = 0;
        for &byte in needles {
            let equal = block ^ u64::from(byte) * 0x0101_0101_0101_0101;
            mask |= equal.wrapping_sub(0x0101_0101_0101_0101) & !equal & 0x8080_8080_8080_8080;
        }
        if mask != 0 {
            return Some(at + mask.trailing_zeros() as usize / 8);
        }
        at += 8;
    }
    source[at..]
        .iter()
        .position(|byte| needles.contains(byte))
        .map(|offset| at + offset)
}

/// First adjacent pair at or after `start` in the two byte sets, or the extent length.
pub fn find_pair(source: &[u8], firsts: &[u8], seconds: &[u8], start: u64) -> u64 {
    let Ok(start) = usize::try_from(start) else {
        return source.len() as u64;
    };
    let Some(rest) = source.get(start..) else {
        return source.len() as u64;
    };
    if firsts.is_empty() || seconds.is_empty() {
        return source.len() as u64;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("ssse3") {
        // SAFETY: SSSE3 was detected. The helper bounds both overlapping loads.
        return unsafe { pair_ssse3(rest, firsts, seconds) }.map_or(source.len(), |at| start + at)
            as u64;
    }
    pair_word(rest, firsts, seconds).map_or(source.len(), |at| start + at) as u64
}

// memchr 2.8.3 arch/generic/packedpair.rs: two overlapping whole-word loads.
// Exact scalar membership confirms lanes on architectures without SSSE3.
fn pair_word(source: &[u8], firsts: &[u8], seconds: &[u8]) -> Option<usize> {
    let mut at = 0;
    while source.len() - at >= 9 {
        let first = u64::from_le_bytes(source[at..at + 8].try_into().unwrap());
        let second = u64::from_le_bytes(source[at + 1..at + 9].try_into().unwrap());
        for lane in 0..8 {
            if firsts.contains(&((first >> (lane * 8)) as u8))
                && seconds.contains(&((second >> (lane * 8)) as u8))
            {
                return Some(at + lane);
            }
        }
        at += 8;
    }
    source[at..]
        .windows(2)
        .position(|pair| firsts.contains(&pair[0]) && seconds.contains(&pair[1]))
        .map(|offset| at + offset)
}

// aho-corasick 1.1.4 packed/teddy/generic.rs:1030-1080 and memchr 2.8.3
// arch/x86_64/sse2/packedpair.rs:139-158. Exact byte-set membership replaces
// Teddy buckets; overlapping loads replace shifts and their boundary carry.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn pair_ssse3(source: &[u8], firsts: &[u8], seconds: &[u8]) -> Option<usize> {
    use std::arch::x86_64::*;
    let tables = |set: &[u8]| {
        let mut lower = [0u8; 16];
        let mut upper = [0u8; 16];
        for &byte in set {
            let row = if byte < 128 { &mut lower } else { &mut upper };
            row[usize::from(byte & 15)] |= 1 << ((byte >> 4) & 7);
        }
        (lower, upper)
    };
    let (first_low, first_high) = tables(firsts);
    let (second_low, second_high) = tables(seconds);
    let bit_values: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    // SAFETY: all five lookup tables contain exactly sixteen initialized bytes.
    let (fl, fh, sl, sh, bits) = unsafe {
        (
            _mm_loadu_si128(first_low.as_ptr().cast()),
            _mm_loadu_si128(first_high.as_ptr().cast()),
            _mm_loadu_si128(second_low.as_ptr().cast()),
            _mm_loadu_si128(second_high.as_ptr().cast()),
            _mm_loadu_si128(bit_values.as_ptr().cast()),
        )
    };
    let mut at = 0;
    while source.len() - at >= 17 {
        // SAFETY: seventeen remaining bytes prove both overlapping sixteen-byte loads.
        let (first, second) = unsafe {
            (
                _mm_loadu_si128(source.as_ptr().add(at).cast()),
                _mm_loadu_si128(source.as_ptr().add(at + 1).cast()),
            )
        };
        let a = unsafe { pair_members(first, fl, fh, bits) };
        let b = unsafe { pair_members(second, sl, sh, bits) };
        let mask = _mm_movemask_epi8(_mm_and_si128(a, b)) as u32;
        if mask != 0 {
            return Some(at + mask.trailing_zeros() as usize);
        }
        at += 16;
    }
    pair_word(&source[at..], firsts, seconds).map(|offset| at + offset)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn pair_members(
    block: std::arch::x86_64::__m128i,
    low: std::arch::x86_64::__m128i,
    high: std::arch::x86_64::__m128i,
    bits: std::arch::x86_64::__m128i,
) -> std::arch::x86_64::__m128i {
    use std::arch::x86_64::*;
    let zero = _mm_setzero_si128();
    let indices = _mm_and_si128(block, _mm_set1_epi8(15));
    let halves = _mm_cmplt_epi8(block, zero);
    let members = _mm_or_si128(
        _mm_and_si128(halves, _mm_shuffle_epi8(high, indices)),
        _mm_andnot_si128(halves, _mm_shuffle_epi8(low, indices)),
    );
    let bit = _mm_shuffle_epi8(
        bits,
        _mm_and_si128(_mm_srli_epi16::<4>(block), _mm_set1_epi8(7)),
    );
    _mm_andnot_si128(
        _mm_cmpeq_epi8(_mm_and_si128(members, bit), zero),
        _mm_set1_epi8(-1),
    )
}

#[cfg(test)]
mod tests {
    use super::{find_any, find_pair, pair_word, short_set_word};
    #[test]
    fn all_bytes_offsets_and_extents() {
        let data: Vec<u8> = (0..=255).collect();
        for byte in 0..=255u8 {
            for start in [0, u64::from(byte), u64::from(byte) + 1, 256, u64::MAX] {
                let expected = if start <= u64::from(byte) {
                    u64::from(byte)
                } else {
                    256
                };
                assert_eq!(find_any(&data, &[byte, byte], start), expected);
            }
        }
        assert_eq!(find_any(&data[64..128], &[63, 64, 127, 128], 1), 63);
        assert_eq!(find_any(&data, &[], 0), 256);
        assert_eq!(find_any(&[], &[0], 0), 0);
        assert_eq!(find_any(b"hello\0world", b"\r\n\0", 0), 5);
        for needles in [
            vec![29, 63, 129, 255],
            (128..=255).collect(),
            (0..=255).filter(|b| b % 3 == 0).collect(),
        ] {
            for start in 0..=257 {
                let expected = data
                    .get(start..)
                    .and_then(|rest| rest.iter().position(|b| needles.contains(b)))
                    .map_or(data.len(), |offset| start + offset);
                assert_eq!(find_any(&data, &needles, start as u64), expected as u64);
            }
        }
        for needles in [b"a".as_slice(), b"ab", b"abc"] {
            for length in 0..=65 {
                for hit in 0..=length {
                    let mut source = vec![b'x'; length];
                    if hit < length {
                        source[hit] = needles[hit % needles.len()];
                    }
                    for start in 0..=length + 1 {
                        let expected = source
                            .get(start..)
                            .and_then(|rest| rest.iter().position(|b| needles.contains(b)))
                            .map_or(length, |offset| start + offset);
                        assert_eq!(find_any(&source, needles, start as u64), expected as u64);
                        if start <= length {
                            assert_eq!(
                                short_set_word(&source[start..], needles),
                                (expected < length).then_some(expected - start.min(expected))
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn pair_boundaries_and_byte_sets() {
        for (firsts, seconds) in [
            (b"a".as_slice(), b"b".as_slice()),
            (b"abc", b"bcd"),
            (b"\x00\x80\xff", b"\xff\x81\x00"),
            (b"", b"a"),
        ] {
            for length in 0..=65 {
                for hit in 0..=length {
                    let mut source = vec![b'x'; length];
                    if hit + 1 < length && !firsts.is_empty() {
                        source[hit] = firsts[hit % firsts.len()];
                        source[hit + 1] = seconds[hit % seconds.len()];
                    }
                    for start in 0..=length + 1 {
                        let expected = source
                            .get(start..)
                            .and_then(|rest| {
                                rest.windows(2)
                                    .position(|p| firsts.contains(&p[0]) && seconds.contains(&p[1]))
                            })
                            .map_or(length, |at| start + at);
                        assert_eq!(
                            find_pair(&source, firsts, seconds, start as u64),
                            expected as u64
                        );
                        if start <= length {
                            assert_eq!(
                                pair_word(&source[start..], firsts, seconds),
                                (expected < length).then(|| expected - start)
                            );
                        }
                    }
                    assert_eq!(find_pair(&source, firsts, seconds, u64::MAX), length as u64);
                }
            }
        }
        let all: Vec<u8> = (0..=255).collect();
        for first in 0..=255u8 {
            for second in 0..=255u8 {
                let source = [first, second];
                assert_eq!(find_pair(&source, &[first], &[second], 0), 0);
                assert_eq!(find_pair(&source, &[first.wrapping_add(1)], &all, 0), 2);
                let mut block = [b'x'; 34];
                block[16] = first;
                block[17] = second;
                let expected = block.windows(2).position(|p| p == [first, second]).unwrap();
                assert_eq!(find_pair(&block, &[first], &[second], 0), expected as u64);
                assert_eq!(pair_word(&block, &[first], &[second]), Some(expected));
            }
        }
    }
}
