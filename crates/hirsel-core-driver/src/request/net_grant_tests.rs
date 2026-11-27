#[test]
fn bare_net_grant_is_refused_before_discovery_with_the_rule_syntax() {
    let result = super::dispatch::run(
        ["run", "unused", "--grant", "Net"]
            .map(str::to_owned)
            .to_vec(),
    );
    let error = result
        .err()
        .expect("a bare Net grant cannot confer authority");
    assert!(error.to_string().contains(hirsel_core_net::RULE_SYNTAX));
}

/// L38 (names half): a `--exact` selector, an `--entry` and a `Foreign` grant on a scoped
/// package read back as the package, module and item they name, split at the first `@`,
/// and two scopes with one `NAME` stay two packages.
#[test]
fn scoped_selectors_and_foreign_grants_split_at_the_first_at() {
    use crate::cli::CompilerOption as Opt;
    let flags = super::flags::CompilerFlags::read(
        vec![
            Opt::TestExact("local/app@1/main/uses_json".into()),
            Opt::TestExact("b.test/json@1/json/version".into()),
            Opt::Entry("a.test/json@1/json/version".into()),
            Opt::Grant("Foreign=fixture.test/demo@1/api/A".into()),
        ],
        false,
        std::path::PathBuf::new(),
    )
    .expect("scoped names are valid definition paths");
    let parts = |p: &hirsel_core_ids::DefPath| {
        (
            p.0.package.name.clone(),
            p.0.package.version.clone(),
            p.0.modules.join("/"),
            p.0.name.clone(),
        )
    };
    let part =
        |n: &str, m: &str, i: &str| (n.to_owned(), "1".to_owned(), m.to_owned(), i.to_owned());
    assert_eq!(
        flags.test_exact.iter().map(parts).collect::<Vec<_>>(),
        [
            part("local/app", "main", "uses_json"),
            part("b.test/json", "json", "version")
        ]
    );
    assert_eq!(
        parts(flags.entry.as_ref().unwrap()),
        part("a.test/json", "json", "version")
    );
    assert_ne!(flags.entry.as_ref(), flags.test_exact.get(1));
    assert!(flags.grants.foreign.contains("fixture.test/demo@1/api/A"));
}
