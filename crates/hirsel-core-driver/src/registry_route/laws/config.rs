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
    assert!(Routes::parse(&route("*", "PKG.HIRSEL.AI:443", true, 3)).is_err());

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
