use super::*;
use hirsel_core_net::{ImportKind, ImportRefusal, ImportUse};

#[test]
fn media_import_marks_follow_secret_aliases() {
    let stored = Secret::new(
        Origin::Sealed(0),
        SecretPolicy::parse("traffic_import(send)").unwrap(),
        Some(Arc::new(SecretBytes::new(Zeroizing::new(vec![7; 32])))),
        None,
    );
    let opaque = Secret::new(
        Origin::Granted(1),
        SecretPolicy::parse("srtp_import(receive)").unwrap(),
        None,
        None,
    );
    for (secret, kind, want) in [
        (stored, ImportKind::Traffic, ImportUse::TrafficSend),
        (opaque, ImportKind::Srtp, ImportUse::SrtpReceive),
    ] {
        let mut slots = Secrets::default();
        let first = slots.register(secret);
        let narrowed = slots.entry(first).secret.clone();
        let second = slots.push(narrowed, None);
        let issued = slots
            .entry(second)
            .secret
            .begin_import(kind, || Ok::<_, ()>(()))
            .map(|(authority, (), _)| authority);
        assert_eq!(issued, Ok(want));
        assert!(matches!(
            slots
                .entry(first)
                .secret
                .begin_import::<(), ()>(kind, || panic!("spent before prepare")),
            Err(ImportRefusal::Spent)
        ));
    }
}
