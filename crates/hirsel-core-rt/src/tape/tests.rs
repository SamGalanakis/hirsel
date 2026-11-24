//! Tape laws (REPLAY-SPEC sections 4, 5 and 8). The testdata vectors use an
//! independent encoder and the system libxxhash.
use super::*;
use crate::os::OsProvider;
use std::collections::VecDeque;
use std::{cell::Cell as Shared, io::Read};
/// The code revision the tests open tapes as.
pub(super) const REVISION: [u8; 32] = {
    let mut b = [0; 32];
    let mut i = 0;
    while i < 32 {
        b[i] = 0x20 + i as u8;
        i += 1;
    }
    b
};
#[path = "aead_tests.rs"]
mod aead;
#[path = "kdf_tests.rs"]
mod kdf;
#[path = "rl2_tests.rs"]
mod rl2;
const HAND: &[u8] = include_bytes!("testdata/hand-tape.bin");
const MODEL: &[u8] = include_bytes!("testdata/model-f4-tape.bin");
const XXH3: &[u8] = include_bytes!("testdata/xxh3.bin");

/// Counts every callback that reaches it. `inner: None` refuses everything:
/// the refuse-all host of the bypass law.
struct Counted {
    inner: Option<Provider>,
    calls: Shared<usize>,
}
impl Counted {
    fn new(inner: Option<Provider>) -> Box<Self> {
        Box::new(Self {
            inner,
            calls: Shared::new(0),
        })
    }
    fn provider(&self) -> Provider {
        unsafe extern "C" fn wait(s: *mut u8, r: *mut Request, t: u64) -> Wait {
            let c = unsafe { &*s.cast::<Counted>() };
            c.calls.set(c.calls.get() + 1);
            match c.inner {
                Some(p) => unsafe { (p.wait)(p.state, r, t) },
                None => Wait::default(),
            }
        }
        unsafe extern "C" fn issue(s: *mut u8, r: *mut Request, t: u64) -> Step {
            let c = unsafe { &*s.cast::<Counted>() };
            c.calls.set(c.calls.get() + 1);
            match c.inner {
                Some(p) => unsafe { (p.issue)(p.state, r, t) },
                None => Step {
                    kind: STEP_DONE,
                    outcome: Outcome {
                        error: IoError { tag: 2, code: 0 },
                        ..Outcome::default()
                    },
                    deadline_ns: 0,
                },
            }
        }
        // Only a host that parked is resumed or aborted.
        unsafe extern "C" fn resume(s: *mut u8, r: *mut Request, t: u64) -> Step {
            let c = unsafe { &*s.cast::<Counted>() };
            c.calls.set(c.calls.get() + 1);
            let p = c.inner.unwrap();
            unsafe { (p.resume)(p.state, r, t) }
        }
        unsafe extern "C" fn abort(s: *mut u8, r: *mut Request, t: u64, why: u8) -> Outcome {
            let c = unsafe { &*s.cast::<Counted>() };
            c.calls.set(c.calls.get() + 1);
            let p = c.inner.unwrap();
            unsafe { (p.abort)(p.state, r, t, why) }
        }
        unsafe extern "C" fn deliver(s: *mut u8, r: *mut Request, o: Outcome) -> Outcome {
            let c = unsafe { &*s.cast::<Counted>() };
            c.calls.set(c.calls.get() + 1);
            match c.inner {
                Some(p) => unsafe { (p.deliver)(p.state, r, o) },
                None => o,
            }
        }
        Provider {
            state: (self as *const Self).cast_mut().cast(),
            wait,
            issue,
            resume,
            abort,
            deliver,
        }
    }
}

/// The scripted host of `scripted_run`: every answer is fixed, as vectors.py
/// encodes it.
struct World {
    walls: VecDeque<u64>,
    monos: VecDeque<u64>,
    data: Vec<u8>,
    at: usize,
}
impl World {
    fn new(read: usize) -> Box<Self> {
        Box::new(Self {
            walls: [1_700_000_000_000, 1_700_000_000_250].into(),
            monos: [5_000, 4_000].into(),
            data: (0..read).map(|i| (i * 7 + 1) as u8).collect(),
            at: 0,
        })
    }
    fn provider(&mut self) -> Provider {
        unsafe extern "C" fn issue(s: *mut u8, r: *mut Request, _: u64) -> Step {
            let w = unsafe { &mut *s.cast::<World>() };
            let r = unsafe { &mut *r };
            let path = unsafe { std::slice::from_raw_parts(r.path, r.path_len) };
            let mut o = Outcome::default();
            match r.operation {
                WALL_MS => o.value = w.walls.pop_front().unwrap(),
                MONO_NS => o.value = w.monos.pop_front().unwrap(),
                STDOUT => o.written = r.buffer.len as u64,
                OPEN if path == b"missing" => o.error = IoError { tag: 1, code: 0 },
                OPEN => o.value = 77,
                READ => {
                    let n = (r.buffer.capacity - r.buffer.len).min(w.data.len() - w.at);
                    unsafe {
                        ptr::copy_nonoverlapping(
                            w.data[w.at..].as_ptr(),
                            r.buffer.data.add(r.buffer.len),
                            n,
                        )
                    };
                    r.buffer.len += n;
                    w.at += n;
                    o.value = n as u64;
                }
                MKDTEMP => {
                    unsafe {
                        ptr::copy_nonoverlapping(
                            b"abc123".as_ptr(),
                            r.buffer.data.add(r.buffer.len),
                            6,
                        )
                    };
                    r.buffer.len += 6;
                    o.value = 78;
                }
                CLOSE | SLEEP_UNTIL | RENAME | SYNC_DIR => {}
                op => panic!("the scripted world has no op {op}"),
            }
            Step {
                kind: STEP_DONE,
                outcome: o,
                deadline_ns: 0,
            }
        }
        Provider {
            state: (self as *mut Self).cast(),
            wait: crate::provider::immediate::wait,
            issue,
            resume: crate::provider::immediate::resume,
            abort: crate::provider::immediate::abort,
            deliver: crate::provider::deliver,
        }
    }
}

/// An in-memory Dir (handle 1) holding tape files, for the campaigns.
#[derive(Default)]
pub(super) struct Store {
    pub(super) files: BTreeMap<Vec<u8>, Vec<u8>>,
    pub(super) close_to: Option<std::path::PathBuf>,
    open: BTreeMap<u64, (Vec<u8>, usize)>,
    next: u64,
    write_error: Option<IoError>,
    fail_after_bytes: Option<(usize, IoError)>,
    read_meter: Option<hirsel_guard::admission::Owner>,
    read_peak: u64,
    bytes_read: usize,
}
impl Store {
    fn with(bytes: &[u8]) -> Box<Self> {
        let mut s = Box::<Self>::default();
        s.files.insert(b"t".to_vec(), bytes.to_vec());
        s
    }
    pub(super) fn file(&self) -> &[u8] {
        &self.files[&b"t".to_vec()]
    }
    pub(super) fn dir(&mut self) -> (Provider, u64) {
        unsafe extern "C" fn issue(s: *mut u8, r: *mut Request, _: u64) -> Step {
            let st = unsafe { &mut *s.cast::<Store>() };
            let r = unsafe { &mut *r };
            let mut o = Outcome::default();
            match r.operation {
                OPEN => {
                    let path = unsafe { std::slice::from_raw_parts(r.path, r.path_len) }.to_vec();
                    match (r.mode, st.files.contains_key(&path)) {
                        (3, true) => o.error = IoError { tag: 3, code: 0 },
                        (0, false) => o.error = IoError { tag: 1, code: 0 },
                        _ => {
                            st.files.entry(path.clone()).or_default();
                            st.next += 1;
                            st.open.insert(100 + st.next, (path, 0));
                            o.value = 100 + st.next;
                        }
                    }
                }
                WRITE if st.write_error.is_some() => o.error = st.write_error.unwrap(),
                WRITE => {
                    let (name, _) = &st.open[&r.handle];
                    let bytes = unsafe { std::slice::from_raw_parts(r.buffer.data, r.buffer.len) };
                    let file = st.files.get_mut(name).unwrap();
                    if let Some((limit, err)) = st.fail_after_bytes
                        && file.len() + bytes.len() > limit
                    {
                        o.error = err;
                    } else {
                        file.extend_from_slice(bytes);
                        o.written = bytes.len() as u64;
                    }
                }
                READ => {
                    let (name, at) = st.open.get_mut(&r.handle).unwrap();
                    let f = &st.files[name];
                    let mut n = (r.buffer.capacity - r.buffer.len).min(f.len() - *at);
                    if let Some(meter) = &st.read_meter {
                        st.read_peak = st.read_peak.max(meter.bytes());
                        // Exercise short reads of both metadata and payload.
                        n = n.min(509);
                    }
                    st.bytes_read += n;
                    unsafe {
                        ptr::copy_nonoverlapping(
                            f[*at..].as_ptr(),
                            r.buffer.data.add(r.buffer.len),
                            n,
                        )
                    };
                    *at += n;
                    o.value = n as u64;
                }
                CLOSE => {
                    let (name, _) = st.open.remove(&r.handle).unwrap();
                    if let Some(directory) = &st.close_to {
                        let path = directory.join(std::str::from_utf8(&name).unwrap());
                        std::fs::write(path, &st.files[&name]).unwrap();
                    }
                }
                op => panic!("the store has no op {op}"),
            }
            Step {
                kind: STEP_DONE,
                outcome: o,
                deadline_ns: 0,
            }
        }
        let p = Provider {
            state: (self as *mut Self).cast(),
            wait: crate::provider::immediate::wait,
            issue,
            resume: crate::provider::immediate::resume,
            abort: crate::provider::immediate::abort,
            deliver: crate::provider::deliver,
        };
        (p, 1)
    }
}

// ---------- driving operations ----------

/// One observation: the delivered outcome and the carrier's bytes after it.
/// A handle a delivery creates is compared by its ordinal (low 32 bits): the
/// generation differs between a recording and its replay.
#[derive(Debug, PartialEq, Eq)]
struct Ob {
    op: u8,
    value: u64,
    error: IoError,
    written: u64,
    status: u8,
    kind: u8,
    bytes: Vec<u8>,
}

