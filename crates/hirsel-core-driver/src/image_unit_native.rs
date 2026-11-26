// The image unit. Law: a link reads the image objects it reaches as one unit,
// `ld -r` of them in link order, so a primed store and a fresh one link the
// same executable through the same unit (the reach laws, whose cores are
// images). The unit is not the separate objects' link: `ld -r` renumbers local
// and section symbols. What a program does and reports is the same. Oracle:
// the same build with the image objects linked one by one.

/// Three programs over std, each failing in or through std code or printing
/// through it: (exit, stdout, stderr).
const UNIT_PROGRAMS: [(&str, &str); 3] = [
    ("bounds", "pub fn main() -> Result[(), Exit] {\n    let mut xs: List[u64] = list.new()\n    let _ = list.remove(mut xs, 3)\n    Ok(())\n}\n"),
    ("adapter", "fn apply(f: fn(take Option[u64]) -> u64, x: Option[u64]) -> u64 {\n    f(x)\n}\npub fn main() -> Result[(), Exit] {\n    let none: Option[u64] = None\n    if string.starts_with(\"ab\", \"a\") && apply(option.unwrap, none) == 0u64 { Ok(()) } else { Error(Exit(status: 7)) }\n}\n"),
    ("strings", "pub fn main(out: Stdout) -> Result[(), Exit] {\n    let s = \"left right\"\n    let Some((a, b)) = string.split_once(s, \" \") else {\n        return Error(Exit(status: 2u8))\n    }\n    if string.starts_with(s, a) && string.ends_with(s, b) && string.find(s, 0, \"right\") == Some(5) {\n        result.unwrap(stdout.write(out, b));\n        Ok(())\n    } else {\n        Error(Exit(status: 1u8))\n    }\n}\n"),
];

pub(crate) struct ImageUnitRead {
    pub(super) key: ContentHash,
    pub(super) blob: PathBuf,
    pub(super) record: PathBuf,
    pub(super) prelinked: bool,
}

