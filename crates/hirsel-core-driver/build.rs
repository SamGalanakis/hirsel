// Freeze the toolchain's core input identity at toolchain construction, not on
// every user build. Cargo and Buck both declare the directory as a build input.
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path};
fn main() {
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../core/core");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "hirsel"))
        .collect::<Vec<_>>();
    files.sort();
    let mut hash = Sha256::new();
    hash.update(b"hirsel.bundled-core.v1");
    let mut generated = String::from("pub const SOURCES: &[(&str, &str)] = &[\n");
    for path in files {
        let module = path.file_stem().unwrap().to_str().unwrap();
        let source = fs::read_to_string(&path).unwrap();
        hash.update((module.len() as u64).to_le_bytes());
        hash.update(module.as_bytes());
        hash.update((source.len() as u64).to_le_bytes());
        hash.update(source.as_bytes());
        generated.push_str(&format!("({module:?}, {source:?}),\n"));
    }
    generated.push_str("];\n");
    generated.push_str(&format!(
        "pub const DIGEST: [u8;32] = {:?};\n",
        <[u8; 32]>::from(hash.finalize())
    ));
    fs::write(
        Path::new(&env::var("OUT_DIR").unwrap()).join("core_sources.rs"),
        generated,
    )
    .unwrap();
}
