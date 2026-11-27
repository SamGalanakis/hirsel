use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn sealed_foreign_archives_check_pin_object_features_and_abi() {
    use crate::code_store::ObjectKind;
    use crate::foreign_archive::{
        ArchiveOrigin, LinkArchives, SealedArchive, check_distinct_definitions,
    };
    use hirsel_core_ids::DefPath;
    use hirsel_core_ir::Identity;
    use hirsel_core_ir::decode::Debit;
    use hirsel_core_ir::release_recipe::{ForeignFunction, ForeignLeaf, ObjectRef};

    let target = "x86_64-unknown-linux-gnu";
    let module = DefPath::from_full("ffi_laws@1/native").unwrap();
    let digest = |text| crate::native::recipe_build::parse_root(text).unwrap();
    let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let open = |bytes, hash, len| {
        SealedArchive::open(
            bytes,
            ObjectRef { hash, len },
            &module,
            target,
            None,
            &Debit::unlimited(),
        )
    };
    let prefix = |hash: ContentHash| {
        format!(
            "FFI-3: archive object {} of ffi_laws@1/native for {target}",
            hash.to_string()
        )
    };
    let wrong = open(b"abc".to_vec(), digest(empty), 3).err().unwrap();
    assert_eq!(
        wrong.to_string(),
        format!(
            "{} does not match its pin: expected sha256 {empty}, found sha256 {abc}",
            prefix(digest(empty))
        )
    );
    let not_ar = open(b"abc".to_vec(), digest(abc), 3).err().unwrap();
    assert_eq!(
        not_ar.to_string(),
        format!("{} is not an indexed ar archive", prefix(digest(abc)))
    );
    let thin = b"!<thin>\n";
    let pin = ObjectKind::ForeignArchive.hash(thin);
    let thin = open(thin.to_vec(), pin, thin.len() as u64).err().unwrap();
    assert_eq!(
        thin.to_string(),
        format!(
            "{} is a thin archive; its members are not inside the pinned bytes",
            prefix(pin)
        )
    );

    let fixture = tempfile::tempdir().unwrap();
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin");
    let source = r#"
static volatile int flag;
int hfl_touch(void) { return flag; }
__attribute__((weak)) int hfl_shared;
__attribute__((used, section(".hirsel_abi")))
static const char abi[sizeof("hfl_touch stack () -> i32\n") - 1] = "hfl_touch stack () -> i32\n";
"#;
    let build_source = |source: &str| {
        std::fs::write(fixture.path().join("laws.c"), source).unwrap();
        let argument = format!("--target={target}");
        let compiled = Command::new(llvm.join("clang"))
            .current_dir(fixture.path())
            .args([
                &argument, "-std=c11", "-O2", "-fPIC", "-c", "laws.c", "-o", "laws.o",
            ])
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        let archived = Command::new(llvm.join("llvm-ar"))
            .current_dir(fixture.path())
            .args(["rcs", "laws.a", "laws.o"])
            .output()
            .unwrap();
        assert!(
            archived.status.success(),
            "{}",
            String::from_utf8_lossy(&archived.stderr)
        );
        let bytes = std::fs::read(fixture.path().join("laws.a")).unwrap();
        let object = ObjectRef {
            hash: ObjectKind::ForeignArchive.hash(&bytes),
            len: bytes.len() as u64,
        };
        (open(bytes, object.hash, object.len).unwrap(), object)
    };
    let build = |extra: &str| build_source(&format!("{source}{extra}"));
    let mut leaf = ForeignLeaf {
        module: module.clone(),
        symbols: "hfl_".into(),
        code: hirsel_core_ir::release_recipe::ForeignCode::Native(vec![]),
        functions: vec![ForeignFunction {
            identity: Identity::plain(DefPath::from_full("ffi_laws@1/native/touch").unwrap()),
            header: "(core@1/core/Foreign[ffi_laws@1/laws/Laws])->i32".into(),
        }],
    };
    let (valid, valid_object) = build("");
    valid
        .check_headers(valid_object, &leaf, target, None)
        .unwrap();
    let archive_path = fixture.path().join("laws.a");
    let mut staged = LinkArchives::default();
    staged
        .admit(
            valid_object,
            &leaf,
            target,
            Some("laws.a"),
            &Debit::unlimited(),
            || Ok(std::fs::read(&archive_path)?),
        )
        .unwrap();
    let second_module = DefPath::from_full("other@1/native").unwrap();
    let origin = ArchiveOrigin {
        object: valid_object,
        module: &module,
        path: None,
    };
    let first_row = (&valid, origin);
    let mut definitions = check_distinct_definitions(
        [
            first_row,
            (
                &valid,
                ArchiveOrigin {
                    module: &second_module,
                    ..origin
                },
            ),
        ],
        &Debit::unlimited(),
    )
    .unwrap();
    let (second, second_object) = build_source(
        r#"
int hfl_other(void) { return 0; }
int hfl_shared;
__attribute__((used, section(".hirsel_abi")))
static const char abi[sizeof("hfl_other stack () -> i32\n") - 1] = "hfl_other stack () -> i32\n";
"#,
    );
    let other_leaf = ForeignLeaf {
        module: second_module.clone(),
        symbols: "hfl".into(),
        code: hirsel_core_ir::release_recipe::ForeignCode::Native(vec![]),
        functions: vec![ForeignFunction {
            identity: Identity::plain(DefPath::from_full("other@1/native/_other").unwrap()),
            header: "(core@1/core/Foreign[other@1/wrapper/Other])->i32".into(),
        }],
    };
    second
        .check_headers(second_object, &other_leaf, target, None)
        .unwrap();
    let collision = check_distinct_definitions(
        [
            first_row,
            (
                &second,
                ArchiveOrigin {
                    object: second_object,
                    module: &second_module,
                    path: None,
                },
            ),
        ],
        &Debit::unlimited(),
    )
    .err()
    .unwrap();
    assert_eq!(
        collision.to_string(),
        format!(
            "FFI-3: symbol hfl_shared is defined by archive object {} of ffi_laws@1/native and by archive object {} of other@1/native",
            valid_object.hash.to_string(),
            second_object.hash.to_string(),
        )
    );
    // The path now names the second archive, which lacks hfl_touch.
    let caller = fixture.path().join("caller.o");
    let source = fixture.path().join("caller.c");
    std::fs::write(
        &source,
        "int hfl_touch(void); int caller(void) { return hfl_touch(); }",
    )
    .unwrap();
    let compiled = Command::new(llvm.join("clang"))
        .args([format!("--target={target}").as_str(), "-fPIC", "-c"])
        .arg(&source)
        .arg("-o")
        .arg(&caller)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let plan = LinkRecipe {
        schema: 0,
        target: target.into(),
        linker: llvm.join("ld.lld"),
        args: vec![],
        files: vec![],
        source_files: vec![],
        absent: vec![],
        identity: ContentHash::default(),
    };
    let mut laws = vec![fixture.path().join("laws.o")];
    let collision = staged.append_link_inputs(&mut laws, &[], &caller, &plan);
    assert_eq!(
        collision.unwrap_err().to_string(),
        "FFI-3: symbol hfl_shared is defined by archive laws.a of ffi_laws@1/native and by the generated code",
    );
    let mut inputs = vec![caller.clone()];
    staged
        .append_link_inputs(&mut inputs, &[], &caller, &plan)
        .unwrap();
    let linked = Command::new(&plan.linker)
        .args(["--shared", "--no-undefined"])
        .args(inputs)
        .arg("-o")
        .arg(fixture.path().join("sealed.so"))
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let runtime = std::fs::read(fixture.path().join("laws.a")).unwrap();
    let collision = definitions
        .check_link_input(&runtime, "the runtime")
        .unwrap_err();
    assert_eq!(
        collision.to_string(),
        format!(
            "FFI-3: symbol hfl_shared is defined by archive object {} of ffi_laws@1/native and by the runtime",
            valid_object.hash.to_string(),
        )
    );
    let linked = Command::new(llvm.join("ld.lld"))
        .current_dir(fixture.path())
        .args(["-shared", "--strip-all", "laws.o", "-o", "libsystem.so"])
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let library = std::fs::read(fixture.path().join("libsystem.so")).unwrap();
    let collision = definitions
        .check_link_input(&library, "link input libsystem.so")
        .unwrap_err();
    assert_eq!(
        collision.to_string(),
        format!(
            "FFI-3: symbol hfl_shared is defined by archive object {} of ffi_laws@1/native and by link input libsystem.so",
            valid_object.hash.to_string(),
        )
    );
    let (invalid, object) =
        build("__attribute__((constructor)) static void initialize(void) { flag = 1; }\n");
    let problem = invalid
        .check_headers(object, &leaf, target, None)
        .unwrap_err();
    assert_eq!(
        problem.to_string(),
        format!(
            "{}: member laws.o has section .init_array of type SHT_INIT_ARRAY, which is not an admitted object feature",
            prefix(object.hash)
        )
    );
    let (invalid, object) = build(
        r#"
__asm__(".data\n.globl hfl_unique\n.type hfl_unique,@gnu_unique_object\nhfl_unique: .byte 0\n");
"#,
    );
    let problem = invalid
        .check_headers(object, &leaf, target, None)
        .unwrap_err();
    assert_eq!(
        problem.to_string(),
        format!(
            "{}: member laws.o has symbol hfl_unique with binding STB_GNU_UNIQUE, which is not an admitted object feature",
            prefix(object.hash)
        )
    );
    for (source, expected) in [
        (
            "void *malloc(__SIZE_TYPE__ n) { return 0; }",
            ": member laws.o defines malloc, which does not start with the prefix hfl_",
        ),
        (
            r#"
static int hfl_impl(void) { return 0; }
static int (*hfl_resolve(void))(void) { return hfl_impl; }
__attribute__((ifunc("hfl_resolve"))) int hfl_indirect(void);
"#,
            ": member laws.o has indirect function hfl_indirect, which is not an admitted object feature",
        ),
    ] {
        let (invalid, object) = build(source);
        let error = invalid
            .check_headers(object, &leaf, target, None)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("{}{expected}", prefix(object.hash))
        );
    }
    let (invalid, object) = build_source("int hfl_touch(void) { return 0; }");
    let error = invalid
        .check_headers(object, &leaf, target, None)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("{} has no ABI text", prefix(object.hash))
    );

    // llvm-ar indexes both definitions. A reached symbol must select one member.
    std::fs::copy(
        fixture.path().join("laws.o"),
        fixture.path().join("duplicate.o"),
    )
    .unwrap();
    let output = Command::new(llvm.join("llvm-ar"))
        .current_dir(fixture.path())
        .args(["rcs", "laws.a", "duplicate.o"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = std::fs::read(fixture.path().join("laws.a")).unwrap();
    let object = ObjectRef {
        hash: ObjectKind::ForeignArchive.hash(&bytes),
        len: bytes.len() as u64,
    };
    let duplicate = open(bytes, object.hash, object.len).unwrap();
    let error = duplicate
        .check_headers(object, &leaf, target, None)
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "{} does not define hfl_touch, declared at ffi_laws@1/native/touch",
            prefix(object.hash)
        )
    );
    leaf.functions[0].header = "(core@1/core/Foreign[ffi_laws@1/laws/Laws])->u64".into();
    let problem = valid
        .check_headers(valid_object, &leaf, target, None)
        .unwrap_err();
    assert_eq!(
        problem.to_string(),
        format!(
            "{}: the ABI of hfl_touch is hfl_touch stack () -> i32, but its header declares hfl_touch stack () -> u64, declared at ffi_laws@1/native/touch",
            prefix(valid_object.hash)
        )
    );
}

