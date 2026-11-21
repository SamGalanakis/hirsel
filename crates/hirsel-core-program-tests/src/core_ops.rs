//! Source runtime laws consume one sealed product per definition.
use crate::cases::inputs_with_core;
use hirsel_core_front::*;
use hirsel_core_ir::eval;
use hirsel_core_resolve::NameIndex;
use std::path::Path;
fn canonical(source: &str) -> (NameIndex, Vec<ModuleInput>) {
    inputs_with_core(source, &crate::repo().join("core/core"))
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
        if source.contains("fn f[T") {
            let f = p
                .fns
                .iter()
                .position(|f| f.path.package_is("app", "1") && f.path.0.name == "f")
                .unwrap();
            let check = p.check_lower(f, &mut Emitter::new());
            assert!(check.refusals.is_empty());
            assert!(check.function.is_none());
            assert!(!check.gaps.is_empty());
            continue;
        }
        let (module, functions) =
            crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

        let run = eval::run(&module, functions["f"], &[], limit);
        assert_eq!(run.outcome, expected, "{source}");
        assert_eq!(run.used, 0, "{source}: unrefunded channel storage");
    }
}

#[test]
fn spawn_adapts_scalar_place_and_unit_results_and_returns_refused_input() {
    let cases = [
        (
            "fn child(take x: u64) -> u64 { x+1u64 }\nfn f(tasks: Tasks) -> u64 waits { let mut h = task.spawn(tasks, bytes 64, 41u64, child); task.cancel(mut h); result.unwrap(task.join(take h)) }",
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
            "fn child(take x: ()) {}\nfn f(tasks: Tasks) -> u64 waits { let h = task.spawn(tasks, bytes 0, (), child); result.unwrap(task.join(take h)); 7u64 }",
            1000,
            eval::Outcome::Returned(vec![7]),
        ),
        (
            "type Pair: send { a: u64, b: u64 }\nfn child(take x: Pair) -> Pair { Pair(a:x.b,b:x.a) }\nfn f(tasks: Tasks) -> u64 waits { let h = task.spawn(tasks, bytes 64, Pair(a:7u64,b:11u64), child); let p = result.unwrap(task.join(take h)); p.a*100u64+p.b }",
            1000,
            eval::Outcome::Returned(vec![1107]),
        ),
        (
            "fn child(take x: u64) -> u64 { x+1u64 }\nfn f(tasks: Tasks) -> u64 waits { let h = task.spawn(tasks, bytes 64, 18446744073709551615u64, child); match task.join(take h) { Ok(_) => 0u64, Error(Failed(take causes)) => { let _ = take causes; 9u64 }, Error(Cancelled(_)) => 1u64 } }",
            1000,
            eval::Outcome::Returned(vec![9]),
        ),
    ];
    for (source, limit, expected) in cases {
        let (index, modules) = canonical(source);
        let _p = Program::declare(&index, &modules);
        let (module, functions) =
            crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

        let run = eval::run(&module, functions["f"], &[1], limit);
        assert_eq!(run.outcome, expected, "{source}");
        assert_eq!(run.used, 0, "{source}: unrefunded task or cause storage");
    }
}

/// MEM-20: the child's unused 64-byte reservation reaches the parent as a
/// pending refund while the parent's 64th poll gives the child the turn. The
/// parent's next own event folds it. An Arc share is that event, so both room
/// reads after it agree; without it, the first refused admission is the folding
/// event, so only the second read sees the 64 bytes.
pub(crate) fn arc_share_refund_fold_source(share: bool) -> String {
    let polls = "task.check_cancel(); ".repeat(64);
    let share = if share {
        "let other = arc.share(a); "
    } else {
        ""
    };
    format!(
        "fn child(take x: u64) -> u64 {{ x }}\nfn room() -> u64 {{ let c: Result[(Sender[u64], Receiver[u64]), Capacity] = channel.new(18446744073709551615u64); match take c {{ Ok(_) => 0u64, Error(e) => e.available }} }}\nfn folded(tasks: Tasks) -> u64 waits {{ let a = arc.new(7u64); let h = task.spawn(tasks, bytes 64, 41u64, child); {polls}{share}let first = room(); let second = room(); let _ = result.unwrap(task.join(take h)); second - first }}\n"
    )
}

