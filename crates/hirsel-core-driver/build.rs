// Freeze installed library sources and identities when the toolchain is built.
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn sources(root: &Path, dir: &Path, paths: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            if dir != root || entry.file_name() != "tests" {
                sources(root, &path, paths);
            }
        } else if path.extension().is_some_and(|e| e == "hirsel") {
            let relative = path.strip_prefix(root).unwrap().with_extension("");
            let module = relative
                .components()
                .map(|c| c.as_os_str().to_str().unwrap())
                .collect::<Vec<_>>()
                .join("/");
            paths.push((module, path));
        }
    }
}

fn package(root: &Path, symbol: &str, generated: &mut String) -> [u8; 32] {
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    sources(root, root, &mut files);
    files.sort();
    let mut hash = Sha256::new();
    hash.update(b"hirsel.bundled-library.v1");
    hash.update(symbol.as_bytes());
    generated.push_str(&format!("pub const {symbol}: &[(&str, &str)] = &[\n"));
    for (module, path) in files {
        let source = fs::read_to_string(&path).unwrap();
        hash.update((module.len() as u64).to_le_bytes());
        hash.update(module.as_bytes());
        hash.update((source.len() as u64).to_le_bytes());
        hash.update(source.as_bytes());
        generated.push_str(&format!("({module:?}, {source:?}),\n"));
    }
    generated.push_str("];\n");
    hash.finalize().into()
}
fn main() {
    println!("cargo:rerun-if-env-changed=RELEASE_TAG");
    println!("cargo:rerun-if-env-changed=HIRSEL_RELEASE_VERSION");
    if let Some(version) = env::var("HIRSEL_RELEASE_VERSION")
        .ok()
        .or_else(|| env::var("RELEASE_TAG").ok())
        .map(|v| v.trim().strip_prefix('v').unwrap_or(v.trim()).to_owned())
        .filter(|v| !v.is_empty())
    {
        println!("cargo:rustc-env=HIRSEL_RELEASE_VERSION={version}");
    }
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../core");
    let mut generated = String::new();
    let core = package(&root.join("core"), "SOURCES", &mut generated);
    let std = package(&root.join("std"), "STD_SOURCES", &mut generated);
    for (name, symbol) in [("store", "STORE_SOURCES"), ("runner", "RUNNER_SOURCES")] {
        let directory = root.join("programs").join(name);
        let manifest = directory.join("hirsel.toml");
        println!("cargo:rerun-if-changed={}", manifest.display());
        let manifest: toml::Table = fs::read_to_string(manifest).unwrap().parse().unwrap();
        generated.push_str(&format!("pub const {symbol}: &[(&str, &str, &str)] = &[\n"));
        for (file, module) in manifest["sources"].as_table().unwrap() {
            let path = directory.join(file);
            println!("cargo:rerun-if-changed={}", path.display());
            let source = fs::read_to_string(path).unwrap();
            let module = module.as_str().unwrap();
            generated.push_str(&format!("({file:?}, {module:?}, {source:?}),\n"));
        }
        generated.push_str("];\n");
    }
    let frame_schema = root.join("programs/runner/frame-schema.bin");
    println!("cargo:rerun-if-changed={}", frame_schema.display());
    let schema = fs::read(frame_schema).unwrap();
    generated.push_str(&format!("pub const FRAME_SCHEMA: &[u8] = &{schema:?};\n"));
    let mut combined = Sha256::new();
    combined.update(b"hirsel.bundled-core-std.v1");
    combined.update(core);
    combined.update(std);
    generated.push_str(&format!(
        "pub const DIGEST: [u8;32] = {core:?};\npub const STD_DIGEST: [u8;32] = {:?};\n",
        <[u8; 32]>::from(combined.finalize())
    ));
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("core_sources.rs"),
        generated,
    )
    .unwrap();
}
