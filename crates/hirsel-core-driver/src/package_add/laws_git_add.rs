// Git source laws of `hirsel add`: L23, L24 and L35 (packages.md section 12) over the fixture
// of laws_git.rs, included after it.

/// The L23 binding, copied from the spec's text block: W (section 6) and C1 (Git 2.43.0).
const L23_BINDING: &str = "(\"wrap\", Published(package: \"fixture.test/wrap\", major: 1, root: \"sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5\", locator: Git(url: \"https://github.com/o/r\", rev: \"daf4441842dbd47464a183be86f1128a41b2bf26\", reference: Some(\"refs/tags/v1\"), dir: Some(\"wrap\")), authority: []))";

/// Moves `name` of the repository at `key` to `commit`.
fn point(server: &GitServer, key: &str, name: &str, commit: &str) {
    server.with(|served| {
        served
            .repos
            .get_mut(key)
            .unwrap()
            .refs
            .insert(name.into(), commit.into())
    });
}

/// L23. bug: a `github:` shorthand that still names a release asset, a `/DIR` read as a package
/// name, a reference matched by prefix (the decoy `v10` would give C2 and W2), and a written
/// `ref` that is not the full name.
/// oracle: the binding line written by hand from W (section 6) and C1 (Git 2.43.0); the
/// request log from GIT-7 and GIT-10.
#[test]
fn l23_github_reference_is_a_commit() {
    let law = PackageLaw::new();
    let server = GitServer::start(&law);
    let mut repo = Repo::r1();
    repo.refs.insert("refs/tags/v10".into(), C2.into());
    server.serve("github.com/o/r", repo);

    app_at(&law, "app");
    PackageLaw::pass(server.add(&law, "app", &["github:o/r/wrap@v1"]));
    assert!(manifest_text(&law, "app").contains(L23_BINDING), "{}", manifest_text(&law, "app"));
    assert_eq!(
        server.log(),
        [
            info("github.com", "o/r"),
            listing("github.com", "o/r", &["refs/tags/v1", "refs/heads/v1"]),
            fetching("github.com", "o/r", C1),
        ]
    );

    app_at(&law, "head");
    let stdout = PackageLaw::pass(server.add(&law, "head", &["github:o/r/wrap"]));
    let head = L23_BINDING.replace("refs/tags/v1", "refs/heads/main");
    assert!(manifest_text(&law, "head").contains(&head), "{}", manifest_text(&law, "head"));
    assert!(stdout.contains(&format!("HEAD of https://github.com/o/r is refs/heads/main at {C1}")), "{stdout}");
    assert_eq!(server.log()[1], listing("github.com", "o/r", &["HEAD"]));

    app_at(&law, "pinned");
    PackageLaw::pass(server.add(&law, "pinned", &[&format!("github:o/r/wrap@{C1}")]));
    let pinned = L23_BINDING.replace("Some(\"refs/tags/v1\")", "None");
    assert!(manifest_text(&law, "pinned").contains(&pinned), "{}", manifest_text(&law, "pinned"));
    assert_eq!(server.log(), [info("github.com", "o/r"), fetching("github.com", "o/r", C1)]);
}

/// L24. bug: a source that is sniffed, and a short ref that picks one of two meanings.
/// oracle: section 9's `add, bad git source` and `add, ambiguous ref` rows and the grammar of
/// section 11, written by hand.
#[test]
fn l24_each_source_text_has_one_meaning() {
    let law = PackageLaw::new();
    let server = GitServer::start(&law);
    let mut repo = Repo::r1();
    repo.refs.insert("refs/heads/v1".into(), C1.into());
    server.serve("alpha.test/r1", repo);
    app_at(&law, "app");
    let before = manifest_text(&law, "app");
    for text in [
        "github:o/r/../x",
        "github:o",
        "git+https://alpha.test/r1?x=1",
        "git+https://user@alpha.test/r1",
        "git+https://alpha.test/r1#",
        "git+http://alpha.test/r1",
        "git+ssh://alpha.test/r1",
        "git+https://Alpha.test/r1",
    ] {
        let (status, _, stderr) = server.add(&law, "app", &[text]);
        assert_eq!(status, Some(2), "{text}: {stderr}");
        let refusal = format!(
            "hirsel: {text} is not git+https://HOST/PATH[?dir=DIR][#REF] or github:OWNER/REPO[/DIR][@REF]: "
        );
        assert!(stderr.starts_with(&refusal) && stderr.lines().count() == 1, "{text}: {stderr}");
        assert_eq!(manifest_text(&law, "app"), before);
        assert_eq!(server.log(), Vec::<String>::new(), "{text}");
    }
    law.empty_store();

    PackageLaw::refuse(
        server.add(&law, "app", &["git+https://alpha.test/r1?dir=wrap#v1"]),
        "https://alpha.test/r1: v1 names both refs/tags/v1 and refs/heads/v1; write the full name",
    );
    assert_eq!(manifest_text(&law, "app"), before);
    law.empty_store();
    server.log();
    PackageLaw::pass(server.add(&law, "app", &["git+https://alpha.test/r1?dir=wrap#refs/tags/v1"]));
    let binding = wrap_binding("https://alpha.test/r1", C1, PACKAGE_W, Some("refs/tags/v1"));
    assert!(manifest_text(&law, "app").contains(&binding), "{}", manifest_text(&law, "app"));
    server.log();

    app_at(&law, "html");
    let (status, _, stderr) = server.add(&law, "html", &["https://alpha.test/r1"]);
    assert_eq!(status, Some(2), "{stderr}");
    assert!(stderr.starts_with("hirsel: url:https://alpha.test/r1 is not a package archive: "), "{stderr}");
    assert_eq!(server.log(), ["alpha.test GET /r1"]);
}

