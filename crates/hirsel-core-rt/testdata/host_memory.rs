//! Shared native FAIL-15 oracle, compiled independently by both emitters.
use hirsel_core_ir::{FuncId, Module};
use std::{fs, path::PathBuf, process::Command};

#[allow(dead_code)] // Each engine uses one of these ABIs during the site cutover.
pub fn check(emit: impl FnOnce(&Module) -> Vec<u8>, symbol: impl Fn(&Module, FuncId) -> String) {
    check_abi(emit, symbol, false, false);
}
/// FORM#91 internal caller-site argument; the test process ABI is unchanged.
#[allow(dead_code)]
pub fn check_with_site(
    emit: impl FnOnce(&Module) -> Vec<u8>,
    symbol: impl Fn(&Module, FuncId) -> String,
) {
    check_abi(emit, symbol, true, false);
}
/// The same real-null oracle on ARM, where QEMU ignores guest RLIMIT_AS.
#[allow(dead_code)]
pub fn check_aarch64(
    emit: impl FnOnce(&Module) -> Vec<u8>,
    symbol: impl Fn(&Module, FuncId) -> String,
) {
    check_abi(emit, symbol, true, true);
}
fn check_abi(
    emit: impl FnOnce(&Module) -> Vec<u8>,
    symbol: impl Fn(&Module, FuncId) -> String,
    with_site: bool,
    aarch64: bool,
) {
    let module = hirsel_core_ir::text::parse(include_str!("host_memory.core")).unwrap();
    hirsel_core_ir::wf::module(&module).unwrap();
    let directory = std::env::temp_dir().join(format!(
        "hirsel-native-host-memory-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&directory).unwrap();
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            if std::thread::panicking() {
                eprintln!("native host-memory artifacts: {}", self.0.display());
            } else {
                fs::remove_dir_all(&self.0).unwrap();
            }
        }
    }
    let _scratch = Scratch(directory.clone());
    for (name, text) in [
        (
            "hirsel_core_rt.h",
            include_str!("../include/hirsel_core_rt.h"),
        ),
        (
            "hirsel_core_rt_host.h",
            include_str!("../include/hirsel_core_rt_host.h"),
        ),
        (
            "hirsel_core_rt_sites.h",
            include_str!("../include/hirsel_core_rt_sites.h"),
        ),
        (
            "hirsel_core_rt_tasks.h",
            include_str!("../include/hirsel_core_rt_tasks.h"),
        ),
    ] {
        fs::write(directory.join(name), text).unwrap();
    }
    fs::write(directory.join("input.o"), emit(&module)).unwrap();
    let names: Vec<_> = (0..module.functions.len())
        .map(|n| symbol(&module, FuncId(n as u32)))
        .collect();
    let site_parameter = if with_site { ", uint64_t" } else { "" };
    let declarations = names
        .iter()
        .map(|name| {
            format!(
                "extern uint8_t {name}(HirselCoreContext *, void *, uint64_t{site_parameter});\n"
            )
        })
        .collect::<String>();
    let source = include_str!("host_memory.c")
        .replace("@@DECLARATIONS@@", &declarations)
        .replace("@@SITE_PARAMETER@@", site_parameter)
        .replace("@@SITE_ARGUMENT@@", if with_site { ", 0" } else { "" })
        .replace("@@SYMBOLS@@", &names.join(","));
    fs::write(directory.join("caller.c"), source).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let archive = PathBuf::from(
        std::env::var_os("HIRSEL_CORE_RT_ARCHIVE")
            .or_else(|| std::env::var_os("HIRSEL_CORE_RUNTIME_ARCHIVE"))
            .expect("native runtime test archive"),
    );
    let archive = if archive.is_absolute() {
        archive
    } else {
        root.join(archive)
    };
    let input = |name: &str| {
        let path = PathBuf::from(std::env::var_os(name).expect("declared native toolchain"));
        if path.is_absolute() {
            path
        } else {
            root.join(path)
        }
    };
    let cc = if aarch64 {
        std::env::var_os("HIRSEL_AARCH64_CC")
            .map(PathBuf::from)
            .unwrap_or_else(|| input("HIRSEL_TOOLCHAIN_ARM").join("bin/aarch64-none-linux-gnu-gcc"))
    } else {
        std::env::var_os("LLVM_SYS_231_PREFIX")
            .map(|p| PathBuf::from(p).join("bin/clang"))
            .unwrap_or_else(|| "cc".into())
    };
    let linked = Command::new(cc)
        .args(["-std=c11", "-O0", "-no-pie", "-I"])
        .arg(&directory)
        .arg(directory.join("caller.c"))
        .arg(directory.join("input.o"))
        .arg(archive)
        .args([
            "-Wl,--wrap=realloc",
            "-lgcc_s",
            "-lpthread",
            "-ldl",
            "-lm",
            "-lrt",
            "-lutil",
            "-o",
        ])
        .arg(directory.join("run"))
        .output()
        .unwrap();
    assert!(
        linked.status.success(),
        "{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let large = if aarch64 { 1u64 << 60 } else { 64 << 20 };
    let invalid_layout = 1u64 << 63;
    let cases = [
        ("consuming native realloc", 8, large),
        ("consuming foreign fallback", 9, large),
        ("consuming invalid layout", 8, invalid_layout),
        ("ordinary inline", 0, large),
        ("foreign-payer growth", 1, large),
        ("first region block", 2, large),
        ("inline region refill", 3, large),
        ("Rc node", 4, large),
        ("Arc node", 5, large),
        ("aligned native entry", 6, large),
        ("wide block layout", 7, large),
        ("ordinary invalid layout", 0, invalid_layout),
        ("growth invalid layout", 1, invalid_layout),
        ("first region invalid layout", 2, invalid_layout),
        ("refill invalid layout", 3, invalid_layout),
        ("native prefix size overflow", 7, u64::MAX - 1023),
    ];
    let mut failures = Vec::new();
    for (name, mode, bytes) in cases {
        let mut command = if aarch64 {
            let runner = std::env::var_os("HIRSEL_QEMU_AARCH64")
                .map(PathBuf::from)
                .unwrap_or_else(|| input("HIRSEL_TOOLCHAIN_QEMU").join("qemu-aarch64"));
            let sysroot = std::env::var_os("HIRSEL_AARCH64_SYSROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    input("HIRSEL_TOOLCHAIN_ARM").join("aarch64-none-linux-gnu/libc")
                });
            let mut command = Command::new(runner);
            command.arg("-L").arg(sysroot).arg(directory.join("run"));
            command
        } else {
            Command::new(directory.join("run"))
        };
        let output = command
            .args([mode.to_string(), bytes.to_string()])
            .output()
            .unwrap();
        if output.status.code() != Some(102)
            || output.stdout != b"entered\n"
            || output.stderr != b"run stop: HostMemory\n"
        {
            failures.push(format!(
                "{name}: {}, stdout {:?}, stderr {:?}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    println!("native HostMemory refusals: 16, each status102 and exact line; no return/cleanup");
}
