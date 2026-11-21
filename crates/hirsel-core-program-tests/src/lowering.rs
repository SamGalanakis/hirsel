//! Whole-program laws retain their independent oracles on product units.
use crate::cases::{inputs_with_core, program_inputs};
use hirsel_core_front::*;
use hirsel_core_ids::PackageId;
use hirsel_core_ir::{FuncId, Param, Ty, eval, wf};
use hirsel_core_resolve::{ModuleSource, NameIndex, PackageManifest};
use hirsel_core_syntax::{ParseMode, Symbols, parse};
use std::path::PathBuf;
fn root() -> PathBuf {
    crate::repo().join("core/front")
}
fn package(name: &str) -> PackageId {
    crate::package(name)
}
#[test]
fn scalar_instances_use_closed_signatures() {
    let (index, modules) = program_inputs(
        "fn id[T](take x:T)->T { x }\n\
         fn borrow[T](x:View[T])->View[T] in x { x }\n\
         fn edit[T](mut x:T) {}\n\
         type W { x:u64 }\n\
         fn run()->u64 { let mut x=7u64;edit(mut x);let a=id(x);let _=id(W(x:8));let _=id((9u64,));let _=id(()); borrow(view a) }",
    );
    let _p = Program::declare(&index, &modules);
    let (module, _functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

    let module = &*module;
    wf::module(module).unwrap();
    for f in &module.functions {
        let key = &module.instances[f.instance.index()];
        if !key.definition.package_is("app", "1") || module.argument_names(key).is_empty() {
            continue;
        }
        let sig = &module.signatures[f.signature.index()];
        match key.definition.0.name.as_str() {
            "edit" => assert!(matches!(sig.params.as_slice(), [Param::Address { .. }])),
            "id" | "borrow" => {
                let row = module
                    .types
                    .iter()
                    .find(|t| t.name == module.argument_names(key)[0])
                    .unwrap();
                match row.carried {
                    hirsel_core_ir::Carried::Value(t) => {
                        assert_eq!(sig.params, [Param::Value(t)], "{key:?}");
                        assert_eq!(sig.results, [t], "{key:?}");
                        assert_eq!(sig.dest, None, "{key:?}");
                    }
                    hirsel_core_ir::Carried::Nothing => {
                        assert!(
                            sig.params.is_empty() && sig.results.is_empty() && sig.dest.is_none(),
                            "{key:?}"
                        );
                    }
                    hirsel_core_ir::Carried::Address => {
                        assert!(matches!(sig.params.as_slice(), [Param::Address { .. }]))
                    }
                }
            }
            _ => {}
        }
    }
}

/// Every source type carried as a provider/identity word has the same payload
/// width as its direct scalar ABI, including arguments and returned values.
#[test]
fn callable_provider_and_identity_words() {
    let names = [
        "Args", "Dir", "Net", "Clock", "Random", "Stdout", "Stderr", "Stdin", "Env", "Process",
        "Tasks", "Unique", "Site",
    ];
    let source = names.iter().enumerate().map(|(i, name)| format!(
        "fn id{i}(take x:{name})->{name}{{x}}\nfn f{i}(x:{name})->{name}{{let f:fn(take {name})->{name}=id{i};f(x)}}\n"
    )).collect::<String>();
    let (index, inputs) = inputs_with_core(&source, &root().join("../core"));
    let p = Program::declare(&index, &inputs);
    assert!(
        p.refusals.is_empty() && p.gaps.is_empty(),
        "{:?} {:?}",
        p.refusals,
        p.gaps
    );
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &inputs, |d| d.package_is("app", "1"));
    let targets = functions
        .iter()
        .filter(|(name, _)| name.starts_with('f'))
        .map(|(_, f)| *f)
        .collect::<Vec<_>>();
    wf::module(&module).unwrap();
    for target in targets {
        let report = eval::run(&module, target, &[0x1234_5678_90ab_cdef], 0);
        assert_eq!(
            report.outcome,
            eval::Outcome::Returned(vec![0x1234_5678_90ab_cdef])
        );
        assert_eq!(report.used, 0);
    }
}

