// Code addresses and revisions (PROG-9, PROG-10),
// on the code-revision-review-1040 corpus. Laws:
// - L1 sensitivity: a pair whose exits differ has different revisions
//   (s01, s02, s03, s04, s51; d1 an edge out of an instance that carries a
//   dictionary). Oracle: the executables' exits, run here.
// - L2 invariance: an edit the language defines as non-semantic leaves the
//   revision equal (c01-c06: local name, parameter name, unreached item,
//   comment, item order, grouping; l1 lines above, l2 re-indent). Oracle: the language
//   rules, and equal exits.
// - L4 reach: an edit to an item main does not reach leaves it equal (c03,
//   r1); an edit inside the reach changes it (s03, s04).
// - L6 configuration: a root limit alone leaves it equal (s45).
// - Engines: Cranelift, LLVM dev and LLVM release give one revision for every
//   corpus program. Oracle: the definition has no engine input.
// - Determinism: a primed store (the other side built first, then a repeat)
//   gives a fresh store's revision, whatever was materialised or numbered.
// - Described (PROG-9 `code.revision`): a program prints the revisions of
//   the functions it describes. An edit to f changes f's and leaves g's
//   equal; a primed store prints a fresh store's bytes; Cranelift, LLVM dev
//   and LLVM release print the same bytes (L3); a build over an imaged core
//   prints a source core's (L5). Oracle: the definition (f's edit is inside
//   f's reach and outside g's; no engine or image input).

/// How a side's std is given: none, the tree's, or the tree's with one edit.
#[derive(Clone, Copy)]
enum Std {
    None,
    Tree,
    Edit(&'static str, &'static str, &'static str),
}
#[derive(Clone, Copy, PartialEq, Debug)]
enum Revision {
    Differ,
    Equal,
}
struct RevisionCase {
    name: &'static str,
    a: &'static str,
    b: &'static str,
    std: (Std, Std),
    expect: Revision,
}
const WRAP_I64: &str = "pub fn main() -> Result[(), Exit] { if entry() == -1 { Ok(()) } else { Error(Exit(status: 1)) } }\n";
const WRAP_7: &str = "pub fn main() -> Result[(), Exit] { if entry() == 7 { Ok(()) } else { Error(Exit(status: 1)) } }\n";
const WRAP_8: &str = "pub fn main() -> Result[(), Exit] { if entry() == 8 { Ok(()) } else { Error(Exit(status: 1)) } }\n";
const WRAP_1002: &str = "pub fn main() -> Result[(), Exit] { if entry(1) == 1002 { Ok(()) } else { Error(Exit(status: 1)) } }\n";
const LINES: &str = "fn entry(x: u64) -> u64 {\n    let xs = [1u64, 2u64, 3u64];\n    xs[x] + x * 1000u64\n}\n";
const S45: &str = "fn entry() -> u64 { let mut xs: List[u64] = list.new(); match list.try_reserve(mut xs, 1) { Ok(()) => 7, Error(_) => 8 } }\n";

const GENERATED_CLOSURE: &str =
    "fn entry() -> u64 { let x = 7u64; let f = fn(n: u64) -> u64 { n }; f(x) }\n";

#[test]
fn generated_names_and_revisions_ignore_parentheses() {
    let observe = |source: &str| {
        let dir = tempfile::tempdir().unwrap();
        let product = record_check(
            &format!("{source}{WRAP_7}"),
            &core_sources(),
            dir.path(),
            "store",
        );
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let reached = product.reachable(&entry, 256).unwrap();
        let identity = Identity::plain(DefPath::from_full("app@1/main/entry").unwrap());
        let module = reached.materialized.get(&identity).ir().unwrap();
        (closure_names(module), reached.address(&entry).unwrap())
    };
    let a = observe(GENERATED_CLOSURE);
    let b = observe(&GENERATED_CLOSURE.replace("7u64", "(7u64)"));
    assert_eq!(a.0, b.0, "grouping leaves the generated name equal");
    assert_eq!(a.1, b.1, "grouping leaves the code address equal");
    assert_eq!(a.0, ["entry$closure0"]);

    let earlier = GENERATED_CLOSURE.replace("let f =", "let first = fn() -> u64 { 3 }; let f =");
    let earlier = earlier.replace("f(x)", "first() + f(x)");
    let control = observe(&earlier);
    assert_eq!(control.0, ["entry$closure0", "entry$closure1"]);
    assert_ne!(control.1, a.1, "the earlier closure adds executable code");
}

fn closure_names(module: &Module) -> Vec<String> {
    module
        .functions
        .iter()
        .filter(|f| f.body.is_some())
        .map(|f| &module.instances[f.instance.index()].definition.0.name)
        .filter(|name| name.contains("$closure") && !name.contains("$callable"))
        .cloned()
        .collect()
}

#[test]
fn generated_names_survive_lowering_again_and_specialization() {
    const APP: &str = "fn pass[T: plain](x: T) -> T { let f = fn(y: T) -> T { copy y }; f(x) }\n\
        fn entry() -> u64 { if pass(true) { pass(7u64) } else { 0u64 } }\n";
    let dir = tempfile::tempdir().unwrap();
    let app = format!("{APP}{WRAP_7}");
    let product = record_check(&app, &core_sources(), dir.path(), "store");
    let main = DefPath::from_full("app@1/main/main").unwrap();
    let reached = product.reachable(&main, 256).unwrap();
    let pass = DefPath::from_full("app@1/main/pass").unwrap();
    let instances: Vec<_> = reached
        .require()
        .unwrap()
        .iter()
        .filter(|i| i.definition == pass)
        .collect();
    assert_eq!(
        instances.len(),
        2,
        "main reaches both pass[bool] and pass[u64]"
    );
    assert_ne!(instances[0].closures, instances[1].closures);
    for instance in instances {
        let module = reached.materialized.get(instance).ir().unwrap();
        assert_eq!(closure_names(module), ["pass$closure0"]);
    }

    // Lower the same definition twice into one emitter, as opposed to reusing
    // its checked body from the driver's store.
    let request = crate::tests::request(APP, dir.path());
    let mut symbols = hirsel_core_syntax::Symbols::default();
    let parsed = request.inputs.packages[0]
        .read(&mut symbols)
        .unwrap()
        .pop()
        .unwrap();
    assert!(parsed.parsed.diagnostics.is_empty());
    let mut index = hirsel_core_resolve::NameIndex::new(Arc::new(symbols));
    index.add_manifests(&request.inputs.packages);
    let input = hirsel_core_front::ModuleInput {
        id: index.module(&parsed.module).unwrap(),
        source: index.add_source(),
        syntax: parsed.parsed.syntax,
    };
    for (i, item) in input.syntax.items().enumerate() {
        index.index_item(input.id, input.source, i as u32, item);
    }
    let inputs = [input];
    let program = hirsel_core_front::Program::declare(&index, &inputs);
    let mut emitter = hirsel_core_front::Emitter::new();
    let f = program.function_id(&pass).unwrap();
    let first = program.check_lower(f, &mut emitter);
    let second = program.check_lower(f, &mut emitter);
    for check in [&first, &second] {
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{check:?}"
        );
        assert!(check.function.is_some());
    }
    assert_eq!(first.function, second.function);
    assert_eq!(closure_names(&emitter.module), ["pass$closure0"]);
}