/// Run one operation as the scheduler would for a Ready, Done provider.
fn call(
    sub: (Provider, u64),
    op: u8,
    mode: u8,
    path: &[u8],
    carrier: &mut Vec<u8>,
    input: bool,
) -> Ob {
    let buffer = if input {
        Buffer {
            data: carrier.as_mut_ptr(),
            len: carrier.len(),
            capacity: 0,
        }
    } else {
        Buffer {
            data: carrier.as_mut_ptr(),
            len: carrier.len(),
            capacity: carrier.capacity(),
        }
    };
    let mut r = Request {
        operation: op,
        mode,
        handle: sub.1,
        path: path.as_ptr(),
        path_len: path.len(),
        buffer,
        site: 0,
    };
    let p = sub.0;
    let context = TASK.get();
    // SAFETY: the request and its carrier are pinned for the operation; a
    // task's context is its own while it runs.
    let o = unsafe {
        if context.is_null() {
            let _ = (p.wait)(p.state, &mut r, 0);
            let step = (p.issue)(p.state, &mut r, 0);
            assert_eq!(step.kind, STEP_DONE);
            (p.deliver)(p.state, &mut r, step.outcome)
        } else {
            crate::provider::hirsel_core_host(context, &p, &mut r)
        }
    };
    if !input {
        // SAFETY: the provider initialised the carrier up to the request's len.
        unsafe { carrier.set_len(r.buffer.len) };
    }
    let created = o.status == 0 && o.error == IoError::OK && creates_object(op);
    Ob {
        op,
        value: if created {
            o.value & 0xffff_ffff
        } else {
            o.value
        },
        error: o.error,
        written: o.written,
        status: o.status,
        kind: o.kind,
        bytes: carrier.clone(),
    }
}
thread_local! {
    /// The running task's context: `call` then issues through the scheduler
    /// (`hirsel_core_host`), else it calls the provider directly.
    static TASK: Shared<*mut crate::Context> = const { Shared::new(ptr::null_mut()) };
}
fn input(sub: (Provider, u64), op: u8, bytes: &[u8]) -> Ob {
    call(sub, op, 0, &[], &mut bytes.to_vec(), true)
}
fn plain(sub: (Provider, u64), op: u8, mode: u8, path: &[u8]) -> Ob {
    call(sub, op, mode, path, &mut Vec::new(), true)
}
fn spare(sub: (Provider, u64), op: u8, prefix: &[u8], capacity: usize) -> Ob {
    let mut c = Vec::with_capacity(capacity);
    c.extend_from_slice(prefix);
    call(sub, op, 0, &[], &mut c, false)
}

#[derive(Clone, Copy, PartialEq)]
enum Variant {
    Same,
    OtherWrite,
    KeepFile,
}

/// The run vectors.py encodes. Stops at the first refused on_tape (there is
/// then no substitute to operate on); keeps going through refused operations.
fn scripted_run(
    tape: &Tape,
    world: Provider,
    read: usize,
    variant: Variant,
) -> (Vec<Ob>, Option<Fault>) {
    let mut obs = Vec::new();
    let mut subs = Vec::new();
    for kind in [crate::entry::CLOCK, crate::entry::STDOUT, crate::entry::DIR] {
        match tape.on_tape(kind, (world, 0)) {
            Ok(s) => subs.push(s),
            Err(f) => return (obs, Some(f)),
        }
    }
    match tape.on_tape_args(&[b"a".as_slice(), b"bc"]) {
        Ok(v) => assert_eq!(v, [b"a".to_vec(), b"bc".to_vec()]),
        Err(f) => return (obs, Some(f)),
    }
    let (clock, out, dir) = (subs[0], subs[1], subs[2]);
    obs.push(plain(clock, WALL_MS, 0, &[]));
    obs.push(plain(clock, MONO_NS, 0, &[]));
    obs.push(plain(clock, MONO_NS, 0, &[]));
    obs.push(plain(clock, WALL_MS, 0, &[]));
    let hello: &[u8] = if variant == Variant::OtherWrite {
        b"hellO"
    } else {
        b"hello"
    };
    obs.push(input(out, STDOUT, hello));
    obs.push(input(out, STDOUT, b"0123456789abcdefXYZ"));
    obs.push(plain(dir, OPEN, 0, b"missing"));
    let opened = plain(dir, OPEN, 0, b"data");
    let file = (
        dir.0,
        (opened.value != 0)
            .then(|| handle_like(dir.1, opened.value))
            .unwrap_or(0),
    );
    obs.push(opened);
    obs.push(spare(file, READ, &[], read));
    obs.push(spare(file, READ, &[], 10));
    if variant != Variant::KeepFile {
        obs.push(plain(file, CLOSE, 0, &[]));
    }
    obs.push(input(clock, SLEEP_UNTIL, &9_000u64.to_le_bytes()));
    obs.push(plain(dir, RENAME, 0, b"a\0b"));
    let made = spare(dir, MKDTEMP, b"t.", 64);
    let temp = (
        dir.0,
        (made.value != 0)
            .then(|| handle_like(dir.1, made.value))
            .unwrap_or(0),
    );
    obs.push(made);
    obs.push(plain(temp, SYNC_DIR, 0, &[]));
    (obs, None)
}
/// The substitute handle for `ordinal` on the tape that made `like`.
fn handle_like(like: u64, ordinal: u64) -> u64 {
    (like & !0xffff_ffff) | ordinal
}

pub(super) fn header(build: [u8; 32]) -> Header {
    Header {
        kind: KIND_CALL,
        tape_id: std::array::from_fn(|i| 0x10 + i as u8),
        identity: b"app@1/main/run".to_vec(),
        revision: build,
        charge_policy: 0,
        limit: 1000,
        bound: 0,
        chooser: CHOOSER_FIFO,
        arg: b"arg".to_vec(),
        net_rules: vec![],
        secret_grants: vec![],
        cred_grants: vec![],
        env: None,
    }
}
pub(super) fn expect() -> Expect<'static> {
    Expect {
        identity: b"app@1/main/run",
        charge_policy: 0,
        limit: 1000,
        arg: None,
        across: false,
    }
}

/// The scripted run's outcome, as vectors.py writes it: Failed with
/// Fail("no data") then Overflow.
pub(super) fn ending() -> Ending {
    Ending::causes(END_FAILED, [(4, Some(&b"no data"[..])), (0, None)])
}

/// Record the scripted run into a fresh store; the store, observations and
/// the world's call count.
fn record(read: usize) -> (Box<Store>, Vec<Ob>) {
    let mut store = Box::<Store>::default();
    let mut world = World::new(read);
    let (tape, arg) = unsafe {
        Tape::open(
            REVISION,
            store.dir(),
            b"t",
            Mode::Record,
            &header(REVISION),
            &expect(),
        )
    }
    .unwrap();
    assert_eq!(arg, b"arg");
    let (obs, fault) = scripted_run(&tape, world.provider(), read, Variant::Same);
    assert_eq!(fault, None);
    tape.end(&ending()).unwrap();
    (store, obs)
}

/// Replay `bytes` against the refuse-all host: the open's result, or the
/// observations, the first fault and the verdict of `end`. Asserts that
/// nothing reached the host.
fn replay(
    bytes: &[u8],
    read: usize,
    variant: Variant,
    ending: &Ending,
) -> Result<(Vec<Ob>, Option<Fault>, Result<(), Fault>), OpenError> {
    let mut store = Store::with(bytes);
    let host = Counted::new(None);
    let (tape, _) = unsafe {
        Tape::open(
            REVISION,
            store.dir(),
            b"t",
            Mode::Replay,
            &header(REVISION),
            &expect(),
        )
    }?;
    let (obs, fault) = scripted_run(&tape, host.provider(), read, variant);
    let verdict = tape.end(ending);
    assert_eq!(
        host.calls.get(),
        0,
        "an operation reached the host under Replay"
    );
    Ok((obs, fault, verdict))
}

/// Block boundaries by the length fields alone (section 5.2's layout).
pub(super) fn boundaries(t: &[u8]) -> Vec<usize> {
    let mut at = vec![0];
    let mut p = 0;
    while p < t.len() {
        let len = u32::from_le_bytes(t[p + 12..p + 16].try_into().unwrap()) as usize;
        p += BLOCK_HEAD + len + BLOCK_CHECK;
        at.push(p);
    }
    at
}

// ---------- laws ----------

/// Wrong implementation caught: a block check that differs from libxxhash
/// (wrong seed, secret, a streaming state reset between parts).
#[test]
fn xxh3_is_libxxhash_whole_and_in_two_parts() {
    let image: Vec<u8> = std::iter::repeat_n(77, 11)
        .chain((0..4096u32).map(|i| (i * 17 + 3) as u8))
        .chain(std::iter::repeat_n(99, 13))
        .collect();
    let rows: Vec<(usize, u64)> = XXH3
        .chunks(16)
        .map(|r| {
            (
                u64::from_le_bytes(r[..8].try_into().unwrap()) as usize,
                u64::from_le_bytes(r[8..].try_into().unwrap()),
            )
        })
        .collect();
    for &(n, h) in &rows {
        assert_eq!(xxh3(&[&image[11..11 + n]]), h, "length {n}");
    }
    let (n, h) = rows[257];
    for cut in 0..=n {
        assert_eq!(
            xxh3(&[&image[11..11 + cut], &image[11 + cut..11 + n]]),
            h,
            "cut {cut}"
        );
    }
}

/// Wrong implementation caught: any byte of the encoder off the spec (field
/// order, varint, zig-zag delta, T at a block's first frame, check input).
/// The oracle is vectors.py's format-12 derivation from REPLAY-SPEC §5,
/// checked with libxxhash. Its Call header has a 32-byte revision.
#[test]
fn the_encoder_writes_the_format_12_model_tape_byte_for_byte() {
    let h = Header {
        kind: KIND_CALL,
        tape_id: std::array::from_fn(|i| i as u8),
        identity: b"pkg/charge#0001".to_vec(),
        revision: std::array::from_fn(|i| i as u8),
        charge_policy: 1,
        limit: 1024,
        bound: 0,
        chooser: CHOOSER_FIFO,
        arg: b"arg-7".to_vec(),
        net_rules: vec![],
        secret_grants: vec![],
        cred_grants: vec![],
        env: None,
    };
    let mono = |v: u64| Frame::Op {
        task: 0,
        op: MONO_NS,
        object: 1,
        args: Args::of(&[]),
        out: Out {
            value: zz(v as i64),
            ..Out::default()
        },
    };
    let mut e = Encoder::default();
    let payload = |frames: &[Frame], e: &mut Encoder| {
        e.new_block();
        frames
            .iter()
            .flat_map(|f| e.frame(f).0)
            .collect::<Vec<u8>>()
    };
    let b1 = payload(
        &[
            Frame::Grant {
                gap: 0,
                kind: crate::entry::CLOCK,
                args: None,
            },
            mono(1_000_000_123),
        ],
        &mut e,
    );
    let b2 = payload(&[mono(5_000)], &mut e);
    // The model's outcome: Returned, the two reads' difference as u64 LE.
    let outcome = Ending {
        kind: END_RETURNED,
        outcome: Args::of(&5_000u64.to_le_bytes()),
    };
    let b3 = payload(
        &[Frame::End {
            gap: 2,
            ending: outcome.clone(),
        }],
        &mut e,
    );
    let tape = [
        block(&h.tape_id, 0, 0, &h.encode(h.revision).unwrap()),
        block(&h.tape_id, 0, 1, &b1),
        block(&h.tape_id, 0, 2, &b2),
        block(&h.tape_id, END, 3, &b3),
    ]
    .concat();
    assert_eq!(tape, MODEL);
    let mut bytes = MODEL;
    let (mut blocks, read) = Blocks::open(|buf| bytes.read(buf)).unwrap();
    assert_eq!(read, h);
    let mut d = Decoder::default();
    let mut frames = Vec::new();
    while let Some((flags, p)) = blocks.next(|buf| bytes.read(buf)).unwrap() {
        frames.extend(d.block(p, flags & END != 0).unwrap());
    }
    assert_eq!(frames[1], mono(1_000_000_123));
    assert_eq!(frames[2], mono(5_000));
    assert_eq!(
        frames[3],
        Frame::End {
            gap: 2,
            ending: outcome
        }
    );
}

