//! Source publishing laws (docs/spec/launch.md "Publish and assemble" step 1): the
//! launcher publishes a submitted tree with the normal checker and admits the publication.
use super::Launcher;
use super::admit::Tree;
use super::laws::{Rig, attempt, group};
use super::session::*;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

enum Compile {
    Normal,
    Capped,
    Held,
}

/// Source byte limits (launch.md "Source by content", "Limits"): exactly 5 MiB of
/// assembled tree codec bytes is admitted for publishing and one byte over it is refused
/// `Invalid`; exactly 4 MiB of file content is admitted and one byte over it is refused.
#[test]
fn source_tree_byte_and_content_limits_are_exact() {
    let padded_tree = |bytes: usize| {
        // The tag-free Tree record with "." and one grant has 25 bytes of framing.
        Tree {
            package: ".".into(),
            grants: vec!["g".repeat(bytes - 25)],
            ..Tree::default()
        }
        .encode()
    };
    let content_tree = |bytes: usize| {
        Tree {
            package: ".".into(),
            files: vec![
                ("a.hirsel".into(), vec![b'a'; bytes - 1]),
                ("b.hirsel".into(), vec![b'b']),
            ],
            ..Tree::default()
        }
        .encode()
    };
    let exact_tree = padded_tree(5 << 20);
    let over_tree = padded_tree((5 << 20) + 1);
    assert_eq!(exact_tree.len(), 5 << 20);
    assert_eq!(over_tree.len(), (5 << 20) + 1);
    assert!(super::admit::source(&exact_tree, false).is_ok());
    assert!(matches!(
        super::admit::source(&over_tree, false),
        Err(Failure::Invalid(text)) if text == "source tree: over its byte limit"
    ));
    assert!(super::admit::source(&content_tree(4 << 20), false).is_ok());
    assert!(matches!(
        super::admit::source(&content_tree((4 << 20) + 1), false),
        Err(Failure::Invalid(text)) if text == "source tree: its files are over the content limit"
    ));
}

/// The Rust launcher's tree limits are the platform's (`launcher.hirsel`), which has no
/// generated Rust side: each `pub const` there, a literal or a sum of earlier ones, has the
/// value of the constant of the same name in `launcher::protocol`.
#[test]
fn law_tree_limits_agree_with_launcher_hirsel() {
    use super::admit::protocol::{MANIFEST_BYTES, MAX_FRAME, SOURCE_BYTES, TREE_BYTES};
    let source = include_str!("../../../../core/programs/platform/launcher.hirsel");
    let mut values = std::collections::BTreeMap::<&str, u64>::new();
    for line in source.lines() {
        let Some((name, expr)) = line
            .strip_prefix("pub const ")
            .and_then(|rest| rest.split_once(": u64 = "))
        else {
            continue;
        };
        let value = expr
            .split(" + ")
            .map(|term| match term.trim().strip_suffix("u64") {
                Some(digits) => digits.parse::<u64>().unwrap(),
                None => *values
                    .get(term.trim())
                    .unwrap_or_else(|| panic!("{name} = {expr}: {term} is not an earlier u64")),
            })
            .sum();
        values.insert(name, value);
    }
    for (name, rust) in [
        ("SOURCE_BYTES", SOURCE_BYTES),
        ("MANIFEST_BYTES", MANIFEST_BYTES),
        ("TREE_BYTES", TREE_BYTES),
        ("MAX_FRAME", MAX_FRAME),
    ] {
        let hirsel = values
            .get(name)
            .unwrap_or_else(|| panic!("launcher.hirsel has no `pub const {name}: u64`"));
        assert_eq!(*hirsel, rust as u64, "launcher.{name}");
    }
}

