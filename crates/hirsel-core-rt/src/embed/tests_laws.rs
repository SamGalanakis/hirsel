//! Slice 2 embedded ABI law tests (M13, B1, B2, M1, M11, M3, M8, Foreign wait).
use super::*;
use crate::embed::{codec, register_program};

// ---------------------------------------------------------------------------
// Law M13: Validation & reservation order, guard-page short struct, zero tail,
// stack callback copy, reentrant calls inside sync refusal `ended`.
// ---------------------------------------------------------------------------

#[test]
fn law_m13_validation_guard_page_zero_tail_and_reentrant_refusal() {
    let _serial = TEST_SERIAL.lock().unwrap();
    register_program(&MOCK_PROGRAM);
    *MOCK_MODE.lock().unwrap() = MockMode::Normal;
    assert_eq!(hirsel_embed_v1_abi(), 1);
    let m = hirsel_embed_v1_manifest();
    // SAFETY: manifest static slice.
    assert_eq!(unsafe { m.as_slice().unwrap() }, TEST_MANIFEST);

    let fp = manifest_fingerprint(TEST_MANIFEST).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().to_str().unwrap();
    let rec = RuntimeRecorder::default();
    let mut cfg = make_config(&fp, store, b"", &rec);

    // 1. NULL out -> HIRSEL_ABI_MISMATCH, no callback.
    assert_eq!(
        unsafe { hirsel_embed_v1_start(&cfg, ptr::null_mut()) },
        HIRSEL_ABI_MISMATCH
    );
    assert!(rec.ended.lock().unwrap().is_empty());

    // 2. Non-null out is zeroed on every non-OK return.
    let mut rt = HirselRuntime { generation: 999 };
    cfg.start_timeout_ms = 0;
    assert_eq!(
        unsafe { hirsel_embed_v1_start(&cfg, &mut rt) },
        HIRSEL_ABI_MISMATCH
    );
    assert_eq!(rt.generation, 0);
    assert!(rec.ended.lock().unwrap().is_empty());

    // 3. Guard-page short struct: allocate 2 pages, make page 2 PROT_NONE, place a
    //    short 8-byte struct right at the end of page 1 so reading past `size` faults!
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
    let mem = unsafe {
        libc::mmap(
            ptr::null_mut(),
            page * 2,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(mem, libc::MAP_FAILED);
    assert_eq!(
        unsafe { libc::mprotect(mem.cast::<u8>().add(page).cast(), page, libc::PROT_NONE) },
        0
    );
    let short_ptr = unsafe { mem.cast::<u8>().add(page - 8) };
    unsafe { short_ptr.cast::<u32>().write(8) };
    let mut rt2 = HirselRuntime { generation: 77 };
    assert_eq!(
        unsafe { hirsel_embed_v1_start(short_ptr.cast(), &mut rt2) },
        HIRSEL_ABI_MISMATCH
    );
    assert_eq!(rt2.generation, 0);
    unsafe { libc::munmap(mem, page * 2) };

    // 4. Oversized struct: nonzero tail refused, zero tail accepted.
    #[repr(C)]
    struct ExtendedConfig {
        base: HirselEmbedConfig,
        extra: u64,
    }
    let valid_base = make_config(&fp, store, b"", &rec);
    let bad_ext = ExtendedConfig {
        base: HirselEmbedConfig {
            size: size_of::<ExtendedConfig>() as u32,
            ..valid_base
        },
        extra: 1,
    };
    assert_eq!(
        unsafe { hirsel_embed_v1_start((&raw const bad_ext).cast(), &mut rt) },
        HIRSEL_ABI_MISMATCH
    );

    let good_ext = ExtendedConfig {
        base: HirselEmbedConfig {
            size: size_of::<ExtendedConfig>() as u32,
            ..valid_base
        },
        extra: 0,
    };
    assert_eq!(
        unsafe { hirsel_embed_v1_start((&raw const good_ext).cast(), &mut rt) },
        HIRSEL_OK
    );
    assert_ne!(rt.generation, 0);

    // 5. Stack-allocated callback table overwritten after `open` still delivers!
    let srec = SessionRecorder::default();
    let mut sess = HirselSession::default();
    {
        let mut cbs = make_callbacks(&srec);
        assert_eq!(
            unsafe { hirsel_embed_v1_open(rt, &cbs, HirselBytes::default(), &mut sess) },
            HIRSEL_OK
        );
        // Overwrite stack table with garbage immediately after `open` returns.
        cbs.outcome = None;
        cbs.session_ended = None;
        cbs.context = ptr::null_mut();
        std::hint::black_box(&cbs);
    }
    assert_eq!(
        unsafe { hirsel_embed_v1_call(sess, 1, 0, HirselBytes::from_slice(b"hi"), 5000) },
        HIRSEL_OK
    );
    srec.wait_outcome(1);
    assert_eq!(hirsel_embed_v1_close(sess), HIRSEL_OK);
    assert_eq!(srec.wait_session_end(), SessionEnd::Closed);
    assert_eq!(hirsel_embed_v1_stop(rt), HIRSEL_OK);

    // 6. Reentrant `start` inside sync refusal `ended` -> HIRSEL_ALREADY_RUNNING;
    //    handle call inside sync refusal `ended` -> HIRSEL_STOPPED.
    unsafe extern "C" fn reentrant_ended(ctx: *mut c_void, code: u32, _reason: HirselBytes) {
        assert_eq!(code, HIRSEL_END_REFUSED);
        let rec = unsafe { &*ctx.cast::<RuntimeRecorder>() };
        let fp = manifest_fingerprint(TEST_MANIFEST).unwrap();
        let cfg = HirselEmbedConfig {
            size: size_of::<HirselEmbedConfig>() as u32,
            abi: ABI_VERSION,
            manifest_fp: HirselBytes::from_slice(&fp),
            digest: HirselBytes::default(),
            n_place: 16 << 20,
            client_bytes: PROGRESS_RESERVE + 4096,
            start_timeout_ms: 1000,
            grants: HirselBytes::default(),
            dir_bases: HirselBytes::default(),
            store_dir: HirselBytes::from_slice(b"/tmp"),
            context: ptr::null_mut(),
            ended: Some(record_ended),
        };
        let mut out = HirselRuntime::default();
        let st = unsafe { hirsel_embed_v1_start(&cfg, &mut out) };
        rec.reentrant_result.store(st as u32, Ordering::SeqCst);
        let st_stop = hirsel_embed_v1_stop(HirselRuntime { generation: 1 });
        assert_eq!(st_stop, HIRSEL_STOPPED);
    }
    let rec2 = RuntimeRecorder::default();
    let mut cfg_small = make_config(&fp, store, b"", &rec2);
    cfg_small.client_bytes = PROGRESS_RESERVE - 1; // ClientBytesTooSmall
    cfg_small.ended = Some(reentrant_ended);
    let mut rt3 = HirselRuntime { generation: 55 };
    assert_eq!(
        unsafe { hirsel_embed_v1_start(&cfg_small, &mut rt3) },
        HIRSEL_REFUSED
    );
    assert_eq!(rt3.generation, 0);
    assert_eq!(
        rec2.reentrant_result.load(Ordering::SeqCst) as i32,
        HIRSEL_ALREADY_RUNNING
    );
    assert_eq!(registry().current_state(), RuntimeState::Stopped);
}

// ---------------------------------------------------------------------------
// Law B1 & B2: close (host barrier, self-close DEFERRED, losing close immediate),
// stop (host vs delivery BUSY, racing stop, stale handles), admission barriers.
// ---------------------------------------------------------------------------

#[test]
fn law_b1_b2_lifecycle_barriers_self_close_and_spontaneous_end() {
    let _serial = TEST_SERIAL.lock().unwrap();
    register_program(&MOCK_PROGRAM);
    *MOCK_MODE.lock().unwrap() = MockMode::Normal;
    let fp = manifest_fingerprint(TEST_MANIFEST).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().to_str().unwrap();
    let rec = RuntimeRecorder::default();
    let cfg = make_config(&fp, store, b"", &rec);

    // Start on a thread that immediately exits — calls afterwards must still work (B2).
    struct SendCfg(HirselEmbedConfig);
    unsafe impl Send for SendCfg {}
    let cfg_copy = SendCfg(cfg);
    let rt1 = thread::spawn(move || {
        let cfg_copy = cfg_copy;
        let mut rt = HirselRuntime::default();
        assert_eq!(
            unsafe { hirsel_embed_v1_start(&cfg_copy.0, &mut rt) },
            HIRSEL_OK
        );
        rt
    })
    .join()
    .unwrap();
    assert_ne!(rt1.generation, 0);

    // Concurrent second start while running -> HIRSEL_ALREADY_RUNNING, *out == 0.
    let mut rt_dup = HirselRuntime { generation: 123 };
    assert_eq!(
        unsafe { hirsel_embed_v1_start(&cfg, &mut rt_dup) },
        HIRSEL_ALREADY_RUNNING
    );
    assert_eq!(rt_dup.generation, 0);

    // Open session; inside outcome callback call `stop` (-> HIRSEL_BUSY) and `close` (-> HIRSEL_DEFERRED).
    let srec = Arc::new(SessionRecorder::default());
    let mut sess = HirselSession::default();
    let cbs = make_callbacks(&srec);
    assert_eq!(
        unsafe { hirsel_embed_v1_open(rt1, &cbs, HirselBytes::default(), &mut sess) },
        HIRSEL_OK
    );
    let busy_rc = Arc::new(AtomicU32::new(999));
    let deferred_rc = Arc::new(AtomicU32::new(999));
    let losing_rc = Arc::new(AtomicU32::new(999));
    {
        let busy = busy_rc.clone();
        let def = deferred_rc.clone();
        let lose = losing_rc.clone();
        *srec.on_outcome_hook.lock().unwrap() = Some(Box::new(move |_| {
            busy.store(hirsel_embed_v1_stop(rt1) as u32, Ordering::SeqCst);
            def.store(hirsel_embed_v1_close(sess) as u32, Ordering::SeqCst);
            lose.store(hirsel_embed_v1_close(sess) as u32, Ordering::SeqCst);
        }));
    }
    assert_eq!(
        unsafe { hirsel_embed_v1_call(sess, 7, 0, HirselBytes::from_slice(b"ok"), 5000) },
        HIRSEL_OK
    );
    assert_eq!(srec.wait_session_end(), SessionEnd::Closed);
    assert_eq!(busy_rc.load(Ordering::SeqCst) as i32, HIRSEL_BUSY);
    assert_eq!(deferred_rc.load(Ordering::SeqCst) as i32, HIRSEL_DEFERRED);
    assert_eq!(losing_rc.load(Ordering::SeqCst) as i32, HIRSEL_STOPPED);

    // Admission barrier test on `close` (B1): hold a producer inside admitted `call`,
    // start `close` on another thread, verify `close` blocks until producer drops guard!
    let srec2 = Arc::new(SessionRecorder::default());
    let mut sess2 = HirselSession::default();
    let cbs2 = make_callbacks(&srec2);
    assert_eq!(
        unsafe { hirsel_embed_v1_open(rt1, &cbs2, HirselBytes::default(), &mut sess2) },
        HIRSEL_OK
    );
    let pause = PauseHandle::arm();
    let producer = thread::spawn(move || unsafe {
        hirsel_embed_v1_call(sess2, 1, 0, HirselBytes::from_slice(b"x"), 5000)
    });
    pause.wait_until_entered();
    let close_done = Arc::new(AtomicBool::new(false));
    let close_done_bg = close_done.clone();
    let closer = thread::spawn(move || {
        let st = hirsel_embed_v1_close(sess2);
        close_done_bg.store(true, Ordering::SeqCst);
        st
    });
    thread::sleep(Duration::from_millis(30));
    assert!(
        !close_done.load(Ordering::SeqCst),
        "close must wait for admitted producer"
    );
    pause.release();
    let _ = producer.join().unwrap();
    assert_eq!(closer.join().unwrap(), HIRSEL_OK);
    assert_eq!(srec2.wait_session_end(), SessionEnd::Closed);

    // Stop generation 1; racing second stop returns HIRSEL_STOPPED.
    assert_eq!(hirsel_embed_v1_stop(rt1), HIRSEL_OK);
    assert_eq!(hirsel_embed_v1_stop(rt1), HIRSEL_STOPPED);

    // Spontaneous RunStop end (B2): state moves to Ended, callbacks fire once, then stop returns OK.
    *MOCK_MODE.lock().unwrap() = MockMode::SpontaneousRunStopAfterReady;
    let rec_spont = RuntimeRecorder::default();
    let cfg_spont = make_config(&fp, store, b"", &rec_spont);
    let mut rt2 = HirselRuntime::default();
    assert_eq!(
        unsafe { hirsel_embed_v1_start(&cfg_spont, &mut rt2) },
        HIRSEL_OK
    );
    assert!(rt2.generation > rt1.generation);
    // Stale generation 1 handle returns HIRSEL_STOPPED.
    let mut dummy_sess = HirselSession::default();
    assert_eq!(
        unsafe { hirsel_embed_v1_open(rt1, &cbs2, HirselBytes::default(), &mut dummy_sess) },
        HIRSEL_STOPPED
    );
    {
        let mut g = rec_spont.ended.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while g.is_empty() {
            let rem = deadline.saturating_duration_since(Instant::now());
            assert!(!rem.is_zero(), "timed out waiting for spontaneous ended");
            g = rec_spont.cv.wait_timeout(g, rem).unwrap().0;
        }
        assert_eq!(g[0].0, HIRSEL_END_RUN_STOP);
        assert_eq!(g[0].1, b"TestFault");
    }
    assert_eq!(hirsel_embed_v1_stop(rt2), HIRSEL_OK);
    assert_eq!(rec_spont.ended.lock().unwrap().len(), 1);
}

// ---------------------------------------------------------------------------
// Law M1: Leased-stream concurrency (2+ held transacts, abandon + id reuse).
// ---------------------------------------------------------------------------

#[test]
fn law_m1_concurrent_leased_streams_abandon_and_id_reuse() {
    let _serial = TEST_SERIAL.lock().unwrap();
    register_program(&MOCK_PROGRAM);
    *MOCK_MODE.lock().unwrap() = MockMode::HoldTwoCallsThenRelease;
    RELEASE_HELD_CALLS.store(false, Ordering::SeqCst);
    HELD_CALL_COUNT.store(0, Ordering::SeqCst);

    let fp = manifest_fingerprint(TEST_MANIFEST).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().to_str().unwrap();
    let rec = RuntimeRecorder::default();
    let cfg = make_config(&fp, store, b"", &rec);
    let mut rt = HirselRuntime::default();
    assert_eq!(unsafe { hirsel_embed_v1_start(&cfg, &mut rt) }, HIRSEL_OK);

    let srec = SessionRecorder::default();
    let mut sess = HirselSession::default();
    let cbs = make_callbacks(&srec);
    assert_eq!(
        unsafe { hirsel_embed_v1_open(rt, &cbs, HirselBytes::default(), &mut sess) },
        HIRSEL_OK
    );

    // Issue two concurrent calls A (id=10) and B (id=20) — both reach the server concurrently!
    assert_eq!(
        unsafe { hirsel_embed_v1_call(sess, 10, 0, HirselBytes::from_slice(b"callA"), 5000) },
        HIRSEL_OK
    );
    // Duplicate live id -> HIRSEL_ID_IN_USE.
    assert_eq!(
        unsafe { hirsel_embed_v1_call(sess, 10, 0, HirselBytes::from_slice(b"dup"), 5000) },
        HIRSEL_ID_IN_USE
    );
    assert_eq!(
        unsafe { hirsel_embed_v1_call(sess, 20, 0, HirselBytes::from_slice(b"callB"), 5000) },
        HIRSEL_OK
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while HELD_CALL_COUNT.load(Ordering::Acquire) < 2 {
        assert!(
            Instant::now() < deadline,
            "both calls must be in flight concurrently"
        );
        thread::sleep(Duration::from_millis(5));
    }

    // Abandon call A (id=10) and immediately reuse id=10 for call C while B is still held!
    assert_eq!(hirsel_embed_v1_abandon(sess, 10), HIRSEL_OK);
    assert_eq!(hirsel_embed_v1_abandon(sess, 10), HIRSEL_NOT_LIVE);
    assert_eq!(
        unsafe { hirsel_embed_v1_call(sess, 10, 0, HirselBytes::from_slice(b"callC"), 5000) },
        HIRSEL_OK
    );
    while HELD_CALL_COUNT.load(Ordering::Acquire) < 3 {
        assert!(
            Instant::now() < deadline,
            "reused call C must dispatch on another lane"
        );
        thread::sleep(Duration::from_millis(5));
    }

    // Release all three held calls: A's outcome must be discarded; B (20) and C (10) delivered!
    RELEASE_HELD_CALLS.store(true, Ordering::SeqCst);
    srec.wait_outcome(2);
    thread::sleep(Duration::from_millis(40));
    let got = srec.outcomes.lock().unwrap().clone();
    assert_eq!(got.len(), 2, "abandoned call A must not deliver");
    let got_ids: Vec<(u64, Outcome)> = got
        .iter()
        .map(|(id, b)| (*id, Outcome::decode(&mut codec::Reader::new(b)).unwrap()))
        .collect();
    assert!(got_ids.contains(&(20, Outcome::Returned(b"callB".to_vec()))));
    assert!(got_ids.contains(&(10, Outcome::Returned(b"callC".to_vec()))));

    assert_eq!(hirsel_embed_v1_stop(rt), HIRSEL_OK);
}

// ---------------------------------------------------------------------------
// Law M11: Host registration refusal (`PlaceCapacity`) & startup timeout
// (pure infinite loop and parked wait), followed by valid `start`.
// ---------------------------------------------------------------------------

#[test]
fn law_m11_capacity_refusal_and_startup_timeouts_then_valid_start() {
    let _serial = TEST_SERIAL.lock().unwrap();
    register_program(&MOCK_PROGRAM);
    let fp = manifest_fingerprint(TEST_MANIFEST).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().to_str().unwrap();

    // 1. PlaceCapacity(Room) refusal.
    *MOCK_MODE.lock().unwrap() = MockMode::RefuseCapacity;
    let rec1 = RuntimeRecorder::default();
    let cfg1 = make_config(&fp, store, b"", &rec1);
    let mut rt = HirselRuntime { generation: 9 };
    assert_eq!(
        unsafe { hirsel_embed_v1_start(&cfg1, &mut rt) },
        HIRSEL_REFUSED
    );
    assert_eq!(rt.generation, 0);
    {
        let e = rec1.ended.lock().unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].0, HIRSEL_END_REFUSED);
        assert!(
            std::str::from_utf8(&e[0].1)
                .unwrap()
                .contains("PlaceCapacity")
        );
    }

    // 2. Hung registration in pure loop with short timeout -> PlaceTimedOut.
    *MOCK_MODE.lock().unwrap() = MockMode::HangPureLoop;
    let rec2 = RuntimeRecorder::default();
    let mut cfg2 = make_config(&fp, store, b"", &rec2);
    cfg2.start_timeout_ms = 50;
    assert_eq!(
        unsafe { hirsel_embed_v1_start(&cfg2, &mut rt) },
        HIRSEL_REFUSED
    );
    assert_eq!(rt.generation, 0);
    {
        let e = rec2.ended.lock().unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].0, HIRSEL_END_REFUSED);
        assert_eq!(e[0].1, b"PlaceTimedOut");
    }

    // 3. Hung registration in parked wait -> PlaceTimedOut.
    *MOCK_MODE.lock().unwrap() = MockMode::HangParkedWait;
    let rec3 = RuntimeRecorder::default();
    let mut cfg3 = make_config(&fp, store, b"", &rec3);
    cfg3.start_timeout_ms = 50;
    assert_eq!(
        unsafe { hirsel_embed_v1_start(&cfg3, &mut rt) },
        HIRSEL_REFUSED
    );
    assert_eq!(rt.generation, 0);
    assert_eq!(rec3.ended.lock().unwrap()[0].1, b"PlaceTimedOut");

    // 4. Subsequent valid start succeeds cleanly!
    *MOCK_MODE.lock().unwrap() = MockMode::Normal;
    let rec4 = RuntimeRecorder::default();
    let cfg4 = make_config(&fp, store, b"", &rec4);
    assert_eq!(unsafe { hirsel_embed_v1_start(&cfg4, &mut rt) }, HIRSEL_OK);
    assert_eq!(hirsel_embed_v1_stop(rt), HIRSEL_OK);
}

