//! Out-of-place AES-GCM over ring 0.17.14's VAES/VPCLMULQDQ assembly.
//!
//! ring seals and opens only in place, which costs a plaintext copy into the
//! send slot and another out of the receive slot. This drives the same audited
//! assembly ring selects on these CPUs (`aes_gcm_{enc,dec}_update_vaes_avx2`
//! and its key and GHASH routines) with a separate source and destination.
//! The framing follows ring's `aes_gcm::{seal, open}`: counter block 1 masks
//! the tag, bulk blocks start at counter 2, and the partial last block is
//! hashed as ciphertext zero-padded to a block.
#![cfg(target_arch = "x86_64")]

const BLOCK: usize = 16;

// Layouts match ring's `AES_KEY` (aes.h) and its 16-byte-aligned `HTable`.
#[repr(C, align(16))]
struct AesKey {
    rd_key: [u32; 60],
    rounds: u32,
}

#[repr(C, align(16))]
struct HTable([u64; 32]);

unsafe extern "C" {
    #[link_name = "ring_core_0_17_14__aes_hw_set_encrypt_key_alt"]
    fn aes_set_key(user: *const u8, bits: i32, key: *mut AesKey) -> i32;
    #[link_name = "ring_core_0_17_14__aes_hw_ctr32_encrypt_blocks"]
    fn aes_ctr32(input: *const u8, output: *mut u8, blocks: usize, key: &AesKey, ivec: &[u8; 16]);
    #[link_name = "ring_core_0_17_14__gcm_init_vpclmulqdq_avx2"]
    fn gcm_init(table: &mut HTable, h: &[u64; 2]);
    #[link_name = "ring_core_0_17_14__gcm_ghash_vpclmulqdq_avx2_1"]
    fn gcm_ghash(xi: &mut [u8; 16], table: &HTable, input: *const u8, len: usize);
    #[link_name = "ring_core_0_17_14__aes_gcm_enc_update_vaes_avx2"]
    fn gcm_enc(
        input: *const u8,
        output: *mut u8,
        len: usize,
        key: &AesKey,
        ivec: &[u8; 16],
        table: &HTable,
        xi: &mut [u8; 16],
    );
    #[link_name = "ring_core_0_17_14__aes_gcm_dec_update_vaes_avx2"]
    fn gcm_dec(
        input: *const u8,
        output: *mut u8,
        len: usize,
        key: &AesKey,
        ivec: &[u8; 16],
        table: &HTable,
        xi: &mut [u8; 16],
    );
}

pub(super) struct GcmKey {
    aes: AesKey,
    table: HTable,
}

impl GcmKey {
    /// The CPU features ring requires before it selects this assembly.
    fn supported() -> bool {
        std::arch::is_x86_feature_detected!("aes")
            && std::arch::is_x86_feature_detected!("pclmulqdq")
            && std::arch::is_x86_feature_detected!("ssse3")
            && std::arch::is_x86_feature_detected!("avx")
            && std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("vaes")
            && std::arch::is_x86_feature_detected!("vpclmulqdq")
    }

    /// An AES-128 or AES-256 key, or none on CPUs without the assembly.
    pub(super) fn new(key: &[u8]) -> Option<Self> {
        if !matches!(key.len(), 16 | 32) || !Self::supported() {
            return None;
        }
        let mut this = Self {
            aes: AesKey {
                rd_key: [0; 60],
                rounds: 0,
            },
            table: HTable([0; 32]),
        };
        // SAFETY: the key is 16 or 32 bytes and the schedule has room for 14 rounds.
        let status = unsafe { aes_set_key(key.as_ptr(), key.len() as i32 * 8, &mut this.aes) };
        assert_eq!(status, 0);
        let mut h = [0; BLOCK];
        // SAFETY: one in-place block under the initialized schedule.
        unsafe { aes_ctr32(h.as_mut_ptr(), h.as_mut_ptr(), 1, &this.aes, &[0; BLOCK]) };
        let (hi, lo) = h.split_at(8);
        let h = [
            u64::from_be_bytes(hi.try_into().unwrap()),
            u64::from_be_bytes(lo.try_into().unwrap()),
        ];
        // SAFETY: the CPU features were checked above.
        unsafe { gcm_init(&mut this.table, &h) };
        Some(this)
    }

    fn ghash(&self, xi: &mut [u8; BLOCK], bytes: &[u8]) {
        let (blocks, tail) = bytes.as_chunks::<BLOCK>();
        for block in blocks {
            // SAFETY: the assembly reads one complete block per call.
            unsafe { gcm_ghash(xi, &self.table, block.as_ptr(), BLOCK) };
        }
        if !tail.is_empty() {
            let mut last = [0; BLOCK];
            last[..tail.len()].copy_from_slice(tail);
            // SAFETY: one block.
            unsafe { gcm_ghash(xi, &self.table, last.as_ptr(), BLOCK) };
        }
    }