#[test]
fn sibling_main_wrapper_executes_the_sibling_body() {
    let app = package("files");
    let manifest = PackageManifest {
        package: app,
        dependencies: vec![],
        sources: ["main", "matched"]
            .into_iter()
            .map(|name| ModuleSource {
                file: name.into(),
                modules: vec![name.into()],
                signatures: false,
                foreign: false,
            })
            .collect(),
    };
    let mut symbols = Symbols::default();
    manifest.intern(&mut symbols);
    let syntax = [
        "pub fn main() -> u64 { 42 }",
        "pub fn main() -> u64 { main.main() }",
    ]
    .into_iter()
    .map(|source| {
        let p = parse(source, ParseMode::Source, &mut symbols);
        assert!(p.diagnostics.is_empty());
        p.syntax
    })
    .collect::<Vec<_>>();
    let mut index = NameIndex::new(std::sync::Arc::new(symbols));
    let ids = index.add_manifests(&[manifest]);
    let modules = ids
        .into_iter()
        .zip(syntax)
        .map(|(id, syntax)| ModuleInput {
            id,
            source: index.add_source(),
            syntax,
        })
        .collect::<Vec<_>>();
    for m in &modules {
        for (i, item) in m.syntax.items().enumerate() {
            index.index_item(m.id, m.source, i as u32, item);
        }
    }
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    assert!(p.gaps.is_empty(), "{:?}", p.gaps);
    let (module, _functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("files", "1"));
    let wrapper = Some(hirsel_core_ir::FuncId(
        module
            .functions
            .iter()
            .position(|f| module.instances[f.instance.index()].definition.0.modules == ["matched"])
            .unwrap() as u32,
    ));
    assert_eq!(
        eval::run(&module, wrapper.unwrap(), &[], 1_000).outcome,
        eval::Outcome::Returned(vec![42])
    );
}

/// FORM #64: a published row says how every signature carries its values, so a generic
/// instance's signature can read it instead of the row's fields. The bug it catches:
/// `u64`, a one-field record and a one-element tuple all publish `{0: u64}`-like
/// bodies, and a classifier reading the body passes the record or tuple by value.
/// Expected: the closed rule as core.md states it (scalars, plain `fn`, capability and
/// identity words by value; unit nothing; declared and structural types by address),
/// and each row agrees with the closed signature `s` takes the same type with.
#[test]
fn rows_publish_the_closed_signature_carriage() {
    use hirsel_core_ir::{Carried, Scalar};
    let (index, modules) = program_inputs(
        "type W { x: u64 }\n\
         fn id[T](take x: T) -> T { x }\n\
         fn s(a: u64, w: W, t: (u64,), c: char, f: f64, b: bool, u: Unique) -> u64 { a }\n\
         fn g(a: u64, w: W, t: (u64,), u: (), c: char, f: f64, b: bool, q: Unique) -> u64 {\n\
           let _ = id(w); let _ = id(t); let _ = id(u); let _ = id(c); let _ = id(f);\n\
           let _ = id(b); let _ = id(q); id(a)\n\
         }",
    );
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));
    let s = Some(functions["s"]);
    let module = &*module;
    wf::module(module).unwrap();
    let row = |name: &str| {
        module
            .types
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| {
                panic!(
                    "no row {name}: {:?}",
                    module.types.iter().map(|t| &t.name).collect::<Vec<_>>()
                )
            })
            .carried
    };
    let value = |s| Carried::Value(Ty::Scalar(s));
    let expected = [
        ("u64", value(Scalar::U64)),
        ("app@1/main/W", Carried::Address),
        ("(u64)", Carried::Address),
        ("char", value(Scalar::Char)),
        ("f64", value(Scalar::F64)),
        ("bool", value(Scalar::Bool)),
        ("core@1/core/Unique", value(Scalar::U64)),
        ("()", Carried::Nothing),
    ];
    for (name, carried) in expected {
        assert_eq!(row(name), carried, "{name}");
    }
    let sig = &module.signatures[module.functions[s.unwrap().index()].signature.index()];
    let from_rows: Vec<Param> = expected
        .iter()
        .filter_map(|&(name, carried)| match carried {
            Carried::Value(t) => Some(Param::Value(t)),
            Carried::Address => Some(Param::Address {
                place: hirsel_core_ir::PlaceTy::Declared(hirsel_core_ir::TypeId(
                    module.types.iter().position(|t| t.name == name).unwrap() as u32,
                )),
                loan: hirsel_core_ir::Loan::View,
            }),
            Carried::Nothing => None,
        })
        .collect();
    assert_eq!(sig.params, from_rows);
}

