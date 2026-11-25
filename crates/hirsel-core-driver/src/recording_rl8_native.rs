// RL8, take positions (docs/recording.md, "Laws" under "Unit recordings"),
// recording half, as native programs on both backends. The in-crate cases
// (hirsel-core-rt tape/rl8.rs) drive the ledger's hold hook from Rust; here a
// C object linked into the program drives it through the scripted runtime's
// `hirsel_core_test_hold` (feature `testing`, absent from production
// archives), between the program's own runtime calls, so the hook adds no
// frame, event or take-point position.
//
// The hand counts are those of tape/rl8.rs, from recording.md: a `TAKE`'s gap
// is the events since the previous frame; its count the in-operation
// take-point positions since the last event or frame (each `try_receive`
// pump, the event commit's drain, a release that takes); a `CONN_TRY_SEND`
// is an `OP` of two events (its commit and its delivery); `try_receive` is no
// frame and no event; `task.check_cancel()` is one event; a `CLOSE` is a
// frame and no event. `conn.new` makes no frame and numbers the connection,
// then its client, after the stream's grants.

/// One hold step of the C driver: before (`after` false) or after the
/// program's `call`th wrapped runtime call (`conn.try_send`,
/// `conn.try_receive`, `conn.send`, `conn.receive` and `task.check_cancel`,
/// counted together from 0). Steps: 0 hook, 1 unhook, 2 reach the oldest.
/// A script that hooks unhooks before the program's cleanup, as tape/rl8.rs
/// does: an inbox's cleaning waits for its own seal answer, which a hooked
/// unit holds, so a run left hooked never ends.
#[derive(Clone, Copy)]
struct HoldStep {
    call: u32,
    after: bool,
    step: u8,
}

const HOLD_HOOK: u8 = 0;
const HOLD_UNHOOK: u8 = 1;
const HOLD_REACH: u8 = 2;
/// Arrival tags of a `TAKE` body (recording.md, "TAKE").
const RL8_ITEM: u8 = 0;
const RL8_SEAL_ANSWER: u8 = 2;
const RL8_LAST_RELEASE: u8 = 6;
const RL8_SHARED_RELEASE: u8 = 7;

fn hold_before(call: u32, step: u8) -> HoldStep {
    HoldStep {
        call,
        after: false,
        step,
    }
}

fn hold_after(call: u32, step: u8) -> HoldStep {
    HoldStep {
        call,
        after: true,
        step,
    }
}

/// The C driver: wraps each counted runtime call and runs its hold steps.
fn rl8_driver(script: &[HoldStep]) -> String {
    let steps: String = script
        .iter()
        .map(|s| format!("{{{},{},{}}},", s.call, u8::from(s.after), s.step))
        .collect();
    format!(
        r#"
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
typedef struct {{ uint8_t outcome, status; }} SendOutcome;
typedef struct {{ uint64_t code_and_requested, available; }} ReceiveOutcome;
extern bool hirsel_core_test_hold(uint8_t step);
static const struct {{ uint32_t call; uint8_t after, step; }} script[] = {{ {steps} {{UINT32_MAX,0,0}} }};
static uint32_t calls;
static void hold(uint8_t after) {{
    for (size_t i = 0; script[i].call != UINT32_MAX; i++)
        if (script[i].call == calls && script[i].after == after && !hirsel_core_test_hold(script[i].step))
            abort();
    if (after) calls++;
}}
extern SendOutcome __real_hirsel_core_conn_send(void *, void *, void *, uint64_t, bool);
SendOutcome __wrap_hirsel_core_conn_send(void *c, void *h, void *v, uint64_t s, bool w) {{
    hold(0); SendOutcome o = __real_hirsel_core_conn_send(c, h, v, s, w); hold(1); return o;
}}
extern ReceiveOutcome __real_hirsel_core_conn_receive(void *, void *, void *, uint64_t, bool);
ReceiveOutcome __wrap_hirsel_core_conn_receive(void *c, void *h, void *d, uint64_t s, bool w) {{
    hold(0); ReceiveOutcome o = __real_hirsel_core_conn_receive(c, h, d, s, w); hold(1); return o;
}}
extern uint8_t __real_hirsel_core_check_cancel(void *, uint64_t);
uint8_t __wrap_hirsel_core_check_cancel(void *c, uint64_t s) {{
    hold(0); uint8_t o = __real_hirsel_core_check_cancel(c, s); hold(1); return o;
}}
"#
    )
}

