// The reach record (hirsel.core-reach.v2). Law: a build in a primed store is
// the build of the same sources and options into an empty store: the same
// executable bytes and exit status. On a no-change request it walks nothing
// (no instance on the M2 path, no object checked, no link); on a one-body edit
// only the edited instance takes the M2 path. Oracle for every case: a fresh
// store's build, and the hand-derived exit and compiled count of each step.

/// One build of a step: the product, the executable's bytes and its status.
fn reach_build(product: &CheckProduct, options: &NativeOptions) -> (BuildProduct, Vec<u8>, Option<i32>) {
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, options).unwrap();
    let status = Command::new(&built.executable).status().unwrap().code();
    let bytes = std::fs::read(&built.executable).unwrap();
    (built, bytes, status)
}
/// A step: the app source, its exit, and its work in the primed store:
/// instances compiled and instances on the M2 path (`None`: not asserted).
struct Step<'a> {
    app: &'a str,
    exit: i32,
    work: Option<(usize, u64)>,
}
/// Build each step in one store against `core`, each against an empty
/// store, and each step again; the work counters of each repeat are a
/// no-change request's.
fn reach_steps(name: &str, backend: Backend, core: &Path, steps: &[Step]) {
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(backend, dir.path());
    for (n, step) in steps.iter().enumerate() {
        let product = record_check(step.app, core, dir.path(), "store");
        let (built, bytes, status) = reach_build(&product, &options);
        let fresh = reach_build(
            &record_check(step.app, core, dir.path(), &format!("fresh{n}")),
            &NativeOptions {
                store: dir.path().join(format!("fresh-native{n}")),
                output: dir.path().join(format!("fresh-program{n}")),
                ..native_options(backend, dir.path())
            },
        );
        assert_eq!(status, Some(step.exit), "{name} step {n}: exit");
        assert!(bytes == fresh.1, "{name} step {n}: the primed store's executable is a fresh store's");
        if let Some((compiled, path)) = step.work {
            assert_eq!(built.compiled, compiled, "{name} step {n}: compiled");
            if matches!(backend, Backend::Cranelift) {
                assert_eq!(built.reach.instance_path, path, "{name} step {n}: M2 path");
            }
        }
        let (again, again_bytes, _) = reach_build(&product, &options);
        assert!(again_bytes == bytes, "{name} step {n}: repeat");
        assert_eq!((again.compiled, again.linked), (0, false), "{name} step {n}: a repeat is a hit");
        if matches!(backend, Backend::Cranelift) {
            assert_eq!(
                (again.reach.instance_path, again.reach.objects_verified),
                (0, 0),
                "{name} step {n}: a no-change request walks and checks nothing"
            );
        }
    }
}
macro_rules! step {
    ($app:expr, $exit:expr) => {
        Step { app: $app, exit: $exit, work: None }
    };
    ($app:expr, $exit:expr, $compiled:expr, $path:expr) => {
        Step { app: $app, exit: $exit, work: Some(($compiled, $path)) }
    };
}
/// The edit kinds, each a sequence over one store (cases R1-R3, R7, R10,
/// R11, R13, R14 and the shadowing name).
fn reach_edit_kinds(backend: Backend) {
    let core = core_sources();
    const DEEP: &str = "fn c() -> u8 { 7u8 }\nfn b() -> u8 { c() }\nfn a() -> u8 { b() }\npub fn main() -> Result[(), Exit] { Error(Exit(status: a())) }\n";
    // R1: a deep leaf edit compiles the leaf alone.
    reach_steps("R1 deep leaf", backend, &core, &[
        step!(DEEP, 7),
        step!(&DEEP.replace("{ 7u8 }", "{ 9u8 }"), 9, 1, 1),
    ]);
    // R2: a call added, then removed; the removed callee leaves the link.
    const ONE: &str = "fn f() -> u8 { 3u8 }\nfn g() -> u8 { 4u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: f())) }\n";
    reach_steps("R2 calls", backend, &core, &[
        step!(ONE, 3),
        step!(&ONE.replace("status: f()", "status: f() + g()"), 7, 2, 2),
        step!(ONE, 3, 0, 1),
    ]);
    // R3: an unused constant is checked (LANG-20), so it enters the record.
    const CONSTANT: &str = "pub fn main() -> Result[(), Exit] { Error(Exit(status: 5u8)) }\n";
    reach_steps("R3 constant", backend, &core, &[
        step!(CONSTANT, 5),
        step!(&format!("{CONSTANT}const UNUSED: u8 = 9u8\n"), 5),
        step!(CONSTANT, 5),
    ]);
    // R7: a header-only change: an unreached function's signature.
    const HEADER: &str = "fn spare(x: u8) -> u8 { x }\npub fn main() -> Result[(), Exit] { Error(Exit(status: 6u8)) }\n";
    reach_steps("R7 header", backend, &core, &[
        step!(HEADER, 6),
        step!(&HEADER.replace("fn spare(x: u8) -> u8 { x }", "fn spare(x: u16) -> u16 { x }"), 6, 0, 0),
    ]);
    // R10: two programs alternate in one store under one entry path. The
    // record is the entry's, so each takes the other's: every row differs,
    // each instance walks its record (DEEP: main, a, b, c; OTHER: main, k),
    // and none compiles again.
    const OTHER: &str = "fn k() -> u8 { 2u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: k())) }\n";
    reach_steps("R10 two programs", backend, &core, &[
        step!(DEEP, 7),
        step!(OTHER, 2),
        step!(DEEP, 7, 0, 4),
        step!(OTHER, 2, 0, 2),
    ]);
    // R11: glue (a generic instance) reached through a changed parent.
    const GLUE: &str = "fn id[T](take x: T) -> T { x }\nfn p() -> u8 { id(4u8) }\npub fn main() -> Result[(), Exit] { Error(Exit(status: p())) }\n";
    reach_steps("R11 glue", backend, &core, &[
        step!(GLUE, 4),
        step!(&GLUE.replace("id(4u8)", "id(5u8)"), 5, 1, 1),
        step!(&GLUE.replace("id(4u8)", "id(id(5u8))"), 5, 1, 1),
    ]);
    // R13: SourceMap-only movement: trailing lines move no row, one relink.
    reach_steps("R13 trailing lines", backend, &core, &[
        step!(DEEP, 7),
        step!(&format!("{DEEP}\n\n\n"), 7, 0, 0),
    ]);
    // R14: two calls swapped in the entry's body: the link order is fresh's.
    const ORDER: &str = "fn x() -> u8 { 1u8 }\nfn y() -> u8 { 2u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: x() + y())) }\n";
    reach_steps("R14 order", backend, &core, &[
        step!(ORDER, 3),
        step!(&ORDER.replace("x() + y()", "y() + x()"), 3, 1, 1),
    ]);
}
#[test]
#[ignore = "explicit native integration target"]
fn reach_edit_kinds_cranelift() {
    reach_edit_kinds(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn reach_edit_kinds_llvm() {
    reach_edit_kinds(Backend::Llvm);
}

/// A core whose prelude adds `exit_with`, returning `status + bump`.
fn reach_core(dir: &Path, name: &str, bump: u8) -> PathBuf {
    let core = dir.join(name);
    copy_tree(&core_sources(), &core);
    let prelude = std::fs::read_to_string(core.join("core.hirsel")).unwrap();
    std::fs::write(
        core.join("core.hirsel"),
        format!("{prelude}\npub fn exit_with(status: u8) -> Exit {{ Exit(status: status + {bump}u8) }}\n"),
    )
    .unwrap();
    core
}
/// Two cores: one app built against two core sources in one store,
/// alternately; and a name the app declares that shadows the core's.
#[test]
#[ignore = "explicit native integration target"]
fn reach_two_images_and_a_shadowing_name() {
    const APP: &str = "pub fn main() -> Result[(), Exit] { Error(exit_with(7u8)) }\n";
    let dir = tempfile::tempdir().unwrap();
    let [a, b] = [("core-a", 0), ("core-b", 1)].map(|(name, bump)| reach_core(dir.path(), name, bump));
    let options = native_options(Backend::Cranelift, dir.path());
    for (n, (core, exit)) in [(&a, 7), (&b, 8), (&a, 7), (&b, 8)].into_iter().enumerate() {
        let (built, bytes, status) = reach_build(&record_check(APP, core, dir.path(), "store"), &options);
        let fresh = reach_build(
            &record_check(APP, core, dir.path(), &format!("fresh{n}")),
            &NativeOptions {
                store: dir.path().join(format!("fresh-native{n}")),
                output: dir.path().join(format!("fresh-program{n}")),
                ..native_options(Backend::Cranelift, dir.path())
            },
        );
        assert_eq!(status, Some(exit), "core {n}");
        assert!(bytes == fresh.1, "core {n}: a fresh store's executable");
        if n >= 1 {
            // The cores differ in one body: its instance alone walks, a hit
            // after the first time each core is built.
            let compiled = usize::from(n == 1);
            assert_eq!((built.compiled, built.reach.instance_path), (compiled, 1), "core {n}");
        }
    }
    let shadow = format!("{APP}fn exit_with(status: u8) -> Exit {{ Exit(status: status + 2u8) }}\n");
    reach_steps("shadowing name", Backend::Cranelift, &a, &[
        step!(APP, 7),
        step!(&shadow, 9),
        step!(APP, 7),
    ]);
}

/// The reach records a native store holds: files named by an address whose
/// checked record decodes as one.
fn reach_records(root: &Path) -> Vec<PathBuf> {
    let store = Store::new(root).unwrap();
    std::fs::read_dir(root)
        .unwrap()
        .filter_map(|e| {
            let path = e.ok()?.path();
            let name = path.file_name()?.to_str()?;
            let bytes = (0..32)
                .map(|i| u8::from_str_radix(name.get(i * 2..i * 2 + 2)?, 16).ok())
                .collect::<Option<Vec<u8>>>()?;
            let record = store.get(ContentHash(bytes.try_into().ok()?)).ok()??;
            crate::reach::decode_record(&record)
                .map(|_| path)
        })
        .collect()
}
const REACH_APP: &str = "fn get(x: u8) -> u8 { x + 1u8 }\nfn base() -> u8 { 5u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: get(base()))) }\n";

/// R8: a damaged reach record is a miss: the build walks again, equals a
/// fresh store's, and writes a record the next request uses.
#[test]
#[ignore = "explicit native integration target"]
fn reach_damaged_record_walks_again() {
    let core = core_sources();
    for how in [Damage::Middle, Damage::Truncate] {
        let dir = tempfile::tempdir().unwrap();
        let options = native_options(Backend::Cranelift, dir.path());
        let product = record_check(REACH_APP, &core, dir.path(), "store");
        let (_, first, _) = reach_build(&product, &options);
        let records = reach_records(&options.store);
        assert_eq!(records.len(), 1, "{how:?}: one reach record");
        damage(&records[0], how);
        let (built, bytes, status) = reach_build(&product, &options);
        assert_eq!(status, Some(6), "{how:?}");
        assert!(bytes == first, "{how:?}: the first build's executable");
        assert!(built.reach.instance_path > 0, "{how:?}: the damaged record is not used");
        let (again, _, _) = reach_build(&product, &options);
        assert_eq!(again.reach.instance_path, 0, "{how:?}: the new record is used");
    }
}

/// R12 (replace): every stored object is replaced by another valid object,
/// then one body is edited: each replaced object fails its check, is
/// rebuilt, and the executable is a fresh store's. Of the n stored objects
/// the n - 1 not of `base` are linked: the first link checks and refuses
/// each, and one retry rebuilds them all, so the request compiles n (the
/// edited `base` and each refused object once) and checks n objects (the
/// n - 1, and `base`'s, which the retry reuses from its record). Bug caught (red at d0d1ee55): the link stopped at the first
/// refusal and each retry rebuilt every instance refused so far, compiling
/// 1 + 2 + ... and dropping the earlier attempts' counts.
#[test]
#[ignore = "explicit native integration target"]
fn reach_replaced_objects_build_a_fresh_stores_executable() {
    let core = core_sources();
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(Backend::Cranelift, dir.path());
    reach_build(&record_check(REACH_APP, &core, dir.path(), "store"), &options);
    let objects = stored_objects(&options.store);
    assert!(objects.len() >= 2);
    let bytes: Vec<_> = objects.iter().map(|p| std::fs::read(p).unwrap()).collect();
    for (i, object) in objects.iter().enumerate() {
        std::fs::write(object, &bytes[(i + 1) % bytes.len()]).unwrap();
    }
    let edited = REACH_APP.replace("{ 5u8 }", "{ 6u8 }");
    let (built, rebuilt, status) = reach_build(&record_check(&edited, &core, dir.path(), "store"), &options);
    let fresh = reach_build(
        &record_check(&edited, &core, dir.path(), "fresh"),
        &NativeOptions {
            store: dir.path().join("fresh-native"),
            output: dir.path().join("fresh-program"),
            ..native_options(Backend::Cranelift, dir.path())
        },
    );
    assert_eq!(status, Some(7));
    assert!(rebuilt == fresh.1, "a fresh store's executable");
    let n = objects.len();
    assert_eq!(
        (built.compiled, built.reach.objects_refused, built.reach.objects_verified),
        (n, n as u64 - 1, n as u64),
        "compiled, refused and checked objects"
    );
}

/// R12b: after the link checks the objects, another valid object is renamed
/// over each checked path; the linker reads the checked inodes, so the
/// executable is a fresh store's.
#[test]
#[ignore = "explicit native integration target"]
fn reach_inode_swap_after_the_check_links_the_checked_object() {
    let core = core_sources();
    let dir = tempfile::tempdir().unwrap();
    let options = native_options(Backend::Cranelift, dir.path());
    reach_build(&record_check(REACH_APP, &core, dir.path(), "store"), &options);
    let swap = dir.path().join("swap");
    AFTER_CHECK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move |paths: &[PathBuf]| {
            let bytes: Vec<_> = paths.iter().map(|p| std::fs::read(p).unwrap()).collect();
            for (i, path) in paths.iter().enumerate() {
                std::fs::write(&swap, &bytes[(i + 1) % bytes.len()]).unwrap();
                std::fs::rename(&swap, path).unwrap();
            }
        }));
    });
    let edited = REACH_APP.replace("{ 5u8 }", "{ 6u8 }");
    let (built, swapped, status) = reach_build(&record_check(&edited, &core, dir.path(), "store"), &options);
    AFTER_CHECK.with(|hook| *hook.borrow_mut() = None);
    assert!(built.linked && built.reach.objects_verified > 0, "the link checked stored objects");
    let fresh = reach_build(
        &record_check(&edited, &core, dir.path(), "fresh"),
        &NativeOptions {
            store: dir.path().join("fresh-native"),
            output: dir.path().join("fresh-program"),
            ..native_options(Backend::Cranelift, dir.path())
        },
    );
    assert_eq!(status, Some(7));
    assert!(swapped == fresh.1, "the checked inodes were linked");
}

