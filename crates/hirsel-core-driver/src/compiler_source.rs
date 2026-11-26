//! The compiler's source: every file that decides what the compiler emits,
//! hashed when the toolchain is built (docs/building.md, "Compiler identity").
//! build.rs computes it; the driver's tests call the same function.
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// The pins the compiler is built with: the workspace and its lockfile, the
/// Rust release, the LLVM tree and the link kit.
pub const PINS: [&str; 5] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "tools/native-toolchain/llvm-23.lock",
    "tools/native-toolchain/native.lock.json",
];
/// Directories a closure crate's build script compiles that sit outside the
/// closure: hirsel-llvm-lazy's component is built from hirsel-llvm's bridge.
pub const EXTRA: [&str; 1] = ["crates/hirsel-llvm/bridge"];
/// Crate subdirectories that build no part of the compiler.
const SKIPPED: [&str; 3] = ["tests", "benches", "examples"];

/// The workspace crates `crate_dir` (repository-relative) compiles from:
/// itself and its path dependencies, normal and build, on every target,
/// transitively. Dev-dependencies build tests only.
pub fn closure(root: &Path, crate_dir: &str) -> io::Result<Vec<String>> {
    let mut crates = Vec::new();
    let mut pending = vec![crate_dir.to_owned()];
    while let Some(dir) = pending.pop() {
        if crates.contains(&dir) {
            continue;
        }
        let text = fs::read_to_string(root.join(&dir).join("Cargo.toml"))?;
        let manifest: toml::Table = text.parse().map_err(io::Error::other)?;
        let mut tables = Vec::new();
        for section in ["dependencies", "build-dependencies"] {
            tables.extend(manifest.get(section).and_then(|t| t.as_table()));
            if let Some(targets) = manifest.get("target").and_then(|t| t.as_table()) {
                for target in targets.values() {
                    tables.extend(target.get(section).and_then(|t| t.as_table()));
                }
            }
        }
        for table in tables {
            for dependency in table.values() {
                if let Some(path) = dependency.get("path").and_then(|p| p.as_str()) {
                    pending.push(normal(&format!("{dir}/{path}")));
                }
            }
        }
        crates.push(dir);
    }
    crates.sort();
    Ok(crates)
}

