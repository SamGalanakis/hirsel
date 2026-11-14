#[path = "src/catalogue_source.rs"]
mod source;
use std::{env, fs, path::PathBuf};
fn main() {
    let spec = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../docs/spec");
    println!("cargo:rerun-if-changed={}", spec.display());
    let rules = source::read(&spec).expect("read current-spec diagnostic catalogue");
    let mut generated = String::from("pub static RULES: &[Rule] = &[\n");
    for rule in rules {
        generated.push_str(&format!(
            "Rule {{ id: {:?}, title: {:?}, spec_file: {:?}, anchor: {:?}, statement: {:?} }},\n",
            rule.id, rule.title, rule.spec_file, rule.anchor, rule.statement
        ));
    }
    generated.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("catalogue.rs"),
        generated,
    )
    .unwrap();
}
