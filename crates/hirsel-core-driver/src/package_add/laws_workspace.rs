// S8 laws for `hirsel add --workspace`, included into `package_add_laws` after laws.rs.
fn text(law: &PackageLaw, path: &str) -> String {
    fs::read_to_string(law.path(path)).unwrap()
}

fn invocation(args: &[&str]) -> crate::cli::Request<crate::cli::AddOption> {
    let crate::cli::Action::Invoke(crate::cli::Invocation::Add(invocation)) =
        crate::cli::parse(args.iter().map(|arg| (*arg).to_owned())).unwrap()
    else {
        panic!("expected an add invocation")
    };
    invocation
}

/// A workspace `work` with library members `apps/ui` and `apps/api`, each with
/// an empty `[dependencies]`, and `jx`/`jy` archives under `work/vendor`.
fn two_members(law: &PackageLaw) {
    law.fixture_json();
    law.write(
        "work/hirsel.hcfg",
        "pkg_v1.Workspace(members: [\"apps/ui\", \"apps/api\"])\n",
    );
    for name in ["ui", "api"] {
        law.library(
            &format!("work/apps/{name}"),
            &format!("local/{name}"),
            "1.0.0",
            "pub fn one() -> u64 { 1u64 }\n",
            "dependencies: [],",
        );
    }
    copy(law, "vendor/jx.hpkg", "work/vendor/pkg.hpkg");
    copy(law, "vendor/jy.hpkg", "work/vendor/jy.hpkg");
}

fn declared(root: &str, archive: &str) -> String {
    format!(
        "pkg_v1.Workspace(members: [\"apps/ui\", \"apps/api\"], dependencies: Some([(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{root}\", locator: Archive(\"{archive}\"), authority: []))]))\n"
    )
}

const S8_CRASH_SELECT: &str = "HIRSEL_S8_ADD_CRASH_ARCHIVE";
const S8_CRASH_STORE: &str = "HIRSEL_S8_ADD_CRASH_STORE";

#[test]
fn s8_add_interrupted_child() {
    let Ok(source) = std::env::var(S8_CRASH_SELECT) else {
        return;
    };
    let store = std::env::var(S8_CRASH_STORE).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let args = ["add", &source, "--workspace", "--package-store", &store];
    run_at(invocation(&args), &cwd, |point| {
        if point == Checkpoint::ManifestCommitted {
            std::process::exit(44);
        }
    })
    .unwrap();
    panic!("the test child did not reach the manifest publication point");
}

/// L8.1, L8.2 and the shared-binding rules: two members end with one root
/// declaration, each source spelled relative to the manifest declaring it.
fn inherits(before: &str) -> String {
    before.replace("dependencies: []", "dependencies: [(\"json\", Inherit)]")
}