/// Law: LLVM keys take the loaded libLLVM's bytes, so a library one byte
/// apart (same name and size) is another identity, and a copy the same.
/// Oracle: the files' bytes.
#[test]
fn a_libllvm_one_byte_apart_is_another_llvm_identity() {
    let work = tempfile::tempdir().unwrap();
    let mut bytes = vec![0x5a; 4096];
    let file = |name: &str, bytes: &[u8]| {
        let path = work.path().join(name).join("libLLVM.so.23.1");
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        LLVM_LIBRARY.set(Some(path));
        let identity = llvm_library().unwrap();
        LLVM_LIBRARY.set(None);
        identity
    };
    let first = file("first", &bytes);
    let copy = file("copy", &bytes);
    bytes[2048] ^= 1;
    let apart = file("apart", &bytes);
    assert_eq!(first, copy);
    assert_ne!(first, apart);
}

#[test]
fn a_rematerialised_toolchain_file_is_read_once() {
    // Buck re-links toolchain files: same bytes, new inode and ctime. The request
    // that sees it re-hashes once and records the new stat; later requests read
    // nothing and rewrite nothing.
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let linker = bin.join("ld.lld");
    let crt = work.path().join("crt1.o");
    std::fs::write(&linker, b"linker").unwrap();
    std::fs::write(&crt, b"crt").unwrap();
    let cc = bin.join("cc");
    crate::executable::write(
        &cc,
        format!(
            "#!/bin/sh\necho '\"{}\" \"-pie\" \"-o\" \"out\" \"{}\" \"-lc\"' >&2\n",
            linker.display(),
            crt.display()
        )
        .as_bytes(),
        0o755,
    )
    .unwrap();
    let runtime = work.path().join("runtime.a");
    std::fs::write(&runtime, b"runtime").unwrap();
    let target = "x86_64-unknown-linux-gnu";
    CLOCK.set(Some(0));
    let identity = driver_toolchain(&cc, target, &runtime).unwrap().0;
    for file in [&linker, &crt, &runtime] {
        let copy = file.with_extension("rematerialised");
        std::fs::copy(file, &copy).unwrap();
        std::fs::rename(&copy, file).unwrap();
    }
    // The new stats are sighted, then settled by a sighting a window later.
    // A request reads each moved file once, though both the recipe and the
    // toolchain record name the linker and the CRT.
    CLOCK.set(Some(1));
    READS.set(0);
    assert_eq!(driver_toolchain(&cc, target, &runtime).unwrap().0, identity);
    assert_eq!(READS.get(), 4, "cc, linker, CRT and runtime, once each");
    CLOCK.set(Some(1 + WINDOW));
    assert_eq!(driver_toolchain(&cc, target, &runtime).unwrap().0, identity);
    let manifest_path = recipes()
        .unwrap()
        .path(toolchain_key(target, &capture_key(&cc)).unwrap());
    let written = std::fs::metadata(&manifest_path).unwrap().ino();
    READS.set(0);
    assert_eq!(driver_toolchain(&cc, target, &runtime).unwrap().0, identity);
    assert_eq!(READS.get(), 0, "settled toolchain files are not read again");
    assert_eq!(
        std::fs::metadata(&manifest_path).unwrap().ino(),
        written,
        "an unchanged toolchain record is not rewritten"
    );
    CLOCK.set(None);
}