/// R12c: between emission and the link's first open, a valid object of the
/// same program with `leaf` returning 42 is renamed over the stored object
/// the build will link (a cached one on a development engine, the program's
/// one object on release). On every engine and mode the linker reads the
/// bytes the build checked or wrote, so the executable is a fresh store's,
/// and so is the next request's: no wrong executable was published under the
/// right objects. Bug caught (red at f488cd81): LLVM checked a cached object
/// in `native_object::load` and the link opened its path again unchecked,
/// linking the swapped inode (exit 42) and caching that executable.
#[test]
#[ignore = "explicit native integration target"]
fn reach_object_swapped_before_the_link_opens_it_links_the_checked_bytes() {
    const A: &str = "fn leaf() -> u8 { 41u8 }\npub fn main() -> Result[(), Exit] { Error(Exit(status: leaf())) }\n";
    let core = core_sources();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let leaf = DefPath::from_full("app@1/main/leaf").unwrap();
    for (backend, optimize) in [(Backend::Cranelift, false), (Backend::Llvm, false), (Backend::Llvm, true)] {
        let how = format!("{backend:?} optimize={optimize}");
        let dir = tempfile::tempdir().unwrap();
        let base = || NativeOptions { optimize, ..native_options(backend, dir.path()) };
        let options = base();
        let at = |name: &str| NativeOptions {
            store: dir.path().join(format!("{name}-native")),
            output: dir.path().join(format!("{name}-program")),
            ..base()
        };
        let target = |objects: Vec<ObjectProduct>| {
            let mut objects = objects.into_iter();
            if optimize {
                objects.next().unwrap()
            } else {
                objects.find(|o| o.instance.definition == leaf).unwrap()
            }
        };
        assert_eq!(reach_build(&record_check(A, &core, dir.path(), "store"), &options).2, Some(41), "{how}: prime");
        // B forces a link; its target object is stored before the build.
        let source = format!("{A}\n");
        let b = record_check(&source, &core, dir.path(), "store");
        let path = target(b.emit_reachable(&entry, &options).unwrap()).path;
        let donor = record_check(&A.replace("41u8", "42u8"), &core, dir.path(), "donor");
        let donor = std::fs::read(target(donor.emit_reachable(&entry, &at("donor")).unwrap()).path).unwrap();
        let swap = dir.path().join("swap");
        let mut once = true;
        BEFORE_OPEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |paths: &[PathBuf]| {
                if once && paths.contains(&path) {
                    once = false;
                    std::fs::write(&swap, &donor).unwrap();
                    std::fs::rename(&swap, &path).unwrap();
                }
            }))
        });
        let (_, swapped, status) = reach_build(&b, &options);
        BEFORE_OPEN.with(|hook| *hook.borrow_mut() = None);
        let fresh = reach_build(&record_check(&source, &core, dir.path(), "fresh"), &at("fresh"));
        assert_eq!(fresh.2, Some(41), "{how}: fresh");
        assert_eq!(status, Some(41), "{how}: the checked bytes were linked");
        assert!(swapped == fresh.1, "{how}: a fresh store's executable");
        let (_, again, status) = reach_build(&b, &options);
        assert_eq!(status, Some(41), "{how}: next request");
        assert!(again == fresh.1, "{how}: the next request's executable is a fresh store's");
    }
}