fn members_share_one_binding() {
    let law = PackageLaw::new();
    two_members(&law);
    let ui = text(&law, "work/apps/ui/hirsel.hcfg");
    let api = text(&law, "work/apps/api/hirsel.hcfg");

    let json = PackageLaw::pass(invoke(
        &law,
        "work/apps/ui",
        &["../../vendor/pkg.hpkg", "--workspace", "--json"],
    ));
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["result"], "added");
    assert_eq!(
        json["committed"],
        serde_json::json!(["hirsel.hcfg", "hirsel.lock", "apps/ui/hirsel.hcfg"])
    );
    assert_eq!(json["pending"], serde_json::json!([]));
    assert_eq!(
        text(&law, "work/hirsel.hcfg"),
        declared(PACKAGE_X, "vendor/pkg.hpkg")
    );
    assert_eq!(text(&law, "work/apps/ui/hirsel.hcfg"), inherits(&ui));

    let json = PackageLaw::pass(invoke(
        &law,
        "work/apps/api",
        &["../../vendor/pkg.hpkg", "--workspace", "--json"],
    ));
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json["result"], "added");
    assert_eq!(
        json["committed"],
        serde_json::json!(["apps/api/hirsel.hcfg", "hirsel.lock"])
    );
    assert_eq!(
        text(&law, "work/hirsel.hcfg"),
        declared(PACKAGE_X, "vendor/pkg.hpkg")
    );
    assert_eq!(text(&law, "work/apps/api/hirsel.hcfg"), inherits(&api));
    PackageLaw::pass(law.run(&["lock", "work/apps/ui", "--locked"]));
    PackageLaw::pass(law.with_store(&["fetch", "work/apps/ui", "--locked"], "cold-store"));
    assert_eq!(
        fs::read(law.path(&package_entry("cold-store", PACKAGE_X))).unwrap(),
        law.encoded(PACKAGE_X)
    );

    // A plain add of the inherited source leaves the inheritance in place; a
    // plain add of other bytes would make the member drift, so it refuses.
    let json = PackageLaw::pass(invoke(
        &law,
        "work/apps/ui",
        &["../../vendor/pkg.hpkg", "--json"],
    ));
    assert!(json.contains("\"result\":\"unchanged\""), "{json}");
    assert_eq!(text(&law, "work/apps/ui/hirsel.hcfg"), inherits(&ui));
    let (status, _, stderr) = invoke(&law, "work/apps/ui", &["../../vendor/jy.hpkg"]);
    assert_eq!(status, Some(2), "{stderr}");
    assert!(
        stderr.contains("dependencies.json inherits the workspace binding"),
        "{stderr}"
    );
    assert_eq!(text(&law, "work/apps/ui/hirsel.hcfg"), inherits(&ui));

    // Refresh an inherited archive in the workspace declaration.
    copy(&law, "vendor/jy.hpkg", "work/vendor/pkg.hpkg");
    PackageLaw::pass(result(
        invocation_command(&law, "work/apps/ui", "update", &["json"])
            .output()
            .unwrap(),
    ));
    assert_eq!(
        text(&law, "work/hirsel.hcfg"),
        declared(PACKAGE_Y, "vendor/pkg.hpkg")
    );
    assert_eq!(text(&law, "work/apps/ui/hirsel.hcfg"), inherits(&ui));
    assert_eq!(text(&law, "work/apps/api/hirsel.hcfg"), inherits(&api));
    PackageLaw::pass(law.run(&["lock", "work/apps/api", "--locked"]));

    // The same local binding can become a shared declaration.
    let law = PackageLaw::new();
    two_members(&law);
    PackageLaw::pass(invoke(&law, "work/apps/ui", &["../../vendor/pkg.hpkg"]));
    PackageLaw::pass(invoke(
        &law,
        "work/apps/ui",
        &["../../vendor/pkg.hpkg", "--workspace"],
    ));
    assert_eq!(text(&law, "work/apps/ui/hirsel.hcfg"), inherits(&ui));
    PackageLaw::pass(law.run(&["lock", "work/apps/ui", "--locked"]));
}

/// L8.3: a virtual root only declares, and the placement refusals.
fn virtual_root_declares_only() {
    let law = PackageLaw::new();
    law.fixture_json();
    law.write("v/hirsel.hcfg", "pkg_v1.Workspace(members: [\"m\"])\n");
    law.library(
        "v/m",
        "local/m",
        "1.0.0",
        "pub fn one() -> u64 { 1u64 }\n",
        "",
    );
    copy(&law, "vendor/jx.hpkg", "v/vendor/pkg.hpkg");
    PackageLaw::pass(law.run(&["lock", "v/m"]));
    let lock = fs::read(law.path("v/hirsel.lock")).unwrap();
    let (status, _, stderr) = invoke(&law, "v", &["vendor/pkg.hpkg"]);
    assert_eq!(status, Some(2), "{stderr}");
    assert!(
        stderr
            .contains("no package manifest here; pass --workspace to declare it for the workspace"),
        "{stderr}"
    );
    PackageLaw::pass(invoke(&law, "v", &["vendor/pkg.hpkg", "--workspace"]));
    assert_eq!(
        fs::read_to_string(law.path("v/hirsel.hcfg")).unwrap(),
        format!(
            "pkg_v1.Workspace(members: [\"m\"], dependencies: Some([(\"json\", Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Archive(\"vendor/pkg.hpkg\"), authority: []))]))\n"
        )
    );
    assert_eq!(fs::read(law.path("v/hirsel.lock")).unwrap(), lock);

    law.app("app", "");
    copy(&law, "vendor/jx.hpkg", "app/jx.hpkg");
    let manifest = fs::read(law.path("app/hirsel.hcfg")).unwrap();
    let (status, _, stderr) = invoke(&law, "app", &["jx.hpkg", "--workspace"]);
    assert_eq!(status, Some(2), "{stderr}");
    assert!(
        stderr.contains("no workspace record; hirsel add does not create a workspace boundary"),
        "{stderr}"
    );
    assert_eq!(fs::read(law.path("app/hirsel.hcfg")).unwrap(), manifest);
}

