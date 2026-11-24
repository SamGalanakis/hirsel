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

#[test]
fn mask_keeps_a_populated_sender_ledger_until_retire() {
    use crate::{
        packet_epoch::{DirectionKeys, Epoch, IssueError},
        packet_nonce::NonceFormat,
        secret_aead::AeadAlg,
    };
    use std::{cell::Cell, rc::Rc};
    struct Charge(Rc<Cell<u64>>, u64);
    impl Drop for Charge {
        fn drop(&mut self) {
            self.0.set(self.0.get() - self.1);
        }
    }
    for opaque in [false, true] {
        let paid = Rc::new(Cell::new(0));
        let admit = |n| {
            paid.set(paid.get() + n);
            Charge(paid.clone(), n)
        };
        let traffic = Tls13Traffic::quic_initial(&bytes("8394c8f03e515708")).unwrap();
        let alg = AeadAlg::Aes128Gcm;
        let key = |side| {
            if opaque {
                DirectionKeys::Opaque
            } else {
                DirectionKeys::Live(traffic.packet_keys(side, alg))
            }
        };
        let mut epoch = Epoch::new(NonceFormat::Quic, alg, key(Side::Client), key(Side::Server));
        let sealer = epoch
            .sealer(0, 2, |n| {
                assert_eq!(n, 1040);
                Ok((admit(1024), admit(16)))
            })
            .unwrap();
        assert_eq!(paid.get(), 1040);
        drop(sealer);
        assert_eq!(paid.get(), 16);
        let (mask, charge) = epoch
            .header_mask(true, |n| {
                assert_eq!(n, 512);
                Ok(admit(n))
            })
            .unwrap();
        assert_eq!(paid.get(), 528);
        assert_eq!(
            epoch
                .sealer(0, 3, |_| panic!("duplicate admits nothing"))
                .err(),
            Some(IssueError::Issued)
        );
        epoch.retire();
        assert_eq!(paid.get(), 512);
        assert_eq!(
            epoch
                .sealer(0, 3, |_| panic!("retired admits nothing"))
                .err(),
            Some(IssueError::Spent)
        );
        assert_eq!(
            epoch.opener(|_| panic!("retired admits nothing")).err(),
            Some(IssueError::Spent)
        );
        if opaque {
            assert!(mask.is_none());
        } else {
            let sample = bytes("d1b1c98dd7689fb8ec11d242b123dc9b");
            assert_eq!(
                mask.unwrap().mask(&sample.try_into().unwrap()).as_slice(),
                bytes("437b9aec36")
            );
        }
        drop(charge);
        assert_eq!(paid.get(), 0);
    }
}

