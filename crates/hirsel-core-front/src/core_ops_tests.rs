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
fn cap6_ambient_kernels_take_authority() {
    let (index, modules) = canonical("");
    let p = Program::declare(&index, &modules);
    crate::body::core_ops::assert_no_ambient_input(&p);
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
            "fn f[T:send]() { let unsupported = int; let c: Result[(Sender[T],Receiver[T]),Capacity] = channel.new(1); let _ = take c; task.check_cancel() }",
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

#[test]
fn args_rows_return_count_and_run_owned_optional_views() {
    let source = "fn count(a:Args) -> u64 { args.len(a) }\nfn first(a:Args) -> bool { match args.get(a,0) { None => false, Some(s) => s==\"alpha\" } }\nfn second(a:Args) -> bool { match args.get(a,1) { None => false, Some(s) => s==\"\" } }\nfn missing(a:Args) -> bool { match args.get(a,2) { None => true, Some(_) => false } }";
    let (index, modules) = canonical(source);
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let mut emitter = Emitter::new();
    let mut functions = HashMap::new();
    for (f, fd) in p
        .fns
        .iter()
        .enumerate()
        .filter(|(_, fd)| fd.path.0.package.name == "app")
    {
        let check = p.check_lower(f, &mut emitter);
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{check:?}"
        );
        functions.insert(fd.path.0.name.as_str(), check.function.unwrap());
    }
    wf::module(&emitter.module).unwrap();
    for (argv, results) in [(vec![], [0, 0, 0, 1]), (vec!["alpha", ""], [2, 1, 1, 1])] {
        for (name, expected) in ["count", "first", "second", "missing"]
            .into_iter()
            .zip(results)
        {
            let result = eval::run_args(&emitter.module, functions[name], &[0], 0, &argv);
            assert_eq!(
                result.outcome,
                eval::Outcome::Returned(vec![expected]),
                "{name} {argv:?}"
            );
            assert_eq!(result.used, 0, "argument views admit no storage");
        }
    }
}

#[test]
fn stream_rows_write_literal_and_argument_views_without_admission() {
    let source = "fn f(a:Args,out:Stdout,err:Stderr) -> u64 waits { result.unwrap(stdout.write(out,\"out\")); let s = option.unwrap(args.get(a,0)); result.unwrap(stdout.write(out,s)); result.unwrap(stderr.write(err,\"err\")); 7u64 }";
    let (index, modules) = canonical(source);
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let mut emitter = Emitter::new();
    let f = p
        .fns
        .iter()
        .position(|fd| fd.path.0.package.name == "app")
        .unwrap();
    let check = p.check_lower(f, &mut emitter);
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{check:?}"
    );
    wf::module(&emitter.module).unwrap();
    let run = eval::run_args(
        &emitter.module,
        check.function.unwrap(),
        &[0, 0, 0],
        0,
        &["alpha"],
    );
    assert_eq!(run.outcome, eval::Outcome::Returned(vec![7]));
    assert_eq!(run.stdout, b"outalpha");
    assert_eq!(run.stderr, b"err");
    assert_eq!(run.used, 0);
}

#[test]
fn host_rows_preserve_handles_result_tags_and_mutated_carriers() {
    let source = r#"fn f(d:Dir,c:Clock) -> u64 waits {
        match host.open(d,"missing",Read) { Error(NotFound) => {}, _ => return 1u64 }
        let mut file = result.unwrap(host.open(d,"file",CreateNew));
        match host.open(d,"file",CreateNew) { Error(Exists) => {}, _ => return 2u64 }
        result.unwrap(host.write(mut file,string.as_bytes("data")));
        if result.unwrap(host.try_lock(mut file)) == false { return 3u64 }
        let mut other = result.unwrap(host.open(d,"file",Append));
        if result.unwrap(host.try_lock(mut other)) { return 4u64 }
        result.unwrap(host.close(take other));
        result.unwrap(host.unlock(mut file));
        result.unwrap(host.close(take file));
        let mut reader = result.unwrap(host.open(d,"file",Read));
        let mut b:Bytes = result.unwrap(list.with_capacity(4));
        if result.unwrap(host.read(mut reader,mut b)) != 4u64 || list.len(b)!=4u64 { return 5u64 }
        if b[0]!=b'd' || b[3]!=b'a' { return 6u64 }
        if result.unwrap(host.read(mut reader,mut b)) != 0u64 { return 7u64 }
        result.unwrap(host.close(take reader));
        result.unwrap(host.remove(d,"file"));
        result.unwrap(host.mkdir(d,"dir"));
        result.unwrap(host.remove_tree(d,"dir"));
        let a = host.mono_ns(c); let _ = host.wall_ms(c);
        if host.mono_ns(c)<a { return 8u64 }
        42u64
    }"#;
    let (index, modules) = canonical(source);
    let p = Program::declare(&index, &modules);
    assert!(p.refusals.is_empty(), "{:?}", p.refusals);
    let mut emitter = Emitter::new();
    let f = p
        .fns
        .iter()
        .position(|fd| fd.path.0.package.name == "app")
        .unwrap();
    let check = p.check_lower(f, &mut emitter);
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{check:?}"
    );
    wf::module(&emitter.module).unwrap();
    let run = eval::run(&emitter.module, check.function.unwrap(), &[0, 0], 8);
    assert_eq!(run.outcome, eval::Outcome::Returned(vec![42]));
    assert_eq!(
        run.used, 0,
        "file handles admit nothing; the literal's storage and the four-byte buffer are refunded"
    );
}