#[test]
fn arc_share_folds_a_pending_refund_like_any_owner_event() {
    for (share, expected) in [(true, 0), (false, 64)] {
        let source = arc_share_refund_fold_source(share);
        let (index, modules) = canonical(&source);
        let _p = Program::declare(&index, &modules);
        let (module, functions) =
            crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));
        let run = eval::run(&module, functions["folded"], &[1], 100_000);
        assert_eq!(
            run.outcome,
            eval::Outcome::Returned(vec![expected]),
            "share {share}"
        );
    }
}

#[test]
fn counted_and_box_kernels_preserve_payloads_handback_and_exact_refunds() {
    let mut cases = vec![
        ("fn f() -> u64 { box.unbox(box.new(7u64)) }".to_owned(), 8, eval::Outcome::Returned(vec![7])),
        ("fn f() -> u64 { box.unbox(box.new(())); 7u64 }".to_owned(), 0, eval::Outcome::Returned(vec![7])),
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
        cases.push((format!("type Data:shareable+send {{ a:u8,b:Box[u64] }}\nfn f() -> u64 {{ let b = box.new(41u64); let data = Data(a:7u8,b:take b); let r = {module}.new(take data); match take r {{ Ok(_) => 0u64, Error((e,take back)) => {{ let Data(a:_,b:take boxed) = take back; let v = box.unbox(take boxed); e.requested*10000u64+e.available*100u64+v }} }} }}"),31,eval::Outcome::Returned(vec![242341])));
    }
    for (source, limit, expected) in cases {
        let (index, modules) = canonical(&source);
        let p = Program::declare(&index, &modules);
        assert!(p.refusals.is_empty(), "{source}: {:?}", p.refusals);
        let (module, functions) =
            crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));
        let run = eval::run(&module, functions["f"], &[], limit);
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
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

    for (argv, results) in [(vec![], [0, 0, 0, 1]), (vec!["alpha", ""], [2, 1, 1, 1])] {
        for (name, expected) in ["count", "first", "second", "missing"]
            .into_iter()
            .zip(results)
        {
            let result = eval::run_args(&module, functions[name], &[0], 0, &argv);
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
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

    let run = eval::run_args(&module, functions["f"], &[0, 0, 0], 0, &["alpha"]);
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
        let mut b:Bytes = list.with_capacity(4);
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
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

    let run = eval::run(&module, functions["f"], &[0, 0], 8);
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
    let _p = Program::declare(&index, &modules);
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

    let run = eval::run(&module, functions["f"], &[], 0);
    assert_eq!(run.outcome, eval::Outcome::Returned(vec![7]));
    assert_eq!(run.used, 0);
}

#[test]
fn generic_box_resource_moves_use_held_width_and_refund_both_nodes() {
    let source = r#"resource type R:send {a:u64,b:u64}
fn boxed[T](take x:T)->Box[T] {box.new(take x)}
fn unboxed[T](take x:Box[T])->T {box.unbox(take x)}
fn f()->u64 {
    let r=result.unwrap(R(a:7u64,b:11u64))
    let boxed_r=boxed(take r)
    let back=unboxed(take boxed_r)
    back.a+back.b
}"#;
    let (index, modules) = canonical(source);
    let _p = Program::declare(&index, &modules);
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

    // Resource block: 32+16=48. Box owns only its eight-byte holder.
    let run = eval::run(&module, functions["f"], &[], 56);
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
        let mut b:Bytes = list.with_capacity(2)
        if result.unwrap(host.read(mut read, mut b)) != 2u64 { return 1u64 }
        result.unwrap(host.close(take read))
        let now = host.mono_ns(c)
        host.sleep_until(c, now)
        result.unwrap(host.remove(d, "new"))
        7u64
    }"#;
    let (index, modules) = canonical(source);
    let _p = Program::declare(&index, &modules);
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

    let run = eval::run(&module, functions["f"], &[1, 1], 256);
    assert_eq!(run.outcome, eval::Outcome::Returned(vec![7]));
    assert_eq!(run.used, 0);
}