/// Review 1012: builds on parallel threads of one process, each executed at
/// once, are launched on every engine and mode. A writable descriptor of one
/// thread's executable copied into another thread's fork made the launch fail
/// with ETXTBSY (45 of 128 on Cranelift before the fix). Oracle: PROG-3, exit
/// 19 for `Error(Exit(status: 19))`.
#[test]
#[ignore = "explicit native integration target"]
fn parallel_builds_launch_at_once() {
    const APP: &str = "fn leaf()->u8{19u8}\npub fn main()->Result[(),Exit]{Error(Exit(status:leaf()))}\n";
    let core = core_sources();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    for (backend, optimize) in [(Backend::Cranelift, false), (Backend::Llvm, false), (Backend::Llvm, true)] {
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(8);
        let refused: usize = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|thread| {
                    let (dir, core, entry, barrier) = (dir.path(), &core, &entry, &barrier);
                    scope.spawn(move || {
                        let here = dir.join(format!("thread-{thread}"));
                        std::fs::create_dir_all(&here).unwrap();
                        let product = record_check(APP, core, &here, "check");
                        let mut options = NativeOptions {
                            store: dir.join("shared-native"),
                            optimize,
                            ..native_options(backend, &here)
                        };
                        barrier.wait();
                        let mut refused = 0;
                        for n in 0..16 {
                            options.output = here.join(format!("program-{n}"));
                            let built = product.build(entry, &options).unwrap();
                            match Command::new(&built.executable).status() {
                                Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) => refused += 1,
                                status => assert_eq!(status.unwrap().code(), Some(19)),
                            }
                        }
                        refused
                    })
                })
                .collect();
            threads.into_iter().map(|t| t.join().unwrap()).sum()
        });
        assert_eq!(refused, 0, "{backend:?} release={optimize}: launches refused with ETXTBSY");
    }
}

