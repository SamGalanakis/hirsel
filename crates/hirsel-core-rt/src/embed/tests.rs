//! Comprehensive Slice 2 unit & in-process integration laws (B1, B2, M1, M11, M3,
//! M4, M8, M13, m2, Kernel vectors, Foreign wait).
use super::{
    EmbedProgram,
    abi::*,
    account::{CALL_BASE_BYTES, PROGRESS_RESERVE, SESSION_BASE_BYTES},
    codec::{
        Caller, Change, Entry, Frame, Group, KeyedDone, MessageIn, Outcome, Part, RoomEvent, Row,
        SessionEnd, StreamId, SubEnd, Want,
    },
    registry::{RuntimeState, registry, test_hooks::PauseHandle},
    ws,
};
use crate::{
    Context,
    entry::{DIR, FOREIGN, PROCESS, Requirement},
    process::{Config, MainResult},
    units::ledger::{Took, UnitCtx, conn::Client},
    units_lang::{ClientH, CrossSend},
};
use hirsel_core_ids::embed::{ABI_VERSION, manifest_fingerprint};
use hirsel_core_layout::cross::{CLIENT, SEQ};
use std::{
    ffi::c_void,
    mem::size_of,
    ptr,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// Serialize tests touching the process-static embed singleton registry.
static TEST_SERIAL: Mutex<()> = Mutex::new(());

const TEST_MANIFEST: &[u8] = br#"{"version":1,"launch":{"digest":[],"target":"x86_64-unknown-linux-gnu","n_place":"16777216","revision":[]},"tables":[],"entries":[{"module":["app"],"name":"echo","kind":"transact","identity":[1,2,3],"revision":[],"args_fp":"101","result_fp":"202","args":{"root":[0],"definitions":[],"key":false},"result":{"root":[0],"definitions":[],"key":false},"slots":[]},{"module":["app"],"name":"watch","kind":"query","identity":[4,5,6],"revision":[],"args_fp":"303","result_fp":"404","args":{"root":[0],"definitions":[],"key":false},"result":{"root":[0],"definitions":[],"key":false},"slots":[{"name":"items","kind":"table","key":{"root":[0],"definitions":[],"key":true},"key_json":{"root":[0],"definitions":[],"key":false},"value":{"root":[0],"definitions":[],"key":false}}]},{"module":["app"],"name":"lobby","kind":"room","identity":[7,8,9],"revision":[],"args_fp":"505","result_fp":"606","args":{"root":[0],"definitions":[],"key":false},"result":{"root":[0],"definitions":[],"key":false},"slots":[],"room":{"peers":4,"state_max":16,"message_max":16,"state":{"root":[0],"definitions":[],"key":false},"message":{"root":[0],"definitions":[],"key":false}}}]}"#;

static TEST_PARAMS: &[Requirement] = &[
    Requirement {
        kind: DIR,
        index: 0,
    },
    Requirement {
        kind: PROCESS,
        index: 0,
    },
];
static TEST_PARAM_NAMES: &[&str] = &["store", "process"];

static TEST_PARAMS_FOREIGN: &[Requirement] = &[
    Requirement {
        kind: DIR,
        index: 0,
    },
    Requirement {
        kind: PROCESS,
        index: 0,
    },
    Requirement {
        kind: FOREIGN,
        index: 0,
    },
];
static TEST_PARAM_NAMES_FOREIGN: &[&str] = &["store", "process", "foreign"];

// ---------------------------------------------------------------------------
// Programmable in-process mock place server over the real `Dial`/`Accepted`/`Bytes`
// transport + real `hirsel_core_describe_emit` + real `run_config_contained`!
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum MockMode {
    Normal,
    RefuseCapacity,
    HangPureLoop,
    HangParkedWait,
    RefuseDial503,
    HoldTwoCallsThenRelease,
    SpontaneousRunStopAfterReady,
    ForeignWaitOnStop,
}

static MOCK_MODE: Mutex<MockMode> = Mutex::new(MockMode::Normal);
static RELEASE_HELD_CALLS: AtomicBool = AtomicBool::new(false);
static HELD_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
static FOREIGN_CALL_ENTERED: AtomicBool = AtomicBool::new(false);
static FOREIGN_CALL_RELEASE: AtomicBool = AtomicBool::new(false);
static FOREIGN_CALL_EXITED: AtomicBool = AtomicBool::new(false);

const BYTES_ROWS: [[u64; 4]; 1] = [[0, SEQ, 1, 1]];
const DIAL_ROWS: [[u64; 4]; 2] = [[0, CLIENT, 0, 0], [8, CLIENT, 0, 0]];

fn emit_bytes(ctx: *const Context, data: &[u8]) {
    let buf = crate::provider::Buffer {
        data: data.as_ptr().cast_mut(),
        len: data.len(),
        capacity: data.len(),
    };
    // SAFETY: `ctx` is active run context and `buf` borrows `data`.
    unsafe {
        crate::process::hirsel_core_describe_emit(ctx, &buf);
    }
}

fn encode_server_ws_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let len = payload.len();
    let mut out = Vec::with_capacity(len + 10);
    out.push(0x80 | (opcode & 0x0f));
    if len <= 125 {
        out.push(len as u8);
    } else if len <= 65535 {
        out.push(126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

fn send_ws_bytes(unit: &mut UnitCtx, to: &Client, raw: &[u8]) {
    let len = raw.len() as u64;
    let carrier = [raw.as_ptr() as u64, len, len];
    let charge = len.div_ceil(8) * 8;
    // SAFETY: `carrier` and `raw` stay live across synchronous send.
    let _ = unsafe {
        UnitCtx::send_cross(
            to,
            CrossSend::new(carrier.as_ptr().cast()),
            charge,
            false,
            || unit,
        )
    };
}

unsafe extern "C" fn mock_place_entry(
    ctx: *mut Context,
    cfg: *const Config,
    _res: *mut MainResult,
) -> u8 {
    let mode = *MOCK_MODE.lock().unwrap();
    match mode {
        MockMode::RefuseCapacity => {
            let mut out = vec![4u8];
            out.extend_from_slice(&4_000_000u64.to_le_bytes());
            out.extend_from_slice(&1024u64.to_le_bytes());
            emit_bytes(ctx, &out);
            return 1;
        }
        MockMode::HangPureLoop => {
            // Simulate a pure Hirsel loop hitting `hirsel_core_step_out` every quantum.
            loop {
                if crate::run_stop::current_generation().is_some_and(|g| g.is_ended()) {
                    crate::run_stop::leave_on_ended();
                }
                std::hint::spin_loop();
            }
        }
        MockMode::HangParkedWait => {
            // Park until generation end wakes us.
            while !crate::run_stop::current_generation().is_some_and(|g| g.is_ended()) {
                thread::sleep(Duration::from_millis(5));
            }
            crate::run_stop::leave_on_ended();
        }
        _ => {}
    }

    // Arm dialer capture and open `Inbox[Dial]` on a local UnitCtx, capturing its Client.
    emit_bytes(ctx, &[1u8]);
    let mut unit = UnitCtx::unfunded();
    let _readies = crate::tasks::Readies::for_unit(crate::units::NONE);
    unit.port.set_waker(&_readies);
    let (dial_inbox, dial_client) = unit.conn_open(16, 16, 0, 16, &DIAL_ROWS);
    super::maybe_capture_dialer(&dial_client);
    unit.drop_client(dial_client);

    // Emit Ready!
    emit_bytes(ctx, &[2u8]);

    if matches!(mode, MockMode::SpontaneousRunStopAfterReady) {
        thread::sleep(Duration::from_millis(40));
        crate::run_stop::raise(crate::run_stop::Stop::from_static(b"TestFault"));
    }

    // Serve dials until dial_inbox closes (when `stop` shuts down `Transport` and drops `Dialer`).
    let mut session_in: Option<crate::units::ledger::conn::Inbox> = None;
    let mut session_out: Option<Client> = None;
    let mut ws_buf: Vec<u8> = Vec::new();
    let mut held_calls: Vec<(StreamId, u64, Vec<u8>)> = Vec::new();
    let mut foreign_thread: Option<thread::JoinHandle<()>> = None;

    if matches!(mode, MockMode::ForeignWaitOnStop) {
        foreign_thread = Some(thread::spawn(|| {
            FOREIGN_CALL_ENTERED.store(true, Ordering::Release);
            while !FOREIGN_CALL_RELEASE.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(2));
            }
            FOREIGN_CALL_EXITED.store(true, Ordering::Release);
        }));
    }

    let mut version = 1u64;
    let dial_key = Arc::as_ptr(&dial_inbox.0) as usize;
    let mut sealing: std::collections::HashMap<usize, crate::units::ledger::conn::Inbox> =
        std::collections::HashMap::new();

    loop {
        if matches!(mode, MockMode::HoldTwoCallsThenRelease)
            && RELEASE_HELD_CALLS.load(Ordering::Acquire)
            && !held_calls.is_empty()
        {
            if let Some(ref out_c) = session_out {
                let outcomes: Vec<KeyedDone> = held_calls
                    .drain(..)
                    .map(|(stream, p, args)| {
                        let o = Outcome::Returned(args);
                        let raw = o.encode_bytes();
                        KeyedDone {
                            stream,
                            p,
                            outcome: o,
                            raw_outcome: raw,
                        }
                    })
                    .collect();
                version += 1;
                let batch = Frame::Batch {
                    version,
                    complete: vec![],
                    groups: vec![],
                    outcomes,
                };
                send_ws_bytes(
                    &mut unit,
                    out_c,
                    &encode_server_ws_frame(ws::OP_BINARY, &batch.encode()),
                );
            }
        }

        let (took, ident) = match unit.take() {
            Some((t, _)) => (t, unit.took_ident),
            None => {
                if matches!(mode, MockMode::HoldTwoCallsThenRelease) && !held_calls.is_empty() {
                    thread::sleep(Duration::from_millis(2));
                    continue;
                }
                loop {
                    if let Some((t, _)) = unit.take() {
                        break (t, unit.took_ident);
                    }
                    if unit.port.arm() {
                        thread::park();
                    }
                }
            }
        };

        match took {
            Took::SealAnswer => {
                if let Some(inbox) = sealing.remove(&ident) {
                    let _ = unit.clean(&inbox);
                }
            }
            Took::Close if ident == dial_key => {
                break;
            }
            Took::Item if ident == dial_key => {
                let mut accepted_pair: Option<(Client, Client)> = None;
                let _ = dial_inbox.receive_cross(|_, flat, _, _| {
                    // SAFETY: `Dial` is `[back: *mut ClientH, accept: *mut ClientH]`.
                    unsafe {
                        let back =
                            Box::from_raw(flat.cast::<*mut ClientH>().read_unaligned()).client;
                        let acc =
                            Box::from_raw(flat.add(8).cast::<*mut ClientH>().read_unaligned())
                                .client;
                        accepted_pair = Some((back, acc));
                    }
                    Ok::<(), ()>(())
                });
                if let Some((back, acc)) = accepted_pair {
                    if matches!(mode, MockMode::RefuseDial503) {
                        let back_ptr = Box::into_raw(Box::new(ClientH { client: back }));
                        let mut acc_flat = [0u8; 16];
                        // SAFETY: Accepted layout: to @ 0, code @ 8.
                        unsafe {
                            acc_flat
                                .as_mut_ptr()
                                .cast::<*mut ClientH>()
                                .write_unaligned(back_ptr);
                            acc_flat
                                .as_mut_ptr()
                                .add(8)
                                .cast::<u16>()
                                .write_unaligned(503);
                            let _ = UnitCtx::send_cross(
                                &acc,
                                CrossSend::new(acc_flat.as_ptr()),
                                0,
                                false,
                                || &mut unit,
                            );
                        }
                        unit.drop_client(acc);
                    } else {
                        let (to_inbox, to_client) = unit.conn_open(8, 24, 65536, 24, &BYTES_ROWS);
                        let to_ptr = Box::into_raw(Box::new(ClientH { client: to_client }));
                        let mut acc_flat = [0u8; 16];
                        // SAFETY: Accepted layout: to @ 0, code @ 8.
                        unsafe {
                            acc_flat
                                .as_mut_ptr()
                                .cast::<*mut ClientH>()
                                .write_unaligned(to_ptr);
                            acc_flat
                                .as_mut_ptr()
                                .add(8)
                                .cast::<u16>()
                                .write_unaligned(101);
                            let _ = UnitCtx::send_cross(
                                &acc,
                                CrossSend::new(acc_flat.as_ptr()),
                                0,
                                false,
                                || &mut unit,
                            );
                        }
                        unit.drop_client(acc);
                        if let Some(old_in) = session_in.take() {
                            let k = Arc::as_ptr(&old_in.0) as usize;
                            unit.seal(&old_in);
                            sealing.insert(k, old_in);
                        }
                        if let Some(old_out) = session_out.take() {
                            unit.drop_client(old_out);
                        }
                        session_in = Some(to_inbox);
                        session_out = Some(back);
                        ws_buf.clear();
                    }
                }
            }
            Took::Item => {
                if let Some(ref inbox) = session_in {
                    while inbox
                        .receive_cross(|_, flat, content, _| {
                            // SAFETY: `Bytes` flat header + `content` slice.
                            unsafe {
                                let len = flat.add(8).cast::<u64>().read_unaligned() as usize;
                                if len > 0 {
                                    ws_buf.extend_from_slice(std::slice::from_raw_parts(
                                        content, len,
                                    ));
                                }
                            }
                            Ok::<(), ()>(())
                        })
                        .is_some()
                    {}
                }
                // Decode client-masked WS frames.
                while let Some((opcode, payload)) = pop_masked_client_ws(&mut ws_buf) {
                    if opcode == ws::OP_CLOSE {
                        if let Some(ref out_c) = session_out {
                            send_ws_bytes(
                                &mut unit,
                                out_c,
                                &encode_server_ws_frame(ws::OP_CLOSE, &1000u16.to_be_bytes()),
                            );
                        }
                        continue;
                    }
                    if opcode != ws::OP_BINARY {
                        continue;
                    }
                    let Ok(frame) = Frame::decode(&payload) else {
                        continue;
                    };
                    let Some(ref out_c) = session_out else {
                        continue;
                    };
                    match frame {
                        Frame::Hello(_cred, wants) => {
                            // Read compiled target from Launch metadata.
                            // SAFETY: `cfg.launch` is live.
                            let target = unsafe { (*(*cfg).launch).target().to_owned() };
                            let welcome = Frame::Welcome {
                                caller: Caller {
                                    issuer: 1,
                                    subject: vec![1, 2, 3, 4],
                                },
                                q: 4,
                                budget: version,
                                digest: vec![],
                                target,
                                n_place: 16 << 20,
                                now_ms: 262_144,
                                retention_ms: 60_000,
                            };
                            send_ws_bytes(
                                &mut unit,
                                out_c,
                                &encode_server_ws_frame(ws::OP_BINARY, &welcome.encode()),
                            );
                            for i in 0..wants.len() {
                                let placed = Frame::Placed(Entry::Ready(i as u32));
                                send_ws_bytes(
                                    &mut unit,
                                    out_c,
                                    &encode_server_ws_frame(ws::OP_BINARY, &placed.encode()),
                                );
                            }
                        }
                        Frame::Call(stream, p, _want, _deadline_ms, args) => {
                            if matches!(mode, MockMode::HoldTwoCallsThenRelease) {
                                held_calls.push((stream, p, args));
                                HELD_CALL_COUNT.fetch_add(1, Ordering::AcqRel);
                            } else {
                                version += 1;
                                let o = Outcome::Returned(args);
                                let raw = o.encode_bytes();
                                let batch = Frame::Batch {
                                    version,
                                    complete: vec![],
                                    groups: vec![],
                                    outcomes: vec![KeyedDone {
                                        stream,
                                        p,
                                        outcome: o,
                                        raw_outcome: raw,
                                    }],
                                };
                                send_ws_bytes(
                                    &mut unit,
                                    out_c,
                                    &encode_server_ws_frame(ws::OP_BINARY, &batch.encode()),
                                );
                            }
                        }
                        Frame::Subscribe(sub, want, _args, _) => {
                            version += 1;
                            let batch = Frame::Batch {
                                version,
                                complete: vec![sub],
                                groups: vec![Group {
                                    sub,
                                    value: vec![0x77],
                                    rows: vec![Row {
                                        slot: 0,
                                        key: vec![0, 0, 0, 1, 0xaa],
                                        value: Some(vec![0xbb]),
                                    }],
                                }],
                                outcomes: vec![],
                            };
                            send_ws_bytes(
                                &mut unit,
                                out_c,
                                &encode_server_ws_frame(ws::OP_BINARY, &batch.encode()),
                            );
                            if want == 2 {
                                // Room join: emit initial membership Part.
                                let live = Frame::Live(vec![Part {
                                    sub,
                                    complete: true,
                                    you: Some(1),
                                    incarnation: (10, 20),
                                    revision: 1,
                                    snapshot: true,
                                    joined: vec![(
                                        1,
                                        Caller {
                                            issuer: 1,
                                            subject: vec![1, 2, 3, 4],
                                        },
                                    )],
                                    states: vec![],
                                    msgs: vec![],
                                    left: vec![],
                                    gap: 0,
                                    lost: 0,
                                    full: false,
                                }]);
                                send_ws_bytes(
                                    &mut unit,
                                    out_c,
                                    &encode_server_ws_frame(ws::OP_BINARY, &live.encode()),
                                );
                            }
                        }
                        Frame::Send(sub, to, msg) => {
                            let live = Frame::Live(vec![Part {
                                sub,
                                complete: false,
                                you: None,
                                incarnation: (10, 20),
                                revision: 2,
                                snapshot: false,
                                joined: vec![],
                                states: vec![],
                                msgs: vec![(1, to.is_some(), msg)],
                                left: vec![],
                                gap: 0,
                                lost: 0,
                                full: false,
                            }]);
                            send_ws_bytes(
                                &mut unit,
                                out_c,
                                &encode_server_ws_frame(ws::OP_BINARY, &live.encode()),
                            );
                        }
                        Frame::Set(sub, st) => {
                            let live = Frame::Live(vec![Part {
                                sub,
                                complete: false,
                                you: None,
                                incarnation: (10, 20),
                                revision: 2,
                                snapshot: false,
                                joined: vec![],
                                states: vec![(1, st)],
                                msgs: vec![],
                                left: vec![],
                                gap: 0,
                                lost: 0,
                                full: false,
                            }]);
                            send_ws_bytes(
                                &mut unit,
                                out_c,
                                &encode_server_ws_frame(ws::OP_BINARY, &live.encode()),
                            );
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(h) = foreign_thread {
        let _ = h.join();
    }
    let mut pending_takes: std::collections::VecDeque<(Took, usize)> =
        std::collections::VecDeque::new();
    if let Some(inbox) = session_in {
        let k = Arc::as_ptr(&inbox.0) as usize;
        unit.seal(&inbox);
        sealing.insert(k, inbox);
    }
    if let Some(out_c) = session_out {
        for (t, ident, _) in unit.drop_client(out_c) {
            pending_takes.push_back((t, ident));
        }
    }
    let dk = Arc::as_ptr(&dial_inbox.0) as usize;
    unit.seal(&dial_inbox);
    sealing.insert(dk, dial_inbox);
    while !sealing.is_empty() {
        let (t, k) = loop {
            if let Some(pair) = pending_takes.pop_front() {
                break pair;
            }
            if let Some((t, _)) = unit.take() {
                break (t, unit.took_ident);
            }
            if unit.port.arm() {
                thread::park();
            }
        };
        if matches!(t, Took::SealAnswer) {
            if let Some(inbox) = sealing.remove(&k) {
                let (_, rel) = unit.clean(&inbox);
                for (rt, rident, _) in rel {
                    pending_takes.push_back((rt, rident));
                }
            }
        }
    }
    0
}

fn pop_masked_client_ws(buf: &mut Vec<u8>) -> Option<(u8, Vec<u8>)> {
    if buf.len() < 6 {
        return None;
    }
    let opcode = buf[0] & 0x0f;
    let masked = (buf[1] & 0x80) != 0;
    let len7 = (buf[1] & 0x7f) as usize;
    let (len, hdr) = match len7 {
        0..=125 => (len7, 2),
        126 => {
            if buf.len() < 8 {
                return None;
            }
            (u16::from_be_bytes([buf[2], buf[3]]) as usize, 4)
        }
        127 => {
            if buf.len() < 14 {
                return None;
            }
            (
                u64::from_be_bytes(buf[2..10].try_into().unwrap()) as usize,
                10,
            )
        }
        _ => return None,
    };
    let mask_len = if masked { 4 } else { 0 };
    if buf.len() < hdr + mask_len + len {
        return None;
    }
    let mut payload = buf[hdr + mask_len..hdr + mask_len + len].to_vec();
    if masked {
        let mask = &buf[hdr..hdr + 4];
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i & 3];
        }
    }
    buf.drain(..hdr + mask_len + len);
    Some((opcode, payload))
}

static MOCK_PROGRAM: EmbedProgram = EmbedProgram {
    manifest: TEST_MANIFEST,
    target: "x86_64-unknown-linux-gnu",
    root_limit: 16 << 20,
    parameters: TEST_PARAMS,
    parameter_names: TEST_PARAM_NAMES,
    revision: None,
    place: true,
    foreign_waits: false,
    sources: ptr::null(),
    entry: mock_place_entry,
    probe_dir: Some(|_, _| Ok(())),
};

static MOCK_PROGRAM_FOREIGN: EmbedProgram = EmbedProgram {
    manifest: TEST_MANIFEST,
    target: "x86_64-unknown-linux-gnu",
    root_limit: 16 << 20,
    parameters: TEST_PARAMS_FOREIGN,
    parameter_names: TEST_PARAM_NAMES_FOREIGN,
    revision: None,
    place: true,
    foreign_waits: true,
    sources: ptr::null(),
    entry: mock_place_entry,
    probe_dir: Some(|_, _| Ok(())),
};

// ---------------------------------------------------------------------------
// Test callback recorder & helper builders
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RuntimeRecorder {
    ended: Mutex<Vec<(u32, Vec<u8>)>>,
    cv: Condvar,
    reentrant_result: AtomicU32,
}

unsafe extern "C" fn record_ended(ctx: *mut c_void, code: u32, reason: HirselBytes) {
    // SAFETY: `ctx` points to a live `RuntimeRecorder`.
    let rec = unsafe { &*ctx.cast::<RuntimeRecorder>() };
    let bytes = unsafe { reason.as_slice().unwrap_or(&[]) }.to_vec();
    rec.ended.lock().unwrap().push((code, bytes));
    rec.cv.notify_all();
}

#[derive(Default)]
struct SessionRecorder {
    outcomes: Mutex<Vec<(u64, Vec<u8>)>>,
    changes: Mutex<Vec<(u64, Vec<u8>)>>,
    sub_ends: Mutex<Vec<(u64, Vec<u8>)>>,
    room_events: Mutex<Vec<(u64, Vec<u8>)>>,
    session_ends: Mutex<Vec<Vec<u8>>>,
    cv: Condvar,
    /// Optional hook run inside `outcome` callback (e.g. for self-close / busy stop tests).
    on_outcome_hook: Mutex<Option<Box<dyn FnMut(u64) + Send>>>,
}

impl SessionRecorder {
    fn wait_outcome(&self, expected: usize) {
        let mut g = self.outcomes.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while g.len() < expected {
            let rem = deadline.saturating_duration_since(Instant::now());
            assert!(
                !rem.is_zero(),
                "timed out waiting for {expected} outcomes, got {}",
                g.len()
            );
            g = self.cv.wait_timeout(g, rem).unwrap().0;
        }
    }

    fn wait_session_end(&self) -> SessionEnd {
        let mut g = self.session_ends.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while g.is_empty() {
            let rem = deadline.saturating_duration_since(Instant::now());
            assert!(!rem.is_zero(), "timed out waiting for session_ended");
            g = self.cv.wait_timeout(g, rem).unwrap().0;
        }
        SessionEnd::decode(&g[0]).unwrap()
    }
}

unsafe extern "C" fn rec_outcome(ctx: *mut c_void, call: u64, bytes: HirselBytes) {
    // SAFETY: `ctx` points to a live `SessionRecorder`.
    let rec = unsafe { &*ctx.cast::<SessionRecorder>() };
    let v = unsafe { bytes.as_slice().unwrap_or(&[]) }.to_vec();
    rec.outcomes.lock().unwrap().push((call, v));
    rec.cv.notify_all();
    let hook = rec.on_outcome_hook.lock().unwrap().take();
    if let Some(mut f) = hook {
        f(call);
    }
}

unsafe extern "C" fn rec_change(ctx: *mut c_void, sub: u64, bytes: HirselBytes) {
    // SAFETY: `ctx` points to a live `SessionRecorder`.
    let rec = unsafe { &*ctx.cast::<SessionRecorder>() };
    let v = unsafe { bytes.as_slice().unwrap_or(&[]) }.to_vec();
    rec.changes.lock().unwrap().push((sub, v));
    rec.cv.notify_all();
}

unsafe extern "C" fn rec_sub_ended(ctx: *mut c_void, sub: u64, bytes: HirselBytes) {
    // SAFETY: `ctx` points to a live `SessionRecorder`.
    let rec = unsafe { &*ctx.cast::<SessionRecorder>() };
    let v = unsafe { bytes.as_slice().unwrap_or(&[]) }.to_vec();
    rec.sub_ends.lock().unwrap().push((sub, v));
    rec.cv.notify_all();
}

unsafe extern "C" fn rec_room_event(ctx: *mut c_void, room: u64, bytes: HirselBytes) {
    // SAFETY: `ctx` points to a live `SessionRecorder`.
    let rec = unsafe { &*ctx.cast::<SessionRecorder>() };
    let v = unsafe { bytes.as_slice().unwrap_or(&[]) }.to_vec();
    rec.room_events.lock().unwrap().push((room, v));
    rec.cv.notify_all();
}

unsafe extern "C" fn rec_session_ended(ctx: *mut c_void, bytes: HirselBytes) {
    // SAFETY: `ctx` points to a live `SessionRecorder`.
    let rec = unsafe { &*ctx.cast::<SessionRecorder>() };
    let v = unsafe { bytes.as_slice().unwrap_or(&[]) }.to_vec();
    rec.session_ends.lock().unwrap().push(v);
    rec.cv.notify_all();
}

fn make_callbacks(rec: &SessionRecorder) -> HirselEmbedCallbacks {
    HirselEmbedCallbacks {
        size: size_of::<HirselEmbedCallbacks>() as u32,
        context: (rec as *const SessionRecorder).cast_mut().cast(),
        outcome: Some(rec_outcome),
        change: Some(rec_change),
        sub_ended: Some(rec_sub_ended),
        room_event: Some(rec_room_event),
        session_ended: Some(rec_session_ended),
    }
}

fn make_config(
    fp: &[u8; 32],
    store_dir: &str,
    grants: &[u8],
    rec: &RuntimeRecorder,
) -> HirselEmbedConfig {
    HirselEmbedConfig {
        size: size_of::<HirselEmbedConfig>() as u32,
        abi: ABI_VERSION,
        manifest_fp: HirselBytes::from_slice(fp),
        digest: HirselBytes::default(),
        n_place: 16 << 20,
        client_bytes: PROGRESS_RESERVE + 65_536,
        start_timeout_ms: 2_000,
        grants: HirselBytes::from_slice(grants),
        dir_bases: HirselBytes::default(),
        store_dir: HirselBytes::from_slice(store_dir.as_bytes()),
        context: (rec as *const RuntimeRecorder).cast_mut().cast(),
        ended: Some(record_ended),
    }
}

#[path = "tests_laws.rs"]
mod laws;
