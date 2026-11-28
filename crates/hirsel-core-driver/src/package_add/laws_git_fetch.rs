// Git source laws L30, L31, L32, L36 and L37 (packages.md section 12) over the fixture of
// laws_git.rs, included after it.

/// L30. bug: a sibling `path` dependency not packed to its own root, a member binding encoded
/// by its path, a `dir` that selects the wrong package, a root over Git object ids instead of
/// over `C`, a resolver that reads members the package never reaches, and a nested source
/// read from the network when the snapshot holds it.
/// oracle: W and X (section 6, reproduced by the spec's reference encoder) and C1 from Git
/// 2.43.0; the lock and request log are written by hand from section 8 and GIT-7/GIT-10.
#[test]
fn l30_git_package_with_sibling_path() {
    let law = PackageLaw::new();
    let server = GitServer::start(&law);
    server.serve("alpha.test/r1", Repo::r1());
    let source = "git+https://alpha.test/r1?dir=wrap#v1";

    app_at(&law, "app");
    PackageLaw::pass(server.add(&law, "app", &[source]));
    let binding = wrap_binding("https://alpha.test/r1", C1, PACKAGE_W, Some("refs/tags/v1"));
    assert!(manifest_text(&law, "app").contains(&binding), "{}", manifest_text(&law, "app"));
    assert_eq!(fs::read_to_string(law.path("app/hirsel.lock")).unwrap(), L30_LOCK);
    assert_eq!(stored(&law, "packages"), roots(&[PACKAGE_X, PACKAGE_W]));
    assert_eq!(
        server.log(),
        [
            info("alpha.test", "r1"),
            listing("alpha.test", "r1", &["refs/tags/v1", "refs/heads/v1"]),
            fetching("alpha.test", "r1", C1),
        ]
    );

    // `hirsel pack` in a checkout of r1 at C1 gives the same roots.
    for (path, bytes) in [
        ("checkout/hirsel.hcfg", "pkg_v1.Workspace(members: [\"json\", \"wrap\"])\n"),
        ("checkout/json/hirsel.hcfg", JSON_MANIFEST),
        ("checkout/json/json.hirsel", PACKAGE_X_SOURCE),
        ("checkout/wrap/hirsel.hcfg", WRAP_MANIFEST),
        ("checkout/wrap/wrap.hirsel", WRAP_V1),
    ] {
        law.write(path, bytes);
    }
    assert_eq!(
        law.fixture("checkout/wrap", "w.hpkg"),
        format!("fixture.test/wrap@1 1.0.0 {PACKAGE_W}\nfixture.test/json@1 1.0.0 {PACKAGE_X}\n")
    );

    let fresh = |name: &str| {
        let _ = fs::remove_dir_all(law.path("packages"));
        fs::create_dir(law.path("packages")).unwrap();
        app_at(&law, name);
        server.log();
    };
    let added = |name: &str, git: &str, rev: &str| {
        PackageLaw::pass(server.add(&law, name, &[&format!("git+{git}?dir=wrap#v1")]));
        let binding = wrap_binding(git, rev, PACKAGE_W, Some("refs/tags/v1"));
        assert!(manifest_text(&law, name).contains(&binding), "{}", manifest_text(&law, name));
    };

    // The same objects as OFS_DELTA and REF_DELTA entries.
    fresh("deltas");
    server.serve("alpha.test/rd", Repo { packing: Packing::Deltas, ..Repo::r1() });
    added("deltas", "https://alpha.test/rd", C1);

    // A third member `broken` that is listed but never reached is never read.
    fresh("broken");
    let mut objects = r1_objects();
    let tree = r1_tree()
        .file("hirsel.hcfg", "pkg_v1.Workspace(members: [\"json\", \"wrap\", \"broken\"])\n")
        .file("broken/hirsel.hcfg", "this is [ not toml\n");
    let commit = objects.commit(&tree, None, 1_700_000_000, "v1");
    server.serve("alpha.test/rb", Repo::at(objects, &commit));
    added("broken", "https://alpha.test/rb", &commit);

    // `json` bound by an archive the commit holds: read from the snapshot, nothing fetched.
    fresh("archived");
    law.fixture_json();
    let mut objects = r1_objects();
    let tree = r1_tree()
        .file(
            "wrap/hirsel.hcfg",
            WRAP_MANIFEST.replace(
                "Path(package: \"fixture.test/json\", major: 1, path: \"../json\", authority: [])",
                &format!("Published(package: \"fixture.test/json\", major: 1, root: \"{PACKAGE_X}\", locator: Archive(\"../vendor/jx.hpkg\"), authority: [])"),
            ),
        )
        .file("vendor/jx.hpkg", fs::read(law.path("vendor/jx.hpkg")).unwrap());
    let commit = objects.commit(&tree, None, 1_700_000_000, "v1");
    server.serve("alpha.test/ra", Repo::at(objects, &commit));
    added("archived", "https://alpha.test/ra", &commit);
    assert_eq!(
        server.log(),
        [
            info("alpha.test", "ra"),
            listing("alpha.test", "ra", &["refs/tags/v1", "refs/heads/v1"]),
            fetching("alpha.test", "ra", &commit),
        ]
    );

    // `json` bound by `git` and `rev` from another repository: one more fetch, no listing.
    fresh("nested");
    let (rj, j) = rj_repo();
    server.serve("alpha.test/rj", rj);
    let commit = nested_wrap(&server, "alpha.test/rn", &j, PACKAGE_X);
    let (status, stdout, stderr) =
        server.add(&law, "nested", &["git+https://alpha.test/rn?dir=wrap#v1", "--json"]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["result"]["evidence"]["root"], PACKAGE_W);
    assert_eq!(json["result"]["evidence"]["fetched"], serde_json::json!([format!("git:https://alpha.test/rj#{j}")]));
    assert_eq!(
        server.log(),
        [
            info("alpha.test", "rn"),
            listing("alpha.test", "rn", &["refs/tags/v1", "refs/heads/v1"]),
            fetching("alpha.test", "rn", &commit),
            info("alpha.test", "rj"),
            fetching("alpha.test", "rj", &j),
        ]
    );
    assert_eq!(stored(&law, "packages"), roots(&[PACKAGE_X, PACKAGE_W]));
}

