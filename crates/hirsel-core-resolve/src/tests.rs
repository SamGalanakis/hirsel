use super::*;
use hirsel_core_ids::PackageId;
use hirsel_core_syntax::{ParseMode, Symbols, SyntaxFile, parse};
use std::sync::Arc;
fn package(name: &str) -> PackageId {
    PackageId {
        name: name.into(),
        version: "1".into(),
    }
}
type Files = Vec<(ModuleId, SyntaxFile)>;
fn fixture(sources: &[(&str, &str)]) -> (NameIndex, Files) {
    packages(
        &sources
            .iter()
            .map(|&(m, s)| ("app", m, s))
            .collect::<Vec<_>>(),
        Symbols::default(),
    )
}
fn packages(sources: &[(&str, &str, &str)], mut symbols: Symbols) -> (NameIndex, Files) {
    let prelude = "pub type Option[T] = None | Some(T)\npub type Result[T,E] = Ok(T) | Error(E)";
    let all = std::iter::once(("core", "core", prelude))
        .chain(sources.iter().copied())
        .collect::<Vec<_>>();
    let mut manifests = vec![];
    for &(pkg, _, _) in &all {
        if !manifests
            .iter()
            .any(|m: &PackageManifest| m.package.name == pkg)
        {
            manifests.push(PackageManifest {
                package: package(pkg),
                sources: vec![],
                dependencies: vec![],
            });
        }
    }
    let deps = manifests
        .iter()
        .filter(|m| m.package.name != "app")
        .map(|m| m.package.clone())
        .collect::<Vec<_>>();
    for &(pkg, m, _) in &all {
        let manifest = manifests
            .iter_mut()
            .find(|p| p.package.name == pkg)
            .unwrap();
        manifest.sources.push(ModuleSource {
            file: m.into(),
            modules: vec![m.into()],
            signatures: false,
        });
    }
    for m in &mut manifests {
        if m.package.name == "app" {
            m.dependencies = deps.clone();
        }
        m.intern(&mut symbols);
    }
    let parsed = all
        .iter()
        .map(|&(_, _, s)| {
            let p = parse(s, ParseMode::Signatures, &mut symbols);
            assert!(p.diagnostics.is_empty(), "{s}: {:?}", p.diagnostics);
            p.syntax
        })
        .collect::<Vec<_>>();
    let mut index = NameIndex::new(Arc::new(symbols));
    index.add_manifests(&manifests);
    let mut files = vec![];
    for ((pkg, m, _), syntax) in all.into_iter().zip(parsed) {
        let module = index
            .module(&ModulePath {
                package: package(pkg),
                modules: vec![m.into()],
            })
            .unwrap();
        for (i, item) in syntax.items().enumerate() {
            index.index_item(module, i as u32, item);
        }
        files.push((module, syntax));
    }
    let core = index
        .package(
            index.symbols.find("core").unwrap(),
            index.symbols.find("1").unwrap(),
        )
        .unwrap();
    index.configure_core(core);
    files.remove(0);
    (index, files)
}
fn run(index: &NameIndex, files: &Files, f: usize, i: usize) -> Resolution {
    resolve(index, files[f].0, i as u32, files[f].1.item(i))
}
fn sym(index: &NameIndex, s: &str) -> hirsel_core_syntax::Symbol {
    index.symbols.find(s).unwrap()
}
fn def(index: &NameIndex, m: ModuleId, s: &str) -> DefinitionId {
    index.definition(m, sym(index, s)).unwrap()
}
#[test]
fn lookup_failures_and_shadowed_reads_are_preserved() {
    let (i, f) = fixture(&[("m", "fn f(x: u64) -> u64 { let x = x + 1; absent(x) }")]);
    let r = run(&i, &f, 0, 0);
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
            .any(|r| r.symbol == sym(&i, "absent") && r.result == LookupResult::Missing)
    );
    assert!(
        r.reads
            .iter()
            .any(|r| r.result == LookupResult::Shadowed(Target::Binder(0)))
    );
    assert_eq!(r.binders.len(), 1);
}
#[test]
fn visibility_opaque_construction_and_no_call_labels() {
    let (i, f) = fixture(&[
        (
            "lib",
            "fn hidden() -> u64 { 0 }\npub opaque type Secret { n: u64 }\npub fn visible(x: u64) -> u64 { x }",
        ),
        (
            "main",
            "fn f() { lib.hidden(); lib.Secret(n: 1); lib.visible(x: 1) }",
        ),
    ]);
    let r = run(&i, &f, 1, 0);
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
            .any(|r| r.target == Target::Definition(def(&i, f[0].0, "visible")))
    );
}
#[test]
fn declaration_binders_and_contextual_variants_do_not_need_other_bodies() {
    let (i, f) = fixture(&[(
        "m",
        "type Choice[T] = One(T) | None\nfn f[T: plain, in r](x: T) -> T where r: r { let y = match One(x) { One(v) => v, None => x }; y }",
    )]);
    let r = run(&i, &f, 0, 1);
    assert!(r.diagnostics.is_empty());
    assert!(r.names.iter().any(|u| u.target
        == Target::ContextualConstructor {
            name: sym(&i, "One")
        }));
    assert_eq!(
        i.constructor_candidates(f[0].0, sym(&i, "One"))
            .collect::<Vec<_>>(),
        [def(&i, f[0].0, "Choice")]
    );
    assert_eq!(
        r.binders
            .iter()
            .map(|b| i.symbols.text(b.name))
            .collect::<Vec<_>>(),
        ["T", "r", "x", "v", "y"]
    );
}
#[test]
fn initializer_scope_and_branch_bindings() {
    let (i, f) = fixture(&[(
        "m",
        "fn f(x: u64) -> u64 { if let Some(v) = Some(x) { v } else { x }; v }",
    )]);
    let r = run(&i, &f, 0, 0);
    assert_eq!(r.diagnostics.len(), 1);
    assert!(r.diagnostics[0].message.contains("`v`"));
}
#[test]
fn callee_body_edits_do_not_change_caller_resolution() {
    let (i, f) = fixture(&[(
        "m",
        "fn callee(x: u64) -> u64 { x }\nfn caller() -> u64 { callee(1) }",
    )]);
    let (j, g) = packages(
        &[(
            "app",
            "m",
            "fn callee(x: u64) -> u64 { missing(x) + 42 }\nfn caller() -> u64 { callee(1) }",
        )],
        (*i.symbols).clone(),
    );
    assert_eq!(
        i.definitions().collect::<Vec<_>>(),
        j.definitions().collect::<Vec<_>>()
    );
    let mut a = run(&i, &f, 0, 1);
    let mut b = run(&j, &g, 0, 1);
    for (r, base) in [
        (&mut a, f[0].1.items[1].nodes.start),
        (&mut b, g[0].1.items[1].nodes.start),
    ] {
        for u in &mut r.names {
            u.node.0 -= base;
        }
        for u in &mut r.reads {
            u.node.0 -= base;
        }
    }
    assert_eq!(a.names, b.names);
    assert_eq!(a.reads, b.reads);
    assert!(a.diagnostics.is_empty());
}
#[test]
fn dependencies_and_prelude_keep_neutral_definition_paths() {
    let (i, f) = packages(
        &[
            (
                "dep",
                "api",
                "pub type Number { n: u64 }\npub fn f() -> u64 { 0 }",
            ),
            ("core", "core_extra", "pub type External {}"),
            (
                "app",
                "caller",
                "fn f(x: Option[u64], y: dep.api.Number) -> u64 { dep.api.f() }",
            ),
        ],
        Symbols::default(),
    );
    let r = run(&i, &f, 2, 0);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    let paths = r
        .names
        .iter()
        .filter_map(|u| {
            if let Target::Definition(d) = u.target {
                Some(i.definition_path(d).to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert!(paths.contains(&"core@1/core/Option".into()));
    assert!(paths.contains(&"dep@1/api/Number".into()));
    assert!(paths.contains(&"dep@1/api/f".into()));
}
#[test]
fn callable_result_anchors_and_missing_prelude_targets_are_reads() {
    // Dangling DefPaths no longer exist: a real missing prelude lookup is the input.
    let (i, f) = fixture(&[("m", "fn f(g: scoped fn(x: u64) -> u64 in x, z: Absent) {}")]);
    let r = run(&i, &f, 0, 0);
    assert_eq!(r.diagnostics.len(), 1);
    assert!(
        r.reads
            .iter()
            .any(|r| matches!(r.scope, LookupScope::Prelude(_))
                && r.symbol == sym(&i, "Absent")
                && r.result == LookupResult::Missing)
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
    let (i, f) = fixture(&[("m", &source)]);
    let r = run(&i, &f, 0, 0);
    assert!(r.resource_refusal.is_some());
    assert!(r.diagnostics.is_empty());
}
#[test]
fn locals_may_shadow_modules_and_dependencies_but_not_prelude_functions() {
    let (i, f) = packages(
        &[
            ("dep", "api", "pub fn External() {}"),
            ("core", "core", "pub fn CoreName() {}"),
            ("app", "lib", "pub fn f() {}"),
            (
                "app",
                "m",
                "fn f() { let lib = 1; let dep = 2; let CoreName = 3 }",
            ),
        ],
        Symbols::default(),
    );
    let r = run(&i, &f, 3, 0);
    assert_eq!(
        r.diagnostics
            .iter()
            .map(|d| d.rule.as_str())
            .collect::<Vec<_>>(),
        ["LANG-7"]
    );
    assert_eq!(r.binders.len(), 2);
}
#[test]
fn prelude_functions_precede_modules_in_value_lookup() {
    let (i, f) = packages(
        &[
            ("core", "core", "pub fn helper() -> u64 { 1 }"),
            ("app", "helper", "pub fn other() {}"),
            ("app", "m", "fn f() { helper }"),
        ],
        Symbols::default(),
    );
    let r = run(&i, &f, 2, 0);
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Definition(def(&i, f[0].0, "helper")))
    );
}
#[test]
fn private_variants_do_not_steal_bare_pattern_binders() {
    let (i, f) = fixture(&[
        ("lib", "type Hidden = Only()"),
        ("m", "fn f(x: u64) -> u64 { match x { Only => Only } }"),
    ]);
    let r = run(&i, &f, 1, 0);
    assert!(r.diagnostics.is_empty());
    assert!(r.binders.iter().any(|b| b.name == sym(&i, "Only")));
    assert!(
        r.reads
            .iter()
            .any(|r| matches!(r.scope, LookupScope::Variants(_))
                && r.symbol == sym(&i, "Only")
                && r.result == LookupResult::Missing)
    );
}
#[test]
fn type_names_do_not_resolve_to_same_spelled_modules_or_values() {
    let (i, f) = fixture(&[
        ("char", "pub fn digit(c: char) -> u64 { 0 }"),
        ("m", "fn f[T](T: u64, c: char) -> T { char.digit(c); T }"),
    ]);
    let r = run(&i, &f, 1, 0);
    assert!(r.diagnostics.is_empty());
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Builtin(sym(&i, "char")))
    );
    assert_eq!(
        r.binders.iter().filter(|b| b.name == sym(&i, "T")).count(),
        2
    );
}
#[test]
fn qualified_variants_are_refused() {
    let (i, f) = fixture(&[("m", "type Choice = A()\nfn f() { Choice.A() }")]);
    assert!(
        run(&i, &f, 0, 1)
            .diagnostics
            .iter()
            .any(|d| d.rule == "LANG-7")
    );
}
#[test]
fn core_module_extensions_preserve_members_and_refuse_collisions() {
    let (i, f) = packages(
        &[
            ("core", "list", "pub fn base() {}"),
            ("std", "list", "pub fn extra() {}\npub fn base() {}"),
            ("other", "list", "pub fn extra() {}"),
            ("app", "m", "fn f() { list.base(); list.extra() }"),
        ],
        Symbols::default(),
    );
    assert!(
        i.diagnostics
            .iter()
            .any(|d| d.message == "extension redeclares a core member")
    );
    let r = run(&i, &f, 3, 0);
    assert_eq!(r.diagnostics.len(), 2);
    assert!(r.reads.iter().any(|r| r.result == LookupResult::Ambiguous));
    // The collision input is separate from the successful extension read.
    let (i, f) = packages(
        &[
            ("core", "list", "pub fn base() {}"),
            ("std", "list", "pub fn extra() {}"),
            ("app", "m", "fn f() { list.base(); list.extra() }"),
        ],
        Symbols::default(),
    );
    let r = run(&i, &f, 2, 0);
    assert!(r.diagnostics.is_empty());
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Definition(def(&i, f[0].0, "base")))
    );
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Definition(def(&i, f[1].0, "extra")))
    );
}
#[test]
fn enum_variant_with_its_types_name_is_still_contextual() {
    let (i, f) = fixture(&[("m", "type Closed = Closed(u64)\nfn f() { Closed(1) }")]);
    let r = run(&i, &f, 0, 1);
    assert!(r.diagnostics.is_empty());
    assert!(r.names.iter().any(|u| u.target
        == Target::ContextualConstructor {
            name: sym(&i, "Closed")
        }));
}
#[test]
fn record_and_variant_constructor_collision_waits_for_expected_type() {
    let (i, f) = fixture(&[(
        "m",
        "type Capacity { n: u64 }\ntype Failure = Capacity(u64)\nfn f() { Capacity(1) }",
    )]);
    let r = run(&i, &f, 0, 2);
    assert!(r.diagnostics.is_empty());
    let cs = i
        .constructor_candidates(f[0].0, sym(&i, "Capacity"))
        .collect::<Vec<_>>();
    assert!(cs.contains(&def(&i, f[0].0, "Capacity")));
    assert!(cs.contains(&def(&i, f[0].0, "Failure")));
}
#[test]
fn clean_measurement_keeps_valid_members_of_a_partly_refused_module() {
    let manifests = vec![
        PackageManifest {
            package: package("core"),
            sources: vec![ModuleSource {
                file: "string.hirsel".into(),
                modules: vec!["string".into()],
                signatures: false,
            }],
            dependencies: vec![],
        },
        PackageManifest {
            package: package("app"),
            sources: vec![ModuleSource {
                file: "main.hirsel".into(),
                modules: vec!["main".into()],
                signatures: false,
            }],
            dependencies: vec![package("core")],
        },
    ];
    let c = Corpus {
        manifest: CorpusManifest {
            core: package("core"),
            packages: manifests,
        },
        sources: vec![
            CorpusSource {
                module: ModulePath {
                    package: package("core"),
                    modules: vec!["string".into()],
                },
                file: "string.hirsel".into(),
                mode: ParseMode::Source,
                text: "pub fn good() -> u64 { 1 }\npub fn bad(x: Missing) {}".into(),
            },
            CorpusSource {
                module: ModulePath {
                    package: package("app"),
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
    let (i, f) = fixture(&[
        (
            "main",
            "type u64 {}\nfn main(args: u64) -> Inline[u64, 2] { [args; 2] }",
        ),
        ("args", "fn count() -> u64 { 0 }"),
    ]);
    assert_eq!(i.diagnostics.len(), 1);
    assert_eq!(i.diagnostics[0].rule, "LANG-7");
    let r = run(&i, &f, 0, 1);
    assert!(r.diagnostics.is_empty());
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Builtin(sym(&i, "Inline")))
    );
    assert!(
        r.names
            .iter()
            .any(|u| u.target == Target::Builtin(sym(&i, "u64")))
    );
    let (i, f) = fixture(&[("m", "fn f[u64](x: u64) {}")]);
    assert!(
        run(&i, &f, 0, 0)
            .diagnostics
            .iter()
            .any(|d| d.message == "builtin type name cannot be redeclared")
    );
    let (i, _) = fixture(&[("bytes", "fn f() {}")]);
    assert_eq!(i.diagnostics[0].rule, "LANG-2");
}
#[test]
fn repeated_reads_are_a_set_but_missing_and_shadowed_reads_survive() {
    let (i, f) = fixture(&[(
        "m",
        "fn f(x: u64) { x; x; x; missing; missing; let x = 0; x }",
    )]);
    let r = run(&i, &f, 0, 0);
    assert_eq!(
        r.names
            .iter()
            .filter(|u| u.target == Target::Binder(0))
            .count(),
        4
    );
    assert_eq!(
        r.names
            .iter()
            .filter(|u| u.target == Target::Missing)
            .count(),
        2
    ); // every written use retains its site
    let keys = r
        .reads
        .iter()
        .map(|r| (r.scope, r.symbol, r.namespace, r.constructor))
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(keys.len(), r.reads.len());
    assert_eq!(
        r.reads
            .iter()
            .filter(|r| matches!(r.result, LookupResult::Found(Target::Binder(0))))
            .count(),
        1
    );
    assert!(
        r.reads
            .iter()
            .any(|r| r.symbol == sym(&i, "missing") && r.result == LookupResult::Missing)
    );
    assert!(
        r.reads
            .iter()
            .any(|r| r.result == LookupResult::Shadowed(Target::Binder(0)))
    );
}
#[test]
fn numeric_bounds_only_on_bodiless_core_type_parameters() {
    for (source, allowed) in [
        ("pub fn f[T: integer, F: float](x: T) -> F", true),
        ("pub opaque type Numeric[T: integer]", true),
        ("fn f[T: integer](x: T) -> T { x }", false),
        ("type Numeric[T: float] { n: T }", false),
        ("type Numeric: integer", false),
    ] {
        let (i, f) = packages(&[("core", "numeric", source)], Symbols::default());
        let r = run(&i, &f, 0, 0);
        assert_eq!(
            r.diagnostics.iter().any(|d| d.rule == "OWN-4"),
            !allowed,
            "{source}: {:?}",
            r.diagnostics
        );
        if allowed {
            let (i, f) = packages(&[("app", "numeric", source)], Symbols::default());
            assert!(
                run(&i, &f, 0, 0)
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
        let r = run(&i, &f, 0, 0);
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
    assert!(run(&i, &f, 0, 0).diagnostics.is_empty());
}

#[test]
fn a_local_wins_at_the_start_of_a_dotted_path() {
    let (i, f) = packages(
        &[
            ("core", "args", "pub fn len() -> u64 { 0 }"),
            ("dep", "api", "pub fn len() -> u64 { 0 }"),
            (
                "app",
                "m",
                "fn f(args: u64, dep: u64) { args.len; dep.len }",
            ),
        ],
        Symbols::default(),
    );
    let r = run(&i, &f, 2, 0);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    let paths = r
        .names
        .iter()
        .filter(|u| !u.projections.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0].target, Target::Binder(0));
    assert_eq!(paths[1].target, Target::Binder(1));
    let symbols = paths
        .iter()
        .flat_map(|u| {
            f[2].1.tree.tokens[u.projections.start as usize..u.projections.end as usize]
                .iter()
                .filter(|t| t.kind == hirsel_core_syntax::TokenKind::Ident)
                .map(|t| i.symbols.text(t.symbol))
        })
        .collect::<Vec<_>>();
    assert_eq!(symbols, ["len", "len"]);
}

#[test]
fn clean_measurement_preserves_module_ids_after_removing_a_whole_file() {
    let mut core = PackageManifest {
        package: package("core"),
        sources: vec![],
        dependencies: vec![],
    };
    let mut app = PackageManifest {
        package: package("app"),
        sources: vec![],
        dependencies: vec![package("core")],
    };
    let mut sources = vec![];
    for (pkg, name, text) in [
        ("core", "core", "pub type Option[T] = None | Some(T)"),
        ("app", "bad", "fn bad(x: Missing) {}"),
        (
            "app",
            "main",
            "fn good(x: u64) -> u64 { if x == 0 { 0 } else { good(x - 1) } }",
        ),
    ] {
        let file = std::path::PathBuf::from(format!("{name}.hirsel"));
        let source = ModuleSource {
            file: file.clone(),
            modules: vec![name.into()],
            signatures: false,
        };
        if pkg == "core" {
            core.sources.push(source);
        } else {
            app.sources.push(source);
        }
        sources.push(CorpusSource {
            module: ModulePath {
                package: package(pkg),
                modules: vec![name.into()],
            },
            file,
            text: text.into(),
            mode: ParseMode::Source,
        });
    }
    let c = Corpus {
        manifest: CorpusManifest {
            core: package("core"),
            packages: vec![core, app],
        },
        sources,
    }
    .cleaned();
    let p = c.parse();
    let i = c.index(&p);
    let r = c.analyze(&p, &i);
    assert_eq!(r.files.len(), 2);
    assert!(r.files.iter().all(|f| f.resolved));
    let good = i
        .definitions()
        .find(|&(_, d)| i.symbols.text(d.name) == "good")
        .unwrap()
        .0;
    assert_eq!(i.module_path(i.get(good).site.module).modules, ["main"]);
}

#[test]
fn syntax_refusals_do_not_cascade_and_recovery_keeps_the_next_definition() {
    let (index, files) = fixture(&[("m", "fn good(x: u64) -> u64 { x }")]);
    let module = files[0].0;
    let mut symbols = index.symbols.as_ref().clone();
    let p = parse(
        "fn bad() { let x; x = 1; x }\nfn good(x: u64) -> u64 { x }",
        ParseMode::Source,
        &mut symbols,
    );
    assert_eq!(p.diagnostics.len(), 1);
    let file = &p.syntax;
    assert!(file.items[0].has_errors);
    assert!(!file.items[1].has_errors);
    let bad = resolve(&index, module, 0, file.item(0));
    assert!(bad.names.is_empty() && bad.diagnostics.is_empty());
    let good = resolve(&index, module, 1, file.item(1));
    assert!(good.diagnostics.is_empty());
    assert_eq!(good.names.len(), 3);
    let recovery = parse(
        "fn bad() { 1\nfn good(x: u64) -> u64 { x }",
        ParseMode::Source,
        &mut Symbols::default(),
    );
    assert!(recovery.syntax.items[0].has_errors);
    assert!(!recovery.syntax.items[1].has_errors);
}

#[test]
fn repeated_path_results_keep_dependency_reads_and_binding_versions() {
    let (index, files) = fixture(&[("m", "fn f() { x; let x = 1; x; x; if true { x; x } }")]);
    let result = run(&index, &files, 0, 0);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(
        result
            .names
            .iter()
            .filter(|u| u.target == Target::Missing)
            .count(),
        1
    );
    assert_eq!(
        result
            .names
            .iter()
            .filter(|u| matches!(u.target, Target::Binder(_)))
            .count(),
        4
    );
    let name = sym(&index, "x");
    assert!(
        result
            .reads
            .iter()
            .any(|r| r.symbol == name && r.result == LookupResult::Missing)
    );
    assert!(result.reads.iter().any(|r|r.symbol==name&&matches!(r.result,LookupResult::Found(Target::Binder(_)))));
    for (i, r) in result.reads.iter().enumerate() {
        assert!(!result.reads[..i].iter().any(|p| p.scope == r.scope
            && p.symbol == r.symbol
            && p.namespace == r.namespace
            && p.constructor == r.constructor));
    }
}