/// The independent vector covers a Run's ordered duplicate rules, both
/// address outcomes (including an ISSUE/DONE pair), and a continued receive.
#[test]
fn network_outcomes_decode_with_exact_payloads() {
    let mut bytes: &[u8] = include_bytes!("testdata/net-tape.bin");
    let (mut blocks, header) = Blocks::open(|buf| bytes.read(buf)).unwrap();
    let listen = hirsel_core_net::Rule::parse("Listen 127.0.0.1/32 0-65535").unwrap();
    let connect = hirsel_core_net::Rule::parse("Connect ::1/128 1-65535").unwrap();
    assert_eq!(
        header.net_rules,
        [listen, connect, listen].map(hirsel_core_net::GrantRule::Address)
    );

    let mut encoded_header = header.encode(header.revision).unwrap();
    assert_eq!(Header::decode(&encoded_header).unwrap(), header);
    let rules_end = encoded_header.len();
    let mut reversed = encoded_header.clone();
    reversed[rules_end - 4..rules_end].copy_from_slice(&[1, 0, 0, 0]);
    assert!(Header::decode(&reversed).is_err(), "reversed range");
    // The rule count is a one-byte varint before three 23-byte address rules.
    encoded_header[rules_end - 3 * 23 - 1] = 4;
    assert!(Header::decode(&encoded_header).is_err(), "short rule list");

    let mut decoder = Decoder::default();
    let mut frames = Vec::new();
    while let Some((flags, payload)) = blocks.next(|buf| bytes.read(buf)).unwrap() {
        frames.extend(decoder.block(payload, flags & END != 0).unwrap());
    }
    let [
        _,
        _,
        Frame::Op {
            op: LISTEN,
            out: bound,
            ..
        },
        Frame::Issue { op: ACCEPT, .. },
        Frame::Done { out: peer, .. },
        Frame::Op {
            op: RECV_BOUNDED,
            out: read,
            ..
        },
        Frame::Op {
            op: SEND_PAIR,
            out: send,
            ..
        },
        Frame::Op {
            op: RECV_BOUNDED,
            out: error,
            ..
        },
        Frame::End { .. },
    ] = frames.as_slice()
    else {
        panic!("wrong network frame sequence");
    };
    for (address, port, object) in [(bound, 49217u16, 3), (peer, 49218u16, 4)] {
        let mut expected = [0; 19];
        expected[9..13].copy_from_slice(&0x7f000001u32.to_le_bytes());
        expected[17..].copy_from_slice(&port.to_le_bytes());
        assert_eq!(address.payload, expected);
        assert_eq!(address.value, object);
    }
    let bytes: Vec<_> = (0..70_000).map(|i| (i * 7 + 1) as u8).collect();
    assert_eq!(read.value, bytes.len() as u64);
    assert_eq!(read.payload, bytes);
    assert_eq!(send.value, 4);
    assert_eq!((error.status, error.error), (ERROR, (5, 0)));
    assert!(error.payload.is_empty());
}

/// A deadline may wake just as input becomes ready. The provider's final
/// attempt appends a byte, so both runs must return success with that byte.
#[test]
fn socket_deadline_final_receive_records_its_success() {
    unsafe extern "C" fn issue(_: *mut u8, _: *mut Request, _: u64) -> Step {
        pending()
    }

    unsafe extern "C" fn abort(_: *mut u8, request: *mut Request, _: u64, why: u8) -> Outcome {
        assert_eq!(why, ABORT_DEADLINE);
        // SAFETY: this case pins the two-byte buffer with one byte of spare room.
        unsafe {
            let buffer = &mut (*request).buffer;
            buffer.data.add(buffer.len).write(b'b');
            buffer.len += 1;
        }
        Outcome {
            value: 1,
            ..Outcome::default()
        }
    }

    let host = Provider {
        state: ptr::null_mut(),
        wait: crate::provider::immediate::wait,
        issue,
        resume: crate::provider::immediate::resume,
        abort,
        deliver: crate::provider::deliver,
    };
    let mut store = Store::default();
    let h = header(REVISION);
    for mode in [Mode::Record, Mode::Replay] {
        // SAFETY: the store, provider and request operands stay pinned through end.
        unsafe {
            let (tape, _) = Tape::open(REVISION, store.dir(), b"t", mode, &h, &expect()).unwrap();
            let (sub, object) = tape.on_tape(crate::entry::NET, (host, 0)).unwrap();
            let mut network = crate::net::NetworkRequest {
                send_tail: ptr::null(),
                address: crate::net::Address::default(),
                sockets: ptr::null_mut(),
                until: None,
                backlog: 0,
                connecting: None,
                denial: None,
                direction: 1,
                os: &crate::os_net::OPERATIONS,
                tls: ptr::null_mut(),
                tls_send_pending: false,
                max_receive: 0,
            };
            let mut bytes = [b'a', 0];
            let mut request = Request {
                operation: RECV_BOUNDED,
                mode: 0,
                handle: object,
                path: (&mut network as *mut crate::net::NetworkRequest).cast(),
                path_len: size_of_val(&network),
                buffer: Buffer {
                    data: bytes.as_mut_ptr(),
                    len: 1,
                    capacity: 2,
                },
                site: 0,
            };
            assert_eq!((sub.wait)(sub.state, &mut request, 0).kind, WAIT_READY);
            assert_eq!((sub.issue)(sub.state, &mut request, 0).kind, STEP_PENDING);
            let result = if mode == Mode::Record {
                (sub.abort)(sub.state, &mut request, 0, ABORT_DEADLINE)
            } else {
                let step = (sub.resume)(sub.state, &mut request, 0);
                assert_eq!(step.kind, STEP_DONE);
                step.outcome
            };
            let result = (sub.deliver)(sub.state, &mut request, result);
            assert_eq!(
                (result.value, result.error, result.status),
                (1, IoError::OK, 0)
            );
            assert_eq!(request.buffer.len, 2);
            assert_eq!(&bytes, b"ab");
            tape.end(&Ending::returned(&[])).unwrap();
        }
    }
}

/// Wrong implementation caught: Record writing anything other than the
/// spec's frames for these operations (ordinals, clock deltas, the args
/// digest, DATA continuation at 64 KiB, an error's tag, block closing).
#[test]
fn record_writes_the_hand_encoded_tape() {
    let (store, obs) = record(70_000);
    assert_eq!(store.file(), HAND);
    assert_eq!(obs[0].value, 1_700_000_000_000);
    assert_eq!(obs[7].value, 5, "the opened file is ordinal 5");
    assert_eq!(obs[8].bytes.len(), 70_000);
}

/// CAP-9 and replay format §5.2: validate only block zero on open, then
/// replay with a measured buffer allowance independent of the tape's length.
#[test]
fn replay_reads_and_allocates_by_block() {
    use hirsel_guard::admission::{Owner, OwnerKind};

    let input = vec![0x5a; CLOSE_AT];
    let mut encoder = Encoder::default();
    let mut start = encoder
        .frame(&Frame::Grant {
            gap: 0,
            kind: crate::entry::DIR,
            args: None,
        })
        .0;
    start.extend(
        encoder
            .frame(&Frame::Op {
                task: 0,
                op: OPEN,
                object: 1,
                args: Args::of(b"\0data"),
                out: Out {
                    value: 2,
                    ..Out::default()
                },
            })
            .0,
    );
    encoder.new_block();
    let (read, data) = encoder.frame(&Frame::Op {
        task: 0,
        op: READ,
        object: 2,
        args: Args::of(&[]).buffered([input.len() as u64, 0]),
        out: Out {
            value: input.len() as u64,
            payload: input.clone(),
            ..Out::default()
        },
    });
    assert!(data.is_empty());
    let end = encoder
        .frame(&Frame::End {
            gap: 2,
            ending: ending(),
        })
        .0;
    let header_end = boundaries(HAND)[1];
    let h = header(REVISION);
    let mut samples = [(0, 0, 0); 2];

    for (sample, count) in samples.iter_mut().zip([4, 128u64]) {
        let mut store = Store::with(&HAND[..header_end]);
        let file = store.files.get_mut(b"t".as_slice()).unwrap();
        file.extend(block(&h.tape_id, 0, 1, &start));
        for seq in 2..count + 2 {
            file.extend(block(&h.tape_id, 0, seq, &read));
        }
        file.extend(block(&h.tape_id, END, count + 2, &end));
        let host = Counted::new(None);

        let meter = Owner::new(OwnerKind::Ledger);
        store.read_meter = Some(meter.clone());
        let refused = {
            let _measure = meter.enter();
            unsafe { Tape::open([0; 32], store.dir(), b"t", Mode::Replay, &h, &expect()) }
        };
        assert_eq!(refused.err(), Some(OpenError::Refused(Refusal::Revision)));
        let reads_on_refusal = store.bytes_read;
        assert!(store.open.is_empty());
        store.bytes_read = 0;
        store.read_peak = 0;

        let (tape, _) = {
            let _measure = meter.enter();
            unsafe { Tape::open(REVISION, store.dir(), b"t", Mode::Replay, &h, &expect()) }
        }
        .unwrap();
        let reads_on_open = store.bytes_read;
        {
            let _measure = meter.enter();
            let dir = tape
                .on_tape(crate::entry::DIR, (host.provider(), 0))
                .unwrap();
            let opened = plain(dir, OPEN, 0, b"data");
            assert_eq!(opened.value, 2);
            let file = (dir.0, handle_like(dir.1, opened.value));
            for _ in 0..count {
                let got = spare(file, READ, &[], input.len());
                assert_eq!(got.status, 0);
                assert_eq!(got.bytes, input);
            }
            tape.end(&ending()).unwrap();
        }
        *sample = (reads_on_refusal, reads_on_open, store.read_peak);
        assert_eq!(host.calls.get(), 0);
        assert!(store.open.is_empty());
        assert_eq!(store.bytes_read, store.file().len());
    }

    // One maximum-size block plus 128 KiB for the READ carrier, provider
    // bookkeeping and tape state. The 8 MiB input lives outside the meter.
    let allowance = (MAX_LEN + 2 * CLOSE_AT) as u64;
    eprintln!("block-zero bytes and peak live allocations (short, long): {samples:?}");
    for (reads_on_refusal, reads_on_open, peak) in samples {
        assert_eq!(
            reads_on_refusal, header_end,
            "read bytes beyond refused header"
        );
        assert_eq!(
            reads_on_open, header_end,
            "read bytes beyond accepted header"
        );
        assert!((CLOSE_AT as u64..=allowance).contains(&peak), "{samples:?}");
    }

    let mut trailing = HAND.to_vec();
    trailing.push(0);
    let (_, _, result) = replay(&trailing, 70_000, Variant::Same, &ending()).unwrap();
    assert_eq!(
        result,
        Err(Fault::Damaged((boundaries(HAND).len() - 1) as u64))
    );

    let mut store = Store::with(HAND);
    let (tape, _) =
        unsafe { Tape::open(REVISION, store.dir(), b"t", Mode::Replay, &h, &expect()) }.unwrap();
    drop(tape);
    assert!(store.open.is_empty());
    assert_eq!(store.bytes_read, header_end);
}