/// A `try_receive`'s result as a number: the item, 0 for `Empty`; the tasks
/// of the parked-receiver case.
const RL8_GOT: &str = r#"
fn wait_one(take i: Inbox[u64]) -> u64 waits {
    result.unwrap(conn.receive(i))
}

fn at_once(take n: u64) -> u64 { n }

fn got(r: Result[u64, InboxPoll]) -> u64 {
    match r {
        Ok(v) => v,
        Error(Empty) => 0,
        _ => 99,
    }
}
"#;

/// One RL8 case: its program's `main` body (each `try_receive` result is
/// checked in the program against the hand table), its hold script and the
/// hand-counted takes before the program's `stdout.write`: (gap, point,
/// count, arrival tag, the object's ordinal after the grants: an item's
/// inbox, a last release's connection, a shared release's node).
struct Rl8Case {
    name: &'static str,
    body: &'static str,
    script: Vec<HoldStep>,
    takes: Vec<(u64, hirsel_tape_format::TakePoint, u64, u8, u64)>,
    /// RL9 over the recording: the items sent in canonical form, given the
    /// grant count, and the connection they were taken from (ordinal after
    /// the grants).
    sent: (fn(u64) -> Vec<Vec<u8>>, u64),
    /// The first item take's whole body, receiver ordinals included, given
    /// the grant count.
    item: Option<fn(u64) -> Vec<u8>>,
}

/// A u64 item as a value: 8 plain bytes, no parts.
fn rl8_word(v: u8) -> Vec<u8> {
    [vec![8, v], vec![0; 7], vec![0]].concat()
}

fn rl8_words(vs: &[u8]) -> Vec<Vec<u8>> {
    vs.iter().map(|&v| rl8_word(v)).collect()
}

/// The `Shared[u64]` item holding 42, as a value: 8 plain bytes (the holder
/// word zeroed), one part: a handle (1), a `Shared` snapshot (1), the
/// receiver's ordinal when `ordinal` is given, home stream 0 and the node's
/// ordinal there, then the payload: 8 plain bytes and no parts.
fn rl8_shared_item(ordinal: Option<u8>, node: u8) -> Vec<u8> {
    let mut v = vec![8, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1];
    v.extend(ordinal);
    v.extend([0, 0, 0, 0, node]);
    v.extend(rl8_word(42));
    v
}

fn rl8_cases() -> Vec<Rl8Case> {
    use hirsel_tape_format::TakePoint::{Operation, Pump};
    vec![
        // tape/rl8.rs `rl8_two_receives_and_no_event_between`: calls 0 and
        // 1 send 10 and 11; call 2 finds `Empty`; 10 reaches the unit and
        // call 3 takes it (gap 2: the second send; count 2: both
        // `try_receive`s); calls 4 and 5 are events, 11 reaching the unit
        // between them; call 6 takes it (gap 2: the two events; count 1).
        Rl8Case {
            name: "two_receives",
            body: r#"
    let (c, i): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(4, bytes 0))
    result.unwrap(conn.try_send(c, 10))
    result.unwrap(conn.try_send(c, 11))
    let a = got(conn.try_receive(i))
    let b = got(conn.try_receive(i))
    task.check_cancel()
    task.check_cancel()
    let d = got(conn.try_receive(i))
    if a != 0 || b != 10 || d != 11 { return Error(Exit(status: 3)) }
"#,
            script: vec![
                hold_before(0, HOLD_HOOK),
                hold_before(3, HOLD_REACH),
                hold_after(4, HOLD_REACH),
                hold_after(6, HOLD_UNHOOK),
            ],
            takes: vec![
                (2, Operation, 2, RL8_ITEM, 1),
                (2, Operation, 1, RL8_ITEM, 1),
            ],
            sent: (|_| rl8_words(&[10, 11]), 1),
            item: None,
        },
        // `rl8_release_takes_an_older_item_before_its_notice`: connection 1's
        // inbox is dropped (a `CLOSE`, no event), and its cleaning reaches
        // its first position and takes its own seal answer there (gap 0,
        // count 1); 20 is sent on connection 2 (call 0)
        // and reaches the unit; dropping connection 1's client is the last
        // release (its `CLOSE`, then one position), which takes the older
        // item, then the notice. Ordinals: connection 1 is 1, its client 2,
        // connection 2 is 3.
        Rl8Case {
            name: "release",
            body: r#"
    let (c1, i1): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(4, bytes 0))
    let (c2, i2): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(4, bytes 0))
    {
        let gone = take i1
    }
    result.unwrap(conn.try_send(c2, 20))
    {
        let gone = take c1
    }
    let a = got(conn.try_receive(i2))
    if a != 20 { return Error(Exit(status: 3)) }
