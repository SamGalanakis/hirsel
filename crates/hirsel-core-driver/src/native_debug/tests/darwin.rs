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

    rejects_bad_macho_structure();

    let apple = Target::Aarch64Darwin.apple().unwrap();
    let target = apple.triple;
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
        code: hirsel_core_ir::release_recipe::ForeignCode::Native(vec![]),
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
            .arg(format!("--target={}", apple.llvm_triple))
            .args(["-O2", "-c"])
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
        args: ["-flavor", "darwin", "-arch", "arm64"]
            .into_iter()
            .chain(apple.platform_version_args())
            .chain(["-adhoc_codesign", "-e", "_hirsel_entry"])
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
    let bundle = held.path().unwrap();
    let mut inputs = vec![bundle.clone()];
    staged
        .append_link_inputs(&mut inputs, &[], &runtime, &recipe)
        .unwrap();
    let image = dir.path().join("sqlite");
    crate::native::direct_link(&recipe, &inputs, Some(&bundle), &image, true, false, false)
        .unwrap();
    let bytes = std::fs::read(&image).unwrap();
    super::macho_structure(&bytes);
    crate::native_debug::macho_structure(&image).unwrap();
}

fn rejects_bad_macho_structure() {
    // Offsets and field widths come from mach_header_64, segment_command_64
    // and section_64 in mach-o/loader.h.
    let words = |values: &[u32]| {
        values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>()
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image");
    for (maxprot, rpath, section, size, flags, want) in [
        (5, false, "__text", 8, 0, None),
        (7, false, "__text", 8, 0, Some("maxprot")),
        (5, true, "__text", 8, 0, Some("LC_RPATH")),
        (5, false, "__mod_init_func", 8, 9, Some("__mod_init_func")),
        (5, false, "__init_offsets", 4, 22, Some("__init_offsets")),
        (5, false, "__init_offsets", 0, 22, None),
        (5, false, "__renamed", 8, 9, Some("__renamed")),
    ] {
        let mut segment = vec![0; 152];
        segment[..8].copy_from_slice(&words(&[0x19, 152]));
        segment[8..14].copy_from_slice(b"__TEXT");
        segment[56..68].copy_from_slice(&words(&[maxprot, 5, 1]));
        segment[72..72 + section.len()].copy_from_slice(section.as_bytes());
        segment[88..94].copy_from_slice(b"__TEXT");
        segment[112..120].copy_from_slice(&words(&[size, 0]));
        segment[136..140].copy_from_slice(&words(&[flags]));
        let apple = hirsel_core_layout::Target::Aarch64Darwin.apple().unwrap();
        let mut commands = words(&[
            0x32,
            24,
            apple.macho_platform,
            apple.macho_minos,
            apple.macho_minos,
            0,
        ]);
        commands.extend(words(&[0x1b, 24, 1, 2, 3, 4]));
        commands.extend(words(&[0x1d, 16, 0, 1]));
        commands.extend(segment);
        if rpath {
            commands.extend(words(&[0x8000_001c, 16, 12]));
            commands.extend(b"/x\0\0");
        }
        let mut bytes = words(&[
            0xfeed_facf,
            0x0100_000c,
            0,
            2,
            4 + u32::from(rpath),
            commands.len() as u32,
            0,
            0,
        ]);
        bytes.extend(commands);
        std::fs::write(&path, bytes).unwrap();
        let result = crate::native_debug::macho_structure(&path);
        if let Some(want) = want {
            assert!(result.unwrap_err().to_string().contains(want), "{want}");
        } else {
            result.unwrap();
        }
    }
}
