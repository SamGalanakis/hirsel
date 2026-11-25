use super::*;

fn hex<const N: usize>(s: &str) -> [u8; N] {
    assert_eq!(s.len(), 2 * N);
    std::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
}

#[test]
fn srtp_derives_authenticates_and_encrypts() {
    // RFC 3711 B.3. The RTP ciphertext and both tags below use Python AES-CTR
    // and HMAC-SHA1 with the independently calculated RFC 3711 counters.
    let master: [u8; 16] = hex("e1f97a0d3e018be0d64fa32c06de4139");
    let salt: [u8; 14] = hex("0ec675ad498afeebb6960b3aabe6");
    let kdf = SrtpKdf::new(&master, &salt).unwrap();
    let mut encryption = [0; 16];
    let mut authentication = [0; 20];
    let mut salting = [0; 14];
    kdf.derive(0, &mut encryption);
    kdf.derive(1, &mut authentication);
    kdf.derive(2, &mut salting);
    assert_eq!(encryption, hex("c61e7a93744f39ee10734afe3ff7a087"));
    assert_eq!(
        authentication,
        hex("cebe321f6ff7716b6fd4ab49af256a156d38baa4")
    );
    assert_eq!(salting, hex("30cbbc08863d8c85d49db34a9ae1"));

    let cipher = SrtpCipher::new(&encryption, &authentication).unwrap();
    let header: [u8; 12] = hex("8060123401020304cafebabe");
    let nonce = hex("30cbbc084cc3363bd49db34b88d50000");
    let roc = 1_u32.to_be_bytes();
    let plain: [u8; 37] = std::array::from_fn(|i| i as u8);
    let expected: [u8; 47] = hex(
        "bc38cc60109e93e08745863fd43d10020cac2a920e035392eeb8469cb520e7ceb876324530e7b3eee2d1d132e5f530",
    );

    let mut body = plain;
    let tag = cipher.seal(nonce, &header, &roc, &mut body).unwrap();
    assert_eq!(&body, &expected[..37]);
    assert_eq!(&tag, &expected[37..]);
    cipher.open(nonce, &header, &roc, &mut body, &tag).unwrap();
    assert_eq!(body, plain);

    let mut sealed = [MaybeUninit::uninit(); 47];
    cipher
        .seal_into(nonce, &header, &roc, &plain, &mut sealed)
        .unwrap();
    // SAFETY: successful seal initialized the entire destination.
    let sealed = sealed.map(|byte| unsafe { byte.assume_init() });
    assert_eq!(sealed, expected);
    let mut opened = [MaybeUninit::uninit(); 37];
    cipher
        .open_into(nonce, &header, &roc, &sealed, &mut opened)
        .unwrap();
    // SAFETY: successful open initialized the entire destination.
    assert_eq!(opened.map(|byte| unsafe { byte.assume_init() }), plain);

    let mut forged = expected;
    forged[46] ^= 1;
    assert!(
        cipher
            .open_into(nonce, &header, &roc, &forged, &mut opened)
            .is_err()
    );
    // SAFETY: a delivered authentication refusal initializes all output to zero.
    assert_eq!(opened.map(|byte| unsafe { byte.assume_init() }), [0; 37]);
    assert!(
        cipher
            .open_into(nonce, &header, &0_u32.to_be_bytes(), &sealed, &mut opened)
            .is_err()
    );

    let rtcp: [u8; 20] = hex("80c80004cafebabe000102030405060708090a0b");
    kdf.derive(3, &mut encryption);
    kdf.derive(4, &mut authentication);
    let rtcp_cipher = SrtpCipher::new(&encryption, &authentication).unwrap();
    let tag = rtcp_cipher
        .seal([0; 16], &rtcp, &2_u32.to_be_bytes(), &mut [])
        .unwrap();
    assert_eq!(tag, hex("9f9041f039601a246bcd"));

    // libSRTP srtp_validate_gcm and the same AES-256 derivation using Python.
    let master: [u8; 32] = std::array::from_fn(|i| i as u8);
    let salt: [u8; 12] = std::array::from_fn(|i| (0xa0 + i) as u8);
    for (key, expected_key, expected_salt) in [
        (
            &master[..16],
            "077c6143cb221bc355ff23d5f984a16e",
            "9af3e95364ebac9c99c5a7c4",
        ),
        (
            &master[..],
            "b7a435ce454463b760dc82c838468a115c699625af4b93a0f8220a2a6119c5d0",
            "944bd21c268a962cd09c674a",
        ),
    ] {
        let kdf = SrtpKdf::new(key, &salt).unwrap();
        let mut derived = [0; 32];
        kdf.derive(0, &mut derived[..key.len()]);
        let mut salting = [0; 12];
        kdf.derive(2, &mut salting);
        let actual = derived[..key.len()]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(actual, expected_key);
        assert_eq!(salting, hex(expected_salt));
    }
}

