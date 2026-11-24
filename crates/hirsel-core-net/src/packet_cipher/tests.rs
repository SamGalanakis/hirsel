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

#[test]
fn quic_path_id_requires_multipath() {
    use crate::packet_nonce::{CounterError, NonceFormat, ReceiverNonce, SenderCounter};

    // RFC 9001 A.2, client Initial, packet number 2.
    let base = [
        0xfa, 0x04, 0x4b, 0x2f, 0x42, 0xa3, 0xfd, 0x3b, 0x46, 0xfb, 0x25, 0x5c,
    ];
    let single = ReceiverNonce::new(NonceFormat::Quic, base);
    assert_eq!(
        single.nonce(0, 2).unwrap(),
        [
            0xfa, 0x04, 0x4b, 0x2f, 0x42, 0xa3, 0xfd, 0x3b, 0x46, 0xfb, 0x25, 0x5e
        ]
    );
    assert_eq!(single.nonce(3, 2), Err(CounterError::Range));
    assert!(SenderCounter::new(NonceFormat::Quic, base, 3, 0).is_none());

    // draft-ietf-quic-multipath-15 §2.4, plus the same packet on path 4.
    let base = [
        0x6b, 0x26, 0x11, 0x4b, 0x9c, 0xba, 0x2b, 0x63, 0xa9, 0xe8, 0xdd, 0x4f,
    ];
    let receive = ReceiverNonce::new(NonceFormat::QuicMultipath, base);
    let nonce3 = [
        0x6b, 0x26, 0x11, 0x48, 0x9c, 0xba, 0x2b, 0x63, 0xa9, 0xe8, 0x09, 0x7e,
    ];
    let nonce4 = [
        0x6b, 0x26, 0x11, 0x4f, 0x9c, 0xba, 0x2b, 0x63, 0xa9, 0xe8, 0x09, 0x7e,
    ];
    assert_eq!(receive.nonce(3, 54_321).unwrap(), nonce3);
    assert_eq!(receive.nonce(4, 54_321).unwrap(), nonce4);
    for (path, expected) in [(3, nonce3), (4, nonce4)] {
        let send = SenderCounter::new(NonceFormat::QuicMultipath, base, path, 0).unwrap();
        assert_eq!(send.check(54_321).unwrap(), expected);
    }
    assert_eq!(receive.nonce(1 << 32, 54_321), Err(CounterError::Range));
    assert_eq!(receive.nonce(3, 1 << 62), Err(CounterError::Range));
}
