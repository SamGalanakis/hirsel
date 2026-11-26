use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};
fn main() {
    let prefix =
        PathBuf::from(env::var_os("LLVM_SYS_231_PREFIX").expect("prepare the pinned LLVM tree"));
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let sources = [
        "core_passes",
        "checked_copy",
        "release_pipeline",
        "pass_cost",
    ];
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/aggregate_phi.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/failure_edges.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/loop_version.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/counted_exit.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/counter_range.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/source_remark.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/guarded_check.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/implied_check.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/forward_reads.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/step_tree.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("bridge/pass_cost.h").display()
    );
    for name in sources {
        let source = manifest.join(format!("bridge/{name}.cpp"));
        println!("cargo:rerun-if-changed={}", source.display());
        let status = cxx(&prefix)
            .args(["-std=c++17", "-fPIC", "-fno-exceptions", "-fno-rtti"])
            .arg("-I")
            .arg(prefix.join("include"))
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(out.join(format!("{name}.o")))
            .status()
            .unwrap();
        assert!(status.success(), "LLVM option bridge compilation");
    }
    assert!(
        Command::new(prefix.join("bin/llvm-ar"))
            .arg("crs")
            .arg(out.join("libhirsel_llvm_bridge.a"))
            .args(sources.map(|name| out.join(format!("{name}.o"))))
            .status()
            .unwrap()
            .success()
    );
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=hirsel_llvm_bridge");
    match env::var("CARGO_CFG_TARGET_OS").unwrap().as_str() {
        "linux" => println!("cargo:rustc-link-lib=dylib:+verbatim=libstdc++.so.6"),
        "macos" => println!("cargo:rustc-link-lib=dylib=c++"),
        os => panic!("no LLVM bridge build for host OS {os}"),
    }
}

/// The C++ compiler for this host's bridge: the pinned tree's clang with its hermetic
/// libstdc++ headers on Linux, the host's Xcode clang and SDK with libc++ on macOS.
fn cxx(prefix: &Path) -> Command {
    match env::var("CARGO_CFG_TARGET_OS").unwrap().as_str() {
        "linux" => {
            let sysroot = prefix.join("sysroot");
            assert!(
                sysroot.join("usr/include/c++/13/vector").is_file(),
                "run python3 tools/buck2/prepare.py for hermetic C++ headers"
            );
            let mut command = Command::new(prefix.join("bin/hirsel-clang++"));
            command
                .arg("-nostdinc++")
                .arg(format!("--sysroot={}", sysroot.display()))
                .arg("-isystem")
                .arg(sysroot.join("usr/include/c++/13"))
                .arg("-isystem")
                .arg(sysroot.join("usr/include/x86_64-linux-gnu/c++/13"));
            command
        }
        "macos" => {
            let mut command = Command::new("xcrun");
            command.args(["--sdk", "macosx", "clang++", "-stdlib=libc++"]);
            command
        }
        os => panic!("no LLVM bridge build for host OS {os}"),
    }
}
