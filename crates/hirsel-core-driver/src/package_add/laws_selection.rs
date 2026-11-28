// The section 13 reference encoding for one file and, optionally, one floor.
fn floor_fixture(
    name: &str,
    version: [u8; 3],
    pre: &str,
    source: &str,
    floor: Option<(&str, [u8; 3], &str)>,
) -> Vec<u8> {
    let mut bytes = vec![3];
    wire_text(&format!("fixture.test/{name}"), &mut bytes);
    bytes.extend(version);
    wire_text(pre, &mut bytes);
    wire_text("charge-v1", &mut bytes);
    bytes.push(u8::from(floor.is_some()));
    if let Some((child, [major, minor, patch], tested)) = floor {
        wire_text(child, &mut bytes);
        wire_text(&format!("fixture.test/{child}"), &mut bytes);
        bytes.extend([major, 2]);
        bytes.extend(hirsel_core_ids::hex::decode(&tested[7..]).unwrap());
        bytes.extend([minor, patch]);
    }
    bytes.push(1);
    wire_text(&format!("{name}.hirsel"), &mut bytes);
    bytes.push(1);
    wire_text(name, &mut bytes);
    wire_text(source, &mut bytes);
    bytes.push(0);
    bytes
}

fn fixture_root(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    for field in [b"hirsel.library-root.v1".as_slice(), bytes] {
        hash.update((field.len() as u64).to_le_bytes());
        hash.update(field);
    }
    format!("sha256:{}", hirsel_core_ids::hex::encode(&hash.finalize()))
}

fn exact_row(name: &str, major: u8, root: &str) -> String {
    format!(
        "(\"{name}\", Published(package: \"fixture.test/{name}\", major: {major}, root: \"{root}\", locator: Archive(\"{name}.hpkg\"), authority: []))"
    )
}

fn select_row(name: &str, major: u8, root: &str) -> String {
    format!(
        "selections: Some([(\"fixture.test/{name}@{major}\", Selection(root: \"{root}\", locator: Archive(\"selected.hpkg\")))]),"
    )
}

#[test]
fn l1b_selected_floor_and_version_collision() {
    let law = PackageLaw::new();
    let json = floor_fixture("json", [1, 0, 0], "", super::PACKAGE_X_SOURCE, None);
    assert_eq!(fixture_root(&json), PACKAGE_X);
    let wrap = floor_fixture(
        "wrap",
        [1, 0, 0],
        "",
        "pub fn one() -> u64 { 1u64 }\n",
        Some(("json", [1, 0, 0], PACKAGE_X)),
    );
    let w = fixture_root(&wrap);
    assert_eq!(
        w,
        "sha256:0a8784b0a67e8789914b019e1d8ae502540cd71fb3bd63d7cea77ff7d6084c7a"
    );
    law.write("app/wrap.hpkg", package_bundle(&[&wrap, &json]));
    let tail = |root| {
        format!(
            "dependencies: [{}, {}],",
            exact_row("wrap", 1, &w),
            exact_row("json", 1, root)
        )
    };
    let y = floor_fixture("json", [1, 0, 1], "", PACKAGE_Y_SOURCE, None);
    assert_eq!(fixture_root(&y), PACKAGE_Y);
    law.write("app/json.hpkg", package_bundle(&[&y]));
    law.app("app", &tail(PACKAGE_Y));
    PackageLaw::refuse(
        law.run(&["check", "app"]),
        &format!(
            "fixture.test/json@1 is bound to more than one root:\n  local/app@1 -> fixture.test/json@1 = {PACKAGE_Y} exact\n  local/app@1 -> fixture.test/wrap@1 -> fixture.test/json@1 = {PACKAGE_X} floor 1.0.0\nto choose one root for every edge, run: hirsel update fixture.test/json@1"
        ),
    );
    assert!(!law.path("app/hirsel.lock").exists());
    let z = floor_fixture("json", [1, 0, 0], "", PACKAGE_Y_SOURCE, None);
    let pin = fixture_root(&z);
    assert_eq!(
        pin,
        "sha256:1ad0b7636d8b0df47998006c9c53324001ceba7c99cbdd1d7658f2adb191107b"
    );
    law.write("app/json.hpkg", package_bundle(&[&z]));
    let reason = format!(
        "fixture.test/json@1 1.0.0 is two roots:\n  local/app@1 -> fixture.test/json@1 = {pin}\n  local/app@1 -> fixture.test/wrap@1 -> fixture.test/json@1 = {PACKAGE_X}"
    );
    law.app("app", &tail(&pin));
    PackageLaw::refuse(law.run(&["check", "app"]), &reason);
    law.write("app/selected.hpkg", package_bundle(&[&z]));
    law.app("app", &(tail(&pin) + &select_row("json", 1, &pin)));
    PackageLaw::refuse(law.run(&["check", "app"]), &reason);
    // An exact edge's archive can be absent: only the selected graph is acquired.
    fs::remove_file(law.path("app/json.hpkg")).unwrap();
    law.app(
        "app",
        &format!(
            "dependencies: [{}], {}",
            exact_row("json", 1, PACKAGE_X),
            select_row("json", 1, PACKAGE_Y)
        ),
    );
    law.write("app/selected.hpkg", package_bundle(&[&y]));
    PackageLaw::refuse(
        law.run(&["lock", "app"]),
        &format!(
            "fixture.test/json@1 is bound to {PACKAGE_Y} (selected in app/hirsel.hcfg), but local/app@1 -> fixture.test/json@1 pins {PACKAGE_X} exactly"
        ),
    );
    // The stale row cannot supply its locator to an unrelated identity at the same root.
    law.app(
        "app",
        &format!(
            "dependencies: [{}], {}",
            exact_row("json", 1, PACKAGE_Y),
            select_row("unused", 1, PACKAGE_Y)
        ),
    );
    fs::remove_file(law.path("app/selected.hpkg")).unwrap();
    law.write("app/json.hpkg", package_bundle(&[&y]));
    let out = law.with_store(&["lock", "app"], "empty");
    assert!(out.2.contains(&format!("hirsel: app/hirsel.hcfg: selections.fixture.test/unused@1 selects {PACKAGE_Y}, which no edge of the closure names; hirsel update removes it")), "{}", out.2);
    PackageLaw::pass(out);
}

