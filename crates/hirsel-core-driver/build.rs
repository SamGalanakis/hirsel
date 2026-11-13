// Freeze installed library sources and identities when the toolchain is built.
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path};
fn package(root: &Path, symbol: &str, generated: &mut String) -> [u8; 32] {
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "hirsel"))
        .collect::<Vec<_>>();
    files.sort();
    let mut hash = Sha256::new();
    hash.update(b"hirsel.bundled-library.v1");
    hash.update(symbol.as_bytes());
    generated.push_str(&format!("pub const {symbol}: &[(&str, &str)] = &[\n"));
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
    hash.finalize().into()
}
fn main() {
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../core");
    let mut generated = String::new();
    let core = package(&root.join("core"), "SOURCES", &mut generated);
    let std = package(&root.join("std"), "STD_SOURCES", &mut generated);
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
