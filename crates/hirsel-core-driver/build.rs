// Freeze installed library sources and identities when the toolchain is built.
#[path = "src/compiler_source.rs"]
mod compiler_source;
#[path = "src/operator_schema.rs"]
mod operator_schema;
use sha2::{Digest, Sha256};
use std::{
    env,
    fmt::Write as _,
    fs,
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

fn manifest(path: &Path) -> toml::Table {
    println!("cargo:rerun-if-changed={}", path.display());
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .parse()
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn toolchain_packages(programs: &Path, generated: &mut String) {
    let inventory = manifest(&programs.join("toolchain-packages.toml"));
    generated.push_str(
        "type ToolchainId = (&'static str, u32);\n\
         pub(crate) const TOOLCHAIN: &[(ToolchainId, &[ToolchainId])] = &[\n    ((\"std\", 1), &[]),\n",
    );
    for name in inventory["packages"].as_array().unwrap() {
        let name = name.as_str().unwrap();
        let path = programs.join(name).join("hirsel.toml");
        let document = manifest(&path);
        let package = document["package"].as_table().unwrap();
        assert_eq!(
            package["name"].as_str().unwrap(),
            name,
            "{}",
            path.display()
        );
        let major = u32::try_from(package["major"].as_integer().unwrap()).unwrap();
        let mut bindings = Vec::new();
        if let Some(dependencies) = document.get("dependencies") {
            for (dependency, binding) in dependencies.as_table().unwrap() {
                let major = binding
                    .get("major")
                    .and_then(toml::Value::as_integer)
                    .and_then(|major| u32::try_from(major).ok())
                    .unwrap_or_else(|| {
                        panic!("{}: {dependency} must declare a major", path.display())
                    });
                bindings.push((dependency, major));
            }
        }
        bindings.sort_unstable();
        writeln!(generated, "    (({name:?}, {major}), &{bindings:?}),").unwrap();
    }
    generated.push_str("];\n");
}

fn main() {
    println!("cargo:rerun-if-env-changed=HIRSEL_RELEASE_VERSION");
    if let Some(version) = env::var("HIRSEL_RELEASE_VERSION")
        .ok()
        .map(|v| v.trim().strip_prefix('v').unwrap_or(v.trim()).to_owned())
        .filter(|v| !v.is_empty())
    {
        println!("cargo:rustc-env=HIRSEL_RELEASE_VERSION={version}");
    }
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../core");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let operator = root.join("programs/platform/operator.hirsel");
    println!("cargo:rerun-if-changed={}", operator.display());
    let operator_wire = operator_schema::generate(&fs::read_to_string(operator).unwrap());
    fs::write(out.join("operator_wire.rs"), &operator_wire).unwrap();

    let mut generated = String::new();
    let core = package(&root.join("core"), "SOURCES", &mut generated);
    let std = package(&root.join("std"), "STD_SOURCES", &mut generated);
    for (name, symbol) in [("store", "STORE_SOURCES"), ("runner", "RUNNER_SOURCES")] {
        let directory = root.join("programs").join(name);
        let manifest = manifest(&directory.join("hirsel.toml"));
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
    toolchain_packages(&root.join("programs"), &mut generated);
    // The compiler identity (docs/building.md, "Compiler identity"): the
    // compiler's source closure and pins, the library sources it bundles, and
    // the toolchain package bindings it supplies.
    let repository = root.parent().unwrap();
    let product = manifest(&repository.join("tools/toolchain-dist/product.toml"));
    for (symbol, socket) in [
        ("LAUNCHER_SOCKET", "launcher"),
        ("OPERATOR_SOCKET", "operator"),
    ] {
        let name = product["sockets"][socket].as_str().unwrap();
        writeln!(generated, "pub(crate) const {symbol}: &str = {name:?};").unwrap();
    }
    if env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "macos") {
        let library = product["hosts"]["aarch64-apple-darwin"]["llvm_library"]
            .as_str()
            .unwrap();
        writeln!(
            generated,
            "pub(crate) const DARWIN_LLVM_LIBRARY: &str = {library:?};"
        )
        .unwrap();
    }
    let (source, watched) =
        compiler_source::digest(repository, "crates/hirsel-core-driver").unwrap();
    for path in watched {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let mut identity = Sha256::new();
    identity.update(b"hirsel.compiler-identity.v1");
    identity.update(source);
    identity.update(Sha256::digest(operator_wire.as_bytes()));
    identity.update(Sha256::digest(generated.as_bytes()));
    generated.push_str(&format!(
        "pub const COMPILER: [u8;32] = {:?};\n",
        <[u8; 32]>::from(identity.finalize())
    ));
    fs::write(out.join("core_sources.rs"), generated).unwrap();
}
