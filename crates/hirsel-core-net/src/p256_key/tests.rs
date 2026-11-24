use super::*;
use ring::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};

fn hex<const N: usize>(s: &str) -> [u8; N] {
    assert_eq!(s.len(), N * 2);
    std::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
}

#[test]
fn p256_key_and_signature_match_rfc6979() {
    // RFC 6979 A.2.5, P-256/SHA-256 and "sample".
    let secret = hex("c9afa9d845ba75166b5c215767b1d6934e50c3db36e89b127b8a622b120f6721");
    let expected = hex(concat!(
        "04",
        "60fed4ba255a9d31c961eb74c6356d68c049b8923b61fa6ce669622e60f29fb6",
        "7903fe1008b8bc99a41ae9e95628bc64f2f1b20c2d7e9f5177a3c294d4462299",
    ));
    let signature = hex(concat!(
        "efd48b2aacb6a8fd1140dd9cd45e81d69d2c877b56aaf991c34d0ea84eaf3716",
        "f7cb1c942d657c41d436c7a1b6e29f65f3e900dbb9aff4064dc4ab2f843acda8",
    ));
    let mut public = [0; PUBLIC_LEN];
    let pkcs8 = generate_virtual(
        |out| {
            *out = secret;
            Ok::<_, ()>(())
        },
        &mut public,
    )
    .unwrap();
    assert_eq!(public, expected);
    assert_eq!(sign_virtual(&pkcs8[..], b"sample"), Ok(signature));
    let verify = UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, public);
    verify.verify(b"sample", &signature).unwrap();
    let host = HostSigner::load(&pkcs8[..]).unwrap();
    verify
        .verify(b"sample", &host.sign(b"sample").unwrap())
        .unwrap();

    let mut invalid = pkcs8.clone();
    invalid[137] ^= 1;
    assert_eq!(
        sign_virtual(&invalid[..], b"sample"),
        Err(KeyFailure::Invalid)
    );
    assert!(matches!(
        HostSigner::load(&invalid[..]),
        Err(KeyFailure::Invalid)
    ));

    let mut public = [0; PUBLIC_LEN];
    let host_key = generate_host(&mut public).unwrap();
    let signature = HostSigner::load(&host_key[..])
        .unwrap()
        .sign(b"sample")
        .unwrap();
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, public)
        .verify(b"sample", &signature)
        .unwrap();
}