#[test]
fn each_runtime_archive_keys_its_own_toolchain_identity() {
    // Two builds share one C driver but link different runtime archives (the pool and
    // openat2 host targets do). A recipe captured for one must not stand in for the other.
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let linker = bin.join("ld.lld");
    let crt = work.path().join("crt1.o");
    std::fs::write(&linker, b"linker").unwrap();
    std::fs::write(&crt, b"crt").unwrap();
    let cc = bin.join("cc");
    crate::executable::write(
        &cc,
        format!(
            "#!/bin/sh\necho '\"{}\" \"-pie\" \"-o\" \"out\" \"{}\" \"-lc\"' >&2\n",
            linker.display(),
            crt.display()
        )
        .as_bytes(),
        0o755,
    )
    .unwrap();
    let target = "x86_64-unknown-linux-gnu";
    let (first, second) = (work.path().join("first.a"), work.path().join("second.a"));
    std::fs::write(&first, b"first runtime").unwrap();
    std::fs::write(&second, b"second runtime").unwrap();
    let first_identity = driver_toolchain(&cc, target, &first).unwrap().0;
    let second_identity = driver_toolchain(&cc, target, &second).unwrap().0;
    assert_ne!(
        first_identity, second_identity,
        "different runtime bytes link differently"
    );
    std::fs::write(&second, b"second runtime, rebuilt").unwrap();
    assert_ne!(
        driver_toolchain(&cc, target, &second).unwrap().0,
        second_identity,
        "a rebuilt runtime archive must miss every object keyed by the old one"
    );
    assert_eq!(
        driver_toolchain(&cc, target, &first).unwrap().0,
        first_identity
    );
    // The archive's bytes are its identity, not the place its build left it.
    let moved = work.path().join("elsewhere/first.a");
    std::fs::create_dir(moved.parent().unwrap()).unwrap();
    std::fs::copy(&first, &moved).unwrap();
    assert_eq!(
        driver_toolchain(&cc, target, &moved).unwrap().0,
        first_identity,
        "one runtime archive at another path"
    );
}

