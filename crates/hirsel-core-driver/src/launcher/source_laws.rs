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
        if args[0] == "publish" {
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
        app.join("hirsel.toml"),
        "[package]\nname = \"app\"\nmajor = 1\ncharge = \"charge-v1\"\n\n[sources]\n\"main.hirsel\" = \"main\"\n",
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
        if args[0] == "publish" {
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
        let files = ["hirsel.toml", "hirsel.lock", "main.hirsel"]
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
    let (status, stdout, stderr) = oracle(&["publish", ".", "--frozen"]);
    assert_eq!(status, Some(0), "{stdout}\n{stderr}");
    let published = crate::native::recipe_build::parse_root(stdout.trim()).unwrap();

    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    let stores = Stores::open(&state, 1 << 20).unwrap();
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
                    "publish",
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
    let launcher = Launcher::new(&state, || Ok(stores)).unwrap();
    let mut rig = Rig { dir, launcher };
    rig.launcher.admit_with(admission).unwrap();
    rig.bind();
    let admit = |rig: &mut Rig<Stores>, n: u64, bytes: Vec<u8>| {
        rig.ask(Request::Admit(
            group(),
            attempt(n),
            Input::Source(bytes),
            Approvals::default(),
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
    let (status, _, diagnostic) = oracle(&["publish", ".", "--frozen"]);
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
            if *a == attempt(6) && text.contains("app@1/main/main")
                && text.contains("reached (HIRSEL_MEMORY_LIMIT)")),
        "a capped compile did not report exhaustion for app@1/main/main: {answered:?}"
    );
    assert_eq!(recipes(), before, "an exhausted compile recorded a recipe");

    main("Error(Exit(status: 7u8))");
    *mode.lock().unwrap() = Compile::Held;
    assert!(
        rig.ask(Request::Admit(
            group(),
            attempt(7),
            Input::Source(tree()),
            Approvals::default()
        ))
        .is_empty()
    );
    let end = Instant::now() + Duration::from_secs(10);
    let (leader, descendant) = loop {
        if let Ok(pids) = fs::read_to_string(&pids) {
            let (leader, descendant) = pids.trim().split_once(' ').unwrap();
            break (
                format!("/proc/{leader}/stat"),
                format!("/proc/{descendant}/stat"),
            );
        }
        assert!(Instant::now() < end, "the held compile never started");
        rig.launcher.turn(Duration::from_millis(20)).unwrap();
        assert!(rig.launcher.take_events().is_empty());
    };
    let desc_alive = || {
        // A killed grandchild can remain a zombie until its new parent reaps it.
        fs::read_to_string(&descendant).is_ok_and(|stat| {
            stat.rsplit_once(") ")
                .is_some_and(|(_, s)| !s.starts_with('Z'))
        })
    };
    assert!(Path::new(&leader).exists() && desc_alive());
    assert!(rig.ask(Request::Release(attempt(7))).is_empty());
    let source_clean = || fs::read_dir(state.join("source")).unwrap().next().is_none();
    let end = Instant::now() + Duration::from_secs(10);
    while Path::new(&leader).exists() || desc_alive() || !source_clean() {
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
    let ffi_manifest = fs::read_to_string(ffi.join("hirsel.toml")).unwrap();
    let (manifest, _) = ffi_manifest.split_once("[tests]").unwrap();
    fs::write(app.join("hirsel.toml"), manifest).unwrap();
    let mut names = vec!["hirsel.toml", "hirsel.lock"];
    for file in [
        "main.hirsel",
        "laws.hirsel",
        "native.hirsel",
        "libffi_laws.a",
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
            if *a == attempt(5) && text.contains("not granted: Foreign[ffi_laws@1/laws/Laws]")),
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
        vec![file("app/hirsel.toml"), file("app-x/a"), file("app/src/a")],
    );
    assert_eq!(super::admit::tree(&kept.encode()), Ok(kept));
}

/// The law gate (launch.md "The law gate"): a test granted anything beyond the allow list,
/// or a `Dir` that leaves its working directory, is `Denied` before any test runs or the
/// pass cache is made; a failing test is `Red` naming it; an all-passing tree is `Green`,
/// and its passes are recorded under `STATE/test-results`, never the user's cache.
#[test]
fn law_gate_denies_authority_and_reads_the_verdict() {
    use super::admit::gate::{Verdict, gate};
    use crate::native::tests::process::request_command_at;
    use std::sync::atomic::AtomicBool;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let core = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../core")
        .canonicalize()
        .unwrap();
    let exe = std::env::current_exe().unwrap();
    let user_cache = root.join("cache");
    let home = user_cache.clone();
    let options = crate::native::tests::native_options(crate::native::Backend::Cranelift, &root);
    let crate::native::Link::Driver(cc) = options.link else {
        unreachable!()
    };
    let native = |path: &Path| path.canonicalize().unwrap().to_string_lossy().into_owned();
    let (runtime, cc) = (native(&options.runtime), native(&cc));
    let compiler: super::Compiler = Arc::new(move |args: &[String]| {
        let mut args = args.to_vec();
        if args[0] == "test" {
            let more = ["--runtime", &runtime, "--cc", &cc, "--libraries"];
            args.extend(more.map(String::from));
            args.push(core.to_string_lossy().into_owned());
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
    fs::create_dir_all(state.join("source")).unwrap();
    let app = root.join("app");
    fs::create_dir_all(&app).unwrap();
    let tree = |tests: &str, body: &str| {
        fs::write(
            app.join("hirsel.toml"),
            format!(
                "[package]\nname = \"app\"\nmajor = 1\ncharge = \"charge-v1\"\n\n[sources]\n\"main.hirsel\" = \"main\"\n\n[tests]\nlimit = 67108864\n{tests}\n[tests.sources]\n\"tests.hirsel\" = \"tests\"\n"
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
        let files = ["hirsel.toml", "hirsel.lock", "main.hirsel", "tests.hirsel"]
            .map(|name| (name.to_owned(), fs::read(app.join(name)).unwrap()));
        Tree {
            package: ".".into(),
            files: files.into(),
            ..Tree::default()
        }
    };
    let green = "@test\nfn good() -> Result[(), ()] { Ok(()) }\n";
    let run = |tree: &Tree| gate(&compiler, &state, tree, &AtomicBool::new(false)).unwrap();

    for (case, tests, named) in [
        (
            "a Net grant",
            "grants = [\"Clock\", \"Net=allow tcp a:1\"]\n",
            "Net=allow tcp a:1",
        ),
        (
            "a parent Dir",
            "\n[tests.runs.good]\ndirs = [\"data/../../up\"]\n",
            "data/../../up",
        ),
        ("an absolute Dir", "dirs = [\"/tmp\"]\n", "/tmp"),
    ] {
        let answered = run(&tree(tests, green));
        assert!(
            matches!(&answered, Err(Failure::Denied(text)) if text.contains(named)),
            "{case} was not denied: {answered:?}"
        );
        assert!(
            !state.join("test-results").exists(),
            "{case} ran before its authority check"
        );
    }

    let answered = run(&tree(
        "grants = [\"Tasks\"]\n",
        "@test\nfn bad() -> Result[(), ()] { Error(()) }\n\n@test\nfn good() -> Result[(), ()] { Ok(()) }\n",
    ));
    assert!(
        matches!(&answered, Ok(Verdict::Red(rows)) if rows.len() == 1
            && rows[0].contains("app@1/tests/bad") && rows[0].contains("status 1")),
        "a failing test was not red: {answered:?}"
    );

    let green_tree = tree("grants = [\"Tasks\"]\ndirs = [\"data\"]\n", green);
    for pass in ["first", "cached"] {
        assert_eq!(run(&green_tree), Ok(Verdict::Green), "{pass} run");
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
