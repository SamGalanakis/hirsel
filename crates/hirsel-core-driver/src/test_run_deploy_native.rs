impl PackageLaw {
    fn operator_state(&self) -> (tempfile::TempDir, PathBuf, PathBuf) {
        use crate::launcher::{admit, private_dir};
        use std::os::unix::fs::PermissionsExt;

        let parent = std::env::var_os("HIRSEL_OPERATOR_TEST_ROOT").unwrap_or_else(|| "/tmp".into());
        let root = tempfile::tempdir_in(parent).unwrap();
        let base = root.path().canonicalize().unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let state = private_dir(base.join("state")).unwrap();
        admit::write_envelope(&state, &admit::limits()).unwrap();
        let pin = base.join("context.toml");
        std::fs::write(
            &pin,
            format!(
                "kind = \"host\"\nstate = {:?}\nauthority = \"00000000000000000000000000000001\"\nuid = {}\n",
                state,
                crate::operator::operator_uid()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&pin, std::fs::Permissions::from_mode(0o600)).unwrap();
        (root, state, pin)
    }
    fn group_spec(&self, grants: &str, recording: bool) -> PathBuf {
        let recording = if recording {
            ", recording: Some(Recording(dir: \"recordings\", retention_days: 30))"
        } else {
            ""
        };
        self.write("group.hcfg",&format!("host_v1.Group(name: \"app\", tenant: \"operator\", listen: \"127.0.0.1:0\", store: \"store\", workdir: \".\", trusted: true, grants: [{grants}], package_roots: [], args: []{recording})"));
        self.path("group.hcfg")
    }
}

#[test]
fn foreign_deploy_and_recording_registration_refuse_before_payloads() {
    use crate::code_admission::{
        AdmissionError, AdmissionStore, ControlConnection, NativeSemantics,
    };
    use crate::code_store::CodeStore;
    use std::{fs, os::unix::net::UnixStream, time::Duration};

    const GRANT: &str = "Foreign=hirsel.ai/ffi_laws@1/laws/Laws";
    const REFUSAL: &str = "FFI-8: a recording build cannot reach foreign code: hirsel.ai/ffi_laws@1/main/main reaches hirsel.ai/ffi_laws@1/native/touch";

    let law = PackageLaw::new();
    for file in [
        "hirsel.toml",
        "laws.hirsel",
        "native.hirsel",
        "tests.hirsel",
        "libffi_laws.a",
        "libffi_laws-aarch64-apple-darwin.a",
    ] {
        let contents = fs::read(law.libraries.join("programs/ffi_laws").join(file)).unwrap();
        law.write(&format!("ffi_laws/{file}"), contents);
    }
    law.write(
        "ffi_laws/main.hirsel",
        r#"
pub fn main(f: Foreign[laws.Laws]) -> Result[(), Exit] {
    let _ = native.touch(f)
    Ok(())
}
"#,
    );
    let source = law.path("code-store");
    // The host has published bounds, but each refusal must come before connecting.
    let (_operator_root, state, pin) = law.operator_state();
    let envelope = fs::read(state.join("admission")).unwrap();
    let request = |args: &[&str]| {
        let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        process::tape_request(&args, law.dir.path())
    };
    for engine in ["cranelift", "llvm"] {
        let root = PackageLaw::pass(law.run(&[
            "publish",
            "ffi_laws",
            "--backend",
            engine,
            "--grant",
            GRANT,
        ]));
        let root = root.trim();
        for (grants, record, reason) in [
            ("", false, format!("the recipe grants {GRANT}")),
            (
                "Foreign(\"hirsel.ai/ffi_laws@1/laws/Laws\")",
                true,
                REFUSAL.to_owned(),
            ),
        ] {
            let spec = law.group_spec(grants, record);
            let (status, out, error) = request(&[
                "deploy",
                "--recipe",
                root,
                "--context",
                pin.to_str().unwrap(),
                "--create",
                spec.to_str().unwrap(),
                "--code-store",
                source.to_str().unwrap(),
                "--json",
            ]);
            assert_eq!(status, Some(2), "{out}\n{error}");
            let payload: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(payload["error"]["detail"], reason);
        }
        assert_eq!(fs::read(state.join("admission")).unwrap(), envelope);
        assert_eq!(fs::read_dir(&state).unwrap().count(), 1);
        assert!(!law.path("recordings").exists());

        // A recording admission refuses the attested recipe before any Need, so no
        // object is staged.
        let root = crate::native::recipe_build::parse_root(root).unwrap();
        let (mut client, peer) = UnixStream::pair().unwrap();
        let code = law.path("host-code");
        let mut limits = crate::request::local_admission_limits();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        limits.outstanding = 4096;
        std::thread::scope(|scope| {
            let sender = scope.spawn(|| {
                crate::code_admission::send_release_until(
                    &mut client,
                    &CodeStore::new(&source),
                    root,
                    &limits,
                    deadline,
                )
            });
            let approved = BTreeSet::from(["hirsel.ai/ffi_laws@1/laws/Laws".to_owned()]);
            let admission = AdmissionStore::open(&code, 1 << 20).unwrap();
            let mut peer = ControlConnection::local(peer);
            match admission.admit_for_runtime(
                &mut peer,
                &limits,
                deadline,
                &NativeSemantics,
                true,
                Some(&approved),
                None,
                None,
            ) {
                Err(AdmissionError::RecordingForeign(reason)) => {
                    assert_eq!(reason, REFUSAL);
                    peer.refuse_before_need(&reason, deadline).unwrap();
                }
                Err(error) => panic!("recording admission failed otherwise: {error}"),
                Ok(_) => panic!("recording admission admitted foreign code"),
            }
            drop(peer);
            match sender.join().unwrap() {
                Err(AdmissionError::Refused(reason)) => assert_eq!(reason, REFUSAL),
                other => panic!("sender saw {other:?}"),
            }
        });
        fs::remove_dir_all(&code).unwrap();
    }
}

#[test]
fn exec_test_and_deploy_require_the_exact_profile_approval() {
    let law = PackageLaw::new();
    law.app(
        "app",
        "[tests]\nlimit = 67108864\ngrants = [\"Exec=profiles.toml git\"]\n",
    );
    law.write(
        "app/main.hirsel",
        "pub fn main(e: Exec) -> Result[(), Exit] { Ok(()) }\n\
         @test\nfn granted(e: Exec) -> Result[(), ()] { Ok(()) }\n\
         @test\nfn ungranted() -> Result[(), ()] { Ok(()) }\n",
    );
    law.write(
        "profiles.toml",
        "[git]\nimage = \"/unused\"\nsha256 = \"0000000000000000000000000000000000000000000000000000000000000000\"\n\
         argv0 = \"git\"\nargs = [[]]\nenv = {}\ndirs = { work = \"/unused\" }\n",
    );
    let (status, stdout, stderr) = law.run(&[
        "test",
        "app",
        "--json",
        "--grant",
        "Exec=./profiles.toml git",
    ]);
    assert_eq!(status, Some(1), "{stdout}\n{stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let tests = json["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 2);
    assert_eq!(tests[0]["definition"], "local/app@1/main/granted");
    assert_eq!(tests[0]["passed"], false);
    assert_eq!(tests[1]["definition"], "local/app@1/main/ungranted");
    assert_eq!(tests[1]["passed"], true);
    assert!(
        stdout.contains(
            "not granted: Exec[git]: pass --grant 'Exec=profiles.toml git' to hirsel test"
        ),
        "{stdout}"
    );

    let source = law.path("code-store");
    let source = source.to_str().unwrap();
    let root = PackageLaw::pass(law.run(&["publish", "app", "--grant", "Exec=profiles.toml git"]));
    let (_operator_root, _state, pin) = law.operator_state();
    let spec = law.group_spec(
        "Exec(policy: \"./profiles.toml\", profiles: [\"git\"])",
        false,
    );
    let args = [
        "deploy",
        "--recipe",
        root.trim(),
        "--code-store",
        source,
        "--context",
        pin.to_str().unwrap(),
        "--create",
        spec.to_str().unwrap(),
        "--json",
    ]
    .map(str::to_owned);
    let (status, stdout, stderr) = process::tape_request(&args, law.dir.path());
    assert_eq!(status, Some(2), "{stdout}\n{stderr}");
    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        envelope["error"]["detail"],
        "the recipe grants Exec=profiles.toml git"
    );
}
