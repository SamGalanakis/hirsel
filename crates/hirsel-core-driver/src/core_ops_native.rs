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
            logical_charge_report: None,
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
            instance_limit:64, directories:vec![], grant_mask:0, logical_charge_report: None };
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
            instance_limit:64, directories:vec![], grant_mask:0, logical_charge_report: None };
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
            instance_limit:64, directories:vec![], grant_mask:(1<<4) | (1<<5) | (1<<9), logical_charge_report: None };
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
    crate::openat2_required::require();
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
            instance_limit:64, directories:vec![grant.to_str().unwrap().into()], grant_mask:(1<<2)|(1<<9), logical_charge_report: None };
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

// MEM-17 through the package product: a core type's clone body is an item with its
// own definition (`<module>/<Type>.$clone`), so reuse keys, scheduling and native
// emission reach it. The body bumps `n`, so 522 (n 4 -> 5, both lists length 2) holds
// only if the declared body ran rather than a structural copy, and the driver panicked
// on the item before it had an identity.
fn clone_body_operations(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = "pub fn main() -> Result[(),Exit] { if list.proof_clone_run()==522u64 { Ok(()) } else { Error(Exit(status:1)) } }\n";
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let module = request.inputs.packages[1].sources.iter_mut().find(|s| s.modules==["list"]).unwrap();
    let mut text = std::fs::read_to_string(&module.file).unwrap();
    text.push_str(r#"
pub type ProofCopy[T] { xs: List[T], n: u64 }
clone[T](p: ProofCopy[T]) { Ok(ProofCopy(xs: clone(p.xs)?, n: p.n + 1)) }
pub fn proof_clone_run() -> u64 {
    let made: Result[List[u64], Capacity] = with_capacity(2)
    let mut xs: List[u64] = match take made { Ok(take l) => take l, Error(_) => return 0 }
    match push(mut xs, 7) { Ok(()) => {}, Error(_) => return 0 }
    match push(mut xs, 9) { Ok(()) => {}, Error(_) => return 0 }
    let p = ProofCopy(xs: take xs, n: 4)
    let c: Result[ProofCopy[u64], Capacity] = clone(p)
    match take c { Ok(take q) => q.n * 100 + len(q.xs) * 10 + len(p.xs), Error(_) => 1 }
}
"#);
    module.file = dir.path().join("list.hirsel");
    std::fs::write(&module.file, text).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let options = NativeOptions { backend, optimize:false, target:TARGET.into(),
        runtime:root.join(std::env::var_os("HIRSEL_CORE_RT_ARCHIVE").expect("native runtime archive")),
        include:root.join("crates/hirsel-core-rt/include"),
        cc:PathBuf::from(std::env::var_os("LLVM_SYS_231_PREFIX").unwrap()).join("bin/clang"),
        store:dir.path().join("native"), output:dir.path().join("program"), root_limit:1_048_576,
        instance_limit:64, directories:vec![], grant_mask:0, logical_charge_report: None };
    let built = product.build(&entry, &options).unwrap();
    let output = Command::new(built.executable).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", String::from_utf8_lossy(&output.stderr));
}
#[test]
#[ignore = "explicit native integration target"]
fn clone_body_cranelift() { clone_body_operations(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn clone_body_llvm() { clone_body_operations(Backend::Llvm); }

fn string_carrier_operations(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = r#"scoped type BytesHolder { value:View[Bytes] }
fn stored(b:Bytes)->BytesHolder in b { BytesHolder(value:string.as_bytes(string.proof_view(b))) }
fn forwarded(b:Bytes)->BytesHolder in b { stored(b) }
pub fn main() -> Result[(),Exit] {
        let empty = string.new()
        if string.byte_len(empty)!=0u64 { return Error(Exit(status:1)) }
        let borrowed = string.proof_view(string.as_bytes("abc"))
        if string.byte_len(borrowed)!=3u64 { return Error(Exit(status:2)) }
        let bs = string.as_bytes(borrowed)
        if list.len(bs)!=3u64 || bs[1]!=b'b' { return Error(Exit(status:3)) }
        if string.proof_moves()!=4u64 { return Error(Exit(status:4)) }
        let buffer:Bytes = result.unwrap(list.with_capacity(4))
        let holder = forwarded(buffer)
        if list.capacity(holder.value)!=4u64 || list.len(holder.value)!=0u64 { return Error(Exit(status:6)) }
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
fn aggregate_constants(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/constant-aggregates.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let options = native_options(backend, dir.path());
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &options).unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn aggregate_constants_cranelift() { aggregate_constants(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn aggregate_constants_llvm() { aggregate_constants(Backend::Llvm); }

fn packed_private_arrays(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/span-private-array.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &native_options(backend, dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore = "explicit native integration target"]
fn packed_private_arrays_cranelift() { packed_private_arrays(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn packed_private_arrays_llvm() { packed_private_arrays(Backend::Llvm); }

fn packed_escaping(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/packed-escaping.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &native_options(backend, dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore = "explicit native integration target"]
fn packed_escaping_cranelift() { packed_escaping(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn packed_escaping_llvm() { packed_escaping(Backend::Llvm); }

fn packed_private_variants(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/span-private-variant.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &native_options(backend, dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore = "explicit native integration target"]
fn packed_private_variants_cranelift() { packed_private_variants(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn packed_private_variants_llvm() { packed_private_variants(Backend::Llvm); }
fn generic_runtime_operations(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let make = "fn make[T:send](n:u64) -> Result[(Sender[T],Receiver[T]),Capacity] { channel.new(n) }";
    let launch = "fn launch[X:send,R:send](tasks:Tasks,bytes n:u64,take x:X,f:fn(take X)->R) -> Result[Task[R],(Capacity,X)] { task.spawn(tasks,bytes n,take x,f) }";
    let vectors = [
        (format!("{make}\npub fn main()->Result[(),Exit] {{ let c:Result[(Sender[()],Receiver[()]),Capacity]=make(1); let _=result.unwrap(take c); Ok(()) }}"),64,0),
        // P is logically 9 bytes and physically 16.
        (format!("{make}\ntype P:send {{a:u8,b:u64}}\npub fn main()->Result[(),Exit] {{ let c:Result[(Sender[P],Receiver[P]),Capacity]=make(2); match take c {{ Ok(_) => Error(Exit(status:1)), Error(e) => if e.requested==82u64 && e.available==81u64 {{ Ok(()) }} else {{ Error(Exit(status:2)) }} }} }}"),81,0),
        // A resource holder is 8, independent of its 16-byte resource block.
        (format!("{make}\nresource type R:send {{a:u64,b:u64}}\npub fn main()->Result[(),Exit] {{ let c:Result[(Sender[R],Receiver[R]),Capacity]=make(1); match take c {{ Ok(_) => Error(Exit(status:1)), Error(e) => if e.requested==72u64 && e.available==71u64 {{ Ok(()) }} else {{ Error(Exit(status:2)) }} }} }}"),71,0),
        (format!("{make}\npub fn main()->Result[(),Exit] {{ let c:Result[(Sender[u64],Receiver[u64]),Capacity]=make(18446744073709551615u64); match take c {{ Ok(_) => Error(Exit(status:1)), Error(e) => if e.requested==18446744073709551615u64 && e.available==1000u64 {{ Ok(()) }} else {{ Error(Exit(status:2)) }} }} }}"),1000,0),
        ("type P:send {a:u8,b:u64}\nfn transfer[T:send](take x:T)->T waits { let c:Result[(Sender[T],Receiver[T]),Capacity]=channel.new(1); let (tx,rx)=result.unwrap(take c); result.unwrap(channel.send(tx,take x)); result.unwrap(channel.receive(rx)) }\npub fn main()->Result[(),Exit] { let p=transfer(P(a:7u8,b:11u64)); if p.a==7u8 && p.b==11u64 { Ok(()) } else { Error(Exit(status:1)) } }".to_owned(),1000,0),
        ("resource type R:send {a:u64,b:u64}\nfn transfer[T:send](take x:T)->T waits { let c:Result[(Sender[T],Receiver[T]),Capacity]=channel.new(1); let (tx,rx)=result.unwrap(take c); result.unwrap(channel.send(tx,take x)); result.unwrap(channel.receive(rx)) }\npub fn main()->Result[(),Exit] { let r=result.unwrap(R(a:7u64,b:11u64)); let p=transfer(take r); if p.a==7u64 && p.b==11u64 { Ok(()) } else { Error(Exit(status:1)) } }".to_owned(),1000,0),
        (format!("{launch}\ntype P:send {{a:u8,b:u64}}\nfn child(take x:u64)->P {{P(a:7u8,b:x)}}\npub fn main(tasks:Tasks)->Result[(),Exit] {{ let c=launch(tasks,bytes 64,41u64,child); match take c {{ Ok(_) => Error(Exit(status:1)), Error((e,x)) => if e.requested==361u64 && e.available==360u64 && x==41u64 {{ Ok(()) }} else {{ Error(Exit(status:2)) }} }} }}"),360,1<<9),
        (format!("{launch}\ntype P:send {{a:u8,b:u64}}\nfn child(take x:u64)->P {{P(a:7u8,b:x)}}\npub fn main(tasks:Tasks)->Result[(),Exit] {{ let h=result.unwrap(launch(tasks,bytes 64,41u64,child)); let p=result.unwrap(task.join(take h)); if p.a==7u8 && p.b==41u64 {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}"),1000,1<<9),
    ];
    let mut failures=vec![];
    for (source,room,grants) in vectors {
        let dir=tempfile::tempdir().unwrap();
        let mut request=source_request(&source,dir.path());
        request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
        let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
        let entry=DefPath::from_full("app@1/main/main").unwrap();
        let mut options=native_options(backend,dir.path());
        options.root_limit=288+room;
        options.grant_mask=grants;
        let built=match product.build(&entry,&options) { Ok(built)=>built, Err(error)=>{failures.push(format!("{source}: {error}"));continue;} };
        let output=Command::new(built.executable).output().unwrap();
        if output.status.code()!=Some(0) || !output.stdout.is_empty() || !output.stderr.is_empty() {
            failures.push(format!("{source}: exit {:?}, stderr {:?}",output.status.code(),String::from_utf8_lossy(&output.stderr)));
        }
    }
    assert!(failures.is_empty(),"{}",failures.join("\n"));
}
#[test]
#[ignore = "explicit native integration target"]
fn generic_runtime_operations_cranelift() { generic_runtime_operations(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn generic_runtime_operations_llvm() { generic_runtime_operations(Backend::Llvm); }

fn generic_counted_operations(backend: Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let boxing="fn boxed[T](take x:T)->Result[Box[T],(Capacity,T)] {box.new(take x)}\nfn unboxed[T](take x:Box[T])->T {box.unbox(take x)}";
    let mut vectors=vec![
        (format!("{boxing}\nfn run()->u64 {{unboxed(result.unwrap(boxed(())));7u64}}"),0,7),
        (format!("{boxing}\nfn run()->u64 {{let s=unboxed(result.unwrap(boxed(\"abc\")));string.byte_len(s)}}"),24,3),
        (format!("{boxing}\ntype P {{a:u8,b:u32,c:u8}}\nfn run()->u64 {{ let c=boxed(P(a:7u8,b:11u32,c:13u8));match take c {{Ok(_) => 0u64,Error((e,p)) => e.requested*10000u64+e.available*100u64+int.widen(p.a)+int.widen(p.b)+int.widen(p.c)}} }}"),7,80731),
        (format!("{boxing}\nresource type R:send {{a:u64,b:u64}}\nfn run()->u64 {{ let r=result.unwrap(R(a:7u64,b:11u64));let c=boxed(take r);match take c {{Ok(_) => 0u64,Error((e,take back)) => e.requested*10000u64+e.available*100u64+back.a+back.b}} }}"),55,80718),
    ];
    for (module,ty,bounds) in [("rc","Rc","shareable"),("arc","Arc","shareable+send")] {
        let helpers=format!("fn new_owner[T:{bounds}](take x:T)->Result[{ty}[T],(Capacity,T)] {{{module}.new(take x)}}\nfn share_owner[T:{bounds}](x:{ty}[T])->{ty}[T] {{{module}.share(x)}}\nfn unwrap_owner[T:{bounds}](take x:{ty}[T])->Result[T,{ty}[T]] {{{module}.try_unwrap(take x)}}\nfn end[T:{bounds}](take x:{ty}[T]) {{}}");
        vectors.push((format!("{helpers}\ntype P:shareable+send {{a:u8,b:u32,c:u8}}\nfn run()->u64 {{ let h=result.unwrap(new_owner(P(a:7u8,b:11u32,c:13u8)));let other=share_owner(h);let h2=match unwrap_owner(take h) {{Ok(_) => return 0u64,Error(take back) => back}};end(take other);let p=result.unwrap(unwrap_owner(take h2));let va:u64=int.widen(p.a);let vb:u64=int.widen(p.b);let vc:u64=int.widen(p.c);va+vb+vc }}"),16,31));
        vectors.push((format!("{helpers}\ntype P:shareable+send {{a:u8,b:Box[u64]}}\nfn run()->u64 {{ let b=result.unwrap(box.new(41u64));let c=new_owner(P(a:7u8,b:take b));match take c {{Ok(_) => 0u64,Error((e,take back)) => {{let P(a:_,b:take b2)=take back;e.requested*10000u64+e.available*100u64+box.unbox(take b2)}}}} }}"),31,242341));
    }
    let mut failures=vec![];
    for (body,room,expected) in vectors {
        let source=format!("{body}\npub fn main()->Result[(),Exit] {{if run()=={expected}u64 {{Ok(())}} else {{Error(Exit(status:1))}}}}");
        let dir=tempfile::tempdir().unwrap();
        let mut request=source_request(&source,dir.path());
        request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
        let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
        let entry=DefPath::from_full("app@1/main/main").unwrap();
        let mut options=native_options(backend,dir.path());
        options.root_limit=288+room;
        let built=match product.build(&entry,&options) {Ok(built)=>built,Err(error)=>{failures.push(format!("{source}: {error}"));continue;}};
        let output=Command::new(built.executable).output().unwrap();
        if output.status.code()!=Some(0) || !output.stdout.is_empty() || !output.stderr.is_empty() {
            failures.push(format!("{source}: exit {:?}, stderr {:?}",output.status.code(),String::from_utf8_lossy(&output.stderr)));
        }
    }
    assert!(failures.is_empty(),"{}",failures.join("\n"));
}
#[test]
#[ignore = "explicit native integration target"]
fn generic_counted_operations_cranelift() {generic_counted_operations(Backend::Cranelift);}
#[test]
#[ignore = "explicit native integration target"]
fn generic_counted_operations_llvm() {generic_counted_operations(Backend::Llvm);}

fn returned_argument_views(backend:Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let source=std::fs::read_to_string(root.join("core/front/runtime/args-returned-views.hirsel")).unwrap();
    let mut request=source_request(&source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let mut options=native_options(backend,dir.path());
    options.root_limit=288;
    options.grant_mask=1<<4;
    let built=product.build(&entry,&options).unwrap();
    let output=Command::new(built.executable).args(["alpha","beta",""]).output().unwrap();
    assert_eq!(output.status.code(),Some(0),"{:?}",String::from_utf8_lossy(&output.stderr));
    assert_eq!(output.stdout,b"alphabeta");
    assert!(output.stderr.is_empty());
}
#[test]
#[ignore = "explicit native integration target"]
fn returned_argument_views_cranelift(){returned_argument_views(Backend::Cranelift);}
#[test]
#[ignore = "explicit native integration target"]
fn returned_argument_views_llvm(){returned_argument_views(Backend::Llvm);}

fn string_growth_operations(backend:Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (name,room) in [("string-growth",16),("string-growth-refusal",0)] {
        let dir=tempfile::tempdir().unwrap();
        let source=std::fs::read_to_string(root.join(format!("core/front/runtime/{name}.hirsel"))).unwrap();
        let mut request=source_request(&source,dir.path());
        request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
        let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
        let entry=DefPath::from_full("app@1/main/main").unwrap();
        let mut options=native_options(backend,dir.path());
        options.root_limit=288+room;
        let built=product.build(&entry,&options).unwrap();
        let output=Command::new(built.executable).output().unwrap();
        assert_eq!(output.status.code(),Some(0),"{name}: {:?}",String::from_utf8_lossy(&output.stderr));
        assert!(output.stdout.is_empty() && output.stderr.is_empty(),"{name}");
    }
}
#[test]
#[ignore = "explicit native integration target"]
fn string_growth_operations_cranelift(){string_growth_operations(Backend::Cranelift);}
#[test]
#[ignore = "explicit native integration target"]
fn string_growth_operations_llvm(){string_growth_operations(Backend::Llvm);}

fn string_shorten_operations(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = std::fs::read_to_string(root.join("core/front/runtime/string-shorten.hirsel")).unwrap();
    let mut request = source_request(&source, dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let mut options = native_options(backend, dir.path());
    options.root_limit = 296;
    let built = product.build(&entry, &options).unwrap();
    let output = Command::new(built.executable).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{:?}", String::from_utf8_lossy(&output.stderr));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}
#[test]
#[ignore = "explicit native integration target"]
fn string_shorten_operations_cranelift() { string_shorten_operations(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn string_shorten_operations_llvm() { string_shorten_operations(Backend::Llvm); }

// A declared finalizer grant reaches the native row: under a grant of 0 the finalizers'
// allocations fail with Capacity and the program does not exit cleanly.
fn resource_grant(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = include_str!("../../../core/front/cases/body/resource-grant.hirsel");
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &native_options(backend, dir.path())).unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn resource_grant_cranelift() { resource_grant(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn resource_grant_llvm() { resource_grant(Backend::Llvm); }
fn ordered_generic(backend: Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let source=include_str!("../../../core/front/cases/body/ordered-generic.hirsel");
    let mut request=source_request(source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore="explicit native integration target"]
fn ordered_generic_cranelift(){ordered_generic(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn ordered_generic_llvm(){ordered_generic(Backend::Llvm);}

// map.get's Option[View[V]] row is made by substitution in the instance's own module.
// With the rigid V placeholder left in its view field, the instance copied 24 bytes into
// the caller's 8-byte slot: a segfault on Cranelift, a missed key under LLVM (M1 json).
fn generic_view_payload(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let source = r#"fn grown(src: String, lo: u64, hi: u64) -> String {
  let mut out = string.new()
  result.unwrap(string.push(mut out, string.slice(src, lo, hi)))
  out
}
fn probe(path: String, doc: String) -> u64 {
  let mut m: Map[String, u64] = map.new()
  let _ = map.insert(mut m, grown(doc, 2, 7), 5u64)
  let _ = map.insert(mut m, grown(doc, 9, 10), 6u64)
  let key = string.slice(path, 1, 6)
  match map.get(m, key) { Some(v) => v, None => 99 }
}
pub fn main() -> Result[(), Exit] {
  let p = result.unwrap(string.from("/items/1/name"))
  let d = result.unwrap(string.from("{\"items\":\"n\"}"))
  if probe(p, d) == 5 { Ok(()) } else { Error(Exit(status: 3)) }
}
"#;
    let mut request = source_request(source, dir.path());
    request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &native_options(backend, dir.path())).unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn generic_view_payload_cranelift() { generic_view_payload(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn generic_view_payload_llvm() { generic_view_payload(Backend::Llvm); }

// std.json.write_value[in r] reaches itself through a closure; erasing its region gives
// that call row the instance's own key, and materialization added the body as a second
// row of the key: two definitions of one link name, which LLVM rejects (M1 json matched).
fn region_recursive_instance(backend: Backend) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(
        app.join("main.hirsel"),
        r#"fn arr(r: Region, s: String) -> bool {
    match std.json.parse(r, s) {
        Ok(v) => match v { Arr(_) => true, _ => false },
        Error(_) => false,
    }
}
pub fn main() -> Result[(), Exit] {
    let s = result.unwrap(string.from("[null, [true]]"))
    region doc {
        if arr(doc, s) { Ok(()) } else { Error(Exit(status: 3)) }
    }
}
"#,
    )
    .unwrap();
    let request = CheckRequest::directory(&app, &root.join("core"), &dir.path().join("store")).unwrap();
    let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
    let entry = DefPath::from_full("app@1/main/main").unwrap();
    let built = product.build(&entry, &native_options(backend, dir.path())).unwrap();
    run_clean(&built.executable, 0);
}
#[test]
#[ignore = "explicit native integration target"]
fn region_recursive_instance_cranelift() { region_recursive_instance(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn region_recursive_instance_llvm() { region_recursive_instance(Backend::Llvm); }
fn map_update_value(backend: Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let source=concat!(include_str!("../../../core/front/cases/body/map-update.hirsel"), r#"
resource type Trace {out:Stdout,id:u64} drop(t) {let _=stdout.write(t.out,"D")}
fn clean_once(out:Stdout)->u64 {
 let mut m:Map[u64,Trace]=map.new()
 result.unwrap(map.insert(mut m,1u64,result.unwrap(Trace(out:out,id:8))))
 option.unwrap(map.update(mut m,1u64,fn(mut value:Trace)->u64 {value.id+=1;value.id}))
}
pub fn main(out:Stdout)->Result[(),Exit] waits {
 if counter()==17 && owners()==9 && clean_once(out)==9 {Ok(())}else{Error(Exit(status:1))}
}
"#);
    let mut request=source_request(source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let mut options=native_options(backend,dir.path());
    options.grant_mask=1<<grant_kind("Stdout").unwrap();
    let built=product.build(&entry,&options).unwrap();
    let output=Command::new(&built.executable).output().unwrap();
    assert_eq!(output.status.code(),Some(0),"{:?}",output.stderr);
    assert_eq!(output.stdout,b"D");
    assert!(output.stderr.is_empty());
}
#[test]
#[ignore="explicit native integration target"]
fn map_update_value_cranelift(){map_update_value(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn map_update_value_llvm(){map_update_value(Backend::Llvm);}


fn ordered_stable(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/ordered-stable.hirsel"));
}
/// FORM #65: compact values cross generic order and equality by their value row.
fn ordered_compact(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/ordered-compact.hirsel"));
}
#[test]
#[ignore="explicit native integration target"]
fn ordered_compact_cranelift(){ordered_compact(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn ordered_compact_llvm(){ordered_compact(Backend::Llvm);}
fn ordered_case(backend: Backend, source: &str) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let mut request=source_request(source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore="explicit native integration target"]
fn ordered_stable_cranelift(){ordered_stable(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn ordered_stable_llvm(){ordered_stable(Backend::Llvm);}
/// MEM-2/MEM-5: a placed stable value is held as a pointer to its region block, inline in a
/// Result payload, a record field and a list element alike; the values are the case's run lines.
fn stable_placement(backend: Backend) {
    let source = format!(
        "{}\npub fn main() -> Result[(), Exit] {{ if sum() == 15 && picked() == 7 && nested() == 12 && refused() == 16 && listed() == 41 && widened() == 64 {{ Ok(()) }} else {{ Error(Exit(status: 1)) }} }}\n",
        include_str!("../../../core/front/cases/body/stable-placement.hirsel")
    );
    ordered_case(backend, &source);
}
#[test]
#[ignore = "explicit native integration target"]
fn stable_placement_cranelift() { stable_placement(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn stable_placement_llvm() { stable_placement(Backend::Llvm); }
/// Run a front case natively: `main` checks each `// expect: run f(args) = v` line of the
/// case, so both engines meet the case's own hand-derived values; exit status i + 1 names
/// the first line that differs.
/// The case's run lines (and `// native: run` lines, for what the evaluator does not
/// run) on a native engine: the returning ones in one program, and each
/// `fail Bounds` in its own, which must stop with exit 101 and `Bounds` on stderr.
fn run_lines_native(backend: Backend, case: &str) {
    let mut main = String::from("pub fn main() -> Result[(), Exit] {\n");
    let runs = case.lines().filter_map(|l| {
        l.strip_prefix("// expect: run ").or_else(|| l.strip_prefix("// native: run "))
    });
    for (i, line) in runs.enumerate() {
        let (call, want) = line.split_once(" = ").expect("a run line");
        if want == "fail Bounds" {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let dir = tempfile::tempdir().unwrap();
            let source = format!("{case}\npub fn main() -> Result[(), Exit] {{ let _ = {call}; Ok(()) }}\n");
            let mut request = source_request(&source, dir.path());
            request.inputs.packages[1] = PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
            let product = Driver::default().check(request, &mut Meter::new(false)).unwrap();
            let entry = DefPath::from_full("app@1/main/main").unwrap();
            let built = product.build(&entry, &native_options(backend, dir.path())).unwrap();
            let output = Command::new(&built.executable).output().unwrap();
            assert_eq!(output.status.code(), Some(101), "{call}: {output:?}");
            assert!(output.stderr.starts_with(b"Bounds"), "{call}: {output:?}");
            continue;
        }
        main += &format!("  if {call} != {want} {{ return Error(Exit(status: {})) }}\n", i + 1);
    }
    main += "  Ok(())\n}\n";
    ordered_case(backend, &format!("{case}\n{main}"));
}
/// core.md "Floats": the unary float operations, bit for bit, on each engine.
fn float_unary(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/body/float-unary.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn float_unary_cranelift() { float_unary(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn float_unary_llvm() { float_unary(Backend::Llvm); }
/// MEM-4: build_in and dest.put, a put that overflows and a foreign child, on each engine.
fn dest_put(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/body/dest-put.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn dest_put_cranelift() { dest_put(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn dest_put_llvm() { dest_put(Backend::Llvm); }
/// MEM-17, T1.5: own clone and its refusal's drop and refund, on each engine.
fn clone_own(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/body/clone-own.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn clone_own_cranelift() { clone_own(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn clone_own_llvm() { clone_own(Backend::Llvm); }
/// MEM-17: clone shares counted edges, on each engine.
fn clone_counted(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/body/clone-counted.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn clone_counted_cranelift() { clone_counted(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn clone_counted_llvm() { clone_counted(Backend::Llvm); }
/// MEM-17: clone_in makes stable twins in a region, on each engine.
fn clone_in(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/body/clone-in.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn clone_in_cranelift() { clone_in(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn clone_in_llvm() { clone_in(Backend::Llvm); }
/// MEM-17: clone copies a stable value into its own twin, on each engine.
fn clone_twin(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/body/clone-twin.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn clone_twin_cranelift() { clone_twin(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn clone_twin_llvm() { clone_twin(Backend::Llvm); }
/// MEM-10: an optional link field is put and read, on each engine.
fn compact_option_link(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/body/compact-option-link.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn compact_option_link_cranelift() { compact_option_link(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn compact_option_link_llvm() { compact_option_link(Backend::Llvm); }
/// MEM-4, MEM-12: a FixedSpan field is emitted from a List view, packed, on each engine.
fn stream_fixed_span(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/stream-fixed-span.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn stream_fixed_span_cranelift() { stream_fixed_span(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn stream_fixed_span_llvm() { stream_fixed_span(Backend::Llvm); }
/// MEM-4: views of completed elements of an open writer, on each engine.
fn stream_completed(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/stream-completed.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn stream_completed_cranelift() { stream_completed(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn stream_completed_llvm() { stream_completed(Backend::Llvm); }
/// LANG-6, MEM-4: an emitted Text operand's temporary is dropped only where it was made.
fn stream_text_temp(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/stream-text-temp.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn stream_text_temp_cranelift() { stream_text_temp(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn stream_text_temp_llvm() { stream_text_temp(Backend::Llvm); }
/// MEM-24: finish_trim refunds round8(n) - round8(used) to the limit, on each engine.
fn stream_finish_trim(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/stream-finish-trim.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn stream_finish_trim_cranelift() { stream_finish_trim(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn stream_finish_trim_llvm() { stream_finish_trim(Backend::Llvm); }

fn span_rigid(backend: Backend) {
    run_lines_native(backend, include_str!("../../../core/front/cases/body/span-rigid.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn span_rigid_cranelift() { span_rigid(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn span_rigid_llvm() { span_rigid(Backend::Llvm); }

fn uninitialized_take_names(backend: Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let source=include_str!("../../../core/front/cases/body/uninit-take-name.hirsel");
    let mut request=source_request(source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore="explicit native integration target"]
fn uninitialized_take_names_cranelift(){uninitialized_take_names(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn uninitialized_take_names_llvm(){uninitialized_take_names(Backend::Llvm);}
// SEM-5 / FORM #65: insertion and lookup derive compact key helpers from its
// value row's typed locator. Equal values in separate blobs hit; prefixes miss.
fn stable_key_map(backend: Backend) {
    ordered_case(backend, include_str!("../../../core/front/cases/body/key-stable-map.hirsel"));
}
#[test]
#[ignore = "explicit native integration target"]
fn stable_key_map_cranelift() { stable_key_map(Backend::Cranelift); }
#[test]
#[ignore = "explicit native integration target"]
fn stable_key_map_llvm() { stable_key_map(Backend::Llvm); }

fn closed_collection_comparison(backend: Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let source=include_str!("../../../core/front/cases/body/compare-collections.hirsel");
    let mut request=source_request(source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore="explicit native integration target"]
fn closed_collection_comparison_cranelift(){closed_collection_comparison(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn closed_collection_comparison_llvm(){closed_collection_comparison(Backend::Llvm);}

fn loop_capture_entry(backend: Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let source=include_str!("../../../core/front/cases/body/loop-capture-entry.hirsel");
    let mut request=source_request(source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore="explicit native integration target"]
fn loop_capture_entry_cranelift(){loop_capture_entry(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn loop_capture_entry_llvm(){loop_capture_entry(Backend::Llvm);}

fn word_load_bit_counts(backend: Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source=include_str!("../../../core/front/cases/body/word-bit-counts.hirsel");
    for (check,exit) in [("positive()",0),("word(8)==0u64",101),("word(18446744073709551615)==0u64",101),("word(18446744073709551608)==0u64",101),("short()==0u64",101)] {
        let dir=tempfile::tempdir().unwrap();
        let main=format!("{source}\npub fn main()->Result[(),Exit] {{ if {check} {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}");
        let mut request=source_request(&main,dir.path());
        request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
        let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
        let entry=DefPath::from_full("app@1/main/main").unwrap();
        let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();
        let output=Command::new(&built.executable).output().unwrap();
        assert_eq!(output.status.code(),Some(exit),"{check}: {output:?}");
        assert!(output.stdout.is_empty(),"{check}: {output:?}");
        if exit==101 { assert!(output.stderr.starts_with(b"Bounds"),"{check}: {output:?}"); }
        else { assert!(output.stderr.is_empty(),"{check}: {output:?}"); }
    }
}
#[test]
#[ignore="explicit native integration target"]
fn word_load_bit_counts_cranelift(){word_load_bit_counts(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn word_load_bit_counts_llvm(){word_load_bit_counts(Backend::Llvm);}

fn holder_payload_read(backend:Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let source=include_str!("../../../core/front/cases/body/holder-payload-read.hirsel");
    let source=format!("{source}\npub fn main()->Result[(),Exit] {{ if positive() {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}");
    let mut request=source_request(&source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore="explicit native integration target"]
fn holder_payload_read_cranelift(){holder_payload_read(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn holder_payload_read_llvm(){holder_payload_read(Backend::Llvm);}

fn loop_anchor_lifetimes(backend:Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let source=include_str!("../../../core/front/cases/body/loop-anchor-accepted.hirsel");
    let declared=include_str!("../../../core/front/cases/body/loop-declared-anchors.hirsel").replace("positive()", "declared_positive()");
    let source=format!("{source}\n{declared}\npub fn main()->Result[(),Exit] {{ if positive() && declared_positive() {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}");
    let mut request=source_request(&source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore="explicit native integration target"]
fn loop_anchor_lifetimes_cranelift(){loop_anchor_lifetimes(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn loop_anchor_lifetimes_llvm(){loop_anchor_lifetimes(Backend::Llvm);}

fn mut_scoped_anchors(backend:Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let source=include_str!("../../../core/front/cases/body/mut-scoped-anchors.hirsel");
    let source=format!("{source}\npub fn main()->Result[(),Exit] {{ if positive() {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}");
    let mut request=source_request(&source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore="explicit native integration target"]
fn mut_scoped_anchors_cranelift(){mut_scoped_anchors(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn mut_scoped_anchors_llvm(){mut_scoped_anchors(Backend::Llvm);}

fn temporary_owner_views(backend:Backend) {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir=tempfile::tempdir().unwrap();
    let source=include_str!("../../../core/front/cases/body/index-temporary-owner.hirsel");
    let source=format!("{source}\npub fn main()->Result[(),Exit] {{ if positive() {{ Ok(()) }} else {{ Error(Exit(status:1)) }} }}");
    let mut request=source_request(&source,dir.path());
    request.inputs.packages[1]=PackageManifest::directory(&root.join("core/core"),package("core"),true).unwrap();
    let product=Driver::default().check(request,&mut Meter::new(false)).unwrap();
    let entry=DefPath::from_full("app@1/main/main").unwrap();
    let built=product.build(&entry,&native_options(backend,dir.path())).unwrap();
    run_clean(&built.executable,0);
}
#[test]
#[ignore="explicit native integration target"]
fn temporary_owner_views_cranelift(){temporary_owner_views(Backend::Cranelift);}
#[test]
#[ignore="explicit native integration target"]
fn temporary_owner_views_llvm(){temporary_owner_views(Backend::Llvm);}