#[test]
fn record_closes_the_file_when_the_header_write_fails() {
    let error = IoError {
        tag: 5,
        code: libc::ENOSPC as u32,
    };
    let mut store = Box::new(Store {
        write_error: Some(error),
        ..Store::default()
    });
    let result = unsafe {
        Tape::open(
            REVISION,
            store.dir(),
            b"t",
            Mode::Record,
            &header(REVISION),
            &expect(),
        )
    };
    assert_eq!(result.err(), Some(OpenError::Io(error)));
    assert!(
        store.open.is_empty(),
        "the provider still owns the opened file"
    );
}

#[test]
fn record_refuses_a_header_that_overflows_block_zero_before_creating_the_file() {
    let mut h = header(REVISION);
    // Format 12 has 72 fixed bytes,
    // 14 identity bytes + 1 length byte, a 3-byte argument length,
    // and a 1-byte empty grants length.
    const HEADER_BYTES: usize = 72 + 14 + 1 + 3 + 1;
    h.arg = vec![0; MAX_LEN - HEADER_BYTES];
    let mut store = Box::<Store>::default();
    let (tape, _) =
        unsafe { Tape::open(REVISION, store.dir(), b"t", Mode::Record, &h, &expect()) }.unwrap();
    tape.end(&ending()).unwrap();
    assert_eq!(
        boundaries(store.file())[1],
        BLOCK_HEAD + MAX_LEN + BLOCK_CHECK
    );

    h.arg.push(0);
    let result = unsafe {
        Tape::open(
            REVISION,
            store.dir(),
            b"oversized",
            Mode::Record,
            &h,
            &expect(),
        )
    };
    assert_eq!(
        result.err(),
        Some(OpenError::Refused(Refusal::HeaderTooLarge))
    );
    assert!(!store.files.contains_key(b"oversized".as_slice()));
}

/// The bypass law. Wrong implementation caught: a Replay that forwards any
/// operation to a source, or answers one differently from the recording.
#[test]
fn replay_answers_every_operation_from_the_tape_and_none_reaches_the_host() {
    super::schedule_cases::run();
    let (_, recorded) = record(70_000);
    let (obs, fault, verdict) = replay(HAND, 70_000, Variant::Same, &ending()).unwrap();
    assert_eq!(fault, None);
    assert_eq!(verdict, Ok(()));
    assert_eq!(obs, recorded);
}

/// Wrong implementation caught: a reader that hands on frames from a cut
/// block, or calls a cut at a block boundary anything but Missing.
#[test]
fn a_cut_at_every_byte_is_truncated_or_missing() {
    let (store, full) = record(100);
    let t = store.file().to_vec();
    let at = boundaries(&t);
    assert_eq!(*at.last().unwrap(), t.len());
    for cut in 0..t.len() {
        let want = |f: Fault| match f {
            Fault::Diverged(Divergence {
                kind: Mismatch::Missing,
                ..
            }) => at.contains(&cut),
            Fault::Truncated(_) => !at.contains(&cut),
            _ => false,
        };
        match replay(&t[..cut], 100, Variant::Same, &ending()) {
            Err(OpenError::Unreadable(f)) => assert!(want(f), "cut {cut}: {f:?}"),
            Err(e) => panic!("cut {cut}: {e:?}"),
            Ok((obs, _, verdict)) => {
                assert!(want(verdict.unwrap_err()), "cut {cut}: {verdict:?}");
                let answered = obs.iter().take_while(|o| o.status == 0).count();
                assert_eq!(obs[..answered], full[..answered], "cut {cut}");
                assert!(
                    obs[answered..].iter().all(|o| o.status == STATUS_REFUSED),
                    "cut {cut}"
                );
            }
        }
    }
}

/// Wrong implementation caught: a check that skips any byte of a block (the
/// head, the payload, the check itself) or the tape id.
#[test]
fn every_one_byte_change_is_refused() {
    let (store, _) = record(100);
    let t = store.file().to_vec();
    let at = boundaries(&t);
    for i in 0..t.len() {
        let k = at.iter().rposition(|&b| b <= i).unwrap();
        let in_len = (at[k] + 12..at[k] + 16).contains(&i);
        for x in [0x01u8, 0x80, 0xff] {
            let mut m = t.clone();
            m[i] ^= x;
            let r = match replay(&m, 100, Variant::Same, &ending()) {
                Err(OpenError::Refused(Refusal::Damaged(0))) => Fault::Damaged(0),
                Err(OpenError::Unreadable(f)) => f,
                Err(e) => panic!("byte {i}: {e:?}"),
                Ok((_, _, verdict)) => verdict.unwrap_err(),
            };
            let ok = r == Fault::Damaged(k as u64) || (in_len && matches!(r, Fault::Truncated(_)));
            assert!(ok, "byte {i} ^ {x:#x}: {r:?}");
        }
    }
}

/// Cuts inside the hand tape's last block: what was answered before the cut
/// block stays answered identically, then the cut is reported.
#[test]
fn a_cut_in_a_later_block_keeps_earlier_answers() {
    let (_, full) = record(70_000);
    let at = boundaries(HAND);
    for cut in [at[2], at[2] + 1, at[2] + BLOCK_HEAD + 3, at[3] - 1] {
        let (obs, _, verdict) = replay(&HAND[..cut], 70_000, Variant::Same, &ending()).unwrap();
        let ok = match verdict {
            Err(Fault::Diverged(Divergence {
                kind: Mismatch::Missing,
                ..
            })) => cut == at[2],
            Err(Fault::Truncated(_)) => cut != at[2],
            _ => false,
        };
        assert!(ok, "cut {cut}: {verdict:?}");
        let answered = obs.iter().take_while(|o| o.status == 0).count();
        assert!(answered >= 8, "block 1 holds the first eight operations");
        assert_eq!(obs[..answered], full[..answered]);
    }
}

/// Wrong implementation caught: a reader that orders blocks by position
/// rather than by their sequence numbers.
#[test]
fn swapped_blocks_are_damaged() {
    let at = boundaries(HAND);
    assert_eq!(at.len(), 4, "three blocks");
    let swapped = [&HAND[..at[1]], &HAND[at[2]..at[3]], &HAND[at[1]..at[2]]].concat();
    let (obs, fault, verdict) = replay(&swapped, 70_000, Variant::Same, &ending()).unwrap();
    assert!(obs.is_empty());
    assert_eq!(fault, Some(Fault::Damaged(1)));
    assert_eq!(verdict, Err(Fault::Damaged(1)));
}

/// Wrong implementation caught: a Replay that answers by op and object
/// lookup instead of in recorded order.
#[test]
fn swapped_frames_diverge() {
    let (store, _) = record(100);
    let t = store.file().to_vec();
    let at = boundaries(&t);
    let h = header(REVISION);
    let mut d = Decoder::default();
    let p = &t[at[1] + BLOCK_HEAD..at[2] - BLOCK_CHECK];
    let flags = t[at[1]];
    let mut frames = d.block(p, flags & END != 0).unwrap();
    // Frames 4 and 5 are the run's first wall read and first mono read.
    frames.swap(4, 5);
    let mut e = Encoder::default();
    let payload: Vec<u8> = frames.iter().flat_map(|f| e.frame(f).0).collect();
    let m = [
        &t[..at[1]],
        &block(&h.tape_id, flags, 1, &payload),
        &t[at[2]..],
    ]
    .concat();
    let (obs, _, verdict) = replay(&m, 100, Variant::Same, &ending()).unwrap();
    // Frames 1 to 4 are the grants; the wall read meets frame 5, now a mono read.
    let at = Divergence {
        at: 5,
        kind: Mismatch::Op,
    };
    assert_eq!(verdict, Err(Fault::Diverged(at)));
    assert_eq!(
        obs[0].status, STATUS_REFUSED,
        "the first read fails before any answer"
    );
    assert!(
        obs.iter().all(|o| o.status == STATUS_REFUSED),
        "a fault is sticky"
    );
}

/// Wrong implementation caught: a Replay that compares op and object but
/// not arguments.
#[test]
fn a_changed_argument_diverges() {
    let (obs, _, verdict) = replay(HAND, 70_000, Variant::OtherWrite, &ending()).unwrap();
    // Four grants, four clock reads, then the first write: frame 9.
    let at = Divergence {
        at: 9,
        kind: Mismatch::Arg,
    };
    assert_eq!(verdict, Err(Fault::Diverged(at)));
    assert!(obs[..4].iter().all(|o| o.status == 0));
    assert!(obs[4..].iter().all(|o| o.status == STATUS_REFUSED));
}

#[test]
fn launcher_reports_the_first_tampered_argument() {
    const INPUT: &str = "HIRSEL_TEST_TAMPERED_TAPE";
    if let Some(path) = std::env::var_os(INPUT) {
        let bytes = std::fs::read(path).unwrap();
        let mut store = Store::with(&bytes);
        let host = Counted::new(None);
        let (tape, _) = unsafe {
            Tape::open(
                REVISION,
                store.dir(),
                b"t",
                Mode::Replay,
                &header(REVISION),
                &expect(),
            )
        }
        .unwrap();
        tape.set_replay_abort(ReplayAbort {
            stderr: 2,
            sources: ptr::null(),
            diagnose: false,
        });
        scripted_run(&tape, host.provider(), 100, Variant::Same);
        panic!("the launcher should stop at the tampered send");
    }

    // Four grants and four clock reads precede the first STDOUT.
    let (store, _) = record(100);
    let tape = store.file();
    let at = boundaries(&tape);
    let flags = tape[at[1]];
    let mut frames = Decoder::default()
        .block(
            &tape[at[1] + BLOCK_HEAD..at[2] - BLOCK_CHECK],
            flags & END != 0,
        )
        .unwrap();
    let Frame::Op {
        op: STDOUT,
        args: Args::Literal(bytes),
        ..
    } = &mut frames[8]
    else {
        panic!("expected the first STDOUT at frame 9");
    };
    assert_eq!(bytes, b"hello");
    bytes[4] = b'X';
    let mut encoder = Encoder::default();
    let payload: Vec<u8> = frames.iter().flat_map(|f| encoder.frame(f).0).collect();
    let changed = [
        &tape[..at[1]],
        &block(&header(REVISION).tape_id, flags, 1, &payload),
        &tape[at[2]..],
    ]
    .concat();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tape");
    std::fs::write(&path, changed).unwrap();
    let output = crate::test_process::command()
        .args([
            "--exact",
            "tape::tests::launcher_reports_the_first_tampered_argument",
            "--nocapture",
        ])
        .env(INPUT, path)
        .output()
        .unwrap();
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{diagnostic}");
    assert_eq!(
        diagnostic,
        format!(
            "replay: Diverged(Arg) at 9 [event 9: frame OP; expected task=0 op={STDOUT} object=2 args=redacted; observed task=0 op={STDOUT} object=2 args=redacted]\n"
        )
    );
}

