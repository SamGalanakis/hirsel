#[test]
fn foreign_recipe_build_uses_only_selected_sealed_archive() {
    use crate::code_store::{CodeStore, ObjectKind};
    use hirsel_core_ir::{decode::Debit, release_recipe::Recipe};

    for engine in ["cranelift", "llvm"] {
        let law = PackageLaw::new();
        for file in ["laws.hirsel", "native.hirsel", "main.hirsel", "libffi_laws.a", "libffi_laws-aarch64-apple-darwin.a"] {
            let contents = std::fs::read(law.libraries.join("programs/ffi_laws").join(file)).unwrap();
            law.write(&format!("ffi_laws/{file}"), contents);
        }
        let store = CodeStore::new(law.path("code-store"));
        let x86 = std::fs::read(law.path("ffi_laws/libffi_laws.a")).unwrap();
        let x86_pin = ObjectKind::ForeignArchive.hash(&x86).to_string();
        let darwin = std::fs::read(law.path("ffi_laws/libffi_laws-aarch64-apple-darwin.a")).unwrap();
        let darwin_pin = ObjectKind::ForeignArchive.hash(&darwin);
        let publish = |arm: &[u8]| {
            let arm_pin = ObjectKind::ForeignArchive.hash(arm).to_string();
            law.write("ffi_laws/arm.a", arm);
            law.manifest("ffi_laws", "hirsel.ai/ffi_laws", "1.0.0", &format!(r#"sources: [("laws.hirsel", "laws"), ("native.hirsel", "native"), ("main.hirsel", "main")], dependencies: [], foreign: Some([
    ("native", Native(
        symbols: "hfl_",
        archives: [
            ArchiveFile(target: "x86_64-unknown-linux-gnu", file: "libffi_laws.a", sha256: "{x86_pin}"),
            ArchiveFile(target: "aarch64-unknown-linux-gnu", file: "arm.a", sha256: "{arm_pin}"),
            ArchiveFile(
                target: "aarch64-apple-darwin",
                file: "libffi_laws-aarch64-apple-darwin.a",
                sha256: "{darwin_pin}",
            ),
        ],
    )),
]),"#));
            PackageLaw::pass(law.run(&[
                "build", "--emit", "recipe", "ffi_laws", "--backend", engine, "--grant", "Foreign=hirsel.ai/ffi_laws@1/laws/Laws",
            ])).trim().to_owned()
        };
        let first = publish(b"");
        let second = publish(b"abc");
        assert_ne!(first, second);
        law.write("ffi_laws/main.hirsel", r#"
fn probe(value: Option[Foreign[laws.Laws]]) -> i32 {
    match value { Some(f) => native.touch(f), None => 0 }
}
pub fn main() -> Result[(), Exit] {
    if probe(None) == 0 { Ok(()) } else { Error(Exit(status: 4)) }
}
"#);
        let without_grant = PackageLaw::pass(law.run(&["build", "--emit", "recipe", "ffi_laws", "--backend", engine]));
        std::fs::remove_dir_all(law.path("ffi_laws")).unwrap();
        let build = |root: &str| law.run(&["build", "--recipe", root, "-o", "sealed-program"]);
        for root in [&first, &second] {
            PackageLaw::pass(build(root));
            let run = Command::new(law.path("sealed-program")).output().unwrap();
            assert!(run.status.success(), "{}", String::from_utf8_lossy(&run.stderr));
            assert_eq!(run.stderr, b"freed 3\nfreed 2\nfreed 1\n");
        }
        PackageLaw::pass(build(without_grant.trim()));
        let run = Command::new(law.path("sealed-program")).output().unwrap();
        assert!(run.status.success(), "{run:?}");
        assert!(run.stderr.is_empty(), "{run:?}");
        let root = crate::native::recipe_build::parse_root(&second).unwrap();
        let bytes = std::fs::read(store.path(ObjectKind::Recipe, root)).unwrap();
        let mut recipe = Recipe::decode(&bytes, &Debit::unlimited()).unwrap();
        let [selected] = recipe.foreign_archives.as_slice() else { panic!("one selected target") };
        let selected = *selected;
        recipe.foreign_archives.clear();
        let missing = store.put(ObjectKind::Recipe, &recipe.encode()).unwrap().hash;
        let (status, _, error) = build(&missing.to_string());
        assert_eq!(status, Some(2), "{error}");
        assert!(error.contains("Missing"), "{error}");
        let extra = store.put(ObjectKind::ForeignArchive, b"abc").unwrap();
        recipe.foreign_archives = vec![selected, extra];
        recipe.foreign_archives.sort_by_key(|archive| archive.hash);
        let extra = store.put(ObjectKind::Recipe, &recipe.encode()).unwrap().hash;
        let (status, _, error) = build(&extra.to_string());
        assert_eq!(status, Some(2), "{error}");
        assert!(error.contains("Extra"), "{error}");
    }
}
