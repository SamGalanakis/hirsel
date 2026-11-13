//! Source-to-runtime tests. Values and charges are hand-derived from CONC-4/5,
//! MEM-24 and the declaration order of Result, Closed and SendError.
use super::*;

fn canonical(source: &str) -> (NameIndex, Vec<ModuleInput>) {
    inputs_with_core(
        source,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../core/core"),
    )
}

#[test]
fn core_operation_declarations_do_not_drift() {
    let (index, modules) = canonical("");
    let p = Program::declare(&index, &modules);
    crate::body::core_ops::assert_declarations(&p);
}

#[test]
fn channel_rows_preserve_values_charges_and_closed_errors() {
    let prefix = "fn close(take rx: Receiver[u64]) {}\nfn close_tx(take tx: Sender[u64]) {}\n";
    let cases = [
        // Two FIFO values, a cloned sender, then a closed/empty receiver.
        (
            "fn f() -> u64 waits { let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(2); let (tx,rx) = result.unwrap(take c); let tx2 = channel.sender(tx); result.unwrap(channel.send(tx,7u64)); result.unwrap(channel.send(tx2,11u64)); let a = result.unwrap(channel.receive(rx)); let b = result.unwrap(channel.receive(rx)); close_tx(take tx); close_tx(take tx2); match channel.receive(rx) { Ok(_) => 0u64, Error(Closed(())) => a*100u64+b } }",
            1000,
            eval::Outcome::Returned(vec![711]),
        ),
        // A full queue returns its value; a closed queue returns its value with
        // the distinct Closed tag, and cleaning the queue drops the first item.
        (
            "fn f() -> u64 { let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(1); let (tx,rx) = result.unwrap(take c); result.unwrap(channel.try_send(tx,7u64)); let a = match channel.try_send(tx,11u64) { Ok(()) => 0u64, Error(Full(x)) => x, Error(Closed(_)) => 1u64 }; close(take rx); let b = match channel.try_send(tx,13u64) { Ok(()) => 0u64, Error(Full(_)) => 1u64, Error(Closed(x)) => x }; a*100u64+b }",
            1000,
            eval::Outcome::Returned(vec![1113]),
        ),
        (
            "fn f() -> u64 waits { let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(1); let (tx,rx) = result.unwrap(take c); close(take rx); match channel.send(tx,17u64) { Ok(()) => 0u64, Error(Closed(x)) => x } }",
            1000,
            eval::Outcome::Returned(vec![17]),
        ),
        // 64 + 2*8 = 80. Refusal admits nothing and reports the original room.
        (
            "fn f() -> u64 { let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(2); match take c { Ok(_) => 0u64, Error(e) => e.requested+e.available } }",
            79,
            eval::Outcome::Returned(vec![159]),
        ),
        (
            "fn f() -> bool { let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(18446744073709551615u64); match take c { Ok(_) => false, Error(e) => e.requested == 18446744073709551615u64 && e.available == 1000u64 } }",
            1000,
            eval::Outcome::Returned(vec![1]),
        ),
        (
            "fn f() { let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(0); let _ = take c }",
            1000,
            eval::Outcome::Failed(vec!["Bounds".into()]),
        ),
        (
            "fn f() -> u64 { task.check_cancel(); 42u64 }",
            1000,
            eval::Outcome::Returned(vec![42]),
        ),
        // After an earlier gap the runtime adapter must not emit or panic.
        (
            "fn f[T:send]() { let c: Result[(Sender[T],Receiver[T]),Capacity] = channel.new(1); let _ = take c; task.check_cancel() }",
            1000,
            eval::Outcome::Returned(vec![]),
        ),
    ];
    for (source, limit, expected) in cases {
        let text = format!("{prefix}{source}");
        let (index, modules) = canonical(&text);
        let p = Program::declare(&index, &modules);
        let mut emitter = Emitter::new();
        let mut entry = None;
        let mut gaps = vec![];
        for (f, fd) in p
            .fns
            .iter()
            .enumerate()
            .filter(|(_, fd)| fd.path.0.package.name == "app")
        {
            let check = p.check_lower(f, &mut emitter);
            assert!(check.refusals.is_empty(), "{source}: {:?}", check.refusals);
            gaps.extend(check.gaps);
            if fd.path.0.name == "f" {
                entry = check.function;
            }
        }
        if source.contains("fn f[T") {
            assert!(entry.is_none());
            assert!(!gaps.is_empty());
            continue;
        }
        assert!(gaps.is_empty(), "{source}: {gaps:?}");
        emitter.materialize_instances().unwrap();
        wf::module(&emitter.module).unwrap();
        let run = eval::run(&emitter.module, entry.unwrap(), &[], limit);
        assert_eq!(run.outcome, expected, "{source}");
        assert_eq!(run.used, 0, "{source}: unrefunded channel storage");
    }
}

