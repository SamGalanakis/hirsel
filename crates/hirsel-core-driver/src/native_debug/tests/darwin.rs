//! The Darwin cross-link of a foreign archive, through FFI-3 and the driver's
//! Mach-O link.

/// macOS S10 gate: sqlite's `aarch64-apple-darwin` archive, as `build.sh`
/// pins it, passes FFI-3's Mach-O rows and its import check against the link
/// kit's libSystem stub, and the driver's Mach-O link of a caller of
/// `hsq_close` with it and that stub is an arm64 executable of the §8.1
/// structure laws. Bugs: the Darwin row importing a name the libSystem
/// allowlist leaves out (`fsync`, `syscall`, `__errno_location`), which ld64
/// would leave unresolved; a non-`hsq_` external, which collides with the
/// runtime's names; and a member ld64 cannot link.
#[test]
fn sqlite_darwin_archive_admits_and_links_against_the_kit() {
    use crate::code_store::ObjectKind;
    use crate::foreign_archive::LinkArchives;
    use crate::tool_inputs::{LinkRecipe, RecipeFile, RecipeStat, Trust};
    use hirsel_core_ids::{ContentHash, DefPath};
    use hirsel_core_ir::decode::Debit;
    use hirsel_core_ir::release_recipe::{ForeignLeaf, ObjectRef};
    use hirsel_core_layout::{ObjectFormat, Target};
    use std::{path::PathBuf, process::Command};

    let target = Target::Aarch64Darwin.triple();
    let sqlite = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../core/programs/sqlite/libhirsel_sqlite-aarch64-apple-darwin.a");
    let bytes = std::fs::read(&sqlite).unwrap();
    let object = ObjectRef {
        hash: ObjectKind::ForeignArchive.hash(&bytes),
        len: bytes.len() as u64,
    };
    let leaf = ForeignLeaf {
        module: DefPath::from_full("sqlite@1/native").unwrap(),
        symbols: "hsq_".into(),
        targets: vec![],
        functions: vec![],
    };
    let mut staged = LinkArchives::default();
    staged
        .admit(
            object,
            &leaf,
            target,
            Some("libhirsel_sqlite-aarch64-apple-darwin.a"),
            &Debit::unlimited(),
            || Ok(bytes),
        )
        .unwrap();

    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin");
    let linker = llvm.join("ld.lld");
    let stub = PathBuf::from(std::env::var_os("HIRSEL_LINK_KIT").unwrap())
        .join(target)
        .join("libSystem.tbd");
    let dir = tempfile::tempdir().unwrap();
    let compile = |name: &str, source: &str| {
        std::fs::write(dir.path().join(format!("{name}.c")), source).unwrap();
        let output = Command::new(llvm.join("clang"))
            .current_dir(dir.path())
            .args(["--target=arm64-apple-macos26.0", "-O2", "-c"])
            .arg(format!("{name}.c"))
            .arg("-o")
            .arg(format!("{name}.o"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        dir.path().join(format!("{name}.o"))
    };
    let runtime = compile("runtime", "int hirsel_runtime;\n");
    let recorded = |path: &std::path::Path| RecipeFile {
        path: path.to_owned(),
        content: ContentHash::default(),
        stat: RecipeStat {
            dev: 0,
            ino: 0,
            len: 0,
            mode: 0,
            mtime: 0,
            mtime_nsec: 0,
            ctime: 0,
            ctime_nsec: 0,
        },
        trust: Trust::Settled,
    };
    let recipe = LinkRecipe {
        schema: 0,
        target: target.into(),
        linker: linker.clone(),
        args: ["-flavor", "darwin", "-arch", "arm64", "-platform_version"]
            .into_iter()
            .chain([
                "macos",
                "26.0",
                "26.0",
                "-adhoc_codesign",
                "-e",
                "_hirsel_entry",
            ])
            .chain([crate::tool_inputs::INPUT_MARKER, stub.to_str().unwrap()])
            .map(str::to_owned)
            .collect(),
        files: vec![recorded(&linker), recorded(&stub)],
        source_files: vec![],
        absent: vec![],
        identity: ContentHash::of_bytes(b"recipe"),
    };
    let entry = std::fs::read(compile(
        "entry",
        "extern int hsq_close(void *);\nint hirsel_entry(void) { return hsq_close(0); }\n",
    ))
    .unwrap();
    let held = crate::native::link_input::Bundle::of(
        &[std::sync::Arc::<[u8]>::from(entry)],
        ObjectFormat::MachO,
    )
    .unwrap();
    let bundle = held.path();
    let mut inputs = vec![bundle.clone()];
    staged
        .append_link_inputs(&mut inputs, &[], &runtime, &recipe)
        .unwrap();
    let image = dir.path().join("sqlite");
    crate::native::direct_link(&recipe, &inputs, Some(&bundle), &image, true, false, false)
        .unwrap();
    let bytes = std::fs::read(&image).unwrap();
    super::macho_structure(&bytes);
}
