use super::*;

fn hex<const N: usize>(text: &str) -> [u8; N] {
    assert_eq!(text.len(), N * 2);
    std::array::from_fn(|i| u8::from_str_radix(&text[i * 2..][..2], 16).unwrap())
}

#[test]
fn packet_buffers_match_aead_and_quic_vectors() {
    // AES-GCM known answers for a zero key, 96-bit IV and one plaintext block.
    for (algorithm, key_len, expected) in [
        (
            AeadAlg::Aes128Gcm,
            16,
            hex::<32>("0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf"),
        ),
        (
            AeadAlg::Aes256Gcm,
            32,
            hex::<32>("cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919"),
        ),
    ] {
        let key = PacketCipher::new(algorithm, &[0; 32][..key_len]).unwrap();
        let nonce = [0; 12];
        let mut packet = [0; 16];
        let tag = key.seal(nonce, &[], &mut packet).unwrap();
        assert_eq!(packet, expected[..16]);
        assert_eq!(tag, expected[16..]);
        let mut dst = [MaybeUninit::uninit(); 32];
        key.seal_into(nonce, &[], &[0; 16], &mut dst).unwrap();
        // SAFETY: a successful seal initialized every ciphertext and tag byte.
        let sealed = dst.map(|byte| unsafe { byte.assume_init() });
        assert_eq!(sealed, expected);

        let mut out = [MaybeUninit::uninit(); 16];
        key.open_into(nonce, &[], &sealed, &mut out).unwrap();
        // SAFETY: a successful open initialized the whole body.
        assert_eq!(out.map(|byte| unsafe { byte.assume_init() }), [0; 16]);
        key.open(nonce, &[], &mut packet, &tag).unwrap();
        assert_eq!(packet, [0; 16]);

        let mut altered_tag = tag;
        altered_tag[0] ^= 1;
        packet.copy_from_slice(&sealed[..16]);
        assert!(key.open(nonce, &[], &mut packet, &altered_tag).is_err());
        assert_eq!(packet, [0; 16]);
    }

    // RFC 9001 Appendix A.2, where the header-protection key is distinct.
    let mask = HeaderMask::new(
        AeadAlg::Aes128Gcm,
        &hex::<16>("9f50449e04a0e810283a1e9933adedd2"),
    )
    .unwrap();
    assert_eq!(
        mask.mask(&hex("d1b1c98dd7689fb8ec11d242b123dc9b")),
        hex("437b9aec36")
    );
}
