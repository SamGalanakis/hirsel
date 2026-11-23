//! Source publishing laws (docs/spec/launch.md "Publish and assemble" step 1): the
//! launcher publishes a submitted tree with the normal checker and admits the publication.
use super::Launcher;
use super::admit::Tree;
use super::laws::{Rig, attempt, group};
use super::session::*;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Submitted source is published by the normal checker under the launcher's own stores
/// and admitted as that publication; a type error is refused with the checker's
/// diagnostic and records no recipe. The oracle for both is `hirsel publish` of the
/// same files under separate stores.
#[test]
fn source_publishes_with_the_normal_checker() {
    use super::stores::Stores;
    use crate::code_store::{CodeStore, ObjectKind};
    use crate::native::tests::process::{CHILD_MARK, request_command_at};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let core = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../core")
        .canonicalize()
        .unwrap();
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
    let compiler: super::Compiler =
        Arc::new(move |args: &[String]| Ok(compile(args, Path::new("/"))));
    let admission = super::Admission::open(&state, records, super::limits(), 1 << 20)
        .unwrap()
        .compile_with(compiler);
    let launcher = Launcher::new(&state, None, || Ok(stores)).unwrap();
    let mut rig = Rig { dir, launcher };
    rig.launcher.admit_with(admission).unwrap();
    rig.bind();
    let mut admit = |n: u64| {
        rig.ask(Request::Admit(
            group(),
            attempt(n),
            Input::Source(tree()),
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
        let answered = admit(n);
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
    let answered = admit(3);
    assert!(
        matches!(answered.as_slice(), [Event::AdmissionFailed(a, Failure::Uncompiled(text))]
            if *a == attempt(3) && text.contains(diagnostic.trim())),
        "a type error was not refused with {diagnostic:?}: {answered:?}"
    );
    assert_eq!(recipes(), before, "a refused source recorded a recipe");
}

/// The tree and the source failures share these bytes with
/// core/programs/store/launcher_wire.hirsel; both were derived by hand from
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
    };
    let bytes = hex("010000002e010000000100000047010000000100000064010000000100000061020000000102");
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
