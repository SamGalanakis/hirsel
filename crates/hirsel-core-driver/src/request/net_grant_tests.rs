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