/// `rj`: a server-built commit J holding `jx` at the top of its tree.
fn rj_repo() -> (Repo, String) {
    let mut objects = Objects::default();
    let tree = Tree::default()
        .file("hirsel.hcfg", JSON_MANIFEST)
        .file("json.hirsel", PACKAGE_X_SOURCE);
    let j = objects.commit(&tree, None, 1_700_000_000, "j");
    (Repo::at(objects, &j), j)
}

/// Serves r1 with `wrap` binding `json` by `git` at `rj`'s J pinned to `pin`, at `key`.
fn nested_wrap(server: &GitServer, key: &str, j: &str, pin: &str) -> String {
    let mut objects = r1_objects();
    let tree = r1_tree().without("json").file(
        "hirsel.hcfg",
        "pkg_v1.Workspace(members: [\"wrap\"])\n",
    ).file(
        "wrap/hirsel.hcfg",
        WRAP_MANIFEST.replace(
            "Path(package: \"fixture.test/json\", major: 1, path: \"../json\", authority: [])",
            &format!("Published(package: \"fixture.test/json\", major: 1, root: \"{pin}\", locator: Git(url: \"https://alpha.test/rj\", rev: \"{j}\"), authority: [])"),
        ),
    );
    let commit = objects.commit(&tree, None, 1_700_000_000, "v1");
    server.serve(key, Repo::at(objects, &commit));
    commit
}

/// A variant of r1 committed at time 1700000000 with message `v1` and no parent, its commit
/// checked against Git 2.43.0's id.
fn variant(tree: Tree, expected: &str) -> Repo {
    let mut objects = Objects::default();
    let commit = objects.commit(&tree, None, 1_700_000_000, "v1");
    assert_eq!(commit, expected, "fixture writer");
    Repo::at(objects, &commit)
}