/// Submitted source is published by the normal checker under the launcher's own stores
/// and admitted as that publication. A type error or exhaustion records no recipe.
/// Releasing an attempt kills the compiler's group and discards its staged recipe.
#[test]
fn source_publishes_with_the_normal_checker() {
    use super::stores::Stores;
    use crate::code_store::{CodeStore, ObjectKind};
    use crate::native::tests::process::{CHILD_MARK, request_command_at};

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let core = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../core")
        .canonicalize()
        .unwrap();
    let ffi = core.join("programs/ffi_laws");
    let exe = std::env::current_exe().unwrap();
    let cache = root.join("cache");
    let child = Arc::new(move |args: &[String], at: &Path| {
        let mut args = args.to_vec();
        if args[0] == "build" {
            args.extend(["--libraries".into(), core.to_string_lossy().into_owned()]);
        }
        let mut command = request_command_at(&exe, &args, at);
        command.env("XDG_CACHE_HOME", &cache);
        command
    });
    let after = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes).into_owned();
        text.split_once(CHILD_MARK)
            .map_or(text.clone(), |(_, t)| t.to_owned())
    };
    let app = root.join("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(
        app.join("hirsel.hcfg"),
        "pkg_v1.Package(name: \"local/app\", version: \"1.0.0\", charge: ChargeV1, sources: [(\"main.hirsel\", \"main\")], dependencies: [])\n",
    )
    .unwrap();
    let main = |body: &str| {
        fs::write(
            app.join("main.hirsel"),
            format!("pub fn main() -> Result[(), Exit] {{ {body} }}\n"),
        )
        .unwrap();
    };
    let oracle = |args: &[&str]| {
        let path = |name: &str| {
            root.join("oracle")
                .join(name)
                .to_string_lossy()
                .into_owned()
        };
        let mut args: Vec<String> = args.iter().map(|&a| a.to_owned()).collect();
        args.extend(["--package-store".into(), path("packages")]);
        if args[0] == "build" {
            args.extend([
                "--store".into(),
                path("compiler"),
                "--code-store".into(),
                path("code"),
            ]);
        }
        let output = child(&args, &app).output().unwrap();
        (
            output.status.code(),
            after(&output.stdout),
            after(&output.stderr),
        )
    };
    let tree = || {
        let files = ["hirsel.hcfg", "hirsel.lock", "main.hirsel"]
            .map(|name| (name.to_owned(), fs::read(app.join(name)).unwrap()));
        Tree {
            package: ".".into(),
            files: files.into(),
            ..Tree::default()
        }
        .encode()
    };

    main("Ok(())");
    let (status, stdout, stderr) = oracle(&["lock", "."]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    let (status, stdout, stderr) = oracle(&["build", "--emit", "recipe", ".", "--frozen"]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    let published = crate::native::recipe_build::parse_root(stdout.trim()).unwrap();

    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    let stores = Stores::open(&state).unwrap();
    let records = stores.records();
    let compile = Arc::clone(&child);
    let mode = Arc::new(Mutex::new(Compile::Normal));
    let next = mode.clone();
    let pids = root.join("pids");
    let marker = pids.clone();
    let compiler: super::Compiler = Arc::new(move |args: &[String]| {
        let mut command = compile(args, Path::new("/"));
        match *next.lock().unwrap() {
            Compile::Normal => Ok(command),
            Compile::Capped => {
                let inherited = std::env::var("HIRSEL_MEMORY_LIMIT").ok();
                let inherited = inherited
                    .as_deref()
                    .map_or(hirsel_guard::DEFAULT_LIMIT, |cap| {
                        hirsel_guard::parse_limit(cap.as_bytes()).unwrap()
                    });
                command.env("HIRSEL_MEMORY_LIMIT", inherited.min(8 << 20).to_string());
                Ok(command)
            }
            Compile::Held => {
                // Hold the command after a real publish creates the recipe. Keep a
                // descendant alive too, so killing only the group leader fails the law.
                let mut held = Command::new("sh");
                held.args([
                    "-ec",
                    r#"
                    marker=$1
                    shift
                    "$@"
                    sleep 30 &
                    printf '%s %s\n' "$$" "$!" > "$marker.tmp"
                    mv "$marker.tmp" "$marker"
                    wait
                "#,
                    "hirsel-source-law",
                ])
                .arg(&marker)
                .arg(command.get_program())
                .args(command.get_args())
                .envs(command.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))));
                Ok(held)
            }
        }
    });
    let admission = super::Admission::open(&state, records, super::limits(), 1 << 20)
        .unwrap()
        .compile_with(compiler);
    let launcher = Launcher::new(&state, super::Lifecycle::Leader, || Ok(stores)).unwrap();
    let mut rig = Rig { dir, launcher };
    rig.launcher.admit_with(admission).unwrap();
    rig.bind();
    let admit = |rig: &mut Rig<Stores>, n: u64, bytes: Vec<u8>| {
        rig.ask(Request::Admit(
            group(),
            attempt(n),
            Input::Source(bytes, false),
            Approvals::default(),
            super::admit::fixture_deadline_ms(),
        ));
        // A cold compile of core takes longer than `until` waits.
        let end = Instant::now() + Duration::from_secs(240);
        let mut seen = Vec::new();
        while !seen
            .iter()
            .any(|e| matches!(e, Event::Admitted(..) | Event::AdmissionFailed(..)))
        {
            assert!(Instant::now() < end, "timed out with {seen:?}");
            rig.launcher.turn(Duration::from_millis(50)).unwrap();
            seen.extend(rig.launcher.take_events());
        }
        rig.ask(Request::Release(attempt(n)));
        seen
    };
    for n in [1, 2] {
        let answered = admit(&mut rig, n, tree());
        assert!(
            matches!(answered.as_slice(), [Event::Admitted(a, root, ..)]
                if *a == attempt(n) && root == &published.0),
            "source {n} did not admit as {published:?}: {answered:?}"
        );
    }

    main("1u64");
    let (status, _, diagnostic) = oracle(&["build", "--emit", "recipe", ".", "--frozen"]);
    assert!(status.is_some_and(|s| s != 0) && !diagnostic.is_empty());
    let recipes = || -> BTreeSet<PathBuf> {
        let code = CodeStore::new(state.join("code"));
        let kind = code
            .path(ObjectKind::Recipe, published)
            .parent()
            .unwrap()
            .to_owned();
        fs::read_dir(kind)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    };
    let before = recipes();
    let answered = admit(&mut rig, 3, tree());
    assert!(
        matches!(answered.as_slice(), [Event::AdmissionFailed(a, Failure::Uncompiled(text))]
            if *a == attempt(3) && text.contains(diagnostic.trim())),
        "a type error was not refused with {diagnostic:?}: {answered:?}"
    );
    assert_eq!(recipes(), before, "a refused source recorded a recipe");
    let mut missing_lock = Tree::decode(&tree()).unwrap();
    missing_lock.files.retain(|(path, _)| path != "hirsel.lock");
    let answered = admit(&mut rig, 4, missing_lock.encode());
    assert!(
        matches!(answered.as_slice(), [Event::AdmissionFailed(a, Failure::Uncompiled(text))]
            if *a == attempt(4) && text.contains("hirsel.lock")),
        "a tree without its frozen lock was not refused: {answered:?}"
    );

    let mut large = String::from("pub fn main() -> Result[(), Exit] { Ok(()) }\n");
    for n in 0..12_000 {
        writeln!(large, "pub fn value_{n}() -> u64 {{ {n}u64 }}").unwrap();
    }
    fs::write(app.join("main.hirsel"), large).unwrap();
    *mode.lock().unwrap() = Compile::Capped;
    let answered = admit(&mut rig, 6, tree());
    assert!(
        matches!(answered.as_slice(), [Event::AdmissionFailed(a, Failure::Exhausted(text))]
            if *a == attempt(6) && text.contains("local/app@1/main/main")
                && text.contains("reached (HIRSEL_MEMORY_LIMIT)")),
        "a capped compile did not report exhaustion for local/app@1/main/main: {answered:?}"
    );
    assert_eq!(recipes(), before, "an exhausted compile recorded a recipe");

    main("Error(Exit(status: 7u8))");
    *mode.lock().unwrap() = Compile::Held;
    assert!(
        rig.ask(Request::Admit(
            group(),
            attempt(7),
            Input::Source(tree(), false),
            Approvals::default(),
            super::admit::fixture_deadline_ms(),
        ))
        .is_empty()
    );
    let end = Instant::now() + Duration::from_secs(10);
    let (leader, descendant) = loop {
        if let Ok(pids) = fs::read_to_string(&pids) {
            let (leader, descendant) = pids.trim().split_once(' ').unwrap();
            break (
                leader.parse::<libc::pid_t>().unwrap(),
                descendant.parse::<libc::pid_t>().unwrap(),
            );
        }
        assert!(Instant::now() < end, "the held compile never started");
        rig.launcher.turn(Duration::from_millis(20)).unwrap();
        assert!(rig.launcher.take_events().is_empty());
    };
    let leader_exists = || crate::host::zombie(leader).is_some();
    // A killed grandchild can remain a zombie until its new parent reaps it.
    let desc_alive = || crate::host::zombie(descendant) == Some(false);
    assert!(leader_exists() && desc_alive());
    assert!(rig.ask(Request::Release(attempt(7))).is_empty());
    let source_clean = || fs::read_dir(state.join("source")).unwrap().next().is_none();
    let end = Instant::now() + Duration::from_secs(10);
    while leader_exists() || desc_alive() || !source_clean() {
        assert!(
            Instant::now() < end,
            "compile group was not cleaned up: {leader}, {descendant}"
        );
        rig.launcher.turn(Duration::from_millis(20)).unwrap();
        assert!(rig.launcher.take_events().is_empty());
    }
    assert_eq!(recipes(), before, "a cancelled compile recorded a recipe");
    for dir in ["images", "assemblies"] {
        assert!(
            fs::read_dir(state.join("objects").join(dir))
                .unwrap()
                .next()
                .is_none()
        );
    }
    *mode.lock().unwrap() = Compile::Normal;

    // This is the same checked-in entry and native archive as the FFI law. It is a valid
    // Foreign call with the capability granted. An untrusted tree submits no grant.
    let ffi_manifest = fs::read_to_string(ffi.join("hirsel.hcfg")).unwrap();
    let doc = hirsel_config_schema::package_document(ffi_manifest.as_bytes(), &mut false).unwrap();
    let tests = doc
        .replace(
            &[hirsel_config_schema::PathPart::Field("tests".into())],
            "None",
            Default::default(),
        )
        .unwrap()
        .unwrap();
    let manifest = tests.apply(&ffi_manifest);
    fs::write(app.join("hirsel.hcfg"), manifest).unwrap();
    let mut names = vec!["hirsel.hcfg", "hirsel.lock"];
    for file in [
        "main.hirsel",
        "laws.hirsel",
        "native.hirsel",
        "libffi_laws.a",
        "libffi_laws-aarch64-apple-darwin.a",
    ] {
        fs::copy(ffi.join(file), app.join(file)).unwrap();
        names.push(file);
    }
    let (status, stdout, stderr) = oracle(&["lock", "."]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    let ffi_source = Tree {
        package: ".".into(),
        files: names
            .into_iter()
            .map(|name| (name.to_owned(), fs::read(app.join(name)).unwrap()))
            .collect(),
        ..Tree::default()
    };
    let answered = admit(&mut rig, 5, ffi_source.encode());
    assert!(
        matches!(answered.as_slice(), [Event::AdmissionFailed(a, Failure::Uncompiled(text))]
            if *a == attempt(5) && text.contains("not granted: Foreign[hirsel.ai/ffi_laws@1/laws/Laws]")),
        "Foreign source without a grant was not refused: {answered:?}"
    );
    assert_eq!(recipes(), before, "refused source recorded a recipe");
}

