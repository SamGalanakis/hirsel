use super::*;

#[test]
fn import_policy_is_exclusive_and_directional() {
    for (sent, received) in [
        ("srtp_import(send)", "srtp_import(receive)"),
        ("traffic_import(send)", "traffic_import(receive)"),
    ] {
        let send = SecretPolicy::parse(sent).unwrap();
        let receive = SecretPolicy::parse(received).unwrap();
        assert_eq!(send.text(), sent);
        assert_eq!(receive.text(), received);
        assert!(receive.intersect(&send).is_none());
        assert!(send.intersect(&receive).is_none());
        for use_word in ["mac", "wrap", "tls_send(a.example)", "mtls_client_id"] {
            for text in [
                format!("{sent} {use_word}"),
                format!("{use_word} {received}"),
            ] {
                assert!(SecretPolicy::parse(&text).is_none(), "{text}");
                assert!(SecretGrant::new("key", "env:IMPORT_KEY", &text).is_none());
            }
        }
        assert!(SecretPolicy::parse(&format!("{sent} {received}")).is_none());
        assert!(
            SecretPolicy::parse("kdf")
                .unwrap()
                .derived(SecretUse::KDF, sent)
                .is_none()
        );
        assert!(SecretGrant::new("key", "env:IMPORT_KEY", sent).is_some());
        assert!(SecretGrant::new("key", "env:IMPORT_KEY", received).is_some());
    }
    assert!(SecretPolicy::parse("srtp_import(send) traffic_import(send)").is_none());
}

