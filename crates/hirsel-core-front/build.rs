use std::env;
use std::fs;
use std::path::Path;

fn hirsel_cases(text: &str) -> Vec<String> {
    let (_, declaration) = text
        .split_once("pub type AddressClass =")
        .expect("core AddressClass type");
    let mut lines = declaration.lines().map(str::trim);
    let first = lines.next().expect("core AddressClass cases");
    let continuations = lines.map_while(|line| line.strip_prefix('|'));
    std::iter::once(first)
        .chain(continuations)
        .flat_map(|line| line.split('|'))
        .map(|case| case.trim().to_owned())
        .collect()
}

fn rust_cases(text: &str) -> Vec<String> {
    let start = text
        .find("pub enum AddressClass")
        .expect("Rust AddressClass enum");
    let open = text[start..].find('{').expect("Rust AddressClass body") + start;
    let close = text[open..].find('}').expect("Rust AddressClass end") + open;
    text[open + 1..close]
        .split(',')
        .map(|case| case.trim().to_owned())
        .filter(|case| !case.is_empty())
        .collect()
}

fn declared_classes(text: &str) -> usize {
    let start = text
        .find("const CLASSES: u32 =")
        .expect("runtime_net_class CLASSES");
    text[start..]
        .split_once('=')
        .expect("runtime_net_class class count")
        .1
        .split(|c: char| !c.is_ascii_digit())
        .find(|part| !part.is_empty())
        .expect("runtime_net_class class count")
        .parse()
        .expect("runtime_net_class class count")
}

fn main() {
    let manifest = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).to_path_buf();
    let core = fs::read_to_string(manifest.join("../../core/core/core.hirsel")).unwrap();
    let rust = fs::read_to_string(manifest.join("../hirsel-core-net/src/class.rs")).unwrap();
    let lowering = fs::read_to_string(manifest.join("src/body/core_ops/net_class.rs")).unwrap();
    let hirsel = hirsel_cases(&core);
    let rust = rust_cases(&rust);
    assert_eq!(
        hirsel, rust,
        "core.net.AddressClass case order differs from hirsel_core_net::AddressClass"
    );
    assert_eq!(
        hirsel.len(),
        declared_classes(&lowering),
        "runtime_net_class CLASSES differs from the AddressClass case count"
    );
}
