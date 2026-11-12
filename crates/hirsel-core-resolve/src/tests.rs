use super::*;
use hirsel_core_ids::PackageId;
use hirsel_core_syntax::{ParseMode, parse};
fn module(name: &str) -> ModuleId {
    ModuleId {
        package: PackageId {
            name: "app".into(),
            version: "1".into(),
        },
        modules: vec![name.into()],
    }
}
fn fixture(
    sources: &[(&str, &str)],
) -> (NameIndex, Vec<(ModuleId, hirsel_core_syntax::SyntaxFile)>) {
    let mut index = NameIndex::default();
    let mut files = vec![];
    for &(name, text) in sources {
        let module = module(name);
        let p = parse(text, ParseMode::Source);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        for (i, item) in p.syntax.items.iter().enumerate() {
            index.index_item(&module, i as u32, item);
        }
        files.push((module, p.syntax));
    }
    (index, files)
}
#[test]
fn lookup_failures_and_shadowed_reads_are_preserved() {
    let (index, files) = fixture(&[("m", "fn f(x: u64) -> u64 { let x = x + 1; absent(x) }")]);
    let (module, file) = &files[0];
    let r = resolve(&index, module, 0, &file.items[0]);
    assert_eq!(
        r.diagnostics
            .iter()
            .map(|d| d.rule.as_str())
            .collect::<Vec<_>>(),
        ["LANG-7", "LANG-7"]
    );
    assert!(
        r.reads
            .iter()
            .any(|r| r.spelling == ["absent"] && r.result == LookupResult::Missing)
    );
    assert!(
        r.reads
            .iter()
            .any(|r| matches!(r.result, LookupResult::Shadowed(Target::Binder(0))))
    );
    assert_eq!(r.binders.len(), 1);
}
#[test]
fn visibility_opaque_construction_and_no_call_labels() {
    let (index, files) = fixture(&[
        (
            "lib",
            "fn hidden() -> u64 { 0 }\npub opaque type Secret { n: u64 }\npub fn visible(x: u64) -> u64 { x }",
        ),
        (
            "main",
            "fn f() { lib.hidden(); lib.Secret(n: 1); lib.visible(x: 1) }",
        ),
    ]);
    let (module, file) = &files[1];
    let r = resolve(&index, module, 0, &file.items[0]);
    assert_eq!(
        r.diagnostics
            .iter()
            .map(|d| d.rule.as_str())
            .collect::<Vec<_>>(),
        ["PROG-2", "PROG-2", "LANG-9"]
    );
    assert!(
        r.names
            .iter()
            .any(|r| r.target == Target::Definition(files[0].0.definition("visible")))
    );
}
#[test]
fn declaration_binders_and_contextual_variants_do_not_need_other_bodies() {
    let (index, files) = fixture(&[(
        "m",
        "type Choice[T] = One(T) | None\nfn f[T: plain, in r](x: T) -> T where r: r { let y = match One(x) { One(v) => v, None => x }; y }",
    )]);
    let (module, file) = &files[0];
    let r = resolve(&index, module, 1, &file.items[1]);
    assert_eq!(r.diagnostics, vec![]);
    assert!(r.names.iter().any(|r|matches!(&r.target,Target::ContextualVariant {name,candidates} if name=="One"&&candidates==&[module.definition("Choice")])));
    assert_eq!(
        r.binders
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        ["T", "r", "x", "v", "y"]
    );
}
#[test]
fn initializer_scope_and_branch_bindings() {
    let (index, files) = fixture(&[(
        "m",
        "fn f(x: u64) -> u64 { if let Some(v) = Some(x) { v } else { x }; v }",
    )]);
    let (module, file) = &files[0];
    let r = resolve(&index, module, 0, &file.items[0]);
    assert_eq!(r.diagnostics.len(), 1);
    assert!(r.diagnostics[0].message.contains("`v`"));
}

