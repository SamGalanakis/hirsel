use super::*;

// ============================================================================
// LAW 3: Same-carrier wrong signature + `waits` mismatch -> `NoEntry` with
//        installed image count and slab bytes strictly unchanged.
// ============================================================================

#[test]
fn law_3_same_carrier_wrong_signature_and_waits_mismatch_leave_slab_unchanged_on_both_backends() {
    let _alone = alone();
    let (loader, loader_id) = primary_loader();

    let callee_src = r#"
pub type Left { n: u64 }
pub type Right { n: u64 }

pub fn sync_entry(x: Left) -> u64 { x.n + 1 }
pub fn waits_entry(x: Left) -> u64 waits {
    let (tx, rx): (Sender[u64], Receiver[u64]) = channel.new(1)
    let _ = channel.send(tx, x.n)
    result.unwrap(channel.receive(rx)) + 2
}
"#;

    let caller_src = r#"
pub type Left { n: u64 }
pub type Right { n: u64 }

pub fn try_wrong_nominal(l: Loader, key: Inline[u8, 32], id: Inline[u8, 32]) -> u64 {
    let r: Result[Code[fn(Right) -> u64], code.LoadError] = code.load(l, key, id)
    match r {
        Error(NoEntry) => 1,
        _ => 0
    }
}

pub fn try_sync_as_waits(l: Loader, key: Inline[u8, 32], id: Inline[u8, 32]) -> u64 {
    let r: Result[Code[fn(Left) -> u64 waits], code.LoadError] = code.load(l, key, id)
    match r {
        Error(NoEntry) => 1,
        _ => 0
    }
}

pub fn try_waits_as_sync(l: Loader, key: Inline[u8, 32], id: Inline[u8, 32]) -> u64 {
    let r: Result[Code[fn(Left) -> u64], code.LoadError] = code.load(l, key, id)
    match r {
        Error(NoEntry) => 1,
        _ => 0
    }
}

pub fn load_and_call_both(
    l: Loader,
    key: Inline[u8, 32],
    sync_id: Inline[u8, 32],
    waits_id: Inline[u8, 32]
) -> u64 waits {
    let s: Code[fn(Left) -> u64] = result.unwrap(code.load(l, key, sync_id))
    let w: Code[fn(Left) -> u64 waits] = result.unwrap(code.load(l, key, waits_id))
    let a = s(Left(n: 40))
    let mut slot: CodeSlot[fn(Left) -> u64 waits] = code.slot(l)
    let _ = result.unwrap(code.activate(mut slot, 0, w))
    let b = result.unwrap(code.dispatch(slot, Left(n: 40)))
    a * 1000 + b
}
"#;

    for backend in BACKENDS {
        let _ = loader.reclaim();
        assert_eq!((loader.installed(), loader.live_images()), (0, 0));

        let callee_mod = compile_pack_module(callee_src, &["sync_entry", "waits_entry"], backend);
        let caller_mod = compile_pack_module(
            caller_src,
            &[
                "try_wrong_nominal",
                "try_sync_as_waits",
                "try_waits_as_sync",
                "load_and_call_both",
            ],
            backend,
        );
        // Admit callee_mod ONLY into the binding table — do NOT load it yet!
        loader.admit_module(&callee_mod.bytes).unwrap();
        loader.admit_module(&caller_mod.bytes).unwrap();

        let wrong_nom_code = caller_mod.load_raw(loader, "try_wrong_nominal");
        let sync_as_waits_code = caller_mod.load_raw(loader, "try_sync_as_waits");
        let waits_as_sync_code = caller_mod.load_raw(loader, "try_waits_as_sync");
        let both_code = caller_mod.load_raw(loader, "load_and_call_both");

        // Snapshot installed bytes and image count (only `caller_mod` is installed;
        // `callee_mod` is admitted but NOT yet published).
        let before_installed = loader.installed();
        let before_images = loader.live_images();
        assert_eq!(before_images, 1, "{backend:?}: only caller_mod installed");

        type CheckFn = unsafe extern "C" fn(
            *mut Context,
            *mut u64,
            u64,
            *const [u8; 32],
            *const [u8; 32],
        ) -> u8;
        type BothFn = unsafe extern "C" fn(
            *mut Context,
            *mut u64,
            u64,
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
        ) -> u8;

        let wrong_nom: CheckFn = unsafe { std::mem::transmute(wrong_nom_code.entry()) };
        let sync_as_waits: CheckFn = unsafe { std::mem::transmute(sync_as_waits_code.entry()) };
        let waits_as_sync: CheckFn = unsafe { std::mem::transmute(waits_as_sync_code.entry()) };
        let call_both: BothFn = unsafe { std::mem::transmute(both_code.entry()) };

        let callee_key = callee_mod.key.0;
        let sync_id = callee_mod.entry_identity("sync_entry").0;
        let waits_id = callee_mod.entry_identity("waits_entry").0;
        let absent_id = [0xef; 32];

        let st = run_in_worker(1 << 20, |ctx| {
            let mut out = 0u64;
            // 1. Same-carrier wrong nominal type (`Right` vs `Left`) -> NoEntry, zero publication.
            assert_eq!(
                unsafe { wrong_nom(ctx, &mut out, loader_id, &callee_key, &sync_id) },
                0
            );
            assert_eq!(
                out, 1,
                "{backend:?}: expected NoEntry on wrong nominal type"
            );
            assert_eq!(
                (loader.installed(), loader.live_images()),
                (before_installed, before_images),
                "{backend:?}: wrong nominal type must not publish image"
            );

            // 2. `waits` mismatch (requesting `waits` on sync entry) -> NoEntry, zero publication.
            out = 0;
            assert_eq!(
                unsafe { sync_as_waits(ctx, &mut out, loader_id, &callee_key, &sync_id) },
                0
            );
            assert_eq!(out, 1, "{backend:?}: expected NoEntry on sync-as-waits");
            assert_eq!(
                (loader.installed(), loader.live_images()),
                (before_installed, before_images),
                "{backend:?}: waits mismatch must not publish image"
            );

            // 3. `waits` mismatch (requesting sync on `waits` entry) -> NoEntry, zero publication.
            out = 0;
            assert_eq!(
                unsafe { waits_as_sync(ctx, &mut out, loader_id, &callee_key, &waits_id) },
                0
            );
            assert_eq!(out, 1, "{backend:?}: expected NoEntry on waits-as-sync");
            assert_eq!(
                (loader.installed(), loader.live_images()),
                (before_installed, before_images),
                "{backend:?}: sync mismatch must not publish image"
            );

            // 4. Absent entry identity -> NoEntry, zero publication.
            out = 0;
            assert_eq!(
                unsafe { waits_as_sync(ctx, &mut out, loader_id, &callee_key, &absent_id) },
                0
            );
            assert_eq!(out, 1, "{backend:?}: expected NoEntry on absent id");
            assert_eq!(
                (loader.installed(), loader.live_images()),
                (before_installed, before_images),
                "{backend:?}: absent id must not publish image"
            );

            // 5. Valid matching signatures load, run, and produce exact results (41 and 42).
            out = 0;
            assert_eq!(
                unsafe { call_both(ctx, &mut out, loader_id, &callee_key, &sync_id, &waits_id) },
                0
            );
            assert_eq!(out, 41_042, "{backend:?}: valid sync + waits calls");
            0
        });
        assert_eq!(st, Ok(0));

        drop(wrong_nom_code);
        drop(sync_as_waits_code);
        drop(waits_as_sync_code);
        drop(both_code);
        loader.reclaim().unwrap();
        assert_eq!((loader.installed(), loader.live_images()), (0, 0));
    }
}