#[test]
fn string_views_empty_carriers_and_unique_ids_admit_nothing() {
    let source = r#"fn f() -> u64 {
        let s = string.new()
        if string.byte_len(s)!=0u64 { return 1u64 }
        let b = string.as_bytes("abc")
        if list.len(b)!=3u64 || b[1]!=b'b' { return 2u64 }
        let a = unique.new()
        let other = unique.new()
        if a==other { return 3u64 }
        7u64
    }"#;
    let (index, modules) = canonical(source);
    let p = Program::declare(&index, &modules);
    let mut emitter = Emitter::new();
    let f = p
        .fns
        .iter()
        .position(|f| f.path.0.package.name == "app")
        .unwrap();
    let check = p.check_lower(f, &mut emitter);
    assert!(
        check.refusals.is_empty() && check.gaps.is_empty(),
        "{check:?}"
    );
    wf::module(&emitter.module).unwrap();
    let run = eval::run(&emitter.module, check.function.unwrap(), &[], 0);
    assert_eq!(run.outcome, eval::Outcome::Returned(vec![7]));
    assert_eq!(run.used, 0);
}

#[test]
fn generic_box_resource_moves_use_held_width_and_refund_both_nodes() {
    let source = r#"resource type R:send {a:u64,b:u64}
fn boxed[T](take x:T)->Box[T] {result.unwrap(box.new(take x))}
fn unboxed[T](take x:Box[T])->T {box.unbox(take x)}
fn f()->u64 {
    let r=result.unwrap(R(a:7u64,b:11u64))
    let boxed_r=boxed(take r)
    let back=unboxed(take boxed_r)
    back.a+back.b
}"#;
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
            "{check:?}"
        );
        if fd.path.0.name == "f" {
            entry = check.function;
        }
    }
    emitter.materialize_instances().unwrap();
    wf::module(&emitter.module).unwrap();
    // Resource block: 32+16=48. Box owns only its eight-byte holder.
    let run = eval::run(&emitter.module, entry.unwrap(), &[], 56);
    assert_eq!(run.outcome, eval::Outcome::Returned(vec![18]));
    assert_eq!(run.used, 0);
}

#[test]
fn host_rename_and_sleep_rows_are_replayable_in_evaluator() {
    let source = r#"fn f(d:Dir,c:Clock)->u64 waits {
        let mut file = result.unwrap(host.open(d, "old", Write))
        result.unwrap(host.write(mut file, string.as_bytes("ok")))
        result.unwrap(host.close(take file))
        result.unwrap(host.rename(d, "old", "new"))
        let mut read = result.unwrap(host.open(d, "new", Read))
        let mut b:Bytes = result.unwrap(list.with_capacity(2))
        if result.unwrap(host.read(mut read, mut b)) != 2u64 { return 1u64 }
        result.unwrap(host.close(take read))
        let now = host.mono_ns(c)
        host.sleep_until(c, now)
        result.unwrap(host.remove(d, "new"))
        7u64
    }"#;
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
            "{check:?}"
        );
        if fd.path.0.name == "f" {
            entry = check.function;
        }
    }
    emitter.materialize_instances().unwrap();
    wf::module(&emitter.module).unwrap();
    let run = eval::run(&emitter.module, entry.unwrap(), &[1, 1], 256);
    assert_eq!(run.outcome, eval::Outcome::Returned(vec![7]));
    assert_eq!(run.used, 0);
}

// std.clock.sleep_ns replayed: the evaluator world delivers each `host.mono_ns` reading
// (one tick per reading) and the `host.sleep_until` completion from its record, never a
// host wait. Hand-derived: `before` reads 0, sleep_ns reads 1 and issues deadline 1 + 1000,
// so the world clock is delivered at 1001 and `after` reads 1001. A deadline taken as
// `ns` alone delivers 1000; a wrapping add never fails the second run.
#[test]
fn std_sleep_ns_replays_its_deadline_from_the_record() {
    let source = format!(
        "{}\nfn f(c: Clock) -> u64 waits {{\n    let before = host.mono_ns(c)\n    sleep_ns(c, 1000)\n    host.mono_ns(c) - before\n}}\nfn g(c: Clock) -> u64 waits {{\n    let _ = host.mono_ns(c)\n    sleep_ns(c, int.wrapping_sub(0u64, 1))\n    0u64\n}}\n",
        include_str!("../../../core/std/clock.hirsel")
    );
    let (index, modules) = canonical(&source);
    let p = Program::declare(&index, &modules);
    let mut emitter = Emitter::new();
    let mut entries = HashMap::new();
    for (f, fd) in p
        .fns
        .iter()
        .enumerate()
        .filter(|(_, fd)| fd.path.0.package.name == "app")
    {
        let check = p.check_lower(f, &mut emitter);
        assert!(
            check.refusals.is_empty() && check.gaps.is_empty(),
            "{check:?}"
        );
        entries.insert(fd.path.0.name.clone(), check.function);
    }
    emitter.materialize_instances().unwrap();
    wf::module(&emitter.module).unwrap();
    let run = eval::run(&emitter.module, entries["f"].unwrap(), &[1], 256);
    assert_eq!(run.outcome, eval::Outcome::Returned(vec![1001]));
    assert_eq!(run.used, 0);
    let run = eval::run(&emitter.module, entries["g"].unwrap(), &[1], 256);
    assert_eq!(run.outcome, eval::Outcome::Failed(vec!["Overflow".into()]));
}
