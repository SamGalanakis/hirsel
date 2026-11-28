// Git source laws L33 and L34 (packages.md section 12) over the fixture of laws_git.rs,
// included after it.

/// Every hook name githooks(5) of Git 2.43.0 lists.
const HOOKS: [&str; 28] = [
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-receive",
    "update",
    "proc-receive",
    "post-receive",
    "post-update",
    "reference-transaction",
    "push-to-checkout",
    "pre-auto-gc",
    "post-rewrite",
    "sendemail-validate",
    "fsmonitor-watchman",
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
    "post-index-change",
];

/// Writes an executable script at `path` that creates `sentinel` and exits 1.
fn trap(law: &PackageLaw, path: &str, sentinel: &Path) {
    use std::os::unix::fs::PermissionsExt;
    law.write(path, format!("#!/bin/sh\n: > '{}'\nexit 1\n", sentinel.display()));
    fs::set_permissions(law.path(path), fs::Permissions::from_mode(0o755)).unwrap();
}

/// L33. bug: a fetch through the `git` program, a checkout that applies `eol=crlf` (30 bytes
/// of `wrap.hirsel` and another root) or `export-ignore` (no `wrap.hirsel`), and one that
/// follows `.gitmodules` to `/evil` or `.lfsconfig` to `/lfs`.
/// oracle: R4 from Git 2.43.0 and W from section 6: the bytes the tree holds, unconverted.
#[test]
fn l33_nothing_in_the_repository_runs() {
    let law = PackageLaw::new();
    let server = GitServer::start(&law);
    server.serve("alpha.test/r4", r4());
    let sentinel = law.path("SENTINEL");
    trap(&law, "bin/git", &sentinel);
    trap(&law, "trap/sentinel", &sentinel);
    for hook in HOOKS {
        trap(&law, &format!("trap/hooks/{hook}"), &sentinel);
    }
    let program = law.path("trap/sentinel");
    law.write(
        "gitconfig",
        format!(
            "[filter \"sentinel\"]\n\tsmudge = {p}\n\tclean = {p}\n\tprocess = {p}\n\trequired = true\n[core]\n\thooksPath = {h}\n",
            p = program.display(),
            h = law.path("trap/hooks").display()
        ),
    );
    app_at(&law, "app");
    let mut command = add(&law, "app", &["git+https://alpha.test/r4?dir=wrap#v1"]);
    server.route(&mut command);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![law.path("bin")];
    paths.extend(std::env::split_paths(&path));
    command
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("GIT_CONFIG_GLOBAL", law.path("gitconfig"));
    PackageLaw::pass(result(command.output().unwrap()));
    let binding = wrap_binding("https://alpha.test/r4", R4, PACKAGE_W, Some("refs/tags/v1"));
    assert!(manifest_text(&law, "app").contains(&binding), "{}", manifest_text(&law, "app"));
    assert!(!sentinel.exists());
    let log = server.log();
    assert!(log.iter().all(|entry| !entry.contains("/evil") && !entry.contains("/lfs")), "{log:?}");
}

/// A fresh, empty package store `name`.
fn empty(law: &PackageLaw, name: &str) {
    let _ = fs::remove_dir_all(law.path(name));
    fs::create_dir(law.path(name)).unwrap();
}

