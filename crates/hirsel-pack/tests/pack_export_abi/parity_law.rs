use super::*;

// ============================================================================
// LAW 2: By-value arg mutation before failure, partial result + pending-result
//        finalizer failure, cleanup trace, cause chain, and bound/payer parity
//        against identically compiled linked calls.
// ============================================================================

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParitySummary {
    orig_a: u64,
    orig_b: u64,
    orig_c: u64,
    orig_d: u64,
    mut_len: u64,
    mut_first: u64,
    trace1: u64,
    causes1_count: u64,
    causes1_fail_len: u64,
    trace2: u64,
    causes2_count: u64,
    causes2_div: u64,
    steps_spent: u64,
    bytes_refused: u64,
}

#[test]
fn law_2_by_value_mutation_partial_result_finalizer_and_bound_parity_on_both_backends() {
    let _alone = alone();
    let (loader, loader_id) = primary_loader();

    let src = r#"
pub type PlainBuf {
    a: u64,
    b: u64,
    c: u64,
    d: u64
}

pub resource type Guard {
    log: Sender[u64],
    tag: u64,
    fail_div: u64
} drop(t) {
    let _ = channel.try_send(t.log, t.tag)
    let _ = 100u64 / t.fail_div
}

pub type PartialRes {
    g1: Guard,
    payload: u64
}

pub type ParitySummary {
    orig_a: u64,
    orig_b: u64,
    orig_c: u64,
    orig_d: u64,
    mut_len: u64,
    mut_first: u64,
    trace1: u64,
    causes1_count: u64,
    causes1_fail_len: u64,
    trace2: u64,
    causes2_count: u64,
    causes2_div: u64,
    steps_spent: u64,
    bytes_refused: u64
}

fn drain_trace(rx: Receiver[u64]) -> u64 {
    let mut acc: u64 = 0
    loop {
        match channel.try_receive(rx) {
            Ok(v) => { acc = acc * 10 + v }
            Error(_) => { break }
        }
    }
    acc
}

pub fn mutate_and_fail(take p: PlainBuf, out: Cell[u64], log: Sender[u64]) -> PartialRes {
    let mut local = p
    local.a = 999
    local.b = 888
    cell.set(out, 77)
    let _g = result.unwrap(Guard(log: channel.sender(log), tag: 3, fail_div: 1))
    let _tmp: Bytes = b"temporary-allocated-payload"
    fail("callee-boom")
}

pub fn push_mut_list(mut out: List[u64], v: u64) -> u64 {
    list.push(mut out, v)
    list.len(out)
}

pub fn pending_result_fin_fail(log: Sender[u64]) -> PartialRes {
    let _local_failing = result.unwrap(Guard(log: channel.sender(log), tag: 4, fail_div: 0))
    let g1 = result.unwrap(Guard(log: channel.sender(log), tag: 5, fail_div: 1))
    PartialRes(g1: take g1, payload: 42)
}

pub fn bounded_steps_callee(n: u64) -> u64 {
    let mut i: u64 = 0
    let mut acc: u64 = 0
    while i < n {
        acc += i
        i += 1
    }
    acc
}

pub fn bounded_bytes_callee() -> u64 {
    let literal: Bytes = b"abcdefghi"
    list.len(literal)
}

pub fn run_linked() -> ParitySummary {
    let orig = PlainBuf(a: 10, b: 20, c: 30, d: 40)
    let mut out_list: List[u64] = list.with_capacity(4)
    let mut_len = push_mut_list(mut out_list, 77)
    let out_cell = cell.new(0u64)
    let (log1_tx, log1_rx): (Sender[u64], Receiver[u64]) = channel.new(8)
    let res1 = isolate(fn() -> PartialRes {
        mutate_and_fail(orig, out_cell, log1_tx)
    })
    let mut causes1_count: u64 = 0
    let mut causes1_fail_len: u64 = 0
    match res1 {
        Ok(Error(take causes)) => {
            for c in causes {
                causes1_count += 1
                match c.kind {
                    Fail(s) => { causes1_fail_len = list.len(string.as_bytes(s)) }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    let (log2_tx, log2_rx): (Sender[u64], Receiver[u64]) = channel.new(8)
    let res2 = isolate(fn() -> PartialRes {
        pending_result_fin_fail(log2_tx)
    })
    let mut causes2_count: u64 = 0
    let mut causes2_div: u64 = 0
    match res2 {
        Ok(Error(take causes)) => {
            for c in causes {
                causes2_count += 1
                match c.kind {
                    Division => { causes2_div += 1 }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    let steps_spent: u64 = match (bound steps 12 { bounded_steps_callee(1000) }) {
        Error(e) => e.steps,
        Ok(_) => 0
    }

    let bytes_attempt = isolate(fn() -> Result[u64, Capacity] {
        bound bytes 15 {
            bounded_bytes_callee()
        }
    })
    let bytes_refused: u64 = match bytes_attempt {
        Ok(Error(take causes)) => {
            let mut found: u64 = 0
            for c in causes {
                match c.kind {
                    Capacity(k) => { found = k.requested * 100 + k.available }
                    _ => {}
                }
            }
            found
        }
        _ => 0
    }

    let first_out: u64 = match list.get(out_list, 0) {
        Some(v) => v,
        None => 0
    }
    ParitySummary(
        orig_a: orig.a,
        orig_b: orig.b,
        orig_c: orig.c,
        orig_d: orig.d,
        mut_len: mut_len,
        mut_first: if first_out == cell.get(out_cell) { first_out } else { 0 },
        trace1: drain_trace(log1_rx),
        causes1_count: causes1_count,
        causes1_fail_len: causes1_fail_len,
        trace2: drain_trace(log2_rx),
        causes2_count: causes2_count,
        causes2_div: causes2_div,
        steps_spent: steps_spent,
        bytes_refused: bytes_refused
    )
}

pub fn run_loaded(
    l: Loader,
    key: Inline[u8, 32],
    id1: Inline[u8, 32],
    id2: Inline[u8, 32],
    id3: Inline[u8, 32],
    id4: Inline[u8, 32],
    id5: Inline[u8, 32]
) -> ParitySummary {
    let c1: Code[fn(take PlainBuf, Cell[u64], Sender[u64]) -> PartialRes] =
        result.unwrap(code.load(l, key, id1))
    let c2: Code[fn(Sender[u64]) -> PartialRes] =
        result.unwrap(code.load(l, key, id2))
    let c3: Code[fn(u64) -> u64] =
        result.unwrap(code.load(l, key, id3))
    let c4: Code[fn() -> u64] =
        result.unwrap(code.load(l, key, id4))
    let c5: Code[fn(mut List[u64], u64) -> u64] =
        result.unwrap(code.load(l, key, id5))

    let orig = PlainBuf(a: 10, b: 20, c: 30, d: 40)
    let mut out_list: List[u64] = list.with_capacity(4)
    let mut_len = c5(mut out_list, 77)
    let out_cell = cell.new(0u64)
    let (log1_tx, log1_rx): (Sender[u64], Receiver[u64]) = channel.new(8)
    let res1 = isolate(fn() -> PartialRes {
        c1(orig, out_cell, log1_tx)
    })
    let mut causes1_count: u64 = 0
    let mut causes1_fail_len: u64 = 0
    match res1 {
        Ok(Error(take causes)) => {
            for c in causes {
                causes1_count += 1
                match c.kind {
                    Fail(s) => { causes1_fail_len = list.len(string.as_bytes(s)) }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    let (log2_tx, log2_rx): (Sender[u64], Receiver[u64]) = channel.new(8)
    let res2 = isolate(fn() -> PartialRes {
        c2(log2_tx)
    })
    let mut causes2_count: u64 = 0
    let mut causes2_div: u64 = 0
    match res2 {
        Ok(Error(take causes)) => {
            for c in causes {
                causes2_count += 1
                match c.kind {
                    Division => { causes2_div += 1 }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    let steps_spent: u64 = match (bound steps 12 { c3(1000) }) {
        Error(e) => e.steps,
        Ok(_) => 0
    }

    let bytes_attempt = isolate(fn() -> Result[u64, Capacity] {
        bound bytes 15 {
            c4()
        }
    })
    let bytes_refused: u64 = match bytes_attempt {
        Ok(Error(take causes)) => {
            let mut found: u64 = 0
            for c in causes {
                match c.kind {
                    Capacity(k) => { found = k.requested * 100 + k.available }
                    _ => {}
                }
            }
            found
        }
        _ => 0
    }

    let first_out: u64 = match list.get(out_list, 0) {
        Some(v) => v,
        None => 0
    }
    ParitySummary(
        orig_a: orig.a,
        orig_b: orig.b,
        orig_c: orig.c,
        orig_d: orig.d,
        mut_len: mut_len,
        mut_first: if first_out == cell.get(out_cell) { first_out } else { 0 },
        trace1: drain_trace(log1_rx),
        causes1_count: causes1_count,
        causes1_fail_len: causes1_fail_len,
        trace2: drain_trace(log2_rx),
        causes2_count: causes2_count,
        causes2_div: causes2_div,
        steps_spent: steps_spent,
        bytes_refused: bytes_refused
    )
}

pub fn raise_direct() -> u64 {
    fail("pack97-loaded-provenance")
}
"#;

    for backend in BACKENDS {
        let _ = loader.reclaim();
        assert_eq!(loader.installed(), 0, "{backend:?}: clean slab start");

        let compiled = compile_pack_module(
            src,
            &[
                "mutate_and_fail",
                "pending_result_fin_fail",
                "bounded_steps_callee",
                "bounded_bytes_callee",
                "push_mut_list",
                "run_linked",
                "run_loaded",
                "raise_direct",
            ],
            backend,
        );
        loader.admit_module(&compiled.bytes).unwrap();
        let linked_code = compiled.load_raw(loader, "run_linked");
        let loaded_code = compiled.load_raw(loader, "run_loaded");

        type RunLinkedFn = unsafe extern "C" fn(*mut Context, *mut ParitySummary) -> u8;
        type RunLoadedFn = unsafe extern "C" fn(
            *mut Context,
            *mut ParitySummary,
            u64,
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
            *const [u8; 32],
        ) -> u8;

        let run_linked_fn: RunLinkedFn = unsafe { std::mem::transmute(linked_code.entry()) };
        let run_loaded_fn: RunLoadedFn = unsafe { std::mem::transmute(loaded_code.entry()) };

        let key = compiled.key.0;
        let id1 = compiled.entry_identity("mutate_and_fail").0;
        let id2 = compiled.entry_identity("pending_result_fin_fail").0;
        let id3 = compiled.entry_identity("bounded_steps_callee").0;
        let id4 = compiled.entry_identity("bounded_bytes_callee").0;
        let id5 = compiled.entry_identity("push_mut_list").0;

        let mut linked_summary = MaybeUninit::<ParitySummary>::uninit();
        let mut loaded_summary = MaybeUninit::<ParitySummary>::uninit();
        let mut linked_avail_delta = 0u64;
        let mut loaded_avail_delta = 0u64;

        let st = run_in_worker(1 << 20, |ctx| {
            let before_linked = unsafe { (*ctx).available() };
            let s1 = unsafe { run_linked_fn(ctx, linked_summary.as_mut_ptr()) };
            assert_eq!(s1, 0, "{backend:?}: run_linked failed");
            linked_avail_delta = before_linked - unsafe { (*ctx).available() };

            let before_loaded = unsafe { (*ctx).available() };
            let s2 = unsafe {
                run_loaded_fn(
                    ctx,
                    loaded_summary.as_mut_ptr(),
                    loader_id,
                    &key,
                    &id1,
                    &id2,
                    &id3,
                    &id4,
                    &id5,
                )
            };
            assert_eq!(s2, 0, "{backend:?}: run_loaded failed");
            loaded_avail_delta = before_loaded - unsafe { (*ctx).available() };
            0
        });
        assert_eq!(st, Ok(0), "{backend:?}: parity worker exit");

        let linked = unsafe { linked_summary.assume_init() };
        let loaded = unsafe { loaded_summary.assume_init() };
        assert_eq!(
            loaded, linked,
            "{backend:?}: loaded PACK-15 call must match identically compiled linked call"
        );
        // Verify every semantic invariant actually fired (not vacuous zeros!):
        assert_eq!(
            (loaded.orig_a, loaded.orig_b, loaded.orig_c, loaded.orig_d),
            (10, 20, 30, 40),
            "{backend:?}: original by-value plain arg must remain intact after callee mutation + failure"
        );
        assert_eq!(
            (loaded.mut_len, loaded.mut_first),
            (1, 77),
            "{backend:?}: legal mut write before failure must be preserved"
        );
        assert_eq!(loaded.trace1, 3, "{backend:?}: local guard cleaned on fail");
        assert_eq!(loaded.causes1_count, 1);
        assert_eq!(loaded.causes1_fail_len, "callee-boom".len() as u64);
        // Pending result `g1` (tag 5) AND local failing guard (tag 4) both cleaned!
        assert!(
            loaded.trace2 == 45 || loaded.trace2 == 54,
            "{backend:?}: expected both pending result (5) and local guard (4) cleaned, got {}",
            loaded.trace2
        );
        assert_eq!(loaded.causes2_count, 1);
        assert_eq!(loaded.causes2_div, 1);
        assert_eq!(loaded.steps_spent, 12);
        assert_eq!(loaded.bytes_refused, 1615);
        assert_eq!(
            loaded_avail_delta, linked_avail_delta,
            "{backend:?}: payer balance delta must match linked call"
        );

        // PACK-9.7 provenance: raise from loaded module, unload + reclaim 100% of RX,
        // and verify formatted cause message + source site survive reclaim intact.
        drop(linked_code);
        drop(loaded_code);
        let raise_code = compiled.load_raw(loader, "raise_direct");
        type RaiseFn = unsafe extern "C" fn(*mut Context, *mut u64) -> u8;
        let raise_fn: RaiseFn = unsafe { std::mem::transmute(raise_code.entry()) };

        let mut raw_chain: usize = 0;
        let st_raise = run_in_worker(1 << 20, |ctx| {
            let mut out = 0u64;
            let failed = unsafe { raise_fn(ctx, &mut out) };
            assert_eq!(failed, 1, "{backend:?}: raise_direct must fail");
            let chain = unsafe { hirsel_core_rt::abi::hirsel_core_causes_take(ctx) };
            raw_chain = chain.addr();
            0
        });
        assert_eq!(st_raise, Ok(0));
        drop(raise_code);
        loader.reclaim().unwrap();
        assert_eq!(
            loader.installed(),
            0,
            "{backend:?}: 100% of RX must reclaim while loaded cause chain survives"
        );
        let chain = unsafe { Causes::from_raw(std::ptr::with_exposed_provenance_mut(raw_chain)) };
        let cells: Vec<_> = chain.iter_cells().collect();
        assert_eq!(cells.len(), 1);
        let formatted = unsafe { abi_sites::format_cell(cells[0], std::ptr::null()) };
        assert!(
            formatted.contains("Fail(\"pack97-loaded-provenance\")")
                && formatted.contains("main.hirsel:"),
            "{backend:?}: unexpected formatted cause after RX reclaim: {formatted}"
        );
        drop(chain);
    }
}
