//! SRTP AES-CM and HMAC-SHA1-80, with the RFC 3711 key-derivation rate of zero.
use ring::{error::Unspecified, hmac};
use std::mem::MaybeUninit;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};

const BLOCK: usize = 16;
pub const TAG_LEN: usize = 10;
const MAX_BODY: usize = 1 << 20;

// Layout from ring 0.17.14's include/ring-core/aes.h.
#[repr(C, align(16))]
struct AesKey {
    round_keys: [u32; 60],
    rounds: u32,
}

impl Drop for AesKey {
    fn drop(&mut self) {
        self.round_keys.zeroize();
        self.rounds.zeroize();
    }
}

type SetKey = unsafe extern "C" fn(*const u8, u32, *mut AesKey) -> i32;
type Crypt = unsafe extern "C" fn(*const u8, *mut u8, usize, &AesKey, &[u8; BLOCK]);

unsafe extern "C" {
    #[link_name = "ring_core_0_17_14__aes_nohw_set_encrypt_key"]
    fn set_key_software(key: *const u8, bits: u32, schedule: *mut AesKey) -> i32;

    #[link_name = "ring_core_0_17_14__aes_nohw_ctr32_encrypt_blocks"]
    fn crypt_software(
        input: *const u8,
        output: *mut u8,
        blocks: usize,
        key: &AesKey,
        counter: &[u8; BLOCK],
    );

    #[cfg(target_arch = "x86_64")]
    #[link_name = "ring_core_0_17_14__aes_hw_set_encrypt_key_base"]
    fn set_key_aesni(key: *const u8, bits: u32, schedule: *mut AesKey) -> i32;

    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    #[link_name = "ring_core_0_17_14__aes_hw_set_encrypt_key"]
    fn set_key_armv8(key: *const u8, bits: u32, schedule: *mut AesKey) -> i32;

    #[cfg(any(
        target_arch = "x86_64",
        all(target_arch = "aarch64", target_endian = "little")
    ))]
    #[link_name = "ring_core_0_17_14__aes_hw_ctr32_encrypt_blocks"]
    fn crypt_hardware(
        input: *const u8,
        output: *mut u8,
        blocks: usize,
        key: &AesKey,
        counter: &[u8; BLOCK],
    );
}

fn provider() -> (SetKey, Crypt) {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("aes") && std::arch::is_x86_feature_detected!("ssse3") {
        return (set_key_aesni, crypt_hardware);
    }
    #[cfg(all(target_arch = "aarch64", target_endian = "little"))]
    if std::arch::is_aarch64_feature_detected!("aes") {
        return (set_key_armv8, crypt_hardware);
    }
    (set_key_software, crypt_software)
}

struct AesCtr {
    key: Box<AesKey>,
    crypt: Crypt,
}

impl AesCtr {
    fn new(key: &[u8]) -> Result<Self, Unspecified> {
        if !matches!(key.len(), 16 | 32) {
            return Err(Unspecified);
        }
        let (set_key, crypt) = provider();
        let mut slot = Box::new(AesKey {
            round_keys: [0; 60],
            rounds: 0,
        });
        // SAFETY: the provider matches the CPU's features, key has exactly
        // its stated width, and the schedule holds every AES round.
        let status = unsafe { set_key(key.as_ptr(), key.len() as u32 * 8, &mut *slot) };
        if status != 0 {
            return Err(Unspecified);
        }
        Ok(Self { key: slot, crypt })
    }

    /// Input holds len initialized bytes; output holds len writable bytes,
    /// with exact overlap or disjoint storage. Callers start with an all-zero
    /// low 16-bit counter and stay within MAX_BODY.
    unsafe fn apply(
        &self,
        mut counter: [u8; BLOCK],
        input: *const u8,
        output: *mut u8,
        len: usize,
    ) {
        let blocks = len / BLOCK;
        let tail = len % BLOCK;
        if blocks != 0 {
            // SAFETY: the caller pins the full blocks. Ring accepts equal or
            // disjoint pointers, and this key was initialized by its backend.
            unsafe { (self.crypt)(input, output, blocks, &self.key, &counter) };
        }
        if tail != 0 {
            let offset = blocks * BLOCK;
            let mut block = Zeroizing::new([0; BLOCK]);
            // SAFETY: the remaining bytes are initialized and the stack
            // block is disjoint from the caller's input and output.
            unsafe {
                input
                    .add(offset)
                    .copy_to_nonoverlapping(block.as_mut_ptr(), tail)
            };
            counter[14..].copy_from_slice(&(blocks as u16).to_be_bytes());
            let at = block.as_mut_ptr();
            // SAFETY: one complete, initialized block with exact overlap.
            unsafe { (self.crypt)(at, at, 1, &self.key, &counter) };
            // SAFETY: output is live through its final tail bytes.
            unsafe {
                block
                    .as_ptr()
                    .copy_to_nonoverlapping(output.add(offset), tail)
            };
        }
    }

