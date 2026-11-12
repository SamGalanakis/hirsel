//! Recording is switched on only for the work that publishes, so a published
//! product's observations equal those of a run recording everywhere exactly
//! when no use falls outside it. The installation recorder logs every row and
//! content use whether or not its thread is traced, so a use on a thread the
//! trace did not reach is reported, not lost. The oracle is that invariant:
//! no untraced use, and every source file's part observed.
use hirsel_driver::CompilerRequestsExt;
use hirsel_front_phases::COMPILER;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const CHILD: &str = "HIRSEL_PRECOMPUTE_OBSERVATIONS_756";
/// Three files sharing callees, so a later file's part is served from caches
/// the first file's part filled.
const FILES: [(&str, &str); 3] = [
    (
        "main.hir",
        r#"fn make()->Result[Body[fn()->u8 uses {}],String]{Ok(codegen.body("fn()->u8 uses {} {7u8}"))}
pub fn main()->u8 uses {} = comptime make()
pub fn second()->u64{9u64}
pub fn third()->u64{second()}
"#,
    ),
    ("fourth.hir", "pub fn fourth()->u64{third()+second()}\n"),
    ("fifth.hir", "pub fn fifth()->u64{fourth()+third()}\n"),
];

fn manifest() -> String {
    let sources: Vec<String> = FILES
        .iter()
        .map(|(path, _)| format!(r#"{{"path":"{path}","module":[]}}"#))
        .collect();
    format!(
        r#"{{"schema":1,"package":{{"name":"observations_756","version":"1"}},"sources":[{}],"dependencies":[],"entry":{{"module":[],"name":"main"}}}}"#,
        sources.join(",")
    )
}

/// One cold Precompute in a fresh process; its observations, one line per use.
fn observed() -> BTreeSet<String> {
    let directory = tempfile::tempdir().unwrap();
    let directory = directory.path();
    std::fs::create_dir(directory.join("pkg")).unwrap();
    for (path, text) in FILES {
        std::fs::write(directory.join("pkg").join(path), text).unwrap();
    }
    std::fs::write(directory.join("pkg/hirsel.json"), manifest()).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "precompute_observations_756::a_published_product_observes_every_use_its_work_made",
            "--nocapture",
        ])
        .env(CHILD, directory.join("out"))
        .env(
            "HIRSEL_OBSERVATIONS_756_MANIFEST",
            directory.join("pkg/hirsel.json"),
        )
        .env("XDG_STATE_HOME", directory.join("state"))
        .env("XDG_CACHE_HOME", directory.join("cache"))
        .env("HIRSEL_RESULTS", "none@1")
        .env_remove("HIRSEL_VERIFY_CACHE")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            line.split_once("observed ")
                .map(|(_, use_)| use_.to_owned())
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn a_published_product_observes_every_use_its_work_made() {
    if let Some(root) = std::env::var_os(CHILD) {
        let manifest = PathBuf::from(std::env::var_os("HIRSEL_OBSERVATIONS_756_MANIFEST").unwrap());
        // From an ordinary thread, so the compiler thread the engine starts
        // for the work is one recording has to reach.
        let observations = COMPILER
            .precompute_package_observed(&manifest, Path::new(&root))
            .unwrap()
            .unwrap();
        let named = observations
            .parts
            .iter()
            .map(|(part, used)| (hex(part), used))
            .chain([
                ("package".to_owned(), &observations.package),
                ("untraced".to_owned(), &observations.untraced),
            ]);
        for (owner, used) in named {
            for row in &used.rows {
                println!("observed {owner} row {}", hex(&row.0));
            }
            for content in &used.contents {
                println!("observed {owner} content {}", hex(&content.0));
            }
        }
        return;
    }
    let observed = observed();
    let untraced: Vec<_> = observed
        .iter()
        .filter(|use_| use_.starts_with("untraced "))
        .collect();
    assert!(untraced.is_empty(), "uses no trace reached: {untraced:?}");
    let parts: BTreeSet<&str> = observed
        .iter()
        .filter(|use_| !use_.starts_with("package "))
        .filter_map(|use_| use_.split(' ').next())
        .collect();
    assert_eq!(parts.len(), FILES.len(), "one part per file: {parts:?}");
}