/// Fieldless word enums need no carrier slot when an indirect generic comparator
/// returns one. The three ordinals are checked independently of its storage.
#[test]
fn generic_comparator_keeps_fieldless_order_in_values() {
    use hirsel_core_ir::PlaceTy;
    let source = r#"
fn compare[T: plain](x: T, before: scoped fn(T) -> Order) -> u64 {
    match before(x) { Less => 10u64, Equal => 20u64, Greater => 30u64 }
}
fn run() -> u64 {
    let before: scoped fn(u64) -> Order = fn(x: u64) -> Order {
        if x == 1 { Less } else if x == 2 { Equal } else { Greater }
    }
    compare(1u64, before) + compare(2u64, before) + compare(3u64, before)
}
"#;
    let (index, modules) = program_inputs(source);
    let program = Program::declare(&index, &modules);
    assert!(program.refusals.is_empty() && program.gaps.is_empty());
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));
    let root = Some(functions["run"]);

    wf::module(&module).unwrap();
    assert_eq!(
        eval::run(&module, root.unwrap(), &[], 100_000).outcome,
        eval::Outcome::Returned(vec![60])
    );
    let instance = module
        .functions
        .iter()
        .find(|f| {
            let key = &module.instances[f.instance.index()];
            key.definition.0.name == "compare" && module.argument_names(key) == ["u64"]
        })
        .unwrap();
    assert!(
        !instance
            .body
            .as_ref()
            .unwrap()
            .slots
            .iter()
            .any(|ty| matches!(ty,
                PlaceTy::Declared(id) if module.types[id.index()].name == "core@1/core/Order"
            )),
        "a word-carried comparator result stays in SSA"
    );
}