/// The tree and the source failures share these bytes with
/// core/programs/platform/launcher_wire.hirsel; both were derived by hand from
/// core/std/api/codec.md (records in field order, a string or bytes as a u32 length and
/// its bytes, a list as a u32 count and its items, a sum as a u32 tag and its payload).
#[test]
fn source_vectors() {
    let hex = |s: &str| -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    };
    let tree = Tree {
        package: ".".into(),
        grants: vec!["G".into()],
        dirs: vec!["d".into()],
        files: vec![("a".into(), vec![1, 2])],
        required_entries: vec![(2, vec![0xaa, 0xbb])],
    };
    let bytes = hex(concat!(
        "010000002e010000000100000047010000000100000064010000000100000061020000000102",
        "010000000200000002000000aabb"
    ));
    assert_eq!(tree.encode(), bytes);
    assert_eq!(super::admit::tree(&bytes), Ok(tree));
    for (failure, expected) in [
        (
            Failure::Uncompiled("no".into()),
            "100000000807060504030201181716151413121103000000020000006e6f",
        ),
        (
            Failure::Exhausted("om".into()),
            "100000000807060504030201181716151413121104000000020000006f6d",
        ),
    ] {
        let shared = Attempt {
            hi: 0x0102030405060708,
            lo: 0x1112131415161718,
        };
        let event = Event::AdmissionFailed(shared, failure);
        let want = hex(expected);
        assert_eq!(event.encode(), want);
        assert_eq!(Event::decode(&want).unwrap(), event);
    }
}

