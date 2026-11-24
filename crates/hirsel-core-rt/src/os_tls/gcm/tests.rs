use super::GcmKey;
use ring::aead::{AES_128_GCM, AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};

#[test]
fn drops_owned_vaes_state_in_place() {
    let key = Box::into_raw(GcmKey::new(&[0x16; 16]).unwrap());
    // Keep the allocation after the destructor, and inspect only the
    // fully initialized integer fields it owns. Padding is not read.
    unsafe {
        let round_keys = std::ptr::addr_of!((*key).aes.rd_key).cast::<u32>();
        let rounds = std::ptr::addr_of!((*key).aes.rounds);
        let table = std::ptr::addr_of!((*key).table.0).cast::<u64>();
        assert!((0..60).any(|index| round_keys.add(index).read_volatile() != 0));
        assert!((0..32).any(|index| table.add(index).read_volatile() != 0));
        std::ptr::drop_in_place(key);
        assert_eq!(rounds.read_volatile(), 0);
        assert!((0..60).all(|index| round_keys.add(index).read_volatile() == 0));
        assert!((0..32).all(|index| table.add(index).read_volatile() == 0));
        drop(Box::from_raw(key.cast::<std::mem::MaybeUninit<GcmKey>>()));
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

/// Opens `sealed` with `key`; a flipped tag bit must fail and leave `out`
/// zeroed.
fn open(key: &GcmKey, nonce: [u8; 12], aad: &[u8], sealed: &[u8]) -> Vec<u8> {
    let mut out = vec![0xa5; sealed.len() - 16];
    key.open_record(nonce, aad, sealed, &mut out).unwrap();
    let mut forged = sealed.to_vec();
    *forged.last_mut().unwrap() ^= 1;
    let mut zeroed = vec![0xa5; out.len()];
    assert!(key.open_record(nonce, aad, &forged, &mut zeroed).is_err());
    assert!(zeroed.iter().all(|&byte| byte == 0));
    out
}

/// McGrew-Viega cases 4 and 16, Wycheproof multi-block AAD, and ring's
/// in-place seal over every partial-block shape, each sealed and opened.
#[test]
fn out_of_place_gcm_matches_ring() {
    if std::env::var("HIRSEL_TLS_GCM_TEST_FALLBACK").as_deref() == Ok("1") {
        assert!(
            !GcmKey::supported(),
            "fallback job requires a CPU without the VAES path"
        );
        super::super::records::check_fallback_seal();
        return;
    }
    assert!(
        GcmKey::supported(),
        "fast GCM conformance requires VAES and VPCLMULQDQ. Set HIRSEL_TLS_GCM_TEST_FALLBACK=1 for the fallback job"
    );
    let plain = hex(concat!(
        "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a72",
        "1c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39"
    ));
    let aad = hex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
    let nonce: [u8; 12] = hex("cafebabefacedbaddecaf888").try_into().unwrap();
    for (key, sealed) in [
        (
            "feffe9928665731c6d6a8f9467308308",
            concat!(
                "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e",
                "21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091",
                "5bc94fbc3221a5db94fae95ae7121a47"
            ),
        ),
        (
            "feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308",
            concat!(
                "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa",
                "8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662",
                "76fc6ece0f4e1768cddf8853bb2d551b"
            ),
        ),
    ] {
        let seal = GcmKey::new(&hex(key)).unwrap();
        let (input, extra) = plain.split_at(plain.len() - 1);
        let mut out = vec![0; plain.len() + 16];
        seal.seal_record(nonce, &aad, input, extra[0], &mut out);
        assert_eq!(out, hex(sealed));
        assert_eq!(open(&seal, nonce, &aad, &out), plain);
    }

    for line in include_str!("../testdata/aes-gcm-multiblock-aad.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
    {
        let [id, key, iv, aad, msg, ct, tag] = line.split('\t').collect::<Vec<_>>()[..] else {
            panic!("malformed Wycheproof vector: {line}");
        };
        let seal = GcmKey::new(&hex(key)).unwrap();
        let msg = hex(msg);
        let (extra, input) = msg.split_last().unwrap();
        let mut out = vec![0; msg.len() + super::BLOCK];
        seal.seal_record(
            hex(iv).try_into().unwrap(),
            &hex(aad),
            input,
            *extra,
            &mut out,
        );
        let (ciphertext, actual_tag) = out.split_at(msg.len());
        assert_eq!(ciphertext, hex(ct), "Wycheproof case {id} ciphertext");
        assert_eq!(actual_tag, hex(tag), "Wycheproof case {id} tag");
        assert_eq!(
            open(&seal, hex(iv).try_into().unwrap(), &hex(aad), &out),
            msg,
            "Wycheproof case {id} open"
        );
    }

    // The record round trips exercise every length of padded AAD.
    let aad: [u8; 33] = std::array::from_fn(|i| (i * 7 + 23) as u8);
    for (algorithm, key) in [(&AES_128_GCM, [0x42; 32]), (&AES_256_GCM, [0x5a; 32])] {
        let key = &key[..algorithm.key_len()];
        let seal = GcmKey::new(key).unwrap();
        let ring = LessSafeKey::new(UnboundKey::new(algorithm, key).unwrap());
        for len in (0..=272).chain(16_368..=16_400) {
            let header = &aad[..len % aad.len()];
            let input: Vec<u8> = (0..len).map(|i| (i * 7 + len) as u8).collect();
            let nonce = [len as u8; 12];
            let mut out = vec![0xa5; len + 17];
            seal.seal_record(nonce, header, &input, 23, &mut out);
            let mut expected = input.clone();
            expected.push(23);
            let tag = ring
                .seal_in_place_separate_tag(
                    Nonce::assume_unique_for_key(nonce),
                    Aad::from(header),
                    &mut expected,
                )
                .unwrap();
            expected.extend_from_slice(tag.as_ref());
            assert_eq!(out, expected, "{} bytes", len);
            let mut plain = input;
            plain.push(23);
            assert_eq!(
                open(&seal, nonce, header, &out),
                plain,
                "{} bytes open",
                len
            );
        }
    }
}
