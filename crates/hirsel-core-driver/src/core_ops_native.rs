// Source-level regressions for the missing core-operation bodies. Each source
// must traverse checking, instance collection, native emission, linking and the
// actual process runtime. Result values are hand-derived in the source tests.
fn core_operations(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cases = [
        ("channel FIFO and sender clone", "fn end(take tx: Sender[u64]) {}\nfn run() -> u64 waits { let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(2); let (tx,rx) = result.unwrap(take c); let other = channel.sender(tx); result.unwrap(channel.send(tx,7u64)); result.unwrap(channel.send(other,11u64)); let a = result.unwrap(channel.receive(rx)); let b = result.unwrap(channel.receive(rx)); end(take tx); end(take other); match channel.receive(rx) { Ok(_) => 0u64, Error(Closed(())) => a*100u64+b } }\npub fn main() -> Result[(), Exit] { task.check_cancel(); if run()==711u64 { Ok(()) } else { Error(Exit(status:1)) } }", 1_048_576, 0),
        ("channel Full and Closed keep input", "fn end(take rx: Receiver[u64]) {}\nfn run() -> u64 waits { let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(1); let (tx,rx) = result.unwrap(take c); result.unwrap(channel.try_send(tx,7u64)); let a = match channel.try_send(tx,11u64) { Ok(()) => 0u64, Error(Full(x)) => x, Error(Closed(_)) => 1u64 }; end(take rx); let b = match channel.try_send(tx,13u64) { Ok(()) => 0u64, Error(Full(_)) => 1u64, Error(Closed(x)) => x }; let z = match channel.send(tx,17u64) { Ok(()) => 0u64, Error(Closed(x)) => x }; a*10000u64+b*100u64+z }\npub fn main() -> Result[(), Exit] { if run()==111317u64 { Ok(()) } else { Error(Exit(status:1)) } }", 1_048_576, 0),
        // Main's 288 startup bytes leave 60. A two-u64 channel needs 80.
        ("channel Capacity", "pub fn main() -> Result[(), Exit] { let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(2); match take c { Ok(_) => Error(Exit(status:1)), Error(e) => if e.requested==80u64 && e.available==60u64 { Ok(()) } else { Error(Exit(status:2)) } } }", 348, 0),
        ("channel zero Bounds", "pub fn main() -> Result[(), Exit] { let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(0); let _ = take c; Ok(()) }", 1_048_576, 101),
        ("spawn scalar ABI", "fn child(take x:u64) -> u64 { x+1u64 }\npub fn main(tasks:Tasks) -> Result[(), Exit] { let h = result.unwrap(task.spawn(tasks,bytes 64,41u64,child)); if result.unwrap(task.join(take h))==42u64 { Ok(()) } else { Error(Exit(status:1)) } }", 1_048_576, 0),
        ("spawn unit ABI", "fn child(take x:()) {}\npub fn main(tasks:Tasks) -> Result[(), Exit] { let h = result.unwrap(task.spawn(tasks,bytes 0,(),child)); result.unwrap(task.join(take h)); Ok(()) }", 1_048_576, 0),
        ("spawn aggregate ABI", "type Pair: send { a:u64,b:u64 }\nfn child(take x:Pair) -> Pair { Pair(a:x.b,b:x.a) }\npub fn main(tasks:Tasks) -> Result[(), Exit] { let h = result.unwrap(task.spawn(tasks,bytes 64,Pair(a:7u64,b:11u64),child)); let p = result.unwrap(task.join(take h)); if p.a==11u64 && p.b==7u64 { Ok(()) } else { Error(Exit(status:1)) } }", 1_048_576, 0),
        // Main's 288 leaves 359. 256+32+8+64 = 360, input 41 comes back.
        ("spawn Capacity returns X", "fn child(take x:u64) -> u64 { x }\npub fn main(tasks:Tasks) -> Result[(), Exit] { let c = task.spawn(tasks,bytes 64,41u64,child); match take c { Ok(_) => Error(Exit(status:1)), Error((e,x)) => if e.requested==360u64 && e.available==359u64 && x==41u64 { Ok(()) } else { Error(Exit(status:2)) } } }", 647, 0),
        ("join Failed owns causes", "fn child(take x:u64) -> u64 { x+1u64 }\npub fn main(tasks:Tasks) -> Result[(), Exit] { let h = result.unwrap(task.spawn(tasks,bytes 64,18446744073709551615u64,child)); match task.join(take h) { Ok(_) => Error(Exit(status:1)), Error(Failed(take causes)) => { let _ = take causes; Ok(()) }, Error(Cancelled(_)) => Error(Exit(status:2)) } }", 1_048_576, 0),
        ("cancel observed at explicit check", "fn child(take x:u64) -> u64 { task.check_cancel(); x }\npub fn main(tasks:Tasks) -> Result[(), Exit] { let mut h = result.unwrap(task.spawn(tasks,bytes 64,41u64,child)); task.cancel(mut h); match task.join(take h) { Ok(_) => Error(Exit(status:1)), Error(Cancelled(take causes)) => { let _ = take causes; Ok(()) }, Error(Failed(_)) => Error(Exit(status:2)) } }", 1_048_576, 0),
    ];
    let mut failures = vec![];
    for (name, source, limit, exit) in cases {
        let dir = tempfile::tempdir().unwrap();
        let mut request = source_request(source, dir.path());
        request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let options = NativeOptions {
            backend, optimize:false, target:TARGET.into(),
            runtime:root.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native runtime archive")),
            include:root.join("crates/hirsel-core-rt/include"),
            cc:PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang"),
            store:dir.path().join("native"), output:dir.path().join("program"), root_limit:limit,
            instance_limit:64, directories:vec![], grant_mask:1<<9,
        };
        let built = match product.build(&entry, &options) {
            Ok(built) => built,
            Err(e) => { failures.push(format!("{name}: {e}")); continue; }
        };
        let outcome = Command::new(&built.executable).output().unwrap();
        if outcome.status.code() != Some(exit) || !outcome.stdout.is_empty()
            || (exit==0 && !outcome.stderr.is_empty())
            || (exit==101 && !outcome.stderr.starts_with(b"Bounds")) {
            failures.push(format!("{name}: expected exit {exit}, got {:?}, stdout {:?}, stderr {:?}", outcome.status.code(), outcome.stdout, String::from_utf8_lossy(&outcome.stderr)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "explicit native integration target"]
fn core_operations_cranelift() { core_operations(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn core_operations_llvm() { core_operations(Backend::Llvm); }

fn counted_operations(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut vectors = vec![
        ("fn run() -> u64 { box.unbox(result.unwrap(box.new(7u64))) }".to_owned(), 8, 7),
        ("fn run() -> u64 { box.unbox(result.unwrap(box.new(()))); 7u64 }".to_owned(), 0, 7),
        ("fn run() -> u64 { let r = box.new(7u64); match take r { Ok(_) => 0u64, Error((e,x)) => e.requested+e.available+x } }".to_owned(), 7, 22),
        ("fn run() -> u64 { let r = bound bytes 0 { box.new(7u64) }; match take r { Ok(Error((e,x))) => e.requested+x, _ => 0u64 } }".to_owned(), 1000, 15),
        ("resource type R:send { a:u64,b:u64 }\nfn run() -> u64 { let c:Result[(Sender[R],Receiver[R]),Capacity] = channel.new(1); match take c { Ok(_) => 0u64, Error(e) => e.requested+e.available } }".to_owned(), 71, 143),
        ("resource type R:send { a:u64,b:u64 }\nfn run() -> u64 { let r = result.unwrap(R(a:7u64,b:11u64)); let c = box.new(take r); match take c { Ok(_) => 0u64, Error((e,take back)) => e.requested*10000u64+e.available*100u64+back.a+back.b } }".to_owned(), 55, 80718),
    ];
    for (module, ty) in [("rc", "Rc"), ("arc", "Arc")] {
        vectors.push((format!("fn end(take h:{ty}[u64]) {{}}\nfn run() -> u64 {{ let h = result.unwrap({module}.new(7u64)); let other = {module}.share(h); let last = match {module}.try_unwrap(take h) {{ Ok(_) => return 0u64, Error(take back) => back }}; end(take other); result.unwrap({module}.try_unwrap(take last)) }}"),16,7));
        vectors.push((format!("type Data:shareable+send {{ a:u8,b:Box[u64] }}\nfn run() -> u64 {{ let b = result.unwrap(box.new(41u64)); let data = Data(a:7u8,b:take b); let r = {module}.new(take data); match take r {{ Ok(_) => 0u64, Error((e,take back)) => {{ let Data(a:_,b:take boxed) = take back; let v = box.unbox(take boxed); e.requested*10000u64+e.available*100u64+v }} }} }}"),31,242341));
        vectors.push((format!("fn run() -> u64 {{ let b = result.unwrap(box.new(41u64)); let h = result.unwrap({module}.new(take b)); let b = result.unwrap({module}.try_unwrap(take h)); box.unbox(take b) }}").replacen("let b = result.unwrap", "let first = result.unwrap", 1).replacen("new(take b)", "new(take first)", 1),24,41));
    }
    let mut failures = vec![];
    for (source, room, expected) in vectors {
        let source = format!("{source}\npub fn main() -> Result[(),Exit] {{ if run()=={expected}u64 {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}\n");
        let dir = tempfile::tempdir().unwrap();
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let options = NativeOptions { backend, optimize:false, target:TARGET.into(),
            runtime:root.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native runtime archive")),
            include:root.join("crates/hirsel-core-rt/include"),
            cc:PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang"),
            store:dir.path().join("native"), output:dir.path().join("program"), root_limit:288+room,
            instance_limit:64, directories:vec![], grant_mask:0 };
        let built = match product.build(&entry, &options) {
            Ok(built) => built,
            Err(e) => { failures.push(format!("{source}: {e}")); continue; }
        };
        let output = Command::new(&built.executable).output().unwrap();
        if !output.status.success() || !output.stdout.is_empty() || !output.stderr.is_empty() {
            failures.push(format!("{source}: exit {:?}, stderr {:?}", output.status.code(), String::from_utf8_lossy(&output.stderr)));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "explicit native integration target"]
fn counted_operations_cranelift() { counted_operations(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn counted_operations_llvm() { counted_operations(Backend::Llvm); }

fn args_operations(backend: Backend, len_only: bool) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (name, argv) in [("args-len", vec![]), ("args-empty", vec![]), ("args-values", vec!["alpha","beta",""])] {
        if (name=="args-len") != len_only { continue; }
        let dir = tempfile::tempdir().unwrap();
        let source = std::fs::read_to_string(root.join(format!("core/front/runtime/{name}.hirsel"))).unwrap();
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let options = NativeOptions { backend, optimize:false, target:TARGET.into(),
            runtime:root.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native runtime archive")),
            include:root.join("crates/hirsel-core-rt/include"),
            cc:PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang"),
            store:dir.path().join("native"), output:dir.path().join("program"), root_limit:1_048_576,
            instance_limit:64, directories:vec![], grant_mask:0 };
        let built = product.build(&entry, &options).unwrap();
        let result = Command::new(built.executable).args(argv).output().unwrap();
        assert_eq!(result.status.code(),Some(0),"{name}: {:?}",String::from_utf8_lossy(&result.stderr));
        assert!(result.stdout.is_empty() && result.stderr.is_empty(),"{name}");
    }
}

#[test]
#[ignore = "explicit native provider target, after args exports and engine rows land"]
fn args_len_cranelift() { args_operations(Backend::Cranelift,true); }
#[test]
#[ignore = "explicit native provider target, after args exports and engine rows land"]
fn args_len_llvm() { args_operations(Backend::Llvm,true); }
#[test]
#[ignore = "explicit native provider target, after args exports and engine rows land"]
fn args_values_cranelift() { args_operations(Backend::Cranelift,false); }
#[test]
#[ignore = "explicit native provider target, after args exports and engine rows land"]
fn args_values_llvm() { args_operations(Backend::Llvm,false); }

fn stream_operations(backend: Backend) {
    use std::os::{fd::OwnedFd, unix::net::UnixStream};
    use std::process::Stdio;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (name, argv, stdout, stderr, closed) in [
        ("streams", vec![], "out", "err", 0),
        ("args-print", vec!["alpha", "beta", ""], "alphabeta", "", 0),
        ("stdout-closed", vec![], "", "", 1),
        ("stderr-closed", vec![], "", "", 2),
        ("stdout-cancel", vec![], "", "", 0),
        ("stderr-cancel", vec![], "", "", 0),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let source = std::fs::read_to_string(root.join(format!("core/front/runtime/{name}.hirsel"))).unwrap();
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let options = NativeOptions { backend, optimize:false, target:TARGET.into(),
            runtime:root.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native runtime archive")),
            include:root.join("crates/hirsel-core-rt/include"),
            cc:PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang"),
            store:dir.path().join("native"), output:dir.path().join("program"), root_limit:1_048_576,
            instance_limit:64, directories:vec![], grant_mask:(1<<4) | (1<<5) | (1<<9) };
        let built = product.build(&entry, &options).unwrap();
        let mut command = Command::new(built.executable);
        command.args(argv);
        if closed != 0 {
            // Close before exec, so EPIPE does not depend on process scheduling.
            let (writer, reader) = UnixStream::pair().unwrap();
            drop(reader);
            let fd: OwnedFd = writer.into();
            if closed == 1 { command.stdout(Stdio::from(fd)); }
            else { command.stderr(Stdio::from(fd)); }
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(0), "{name}: {:?}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, stdout.as_bytes(), "{name}");
        assert_eq!(output.stderr, stderr.as_bytes(), "{name}");
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn stream_operations_cranelift() { stream_operations(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn stream_operations_llvm() { stream_operations(Backend::Llvm); }

fn host_operations(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (name, expected_exit) in [("host-read",0), ("host",0), ("host-close-cancel",0), ("clocks",0), ("mkdtemp-bounds",101), ("mkdtemp",0), ("host-wait-cancel",0)] {
        let dir = tempfile::tempdir().unwrap();
        let grant = dir.path().join("grant");
        std::fs::create_dir(&grant).unwrap();
        if name=="host-read" || name=="host-close-cancel" || name=="host-wait-cancel" { std::fs::write(grant.join("input"), b"hello").unwrap(); }
        let source = std::fs::read_to_string(root.join(format!("core/front/runtime/{name}.hirsel"))).unwrap();
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        if name == "mkdtemp" || name == "host-wait-cancel" {
            let module = request.inputs.packages[1].sources.iter_mut().find(|s| s.modules==["string"]).unwrap();
            let mut text = std::fs::read_to_string(&module.file).unwrap();
            text.push_str("\npub fn proof_temp_name() -> String { let b:Bytes = result.unwrap(list.with_capacity(32)); of_bytes(take b) }\n");
            module.file = dir.path().join("string.hirsel");
            std::fs::write(&module.file,text).unwrap();
        }
        let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let options = NativeOptions { backend, optimize:false, target:TARGET.into(),
            runtime:root.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native runtime archive")),
            include:root.join("crates/hirsel-core-rt/include"),
            cc:PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang"),
            store:dir.path().join("native"), output:dir.path().join("program"), root_limit:1_048_576,
            instance_limit:64, directories:vec![grant.to_str().unwrap().into()], grant_mask:(1<<2)|(1<<9) };
        let built = product.build(&entry, &options).unwrap();
        let output = Command::new(built.executable).output().unwrap();
        assert_eq!(output.status.code(),Some(expected_exit),"{name}: {:?}",String::from_utf8_lossy(&output.stderr));
        assert!(output.stdout.is_empty(),"{name}");
        if expected_exit==0 { assert!(output.stderr.is_empty(),"{name}: {:?}",String::from_utf8_lossy(&output.stderr)); }
        else { assert!(output.stderr.starts_with(b"Bounds"),"{name}: {:?}",String::from_utf8_lossy(&output.stderr)); }
        if name=="host-read" || name=="host-close-cancel" || name=="host-wait-cancel" { assert_eq!(std::fs::read(grant.join("input")).unwrap(),b"hello"); }
        else { assert_eq!(std::fs::read_dir(&grant).unwrap().count(),0,"{name}"); }
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn host_operations_cranelift() { host_operations(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn host_operations_llvm() { host_operations(Backend::Llvm); }

fn string_carrier_operations(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = r#"pub fn main() -> Result[(),Exit] {
        let empty = string.new()
        if string.byte_len(empty)!=0u64 { return Error(Exit(status:1)) }
        let borrowed = string.proof_view(b"abc")
        if string.byte_len(borrowed)!=3u64 { return Error(Exit(status:2)) }
        let bs = string.as_bytes(borrowed)
        if list.len(bs)!=3u64 || bs[1]!=b'b' { return Error(Exit(status:3)) }
        if string.proof_moves()!=4u64 { return Error(Exit(status:4)) }
        let a = unique.new()
        let b = unique.new()
        if a==b { return Error(Exit(status:5)) }
        Ok(())
    }"#;
    let mut request = source_request(source,dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let module = request.inputs.packages[1].sources.iter_mut().find(|s| s.modules==["string"]).unwrap();
    let mut text = std::fs::read_to_string(&module.file).unwrap();
    // Test-package bodies call all three private kernels with valid empty/ASCII bytes.
    text.push_str(r#"
pub fn proof_view(b:Bytes) -> View[String] in b { view_string(b) }
pub fn proof_moves() -> u64 {
    let b:Bytes = result.unwrap(list.with_capacity(4))
    let s = of_bytes(take b)
    if string.byte_len(s)!=0u64 { return 0u64 }
    let back = to_bytes(take s)
    list.capacity(back)
}
"#);
    module.file = dir.path().join("string.hirsel");
    std::fs::write(&module.file,text).unwrap();
    let product = Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(backend,dir.path());
    options.root_limit = 296; // 288 main + round8(4); the carrier moves admit nothing.
    let built = product.build(&entry,&options).unwrap_or_else(|error| panic!("{error:?}\n{}",hirsel_core_ir::text::print(&product.demand(&entry).unwrap().ir)));
    run_clean(&built.executable,0);
}
#[test]
#[ignore = "explicit native integration target"]
fn string_carrier_operations_cranelift() { string_carrier_operations(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn string_carrier_operations_llvm() { string_carrier_operations(Backend::Llvm); }

fn bodied_core_operations(backend: Backend) {
    let dir = tempfile::tempdir().unwrap();
    let source = "pub fn main(a:Args) -> Result[(),Exit] { if args.echo(args.len(a))==42u64 && args.echo(7u8)==7u8 { Ok(()) } else { Error(Exit(status:1)) } }";
    let mut request = source_request(source,dir.path());
    let file = dir.path().join("args.hirsel");
    std::fs::write(&file,"pub fn len(a:Args) -> u64 { 42u64 }\npub fn echo[T:plain](take x:T) -> T { x }\n").unwrap();
    request.inputs.packages[1].sources.push(hirsel_core_resolve::ModuleSource { file, modules:vec!["args".into()], signatures:true });
    let product = Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let options = native_options(backend,dir.path());
    let built = product.build(&entry,&options).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore = "explicit native integration target"]
fn bodied_core_operations_cranelift() { bodied_core_operations(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn bodied_core_operations_llvm() { bodied_core_operations(Backend::Llvm); }
fn causes_iteration_kind(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/causes-kind.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let mut options = native_options(backend, dir.path());
    options.grant_mask = 1 << 9;
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &options).unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn causes_iteration_kind_cranelift() { causes_iteration_kind(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn causes_iteration_kind_llvm() { causes_iteration_kind(Backend::Llvm); }

fn stream_partial_progress(backend: Backend) {
    use std::io::Read;
    use std::os::{fd::OwnedFd,unix::net::UnixStream};
    use std::process::Stdio;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    // Larger than a Unix stream's send buffer. The reader observes a byte and
    // closes, so the writer must report both positive progress and EPIPE.
    let payload = "x".repeat(1_048_576);
    let source = format!("pub fn main(out:Stdout) -> Result[(),Exit] {{ match stdout.write(out,\"{payload}\") {{ Ok(()) => Error(Exit(status:1)), Error(e) => if e.written==0u64 || e.written>=1048576u64 {{ Error(Exit(status:2)) }} else {{ match e.error {{ Other(32u32) => Ok(()), _ => Error(Exit(status:3)) }} }} }} }}");
    let mut request = source_request(&source,dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product = Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(backend,dir.path());
    options.grant_mask = 1<<4;
    options.root_limit = 288;
    let built = product.build(&entry,&options).unwrap();
    let (writer,mut reader) = UnixStream::pair().unwrap();
    let reading = std::thread::spawn(move || {
        let mut byte = [0];
        assert_eq!(reader.read(&mut byte).unwrap(),1);
        assert_eq!(byte,[b'x']);
    });
    let fd:OwnedFd = writer.into();
    let output = Command::new(built.executable).stdout(Stdio::from(fd)).output().unwrap();
    reading.join().unwrap();
    assert_eq!(output.status.code(),Some(0),"{:?}",String::from_utf8_lossy(&output.stderr));
    assert!(output.stderr.is_empty());
}
#[test]
#[ignore = "explicit native integration target"]
fn stream_partial_progress_cranelift() { stream_partial_progress(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn stream_partial_progress_llvm() { stream_partial_progress(Backend::Llvm); }