#[test]
fn dtls12_record_keys_finished_and_exporter_match_openssl() {
    use super::dtls12::Dtls12Keys;
    use crate::{packet_cipher::PacketCipher, secret_aead::AeadAlg};
    let shared = bytes("8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d");
    let client = std::array::from_fn(|index| index as u8);
    let server = std::array::from_fn(|index| 160 + index as u8);
    let seq = 0x0001_0000_0000_0001u64.to_be_bytes();
    let mut aad = [0; 13];
    aad[..8].copy_from_slice(&seq);
    aad[8..].copy_from_slice(&[23, 0xfe, 0xfd, 0, 22]);
    // P_hash with Python hmac, independently capped at each requested length.
    // OpenSSL AES-GCM and ChaCha20-Poly1305 provide the expected records.
    for (algorithm, ems, record, exporter, finished) in [
        (
            AeadAlg::Aes128Gcm,
            false,
            "5838181a5c0e550b83a796bc0420474963330d92aa3ee92302d6c35801c0869e745a4d1344d6",
            "75c76a55ba0558066cd1941b4a47836e558dc4e0f19dfe9e32f729b08832bdd638e40b88a3b77eb4c8e5bd8d19a2a7c86908cb4d93ec8d5e",
            "cdf7b3a0776ff52c4601258e",
        ),
        (
            AeadAlg::Aes128Gcm,
            true,
            "e064f066f85a8648b81b5c64ee9b88c57680bf39d36cb15997a8a6b94d6edd661b66f6d048ba",
            "2378f83434d06bf54074c98305451b49d694859425006ed23ae8909db145dacd8b046e1dc515e4a3145b174187044d6a9c373627f61b1764",
            "55c67a1e89d48a06231bfcc9",
        ),
        (
            AeadAlg::Aes256Gcm,
            false,
            "64adde471a0f0f1652eb3bcd46870341d070710b9b682c68595c0fd8ae7aadfb87865970df0d",
            "613d93e5bd40ce17e28fa24d7c4df4f6c245712e1afb397927a98f8ef9b07916f50fec0ab03d95a3dbe61969b162ccea4486a1e4cf982455",
            "1725abd8c2457a08d6a35e8e",
        ),
        (
            AeadAlg::Aes256Gcm,
            true,
            "c7895b6b0c565abe094851ac9f1aa5eb6843f3dfe6c846cbce835343546e739e87692dbcdffb",
            "b51c305f0737929f4ab29f20b2548715b6c6a7372bd3373f9ebece6050631903927a042bc3b1db4e1a976f721fd72769699e1279fc8c6689",
            "c7d34b30663fda4c330392ad",
        ),
        (
            AeadAlg::ChaCha20Poly1305,
            false,
            "9341ef93d6f84fb2235c3e8552d3f190e7c65c129d79c3188f7d9191224da2de5d386af172a7",
            "75c76a55ba0558066cd1941b4a47836e558dc4e0f19dfe9e32f729b08832bdd638e40b88a3b77eb4c8e5bd8d19a2a7c86908cb4d93ec8d5e",
            "cdf7b3a0776ff52c4601258e",
        ),
        (
            AeadAlg::ChaCha20Poly1305,
            true,
            "aec8a50ce0e2ce8ae0adddda95021f655764a34d32a0e82934ff90f9b30260f5ee0358ee524d",
            "2378f83434d06bf54074c98305451b49d694859425006ed23ae8909db145dacd8b046e1dc515e4a3145b174187044d6a9c373627f61b1764",
            "55c67a1e89d48a06231bfcc9",
        ),
    ] {
        let hash_len = if algorithm == AeadAlg::Aes256Gcm {
            48
        } else {
            32
        };
        let transcript = [0x5e; 48];
        let keys = Dtls12Keys::new(
            algorithm,
            shared.as_slice().try_into().unwrap(),
            client,
            server,
            ems.then_some(&transcript[..hash_len]),
        )
        .unwrap();
        let mut nonce = keys.iv(Side::Client);
        for (dst, src) in nonce[4..].iter_mut().zip(seq) {
            *dst ^= src;
        }
        let cipher = PacketCipher::new(algorithm, keys.payload(Side::Client)).unwrap();
        let mut plaintext = b"DTLS record transcript".to_vec();
        let tag = cipher.seal(nonce, &aad, &mut plaintext).unwrap();
        plaintext.extend(tag);
        assert_eq!(plaintext, bytes(record), "{algorithm:?} EMS={ems}");
        let mut output = [0; 56];
        keys.srtp_export(&mut output);
        assert_eq!(
            output.as_slice(),
            bytes(exporter),
            "{algorithm:?} EMS={ems}"
        );
        let finished_bytes = keys
            .finished(Side::Server, &[0xab; 48][..hash_len])
            .unwrap();
        assert_eq!(
            finished_bytes.as_slice(),
            bytes(finished),
            "{algorithm:?} EMS={ems}"
        );
        assert!(keys.finished(Side::Server, &[]).is_none());
    }
}

