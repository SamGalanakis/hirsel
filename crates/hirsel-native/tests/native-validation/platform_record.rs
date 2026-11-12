use super::*;
use hirsel_engine_store::{Bytes, DirStore, EntryKey};
use std::process::Command;
use std::sync::Mutex;

fn spec(root: &Path) -> PlatformSpec {
    for file in ["ld.so", "audit.so", "rt/libhirsel_rt.a", "a.o", "b.o"] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, file).unwrap();
    }
    for tree in ["linker/bin", "kit", "floor"] {
        std::fs::create_dir_all(root.join(tree)).unwrap();
    }
    PlatformSpec {
        target: super::super::super::super::TargetSpec::x86_64_unknown_linux_gnu(),
        linker: root.join("linker"),
        kit: root.join("kit"),
        runtime_archive: root.join("rt/libhirsel_rt.a"),
        foreign: vec![root.join("a.o"), root.join("b.o")],
        loader: root.join("ld.so"),
        auditor: root.join("audit.so"),
        library_path: std::env::join_paths([root.join("floor"), root.join("absent/lib")]).unwrap(),
        emulator: None,
        timeout: Duration::from_secs(20),
        events_bytes: 64 << 10,
        runtime_limits: RuntimeLimits {
            aliases: 128,
            name_bytes: 64 << 10,
        },
        limits: CaptureLimits {
            archive_bytes: 256 << 20,
            files: 1024,
        },
        debug_tables: false,
    }
}

/// Bug: a selection the key omits reuses a platform captured for another
/// selection. Oracle: each field class below selects a different capture, so
/// each must name a different record.
#[test]
fn each_selected_field_class_names_another_record() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let base = spec(root);
    let mut variants: Vec<(&str, PlatformSpec)> = Vec::new();
    let mut vary = |name, change: &dyn Fn(&mut PlatformSpec)| {
        let mut variant = base.clone();
        change(&mut variant);
        variants.push((name, variant));
    };
    vary("target", &|s| {
        s.target = super::super::super::super::TargetSpec::aarch64_unknown_linux_gnu()
    });
    vary("debug tables", &|s| s.debug_tables = true);
    vary("capture limits", &|s| s.limits.files += 1);
    vary("runtime limits", &|s| s.runtime_limits.aliases += 1);
    vary("events bytes", &|s| s.events_bytes += 1);
    vary("timeout", &|s| s.timeout += Duration::from_secs(1));
    vary("linker root", &|s| s.linker = root.join("linker2"));
    vary("kit root", &|s| s.kit = root.join("kit2"));
    vary("loader", &|s| s.loader = root.join("audit.so"));
    vary("auditor", &|s| s.auditor = root.join("ld.so"));
    vary("emulator", &|s| s.emulator = Some(root.join("qemu")));
    vary("foreign order", &|s| s.foreign.reverse());
    vary("foreign duplicate", &|s| s.foreign.push(root.join("b.o")));
    vary("library path order", &|s| {
        s.library_path =
            std::env::join_paths([root.join("absent/lib"), root.join("floor")]).unwrap()
    });
    vary("library path duplicate", &|s| {
        s.library_path = std::env::join_paths([
            root.join("floor"),
            root.join("absent/lib"),
            root.join("floor"),
        ])
        .unwrap()
    });
    // The link names the archive as spelled, so one archive through a link
    // beside it is another selection.
    std::os::unix::fs::symlink(root.join("rt/libhirsel_rt.a"), root.join("rt/alias.a")).unwrap();
    vary("archive spelling", &|s| {
        s.runtime_archive = root.join("rt/alias.a")
    });
    let mut seen = BTreeMap::from([(base.key().unwrap(), "base")]);
    for (name, variant) in &variants {
        if let Some(earlier) = seen.insert(variant.key().unwrap(), name) {
            panic!("{name} names the same record as {earlier}");
        }
    }
    let unordered = base.key().unwrap();
    std::fs::write(root.join("rt/startup.order"), "").unwrap();
    assert_ne!(
        base.key().unwrap(),
        unordered,
        "startup.order presence selects another capture"
    );
    // A relative spelling selects the path under the working directory, so
    // it names that path's record, not one shared by every directory.
    let mut relative = base.clone();
    relative.loader = PathBuf::from("ld.so");
    let mut resolved = base.clone();
    resolved.loader = std::env::current_dir().unwrap().join("ld.so");
    assert_eq!(relative.key().unwrap(), resolved.key().unwrap());
}

