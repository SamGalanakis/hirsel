//! AArch64 source agreement through production definition units.
#![allow(dead_code)]
const NATIVE: hirsel_core_layout::Target = hirsel_core_layout::Target::Aarch64Linux;
hirsel_guard::install!();
include!("support/cranelift_source.rs");
#[test]
fn front_end_run_cases() {
    check_front_end_run_cases();
}

#[test]
fn failure_sites_in_dev_and_speed() {
    check_failure_sites();
}

#[test]
fn float_nan_paths_in_dev_and_speed() {
    check_float_nan_paths();
}

#[test]
fn growth_shadow_copies_back_data_and_capacity() {
    check_growth_shadow_words();
}

#[test]
fn wasm_laws() {
    let driver_laws = input_path("HIRSEL_CONTAINED_DRIVER_TESTS");
    let driver_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../hirsel-core-driver");
    let cases = [
        "native::tests::contained_modules_are_admitted_before_anything_compiles",
        "native::tests::contained_calls_run_in_the_tasks_own_instance",
        "native::tests::contained_tests_record_and_replay",
        "native::tests::contained_recipes_pin_their_module",
    ];
    let mut passed = true;
    for engine in ["cranelift", "llvm"] {
        let output = Command::new(&driver_laws)
            .current_dir(&driver_dir)
            .args([
                "--exact",
                "--include-ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .args(cases)
            .env("HIRSEL_CONTAINED_TEST_ENGINE", engine)
            .env(
                "HIRSEL_CONTAINED_RUNTIME",
                input_path("HIRSEL_CONTAINED_RUNTIME"),
            )
            .env(
                "HIRSEL_CONTAINED_RECORD_RUNTIME",
                input_path("HIRSEL_CONTAINED_RECORD_RUNTIME"),
            )
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        println!("{engine} aarch64 contained laws\n{stdout}\n{stderr}");
        passed &= output.status.success()
            && cases
                .iter()
                .all(|case| stdout.contains(&format!("test {case} ... ok")));
    }
    assert!(passed, "aarch64 contained laws failed");
}