#[test]
fn callee_body_edits_do_not_change_caller_resolution() {
    let (before, files) = fixture(&[(
        "m",
        "fn callee(x: u64) -> u64 { x }\nfn caller() -> u64 { callee(1) }",
    )]);
    let (after, _) = fixture(&[(
        "m",
        "fn callee(x: u64) -> u64 { missing(x) + 42 }\nfn caller() -> u64 { callee(1) }",
    )]);
    assert_eq!(
        before.definitions().collect::<Vec<_>>(),
        after.definitions().collect::<Vec<_>>()
    );
    let (m, f) = &files[0];
    let a = resolve(&before, m, 1, &f.items[1]);
    let b = resolve(&after, m, 1, &f.items[1]);
    assert_eq!(a.names, b.names);
    assert_eq!(a.reads, b.reads);
    assert_eq!(a.diagnostics, vec![]);
}
#[test]
fn dependencies_and_prelude_keep_neutral_definition_paths() {
    let (mut index, files) = fixture(&[
        ("lib", "pub type Number { n: u64 }\npub fn f() -> u64 { 0 }"),
        ("caller", "fn f(x: External) -> u64 { dep.f() }"),
    ]);
    let library = ModuleId {
        package: PackageId {
            name: "other".into(),
            version: "2".into(),
        },
        modules: vec!["api".into()],
    };
    let defs = index
        .definitions()
        .filter(|d| d.site.module.modules == ["lib"])
        .cloned()
        .collect::<Vec<_>>();
    for mut d in defs {
        d.path = library.definition(&d.path.0.name);
        d.site.module = library.clone();
        index.add(d);
    }
    let (m, f) = &files[1];
    index
        .environment
        .dependencies
        .entry(m.package.clone())
        .or_default()
        .insert("dep".into(), library.clone());
    index
        .environment
        .prelude
        .insert("External".into(), library.definition("Number"));
    let r = resolve(&index, m, 0, &f.items[0]);
    assert_eq!(r.diagnostics, vec![]);
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Definition(library.definition("Number")))
    );
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Definition(library.definition("f")))
    );
}

#[test]
fn brands_resolve_to_owner_binders_and_sibling_fields() {
    let (index, files) = fixture(&[(
        "m",
        "pub opaque type Pool {}\ntype Id {}\ntype Row { pool: Pool, id: Id of .pool }\nresource type Owned of p { id: Id of p } drop(s, mut p) { p }\n",
    )]);
    let (m, f) = &files[0];
    let row = resolve(&index, m, 2, &f.items[2]);
    assert_eq!(row.diagnostics, vec![]);
    assert!(row.names.iter().any(|u| u.target
        == Target::Field {
            owner: m.definition("Row"),
            name: "pool".into()
        }));
    let owned = resolve(&index, m, 3, &f.items[3]);
    assert_eq!(owned.diagnostics, vec![]);
    assert!(
        owned
            .binders
            .iter()
            .any(|b| b.name == "p" && b.kind == BinderKind::Brand)
    );
}

#[test]
fn callable_result_anchors_and_missing_prelude_targets_are_reads() {
    let (mut index, files) =
        fixture(&[("m", "fn f(g: scoped fn(x: u64) -> u64 in x, z: Absent) {}")]);
    let (m, f) = &files[0];
    index
        .environment
        .prelude
        .insert("Absent".into(), m.definition("not_declared"));
    let r = resolve(&index, m, 0, &f.items[0]);
    assert_eq!(r.diagnostics.len(), 1);
    assert!(
        r.reads
            .iter()
            .any(|r| r.spelling == ["not_declared"] && r.result == LookupResult::Missing)
    );
    assert!(
        r.names
            .iter()
            .any(|u| matches!(u.target, Target::Binder(_)))
    );
}
#[test]
fn long_expression_tree_has_an_explicit_compiler_resource_outcome() {
    let source = format!("fn f() -> u64 {{ {}1 }}", "1 + ".repeat(2000));
    let (index, files) = fixture(&[("m", &source)]);
    let (m, f) = &files[0];
    let r = resolve(&index, m, 0, &f.items[0]);
    assert!(r.resource_refusal.is_some());
    assert_eq!(r.diagnostics, vec![]);
}