/// A server-built variant: no Git id to check against, the law's own fixture.
fn built(tree: Tree) -> (Repo, String) {
    let mut objects = Objects::default();
    let commit = objects.commit(&tree, None, 1_700_000_000, "v1");
    (Repo::at(objects, &commit), commit)
}

fn r2() -> Repo {
    variant(r1_tree().link("wrap/wrap.hirsel", "../json/json.hirsel"), R2)
}

fn r3() -> Repo {
    variant(
        r1_tree()
            .link("wrap/wrap.hirsel", "real/wrap.hirsel")
            .file("wrap/real/wrap.hirsel", WRAP_V1),
        R3,
    )
}

fn r4() -> Repo {
    variant(
        r1_tree()
            .file(".gitattributes", "* text eol=crlf filter=sentinel ident export-subst\nwrap/wrap.hirsel export-ignore\n")
            .file(".gitmodules", "[submodule \"evil\"]\n\tpath = vendor/evil\n\turl = https://alpha.test/evil\n")
            .file(".lfsconfig", "[lfs]\n\turl = https://alpha.test/lfs\n")
            .submodule("vendor/evil", C2),
        R4,
    )
}

fn r5() -> Repo {
    variant(
        r1_tree()
            .file(
                "wrap/hirsel.hcfg",
                WRAP_MANIFEST.replace(
                    "(\"wrap.hirsel\", \"wrap\")",
                    "(\"wrap.hirsel\", \"wrap\"), (\"ext/x.hirsel\", \"x\")",
                ),
            )
            .submodule("wrap/ext", C2),
        R5,
    )
}

const R6_MANIFEST: &str = r#"pkg_v1.Package(
    name: "fixture.test/demo",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("api.hirsel", "api"), ("aa.hirsel", "aa"), ("b.hirsel", "b")],
    dependencies: [],
    foreign: Some([
        ("aa", Native(
            symbols: "ha_",
            archives: [
                ArchiveFile(
                    target: "x86_64-unknown-linux-gnu",
                    file: "native/aa-x86.a",
                    sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                ),
                ArchiveFile(
                    target: "aarch64-unknown-linux-gnu",
                    file: "native/aa-arm.a",
                    sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                ),
            ],
        )),
        ("b", Native(
            symbols: "hb_",
            archives: [
                ArchiveFile(
                    target: "x86_64-unknown-linux-gnu",
                    file: "native/b.a",
                    sha256: "3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d",
                ),
            ],
        )),
    ]),
)
"#;

fn r6() -> Repo {
    variant(
        Tree::default()
            .file("hirsel.hcfg", R6_MANIFEST)
            .file("api.hirsel", "pub opaque type A {}\npub opaque type B {}\n")
            .file("aa.hirsel", "pub fn ping(f: Foreign[api.A]) -> u64\n")
            .file("b.hirsel", "pub fn pong(f: Foreign[api.B]) -> u64\n")
            .file("native/aa-x86.a", "abc")
            .file("native/aa-arm.a", "")
            .file("native/b.a", "version https://git-lfs.github.com/spec/v1\noid sha256:3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d\nsize 1\n"),
        R6,
    )
}

/// A whole pack of `ids` from `objects`, with the entry for `replace` swapped for `entry`.
fn pack_with(objects: &Objects, ids: &[String], replace: &[(&str, Entry)]) -> Vec<u8> {
    let mut entries = Vec::new();
    for id in ids {
        match replace.iter().position(|(r, _)| r == id) {
            Some(at) => entries.push(match &replace[at].1 {
                Entry::Reference(base, data) => Entry::Reference(base.clone(), data.clone()),
                Entry::Whole(kind, data) => Entry::Whole(*kind, data.clone()),
                Entry::Offset(base, data) => Entry::Offset(*base, data.clone()),
                Entry::Raw(bytes) => Entry::Raw(bytes.clone()),
            }),
            None => {
                let (kind, data) = &objects.0[id];
                entries.push(Entry::Whole(*kind, data.clone()));
            }
        }
    }
    write_pack(&entries)
}

