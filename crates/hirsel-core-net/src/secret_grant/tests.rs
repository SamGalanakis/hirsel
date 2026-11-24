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
        assert!(SecretPolicy::parse("kdf").unwrap().derived(sent).is_none());
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
        let bytes = std::sync::Arc::new(ImportSpend::new(policy.uses).unwrap());
        assert_eq!(
            bytes.issue(kind, || Err::<(), _>(511)),
            Err(ImportRefusal::BeforeIssue(511))
        );
        assert_eq!(
            bytes.issue::<(), ()>(other, || panic!("the wrong family cannot admit memory")),
            Err(ImportRefusal::Denied),
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
                    alias.issue(kind, || {
                        admissions.fetch_add(1, Ordering::Relaxed);
                        Ok::<_, ()>(512)
                    })
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
            bytes.issue::<(), ()>(other, || panic!("wrong family takes precedence over spent")),
            Err(ImportRefusal::Denied),
        );
        assert_eq!(
            bytes.issue::<(), ()>(kind, || panic!(
                "spent takes precedence over other prechecks"
            )),
            Err(ImportRefusal::Spent),
        );
    }
}
