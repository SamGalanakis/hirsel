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
    if needles == JSON_STRING_STOP {
        return (start + json_string_stop(rest)) as u64;
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

const JSON_STRING_STOP: &[u8] = b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\x0d\x0e\x0f\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f\"\\";

// serde_json 1.0.154 src/read.rs:459-507, SliceRead::skip_to_escape
// (MIT OR Apache-2.0). Exact membership above admits its control/quote/slash
// predicate; other sets retain the generic search. All loads stay in bounds.
fn json_string_stop(source: &[u8]) -> usize {
    let Some(&first) = source.first() else {
        return 0;
    };
    if first < 32 || first == b'"' || first == b'\\' {
        return 0;
    }
    let mut at = 1;
    for chunk in source[at..].chunks_exact(8) {
        let word = u64::from_le_bytes(chunk.try_into().unwrap());
        let controls = word.wrapping_sub(0x2020202020202020) & !word;
        let quotes = word ^ 0x2222222222222222;
        let quotes = quotes.wrapping_sub(0x0101010101010101) & !quotes;
        let slashes = word ^ 0x5c5c5c5c5c5c5c5c;
        let slashes = slashes.wrapping_sub(0x0101010101010101) & !slashes;
        let stops = (controls | quotes | slashes) & 0x8080808080808080;
        if stops != 0 {
            return at + stops.trailing_zeros() as usize / 8;
        }
        at += 8;
    }
    source[at..]
        .iter()
        .position(|&byte| byte < 32 || byte == b'"' || byte == b'\\')
        .map_or(source.len(), |offset| at + offset)
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

/// First adjacent pair in two prepared 32-byte membership masks, or the extent length.
pub fn find_pair(source: &[u8], firsts: &[u8], seconds: &[u8], start: u64) -> u64 {
    let Ok(start) = usize::try_from(start) else {
        return source.len() as u64;
    };
    let Some(rest) = source.get(start..) else {
        return source.len() as u64;
    };
    let (Ok(firsts), Ok(seconds)): (Result<&[u8; 32], _>, Result<&[u8; 32], _>) =
        (firsts.try_into(), seconds.try_into())
    else {
        return source.len() as u64;
    };
    #[cfg(target_arch = "x86_64")]
    if rest.len() >= 33 && std::is_x86_feature_detected!("avx2") {
        // SAFETY: AVX2 was detected. The helper bounds both overlapping loads.
        return unsafe { pair_avx2(rest, firsts, seconds) }.map_or(source.len(), |at| start + at)
            as u64;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("ssse3") {
        // SAFETY: SSSE3 was detected. The helper bounds both overlapping loads.
        return unsafe { pair_ssse3(rest, firsts, seconds) }.map_or(source.len(), |at| start + at)
            as u64;
    }
    pair_word(rest, firsts, seconds).map_or(source.len(), |at| start + at) as u64
}

fn pair_member(mask: &[u8; 32], byte: u8) -> bool {
    mask[usize::from(byte & 15) + usize::from(byte >> 7) * 16] & (1 << ((byte >> 4) & 7)) != 0
}

// memchr 2.8.3 arch/generic/packedpair.rs: two overlapping whole-word loads.
// Exact scalar membership confirms lanes on architectures without SSSE3.
fn pair_word(source: &[u8], firsts: &[u8; 32], seconds: &[u8; 32]) -> Option<usize> {
    let mut at = 0;
    while source.len() - at >= 9 {
        let first = u64::from_le_bytes(source[at..at + 8].try_into().unwrap());
        let second = u64::from_le_bytes(source[at + 1..at + 9].try_into().unwrap());
        for lane in 0..8 {
            if pair_member(firsts, (first >> (lane * 8)) as u8)
                && pair_member(seconds, (second >> (lane * 8)) as u8)
            {
                return Some(at + lane);
            }
        }
        at += 8;
    }
    source[at..]
        .windows(2)
        .position(|pair| pair_member(firsts, pair[0]) && pair_member(seconds, pair[1]))
        .map(|offset| at + offset)
}

// aho-corasick 1.1.4 packed/teddy/generic.rs:1030-1080 and memchr 2.8.3
// arch/x86_64/sse2/packedpair.rs:139-158. Exact byte-set membership replaces
// Teddy buckets; overlapping loads replace shifts and their boundary carry.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn pair_ssse3(source: &[u8], firsts: &[u8; 32], seconds: &[u8; 32]) -> Option<usize> {
    use std::arch::x86_64::*;
    let bit_values: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    // SAFETY: all five lookup tables contain exactly sixteen initialized bytes.
    let (fl, fh, sl, sh, bits) = unsafe {
        (
            _mm_loadu_si128(firsts.as_ptr().cast()),
            _mm_loadu_si128(firsts.as_ptr().add(16).cast()),
            _mm_loadu_si128(seconds.as_ptr().cast()),
            _mm_loadu_si128(seconds.as_ptr().add(16).cast()),
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

// memchr 2.8.3 arch/x86_64/avx2/packedpair.rs:149-176 and
// arch/generic/packedpair.rs:222-276: use two overlapping full-width loads,
// report the first set lane, and keep the narrower scanner for the tail.
// Exact nibble membership retains the SSSE3 contract for arbitrary byte sets.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn pair_avx2(source: &[u8], firsts: &[u8; 32], seconds: &[u8; 32]) -> Option<usize> {
    use std::arch::x86_64::*;
    let bit_values: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
    // SAFETY: each table load reads sixteen initialized bytes. Broadcasting
    // repeats the table in both independent AVX2 shuffle lanes.
    let (fl, fh, sl, sh, bits) = unsafe {
        (
            _mm256_broadcastsi128_si256(_mm_loadu_si128(firsts.as_ptr().cast())),
            _mm256_broadcastsi128_si256(_mm_loadu_si128(firsts.as_ptr().add(16).cast())),
            _mm256_broadcastsi128_si256(_mm_loadu_si128(seconds.as_ptr().cast())),
            _mm256_broadcastsi128_si256(_mm_loadu_si128(seconds.as_ptr().add(16).cast())),
            _mm256_broadcastsi128_si256(_mm_loadu_si128(bit_values.as_ptr().cast())),
        )
    };
    let mut at = 0;
    while source.len() - at >= 33 {
        // SAFETY: thirty-three remaining bytes bound both thirty-two-byte loads.
        let (first, second) = unsafe {
            (
                _mm256_loadu_si256(source.as_ptr().add(at).cast()),
                _mm256_loadu_si256(source.as_ptr().add(at + 1).cast()),
            )
        };
        let a = unsafe { pair_members_avx2(first, fl, fh, bits) };
        let b = unsafe { pair_members_avx2(second, sl, sh, bits) };
        let mask = _mm256_movemask_epi8(_mm256_and_si256(a, b)) as u32;
        if mask != 0 {
            return Some(at + mask.trailing_zeros() as usize);
        }
        at += 32;
    }
    let tail = if std::is_x86_feature_detected!("ssse3") {
        // SAFETY: SSSE3 is detected independently; the helper bounds its tail.
        unsafe { pair_ssse3(&source[at..], firsts, seconds) }
    } else {
        pair_word(&source[at..], firsts, seconds)
    };
    tail.map(|offset| at + offset)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn pair_members_avx2(
    block: std::arch::x86_64::__m256i,
    low: std::arch::x86_64::__m256i,
    high: std::arch::x86_64::__m256i,
    bits: std::arch::x86_64::__m256i,
) -> std::arch::x86_64::__m256i {
    use std::arch::x86_64::*;
    let zero = _mm256_setzero_si256();
    let indices = _mm256_and_si256(block, _mm256_set1_epi8(15));
    let halves = _mm256_cmpgt_epi8(zero, block);
    let members = _mm256_or_si256(
        _mm256_and_si256(halves, _mm256_shuffle_epi8(high, indices)),
        _mm256_andnot_si256(halves, _mm256_shuffle_epi8(low, indices)),
    );
    let bit = _mm256_shuffle_epi8(
        bits,
        _mm256_and_si256(_mm256_srli_epi16::<4>(block), _mm256_set1_epi8(7)),
    );
    _mm256_andnot_si256(
        _mm256_cmpeq_epi8(_mm256_and_si256(members, bit), zero),
        _mm256_set1_epi8(-1),
    )
}

/// Run known ASCII DFA transitions, leaving tagged/unknown/Unicode bytes unread.
/// Adapted from regex-automata 0.4.11 hybrid/search.rs:189-239 and
/// hybrid/dfa.rs:1415-1425. Row offsets are already prepared by the source
/// determinizer. Bit zero records a delayed match; bits one/two stop this
/// cached run. Zero means unknown. Return state, cursor and the latest match
/// offset, or u64::MAX when no match was observed. Invalid arithmetic/table
/// accesses return None so adapters report a resource-independent failure.
/// The lookup closures keep interpreter values borrowed without copying a slab.
pub fn scan_dfa(
    source: &[u8],
    class: impl Fn(u8) -> Option<u64>,
    transition: impl Fn(u64) -> Option<u64>,
    start: u64,
    mut state: u64,
) -> Option<[u64; 3]> {
    let mut cursor = usize::try_from(start).ok()?;
    if cursor > source.len() {
        return None;
    }
    let mut matched = u64::MAX;
    // hybrid/search.rs:189-239: one bounds decision admits four dependent
    // transitions. Each stop still returns the exact unconsumed byte.
    macro_rules! advance {
        () => {{
            let byte = source[cursor];
            if byte >= 128 {
                return Some([state, cursor as u64, matched]);
            }
            let hit = transition(state.checked_add(class(byte)?)?)?;
            if hit == 0 || hit & 6 != 0 {
                return Some([state, cursor as u64, matched]);
            }
            if hit & 1 != 0 {
                matched = cursor as u64;
            }
            state = hit >> 3;
            cursor += 1;
        }};
    }
    while source.len() - cursor >= 4 {
        advance!();
        advance!();
        advance!();
        advance!();
    }
    while cursor < source.len() {
        advance!();
    }
    Some([state, cursor as u64, matched])
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
            super::JSON_STRING_STOP.to_vec(),
        ] {
            for start in 0..=257 {
                let expected = data
                    .get(start..)
                    .and_then(|rest| rest.iter().position(|b| needles.contains(b)))
                    .map_or(data.len(), |offset| start + offset);
                assert_eq!(find_any(&data, &needles, start as u64), expected as u64);
            }
        }
        for needles in [b"a".as_slice(), b"ab", b"abc", super::JSON_STRING_STOP] {
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
    fn mask(set: &[u8]) -> [u8; 32] {
        let mut out = [0; 32];
        for &byte in set {
            out[usize::from(byte & 15) + usize::from(byte >> 7) * 16] |= 1 << ((byte >> 4) & 7);
        }
        out
    }
    #[test]
    fn pair_boundaries_and_byte_sets() {
        for (firsts, seconds) in [
            (b"a".as_slice(), b"b".as_slice()),
            (b"abc", b"bcd"),
            (b"\x00\x80\xff", b"\xff\x81\x00"),
            (b"", b"a"),
        ] {
            let (first_mask, second_mask) = (mask(firsts), mask(seconds));
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
                            find_pair(&source, &first_mask, &second_mask, start as u64),
                            expected as u64
                        );
                        if start <= length {
                            assert_eq!(
                                pair_word(&source[start..], &first_mask, &second_mask),
                                (expected < length).then(|| expected - start)
                            );
                            #[cfg(target_arch = "x86_64")]
                            {
                                let wanted = (expected < length).then(|| expected - start);
                                if std::is_x86_feature_detected!("ssse3") {
                                    // SAFETY: the feature is detected and all extents are live.
                                    assert_eq!(
                                        unsafe {
                                            super::pair_ssse3(
                                                &source[start..],
                                                &first_mask,
                                                &second_mask,
                                            )
                                        },
                                        wanted
                                    );
                                }
                                if std::is_x86_feature_detected!("avx2") {
                                    // SAFETY: the feature is detected and all extents are live.
                                    assert_eq!(
                                        unsafe {
                                            super::pair_avx2(
                                                &source[start..],
                                                &first_mask,
                                                &second_mask,
                                            )
                                        },
                                        wanted
                                    );
                                }
                            }
                        }
                    }
                    assert_eq!(
                        find_pair(&source, &first_mask, &second_mask, u64::MAX),
                        length as u64
                    );
                }
            }
        }
        assert_eq!(find_pair(b"ab", &[], &mask(b"b"), 0), 2);
        assert_eq!(find_pair(b"ab", &[255; 33], &mask(b"b"), 0), 2);
        let all: Vec<u8> = (0..=255).collect();
        for first in 0..=255u8 {
            for second in 0..=255u8 {
                let source = [first, second];
                assert_eq!(find_pair(&source, &mask(&[first]), &mask(&[second]), 0), 0);
                assert_eq!(
                    find_pair(&source, &mask(&[first.wrapping_add(1)]), &mask(&all), 0),
                    2
                );
                let mut block = [b'x'; 34];
                block[16] = first;
                block[17] = second;
                let expected = block.windows(2).position(|p| p == [first, second]).unwrap();
                assert_eq!(
                    find_pair(&block, &mask(&[first]), &mask(&[second]), 0),
                    expected as u64
                );
                assert_eq!(
                    pair_word(&block, &mask(&[first]), &mask(&[second])),
                    Some(expected)
                );
            }
        }
        // Hand-derived cached-state vectors. a enters row 136; b records a
        // delayed match there; other ASCII bytes stop before consumption.
        // Native and interpreter adapters use this same primitive contract.
        let mut rows = [0u64; 140];
        rows[128] = 136 * 8;
        rows[136] = 136 * 8;
        rows[137] = 136 * 8 + 1;
        rows[138] = 136 * 8 + 2;
        let class = |byte| {
            Some(match byte {
                b'a' => 0,
                b'b' => 1,
                _ => 2,
            })
        };
        let transition = |at| {
            usize::try_from(at)
                .ok()
                .and_then(|at| rows.get(at).copied())
        };
        for (source, start, state, expected) in [
            (b"abbb!".as_slice(), 0, 128, Some([136, 4, 3])),
            (b"abbbbbbb", 0, 128, Some([136, 8, 7])),
            (b"abbbbbbbbb!", 0, 128, Some([136, 10, 9])),
            (b"aa!abbbb", 0, 128, Some([136, 2, u64::MAX])),
            (b"bb", 0, 128, Some([128, 0, u64::MAX])),
            (b"ab\xc3\xa9", 0, 128, Some([136, 2, 1])),
            (b"_ab", 1, 128, Some([136, 3, 2])),
            (b"ab", 2, 128, Some([128, 2, u64::MAX])),
            (b"ab", 3, 128, None),
            (b"b", 0, u64::MAX, None),
            (b"b", 0, 139, None),
        ] {
            assert_eq!(
                super::scan_dfa(source, class, transition, start, state),
                expected
            );
        }
        for flags in 2..8 {
            rows[138] = 136 * 8 + flags;
            let transition = |at| {
                usize::try_from(at)
                    .ok()
                    .and_then(|at| rows.get(at).copied())
            };
            assert_eq!(
                super::scan_dfa(b"ab!", class, transition, 0, 128),
                Some([136, 2, 1])
            );
            assert_eq!(
                super::scan_dfa(b"aaaa!", class, transition, 0, 128),
                Some([136, 4, u64::MAX])
            );
        }
    }
}