/// A hook answering every `fetch` with `pack` framed as protocol version 2 does.
fn fetch_answers(pack: Vec<u8>) -> impl FnMut(&Request) -> Option<Reply> + Send + 'static {
    move |request| {
        (request.command.as_deref() == Some("fetch")).then(|| {
            let mut out = Vec::new();
            framed_pack(&mut out, &[C1], true, &pack);
            Reply::Ok("application/x-git-upload-pack-result", out)
        })
    }
}

fn hand_binding(root: &str) -> String {
    format!(
        "dependencies: [(\"wrap\", Published(package: \"fixture.test/wrap\", major: 1, root: \"{root}\", locator: Git(url: \"https://alpha.test/r1\", rev: \"{C1}\", dir: Some(\"wrap\")), authority: []))],"
    )
}

/// L31. bug: a fetch that trusts the server's object ids or the ref, a missing delta base
/// treated as empty or followed for ever, and an admission, nested or outer, before the root
/// check.
/// oracle: C1, its tree and blob ids from Git 2.43.0; X and W from section 6; the refusal
/// texts are section 9's rows written by hand.
#[test]
fn l31_tampered_commit_keeps_nothing() {
    let law = PackageLaw::new();
    let server = GitServer::start(&law);
    server.serve("alpha.test/r1", Repo::r1());
    let source = format!("git:https://alpha.test/r1#{C1}/wrap");
    law.app("app", &hand_binding(PACKAGE_X));
    PackageLaw::refuse(
        server.run(&law, &["check", "app"], "packages"),
        &format!("fixture.test/wrap@1: {source} holds {PACKAGE_W}, but the binding pins {PACKAGE_X}"),
    );
    law.empty_store();

    law.app("app", &hand_binding(PACKAGE_W));
    let objects = r1_objects();
    let ids = objects.reachable(C1);
    let bad = |pack: Vec<u8>, failure: &str| {
        server.hook(fetch_answers(pack));
        PackageLaw::refuse(
            server.run(&law, &["check", "app"], "packages"),
            &format!("fixture.test/wrap@1: {source} sent a bad pack: {failure}"),
        );
        law.empty_store();
    };
    // C2's bytes in place of C1's blob: they hash to C2's blob, which nothing names.
    bad(
        pack_with(&objects, &ids, &[(C1_WRAP_BLOB, Entry::Whole(Kind::Blob, WRAP_V2.into()))]),
        &format!("object {C1_WRAP_BLOB} is not in the pack"),
    );
    // C2's objects for `want C1`.
    bad(
        pack_with(&objects, &objects.reachable(C2), &[]),
        &format!("commit {C1} is not in the pack"),
    );
    // A REF_DELTA whose base is absent.
    let absent = "1111111111111111111111111111111111111111";
    bad(
        pack_with(
            &objects,
            &ids,
            &[(C1_WRAP_BLOB, Entry::Reference(absent.into(), delta(b"", WRAP_V1.as_bytes())))],
        ),
        &format!("delta base {absent} is not in the pack"),
    );
    // Two REF_DELTA entries that name each other.
    let json_blob = ids
        .iter()
        .find(|id| objects.0[*id].1 == PACKAGE_X_SOURCE.as_bytes())
        .unwrap()
        .clone();
    bad(
        pack_with(
            &objects,
            &ids,
            &[
                (
                    &json_blob,
                    Entry::Reference(C1_WRAP_BLOB.into(), delta(WRAP_V1.as_bytes(), PACKAGE_X_SOURCE.as_bytes())),
                ),
                (
                    C1_WRAP_BLOB,
                    Entry::Reference(json_blob.clone(), delta(PACKAGE_X_SOURCE.as_bytes(), WRAP_V1.as_bytes())),
                ),
            ],
        ),
        &format!("delta base {} is not in the pack", first_reference(&ids, &json_blob)),
    );
    server.with(|served| served.hook = None);

    // The nested source fetched and verified X; the outer root then differs: nothing is kept.
    let (rj, j) = rj_repo();
    server.serve("alpha.test/rj", rj);
    let commit = nested_wrap(&server, "alpha.test/rn", &j, PACKAGE_X);
    law.app(
        "app",
        &format!("dependencies: [(\"wrap\", Published(package: \"fixture.test/wrap\", major: 1, root: \"{PACKAGE_X}\", locator: Git(url: \"https://alpha.test/rn\", rev: \"{commit}\", dir: Some(\"wrap\")), authority: []))],"),
    );
    let (status, _, stderr) = server.run(&law, &["check", "app"], "packages");
    assert_eq!(status, Some(2), "{stderr}");
    assert!(
        stderr.starts_with(&format!(
            "hirsel: fixture.test/wrap@1: git:https://alpha.test/rn#{commit}/wrap holds {PACKAGE_W}, but the binding pins {PACKAGE_X}"
        )),
        "{stderr}"
    );
    law.empty_store();
}

