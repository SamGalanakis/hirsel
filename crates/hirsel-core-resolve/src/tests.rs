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
    let core = ModuleId {
        package: PackageId {
            name: "core".into(),
            version: "1".into(),
        },
        modules: vec!["core".into()],
    };
    let prelude = parse(
        "pub type Option[T] = None | Some(T)\npub type Result[T, E] = Ok(T) | Error(E)",
        ParseMode::Source,
    );
    for (i, item) in prelude.syntax.items.iter().enumerate() {
        index.index_item(&core, i as u32, item);
    }
    index.configure_core(core.package);
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
    assert!(r.names.iter().any(|r|matches!(&r.target,Target::ContextualConstructor {name,candidates} if name=="One"&&candidates==&[module.definition("Choice")])));
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

#[test]
fn locals_cannot_shadow_modules_dependencies_or_core_names() {
    let (mut index, files) = fixture(&[
        ("lib", "pub fn External() {}"),
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
fn prelude_functions_precede_modules_in_value_lookup() {
    let (mut index, files) = fixture(&[
        ("lib", "pub fn helper() -> u64 { 1 }"),
        ("m", "fn f() { helper }"),
    ]);
    let (m, f) = &files[1];
    index
        .environment
        .prelude
        .insert("helper".into(), files[0].0.definition("helper"));
    index.add_module(&ModuleId {
        package: m.package.clone(),
        modules: vec!["helper".into()],
    });
    let r = resolve(&index, m, 0, &f.items[0]);
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Definition(files[0].0.definition("helper")))
    );
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

#[test]
fn type_names_do_not_resolve_to_same_spelled_modules_or_values() {
    let (index, files) = fixture(&[
        ("char", "pub fn digit(c: char) -> u64 { 0 }"),
        ("m", "fn f[T](T: u64, c: char) -> T { char.digit(c); T }"),
    ]);
    let (m, f) = &files[1];
    let r = resolve(&index, m, 0, &f.items[0]);
    assert_eq!(r.diagnostics, vec![]);
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Builtin("char".into()))
    );
    assert_eq!(r.binders.iter().filter(|b| b.name == "T").count(), 2);
}
#[test]
fn qualified_variants_are_refused() {
    let (index, files) = fixture(&[("m", "type Choice = A()\nfn f() { Choice.A() }")]);
    let (m, f) = &files[0];
    let r = resolve(&index, m, 1, &f.items[1]);
    assert!(r.diagnostics.iter().any(|d| d.rule == "LANG-7"));
}

#[test]
fn core_module_extensions_preserve_members_and_refuse_collisions() {
    let (mut index, files) = fixture(&[("m", "fn f() { list.base(); list.extra() }")]);
    let core = PackageId {
        name: "core".into(),
        version: "1".into(),
    };
    let std = PackageId {
        name: "std".into(),
        version: "1".into(),
    };
    let other = PackageId {
        name: "other".into(),
        version: "1".into(),
    };
    for (pkg, source) in [
        (core.clone(), "pub fn base() {}"),
        (std.clone(), "pub fn extra() {}"),
        (other.clone(), "pub fn extra() {}"),
    ] {
        let m = ModuleId {
            package: pkg,
            modules: vec!["list".into()],
        };
        let p = parse(source, ParseMode::Source);
        for (i, item) in p.syntax.items.iter().enumerate() {
            index.index_item(&m, i as u32, item);
        }
    }
    let (m, f) = &files[0];
    for pkg in [&std, &other] {
        index
            .environment
            .dependencies
            .entry(m.package.clone())
            .or_default()
            .insert(
                pkg.name.clone(),
                ModuleId {
                    package: pkg.clone(),
                    modules: vec![],
                },
            );
    }
    index.configure_core(core.clone());
    let r = resolve(&index, m, 0, &f.items[0]);
    assert_eq!(r.diagnostics.len(), 1);
    assert!(
        r.reads
            .iter()
            .any(|r| matches!(&r.result,LookupResult::Ambiguous(paths) if paths.len()==2))
    );
    assert!(r.names.iter().any(|u| {
        u.target
            == Target::Definition(
                ModuleId {
                    package: core.clone(),
                    modules: vec!["list".into()],
                }
                .definition("base"),
            )
    }));
    let p = parse("pub fn base() {}", ParseMode::Source);
    index.index_item(
        &ModuleId {
            package: std,
            modules: vec!["list".into()],
        },
        0,
        &p.syntax.items[0],
    );
    index.configure_core(core);
    assert!(
        index
            .diagnostics
            .iter()
            .any(|d| d.message == "extension redeclares a core member")
    );
}
#[test]
fn enum_variant_with_its_types_name_is_still_contextual() {
    let (index, files) = fixture(&[("m", "type Closed = Closed(u64)\nfn f() { Closed(1) }")]);
    let (m, f) = &files[0];
    let r = resolve(&index, m, 1, &f.items[1]);
    assert_eq!(r.diagnostics, vec![]);
    assert!(
        r.names
            .iter()
            .any(|u| matches!(&u.target,Target::ContextualConstructor {name,..} if name=="Closed"))
    );
}