    /// The counter block that masks the tag, the first bulk counter block and
    /// GHASH over `aad`.
    fn start(&self, nonce: [u8; 12], aad: &[u8]) -> ([u8; BLOCK], [u8; BLOCK], [u8; BLOCK]) {
        let mut counter = [0; BLOCK];
        counter[..12].copy_from_slice(&nonce);
        counter[15] = 1;
        let tag_mask = counter;
        counter[15] = 2;
        let mut xi = [0; BLOCK];
        self.ghash(&mut xi, aad);
        (tag_mask, counter, xi)
    }

    fn advance(counter: &mut [u8; BLOCK], bytes: usize) {
        let next = u32::from_be_bytes(counter[12..].try_into().unwrap())
            .wrapping_add((bytes / BLOCK) as u32);
        counter[12..].copy_from_slice(&next.to_be_bytes());
    }

    /// Hashes the bit lengths and masks the result into the tag.
    fn finish(
        &self,
        mut xi: [u8; BLOCK],
        tag_mask: &[u8; BLOCK],
        aad: usize,
        len: usize,
    ) -> [u8; BLOCK] {
        let mut lengths = [0; BLOCK];
        lengths[..8].copy_from_slice(&(aad as u64 * 8).to_be_bytes());
        lengths[8..].copy_from_slice(&(len as u64 * 8).to_be_bytes());
        // SAFETY: one block.
        unsafe { gcm_ghash(&mut xi, &self.table, lengths.as_ptr(), BLOCK) };
        // SAFETY: one in-place block.
        unsafe { aes_ctr32(xi.as_mut_ptr(), xi.as_mut_ptr(), 1, &self.aes, tag_mask) };
        xi
    }

    /// Seals `input ‖ [extra]` under `nonce` and `aad` into `out`, which holds
    /// `input.len() + 1` ciphertext bytes followed by the 16-byte tag. The
    /// plaintext is read from `input` and written nowhere but `out`.
    pub(super) fn seal(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        input: &[u8],
        extra: u8,
        out: &mut [u8],
    ) {
        let len = input.len() + 1;
        assert_eq!(out.len(), len + BLOCK);
        let (tag_mask, mut counter, mut xi) = self.start(nonce, aad);

        let whole = input.len() / BLOCK * BLOCK;
        if whole != 0 {
            // SAFETY: `input` and `out` are distinct and hold `whole` bytes.
            unsafe {
                gcm_enc(
                    input.as_ptr(),
                    out.as_mut_ptr(),
                    whole,
                    &self.aes,
                    &counter,
                    &self.table,
                    &mut xi,
                )
            };
            Self::advance(&mut counter, whole);
        }

        // The partial block (1 to 16 bytes, ending in `extra`) is built in
        // `out`, whose tag bytes leave room for a whole block.
        let rest = len - whole;
        let block = &mut out[whole..whole + BLOCK];
        block[..rest - 1].copy_from_slice(&input[whole..]);
        block[rest - 1] = extra;
        // SAFETY: one in-place block.
        unsafe {
            aes_ctr32(
                block.as_mut_ptr(),
                block.as_mut_ptr(),
                1,
                &self.aes,
                &counter,
            )
        };
        block[rest..].fill(0);
        // SAFETY: one block.
        unsafe { gcm_ghash(&mut xi, &self.table, block.as_ptr(), BLOCK) };
        out[len..].copy_from_slice(&self.finish(xi, &tag_mask, aad.len(), len));
    }

