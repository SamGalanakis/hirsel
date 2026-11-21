// G1 and B1-B2. Use the native launcher to observe the grant and site data.
mod build_identity {
    use super::*;
    use crate::code_store::CodeStore;
    use crate::native::recipe_build::Publication;

    fn checked(source: &str, dir: &Path) -> Arc<CheckProduct> {
        let mut request = source_request(source, dir);
        let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
        request.inputs.packages[1] =
            PackageManifest::directory(&libraries.join("core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{:?} {:?}",
            product.diagnostics,
            product.incomplete
        );
        product
    }

    pub(super) fn publish(
        source: &str,
        dir: &Path,
        options: &NativeOptions,
        code: &CodeStore,
    ) -> Publication {
        let product = checked(source, dir);
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let reached = product.reachable(&entry, options.instance_limit).unwrap();
        product
            .publish_release(&entry, &reached, options, code, 16 * 1024 * 1024)
            .unwrap()
    }

    #[test]
    fn grant_binding_order() {
        let dir = tempfile::tempdir().unwrap();
        let code = CodeStore::new(dir.path().join("code"));
        let mut options = native_options(Backend::Cranelift, dir.path());
        let source = "pub fn main(first: Dir, second: Dir) -> Result[(), Exit] waits {\n\
            let one = result.unwrap(host.open(first, \"first\", CreateNew));\n\
            result.unwrap(host.close(take one));\n\
            let two = result.unwrap(host.open(second, \"second\", CreateNew));\n\
            result.unwrap(host.close(take two));\n\
            Ok(())\n}\n";
        for path in ["x", "y"] {
            std::fs::create_dir(dir.path().join(path)).unwrap();
        }
        let mut recipes = Vec::new();
        for paths in [["x", "y"], ["y", "x"]] {
            options.directories = paths.map(str::to_owned).to_vec();
            let release = publish(source, dir.path(), &options, &code);
            recipes.push(release.root);
            let built = release.assemble(&options, Default::default()).unwrap();
            let run = Command::new(&built.executable)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(run.status.success(), "{run:?}");
            assert!(
                dir.path().join(paths[0]).join("first").is_file(),
                "first parameter received {}",
                paths[0]
            );
            assert!(
                dir.path().join(paths[1]).join("second").is_file(),
                "second parameter received {}",
                paths[1]
            );
            std::fs::remove_file(dir.path().join(paths[0]).join("first")).unwrap();
            std::fs::remove_file(dir.path().join(paths[1]).join("second")).unwrap();
        }
        assert_ne!(
            recipes[0], recipes[1],
            "swapping the grant bindings must change R"
        );
    }

    #[test]
    fn assemble_is_build() {
        for (backend, optimize) in [(Backend::Cranelift, false), (Backend::Llvm, true)] {
            assert_agreement(backend, optimize);
        }
    }

    fn assert_agreement(backend: Backend, optimize: bool) {
        let dir = tempfile::tempdir().unwrap();
        let code = CodeStore::new(dir.path().join("code"));
        let mut options = native_options(backend, dir.path());
        options.optimize = optimize;
        let source = "pub fn main() -> Result[(), Exit] { Error(Exit(status: 7u8)) }\n";
        let release = publish(source, dir.path(), &options, &code);
        let root = release.root;
        let built = release.assemble(&options, Default::default()).unwrap();
        let bytes = std::fs::read(built.executable).unwrap();
        std::fs::remove_file(dir.path().join("main.hirsel")).unwrap();
        options.store = dir.path().join("cold");
        options.parts = dir.path().join("cold-parts");
        let reopened =
            Publication::open(&code, root, &crate::request::local_admission_limits()).unwrap();
        let replayed = reopened.assemble(&options, Default::default()).unwrap();
        assert_eq!(
            bytes,
            std::fs::read(replayed.executable).unwrap(),
            "source and published assembly disagree"
        );
    }

    #[test]
    fn site_only_change() {
        let dir = tempfile::tempdir().unwrap();
        let code = CodeStore::new(dir.path().join("code"));
        let options = native_options(Backend::Cranelift, dir.path());
        let original = "fn overflow(x: u64) -> u64 {\n    x + 1\n}\n\
            pub fn main() -> Result[(), Exit] {\n\
            if overflow(18446744073709551615u64) == 0 { Ok(()) } else { Error(Exit(status: 7)) }\n}\n";
        let edited = format!("// Move the site without changing the code.\n{original}");
        let mut rows = Vec::new();
        for (source, line) in [(original, 2), (edited.as_str(), 3)] {
            let release = publish(source, dir.path(), &options, &code);
            rows.push((release.root, release.recipe.address));
            let artifact = Publication::open(
                &code,
                release.root,
                &crate::request::local_admission_limits(),
            )
            .unwrap()
            .assemble(&options, Default::default())
            .unwrap();
            let run = Command::new(&artifact.executable).output().unwrap();
            assert_eq!(run.status.code(), Some(101), "{run:?}");
            assert_eq!(
                run.stderr,
                format!("Overflow at app@1/main.hirsel:{line}:5\n").as_bytes(),
                "assembled failure reports the annex site"
            );
        }
        assert_eq!(
            rows[0].1, rows[1].1,
            "a moved comment keeps the entry address"
        );
        assert_ne!(rows[0].0, rows[1].0, "a moved site changes R");
    }
}

mod assembly_environment {
    use super::*;
    use crate::{code_store::CodeStore, native::recipe_build::Publication};

