use std::process::{Command, Output};

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hirsel"))
        .args(args)
        .env("HIRSEL_DAEMON", "off")
        .output()
        .unwrap()
}

#[test]
fn help_describes_native_defaults_and_rejects_retired_options() {
    for topic in ["run", "test"] {
        let output = cli(&[topic, "--help"]);
        assert!(output.status.success(), "{output:?}");
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("--backend"), "{help}");
        assert!(
            help.contains(if topic == "run" { "Cranelift" } else { "LLVM" }),
            "{help}"
        );
        for option in [
            "--simulation",
            "--max-steps",
            "--race",
            "--sim-points",
            "--native",
        ] {
            assert!(!help.contains(option), "{help}");
            let output = cli(&[topic, option, "missing.hir"]);
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            assert!(!output.stderr.is_empty());
        }
    }
}

#[test]
fn native_defaults_run_and_test_and_preserve_exit_codes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("app.hir");
    std::fs::write(&path, "pub fn main() -> Result[(), u8] { Ok(()) }\n@test\nfn passes(t: Test) { test.check(t, true) }\n@test\nfn fails(t: Test) { test.check(t, false) }\n").unwrap();
    let path = path.to_str().unwrap();
    // The unqualified run uses Cranelift and the unqualified test uses LLVM.
    for args in [
        vec!["run", path],
        vec!["run", "--backend", "llvm", path],
        vec!["test", path, "passes"],
        vec!["test", "--backend", "cranelift", path, "passes"],
    ] {
        let output = cli(&args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {output:?}");
    }
    let output = cli(&["test", path, "fails"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let output = cli(&["test", path, "does_not_exist"]);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no tests match"),
        "{output:?}"
    );
    std::fs::write(path, "pub fn main(rt: Io) -> Result[(), u8] { Ok(()) }\n").unwrap();
    let output = cli(&["run", path]);
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--grants"),
        "{output:?}"
    );
    std::fs::write(
        path,
        "pub fn main() -> Result[(), u8] { let x = 1u64 / 0u64\n Ok(()) }\n",
    )
    .unwrap();
    let output = cli(&["run", path]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("runtime failure[SEM-1]"),
        "{output:?}"
    );
}