/// A child's effects before its first wait are visible to the parent after the
/// parent's own wait (CONC-10, CONC-13; both native engines agree). The
/// sleep-deferral device broke both: the first read 1, the second faulted.
#[test]
fn child_effects_before_its_sleep_reach_the_parent() {
    let cases = [
        (
            r#"
type In:send { clock: Clock, h: Latest[u64] }
fn number(x: u64) -> u64 { x }
fn child(take s: In) waits {
    let In(clock: clock, h: h) = take s
    latest.publish(h, arc.new(2u64))
    let now = host.mono_ns(clock)
    host.sleep_until(clock, now + 20_000_000u64)
}
fn f(tasks: Tasks, clock: Clock, out: Stdout) -> u64 waits {
    let h = latest.new(arc.new(1u64))
    let t = task.spawn(tasks, bytes 4096, In(clock: clock, h: latest.handle(h)), child)
    let now = host.mono_ns(clock)
    host.sleep_until(clock, now + 60_000_000u64)
    let seen = number(latest.read(h))
    result.unwrap(task.join(take t))
    seen
}
"#,
            2,
        ),
        (
            r#"
type In:send { clock: Clock, tx: Sender[u64] }
fn child(take s: In) waits {
    let In(clock: clock, tx: tx) = take s
    result.unwrap(channel.send(tx, 7u64))
    let now = host.mono_ns(clock)
    host.sleep_until(clock, now + 20_000_000u64)
}
fn f(tasks: Tasks, clock: Clock, out: Stdout) -> u64 waits {
    let (tx, rx): (Sender[u64], Receiver[u64]) = channel.new(1)
    let h = task.spawn(tasks, bytes 4096, In(clock: clock, tx: take tx), child)
    let v = result.unwrap(channel.receive(rx))
    result.unwrap(task.join(take h))
    v
}
"#,
            7,
        ),
    ];
    for (source, expected) in cases {
        let run = run_tasks(source, "f");
        assert_eq!(
            run.outcome,
            eval::Outcome::Returned(vec![expected]),
            "{source}"
        );
    }
}

#[test]
fn runtime_task_rows_run_on_the_evaluator() {
    for (name, stdout) in [
        ("sleep-peer", b"PS".as_slice()),
        ("sleep-cancel", b"C"),
        ("parent-failure", b"F"),
    ] {
        let source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../core/front/runtime/{name}.hirsel")),
        )
        .unwrap();
        // `main` returns its Result through a place; the entry returns a word.
        let source = source
            + "\nfn ok(tasks: Tasks, clock: Clock, out: Stdout) -> u64 waits { match main(tasks, clock, out) { Ok(()) => 1u64, Error(_) => 0u64 } }\n";
        let run = run_tasks(&source, "ok");
        assert_eq!(run.outcome, eval::Outcome::Returned(vec![1]), "{name}");
        assert_eq!(run.stdout, stdout, "{name}");
    }
}

/// Check and lower an app source and run `entry(tasks, clock, out)` on the
/// evaluator.
fn run_tasks(source: &str, entry: &str) -> eval::Report {
    let (index, modules) = canonical(source);
    let _p = Program::declare(&index, &modules);
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

    eval::run(&module, functions[entry], &[1, 1, 1], 1 << 20)
}

#[test]
fn log_boxed_list_append_and_finish_in_evaluator() {
    let source = r#"fn f() -> u64 {
        let l:Log[u64] = log.new()
        log.append(l, 7u64)
        let xs = log.finish(take l)
        list.len(xs)
    }"#;
    let (index, modules) = canonical(source);
    let _p = Program::declare(&index, &modules);
    let (module, functions) =
        crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));

    let run = eval::run(&module, functions["f"], &[], 64);
    assert_eq!(run.outcome, eval::Outcome::Returned(vec![1]));
    assert_eq!(run.used, 0);
}

