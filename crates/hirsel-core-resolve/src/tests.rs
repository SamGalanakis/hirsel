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
        ["PROG-2", "PROG-2", "LANG-5"]
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
