// The scalar rows belong to core's ordinary generic bodies; no body is rechecked
// when the native collector materializes u8/i8/u64/i64 instances.
fn generic_numeric(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = r#"
fn positive() -> bool {
    let a: u8 = result.unwrap(int.parse("0255"))
    let b: i8 = result.unwrap(int.parse("-128"))
    let c: u64 = result.unwrap(int.parse("18446744073709551615"))
    let d: i64 = result.unwrap(int.parse("-9223372036854775808"))
    let out: Result[u8, ParseError] = int.parse("256")
    let bad: Result[u8, ParseError] = int.parse("99999999999999999999x")
    let good_range = match out { Error(OutOfRange) => true, _ => false }
    let good_grammar = match bad { Error(Invalid(i)) => i==20u64, _ => false }
    let cast: u16 = numbers.cast(65535u64)
    let absent: Option[u8] = numbers.maybe(-1i64)
    let signed: Option[i8] = numbers.maybe(-128i64)
    let mut xs: List[u8] = list.new()
    result.unwrap(list.push(mut xs,250u8))
    result.unwrap(list.push(mut xs,5u8))
    let sum = list.sum(xs)
    let mut ys: List[i8] = list.new()
    result.unwrap(list.push(mut ys,-128i8))
    result.unwrap(list.push(mut ys,127i8))
    result.unwrap(list.push(mut ys,-1i8))
    let signed_sum = list.sum(ys)
    a==255u8 && b== -128i8 && c==18446744073709551615u64 && d== -9223372036854775808i64 && good_range && good_grammar && cast==65535u16 && absent==None && option.unwrap(signed)== -128i8 && sum==255u8 && signed_sum== -2i8
}
fn overflow_sum() -> u8 {
    let mut xs: List[u8] = list.new()
    result.unwrap(list.push(mut xs,250u8))
    result.unwrap(list.push(mut xs,6u8))
    list.sum(xs)
}
fn overflow_cast() -> u8 { numbers.cast(256u64) }
"#;
    for (check, exit) in [
        ("positive()", 0),
        ("overflow_sum()==0u8", 101),
        ("overflow_cast()==0u8", 101),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let core = dir.path().join("core");
        std::fs::create_dir(&core).unwrap();
        for entry in std::fs::read_dir(root.join("core/core")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|x| x == "hirsel") {
                std::fs::copy(&path, core.join(path.file_name().unwrap())).unwrap();
            }
        }
        std::fs::write(core.join("numbers.hirsel"),
            "pub fn cast[T:integer](x:u64)->T { int.narrow(x) }\npub fn maybe[T:integer](x:i64)->Option[T] { int.try_narrow(x) }\n").unwrap();
        let main = format!(
            "{source}\npub fn main()->Result[(),Exit] {{ if {check} {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}"
        );
        let mut request = source_request(&main, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&core, package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        let built = product
            .build(
                &DefPath::from_full("app@1/main/main").unwrap(),
                &native_options(backend, dir.path()),
            )
            .unwrap_or_else(|e| panic!("{check}: {e:?}"));
        let output = Command::new(&built.executable).output().unwrap();
        assert_eq!(output.status.code(), Some(exit), "{check}: {output:?}");
        assert!(output.stdout.is_empty(), "{check}: {output:?}");
        if exit == 101 {
            assert!(
                output.stderr.starts_with(b"Overflow"),
                "{check}: {output:?}"
            );
        } else {
            assert!(output.stderr.is_empty(), "{check}: {output:?}");
        }
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn generic_numeric_cranelift() {
    generic_numeric(Backend::Cranelift);
}
#[test]
#[ignore = "explicit native integration target"]
fn generic_numeric_llvm() {
    generic_numeric(Backend::Llvm);
}
