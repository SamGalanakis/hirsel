/// RFC 3174 sections 4 and 6.1. The evaluator always uses scalar rounds.
pub(super) fn padded(h: &mut [u32; 5], data: &[u8]) {
    let (blocks, tail) = data.as_chunks::<64>();
    let mut padding = [0u8; 128];
    padding[..tail.len()].copy_from_slice(tail);
    padding[tail.len()] = 0x80;
    let end = if tail.len() < 56 { 64 } else { 128 };
    padding[end - 8..end].copy_from_slice(&((data.len() as u64) << 3).to_be_bytes());

    compress(h, blocks);
    compress(h, padding[..end].as_chunks::<64>().0);
}

pub(super) fn compress(h: &mut [u32; 5], blocks: &[[u8; 64]]) {
    for block in blocks {
        let mut w = [0u32; 80];
        for (word, bytes) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = *h;
        for (i, word) in w.into_iter().enumerate() {
            let (f, k) = match i {
                0..20 => ((b & c) | (!b & d), 0x5a827999),
                20..40 => (b ^ c ^ d, 0x6ed9eba1),
                40..60 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            (a, b, c, d, e) = (next, a, b.rotate_left(30), c, d);
        }
        for (word, update) in h.iter_mut().zip([a, b, c, d, e]) {
            *word = word.wrapping_add(update);
        }
    }
}
