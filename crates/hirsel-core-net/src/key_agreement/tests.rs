use super::*;

fn hex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn share(group: Group, scalar: &[u8]) -> (KeyShare, Vec<u8>) {
    let mut public = vec![0; group.public_len()];
    let mut draws = 0;
    let key = KeyShare::from_draws(
        group,
        |block| {
            assert_eq!(draws, 0);
            draws += 1;
            block.copy_from_slice(scalar);
            Ok::<_, ()>(())
        },
        &mut public,
    )
    .unwrap();
    assert_eq!(draws, 1);
    (key, public)
}

#[test]
fn agreement_matches_rfc7748_and_rfc5903() {
    // RFC 7748 §6.1 and RFC 5903 §8.1. P-256 points include the SEC 1 prefix.
    let vectors = [
        (
            Group::X25519,
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a",
            "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb",
            "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f",
            "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742",
        ),
        (
            Group::P256,
            "c88f01f510d9ac3f70a292daa2316de544e9aab8afe84049c62a9c57862d1433",
            concat!(
                "04",
                "dad0b65394221cf9b051e1feca5787d098dfe637fc90b9ef945d0c3772581180",
                "5271a0461cdb8252d61f1c456fa3e59ab1f45b33accf5f58389e0577b8990bb3"
            ),
            "c6ef9c5d78ae012a011164acb397ce2088685d8f06bf9be0b283ab46476bee53",
            concat!(
                "04",
                "d12dfb5289c8d4f81208b70270398c342296970a0bccb74c736fc7554494bf63",
                "56fbf3ca366cc23e8157854c13c58d6aac23f046ada30f8353e74f33039872ab"
            ),
            "d6840f6b42f6edafd13116e0e12565202fef8e9ece7dce03812464d04b9442de",
        ),
    ];
    for (group, a, published_a, b, published_b, shared) in vectors {
        let (a_key, a_public) = share(group, &hex(a));
        let (b_key, b_public) = share(group, &hex(b));
        assert_eq!(a_public, hex(published_a));
        assert_eq!(b_public, hex(published_b));
        assert_eq!(&*a_key.agree(&b_public).unwrap(), hex(shared).as_slice());
        assert_eq!(&*b_key.agree(&a_public).unwrap(), hex(shared).as_slice());

        let (virtual_key, virtual_public) = share(group, &hex(a));
        let mut host_public = vec![0; group.public_len()];
        let host_key = KeyShare::host(group, &mut host_public).unwrap();
        let agreed = virtual_key.agree(&host_public).unwrap();
        assert_eq!(*host_key.agree(&virtual_public).unwrap(), *agreed);
        assert!(share(group, &hex(a)).0.agree(&[]).is_none());
        assert!(
            share(group, &hex(a))
                .0
                .agree(&vec![0; group.public_len()])
                .is_none()
        );
    }

    let (_, peer) = share(Group::P256, &hex(vectors[1].3));
    let mut compressed = peer[..33].to_vec();
    compressed[0] = 2 + (peer[64] & 1);
    assert!(
        share(Group::P256, &hex(vectors[1].1))
            .0
            .agree(&compressed)
            .is_none()
    );

    let mut draws = 0;
    let mut public = [0; 65];
    KeyShare::from_draws(
        Group::P256,
        |block| {
            match draws {
                0 => block.fill(0xff),
                1 => block.copy_from_slice(&hex(vectors[1].1)),
                _ => panic!("an extra scalar draw"),
            }
            draws += 1;
            Ok::<_, ()>(())
        },
        &mut public,
    )
    .unwrap();
    assert_eq!(draws, 2);
    assert_eq!(&public[..], hex(vectors[1].2));
}