/// The base the first `REF_DELTA` of a pack written in `ids` order names, of the two that name
/// each other (`json_blob` names the wrap blob, and the wrap blob names `json_blob`).
fn first_reference(ids: &[String], json_blob: &str) -> String {
    let json_first = ids.iter().position(|id| id == json_blob)
        < ids.iter().position(|id| id == C1_WRAP_BLOB);
    if json_first { C1_WRAP_BLOB.to_owned() } else { json_blob.to_owned() }
}

/// L32. bug: a walk that contains links only to the commit instead of the package or the
/// workspace, containment decided on a normalised path, a tree flattened into a map of paths,
/// a link loop decided by the host, and an encoder that hashes link text.
/// oracle: r2 and r3 from Git 2.43.0, W and X from section 6, the 40-link bound of section 1;
/// refusal texts written by hand from section 9.
#[test]
fn l32_link_that_leaves_the_package() {
    let law = PackageLaw::new();
    let server = GitServer::start(&law);
    server.serve("alpha.test/r2", r2());
    server.serve("alpha.test/r3", r3());
    let outside = |repo: &str, rev: &str| {
        format!("git:https://alpha.test/{repo}#{rev}/wrap/hirsel.hcfg: sources: \"wrap.hirsel\" resolves outside the package")
    };
    app_at(&law, "app");
    let manifest = manifest_text(&law, "app");
    PackageLaw::refuse(
        server.add(&law, "app", &["git+https://alpha.test/r2?dir=wrap#v1"]),
        &outside("r2", R2),
    );
    assert_eq!(manifest_text(&law, "app"), manifest);
    assert!(!law.path("app/hirsel.lock").exists());
    law.empty_store();

    let writes_w = |name: &str, repo: &str, rev: &str| {
        app_at(&law, name);
        PackageLaw::pass(server.add(&law, name, &[&format!("git+https://alpha.test/{repo}?dir=wrap#v1")]));
        let binding = wrap_binding(&format!("https://alpha.test/{repo}"), rev, PACKAGE_W, Some("refs/tags/v1"));
        assert!(manifest_text(&law, name).contains(&binding), "{}", manifest_text(&law, name));
        let lock = fs::read_to_string(law.path(&format!("{name}/hirsel.lock"))).unwrap();
        assert!(lock.contains(&format!("id = \"fixture.test/json@1\"\nversion = \"1.0.0\"\nroot = \"{PACKAGE_X}\"")), "{lock}");
    };
    writes_w("control", "r3", R3);

    // A chain of 40 links inside `wrap` reaches the file; 41 do not.
    let chain = |links: usize| {
        let mut tree = r1_tree().file("wrap/real.hirsel", WRAP_V1);
        for n in 1..=links {
            let path = if n == 1 { "wrap/wrap.hirsel".to_owned() } else { format!("wrap/l{n}") };
            let target = if n == links { "real.hirsel".to_owned() } else { format!("l{}", n + 1) };
            tree = tree.link(&path, &target);
        }
        built(tree)
    };
    let (repo, forty) = chain(40);
    server.serve("alpha.test/forty", repo);
    writes_w("forty", "forty", &forty);
    let (repo, rev) = chain(41);
    server.serve("alpha.test/fortyone", repo);
    app_at(&law, "fortyone");
    PackageLaw::refuse(
        server.add(&law, "fortyone", &["git+https://alpha.test/fortyone?dir=wrap#v1"]),
        &format!("git:https://alpha.test/fortyone#{rev}/wrap/hirsel.hcfg: sources: \"wrap.hirsel\" does not resolve to a regular file"),
    );

    // A top-level entry named `json/json.hirsel` is never what `json/json.hirsel` names.
    let (repo, rev) = built(r1_tree().named("json/json.hirsel", Node::File(PACKAGE_Y_SOURCE.into())));
    server.serve("alpha.test/slash", repo);
    writes_w("slash", "slash", &rev);

    // `up` leads out of `wrap` although the final file is inside.
    let (repo, rev) = built(
        r1_tree()
            .link("wrap/up", "..")
            .link("wrap/wrap.hirsel", "up/wrap/real.hirsel")
            .file("wrap/real.hirsel", WRAP_V1),
    );
    server.serve("alpha.test/up", repo);
    app_at(&law, "up");
    PackageLaw::refuse(
        server.add(&law, "up", &["git+https://alpha.test/up?dir=wrap#v1"]),
        &outside("up", &rev),
    );

    // A listed member whose directory is a link out of the workspace.
    let (repo, rev) = built(
        Tree::default()
            .file("ws/hirsel.hcfg", "pkg_v1.Workspace(members: [\"client\", \"lib\"])\n")
            .file(
                "ws/client/hirsel.hcfg",
                r#"pkg_v1.Package(
    name: "fixture.test/client",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("client.hirsel", "client")],
    dependencies: [
        ("lib", Path(package: "fixture.test/lib", major: 1, path: "../lib", authority: [])),
    ],
)
"#,
            )
            .file("ws/client/client.hirsel", WRAP_V1)
            .link("ws/lib", "../elsewhere/lib")
            .file(
                "elsewhere/lib/hirsel.hcfg",
                "pkg_v1.Package(name: \"fixture.test/lib\", version: \"1.0.0\", charge: ChargeV1, sources: [(\"lib.hirsel\", \"lib\")], dependencies: [])\n",
            )
            .file("elsewhere/lib/lib.hirsel", PACKAGE_X_SOURCE),
    );
    server.serve("alpha.test/ws", repo);
    app_at(&law, "ws");
    PackageLaw::refuse(
        server.add(&law, "ws", &["git+https://alpha.test/ws?dir=ws/client#v1"]),
        &format!("git:https://alpha.test/ws#{rev}/ws/hirsel.hcfg: workspace.members: \"lib\" resolves outside the workspace"),
    );
}