/// Law: one installation copied
/// to another root keys the same recipe and toolchain, so its prebuilt
/// objects are hits there. Oracle: the two roots hold byte-equal files.
#[test]
fn a_relocated_installation_keys_the_same_toolchain() {
    let work = tempfile::tempdir().unwrap();
    let target = "x86_64-unknown-linux-gnu";
    let keyed = |root: &str| {
        let root = work.path().join(root);
        let recipe = root.join("link/link.json");
        let linker = root.join(INSTALLED_LINKER);
        let runtime = root.join("lib/libhirsel_core_rt.a");
        let crt = "link/kit/crt1.o";
        let crt_file = root.join(crt);
        for dir in [&recipe, &linker, &runtime, &crt_file] {
            std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        }
        std::fs::write(crt_file, b"\x7fELFcrt").unwrap();
        std::fs::write(&linker, b"linker").unwrap();
        std::fs::write(&runtime, b"runtime").unwrap();
        let installed = InstalledRecipe {
            schema: INSTALLED_RECIPE_SCHEMA,
            target: target.into(),
            args: vec![crt.into(), INPUT_MARKER.into()],
            archive_tools: Default::default(),
        };
        std::fs::write(&recipe, serde_json::to_vec(&installed).unwrap()).unwrap();
        let (toolchain, recipe) = installed_toolchain(&recipe, target, &runtime).unwrap();
        (toolchain, recipe.identity)
    };
    assert_eq!(keyed("first"), keyed("second/deeper"));
}