#[test]
fn wire_socket_model_carries_addresses_deadlines_and_bytes_on_evaluator() {
    use hirsel_core_net::{
        GrantRule, Sockets,
        model::{Choice, Config, Model},
    };
    for latency in [0, 3] {
        let source = if latency == 0 {
            crate::wire_socket::SOURCE.to_owned()
        } else {
            crate::wire_socket::SOURCE
                .replace("mut input, 2, At(c, 0)", "mut input, 2, Never")
                .replace("mut input, 1, At(c, 0)", "mut input, 1, Never")
                .replace("mut empty, 1, At(c, 0)", "mut empty, 1, At(c, 100)")
        };
        let (program, names) = crate::SourceProgram::lower(&source, None);
        let model = Model::new(Config {
            latency_ns: latency,
            prefix: [
                (Choice::Send, vec![2]),
                (Choice::Delivery, vec![2, 2]),
                (Choice::Receive, vec![1, 0]),
            ]
            .into(),
            ..Config::default()
        });
        let mut network = eval::Network {
            sockets: Sockets::with_model(model.clone()),
            ..eval::Network::default()
        };
        network.grants.insert(
            1,
            vec![
                GrantRule::parse("Connect 127.0.0.0/8 1-65535").unwrap(),
                GrantRule::parse("Listen 127.0.0.0/8 0-65535").unwrap(),
            ],
        );
        let report = eval::run_network(&program, names["wire"], &[1, 2], 1 << 20, network);
        assert_eq!(report.outcome, eval::Outcome::Returned(vec![42]));
        assert_eq!(report.used, 0);
        if latency != 0 {
            assert_eq!(model.now(), 100);
        }
        assert_eq!((model.live_handles(), model.pending()), (0, 0));
    }
}

#[test]
fn wire_model_cancellation_after_issue_returns_aborted_without_consuming_bytes() {
    use hirsel_core_net::GrantRule;
    let source = r#"
type Input: send { r: ReadHalf }
fn reader(take x: Input) -> (u64, ReadHalf) waits {
    let Input(r: r0) = take x
    let mut r = r0
    let mut empty: Bytes = list.new()
    let zero = result.unwrap(host.recv_bounded(mut r, mut empty, 0, Never))
    let mut b: Bytes = list.with_capacity(1)
    let signal = match host.recv_bounded(mut r, mut b, 1, Never) { Error(Aborted) => if list.len(b) == 0 && zero == 0 { 7u64 } else { 0u64 }, _ => 0u64 }
    (signal, take r)
}
fn f(n: Net, c: Clock, tasks: Tasks) -> u64 waits {
    let (l0, address) = result.unwrap(host.listen(n, Addr(ip: V4(2130706433u32), port: 0), 1))
    let mut l = l0
    let client = result.unwrap(host.connect(n, address, Never))
    let (server, peer) = result.unwrap(host.accept(mut l, Never))
    let (cr, cw0) = net.split(take client)
    let mut cw = cw0
    let (sr, sw) = net.split(take server)
    let mut h = task.spawn(tasks, bytes 4096, Input(r: take sr), reader)
    host.sleep_until(c, 10)
    task.cancel(mut h)
    let payload = result.unwrap(b"x")
    let sent = result.unwrap(host.send_pair(mut cw, payload, string.as_bytes(""), Never))
    let (signal, r0) = result.unwrap(task.join(take h))
    let mut r = r0
    let mut b: Bytes = list.with_capacity(1)
    let received = result.unwrap(host.recv_bounded(mut r, mut b, 1, At(c, 0)))
    if signal == 7 && sent == 1 && received == 1 && b[0] == 120u8 { 7u64 } else { 0u64 }
}
"#;
    let (program, names) = crate::SourceProgram::lower(source, None);
    let mut network = eval::Network::default();
    let model = network.sockets.model().unwrap().clone();
    network.grants.insert(
        1,
        vec![
            GrantRule::parse("Connect 127.0.0.0/8 1-65535").unwrap(),
            GrantRule::parse("Listen 127.0.0.0/8 0-65535").unwrap(),
        ],
    );
    let report = eval::run_network(&program, names["f"], &[1, 2, 3], 1 << 20, network);
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![7]));
    assert_eq!(report.used, 0);
    assert_eq!((model.live_handles(), model.pending()), (0, 0));
}