/// A tree whose package could read as an option, or whose files could land outside the
/// scratch directory or collide there, is refused before anything is written or run.
#[test]
fn source_trees_stay_inside_their_scratch() {
    let file = |path: &str| (path.to_owned(), b"x".to_vec());
    let tree = |package: &str, files: Vec<(String, Vec<u8>)>| Tree {
        package: package.into(),
        files,
        ..Tree::default()
    };
    for (case, refused) in [
        ("an option package", tree("--libraries", vec![file("a")])),
        ("a parent package", tree("../app", vec![file("a")])),
        ("an absolute package", tree("/app", vec![file("a")])),
        ("a parent file", tree(".", vec![file("../a")])),
        ("an absolute file", tree(".", vec![file("/etc/a")])),
        ("an inner parent file", tree(".", vec![file("a/../../b")])),
        ("a dot file component", tree(".", vec![file("./a")])),
        ("a repeated file", tree(".", vec![file("a"), file("a")])),
        (
            "a file that is a directory",
            tree(".", vec![file("a"), file("a/b")]),
        ),
    ] {
        assert!(
            matches!(
                super::admit::tree(&refused.encode()),
                Err(Failure::Invalid(_))
            ),
            "{case} was accepted"
        );
    }
    let kept = tree(
        "app",
        vec![file("app/hirsel.hcfg"), file("app-x/a"), file("app/src/a")],
    );
    assert_eq!(super::admit::tree(&kept.encode()), Ok(kept));
}

