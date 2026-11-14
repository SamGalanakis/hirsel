use std::{env, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rustc-cfg=hirsel_lazy_core");
    println!("cargo:rustc-check-cfg=cfg(hirsel_lazy_core)");
    let prefix = PathBuf::from(env::var_os("LLVM_SYS_231_PREFIX").expect("prepare pinned LLVM"));
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("bridge/engine.c");
    println!("cargo:rerun-if-changed={}", source.display());
    let shared =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../hirsel-llvm/bridge");
    println!(
        "cargo:rerun-if-changed={}",
        shared.join("aggregate_phi.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        prefix.join("lib/libLLVM.so.23.1").display()
    );
    println!("cargo:rerun-if-env-changed=LLVM_SYS_231_PREFIX");
    let copy_source = shared.join("checked_copy.cpp");
    for source in [&copy_source, &shared.join("loop_version.h")] {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    let copy_object = out.join("checked_copy.o");
    let sysroot = prefix.join("sysroot");
    assert!(
        Command::new(prefix.join("bin/hirsel-clang++"))
            .args([
                "-std=c++17",
                "-fPIC",
                "-O2",
                "-fno-exceptions",
                "-fno-rtti",
                "-nostdinc++"
            ])
            .arg(format!("--sysroot={}", sysroot.display()))
            .arg("-isystem")
            .arg(sysroot.join("usr/include/c++/13"))
            .arg("-isystem")
            .arg(sysroot.join("usr/include/x86_64-linux-gnu/c++/13"))
            .arg("-I")
            .arg(prefix.join("include"))
            .arg("-c")
            .arg(copy_source)
            .arg("-o")
            .arg(&copy_object)
            .status()
            .unwrap()
            .success()
    );
    let component = out.join("libhirsel_llvm_engine.so");
    assert!(
        Command::new(prefix.join("bin/hirsel-clang"))
            .args([
                "-shared",
                "-fPIC",
                "-O2",
                "-fvisibility=hidden",
                "-Wl,--build-id=sha1"
            ])
            .arg("-I")
            .arg(prefix.join("include"))
            .arg("-I")
            .arg(&shared)
            .arg(&source)
            .arg(&copy_object)
            .arg("-l:libstdc++.so.6")
            .arg("-L")
            .arg(prefix.join("lib"))
            .arg("-l:libLLVM.so.23.1")
            .arg("-o")
            .arg(&component)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(out.join("component.rs"), format!(
        "static COMPONENT: &[u8] = include_bytes!(\"libhirsel_llvm_engine.so\");\nconst LLVM_LIBRARY: &str = {:?};\n",
        prefix.join("lib/libLLVM.so.23.1").to_str().unwrap(),
    )).unwrap();
}
