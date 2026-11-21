use hirsel_core_net::{SignatureAlgorithm, verify_signature};

macro_rules! verifier {
    ($name:ident, $algorithm:ident) => {
        /// Verify a signature over the message. Returns 1 on success and 0 otherwise.
        ///
        /// # Safety
        /// Each nonempty range must contain its length in initialized bytes in one
        /// allocation, readable and immutable for the call. Its length must fit
        /// in `isize`. Empty ranges may have null pointers.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(
            key: *const u8,
            key_len: u64,
            msg: *const u8,
            msg_len: u64,
            sig: *const u8,
            sig_len: u64,
        ) -> u64 {
            let range = |data: *const u8, len: u64| {
                if len == 0 {
                    &[]
                } else {
                    // SAFETY: the ABI requires readable, immutable nonempty ranges.
                    unsafe { std::slice::from_raw_parts(data, len as usize) }
                }
            };
            u64::from(verify_signature(
                SignatureAlgorithm::$algorithm,
                range(key, key_len),
                range(msg, msg_len),
                range(sig, sig_len),
            ))
        }
    };
}

verifier!(hirsel_core_ed25519_verify, Ed25519);
verifier!(hirsel_core_p256_verify, P256Fixed);
verifier!(hirsel_core_p256_verify_der, P256Der);