    fn in_place(&self, counter: [u8; BLOCK], body: &mut [u8]) {
        let ptr = body.as_mut_ptr();
        // SAFETY: the single exclusive borrow permits exact in-place overlap.
        unsafe { self.apply(counter, ptr, ptr, body.len()) };
    }

    fn crypt_into<'a>(
        &self,
        counter: [u8; BLOCK],
        body: &[u8],
        dest: &'a mut [MaybeUninit<u8>],
    ) -> &'a [u8] {
        assert_eq!(body.len(), dest.len());
        // SAFETY: input and output borrows are disjoint, and apply writes
        // every byte of the destination.
        unsafe {
            self.apply(counter, body.as_ptr(), dest.as_mut_ptr().cast(), body.len());
            std::slice::from_raw_parts(dest.as_ptr().cast(), dest.len())
        }
    }
}

/// One master key expansion for a direction's six SRTP and SRTCP labels.
pub struct SrtpKdf {
    aes: AesCtr,
    salt: Zeroizing<[u8; BLOCK]>,
}

impl SrtpKdf {
    pub fn new(key: &[u8], salt: &[u8]) -> Result<Self, Unspecified> {
        if !matches!(salt.len(), 12 | 14) {
            return Err(Unspecified);
        }
        let mut base = Zeroizing::new([0; BLOCK]);
        base[..salt.len()].copy_from_slice(salt);
        Ok(Self {
            aes: AesCtr::new(key)?,
            salt: base,
        })
    }

    pub fn derive(&self, label: u8, out: &mut [u8]) {
        assert!(out.len() <= MAX_BODY);
        let mut counter = *self.salt;
        counter[7] ^= label;
        out.fill(0);
        self.aes.in_place(counter, out);
        counter.zeroize();
    }
}

/// An expanded AES-128-CM and HMAC-SHA1-80 key for one packet space.
pub struct SrtpCipher {
    aes: AesCtr,
    mac: hmac::Key,
}

impl SrtpCipher {
    pub fn new(key: &[u8; 16], auth: &[u8; 20]) -> Result<Self, Unspecified> {
        Ok(Self {
            aes: AesCtr::new(key)?,
            mac: hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, auth),
        })
    }

    fn check(nonce: [u8; BLOCK], len: usize) -> Result<(), Unspecified> {
        (len <= MAX_BODY && nonce[14..] == [0; 2])
            .then_some(())
            .ok_or(Unspecified)
    }

    fn tag(&self, header: &[u8], ciphertext: &[u8], extra: &[u8]) -> [u8; TAG_LEN] {
        let mut mac = hmac::Context::with_key(&self.mac);
        mac.update(header);
        mac.update(ciphertext);
        mac.update(extra);
        mac.sign().as_ref()[..TAG_LEN].try_into().unwrap()
    }

    pub fn seal(
        &self,
        nonce: [u8; BLOCK],
        header: &[u8],
        extra: &[u8],
        body: &mut [u8],
    ) -> Result<[u8; TAG_LEN], Unspecified> {
        Self::check(nonce, body.len())?;
        self.aes.in_place(nonce, body);
        Ok(self.tag(header, body, extra))
    }

    pub fn seal_into(
        &self,
        nonce: [u8; BLOCK],
        header: &[u8],
        extra: &[u8],
        plain: &[u8],
        sealed: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified> {
        Self::check(nonce, plain.len())?;
        assert_eq!(sealed.len(), plain.len() + TAG_LEN);
        let (body, tag) = sealed.split_at_mut(plain.len());
        let ciphertext = self.aes.crypt_into(nonce, plain, body);
        tag.write_copy_of_slice(&self.tag(header, ciphertext, extra));
        Ok(())
    }

    pub fn open(
        &self,
        nonce: [u8; BLOCK],
        header: &[u8],
        extra: &[u8],
        body: &mut [u8],
        tag: &[u8; TAG_LEN],
    ) -> Result<(), Unspecified> {
        Self::check(nonce, body.len())?;
        if !bool::from(self.tag(header, body, extra).as_slice().ct_eq(tag)) {
            body.fill(0);
            return Err(Unspecified);
        }
        self.aes.in_place(nonce, body);
        Ok(())
    }

    pub fn open_into(
        &self,
        nonce: [u8; BLOCK],
        header: &[u8],
        extra: &[u8],
        sealed: &[u8],
        plain: &mut [MaybeUninit<u8>],
    ) -> Result<(), Unspecified> {
        let len = sealed.len().checked_sub(TAG_LEN).ok_or(Unspecified)?;
        Self::check(nonce, len)?;
        assert_eq!(plain.len(), len);
        let (body, tag) = sealed.split_at(len);
        if !bool::from(self.tag(header, body, extra).as_slice().ct_eq(tag)) {
            plain.fill(MaybeUninit::new(0));
            return Err(Unspecified);
        }
        self.aes.crypt_into(nonce, body, plain);
        Ok(())
    }
}

mod session;
pub use session::{SrtpKeys, SrtpProfile, SrtpSessionKey, SrtpSpace};

#[cfg(test)]
mod tests;
