use hirsel_core_net::{
    verify_ed25519, verify_p256_asn1, verify_p256_fixed, verify_p384_asn1, verify_p384_fixed,
    verify_rsa_pkcs1_sha256, verify_rsa_pss_sha256,
};

macro_rules! verifier {
    ($name:ident, $verify:ident, [$($key:ident, $key_len:ident),+]) => {
        /// Verify a signature over the message. Returns 1 on success and 0 otherwise.
        ///
        /// # Safety
        /// Each nonempty range must contain its length in initialized bytes in one
        /// allocation, readable and immutable for the call. Its length must fit
        /// in `isize`. Empty ranges may have null pointers.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(
            $($key: *const u8, $key_len: u64,)+
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
            u64::from($verify(
                $(range($key, $key_len),)+
                range(msg, msg_len),
                range(sig, sig_len),
            ))
        }
    };
}

verifier!(hirsel_core_ed25519_verify, verify_ed25519, [key, key_len]);
verifier!(hirsel_core_p256_verify, verify_p256_fixed, [key, key_len]);
verifier!(
    hirsel_core_p256_verify_der,
    verify_p256_asn1,
    [key, key_len]
);
verifier!(hirsel_core_p384_verify, verify_p384_fixed, [key, key_len]);
verifier!(
    hirsel_core_p384_verify_der,
    verify_p384_asn1,
    [key, key_len]
);
verifier!(
    hirsel_core_rsa_verify_pkcs1_sha256,
    verify_rsa_pkcs1_sha256,
    [n, n_len, e, e_len]
);
verifier!(
    hirsel_core_rsa_verify_pss_sha256,
    verify_rsa_pss_sha256,
    [n, n_len, e, e_len]
);