#[test]
fn dtls13_seq_beyond_48_bits() {
    use crate::{
        ImportUse,
        packet_cipher::{PacketCipher, TAG_LEN},
        packet_epoch::{Epoch, IssueError},
        packet_nonce::{CounterError, NonceFormat},
        secret_aead::AeadAlg,
    };

    let algorithm = AeadAlg::Aes128Gcm;
    let format = NonceFormat::Dtls13;
    let secret: [u8; 32] = std::array::from_fn(|i| i as u8);
    let mut send =
        Epoch::<()>::import_traffic(format, algorithm, ImportUse::TrafficSend, &secret).unwrap();
    let mut receive =
        Epoch::<()>::import_traffic(format, algorithm, ImportUse::TrafficReceive, &secret).unwrap();
    assert_eq!(
        send.opener(|_| panic!("send policy authorizes no opener"))
            .err(),
        Some(IssueError::Invalid)
    );
    assert_eq!(
        receive
            .sealer(0, 0, |_| panic!("receive policy authorizes no sealer"))
            .err(),
        Some(IssueError::Invalid)
    );
    assert_eq!(
        send.header_mask(false, |_| panic!("send policy authorizes no receiver mask"))
            .err(),
        Some(IssueError::Invalid)
    );
    assert_eq!(
        receive
            .header_mask(true, |_| panic!("receive policy authorizes no sender mask"))
            .err(),
        Some(IssueError::Invalid)
    );
    let (_, _) = send.header_mask(true, |_| Ok(())).unwrap();
    let (_, _) = receive.header_mask(false, |_| Ok(())).unwrap();
    let mut sealer = send.sealer(0, 0, |_| Ok(((), ()))).unwrap();
    let opener = receive.opener(|_| Ok(())).unwrap();
    use crate::packet_protection::Key;
    let (Some(Key::Aead(send)), Some(Key::Aead(receive))) = (sealer.key, opener.key) else {
        panic!("DTLS 1.3 uses AEAD keys");
    };
    let encryption = PacketCipher::new(algorithm, send).unwrap();
    let decryption = PacketCipher::new(algorithm, receive).unwrap();
    let aad = [0x2c, 0, 1];
    let plain = b"dtls13 width";
    // Python hmac with RFC 9147 §5.9's label prefix, and OpenSSL AES-GCM.
    for (seq, expected) in [
        (
            1u64 << 48,
            "3b9f811ad532922364df49738c048631aa857afa5ef2d2d2009a38c9",
        ),
        (
            u64::MAX,
            "530d0f604f3ba02a4e41432937e18849fb7f48778b51d0d557b46679",
        ),
    ] {
        let nonce = sealer.counter.check(seq).unwrap();
        let mut body = plain.to_vec();
        let tag = encryption.seal(nonce, &aad, &mut body).unwrap();
        assert_eq!([body.as_slice(), tag.as_slice()].concat(), bytes(expected));
        let nonce = opener.nonce.nonce(0, seq).unwrap();
        decryption.open(nonce, &aad, &mut body, &tag).unwrap();
        assert_eq!(body.as_slice(), plain);
        assert_eq!(tag.len(), TAG_LEN);
        sealer.counter.commit(seq);
    }
    assert_eq!(sealer.counter.check(u64::MAX), Err(CounterError::Reused));
}

#[test]
fn dtls12_srtp_child_uses_each_exporter_direction_once() {
    use crate::{
        secret_aead::AeadAlg,
        srtp_cipher::{SrtpProfile, SrtpSpace},
    };
    let shared = bytes("8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d");
    let client = std::array::from_fn(|i| i as u8);
    let server = std::array::from_fn(|i| 0xa0 + i as u8);
    let header = bytes("800f1234decafbadcafebabe");
    let profile = SrtpProfile::Aes128CmHmacSha1_80;
    let extra = [0; 4];
    let plain = [0xab; 16];
    // Independent Python P_SHA256 and OpenSSL AES-CTR with the RFC 5764 cut.
    let known = [
        bytes("a96910a3d7e66d0f6c03290697acbb07443b55797ea969d2e520"),
        bytes("c8c224bfc6a8c71d2fe207d31482cc44eb3093e300355b0f3367"),
    ];
    for (side, local, peer) in [(Side::Client, 0, 1), (Side::Server, 1, 0)] {
        let mut parent = dtls12::Dtls12Keys::new(
            AeadAlg::Aes128Gcm,
            shared.as_slice().try_into().unwrap(),
            client,
            server,
            None,
        )
        .unwrap();
        let [send, receive] = parent.take_srtp(profile, side).unwrap();
        assert!(parent.take_srtp(profile, side).is_none());
        assert!(send.keys(SrtpSpace::Rtp, false).is_none());
        assert!(receive.keys(SrtpSpace::Rtp, true).is_none());
        let key = send.keys(SrtpSpace::Rtp, true).unwrap();
        let nonce = key.cm_nonce(0xcafebabe, 0x1234).unwrap();
        let mut cipher = plain;
        let tag = key
            .expand_cm()
            .unwrap()
            .seal(nonce, &header, &extra, &mut cipher)
            .unwrap();
        assert_eq!([&cipher[..], &tag[..]].concat(), known[local]);
        let key = receive.keys(SrtpSpace::Rtp, false).unwrap();
        let nonce = key.cm_nonce(0xcafebabe, 0x1234).unwrap();
        cipher.copy_from_slice(&known[peer][..16]);
        let tag = known[peer][16..].try_into().unwrap();
        key.expand_cm()
            .unwrap()
            .open(nonce, &header, &extra, &mut cipher, tag)
            .unwrap();
        assert_eq!(cipher, plain);
    }
}