/// L35. bug: an add that follows a moved tag, one that cannot tell a moved tag from a new
/// source, and one that calls a branch's change progress.
/// oracle: the refusal and update lines written by hand from section 9 and GIT-11, with C1
/// and C2 from Git 2.43.0 and W and W2 from section 6.
#[test]
fn l35_moved_tag_is_reported() {
    let law = PackageLaw::new();
    let server = GitServer::start(&law);
    server.serve("alpha.test/r1", Repo::r1());
    let tagged = "git+https://alpha.test/r1?dir=wrap#v1";
    app_at(&law, "app");
    PackageLaw::pass(server.add(&law, "app", &[tagged]));

    point(&server, "alpha.test/r1", "refs/tags/v1", C2);
    let manifest = manifest_text(&law, "app");
    let lock = fs::read(law.path("app/hirsel.lock")).unwrap();
    PackageLaw::refuse(
        server.add(&law, "app", &[tagged]),
        "https://alpha.test/r1 refs/tags/v1 moved:
  bound: daf4441842dbd47464a183be86f1128a41b2bf26 sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5
  fetched: a9bad28634316d4b90052da470a735871d764838 sha256:f9ce1f7be3bb0142c589e371080224a62b625a483bba4347f9e3df47483146c5
to take it, run: hirsel update wrap",
    );
    assert_eq!(manifest_text(&law, "app"), manifest);
    assert_eq!(fs::read(law.path("app/hirsel.lock")).unwrap(), lock);
    assert_eq!(stored(&law, "packages"), roots(&[PACKAGE_X, PACKAGE_W]));

    let updated = format!("hirsel: updated fixture.test/wrap@1 {PACKAGE_W} -> {PACKAGE_W2} in ");
    let mut command = invocation_command(&law, "app", "update", &["wrap"]);
    server.route(&mut command);
    let stdout = PackageLaw::pass(result(command.output().unwrap()));
    assert!(stdout.contains(&format!("{PACKAGE_W} -> ")), "{stdout}");
    assert!(stdout.contains(PACKAGE_W2), "{stdout}");
    let binding = wrap_binding("https://alpha.test/r1", C2, PACKAGE_W2, Some("refs/tags/v1"));
    assert!(manifest_text(&law, "app").contains(&binding), "{}", manifest_text(&law, "app"));
    point(&server, "alpha.test/r1", "refs/tags/v1", C1);

    // A branch that names another commit.
    app_at(&law, "branch");
    let branch = "git+https://alpha.test/r1?dir=wrap#main";
    PackageLaw::pass(server.add(&law, "branch", &[branch]));
    point(&server, "alpha.test/r1", "refs/heads/main", C2);
    PackageLaw::refuse(
        server.add(&law, "branch", &[branch]),
        "https://alpha.test/r1 refs/heads/main now names another commit:
  bound: daf4441842dbd47464a183be86f1128a41b2bf26 sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5
  fetched: a9bad28634316d4b90052da470a735871d764838 sha256:f9ce1f7be3bb0142c589e371080224a62b625a483bba4347f9e3df47483146c5
to take it, run: hirsel update wrap",
    );
    point(&server, "alpha.test/r1", "refs/heads/main", C1);

    // Another tag is a named update.
    app_at(&law, "named");
    PackageLaw::pass(server.add(&law, "named", &[tagged]));
    point(&server, "alpha.test/r1", "refs/tags/v2", C2);
    let stdout = PackageLaw::pass(server.add(&law, "named", &["git+https://alpha.test/r1?dir=wrap#v2"]));
    assert!(stdout.starts_with(&updated), "{stdout}");
    let binding = wrap_binding("https://alpha.test/r1", C2, PACKAGE_W2, Some("refs/tags/v2"));
    assert!(manifest_text(&law, "named").contains(&binding), "{}", manifest_text(&law, "named"));
}
