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
        shared.join("failure_edges.h").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        prefix.join("lib/libLLVM.so.23.1").display()
    );
    println!("cargo:rerun-if-env-changed=LLVM_SYS_231_PREFIX");
    for source in [
        &shared.join("forward_reads.h"),
        &shared.join("loop_version.h"),
        &shared.join("step_tree.h"),
    ] {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    let sysroot = prefix.join("sysroot");
    let shared_objects = ["checked_copy", "release_pipeline"].map(|name| {
        let source = shared.join(format!("{name}.cpp"));
        println!("cargo:rerun-if-changed={}", source.display());
        let object = out.join(format!("{name}.o"));
        assert!(Command::new(prefix.join("bin/hirsel-clang++"))
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
            .arg(source)
            .arg("-o")
            .arg(&object)
            .status()
            .unwrap()
            .success());
        object
    });
    let compose = source.with_file_name("compose.cpp");
    println!("cargo:rerun-if-changed={}", compose.display());
    let compose_object = out.join("compose.o");
    assert!(
        Command::new(prefix.join("bin/hirsel-clang++"))
            .args([
                "-std=c++17",
                "-O2",
                "-fPIC",
                "-fvisibility=hidden",
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
            .arg(&compose)
            .arg("-o")
            .arg(&compose_object)
            .status()
            .unwrap()
            .success()
    );
    let builder = source.with_file_name("builder.cpp");
    println!("cargo:rerun-if-changed={}", builder.display());
    println!("cargo:rerun-if-changed={}", source.with_file_name("builder.h").display());
    let builder_object = out.join("builder.o");
    assert!(Command::new(prefix.join("bin/hirsel-clang++"))
        .args(["-std=c++17", "-O2", "-fPIC", "-fvisibility=hidden", "-fno-exceptions", "-fno-rtti", "-nostdinc++"])
        .arg(format!("--sysroot={}", sysroot.display()))
        .arg("-isystem").arg(sysroot.join("usr/include/c++/13"))
        .arg("-isystem").arg(sysroot.join("usr/include/x86_64-linux-gnu/c++/13"))
        .arg("-I").arg(prefix.join("include"))
        .arg("-c").arg(&builder).arg("-o").arg(&builder_object)
        .status().unwrap().success());
    let parts = source.with_file_name("parts.cpp");
    println!("cargo:rerun-if-changed={}", parts.display());
    let parts_object = out.join("parts.o");
    assert!(Command::new(prefix.join("bin/hirsel-clang++"))
        .args(["-std=c++17", "-O2", "-fPIC", "-fvisibility=hidden", "-fno-exceptions", "-fno-rtti", "-nostdinc++"])
        .arg(format!("--sysroot={}", sysroot.display()))
        .arg("-isystem").arg(sysroot.join("usr/include/c++/13"))
        .arg("-isystem").arg(sysroot.join("usr/include/x86_64-linux-gnu/c++/13"))
        .arg("-I").arg(prefix.join("include"))
        .arg("-c").arg(&parts).arg("-o").arg(&parts_object)
        .status().unwrap().success());
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
            .args(&shared_objects)
            .arg(&compose_object)
            .arg(&builder_object)
            .arg(&parts_object)
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