"#,
            script: vec![
                hold_before(0, HOLD_HOOK),
                hold_after(0, HOLD_REACH),
                hold_before(1, HOLD_UNHOOK),
            ],
            takes: vec![
                (0, Operation, 1, RL8_SEAL_ANSWER, 1),
                (2, Operation, 1, RL8_ITEM, 3),
                (0, Operation, 1, RL8_LAST_RELEASE, 1),
            ],
            sent: (|_| rl8_words(&[20]), 3),
            item: None,
        },
        // `rl8_take_single_item`, hook off: the item is all the port holds
        // while its receiver runs; gap 2 (the send), count 1.
        Rl8Case {
            name: "single_item",
            body: r#"
    let (c, i): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(4, bytes 0))
    result.unwrap(conn.try_send(c, 5))
    let a = got(conn.try_receive(i))
    if a != 5 { return Error(Exit(status: 3)) }
"#,
            script: vec![],
            takes: vec![(2, Operation, 1, RL8_ITEM, 1)],
            sent: (|_| rl8_words(&[5]), 1),
            item: None,
        },
        // `rl8_one_receiver_at_kinds_0_and_1`, kind 0: the item reaches the
        // unit before its `try_receive`, which takes it.
        Rl8Case {
            name: "reached_before_receive",
            body: r#"
    let (c, i): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(4, bytes 0))
    result.unwrap(conn.try_send(c, 9))
    let a = got(conn.try_receive(i))
    if a != 9 { return Error(Exit(status: 3)) }
"#,
            script: vec![
                hold_before(0, HOLD_HOOK),
                hold_after(0, HOLD_REACH),
                hold_after(1, HOLD_UNHOOK),
            ],
            takes: vec![(2, Operation, 1, RL8_ITEM, 1)],
            sent: (|_| rl8_words(&[9]), 1),
            item: None,
        },
        // Item 1, a kind-1 take with a real parked task: the root spawns a
        // waiter, whose `conn.receive` (call 0) parks with nothing to take,
        // and a task that ends at once, and joins that one so the waiter
        // runs first. The root's `try_send` of 7 (also call 0, since the
        // receive has not returned) runs hooked, so its own pump holds the
        // item; after it the unit unhooks and the item has reached the unit
        // untaken. The root's `join` of the waiter is no take point, so every
        // task waits and the scheduler's idle pump (`pump_ready`) takes it and
        // wakes the waiter: gap 2 (the send's two events, the last before the
        // take), kind 1, count 0 (no in-operation position after the send).
        // The waiter then drops its inbox: a `CLOSE` (no event), and the
        // cleaning takes its own seal answer at its first position (gap 0,
        // count 1).
        Rl8Case {
            name: "parked_receiver_at_pump",
            body: r#"
    let (c, i): (Client[u64], Inbox[u64]) = result.unwrap(conn.new(4, bytes 0))
    let w = task.spawn(tasks, bytes 65536, take i, wait_one)
    let y = task.spawn(tasks, bytes 65536, 0, at_once)
    let z = result.unwrap(task.join(take y))
    result.unwrap(conn.try_send(c, 7))
    let a = result.unwrap(task.join(take w))
    if z != 0 || a != 7 { return Error(Exit(status: 3)) }
"#,
            script: vec![hold_before(0, HOLD_HOOK), hold_after(0, HOLD_UNHOOK)],
            takes: vec![
                (2, Pump, 0, RL8_ITEM, 1),
                (0, Operation, 1, RL8_SEAL_ANSWER, 1),
            ],
            sent: (|_| rl8_words(&[7]), 1),
            item: None,
        },
        // A `Shared` node crossing its own unit's connection and released at
        // home. `shared.new` numbers the node 1; `conn.new` the connection 2
        // and its client 3. The send (two events) carries the node's
        // snapshot; `try_receive`'s pump is position 1 after the send and
        // takes it (gap 2, count 1), the arriving handle numbered 4. With no
        // event since, each drop of a holder is the next position: `t`'s is
        // 2, and `s`'s is 3, the last holder, dropped at home, so its shared
        // release is taken there (gap 0, kind 0, count 3), naming home stream
        // 0 and node 1.
        Rl8Case {
            name: "shared_at_home",
            body: r#"
    let s: Shared[u64] = shared.new(42)
    let (c, i): (Client[Shared[u64]], Inbox[Shared[u64]]) = result.unwrap(conn.new(4, bytes 0))
    result.unwrap(conn.try_send(c, shared.share(s)))
    let t = result.unwrap(conn.try_receive(i))
    {
        let gone = take t
    }
    {
        let gone = take s
    }
"#,
            script: vec![],
            takes: vec![
                (2, Operation, 1, RL8_ITEM, 2),
                (0, Operation, 3, RL8_SHARED_RELEASE, 1),
            ],
            sent: (|g| vec![rl8_shared_item(None, g as u8 + 1)], 2),
            item: Some(|g| {
                let g = g as u8;
                let mut body = vec![RL8_ITEM, g + 2];
                body.extend(rl8_shared_item(Some(g + 4), g + 1));
                body
            }),
        },
    ]
}