/// Later than the coarse kernel clock that stamps files; the record's own
/// racy margin (two seconds) is what guarantees this outside a test.
fn tick() {
    std::thread::sleep(Duration::from_millis(50));
}

fn recorded(guard: &Guard) -> Vec<(String, Vec<u64>)> {
    guard
        .stamps
        .iter()
        .map(|(path, stamp)| (path.to_str().unwrap().to_owned(), stamp.to_vec()))
        .collect()
}

/// Bug: a hit after an in-place rewrite that restored size and mtime (a
/// backup tool, `touch -r`). Oracle: the bytes changed, so the record must
/// not be reused; only ctime shows it.
#[test]
fn a_rewrite_restoring_size_and_mtime_is_a_miss() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("ld.so");
    std::fs::write(&file, "one").unwrap();
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    let selection = Selection {
        files: vec![file.clone()],
        ..Selection::default()
    };
    let guard = recorded(&selection.guard(&[], &[]).unwrap());
    assert_eq!(changed(&guard, &[]), None);
    tick();
    std::fs::write(&file, "two").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(changed(&guard, &[]), Some(file.to_str().unwrap()));
}

/// Bug: the loader would now find a library in a library-path directory
/// that did not exist at capture, or a preferred DSO in a nested directory
/// of a guarded tree, and the record is reused anyway. Oracle: the search
/// result changes, so the record must miss.
#[test]
fn a_new_entry_on_a_search_path_is_a_miss() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let selected = spec(root);
    let tree = root.join("floor");
    std::fs::create_dir_all(tree.join("glibc-hwcaps/x86-64-v3")).unwrap();
    std::fs::write(tree.join("libc.so.6"), "libc").unwrap();
    let selection = Selection::resolve(&selected).unwrap();
    let guard = recorded(&selection.guard(&[], &[]).unwrap());
    assert_eq!(changed(&guard, &[]), None);
    tick();
    std::fs::write(tree.join("glibc-hwcaps/x86-64-v3/libc.so.6"), "v3").unwrap();
    assert_eq!(
        changed(&guard, &[]),
        Some(tree.join("glibc-hwcaps/x86-64-v3").to_str().unwrap())
    );
    // The absent library-path directory `absent/lib` is guarded by its
    // nearest existing ancestor.
    let guard = recorded(&selection.guard(&[], &[]).unwrap());
    tick();
    std::fs::create_dir(root.join("absent")).unwrap();
    assert_eq!(changed(&guard, &[]), Some(root.to_str().unwrap()));
}

/// Bug: a file rewritten while a capture child read it by path, then the
/// rewrite's stamps aged past the racy margin before the record was issued.
/// Oracle: the child may have read either version, so nothing is recorded.
#[test]
fn a_change_between_the_bracketing_stamps_records_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("libhirsel_rt.a");
    std::fs::write(&file, "one").unwrap();
    let selection = Selection {
        files: vec![file.clone()],
        ..Selection::default()
    };
    // A clock past the racy margin stands for stamps that have aged.
    let started = SystemTime::now() + Duration::from_secs(10);
    let before = selection.guard(&[], &[]).unwrap();
    assert_eq!(witnessed(&before, &before, started), Ok(()));
    tick();
    std::fs::write(&file, "two").unwrap();
    let after = selection.guard(&[], &[]).unwrap();
    assert!(witnessed(&before, &after, started).is_err());
    assert!(
        witnessed(&after, &after, SystemTime::now()).is_err(),
        "a stamp inside the racy margin is not a witness"
    );
}

