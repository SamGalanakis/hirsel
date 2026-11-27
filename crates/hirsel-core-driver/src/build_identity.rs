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
        options.cas = dir.path().join("cold-cas");
        let reopened =
            Publication::open(&code, root, &crate::request::local_admission_limits()).unwrap();
        let replayed = reopened.assemble(&options, Default::default()).unwrap();
        assert_eq!(
            bytes,
            std::fs::read(replayed.executable).unwrap(),
            "source and published assembly disagree"
        );
    }

    /// A test's Dir slot and its Exec policy directory are the launch's:
    /// identical packages in two places give identical executable keys and
    /// identical test binaries, and each run still uses its own directory.
    #[test]
    fn test_binaries_take_no_place() {
        use crate::package_content::archive_digest;
        let search = std::env::var_os("PATH").unwrap_or_default();
        let image = crate::host::find_program(Path::new("true"), &search).unwrap();
        let profile = format!(
            "[true]\nimage = {:?}\nsha256 = \"{}\"\nargv0 = \"true\"\nargs = [[]]\nenv = {{}}\ndirs = {{ work = \"/\" }}\n",
            image.to_str().unwrap(),
            hirsel_core_ids::hex::encode(&archive_digest(&std::fs::read(&image).unwrap())),
        );
        let source = "@test\nfn writes_its_directory(d: Dir) -> Result[(), ()] waits {\n\
            let file = result.unwrap(host.open(d, \"probe\", CreateNew))\n\
            result.unwrap(host.close(take file))\n\
            Ok(())\n}\n\
            @test\nfn reads_its_policy(e: Exec) -> Result[(), ()] { Ok(()) }\n";
        let tests = ["writes_its_directory", "reads_its_policy"];
        let places = [PackageLaw::new(), PackageLaw::new()];
        let mut runs = Vec::new();
        for law in &places {
            law.app("app", "");
            law.write("app/main.hirsel", source);
            law.write("profiles.toml", &profile);
            std::fs::create_dir(law.path("data")).unwrap();
            let out = law.path("out");
            let (status, stdout, stderr) = law.run(&[
                "test",
                "app",
                "--json",
                "--limit",
                "67108864",
                "--grant",
                "Exec=profiles.toml true",
                "--dir",
                "data",
                "-o",
                out.to_str().unwrap(),
            ]);
            assert_eq!(status, Some(0), "{stdout}\n{stderr}");
            assert!(
                law.path("data/probe").is_file(),
                "the Dir slot is this place's"
            );
            // Each binary, and the name of the executable record that holds it.
            let records = law.path("cache/hirsel/cas/keys");
            let run = tests.map(|test| {
                let test = DefPath::from_full(&format!("app@1/main/{test}")).unwrap();
                let name = ContentHash::of_bytes(test.to_string().as_bytes()).to_string();
                let binary = std::fs::read(out.join(name)).unwrap();
                let digest = archive_digest(&binary);
                let keys = walkdir(&records)
                    .into_iter()
                    .filter(|record| {
                        std::fs::read(record)
                            .unwrap()
                            .windows(32)
                            .any(|window| window == digest)
                    })
                    .map(|record| record.strip_prefix(&records).unwrap().to_owned())
                    .collect::<Vec<_>>();
                assert_eq!(keys.len(), 1, "one executable record names {test}");
                (binary, keys)
            });
            runs.push(run);
        }
        for (test, (a, b)) in tests.iter().zip(runs[0].iter().zip(&runs[1])) {
            assert!(a.0 == b.0, "{test}: the binaries differ between places");
            assert_eq!(
                a.1, b.1,
                "{test}: the executable keys differ between places"
            );
        }
    }

    fn walkdir(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_owned()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
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
        options.cas = dir.path().join("cold");
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
        options.cas = dir.path().join("cold");
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

#[test]
fn development_link_recapture_preserves_the_observed_recipe() {
    for after_check in [false, true] {
        for changed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut options = native_options(Backend::Cranelift, dir.path());
            let bin = dir.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            let linker = crate::link_closure::editor_name(crate::host::HOST_TARGET.object_format());
            let first = bin.join("first").join(linker);
            let second = bin.join("second").join(linker);
            for path in [&first, &second] {
                std::fs::create_dir(path.parent().unwrap()).unwrap();
            }
            let clang = options.link.driver();
            let lld = clang.with_file_name(linker);
            let quote =
                |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
            let editor = format!("#!/bin/sh\nexec {} \"$@\"\n", quote(&lld));
            crate::executable::write(&first, editor.as_bytes(), 0o755).unwrap();
            let replacement = if changed {
                format!("{editor}# different editor bytes\n")
            } else {
                editor.clone()
            };
            crate::executable::write(&second, replacement.as_bytes(), 0o755).unwrap();
            let cc = bin.join("clang");
            let script = format!(
                "#!/bin/sh\neditor={}\nif [ ! -f \"$editor\" ]; then editor={}; fi\n\
                 if [ \"$1\" = -print-prog-name={linker} ]; then printf '%s\\n' \"$editor\"; exit; fi\n\
                 exec {} --ld-path=\"$editor\" \"$@\"\n",
                quote(&first),
                quote(&second),
                quote(clang)
            );
            crate::executable::write(&cc, script.as_bytes(), 0o755).unwrap();
            options.link = Link::Driver(cc);
            let observed =
                crate::tool_inputs::toolchain(&options.link, &options.target, &options.runtime)
                    .unwrap();
            assert_eq!(observed.1.linker, first.canonicalize().unwrap());
            options.observed = Some(Arc::new(observed));
            let entry = DefPath::from_full("app@1/main/main").unwrap();
            let request = source_request(
                "pub fn main() -> Result[(), Exit] { Error(Exit(status: 7)) }",
                dir.path(),
            );
            let product = Driver::default()
                .check(request, &mut Meter::new(false))
                .unwrap();
            if after_check {
                let first = first.clone();
                AFTER_CHECK.with(|hook| {
                    *hook.borrow_mut() =
                        Some(Box::new(move |_| std::fs::remove_file(&first).unwrap()));
                });
            } else {
                std::fs::remove_file(&first).unwrap();
            }
            let result = product.build(&entry, &options);
            AFTER_CHECK.with(|hook| hook.borrow_mut().take());
            if changed {
                match result {
                    Err(NativeStop::Link(reason)) => {
                        assert_eq!(reason, "link inputs changed during native build")
                    }
                    Err(other) => panic!("unexpected link refusal: {other}"),
                    Ok(_) => panic!("changed recipe linked under the original key"),
                }
                assert!(!options.output.exists());
                crate::executable::write(&first, editor.as_bytes(), 0o755).unwrap();
                let original = product.build(&entry, &options).unwrap();
                assert!(
                    original.linked,
                    "the refused attempt must leave the original key empty"
                );
                run_clean(&original.executable, 7);
            } else {
                let recovered = result.unwrap();
                assert!(recovered.linked);
                run_clean(&recovered.executable, 7);
                assert!(!product.build(&entry, &options).unwrap().linked);
            }
        }
    }
}