#[test]
fn scripted_tls_laws_on_evaluator() {
    use hirsel_core_net::{
        GrantRule, Sockets,
        model::{
            Address, CertFailure, Config, Model, TlsFailure, TlsHandshake, TlsIdentity, TlsScript,
            TlsStep,
        },
    };

    let source = r#"
fn close_writer(take w: WriteHalf) {}

fn prefix(n: Net, c: Clock) -> u64 waits {
    let peer = Addr(ip: V4(2130706433u32), port: 443)
    let socket = result.unwrap(host.connect(n, peer, Never))
    let child = result.unwrap(net.narrow(n, Tls(names: "alpha.test", ports: (443u16, 443u16), trust: SystemPin)))
    let mut offer: List[String] = list.new()
    list.push(mut offer, "h2")
    list.push(mut offer, "http/1.1")
    let (r0, w0, h) = result.unwrap(tls.connect(child, take socket, "ALPHA.TEST.", System, offer, Never))
    let (a, b, c0, d) = h.peer
    if h.protocol != Some(1u64) || a != 72623859790382856u64 || b != 1230066625199609624u64 || c0 != 2387509390608836392u64 || d != 3544952156018063160u64 {
        return 1
    }
    let size = 1048576u64
    let mut payload: Bytes = list.with_capacity(size)
    for i in 0u64 .. 256u64 {
        list.push(mut payload, int.narrow(i))
        list.fill(mut payload, 4095, 97u8)
    }
    let mut w = w0
    let mut sent = 0u64
    let mut short = false
    let mut timed_out = false
    while sent < size {
        let middle = sent + 1
        let end = if size - sent < 16384 { size } else { sent + 16384 }
        let first = list.slice(payload, sent, middle)
        let rest = list.slice(payload, middle, end)
        let deadline = host.mono_ns(c) + 1
        let count = match host.send_pair(mut w, first, rest, At(c, deadline)) {
            Ok(count) => {
                short = short || count < end - sent
                count
            },
            Error(TimedOut) => {
                timed_out = true
                result.unwrap(host.send_pair(mut w, first, rest, Never))
            },
            Error(_) => return 2
        }
        if count == 0 || count > end - sent { return 3 }
        sent = sent + count
    }
    let empty: Bytes = list.new()
    result.unwrap(host.send_pair(mut w, empty, empty, Never))
    close_writer(take w)
    let mut r = r0
    let mut reply: Bytes = list.with_capacity(1)
    let count = result.unwrap(host.recv_bounded(mut r, mut reply, 1, Never))
    if short && timed_out && count == 1 && reply[0] == 33u8 { 0u64 } else { 4u64 }
}

type HandshakeJob: send { n: Net, socket: Socket }
fn handshake(take job: HandshakeJob) -> u64 waits {
    let HandshakeJob(n: n, socket: socket) = take job
    let empty: List[String] = list.new()
    match tls.connect(n, take socket, "alpha.test", System, empty, Never) {
        Error(Aborted) => 7u64,
        _ => 0u64
    }
}

fn cancel(n: Net, c: Clock, tasks: Tasks) -> u64 waits {
    let peer = Addr(ip: V4(2130706433u32), port: 443)
    let socket = result.unwrap(host.connect(n, peer, Never))
    let mut h = task.spawn(tasks, bytes 4096, HandshakeJob(n: n, socket: take socket), handshake)
    host.sleep_until(c, 10)
    task.cancel(mut h)
    result.unwrap(task.join(take h))
}

fn exit(n: Net, c: Clock, kind: u64) -> u64 waits {
    let peer = Addr(ip: V4(2130706433u32), port: 443)
    let socket = result.unwrap(host.connect(n, peer, Never))
    let empty: List[String] = list.new()
    let until = if kind == 6 { At(c, 10) } else { Never }
    match tls.connect(n, take socket, "alpha.test", System, empty, until) {
        Error(TimedOut) => 6u64,
        Error(Tls(Certificate(NameMismatch))) => 9u64,
        Error(Denied) => 1u64,
        _ => 0u64
    }
}
"#;
    let (program, functions) = crate::SourceProgram::lower(source, None);
    let address = Address {
        family: 0,
        hi: 0,
        lo: 0x7f000001,
        port: 443,
    };
    let run = |name, args: &[u64], expected, allowed, script| {
        let model = Model::new(Config {
            queue_bytes: 4096,
            max_prefix: 4096,
            latency_ns: 3,
            record_decisions: false,
            ..Config::default()
        });
        model.script_tls_peer(address, script);
        let mut rules = vec![GrantRule::parse("Connect 127.0.0.1/32 443-443").unwrap()];
        if allowed {
            rules.push(GrantRule::parse("Tls *.test 443-443 System").unwrap());
        }
        let network = eval::Network {
            grants: [(1, rules)].into(),
            sockets: Sockets::with_model(model.clone()),
        };
        let mut remaining = eval::Network::default();
        let report = eval::run_network_into(
            &program,
            functions[name],
            args,
            4 << 20,
            network,
            &mut remaining,
        );
        assert_eq!(
            report.outcome,
            eval::Outcome::Returned(vec![expected]),
            "{name}"
        );
        assert!(remaining.sockets.is_empty(), "{name}");
        assert_eq!(
            (
                model.live_handles(),
                model.live_tls_sessions(),
                model.pending()
            ),
            (0, 0, 0),
            "{name}"
        );
    };

    // TL5. The peer answers only after it has matched every byte of the 1 MiB pattern.
    let pattern = (0u8..=255)
        .flat_map(|i| [i].into_iter().chain(std::iter::repeat_n(b'a', 4095)))
        .collect();
    let identity = TlsIdentity {
        protocol: Some(b"http/1.1".to_vec()),
        peer: [
            1, 2, 3, 4, 5, 6, 7, 8, 17, 18, 19, 20, 21, 22, 23, 24, 33, 34, 35, 36, 37, 38, 39, 40,
            49, 50, 51, 52, 53, 54, 55, 56,
        ],
    };
    run(
        "prefix",
        &[1, 2],
        0,
        true,
        TlsScript {
            handshake: TlsHandshake::After {
                delay_ns: 3,
                result: Ok(identity),
            },
            steps: vec![TlsStep::Expect(pattern), TlsStep::Send(vec![b'!'])],
        },
    );

    // TL7. Returning with a live socket, session or pending operation violates TLS-1.
    for (name, expected, allowed, handshake) in [
        ("cancel", 7, true, TlsHandshake::Silent),
        ("exit", 6, true, TlsHandshake::Silent),
        (
            "exit",
            9,
            true,
            TlsHandshake::After {
                delay_ns: 3,
                result: Err(TlsFailure::Certificate(CertFailure::NameMismatch)),
            },
        ),
        (
            "exit",
            1,
            false,
            TlsHandshake::After {
                delay_ns: 0,
                result: Err(TlsFailure::Closed),
            },
        ),
    ] {
        let kind = if name == "cancel" { 3 } else { expected };
        run(
            name,
            &[1, 2, kind],
            expected,
            allowed,
            TlsScript {
                handshake,
                steps: vec![],
            },
        );
    }
}

