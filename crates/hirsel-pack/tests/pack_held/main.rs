//! PACK-16 exercises the compiled producers and consumers in separate loaded images.
include!("../support/pack.rs");
hirsel_guard::install!();
mod host;

type Make = unsafe extern "C" fn(*mut Context, *mut usize, u64) -> u8;
type Take = unsafe extern "C" fn(*mut Context, *mut u64, *mut usize) -> u8;

fn plain(backend: Backend) {
    let _alone = alone();
    let (loader, _) = primary_loader();
    let a = compile_pack_module(
        "pub fn make(v: u64) -> Held[send] { code.hold(v) }",
        &["make"],
        backend,
    );
    let b = compile_pack_module(
        r#"
pub fn extract(take h: Held[send]) -> u64 {
    let r: Result[u64, (code.TakeError, Held[send])] = code.take_as(take h)
    match take r {
        Ok(v) => v,
        Error(_) => 0
    }
}
"#,
        &["extract"],
        backend,
    );
    loader.admit_module(&a.bytes).unwrap();
    loader.admit_module(&b.bytes).unwrap();
    let make = a.load_raw(loader, "make");
    // SAFETY: PACK-15 adapter for make(u64) -> Held[send].
    let produce: Make = unsafe { std::mem::transmute(make.entry()) };
    let mut word = 0usize;
    let mut output = 0;
    let mut after_reclaim = u64::MAX;
    let mut balance = (0, 0);
    let status = run_in_worker(1 << 20, |ctx| {
        balance.0 = unsafe { (*ctx).available() };
        let status = unsafe { produce(ctx, &mut word, 0xfeed_cafe) };
        drop(make);
        loader.reclaim().unwrap();
        after_reclaim = loader.installed();
        if status != 0 {
            return status;
        }
        let take = b.load_raw(loader, "extract");
        // SAFETY: PACK-15 passes the owning argument in its place.
        let extract: Take = unsafe { std::mem::transmute(take.entry()) };
        let status = unsafe { extract(ctx, &mut output, &mut word) };
        balance.1 = unsafe { (*ctx).available() };
        status
    });
    assert_eq!(status, Ok(0), "{backend:?}");
    assert_eq!(
        after_reclaim, 0,
        "{backend:?}: producer RX bytes after retirement"
    );
    assert_eq!(output, 0xfeed_cafe);
    assert_eq!(balance.0, balance.1);
    loader.reclaim().unwrap();
    assert_eq!(loader.installed(), 0);
}

#[test]
fn plain_cranelift() {
    plain(Backend::Cranelift);
}
#[test]
fn plain_llvm() {
    plain(Backend::Llvm);
}

fn resources(origin: &str, inner_type: &str) -> String {
    format!(
        r#"
pub type Inner {{ n: {inner_type} }}
pub resource type Child {{ inner: Inner }} drop(c) {{ fail("child-{origin}") }}
pub resource type Guard {{ child: Child }} drop(g) {{ fail("guard-{origin}") }}
pub type Bundle {{ guard: Guard, tag: u64 }}
"#
    )
}