/// L8.4: partial completion is exit 1 and a rerun finishes it; a crash between
/// the operations leaves a valid workspace; a sibling's local pin of other
/// bytes refuses through the resolver.
fn partial_completion() {
    let law = PackageLaw::new();
    two_members(&law);
    let api = text(&law, "work/apps/api/hirsel.hcfg");
    let edited = format!("{api}// edited while add runs\n");
    let store = law.path("packages");
    let args = [
        "add",
        "../../vendor/pkg.hpkg",
        "--workspace",
        "--package-store",
        store.to_str().unwrap(),
    ];
    let mut fired = false;
    let status = run_at(invocation(&args), &law.path("work/apps/api"), |point| {
        if point == Checkpoint::ManifestCommitted && !fired {
            fired = true;
            fs::write(law.path("work/apps/api/hirsel.hcfg"), &edited).unwrap();
        }
    })
    .unwrap();
    assert_eq!(status, 1);
    assert_eq!(
        text(&law, "work/hirsel.hcfg"),
        declared(PACKAGE_X, "vendor/pkg.hpkg")
    );
    assert_eq!(text(&law, "work/apps/api/hirsel.hcfg"), edited);
    PackageLaw::pass(law.run(&["lock", "work/apps/api", "--locked"]));
    let json = PackageLaw::pass(invoke(
        &law,
        "work/apps/api",
        &["../../vendor/pkg.hpkg", "--workspace", "--json"],
    ));
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        json["committed"],
        serde_json::json!(["apps/api/hirsel.hcfg", "hirsel.lock"])
    );
    assert_eq!(
        text(&law, "work/apps/api/hirsel.hcfg"),
        inherits(&edited)
    );

    let law = PackageLaw::new();
    two_members(&law);
    PackageLaw::pass(law.run(&["lock", "work/apps/ui"]));
    let ui = text(&law, "work/apps/ui/hirsel.hcfg");
    let crash = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "native::tests::package_add_laws::s8_add_interrupted_child",
        ])
        .env(S8_CRASH_SELECT, "../../vendor/pkg.hpkg")
        .env(S8_CRASH_STORE, law.path("packages"))
        .current_dir(law.path("work/apps/ui"))
        .output()
        .unwrap();
    assert_eq!(crash.status.code(), Some(44), "{crash:?}");
    assert_eq!(
        text(&law, "work/hirsel.hcfg"),
        declared(PACKAGE_X, "vendor/pkg.hpkg")
    );
    assert_eq!(text(&law, "work/apps/ui/hirsel.hcfg"), ui);
    PackageLaw::pass(law.run(&["lock", "work/apps/ui"]));
    PackageLaw::pass(law.run(&["lock", "work/apps/ui", "--locked"]));
    let json = PackageLaw::pass(invoke(
        &law,
        "work/apps/ui",
        &["../../vendor/pkg.hpkg", "--workspace", "--json"],
    ));
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        json["committed"],
        serde_json::json!(["apps/ui/hirsel.hcfg", "hirsel.lock"])
    );
    assert_eq!(
        text(&law, "work/apps/ui/hirsel.hcfg"),
        inherits(&ui)
    );
    PackageLaw::pass(law.run(&["lock", "work/apps/ui", "--locked"]));

    let law = PackageLaw::new();
    two_members(&law);
    PackageLaw::pass(invoke(&law, "work/apps/api", &["../../vendor/pkg.hpkg"]));
    PackageLaw::pass(law.run(&["lock", "work/apps/api", "--locked"]));
    let files = [
        "work/hirsel.hcfg",
        "work/apps/ui/hirsel.hcfg",
        "work/apps/api/hirsel.hcfg",
        "work/hirsel.lock",
    ]
    .map(|path| (path, text(&law, path)));
    let (status, _, stderr) = invoke(
        &law,
        "work/apps/ui",
        &["../../vendor/jy.hpkg", "--workspace"],
    );
    assert_eq!(status, Some(2), "{stderr}");
    assert!(
        stderr.contains("fixture.test/json@1 is bound to more than one root"),
        "{stderr}"
    );
    for (path, bytes) in files {
        assert_eq!(text(&law, path), bytes, "{path}");
    }
}

#[test]
fn s8_add_workspace() {
    members_share_one_binding();
    virtual_root_declares_only();
    partial_completion();
}
