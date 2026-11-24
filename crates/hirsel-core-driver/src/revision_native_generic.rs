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