fn resource(backend: Backend) {
    use hirsel_core_rt::{abi_sites, causes::Causes};
    let _alone = alone();
    let (loader, _) = primary_loader();
    let a = compile_pack_module(
        &(resources("A", "u64")
            + r#"
pub fn make() -> Held {
    let child = result.unwrap(Child(inner: Inner(n: 23)))
    let guard = result.unwrap(Guard(child: take child))
    code.hold(Bundle(guard: take guard, tag: 57))
}
"#),
        &["make"],
        backend,
    );
    let b = compile_pack_module(
        &(resources("B", "u64")
            + r#"
pub fn extract(take h: Held) -> Bundle {
    let r: Result[Bundle, (code.TakeError, Held)] = code.take_as(take h)
    match take r { Ok(take b) => b, Error(_) => fail("unexpected mismatch") }
}
pub fn finish(take b: Bundle) -> u64 { b.tag }
pub fn discard(take h: Held) -> u64 { 0 }
"#),
        &["extract", "finish", "discard"],
        backend,
    );
    let c = compile_pack_module(
        &(resources("C", "u32")
            + r#"
pub fn retry(take h: Held) -> Held {
    let r: Result[Bundle, (code.TakeError, Held)] = code.take_as(take h)
    match take r {
        Ok(_) => fail("unexpected match"),
        Error(take pair) => { let (_, h2) = take pair; h2 }
    }
}
"#),
        &["retry"],
        backend,
    );

    // Bundle holds a resource pointer and a u64. The adapters take owned
    // aggregate arguments by address and construct their result in caller storage.
    type MakeResource = unsafe extern "C" fn(*mut Context, *mut usize) -> u8;
    type Extract = unsafe extern "C" fn(*mut Context, *mut [usize; 2], *mut usize) -> u8;
    type Finish = unsafe extern "C" fn(*mut Context, *mut u64, *mut [usize; 2]) -> u8;
    type Retry = unsafe extern "C" fn(*mut Context, *mut usize, *mut usize) -> u8;
    for image in [&a, &b, &c] {
        loader.admit_module(&image.bytes).unwrap();
    }
    for extract_first in [false, true] {
        let make = a.load_raw(loader, "make");
        let retry = c.load_raw(loader, "retry");
        let extract = b.load_raw(loader, "extract");
        let finish = b.load_raw(loader, "finish");
        let discard = b.load_raw(loader, "discard");
        let origin_pc = make.entry();
        let mut trace = vec![];
        let mut balances = [0, 0];
        let mut still_pinned = [false; 2];
        let mut kept_owner = false;
        let status = run_in_worker(1 << 20, |ctx| unsafe {
            balances[0] = (*ctx).available();
            let mut held = 0;
            let produce: MakeResource = std::mem::transmute(origin_pc);
            let s = produce(ctx, &mut held);
            drop(make);
            if s != 0 {
                return s;
            }
            loader.reclaim().unwrap();
            still_pinned[0] = loader.resolve(origin_pc).is_some();
            let original = held;
            let mut again = 0;
            let mismatch: Retry = std::mem::transmute(retry.entry());
            let s = mismatch(ctx, &mut again, &mut held);
            if s != 0 {
                return s;
            }
            kept_owner = again == original;
            let mut out = 0;
            let s = if extract_first {
                let restore: Extract = std::mem::transmute(extract.entry());
                let mut bundle = [0; 2];
                let s = restore(ctx, &mut bundle, &mut again);
                if s != 0 {
                    return s;
                }
                loader.reclaim().unwrap();
                still_pinned[1] = loader.resolve(origin_pc).is_some();
                let release: Finish = std::mem::transmute(finish.entry());
                release(ctx, &mut out, &mut bundle)
            } else {
                let release: Take = std::mem::transmute(discard.entry());
                release(ctx, &mut out, &mut again)
            };
            if s != 1 {
                return 2;
            }
            let raw = hirsel_core_rt::abi::hirsel_core_causes_take(ctx);
            let causes = Causes::from_raw(raw);
            for cell in causes.iter_cells() {
                trace.push(abi_sites::format_cell(cell, std::ptr::null()));
            }
            drop(causes);
            balances[1] = (*ctx).available();
            0
        });
        assert_eq!(status, Ok(0), "{backend:?}");
        assert!(kept_owner);
        assert!(still_pinned[0]);
        if extract_first {
            assert!(still_pinned[1]);
        }
        assert_eq!(trace.len(), 2, "{trace:?}");
        assert!(trace[0].contains("guard-A"), "{trace:?}");
        assert!(trace[1].contains("child-A"), "{trace:?}");
        assert_eq!(balances[0], balances[1]);
        drop((retry, extract, finish, discard));
        loader.reclaim().unwrap();
        assert_eq!(loader.installed(), 0);
    }
}

#[test]
fn resource_cranelift() {
    resource(Backend::Cranelift);
}
#[test]
fn resource_llvm() {
    resource(Backend::Llvm);
}

fn literal(backend: Backend) {
    use hirsel_core_rt::{abi_sites, causes::Causes};
    let _alone = alone();
    let (loader, _) = primary_loader();
    let a = compile_pack_module(
        r#"
pub fn text() -> String { "foreign origin" }
pub fn frozen() -> FrozenBytes { b"frozen-origin-bytes" }
"#,
        &["text", "frozen"],
        backend,
    );
    let b = compile_pack_module(
        r#"
pub fn erase(take s: String) -> Held { code.hold(take s) }
pub fn erase_frozen(take f: FrozenBytes) -> Held { code.hold(take f) }
"#,
        &["erase", "erase_frozen"],
        backend,
    );
    let c = compile_pack_module(
        r#"
pub fn restore(take h: Held) -> String {
    let r: Result[String, (code.TakeError, Held)] = code.take_as(take h)
    match take r { Ok(take s) => s, Error(_) => fail("wrong type") }
}
pub fn consume(take s: String) -> u64 { list.len(string.as_bytes(s)) }
pub fn grow_and_len(take s: String) -> u64 {
    let mut owned = take s
    string.push(mut owned, "!")
    list.len(string.as_bytes(owned))
}
pub fn fail_with(take s: String) -> u64 { fail(take s) }
pub fn restore_frozen(take h: Held) -> FrozenBytes {
    let r: Result[FrozenBytes, (code.TakeError, Held)] = code.take_as(take h)
    match take r { Ok(take f) => f, Error(_) => fail("wrong frozen") }
}
pub fn consume_frozen(take f: FrozenBytes) -> u64 {
    let copied: Result[u64, Capacity] = bound bytes 0 {
        let shared: FrozenBytes = clone(f)
        list.len(bytebuf.as_bytes(shared))
    }
    match copied { Ok(n) => n, Error(_) => 0u64 }
}
"#,
        &[
            "restore",
            "consume",
            "grow_and_len",
            "fail_with",
            "restore_frozen",
            "consume_frozen",
        ],
        backend,
    );
    for m in [&a, &b, &c] {
        loader.admit_module(&m.bytes).unwrap();
    }
    let text = a.load_raw(loader, "text");
    let frozen = a.load_raw(loader, "frozen");
    let text_pc = text.entry();
    let erase = b.load_raw(loader, "erase");
    let erase_frozen = b.load_raw(loader, "erase_frozen");
    let restore = c.load_raw(loader, "restore");
    let consume = c.load_raw(loader, "consume");
    let grow_and_len = c.load_raw(loader, "grow_and_len");
    let fail_with = c.load_raw(loader, "fail_with");
    let restore_frozen = c.load_raw(loader, "restore_frozen");
    let consume_frozen = c.load_raw(loader, "consume_frozen");
    type Text = unsafe extern "C" fn(*mut Context, *mut [usize; 3]) -> u8;
    type Erase = unsafe extern "C" fn(*mut Context, *mut usize, *mut [usize; 3]) -> u8;
    type Restore = unsafe extern "C" fn(*mut Context, *mut [usize; 3], *mut usize) -> u8;
    type Consume = unsafe extern "C" fn(*mut Context, *mut u64, *mut [usize; 3]) -> u8;
    let mut preserved = [false; 2];
    let mut literal_capacity = 0;
    let mut a_unloaded = false;
    let mut values = [0u64; 3];
    let mut fail_msg = String::new();
    let mut balance = [0, 0];
    let status = run_in_worker(1 << 20, |ctx| unsafe {
        balance[0] = (*ctx).available();
        let make: Text = std::mem::transmute(text.entry());
        let make_frozen: Text = std::mem::transmute(frozen.entry());
        let hold: Erase = std::mem::transmute(erase.entry());
        let hold_frozen: Erase = std::mem::transmute(erase_frozen.entry());
        let take_s: Restore = std::mem::transmute(restore.entry());
        let take_f: Restore = std::mem::transmute(restore_frozen.entry());
        let finish: Consume = std::mem::transmute(consume.entry());
        let grow: Consume = std::mem::transmute(grow_and_len.entry());
        let raise_s: Consume = std::mem::transmute(fail_with.entry());
        let finish_f: Consume = std::mem::transmute(consume_frozen.entry());
        let mut s1 = [0; 3];
        let mut s2 = [0; 3];
        let mut s3 = [0; 3];
        let mut f1 = [0; 3];
        for s in [&mut s1, &mut s2, &mut s3] {
            let st = make(ctx, s);
            if st != 0 {
                return st;
            }
        }
        let st = make_frozen(ctx, &mut f1);
        if st != 0 {
            return st;
        }
        let (mut h1, mut h2, mut h3, mut hf) = (0, 0, 0, 0);
        for (h, s) in [(&mut h1, &mut s1), (&mut h2, &mut s2), (&mut h3, &mut s3)] {
            let st = hold(ctx, h, s);
            if st != 0 {
                return st;
            }
        }
        let st = hold_frozen(ctx, &mut hf, &mut f1);
        if st != 0 {
            return st;
        }
        // Unload A completely while its owned String & FrozenBytes literals survive in Held!
        drop((text, frozen));
        loader.reclaim().unwrap();
        a_unloaded = loader.resolve(text_pc).is_none();

        let (mut r1, mut r2, mut r3, mut rf) = ([0; 3], [0; 3], [0; 3], [0; 3]);
        for (r, h) in [(&mut r1, &mut h1), (&mut r2, &mut h2), (&mut r3, &mut h3)] {
            let st = take_s(ctx, r, h);
            if st != 0 {
                return st;
            }
        }
        let st = take_f(ctx, &mut rf, &mut hf);
        if st != 0 {
            return st;
        }
        let (lo, hi) = loader.data_band();
        literal_capacity = r1[2];
        preserved[0] = !(lo..hi).contains(&r1[0]);
        preserved[1] = !(lo..hi).contains(&rf[0]);
        let st = finish(ctx, &mut values[0], &mut r1);
        if st != 0 {
            return st;
        }
        let st = grow(ctx, &mut values[1], &mut r2);
        if st != 0 {
            return st;
        }
        let st = finish_f(ctx, &mut values[2], &mut rf);
        if st != 0 {
            return st;
        }
        balance[1] = (*ctx).available();
        let mut ignored = 0;
        let st = raise_s(ctx, &mut ignored, &mut r3);
        if st != 1 {
            return 2;
        }
        let raw = hirsel_core_rt::abi::hirsel_core_causes_take(ctx);
        let causes = Causes::from_raw(raw);
        for cell in causes.iter_cells() {
            fail_msg = abi_sites::format_cell(cell, std::ptr::null());
        }
        drop(causes);
        // Dropping the root task's primary cause cell refunds its prepaid 32 bytes.
        assert_eq!((*ctx).available(), balance[0] + 32);
        0
    });
    assert_eq!(status, Ok(0), "{backend:?}");
    assert!(
        a_unloaded,
        "{backend:?}: producer A did not unload while literals survived"
    );
    assert!(
        preserved[0],
        "{backend:?}: extracted String literal points into reclaimable data band"
    );
    assert!(
        preserved[1],
        "{backend:?}: extracted FrozenBytes literal points into reclaimable data band"
    );
    assert_eq!(
        (values, literal_capacity & 1),
        ([14, 15, 19], 1),
        "{backend:?}"
    );
    assert!(
        fail_msg.contains("foreign origin"),
        "{backend:?}: {fail_msg}"
    );
    assert_eq!(balance[0], balance[1], "{backend:?}");
    drop((
        erase,
        erase_frozen,
        restore,
        consume,
        grow_and_len,
        fail_with,
        restore_frozen,
        consume_frozen,
    ));
    loader.reclaim().unwrap();
    assert_eq!(loader.installed(), 0, "{backend:?}");
}
#[test]
fn literal_cranelift() {
    literal(Backend::Cranelift);
}
#[test]
fn literal_llvm() {
    literal(Backend::Llvm);
}

fn stop(backend: Backend) {
    let _alone = alone();
    // A separate pool makes the balance independent of any other stopped test.
    let loader = hirsel_pack::start(hirsel_pack::config(0, Mask::PURE, 16 << 20, 8 << 20)).unwrap();
    let _ = primary_loader();
    let a = compile_pack_module(
        &(resources("stopped", "u64")
            + r#"
pub fn make() -> Held {
    let child = result.unwrap(Child(inner: Inner(n: 23)))
    let guard = result.unwrap(Guard(child: take child))
    code.hold(Bundle(guard: take guard, tag: 57))
}
"#),
        &["make"],
        backend,
    );
    loader.admit_module(&a.bytes).unwrap();
    let make = a.load_raw(loader, "make");
    type MakeResource = unsafe extern "C" fn(*mut Context, *mut usize) -> u8;
    let produce: MakeResource = unsafe { std::mem::transmute(make.entry()) };
    let status = run_in_worker(1 << 20, |ctx| {
        let mut held = 0;
        let status = unsafe { produce(ctx, &mut held) };
        if status != 0 {
            return status;
        }
        hirsel_core_rt::run_stop::raise(hirsel_core_rt::run_stop::Stop::from_static(b"HeldStop"));
    });
    assert_eq!(
        status,
        Err(EndReason::RunStop(
            hirsel_core_rt::run_stop::Stop::from_static(b"HeldStop")
        ))
    );
    drop(make);
    loader.reclaim().unwrap();
    assert_eq!(
        loader.installed(),
        0,
        "{backend:?}: an abandoned Held retains executable owners"
    );
}
#[test]
fn stop_cranelift() {
    stop(Backend::Cranelift);
}
#[test]
fn stop_llvm() {
    stop(Backend::Llvm);
}

fn transfer_and_refusal(backend: Backend) {
    let _alone = alone();
    let (loader, _) = primary_loader();
    let app = compile_pack_module(
        r#"
fn extract(take h: Held[send]) -> u64 {
    let r: Result[u64, (code.TakeError, Held[send])] = code.take_as(take h)
    match take r { Ok(x) => x, Error(_) => 0 }
}
pub fn send(tasks: Tasks) -> u64 waits {
    let h: Held[send] = code.hold(987u64)
    let child = task.spawn(tasks, bytes 4096, take h, extract)
    match task.join(take child) { Ok(v) => v, Error(_) => 0 }
}
pub fn deny() -> u64 {
    let r = isolate(fn() -> u64 {
        let _ = bound bytes 0 { let h: Held = code.hold(123u64); 0u64 }
        0u64
    })
    match take r {
        Ok(Error(take causes)) => {
            for c in causes { match c.kind { Capacity(k) => { return k.requested }, _ => {} } }
            0
        },
        _ => 0
    }
}
"#,
        &["send", "deny"],
        backend,
    );
    loader.admit_module(&app.bytes).unwrap();
    let send = app.load_raw(loader, "send");
    let deny = app.load_raw(loader, "deny");
    type Deny = unsafe extern "C" fn(*mut Context, *mut u64) -> u8;
    type Send = unsafe extern "C" fn(*mut Context, *mut u64, u64) -> u8;
    let mut received = 0;
    let mut requested = 0;
    let mut funds = [0; 3];
    let st = run_in_worker(1 << 20, |ctx| unsafe {
        funds[0] = (*ctx).available();
        let transfer: Send = std::mem::transmute(send.entry());
        let refuse: Deny = std::mem::transmute(deny.entry());
        let s = transfer(ctx, &mut received, 0);
        if s != 0 {
            return s;
        }
        funds[1] = (*ctx).available();
        let s = refuse(ctx, &mut requested);
        funds[2] = (*ctx).available();
        s
    });
    assert_eq!(st, Ok(0), "{backend:?}");
    assert_eq!(received, 987);
    assert_eq!(requested, 40);
    assert_eq!(funds, [funds[0]; 3]);
    drop((send, deny));
    loader.reclaim().unwrap();
    assert_eq!(loader.installed(), 0);
}
#[test]
fn transfers_cranelift() {
    transfer_and_refusal(Backend::Cranelift);
}
#[test]
fn transfers_llvm() {
    transfer_and_refusal(Backend::Llvm);
}

#[test]
fn empty_dispatch_finishes_taken_arguments_in_reverse_order() {
    use hirsel_core_rt::{abi_sites, causes::Causes};

    let _alone = alone();
    let (loader, pool) = primary_loader();
    for backend in BACKENDS {
        let module = compile_pack_module(
            r#"
pub resource type Mark { n: u64 } drop(m) {
    if m.n == 1 { fail("first argument") }
    fail("second argument")
}
pub fn run(l: Loader) -> u64 {
    let slot: CodeSlot[fn(take Mark, take Mark) -> u64] = code.slot(l)
    let a = result.unwrap(Mark(n: 1))
    let b = result.unwrap(Mark(n: 2))
    let _ = code.dispatch(slot, take a, take b)
    0
}
"#,
            &["run"],
            backend,
        );
        loader.admit_module(&module.bytes).unwrap();
        let code = module.load_raw(loader, "run");
        let mut messages = Vec::new();
        let status = run_in_worker(1 << 20, |context| unsafe {
            let entry: unsafe extern "C" fn(*mut Context, *mut u64, u64) -> u8 =
                std::mem::transmute(code.entry());
            let status = entry(context, &mut 0, pool);
            let raw = hirsel_core_rt::abi::hirsel_core_causes_take(context);
            let causes = Causes::from_raw(raw);
            for cell in causes.iter_cells() {
                messages.push(abi_sites::format_cell(cell, std::ptr::null()));
            }
            u8::from(status != 1)
        });
        assert_eq!(status, Ok(0));
        assert_eq!(messages.len(), 2, "{backend:?} {messages:?}");
        assert!(messages[0].contains("second argument"), "{messages:?}");
        assert!(messages[1].contains("first argument"), "{messages:?}");
        drop(code);
        loader.reclaim().unwrap();
    }
}

fn recipe_lifetime(make_source: &str, finish_source: &str) {
    let _alone = alone();
    use hirsel_mod::Class;
    let mask = Mask::of(&[Class::Pure, Class::Metered, Class::Task, Class::Launch]);
    let loader = hirsel_pack::start(hirsel_pack::config(0, mask, 16 << 20, 8 << 20)).unwrap();
    let make_module = compile_pack_module(make_source, &["make"], Backend::Cranelift);
    let finish_module = compile_pack_module(finish_source, &["finish"], Backend::Cranelift);
    for module in [&make_module, &finish_module] {
        loader.admit_module(&module.bytes).unwrap();
    }
    let make = make_module.load_raw(loader, "make");
    let finish = finish_module.load_raw(loader, "finish");
    let producer_pc = make.entry();
    let mut result = 0;
    let status = run_in_worker(1 << 20, |ctx| unsafe {
        let create: Make = std::mem::transmute(producer_pc);
        let consume: Take = std::mem::transmute(finish.entry());
        let mut handle = 0;
        let status = create(ctx, &mut handle, 0);
        if status != 0 {
            return status;
        }
        drop(make);
        loader.reclaim().unwrap();
        if loader.resolve(producer_pc).is_none() {
            // Leave the worker before running a recipe from the retired producer.
            hirsel_core_rt::run_stop::raise(hirsel_core_rt::run_stop::Stop::from_static(
                b"MissingRecipePin",
            ));
        }
        consume(ctx, &mut result, &mut handle)
    });
    assert_eq!(status, Ok(0));
    assert_eq!(result, 42);
    drop(finish);
    loader.reclaim().unwrap();
    assert!(loader.resolve(producer_pc).is_none());
}

#[test]
fn task_keeps_producer_through_join() {
    recipe_lifetime(
        "fn child(take x: u64) -> u64 { x }
         pub fn make(tasks: Tasks) -> Task[u64] { task.spawn(tasks, bytes 4096, 42u64, child) }",
        "pub fn finish(take t: Task[u64]) -> u64 waits { result.unwrap(task.join(take t)) }",
    );
}

#[test]
fn channel_keeps_item_recipe_until_receiver_drop() {
    recipe_lifetime(
        "pub fn make(seed: u64) -> Receiver[String] {
            let (tx, rx): (Sender[String], Receiver[String]) = channel.new(1)
            let _ = channel.try_send(tx, \"forty-two\")
            rx
         }",
        "pub fn finish(take rx: Receiver[String]) -> u64 { 42 }",
    );
}

#[test]
fn latest_keeps_root_recipe_until_last_handle_drop() {
    recipe_lifetime(
        "pub fn make(seed: u64) -> Latest[u64] { latest.new(arc.new(42u64)) }",
        "pub fn finish(take head: Latest[u64]) -> u64 { 42 }",
    );
}

#[test]
fn parked_worker_releases_dropped_slot_image() {
    use hirsel_core_rt::{code, entry, host_ops, os, provider};
    let _alone = alone();
    let (loader, pool) = primary_loader();
    let module = compile_pack_module(
        "pub fn answer() -> u64 { 42 }",
        &["answer"],
        Backend::Cranelift,
    );
    loader.admit_module(&module.bytes).unwrap();
    let raw = module.load_raw(loader, "answer");
    let pc = raw.entry();
    let null_fd = || std::fs::File::open("/dev/null").unwrap().into();
    let mut os = os::OsProvider::new(null_fd(), null_fd());
    let provider = os.provider();
    let grant = entry::Grant {
        kind: entry::CLOCK,
        provider: &provider,
        handle: 0,
        terminal: false,
    };
    let mut retained = [false; 2];
    let status = run_with_grants(1 << 20, &[grant], |ctx| unsafe {
        let c = code::hirsel_core_code_bind(raw.descriptor() as *const u8);
        let slot = code::hirsel_core_code_slot_new(pool);
        assert_eq!(code::hirsel_core_code_activate(slot, 0, c, &mut 0), 0);
        let mut token = 0;
        assert_eq!(
            code::hirsel_core_code_dispatch_begin(slot, &mut token).addr(),
            pc
        );
        code::hirsel_core_code_dispatch_end(token);
        code::hirsel_core_code_slot_drop(slot);
        code::hirsel_core_code_drop(c);
        drop(raw);
        retained[0] = loader.pin_pc(pc).is_some();
        let now =
            host_ops::hirsel_core_host_clock(ctx, provider::MONO_NS, 1, std::ptr::null_mut(), 0, 0);
        let slept = host_ops::hirsel_core_host_sleep_until(ctx, 1, now.value + 40_000_000, 0);
        assert_eq!(slept.error.tag, 0);
        retained[1] = loader.pin_pc(pc).is_some();
        0
    });
    assert_eq!(status, Ok(0));
    assert_eq!(retained, [true, false]);
}
