//! LANG-6 definition hashes: renames keep every hash, a body edit moves its
//! own hash and every transitive dependent's, recursive groups hash without
//! names, and identical definitions share a hash wherever they live.
use hirsel_engine::*;
use hirsel_front_phases::*;
use hirsel_host_io::launch::*;
use hirsel_intern::jobs::with_compiler_stack;
use hirsel_proof_export::{CallSiteKind, DefinitionContent};
use hirsel_syntax::ContentHash;
use hirsel_types::SignatureKind;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

fn named(name: &str) -> DefPath {
    let mut path = entry_path(&default_context());
    path.0.name = name.into();
    path
}

fn engine(files: &[(&str, &[&str], &str)]) -> QueryEngine {
    let mut engine = QueryEngine::new(default_environment(), compiler_adapters());
    for (file, modules, text) in files {
        let context = FileContext {
            modules: modules.iter().map(|m| m.to_string()).collect(),
            ..default_context()
        };
        let file = engine.sources.add(*file, (*text).into()).unwrap();
        engine.parse_file(file, &context).unwrap();
    }
    engine
}

/// The hashes of `names` in the one-file program `source`.
fn hashes(source: &str, names: &[&str]) -> Vec<ContentHash> {
    let mut engine = engine(&[("program.hir", &[], source)]);
    let roots: Vec<_> = names.iter().map(|n| named(n)).collect();
    let all = engine
        .definition_hashes(roots.clone())
        .unwrap_or_else(|e| panic!("{source}\n{e:?}"))
        .hashes;
    roots.iter().map(|r| all[r]).collect()
}

const CHAIN: &str = "fn leaf(x: u64) -> u64 {\n  let doubled = x * 2u64\n  doubled + 1u64\n}\nfn mid(n: u64) -> u64 { leaf(n) + leaf(1u64) }\nfn other(n: u64) -> u64 { n }\npub fn main() -> Result[(), ()] {\n  let n = mid(3u64) + other(1u64)\n  Ok(())\n}\n";

#[test]
fn renaming_a_definition_a_parameter_or_a_local_keeps_its_hash_and_its_dependents() {
    with_compiler_stack(|| {
        let base = hashes(CHAIN, &["leaf", "mid", "main"]);
        let renamed = CHAIN
            .replace("leaf", "twice")
            .replace("doubled", "d")
            .replace("x: u64", "value: u64")
            .replace("x * 2u64", "value * 2u64")
            .replace("mid", "middle");
        assert_eq!(base, hashes(&renamed, &["twice", "middle", "main"]));
        // Moving definitions and editing comments and layout changes nothing.
        let moved = format!(
            "// leading comment\n\n{}",
            CHAIN.replace("fn other(n: u64) -> u64 { n }\n", "")
        ) + "fn other(n: u64) -> u64 {\n  n\n}\n";
        assert_eq!(base, hashes(&moved, &["leaf", "mid", "main"]));
    });
}

#[test]
fn a_body_edit_changes_its_hash_and_every_transitive_dependent_only() {
    with_compiler_stack(|| {
        let names = ["leaf", "mid", "main", "other"];
        let base = hashes(CHAIN, &names);
        let edited = hashes(&CHAIN.replace("doubled + 1u64", "doubled + 2u64"), &names);
        for (i, name) in names[..3].iter().enumerate() {
            assert_ne!(base[i], edited[i], "{name}");
        }
        assert_eq!(base[3], edited[3], "other does not reach leaf");
        // A signature edit is a content edit too.
        let signature = hashes(
            &CHAIN.replace("fn other(n: u64)", "fn other(take n: u64)"),
            &names,
        );
        assert_eq!(base[0], signature[0]);
        assert_ne!(base[3], signature[3]);
        assert_ne!(base[2], signature[2]);
    });
}

const EVEN_ODD: &str = "fn even(n: u64) -> bool { if n == 0u64 { true } else { odd(n - 1u64) } }\nfn odd(n: u64) -> bool { if n == 0u64 { false } else { even(n - 1u64) } }\npub fn main() -> Result[(), ()] {\n  let b = even(4u64)\n  Ok(())\n}\n";