#[test]
fn spawn_adapts_scalar_place_and_unit_results_and_returns_refused_input() {
    let cases = [
        (
            "fn child(take x: u64) -> u64 { x+1u64 }\nfn f(tasks: Tasks) -> u64 waits { let mut h = result.unwrap(task.spawn(tasks, bytes 64, 41u64, child)); task.cancel(mut h); result.unwrap(task.join(take h)) }",
            1000,
            eval::Outcome::Returned(vec![42]),
        ),
        // 256 record + 32 primary cell + 8 result + 64 reservation = 360.
        (
            "fn child(take x: u64) -> u64 { x }\nfn f(tasks: Tasks) -> u64 { let c = task.spawn(tasks, bytes 64, 41u64, child); match take c { Ok(_) => 0u64, Error((e,x)) => e.requested+e.available+x } }",
            359,
            eval::Outcome::Returned(vec![760]),
        ),
        (
            "fn child(take x: ()) {}\nfn f(tasks: Tasks) -> u64 waits { let h = result.unwrap(task.spawn(tasks, bytes 0, (), child)); result.unwrap(task.join(take h)); 7u64 }",
            1000,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "type Pair: send { a: u64, b: u64 }\nfn child(take x: Pair) -> Pair { Pair(a:x.b,b:x.a) }\nfn f(tasks: Tasks) -> u64 waits { let h = result.unwrap(task.spawn(tasks, bytes 64, Pair(a:7u64,b:11u64), child)); let p = result.unwrap(task.join(take h)); p.a*100u64+p.b }",
            1000,
            eval::Outcome::Returned(vec![1107]),
        ),
        (
            "fn child(take x: u64) -> u64 { x+1u64 }\nfn f(tasks: Tasks) -> u64 waits { let h = result.unwrap(task.spawn(tasks, bytes 64, 18446744073709551615u64, child)); match task.join(take h) { Ok(_) => 0u64, Error(Failed(take causes)) => { let _ = take causes; 9u64 }, Error(Cancelled(_)) => 1u64 } }",
            1000,
            eval::Outcome::Returned(vec![9]),
        ),
    ];
    for (source, limit, expected) in cases {
        let (index, modules) = canonical(source);
        let p = Program::declare(&index, &modules);
        let mut emitter = Emitter::new();
        let mut entry = None;
        for (f, fd) in p
            .fns
            .iter()
            .enumerate()
            .filter(|(_, fd)| fd.path.0.package.name == "app")
        {
            let check = p.check_lower(f, &mut emitter);
            assert!(
                check.refusals.is_empty() && check.gaps.is_empty(),
                "{source}: {check:?}"
            );
            if fd.path.0.name == "f" {
                entry = check.function;
            }
        }
        emitter.materialize_instances().unwrap();
        wf::module(&emitter.module).unwrap();
        let run = eval::run(&emitter.module, entry.unwrap(), &[1], limit);
        assert_eq!(run.outcome, expected, "{source}");
        assert_eq!(run.used, 0, "{source}: unrefunded task or cause storage");
    }
}

