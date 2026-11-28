use super::*;

pub(crate) fn route(prefix: &str, authority: &str, preview: bool, legal: u8) -> String {
    routes(&route_row(prefix, authority, preview, legal))
}

pub(super) fn routes(rows: &str) -> String {
    format!("user_v1.Config(contexts: [], routes: [{rows}])")
}

pub(super) fn route_row(prefix: &str, authority: &str, preview: bool, legal: u8) -> String {
    let prefix = match prefix.split('/').collect::<Vec<_>>().as_slice() {
        ["*"] => "Global".to_string(),
        [host, "*"] => format!("HostPrefix({host:?})"),
        [host, tenant, "*"] => format!("Tenant(host: {host:?}, tenant: {tenant:?})"),
        _ => panic!("bad fixture scope"),
    };
    let quorum = if preview {
        "Preview".into()
    } else {
        let [first, second] = witnesses().map(|key| c2sp_key(key).0);
        format!("Witnessed(first: {first:?}, second: {second:?})")
    };
    format!(
        "Route(prefix: {prefix}, url: \"https://{authority}\", release_key: {:?}, status_key: {:?}, legal_key: {:?}, quorum: {quorum}),",
        c2sp_key((&format!("{authority}/log"), 1, 1)).0,
        c2sp_key((&format!("{authority}/status"), 2, 1)).0,
        b64(signer(legal).public_key().as_ref())
    )
}

pub(super) fn route_checks() {
    let missing = Routes::default().for_name(TARGET).err().unwrap();
    assert!(missing.contains("https://pkg.hirsel.ai") && missing.contains("routes"));

    let rows = route_row("*", "default.test", false, 3)
        + &route_row("fixture.test/*", BETA, false, 3)
        + &route_row("fixture.test/tenant/*", ALPHA, true, 3);
    let text = routes(&rows);
    let routes = Routes::parse(&text).unwrap();
    for (name, origin) in [
        (TARGET, ALPHA),
        ("fixture.test/tenant", BETA),
        ("fixture.test/tenant-team/wrap", BETA),
        ("another.test/wrap", "default.test"),
    ] {
        assert_eq!(
            routes.for_name(name).unwrap().origin().to_string(),
            format!("https://{origin}")
        );
    }
    assert!(routes.for_name("fixture.test/tenant/wrap/extra").is_err());
    assert!(
        routes
            .for_pin(TARGET, &Origin::parse("https://beta.test").unwrap())
            .is_err()
    );
    assert!(
        Routes::parse(&self::routes(
            &(rows.clone() + &route_row("*", ALPHA, true, 3))
        ))
        .is_err()
    );
    assert!(
        Routes::parse(&self::routes(
            &(rows.clone() + &route_row("another.test/*", ALPHA, false, 3))
        ))
        .is_err()
    );
    assert!(Routes::parse(&text.replace("Witnessed(first:", "Preview(first:")).is_err());
    assert!(Routes::parse(&text.replace("quorum: Preview", "quorum: 1")).is_err());
    assert!(Routes::parse(&text.replace("https://alpha.test", "https://alpha.test/path")).is_err());
    let witness = c2sp_key(("another.test/witness", 7, 4)).0;
    assert!(Routes::parse(&text.replace(&c2sp_key(witnesses()[1]).0, &witness)).is_err());
}

#[test]
fn public_preview_requires_the_operator_signature_and_release_proof() {
    use crate::registry_verify::{Error, ReleaseClaim};

    let authority = "pkg.hirsel.ai";
    let configured =
        route("*", authority, true, 3).replace(PUBLIC_ORIGIN, "https://PKG.HIRSEL.AI:443");
    let preview = Routes::parse(&configured).unwrap();
    let preview = preview.for_name(TARGET).unwrap();
    assert_eq!(preview.origin().to_string(), PUBLIC_ORIGIN);
    assert!(preview.preview());

    let witnessed = Routes::parse(&route("*", authority, false, 3)).unwrap();
    let witnessed = witnessed.for_name(TARGET).unwrap();
    assert_ne!(preview.trust_digest(), witnessed.trust_digest());

    let root = Root([3; 32]);
    let rows = [release_row(root)];
    let digest = merkle(&rows);
    let log = Log::Release;
    let identity = format!("{TARGET}@1");
    let claim = ReleaseClaim {
        identity: &identity,
        version: "1.2.3",
        root,
        facts: Some([0xbb; 32]),
    };
    let signed = note(authority, log, 1, digest, None);
    let trust = preview.trust(log);
    let head = trust.authenticate(&signed).unwrap();
    assert!(head.is_preview());
    assert_eq!(
        head.release(0, claim, |_| Ok(bundle(&rows)))
            .unwrap()
            .root(),
        root
    );

    let unsigned = format!("{authority}/log\n1\n{}\n\n", b64(&digest));
    assert!(trust.authenticate(unsigned.as_bytes()).is_err());
    let signed_text = std::str::from_utf8(&signed).unwrap();
    let (body, signature) = signed_text.split_once("\n\n").unwrap();
    let (_, data) = signature.trim_end().rsplit_once(' ').unwrap();
    let mut bytes = decode_base64(data).unwrap();
    bytes[4] ^= 1;
    let bad_signature = format!("{body}\n\n— {authority}/log {}\n", b64(&bytes));
    assert_eq!(
        trust.authenticate(bad_signature.as_bytes()).unwrap_err(),
        Error::Signature
    );

    let wrong_digest = note(authority, log, 1, [0; 32], None);
    let wrong_head = trust.authenticate(&wrong_digest).unwrap();
    assert_eq!(
        wrong_head
            .release(0, claim, |_| Ok(bundle(&rows)))
            .unwrap_err(),
        Error::MerkleRoot
    );

    let witnessed = witnessed.trust(log);
    let both = note(authority, log, 1, digest, Some(NOW));
    let one = std::str::from_utf8(&both)
        .unwrap()
        .trim_end()
        .rsplit_once('\n')
        .unwrap()
        .0;
    let one = format!("{one}\n");
    assert_eq!(
        witnessed.authenticate(one.as_bytes()).unwrap_err(),
        Error::WitnessQuorum
    );
    assert!(!witnessed.authenticate(&both).unwrap().is_preview());
}