#[test]
fn srtp_import_direction_matches_libsrtp() {
    use crate::{ImportUse, SecretPolicy};
    let direction = |policy| SecretPolicy::parse(policy).unwrap().import().unwrap();
    let send = direction("srtp_import(send)");
    let receive = direction("srtp_import(receive)");
    let master = hex::<30>("e1f97a0d3e018be0d64fa32c06de41390ec675ad498afeebb6960b3aabe6");
    let tx = SrtpKeys::import(SrtpProfile::Aes128CmHmacSha1_80, send, &master).unwrap();
    let rx = SrtpKeys::import(SrtpProfile::Aes128CmHmacSha1_80, receive, &master).unwrap();
    assert!(
        SrtpKeys::import(
            SrtpProfile::Aes128CmHmacSha1_80,
            ImportUse::TrafficSend,
            &master
        )
        .is_err()
    );
    assert!(SrtpKeys::import(SrtpProfile::Aes128CmHmacSha1_80, send, &master[..28]).is_err());

    // libSRTP fd08747f, srtp_validate, RTP and SRTCP on SSRC 0xcafebabe.
    for (space, index, header, extra, ciphertext_and_tag) in [
        (
            SrtpSpace::Rtp,
            0x1234,
            &hex::<12>("800f1234decafbadcafebabe")[..],
            [0; 4],
            hex::<26>("4e55dc4ce79978d88ca4d215949d2402b78d6acc99ea179b8dbb"),
        ),
        (
            SrtpSpace::Rtcp,
            1,
            &hex::<8>("81c8000bcafebabe")[..],
            [0x80, 0, 0, 1],
            hex::<26>("7128035be487b9bdbef89041f977a5a8993e08cd54d6c1230798"),
        ),
    ] {
        assert!(tx.keys(space, false).is_none());
        assert!(rx.keys(space, true).is_none());
        let seal = tx.keys(space, true).unwrap();
        let seal_nonce = seal.cm_nonce(0xcafebabe, index).unwrap();
        let seal = seal.expand_cm().unwrap();
        let open = rx.keys(space, false).unwrap();
        let open_nonce = open.cm_nonce(0xcafebabe, index).unwrap();
        let open = open.expand_cm().unwrap();
        let mut body = [0xab; 16];
        let tag = seal.seal(seal_nonce, header, &extra, &mut body).unwrap();
        assert_eq!([&body[..], &tag[..]].concat(), ciphertext_and_tag);
        let mut received: [u8; 16] = ciphertext_and_tag[..16].try_into().unwrap();
        open.open(
            open_nonce,
            header,
            &extra,
            &mut received,
            ciphertext_and_tag[16..].try_into().unwrap(),
        )
        .unwrap();
        assert_eq!(received, [0xab; 16]);
    }

    use crate::{
        packet_cipher::PacketCipher,
        packet_nonce::{NonceFormat, ReceiverNonce},
    };
    let master = hex::<28>("000102030405060708090a0b0c0d0e0fa0a1a2a3a4a5a6a7a8a9aaab");
    let want = hex::<32>("c5002ede04cfdd2eb91159e0880aa06ed2976826f796b201df3131a127e8a392");
    let header = hex::<12>("800f1234decafbadcafebabe");
    let mut material = Vec::from(master);
    for edge in [0, 2] {
        material.resize(master.len() + edge, 0);
        let send = SrtpKeys::import(SrtpProfile::AeadAes128Gcm, send, &material).unwrap();
        let keys = send.keys(SrtpSpace::Rtp, true).unwrap();
        let (algorithm, payload, salt) = keys.aead_material().unwrap();
        let nonce = ReceiverNonce::new(NonceFormat::SrtpRtp, salt)
            .nonce(0xcafebabe, 0x1234)
            .unwrap();
        let cipher = PacketCipher::new(algorithm, payload).unwrap();
        let mut body = [0xab; 16];
        let tag = cipher.seal(nonce, &header, &mut body).unwrap();
        assert_eq!([&body[..], &tag[..]].concat(), want);
    }
}
