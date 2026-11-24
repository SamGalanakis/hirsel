//! Source runtime laws consume one sealed product per definition.
use crate::cases::inputs_with_core;
use hirsel_core_front::*;
use hirsel_core_ir::eval;
use hirsel_core_resolve::NameIndex;
use std::path::Path;
fn canonical(source: &str) -> (NameIndex, Vec<ModuleInput>) {
    inputs_with_core(source, &crate::repo().join("core/core"))
}

// FAIL-16 reserves a cause cell before running the allocation. Add its MEM-24
// charge to the test limit so the allocation itself still sees the stated room.
const ISOLATE_CELL_BYTES: u64 = 32;

fn capacity_refusal(body: &str, requested: u64, available: u64) -> String {
    format!(
        r#"fn f() -> bool {{
    let attempt = isolate(fn() -> u64 {{ {body}; 0u64 }})
    match take attempt {{
        Ok(Error(take causes)) => {{
            for cause in causes {{
                return match cause.kind {{
                    Capacity(c) => c.requested == {requested}u64 && c.available == {available}u64,
                    _ => false
                }}
            }}
            false
        }},
        _ => false
    }}
}}"#
    )
}

#[test]
fn channel_rows_preserve_values_charges_and_closed_errors() {
    let prefix = "fn close(take rx: Receiver[u64]) {}\nfn close_tx(take tx: Sender[u64]) {}\n";
    let cases = [
        // Two FIFO values, a cloned sender, then a closed/empty receiver.
        (
            "fn f() -> u64 waits { let (tx,rx): (Sender[u64], Receiver[u64]) = channel.new(2); let tx2 = channel.sender(tx); result.unwrap(channel.send(tx,7u64)); result.unwrap(channel.send(tx2,11u64)); let a = result.unwrap(channel.receive(rx)); let b = result.unwrap(channel.receive(rx)); close_tx(take tx); close_tx(take tx2); match channel.receive(rx) { Ok(_) => 0u64, Error(Closed(())) => a*100u64+b } }".to_owned(),
            1000,
            eval::Outcome::Returned(vec![711]),
            0,
        ),
        // A full queue returns its value; a closed queue returns its value with
        // the distinct Closed tag, and cleaning the queue drops the first item.
        (
            "fn f() -> u64 { let (tx,rx): (Sender[u64], Receiver[u64]) = channel.new(1); result.unwrap(channel.try_send(tx,7u64)); let a = match channel.try_send(tx,11u64) { Ok(()) => 0u64, Error(Full(x)) => x, Error(Closed(_)) => 1u64 }; close(take rx); let b = match channel.try_send(tx,13u64) { Ok(()) => 0u64, Error(Full(_)) => 1u64, Error(Closed(x)) => x }; a*100u64+b }".to_owned(),
            1000,
            eval::Outcome::Returned(vec![1113]),
            0,
        ),
        // A queued item holds the channel's own last Sender. Cleaning it at the
        // receiver's drop ends the channel exactly once, after the queue is
        // clean (CONC-9): no reentrant second refund, nothing left held.
        (
            "type Note: send { back: Sender[Note] }\nfn close_note(take rx: Receiver[Note]) {}\nfn close_back(take tx: Sender[Note]) {}\nfn f() -> u64 { let (tx,rx): (Sender[Note], Receiver[Note]) = channel.new(1); let back = channel.sender(tx); result.unwrap(channel.try_send(tx, Note(back: take back))); close_back(take tx); close_note(take rx); 1u64 }".to_owned(),
            1000,
            eval::Outcome::Returned(vec![1]),
            0,
        ),
        (
            "fn f() -> u64 waits { let (tx,rx): (Sender[u64], Receiver[u64]) = channel.new(1); close(take rx); match channel.send(tx,17u64) { Ok(()) => 0u64, Error(Closed(x)) => x } }".to_owned(),
            1000,
            eval::Outcome::Returned(vec![17]),
            0,
        ),
        // 64 + 2*8 = 80. Refusal admits nothing and reports the original room.
        (
            capacity_refusal("let _: (Sender[u64], Receiver[u64]) = channel.new(2)", 80, 79),
            79 + ISOLATE_CELL_BYTES,
            eval::Outcome::Returned(vec![1]),
            ISOLATE_CELL_BYTES,
        ),
        (
            capacity_refusal(
                "let _: (Sender[u64], Receiver[u64]) = channel.new(18446744073709551615u64)",
                u64::MAX,
                1000,
            ),
            1000 + ISOLATE_CELL_BYTES,
            eval::Outcome::Returned(vec![1]),
            ISOLATE_CELL_BYTES,
        ),
        (
            "fn f() { let _: (Sender[u64], Receiver[u64]) = channel.new(0) }".to_owned(),
            1000,
            eval::Outcome::Failed(vec!["Bounds".into()]),
            0,
        ),
        (
            "fn f() -> u64 { task.check_cancel(); 42u64 }".to_owned(),
            1000,
            eval::Outcome::Returned(vec![42]),
            0,
        ),
        // After an earlier gap the runtime adapter must not emit or panic.
        (
            "fn f[T:send]() { let unsupported = int; let c: (Sender[T],Receiver[T]) = channel.new(1); let _ = take c; task.check_cancel() }".to_owned(),
            1000,
            eval::Outcome::Returned(vec![]),
            0,
        ),
    ];
    for (source, limit, expected, held) in cases {
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
        assert_eq!(run.used, held, "{source}: unrefunded channel storage");
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
            "fn child(take x: u64) -> u64 { x }\nfn f(tasks: Tasks) -> u64 { let c = task.try_spawn(tasks, bytes 64, 41u64, child); match take c { Ok(_) => 0u64, Error((e,x)) => e.requested+e.available+x } }",
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
        "fn child(take x: u64) -> u64 {{ x }}\nfn room() -> u64 {{ let mut xs: List[u64] = list.new(); match list.try_reserve(mut xs, 18446744073709551615u64) {{ Ok(_) => 0u64, Error(e) => e.available }} }}\nfn folded(tasks: Tasks) -> u64 waits {{ let a = arc.new(7u64); let h = task.spawn(tasks, bytes 64, 41u64, child); {polls}{share}let first = room(); let second = room(); let _ = result.unwrap(task.join(take h)); second - first }}\n"
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
        (
            "fn f() -> u64 { box.unbox(box.new(7u64)) }".to_owned(),
            8,
            eval::Outcome::Returned(vec![7]),
            0,
        ),
        (
            "fn f() -> u64 { box.unbox(box.new(())); 7u64 }".to_owned(),
            0,
            eval::Outcome::Returned(vec![7]),
            0,
        ),
        (
            capacity_refusal("let _ = box.new(7u64)", 8, 7),
            7 + ISOLATE_CELL_BYTES,
            eval::Outcome::Returned(vec![1]),
            ISOLATE_CELL_BYTES,
        ),
        (
            capacity_refusal("let _ = bound bytes 0 { box.new(7u64) }", 8, 0),
            1000,
            eval::Outcome::Returned(vec![1]),
            ISOLATE_CELL_BYTES,
        ),
        // Resource R has a 16-byte block, but its holder in a channel is 8.
        (
            format!(
                "resource type R:send {{ a:u64,b:u64 }}\n{}",
                capacity_refusal("let _: (Sender[R],Receiver[R]) = channel.new(1)", 72, 71)
            ),
            71 + ISOLATE_CELL_BYTES,
            eval::Outcome::Returned(vec![1]),
            ISOLATE_CELL_BYTES,
        ),
        // R's own block + cell costs 48. Box[R] admits a further eight, so
        // seven remaining bytes refuse, and failure cleanup drops the resource.
        (
            format!(
                "resource type R:send {{ a:u64,b:u64 }}\n{}",
                capacity_refusal(
                    "let r = result.unwrap(R(a:7u64,b:11u64)); let _ = box.new(take r)",
                    8,
                    7
                )
            ),
            55 + ISOLATE_CELL_BYTES,
            eval::Outcome::Returned(vec![1]),
            ISOLATE_CELL_BYTES,
        ),
    ];
    for (module, ty) in [("rc", "Rc"), ("arc", "Arc")] {
        cases.push((format!("fn end(take h:{ty}[u64]) {{}}\nfn f() -> u64 {{ let h = {module}.new(7u64); let other = {module}.share(h); let last = match {module}.try_unwrap(take h) {{ Ok(_) => return 0u64, Error(take back) => back }}; end(take other); result.unwrap({module}.try_unwrap(take last)) }}"),16,eval::Outcome::Returned(vec![7]), 0));
        // The live eight-byte box leaves 23 bytes for a 24-byte counted node.
        // Refusal must drop the boxed payload as part of the surrounding value.
        let refusal = capacity_refusal(
            &format!(
                "let b = box.new(41u64); let data = Data(a:7u8,b:take b); let _ = {module}.new(take data)"
            ),
            24,
            23,
        );
        cases.push((
            format!("type Data:shareable+send {{ a:u8,b:Box[u64] }}\n{refusal}"),
            31 + ISOLATE_CELL_BYTES,
            eval::Outcome::Returned(vec![1]),
            ISOLATE_CELL_BYTES,
        ));
    }
    for (source, limit, expected, held) in cases {
        let (index, modules) = canonical(&source);
        let p = Program::declare(&index, &modules);
        assert!(p.refusals.is_empty(), "{source}: {:?}", p.refusals);
        let (module, functions) =
            crate::SourceProgram::from_inputs(&index, &modules, |d| d.package_is("app", "1"));
        let run = eval::run(&module, functions["f"], &[], limit);
        assert_eq!(run.outcome, expected, "{source}");
        assert_eq!(run.used, held, "{source}: leaked payload, holder or node");
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
fn generic_closure_environment_charges_and_refunds_the_closed_capture() {
    let source = r#"fn keep[T: plain + inert](x: T) -> own fn() -> T {
    fn() -> T { x }
}
fn f() -> u64 {
    let cb = keep((7u64, 11u64))
    let (a, b) = cb()
    a + b
}"#;
    let (module, functions) = crate::SourceProgram::lower(source, None);

    // Two u64 fields cost 16 bytes. The descriptor word is uncharged.
    let run = eval::run(&module, functions["f"], &[], 32);
    assert_eq!(run.outcome, eval::Outcome::Returned(vec![18]));
    assert_eq!(run.peak_reserved, 16);
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
fn secret_mac_authority_and_vectors() {
    let source = include_str!("../../../core/core/tests/secret_hmac.hirsel");
    let (module, functions) = crate::SourceProgram::lower(source, None);
    let result = eval::run(&module, functions["secret_hmac_law"], &[], 4096);
    assert_eq!(
        result.outcome,
        eval::Outcome::Returned(vec![0]),
        "MAC law return code"
    );
}