    #[test]
    fn runtime_archive_change() {
        let dir = tempfile::tempdir().unwrap();
        let code = CodeStore::new(dir.path().join("code"));
        let mut options = native_options(Backend::Cranelift, dir.path());
        let input = options.runtime.clone();
        options.runtime = dir.path().join("runtime.a");
        std::fs::copy(&input, &options.runtime).unwrap();
        let source = "pub fn main() -> Result[(), Exit] { Ok(()) }\n";
        let root = build_identity::publish(source, dir.path(), &options, &code).root;
        let assemble = |options: &NativeOptions| {
            let built = Publication::open(&code, root, &crate::request::local_admission_limits())
                .unwrap()
                .assemble(options, Default::default())
                .unwrap();
            (
                std::fs::read(built.executable).unwrap(),
                built.assembly.unwrap(),
            )
        };
        let original = assemble(&options);
        let marker = dir.path().join("marker");
        std::fs::write(&marker, b"recipe runtime archive B3\0").unwrap();
        let editor = options.link.driver().with_file_name("llvm-objcopy");
        let status = Command::new(editor)
            .arg("--add-section")
            .arg(format!(".rodata.recipe_runtime={}", marker.display()))
            .args([
                "--set-section-flags",
                ".rodata.recipe_runtime=alloc,load,readonly,data",
            ])
            .arg(&input)
            .arg(&options.runtime)
            .status()
            .unwrap();
        assert!(status.success(), "prepare the changed runtime fixture");
        let warm = assemble(&options);
        options.store = dir.path().join("cold");
        let cold = assemble(&options);
        assert_eq!(
            warm.0, cold.0,
            "warm runtime assembly must match the cold executable"
        );
        assert_ne!(
            original.0, cold.0,
            "the runtime edit reaches the executable"
        );
        assert_eq!(original.1.recipe, cold.1.recipe);
        assert_ne!(
            original.1.toolchain, cold.1.toolchain,
            "runtime bytes change the fingerprint"
        );
        assert_eq!(warm.1.toolchain, cold.1.toolchain);
    }
    #[test]
    fn builtin_version_change() {
        let dir = tempfile::tempdir().unwrap();
        let code = CodeStore::new(dir.path().join("code"));
        let mut options = native_options(Backend::Cranelift, dir.path());
        let source = "pub fn main() -> Result[(), Exit] {
            let value = float.widen(7.0f32);
            if float.eq(value, 7.0f64) { Error(Exit(status: 7)) } else { Error(Exit(status: 8)) }
}
";
        let assemble = |options: &NativeOptions, bump: bool| {
            let mut release = build_identity::publish(source, dir.path(), options, &code);
            if bump {
                release.bump_inline_builtin(&code);
            }
            let product = release.assemble(options, Default::default()).unwrap();
            let exit = Command::new(&product.executable).status().unwrap().code();
            (
                std::fs::read(product.executable).unwrap(),
                exit,
                product.assembly.unwrap(),
            )
        };
        let original = assemble(&options, false);
        let warm = assemble(&options, true);
        options.store = dir.path().join("cold");
        let cold = assemble(&options, true);
        assert_eq!(original.1, Some(7));
        assert_eq!(cold.1, Some(8));
        assert_eq!(
            warm.0, cold.0,
            "warm builtin assembly must match the cold executable"
        );
        assert_ne!(
            original.0, cold.0,
            "the changed inline builtin reaches the executable"
        );
        assert_ne!(original.2.recipe, cold.2.recipe, "builtin numbers enter R");
    }
}
