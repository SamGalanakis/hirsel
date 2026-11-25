//! Dependency allowlist: implementation crates never depend on one another;
//! the driver composes them.
//!
//! Reads `cargo metadata`, classifies every workspace crate, and checks each
//! declared workspace edge by kind. Production and build edges must respect the
//! layering; dev edges (test-only) are reported and allowed.
hirsel_guard::install!();

use serde_json::Value;
use std::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Class {
    /// Shared data crates: identities, IR, layouts. They depend only on data crates.
    Data,
    /// Independent implementation crates: the front-end phases, the backends
    /// and the runtime.
    Implementation,
    /// Dependency-free utilities any layer may consume.
    LeafUtility,
    /// The driver composes implementation crates.
    Wiring,
    /// Tools beside the compiler: never a compiler dependency.
    Tool,
}

const CRATES: &[(&str, Class)] = &[
    ("hirsel-guard", Class::LeafUtility),
    ("hirsel-stack", Class::LeafUtility),
    ("hirsel-span", Class::Data),
    ("hirsel-intern", Class::Data),
    ("hirsel-core-ids", Class::Data),
    ("hirsel-core-diag", Class::Data),
    ("hirsel-core-syntax", Class::Data),
    ("hirsel-core-ir", Class::Data),
    ("hirsel-core-layout", Class::Data),
    ("hirsel-core-net", Class::Data),
    ("hirsel-tape-format", Class::Data),
    ("hirsel-core-resolve", Class::Implementation),
    ("hirsel-core-front", Class::Implementation),
    // One instance's module from its identity and its arguments' closures.
    ("hirsel-core-materialize", Class::Implementation),
    ("hirsel-core-rt", Class::Implementation),
    ("hirsel-cranelift", Class::Implementation),
    ("hirsel-llvm", Class::Implementation),
    ("hirsel-llvm-lazy", Class::Implementation),
    // The reference evaluator: the pure core and the test world, the oracle the
    // backends and the front run cases are compared against.
    ("hirsel-core-eval", Class::Implementation),
    // Native profiles: samples, symbolization and the profile store.
    ("hirsel-prof", Class::Implementation),
    ("hirsel-core-driver", Class::Wiring),
    ("hirsel-core-contract-eval", Class::Tool),
    ("hirsel-core-program-tests", Class::Tool),
];

/// Shared utilities and data crates that take no external production dependency.
const NO_EXTERNAL_PRODUCTION: &[&str] = &["hirsel-core-ir", "hirsel-core-layout"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Production,
    Dev,
    Build,
}

#[derive(Clone, Debug)]
struct EdgeRow {
    from: String,
    to: String,
    kind: Kind,
    workspace: bool,
}

