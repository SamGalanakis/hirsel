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

use std::arch::x86_64::{__m128i, _mm_set_epi64x};
use std::mem::MaybeUninit;
use zeroize::{Zeroize, Zeroizing};

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

impl Drop for GcmKey {
    fn drop(&mut self) {
        self.aes.rd_key.zeroize();
        self.aes.rounds.zeroize();
        self.table.0.zeroize();
    }
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
    pub(super) fn new(key: &[u8]) -> Option<Box<Self>> {
        if !matches!(key.len(), 16 | 32) || !Self::supported() {
            return None;
        }
        let mut this = Box::new(Self {
            aes: AesKey {
                rd_key: [0; 60],
                rounds: 0,
            },
            table: HTable([0; 32]),
        });
        // SAFETY: the key is 16 or 32 bytes and the schedule has room for 14 rounds.
        let status = unsafe { aes_set_key(key.as_ptr(), key.len() as i32 * 8, &mut this.aes) };
        assert_eq!(status, 0);
        let mut h = Zeroizing::new([0; BLOCK]);
        // SAFETY: one in-place block under the initialized schedule.
        unsafe { aes_ctr32(h.as_mut_ptr(), h.as_mut_ptr(), 1, &this.aes, &[0; BLOCK]) };
        let (hi, lo) = h.split_at(8);
        let h = Zeroizing::new([
            u64::from_be_bytes(hi.try_into().unwrap()),
            u64::from_be_bytes(lo.try_into().unwrap()),
        ]);
        // SAFETY: the CPU features were checked above.
        unsafe { gcm_init(&mut this.table, &h) };
        Some(this)
    }

    /// Forms a zero-padded block without a partial store to the block ring
    /// will load. The two end loads always lie inside the input.
    fn padded_aad(bytes: &[u8]) -> __m128i {
        let n = bytes.len();
        let (lo, hi) = match n {
            9..=15 => {
                let lo = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                let last = u64::from_le_bytes(bytes[n - 8..].try_into().unwrap());
                (lo, last >> (8 * (16 - n)))
            }
            8 => (u64::from_le_bytes(bytes.try_into().unwrap()), 0),
            4..=7 => {
                let lo = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                let last = u32::from_le_bytes(bytes[n - 4..].try_into().unwrap());
                (u64::from(lo) | (u64::from(last) << (8 * (n - 4))), 0)
            }
            2..=3 => {
                let lo = u16::from_le_bytes(bytes[..2].try_into().unwrap());
                let last = u16::from_le_bytes(bytes[n - 2..].try_into().unwrap());
                (u64::from(lo) | (u64::from(last) << (8 * (n - 2))), 0)
            }
            1 => (u64::from(bytes[0]), 0),
            _ => unreachable!("the tail has one to fifteen bytes"),
        };
        // SAFETY: SSE2 is part of the x86-64 target.
        unsafe { _mm_set_epi64x(hi as i64, lo as i64) }
    }

    fn ghash(&self, xi: &mut [u8; BLOCK], bytes: &[u8]) {
        let (blocks, tail) = bytes.as_chunks::<BLOCK>();
        for block in blocks {
            // SAFETY: the assembly reads one complete block per call.
            unsafe { gcm_ghash(xi, &self.table, block.as_ptr(), BLOCK) };
        }
        if !tail.is_empty() {
            let last = Self::padded_aad(tail);
            // SAFETY: a fully initialized 16-byte vector kept live through the call.
            unsafe { gcm_ghash(xi, &self.table, (&raw const last).cast(), BLOCK) };
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
        // SAFETY: SSE2 is part of the x86-64 target. The vector consists of
        // the two initialized big-endian lengths consumed by the next call.
        let lengths = unsafe {
            _mm_set_epi64x(
                (len as u64 * 8).to_be() as i64,
                (aad as u64 * 8).to_be() as i64,
            )
        };
        // SAFETY: one initialized 16-byte length block.
        unsafe { gcm_ghash(&mut xi, &self.table, (&raw const lengths).cast(), BLOCK) };
        // SAFETY: one in-place block.
        unsafe { aes_ctr32(xi.as_mut_ptr(), xi.as_mut_ptr(), 1, &self.aes, tag_mask) };
        xi
    }

    /// The input and output have identical starting addresses for an in-place
    /// operation, or are completely disjoint. `tail` is the TLS record's inner
    /// content type, absent on a packet.
    unsafe fn seal_raw(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        input: *const u8,
        len: usize,
        tail: Option<u8>,
        output: *mut u8,
    ) -> [u8; BLOCK] {
        let (tag_mask, mut counter, mut xi) = self.start(nonce, aad);
        let whole = len / BLOCK * BLOCK;
        unsafe {
            if whole != 0 {
                // SAFETY: the caller pins complete blocks; ring's vector code
                // accepts a disjoint destination or an exact in-place alias.
                gcm_enc(
                    input,
                    output,
                    whole,
                    &self.aes,
                    &counter,
                    &self.table,
                    &mut xi,
                );
                Self::advance(&mut counter, whole);
            }
            let bytes = len - whole;
            let used = bytes + usize::from(tail.is_some());
            if used != 0 {
                let mut block = [0; BLOCK];
                input
                    .add(whole)
                    .copy_to_nonoverlapping(block.as_mut_ptr(), bytes);
                if let Some(tail) = tail {
                    block[bytes] = tail;
                }
                // SAFETY: one initialized block with exactly overlapping input and output.
                aes_ctr32(block.as_ptr(), block.as_mut_ptr(), 1, &self.aes, &counter);
                block
                    .as_ptr()
                    .copy_to_nonoverlapping(output.add(whole), used);
                block[used..].fill(0);
                gcm_ghash(&mut xi, &self.table, block.as_ptr(), BLOCK);
            }
        }
        self.finish(xi, &tag_mask, aad.len(), len + usize::from(tail.is_some()))
    }

    pub(super) fn seal_record(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        input: &[u8],
        tail: u8,
        output: &mut [u8],
    ) {
        assert_eq!(output.len(), input.len() + 1 + BLOCK);
        let (body, tag) = output.split_at_mut(input.len() + 1);
        // SAFETY: the two safe borrows are disjoint and carry the required extents.
        let sealed = unsafe {
            self.seal_raw(
                nonce,
                aad,
                input.as_ptr(),
                input.len(),
                Some(tail),
                body.as_mut_ptr(),
            )
        };
        tag.copy_from_slice(&sealed);
    }

    pub(super) fn seal(&self, nonce: [u8; 12], aad: &[u8], body: &mut [u8]) -> [u8; BLOCK] {
        let at = body.as_mut_ptr();
        // SAFETY: `body` is initialized, exclusive and both pointers start at its beginning.
        unsafe { self.seal_raw(nonce, aad, at, body.len(), None, at) }
    }

    pub(super) fn seal_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        input: &[u8],
        output: &mut [MaybeUninit<u8>],
    ) -> [u8; BLOCK] {
        assert_eq!(input.len(), output.len());
        // SAFETY: disjoint safe input and exclusive writable output of equal length.
        unsafe {
            self.seal_raw(
                nonce,
                aad,
                input.as_ptr(),
                input.len(),
                None,
                output.as_mut_ptr().cast(),
            )
        }
    }