#[test]
fn record_and_variant_constructor_collision_waits_for_expected_type() {
    let (index, files) = fixture(&[(
        "m",
        "type Capacity { n: u64 }\ntype Failure = Capacity(u64)\nfn f() { Capacity(1) }",
    )]);
    let (m, f) = &files[0];
    let r = resolve(&index, m, 2, &f.items[2]);
    assert_eq!(r.diagnostics, vec![]);
    assert!(r.names.iter().any(|u|matches!(&u.target,Target::ContextualConstructor {candidates,..} if candidates.contains(&m.definition("Capacity"))&&candidates.contains(&m.definition("Failure")))));
}

#[test]
fn clean_measurement_keeps_valid_members_of_a_partly_refused_module() {
    let core = PackageId {
        name: "core".into(),
        version: "1".into(),
    };
    let app = PackageId {
        name: "app".into(),
        version: "1".into(),
    };
    let manifest = CorpusManifest {
        core: core.clone(),
        packages: vec![PackageManifest {
            package: app.clone(),
            sources: vec![],
            dependencies: vec![core.clone()],
        }],
    };
    let c = Corpus {
        manifest,
        sources: vec![
            CorpusSource {
                module: ModuleId {
                    package: core,
                    modules: vec!["string".into()],
                },
                file: "string.hirsel".into(),
                mode: ParseMode::Source,
                text: "pub fn good() -> u64 { 1 }\npub fn bad(x: Missing) {}".into(),
            },
            CorpusSource {
                module: ModuleId {
                    package: app,
                    modules: vec!["main".into()],
                },
                file: "main.hirsel".into(),
                mode: ParseMode::Source,
                text: "fn f() -> u64 { string.good() }".into(),
            },
        ],
    };
    let clean = c.cleaned();
    let p = clean.parse();
    let i = clean.index(&p);
    let r = clean.analyze(&p, &i);
    assert_eq!(r.files.len(), 2);
    assert!(r.files.iter().all(|f| f.resolved));
    assert_eq!(r.files.iter().map(|f| f.items).sum::<usize>(), 2);
}

#[test]
fn revision_four_builtin_names_and_visible_modules() {
    let (index, files) = fixture(&[
        (
            "main",
            "type u64 {}\nfn main(args: u64) -> Inline[u64, 2] { [args; 2] }",
        ),
        ("args", "fn count() -> u64 { 0 }"),
    ]);
    assert_eq!(index.diagnostics.len(), 1);
    assert_eq!(index.diagnostics[0].rule, "LANG-7");
    let (module, file) = &files[0];
    let r = resolve(&index, module, 1, &file.items[1]);
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.rule == "LANG-7" && d.message.contains("shadows"))
    );
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Builtin("Inline".into()))
    );
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Builtin("u64".into()))
    );
    let p = parse("fn f[u64](x: u64) {}", ParseMode::Source);
    assert!(
        resolve(&index, module, 0, &p.syntax.items[0])
            .diagnostics
            .iter()
            .any(|d| d.message == "builtin type name cannot be redeclared")
    );
    let (index, _) = fixture(&[("bytes", "fn f() {}")]);
    assert_eq!(index.diagnostics[0].rule, "LANG-2");
}

#[test]
fn numeric_bounds_only_on_bodiless_core_type_parameters() {
    let core = module("core");
    let mut index = NameIndex::default();
    index.configure_core(core.package.clone());
    for (source, mode, allowed) in [
        (
            "pub fn f[T: integer, F: float](x: T) -> F",
            ParseMode::Signatures,
            true,
        ),
        (
            "pub opaque type Numeric[T: integer]",
            ParseMode::Signatures,
            true,
        ),
        (
            "fn f[T: integer](x: T) -> T { x }",
            ParseMode::Source,
            false,
        ),
        ("type Numeric[T: float] { n: T }", ParseMode::Source, false),
        ("type Numeric: integer", ParseMode::Signatures, false),
    ] {
        let p = parse(source, mode);
        assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
        let r = resolve(&index, &core, 0, &p.syntax.items[0]);
        assert_eq!(
            r.diagnostics.iter().any(|d| d.rule == "OWN-4"),
            !allowed,
            "{source}: {:?}",
            r.diagnostics
        );
        if allowed {
            let external = ModuleId {
                package: PackageId {
                    name: "other".into(),
                    version: "1".into(),
                },
                modules: vec!["api".into()],
            };
            assert!(
                resolve(&index, &external, 0, &p.syntax.items[0])
                    .diagnostics
                    .iter()
                    .any(|d| d.rule == "OWN-4")
            );
        }
    }
}
#[test]
fn exits_resolve_only_to_legal_targets_in_the_current_function() {
    for source in [
        "fn f() { break }",
        "fn f() { continue }",
        "fn f() { 'b: { continue 'b } }",
        "fn f() { while true { break 1 } }",
        "fn f() { 'w: while true { break 'w 1 } }",
        "fn f() { loop { let g = fn() { break }; } }",
    ] {
        let (i, f) = fixture(&[("m", source)]);
        let r = resolve(&i, &f[0].0, 0, &f[0].1.items[0]);
        assert!(
            r.diagnostics.iter().any(|d| d.rule == "LANG-15"),
            "{source}: {:?}",
            r.diagnostics
        );
    }
    let (i, f) = fixture(&[(
        "m",
        "fn f() { 'b: { break 'b 1 }; 'w: while true { continue 'w }; loop { break 2 } }",
    )]);
    assert!(
        resolve(&i, &f[0].0, 0, &f[0].1.items[0])
            .diagnostics
            .is_empty()
    );
}
