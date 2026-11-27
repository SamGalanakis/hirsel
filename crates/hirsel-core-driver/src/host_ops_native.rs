fn host_operations(backend: Backend) {
    crate::openat2_required::require();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir_only: &[&str] = &["Dir"];
    let dir_tasks: &[&str] = &["Dir", "Tasks"];
    let sleep: &[&str] = &["Tasks", "Clock", "Stdout"];
    for (name, expected_exit, grants) in [
        ("host-read", 0, dir_only),
        ("host", 0, dir_only),
        ("host-close-cancel", 0, dir_tasks),
        ("clocks", 0, &["Clock"]),
        ("mkdtemp-bounds", 101, dir_only),
        ("mkdtemp", 0, dir_only),
        ("host-wait-cancel", 0, dir_tasks),
        ("host-rename-sleep", 0, &["Dir", "Clock"]),
        ("sleep-peer", 0, sleep),
        ("sleep-cancel", 0, sleep),
        ("parent-failure", 0, sleep),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant");
        std::fs::create_dir(&grant).unwrap();
        if name == "host-read" || name == "host-close-cancel" || name == "host-wait-cancel" {
            std::fs::write(grant.join("input"), b"hello").unwrap();
        }
        let source =
            std::fs::read_to_string(root.join(format!("core/front/runtime/{name}.hirsel")))
                .unwrap();
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        if name == "mkdtemp" || name == "host-wait-cancel" {
            let module = request.inputs.packages[1]
                .sources
                .iter_mut()
                .find(|s| s.modules == ["string"])
                .unwrap();
            let mut text = std::fs::read_to_string(&module.file).unwrap();
            text.push_str("\npub fn proof_temp_name() -> String { let b:Bytes = list.with_capacity(32); of_bytes(take b) }\n");
            module.file = dir.path().join("string.hirsel");
            std::fs::write(&module.file, text).unwrap();
        }
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let options = NativeOptions {
            directories: if grants.contains(&"Dir") {
                vec![grant.to_str().unwrap().into()]
            } else {
                Vec::new()
            },
            grant_mask: grants
                .iter()
                .fold(0, |mask, kind| mask | 1 << grant_kind(kind).unwrap()),
            ..core_ops_options(backend, &root, dir.path())
        };
        let built = product.build(&entry, &options).unwrap();
        let output = Command::new(built.executable).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected_exit),
            "{name}: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        let want_stdout = match name {
            "sleep-peer" => b"PS".as_slice(),
            "sleep-cancel" => b"C",
            "parent-failure" => b"F",
            _ => b"",
        };
        assert_eq!(output.stdout, want_stdout, "{name}");
        if expected_exit == 0 {
            assert!(
                output.stderr.is_empty(),
                "{name}: {:?}",
                String::from_utf8_lossy(&output.stderr)
            );
        } else {
            assert!(
                output.stderr.starts_with(b"Bounds"),
                "{name}: {:?}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if name == "host-read" || name == "host-close-cancel" || name == "host-wait-cancel" {
            assert_eq!(std::fs::read(grant.join("input")).unwrap(), b"hello");
        } else {
            assert_eq!(std::fs::read_dir(&grant).unwrap().count(), 0, "{name}");
        }
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn host_operations_cranelift() {
    host_operations(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn host_operations_llvm() {
    host_operations(Backend::Llvm);
}