// ============================================================================
// LAW 4: `code.slot`, `code.activate` (`WrongPool`, `Stale`, valid swap),
//        `code.dispatch` (`Empty`, hit, miss), reflection, and pinning across
//        nested calls, unwind, task suspension / worker migration, and Unit end.
// ============================================================================

#[test]
fn law_4_slot_activate_dispatch_and_pinning_on_both_backends() {
    let _alone = alone();
    let (loader_a, id_a) = primary_loader();
    let (loader_b, id_b) = secondary_loader();

    let v1_src = r#"
pub fn compute(x: u64) -> u64 { x + 100 }

pub fn boom(x: u64) -> u64 {
    if x == 0 {
        fail("dispatch-unwind-boom")
    }
    x
}

pub fn nested_step(mut s: CodeSlot[fn(u64) -> u64], v2: Code[fn(u64) -> u64], x: u64) -> u64 {
    let first = result.unwrap(code.dispatch(s, x))
    let _ = result.unwrap(code.activate(mut s, 1, v2))
    let second = result.unwrap(code.dispatch(s, x))
    first * 1000 + second
}

pub fn waiter(rx: Receiver[u64]) -> u64 waits {
    let v = result.unwrap(channel.receive(rx))
    v + 100
}

pub fn unit_body(take x: u64) -> u64 waits {
    let (tx, rx): (Sender[u64], Receiver[u64]) = channel.new(1)
    let _ = channel.send(tx, x)
    result.unwrap(channel.receive(rx)) * 7 + 3
}
"#;

    let v2_src = r#"
pub fn compute(x: u64) -> u64 { x + 200 }
pub fn waiter(rx: Receiver[u64]) -> u64 waits {
    let v = result.unwrap(channel.receive(rx))
    v + 200
}
"#;

    let host_src = r#"
pub fn local_fn(x: u64) -> u64 { x + 5 }

pub resource type DropMark {
    log: Sender[u64],
    tag: u64
} drop(t) {
    let _ = channel.try_send(t.log, t.tag)
}

pub fn run_slot_and_reflection(
    la: Loader,
    lb: Loader,
    k1: Inline[u8, 32],
    k2: Inline[u8, 32],
    id_compute1: Inline[u8, 32],
    id_compute2: Inline[u8, 32],
    id_boom: Inline[u8, 32],
    id_nested: Inline[u8, 32]
) -> u64 {
    let c1: Code[fn(u64) -> u64] = result.unwrap(code.load(la, k1, id_compute1))
    let c1_shared: Code[fn(u64) -> u64] = code.share(c1)
    let c2: Code[fn(u64) -> u64] = result.unwrap(code.load(la, k2, id_compute2))
    let cb: Code[fn(u64) -> u64] = result.unwrap(code.load(lb, k1, id_compute1))
    let c_boom: Code[fn(u64) -> u64] = result.unwrap(code.load(la, k1, id_boom))
    let c_nested: Code[fn(mut CodeSlot[fn(u64) -> u64], Code[fn(u64) -> u64], u64) -> u64] =
        result.unwrap(code.load(la, k1, id_nested))

    // Reflection on both loaded (`code.load`) and static (`code.bind`) handles.
    let bound_fn = code.bind(local_fn)
    if bound_fn(10) != 15 { return 1001 }
    if list.len(code.identity(bound_fn)) == 0 { return 1002 }
    if list.len(code.identity(c1)) == 0 { return 1003 }
    match code.revision(bound_fn) {
        Some(rev) => { if list.len(rev) != 32 { return 1004 } }
        None => { return 1005 }
    }
    match code.revision(c1) {
        Some(rev) => { if list.len(rev) != 32 { return 1006 } }
        None => { return 1007 }
    }

    // Empty slot dispatch returns Error(Empty) and drops taken resource args cleanly.
    let mut s: CodeSlot[fn(u64) -> u64] = code.slot(la)
    match code.dispatch(s, 10) {
        Error(Empty()) => {}
        _ => { return 1008 }
    }
    let (drop_tx, drop_rx): (Sender[u64], Receiver[u64]) = channel.new(4)
    let empty_take_slot: CodeSlot[fn(take DropMark) -> u64] = code.slot(la)
    let dm = result.unwrap(DropMark(log: channel.sender(drop_tx), tag: 9))
    match code.dispatch(empty_take_slot, take dm) {
        Error(Empty()) => {}
        _ => { return 1009 }
    }
    match channel.try_receive(drop_rx) {
        Ok(v) => { if v != 9 { return 1010 } }
        Error(_) => { return 1010 }
    }

    // Activate: WrongPool, Stale(0), valid 0->1, Stale(1).
    match code.activate(mut s, 0, cb) {
        Error(WrongPool) => {}
        _ => { return 1011 }
    }
    match code.activate(mut s, 7, c1) {
        Error(Stale(cur)) => { if cur != 0 { return 1012 } }
        _ => { return 1013 }
    }
    match code.activate(mut s, 0, c1) {
        Ok(ver) => { if ver != 1 { return 1014 } }
        _ => { return 1015 }
    }
    match code.activate(mut s, 0, c2) {
        Error(Stale(cur)) => { if cur != 1 { return 1016 } }
        _ => { return 1017 }
    }

    // Dispatch miss then hit through worker cache.
    if result.unwrap(code.dispatch(s, 10)) != 110 { return 1018 }
    if result.unwrap(code.dispatch(s, 10)) != 110 { return 1019 }
    if c1_shared(10) != 110 { return 1020 }

    // Nested dispatch + live swap from v1 -> v2 inside loaded callee while outer call runs.
    let nested_val = c_nested(mut s, c2, 5)
    if nested_val != 105_205 { return 1021 }

    // Swap to failing callee and unwind across `code.dispatch`.
    match code.activate(mut s, 2, c_boom) {
        Ok(ver) => { if ver != 3 { return 1022 } }
        _ => { return 1023 }
    }
    let unwound = isolate(fn() -> Result[u64, code.DispatchError] {
        code.dispatch(s, 0)
    })
    match unwound {
        Ok(Error(_)) => {}
        _ => { return 1024 }
    }
    0
}

pub fn run_suspend_migration(
    tasks: Tasks,
    la: Loader,
    k1: Inline[u8, 32],
    k2: Inline[u8, 32],
    id_w1: Inline[u8, 32],
    id_w2: Inline[u8, 32]
) -> u64 waits {
    let w1: Code[fn(Receiver[u64]) -> u64 waits] = result.unwrap(code.load(la, k1, id_w1))
    let w2: Code[fn(Receiver[u64]) -> u64 waits] = result.unwrap(code.load(la, k2, id_w2))
    let mut slot: CodeSlot[fn(Receiver[u64]) -> u64 waits] = code.slot(la)
    let _ = result.unwrap(code.activate(mut slot, 0, w1))

    let (tx, rx): (Sender[u64], Receiver[u64]) = channel.new(1)
    let (done_tx, done_rx): (Sender[u64], Receiver[u64]) = channel.new(1)
    // Spawn a child task that dispatches `w1` through `slot` and suspends on `rx`!
    // Suspension promotes its worker borrow to `Task.code_pins`.
    let arg = (take slot, take rx, take done_tx)
    let child = task.spawn(
        tasks,
        bytes 65536,
        take arg,
        fn(take t_arg: (CodeSlot[fn(Receiver[u64]) -> u64 waits], Receiver[u64], Sender[u64])) -> u64 waits {
            let (t_slot, t_rx, t_done_tx) = take t_arg
            let ans = result.unwrap(code.dispatch(t_slot, t_rx))
            let _ = channel.send(t_done_tx, ans)
            ans
        }
    )
    // Yield / send after child suspends, while swapping `slot`? Even stronger:
    // keep `w2` ready, send `42`, join child, verify 142; also verify swap+resume!
    let _ = channel.send(tx, 42)
    let from_done = result.unwrap(channel.receive(done_rx))
    let joined = result.unwrap(task.join(take child))
    // Drop `w1` explicitly, verify `w2` works too.
    drop(take w1)
    let mut slot2: CodeSlot[fn(Receiver[u64]) -> u64 waits] = code.slot(la)
    let _ = result.unwrap(code.activate(mut slot2, 0, w2))
    let (tx2, rx2): (Sender[u64], Receiver[u64]) = channel.new(1)
    let _ = channel.send(tx2, 7)
    let v2_ans = result.unwrap(code.dispatch(slot2, rx2))
    if from_done != 142 || joined != 142 || v2_ans != 207 {
        return 1
    }
    0
}

pub fn run_unit_end_pinning(
    tasks: Tasks,
    la: Loader,
    k1: Inline[u8, 32],
    id_unit: Inline[u8, 32]
) -> u64 waits {
    let unit_code: Code[fn(take u64) -> u64 waits] = result.unwrap(code.load(la, k1, id_unit))
    // Run a loaded function that itself spawns a Unit on loaded code inside `v1`.
    let _ = unit_code
    let h = result.unwrap(task.spawn_unit(tasks, bytes 65536, 6u64, unit_local))
    let got = result.unwrap(task.join(take h))
    if got != 45 { return 1 }
    0
}

fn unit_local(take x: u64) -> u64 waits {
    let (tx, rx): (Sender[u64], Receiver[u64]) = channel.new(1)
    let _ = channel.send(tx, x)
    result.unwrap(channel.receive(rx)) * 7 + 3
}
"#;

    for backend in BACKENDS {
        let _ = loader_a.reclaim();
        let _ = loader_b.reclaim();
        assert_eq!(loader_a.installed(), 0, "{backend:?}: clean loader_a");
        assert_eq!(loader_b.installed(), 0, "{backend:?}: clean loader_b");

        let v1_mod = compile_pack_module(
            v1_src,
            &["compute", "boom", "nested_step", "waiter", "unit_body"],
            backend,
        );
        let v2_mod = compile_pack_module(v2_src, &["compute", "waiter"], backend);
        let host_mod = compile_pack_module(
            host_src,
            &[
                "run_slot_and_reflection",
                "run_suspend_migration",
                "run_unit_end_pinning",
            ],
            backend,
        );

        loader_a.admit_module(&v1_mod.bytes).unwrap();
        loader_a.admit_module(&v2_mod.bytes).unwrap();
        loader_a.admit_module(&host_mod.bytes).unwrap();
        loader_b.admit_module(&v1_mod.bytes).unwrap();

        let slot_fn_code = host_mod.load_raw(loader_a, "run_slot_and_reflection");
        let suspend_fn_code = host_mod.load_raw(loader_a, "run_suspend_migration");
        let unit_fn_code = host_mod.load_raw(loader_a, "run_unit_end_pinning");

        type SlotTestFn = unsafe extern "C" fn(
            *mut Context,
            *mut u64,
            u64,
            u64,
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
        ) -> u8;
        type SuspendFn = unsafe extern "C" fn(
            *mut Context,
            *mut u64,
            u64,
            u64,
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
        ) -> u8;
        type UnitFn = unsafe extern "C" fn(
            *mut Context,
            *mut u64,
            u64,
            u64,
            *const [u8; 32],
            *const [u8; 32],
        ) -> u8;

        let slot_fn: SlotTestFn = unsafe { std::mem::transmute(slot_fn_code.entry()) };
        let suspend_fn: SuspendFn = unsafe { std::mem::transmute(suspend_fn_code.entry()) };
        let unit_fn: UnitFn = unsafe { std::mem::transmute(unit_fn_code.entry()) };

        let k1 = v1_mod.key.0;
        let k2 = v2_mod.key.0;
        let id_c1 = v1_mod.entry_identity("compute").0;
        let id_c2 = v2_mod.entry_identity("compute").0;
        let id_boom = v1_mod.entry_identity("boom").0;
        let id_nested = v1_mod.entry_identity("nested_step").0;
        let id_w1 = v1_mod.entry_identity("waiter").0;
        let id_w2 = v2_mod.entry_identity("waiter").0;
        let id_unit = v1_mod.entry_identity("unit_body").0;

        let mut slot_res = (0u8, 999u64, 0u32);
        let mut suspend_res = (0u8, 999u64, 0u32);
        let mut unit_res = (0u8, 999u64);
        let st = run_in_worker(2 << 20, |ctx| {
            let mut out = 999u64;
            let s1 = unsafe {
                slot_fn(
                    ctx, &mut out, id_a, id_b, &k1, &k2, &id_c1, &id_c2, &id_boom, &id_nested,
                )
            };
            slot_res = (
                s1,
                out,
                hirsel_pack::runtime::current_thread_active_borrows(),
            );
            if s1 != 0 || out != 0 {
                return 0;
            }

            out = 999;
            let s2 = unsafe { suspend_fn(ctx, &mut out, 0, id_a, &k1, &k2, &id_w1, &id_w2) };
            suspend_res = (
                s2,
                out,
                hirsel_pack::runtime::current_thread_active_borrows(),
            );
            if s2 != 0 || out != 0 {
                return 0;
            }

            out = 999;
            let s3 = unsafe { unit_fn(ctx, &mut out, 0, id_a, &k1, &id_unit) };
            unit_res = (s3, out);
            0
        });
        assert_eq!(st, Ok(0), "{backend:?}: law 4 worker exit");
        assert_eq!(slot_res.0, 0, "{backend:?}: run_slot_and_reflection status");
        assert_eq!(
            slot_res.1, 0,
            "{backend:?}: run_slot_and_reflection code {}",
            slot_res.1
        );
        assert_eq!(
            slot_res.2, 0,
            "{backend:?}: zero leaked worker borrows after unwind & nested dispatch"
        );
        assert_eq!(
            suspend_res.0, 0,
            "{backend:?}: run_suspend_migration status"
        );
        assert_eq!(
            suspend_res.1, 0,
            "{backend:?}: run_suspend_migration code {}",
            suspend_res.1
        );
        assert_eq!(
            suspend_res.2, 0,
            "{backend:?}: zero leaked worker borrows after task suspension"
        );
        assert_eq!(unit_res.0, 0, "{backend:?}: run_unit_end_pinning status");
        assert_eq!(
            unit_res.1, 0,
            "{backend:?}: run_unit_end_pinning code {}",
            unit_res.1
        );

        // Drop all handles and park worker tables -> 100% of RX reclaims cleanly!
        drop(slot_fn_code);
        drop(suspend_fn_code);
        drop(unit_fn_code);
        hirsel_core_rt::code::worker_park();
        loader_a.reclaim().unwrap();
        loader_b.reclaim().unwrap();
        assert_eq!(
            loader_a.installed(),
            0,
            "{backend:?}: loader_a must reclaim to 0 installed bytes"
        );
        assert_eq!(
            loader_b.installed(),
            0,
            "{backend:?}: loader_b must reclaim to 0 installed bytes"
        );
    }
}
