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
    let macos = env::var("CARGO_CFG_TARGET_OS").unwrap() == "macos";
    let llvm_library = if macos {
        "libLLVM.23.1.dylib"
    } else {
        "libLLVM.so.23.1"
    };
    println!(
        "cargo:rerun-if-changed={}",
        prefix.join("lib").join(llvm_library).display()
    );
    println!("cargo:rerun-if-env-changed=LLVM_SYS_231_PREFIX");
    for source in [
        &shared.join("counted_exit.h"),
        &shared.join("counter_range.h"),
        &shared.join("forward_reads.h"),
        &shared.join("guarded_check.h"),
        &shared.join("implied_check.h"),
        &shared.join("loop_version.h"),
        &shared.join("pass_cost.h"),
        &shared.join("step_tree.h"),
    ] {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    let shared_objects = ["checked_copy", "release_pipeline", "pass_cost"].map(|name| {
        let source = shared.join(format!("{name}.cpp"));
        println!("cargo:rerun-if-changed={}", source.display());
        let object = out.join(format!("{name}.o"));
        assert!(
            cxx(&prefix, macos)
                .args(["-std=c++17", "-fPIC", "-O2", "-fno-exceptions", "-fno-rtti"])
                .arg("-I")
                .arg(prefix.join("include"))
                .arg("-c")
                .arg(source)
                .arg("-o")
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        object
    });
    for header in ["builder.h", "key.h"] {
        println!(
            "cargo:rerun-if-changed={}",
            source.with_file_name(header).display()
        );
    }
    let compose_objects = ["compose", "builder", "parts", "key", "out_of_memory"].map(|name| {
        let source = source.with_file_name(format!("{name}.cpp"));
        println!("cargo:rerun-if-changed={}", source.display());
        let object = out.join(format!("{name}.o"));
        assert!(
            cxx(&prefix, macos)
                .args([
                    "-std=c++17",
                    "-O2",
                    "-fPIC",
                    "-fvisibility=hidden",
                    "-fno-exceptions",
                    "-fno-rtti",
                ])
                .arg("-I")
                .arg(prefix.join("include"))
                .arg("-I")
                .arg(&shared)
                .arg("-c")
                .arg(source)
                .arg("-o")
                .arg(&object)
                .status()
                .unwrap()
                .success()
        );
        object
    });
    let component_name = if macos {
        "libhirsel_llvm_engine.dylib"
    } else {
        "libhirsel_llvm_engine.so"
    };
    let component = out.join(component_name);
    let mut link = Command::new(prefix.join(if macos {
        "bin/clang"
    } else {
        "bin/hirsel-clang"
    }));
    link.args(["-shared", "-fPIC", "-O2", "-fvisibility=hidden"]);
    if !macos {
        link.args(["-Wl,--build-id=sha1", "-Wl,-z,defs"]);
    }
    link.arg("-I")
        .arg(prefix.join("include"))
        .arg("-I")
        .arg(&shared)
        .arg(&source)
        .args(&shared_objects)
        .args(&compose_objects);
    if macos {
        link.arg("-lc++")
            .arg("-L")
            .arg(prefix.join("lib"))
            .arg("-lLLVM");
    } else {
        link.arg("-l:libstdc++.so.6")
            .arg("-L")
            .arg(prefix.join("lib"))
            .arg("-l:libLLVM.so.23.1");
    }
    assert!(link.arg("-o").arg(&component).status().unwrap().success());
    let engine = if macos {
        prefix.join("lib").join(llvm_library)
    } else {
        prefix.join("engine").join(llvm_library)
    }
    .canonicalize()
    .unwrap();
    println!("cargo:rerun-if-changed={}", engine.display());
    let build_id = if macos {
        mach_uuid(&engine)
    } else {
        elf_build_id(&engine)
    };
    let profile = out
        .ancestors()
        .nth(3)
        .expect("Cargo profile directory")
        .canonicalize()
        .unwrap();
    std::fs::write(out.join("component.rs"), format!(
        "static COMPONENT: &[u8] = include_bytes!({component_name:?});\nconst CARGO_ENGINE: Option<(&str, &str)> = Some(({:?}, {:?}));\nconst LLVM_BUILD_ID: &str = {build_id:?};\n",
        profile.to_str().unwrap(), engine.to_str().unwrap(),
    )).unwrap();
}

/// The C++ compiler: the pinned tree's, against its hermetic libstdc++ on
/// Linux, or the prefix's clang++ against the SDK's libc++ on macOS.
fn cxx(prefix: &std::path::Path, macos: bool) -> Command {
    if macos {
        return Command::new(prefix.join("bin/clang++"));
    }
    let sysroot = prefix.join("sysroot");
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

fn elf_build_id(engine: &std::path::Path) -> String {
    let notes = Command::new("readelf")
        .arg("-n")
        .arg(engine)
        .output()
        .unwrap();
    assert!(
        notes.status.success(),
        "cannot read LLVM engine notes: {}",
        engine.display()
    );
    let notes = String::from_utf8(notes.stdout).unwrap();
    notes
        .lines()
        .find_map(|line| line.trim().strip_prefix("Build ID: "))
        .expect("pinned LLVM engine has no build ID")
        .to_string()
}

/// The Mach-O `LC_UUID`, as lowercase hex without dashes.
fn mach_uuid(engine: &std::path::Path) -> String {
    let out = Command::new("dwarfdump")
        .arg("--uuid")
        .arg(engine)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cannot read LLVM engine UUID: {}",
        engine.display()
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let uuid = text
        .split_whitespace()
        .nth(1)
        .expect("LLVM engine has no UUID");
    uuid.replace('-', "").to_ascii_lowercase()
}