/// The law gate (launch.md "The law gate"), driven by a gated `Admit(Source(tree, true))`:
/// a test granted anything beyond the allow list, or a `Dir` that leaves its working
/// directory, is `Denied` before any test runs or the pass cache is made; a failing test,
/// or a test source that does not parse, is `Red` naming it and records no recipe; an
/// all-passing tree admits with `Admitted.gate` naming the tree's digest, the published
/// root and the toolchain, and its passes are recorded under `STATE/test-results`, never
/// the user's cache. The same failing tree without the gate admits with no gate.
#[test]
fn law_gate_denies_authority_and_reads_the_verdict() {
    use super::stores::Stores;
    use crate::code_store::{CodeStore, ObjectKind};
    use crate::native::tests::process::request_command_at;
    use hirsel_core_ids::ContentHash;
    use sha2::Digest as _;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let core = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../core")
        .canonicalize()
        .unwrap();
    let user_cache = root.join("cache");
    let home = user_cache.clone();
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").unwrap());
    let kit = PathBuf::from(std::env::var_os("HIRSEL_LINK_KIT").unwrap());
    let prefix = root.join("toolchain");
    let target = crate::host::HOST_TARGET.triple();
    crate::installation::publish(
        &prefix,
        false,
        [(
            crate::installation::Flavour::PLAIN,
            (runtime.as_path(), target),
        )]
        .into(),
        Some(&kit),
        &mut crate::Meter::new(false),
    )
    .unwrap();
    let exe = prefix.join("hirsel");
    let compiler: super::Compiler = Arc::new(move |args: &[String]| {
        let mut args = args.to_vec();
        if args[0] != "lock" {
            args.extend(["--libraries".into(), core.to_string_lossy().into_owned()]);
        }
        let mut command = request_command_at(&exe, &args, Path::new("/"));
        command.env("XDG_CACHE_HOME", &home);
        if args[0] != "test" {
            return Ok(command);
        }
        // The gate reads the report as `hirsel test` prints it: drop the child's preamble.
        let mut report = Command::new("sh");
        report
            .args(["-c", "\"$@\" | sed '1,/request-child/d'", "sh"])
            .arg(command.get_program())
            .args(command.get_args())
            .envs(command.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))));
        Ok(report)
    });
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    let app = root.join("app");
    fs::create_dir_all(&app).unwrap();
    let tree = |tests: &str, body: &str| {
        fs::write(
            app.join("hirsel.hcfg"),
            format!(
                "pkg_v1.Package(name: \"local/app\", version: \"1.0.0\", charge: ChargeV1, sources: [(\"main.hirsel\", \"main\")], dependencies: [], tests: Some(Tests(limit: 67108864, sources: [(\"tests.hirsel\", \"tests\")], {tests})))\n"
            ),
        )
        .unwrap();
        fs::write(
            app.join("main.hirsel"),
            "pub fn main() -> Result[(), Exit] { Ok(()) }\n",
        )
        .unwrap();
        fs::write(app.join("tests.hirsel"), body).unwrap();
        let mut lock = compiler(&[
            "lock".into(),
            ".".into(),
            "--package-store".into(),
            state.join("packages").to_string_lossy().into_owned(),
        ])
        .unwrap();
        let output = lock.current_dir(&app).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let files = ["hirsel.hcfg", "hirsel.lock", "main.hirsel", "tests.hirsel"]
            .map(|name| (name.to_owned(), fs::read(app.join(name)).unwrap()));
        Tree {
            package: ".".into(),
            files: files.into(),
            ..Tree::default()
        }
        .encode()
    };
    let toolchain = ContentHash([0x7c; 32]);
    let stores = Stores::open(&state).unwrap();
    let admission = super::Admission::open(&state, stores.records(), super::limits(), 1 << 20)
        .unwrap()
        .compile_with(compiler.clone())
        .toolchain(toolchain);
    let launcher = Launcher::new(&state, super::Lifecycle::Leader, || Ok(stores)).unwrap();
    let mut rig = Rig { dir, launcher };
    rig.launcher.admit_with(admission).unwrap();
    rig.bind();
    let mut next = 0;
    let mut admit = |rig: &mut Rig<Stores>, bytes: &[u8], gate: bool| {
        next += 1;
        rig.ask(Request::Admit(
            group(),
            attempt(next),
            Input::Source(bytes.to_vec(), gate),
            Approvals::default(),
            super::admit::fixture_deadline_ms(),
        ));
        // A cold compile of core takes longer than `until` waits.
        let end = Instant::now() + Duration::from_secs(240);
        let mut seen = Vec::new();
        while !seen
            .iter()
            .any(|e| matches!(e, Event::Admitted(..) | Event::AdmissionFailed(..)))
        {
            assert!(Instant::now() < end, "timed out with {seen:?}");
            rig.launcher.turn(Duration::from_millis(50)).unwrap();
            seen.extend(rig.launcher.take_events());
        }
        rig.ask(Request::Release(attempt(next)));
        seen
    };
    let recipes = || -> BTreeSet<PathBuf> {
        let code = CodeStore::new(state.join("code"));
        let kind = code
            .path(ObjectKind::Recipe, ContentHash([0; 32]))
            .parent()
            .unwrap()
            .to_owned();
        fs::read_dir(kind)
            .into_iter()
            .flatten()
            .map(|e| e.unwrap().path())
            .collect()
    };
    let green = "@test\nfn good() -> Result[(), ()] { Ok(()) }\n";

    for (case, tests, named) in [
        (
            "a Net grant",
            "grants: [Clock, Net(Connect(cidr: \"127.0.0.1/32\", first_port: 1, last_port: 1))], runs: []",
            "Net",
        ),
        (
            "a parent Dir",
            "grants: [], runs: [(\"good\", TestRun(grant_change: Some(ReplaceDirs([Dir(\"data/../../up\")]))))]",
            "data/../../up",
        ),
        (
            "an absolute Dir",
            "grants: [Dir(\"/tmp\")], runs: []",
            "/tmp",
        ),
    ] {
        let answered = admit(&mut rig, &tree(tests, green), true);
        assert!(
            matches!(answered.as_slice(), [Event::AdmissionFailed(_, Failure::Denied(text))]
                if text.contains(named)),
            "{case} was not denied: {answered:?}"
        );
        assert!(
            !state.join("test-results").exists(),
            "{case} ran before its authority check"
        );
    }

    let failing = tree(
        "grants: [Tasks], runs: []",
        "@test\nfn bad() -> Result[(), ()] { Error(()) }\n\n@test\nfn good() -> Result[(), ()] { Ok(()) }\n",
    );
    let before = recipes();
    let answered = admit(&mut rig, &failing, true);
    assert!(
        matches!(answered.as_slice(), [Event::AdmissionFailed(_, Failure::Red(rows))]
            if rows.len() == 1 && rows[0].contains("local/app@1/tests/bad") && rows[0].contains("status 1")),
        "a failing test was not red: {answered:?}"
    );
    assert_eq!(recipes(), before, "a red gate recorded a recipe");

    // A test source that does not parse lists no tests; read alone, that run is green.
    let answered = admit(
        &mut rig,
        &tree("grants: [Tasks], runs: []", "@test\nfn broken( -> {\n"),
        true,
    );
    assert!(
        matches!(answered.as_slice(), [Event::AdmissionFailed(_, Failure::Red(rows))]
            if rows.len() == 1 && rows[0].contains("tests.hirsel") && rows[0].contains("do not check")),
        "an unparsable test source was not red: {answered:?}"
    );
    assert_eq!(recipes(), before, "a red gate recorded a recipe");

    let answered = admit(&mut rig, &failing, false);
    assert!(
        matches!(answered.as_slice(), [Event::Admitted(_, _, summary, _)] if summary.gate.is_none()),
        "an ungated source ran the gate: {answered:?}"
    );

    let green_tree = tree("grants: [Tasks, Dir(\"data\")], runs: []", green);
    // The gate names the tree by its manifest's digest, which is what the operator sent.
    let digest = sha2::Sha256::digest(Tree::decode(&green_tree).unwrap().manifest()).to_vec();
    for pass in ["first", "cached"] {
        let answered = admit(&mut rig, &green_tree, true);
        assert!(
            matches!(answered.as_slice(), [Event::Admitted(_, root, summary, _)]
            if summary.gate == Some(Green {
                tree: digest.clone(),
                root: root.clone(),
                toolchain: toolchain.0.to_vec(),
            })),
            "the {pass} green run did not admit with its gate: {answered:?}"
        );
    }
    assert!(
        fs::read_dir(state.join("test-results"))
            .unwrap()
            .next()
            .is_some(),
        "no pass was recorded under STATE/test-results"
    );
    assert!(
        !user_cache
            .join("hirsel/core-packages/test-results")
            .exists(),
        "the gate wrote the user's pass cache"
    );
}