/// Build `source` as project `project` over std imaged into `cache`, with
/// debug lines; the executable, its run, and the unit the link read.
fn unit_build(
    dir: &Path,
    backend: Backend,
    project: &str,
    source: &str,
    cache: &str,
    separate: bool,
) -> (Vec<u8>, std::process::Output, Option<ImageUnitRead>) {
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let project = dir.join(project);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(dir.join(cache)).unwrap();
    std::fs::write(project.join("main.hirsel"), source).unwrap();
    let request = CheckRequest::program(&project, &libraries, &project.join("store")).unwrap();
    let product = Driver::default()
        .check_reachable_with_product(request, &dir.join(cache), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let mut options = native_options(backend, &project);
    options.debug_lines = true;
    if source == UNIT_PROGRAMS[2].1 {
        options.grant_mask = 1 << grant_kind("Stdout").unwrap();
    }
    UNIT.set(None);
    SEPARATE_IMAGE.set(separate);
    let built = product.build(&entry, &options);
    SEPARATE_IMAGE.set(false);
    let built = built.unwrap();
    assert!(built.linked, "{project:?}: linked");
    let output = Command::new(&built.executable).output().unwrap();
    (std::fs::read(&built.executable).unwrap(), output, UNIT.take())
}

/// The defined functions of an executable, by name and size: the symbols a
/// failure report or a debugger reads.
fn unit_functions(bytes: &[u8]) -> BTreeSet<(String, u64)> {
    use object::{Object as _, ObjectSymbol as _};
    let file = object::File::parse(bytes).unwrap();
    file.symbols()
        .filter(|s| s.kind() == object::SymbolKind::Text && s.is_definition())
        .map(|s| (s.name().unwrap().to_owned(), s.size()))
        .collect()
}

/// Each program linked through the unit runs, prints and reports as linked
/// from its separate objects, with the same functions and debug line rows.
/// Mutant: a unit missing one of its objects, red (the link refuses it). A
/// unit prelinked with `--discard-locals` or `--strip-all` stays green: the
/// functions are global symbols, which a relocatable output keeps.
fn image_unit_links_what_its_objects_link(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    for (name, source) in UNIT_PROGRAMS {
        let (unit, ran, read) = unit_build(dir.path(), backend, &format!("{name}-unit"), source, "unit-cache", false);
        let (separate, expected, none) =
            unit_build(dir.path(), backend, &format!("{name}-separate"), source, "separate-cache", true);
        assert!(read.is_some_and(|unit| unit.prelinked), "{name}: the link prelinked its unit");
        assert!(none.is_none(), "{name}: the separate link read no unit");
        assert_eq!(
            (ran.status.code(), &ran.stdout, &ran.stderr),
            (expected.status.code(), &expected.stdout, &expected.stderr),
            "{name}: what the program does and reports"
        );
        assert_eq!(unit_functions(&unit), unit_functions(&separate), "{name}: functions");
        assert_eq!(
            crate::native_debug::source_positions(&unit).unwrap(),
            crate::native_debug::source_positions(&separate).unwrap(),
            "{name}: debug line rows"
        );
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn image_unit_links_what_its_objects_link_cranelift() { image_unit_links_what_its_objects_link(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn image_unit_links_what_its_objects_link_llvm() { image_unit_links_what_its_objects_link(Backend::Llvm); }

/// A unit whose bytes fail their check, or a valid unit's frame under another
/// unit's key, is a miss: the link prelinks the unit again and links the
/// executable it linked first. Mutant: a unit read without its check, red on
/// the damaged unit.
#[test]
#[ignore = "explicit native integration target"]
fn a_damaged_or_misplaced_image_unit_is_prelinked_again() {
    let dir = tempfile::tempdir().unwrap();
    let build = |project: &str, source: &str| {
        unit_build(dir.path(), Backend::Cranelift, project, source, "cache", false)
    };
    let (bounds, strings) = (UNIT_PROGRAMS[0].1, UNIT_PROGRAMS[2].1);
    let (first, _, first_unit) = build("bounds", bounds);
    let first_unit = first_unit.unwrap();
    let (other, _, other_unit) = build("strings", strings);
    let other_unit = other_unit.unwrap();
    assert_ne!(first_unit.key, other_unit.key, "two image object sets, two units");

    // One flipped byte inside the unit's text.
    let mut bytes = std::fs::read(&first_unit.blob).unwrap();
    bytes[0x50] ^= 0x01;
    std::fs::write(&first_unit.blob, &bytes).unwrap();
    let (again, ran, unit) = build("bounds-damaged", bounds);
    assert!(unit.is_some_and(|unit| unit.key == first_unit.key && unit.prelinked), "a damaged unit is prelinked again");
    assert!(again == first, "the damaged unit's program links its first executable");
    assert_eq!(ran.status.code(), Some(101), "{ran:?}");

    // The first unit's record placed under the second's key.
    std::fs::copy(&first_unit.record, &other_unit.record).unwrap();
    let (again, ran, unit) = build("strings-misplaced", strings);
    assert!(unit.is_some_and(|unit| unit.key == other_unit.key && unit.prelinked), "a misplaced unit is a miss");
    assert!(again == other, "the misplaced unit's program links its first executable");
    assert_eq!((ran.status.code(), ran.stdout.as_slice()), (Some(0), &b"right"[..]), "{ran:?}");
}

/// One build of the strings program through the C driver `TOOL_ENV_DRIVER`,
/// in `TOOL_ENV_DIR`, under this process's environment: its executable to
/// `<step>.exe`, and the unit the link read with whether it prelinked it to
/// `<step>.unit` (empty when the link was reused).
#[test]
#[ignore = "explicit native integration target"]
fn tool_environment_child() {
    let Some(dir) = std::env::var_os("TOOL_ENV_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let step = std::env::var("TOOL_ENV_STEP").unwrap();
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("main.hirsel"), UNIT_PROGRAMS[2].1).unwrap();
    let request = CheckRequest::program(&project, &libraries, &project.join("store")).unwrap();
    let product = Driver::default()
        .check_reachable_with_product(request, &dir.join("cache"), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?}", product.diagnostics);
    let mut options = native_options(Backend::Cranelift, &project);
    options.link = Link::Driver(PathBuf::from(std::env::var_os("TOOL_ENV_DRIVER").unwrap()));
    options.grant_mask = 1 << 4;
    UNIT.set(None);
    let built = product
        .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
        .unwrap();
    std::fs::copy(&built.executable, dir.join(format!("{step}.exe"))).unwrap();
    // A reused executable reads no unit: an empty file.
    let read = UNIT.take().map_or_else(Vec::new, |unit| {
        let mut read = std::fs::read(unit.blob).unwrap();
        read.push(u8::from(unit.prelinked));
        read
    });
    std::fs::write(dir.join(format!("{step}.unit")), read).unwrap();
}

/// Law: a child tool observes only its argv, the files it reads and the
/// environment the compiler gives it, and that environment is in the key
/// of the capture it makes. Oracle: a fresh store, image cache and recipe
/// directory under the second request's environment.
///
/// The C driver is the review's wrapper (step-c-review-1029 recipe_env.py):
/// it passes `--defsym=review_marker=${REVIEW_MARK:-1}` to the link, and
/// names a link editor that logs its environment, as the driver does.
/// 1. Primed under REVIEW_MARK=1, rebuilt under REVIEW_MARK=2: the
///    executable equals a fresh build's under REVIEW_MARK=2 (finding 1 of
///    the review: reuse gave 1 and fresh 2).
/// 2. REVIEW_MARK is on no list: the rebuild runs no driver, and no tool
///    ever sees a variable off its list.
/// 3. The image unit (`ld -r`) prelinked fresh under REVIEW_MARK=2 equals
///    the one primed under REVIEW_MARK=1.
/// 4. A request that differs only in `PATH`, a listed variable, captures
///    again, its driver sees that PATH, and it links a fresh build's
///    executable.
///
/// Mutant: a child that inherits this process's environment, red at 1.
#[test]
#[ignore = "explicit native integration target"]
fn a_child_tool_sees_only_its_listed_environment() {
    let work = tempfile::tempdir().unwrap();
    let dir = work.path();
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let bin = dir.join("tree/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let (driver, editor) = (bin.join("clang"), bin.join("ld.lld"));
    let (driver_log, editor_log) = (dir.join("driver-env"), dir.join("editor-env"));
    let script = |path: &Path, body: String| {
        crate::executable::write(path, format!("#!/bin/sh\n/usr/bin/env >> '{body}").as_bytes(), 0o755)
            .unwrap()
    };
    script(
        &driver,
        format!(
            "{}'\necho >> '{}'\nexec '{}' --ld-path='{}' \"$@\" \"-Wl,--defsym=review_marker=${{REVIEW_MARK:-1}}\"\n",
            driver_log.display(),
            driver_log.display(),
            llvm.join("bin/clang").display(),
            editor.display()
        ),
    );
    script(
        &editor,
        format!(
            "{}'\necho >> '{}'\nexec '{}' \"$@\"\n",
            editor_log.display(),
            editor_log.display(),
            llvm.join("bin/ld.lld").display()
        ),
    );
    let path = std::env::var_os("PATH").unwrap();
    let build = |step: &str, mark: &str, path: &std::ffi::OsStr| {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "native::tests::tool_environment_child"])
            .env("TOOL_ENV_DIR", dir)
            .env("TOOL_ENV_STEP", step)
            .env("TOOL_ENV_DRIVER", &driver)
            .env("XDG_CACHE_HOME", dir.join("xdg"))
            .env("REVIEW_MARK", mark)
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(output.status.success(), "{step}: {}", String::from_utf8_lossy(&output.stdout));
        let read = |suffix| std::fs::read(dir.join(format!("{step}.{suffix}"))).unwrap();
        let mut unit = read("unit");
        let prelinked = unit.pop() == Some(1);
        (read("exe"), unit, prelinked)
    };
    // Each environment block a tool logged, as its variable names.
    let blocks = |log: &Path| -> Vec<BTreeSet<String>> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .split("\n\n")
            .filter(|block| !block.trim().is_empty())
            .map(|block| block.lines().filter_map(|l| l.split_once('=')).map(|(n, _)| n.to_owned()).collect())
            .collect()
    };
    let fresh = || {
        for stale in ["project", "cache", "xdg"] {
            std::fs::remove_dir_all(dir.join(stale)).unwrap();
        }
    };

    let (primed, primed_unit, _) = build("primed", "1", &path);
    let driver_runs = blocks(&driver_log).len();
    let (rebuilt, _, _) = build("rebuilt", "2", &path);
    assert_eq!(blocks(&driver_log).len(), driver_runs, "2: a variable off the list captures nothing again");
    fresh();
    let (oracle, oracle_unit, prelinked) = build("fresh", "2", &path);
    assert!(rebuilt == oracle, "1: the rebuilt executable is a fresh build's");
    assert!(rebuilt == primed, "1: REVIEW_MARK reaches no tool");
    assert!(prelinked, "3: the fresh build prelinks its unit");
    assert!(primed_unit == oracle_unit, "3: the unit prelinked under another variable is the primed one");
    let shell = BTreeSet::from(["PWD".to_owned()]);
    let listed: BTreeSet<String> =
        ["PATH", "LD_LIBRARY_PATH", "PWD"].into_iter().map(str::to_owned).collect();
    let (drivers, editors) = (blocks(&driver_log), blocks(&editor_log));
    assert!(!drivers.is_empty() && editors.len() >= 2, "every capture, prelink and link logged");
    for names in &drivers {
        assert!(names.is_subset(&listed), "2: a driver saw {names:?}");
    }
    for names in &editors {
        assert!(names.is_subset(&shell), "2: a link editor saw {names:?}");
    }

    let other = std::env::join_paths(
        std::iter::once(dir.join("empty")).chain(std::env::split_paths(&path)),
    )
    .unwrap();
    std::fs::create_dir(dir.join("empty")).unwrap();
    let (moved, _, _) = build("moved", "2", &other);
    let runs = std::fs::read_to_string(&driver_log).unwrap();
    let last = runs.trim_end().rsplit("\n\n").next().unwrap();
    assert!(blocks(&driver_log).len() > drivers.len(), "4: another PATH captures again");
    assert!(
        last.lines().any(|line| line.strip_prefix("PATH=").map(std::ffi::OsStr::new) == Some(other.as_os_str())),
        "4: the driver runs under the request's PATH"
    );
    assert!(moved == oracle, "4: the executable is a fresh build's");
}

#[test]
#[ignore = "explicit native integration target"]
fn run_and_build_share_the_release_unit() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(
        app.join("main.hirsel"),
        "pub fn main() -> Result[(), Exit] { Error(Exit(status: 7)) }\n",
    )
    .unwrap();
    let options = native_options(Backend::Llvm, dir.path());
    let runtime = options.runtime.canonicalize().unwrap();
    let clang = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang");
    let libraries = core_sources().parent().unwrap().canonicalize().unwrap();
    let cache = dir.path().join("cache");
    let cas = cache.join("hirsel/cas");
    let metrics = dir.path().join("metrics.json");
    let store = dir.path().join("store");
    let invoke = |command, expected_exit, stage| {
        let arguments = [
            command,
            app.to_str().unwrap(),
            "--release",
            "--limit",
            "67108864",
            "--source-core",
            "--libraries",
            libraries.to_str().unwrap(),
            "--runtime",
            runtime.to_str().unwrap(),
            "--cc",
            clang.to_str().unwrap(),
            "--store",
            store.to_str().unwrap(),
            "--metrics",
            metrics.to_str().unwrap(),
        ]
        .map(str::to_owned);
        let mut request = process::tape_request_command(&arguments, dir.path());
        request.env("XDG_CACHE_HOME", &cache);
        let (status, _, stderr) = process::tape_request_output(request);
        assert_eq!(status, Some(expected_exit), "{command}: {stderr}");
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&metrics).unwrap()).unwrap();
        assert!(
            report["stages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|s| s["name"] == stage),
            "{command}: expected {stage} in {report}"
        );
        let store = Store::cas(&cas).unwrap();
        crate::store::collect::records(&cas)
            .into_iter()
            .map(|(_, key)| key)
            .filter(|&key| {
                store
                    .get(key)
                    .unwrap()
                    .is_some_and(|p| super::release::is_unit_record(&p))
            })
            .collect::<BTreeSet<_>>()
    };
    let built = invoke("build", 0, "build store/miss");
    assert!(!built.is_empty(), "build published release units");
    assert_eq!(invoke("run", 7, "build store/hit"), built);
}
