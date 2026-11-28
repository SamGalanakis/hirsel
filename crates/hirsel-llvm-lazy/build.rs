use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};
fn main() -> Result<(), String> {
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
    // A metadata-only check of another target (tools/apple_check.py) builds no
    // engine: it needs the target's SDK and engine, which this host lacks. The
    // library it would link names nothing, so a skipped build never links.
    println!("cargo:rerun-if-env-changed=HIRSEL_METADATA_CHECK");
    if env::var_os("HIRSEL_METADATA_CHECK").is_some() {
        println!("cargo:rustc-link-lib=hirsel-metadata-check-builds-no-engine");
        std::fs::write(
            out.join("component.rs"),
            "static COMPONENT: &[u8] = &[];\nconst CARGO_ENGINE: Option<(&str, &str)> = None;\nconst LLVM_BUILD_ID: &str = \"\";\nconst LLVM_LIBRARY: &str = \"\";\nconst BUCK_ENGINE: Option<&str> = None;\n",
        )
        .unwrap();
        return Ok(());
    }
    let product_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/toolchain-dist/product.hcfg");
    println!("cargo:rerun-if-changed={}", product_path.display());
    let product: hirsel_config_schema::product_v1::Product =
        hirsel_config_schema::read_file(&product_path)
            .map_err(|error| format!("{}: {error}", product_path.display()))?;
    let target = env::var("TARGET").unwrap();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
        return Err(format!("no LLVM engine build for {target}"));
    }
    let artifacts = product
        .hosts
        .iter()
        .find(|(triple, _)| triple == &target)
        .ok_or_else(|| format!("no host artifacts for {target}"))?;
    let host = Host::of(&prefix, &artifacts.1);
    let buck_engine = match host.os {
        Os::Linux => Some(
            Path::new("../../../../../../tools/buck2/toolchains/llvm-files").join(&host.engine),
        ),
        Os::Macos => None,
    };
    println!(
        "cargo:rerun-if-changed={}",
        prefix.join(host.library).display()
    );
    let shared_objects = ["checked_copy", "release_pipeline", "pass_cost"].map(|name| {
        let source = shared.join(format!("{name}.cpp"));
        println!("cargo:rerun-if-changed={}", source.display());
        let object = out.join(format!("{name}.o"));
        assert!(
            host.cxx()
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
            host.cxx()
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
    let component = out.join(host.component);
    assert!(
        host.c()
            .args(["-fPIC", "-O2", "-fvisibility=hidden"])
            .args(host.shared)
            .arg("-I")
            .arg(prefix.join("include"))
            .arg("-I")
            .arg(&shared)
            .arg(&source)
            .args(&shared_objects)
            .args(&compose_objects)
            .arg(host.cxx_runtime)
            .arg("-L")
            .arg(prefix.join(host.library.parent().unwrap()))
            .arg(host.link_llvm())
            .arg("-o")
            .arg(&component)
            .status()
            .unwrap()
            .success()
    );
    let engine = prefix.join(&host.engine).canonicalize().unwrap();
    println!("cargo:rerun-if-changed={}", engine.display());
    let build_id = host.identity(&prefix, &engine);
    let profile = out
        .ancestors()
        .nth(3)
        .expect("Cargo profile directory")
        .canonicalize()
        .unwrap();
    std::fs::write(out.join("component.rs"), format!(
        "static COMPONENT: &[u8] = include_bytes!({:?});\nconst CARGO_ENGINE: Option<(&str, &str)> = Some(({:?}, {:?}));\nconst LLVM_BUILD_ID: &str = {build_id:?};\nconst LLVM_LIBRARY: &str = {:?};\nconst BUCK_ENGINE: Option<&str> = {buck_engine:?};\n",
        host.component, profile.to_str().unwrap(), engine.to_str().unwrap(), host.library.file_name().unwrap(),
    )).unwrap();
    Ok(())
}

/// How this host builds the engine component. The driver is a host binary, so the
/// target OS Cargo builds for is the host the compiler runs on.
struct Host<'a> {
    os: Os,
    sysroot: PathBuf,
    /// The component's file name, embedded in the driver.
    component: &'static str,
    /// The shared library path in both a pinned tree and an installation.
    library: &'a Path,
    /// The engine inside the LLVM prefix the build checks the component against.
    engine: PathBuf,
    shared: &'static [&'static str],
    cxx_runtime: &'static str,
}

#[derive(Clone, Copy)]
enum Os {
    /// The pinned tree's clang with its hermetic C++ sysroot; libstdc++.
    Linux,
    /// The host's Xcode clang and SDK; libc++.
    Macos,
}