#[test]
fn a_link_source_the_driver_adds_and_the_capture_cannot_replay_refuses() {
    // A wrapper `cc "$@" support.c -Wl,--wrap=f` over gcc: gcc compiles support.c
    // through cc1 and as, which the capture does not replay, so the link line names
    // two transient objects. Folding both into the one input marker kept --wrap=f
    // and dropped __wrap_f's definition (the 922 Arc recording sink).
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let linker = bin.join("ld.lld");
    std::fs::write(&linker, b"linker").unwrap();
    let cc = bin.join("cc");
    let (own, added) = (work.path().join("cc1.o"), work.path().join("cc2.o"));
    crate::executable::write(&cc, format!(
                "#!/bin/sh\necho '\"{}\" \"-pie\" \"-o\" \"out\" \"{}\" \"{}\" \"--wrap=f\" \"-lc\"' >&2\n",
                linker.display(),
                own.display(),
                added.display()
            ).as_bytes(), 0o755).unwrap();
    let refused =
        capture_link_recipe(&cc, "x86_64-unknown-linux-gnu", false, &search()).unwrap_err();
    assert!(refused.to_string().contains("cannot replay"), "{refused}");
}

#[test]
fn a_startup_file_the_driver_cannot_find_is_a_missing_link_input() {
    // clang with no GCC installation for the target (a pool worker without an
    // aarch64 cross libc) names its startup files bare. They are not sources
    // the driver adds, so the capture names the missing file.
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let linker = bin.join("ld.lld");
    std::fs::write(&linker, b"linker").unwrap();
    let cc = bin.join("cc");
    let own = work.path().join("recipe-1.o");
    crate::executable::write(&cc, format!(
                "#!/bin/sh\necho '\"{}\" \"-pie\" \"-o\" \"out\" \"Scrt1.o\" \"crti.o\" \"{}\" \"-lc\" \"crtn.o\"' >&2\n",
                linker.display(),
                own.display()
            ).as_bytes(), 0o755).unwrap();
    let target = "aarch64-unknown-linux-gnu";
    let refused = capture_link_recipe(&cc, target, false, &search()).unwrap_err();
    let expected = format!(
        "missing link input: {} found no Scrt1.o for {target}",
        cc.display()
    );
    assert!(refused.to_string().starts_with(&expected), "{refused}");
}

