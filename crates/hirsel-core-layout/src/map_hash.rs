//! The SipHash-1-3 stream used by the native and interpreted Map key ABIs.

pub const STATE_WORDS: usize = 5;

/// Four SipHash lanes and the length of the structural byte stream.
/// Every component ends at a word boundary.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct State(pub [u64; STATE_WORDS]);

impl State {
    #[inline]
    pub fn new(key: &[u8; 16]) -> Self {
        let (low, high) = key.split_at(8);
        let k0 = u64::from_le_bytes(low.try_into().unwrap());
        let k1 = u64::from_le_bytes(high.try_into().unwrap());
        Self([
            k0 ^ 0x736f6d6570736575,
            k1 ^ 0x646f72616e646f6d,
            k0 ^ 0x6c7967656e657261,
            k1 ^ 0x7465646279746573,
            0,
        ])
    }

    #[inline(always)]
    fn round(&mut self) {
        let [v0, v1, v2, v3, _] = &mut self.0;
        *v0 = v0.wrapping_add(*v1);
        *v1 = v1.rotate_left(13) ^ *v0;
        *v0 = v0.rotate_left(32);
        *v2 = v2.wrapping_add(*v3);
        *v3 = v3.rotate_left(16) ^ *v2;
        *v0 = v0.wrapping_add(*v3);
        *v3 = v3.rotate_left(21) ^ *v0;
        *v2 = v2.wrapping_add(*v1);
        *v1 = v1.rotate_left(17) ^ *v2;
        *v2 = v2.rotate_left(32);
    }

    #[inline(always)]
    fn compress(&mut self, word: u64) {
        self.0[3] ^= word;
        self.round();
        self.0[0] ^= word;
    }

    #[inline]
    pub fn word(&mut self, word: u64) {
        self.compress(word);
        self.0[4] = self.0[4].wrapping_add(8);
    }

    /// Append the byte count, the bytes and zero padding to the word boundary.
    #[inline]
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.word(bytes.len() as u64);
        let (words, tail) = bytes.as_chunks::<8>();
        for &word in words {
            self.compress(u64::from_le_bytes(word));
        }
        self.0[4] = self.0[4].wrapping_add(words.len() as u64 * 8);
        if !tail.is_empty() {
            let mut word = [0u8; 8];
            word[..tail.len()].copy_from_slice(tail);
            self.word(u64::from_le_bytes(word));
        }
    }

    #[inline]
    pub fn finish(mut self) -> u64 {
        self.compress(self.0[4] << 56);
        self.0[2] ^= 0xff;
        self.round();
        self.round();
        self.round();
        self.0[0] ^ self.0[1] ^ self.0[2] ^ self.0[3]
    }
}

#[inline]
pub fn word(key: &[u8; 16], word: u64) -> u64 {
    let mut state = State::new(key);
    state.word(word);
    state.finish()
}

#[inline]
pub fn bytes(key: &[u8; 16], bytes: &[u8]) -> u64 {
    let mut state = State::new(key);
    state.bytes(bytes);
    state.finish()
}