#[test]
fn counted_and_box_kernels_preserve_payloads_handback_and_exact_refunds() {
    let mut cases = vec![
        ("fn f() -> u64 { box.unbox(result.unwrap(box.new(7u64))) }".to_owned(), 8, eval::Outcome::Returned(vec![7])),
        ("fn f() -> u64 { box.unbox(result.unwrap(box.new(()))); 7u64 }".to_owned(), 0, eval::Outcome::Returned(vec![7])),
        ("fn f() -> u64 { let r = box.new(7u64); match take r { Ok(_) => 0u64, Error((e,x)) => e.requested+e.available+x } }".to_owned(), 7, eval::Outcome::Returned(vec![22])),
        ("fn f() -> u64 { let r = bound bytes 0 { box.new(7u64) }; match take r { Ok(Error((e,x))) => e.requested+x, _ => 0u64 } }".to_owned(), 1000, eval::Outcome::Returned(vec![15])),
        // Resource R has a 16-byte block, but its holder in a channel is 8.
        ("resource type R:send { a:u64,b:u64 }\nfn f() -> u64 { let c: Result[(Sender[R],Receiver[R]),Capacity] = channel.new(1); match take c { Ok(_) => 0u64, Error(e) => e.requested+e.available } }".to_owned(), 71, eval::Outcome::Returned(vec![143])),
        // R's own block + cell costs 48. Box[R] admits a further eight, so
        // seven remaining bytes refuse, returning the live resource unchanged.
        ("resource type R:send { a:u64,b:u64 }\nfn f() -> u64 { let r = result.unwrap(R(a:7u64,b:11u64)); let c = box.new(take r); match take c { Ok(_) => 0u64, Error((e,take back)) => e.requested*10000u64+e.available*100u64+back.a+back.b } }".to_owned(), 55, eval::Outcome::Returned(vec![80718])),
    ];
    for (module, ty) in [("rc", "Rc"), ("arc", "Arc")] {
        cases.push((format!("fn end(take h:{ty}[u64]) {{}}\nfn f() -> u64 {{ let h = result.unwrap({module}.new(7u64)); let other = {module}.share(h); let h = match {module}.try_unwrap(take h) {{ Ok(_) => return 0u64, Error(take back) => back }}; end(take other); result.unwrap({module}.try_unwrap(take h)) }}").replace("let h = match", "let last = match").replace("try_unwrap(take h))", "try_unwrap(take last))"),16,eval::Outcome::Returned(vec![7])));
        cases.push((format!("type Data:shareable+send {{ a:u8,b:Box[u64] }}\nfn f() -> u64 {{ let b = result.unwrap(box.new(41u64)); let data = Data(a:7u8,b:take b); let r = {module}.new(take data); match take r {{ Ok(_) => 0u64, Error((e,take back)) => {{ let Data(a:_,b:take boxed) = take back; let v = box.unbox(take boxed); e.requested*10000u64+e.available*100u64+v }} }} }}"),31,eval::Outcome::Returned(vec![242341])));
    }
    for (source, limit, expected) in cases {
        let (index, modules) = canonical(&source);
        let p = Program::declare(&index, &modules);
        assert!(p.refusals.is_empty(), "{source}: {:?}", p.refusals);
        let mut emitter = Emitter::new();
        let mut entry = None;
        for (f, fd) in p
            .fns
            .iter()
            .enumerate()
            .filter(|(_, fd)| fd.path.0.package.name == "app" && fd.body.is_some())
        {
            let check = p.check_lower(f, &mut emitter);
            assert!(
                check.refusals.is_empty() && check.gaps.is_empty(),
                "{source}: {check:?}"
            );
            if fd.path.0.name == "f" {
                entry = check.function;
            }
        }
        emitter.materialize_instances().unwrap();
        if let Err(error) = wf::module(&emitter.module) {
            panic!(
                "{source}: {error}\n{}",
                hirsel_core_ir::text::print(&emitter.module)
            );
        }
        let run = eval::run(&emitter.module, entry.unwrap(), &[], limit);
        assert_eq!(run.outcome, expected, "{source}");
        assert_eq!(run.used, 0, "{source}: leaked payload, holder or node");
    }
}