#[test]
fn terminal_prefix_is_derived_after_generic_substitution() {
    use hirsel_core_ir::Inst;
    let source = concat!(
        include_str!("../../../core/std/tests/terminal_blocks.hirsel"),
        "\nfn run()->u64 {leaf_box()+child_box()+leaf_growth()+child_growth()+leaf_counted()}\n"
    );
    let (index, modules) = program_inputs(source);
    let program = Program::declare(&index, &modules);
    assert!(program.refusals.is_empty() && program.gaps.is_empty());
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));
    let root = Some(functions["run"]);

    wf::module(&module).unwrap();
    let report = eval::run(&module, root.unwrap(), &[], 100_000);
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![45]));
    assert_eq!(report.used, 0);
    let recipes = hirsel_core_layout::compute(&module.types).unwrap();
    let mut checked = 0;
    for function in &module.functions {
        let key = &module.instances[function.instance.index()];
        if !matches!(key.definition.0.name.as_str(), "wrap" | "buffered")
            || module.argument_names(key).is_empty()
        {
            continue;
        }
        let plain = module.argument_names(key) == ["u64"];
        let body = function.body.as_ref().unwrap();
        for inst in &body.insts {
            if let Inst::Alloc { kind, overflow, .. } = inst {
                // A synthetic count-overflow admission cannot return storage.
                if overflow.is_some_and(|v| {
                    body.insts
                        .iter()
                        .any(|i| matches!(i, Inst::Const { result, bits: 1 } if *result == v))
                }) {
                    continue;
                }
                let class = kind.cleanup(&module.types, &recipes);
                assert_eq!(
                    class == Some(hirsel_core_layout::Cleanup::Terminal),
                    plain,
                    "{key:?}: {kind:?} is {class:?}"
                );
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 6,
        "both generic boxes and both growth branches were instantiated"
    );
}

/// The carrier is an independent hand-written FORM producer, so these cases can
/// exercise populated Lists before the allocation/growth core calls are emitted.
/// Three elements 7,11,13 have sum 31; both success and failure must refund 24 bytes.
#[test]
fn list_index_iteration_and_failure_cleanup() {
    use hirsel_core_ir::{TypeId, text};
    let vectors = [
        (
            "type Choice = Small(u8) | Large(u64)\nfn f(xs: FixedSpan[Choice]) -> u64 { let mut s:u64=0; for c in xs { s+=match c { Small(v)=>{let n:u64=int.widen(v); n}, Large(v)=>v*100u64 } }; s }",
            false,
            eval::Outcome::Returned(vec![1120]),
        ),
        (
            "fn helper(xs: SeqSpan[(u8,u64)]) -> Option[View[(u8,u64)]] in xs { seq.nth(xs,1) }\nfn f(xs: SeqSpan[(u8,u64)]) -> u64 { match helper(xs) { Some(pair) => { let (_,v)=pair; v }, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn helper(xs: IndexedSpan[(u8,u64)]) -> Option[View[(u8,u64)]] in xs { indexed.get(xs,2) }\nfn f(xs: IndexedSpan[(u8,u64)]) -> u64 { match helper(xs) { Some(pair) => { let (_,v)=pair; v }, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![13]),
        ),
        (
            "fn f(w32: IndexedSpan[u64]) -> u64 { match indexed.get(w32,2) { Some(v)=>v+0u64, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![13]),
        ),
        (
            "fn f(w64: IndexedSpan[u64]) -> u64 { match indexed.get(w64,3) { Some(v)=>v+0u64, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![0]),
        ),
        (
            "fn f(xs: FixedSpan[Inline[(u8,u64),2]]) -> u64 { let mut s:u64=0; for row in xs { for (_, y) in row { s += y } }; s }",
            false,
            eval::Outcome::Returned(vec![341]),
        ),
        (
            "fn f(xs: FixedSpan[Inline[(u8,u64),2]]) -> u64 { let (_,y) = xs[1][1]; y }",
            false,
            eval::Outcome::Returned(vec![110]),
        ),
        (
            "fn f(xs: FixedSpan[Site]) -> u64 { if xs[0] == xs[2] { 1 } else { 0 } }",
            false,
            eval::Outcome::Returned(vec![0]),
        ),
        (
            "type Entry { tiny: u8, num: u64 }\nfn f(xs: FixedSpan[Entry]) -> u64 { xs[1].num }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(xs: FixedSpan[()]) -> u64 { xs[3]; 0 }",
            false,
            eval::Outcome::Failed(vec!["Bounds".into()]),
        ),
        (
            "fn f(xs: FixedSpan[()]) -> u64 { let mut s: u64 = 0; for _ in xs { s += 1 }; s }",
            false,
            eval::Outcome::Returned(vec![3]),
        ),
        (
            "fn f(xs: SeqSpan[u64]) -> u64 { let mut s: u64 = 0; for x in xs { s += x }; s }",
            false,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(xs: IndexedSpan[u64]) -> u64 { let mut s: u64 = 0; for x in xs { s += x }; s }",
            false,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(xs: SeqSpan[u64]) -> u64 { seq.len(xs) }",
            false,
            eval::Outcome::Returned(vec![3]),
        ),
        (
            "fn f(xs: SeqSpan[u64]) -> u64 { match seq.nth(xs, 1) { Some(x) => x + 0u64, None => 0 } }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(xs: SeqSpan[u64]) -> u64 { match seq.nth(xs, 3) { Some(x) => x + 0u64, None => 0 } }",
            false,
            eval::Outcome::Returned(vec![0]),
        ),
        (
            "fn f(xs: IndexedSpan[u64]) -> u64 { match indexed.get(xs, 2) { Some(x) => x + 0u64, None => 0 } }",
            false,
            eval::Outcome::Returned(vec![13]),
        ),
        (
            "fn f(w32: FixedSpan[u64]) -> u64 { w32[2] }",
            false,
            eval::Outcome::Returned(vec![13]),
        ),
        (
            "fn f(w64: FixedSpan[u64]) -> u64 { w64[2] }",
            false,
            eval::Outcome::Returned(vec![13]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { span.len(xs) }",
            false,
            eval::Outcome::Returned(vec![3]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { match span.get(xs, 1) { Some(x) => x, None => 0 } }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { match span.get(xs, 3) { Some(x) => x, None => 0 } }",
            false,
            eval::Outcome::Returned(vec![0]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { xs[1] }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { xs[3] }",
            false,
            eval::Outcome::Failed(vec!["Bounds".into()]),
        ),
        (
            "fn f(xs: FixedSpan[u64]) -> u64 { let mut s: u64 = 0; for x in xs { s += x }; s }",
            false,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(xs: FixedSpan[(u8,u64)]) -> u64 { let mut s: u64 = 0; for (x,y) in xs { s += y }; s }",
            false,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(xs: FixedSpan[(u8,u64)]) -> u64 { let (x,y) = xs[1]; y }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(mut xs:Map[u64,u64])->u64 { map.insert(mut xs,7,99); match map.get(xs,7) {Some(v)=>map.len(xs)*1000+v,None=>0} }",
            false,
            eval::Outcome::Returned(vec![2099]),
        ),
        (
            "fn f(mut xs:Map[u64,u64])->u64 { let old=option.unwrap(map.remove(mut xs,7)); old*100+map.len(xs) }",
            false,
            eval::Outcome::Returned(vec![7001]),
        ),
        (
            "fn f(mut xs:Map[u64,u64])->u64 { let _=map.remove(mut xs,11); map.len(xs) }",
            false,
            eval::Outcome::Returned(vec![2]),
        ),
        (
            "fn f(mut xs:Map[u64,u64])->u64 { let _=map.remove(mut xs,13); match map.get(xs,13) {Some(_)=>99,None=>map.len(xs)} }",
            false,
            eval::Outcome::Returned(vec![1]),
        ),
        (
            "fn f(xs:Map[u64,u64])->u64 { match map.get(xs,7) { Some(v)=>v, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![70]),
        ),
        (
            "fn f(xs:Map[u64,u64])->u64 { match map.get(xs,13) { Some(v)=>v, None=>0 } }",
            false,
            eval::Outcome::Returned(vec![130]),
        ),
        (
            "fn f(xs:Map[u64,u64])->u64 { match map.get(xs,11) { Some(v)=>v, None=>99 } }",
            false,
            eval::Outcome::Returned(vec![99]),
        ),
        (
            "fn f(xs:Map[u64,u64])->u64 { match map.get(xs,2) { Some(v)=>v, None=>99 } }",
            false,
            eval::Outcome::Returned(vec![99]),
        ),
        (
            "fn lookup[K:key,V](m:Map[K,V],k:K)->Option[View[V]] in m {map.get(m,k)}\nfn f(xs:Map[u64,u64])->u64 {match lookup(xs,7) {Some(v)=>v,None=>0}}",
            false,
            eval::Outcome::Returned(vec![70]),
        ),
        (
            "fn f(xs: Map[u64,u64]) -> u64 { map.len(xs) }",
            false,
            eval::Outcome::Returned(vec![2]),
        ),
        (
            "fn count[K:key,V](m:Map[K,V])->u64 { map.len(m) }\nfn f(xs:Map[u64,u64])->u64 { count(xs) }",
            false,
            eval::Outcome::Returned(vec![2]),
        ),
        (
            "fn count[K:key,V](m:Map[K,V])->u64 { let mut n:u64=0; for (k,v) in m { n+=1 }; n }\nfn f(xs:Map[u64,u64])->u64 { count(xs) }",
            false,
            eval::Outcome::Returned(vec![2]),
        ),
        (
            "fn f(xs: Map[u64,u64]) -> u64 { let mut s: u64 = 0; for (k,v) in xs { s += k + v }; s }",
            false,
            eval::Outcome::Returned(vec![220]),
        ),
        (
            "fn f(xs: Map[u64,u64]) -> u64 { let mut s: u64 = 0; for (k,v) in xs { s = s * 100 + k }; s }",
            false,
            eval::Outcome::Returned(vec![713]),
        ),
        (
            "fn add(x: u64) -> u64 { x + 1 }\nfn f(xs: Map[u64,u64]) -> u64 { let mut s: u64 = 0; for (k,v) in xs { s += add(v) }; s }",
            false,
            eval::Outcome::Returned(vec![202]),
        ),
        (
            "fn f(xs: List[u64]) -> u64 { let v = view xs; v[1] }",
            false,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(xs: List[u64]) -> u64 { xs[3] }",
            false,
            eval::Outcome::Failed(vec!["Bounds".into()]),
        ),
        (
            "fn f(mut xs: List[u64]) -> u64 { xs[1] += 5; xs[1] }",
            false,
            eval::Outcome::Returned(vec![16]),
        ),
        (
            "fn f(xs: List[u64]) -> u64 { let mut s: u64 = 0; for x in xs { s += x }; s }",
            false,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(xs: List[u64]) -> u64 { let mut s: u64 = 0; for x in xs { if x == 11 { continue }; s += x }; s }",
            false,
            eval::Outcome::Returned(vec![20]),
        ),
        (
            "fn f(xs: List[u64]) -> u64 { let mut s: u64 = 0; for x in xs { s += x; break }; s }",
            false,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let mut s: u64 = 0; for take x in xs { s += x }; s }",
            true,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let mut s: u64 = 0; for take x in xs { s += x; break }; s }",
            true,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let mut s: u64 = 0; for take x in xs { if x == 11 { continue }; s += x }; s }",
            true,
            eval::Outcome::Returned(vec![20]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { for take x in xs { return x }; 0 }",
            true,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "fn f(take xs: List[List[u64]]) -> u64 { let mut s: u64 = 0; for take row in xs { s += row[0]; break }; s }",
            true,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "fn f(take xs: List[List[u64]]) -> u64 { let mut s: u64 = 0; for take row in xs { s += row[0] }; s }",
            true,
            eval::Outcome::Returned(vec![31]),
        ),
        (
            "fn f(take xs: List[List[u64]]) -> u64 { for take row in xs { return row[0] }; 0 }",
            true,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "fn f(take xs: List[List[u64]]) -> u64 { for take row in xs { let n: u8 = int.narrow(256u64) }; 0 }",
            true,
            eval::Outcome::Failed(vec!["Overflow".into()]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let a = [take xs]; let mut s: u64 = 0; for row in a { s += row[1] }; s }",
            true,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let a = [take xs]; let row = a[0]; row[1] }",
            true,
            eval::Outcome::Returned(vec![11]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let n: u8 = int.narrow(256u64); 0 }",
            true,
            eval::Outcome::Failed(vec!["Overflow".into()]),
        ),
        (
            "fn f(take xs: List[u64]) -> u64 { let a = [take xs, { let z = 1u64 / 0u64; loop {} }]; 0 }",
            true,
            eval::Outcome::Failed(vec!["Division".into()]),
        ),
    ];
    for (source, takes, expected) in vectors {
        let (index, inputs) = program_inputs(source);
        let p = Program::declare(&index, &inputs);
        assert!(p.refusals.is_empty(), "{source}: {:?}", p.refusals);
        let _f = p.fns.iter().position(|f| f.path.0.name == "f").unwrap();
        let (module, functions) =
            crate::SourceProgram::from_inputs(&index, &inputs, |d| d.package_is("app", "1"));
        let target = functions["f"];
        if source == "fn f(xs:Map[u64,u64])->u64 { match map.get(xs,7) { Some(v)=>v, None=>0 } }" {
            for function in &module.functions {
                let key = &module.instances[function.instance.index()];
                if key.definition.0.modules == ["map"]
                    && let Some(body) = &function.body
                {
                    eprintln!(
                        "Map instance {} {:?}: {} instructions, {} terminators",
                        key.definition,
                        module.argument_names(key),
                        body.insts.len(),
                        body.terminators.len()
                    );
                }
            }
        }
        let module = &*module;
        let signature = module.functions[target.index()].signature;
        let Param::Address {
            place: hirsel_core_ir::PlaceTy::Declared(TypeId(list)),
            ..
        } = module.signatures[signature.index()].params[0]
        else {
            panic!("List by address")
        };
        // The owning edge of the fixture's buffer: the row's storage field as the
        // printed form spells it (`seq u64`, `map u64 u64`, `seq t5`); a span's blob
        // is bytes, freed explicitly.
        let printed = text::print(module);
        let row = printed
            .lines()
            .find(|l| l.starts_with(&format!("type t{list} =")))
            .unwrap();
        let edge = row[row.find('{').unwrap() + 1..]
            .split([',', '}'])
            .filter_map(|f| f.split_once(':').map(|(_, ty)| ty.trim()))
            .find(|ty| ty.starts_with("seq ") || ty.starts_with("map "))
            .unwrap_or("seq u8")
            .to_owned();
        let (g, instance, entry) = (
            module.signatures.len(),
            module.instances.len(),
            module.functions.len(),
        );
        let drop = if takes {
            String::new()
        } else {
            format!("drop a0 t{list}\n")
        };
        // The entry alone: the vectors below edit its text, never the core bodies.
        let fixture = format!(
            r#"
sig g{g} = () -> (u64)
inst n{instance} = app@1/fixture/entry [] []
fn f{entry} = n{instance} g{g} {{
 slot s0 = t{list}
 addr a0 = s0 : t{list}
 addr a1 = a0 byte v1 : ptr
 addr a2 = a0 byte v2 : u64
 addr a3 = a0 byte v3 : u64
 addr a4 = v4 : u64
 addr a5 = v4 byte v2 : u64
 addr a6 = v4 byte v3 : u64
 b0:
  live s0
  v0: u64 = const 24
  v1: u64 = const 0
  v2: u64 = const 8
  v3: u64 = const 16
  v4: ptr, v5: u64 = alloc for {edge} v0 align 8 charge v0
  v6: u64 = const 7
  v7: u64 = const 11
  v8: u64 = const 13
  v9: u64 = const 3
  address a0
  address a1
  address a2
  address a3
  address a4
  address a5
  address a6
  store a1, v4
  store a2, v9
  store a3, v9
  store a4, v6
  store a5, v7
  store a6, v8
  v10: u64 = call f{} g{} (&a0) fail b1
  {drop}
  return v10
 b1:
  {drop}
  propagate
}}
"#,
            target.0, signature.0
        );
        let fixture = if source.contains("FixedSpan[")
            || source.contains("SeqSpan[")
            || source.contains("IndexedSpan[")
        {
            // Independent W16 blob: header, count=3, then tightly packed elements.
            let sequence = !source.contains("FixedSpan[");
            let tuple = source.contains("Span[(u8,u64)]") || source.contains("FixedSpan[Entry]");
            let width = if source.contains("w32") {
                4
            } else if source.contains("w64") {
                8
            } else {
                2
            };
            let mut addresses = String::new();
            let mut build = String::new();
            let mut fields = vec![
                (8, width, "u64"),
                (
                    16,
                    3,
                    if width == 2 {
                        "u16"
                    } else if width == 4 {
                        "u32"
                    } else {
                        "u64"
                    },
                ),
            ];
            if sequence {
                fields.push((
                    16 + width as usize,
                    if tuple { 27 } else { 24 },
                    if width == 2 {
                        "u16"
                    } else if width == 4 {
                        "u32"
                    } else {
                        "u64"
                    },
                ));
            }
            if source.contains("IndexedSpan[") {
                let stride = if tuple { 9 } else { 8 };
                let table = 16 + 2 * width as usize + 3 * stride;
                let word = if width == 2 {
                    "u16"
                } else if width == 4 {
                    "u32"
                } else {
                    "u64"
                };
                for i in 0..3usize {
                    fields.push((table + i * width as usize, (i * stride) as u64, word));
                }
            }
            let variant = source.contains("FixedSpan[Choice]");
            let array = source.contains("FixedSpan[Inline[");
            for (i, bits) in [7, 11, 13].into_iter().enumerate() {
                let first = 16 + width as usize * if sequence { 2 } else { 1 };
                if variant {
                    let offset = first + i * 16;
                    fields.push((offset, u64::from(i == 1), "u64"));
                    fields.push((offset + 8, bits, if i == 1 { "u64" } else { "u8" }));
                } else if array {
                    for (j, bits) in [bits, bits * 10].into_iter().enumerate() {
                        let offset = first + i * 18 + j * 9;
                        fields.push((offset, 1, "u8"));
                        fields.push((offset + 1, bits, "u64"));
                    }
                } else {
                    let offset = first + i * if tuple { 9 } else { 8 };
                    if tuple {
                        fields.push((offset, 1, "u8"));
                    }
                    fields.push((offset + usize::from(tuple), bits, "u64"));
                }
            }
            for (i, (offset, bits, ty)) in fields.into_iter().enumerate() {
                let a = 7 + i;
                let v = 11 + 2 * i;
                addresses.push_str(&format!("addr a{a} = v4 byte v{v} : {ty}\n"));
                build.push_str(&format!("v{v}: u64 = const {offset}\nv{}: {ty} = const {bits}\naddress a{a}\nstore a{a}, v{}\n",v+1,v+1));
            }
            fixture
                .replace("v0: u64 = const 24", "v0: u64 = const 128")
                .replace(" b0:", &format!("{addresses} b0:"))
                .replace("store a3, v9", "")
                .replace("store a2, v9", "store a2, v3")
                .replace("store a4, v6\n  store a5, v7\n  store a6, v8", &build)
                .replace(&format!("drop a0 t{list}"), "free v4 charge v0")
        } else if source.contains("Map[u64,u64]") {
            let mut addresses = String::new();
            let mut build = String::new();
            for (i, (offset, bits)) in [
                (0, 0x49ec259204a234c2u64),
                (8, 1),
                (16, 7),
                (24, 70),
                (32, 0xe1e0cd537ed145b8),
                (40, 0),
                (48, 11),
                (56, 110),
                (64, 0xaddb2134e9f8658d),
                (72, 1),
                (80, 13),
                (88, 130),
                (96, 2),  // live count differs from the three occupied slots
                (104, 8), // cached index length, independent of capacity=3
                (112, 0xe1e0cd537ed145ba),
                (120, 0),
                (128, 0x49ec259204a234c1),
                (136, 0),
                (144, 0),
                (152, 0xaddb2134e9f8658b),
                (160, 0),
                (168, 0),
            ]
            .into_iter()
            .enumerate()
            {
                let a = 7 + i;
                let v = 12 + 2 * i;
                addresses.push_str(&format!("addr a{a} = v4 byte v{v} : u64\n"));
                build.push_str(&format!("v{v}: u64 = const {offset}\nv{}: u64 = const {bits}\naddress a{a}\nstore a{a}, v{}\n",v+1,v+1));
            }
            fixture
                .replace(
                    "v0: u64 = const 24",
                    "v0: u64 = const 96\n  v11: u64 = const 176",
                )
                .replace(
                    &format!("alloc for {edge} v0 align 8 charge v0"),
                    &format!("alloc for {edge} v11 align 8 charge v0"),
                )
                .replace(" b0:", &format!("{addresses} b0:"))
                .replace("store a4, v6\n  store a5, v7\n  store a6, v8", &build)
                .replace(&format!("drop a0 t{list}"), "free v4 charge v0")
        } else if source.contains("List[List[u64]]") {
            let mut addresses = String::new();
            let mut build = String::new();
            for i in 0..3 {
                let a = 7 + 4 * i;
                let v = 11 + 7 * i;
                let offset = i * 24;
                addresses.push_str(&format!("addr a{a} = v4 byte v{v} : ptr\naddr a{} = v4 byte v{} : u64\naddr a{} = v4 byte v{} : u64\naddr a{} = v{} : u64\n",a+1,v+1,a+2,v+2,a+3,v+3));
                build.push_str(&format!("v{v}: u64 = const {offset}\nv{}: u64 = const {}\nv{}: u64 = const {}\nv{}: ptr, v{}: u64 = alloc for seq u64 v2 align 8 charge v2\nv{}: u64 = const 1\nv{}: u64 = const {}\naddress a{a}\naddress a{}\naddress a{}\naddress a{}\nstore a{a}, v{}\nstore a{}, v{}\nstore a{}, v{}\nstore a{}, v{}\n",v+1,offset+8,v+2,offset+16,v+3,v+4,v+5,v+6,[7,11,13][i],a+1,a+2,a+3,v+3,a+1,v+5,a+2,v+5,a+3,v+6));
            }
            fixture
                .replace("v0: u64 = const 24", "v0: u64 = const 72")
                .replace(" b0:", &format!("{addresses} b0:"))
                .replace("store a4, v6\n  store a5, v7\n  store a6, v8", &build)
        } else {
            fixture
        };
        let fixture = text::parse(&format!("{}{fixture}", text::print(module))).unwrap();
        wf::module(&fixture).unwrap();
        let report = eval::run(&fixture, FuncId(entry as u32), &[], 1_000_000);
        assert_eq!(report.outcome, expected, "{source}");
        assert_eq!(report.used, 0, "{source}: list storage leaked");
    }
}
