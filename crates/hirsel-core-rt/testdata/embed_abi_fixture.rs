//! Rust fixture crate for the Slice 2 native C + ASan + refusing-allocator
//! + shared 3145 kernel vector harnesses (`embed_abi_asan.c`, `embed_abi_oom.c`,
//! `embed_abi_vectors.c`). Links against the embed-slot `hirsel-core-rt`
//! (`features = []`).
use hirsel_core_ids::embed::{HEADER_V1, manifest_fingerprint};
use hirsel_core_rt::{
    Context,
    embed::{
        EmbedProgram,
        abi::*,
        codec::{Caller, Entry, Frame, KeyedDone, Outcome},
        dial::{DialServer, DialServerEvent},
        kernel::{
            self, Action, Attempt, ClientState, Event, LostCall, Phase, StepContext, StoppedState,
        },
        ws,
    },
    entry::{DIR, PROCESS, Requirement},
    process::{Config, MainResult},
    run_stop,
};
use std::{
    collections::HashMap,
    ptr,
    sync::atomic::{AtomicI32, Ordering},
    thread,
    time::Duration,
};

const FIXTURE_MANIFEST: &[u8] = br#"{"version":1,"launch":{"digest":[],"target":"x86_64-unknown-linux-gnu","n_place":"16777216","revision":[]},"tables":[],"entries":[{"module":["app"],"name":"echo","kind":"transact","identity":[1,2,3],"revision":[],"args_fp":"101","result_fp":"202","args":{"root":[0],"definitions":[],"key":false},"result":{"root":[0],"definitions":[],"key":false},"slots":[]}]}"#;

static PARAMS: &[Requirement] = &[
    Requirement {
        kind: DIR,
        index: 0,
    },
    Requirement {
        kind: PROCESS,
        index: 0,
    },
];
static PARAM_NAMES: &[&str] = &["store", "process"];

// 0 = normal echo, 1 = spontaneous RunStop after 40ms, 2 = refuse capacity
static FIXTURE_MODE: AtomicI32 = AtomicI32::new(0);