fn input(name: &str) -> PathBuf {
    std::env::var_os(name).unwrap().into()
}
fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A store that lost some content until it is published again.
struct Lost<'a> {
    inner: &'a dyn ResultStore,
    lost: Mutex<BTreeSet<Reference>>,
}
impl ResultStore for Lost<'_> {
    fn admit_product_memory(&self, bytes: u64) -> Option<std::sync::Arc<dyn Send + Sync>> {
        self.inner.admit_product_memory(bytes)
    }
    fn retention_scope(&self) -> Option<Box<dyn Send + Sync>> {
        self.inner.retention_scope()
    }
    fn capability(&self) -> hirsel_engine_store::ProviderCapability {
        self.inner.capability()
    }
    fn get(&self, key: &EntryKey) -> Option<Bytes> {
        self.inner.get(key)
    }
    fn put(&self, key: &EntryKey, bytes: &[u8]) -> hirsel_engine_store::WriteOutcome {
        self.inner.put(key, bytes)
    }
    fn get_content(&self, reference: Reference) -> Option<Bytes> {
        (!self.lost.lock().unwrap().contains(&reference))
            .then(|| self.inner.get_content(reference))
            .flatten()
    }
    fn put_content(&self, domain: Domain, bytes: &[u8]) -> Option<Reference> {
        let reference = self.inner.put_content(domain, bytes)?;
        self.lost.lock().unwrap().remove(&reference);
        Some(reference)
    }
    fn name(&self) -> &'static str {
        "lost-platform-chunks"
    }
}