// ---------------------------------------------------------------------------
// Law M3 (accounting boundary B-1 vs B, open at capacity), M8 (dial 503,
// room bounds HIRSEL_OVER_BOUND), and Foreign wait on stop.
// ---------------------------------------------------------------------------

#[test]
fn law_m3_m8_and_foreign_wait() {
    let _serial = TEST_SERIAL.lock().unwrap();
    register_program(&MOCK_PROGRAM);
    let fp = manifest_fingerprint(TEST_MANIFEST).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().to_str().unwrap();

    // 1. Dial refused 503 -> open returns OK, session_ended delivers DialRefused(503).
    *MOCK_MODE.lock().unwrap() = MockMode::RefuseDial503;
    let rec = RuntimeRecorder::default();
    let cfg = make_config(&fp, store, b"", &rec);
    let mut rt = HirselRuntime::default();
    assert_eq!(unsafe { hirsel_embed_v1_start(&cfg, &mut rt) }, HIRSEL_OK);
    let srec = SessionRecorder::default();
    let mut sess = HirselSession::default();
    let cbs = make_callbacks(&srec);
    assert_eq!(
        unsafe { hirsel_embed_v1_open(rt, &cbs, HirselBytes::default(), &mut sess) },
        HIRSEL_OK
    );
    assert_eq!(srec.wait_session_end(), SessionEnd::DialRefused(503));
    assert_eq!(hirsel_embed_v1_stop(rt), HIRSEL_OK);

    // 2. M3 exact admissible bytes: one session + one 100-byte call fits at B, fails at B-1;
    //    second open at capacity returns HIRSEL_FULL with no session_ended.
    *MOCK_MODE.lock().unwrap() = MockMode::Normal;
    let exact_pool = SESSION_BASE_BYTES + CALL_BASE_BYTES + 100;
    let rec2 = RuntimeRecorder::default();
    let mut cfg2 = make_config(&fp, store, b"", &rec2);
    cfg2.client_bytes = PROGRESS_RESERVE + exact_pool - 1;
    assert_eq!(unsafe { hirsel_embed_v1_start(&cfg2, &mut rt) }, HIRSEL_OK);
    let srec2 = SessionRecorder::default();
    let cbs2 = make_callbacks(&srec2);
    assert_eq!(
        unsafe { hirsel_embed_v1_open(rt, &cbs2, HirselBytes::default(), &mut sess) },
        HIRSEL_OK
    );
    let big_arg = vec![0xab; 100];
    assert_eq!(
        unsafe { hirsel_embed_v1_call(sess, 1, 0, HirselBytes::from_slice(&big_arg), 5000) },
        HIRSEL_FULL
    );
    // Second open also exceeds remaining pool -> HIRSEL_FULL, no session_ended!
    let srec_full = SessionRecorder::default();
    let cbs_full = make_callbacks(&srec_full);
    let mut sess_full = HirselSession {
        generation: 1,
        session: 1,
    };
    assert_eq!(
        unsafe { hirsel_embed_v1_open(rt, &cbs_full, HirselBytes::default(), &mut sess_full) },
        HIRSEL_FULL
    );
    assert_eq!(sess_full, HirselSession::default());
    assert!(srec_full.session_ends.lock().unwrap().is_empty());
    // Existing session still closes and delivers session_ended!
    assert_eq!(hirsel_embed_v1_close(sess), HIRSEL_OK);
    assert_eq!(srec2.wait_session_end(), SessionEnd::Closed);
    assert_eq!(hirsel_embed_v1_stop(rt), HIRSEL_OK);

    // 3. Room join + HIRSEL_OVER_BOUND on state & message + direct send + clear set.
    let rec3 = RuntimeRecorder::default();
    let cfg3 = make_config(&fp, store, b"", &rec3);
    assert_eq!(unsafe { hirsel_embed_v1_start(&cfg3, &mut rt) }, HIRSEL_OK);
    let srec3 = SessionRecorder::default();
    let cbs3 = make_callbacks(&srec3);
    assert_eq!(
        unsafe { hirsel_embed_v1_open(rt, &cbs3, HirselBytes::default(), &mut sess) },
        HIRSEL_OK
    );
    // Wrong kind for room_join (want 0 is transact) -> HIRSEL_UNKNOWN_ENTRY.
    assert_eq!(
        unsafe { hirsel_embed_v1_room_join(sess, 1, 0, HirselBytes::default()) },
        HIRSEL_UNKNOWN_ENTRY
    );
    assert_eq!(
        unsafe { hirsel_embed_v1_room_join(sess, 1, 2, HirselBytes::default()) },
        HIRSEL_OK
    );
    // Exceeds state_max (16) / message_max (16) -> HIRSEL_OVER_BOUND.
    let over = vec![0u8; 17];
    assert_eq!(
        unsafe { hirsel_embed_v1_room_set(sess, 1, 1, HirselBytes::from_slice(&over)) },
        HIRSEL_OVER_BOUND
    );
    assert_eq!(
        unsafe { hirsel_embed_v1_room_send(sess, 1, 0, HirselBytes::from_slice(&over)) },
        HIRSEL_OVER_BOUND
    );
    // Wait for initial Admission + Peers events, then direct room_send + clear room_set.
    {
        let mut g = srec3.room_events.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while g.len() < 2 {
            let rem = deadline.saturating_duration_since(Instant::now());
            assert!(!rem.is_zero(), "timed out waiting for room join events");
            g = srec3.cv.wait_timeout(g, rem).unwrap().0;
        }
    }
    assert_eq!(
        unsafe { hirsel_embed_v1_room_send(sess, 1, 1, HirselBytes::from_slice(b"dm")) },
        HIRSEL_OK
    );
    assert_eq!(
        unsafe { hirsel_embed_v1_room_set(sess, 1, 0, HirselBytes::default()) },
        HIRSEL_OK
    );
    {
        let mut g = srec3.room_events.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while g.len() < 4 {
            let rem = deadline.saturating_duration_since(Instant::now());
            assert!(
                !rem.is_zero(),
                "timed out waiting for direct message + clear set"
            );
            g = srec3.cv.wait_timeout(g, rem).unwrap().0;
        }
        let decoded: Vec<RoomEvent> = g
            .iter()
            .map(|(_, b)| RoomEvent::decode(&mut codec::Reader::new(b)).unwrap())
            .collect();
        assert!(decoded.contains(&RoomEvent::Message(MessageIn {
            from: 1,
            message: b"dm".to_vec(),
            direct: true,
        })));
    }
    assert_eq!(hirsel_embed_v1_room_leave(sess, 1), HIRSEL_OK);
    assert_eq!(hirsel_embed_v1_stop(rt), HIRSEL_OK);

    // 4. Foreign wait law: `stop` waits until an issued foreign `waits` call returns,
    //    and host receives `ended` once.
    register_program(&MOCK_PROGRAM_FOREIGN);
    *MOCK_MODE.lock().unwrap() = MockMode::ForeignWaitOnStop;
    FOREIGN_CALL_ENTERED.store(false, Ordering::SeqCst);
    FOREIGN_CALL_RELEASE.store(false, Ordering::SeqCst);
    FOREIGN_CALL_EXITED.store(false, Ordering::SeqCst);
    let rec4 = RuntimeRecorder::default();
    let cfg4 = make_config(&fp, store, b"Foreign\n", &rec4);
    assert_eq!(unsafe { hirsel_embed_v1_start(&cfg4, &mut rt) }, HIRSEL_OK);
    while !FOREIGN_CALL_ENTERED.load(Ordering::Acquire) {
        thread::sleep(Duration::from_millis(2));
    }
    let stop_done = Arc::new(AtomicBool::new(false));
    let stop_done_bg = stop_done.clone();
    let stopper = thread::spawn(move || {
        let st = hirsel_embed_v1_stop(rt);
        stop_done_bg.store(true, Ordering::SeqCst);
        st
    });
    thread::sleep(Duration::from_millis(40));
    assert!(
        !stop_done.load(Ordering::SeqCst),
        "stop must wait for issued foreign waits call"
    );
    FOREIGN_CALL_RELEASE.store(true, Ordering::Release);
    assert_eq!(stopper.join().unwrap(), HIRSEL_OK);
    assert!(FOREIGN_CALL_EXITED.load(Ordering::Acquire));
    assert_eq!(rec4.ended.lock().unwrap().len(), 1);
    assert_eq!(rec4.ended.lock().unwrap()[0].0, HIRSEL_END_STOPPED);
}

