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