impl<'a> Host<'a> {
    fn of(prefix: &Path, artifacts: &'a hirsel_config_schema::product_v1::Host) -> Self {
        let library = Path::new(&artifacts.llvm_library);
        let engine = artifacts.llvm_engine_dir.as_deref().map_or_else(
            || library.to_owned(),
            |dir| Path::new(dir).join(library.file_name().unwrap()),
        );
        match env::var("CARGO_CFG_TARGET_OS").unwrap().as_str() {
            "linux" => {
                let sysroot = prefix.join("sysroot");
                assert!(
                    sysroot.join("usr/include/c++/13/vector").is_file(),
                    "run python3 tools/buck2/prepare.py for hermetic C++ headers"
                );
                Self {
                    os: Os::Linux,
                    sysroot,
                    component: "libhirsel_llvm_engine.so",
                    library,
                    engine,
                    shared: &["-shared", "-Wl,--build-id=sha1", "-Wl,-z,defs"],
                    cxx_runtime: "-l:libstdc++.so.6",
                }
            }
            "macos" => {
                let sdk = Command::new("xcrun")
                    .args(["--sdk", "macosx", "--show-sdk-path"])
                    .output()
                    .expect("xcrun: install the Xcode command line tools");
                assert!(sdk.status.success(), "xcrun found no macOS SDK");
                let sdk = String::from_utf8(sdk.stdout).unwrap();
                Self {
                    os: Os::Macos,
                    sysroot: PathBuf::from(sdk.trim()),
                    component: "libhirsel_llvm_engine.dylib",
                    library,
                    engine,
                    shared: &["-dynamiclib", "-Wl,-undefined,error"],
                    cxx_runtime: "-lc++",
                }
            }
            os => panic!("no LLVM engine build for host OS {os}"),
        }
    }

    fn link_llvm(&self) -> String {
        let name = self.library.file_name().unwrap().to_str().unwrap();
        match self.os {
            Os::Linux => format!("-l:{name}"),
            Os::Macos => {
                let name = name
                    .strip_prefix("lib")
                    .unwrap()
                    .strip_suffix(".dylib")
                    .unwrap();
                format!("-l{name}")
            }
        }
    }

    fn c(&self) -> Command {
        match self.os {
            // The component links against the build host's C runtime, as before the split.
            Os::Linux => Command::new(self.tool("hirsel-clang")),
            Os::Macos => self.xcrun("clang"),
        }
    }

    fn cxx(&self) -> Command {
        match self.os {
            Os::Linux => {
                let mut command = Command::new(self.tool("hirsel-clang++"));
                command
                    .arg("-nostdinc++")
                    .arg(format!("--sysroot={}", self.sysroot.display()))
                    .arg("-isystem")
                    .arg(self.sysroot.join("usr/include/c++/13"))
                    .arg("-isystem")
                    .arg(self.sysroot.join("usr/include/x86_64-linux-gnu/c++/13"));
                command
            }
            Os::Macos => {
                let mut command = self.xcrun("clang++");
                command.arg("-stdlib=libc++");
                command
            }
        }
    }

    fn tool(&self, name: &str) -> PathBuf {
        PathBuf::from(env::var_os("LLVM_SYS_231_PREFIX").unwrap())
            .join("bin")
            .join(name)
    }

    fn xcrun(&self, tool: &str) -> Command {
        let mut command = Command::new("xcrun");
        command
            .args(["--sdk", "macosx", tool])
            .arg("-isysroot")
            .arg(&self.sysroot);
        command
    }

    /// The engine's link-time identity, as the loader reads it from the file it opens:
    /// the ELF build ID or the Mach-O LC_UUID, in lowercase hex.
    fn identity(&self, prefix: &Path, engine: &Path) -> String {
        let (tool, args, field): (_, &[&str], _) = match self.os {
            Os::Linux => (PathBuf::from("readelf"), &["-n"], "Build ID: "),
            Os::Macos => (
                prefix.join("bin/llvm-objdump"),
                &["--macho", "--private-headers"],
                "uuid ",
            ),
        };
        let notes = Command::new(tool).args(args).arg(engine).output().unwrap();
        assert!(
            notes.status.success(),
            "cannot read the LLVM engine's identity: {}",
            engine.display()
        );
        let notes = String::from_utf8(notes.stdout).unwrap();
        let id = notes
            .lines()
            .find_map(|line| line.trim().strip_prefix(field))
            .expect("pinned LLVM engine has no build ID");
        id.chars()
            .filter(|c| *c != '-')
            .flat_map(char::to_lowercase)
            .collect()
    }
}