/// L36. bug: submodule recursion, and an LFS pointer packed as if it were the archive.
/// oracle: r5 and r6 from Git 2.43.0; the pointer's SHA-256 computed by hand over the 126
/// bytes of section 12; refusal texts written by hand from section 9.
#[test]
fn l36_submodules_and_lfs_pointers() {
    let law = PackageLaw::new();
    let server = GitServer::start(&law);
    server.serve("alpha.test/r5", r5());
    server.serve("alpha.test/r6", r6());
    app_at(&law, "app");
    PackageLaw::refuse(
        server.add(&law, "app", &["git+https://alpha.test/r5?dir=wrap#v1"]),
        &format!("git:https://alpha.test/r5#{R5}/wrap/hirsel.hcfg: sources: \"ext/x.hirsel\" reaches submodule \"ext\", and hirsel does not fetch submodules"),
    );
    assert!(server.log().iter().all(|entry| entry.contains("/r5/")), "a second repository");
    PackageLaw::refuse(
        server.add(&law, "app", &["git+https://alpha.test/r6#v1"]),
        &format!("git:https://alpha.test/r6#{R6}/hirsel.hcfg: foreign.b: archive \"native/b.a\" for x86_64-unknown-linux-gnu does not match its pin: expected sha256 3e23e8160039594a33894f6564e1b1348bbd7a0088d42c4acb73eeaed59c009d, found sha256 70edffc238bc4d1e48b82fd69143269662f8daf7556f399d76669f3976dad323; it is a Git LFS pointer, and hirsel does not fetch LFS content"),
    );
    law.empty_store();
}
