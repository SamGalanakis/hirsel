#[test]
fn l38_l39_update_and_audit() {
    l38_aliases_share_nominal_identity();
    l39_selection_rechecks_the_reader();
    update_refuses_widening_and_audit_reports_effects();
}

fn l38_aliases_share_nominal_identity() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.library(
        "other",
        "other.test/json",
        "1.0.0",
        "pub fn other() -> u64 { 2u64 }\n",
        "",
    );
    law.library(
        "point",
        "fixture.test/pt",
        "1.0.0",
        r#"
pub type T { v: u64 }
pub fn make() -> T { T(v: 3u64) }
pub fn get(t: T) -> u64 { t.v }
"#,
        "",
    );
    law.app("app", "");
    copy(&law, "vendor/jx.hpkg", "app/json.hpkg");
    law.fixture("other", "app/other.hpkg");
    law.fixture("point", "app/point.hpkg");
    PackageLaw::pass(invoke(&law, "app", &["json.hpkg"]));
    let snapshot = || {
        [
            fs::read(law.path("app/hirsel.hcfg")).unwrap(),
            fs::read(law.path("app/hirsel.lock")).unwrap(),
        ]
    };
    let before = snapshot();
    let (status, _, stderr) = invoke(&law, "app", &["other.hpkg"]);
    assert_eq!(status, Some(2), "{stderr}");
    assert!(
        stderr.contains("dependencies.json already binds fixture.test/json@1"),
        "{stderr}"
    );
    let (status, _, stderr) = invoke(&law, "app", &["other.hpkg", "--update"]);
    assert_eq!(status, Some(2), "{stderr}");
    assert!(stderr.contains("unknown option `--update`"), "{stderr}");
    assert_eq!(snapshot(), before);
    PackageLaw::pass(invoke(&law, "app", &["other.hpkg", "--as", "bjson"]));
    PackageLaw::pass(invoke(&law, "app", &["point.hpkg", "--as", "p1"]));
    PackageLaw::pass(invoke(&law, "app", &["point.hpkg", "--as", "p2"]));
    law.write(
        "app/main.hirsel",
        format!(
            "{}
pub fn value() -> u64 {{ json.version() * 100u64 + bjson.other() * 10u64 + p2.get(p1.make()) }}
",
            super::PACKAGE_MAIN
        ),
    );
    PackageLaw::pass(law.run(&["check", "app", "--frozen"]));
    let [manifest, lock] = snapshot();
    let config = <hirsel_config_schema::pkg_v1::Package as hirsel_config_schema::Config>::read(
        &manifest, hirsel_config_schema::Limits::default(),
    ).unwrap();
    assert!(config.dependencies.iter().all(|(_, edge)| matches!(edge,
        hirsel_config_schema::pkg_v1::Dependency::Published { authority, .. } if authority.is_empty()
    )));
    assert_eq!(
        String::from_utf8_lossy(&lock)
            .matches("id = \"fixture.test/pt@1\"")
            .count(),
        1
    );
}

fn update_refuses_widening_and_audit_reports_effects() {
    let law = PackageLaw::new();
    law.app("app", "");
    let v1 = floor_fixture(
        "effects",
        [1, 0, 0],
        "",
        r#"
pub fn ping(n: Net, addr: Addr) -> Result[Socket, NetError] waits { host.connect(n, addr, Never) }
"#,
        None,
    );
    let v2 = floor_fixture(
        "effects",
        [1, 1, 0],
        "",
        r#"
pub fn ping(n: Net, e: Env, addr: Addr) -> Result[Socket, NetError] waits {
    let _ = env.len(e)
    host.connect(n, addr, Never)
}
"#,
        None,
    );
    let hof = floor_fixture(
        "hof",
        [1, 0, 0],
        "",
        "pub fn twice(f: fn() -> u64) -> u64 { f() + f() }\n",
        None,
    );
    let fake = floor_fixture(
        "fake",
        [1, 0, 0],
        "",
        "pub fn clock() -> (Clock, ClockHand) { provide.clock(0, 0) }\n",
        None,
    );
    for (name, content) in [("effects", &v1), ("hof", &hof), ("fake", &fake)] {
        law.write(&format!("app/{name}.hpkg"), package_bundle(&[content]));
    }
    let tail = |root: &str, authority: &str| {
        let effects = exact_row("effects", 1, root).replace("authority: []", authority);
        let hof = exact_row("hof", 1, &fixture_root(&hof));
        let fake = exact_row("fake", 1, &fixture_root(&fake)).replace("authority: []", "authority: [Clock]");
        format!("dependencies: [{effects}, {hof}, {fake}],")
    };
    let root = fixture_root(&v1);
    law.app("app", &tail(&root, "authority: [Net]"));
    PackageLaw::pass(law.run(&["check", "app"]));
    law.write("app/effects.hpkg", package_bundle(&[&v2]));
    let snapshot = || {
        [
            fs::read(law.path("app/hirsel.hcfg")).unwrap(),
            fs::read(law.path("app/hirsel.lock")).unwrap(),
        ]
    };
    let before = snapshot();
    let (status, stdout, stderr) = result(
        invocation_command(&law, "app", "update", &["effects"])
            .output()
            .unwrap(),
    );
    assert_eq!(status, Some(2), "{stdout}\n{stderr}");
    assert!(stdout.contains("effects/ping: + Env"), "{stdout}");
    assert!(
        stderr.contains("reaches Env, which authority = [\"Net\"] does not grant"),
        "{stderr}"
    );
    assert_eq!(snapshot(), before);

    let text = PackageLaw::pass(law.run(&["audit", "app"]));
    for expected in [
        "carried authority",
        "takes [Net], returns [Net]",
        "provides Clock",
        "verdict: conditional",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    let json = PackageLaw::pass(law.run(&["audit", "app", "--json"]));
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    let card = |name: &str| {
        &json["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == format!("fixture.test/{name}@1"))
            .unwrap()["card"]
    };
    assert_eq!(
        card("effects")["carried"]["effects/ping"]["takes"],
        serde_json::json!(["Net"])
    );
    assert_eq!(
        card("effects")["reached"]["effects/ping"]["kinds"],
        serde_json::json!(["Net"])
    );
    assert_eq!(card("effects")["verdict"], serde_json::json!(["Net"]));
    assert_eq!(card("hof")["verdict"], "conditional");
    assert_eq!(
        card("fake")["reached"]["fake/clock"]["provided"],
        serde_json::json!(["Clock"])
    );
    assert_eq!(snapshot(), before);

    law.app("app", &tail(&fixture_root(&v2), "authority: [Net]"));
    let before = snapshot();
    let (status, json, stderr) = law.run(&["audit", "app", "--json"]);
    assert_eq!(status, Some(2), "{json}\n{stderr}");
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(
        json["packages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["card"]["verdict"] == serde_json::json!(["Net", "Env"]))
    );
    assert_eq!(snapshot(), before);
}