#[test]
fn a_recipe_links_with_the_editor_its_own_path_selects() {
    // gcc names ld.lld bare, so each request's PATH picks the editor. In 4838 a
    // test whose PATH put a private LLVM copy first captured the one recipe of
    // the ARM gcc; the next request linked through that copy, then relinked
    // through its own once the copy was gone.
    let work = tempfile::tempdir().unwrap();
    let bin = work.path().join("tree/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let (first, second) = (work.path().join("first"), work.path().join("second"));
    for (directory, bytes) in [(&first, b"first".as_slice()), (&second, b"second")] {
        std::fs::create_dir(directory).unwrap();
        std::fs::write(directory.join("ld.lld"), bytes).unwrap();
        std::fs::set_permissions(
            directory.join("ld.lld"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let crt = work.path().join("crt1.o");
    std::fs::write(&crt, b"crt").unwrap();
    let runs = work.path().join("driver-runs");
    let cc = bin.join("cc");
    crate::executable::write(
            &cc,
            format!(
                "#!/bin/sh\necho run >> '{}'\ncase \"$1\" in -print-prog-name=*) echo ld.lld; exit 0;; esac\necho '\"/gcc/collect2\" \"-pie\" \"-o\" \"out\" \"{}\" \"{}\" \"-lc\"' >&2\n",
                runs.display(),
                crt.display(),
                work.path().join("transient.o").display()
            )
            .as_bytes(),
            0o755,
        )
        .unwrap();
    let target = "x86_64-unknown-linux-gnu";
    let linker = |search: &[&PathBuf]| {
        capture_link_recipe(&cc, target, false, &std::env::join_paths(search).unwrap())
            .unwrap()
            .linker
    };
    let editor = |directory: &Path| directory.join("ld.lld").canonicalize().unwrap();
    let driver_runs = || std::fs::read_to_string(&runs).map_or(0, |runs| runs.lines().count());
    assert_eq!(linker(&[&second]), editor(&second));
    assert_eq!(
        linker(&[&first, &second]),
        editor(&first),
        "an earlier PATH candidate is this request's editor"
    );
    let before = driver_runs();
    assert_eq!(linker(&[&second]), editor(&second));
    assert_eq!(
        driver_runs(),
        before,
        "each selection keeps its own recipe; neither request captures again"
    );
}

/// The key a capture through `cc` under this process's environment
/// names its records with.
fn capture_key(cc: &Path) -> String {
    let environment = CaptureEnvironment::new(&search()).unwrap();
    environment.key(&driver_file_key(cc).unwrap()).unwrap()
}

fn recipe(input: &Path) -> LinkRecipe {
    LinkRecipe {
        schema: LINK_RECIPE_SCHEMA,
        target: "x86_64-unknown-linux-gnu".into(),
        linker: input.to_owned(),
        args: vec![INPUT_MARKER.into()],
        files: vec![recipe_file(input).unwrap()],
        source_files: vec![],
        absent: vec![],
        identity: ContentHash::of_bytes(b"recipe"),
    }
}

mod stat_trust;