/// A group links only the package roots its registration names (launch.md "Package roots
/// per group"): the launcher's package store is shared by every group, so a lock that pins
/// a root outside the group's list is Invalid before anything compiles, naming the
/// package; a root in the list, or a lock that pins none, passes.
#[test]
fn source_links_only_its_groups_package_roots() {
    let root = |byte: u8| format!("sha256:{}", format!("{byte:02x}").repeat(32));
    let lock = |pinned: &str| {
        format!(
            "# hirsel.lock: written by hirsel; do not edit.\nversion = 2\n\n[[package]]\nid = \"local/app@1\"\nversion = \"1.0.0\"\nsource = \"member:.\"\nedges = [\"fixture.test/json@1\"]\n\n[[package]]\nid = \"fixture.test/json@1\"\nversion = \"1.0.0\"\nroot = \"{pinned}\"\nsource = \"archive:vendor/json.hpkg\"\nedges = []\n"
        )
    };
    let tree = |lock: &str| Tree {
        package: ".".into(),
        files: vec![("hirsel.lock".into(), lock.as_bytes().to_vec())],
        ..Tree::default()
    };
    let allowed =
        |tree: &Tree, packages: &[[u8; 32]]| super::admit::lock_roots_allowed(tree, packages);
    let pinned = tree(&lock(&root(0xab)));
    assert!(
        matches!(allowed(&pinned, &[[0xcd; 32]]), Err(Failure::Invalid(text))
            if text.contains("fixture.test/json@1") && text.contains(&root(0xab))),
        "a root outside the group's list was linked"
    );
    assert_eq!(allowed(&pinned, &[[0xcd; 32], [0xab; 32]]), Ok(()));
    let unpinned = tree("# hirsel.lock: written by hirsel; do not edit.\nversion = 2\n");
    assert_eq!(allowed(&unpinned, &[]), Ok(()));
    assert!(
        matches!(
            allowed(&tree("version = 1\n"), &[]),
            Err(Failure::Invalid(_))
        ),
        "an unreadable lock was not refused"
    );
}