/// TL13. A deadline at the current model time permits one send attempt.
/// The one-record queue commits "A" then blocks. The peer acknowledges only
/// that prefix, and both send operations subsequently observe Reset.
#[test]
fn scripted_secret_send_spends_the_write_half() {
    use hirsel_core_net::{
        GrantRule, Sockets,
        model::{Address, Config, Model, TlsHandshake, TlsIdentity, TlsScript, TlsStep},
    };

    let source = r#"
fn f(n: Net, c: Clock) -> u64 waits {
    let peer = Addr(ip: V4(2130706433u32), port: 443)
    let socket = result.unwrap(host.connect(n, peer, Never))
    let empty: List[String] = list.new()
    let (reader, writer, _) = result.unwrap(tls.connect(n, take socket, "alpha.test", System, empty, Never))
    let mut r = take reader
    let mut w = take writer
    let b = string.into_bytes("BCD")
    let s = result.unwrap(secret.seal(take b, "*.test"))
    let narrowed = result.unwrap(secret.narrow(s, "ALPHA.TEST."))
    let before = string.into_bytes("A")
    let after = string.into_bytes("E")
    match tls.send_secret(mut w, before, narrowed, after, At(c, host.mono_ns(c))) {
        Error(TimedOut) => {},
        _ => return 1,
    }
    match host.send_pair(mut w, after, before, Never) {
        Error(Reset) => {},
        _ => return 2,
    }
    match tls.send_secret(mut w, after, s, before, Never) {
        Error(Reset) => {},
        _ => return 3,
    }
    let mut reply: Bytes = list.with_capacity(1)
    if result.unwrap(host.recv_bounded(mut r, mut reply, 1, Never)) == 1 && reply[0] == 33u8 {
        0u64
    } else {
        4u64
    }
}

"#;
    let (program, functions) = crate::SourceProgram::lower(source, None);
    let model = Model::new(Config {
        queue_bytes: 1,
        max_prefix: 1,
        latency_ns: 3,
        ..Config::default()
    });
    model.script_tls_peer(
        Address {
            family: 0,
            hi: 0,
            lo: 0x7f000001,
            port: 443,
        },
        TlsScript {
            handshake: TlsHandshake::After {
                delay_ns: 3,
                result: Ok(TlsIdentity {
                    protocol: None,
                    peer: [7; 32],
                }),
            },
            steps: vec![TlsStep::Expect(b"A".to_vec()), TlsStep::Send(b"!".to_vec())],
        },
    );
    let rules = vec![
        GrantRule::parse("Connect 127.0.0.1/32 443-443").unwrap(),
        GrantRule::parse("Tls *.test 443-443 System").unwrap(),
    ];
    let network = eval::Network {
        grants: [(1, rules)].into(),
        sockets: Sockets::with_model(model.clone()),
    };
    let report = eval::run_network(&program, functions["f"], &[1, 2], 4 << 20, network);
    assert_eq!(report.outcome, eval::Outcome::Returned(vec![0]));
    assert_eq!(report.used, 0);
    assert_eq!((model.live_handles(), model.pending()), (0, 0));
}

