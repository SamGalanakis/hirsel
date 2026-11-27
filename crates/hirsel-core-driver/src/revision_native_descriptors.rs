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
    let entry = DefPath::from_full("local/app@1/main/main").unwrap();
    let mut request = revision_request(app, std, dir);
    request.inputs.packages[0].package = entry.0.package.clone();
    request.entry = Some(entry.clone());
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(product.accepted(), "{:?} {:?}", product.diagnostics, product.incomplete);
    let options = NativeOptions { optimize, root_limit, grant_mask: 1 << 4, ..native_options(backend, dir) };
    let built = product
        .build(&entry, &options)
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
    let built = product.build(&DefPath::from_full("local/app@1/main/main").unwrap(), &options).unwrap();
    assert_eq!((described_run(&built.executable), built.revision), source, "A8: an imaged core's revisions");
    assert_eq!(hirsel_core_ir::code_revision(built.address), built.revision);
}

#[test]
#[ignore = "explicit native integration target"]
fn revision_described_generic_instance() {
    let dir = tempfile::tempdir().unwrap();
    let std = dir.path().join("std");
    std::fs::create_dir(&std).unwrap();
    std::fs::write(
        std.join("descriptor.hirsel"),
        r#"
pub fn same[T: key](a: T, b: T) -> bool { a == b }

pub fn describe_same[T: key]() -> StaticFn[fn(T, T) -> bool] {
    code.describe(same)
}
"#,
    )
    .unwrap();
    let source = r#"
fn direct() -> StaticFn[fn(u64, u64) -> bool] { code.describe(std.descriptor.same) }

pub fn main() -> Result[(), Exit] {
    let a = direct()
    let b: StaticFn[fn(u64, u64) -> bool] = std.descriptor.describe_same()
    let c: StaticFn[fn(u32, u32) -> bool] = std.descriptor.describe_same()
    if a != b || code.identity(b) != b"\x01std@1/descriptor/same[u64]{key[u64]}($0,$0)->bool"
        || code.identity(c) != b"\x01std@1/descriptor/same[u32]{key[u32]}($0,$0)->bool"
        || !(code.callable(b))(3, 3) {
        return Error(Exit(status: 1u8))
    }
    let Some(revision64) = code.revision(a) else { return Error(Exit(status: 2u8)) }
    let Some(revision32) = code.revision(c) else { return Error(Exit(status: 3u8)) }
    if list.len(revision64) == 32 && list.len(revision32) == 32 && revision64 != revision32 {
        Ok(())
    } else {
        Error(Exit(status: 4u8))
    }
}
"#;
    let request = revision_request(source, Some(&std), dir.path());
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let options = native_options(Backend::Cranelift, dir.path());
    let built = product
        .build(&DefPath::from_full("app@1/main/main").unwrap(), &options)
        .unwrap();
    let output = Command::new(&built.executable).output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "explicit native integration target"]
fn codec_via_storage_fingerprint() {
    use sha2::{Digest, Sha256};

    const SOURCE: &str = r#"
type DRangeWire: codec {
    lo: u64,
    hi: u64
}

type DRange: codec via DRangeWire(to: dwire, from: dcheck) {
    w: DRangeWire
}

fn dwire(r: DRange) -> View[DRangeWire] in r {
    view r.w
}

fn dcheck(take w: DRangeWire) -> Result[DRange, ()] {
    if w.lo <= w.hi { Ok(DRange(w)) } else { Error(()) }
}

pub fn main(out: Stdout) -> Result[(), Exit] waits {
    let codec: std.codec.Codec[DRange] = std.codec.of()
    let Some(fingerprint) = std.codec.fingerprint(codec) else {
        return Error(Exit(status: 1))
    }
    result.unwrap(stdout.write_bytes(out, fingerprint))
    Ok(())
}
"#;
    // core/tools/codec_storage_reference.py: range_via_prefix("app@1/main/").
    const VIA_PREFIX: &[u8] = b"\x01\0\0\0\x03\0\0\0via\x01\0\0\0\x11\0\0\0app@1/main/DRange\0\
        \x8e\xd8\x6f\x47\xa0\x47\x7a\xbc\x03\xe4\x0f\x57\x5e\x54\xf8\xa7\
        \x6e\x04\x98\x46\x8c\x92\x71\xf8\x26\xd4\x84\x5d\xb7\x2f\xb6\xb3";

    let dir = tempfile::tempdir().unwrap();
    let std = revision_std(Std::Tree, dir.path(), "via");
    let request = revision_request(SOURCE, std.as_deref(), dir.path());
    let product = Driver::default()
        .check(request, &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let options = NativeOptions {
        grant_mask: 1 << 4,
        instance_limit: 2048,
        ..native_options(Backend::Cranelift, dir.path())
    };
    let reached = product.reachable_objects(&entry, &options).unwrap();
    let revisions = reached.revisions().unwrap();
    let [to, from] = ["app@1/main/dwire", "app@1/main/dcheck"]
        .map(|path| revisions[&Identity::plain(DefPath::from_full(path).unwrap())].0);
    assert_ne!(to, from);

    fn digest(domain: &[u8], data: &[u8]) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update((domain.len() as u64).to_le_bytes());
        hash.update(domain);
        hash.update((data.len() as u64).to_le_bytes());
        hash.update(data);
        hash.finalize().into()
    }

    let group = digest(
        "hirsel.codec-group.v1".as_bytes(),
        &[VIA_PREFIX, &to, &from].concat(),
    );
    let mut member = [0; 37];
    member[0] = 1;
    member[1..33].copy_from_slice(&group);
    let expected = digest(b"hirsel.codec-storage.v1", &member);
    let built = product.build_objects(&entry, &reached, &options).unwrap();
    let output = Command::new(&built.executable).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, expected);
}