/// Bugs: a record reused after the forced-cold switch, after a day, or
/// after a failure it fed; a recovery that keeps output from a platform
/// other than the recorded one. Oracle: the cold capture of the same
/// selection, and the program it links.
#[test]
#[ignore = "requires platform-record-proof selected linker, kit, runtime and loaders"]
fn a_platform_record_is_reused_until_switched_off_aged_or_shown_wrong() {
    let directory = tempfile::tempdir().unwrap();
    let native = directory.path().join("native");
    // SAFETY: the proof runs this one case in its own process, before any
    // thread that reads the environment.
    unsafe {
        std::env::set_var("HIRSEL_NATIVE_CACHE", &native);
        std::env::remove_var("HIRSEL_PLATFORM_CAPTURE");
    };
    let store = DirStore::new(directory.path().join("products"));
    let cache = directory.path().join("products/platform-v1");
    let authority = registered(directory.path());
    let at = |cache: &Path, clock: &Clock| -> Records {
        Ok(RecordPlace {
            directory: cache.to_owned(),
            authority: authority.clone(),
            clock: clock.clone(),
        })
    };
    let llvm = input("LLVM_SYS_231_PREFIX");
    let target = super::super::super::super::TargetSpec::x86_64_unknown_linux_gnu();
    let floor = input("HIRSEL_GLIBC_FLOOR").join(&target.triple);
    let original = directory.path().join("floor");
    std::fs::create_dir_all(&original).unwrap();
    for entry in std::fs::read_dir(&floor).unwrap() {
        let entry = entry.unwrap();
        if entry.path().is_file() {
            std::fs::copy(entry.path(), original.join(entry.file_name())).unwrap();
        }
    }
    let auditor = directory.path().join("audit.so");
    std::fs::copy(
        input("HIRSEL_AUDITOR_PACKAGE").join(format!("{}.so", target.triple)),
        &auditor,
    )
    .unwrap();
    let archives = input("HIRSEL_RUNTIME_ARCHIVES").join(&target.triple);
    let runtime = directory.path().join("rt");
    std::fs::create_dir(&runtime).unwrap();
    for file in ["libhirsel_rt.a", "startup.order"] {
        std::fs::copy(archives.join("default").join(file), runtime.join(file)).unwrap();
    }
    let limits = CaptureLimits {
        archive_bytes: 256 << 20,
        files: 1024,
    };
    let runtime_limits = RuntimeLimits {
        aliases: 128,
        name_bytes: 64 << 10,
    };
    // Private copies: an executor's shared input inodes change ctime when
    // another action links them, which (rightly) records nothing.
    let packages = directory.path().join("packages");
    std::fs::create_dir(&packages).unwrap();
    for package in ["HIRSEL_CAPTURE_LINKER", "HIRSEL_LINK_KIT"] {
        run(Command::new("cp")
            .arg("-a")
            .arg(input(package))
            .arg(packages.join(package)));
    }
    let spec = PlatformSpec {
        target: target.clone(),
        linker: packages.join("HIRSEL_CAPTURE_LINKER"),
        kit: packages.join("HIRSEL_LINK_KIT"),
        runtime_archive: runtime.join("libhirsel_rt.a"),
        foreign: vec![],
        loader: original.join("ld-2.28.so"),
        auditor,
        library_path: original.clone().into_os_string(),
        emulator: None,
        timeout: Duration::from_secs(20),
        events_bytes: 64 << 10,
        runtime_limits,
        limits,
        debug_tables: false,
    };
    let source = directory.path().join("main.s");
    let main = directory.path().join("main.o");
    std::fs::write(&source, ".globl main\nmain:\n mov $42,%eax\n ret\n").unwrap();
    run(Command::new(llvm.join("bin/llvm-mc"))
        .args(["-triple=x86_64-unknown-linux-gnu", "-filetype=obj", "-o"])
        .arg(&main)
        .arg(&source));
    let link = |platform: &CapturedPlatform, store: &dyn ResultStore, name: &str| {
        let output = directory.path().join(name);
        std::fs::create_dir(&output).unwrap();
        let plan = platform.plan_inputs(store, &target, &output, &[main.clone()], None)?;
        let program = output.join("program");
        plan.capture(store, platform, limits)?
            .link(store, &program)?;
        platform.validate_image(
            store,
            &target,
            &program,
            None,
            Duration::from_secs(20),
            runtime_limits,
        )?;
        Ok::<_, String>(std::fs::read(program).unwrap())
    };
    // The files above were just written; a clock past the racy margin lets
    // their capture be recorded.
    let now = Clock::now().unwrap().after(Duration::from_secs(10));
    let select = |store: &dyn ResultStore, mode, now: &Clock| {
        spec.select_in(store, &at(&cache, now), mode).unwrap()
    };

    let cold = select(&store, CaptureMode::Record, &now);
    assert!(!cold.hit);
    let program = link(&cold.platform, &store, "cold").unwrap();
    crate::object_queries::forget_retained();
    let hit = select(&store, CaptureMode::Record, &now);
    assert!(
        hit.hit,
        "an unchanged selection reuses its record: {:?}",
        crate::runner::counts().platform_reasons
    );
    assert_eq!(hit.platform.identity, cold.platform.identity);
    assert_eq!(link(&hit.platform, &store, "hit").unwrap(), program);

    // Every build preset follows one rule: a valid record answers it. A
    // fresh cache is the byte oracle; observing lookup and actual linker
    // capture children distinguishes full verification from a record hit.
    // Bug: a preset (release, LTO, profiling) that verifies fully on every
    // build despite a record that witnessed its complete selection.
    let children = || crate::object_queries::capture::CAPTURE_CHILDREN.get();
    let options = |profile: crate::BuildProfile| {
        profile
            .options(
                crate::Backend::Llvm,
                target.clone(),
                crate::Validation::EveryStage,
            )
            .unwrap()
    };
    let fresh = directory.path().join("fresh-platform");
    let fresh_store = DirStore::new(directory.path().join("fresh-products"));
    let reference = spec
        .select_in(&fresh_store, &at(&fresh, &now), CaptureMode::Record)
        .unwrap();
    let reference_program = link(&reference.platform, &fresh_store, "fresh").unwrap();
    assert_eq!(program, reference_program);
    for (index, build) in [
        options(crate::BuildProfile::Debug),
        options(crate::BuildProfile::Default),
        options(crate::BuildProfile::Release),
        options(crate::BuildProfile::ReleaseLto),
        options(crate::BuildProfile::Profiling),
        crate::BuildProfile::run_options(
            crate::Backend::Llvm,
            target.clone(),
            crate::Validation::EveryStage,
        ),
        crate::BuildProfile::run_options(
            crate::Backend::Cranelift,
            target.clone(),
            crate::Validation::EveryStage,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let before = (RECORD_READS.get(), children());
        let reused = select(&store, CaptureMode::from_environment().unwrap(), &now);
        assert!(
            reused.hit,
            "{:?} {:?} reuses the record",
            build.opt, build.backend
        );
        assert_eq!(RECORD_READS.get(), before.0 + 1);
        assert_eq!(children(), before.1, "a hit runs no capture children");
        assert_eq!(
            link(&reused.platform, &store, &format!("preset-{index}")).unwrap(),
            reference_program
        );
    }
    // A selection handed ahead to a request (a program record lookup's, for
    // its miss) answers only that selection and capture mode, once. Bug: a
    // request with foreign inputs or another mode links against a platform
    // captured for another selection. Oracle: whether capture children run,
    // and the other selection's own cold capture.
    let empty_source = directory.path().join("empty.s");
    let empty = directory.path().join("empty.o");
    std::fs::write(&empty_source, "").unwrap();
    run(Command::new(llvm.join("bin/llvm-mc"))
        .args(["-triple=x86_64-unknown-linux-gnu", "-filetype=obj", "-o"])
        .arg(&empty)
        .arg(&empty_source));
    let handed = spec.hand_off(&store, CaptureMode::Cold).unwrap();
    let mut foreign = spec.clone();
    foreign.foreign = vec![empty];
    let own = foreign
        .select(&store, CaptureMode::Cold)
        .unwrap()
        .platform
        .identity;
    assert_ne!(own, handed.identity());
    let before = children();
    let other = foreign
        .select_handed(Some(&handed), &store, CaptureMode::Cold)
        .unwrap();
    assert!(children() > before, "foreign inputs capture for themselves");
    assert_eq!(other.platform.identity, own);
    // (This store has no registration, so it keeps no records: a Record
    // selection of its own captures.)
    let before = children();
    spec.select_handed(Some(&handed), &store, CaptureMode::Record)
        .unwrap();
    assert!(
        children() > before,
        "another capture mode selects for itself"
    );
    let before = children();
    let taken = spec
        .select_handed(Some(&handed), &store, CaptureMode::Cold)
        .unwrap();
    assert_eq!(children(), before, "the handed selection runs no capture");
    assert_eq!(taken.platform.identity, handed.identity());
    assert_eq!(
        link(&taken.platform, &store, "handed").unwrap(),
        reference_program
    );
    let before = children();
    spec.select_handed(Some(&handed), &store, CaptureMode::Cold)
        .unwrap();
    assert!(children() > before, "a handed selection answers once");

    // SAFETY: this proof runs alone, as at its initial environment setup.
    unsafe { std::env::set_var("HIRSEL_PLATFORM_CAPTURE", "cold") };
    let before = (RECORD_READS.get(), children());
    let override_cold = select(&store, CaptureMode::from_environment().unwrap(), &now);
    assert!(!override_cold.hit);
    assert_eq!(RECORD_READS.get(), before.0);
    assert!(children() > before.1);
    // SAFETY: this proof runs alone.
    unsafe { std::env::remove_var("HIRSEL_PLATFORM_CAPTURE") };

    let record = cache.join(name(&spec.key().unwrap()));
    let issued = std::fs::read(&record).unwrap();
    let forced = select(
        &store,
        CaptureMode::Cold,
        &now.after(Duration::from_secs(1)),
    );
    assert!(!forced.hit, "the forced-cold switch reads no record");
    assert_eq!(forced.platform.identity, cold.platform.identity);
    assert_ne!(
        std::fs::read(&record).unwrap(),
        issued,
        "and records afresh"
    );

    let later = now.after(PERIOD + Duration::from_secs(2));
    let aged = select(&store, CaptureMode::Record, &later);
    assert!(!aged.hit, "a record older than a day is verified fully");
    assert_eq!(aged.platform.identity, cold.platform.identity);
    assert!(select(&store, CaptureMode::Record, &later).hit);

    // The store loses the runtime archive's chunks and the process its
    // checked copy: the record still names a structurally complete platform.
    let archive = cold.platform.runtime_archive.content;
    let lost = Lost {
        inner: &store,
        lost: Mutex::new(
            blob_record(&store, archive)
                .unwrap()
                .chunks
                .into_iter()
                .map(Reference::from)
                .collect(),
        ),
    };
    crate::object_queries::forget_retained();
    std::fs::remove_dir_all(native.join("inputs-v1")).unwrap();
    let wrong = select(&lost, CaptureMode::Record, &later);
    assert!(wrong.hit);
    let failure = link(&wrong.platform, &lost, "wrong").unwrap_err();
    let fresh = spec
        .recover_in(
            &lost,
            &at(&cache, &later),
            wrong.platform.identity,
            &failure,
        )
        .unwrap();
    assert_eq!(link(&fresh, &lost, "recovered").unwrap(), program);

    // Full verification finds another platform than the one a failure's
    // record named: every output that platform fed is withheld.
    let debug = archives.join("default/debug/libhirsel_rt.a");
    std::fs::remove_file(runtime.join("libhirsel_rt.a")).unwrap();
    std::fs::copy(&debug, runtime.join("libhirsel_rt.a")).unwrap();
    let Err(error) = spec.recover_in(
        &store,
        &at(&cache, &later),
        cold.platform.identity,
        "Link failed",
    ) else {
        panic!("recovery kept a platform other than the recorded one");
    };
    assert!(error.starts_with("platform cache mismatch"), "{error}");
    let replaced = select(&store, CaptureMode::Record, &later);
    assert!(replaced.hit, "recovery recorded the full verification");
    assert_ne!(replaced.platform.identity, cold.platform.identity);

    // Rule R6. Bug: a hit that renews its record's day, so a record in daily
    // use is never verified again. Oracle: the fixed day from issue.
    assert!(select(&store, CaptureMode::Record, &later.after(PERIOD / 2)).hit);
    let day = later.after(PERIOD + Duration::from_secs(1));
    assert!(
        !select(&store, CaptureMode::Record, &day).hit,
        "a hit within the day must not renew it"
    );
    // Bug: a record that outlives a reboot (boottime restarts; the guarded
    // stamps may come from a restored disk).
    let rebooted = Clock {
        boot: "another boot".into(),
        ..day.after(Duration::from_secs(1))
    };
    assert!(
        !select(&store, CaptureMode::Record, &rebooted).hit,
        "reboot"
    );
    assert!(
        !select(
            &store,
            CaptureMode::Record,
            &day.after(Duration::from_secs(2))
        )
        .hit
    );
    let hour = day.after(Duration::from_secs(3600));
    assert!(select(&store, CaptureMode::Record, &hour).hit);
    // Bug: a realtime jump (or rollback) within one boot passing as the
    // record's age. Oracle: boottime, which no clock change moves.
    let jumped = Clock {
        realtime: hour.realtime + Duration::from_secs(10),
        ..hour.after(Duration::from_secs(1))
    };
    assert!(
        !select(&store, CaptureMode::Record, &jumped).hit,
        "clock jump"
    );

    // Rule R5. Bug: a record whose searched-path guard was edited (here an
    // absent candidate the loader searched, renamed) still answers. Oracle:
    // the MAC under the registration covers every byte of the record.
    let settled = jumped.after(Duration::from_secs(60));
    assert!(select(&store, CaptureMode::Record, &settled).hit);
    let text = std::fs::read_to_string(&record).unwrap();
    let (_, body) = text.split_once('\n').unwrap();
    let recorded: LocalRecord = hirsel_codec::from_text(body).unwrap();
    let candidate = recorded
        .absent
        .first()
        .expect("the loader searched an absent path");
    std::fs::write(
        &record,
        text.replacen(candidate.as_str(), &format!("{candidate}-renamed"), 1),
    )
    .unwrap();
    assert!(
        !select(&store, CaptureMode::Record, &settled).hit,
        "an edited guard must not answer"
    );
}

/// A registration whose authority keys this proof's platform records.
fn registered(directory: &Path) -> hirsel_artifact_store::programs::ProgramAuthority {
    use hirsel_artifact_store::{Action, Address, Association, Domain, Store};
    let root = directory.join("registered");
    let state = directory.join("registration-state");
    std::fs::create_dir_all(&root).unwrap();
    let verifier = ContentHash::of_bytes(b"platform record proof verifier");
    let store = Store::new(root.clone());
    let reference = store
        .put(
            Domain {
                kind: 31,
                schema: 1,
            },
            b"platform record proof header",
        )
        .unwrap();
    let address = Address(ContentHash::of_bytes(b"ordinary"));
    let action = Action(ContentHash::default());
    let install =
        hirsel_artifact_store::registration::Installation::begin(&root, &state, verifier).unwrap();
    store.associate(address, action, reference).unwrap();
    install
        .finish((address, Association { action, reference }))
        .unwrap();
    let registered =
        hirsel_artifact_store::registration::Installation::registered(&root, &state, verifier)
            .unwrap();
    hirsel_artifact_store::programs::ProgramAuthority::new(vec![registered.authority]).unwrap()
}