#[test]
fn secret_mac_authority_is_an_intersection() {
    let source = r#"
fn f() -> u64 {
    let data = string.as_bytes("what do ya want for nothing?")
    let key = result.unwrap(string.into_bytes("Jefe"))
    let mac = result.unwrap(secret.seal(take key, "mac"))
    let tag = result.unwrap(secret.hmac_sha256(mac, data))
    let mut tag_data: Bytes = result.unwrap(list.with_capacity(32))
    if !bytebuf.append_inline(mut tag_data, tag, 0, 32) { return 1 }
    let verify = result.unwrap(secret.narrow(mac, "verify"))
    let narrowed = result.unwrap(secret.narrow(verify, "mac"))
    match secret.hmac_sha256(narrowed, data) {
        Error(Denied) => {},
        _ => return 2,
    }
    if !result.unwrap(secret.verify_hmac_sha256(narrowed, data, tag_data)) { return 3 }
    if result.unwrap(secret.verify_hmac_sha256(narrowed, data, list.slice(tag_data, 0, 31))) { return 4 }
    let other_key = result.unwrap(string.into_bytes("Jefe"))
    let no_use = result.unwrap(secret.seal(take other_key, "alpha.test"))
    match secret.narrow(no_use, "mac") {
        Error(Invalid) => {},
        _ => return 5,
    }
    match secret.hmac_sha256(no_use, data) {
        Error(Denied) => 0,
        _ => 6,
    }
}
"#;
    let (module, functions) = crate::SourceProgram::lower(source, None);
    let result = eval::run(&module, functions["f"], &[], 4096);
    assert_eq!(
        result.outcome,
        eval::Outcome::Returned(vec![0]),
        "MAC authority case"
    );
}