    /// `len` initialized ciphertext bytes at `input`, and `len` writable
    /// bytes at `output`, with exact overlap or disjoint storage.
    unsafe fn open_raw(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        input: *const u8,
        len: usize,
        tag: &[u8; BLOCK],
        output: *mut u8,
    ) -> bool {
        let (tag_mask, mut counter, mut xi) = self.start(nonce, aad);
        let whole = len / BLOCK * BLOCK;
        unsafe {
            if whole != 0 {
                // SAFETY: full blocks under the caller's raw buffer contract.
                gcm_dec(
                    input,
                    output,
                    whole,
                    &self.aes,
                    &counter,
                    &self.table,
                    &mut xi,
                );
                Self::advance(&mut counter, whole);
            }
            let bytes = len - whole;
            if bytes != 0 {
                let mut block = [0; BLOCK];
                input
                    .add(whole)
                    .copy_to_nonoverlapping(block.as_mut_ptr(), bytes);
                gcm_ghash(&mut xi, &self.table, block.as_ptr(), BLOCK);
                // SAFETY: one initialized block with exactly overlapping input and output.
                aes_ctr32(block.as_ptr(), block.as_mut_ptr(), 1, &self.aes, &counter);
                block
                    .as_ptr()
                    .copy_to_nonoverlapping(output.add(whole), bytes);
            }
        }
        let expected = self.finish(xi, &tag_mask, aad.len(), len);
        let differ = expected
            .iter()
            .zip(tag)
            .fold(0, |differ, (a, b)| differ | (a ^ b));
        if std::hint::black_box(differ) == 0 {
            true
        } else {
            // SAFETY: the caller pins the whole decrypted range exclusively.
            unsafe { output.write_bytes(0, len) };
            false
        }
    }

    pub(super) fn open(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        body: &mut [u8],
        tag: &[u8; BLOCK],
    ) -> bool {
        let at = body.as_mut_ptr();
        // SAFETY: `body` is initialized and exclusive. Both pointers start at its beginning.
        unsafe { self.open_raw(nonce, aad, at, body.len(), tag, at) }
    }

    pub(super) fn open_into(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        input: &[u8],
        tag: &[u8; BLOCK],
        output: &mut [MaybeUninit<u8>],
    ) -> bool {
        assert_eq!(input.len(), output.len());
        // SAFETY: disjoint safe input and exclusive writable output of equal length.
        unsafe {
            self.open_raw(
                nonce,
                aad,
                input.as_ptr(),
                input.len(),
                tag,
                output.as_mut_ptr().cast(),
            )
        }
    }

    #[cfg(test)]
    fn open_record(
        &self,
        nonce: [u8; 12],
        aad: &[u8],
        sealed: &[u8],
        output: &mut [u8],
    ) -> Result<(), ()> {
        let len = sealed.len().checked_sub(BLOCK).ok_or(())?;
        let (ciphertext, tag) = sealed.split_at(len);
        // SAFETY: initialized `u8` storage may be lent for writes as MaybeUninit.
        let output =
            unsafe { std::slice::from_raw_parts_mut(output.as_mut_ptr().cast(), output.len()) };
        self.open_into(nonce, aad, ciphertext, tag.try_into().unwrap(), output)
            .then_some(())
            .ok_or(())
    }
}

#[cfg(test)]
mod tests;