/// L34. bug: a build that resolves a ref, a `--frozen` command that touches the network, a
/// store miss that cannot be refilled from the pinned commit, and a POST that follows a
/// redirect or a base taken from anywhere but the `info/refs` answer.
/// oracle: the request logs written by hand from GIT-7 and GIT-12, X and W from section 6,
/// and the refusal texts of section 9.
#[test]
fn l34_locked_builds_use_the_commit() {
    let law = PackageLaw::new();
    let source = format!("git:https://alpha.test/r1#{C1}/wrap");
    {
        let server = GitServer::start(&law);
        server.serve("alpha.test/r1", Repo::r1());
        app_at(&law, "app");
        PackageLaw::pass(server.add(&law, "app", &["git+https://alpha.test/r1?dir=wrap#v1"]));
    }
    // 1. The server is gone, and its address with it.
    let dead = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let mut command = law.command(&["build", "app", "--frozen"], "packages");
    command
        .env("ALL_PROXY", format!("http://{}", dead.local_addr().unwrap()))
        .env("NO_PROXY", "")
        .env_remove("no_proxy");
    drop(dead);
    PackageLaw::pass(PackageLaw::output(command));

    let server = GitServer::start(&law);
    server.serve("alpha.test/r1", Repo::r1());
    let locked = |server: &GitServer| {
        empty(&law, "s");
        PackageLaw::pass(server.run(&law, &["check", "app", "--locked"], "s"));
        assert_eq!(stored(&law, "s"), roots(&[PACKAGE_X, PACKAGE_W]));
    };
    // 2. One info/refs GET and one fetch of C1, nothing listed.
    locked(&server);
    let only_c1 = [info("alpha.test", "r1"), fetching("alpha.test", "r1", C1)];
    assert_eq!(server.log(), only_c1);

    // 3. A moved tag changes nothing.
    point(&server, "alpha.test/r1", "refs/tags/v1", C2);
    locked(&server);
    assert_eq!(server.log(), only_c1);

    // 4. W without X: X is refilled from the commit through fixture.test/wrap@1's source.
    let x = law.path(&package_entry("s", PACKAGE_X));
    if x.is_dir() {
        fs::remove_dir_all(&x).unwrap();
    } else {
        fs::remove_file(&x).unwrap();
    }
    PackageLaw::pass(server.run(&law, &["check", "app", "--locked"], "s"));
    assert_eq!(stored(&law, "s"), roots(&[PACKAGE_X, PACKAGE_W]));
    assert_eq!(server.log(), only_c1);

    // 5. The server will not send C1.
    server.hook(|request| {
        (request.command.as_deref() == Some("fetch")).then(|| {
            let mut out = Vec::new();
            pkt(&mut out, format!("ERR upload-pack: not our ref {C1}").as_bytes());
            Reply::Ok("application/x-git-upload-pack-result", out)
        })
    });
    empty(&law, "s");
    PackageLaw::refuse(
        server.run(&law, &["check", "app", "--locked"], "s"),
        "fixture.test/wrap@1: fetching git:https://alpha.test/r1#daf4441842dbd47464a183be86f1128a41b2bf26/wrap failed: the server would not send commit daf4441842dbd47464a183be86f1128a41b2bf26",
    );
    server.with(|served| served.hook = None);
    server.log();

    // 6. --frozen over an empty store touches nothing.
    empty(&law, "s");
    PackageLaw::refuse(
        server.run(&law, &["check", "app", "--frozen"], "s"),
        "fixture.test/wrap@1 sha256:c469ba7bc32d09ebf7461d8f075f6cb53ae9009432441da1ab6707229dae69f5 is not in the package store, and --frozen does not fetch (source git:https://alpha.test/r1#daf4441842dbd47464a183be86f1128a41b2bf26/wrap)",
    );
    assert_eq!(server.log(), Vec::<String>::new());

    // 7. info/refs redirects to beta.test: the POST goes to beta.test once.
    server.serve("beta.test/r1", Repo::r1());
    let redirect = |target: &'static str, post: bool| {
        move |request: &Request| {
            let location = || vec![("Location", target.to_owned())];
            if request.host == "alpha.test" && request.method == "GET" {
                return Some(Reply::Status(302, location()));
            }
            (post && request.host == "beta.test" && request.method == "POST")
                .then(|| Reply::Status(302, vec![("Location", "https://alpha.test/r1/git-upload-pack".to_owned())]))
        }
    };
    server.hook(redirect("https://beta.test/r1/info/refs?service=git-upload-pack", false));
    locked(&server);
    assert_eq!(
        server.log(),
        [info("alpha.test", "r1"), info("beta.test", "r1"), fetching("beta.test", "r1", C1)]
    );
    server.hook(redirect("https://beta.test/r1/info/refs?service=git-upload-pack", true));
    empty(&law, "s");
    PackageLaw::refuse(
        server.run(&law, &["check", "app", "--locked"], "s"),
        &format!("fixture.test/wrap@1: fetching {source} failed: the server answered 302"),
    );
    server.hook(redirect("http://beta.test/r1/info/refs?service=git-upload-pack", false));
    empty(&law, "s");
    PackageLaw::refuse(
        server.run(&law, &["check", "app", "--locked"], "s"),
        &format!(
            "fixture.test/wrap@1: fetching {source} failed: redirected to http://beta.test/r1/info/refs?service=git-upload-pack, which is not https"
        ),
    );
}