/// Build `files` (name, source) as project `project` over std imaged into
/// `dir/cache`, with debug lines, into `store` on `backend`: the executable's
/// bytes and its run's exit and stderr.
fn reach_files_build(dir: &Path, project: &str, files: &[(&str, &str)], store: &str, backend: Backend) -> (Vec<u8>, (Option<i32>, Vec<u8>)) {
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let project = dir.join(project);
    if project.exists() {
        std::fs::remove_dir_all(&project).unwrap();
    }
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    for (name, source) in files {
        std::fs::write(project.join(name), source).unwrap();
    }
    let request = CheckRequest::program(&project, &libraries, &dir.join(store).join("check")).unwrap();
    let product = Driver::default()
        .check_reachable_with_product(request, &dir.join("cache"), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let mut options = native_options(backend, dir);
    options.store = dir.join(store).join("native");
    options.output = dir.join(store).join("program");
    options.debug_lines = true;
    options.grant_mask = 1 << 4;
    let built = product.build(&DefPath::from_full("app@1/main/main").unwrap(), &options).unwrap();
    let output = Command::new(&built.executable).output().unwrap();
    (std::fs::read(&built.executable).unwrap(), (output.status.code(), output.stderr))
}

/// A link's source places (each reached item's file and offset) are the
/// current sources', whichever the record that a primed store holds says:
/// the record's place of an item in an image file is borrowed, the rest found
/// again. Each step is built in one primed store and in an empty one, with
/// debug lines, over a program in three files whose failure report names its
/// line: blank lines above a reached item (its row holds, its place moves), an
/// edit in a file the link reaches nothing in, a file added ahead of the
/// others, an item added above a reached one, and an item moved between
/// files. Oracle: the empty store's executable. Mutant: every recorded place
/// borrowed, red (the blank lines move the reported line).
#[test]
#[ignore = "explicit native integration target"]
fn reach_source_places_are_the_current_sources() {
    const MAIN: &str = "pub fn main() -> Result[(), Exit] {\n    if string.starts_with(\"ab\", \"a\") && other.f(200u8) > 0u8 { Ok(()) } else { Error(Exit(status: 3)) }\n}\n";
    const OTHER: &str = "pub fn f(x: u8) -> u8 {\n    x + 100u8\n}\n";
    const SPARE: &str = "pub fn unused() -> u8 { 1u8 }\n";
    let moved_main = "pub fn main() -> Result[(), Exit] {\n    if string.starts_with(\"ab\", \"a\") && f(200u8) > 0u8 { Ok(()) } else { Error(Exit(status: 3)) }\n}\nfn f(x: u8) -> u8 {\n    x + 100u8\n}\n";
    let shifted = format!("pub fn g() -> u8 {{\n    2u8\n}}\n{OTHER}");
    let blank = format!("\n\n\n{OTHER}");
    let steps: [&[(&str, &str)]; 6] = [
        &[("main.hirsel", MAIN), ("other.hirsel", OTHER), ("spare.hirsel", SPARE)],
        &[("main.hirsel", MAIN), ("other.hirsel", &blank), ("spare.hirsel", SPARE)],
        &[("main.hirsel", MAIN), ("other.hirsel", OTHER), ("spare.hirsel", "\n\npub fn unused() -> u8 { 2u8 }\n")],
        &[("aaa.hirsel", SPARE), ("main.hirsel", MAIN), ("other.hirsel", OTHER), ("spare.hirsel", SPARE)],
        &[("aaa.hirsel", SPARE), ("main.hirsel", MAIN), ("other.hirsel", &shifted), ("spare.hirsel", SPARE)],
        &[("aaa.hirsel", SPARE), ("main.hirsel", moved_main), ("other.hirsel", SPARE), ("spare.hirsel", SPARE)],
    ];
    let lines = ["app@1/other.hirsel:2", "app@1/other.hirsel:5", "app@1/other.hirsel:2", "app@1/other.hirsel:2", "app@1/other.hirsel:5", "app@1/main.hirsel:5"];
    let dir = tempfile::tempdir().unwrap();
    for (n, files) in steps.iter().enumerate() {
        let (primed, ran) = reach_files_build(dir.path(), "app", files, "primed", Backend::Cranelift);
        let (fresh, expected) = reach_files_build(dir.path(), "app", files, &format!("fresh{n}"), Backend::Cranelift);
        assert_eq!(ran.0, Some(101), "step {n}: {:?}", String::from_utf8_lossy(&ran.1));
        assert!(String::from_utf8_lossy(&ran.1).contains(lines[n]), "step {n}: {}", String::from_utf8_lossy(&ran.1));
        assert_eq!(ran, expected, "step {n}: what the program reports");
        assert!(primed == fresh, "step {n}: the primed store's executable is a fresh store's");
    }
}

/// A generic body that names `code.identity` dispatches over every function
/// item of the program, so a store that built another program's function
/// table must not serve its body: a build in the primed store is the build of
/// the same files into an empty one. Steps over one store: a program with a
/// spare module; the spare module dropped (a served stale table reaches the
/// dropped item: OutsideReached); a new module whose function the entry
/// describes (a served stale table lacks it: the dispatch traps). Oracle: the
/// empty store's executable, and exit 0 (the identity matches). Mutant: the
/// key's function table left out (`DefinitionKey::functions` None), red.
#[test]
#[ignore = "explicit native integration target"]
fn reach_code_identity_dispatch_follows_the_function_table() {
    const NAMED: &str = "fn named[F](s: StaticFn[F], want: Bytes) -> bool {\n    code.identity(s) == want\n}\nfn double(x: u64) -> u64 {\n    x * 2\n}\n";
    let local = format!("{NAMED}pub fn main() -> Result[(), Exit] {{\n    if named(code.describe(double), code.identity(code.describe(double))) {{ Ok(()) }} else {{ Error(Exit(status: 3)) }}\n}}\n");
    let other = format!("{NAMED}pub fn main() -> Result[(), Exit] {{\n    if named(code.describe(other.pick), code.identity(code.describe(other.pick))) {{ Ok(()) }} else {{ Error(Exit(status: 3)) }}\n}}\n");
    let steps: [&[(&str, &str)]; 3] = [
        &[("main.hirsel", &local), ("spare.hirsel", "pub fn spare(x: u64) -> u64 { x }\n")],
        &[("main.hirsel", &local)],
        &[("main.hirsel", &other), ("other.hirsel", "pub fn pick(x: u64) -> u64 { x + 1 }\n")],
    ];
    for backend in [Backend::Cranelift, Backend::Llvm] {
        let dir = tempfile::tempdir().unwrap();
        for (n, files) in steps.iter().enumerate() {
            let case = format!("{backend:?} step {n}");
            let (primed, ran) = reach_files_build(dir.path(), "app", files, "primed", backend);
            let (fresh, expected) = reach_files_build(dir.path(), "app", files, &format!("fresh{n}"), backend);
            assert_eq!(expected.0, Some(0), "{case}: {}", String::from_utf8_lossy(&expected.1));
            assert_eq!(ran, expected, "{case}: what the program reports");
            assert!(primed == fresh, "{case}: the primed store's executable is a fresh store's");
        }
    }
}