fn seed_corpus() -> Vec<ParsedModule> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut modules = vec![];
    for (dir, pkg, signatures) in [
        ("core/std", "core", true),
        ("core/syntax-examples", "example", false),
    ] {
        modules.extend(
            PackageManifest::directory(
                &root.join(dir),
                PackageId {
                    name: pkg.into(),
                    version: "1".into(),
                },
                signatures,
            )
            .unwrap()
            .read()
            .unwrap(),
        );
    }
    modules
}
#[test]
fn seed_corpus_token_tapes_roundtrip_every_byte() {
    let modules = seed_corpus();
    assert!(!modules.is_empty());
    for m in &modules {
        let original = std::fs::read_to_string(&m.file).unwrap();
        assert_eq!(m.parsed.syntax.text(), original, "{}", m.file.display());
    }
    println!(
        "{} complete source files round-tripped through token tapes",
        modules.len()
    );
}
#[test]
fn seed_corpus_parses_and_resolves_without_refusals() {
    let modules = seed_corpus();
    let mut environment = Environment::default();
    for m in modules.iter().filter(|m| m.module.package.name == "core") {
        let head = &m.module.modules[0];
        environment
            .dependencies
            .entry(PackageId {
                name: "example".into(),
                version: "1".into(),
            })
            .or_default()
            .insert(head.clone(), m.module.clone());
    }
    let mut index = NameIndex::new(environment);
    for m in &modules {
        index.add_module(&m.module);
        for (i, item) in m.parsed.syntax.items.iter().enumerate() {
            index.index_item(&m.module, i as u32, item);
        }
    }
    let mut failures = vec![];
    for m in &modules {
        for d in &m.parsed.diagnostics {
            failures.push(format!("{} {} {}", m.file.display(), d.rule, d.message));
        }
        assert!(m.parsed.resource_refusal.is_none());
        for (i, item) in m.parsed.syntax.items.iter().enumerate() {
            let r = resolve(&index, &m.module, i as u32, item);
            assert!(r.resource_refusal.is_none());
            for d in r.diagnostics {
                failures.push(format!("{} {} {}", m.file.display(), d.rule, d.message));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} refusals in the spec/seed corpus:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn locals_cannot_shadow_modules_dependencies_or_core_names() {
    let (mut index, files) = fixture(&[
        ("lib", "pub type External {}"),
        ("m", "fn f() { let lib = 1; let dep = 2; let CoreName = 3 }"),
    ]);
    let (m, f) = &files[1];
    index
        .environment
        .dependencies
        .entry(m.package.clone())
        .or_default()
        .insert("dep".into(), files[0].0.clone());
    index
        .environment
        .prelude
        .insert("CoreName".into(), files[0].0.definition("External"));
    let r = resolve(&index, m, 0, &f.items[0]);
    assert_eq!(
        r.diagnostics
            .iter()
            .map(|d| d.rule.as_str())
            .collect::<Vec<_>>(),
        ["LANG-7", "LANG-7", "LANG-7"]
    );
    assert_eq!(r.binders.len(), 0);
}

#[test]
fn missing_module_heads_are_incremental_reads_before_core_fallback() {
    let (mut index, files) = fixture(&[
        ("lib", "pub fn helper() -> u64 { 1 }"),
        ("m", "fn f() { helper }"),
    ]);
    let (m, f) = &files[1];
    index
        .environment
        .prelude
        .insert("helper".into(), files[0].0.definition("helper"));
    let before = resolve(&index, m, 0, &f.items[0]);
    assert!(
        before
            .reads
            .iter()
            .any(|r| r.scope == LookupScope::Modules(m.package.clone())
                && r.spelling == ["helper"]
                && r.result == LookupResult::Missing)
    );
    let head = ModuleId {
        package: m.package.clone(),
        modules: vec!["helper".into()],
    };
    index.add_module(&head);
    let after = resolve(&index, m, 0, &f.items[0]);
    assert!(
        after
            .names
            .iter()
            .any(|u| u.target == Target::Module(head.clone()))
    );
    assert_ne!(before.names, after.names);
}

#[test]
fn private_variants_do_not_steal_bare_pattern_binders() {
    let (index, files) = fixture(&[
        ("lib", "type Hidden = Only()"),
        ("m", "fn f(x: u64) -> u64 { match x { Only => Only } }"),
    ]);
    let (m, f) = &files[1];
    let r = resolve(&index, m, 0, &f.items[0]);
    assert_eq!(r.diagnostics, vec![]);
    assert!(r.binders.iter().any(|b| b.name == "Only"));
    assert!(r.reads.iter().any(|r| r.scope == LookupScope::Variants
        && r.spelling == ["Only"]
        && r.result == LookupResult::Missing));
}