const OWN_LAYOUT: &str = r#"
type Key: key + ordered { a: u8, b: u64, c: u16 }
const FIRST: Key = Key(a: 5, b: 0x0102030405060708, c: 0x090a)

pub fn main() -> Result[(), Exit] {
    let mut first = FIRST
    first.a = 5
    let next = Key(a: 6, b: 0, c: 0)
    if first < next && first.b == 0x0102030405060708 && first.c == 0x090a {
        Ok(())
    } else {
        Error(Exit(status: 1))
    }
}
"#;

#[test]
fn own_layout_is_resolved_after_the_body_hash() {
    use hirsel_core_ir::{Inst, LayoutQuery, Static};
    use hirsel_core_layout::Layout;

    let dir = tempfile::tempdir().unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let product = Driver::default()
        .check(revision_request(OWN_LAYOUT, None, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let reached = product.reachable(&entry, 65_536).unwrap();
    assert!(reached.accepted());
    let instance = reached.instances.as_ref().unwrap().iter()
        .find(|i| i.definition == entry).unwrap();
    let instance = reached.materialized.get(instance);
    let module = instance.ir().unwrap();
    let body_hash = || hirsel_core_ir::normal::encode(module, instance.function).hash();
    let saved = body_hash();
    let mut layouts = hirsel_core_layout::compute(&module.types).unwrap();
    let (ty, data) = module.statics.iter().find_map(|s| match s {
        Static::Aggregate { ty, .. } => Some((*ty, s)),
        _ => None,
    }).expect("checked aggregate");
    let body = module.functions.iter().find_map(|f| f.body.as_ref()).unwrap();
    let size = body.insts.iter().find_map(|i| match i {
        Inst::LayoutConst { result, query: LayoutQuery::HeldSize(row) } if *row == ty => Some(*result),
        _ => None,
    }).expect("checked copy uses its type's size");
    for (size_bytes, offsets, expected) in [
        (24, vec![0, 8, 16], vec![5, 0, 0, 0, 0, 0, 0, 0, 8, 7, 6, 5, 4, 3, 2, 1, 10, 9, 0, 0, 0, 0, 0, 0]),
        (16, vec![10, 0, 8], vec![8, 7, 6, 5, 4, 3, 2, 1, 10, 9, 5, 0, 0, 0, 0, 0]),
    ] {
        let Layout::Fixed(layout) = &mut layouts[ty.index()].layout else { unreachable!() };
        layout.size = size_bytes;
        layout.offsets = vec![offsets];
        assert_eq!(data.native(&module.types, &layouts), (8, expected.into()));
        let native = hirsel_core_ir::native_layout::resolve(module, &layouts);
        assert!(native.functions.iter().filter_map(|f| f.body.as_ref()).any(|b| {
            b.insts.contains(&Inst::Const { result: size, bits: size_bytes })
        }));
        assert_eq!(body_hash(), saved);
    }
}

fn revision_cases() -> Vec<RevisionCase> {
    let s = |name, a: &str, b: &str, wrap: &str, expect| RevisionCase {
        name,
        a: format!("{a}{wrap}").leak(),
        b: format!("{b}{wrap}").leak(),
        std: (Std::None, Std::None),
        expect,
    };
    const OPAQUE: &str = "fn f(x: u64) -> u64 { x + 7 }\n\
        fn same[F](a: StaticFn[F], b: StaticFn[F]) -> bool { code.identity(a) == code.identity(b) }\n\
        fn entry() -> u64 { if same(code.describe(f), code.describe(f)) { 7 } else { 0 } }\n";
    use Revision::*;
    vec![
        s("s01 newline subtraction", "fn entry() -> i64 { 1i64 - 2 }\n", "fn entry() -> i64 { 1i64\n - 2 }\n", WRAP_I64, Differ),
        s("s02 newline in block comment", "fn entry() -> i64 { 1i64 /* x */ - 2 }\n", "fn entry() -> i64 { 1i64 /* x\n */ - 2 }\n", WRAP_I64, Differ),
        s("s03 direct call", "fn helper() -> u64 { 7 }\nfn entry() -> u64 { helper() }\n", "fn helper() -> u64 { 8 }\nfn entry() -> u64 { helper() }\n", WRAP_7, Differ),
        s(
            "s04 transitive call",
            "fn leaf() -> u64 { 7 }\nfn helper() -> u64 { leaf() }\nfn entry() -> u64 { helper() }\n",
            "fn leaf() -> u64 { 8 }\nfn helper() -> u64 { leaf() }\nfn entry() -> u64 { helper() }\n",
            WRAP_7,
            Differ,
        ),
        RevisionCase {
            name: "s51 builtin codec maker",
            a: format!("fn entry() -> u64 {{ let c: std.codec.Codec[u64] = std.codec.of(); std.codec.least(c) }}\n{WRAP_8}").leak(),
            b: format!("fn entry() -> u64 {{ let c: std.codec.Codec[u64] = std.codec.of(); std.codec.least(c) }}\n{WRAP_8}").leak(),
            std: (
                Std::Tree,
                Std::Edit(
                    "codec.hirsel",
                    "make(encode_u64, take_u64, describe_u64, 8)",
                    "make(encode_u64, take_u64, describe_u64, 9)",
                ),
            ),
            expect: Differ,
        },
        s("i1 opaque code.identity ignores an unreached edit",
          &format!("{OPAQUE}fn spare() -> u64 {{ 1 }}\n"),
          &format!("{OPAQUE}fn spare() -> u64 {{ 2 }}\n"), WRAP_7, Equal),
        s("i2 opaque code.identity ignores an unreached addition",
          OPAQUE, &format!("{OPAQUE}fn spare() -> u64 {{ 1 }}\n"), WRAP_7, Equal),
        s("c01 local rename", "fn entry() -> u64 { let n = 7u64; n }\n", "fn entry() -> u64 { let z = 7u64; z }\n", WRAP_7, Equal),
        s(
            "c02 parameter rename",
            "fn helper(n: u64) -> u64 { n }\nfn entry() -> u64 { helper(7) }\n",
            "fn helper(z: u64) -> u64 { z }\nfn entry() -> u64 { helper(7) }\n",
            WRAP_7,
            Equal,
        ),
        s("c03 unreached addition", "fn entry() -> u64 { 7 }\n", "fn spare() -> u64 { 9 }\nfn entry() -> u64 { 7 }\n", WRAP_7, Equal),
        s("c04 comment", "fn entry() -> u64 { 7 }\n", "fn entry() -> u64 { /* ordinary */ 7 }\n", WRAP_7, Equal),
        s(
            "c05 item order",
            "fn helper() -> u64 { 7 }\nfn entry() -> u64 { helper() }\n",
            "fn entry() -> u64 { helper() }\nfn helper() -> u64 { 7 }\n",
            WRAP_7,
            Equal,
        ),
        s(
            "c06 grouping before a closure",
            GENERATED_CLOSURE,
            &GENERATED_CLOSURE.replace("7u64", "(7u64)"),
            WRAP_7,
            Equal,
        ),
        s("l1 lines above", LINES, &format!("// a\n// b\n\n\n{LINES}"), WRAP_1002, Equal),
        s("l2 re-indent", LINES, &LINES.replace("    ", "        "), WRAP_1002, Equal),
        // The callee is reached only through an instance that carries a
        // dictionary (`via[u64]`, constraint `plain`), so the edge that
        // carries the edit leaves that instance.
        s(
            "d1 call out of a constrained instance",
            "fn helper() -> u64 { 7 }\nfn via[T: plain](take x: T) -> u64 { helper() }\nfn entry() -> u64 { via(1u64) }\n",
            "fn helper() -> u64 { 8 }\nfn via[T: plain](take x: T) -> u64 { helper() }\nfn entry() -> u64 { via(1u64) }\n",
            WRAP_7,
            Differ,
        ),
        s(
            "r1 unreached edit",
            "fn spare() -> u64 { 9 }\nfn entry() -> u64 { 7 }\n",
            "fn spare() -> u64 { 8 }\nfn entry() -> u64 { 7 }\n",
            WRAP_7,
            Equal,
        ),
    ]
}

/// The check request of `app` in `dir`, with std from `std` when given.
fn revision_request(app: &str, std: Option<&Path>, dir: &Path) -> CheckRequest {
    let mut request = record_request(app, &core_sources(), dir, "store");
    if let Some(std) = std {
        let mut manifest = PackageManifest::directory(std, package("std"), false).unwrap();
        manifest.sources.retain(|s| s.modules[0] != "tests");
        manifest.dependencies = vec![package("core")];
        request.inputs.packages[0].dependencies.push(package("std"));
        request.inputs.packages.push(manifest);
    }
    request
}
/// A build configuration: backend, optimisation, root limit.
type Config = (Backend, bool, u64);
const CRANELIFT: Config = (Backend::Cranelift, false, 1_048_576);
/// Check and build `app` in `dir` (its stores primed by whatever was built
/// there before); the entry's revision and the executable's exit.
fn revision_build(app: &str, std: Option<&Path>, dir: &Path, (backend, optimize, root_limit): Config) -> (ContentHash, Option<i32>) {
    let product = Driver::default()
        .check(revision_request(app, std, dir), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let options = NativeOptions { optimize, root_limit, ..native_options(backend, dir) };
    let built = product
        .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
        .unwrap();
    let exit = Command::new(&built.executable).status().unwrap().code();
    assert_eq!(built.revision, hirsel_core_ir::code_revision(built.address));
    (built.address, exit)
}
/// The std directory of a side: the tree's, or a copy under `dir` with one edit.
fn revision_std(std: Std, dir: &Path, side: &str) -> Option<PathBuf> {
    let tree = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/std");
    match std {
        Std::None => None,
        Std::Tree => Some(tree),
        Std::Edit(file, from, to) => {
            let copy = dir.join(format!("std-{side}"));
            copy_tree(&tree, &copy);
            let text = std::fs::read_to_string(copy.join(file)).unwrap();
            assert_eq!(text.matches(from).count(), 1, "the std edit names one site");
            std::fs::write(copy.join(file), text.replace(from, to)).unwrap();
            Some(copy)
        }
    }
}

#[test]
#[ignore = "explicit native integration target"]
fn revision_laws_and_reuse() {
    for case in revision_cases() {
        let dir = tempfile::tempdir().unwrap();
        let (a_dir, b_dir) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a_dir).unwrap();
        std::fs::create_dir_all(&b_dir).unwrap();
        let a_std = revision_std(case.std.0, dir.path(), "a");
        let b_std = revision_std(case.std.1, dir.path(), "b");
        let a = revision_build(case.a, a_std.as_deref(), &a_dir, CRANELIFT);
        let b = revision_build(case.b, b_std.as_deref(), &b_dir, CRANELIFT);
        let name = case.name;
        assert_eq!(a.1, Some(0), "{name}: A runs clean");
        match case.expect {
            Revision::Differ => {
                assert_ne!(b.1, a.1, "{name}: the exits differ (the oracle)");
                assert_ne!(a.0, b.0, "{name}: L1 the revisions differ");
            }
            Revision::Equal => {
                assert_eq!(b.1, a.1, "{name}: the exits agree");
                assert_eq!(a.0, b.0, "{name}: L2/L4 the revisions are equal");
            }
        }
        // B into A's store (partly reused), then A again (wholly reused).
        let primed_b = revision_build(case.b, b_std.as_deref(), &a_dir, CRANELIFT);
        let primed_a = revision_build(case.a, a_std.as_deref(), &a_dir, CRANELIFT);
        assert_eq!(primed_b.0, b.0, "{name}: B in a primed store");
        assert_eq!(primed_a.0, a.0, "{name}: A again in a primed store");
    }
    // L6: the root limit alone.
    let dir = tempfile::tempdir().unwrap();
    let app = format!("{S45}{WRAP_7}");
    let (a_dir, b_dir) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a_dir).unwrap();
    std::fs::create_dir_all(&b_dir).unwrap();
    let a = revision_build(&app, None, &a_dir, CRANELIFT);
    let b = revision_build(&app, None, &b_dir, (Backend::Cranelift, false, 0));
    assert_eq!(a.0, b.0, "s45: L6 a root limit is not a revision input");
    revision_warm_slot_order_is_current();
}

#[test]
#[ignore = "explicit native integration target"]
fn revision_engines_agree() {
    weak_adapter_link_orders();
    let variant_source = "fn apply[T: plain](x: T, f: fn(T) -> T) -> T { f(x) }\n\
        fn forward[T: plain](x: T, f: fn(T) -> T) -> T { apply(x, f) }\n\
        fn inc(x: u64) -> u64 { x + 1 }\n\
        fn vector(x: u8) -> bool { let v: Simd[u8, 16] = simd.splat(x); simd.any(v) }\n\
        pub fn main() -> Result[(), Exit] { if forward(41u64, inc) == 42 && vector(7u8) && !vector(0u8) { Ok(()) } else { Error(Exit(status: 1)) } }\n";
    let dir = tempfile::tempdir().unwrap();
    let product = Driver::default()
        .check(revision_request(variant_source, None, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let identity = Identity::plain(entry.clone());
    let units = product.units([identity.clone()], 4096).unwrap();
    let address = units.address(&identity).unwrap();
    let vector = DefPath::from_full("app@1/main/vector").unwrap();
    let modules = units.modules().unwrap();
    let vector = modules.iter().find(|unit| unit.identity.definition == vector).unwrap();
    assert!(vector.module.functions[vector.function.index()].versioned, "D12 a real versioned definition");
    for (backend, optimize) in [(Backend::Cranelift, false), (Backend::Llvm, true)] {
        let options = NativeOptions { optimize, ..native_options(backend, dir.path()) };
        let built = product.build(&entry, &options).unwrap();
        assert_eq!(Command::new(&built.executable).status().unwrap().code(), Some(0));
        assert_eq!(built.address, address, "A7 native and checked addresses agree");
        if optimize {
            assert!(built.private_variants > 0, "A7 LLVM release emitted a private callable variant");
        }
    }
    for case in revision_cases() {
        for (side, app, std) in [("a", case.a, case.std.0), ("b", case.b, case.std.1)] {
            let dir = tempfile::tempdir().unwrap();
            let std = revision_std(std, dir.path(), side);
            let revisions: Vec<_> = [CRANELIFT, (Backend::Llvm, false, 1_048_576), (Backend::Llvm, true, 1_048_576)]
                .into_iter()
                .enumerate()
                .map(|(n, config)| {
                    let at = dir.path().join(format!("e{n}"));
                    std::fs::create_dir_all(&at).unwrap();
                    revision_build(app, std.as_deref(), &at, config)
                })
                .collect();
            let name = case.name;
            assert_eq!(revisions[1], revisions[0], "{name} {side}: LLVM dev is Cranelift's");
            assert_eq!(revisions[2], revisions[0], "{name} {side}: LLVM release is Cranelift's");
        }
    }
}

fn weak_adapter_link_orders() {
    use hirsel_core_ir::FuncId;
    use hirsel_core_materialize::{Unit, children, join};

    const APP: &str = "fn target(x: u64) -> u64 { x + 1u64 }\n\
        fn shift(x: u64) -> u64 { x + 2u64 }\n\
        fn left(x: u64) -> u64 { let f: fn(u64) -> u64 = target; f(x) }\n\
        fn right(x: u64) -> u64 { let shifted = shift(x); let f: fn(u64) -> u64 = target; f(shifted) }\n\
        pub fn main() -> Result[(), Exit] { if left(4) == 5 && right(5) == 8 { Ok(()) } else { Error(Exit(status: 1)) } }\n";
    let dir = tempfile::tempdir().unwrap();
    let product = record_check(APP, &core_sources(), dir.path(), "store");
    let plain = |name| Identity::plain(DefPath::from_full(&format!("app@1/main/{name}")).unwrap());
    let [main, left, right, target] = ["main", "left", "right", "target"].map(plain);
    let reached = product.reachable(&main.definition, 256).unwrap();
    assert!(reached.accepted());
    let units = [&left, &right].map(|owner| {
        let body = reached.materialized.get(owner);
        Unit {
            identity: owner,
            module: body.ir().unwrap(),
            function: body.function,
        }
    });
    let adapters = units.map(|unit| {
        let mut generated = unit.module.functions.iter().enumerate().filter(|(_, f)| {
            f.body.is_some()
                && unit.module.instances[f.instance.index()]
                    .definition
                    .0
                    .name
                    .starts_with("target$callable[")
        });
        let (row, adapter) = generated.next().expect("owner defines a callable adapter");
        assert!(generated.next().is_none());
        let row = FuncId(row as u32);
        let bindings: Vec<_> = children(unit.module, row)
            .into_iter()
            .map(|child| child.identity)
            .collect();
        assert_eq!(bindings.as_slice(), std::slice::from_ref(&target));
        let symbols = reached
            .materialized
            .link_symbols(unit.identity, &mut Vec::new())
            .unwrap();
        (
            row,
            unit.module.identity(adapter.instance),
            symbols[row.index()].clone(),
            hirsel_core_ir::normal::encode(unit.module, row).hash(),
        )
    });
    assert_ne!(
        adapters[0].0, adapters[1].0,
        "shift changes unit-local numbering"
    );
    assert_eq!(adapters[0].1, adapters[1].1);
    assert_eq!(adapters[0].2, adapters[1].2);
    assert_eq!(adapters[0].3, adapters[1].3, "P1 the shared adapter has one normal body");
    for order in [units, [units[1], units[0]]] {
        // The IR linker remaps both copies into one set of tables and compares
        // their bodies and signatures under the shared identity.
        join(&order, &left).expect("remapped copies of the shared adapter agree");
    }

    let mut expected = None;
    for reverse in [false, true] {
        let at = dir.path().join(format!("Cranelift-{reverse}"));
        let options = native_options(Backend::Cranelift, &at);
        let built = link_weak_adapter_order(&product, &main, [&left, &right], reverse, &options);
        run_clean(&built.executable, 0);
        assert_eq!(built.revision, *expected.get_or_insert(built.revision));
    }
    // A release build joins its recipe's units in identity order, whichever
    // order they were reached in; its image has the development build's revision.
    let at = dir.path().join("Llvm-release");
    let options = NativeOptions {
        optimize: true,
        ..native_options(Backend::Llvm, &at)
    };
    let built = product.build(&main.definition, &options).unwrap();
    run_clean(&built.executable, 0);
    assert_eq!(Some(built.revision), expected);
}

fn link_weak_adapter_order(
    product: &CheckProduct,
    main: &Identity,
    owners: [&Identity; 2],
    reverse: bool,
    options: &NativeOptions,
) -> BuildProduct {
    let entry = &main.definition;
    let reached = product.reachable_for(entry, options).unwrap();
    let instances = reached.require().unwrap();
    let inputs = reached.native.as_ref().unwrap();
    let [a, b] = owners.map(|owner| instances.iter().position(|i| i == owner).unwrap());
    assert!(a < b);
    // The reach walk keeps its sorted table. Reverse just the inputs to the
    // native object link.
    let (mut objects, _) = product.emit_reached(entry, &reached, options).unwrap();
    assert_eq!(&objects[a].instance, owners[0]);
    assert_eq!(&objects[b].instance, owners[1]);
    if reverse {
        objects.swap(a, b);
    }
    let items: Vec<_> = instances
        .iter()
        .flat_map(|i| reached.materialized.items(i).unwrap())
        .collect();
    let rows = product.source_rows(&items).unwrap();
    let (abi, cleanup) = product
        .bind_entry_native(entry, options, inputs, &BTreeSet::new())
        .unwrap();
    let plan = crate::native_entry::Plan::new(
        main.symbol(),
        entry.to_string(),
        &abi,
        cleanup.as_ref().map(|o| o.instance.symbol()),
        false,
        options,
    )
    .unwrap();
    objects.extend(cleanup);
    let linked = link_objects(
        &plan,
        &objects,
        &crate::foreign_archive::LinkArchives::default(),
        None,
        options,
        crate::reach::sources_digest(product, &rows).unwrap(),
        || product.source_map(&rows),
        inputs.toolchain.image,
        inputs.recipe.clone(),
        false,
        reached.counters,
        |_| Ok(0),
        reached.address(entry).unwrap(),
        || reached.revisions(),
        None,
    )
    .unwrap();
    let Linked::Built(built) = linked else {
        panic!("fresh objects failed verification");
    };
    built
}

/// A program printing the revisions of `f` (body `f`), `g`, `main` and the
/// intrinsic `float.widen`, 32 bytes each.
fn described(f: &str) -> String {
    format!(
        "fn f() -> u64 {{ {f} }}\nfn g() -> u64 {{ 9 }}\n\
         fn put[F](out: Stdout, s: StaticFn[F]) -> bool waits {{\n\
         let Some(r) = code.revision(s) else {{ return false }}\n\
         result.unwrap(stdout.write_bytes(out, r));\n\
         true\n}}\n\
         pub fn main(out: Stdout) -> Result[(), Exit] waits {{\n\
         if put(out, code.describe(f)) && put(out, code.describe(g)) && put(out, code.describe(main)) && put(out, code.describe(float.widen)) \
         {{ Ok(()) }} else {{ Error(Exit(status: 2u8)) }}\n}}\n"
    )
}
/// The printed revisions of f, g, main and float.widen, and the build's
/// entry revision.
type Printed = ([Vec<u8>; 4], ContentHash);
/// Build `app` over a source core (and std when given) in `dir` and run it.
fn described_build(app: &str, std: Option<&Path>, dir: &Path, (backend, optimize, root_limit): Config) -> Printed {
    let product = Driver::default()
        .check(revision_request(app, std, dir), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let options = NativeOptions { optimize, root_limit, grant_mask: 1 << 4, ..native_options(backend, dir) };
    let built = product
        .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
        .unwrap();
    (described_run(&built.executable), built.revision)
}
fn described_run(executable: &Path) -> [Vec<u8>; 4] {
    let output = Command::new(executable).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout.len(), 128, "four revisions");
    std::array::from_fn(|k| output.stdout[32 * k..32 * (k + 1)].to_vec())
}

#[test]
#[ignore = "explicit native integration target"]
fn revision_described() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (described("7"), described("8"));
    let at = |name: &str| {
        let at = dir.path().join(name);
        std::fs::create_dir_all(&at).unwrap();
        at
    };
    let fresh_a = described_build(&a, None, &at("a"), CRANELIFT);
    let fresh_b = described_build(&b, None, &at("b"), CRANELIFT);
    let ([f, g, main, _], entry) = &fresh_a;
    assert_ne!(f, g, "f and g have their own rows");
    assert_eq!(&main[..], &entry.0[..], "main's printed revision is the build's");
    assert_ne!(fresh_a.0[0], fresh_b.0[0], "an edit to f changes f's revision");
    assert_eq!(fresh_a.0[1], fresh_b.0[1], "and leaves g's equal");
    assert_eq!(fresh_a.0[3], fresh_b.0[3], "and the intrinsic's");
    let intrinsic = Identity::plain(DefPath::core("float.widen"));
    let expected = hirsel_core_ir::code_revision(hirsel_core_ir::builtin_address(&intrinsic).unwrap());
    assert_eq!(fresh_a.0[3], expected.0);
    // B into A's store, then A again (a link hit), then A under another root
    // limit (a reach hit and a link miss: the rows come from the record).
    assert_eq!(described_build(&b, None, &at("a"), CRANELIFT), fresh_b, "B in a primed store");
    assert_eq!(described_build(&a, None, &at("a"), CRANELIFT), fresh_a, "A again in a primed store");
    assert_eq!(described_build(&a, None, &at("a"), (Backend::Cranelift, false, 2_097_152)).0, fresh_a.0, "A from its reach record");
    // L3: the engines.
    for (n, config) in [(Backend::Llvm, false, 1_048_576), (Backend::Llvm, true, 1_048_576)].into_iter().enumerate() {
        assert_eq!(described_build(&a, None, &at(&format!("e{n}")), config).0, fresh_a.0, "{config:?}: Cranelift's bytes");
    }
    // L5: an imaged core and std against their sources (the package set is
    // the same: std's codec makers are selected from types).
    let std = revision_std(Std::Tree, dir.path(), "a");
    let source = described_build(&a, std.as_deref(), &at("source"), CRANELIFT);
    let libraries = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core");
    let project = at("imaged");
    std::fs::write(project.join("main.hirsel"), &a).unwrap();
    let request = CheckRequest::program(&project, &libraries, &project.join("store")).unwrap();
    let product = Driver::default()
        .check_reachable_with_product(request, &at("cache"), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let options = NativeOptions { grant_mask: 1 << 4, ..native_options(Backend::Cranelift, &project) };
    let built = product.build(&DefPath::from_full("app@1/main/main").unwrap(), &options).unwrap();
    assert_eq!((described_run(&built.executable), built.revision), source, "A8: an imaged core's revisions");
    assert_eq!(hirsel_core_ir::code_revision(built.address), built.revision);
}

// Hand-framed v1 singleton group. This oracle assembles the specified bytes
// directly: one binding per builtin at its version, beside the local body hash.
fn builtin_group(body: ContentHash, slots: usize) -> ContentHash {
    assert!(slots < 128);
    let mut group = b"HIRGROUP\0\x01\x00\x01".to_vec();
    group.extend_from_slice(&body.0);
    group.push(slots as u8);
    for _ in 0..slots {
        group.extend_from_slice(&[2, 1, 0, 0, 0]);
    }
    let group = ContentHash::of_parts("hirsel.code-group.v1", [&group[..]]);
    let mut member = [0; 36];
    member[..32].copy_from_slice(&group.0);
    ContentHash::of_parts("hirsel.code-address.v1", [&member[..]])
}

#[test]
fn revision_builtin_dependencies_use_the_producer() {
    let dir = tempfile::tempdir().unwrap();
    let app = "fn direct(x: f32) -> f64 { float.widen(x) }\n\
               fn callable(x: f32) -> f64 { let widen: fn(f32) -> f64 = float.widen; widen(x) }\n\
               fn descriptor(out: Stdout) -> bool waits {\n\
                   let Some(r) = code.revision(code.describe(float.widen)) else { return false };\n\
                   match stdout.write_bytes(out, r) { Ok(_) => true, Error(_) => false }\n\
               }\n\
               fn unrelated(x: f64) -> f32 { float.narrow(x) }\n\
               pub fn main() -> Result[(), Exit] { Ok(()) }\n";
    let product = Driver::default()
        .check(revision_request(app, None, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let roots = ["direct", "callable", "descriptor", "unrelated"].map(|name| {
        Identity::plain(DefPath::from_full(&format!("app@1/main/{name}")).unwrap())
    });
    let units = product.units(roots.clone(), 4096).unwrap();
    let modules = units.modules().unwrap();
    let mut observed = Vec::new();
    let mut expected = Vec::new();
    for (root, name) in roots.iter().zip(["float.widen", "float.widen", "float.widen", "float.narrow"]) {
        let unit = modules.iter().find(|unit| unit.identity == root).unwrap();
        let normal = hirsel_core_ir::normal::encode(unit.module, unit.function);
        assert!(normal.slots.iter().any(|slot| matches!(slot,
            hirsel_core_ir::normal::Slot::Builtin(definition) if definition.is_core(name)
        )), "{root:?}: D5 records {name} in the producer");
        assert!(normal.slots.iter().all(|slot| matches!(slot, hirsel_core_ir::normal::Slot::Builtin(_))));
        observed.push(units.address(root).unwrap());
        expected.push(builtin_group(normal.hash(), normal.slots.len()));
    }
    let (module, descriptor) = hirsel_core_materialize::join(&modules, &roots[2]).unwrap();
    let result = hirsel_core_ir::eval::run(&module, descriptor, &[0], 4096);
    assert_eq!(result.outcome, hirsel_core_ir::eval::Outcome::Returned(vec![1]));
    // SHA-256 of the framed revision domain and the V0 float.widen address.
    let revision = [
        0xa6, 0x0a, 0x72, 0xe6, 0xe0, 0x12, 0x67, 0xbd, 0x93, 0xfe, 0x57, 0x58, 0xfe, 0x4c, 0xeb, 0xd3,
        0x06, 0xf3, 0x17, 0x59, 0x05, 0xa7, 0xeb, 0xe7, 0x83, 0x9b, 0x6d, 0x52, 0x59, 0x49, 0xbf, 0x6c,
    ];
    assert_eq!(result.stdout, revision, "D5 evaluator carrier binds builtin 1");
    assert_eq!(observed, expected, "D5 direct, callable and descriptor use bind builtin 1");
}

#[test]
fn revision_cross_module_and_recursive_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let main_source = "fn caller() -> u64 { leaf.value() }\n\
                       fn a(x: u64) -> u64 { if x == 0 { 7 } else { b(x - 1) } }\n\
                       fn b(x: u64) -> u64 { if x == 0 { 7 } else { a(x - 1) } }\n\
                       fn sibling() -> u64 { 9 }\n\
                       pub fn main() -> Result[(), Exit] { if caller() + a(1) + sibling() == 23 { Ok(()) } else { Error(Exit(status: 1)) } }\n";
    let roots = ["main", "caller", "a", "b", "sibling"].map(|name| {
        Identity::plain(DefPath::from_full(&format!("app@1/main/{name}")).unwrap())
    });
    let leaf = Identity::plain(DefPath::from_full("app@1/leaf/value").unwrap());
    let measure = |source: &str, leaf_value: u64| {
        let file = dir.path().join("leaf.hirsel");
        std::fs::write(&file, format!("pub fn value() -> u64 {{ {leaf_value} }}\n")).unwrap();
        let mut request = revision_request(source, None, dir.path());
        request.inputs.packages[0].sources.push(hirsel_core_resolve::ModuleSource {
                    foreign: false,
                    captured: None,
            file, modules: vec!["leaf".into()], signatures: false,
        });
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
        let units = product.units([roots[0].clone()], 4096).unwrap();
        let modules = units.modules().unwrap();
        let body_and_address = |root: &Identity| {
            let unit = modules.iter().find(|unit| unit.identity == root).unwrap();
            let body = hirsel_core_ir::normal::encode(unit.module, unit.function).hash();
            (body, units.address(root).unwrap())
        };
        let result = [
            body_and_address(&roots[0]), body_and_address(&roots[1]), body_and_address(&roots[2]),
            body_and_address(&roots[3]), body_and_address(&roots[4]), body_and_address(&leaf),
        ];
        for root in [&roots[2], &roots[3]] {
            let from_member = product.units([root.clone()], 4096).unwrap();
            assert_eq!(from_member.address(&roots[2]), Some(result[2].1));
            assert_eq!(from_member.address(&roots[3]), Some(result[3].1));
        }
        result
    };
    let before = measure(main_source, 7);
    let after_leaf = measure(main_source, 8);
    for i in [0, 1] {
        assert_eq!(before[i].0, after_leaf[i].0, "D3 caller body is local");
        assert_ne!(before[i].1, after_leaf[i].1, "D3 caller address follows the binding");
    }
    assert_ne!(before[5].0, after_leaf[5].0);
    assert_ne!(before[5].1, after_leaf[5].1);
    assert_eq!(before[2..5], after_leaf[2..5]);
    let after_b = measure(&main_source.replace(
        "fn b(x: u64) -> u64 { if x == 0 { 7 }",
        "fn b(x: u64) -> u64 { if x == 0 { 8 }",
    ), 7);
    assert_eq!(before[2].0, after_b[2].0, "D4 a's body is local");
    assert_ne!(before[2].1, after_b[2].1, "D4 a follows its recursive group");
    assert_ne!(before[3].0, after_b[3].0);
    assert_ne!(before[3].1, after_b[3].1);
    assert_eq!(before[4..], after_b[4..]);
}

#[test]
fn revision_s51_maker_changes_the_binding_and_survives_an_image() {
    let dir = tempfile::tempdir().unwrap();
    let case = revision_cases().into_iter().find(|case| case.name.starts_with("s51 ")).unwrap();
    let std = revision_std(Std::Tree, dir.path(), "base").unwrap();
    let edited = revision_std(case.std.1, dir.path(), "edit").unwrap();
    let product = |std: &Path, store: &Path| {
        let mut request = revision_request(case.a, Some(std), dir.path());
        request.store = store.to_owned();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
        product
    };
    let source = product(&std, &dir.path().join("source"));
    let edit = product(&edited, &dir.path().join("edited"));
    let image = prebuild_std(&dir.path().join("image"));
    let request = revision_request(case.a, Some(&std), dir.path());
    let (image_build, _) = prebuild_check(
        &image, &dir.path().join("installed-check"), vec![request.inputs.packages[0].clone()], Some("app@1/main/main"),
    );
    let main = Identity::plain(DefPath::from_full("app@1/main/main").unwrap());
    let entry = Identity::plain(DefPath::from_full("app@1/main/entry").unwrap());
    let maker = Identity::plain(DefPath::from_full("std@1/codec/of_u64").unwrap());
    let facts = |product: &CheckProduct| {
        let units = product.units([main.clone()], 65_536).unwrap();
        let modules = units.modules().unwrap();
        [&main, &entry, &maker].map(|root| {
            let unit = modules.iter().find(|unit| unit.identity == root).unwrap();
            let body = hirsel_core_ir::normal::encode(unit.module, unit.function);
            let slots = body.slots.iter().map(|slot| slot.identity(unit.module)).collect::<Vec<_>>();
            (body.hash(), units.address(root).unwrap(), slots)
        })
    };
    let before = facts(&source);
    let after = facts(&edit);
    assert!(before[1].2.contains(&maker), "D2 type-selected maker is a slot");
    assert_eq!(before[1].0, after[1].0, "D2 caller body stays local");
    assert_ne!(before[2].0, after[2].0, "D2 selected maker body changed");
    assert_ne!(before[2].1, after[2].1, "D2 selected maker address changed");
    assert_ne!(before[1].1, after[1].1, "D2 caller address followed the maker");
    assert_ne!(source.root, image_build.root, "A8 the pre-check root differs");
    assert!(!image_build.definitions.contains_key(&maker.definition));
    assert!(image_build.image.as_ref().unwrap().has_body(&maker.definition).unwrap(), "A8 the maker is supplied by the installed image");
    assert_eq!(before, facts(&image_build), "A8 image and source body, slots, and address agree");
}

#[test]
fn revision_retargeting_is_in_the_body() {
    let dir = tempfile::tempdir().unwrap();
    let original = "fn f() -> i64 { 8 }\nfn g() -> i64 { 3 }\nfn diff() -> i64 { f() - g() }\n\
        pub fn main() -> Result[(), Exit] { if diff() > 0 { Ok(()) } else { Error(Exit(status: 1)) } }\n";
    let diff = Identity::plain(DefPath::from_full("app@1/main/diff").unwrap());
    let check = |app: &str| {
        let product = Driver::default()
            .check(revision_request(app, None, dir.path()), &mut Meter::new(false)).unwrap();
        assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
        let units = product.units([diff.clone()], 4096).unwrap();
        let modules = units.modules().unwrap();
        let owner = modules.iter().find(|unit| *unit.identity == diff).unwrap();
        let body = hirsel_core_ir::normal::encode(owner.module, owner.function).hash();
        let leaves = modules.iter().filter(|unit| unit.identity != &diff)
            .map(|unit| (unit.identity.clone(), units.address(unit.identity).unwrap()))
            .collect::<BTreeMap<_, _>>();
        let order = if app.contains("g() - f()") { ["g", "f"] } else { ["f", "g"] };
        let mut group = b"HIRGROUP\0\x01\x00\x01".to_vec();
        group.extend_from_slice(&body.0);
        group.push(2);
        for name in order {
            let leaf = Identity::plain(DefPath::from_full(&format!("app@1/main/{name}")).unwrap());
            group.push(1);
            group.extend_from_slice(&leaves[&leaf].0);
        }
        let group = ContentHash::of_parts("hirsel.code-group.v1", [&group[..]]);
        let mut member = [0; 36];
        member[..32].copy_from_slice(&group.0);
        let expected = ContentHash::of_parts("hirsel.code-address.v1", [&member[..]]);
        assert_eq!(units.address(&diff), Some(expected), "A9 group uses canonical slot order");
        (body, units.address(&diff).unwrap(), leaves)
    };
    let (a, b) = (check(original), check(&original.replace("f() - g()", "g() - f()")));
    assert_eq!(a.2, b.2, "D9 the same identities and leaf addresses are reached");
    assert_ne!(a.0, b.0, "D9 the first use names a different slot");
    assert_ne!(a.1, b.1);
}

fn revision_warm_slot_order_is_current() {
    let dir = tempfile::tempdir().unwrap();
    let a = "fn z() -> u64 { 7 }\nfn a() -> u64 { 1 }\nfn p() -> u64 { z() - a() }\n\
             pub fn main() -> Result[(), Exit] { if p() == 6 { Ok(()) } else { Error(Exit(status: 1)) } }\n\
             pub fn alt() -> Result[(), Exit] { if p() == 7 { Ok(()) } else { Error(Exit(status: 1)) } }\n";
    let b = a.replace("z() -> u64 { 7 }", "z() -> u64 { 8 }");
    let identity = |name: &str| Identity::plain(DefPath::from_full(&format!("app@1/main/{name}")).unwrap());
    let p = identity("p");
    let (z, leaf_a) = (identity("z"), identity("a"));
    let options = native_options(Backend::Cranelift, dir.path());
    let check = |source: &str, entry: &DefPath| {
        let mut request = revision_request(source, None, dir.path());
        request.entry = Some(entry.clone());
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
        product
    };
    let main = identity("main").definition;
    let alt = identity("alt").definition;
    let first = check(a, &main);
    let built = first.build(&main, &options).unwrap();
    assert_eq!(Command::new(&built.executable).status().unwrap().code(), Some(0));
    let old = first.units([p.clone()], 4096).unwrap().address(&p).unwrap();
    for (entry, is_expansion) in [(&alt, false), (&main, true)] {
        let current = check(&b, entry);
        let reached = current.reachable_for(entry, &options).unwrap();
        let prior = reached.reach.as_ref().and_then(|r| r.prior.as_ref());
        let (body, slots) = if is_expansion {
            let row = reached.materialized.table[&p];
            let row = prior.unwrap().entry(row as usize);
            (row.body, &row.slots)
        } else {
            let hit = &reached.materialized.hits[&p];
            (hit.body, &hit.slots)
        };
        assert_eq!(slots, &[z.clone(), leaf_a.clone()], "A9 exact slot order from the warm record");
        let bound = reached.materialized.addresses(reached.require().unwrap(), prior).unwrap();
        // Hand framing for a singleton with External(z) then External(a).
        let mut bytes = b"HIRGROUP\0\x01\x00\x01".to_vec();
        bytes.extend_from_slice(&body.0);
        bytes.push(2);
        for target in [&z, &leaf_a] {
            bytes.push(1);
            bytes.extend_from_slice(&bound[target].0);
        }
        let group = ContentHash::of_parts("hirsel.code-group.v1", [&bytes[..]]);
        let mut member = [0; 36];
        member[..32].copy_from_slice(&group.0);
        let expected = ContentHash::of_parts("hirsel.code-address.v1", [&member[..]]);
        assert_eq!(bound[&p], expected, "A9 warm slots rebind in the current graph");
        assert_ne!(bound[&p], old, "A9 callee edit invalidates the prior address");
        let cold = current.units([p.clone()], 4096).unwrap();
        assert_eq!(cold.address(&p), Some(expected));
        let built = current.build_reachable(entry, &reached, &options).unwrap();
        assert_eq!(Command::new(&built.executable).status().unwrap().code(), Some(i32::from(is_expansion)));
    }
}

#[test]
fn revision_parser_and_source_keys_are_distinct_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let root = Identity::plain(DefPath::from_full("app@1/main/entry").unwrap());
    let measure = |case: &str, changed: &DefPath| {
        let product = Driver::default()
            .check(revision_request(case, None, dir.path()), &mut Meter::new(false)).unwrap();
        assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
        let key = product.demand_body(changed).unwrap().record;
        let units = product.units([root.clone()], 4096).unwrap();
        let modules = units.modules().unwrap();
        let (module, function) = hirsel_core_materialize::join(&modules, &root).unwrap();
        let address = units.address(&root).unwrap();
        (key, address, hirsel_core_ir::eval::run(&module, function, &[], 4096).outcome)
    };
    for case in revision_cases().into_iter().filter(|case| {
        ["s01 ", "s02 ", "c01 ", "c02 ", "c04 "].into_iter().any(|prefix| case.name.starts_with(prefix))
    }) {
        let changed = if case.name.starts_with("c02 ") {
            DefPath::from_full("app@1/main/helper").unwrap()
        } else {
            root.definition.clone()
        };
        let a = measure(case.a, &changed);
        let b = measure(case.b, &changed);
        assert_ne!(a.0, b.0, "{} varied the pre-check record", case.name);
        match case.expect {
            Revision::Equal => {
                assert_eq!(a.2, b.2, "{} equivalent execution", case.name);
                assert_eq!(a.1, b.1, "{} A3 source record is not the address", case.name);
            }
            Revision::Differ => {
                assert_ne!(a.2, b.2, "{} independent D1 evaluator difference", case.name);
                assert_ne!(a.1, b.1, "{} D1 different parse reaches different code", case.name);
            }
        }
    }
}

#[test]
fn revision_opaque_identity_ignores_unreached_items() {
    let dir = tempfile::tempdir().unwrap();
    let main = Identity::plain(DefPath::from_full("app@1/main/main").unwrap());
    let measure = |source| {
        let product = Driver::default()
            .check(revision_request(source, None, dir.path()), &mut Meter::new(false)).unwrap();
        assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
        let units = product.units([main.clone()], 4096).unwrap();
        let modules = units.modules().unwrap();
        let (module, function) = hirsel_core_materialize::join(&modules, &main).unwrap();
        let report = hirsel_core_ir::eval::run(&module, function, &[], 4096);
        (product.root, units.address(&main).unwrap(), report.outcome)
    };
    for case in revision_cases().into_iter().filter(|case| case.name.starts_with("i1 ") || case.name.starts_with("i2 ")) {
        let a = measure(case.a);
        let b = measure(case.b);
        assert_ne!(a.0, b.0, "{} changed the source", case.name);
        assert_eq!(a.2, b.2, "{} observed equal returns", case.name);
        assert_eq!(a.1, b.1, "{} I1 an opaque descriptor reads its own identity", case.name);
    }
}
