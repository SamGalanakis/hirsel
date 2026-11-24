use super::*;

fn bytes(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|chunk| u8::from_str_radix(std::str::from_utf8(chunk).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn tls_derivation_vectors() {
    let salt = bytes("000102030405060708090a0b0c");
    let info = bytes("f0f1f2f3f4f5f6f7f8f9");
    // RFC 5869 A.1. The SHA-384 row uses the same inputs through OpenSSL HKDF.
    let cases = [
        (
            TlsHash::Sha256,
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
        ),
        (
            TlsHash::Sha384,
            concat!(
                "9b5097a86038b805309076a44b3a9f38063e25b516dcbf369f394cfab43685f7",
                "48b6457763e4f0204fc5d95d1da3e62587b22eb8943d0fab6bb631a2fe9df1a6",
                "8c6ce5d56116a52005b3f122b88b39b7251f",
            ),
        ),
    ];
    for (hash, expected) in cases {
        let expected = bytes(expected);
        let mut output = vec![0; expected.len()];
        hkdf(hash, &salt, &[0x0b; 22], &[&info], &mut output).unwrap();
        assert_eq!(output, expected);
    }

    // TLS WG known answers, as pinned by rustls at a40acf9d. The SHA-256
    // output crosses three block boundaries; the SHA-384 output crosses all
    // three and ends with four bytes of its fourth block.
    let cases = [
        (
            TlsHash::Sha256,
            "9bbe436ba940f017b17652849a71db35",
            "a0ba9f936cda311827a6f796ffd5198c",
            concat!(
                "e3f229ba727be17b8d122620557cd453c2aab21d07c3d495329b52d4e61edb5a6",
                "b301791e90d35c9c9a46b4e14baf9af0fa022f7077def17abfd3797c0564bab4",
                "fbc91666e9def9b97fce34f796789baa48082d122ee42c5a72e5a5110fff7018",
                "7347b66",
            ),
        ),
        (
            TlsHash::Sha384,
            "b80b733d6ceefcdc71566ea48e5567df",
            "cd665cf6a8447dd6ff8b27555edb7465",
            concat!(
                "7b0c18e9ced410ed1804f2cfa34a336a1c14dffb4900bb5fd7942107e81c83cde",
                "9ca0faa60be9fe34f82b1233c9146a0e534cb400fed2700884f9dc236f80edd8",
                "bfa961144c9e8d792eca722a7b32fc3d416d473ebc2c5fd4abfdad05d9184259",
                "b5bf8cd4d90fa0d31e2dec479e4f1a26066f2eea9a69236a3e52655c9e9aee69",
                "1c8f3a26854308d5eaa3be85e0990703d73e56f",
            ),
        ),
    ];
    for (hash, secret, seed, expected) in cases {
        let (secret, seed, expected) = (bytes(secret), bytes(seed), bytes(expected));
        for len in [32, 100, expected.len()] {
            let mut output = vec![0; len];
            tls12_prf(
                hash,
                &secret,
                b"test label",
                &[&seed[..9], &seed[9..]],
                &mut output,
            );
            assert_eq!(output, expected[..len]);
        }
    }

    // RFC 8448 §3 gives both traffic secrets at both transitions. These
    // handshake and application contexts are already SHA-256 digests.
    let shared = bytes("8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d");
    let hello = bytes("860c06edc07858ee8e78f0e7428c58edd6b43f2ca3e6e95f02ed063cf0e1cad8");
    let mut handshake = Tls13Handshake::new(
        TlsHash::Sha256,
        Tls13Labels::Tls,
        shared.as_slice().try_into().unwrap(),
        &hello,
    )
    .unwrap();
    let expected = [
        "b3eddb126e067f35a780b3abf45e2d8f3b1a950738f52e9600746a0e27a55a21",
        "b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38",
    ];
    for (secret, expected) in handshake.traffic().secrets.iter().zip(expected) {
        assert_eq!(secret.as_bytes(), bytes(expected));
    }
    let finished = bytes("9608102a0f1ccc6db6250b7b7e417b1a000eaada3daae4777a7686c9ff83df13");
    let application = handshake.application(&finished).unwrap();
    let expected = [
        "9e40646ce79a7f9dc05af8889bce6552875afa0b06df0087f792ebb7c17504a5",
        "a11af9f05531f856ad47116b45a950328204b4f44bfb6b3a4b4f1f3fcb631643",
    ];
    for (secret, expected) in application.secrets.iter().zip(expected) {
        assert_eq!(secret.as_bytes(), bytes(expected));
    }
    assert!(handshake.application(&finished).is_none());
}

#[test]
fn rfc8448_handshake_records_use_the_sending_side() {
    use crate::{
        packet_cipher::{PacketCipher, TAG_LEN},
        secret_aead::AeadAlg,
    };
    let shared = bytes("8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d");
    let hello = bytes("860c06edc07858ee8e78f0e7428c58edd6b43f2ca3e6e95f02ed063cf0e1cad8");
    let hs = Tls13Handshake::new(
        TlsHash::Sha256,
        Tls13Labels::Tls,
        shared.as_slice().try_into().unwrap(),
        &hello,
    )
    .unwrap();
    // RFC 8448 §3. Both sides start their handshake traffic at sequence zero.
    let [server_record, client_record] = [
        include_bytes!("testdata/server-handshake-record.bin").as_slice(),
        include_bytes!("testdata/client-finished-record.bin").as_slice(),
    ];
    let [server_plain, client_plain] = [
        include_bytes!("testdata/server-handshake-plain.bin").as_slice(),
        include_bytes!("testdata/client-finished-plain.bin").as_slice(),
    ];
    for (sending, record, plain, other) in [
        (Side::Server, server_record, server_plain, Side::Client),
        (Side::Client, client_record, client_plain, Side::Server),
    ] {
        let alg = AeadAlg::Aes128Gcm;
        let key = hs.traffic().packet_keys(sending, alg);
        let cipher = PacketCipher::new(alg, key.payload()).unwrap();
        let (aad, sealed) = record.split_at(5);
        let (encrypted, tag) = sealed.split_at(sealed.len() - TAG_LEN);
        let tag = tag.try_into().unwrap();
        let mut body = encrypted.to_vec();
        cipher.open(key.iv(), aad, &mut body, tag).unwrap();
        assert_eq!(body, [plain, &[0x16]].concat());
        assert_eq!(&cipher.seal(key.iv(), aad, &mut body).unwrap(), tag);
        assert_eq!(body, encrypted);

        let other = hs.traffic().packet_keys(other, alg);
        let cipher = PacketCipher::new(alg, other.payload()).unwrap();
        assert!(cipher.open(other.iv(), aad, &mut body, tag).is_err());
    }
}