#[test]
fn mutual_recursion_hashes_without_names_or_declaration_order() {
    with_compiler_stack(|| {
        let names = ["even", "odd", "main"];
        let base = hashes(EVEN_ODD, &names);
        assert_ne!(base[0], base[1], "the members' bodies differ");
        let renamed = EVEN_ODD.replace("even", "is_even").replace("odd", "is_odd");
        assert_eq!(base, hashes(&renamed, &["is_even", "is_odd", "main"]));
        let mut lines: Vec<_> = EVEN_ODD.lines().collect();
        lines.swap(0, 1);
        assert_eq!(base, hashes(&(lines.join("\n") + "\n"), &names));
        // An edit to one member moves the whole group and its dependents.
        let edited = hashes(&EVEN_ODD.replace("{ false }", "{ n == 7u64 }"), &names);
        for i in 0..3 {
            assert_ne!(base[i], edited[i], "{}", names[i]);
        }
        // Self recursion is a group of one.
        let fact = "fn fact(n: u64) -> u64 { if n == 0u64 { 1u64 } else { n * fact(n - 1u64) } }\n";
        assert_eq!(
            hashes(fact, &["fact"]),
            hashes(&fact.replace("fact", "factorial"), &["factorial"])
        );
        assert_ne!(
            hashes(fact, &["fact"]),
            hashes(&fact.replace("{ 1u64 }", "{ 2u64 }"), &["fact"])
        );
    });
}

#[test]
fn identical_definitions_in_different_modules_share_a_hash() {
    with_compiler_stack(|| {
        let helper = "pub fn helper(n: u64) -> u64 {\n  let m = n + 1u64\n  m * m\n}\n";
        let renamed = "pub fn square_next(k: u64) -> u64 { let j = k + 1u64; j * j }\n";
        let mut engine = engine(&[
            (
                "program.hir",
                &[],
                "pub fn main() -> Result[(), ()] { Ok(()) }\n",
            ),
            ("first.hir", &["first"], helper),
            ("second.hir", &["second"], renamed),
        ]);
        let in_module = |module: &str, name: &str| {
            let mut path = named(name);
            path.0.modules = vec![module.into()];
            path
        };
        let (first, second) = (
            in_module("first", "helper"),
            in_module("second", "square_next"),
        );
        let all = engine
            .definition_hashes([first.clone(), second.clone()])
            .unwrap()
            .hashes;
        assert_eq!(all[&first], all[&second]);
    });
}

const CORE_USERS: &str = "fn f(x: u64) -> u64 { x + 1u64 }
fn half(x: u64) -> Result[u64, ()] {
  if x == 0u64 { Error(()) } else { Ok(x / 2u64) }
}
fn tried(x: u64) -> Result[u64, ()] {
  let v = half(x)?
  Ok(v + 1u64)
}
fn matched(x: u64) -> u64 {
  match half(x) {
    Ok(v) => v,
    Error(_) => 0u64,
  }
}
pub fn launch(tasks: Tasks) -> () {
  task.scope(
    tasks,
    fn(s: Scope) {
      ()
    }
  )
}
";

/// The closure of `roots` as the engine hashes it: a program definition by its
/// signature and TIR, a core one by its path and signature.
fn closure(engine: &mut QueryEngine, roots: &[DefPath]) -> BTreeMap<DefPath, DefinitionContent> {
    let mut contents = BTreeMap::new();
    let mut pending = roots.to_vec();
    while let Some(path) = pending.pop() {
        if contents.contains_key(&path) {
            continue;
        }
        let signature = DefinitionQueries::item_signature(engine, &path)
            .unwrap()
            .value;
        let content = if path.0.package.name == "core" {
            DefinitionContent::Named {
                signature,
                source: vec![],
                reads: vec![],
            }
        } else {
            let tir = DefinitionQueries::lower_to_tir(engine, &path)
                .unwrap()
                .value;
            DefinitionContent::Body {
                signature,
                tir: Some(tir),
            }
        };
        pending.extend(hirsel_proof_export::references(engine.types(), &content));
        contents.insert(path, content);
    }
    contents
}

#[test]
fn call_sites_record_each_callee_hash() {
    with_compiler_stack(|| {
        let source = "fn helper(n: u64) -> u64 { n }\npub fn main() -> Result[(), ()] {\n  let n = helper(1u64)\n  let f = helper\n  let m = f(2u64)\n  Ok(())\n}\n";
        let mut engine = engine(&[("program.hir", &[], source)]);
        let main = named("main");
        let result = engine.definition_hashes([main.clone()]).unwrap();
        let helper = result.hashes[&named("helper")];
        let kinds: BTreeMap<_, _> = result.call_sites[&main]
            .iter()
            .filter(|site| site.callee == named("helper"))
            .map(|site| {
                assert_eq!(site.hash, helper);
                (matches!(site.kind, CallSiteKind::Call(_)), site.hash)
            })
            .collect();
        assert_eq!(kinds.len(), 2, "{:?}", result.call_sites[&main]);
    });
}