fn l39_selection_rechecks_the_reader() {
    let law = PackageLaw::new();
    let num =
        |minor, patch, pre: &str, body| floor_fixture("num", [0, minor, patch], pre, body, None);
    let old = num(3, 1, "", "pub fn n() -> u64 { 31u64 }\n");
    let tested = fixture_root(&old);
    let reader = floor_fixture(
        "reader",
        [1, 0, 0],
        "",
        "pub fn r() -> u64 { num.n() }\n",
        Some(("num", [0, 3, 1], &tested)),
    );
    let reader_row = exact_row("reader", 1, &fixture_root(&reader));
    law.write("app/reader.hpkg", package_bundle(&[&reader, &old]));
    let configure = |bytes: &[u8], exact: bool, selected: bool| {
        let root = fixture_root(bytes);
        let mut tail = format!("dependencies: [{reader_row}");
        if exact {
            law.write("app/num.hpkg", package_bundle(&[bytes]));
            tail.push_str(", ");
            tail += &exact_row("num", 0, &root);
        }
        tail.push_str("],");
        if selected {
            law.write("app/selected.hpkg", package_bundle(&[bytes]));
            tail += &select_row("num", 0, &root);
        }
        law.app("app", &tail);
        root
    };
    let good = num(3, 4, "", "pub fn n() -> u64 { 34u64 }\n");
    let outside = num(4, 0, "", "pub fn n() -> u64 { 40u64 }\n");
    for (candidate, ending) in [
        (
            &good,
            "to choose one root for every edge, run: hirsel update fixture.test/num@0",
        ),
        (
            &outside,
            "no single root satisfies every edge; the exact edges of local/app@1 must name one root",
        ),
    ] {
        let root = configure(candidate, true, false);
        PackageLaw::refuse(
            law.run(&["check", "app"]),
            &format!(
                "fixture.test/num@0 is bound to more than one root:\n  local/app@1 -> fixture.test/num@0 = {root} exact\n  local/app@1 -> fixture.test/reader@1 -> fixture.test/num@0 = {tested} floor 0.3.1\n{ending}"
            ),
        );
        if candidate == &outside {
            PackageLaw::refuse(
                result(
                    invocation_command(&law, "app", "update", &["fixture.test/num@0"])
                        .output()
                        .unwrap(),
                ),
                &format!(
                    "fixture.test/num@0 is bound to more than one root:\n  local/app@1 -> fixture.test/num@0 = {root} exact\n  local/app@1 -> fixture.test/reader@1 -> fixture.test/num@0 = {tested} floor 0.3.1\n{ending}"
                ),
            );
        }
    }
    let root = configure(&good, true, false);
    let update = || {
        result(
            invocation_command(&law, "app", "update", &["fixture.test/num@0"])
                .output()
                .unwrap(),
        )
    };
    let stdout = PackageLaw::pass(update());
    assert!(
        stdout.contains(&format!("selected fixture.test/num@0 {root}")),
        "{stdout}"
    );
    assert_eq!(
        fs::read_to_string(law.path("app/hirsel.hcfg"))
            .unwrap()
            .matches(r#"("fixture.test/num@0", Selection("#)
            .count(),
        1,
    );
    let lock = fs::read_to_string(law.path("app/hirsel.lock")).unwrap();
    assert!(
        lock.contains(&format!("tested = [\"fixture.test/num@0 {tested}\"]")),
        "{lock}"
    );
    let store = crate::package_store::PackageStore::new(Some(law.path("selected-only")));
    for content in [&reader, &good] {
        let root = crate::package_content::Root::parse(&fixture_root(content)).unwrap();
        store.install_entry(root, content).unwrap();
    }
    PackageLaw::pass(law.with_store(&["check", "app", "--frozen"], "selected-only"));
    assert_eq!(
        fs::read_to_string(law.path("app/hirsel.lock")).unwrap(),
        lock
    );

    for (version, candidate) in [
        ("0.3.0", num(3, 0, "", "pub fn n() -> u64 { 30u64 }\n")),
        (
            "0.3.9-beta",
            num(3, 9, "beta", "pub fn n() -> u64 { 39u64 }\n"),
        ),
    ] {
        let root = configure(&candidate, false, true);
        PackageLaw::refuse(
            law.run(&["check", "app"]),
            &format!(
                "fixture.test/num@0 is bound to {version} {root} (selected in app/hirsel.hcfg), but local/app@1 -> fixture.test/reader@1 -> fixture.test/num@0 requires 0.3.1 or a later 0.3.x"
            ),
        );
        assert_eq!(
            fs::read_to_string(law.path("app/hirsel.lock")).unwrap(),
            lock
        );
    }
    let broken = num(3, 9, "", "pub fn missing() -> u64 { 39u64 }\n");
    configure(&broken, true, false);
    let lock = fs::read(law.path("app/hirsel.lock")).unwrap();
    let manifest = fs::read(law.path("app/hirsel.hcfg")).unwrap();
    let (status, stdout, stderr) = update();
    assert_eq!(status, Some(2), "{stderr}");
    assert!(
        format!("{stdout}{stderr}").contains("num.n"),
        "{stdout}{stderr}"
    );
    assert_eq!(fs::read(law.path("app/hirsel.lock")).unwrap(), lock);
    assert_eq!(fs::read(law.path("app/hirsel.hcfg")).unwrap(), manifest);
}

#[test]
fn l18_changed_archive_names_the_bound_alias() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.app("app", "");
    copy(&law, "vendor/jx.hpkg", "app/dep.hpkg");
    PackageLaw::pass(invoke(&law, "app", &["dep.hpkg"]));
    let manifest = fs::read(law.path("app/hirsel.hcfg")).unwrap();
    let lock = fs::read(law.path("app/hirsel.lock")).unwrap();
    copy(&law, "vendor/jy.hpkg", "app/dep.hpkg");
    PackageLaw::refuse(
        invoke(&law, "app", &["dep.hpkg"]),
        &format!(
            "dep.hpkg is already bound to another root\n  bound: {PACKAGE_X}\n  fetched: {PACKAGE_Y}\nto take the fetched root, run: hirsel update json"
        ),
    );
    assert_eq!(fs::read(law.path("app/hirsel.hcfg")).unwrap(), manifest);
    assert_eq!(fs::read(law.path("app/hirsel.lock")).unwrap(), lock);
}