/// v2.3 R9: identical operations, changed result. Wrong implementation
/// caught: a replay that does not compare END's outcome (the silent case:
/// it would report the run complete). The hand tape records Failed with
/// Fail("no data") then Overflow; END is frame 20 (4 grants, 15 ops).
#[test]
fn identical_operations_with_a_changed_result_diverge_at_end() {
    let at_end = Err(Fault::Diverged(Divergence {
        at: 20,
        kind: Mismatch::Outcome,
    }));
    let returned = Ending::returned(&0u32.to_le_bytes());
    let reordered = Ending::causes(END_FAILED, [(0, None), (4, Some(&b"no data"[..]))]);
    let cancelled = Ending::causes(END_CANCELLED, [(4, Some(&b"no data"[..])), (0, None)]);
    for changed in [returned, reordered, cancelled] {
        let (obs, fault, verdict) = replay(HAND, 70_000, Variant::Same, &changed).unwrap();
        assert_eq!((obs.len(), fault), (15, None), "the operations replay");
        assert_eq!(verdict, at_end, "{changed:?}");
    }
}

/// v2.3 R9: the same, with only the failure text changed. Wrong
/// implementation caught: an outcome that keeps the cause kinds and drops
/// the Fail texts (or compares only the outcome's kind).
#[test]
fn a_changed_failure_text_diverges_at_end() {
    let changed = Ending::causes(END_FAILED, [(4, Some(&b"no date"[..])), (0, None)]);
    let (_, fault, verdict) = replay(HAND, 70_000, Variant::Same, &changed).unwrap();
    assert_eq!(fault, None);
    assert_eq!(
        verdict,
        Err(Fault::Diverged(Divergence {
            at: 20,
            kind: Mismatch::Outcome
        }))
    );
}

/// Section 5.3's refusals, in order. Wrong implementation caught: an open
/// that skips one comparison or checks them in another order.
#[test]
fn open_refuses_a_changed_code_revision_arg_limit_or_policy() {
    let open = |build: [u8; 32], e: &Expect| {
        let mut store = Store::with(HAND);
        unsafe { Tape::open(build, store.dir(), b"t", Mode::Replay, &header(REVISION), e) }.map(
            |(t, arg)| {
                drop(t);
                arg
            },
        )
    };
    let mut other = REVISION;
    other[31] ^= 1;
    let e = expect();
    assert_eq!(open(REVISION, &e), Ok(b"arg".to_vec()));
    assert_eq!(
        open(
            REVISION,
            &Expect {
                arg: Some(b"arg"),
                ..expect()
            }
        ),
        Ok(b"arg".to_vec())
    );
    let refused = |r| Err(OpenError::Refused(r));
    assert_eq!(
        open(
            REVISION,
            &Expect {
                identity: b"app@1/main/ran",
                ..expect()
            }
        ),
        refused(Refusal::Code)
    );
    assert_eq!(open(other, &e), refused(Refusal::Revision));
    assert_eq!(
        open(
            other,
            &Expect {
                identity: b"x",
                ..expect()
            }
        ),
        refused(Refusal::Code)
    );
    assert_eq!(
        open(
            REVISION,
            &Expect {
                arg: Some(b"arh"),
                ..expect()
            }
        ),
        refused(Refusal::Arg)
    );
    assert_eq!(
        open(
            REVISION,
            &Expect {
                limit: 999,
                ..expect()
            }
        ),
        refused(Refusal::Limit)
    );
    assert_eq!(
        open(
            REVISION,
            &Expect {
                charge_policy: 1,
                ..expect()
            }
        ),
        refused(Refusal::Limit)
    );
    assert_eq!(
        open(
            other,
            &Expect {
                arg: Some(b"arh"),
                limit: 999,
                ..expect()
            }
        ),
        refused(Refusal::Revision)
    );
}

/// Section 5.1 / Step 8.6: a recording write failure after K bytes leaves every
/// program observation identical to a writer that never fails, stops writing at
/// the last complete frame, and reports the truncation at end.
#[test]
fn recording_write_failure_preserves_observations_and_keeps_last_complete_frame() {
    let mut full_store = Box::<Store>::default();
    let mut full_world = World::new(100_000);
    let (full_tape, _) = unsafe {
        Tape::open(
            REVISION,
            full_store.dir(),
            b"t",
            Mode::Record,
            &header(REVISION),
            &expect(),
        )
    }
    .unwrap();
    let (expected_obs, expected_fault) =
        scripted_run(&full_tape, full_world.provider(), 100_000, Variant::Same);
    assert_eq!(expected_fault, None);
    assert_eq!(full_tape.end_with_truncation(&ending()), Ok(None));
    let full_bytes = full_store.file().to_vec();
    let blocks = boundaries(&full_bytes);
    assert!(blocks.len() > 2, "need multiple recorded blocks");

    // Fail writes once the file would cross the middle of block 2: the header
    // and earlier complete blocks succeed, and the failing write is rejected.
    let limit = blocks[2] + 1;
    let enospc = IoError {
        tag: 5,
        code: libc::ENOSPC as u32,
    };
    let mut failing_store = Box::new(Store {
        fail_after_bytes: Some((limit, enospc)),
        ..Store::default()
    });
    let mut failing_world = World::new(100_000);
    let (failing_tape, _) = unsafe {
        Tape::open(
            REVISION,
            failing_store.dir(),
            b"t",
            Mode::Record,
            &header(REVISION),
            &expect(),
        )
    }
    .unwrap();
    let (obs, fault) = scripted_run(
        &failing_tape,
        failing_world.provider(),
        100_000,
        Variant::Same,
    );
    assert_eq!(
        obs, expected_obs,
        "recording failure must not change any program observation"
    );
    assert_eq!(fault, None, "recording failure must not fault the program");
    let truncation = failing_tape.end_with_truncation(&ending()).unwrap();
    let Some((at, err)) = truncation else {
        panic!("expected truncation report from end_with_truncation");
    };
    assert_eq!(err, enospc);
    assert!(at >= 1, "truncation position must be at or after block 1");
    let truncated = failing_store.file();
    assert!(
        truncated.len() <= limit,
        "truncated recording must not exceed failing writer's limit"
    );
    assert_eq!(
        truncated,
        &full_bytes[..truncated.len()],
        "truncated recording must be a prefix of the full recording"
    );
    assert!(
        blocks.contains(&truncated.len()),
        "truncated recording must end on a complete frame boundary"
    );
    let (_, _, verdict) = replay(truncated, 100_000, Variant::Same, &ending()).unwrap();
    assert!(
        matches!(
            verdict,
            Err(Fault::Diverged(Divergence {
                kind: Mismatch::Missing,
                ..
            }))
        ),
        "replaying a frame-aligned truncated recording reaches Missing, got {verdict:?}"
    );
}

/// Section 8, drop. Wrong implementation caught: a drop that loses the
/// buffered frames, or one that writes END (a crash must not look complete).
#[test]
fn a_dropped_tape_keeps_its_frames_and_has_no_end() {
    let mut store = Box::<Store>::default();
    let mut world = World::new(100);
    let (tape, _) = unsafe {
        Tape::open(
            REVISION,
            store.dir(),
            b"t",
            Mode::Record,
            &header(REVISION),
            &expect(),
        )
    }
    .unwrap();
    let (recorded, _) = scripted_run(&tape, world.provider(), 100, Variant::Same);
    drop(tape);
    let (obs, fault, verdict) = replay(store.file(), 100, Variant::Same, &ending()).unwrap();
    assert_eq!((obs, fault), (recorded, None));
    // Four grants and fifteen operations were answered; END would be frame 20.
    let at = Divergence {
        at: 20,
        kind: Mismatch::Missing,
    };
    assert_eq!(verdict, Err(Fault::Diverged(at)));
}

/// A task that takes one tape frame of its own: a clock read.
#[derive(Clone, Copy)]
struct Other {
    clock: (Provider, u64),
    seen: *mut Option<Ob>,
}
unsafe extern "C" fn other(context: *mut crate::Context, _: *mut u8, input: *mut u8) -> u8 {
    // SAFETY: the spawner pins `seen` until it joins this task.
    unsafe {
        let job = input.cast::<Other>().read();
        // This task does not suspend between taking the context and giving
        // the spawner's back.
        let spawner = TASK.replace(context);
        *job.seen = Some(plain(job.clock, WALL_MS, 0, &[]));
        crate::os_sync::hold(false);
        TASK.set(spawner);
    }
    0
}

/// How the host answers SYNC and SYNC_DIR.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Syncs {
    /// The OS: parked while the helper syncs.
    Parked,
    /// The OS, finished inside issue: the tape records OP, as every tape
    /// before the sync helper did.
    Blocking,
    /// The file's SYNC parks and fails with EIO; no syscall.
    Failing,
}
struct SyncHost(Provider, Syncs);
impl SyncHost {
    fn provider(&self) -> Provider {
        unsafe extern "C" fn wait(s: *mut u8, r: *mut Request, t: u64) -> Wait {
            let p = unsafe { (*s.cast::<SyncHost>()).0 };
            unsafe { (p.wait)(p.state, r, t) }
        }
        unsafe extern "C" fn issue(s: *mut u8, r: *mut Request, t: u64) -> Step {
            let SyncHost(p, syncs) = unsafe { &*s.cast::<SyncHost>() };
            if *syncs == Syncs::Failing && unsafe { (*r).operation } == SYNC {
                crate::tasks::hirsel_core_ready(t);
                return Step {
                    kind: STEP_PENDING,
                    ..Step::default()
                };
            }
            let mut step = unsafe { (p.issue)(p.state, r, t) };
            while *syncs == Syncs::Blocking && step.kind == STEP_PENDING {
                std::thread::sleep(std::time::Duration::from_micros(100));
                step = unsafe { (p.resume)(p.state, r, t) };
            }
            step
        }
        unsafe extern "C" fn resume(s: *mut u8, r: *mut Request, t: u64) -> Step {
            let SyncHost(p, syncs) = unsafe { &*s.cast::<SyncHost>() };
            if *syncs == Syncs::Failing && unsafe { (*r).operation } == SYNC {
                return Step {
                    kind: STEP_DONE,
                    outcome: Outcome {
                        error: IoError::from_errno(libc::EIO),
                        ..Outcome::default()
                    },
                    deadline_ns: 0,
                };
            }
            unsafe { (p.resume)(p.state, r, t) }
        }
        unsafe extern "C" fn abort(_: *mut u8, _: *mut Request, _: u64, _: u8) -> Outcome {
            unreachable!("an issued sync is never aborted")
        }
        unsafe extern "C" fn deliver(s: *mut u8, r: *mut Request, o: Outcome) -> Outcome {
            let p = unsafe { (*s.cast::<SyncHost>()).0 };
            unsafe { (p.deliver)(p.state, r, o) }
        }
        Provider {
            state: (self as *const Self).cast_mut().cast(),
            wait,
            issue,
            resume,
            abort,
            deliver,
        }
    }
}