#[test]
fn import_aliases_share_one_spent_mark() {
    use super::import_spend::{ImportKind, ImportRefusal, ImportSpend};
    use std::sync::{
        Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    for (policy, kind, other, authority) in [
        (
            "srtp_import(receive)",
            ImportKind::Srtp,
            ImportKind::Traffic,
            ImportUse::SrtpReceive,
        ),
        (
            "traffic_import(send)",
            ImportKind::Traffic,
            ImportKind::Srtp,
            ImportUse::TrafficSend,
        ),
    ] {
        let policy = SecretPolicy::parse(policy).unwrap();
        let bytes = std::sync::Arc::new(ImportSpend::new(&policy, None).unwrap());
        assert_eq!(
            bytes.issue(kind, || Err::<(), _>(511)).err(),
            Some(ImportRefusal::BeforeIssue(511))
        );
        assert_eq!(
            bytes
                .issue::<(), ()>(other, || panic!("the wrong family cannot admit memory"))
                .err(),
            Some(ImportRefusal::Denied),
        );
        let barrier = Barrier::new(2);
        let admissions = AtomicUsize::new(0);
        let replies = std::thread::scope(|tasks| {
            let issue = || {
                let alias = bytes.clone();
                let barrier = &barrier;
                let admissions = &admissions;
                tasks.spawn(move || {
                    barrier.wait();
                    alias
                        .issue(kind, || {
                            admissions.fetch_add(1, Ordering::Relaxed);
                            Ok::<_, ()>(512)
                        })
                        .map(|(authority, admitted, _)| (authority, admitted))
                })
            };
            let first = issue();
            let second = issue();
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(admissions.load(Ordering::Relaxed), 1);
        assert_eq!(
            replies
                .iter()
                .filter(|r| **r == Ok((authority, 512)))
                .count(),
            1
        );
        assert_eq!(
            replies
                .iter()
                .filter(|r| **r == Err(ImportRefusal::Spent))
                .count(),
            1
        );
        assert_eq!(
            bytes
                .issue::<(), ()>(other, || panic!("wrong family takes precedence over spent"))
                .err(),
            Some(ImportRefusal::Denied),
        );
        assert_eq!(
            bytes
                .issue::<(), ()>(kind, || panic!(
                    "spent takes precedence over other prechecks"
                ))
                .err(),
            Some(ImportRefusal::Spent),
        );
    }
}

/// TLS-8 PC13's grammar cases: the KID block's edges, minimal decimal, one
/// `sframe_import` use alone, and narrowing only to itself. Expected values are
/// read from the grammar, not from a run.
#[test]
fn sframe_import_policy_grammar() {
    let send = |kid, bits| SframeDirection::Send { kid, bits };
    for (text, want) in [
        ("sframe_import(receive)", Some(SframeDirection::Receive)),
        ("sframe_import(send,4,2)", Some(send(4, 2))),
        ("sframe_import(send,0,0)", Some(send(0, 0))),
        ("sframe_import(send,0,16)", Some(send(0, 16))),
        (
            "sframe_import(send,18446744073709486080,16)",
            Some(send(u64::MAX - 0xffff, 16)),
        ),
        (
            "sframe_import(send,18446744073709551615,0)",
            Some(send(u64::MAX, 0)),
        ),
        ("sframe_import(send,18446744073709551616,0)", None),
        ("sframe_import(send,5,2)", None),
        ("sframe_import(send,0,17)", None),
        ("sframe_import(send,04,2)", None),
        ("sframe_import(send,4,02)", None),
        ("sframe_import(send,+4,2)", None),
        ("sframe_import(send,4)", None),
        ("sframe_import(send,4,2,1)", None),
        ("sframe_import(send, 4,2)", None),
        ("sframe_import(send)", None),
        ("sframe_import(receive) sframe_import(send,4,2)", None),
        ("sframe_import(send,0,16) sframe_import(send,0,16)", None),
        ("sframe_import(send,4,2) aead(aes128-gcm)", None),
        ("mac sframe_import(receive)", None),
        ("sframe_import(receive) srtp_import(receive)", None),
        ("sframe_import", None),
    ] {
        let parsed = SecretPolicy::parse(text);
        assert_eq!(
            parsed.as_ref().and_then(SecretPolicy::import),
            want.map(ImportUse::Sframe),
            "{text}"
        );
        assert_eq!(parsed.is_some(), want.is_some(), "{text}");
        if let Some(policy) = parsed {
            assert_eq!(policy.text(), text);
            assert_eq!(policy.intersect(&policy).as_ref(), Some(&policy));
            assert!(policy.within(&policy));
            assert!(SecretGrant::new("key", "env:K", text).is_some());
            assert!(
                SecretPolicy::parse("kdf")
                    .unwrap()
                    .derived(SecretUse::KDF, text)
                    .is_none()
            );
        }
    }
    let wide = SecretPolicy::parse("sframe_import(send,4,2)").unwrap();
    for other in ["sframe_import(send,4,1)", "sframe_import(receive)", "mac"] {
        let other = SecretPolicy::parse(other).unwrap();
        assert!(wide.intersect(&other).is_none());
        assert!(other.intersect(&wide).is_none());
        assert!(!other.within(&wide));
    }
    // The block's top edge: KID 2^64 - 1 is in the last block, the KID below it is not.
    let top = send(u64::MAX - 0xffff, 16);
    assert!(top.in_block(u64::MAX));
    assert!(!top.in_block(u64::MAX - 0x10000));
    assert!(send(4, 2).in_block(7) && !send(4, 2).in_block(8) && !send(4, 2).in_block(3));
    assert!(!SframeDirection::Receive.in_block(0));
}

/// M2: an import zeroes the bytes it spends, on every path out, while
/// another designation still holds the mark.
#[test]
fn import_zeroes_the_bytes_it_spends() {
    use super::import_spend::{ImportKind, ImportRefusal, ImportSpend};
    let policy = SecretPolicy::parse("sframe_import(receive)").unwrap();
    let mark = std::sync::Arc::new(
        ImportSpend::new(&policy, Some(zeroize::Zeroizing::new(vec![0xa5; 32]))).unwrap(),
    );
    let alias = mark.clone();
    let (ptr, capacity) = mark.allocation().unwrap();
    let (_, (), spending) = mark
        .issue(ImportKind::Sframe, || Ok::<_, ()>(()))
        .ok()
        .unwrap();
    spending.bytes(|bytes| assert_eq!(bytes, Some(&[0xa5; 32][..])));
    drop(spending);
    drop(mark);
    assert_eq!(alias.allocation(), Some((ptr, capacity)));
    // SAFETY: the allocation is owned by the mark, which `alias` keeps alive.
    let after = unsafe { std::slice::from_raw_parts(ptr, capacity) };
    assert!(after.iter().all(|&b| b == 0));
    assert_eq!(
        alias
            .issue::<(), ()>(ImportKind::Sframe, || panic!("spent"))
            .err(),
        Some(ImportRefusal::Spent)
    );
}
