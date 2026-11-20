#[path = "../../core/front/support/cases.rs"]
mod cases;
#[path = "src/catalogue_source.rs"]
mod source;
use cases::DiagnosticExpectation;
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fmt::Write,
    fs, io,
    path::{Path, PathBuf},
};

struct Example {
    path: String,
    source: String,
    assertions: Vec<String>,
}

fn examples(repo: &Path) -> io::Result<BTreeMap<String, Example>> {
    let mut examples: BTreeMap<String, Example> = BTreeMap::new();
    for file in cases::files(&repo.join("core/front/cases"))? {
        let source = fs::read_to_string(&file)?;
        let expectations: Vec<_> = cases::expectations(&source)
            .filter_map(|text| {
                let words: Vec<_> = text.split_whitespace().collect();
                DiagnosticExpectation::parse(text, &words)
            })
            .collect();
        let mut refusals: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for expectation in &expectations {
            if let DiagnosticExpectation::Refuse { rule, at } = expectation {
                refusals.entry(rule).or_default().insert(at);
            }
        }
        let path = file.strip_prefix(repo).unwrap().to_str().unwrap();
        for (rule, sites) in refusals {
            if examples
                .get(rule)
                .is_some_and(|old| (old.source.len(), old.path.as_str()) <= (source.len(), path))
            {
                continue;
            }
            let assertions = expectations
                .iter()
                .filter_map(|expectation| {
                    let site = match *expectation {
                        DiagnosticExpectation::Refuse { rule, at }
                        | DiagnosticExpectation::Cause { rule, at, .. }
                        | DiagnosticExpectation::Message { rule, at, .. } => (rule, at),
                        DiagnosticExpectation::Compared { head, .. } => head.split_once(" at ")?,
                    };
                    (site.0 == rule && sites.contains(site.1)).then(|| expectation.to_string())
                })
                .collect();
            examples.insert(
                rule.to_owned(),
                Example {
                    path: path.to_owned(),
                    source: source.clone(),
                    assertions,
                },
            );
        }
    }
    Ok(examples)
}

fn main() {
    let repo = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let spec = repo.join("docs/spec");
    println!("cargo:rerun-if-changed={}", spec.display());
    println!(
        "cargo:rerun-if-changed={}",
        repo.join("core/front/cases").display()
    );
    let rules = source::read(&spec).expect("read current-spec diagnostic catalogue");
    let examples = examples(&repo).expect("read front-end cases");
    let mut generated = String::from("pub static RULES: &[Rule] = &[\n");
    for rule in &rules {
        generated.push_str(&format!(
            "Rule {{ id: {:?}, title: {:?}, spec_file: {:?}, anchor: {:?}, statement: {:?} }},\n",
            rule.id, rule.title, rule.spec_file, rule.anchor, rule.statement
        ));
    }
    generated.push_str("];\n");
    generated.push_str("static EXAMPLES: &[(&str, Example)] = &[\n");
    for rule in &rules {
        if let Some(example) = examples.get(&rule.id) {
            writeln!(
                generated,
                "({:?}, Example {{ path: {:?}, source: {:?}, assertions: &{:?} }}),",
                rule.id, example.path, example.source, example.assertions
            )
            .unwrap();
        }
    }
    generated.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("catalogue.rs"),
        generated,
    )
    .unwrap();
}
