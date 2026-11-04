use std::{fs, path::PathBuf, process::Command};

use super::{checked, qual_native_output};

#[test]
fn contended_free_mutex_keeps_role_borrows_on_its_worker() {
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    fs::write(
        root.join("mutex-borrow.c"),
        include_str!("../../../../hirsel-rt/harness/mutex-borrow.c"),
    )
    .unwrap();
    fs::write(
        root.join("hirsel_rt_v2.h"),
        include_str!("../../../../hirsel-rt/include/hirsel_rt_v2.h"),
    )
    .unwrap();
    let llvm = PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap());
    let archives = PathBuf::from(std::env::var_os("HIRSEL_RUNTIME_ARCHIVES").unwrap());
    let executable = root.join("mutex-borrow");
    checked(qual_native_output(
        Command::new(llvm.join("bin/clang"))
            .arg(format!("--ld-path={}", llvm.join("bin/ld.lld").display()))
            .args(["-std=c11", "-O2", "-Wall", "-Wextra", "-Werror"])
            .arg(root.join("mutex-borrow.c"))
            .arg(archives.join("x86_64-unknown-linux-gnu/diagnostic/libhirsel_rt.a"))
            .args([
                "-lgcc_s",
                "-lutil",
                "-lrt",
                "-lpthread",
                "-lm",
                "-ldl",
                "-o",
            ])
            .arg(&executable),
        root,
    ));
    for width in [1, 2, 4, 8] {
        let grants = root.join("grants.json");
        fs::write(
            &grants,
            serde_json::json!({
                "io": {"tasks": "tasks.free@1", "dirs": []},
                "placement": {"cores": width}
            })
            .to_string(),
        )
        .unwrap();
        let output = checked(qual_native_output(
            Command::new(&executable)
                .arg("--hirsel-grants")
                .arg(grants)
                .env("HIRSEL_RT_MIGRATE", "1")
                .env("RUST_BACKTRACE", "1"),
            root,
        ));
        assert_eq!(output.stdout, b"mutex total 15\n", "width {width}");
        assert!(output.stderr.is_empty(), "width {width}: {output:?}");
        eprintln!("contended Free mutex width {width}: eight increments, total 15");
    }
}