fn class(name: &str) -> Option<Class> {
    CRATES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// The rule for one edge between workspace crates; `Err` names the violation.
fn check_edge(from: &str, to: &str, kind: Kind) -> Result<(), String> {
    let (Some(source), Some(target)) = (class(from), class(to)) else {
        return Err(format!("unclassified crate in {from} -> {to}"));
    };
    if kind == Kind::Dev {
        return Ok(());
    }
    let allowed = match (source, target) {
        _ if from == "hirsel-core-driver" => matches!(
            to,
            "hirsel-core-ids"
                | "hirsel-core-diag"
                | "hirsel-core-syntax"
                | "hirsel-core-resolve"
                | "hirsel-core-front"
                | "hirsel-core-materialize"
                | "hirsel-core-ir"
                | "hirsel-core-layout"
                | "hirsel-core-net"
                | "hirsel-tape-format"
                | "hirsel-intern"
                | "hirsel-guard"
                | "hirsel-cranelift"
                | "hirsel-llvm-lazy"
                | "hirsel-prof"
        ),
        _ if from == "hirsel-core-materialize" => matches!(
            to,
            "hirsel-core-front" | "hirsel-core-ids" | "hirsel-core-ir" | "hirsel-guard"
        ),
        _ if from == "hirsel-core-resolve" => matches!(
            to,
            "hirsel-core-syntax" | "hirsel-core-ids" | "hirsel-guard"
        ),
        _ if from == "hirsel-core-front" => matches!(
            to,
            "hirsel-core-ids"
                | "hirsel-core-ir"
                | "hirsel-core-layout"
                | "hirsel-core-syntax"
                | "hirsel-core-resolve"
                | "hirsel-guard"
        ),
        _ if from == "hirsel-core-syntax" => {
            matches!(to, "hirsel-core-ids" | "hirsel-guard")
        }
        (Class::LeafUtility, _) => false,
        (_, Class::LeafUtility) => true,
        (_, Class::Data) => true,
        (Class::Wiring, Class::Implementation) => true,
        (Class::Tool, Class::Wiring | Class::Implementation) => true,
        (Class::Data | Class::Implementation, _) => false,
        (_, Class::Tool) => false,
        (Class::Wiring, Class::Wiring) => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(format!(
            "{kind:?} edge {from} ({source:?}) -> {to} ({target:?})"
        ))
    }
}

fn metadata() -> Value {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let prepared = std::path::Path::new(root).join("tools/buck2/workspace-metadata.json");
    if prepared.is_file() {
        return serde_json::from_slice(&std::fs::read(prepared).unwrap())
            .expect("prepared Cargo metadata JSON");
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(root)
        .output()
        .expect("run cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("cargo metadata JSON")
}

fn edges(metadata: &Value) -> (Vec<String>, Vec<EdgeRow>) {
    let mut members = Vec::new();
    let mut edges = Vec::new();
    for package in metadata["packages"].as_array().expect("packages") {
        let from = package["name"].as_str().expect("package name").to_string();
        for dependency in package["dependencies"].as_array().expect("dependencies") {
            let kind = match dependency["kind"].as_str() {
                None => Kind::Production,
                Some("dev") => Kind::Dev,
                Some("build") => Kind::Build,
                Some(other) => panic!("unknown dependency kind {other}"),
            };
            edges.push(EdgeRow {
                from: from.clone(),
                to: dependency["name"].as_str().expect("dependency name").into(),
                kind,
                workspace: dependency.get("path").is_some_and(|p| !p.is_null()),
            });
        }
        members.push(from);
    }
    (members, edges)
}

#[test]
fn workspace_edges_respect_the_crate_layering() {
    let (members, edges) = edges(&metadata());
    let unclassified: Vec<_> = members.iter().filter(|m| class(m).is_none()).collect();
    assert!(
        unclassified.is_empty(),
        "classify every workspace crate in crates/hirsel-core-driver/tests/layering.rs: {unclassified:?}"
    );
    let stale: Vec<_> = CRATES
        .iter()
        .filter(|(name, _)| !members.iter().any(|m| m == name))
        .collect();
    assert!(
        stale.is_empty(),
        "classified crates that are gone: {stale:?}"
    );
    let mut violations = Vec::new();
    let mut counts = [0usize; 3];
    for edge in edges.iter().filter(|e| e.workspace) {
        counts[edge.kind as usize] += 1;
        if let Err(violation) = check_edge(&edge.from, &edge.to, edge.kind) {
            violations.push(violation);
        }
    }
    for edge in &edges {
        if !edge.workspace
            && edge.kind == Kind::Production
            && NO_EXTERNAL_PRODUCTION.contains(&edge.from.as_str())
            && !(matches!(edge.from.as_str(), "hirsel-core-ir" | "hirsel-core-layout")
                && edge.to == "serde")
        {
            violations.push(format!(
                "external production dependency {} -> {}",
                edge.from, edge.to
            ));
        }
    }
    println!(
        "{} workspace crates; workspace edges: {} production, {} dev, {} build",
        members.len(),
        counts[0],
        counts[1],
        counts[2]
    );
    assert!(
        violations.is_empty(),
        "dependency violations: {violations:#?}"
    );
    assert!(
        counts[0] > 0 && counts[1] > 0,
        "metadata parse saw no edges"
    );
}

#[test]
fn layering_controls() {
    assert!(check_edge("hirsel-core-driver", "hirsel-core-front", Kind::Production).is_ok());
    assert!(check_edge("hirsel-core-driver", "hirsel-llvm-lazy", Kind::Production).is_ok());
    // The driver reaches LLVM only through the lazily loaded component.
    assert!(check_edge("hirsel-core-driver", "hirsel-llvm", Kind::Production).is_err());
    assert!(
        check_edge(
            "hirsel-core-front",
            "hirsel-core-materialize",
            Kind::Production
        )
        .is_err()
    );
    assert!(check_edge("hirsel-cranelift", "hirsel-core-front", Kind::Production).is_err());
    assert!(check_edge("hirsel-cranelift", "hirsel-core-front", Kind::Dev).is_ok());
    assert!(check_edge("hirsel-core-ir", "hirsel-cranelift", Kind::Production).is_err());
    assert!(check_edge("hirsel-core-ir", "hirsel-core-net", Kind::Production).is_ok());
    assert!(check_edge("hirsel-core-rt", "hirsel-core-net", Kind::Production).is_ok());
    assert!(check_edge("hirsel-core-driver", "hirsel-core-net", Kind::Production).is_ok());
    assert!(check_edge("hirsel-core-front", "hirsel-core-net", Kind::Production).is_err());
    assert!(check_edge("hirsel-core-net", "hirsel-core-rt", Kind::Production).is_err());
    assert!(check_edge("hirsel-guard", "hirsel-core-ids", Kind::Production).is_err());
    assert!(
        check_edge(
            "hirsel-core-driver",
            "hirsel-core-contract-eval",
            Kind::Production
        )
        .is_err()
    );
    assert!(check_edge("hirsel-new", "hirsel-core-ir", Kind::Production).is_err());
}