    /// Opens `sealed` (ciphertext, then the 16-byte tag) under `nonce` and
    /// `aad` into `out`, which holds exactly the ciphertext's length. On a tag
    /// mismatch `out` is zeroed: unauthenticated plaintext never outlives the
    /// call.
    pub(super) fn open(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        sealed: &[u8],
        out: &mut [u8],
    ) -> Result<(), ()> {
        let len = sealed.len().checked_sub(BLOCK).ok_or(())?;
        assert_eq!(out.len(), len);
        let (tag_mask, mut counter, mut xi) = self.start(nonce, aad);

        let whole = len / BLOCK * BLOCK;
        if whole != 0 {
            // SAFETY: `sealed` and `out` are distinct and hold `whole` bytes.
            unsafe {
                gcm_dec(
                    sealed.as_ptr(),
                    out.as_mut_ptr(),
                    whole,
                    &self.aes,
                    &counter,
                    &self.table,
                    &mut xi,
                )
            };
            Self::advance(&mut counter, whole);
        }

        let rest = len - whole;
        if rest != 0 {
            let mut block = [0; BLOCK];
            block[..rest].copy_from_slice(&sealed[whole..len]);
            // SAFETY: one block.
            unsafe { gcm_ghash(&mut xi, &self.table, block.as_ptr(), BLOCK) };
            // SAFETY: one in-place block.
            unsafe { aes_ctr32(block.as_ptr(), block.as_mut_ptr(), 1, &self.aes, &counter) };
            out[whole..].copy_from_slice(&block[..rest]);
        }

        let tag = self.finish(xi, &tag_mask, aad.len(), len);
        let differ = tag
            .iter()
            .zip(&sealed[len..])
            .fold(0, |differ, (a, b)| differ | (a ^ b));
        if std::hint::black_box(differ) != 0 {
            out.fill(0);
            return Err(());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::GcmKey;
    use ring::aead::{AES_128_GCM, AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Opens `sealed` with `key`; a flipped tag bit must fail and leave `out`
    /// zeroed.
    fn open(key: &GcmKey, nonce: [u8; 12], aad: &[u8], sealed: &[u8]) -> Vec<u8> {
        let mut out = vec![0xa5; sealed.len() - 16];
        key.open(nonce, aad, sealed, &mut out).unwrap();
        let mut forged = sealed.to_vec();
        *forged.last_mut().unwrap() ^= 1;
        let mut zeroed = vec![0xa5; out.len()];
        assert!(key.open(nonce, aad, &forged, &mut zeroed).is_err());
        assert!(zeroed.iter().all(|&byte| byte == 0));
        out
    }

    /// McGrew-Viega cases 4 and 16, Wycheproof multi-block AAD, and ring's
    /// in-place seal over every partial-block shape, each sealed and opened.
    #[test]
    fn out_of_place_gcm_matches_ring() {
        if std::env::var("HIRSEL_TLS_GCM_TEST_FALLBACK").as_deref() == Ok("1") {
            assert!(
                !GcmKey::supported(),
                "fallback job requires a CPU without the VAES path"
            );
            super::super::records::check_fallback_seal();
            return;
        }
        assert!(
            GcmKey::supported(),
            "fast GCM conformance requires VAES and VPCLMULQDQ. Set HIRSEL_TLS_GCM_TEST_FALLBACK=1 for the fallback job"
        );
        let plain = hex(concat!(
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72",
            "1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39"
        ));
        let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let nonce: [u8; 12] = hex("cafebabefacedbaddecaf888").try_into().unwrap();
        for (key, sealed) in [
            (
                "feffe9928665731c6d6a8f9467308308",
                concat!(
                    "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e",
                    "21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091",
                    "5bc94fbc3221a5db94fae95ae7121a47"
                ),
            ),
            (
                "feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308",
                concat!(
                    "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa",
                    "8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662",
                    "76fc6ece0f4e1768cddf8853bb2d551b"
                ),
            ),
        ] {
            let seal = GcmKey::new(&hex(key)).unwrap();
            let (input, extra) = plain.split_at(plain.len() - 1);
            let mut out = vec![0; plain.len() + 16];
            seal.seal(nonce, &aad, input, extra[0], &mut out);
            assert_eq!(out, hex(sealed));
            assert_eq!(open(&seal, nonce, &aad, &out), plain);
        }

        for line in include_str!("testdata/aes-gcm-multiblock-aad.tsv")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let [id, key, iv, aad, msg, ct, tag] = line.split('\t').collect::<Vec<_>>()[..] else {
                panic!("malformed Wycheproof vector: {line}");
            };
            let seal = GcmKey::new(&hex(key)).unwrap();
            let msg = hex(msg);
            let (extra, input) = msg.split_last().unwrap();
            let mut out = vec![0; msg.len() + super::BLOCK];
            seal.seal(
                hex(iv).try_into().unwrap(),
                &hex(aad),
                input,
                *extra,
                &mut out,
            );
            let (ciphertext, actual_tag) = out.split_at(msg.len());
            assert_eq!(ciphertext, hex(ct), "Wycheproof case {id} ciphertext");
            assert_eq!(actual_tag, hex(tag), "Wycheproof case {id} tag");
            assert_eq!(
                open(&seal, hex(iv).try_into().unwrap(), &hex(aad), &out),
                msg,
                "Wycheproof case {id} open"
            );
        }

        let header = [23, 3, 3, 0x40, 0x11];
        for (algorithm, key) in [(&AES_128_GCM, [0x42; 32]), (&AES_256_GCM, [0x5a; 32])] {
            let key = &key[..algorithm.key_len()];
            let seal = GcmKey::new(key).unwrap();
            let ring = LessSafeKey::new(UnboundKey::new(algorithm, key).unwrap());
            for len in (0..=272).chain(16_368..=16_400) {
                let input: Vec<u8> = (0..len).map(|i| (i * 7 + len) as u8).collect();
                let nonce = [len as u8; 12];
                let mut out = vec![0xa5; len + 17];
                seal.seal(nonce, &header, &input, 23, &mut out);
                let mut expected = input.clone();
                expected.push(23);
                let tag = ring
                    .seal_in_place_separate_tag(
                        Nonce::assume_unique_for_key(nonce),
                        Aad::from(header),
                        &mut expected,
                    )
                    .unwrap();
                expected.extend_from_slice(tag.as_ref());
                assert_eq!(out, expected, "{} bytes", len);
                let mut plain = input;
                plain.push(23);
                assert_eq!(
                    open(&seal, nonce, &header, &out),
                    plain,
                    "{} bytes open",
                    len
                );
            }
        }
    }
}