fn emit_bytes(ctx: *const Context, data: &[u8]) {
    let buf = hirsel_core_rt::provider::Buffer {
        data: data.as_ptr().cast_mut(),
        len: data.len(),
        capacity: data.len(),
    };
    // SAFETY: `ctx` is live run context and `buf` borrows `data`.
    unsafe {
        hirsel_core_rt::process::hirsel_core_describe_emit(ctx, &buf);
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

unsafe extern "C" fn fixture_entry(
    ctx: *mut Context,
    cfg: *const Config,
    _res: *mut MainResult,
) -> u8 {
    let mode = FIXTURE_MODE.load(Ordering::Acquire);
    if mode == 2 {
        let mut out = vec![4u8];
        out.extend_from_slice(&4_000_000u64.to_le_bytes());
        out.extend_from_slice(&1024u64.to_le_bytes());
        emit_bytes(ctx, &out);
        return 1;
    }

    emit_bytes(ctx, &[1u8]);
    let mut server = DialServer::bind_and_capture();
    emit_bytes(ctx, &[2u8]);

    if mode == 1 {
        thread::sleep(Duration::from_millis(40));
        run_stop::raise(run_stop::Stop::from_static(b"AsanFault"));
    }

    let mut bufs: HashMap<usize, Vec<u8>> = HashMap::new();
    let mut version = 1u64;

    loop {
        match server.next_event() {
            DialServerEvent::DialerClosed => break,
            DialServerEvent::Connected(id) => {
                bufs.insert(id, Vec::new());
            }
            DialServerEvent::Closed(id) => {
                bufs.remove(&id);
            }
            DialServerEvent::Bytes(id, chunk) => {
                let mut replies: Vec<Vec<u8>> = Vec::new();
                if let Some(ws_buf) = bufs.get_mut(&id) {
                    ws_buf.extend_from_slice(&chunk);
                    while let Some((opcode, payload)) = pop_masked_client_ws(ws_buf) {
                        if opcode == ws::OP_CLOSE {
                            replies
                                .push(encode_server_ws_frame(ws::OP_CLOSE, &1000u16.to_be_bytes()));
                            continue;
                        }
                        if opcode != ws::OP_BINARY {
                            continue;
                        }
                        let Ok(frame) = Frame::decode(&payload) else {
                            continue;
                        };
                        match frame {
                            Frame::Hello(_, wants) => {
                                // SAFETY: `cfg.launch` lives through the run.
                                let target = unsafe { (*(*cfg).launch).target().to_owned() };
                                let welcome = Frame::Welcome {
                                    caller: Caller {
                                        issuer: 1,
                                        subject: vec![1, 2],
                                    },
                                    q: 4,
                                    budget: version,
                                    digest: vec![],
                                    target,
                                    n_place: 16 << 20,
                                    now_ms: 262_144,
                                    retention_ms: 60_000,
                                };
                                replies
                                    .push(encode_server_ws_frame(ws::OP_BINARY, &welcome.encode()));
                                for i in 0..wants.len() {
                                    let placed = Frame::Placed(Entry::Ready(i as u32));
                                    replies.push(encode_server_ws_frame(
                                        ws::OP_BINARY,
                                        &placed.encode(),
                                    ));
                                }
                            }
                            Frame::Call(stream, p, _, _, args) => {
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
                                replies
                                    .push(encode_server_ws_frame(ws::OP_BINARY, &batch.encode()));
                            }
                            _ => {}
                        }
                    }
                }
                for raw in replies {
                    server.send_bytes(id, &raw);
                }
            }
        }
    }

    server.shutdown();
    0
}

static PROGRAM: EmbedProgram = EmbedProgram {
    manifest: FIXTURE_MANIFEST,
    target: "x86_64-unknown-linux-gnu",
    root_limit: 16 << 20,
    parameters: PARAMS,
    parameter_names: PARAM_NAMES,
    revision: None,
    place: true,
    foreign_waits: false,
    sources: ptr::null(),
    entry: fixture_entry,
    probe_dir: Some(|_, _| Ok(())),
};

#[unsafe(no_mangle)]
pub extern "C" fn embed_abi_fixture_init() {
    hirsel_core_rt::embed::register_program(&PROGRAM);
    let _ = hirsel_embed_v1_abi();
}

#[unsafe(no_mangle)]
pub extern "C" fn embed_abi_fixture_set_mode(mode: i32) {
    FIXTURE_MODE.store(mode, Ordering::Release);
}

/// # Safety
/// `out_32` must point to 32 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_abi_fixture_manifest_fp(out_32: *mut u8) {
    let fp = manifest_fingerprint(FIXTURE_MANIFEST).unwrap();
    // SAFETY: caller provides 32 writable bytes.
    unsafe {
        ptr::copy_nonoverlapping(fp.as_ptr(), out_32, 32);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn embed_abi_fixture_header() -> HirselBytes {
    HirselBytes::from_slice(HEADER_V1.as_bytes())
}

#[unsafe(no_mangle)]
pub extern "C" fn embed_abi_fixture_discriminants(visit: extern "C" fn(HirselBytes, i64)) {
    macro_rules! visit_codes {
        ($($code:ident),+ $(,)?) => {
            $(visit(HirselBytes::from_slice(stringify!($code).as_bytes()), i64::from($code));)+
        };
    }
    visit_codes!(
        HIRSEL_OK,
        HIRSEL_ABI_MISMATCH,
        HIRSEL_REFUSED,
        HIRSEL_STOPPED,
        HIRSEL_UNKNOWN_ENTRY,
        HIRSEL_BUSY,
        HIRSEL_ALREADY_RUNNING,
        HIRSEL_ID_IN_USE,
        HIRSEL_DEFERRED,
        HIRSEL_NOT_LIVE,
        HIRSEL_FULL,
        HIRSEL_HOST_MEMORY,
        HIRSEL_OVER_BOUND,
        HIRSEL_END_STOPPED,
        HIRSEL_END_REFUSED,
        HIRSEL_END_PLACE_FAILED,
        HIRSEL_END_RUN_STOP,
    );
}

// ---------------------------------------------------------------------------
// Shared 3145 transition vector runner (`sdk/client/state.ts` == `embed::kernel`)
// ---------------------------------------------------------------------------

fn phase_tag(p: &Phase) -> &'static str {
    match p {
        Phase::Initial { .. } => "Initial",
        Phase::Acquiring { .. } => "Acquiring",
        Phase::Starting { .. } => "Starting",
        Phase::Opening { .. } => "Opening",
        Phase::AwaitWelcome { .. } => "AwaitWelcome",
        Phase::Binding { .. } => "Binding",
        Phase::Placing { .. } => "Placing",
        Phase::Ready { .. } => "Ready",
        Phase::Losing { .. } => "Losing",
        Phase::Backoff { .. } => "Backoff",
        Phase::Incompatible { .. } => "Incompatible",
        Phase::Unbound { .. } => "Unbound",
        Phase::Stopped {
            state: StoppedState::Fenced,
            ..
        } => "Fenced",
        Phase::Stopped {
            state: StoppedState::StorageUnavailable,
            ..
        } => "StorageUnavailable",
        Phase::Stopped {
            state: StoppedState::Closed,
            ..
        } => "Closed",
    }
}

fn phase_backoff(p: &Phase) -> i64 {
    match p {
        Phase::Initial { backoff, .. }
        | Phase::Acquiring { backoff, .. }
        | Phase::Starting { backoff, .. }
        | Phase::Opening { backoff, .. }
        | Phase::AwaitWelcome { backoff, .. }
        | Phase::Binding { backoff, .. }
        | Phase::Placing { backoff, .. }
        | Phase::Ready { backoff, .. }
        | Phase::Losing { backoff, .. }
        | Phase::Backoff { backoff, .. }
        | Phase::Incompatible { backoff, .. }
        | Phase::Unbound { backoff, .. } => *backoff as i64,
        Phase::Stopped { .. } => -1,
    }
}

fn client_state_name(s: ClientState) -> &'static str {
    match s {
        ClientState::Connecting => "Connecting",
        ClientState::Ready => "Ready",
        ClientState::Backoff => "Backoff",
        ClientState::CredentialNeeded => "CredentialNeeded",
        ClientState::Incompatible => "Incompatible",
        ClientState::Unbound => "Unbound",
        ClientState::Fenced => "Fenced",
        ClientState::StorageUnavailable => "StorageUnavailable",
        ClientState::Closed => "Closed",
    }
}

fn action_sig(a: &Action) -> String {
    match a {
        Action::CancelTimers => "CancelTimers".into(),
        Action::ClearSubscriptions => "ClearSubscriptions".into(),
        Action::CloseSocket => "CloseSocket".into(),
        Action::OpenSocket => "OpenSocket".into(),
        Action::SendHello => "SendHello".into(),
        Action::RecordLoss => "RecordLoss".into(),
        Action::ResetReady => "ResetReady".into(),
        Action::ContinueFrame => "ContinueFrame".into(),
        Action::Begin => "Begin".into(),
        Action::SendPending => "SendPending".into(),
        Action::State(s) => format!("State:{}", client_state_name(*s)),
        Action::RefreshCredential => "RefreshCredential".into(),
        Action::Diagnostic(_) => "Diagnostic".into(),
        Action::BindWelcome(_) => "BindWelcome".into(),
        Action::Update(_) => "Update".into(),
        Action::ArmBackoff { max_ms } => format!("ArmBackoff:{max_ms}"),
        Action::ArmRetry { lane, max_ms } => format!("ArmRetry:{lane}:{max_ms}"),
        Action::BecomeReady { welcomed_at } => format!("BecomeReady:{welcomed_at}"),
        Action::Finish(LostCall::LocallyUnknown) => "Finish:LocallyUnknown".into(),
        Action::Finish(LostCall::RequestMismatch) => "Finish:RequestMismatch".into(),
        Action::RejectReady(c) => format!("RejectReady:{c}"),
        Action::RejectCalls(c) => format!("RejectCalls:{c}"),
        Action::ClearOverlays => "ClearOverlays".into(),
        Action::RunAdvance { .. }
        | Action::RunPrepare { .. }
        | Action::RunSend { .. }
        | Action::RunHttpComplete { .. } => "Run".into(),
    }
}

fn run_kernel_script(script: &str) -> String {
    let mut out = String::new();
    let mut ctx = StepContext {
        wants: 0,
        has_credential_source: false,
    };
    let mut phase = Phase::initial();
    for raw_line in script.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts[0] == "SCENARIO" {
            let name = parts[1];
            ctx.wants = parts[2].parse().unwrap();
            ctx.has_credential_source = parts[3] == "1";
            phase = Phase::initial();
            out.push_str(&format!("SCENARIO {name}\n"));
            continue;
        }
        let ev = match parts[0] {
            "Start" => Event::Start,
            "Reconnect" => Event::Reconnect,
            "Begin" => Event::Begin {
                epoch: parts[1].parse().unwrap(),
            },
            "SocketOpen" => Event::SocketOpen {
                epoch: parts[1].parse().unwrap(),
            },
            "CredentialResult" => {
                let epoch = parts[1].parse().unwrap();
                let bytes = if parts[2] == "-" {
                    None
                } else {
                    Some(parts[2].as_bytes().to_vec())
                };
                Event::CredentialResult { epoch, bytes }
            }
            "SocketLost" => {
                let epoch = parts[1].parse().unwrap();
                let code = if parts[2] == "-" {
                    None
                } else {
                    Some(parts[2].parse().unwrap())
                };
                Event::SocketLost { epoch, code }
            }
            "LossRecorded" => Event::LossRecorded {
                epoch: parts[1].parse().unwrap(),
            },
            "BackoffElapsed" => Event::BackoffElapsed {
                epoch: parts[1].parse().unwrap(),
            },
            "StableElapsed" => Event::StableElapsed {
                epoch: parts[1].parse().unwrap(),
                at: parts[2].parse().unwrap(),
            },
            "FrameWelcome" => Event::Frame {
                epoch: parts[1].parse().unwrap(),
                frame: Frame::Welcome {
                    caller: Caller {
                        issuer: 1,
                        subject: vec![1],
                    },
                    q: parts[2].parse().unwrap(),
                    budget: 0,
                    digest: vec![],
                    target: "t".into(),
                    n_place: 16 << 20,
                    now_ms: 262_144,
                    retention_ms: 1000,
                },
            },
            "WelcomeBound" => Event::WelcomeBound {
                epoch: parts[1].parse().unwrap(),
                at: parts[2].parse().unwrap(),
                width: parts[3].parse().unwrap(),
            },
            "FramePlaced" => Event::Frame {
                epoch: parts[1].parse().unwrap(),
                frame: Frame::Placed(if parts[2] == "gone" {
                    Entry::Gone
                } else {
                    Entry::Ready(parts[2].parse().unwrap())
                }),
            },
            "FrameBatch" => Event::Frame {
                epoch: parts[1].parse().unwrap(),
                frame: Frame::Batch {
                    version: parts[2].parse().unwrap(),
                    complete: vec![],
                    groups: vec![],
                    outcomes: vec![],
                },
            },
            "AdvanceVersion" => Event::AdvanceVersion {
                epoch: parts[1].parse().unwrap(),
                version: parts[2].parse().unwrap(),
            },
            "PrepareCall" => Event::PrepareCall {
                epoch: parts[1].parse().unwrap(),
                lane: parts[2].parse().unwrap(),
            },
            "SendCall" => Event::SendCall {
                epoch: parts[1].parse().unwrap(),
                lane: parts[2].parse().unwrap(),
                delivery: if parts[3] == "ws" {
                    Attempt::WebSocket
                } else {
                    Attempt::Http {
                        p: parts[3].parse().unwrap(),
                    }
                },
            },
            "CallFinished" => Event::CallFinished {
                epoch: parts[1].parse().unwrap(),
                lane: parts[2].parse().unwrap(),
            },
            "RetryCall" => Event::RetryCall {
                epoch: parts[1].parse().unwrap(),
                lane: parts[2].parse().unwrap(),
            },
            "RetryElapsed" => Event::RetryElapsed {
                epoch: parts[1].parse().unwrap(),
                lane: parts[2].parse().unwrap(),
            },
            "Stop" => Event::Stop {
                state: match parts[1] {
                    "Closed" => StoppedState::Closed,
                    "Fenced" => StoppedState::Fenced,
                    _ => StoppedState::StorageUnavailable,
                },
                outcome: match parts.get(2).copied() {
                    Some("LocallyUnknown") => Some(LostCall::LocallyUnknown),
                    Some("RequestMismatch") => Some(LostCall::RequestMismatch),
                    _ => None,
                },
                diagnostic: None,
            },
            other => panic!("unknown kernel vector event: {other}"),
        };
        let tr = kernel::step(phase, ev, ctx);
        phase = tr.phase;
        let acts: Vec<String> = tr.actions.iter().map(action_sig).collect();
        out.push_str(&format!(
            "{}|{}|{}|{}|{}\n",
            phase_tag(&phase),
            phase.epoch(),
            phase_backoff(&phase),
            phase.owed_count(),
            acts.join(",")
        ));
    }
    out
}

/// Execute a 3145 transition vector script through `embed::kernel::step`.
///
/// # Safety
/// `input` must point to `input_len` readable bytes; `output` must point to `output_cap` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn embed_abi_fixture_run_kernel_vectors(
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_cap: usize,
) -> isize {
    let slice = if input_len == 0 {
        &[][..]
    } else {
        // SAFETY: caller provides readable slice.
        unsafe { std::slice::from_raw_parts(input, input_len) }
    };
    let Ok(script) = std::str::from_utf8(slice) else {
        return -1;
    };
    let result = run_kernel_script(script);
    let bytes = result.as_bytes();
    if bytes.len() > output_cap {
        return -2;
    }
    // SAFETY: caller provides writable `output_cap` buffer.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len());
    }
    bytes.len() as isize
}
