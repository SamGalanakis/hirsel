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