#[test]
fn law_m3_exact_b_stalled_peak_b2_stop_barrier_and_m1_reconnect() {
    let _serial = TEST_SERIAL.lock().unwrap();
    register_program(&MOCK_PROGRAM);
    *MOCK_MODE.lock().unwrap() = MockMode::Normal;
    let fp = manifest_fingerprint(TEST_MANIFEST).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().to_str().unwrap();

    // 1. M3 exact B boundary: at `B` bytes, call is admitted, reads reply using
    //    progress reserve without stalling, completes, and refunds for reuse.
    let exact_pool = SESSION_BASE_BYTES + CALL_BASE_BYTES + 100;
    let rec = RuntimeRecorder::default();
    let mut cfg = make_config(&fp, store, b"", &rec);
    cfg.client_bytes = PROGRESS_RESERVE + exact_pool;
    let mut rt = HirselRuntime::default();
    assert_eq!(unsafe { hirsel_embed_v1_start(&cfg, &mut rt) }, HIRSEL_OK);

    let srec = Arc::new(SessionRecorder::default());
    let cbs = make_callbacks(&srec);
    let mut sess = HirselSession::default();
    assert_eq!(
        unsafe { hirsel_embed_v1_open(rt, &cbs, HirselBytes::default(), &mut sess) },
        HIRSEL_OK
    );
    let payload = vec![0xcd; 100];
    assert_eq!(
        unsafe { hirsel_embed_v1_call(sess, 1, 0, HirselBytes::from_slice(&payload), 5000) },
        HIRSEL_OK
    );
    srec.wait_outcome(1);
    // Budget refunded after outcome delivery -> exact same 100-byte call fits again!
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let st =
            unsafe { hirsel_embed_v1_call(sess, 2, 0, HirselBytes::from_slice(&payload), 5000) };
        if st == HIRSEL_OK {
            break;
        }
        assert_eq!(st, HIRSEL_FULL);
        assert!(Instant::now() < deadline, "refund did not restore budget");
        thread::sleep(Duration::from_millis(5));
    }
    srec.wait_outcome(2);

    // 2. Stalled delivery callback: peak admissible bytes stays <= admissible_limit!
    let hold_gate = Arc::new((Mutex::new((false, false)), Condvar::new()));
    {
        let gate = hold_gate.clone();
        *srec.on_outcome_hook.lock().unwrap() = Some(Box::new(move |_| {
            let (lock, cv) = &*gate;
            let mut g = lock.lock().unwrap();
            g.0 = true;
            cv.notify_all();
            while !g.1 {
                g = cv.wait(g).unwrap();
            }
        }));
    }
    assert_eq!(
        unsafe { hirsel_embed_v1_call(sess, 3, 0, HirselBytes::from_slice(b"hold"), 5000) },
        HIRSEL_OK
    );
    {
        let (lock, cv) = &*hold_gate;
        let mut g = lock.lock().unwrap();
        while !g.0 {
            g = cv.wait(g).unwrap();
        }
    }
    if let Some((gen_rec, _g)) = registry().admit_runtime(rt.generation) {
        assert!(gen_rec.account.peak_admissible() <= gen_rec.account.admissible_limit());
    }
    {
        let (lock, cv) = &*hold_gate;
        let mut g = lock.lock().unwrap();
        g.1 = true;
        cv.notify_all();
    }

    // 3. B2 admission barrier on `stop`: hold an admitted `call`, invoke `stop`
    //    from another thread, confirm `stop` blocks until producer drops guard!
    let pause = PauseHandle::arm();
    let producer = thread::spawn(move || unsafe {
        hirsel_embed_v1_call(sess, 4, 0, HirselBytes::from_slice(b"z"), 5000)
    });
    pause.wait_until_entered();
    let stop_done = Arc::new(AtomicBool::new(false));
    let stop_done_bg = stop_done.clone();
    let stopper = thread::spawn(move || {
        let st = hirsel_embed_v1_stop(rt);
        stop_done_bg.store(true, Ordering::SeqCst);
        st
    });
    thread::sleep(Duration::from_millis(30));
    assert!(
        !stop_done.load(Ordering::SeqCst),
        "stop must wait for admitted producer"
    );
    pause.release();
    let _ = producer.join().unwrap();
    assert_eq!(stopper.join().unwrap(), HIRSEL_OK);

    // 4. M1 crash/reconnect recovery over shared 3145 kernel with 2 concurrent transacts.
    use super::super::kernel::{
        self, Action, Attempt, Event, PendingLaneCall, Phase, StepContext, StreamLane,
    };
    let ctx = StepContext {
        wants: 1,
        has_credential_source: false,
    };
    let mut phase = kernel::step(Phase::initial(), Event::Start, ctx).phase;
    phase = kernel::step(phase, Event::SocketOpen { epoch: 1 }, ctx).phase;
    phase = kernel::step(
        phase,
        Event::Frame {
            epoch: 1,
            frame: Frame::Welcome {
                caller: Caller {
                    issuer: 1,
                    subject: vec![1],
                },
                q: 2,
                budget: 1,
                digest: vec![],
                target: "x86_64-unknown-linux-gnu".into(),
                n_place: 16 << 20,
                now_ms: 262_144,
                retention_ms: 60_000,
            },
        },
        ctx,
    )
    .phase;
    phase = kernel::step(
        phase,
        Event::WelcomeBound {
            epoch: 1,
            at: 10,
            width: 3,
        },
        ctx,
    )
    .phase;
    phase = kernel::step(
        phase,
        Event::Frame {
            epoch: 1,
            frame: Frame::Placed(Entry::Ready(0)),
        },
        ctx,
    )
    .phase;
    let mut lane_a = StreamLane::new(0, StreamId { t: 7, n: 1 });
    let mut lane_b = StreamLane::new(1, StreamId { t: 7, n: 2 });
    for (lane, host_id, payload) in [
        (&mut lane_a, 100u64, b"txA".as_slice()),
        (&mut lane_b, 200u64, b"txB".as_slice()),
    ] {
        let tr = kernel::step(
            phase,
            Event::PrepareCall {
                epoch: 1,
                lane: lane.id,
            },
            ctx,
        );
        assert!(tr.actions.contains(&Action::RunPrepare { lane: lane.id }));
        let tr = kernel::step(
            tr.phase,
            Event::SendCall {
                epoch: 1,
                lane: lane.id,
                delivery: Attempt::WebSocket,
            },
            ctx,
        );
        assert!(tr.actions.contains(&Action::RunSend { lane: lane.id }));
        phase = tr.phase;
        lane.pending = Some(PendingLaneCall {
            p: lane.next,
            want: 0,
            deadline_ms: 5000,
            args: payload.to_vec(),
            inflight: true,
            host_call: Some(host_id),
            charge: 0,
        });
    }
    // Simulate connection loss while BOTH transacts are in flight, then reconnect!
    phase = kernel::step(
        phase,
        Event::SocketLost {
            epoch: 1,
            code: Some(1006),
        },
        ctx,
    )
    .phase;
    lane_a.pending.as_mut().unwrap().inflight = false;
    lane_b.pending.as_mut().unwrap().inflight = false;
    phase = kernel::step(phase, Event::LossRecorded { epoch: 2 }, ctx).phase;
    phase = kernel::step(phase, Event::BackoffElapsed { epoch: 3 }, ctx).phase;
    phase = kernel::step(phase, Event::SocketOpen { epoch: 4 }, ctx).phase;
    phase = kernel::step(
        phase,
        Event::Frame {
            epoch: 4,
            frame: Frame::Welcome {
                caller: Caller {
                    issuer: 1,
                    subject: vec![1],
                },
                q: 2,
                budget: 3,
                digest: vec![],
                target: "x86_64-unknown-linux-gnu".into(),
                n_place: 16 << 20,
                now_ms: 262_144,
                retention_ms: 60_000,
            },
        },
        ctx,
    )
    .phase;
    phase = kernel::step(
        phase,
        Event::WelcomeBound {
            epoch: 4,
            at: 200,
            width: 3,
        },
        ctx,
    )
    .phase;
    phase = kernel::step(
        phase,
        Event::Frame {
            epoch: 4,
            frame: Frame::Placed(Entry::Ready(0)),
        },
        ctx,
    )
    .phase;
    // Replay BOTH under their exact original `(stream, p)` idempotency keys and settle out-of-order!
    for lane in [&mut lane_a, &mut lane_b] {
        let tr = kernel::step(
            phase,
            Event::PrepareCall {
                epoch: 4,
                lane: lane.id,
            },
            ctx,
        );
        assert!(tr.actions.contains(&Action::RunPrepare { lane: lane.id }));
        phase = kernel::step(
            tr.phase,
            Event::SendCall {
                epoch: 4,
                lane: lane.id,
                delivery: Attempt::WebSocket,
            },
            ctx,
        )
        .phase;
        lane.pending.as_mut().unwrap().inflight = true;
    }
    let done_b = KeyedDone {
        stream: StreamId { t: 7, n: 2 },
        p: 0,
        outcome: Outcome::Returned(b"replyB".to_vec()),
        raw_outcome: Outcome::Returned(b"replyB".to_vec()).encode_bytes(),
    };
    let done_a = KeyedDone {
        stream: StreamId { t: 7, n: 1 },
        p: 0,
        outcome: Outcome::Returned(b"replyA".to_vec()),
        raw_outcome: Outcome::Returned(b"replyA".to_vec()).encode_bytes(),
    };
    assert!(lane_b.matches_done(&done_b) && !lane_a.matches_done(&done_b));
    assert_eq!(lane_b.settle(&done_b.outcome).unwrap().host_call, Some(200));
    assert!(lane_a.matches_done(&done_a));
    assert_eq!(lane_a.settle(&done_a.outcome).unwrap().host_call, Some(100));
}
