/// PROG-4: a relative grant still takes its slot number, so an absolute grant
/// after it is inherited at its own sparse descriptor.
#[test]
fn two_dir_grants_pass_relative_and_inherited_directories() {
    crate::openat2_required::require();
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("granted"), b"z").unwrap();
    let source = r#"
@test
fn two_dirs(private: Dir, outside: Dir) -> Result[(), ()] waits {
    let mut output = result.unwrap(host.open(private, "note", CreateNew))
    result.unwrap(host.write(mut output, string.as_bytes("ok")))
    result.unwrap(host.close(take output))
    let mut input = result.unwrap(host.open(private, "note", Read))
    let mut mine: Bytes = list.with_capacity(2)
    if result.unwrap(host.read(mut input, mut mine)) != 2u64 || mine[0] != b'o' || mine[1] != b'k' {
        return Error(())
    }
    result.unwrap(host.close(take input))
    let mut granted = result.unwrap(host.open(outside, "granted", Read))
    let mut byte: Bytes = list.with_capacity(1)
    if result.unwrap(host.read(mut granted, mut byte)) != 1u64 || byte[0] != b'z' {
        return Error(())
    }
    result.unwrap(host.close(take granted))
    Ok(())
}
"#;
    let product = Driver::default()
        .check(prog4_request(source, dir.path()), &mut Meter::new(false))
        .unwrap();
    assert!(
        product.accepted(),
        "{:?} {:?}",
        product.diagnostics,
        product.incomplete
    );
    let test = DefPath::from_full("app@1/main/two_dirs").unwrap();
    let mut options = native_options(Backend::Cranelift, dir.path());
    options.directories = vec![".".into(), outside.to_str().unwrap().into()];
    options.grant_mask = 1 << grant_kind("Dir").unwrap();
    let outcome = product.run_test(&test, &options).unwrap();
    assert!(outcome.passed(), "{outcome:?}");
}
