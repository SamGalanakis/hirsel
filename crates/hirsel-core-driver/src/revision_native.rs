// The code revision (hirsel.code-revision.v1, code-revision-1038 DESIGN v2),
// on the code-revision-review-1040 corpus. Laws:
// - L1 sensitivity: a pair whose exits differ has different revisions
//   (s01, s02, s03, s04, s51; d1 an edge out of an instance that carries a
//   dictionary). Oracle: the executables' exits, run here.
// - L2 invariance: an edit the language defines as non-semantic leaves the
//   revision equal (c01-c05: local name, parameter name, unreached item,
//   comment, item order; l1 lines above, l2 re-indent). Oracle: the language
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
const S45: &str = "fn entry() -> u64 { let r: Result[List[u64], Capacity] = list.with_capacity(1); match r { Ok(_) => 7, Error(_) => 8 } }\n";

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
fn own_layout_is_resolved_after_the_code_revision() {
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
    let module = reached.materialized.get(instance).ir().unwrap();
    let saved = crate::revision::body(module, &mut Vec::new()).unwrap();
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
        assert_eq!(crate::revision::body(module, &mut Vec::new()).unwrap(), saved);
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
                    "Codec(encoder: put_u64, decoder: take_u64, describer: describe_u64, smallest: 8)",
                    "Codec(encoder: put_u64, decoder: take_u64, describer: describe_u64, smallest: 9)",
                ),
            ),
            expect: Differ,
        },
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
    (built.revision, exit)
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
}

#[test]
#[ignore = "explicit native integration target"]
fn revision_engines_agree() {
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
    let source = described_build(&a, std.as_deref(), &at("source"), CRANELIFT).0;
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
    assert_eq!(described_run(&built.executable), source, "L5: an imaged core's bytes");
}