/// The law gate reads its test run's whole report (launch.md "The law gate", step 2): a
/// passing `hirsel test --json` report over 16 MiB is a green verdict, never a cut report
/// that fails to parse; a report that is not JSON is red, saying it is not JSON.
#[test]
fn law_gate_reads_a_report_of_any_size() {
    use super::admit::gate::{Verdict, gate};
    use std::sync::atomic::AtomicBool;

    let pad = (17 << 20).to_string();
    let run = |report: String| {
        let compiler: super::Compiler = Arc::new(move |args: &[String]| {
            let mut command = Command::new("sh");
            match args[0].as_str() {
                "test" => command.args(["-c", &report, "sh"]),
                _ => command.args(["-c", "true"]),
            };
            Ok(command)
        });
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("scratch/tree");
        fs::create_dir_all(&source).unwrap();
        let tree = Tree {
            package: ".".into(),
            ..Tree::default()
        };
        let cancel = AtomicBool::new(false);
        let deadline = Instant::now() + Duration::from_secs(120);
        gate(&compiler, dir.path(), &tree, &source, &cancel, deadline)
            .unwrap()
            .unwrap()
    };
    let big = format!(
        r#"printf '{{"passed":true,"tests":[{{"definition":"t","passed":true,"pad":"'; head -c {pad} /dev/zero | tr '\0' a; printf '"}}]}}'"#
    );
    assert_eq!(run(big), Verdict::Green);
    assert!(matches!(
        run(r#"printf '{"passed":true,"tests":['"#.into()),
        Verdict::Red(rows) if rows.len() == 1 && rows[0].contains("is not JSON")
    ));
}