/// The bypass law on the real host, every S1 operation kind, each issued by
/// a task through the scheduler. Wrong implementation caught: one op kind
/// whose Replay reaches the host, or whose recorded answer (value, error,
/// written, carrier bytes) is lost; a parked SYNC whose ISSUE and DONE do not
/// replay around another task's frame; a tape recorded with SYNC as one OP
/// (before the sync helper) that no longer replays.
#[test]
fn every_s1_kind_recorded_on_the_os_replays_with_no_host() {
    crate::openat2_required::require();
    super::run::check_launch_replay();
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let (world_dir, store_dir) = (root.join("world"), root.join("store"));
    std::fs::create_dir_all(&world_dir).unwrap();
    std::fs::create_dir_all(&store_dir).unwrap();
    let stdout = std::fs::File::create(root.join("stdout")).unwrap();
    let stderr = std::fs::File::create(root.join("stderr")).unwrap();
    let mut os = Box::new(OsProvider::new(stdout.into(), stderr.into()));
    let (wd, sd) = (os.grant(&world_dir).unwrap(), os.grant(&store_dir).unwrap());
    let mut h = header(REVISION);
    h.kind = KIND_RUN;
    let (store, real) = (os.provider(), os.provider());

    let ops = |context: *mut crate::Context, tape: &Tape, h: Provider, wd: u64| {
        let dir = tape.on_tape(crate::entry::DIR, (h, wd)).unwrap();
        let clock = tape.on_tape(crate::entry::CLOCK, (h, 0)).unwrap();
        let out = tape.on_tape(crate::entry::STDOUT, (h, 0)).unwrap();
        let err = tape.on_tape(crate::entry::STDERR, (h, 0)).unwrap();
        let sub = |o: &Ob| (dir.0, handle_like(dir.1, o.value));
        let mut obs = vec![plain(dir, MKDIR, 0, b"a")];
        let f = plain(dir, OPEN, 3, b"a/f");
        let file = sub(&f);
        obs.push(f);
        let big: Vec<u8> = (0..100_000u32).map(|i| (i * 13) as u8).collect();
        obs.push(input(file, WRITE, &big));
        // Another task, runnable while this one's SYNC is parked.
        let mut seen = None;
        let mut job = Other {
            clock,
            seen: &mut seen,
        };
        // SAFETY: the job and `seen` outlive the join below.
        let task = unsafe {
            crate::tasks::hirsel_core_spawn(
                context,
                0,
                1 << 16,
                (&mut job as *mut Other).cast(),
                other,
                crate::tasks::TaskLayout {
                    input: crate::tasks::ValueLayout {
                        bytes: size_of::<Other>(),
                        align: align_of::<Other>(),
                        logical: 0,
                        clean: None,
                    },
                    result: crate::tasks::ValueLayout {
                        bytes: 0,
                        align: 1,
                        logical: 0,
                        clean: None,
                    },
                },
                0,
            )
        }
        .task;
        assert!(!task.is_null(), "spawn failed: {:?}", last_fault());
        obs.push(plain(file, SYNC, 0, &[]));
        // SAFETY: the task is this one's child.
        unsafe { crate::tasks::hirsel_core_join(context, task, ptr::null_mut(), 0) };
        obs.push(plain(file, TRY_LOCK, 0, &[]));
        obs.push(plain(file, UNLOCK, 0, &[]));
        obs.push(plain(file, CLOSE, 0, &[]));
        let g = plain(dir, OPEN, 0, b"a/f");
        let read = sub(&g);
        obs.push(g);
        obs.push(spare(read, READ, &[], 150_000));
        obs.push(spare(read, READ, &[], 16));
        obs.push(plain(read, CLOSE, 0, &[]));
        obs.push(plain(dir, OPEN, 0, b"absent"));
        obs.push(plain(dir, RENAME, 0, b"a/f\0a/g"));
        let t = spare(dir, MKDTEMP, b"t.", 64);
        let temp = sub(&t);
        obs.push(t);
        obs.push(plain(temp, SYNC_DIR, 0, &[]));
        obs.push(plain(dir, REMOVE, 0, b"a/g"));
        obs.push(plain(dir, REMOVE_TREE, 0, b"a"));
        obs.push(plain(dir, SYNC_DIR, 0, &[]));
        obs.push(plain(clock, WALL_MS, 0, &[]));
        let mono = plain(clock, MONO_NS, 0, &[]);
        let deadline = mono.value + 1_000;
        obs.push(mono);
        obs.push(plain(clock, MONO_NS, 0, &[]));
        obs.push(input(clock, SLEEP_UNTIL, &deadline.to_le_bytes()));
        obs.push(input(out, STDOUT, b"out"));
        obs.push(input(err, STDERR, b"err"));
        obs.push(seen.expect("the other task ran"));
        obs
    };
    let taped = |mode: Mode, host: Provider, wd: u64| -> Vec<Ob> {
        let mut obs = Vec::new();
        in_task(mode == Mode::Record, &mut |context| {
            TASK.set(context);
            // SAFETY: the tape's store is the OS provider's store grant.
            let (tape, _) =
                unsafe { Tape::open(REVISION, (store, sd), b"t", mode, &h, &expect()) }.unwrap();
            obs = ops(context, &tape, host, wd);
            tape.end(&ending()).unwrap();
            TASK.set(ptr::null_mut());
        });
        obs
    };

    for syncs in [Syncs::Parked, Syncs::Blocking, Syncs::Failing] {
        let host = Counted::new(Some(real));
        let answers = SyncHost(host.provider(), syncs);
        // The helper holds a parked SYNC until the other task has taken its
        // frame, so that frame falls between the SYNC's ISSUE and DONE.
        let _ = std::fs::remove_file(store_dir.join("t"));
        crate::os_sync::hold(syncs == Syncs::Parked);
        let recorded = taped(Mode::Record, answers.provider(), wd);
        crate::os_sync::hold(false);
        let kinds: std::collections::BTreeSet<u8> = recorded.iter().map(|o| o.op).collect();
        assert_eq!(kinds, (OPEN..=SYNC_DIR).collect(), "every S1 op kind");
        assert_eq!(
            recorded[11].error.tag, 1,
            "the absent open recorded its error"
        );
        assert!(recorded.iter().all(|o| o.status == 0));
        let failed: Vec<usize> = (0..recorded.len())
            .filter(|&i| recorded[i].error != IoError::OK)
            .collect();
        if syncs == Syncs::Failing {
            assert_eq!(failed, [3, 11], "the SYNC recorded its EIO");
            assert_eq!(recorded[3].error, IoError::from_errno(libc::EIO));
        } else {
            assert_eq!(failed, [11]);
        }
        // The recording's blocks are written by the writer thread.
        super::writer::flush();
        let frames = tape_frames(&std::fs::read(store_dir.join("t")).unwrap());
        let syncs_at: Vec<usize> = (0..frames.len())
            .filter(|&i| {
                matches!(
                    frames[i],
                    Frame::Issue {
                        op: SYNC | SYNC_DIR,
                        ..
                    } | Frame::Op {
                        op: SYNC | SYNC_DIR,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(syncs_at.len(), 3);
        if syncs == Syncs::Blocking {
            assert!(
                syncs_at
                    .iter()
                    .all(|&i| matches!(frames[i], Frame::Op { .. }))
            );
        } else {
            let Frame::Issue { task, .. } = frames[syncs_at[0]] else {
                panic!("the parked SYNC records an ISSUE");
            };
            let done = (syncs_at[0]..frames.len())
                .find(|&i| matches!(frames[i], Frame::Done { task: t, .. } if t == task))
                .expect("the SYNC's DONE");
            assert!(
                syncs == Syncs::Failing
                    || frames[syncs_at[0] + 1..done]
                        .iter()
                        .any(|f| matches!(f, Frame::Op { task: t, .. } if *t != task)),
                "another task's frame falls inside the parked SYNC"
            );
        }

        let refuse = Counted::new(None);
        let replayed = taped(Mode::Replay, refuse.provider(), 0);
        assert_eq!(
            refuse.calls.get(),
            0,
            "an operation reached the host under Replay"
        );
        assert_eq!(replayed, recorded);
    }

    // With no task (no scheduler) nothing but the tape can ready a replayed
    // parked SYNC: Replay must still finish, from the tape alone.
    let bare = |mode: Mode, host: Provider, wd: u64| -> Vec<Ob> {
        let mut context = crate::Context::new(1 << 20);
        let mut files = crate::host_file::Files::default();
        context.host_files = &mut files;
        crate::causes::task_start(&mut context).unwrap();
        TASK.set(&mut context);
        // SAFETY: the tape's store is the OS provider's store grant.
        let (tape, _) =
            unsafe { Tape::open(REVISION, (store, sd), b"t", mode, &h, &expect()) }.unwrap();
        let dir = tape.on_tape(crate::entry::DIR, (host, wd)).unwrap();
        let f = plain(dir, OPEN, 3, b"bare");
        let file = (dir.0, handle_like(dir.1, f.value));
        let obs = vec![
            f,
            input(file, WRITE, b"x"),
            plain(file, SYNC, 0, &[]),
            plain(file, CLOSE, 0, &[]),
            plain(dir, SYNC_DIR, 0, &[]),
            plain(dir, REMOVE, 0, b"bare"),
        ];
        tape.end(&ending()).unwrap();
        TASK.set(ptr::null_mut());
        obs
    };
    let _ = std::fs::remove_file(store_dir.join("t"));
    let recorded = bare(Mode::Record, real, wd);
    assert!(
        recorded
            .iter()
            .all(|o| o.status == 0 && o.error == IoError::OK)
    );
    // The recording's blocks are written by the writer thread.
    super::writer::flush();
    let frames = tape_frames(&std::fs::read(store_dir.join("t")).unwrap());
    let issued = frames
        .iter()
        .filter(|f| {
            matches!(
                f,
                Frame::Issue {
                    op: SYNC | SYNC_DIR,
                    ..
                }
            )
        })
        .count();
    assert_eq!(issued, 2, "both syncs parked on the helper");
    let refuse = Counted::new(None);
    assert_eq!(bare(Mode::Replay, refuse.provider(), 0), recorded);
    assert_eq!(refuse.calls.get(), 0);

    drop(os);
}

/// Replay supplies directory records, file data, and metadata after the
/// recorded host namespace disappears. The substitute may issue no host call.
#[test]
fn filesystem_observations_survive_removing_the_recorded_directory() {
    super::run::check_launch_replay();
    crate::openat2_required::require();
    let scratch = tempfile::tempdir().unwrap();
    let root = scratch.path();
    let world = root.join("world");
    let store = root.join("store");
    std::fs::create_dir_all(world.join("child")).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    let child = world.join("child");
    let child_fd = std::fs::File::open(&child).unwrap();
    let candidates = [17_u128, 23_u128];
    let collision = child.join(format!("trial-{:032x}", candidates[0]));
    std::fs::write(&collision, b"existing file").unwrap();
    let mut entropy = candidates.into_iter();
    let (created, suffix) = crate::os::temp::create_temp_file(
        std::os::fd::AsRawFd::as_raw_fd(&child_fd),
        b"trial-",
        || Ok(entropy.next().expect("exactly one retry after collision")),
    )
    .unwrap();
    assert_eq!(suffix, format!("{:032x}", candidates[1]));
    let created = std::fs::File::from(created);
    assert_eq!(created.metadata().unwrap().len(), 0);
    assert_eq!(std::fs::read(&collision).unwrap(), b"existing file");
    drop(created);
    drop(child_fd);
    std::fs::remove_file(&collision).unwrap();
    std::fs::remove_file(child.join(format!("trial-{suffix}"))).unwrap();

    let child_fds = || {
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .flatten()
            .filter(|fd| std::fs::read_link(fd.path()).is_ok_and(|path| path == child))
            .count()
    };
    let file_path = world.join("child/data");
    let data = std::fs::File::create(&file_path).unwrap();
    let before_epoch = std::time::UNIX_EPOCH - std::time::Duration::from_secs(2);
    data.set_times(std::fs::FileTimes::new().set_modified(before_epoch))
        .unwrap();
    drop(data);
    std::fs::create_dir(world.join("child/other")).unwrap();
    let stdout = std::fs::File::create(root.join("stdout")).unwrap();
    let stderr = std::fs::File::create(root.join("stderr")).unwrap();
    let mut os = Box::new(OsProvider::new(stdout.into(), stderr.into()));
    let (wd, sd) = (os.grant(&world).unwrap(), os.grant(&store).unwrap());
    let (store, real) = (os.provider(), os.provider());

    let listed = |dir, path: &[u8]| {
        let opened = plain(dir, ENTRIES, 0, path);
        assert_eq!(opened.error, IoError::OK);
        let listing = (dir.0, handle_like(dir.1, opened.value));
        let first = spare(listing, NEXT_ENTRIES, &[], 258);
        let last = spare(listing, NEXT_ENTRIES, &[], 258);
        assert_eq!(plain(listing, CLOSE_ENTRIES, 0, &[]).error, IoError::OK);
        [first, last]
    };
    let taped = |mode: Mode, host: Provider, wd: u64| {
        let mut observations = Vec::new();
        in_task(mode == Mode::Record, &mut |context| {
            TASK.set(context);
            let mut h = header(REVISION);
            h.kind = KIND_RUN;
            // SAFETY: the provider and tape store grant are pinned outside this task.
            let (tape, _) =
                unsafe { Tape::open(REVISION, (store, sd), b"t", mode, &h, &expect()) }.unwrap();
            let root = tape.on_tape(crate::entry::DIR, (host, wd)).unwrap();
            let before = child_fds();
            let [first, last] = listed(root, b"child");
            assert_eq!(
                child_fds(),
                before,
                "listing retained a directory descriptor"
            );
            let open_dir = plain(root, OPEN_DIR, 0, b"child");
            let dir = (root.0, handle_like(root.1, open_dir.value));
            let [from_narrow, end_narrow] = listed(dir, b"");
            assert_eq!((&first, &last), (&from_narrow, &end_narrow));
            let open_file = plain(dir, OPEN, 4, b"data");
            let file = (dir.0, handle_like(dir.1, open_file.value));
            observations = vec![
                open_dir,
                first,
                last,
                plain(dir, STAT, 0, b"data"),
                open_file,
                call(
                    file,
                    WRITE_AT,
                    4,
                    &4u64.to_le_bytes(),
                    &mut b"Z".to_vec(),
                    true,
                ),
                call(
                    file,
                    READ_AT,
                    4,
                    &2u64.to_le_bytes(),
                    &mut Vec::with_capacity(8),
                    false,
                ),
                plain(file, STAT_FILE, 0, &[]),
                plain(file, CLOSE, 0, &[]),
            ];
            let temp = spare(dir, MKSTEMP, b"temp-", 37);
            let key = (dir.0, handle_like(dir.1, temp.value));
            observations.push(temp);
            observations.push(input(key, WRITE, b"new file"));
            observations.push(plain(key, CLOSE, 0, &[]));
            tape.end(&ending()).unwrap();
            TASK.set(ptr::null_mut());
        });
        observations
    };

    let recorded = taped(Mode::Record, real, wd);
    assert!(
        recorded
            .iter()
            .all(|o| o.status == 0 && o.error == IoError::OK)
    );
    assert_eq!(recorded[1].value, 2);
    assert_eq!(recorded[2].value, 0);
    assert_eq!(
        (
            recorded[3].kind,
            recorded[3].value,
            recorded[3].written as i64
        ),
        (0, 0, -2_000_000_000)
    );
    assert_eq!(
        (recorded[5].written, recorded[6].bytes.as_slice()),
        (1, &[0, 0, b'Z'][..])
    );
    assert_eq!((recorded[7].kind, recorded[7].value), (0, 5));
    let temp = &recorded[9].bytes;
    let suffix = temp.strip_prefix(b"temp-").unwrap();
    assert!((1..=32).contains(&suffix.len()) && suffix.iter().all(u8::is_ascii_alphanumeric));
    assert_eq!(
        std::fs::read(child.join(std::str::from_utf8(temp).unwrap())).unwrap(),
        b"new file"
    );
    super::writer::flush();
    let frames = tape_frames(&std::fs::read(root.join("store/t")).unwrap());
    assert!(frames.iter().any(|f| matches!(
        f,
        Frame::Op { op: ENTRIES, object: 1, args, .. } if *args == Args::Literal(b"child".to_vec())
    )));
    assert!(frames.iter().any(|f| matches!(
        f,
        Frame::Op { op: MKSTEMP, object: 3, args, out, .. }
            if args.operands() == &Args::Literal(b"temp-".to_vec())
                && args.recorded_spare() == Some([32, 0])
                && out.payload == suffix
    )));

    std::fs::remove_dir_all(&world).unwrap();
    let blocked = Counted::new(None);
    let replayed = taped(Mode::Replay, blocked.provider(), 0);
    assert_eq!(
        blocked.calls.get(),
        0,
        "replay touched the removed directory"
    );
    assert_eq!(
        replayed, recorded,
        "the recorded filesystem observations changed"
    );
    drop(os);
}

/// A REPEAT needs two consecutive CHOOSE frames in its block and a positive
/// count. It cannot expand the block past MAX_FRAMES (REPLAY-SPEC §5.4).
#[test]
fn a_repeat_the_format_forbids_is_corrupt() {
    let choose = |gap, index| {
        let mut out = Vec::new();
        put_choose(&mut out, gap, index);
        out
    };
    let repeat = |count| {
        let mut out = Vec::new();
        put_repeat(&mut out, count);
        out
    };
    let (a, b) = (choose(1, 0), choose(1, 1));
    let grant = vec![F_GRANT, 0, crate::entry::CLOCK];
    for (why, payload) in [
        ("no CHOOSE", repeat(1)),
        ("one CHOOSE", [a.clone(), repeat(1)].concat()),
        (
            "another frame between",
            [a.clone(), b.clone(), grant, repeat(1)].concat(),
        ),
        ("count zero", [a.clone(), b.clone(), repeat(0)].concat()),
        (
            "after a REPEAT",
            [a.clone(), b.clone(), repeat(1), repeat(1)].concat(),
        ),
        (
            "only one CHOOSE after a REPEAT",
            [a.clone(), b.clone(), repeat(1), b.clone(), repeat(1)].concat(),
        ),
        (
            "past the bound",
            [a.clone(), b.clone(), repeat(MAX_FRAMES as u64 - 1)].concat(),
        ),
        ("2^63 frames", [a, b, repeat(1 << 63)].concat()),
    ] {
        for physical in [false, true] {
            let mut decoder = Decoder::default();
            decoder.physical = physical;
            assert_eq!(decoder.block(&payload, false), Err(Corrupt), "{why}");
        }
    }
}

/// The hand-built rows put stdin and Proc release between distinct issues.
#[test]
fn exec_close_rows_match_by_part_and_impossible_spawn_issues_no_handles() {
    use crate::{
        Context,
        exec_runtime::{hirsel_core_exec_spawn, hirsel_core_exec_wait},
        net::{
            SocketTape, hirsel_core_host_recv_bounded, hirsel_core_host_send_pair,
            hirsel_core_socket_drop,
        },
    };

    fn row(op: u8, object: u64, args: &[u8], value: u64, payload: &[u8]) -> Frame {
        Frame::Op {
            task: 0,
            op,
            object,
            args: if op == RECV_BOUNDED {
                Args::of(args).buffered([4, 0])
            } else {
                Args::of(args)
            },
            out: Out {
                value,
                payload: payload.to_vec(),
                ..Out::default()
            },
        }
    }

    fn carrier(data: &[u8]) -> Buffer {
        Buffer {
            data: data.as_ptr().cast_mut(),
            len: data.len(),
            capacity: 0,
        }
    }

    let mut snapshot = b"\x01\x00\x00\x00\x04\x00\x00\x00tool".to_vec();
    snapshot.extend_from_slice(&[0x42; 32]); // public pin
    snapshot.extend_from_slice(b"\x04\x00\x00\x00tool\x01\x00\x00\x00");
    snapshot.extend_from_slice(b"\x00\x00\x00\x00\x00\x00\x00\x00");
    snapshot.extend_from_slice(b"\x01\x00\x00\x00\x04\x00\x00\x00work");
    let launch = b"\x04\x00\x00\x00tool\x04\x00\x00\x00work\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x01\x00";
    let normal = vec![
        Frame::Grant {
            gap: 0,
            kind: crate::entry::EXEC,
            args: Some(snapshot),
        },
        row(EXEC_SPAWN, 1, launch, 2, &[3, 4, 0]),
        row(SEND_PAIR, 3, &[0, 1, b'x', 0], 1, &[]),
        row(CLOSE, 3, &[2], 0, &[]),
        row(RECV_BOUNDED, 4, &[0], 1, b"x"),
        row(EXEC_WAIT, 2, &[0], 23, &[]),
        row(CLOSE, 2, &[0], 0, &[]),
        row(CLOSE, 4, &[1], 0, &[]),
        Frame::End {
            gap: 2, // last ordered operation and its synchronous delivery
            ending: ending(),
        },
    ];

    for case in 0..6 {
        let mut frames = normal.clone();
        let want = match case {
            0 => Ok(()),
            1 => {
                frames.swap(3, 4);
                Ok(())
            }
            2 => {
                frames.swap(6, 7);
                Ok(())
            }
            3 => {
                frames[1] = row(EXEC_SPAWN, 1, launch, 2, &[3, 3, 0]);
                Err(Fault::Corrupt)
            }
            4 => {
                frames[1] = row(EXEC_SPAWN, 1, launch, 2, &[3, 4, 5]);
                Err(Fault::Corrupt)
            }
            5 => {
                let Frame::Op { out, .. } = &mut frames[1] else {
                    unreachable!()
                };
                *out = Out {
                    status: TIMED_OUT,
                    ..Out::default()
                };
                Err(Fault::Corrupt)
            }
            _ => unreachable!(),
        };
        let h = header(REVISION);
        let mut encoder = Encoder::default();
        let data: Vec<_> = frames.iter().flat_map(|f| encoder.frame(f).0).collect();
        let tape_bytes = [
            block(&h.tape_id, 0, 0, &h.encode(REVISION).unwrap()),
            block(&h.tape_id, END, 1, &data),
        ]
        .concat();
        let mut store = Store::with(&tape_bytes);
        let no_host = Counted::new(None);
        // SAFETY: the store and all fixture carriers remain owned through replay.
        unsafe {
            let (tape, _) =
                Tape::open(REVISION, store.dir(), b"t", Mode::Replay, &h, &expect()).unwrap();
            let (provider, key) = tape.on_tape_exec((no_host.provider(), 0)).unwrap();
            let mut files = crate::host_file::Files::default();
            let mut context = Context::new(1 << 20);
            files.net_tape = Some(SocketTape::new(provider, key));
            let grant = files.registry.grant(provider, key);
            context.host_files = &mut files;
            let spawned = hirsel_core_exec_spawn(
                &mut context,
                grant,
                &carrier(b"tool"),
                &carrier(b""),
                &carrier(b""),
                &carrier(b"work"),
                &carrier(b""),
                1,
                1,
                0,
                801,
            );
            if case >= 3 {
                assert_eq!(spawned.outcome.status, STATUS_REFUSED);
                assert_eq!(spawned.pipes, [0; 3]);
                assert_eq!(
                    tape.cell
                        .state
                        .lock()
                        .unwrap()
                        .live
                        .as_ref()
                        .unwrap()
                        .objects
                        .len(),
                    1
                );
            } else {
                let fault = tape.cell.state.lock().unwrap().live.as_ref().unwrap().fault;
                assert_eq!(
                    (spawned.outcome.status, spawned.outcome.error),
                    (0, IoError::OK),
                    "spawn: {fault:?}"
                );
                let [input, output, no_stderr] = spawned.pipes;
                assert_eq!(no_stderr, 0);
                let sent = hirsel_core_host_send_pair(
                    &mut context,
                    input,
                    &carrier(b"x"),
                    &carrier(b""),
                    0,
                    0,
                    0,
                    802,
                );
                let fault = tape.cell.state.lock().unwrap().live.as_ref().unwrap().fault;
                assert_eq!((sent.status, sent.value), (0, 1), "send: {fault:?}");
                hirsel_core_socket_drop(&mut context, input);
                let mut bytes = [0xcc; 4];
                let mut b = Buffer {
                    data: bytes.as_mut_ptr(),
                    len: 0,
                    capacity: bytes.len(),
                };
                let read =
                    hirsel_core_host_recv_bounded(&mut context, output, &mut b, 4, 0, 0, 0, 803);
                let proc = spawned.outcome.value;
                assert_eq!(read.value, 1);
                assert_eq!((b.len, bytes), (1, [b'x', 0xcc, 0xcc, 0xcc]));
                assert_eq!(
                    hirsel_core_exec_wait(&mut context, proc, 0, 0, 0, 804).value,
                    23
                );
                hirsel_core_socket_drop(&mut context, proc);
                hirsel_core_socket_drop(&mut context, output);
            }
            assert_eq!(tape.end(&ending()), want, "variant {case}");
        }
        assert_eq!(no_host.calls.get(), 0);
    }
}

#[test]
fn unix_tape_replays_hand_encoded_stream_and_connected_datagram_rows() {
    // The Net grant is ordinal 1. Each creation consumes the next ordinal.
    // Root and path have varint lengths, backlog is little-endian u16, and
    // Never contributes one zero byte before the send's length and payload.
    let rows: &[(u8, u64, &[u8], u64)] = &[
        (UNIX_LISTEN, 1, b"\x01r\x04sock\x34\x12", 2),
        (UNIX_CONNECT, 1, b"\x01r\x04sock\x00", 3),
        (UNIX_ACCEPT, 2, b"\x00", 4),
        (DATAGRAM_OPEN, 1, b"", 5),
        (DATAGRAM_CONNECT, 5, b"\x01r\x04sock", 0),
        (DATAGRAM_SEND, 5, b"\x00\x03abc", 0),
    ];
    let h = header(REVISION);
    let mut encoder = Encoder::default();
    let mut data = encoder
        .frame(&Frame::Grant {
            gap: 0,
            kind: crate::entry::NET,
            args: None,
        })
        .0;
    for &(op, object, args, value) in rows {
        data.extend(
            encoder
                .frame(&Frame::Op {
                    task: 0,
                    op,
                    object,
                    args: Args::Literal(args.to_vec()),
                    out: Out {
                        value,
                        ..Out::default()
                    },
                })
                .0,
        );
    }
    data.extend(
        encoder
            .frame(&Frame::End {
                gap: 2,
                ending: ending(),
            })
            .0,
    );
    let tape_bytes = [
        block(&h.tape_id, 0, 0, &h.encode(REVISION).unwrap()),
        block(&h.tape_id, END, 1, &data),
    ]
    .concat();
    let mut store = Store::with(&tape_bytes);
    let no_host = Counted::new(None);
    let mut context = crate::Context::new(1000);
    let mut local = hirsel_core_net::Sockets::default();
    let mut unix = crate::unix_net::UnixRequest {
        os: &crate::os_unix::OPERATIONS,
        root: "r",
        path: "sock",
        backlog: 0x1234,
        authorized: true,
        until: None,
        sockets: &raw mut local,
        connecting: None,
        target: None,
        retry: None,
    };
    // SAFETY: the store, provider state and request carriers outlive each call.
    unsafe {
        let (tape, _) =
            Tape::open(REVISION, store.dir(), b"t", Mode::Replay, &h, &expect()).unwrap();
        let (provider, net) = tape
            .on_tape(crate::entry::NET, (no_host.provider(), 0))
            .unwrap();
        for &(op, object, _, value) in rows {
            let mut bytes = *b"abc";
            let buffer = if op == DATAGRAM_SEND {
                Buffer {
                    data: bytes.as_mut_ptr(),
                    len: bytes.len(),
                    capacity: 0,
                }
            } else {
                empty_buffer()
            };
            let mut r = request(op, 0, handle_like(net, object), &[], buffer);
            r.path = (&raw mut unix).cast();
            let out = hirsel_core_host(&mut context, &provider, &mut r);
            assert_eq!(
                (out.status, out.error, out.value),
                (
                    0,
                    IoError::OK,
                    if value == 0 {
                        0
                    } else {
                        handle_like(net, value)
                    }
                ),
                "op {op}"
            );
            if value != 0 {
                assert!(crate::tape::typed_socket(&provider, out.value));
            }
        }
        assert_eq!(tape.end(&ending()), Ok(()));
    }
    assert_eq!(no_host.calls.get(), 0);
}

#[test]
fn unix_tape_rejects_impossible_packet_before_appending() {
    let make = |flag, written, payload: &[u8], outcome| {
        let mut bytes = vec![flag];
        bytes.extend_from_slice(payload);
        if outcome != OK {
            bytes.clear();
        }
        Out {
            status: outcome,
            value: 3,
            written,
            payload: bytes,
            ..Out::default()
        }
    };
    let stale_listen_error = Frame::Op {
        task: 0,
        op: DATAGRAM_BIND,
        object: 1,
        args: Args::of(&[]),
        out: Out {
            status: ERROR,
            error: (10, libc::EIO as u64),
            ..Out::default()
        },
    };
    let bytes = Encoder::default().frame(&stale_listen_error).0;
    assert!(Decoder::default().block(&bytes, false).is_err());

    let h = header(REVISION);
    for (is_valid, packet) in [
        (true, make(1, 2, b"ab", OK)),
        (false, make(0, 2, b"ab", OK)),
        (false, make(1, 1, b"a", OK)),
        (false, make(1, 3, b"abc", OK)),
        (false, make(0, 0, b"", ABORTED)),
    ] {
        let frames = [
            Frame::Grant {
                gap: 0,
                kind: crate::entry::NET,
                args: None,
            },
            Frame::Op {
                task: 0,
                op: DATAGRAM_BIND,
                object: 1,
                args: Args::of(b"\x04work\x06packet"),
                out: Out {
                    value: 2,
                    ..Out::default()
                },
            },
            Frame::Op {
                task: 0,
                op: DATAGRAM_RECV,
                object: 2,
                args: Args::of(&[0]).buffered([2, 0]),
                out: packet,
            },
            Frame::Op {
                task: 0,
                op: CLOSE,
                object: 2,
                args: Args::of(&[0]),
                out: Out::default(),
            },
            Frame::End {
                gap: 2, // last ordered operation and its synchronous delivery
                ending: ending(),
            },
        ];
        let mut encoder = Encoder::default();
        let data: Vec<_> = frames.iter().flat_map(|f| encoder.frame(f).0).collect();
        let tape_bytes = [
            block(&h.tape_id, 0, 0, &h.encode(REVISION).unwrap()),
            block(&h.tape_id, END, 1, &data),
        ]
        .concat();
        let mut store = Store::with(&tape_bytes);
        let no_host = Counted::new(None);
        // SAFETY: the store and request carriers remain owned through replay.
        unsafe {
            let (tape, _) =
                Tape::open(REVISION, store.dir(), b"t", Mode::Replay, &h, &expect()).unwrap();
            let (provider, net) = tape
                .on_tape(crate::entry::NET, (no_host.provider(), 0))
                .unwrap();
            let mut context = crate::Context::new(1000);
            let mut local = hirsel_core_net::Sockets::default();
            let mut unix = crate::unix_net::UnixRequest {
                os: &crate::os_unix::OPERATIONS,
                root: "work",
                path: "packet",
                backlog: 0,
                authorized: true,
                until: None,
                sockets: &raw mut local,
                connecting: None,
                target: None,
                retry: None,
            };
            let mut bind = request(DATAGRAM_BIND, 0, net, &[], empty_buffer());
            bind.path = (&raw mut unix).cast();
            let result = hirsel_core_host(&mut context, &provider, &mut bind);
            assert_eq!(ordinal_of(result.value), 2);
            let mut storage = [b'!', 0xcc, 0xcc, 0xcc];
            let buffer = Buffer {
                data: storage.as_mut_ptr(),
                len: 1,
                capacity: storage.len(),
            };
            let mut recv = request(DATAGRAM_RECV, 0, result.value, &[], buffer);
            recv.path = (&raw mut unix).cast();
            let got = hirsel_core_host(&mut context, &provider, &mut recv);
            if is_valid {
                assert_eq!((got.value, got.written, got.kind), (3, 2, 1));
                assert_eq!((recv.buffer.len, storage), (3, [b'!', b'a', b'b', 0xcc]));
                let mut close = request(
                    CLOSE,
                    crate::net::SOCKET_CLOSE,
                    result.value,
                    &[],
                    empty_buffer(),
                );
                assert_eq!(
                    hirsel_core_host(&mut context, &provider, &mut close).value,
                    1
                );
                assert_eq!(tape.end(&ending()), Ok(()));
            } else {
                assert_eq!(got.status, STATUS_REFUSED);
                assert_eq!((recv.buffer.len, storage), (1, [b'!', 0xcc, 0xcc, 0xcc]));
                assert_eq!(tape.end(&ending()), Err(Fault::Corrupt));
            }
        }
        assert_eq!(no_host.calls.get(), 0);
    }
}

#[path = "test_frames.rs"]
mod test_frames;
use test_frames::{in_task, tape_frames};

#[path = "registry_tests.rs"]
mod registry_tests;

#[path = "p256_tests.rs"]
mod p256_tests;
