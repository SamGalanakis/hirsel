use hirsel_core_ids::embed::{binding_projection, manifest_fingerprint};
use object::read::archive::ArchiveFile;
use object::{Object, ObjectSymbol};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[path = "archive_laws/ios.rs"]
mod ios;

const HOST: &str = include_str!("archive_host.c");
const EXEC_HOST: &str = include_str!("exec_host.c");
const EXEC_FAKE_HELPER: &str = include_str!("exec_fake_helper.c");

struct Fixture {
    compiler: PathBuf,
    runtime: PathBuf,
    llvm: PathBuf,
    kit: PathBuf,
    rust: PathBuf,
    qemu: PathBuf,
    work: PathBuf,
    program: PathBuf,
    libraries: PathBuf,
    args: Vec<String>,
    grants: String,
    store: PathBuf,
    cache: PathBuf,
    run_dir: PathBuf,
}

fn passed(command: &mut Command) -> Vec<u8> {
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{command:?}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    out.stdout
}

fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn copy_package(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_package(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

impl Fixture {
    fn new(work: &Path) -> Self {
        let data = work.join("data");
        let cache_dir = work.join("place-cache");
        let secret = work.join("secret");
        fs::write(&secret, b"test value").unwrap();
        Self::stage(
            work,
            "embed_place",
            &[data, cache_dir],
            &[
                format!("Secret:api_key=file:{} verify", secret.display()),
                "Net=Listen 127.0.0.1/32 0-65535".into(),
                "Clock".into(),
                "Random".into(),
                "Tasks".into(),
                "Process".into(),
                "Stderr".into(),
            ],
        )
    }

    /// The testdata program `name` beside store and runner, built with a
    /// store directory, the directories `dirs` and the grants `grants`.
    fn stage(work: &Path, name: &str, dirs: &[PathBuf], grants: &[String]) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        let input = |name: &str| {
            root.join(std::env::var_os(name).unwrap())
                .canonicalize()
                .unwrap()
        };
        let workspace = work.join("workspace");
        let libraries = workspace.join("libraries");
        let program = workspace.join(name);
        fs::create_dir_all(&program).unwrap();
        fs::create_dir_all(libraries.join("programs")).unwrap();
        fs::write(
            workspace.join("hirsel.toml"),
            format!("[workspace]\nmembers = [\"store\", \"runner\", \"{name}\"]\n"),
        )
        .unwrap();
        for package in ["store", "runner"] {
            let source = root.join("core/programs").join(package);
            let staged = workspace.join(package);
            copy_package(&source, &staged);
            symlink(&staged, libraries.join("programs").join(package)).unwrap();
        }
        for name in ["core", "std"] {
            symlink(root.join("core").join(name), libraries.join(name)).unwrap();
        }
        let source = root.join("crates/hirsel-core-rt/testdata").join(name);
        fs::copy(source.join("hirsel.toml"), program.join("hirsel.toml")).unwrap();
        let file = format!("{name}.hirsel");
        fs::copy(source.join(&file), program.join(&file)).unwrap();

        let llvm = input("LLVM_SYS_231_PREFIX");
        let kit = input("HIRSEL_LINK_KIT");
        let store = work.join("store");
        let mut args = Vec::new();
        let mut text = String::new();
        for dir in std::iter::once(&store).chain(dirs) {
            fs::create_dir(dir).unwrap();
            args.extend(["--dir".into(), path(dir).into()]);
            if dir != &store {
                text.push_str(&format!("Dir={}\n", dir.display()));
            }
        }
        for grant in grants {
            text.push_str(grant);
            text.push('\n');
            args.extend(["--grant".into(), grant.clone()]);
        }
        let grants = text;
        let run_dir = work.join("run");
        fs::DirBuilder::new().mode(0o700).create(&run_dir).unwrap();
        Self {
            compiler: input("HIRSEL_DEV_TEST_DRIVER"),
            runtime: input("HIRSEL_CORE_RT_ARCHIVE"),
            llvm,
            kit,
            rust: input("HIRSEL_ARCHIVE_RUST").join("bin/rustc"),
            qemu: input("HIRSEL_TOOLCHAIN_QEMU").join("qemu-x86_64"),
            work: work.into(),
            program,
            libraries,
            args,
            grants,
            store,
            cache: work.join("compiler-cache"),
            run_dir,
        }
    }

    fn hirsel(&self) -> Command {
        let mut command = Command::new(&self.compiler);
        command
            .current_dir(&self.work)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("LLVM_SYS_231_PREFIX", &self.llvm)
            .env("XDG_RUNTIME_DIR", &self.run_dir);
        command
    }

    fn build(&self, kind: &str, output: &Path, store: &Path, limit: u64, resident: bool) {
        let mut command = self.hirsel();
        command
            .arg("build")
            .arg(&self.program)
            .args(["--emit", kind, "--runtime"])
            .arg(&self.runtime)
            .arg("--libraries")
            .arg(&self.libraries)
            .arg("--store")
            .arg(store)
            .arg("--limit")
            .arg(limit.to_string())
            .args(&self.args)
            .arg("-o")
            .arg(output);
        if resident {
            command.arg("--resident");
        }
        passed(&mut command);
        let magic = fs::read(output).unwrap();
        let expected = if kind == "exe" {
            &b"\x7fELF"[..]
        } else {
            &b"!<arch>\n"[..]
        };
        assert!(
            magic.starts_with(expected),
            "{output:?} returned the wrong kind"
        );
    }

    fn cc(&self) -> Command {
        let mut cc = Command::new(self.llvm.join("bin/clang"));
        cc.arg(format!("--ld-path={}", self.kit.join("ld.lld").display()));
        cc
    }

    fn link_host(&self, archive: &Path, adjacent: &Path) -> PathBuf {
        self.link(HOST, archive, adjacent)
    }

    fn link(&self, text: &str, archive: &Path, adjacent: &Path) -> PathBuf {
        let directory = archive.parent().unwrap();
        let source = directory.join("host.c");
        fs::write(&source, text).unwrap();
        let host = directory.join("host");
        passed(
            self.cc()
                .args([
                    "-O2",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-std=c11",
                    "-pthread",
                    "-Wl,--gc-sections,--wrap=exit,--wrap=_exit",
                ])
                .arg(format!("-DREQUIRE_ANCHOR={}", anchor(archive)))
                .arg(source)
                .arg(archive)
                .arg(adjacent)
                .args(["-ldl", "-lm", "-lresolv"])
                .arg("-o")
                .arg(&host),
        );
        host
    }

    fn adjacent(&self) -> PathBuf {
        let source = self.work.join("adjacent.rs");
        fs::write(
            &source,
            r#"
#![no_std]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! { loop { core::hint::spin_loop(); } }
#[unsafe(no_mangle)]
pub extern "C" fn rust_eh_personality() {}
#[unsafe(no_mangle)]
pub extern "C" fn __rust_alloc(_: usize, _: usize) -> *mut u8 { core::ptr::null_mut() }
#[unsafe(no_mangle)]
pub extern "C" fn adjacent_value() -> u64 { 71 }
"#,
        )
        .unwrap();
        let archive = self.work.join("libadjacent.a");
        passed(
            Command::new(&self.rust)
                .args([
                    "--crate-type=staticlib",
                    "--edition=2024",
                    "-Cpanic=abort",
                    "-Copt-level=2",
                ])
                .arg(source)
                .arg("-o")
                .arg(&archive),
        );
        archive
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.hirsel().args(["serve", "--stop"]).output();
    }
}

/// Outcome::Returned, 12 payload bytes, Result::Ok, and the U64 value.
fn returned(value: u64) -> Vec<u8> {
    [
        0u32.to_le_bytes().as_slice(),
        &12u32.to_le_bytes(),
        &0u32.to_le_bytes(),
        &value.to_le_bytes(),
    ]
    .concat()
}

/// Outcome::Returned of a granted entry: 8 payload bytes, the U64 value
/// itself: a granted entry's Returned bytes carry R, not Result (RUNNER-SPEC).
fn granted_returned(value: u64) -> Vec<u8> {
    [
        0u32.to_le_bytes().as_slice(),
        &8u32.to_le_bytes(),
        &value.to_le_bytes(),
    ]
    .concat()
}

fn anchor(archive: &Path) -> String {
    fs::read_to_string(archive.with_file_name("hirsel_product.h"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("static inline void "))
        .unwrap()
        .split('(')
        .next()
        .unwrap()
        .into()
}

fn exports_and_index(fixture: &Fixture, path: &Path) {
    let anchor = anchor(path);
    let build = anchor
        .strip_prefix("hirsel_embed_v1_require_anchor_")
        .unwrap();
    let prefix = format!("hirsel_{build}_");
    let symbol = format!("hirsel_embed_v1_anchor_{build}");
    let spec = include_str!("../../../../../docs/spec/embed.md");
    let fence = spec
        .split("## EMB-4:")
        .nth(1)
        .unwrap()
        .split("```text\n#define HIRSEL_EMBED_ABI")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let exports: BTreeSet<_> = fence
        .lines()
        .filter_map(|line| {
            let (_, name) = line.split_once(" hirsel_embed_v1_")?;
            name.split_once('(')
                .map(|(name, _)| format!("hirsel_embed_v1_{name}"))
        })
        .collect();
    assert_eq!(exports.len(), 14);
    let bytes = fs::read(path).unwrap();
    let archive = ArchiveFile::parse(bytes.as_slice()).unwrap();
    let mut exports_found = false;
    let mut definitions = BTreeMap::<String, usize>::new();
    for member in archive.members() {
        let member = member.unwrap();
        let object = object::File::parse(member.data(bytes.as_slice()).unwrap()).unwrap();
        let symbols: BTreeSet<_> = object
            .symbols()
            .filter(|s| s.is_global() && !s.is_undefined())
            .map(|s| s.name().unwrap().to_owned())
            .collect();
        if symbols.contains(&symbol) {
            assert!(!exports_found);
            exports_found = true;
            let expected = exports.iter().cloned().chain([symbol.clone()]).collect();
            assert_eq!(symbols, expected);
        } else {
            assert!(
                symbols.iter().all(|name| name.starts_with(&prefix)),
                "{symbols:?}"
            );
        }
        for name in symbols {
            *definitions.entry(name).or_default() += 1;
        }
    }
    assert!(exports_found);
    let index = passed(
        Command::new(fixture.llvm.join("bin/llvm-nm"))
            .arg("--print-armap")
            .arg(path),
    );
    let index = String::from_utf8(index).unwrap();
    let indexed: BTreeSet<_> = index
        .lines()
        .filter_map(|line| line.split_once(" in ").map(|(name, _)| name))
        .collect();
    let missing: Vec<_> = definitions
        .keys()
        .filter(|name| !indexed.contains(name.as_str()))
        .take(12)
        .collect();
    assert!(
        missing.is_empty(),
        "definitions missing from the index: {missing:?}"
    );
}

fn duplicate_exports(fixture: &Fixture, a: &Path, b: &Path) {
    let source = fixture.work.join("two.c");
    fs::write(&source, format!(
        "#include \"{}\"\n#include \"{}\"\nint main(void) {{ {}(); {}(); return hirsel_embed_v1_abi() != 1; }}",
        a.with_file_name("hirsel_product.h").display(), b.with_file_name("hirsel_product.h").display(),
        anchor(a), anchor(b),
    )).unwrap();
    for (first, second) in [(a, b), (b, a)] {
        let Output { status, stderr, .. } = fixture
            .cc()
            .args(["-O2", "-Wl,--gc-sections"])
            .arg(&source)
            .arg(first)
            .arg(second)
            .arg("-o")
            .arg(fixture.work.join("two"))
            .output()
            .unwrap();
        assert!(!status.success());
        let stderr = String::from_utf8_lossy(&stderr);
        assert!(
            stderr.contains("duplicate symbol: hirsel_embed_v1_"),
            "{stderr}"
        );
    }
}

#[test]
#[ignore = "uses pinned native tools and QEMU"]
fn embedded_archive_laws() {
    let work = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(work.path());
    let limit = 1 << 30;
    let executable = work.path().join("executable");
    let a = work.path().join("a/libplace.a");
    let b = work.path().join("b/libplace.a");

    // A cold process in each direction, over the same store for each pair.
    for (resident, reverse, index) in [
        (false, false, 0),
        (false, true, 1),
        (true, false, 2),
        (true, true, 3),
    ] {
        let store = work.path().join(format!("store-{index}"));
        let order = [("exe", &executable), ("staticlib", &a)];
        for step in 0..2 {
            let (kind, target) = order[if reverse { 1 - step } else { step }];
            fixture.build(kind, target, &store, limit + index, resident);
        }
    }

    let describe = passed(
        Command::new(&executable)
            .current_dir(work.path())
            .args(["--hirsel-n-place=16777216", "--hirsel-describe"]),
    );
    let native =
        crate::sdk::manifest_json(&crate::sdk::Describe::read(&describe).unwrap()).unwrap();

    let adjacent = fixture.adjacent();
    let host = fixture.link_host(&a, &adjacent);
    let embedded = passed(&mut Command::new(&host));
    assert_eq!(
        binding_projection(native.as_bytes()).unwrap(),
        binding_projection(&embedded).unwrap()
    );
    let manifest: serde_json::Value = serde_json::from_slice(&embedded).unwrap();
    assert_eq!(
        manifest["launch"]["target"],
        crate::host::HOST_TARGET.triple()
    );
    assert_eq!(manifest["launch"]["digest"], serde_json::json!([]));
    assert!(
        manifest["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["revision"].as_array().unwrap().len() == 32)
    );
    exports_and_index(&fixture, &a);

    let fp = work.path().join("fingerprint");
    fs::write(&fp, manifest_fingerprint(&embedded).unwrap()).unwrap();
    let run_args = [path(&fp), path(&fixture.store), &fixture.grants];
    passed(
        Command::new(&fixture.qemu)
            .args(["-cpu", "qemu64"])
            .arg(&host)
            .args(run_args)
            .args(["refused", "UnsupportedCpu"]),
    );
    let answer_a = passed(Command::new(&host).args(run_args).args(["call", ""]));

    let source = fixture.program.join("embed_place.hirsel");
    let text = fs::read_to_string(&source).unwrap();
    let changed = text.replacen("Ok(k),", "Ok(k + 1u64),", 1);
    assert_ne!(text, changed);
    fs::write(source, changed).unwrap();
    fixture.build(
        "staticlib",
        &b,
        &work.path().join("alternate"),
        limit,
        false,
    );
    let host_b = fixture.link_host(&b, &adjacent);
    let embedded_b = passed(&mut Command::new(&host_b));
    assert_eq!(
        binding_projection(&embedded).unwrap(),
        binding_projection(&embedded_b).unwrap()
    );
    let store_b = work.path().join("store-b");
    fs::create_dir(&store_b).unwrap();
    let answer_b = passed(Command::new(&host_b).args([
        path(&fp),
        path(&store_b),
        &fixture.grants,
        "call",
        "",
    ]));
    assert_eq!(answer_a, returned(7));
    assert_eq!(answer_b, returned(8));
    duplicate_exports(&fixture, &a, &b);

    passed(fixture.hirsel().args(["serve", "--stop"]));
}

/// The embedded Exec laws (EMB-3 "Exec in an embedded launch"), each in a C
/// host linked with `--wrap=exit,--wrap=_exit`: a send to an exited child's
/// stdin under `SIGPIPE=SIG_DFL` returns an error and the host goes on; the
/// host's subreaper flag, dumpable flag, `SIGCHLD` action and its own child
/// survive a launch that spawns and releases children; and `SIGKILL` of the
/// helper kills its child and ends the launch with `ExecSupervisor`.
#[test]
#[ignore = "uses pinned native tools"]
fn embedded_exec_laws() {
    let work = tempfile::tempdir().unwrap();
    let work = work.path();
    let policy = work.join("exec.toml");
    let profile = |name: &str, image: &str, args: &str| {
        let digest = passed(Command::new("sha256sum").arg(image));
        let digest = String::from_utf8(digest).unwrap();
        format!(
            "[{name}]\nimage = {image:?}\nsha256 = {:?}\nargv0 = {name:?}\nargs = [{args}]\nenv = {{}}\ndirs = {{ work = {:?} }}\n\n",
            digest.split_whitespace().next().unwrap(),
            path(work),
        )
    };
    fs::write(
        &policy,
        profile("true", "/usr/bin/true", "[]") + &profile("sleep", "/usr/bin/sleep", "[\"1000\"]"),
    )
    .unwrap();
    let fixture = Fixture::stage(
        work,
        "embed_exec",
        &[],
        &[
            format!("Exec={} true,sleep", policy.display()),
            "Net=Listen 127.0.0.1/32 0-65535".into(),
            "Clock".into(),
            "Random".into(),
            "Tasks".into(),
            "Process".into(),
            "Stderr".into(),
        ],
    );
    let archive = work.join("lib/libplace.a");
    fixture.build("staticlib", &archive, &fixture.store, 1 << 30, false);
    let host = fixture.link(EXEC_HOST, &archive, &fixture.adjacent());
    let manifest = passed(Command::new(&host).current_dir(work));
    let fp = work.join("fingerprint");
    fs::write(&fp, manifest_fingerprint(&manifest).unwrap()).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let helper = root
        .join(std::env::var_os("HIRSEL_EXEC_HELPER").unwrap())
        .canonicalize()
        .unwrap();
    let fake = work.join("fake-helper");
    let fake_source = work.join("fake-helper.c");
    fs::write(&fake_source, EXEC_FAKE_HELPER).unwrap();
    passed(
        fixture
            .cc()
            .args(["-O2", "-Wall", "-Werror"])
            .arg(&fake_source)
            .arg("-o")
            .arg(&fake),
    );
    let missing = work.join("no-such-helper");
    // The host holds a constructor's lock throughout (exec_host.c), so a
    // helper that ran the host's initializers would never answer.
    for (case, (image, mode, expected)) in [
        (&helper, "pipe", granted_returned(1)),
        (&helper, "state", granted_returned(2)),
        (&helper, "death", b"ExecSupervisor".to_vec()),
        (
            &missing,
            "refused",
            format!("ExecHelper(missing: {})", path(&missing)).into_bytes(),
        ),
        (
            &fake,
            "refused",
            b"ExecHelper(version: got 0.0.0-fake, want ".to_vec(),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let store = work.join(format!("store-{case}"));
        fs::create_dir(&store).unwrap();
        let answer = passed(
            Command::new("timeout")
                .arg("120")
                .arg(&host)
                .current_dir(work)
                .args([path(&fp), path(&store), &fixture.grants, path(image), mode]),
        );
        let shown = String::from_utf8_lossy(&answer);
        if mode == "refused" {
            assert!(answer.starts_with(&expected), "{}: {shown}", path(image));
        } else {
            assert_eq!(answer, expected, "{mode}: {shown}");
        }
    }
    passed(fixture.hirsel().args(["serve", "--stop"]));
}