/// The root stream's frames of a recording with no helpers.
fn rl8_root_frames(path: &Path) -> Vec<hirsel_tape_format::Frame> {
    use hirsel_tape_format::Index;
    use std::os::unix::fs::FileExt;
    let file = std::fs::File::open(path).unwrap();
    let read = |at: u64, b: &mut [u8]| file.read_at(b, at);
    let (mut index, _) = Index::open(read).unwrap();
    let paths: Vec<_> = index.paths().collect();
    assert_eq!(paths.len(), 1, "one stream: {paths:?}");
    let mut blocks = index.take(paths[0].0).unwrap();
    let mut decoder = blocks.decoder(read).unwrap();
    let mut frames = Vec::new();
    while let Some((flags, payload)) = blocks.next(read).unwrap() {
        frames.extend(
            decoder
                .block(payload, flags & hirsel_tape_format::END != 0)
                .unwrap(),
        );
    }
    frames
}

/// RL8, recording half, natively: each case's `TAKE`s before its
/// `stdout.write` hold the hand-counted gap, point kind, count, arrival and
/// ordinal on Cranelift and LLVM; RL9 over each recording pairs the accepted
/// `CONN_TRY_SEND` items with the item takes.
#[test]
#[ignore = "explicit native integration target"]
fn rl8_take_positions_natively() {
    // The recorder opens its tape beneath the tape directory (openat2).
    crate::openat2_required::require();
    use hirsel_tape_format::{Args, Arrival, Frame, op};

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let runtime = root.join(
        std::env::var_os("HIRSEL_CORE_RT_SCRIPTED_RECORD_ARCHIVE")
            .expect("scripted recording runtime archive"),
    );
    for case in rl8_cases() {
        let dir = tempfile::tempdir().unwrap();
        let source = format!(
            "{RL8_GOT}\npub fn main(tasks: Tasks, out: Stdout) -> Result[(), Exit] waits {{{}    result.unwrap(stdout.write(out, \"rl8\"))\n    Ok(())\n}}\n",
            case.body
        );
        let mut request = source_request(&source, dir.path());
        request.inputs.packages[1] =
            PackageManifest::directory(&root.join("core/core"), package("core"), true).unwrap();
        let product = Driver::default()
            .check(request, &mut Meter::new(false))
            .unwrap();
        assert!(
            product.accepted(),
            "{}: {:?}",
            case.name,
            product.diagnostics
        );
        let entry = DefPath::from_full("app@1/main/main").unwrap();
        let c = dir.path().join("hold.c");
        let object = dir.path().join("hold.o");
        std::fs::write(&c, rl8_driver(&case.script)).unwrap();
        for backend in [Backend::Cranelift, Backend::Llvm] {
            let at = format!("{}/{backend:?}", case.name);
            let mut options = native_options(backend, dir.path());
            command(
                Command::new(options.link.driver())
                    .args(["-c", "-fPIC"])
                    .arg(&c)
                    .arg("-o")
                    .arg(&object),
            )
            .unwrap();
            let wraps = ["conn_send", "conn_receive", "check_cancel"]
                .map(|s| format!(" -Wl,--wrap=hirsel_core_{s}"))
                .concat();
            options.link = Link::Driver(appending_driver(
                dir.path(),
                &format!("cc-rl8-{backend:?}"),
                &format!("'{}'{wraps}", object.display()),
            ));
            options.runtime = runtime.clone();
            options.recording = true;
            options.grant_mask = ["Tasks", "Stdout"]
                .iter()
                .fold(0, |m, g| m | 1 << grant_kind(g).unwrap());
            options.store = dir.path().join(format!("store-{backend:?}"));
            options.output = dir.path().join(format!("rl8-{backend:?}"));
            let program = product.build(&entry, &options).unwrap().executable;
            let path = dir.path().join(format!("rl8-{backend:?}.tape"));
            // A run that waits on a held arrival never ends: bound it.
            let run = Command::new("timeout")
                .args(["60", program.to_str().unwrap()])
                .env("HIRSEL_RECORD", &path)
                .output()
                .unwrap();
            let (code, out, err) = (
                run.status.code(),
                String::from_utf8(run.stdout).unwrap(),
                String::from_utf8(run.stderr).unwrap(),
            );
            assert_eq!((code, out.as_str()), (Some(0), "rl8"), "{at}: {err}");

            let frames = rl8_root_frames(&path);
            let grants = frames
                .iter()
                .filter(|f| matches!(f, Frame::Grant { .. }))
                .count() as u64;
            // The program's `stdout.write`: its first operation that is no
            // unit operation.
            let write = frames
                .iter()
                .position(|f| match f {
                    Frame::Op { op: o, .. } | Frame::Issue { op: o, .. } => {
                        ![op::CONN_TRY_SEND, op::CLOSE].contains(o)
                    }
                    _ => false,
                })
                .expect("the stdout write");
            let takes: Vec<_> = frames[..write]
                .iter()
                .filter_map(|f| match f {
                    Frame::Take {
                        gap,
                        point,
                        count,
                        body,
                    } => {
                        // The connection's ordinal: an item's inbox, a seal
                        // answer's, or a last release's connection in its
                        // making stream (0).
                        let conn = match body.arrival() {
                            Arrival::Item(inbox, _) | Arrival::SealAnswer(inbox) => inbox,
                            Arrival::LastRelease(0, conn, ..) => conn,
                            Arrival::SharedRelease(0, node) => node,
                            a => panic!("{at}: {a:?}"),
                        };
                        Some((*gap, *point, *count, body.bytes()[0], conn))
                    }
                    _ => None,
                })
                .collect();
            let want: Vec<_> = case
                .takes
                .iter()
                .map(|&(g, p, n, tag, conn)| (g, p, n, tag, grants + conn))
                .collect();
            assert_eq!(takes, want, "{at}: takes");

            // RL9 over the recording.
            let want = (case.sent.0)(grants);
            let sends: Vec<_> = frames
                .iter()
                .filter_map(|f| match f {
                    Frame::Op {
                        op: o, args, out, ..
                    } if *o == op::CONN_TRY_SEND && out.value == 0 => Some(args.clone()),
                    _ => None,
                })
                .collect();
            let sent: Vec<_> = want.iter().map(|w| Args::of_parts(&[w])).collect();
            assert_eq!(sends, sent, "{at}: sent items");
            let conn = grants + case.sent.1;
            let taken: Vec<_> = frames
                .iter()
                .filter_map(|f| match f {
                    Frame::Take { body, .. } => match body.arrival() {
                        Arrival::Item(inbox, value) if inbox == conn => Some(value.canonical()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            assert_eq!(taken, want, "{at}: taken items");
            if let Some(item) = case.item {
                let first = frames.iter().find_map(|f| match f {
                    Frame::Take { body, .. } if body.bytes()[0] == RL8_ITEM => Some(body.bytes()),
                    _ => None,
                });
                assert_eq!(first, Some(&item(grants)[..]), "{at}: item take");
            }
        }
    }
}
