use super::*;
use ring::digest;

#[test]
fn vaes_seals_and_opens_packet_buffers() {
    // Python cryptography AESGCM over a 12-byte RTP header and 1,200-byte payload.
    let plain: [u8; 1200] = std::array::from_fn(|i| (i % 251) as u8);
    let aad = [0x80, 0x60, 0x12, 0x34, 1, 2, 3, 4, 0xaa, 0xbb, 0xcc, 0xdd];
    let nonce = std::array::from_fn(|i| i as u8);
    let key: [u8; 32] = std::array::from_fn(|i| i as u8);
    for (algorithm, key_len, expected) in [
        (
            AeadAlg::Aes128Gcm,
            16,
            "15c25197063371f7087f34ac863063d94c27a7c0e5502d498b35b94ce7ad568b",
        ),
        (
            AeadAlg::Aes256Gcm,
            32,
            "f982022645f5c027d36e300d1376eecaa0ce263851351dca84fd8dc76a52f103",
        ),
    ] {
        let cipher = PacketCipher::new(algorithm, &key[..key_len]).unwrap();
        assert!(cipher.writes_into());
        let mut output = [MaybeUninit::uninit(); 1216];
        cipher.seal_into(nonce, &aad, &plain, &mut output).unwrap();
        // SAFETY: a successful seal writes the entire ciphertext and tag.
        let sealed = output.map(|byte| unsafe { byte.assume_init() });
        let actual: String = digest::digest(&digest::SHA256, &sealed)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(actual, expected);

        let mut inline = sealed;
        inline[..1200].copy_from_slice(&plain);
        let tag = cipher.seal(nonce, &aad, &mut inline[..1200]).unwrap();
        inline[1200..].copy_from_slice(&tag);
        assert_eq!(inline, sealed);
        assert_eq!(cipher.open(nonce, &aad, &mut inline).unwrap(), plain);

        let mut separate = [MaybeUninit::uninit(); 1200];
        cipher
            .open_into(nonce, &aad, &sealed, &mut separate)
            .unwrap();
        // SAFETY: a successful open writes the entire plaintext.
        assert_eq!(separate.map(|byte| unsafe { byte.assume_init() }), plain);

        inline = sealed;
        inline[1215] ^= 1;
        let rejected_tag: [u8; 16] = inline[1200..].try_into().unwrap();
        assert!(cipher.open(nonce, &aad, &mut inline).is_err());
        assert_eq!(inline[..1200], [0; 1200]);
        assert_eq!(inline[1200..], rejected_tag);
    }
}