/// `a/b/../c` as `a/c`.
fn normal(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

/// The repository-relative files the source digest covers: [`PINS`], and
/// every file under [`EXTRA`] and each closure crate except its [`SKIPPED`]
/// directories. Names starting with `.` and Buck build files are skipped: a
/// build recipe, like a profile, does not change what the compiler emits.
/// Sorted.
pub fn files(root: &Path, crate_dir: &str) -> io::Result<Vec<String>> {
    let mut files: Vec<String> = PINS.iter().map(|p| (*p).to_owned()).collect();
    for dir in roots(root, crate_dir)? {
        walk(root, &dir, true, &mut files)?;
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// The closure crates' directories and [`EXTRA`].
fn roots(root: &Path, crate_dir: &str) -> io::Result<Vec<String>> {
    let mut dirs = closure(root, crate_dir)?;
    dirs.extend(EXTRA.iter().map(|d| (*d).to_owned()));
    Ok(dirs)
}

fn walk(root: &Path, dir: &str, top: bool, files: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(root.join(dir))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| io::Error::other(format!("{dir}: a non-UTF-8 name")))?;
        if name.starts_with('.') || name == "BUCK" || (top && SKIPPED.contains(&name)) {
            continue;
        }
        let path = format!("{dir}/{name}");
        // Buck lays inputs out as symbolic links: follow them.
        if fs::metadata(entry.path())?.is_dir() {
            walk(root, &path, false, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

/// SHA-256 over the domain and each file's name and bytes, each framed by its
/// u64 LE length, in [`files`] order. Also the paths a change to any of them
/// shows in: the pins and the directories walked.
pub fn digest(root: &Path, crate_dir: &str) -> io::Result<([u8; 32], Vec<PathBuf>)> {
    let mut hash = Sha256::new();
    hash.update(b"hirsel.compiler-source.v1");
    for file in files(root, crate_dir)? {
        let bytes = fs::read(root.join(&file))?;
        hash.update((file.len() as u64).to_le_bytes());
        hash.update(file.as_bytes());
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    let watched = PINS
        .iter()
        .map(|p| (*p).to_owned())
        .chain(roots(root, crate_dir)?)
        .map(|p| root.join(p))
        .collect();
    Ok((hash.finalize().into(), watched))
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    /// The compiler identity `hirsel --version` prints for the executable at `path`.
    fn printed(path: &std::path::Path) -> String {
        let out = Command::new(path).arg("--version").output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8(out.stdout).unwrap();
        text.lines()
            .find_map(|line| line.strip_prefix("compiler "))
            .unwrap()
            .to_owned()
    }

    /// Two builds of one source whose links differ, here only in their build
    /// ID, are one compiler: products are keyed by what the compiler is, so a
    /// cross and a native build share every key (docs/building.md, "Compiler
    /// identity").
    #[test]
    fn a_link_id_is_not_the_compiler() {
        let driver = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(std::env::var_os("HIRSEL_DEV_TEST_DRIVER").unwrap());
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        let mut bytes = std::fs::read(&driver).unwrap();
        std::fs::write(&a, &bytes).unwrap();
        let id = {
            use object::Object as _;
            object::File::parse(&*bytes)
                .unwrap()
                .build_id()
                .unwrap()
                .unwrap()
                .to_vec()
        };
        let at = bytes.windows(id.len()).position(|w| w == id).unwrap();
        bytes[at] ^= 0xff;
        std::fs::write(&b, &bytes).unwrap();
        for path in [&a, &b] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let link = |path| {
            crate::reuse::executable_identity(&mut std::fs::File::open(path).unwrap()).unwrap()
        };
        assert_ne!(link(&a), link(&b));
        assert_eq!(printed(&a), printed(&b));
    }

    /// What decides the compiler's output is in its source digest: the LLVM
    /// and kit pins, a dependency crate's sources, hirsel-llvm's bridge. A
    /// crate's tests and a Buck build file are not.
    #[test]
    fn the_source_digest_covers_the_pins_and_the_closure() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        for pin in super::PINS {
            write(pin, pin);
        }
        write(
            "crates/hirsel-core-driver/Cargo.toml",
            "[dependencies]\na = { path = \"../a\" }\n[dev-dependencies]\nt = { path = \"../t\" }\n",
        );
        write(
            "crates/a/Cargo.toml",
            "[target.'cfg(unix)'.build-dependencies]\nb = { path = \"../b\" }\n",
        );
        write("crates/a/src/lib.rs", "a");
        write("crates/b/Cargo.toml", "");
        write("crates/b/build.rs", "b");
        write("crates/hirsel-llvm/bridge/pass.cpp", "pass");
        let digest = || super::digest(root, "crates/hirsel-core-driver").unwrap().0;
        let first = digest();
        let mut seen = vec![first];
        for (path, text) in [
            ("tools/native-toolchain/llvm-23.lock", "llvm_version 23.1.6"),
            ("tools/native-toolchain/native.lock.json", "{}"),
            ("crates/a/src/lib.rs", "a2"),
            ("crates/b/build.rs", "b2"),
            ("crates/hirsel-llvm/bridge/pass.cpp", "pass2"),
            ("crates/a/src/new.rs", "new"),
        ] {
            write(path, text);
            let next = digest();
            assert!(!seen.contains(&next), "{path} is not in the digest");
            seen.push(next);
        }
        let last = *seen.last().unwrap();
        for path in [
            "crates/a/tests/law.rs",
            "crates/a/src/BUCK",
            "crates/t/src/lib.rs",
        ] {
            write(path, "x");
            assert_eq!(digest(), last, "{path} is in the digest");
        }
    }
}
