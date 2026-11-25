//! RL8, take positions (docs/recording.md, "Laws" under "Unit recordings"),
//! recording half: the ledger's hold hook scripts when each arrival reaches
//! the root unit between its events, and the recorded `TAKE`s hold the
//! hand-counted gap, point kind and count. RL9 over the same recordings: each
//! sent item equals the item taken, in connection order.
//!
//! The hand counts, from recording.md ("Takes" and "Unit operations"):
//! - gap: "the gap of events since the previous frame, as a `WAKE` does";
//!   a `TAKE` is a frame, so a second take with no event between has gap 0.
//! - count: "the number of in-operation take-point positions the unit's code
//!   reached since its last event or frame"; each `conn.try_receive` pump is
//!   one ("inside an operation, each pump of ... `conn.try_receive`"), and so
//!   is a release that takes (an inbox's cleaning). Takes recorded for one
//!   position share its count ("Replay applies every `TAKE` recorded for a
//!   position when it reaches that position ... with that count"), so the
//!   "frame" that starts a new count is any frame but a `TAKE`.
//! - point kind: 1 at a scheduler pump, which "adds nothing to the count; a
//!   take there carries the count the unit has reached".
//! - `CONN_TRY_SEND` is format 13's `OP` ("Their frames are format 13's
//!   `ISSUE` and `OP`"), whose issue commit and delivery are two events, as
//!   for every host `OP`; a `CLOSE` is task 0 and no event; `try_receive` is
//!   "not [a] frame" and records no event.
//! - ordinals: `conn.new` gives the connection (and its inbox) one, then its
//!   client the next (2553's logged reading).
use super::*;
use crate::units_lang::{
    Cross, hirsel_core_client_drop, hirsel_core_conn_new, hirsel_core_conn_receive,
    hirsel_core_conn_send, hirsel_core_inbox_drop, hold,
};
use format::TakePoint::{self, Operation};

const PLAIN: Cross = Cross {
    bytes: 8,
    owners: ptr::null(),
    count: 0,
    at: 0,
    len: 8,
};

/// Arrival tags of a `TAKE` body (recording.md, "TAKE").
const ITEM: u8 = 0;
const LAST_RELEASE: u8 = 6;

/// One script step of the receiving root.
#[derive(Clone, Copy, Debug)]
enum Act {
    /// Hold every arrival published from now on (the hold hook), or let all
    /// held arrivals reach the unit and stop holding.
    Hook(bool),
    /// `try_send(v)` on connection `c`: a `CONN_TRY_SEND` `OP`.
    TrySend(usize, u64),
    /// The oldest held arrival reaches the unit.
    Reach,
    /// The scheduler's pump of the unit between events.
    Pump,
    /// `try_receive` on connection `c`; the value or `None` for `Empty`.
    TryReceive(usize),
    /// One language event.
    Event,
    /// Drop connection `c`'s inbox or client.
    DropInbox(usize),
    DropClient(usize),
}

/// Run `script` as the root of a recording; returns each `try_receive`'s
/// result and the tape's frames. Handles the script leaves are dropped after
/// it, unhooked.
fn record(conns: usize, script: &[Act]) -> (Vec<Option<u64>>, Vec<Frame>) {
    let mut store = Store::default();
    let mut h = header(REVISION);
    h.kind = KIND_RUN;
    let mut got = Vec::new();
    in_task(true, &mut |context| {
        // SAFETY: the root task pins the store and every handle it makes; each
        // handle is dropped once.
        unsafe {
            let (tape, _) =
                Tape::open(REVISION, store.dir(), b"t", Mode::Record, &h, &expect()).unwrap();
            let made: Vec<_> = (0..conns)
                .map(|_| hirsel_core_conn_new(context, 4, 0, &PLAIN, 8, 0))
                .collect();
            let mut inboxes: Vec<_> = made.iter().map(|m| Some(m.inbox)).collect();
            let mut clients: Vec<_> = made.iter().map(|m| Some(m.client)).collect();
            for step in script {
                match *step {
                    Act::Hook(true) => hold::hook(),
                    Act::Hook(false) => hold::unhook(),
                    Act::TrySend(c, mut v) => {
                        let o = hirsel_core_conn_send(
                            context,
                            clients[c].unwrap(),
                            (&raw mut v).cast(),
                            0,
                            false,
                        );
                        assert_eq!((o.status, o.outcome), (0, 0), "{step:?}");
                    }
                    Act::Reach => assert!(hold::reach_oldest(), "{step:?}: nothing held"),
                    Act::Pump => hold::pump(),
                    Act::TryReceive(c) => {
                        let mut v = 0u64;
                        let o = hirsel_core_conn_receive(
                            context,
                            inboxes[c].unwrap(),
                            (&raw mut v).cast(),
                            0,
                            false,
                        );
                        got.push(match o.code_and_requested {
                            0 => Some(v),
                            3 => None,
                            n => panic!("{step:?}: outcome {n}"),
                        });
                    }
                    Act::Event => crate::tasks::event(crate::tasks::CHECK_CANCEL, 0, 0),
                    Act::DropInbox(c) => {
                        let s = hirsel_core_inbox_drop(context, inboxes[c].take().unwrap());
                        assert_eq!(s, 0, "{step:?}");
                    }
                    Act::DropClient(c) => {
                        hirsel_core_client_drop(context, clients[c].take().unwrap())
                    }
                }
            }
            hold::unhook();
            for (client, inbox) in clients.into_iter().zip(inboxes) {
                if let Some(client) = client {
                    hirsel_core_client_drop(context, client);
                }
                if let Some(inbox) = inbox {
                    assert_eq!(hirsel_core_inbox_drop(context, inbox), 0);
                }
            }
            tape.end(&Ending::returned(&0u32.to_le_bytes())).unwrap();
        }
    });
    (got, tape_frames(store.file()))
}

/// Each `TAKE` before the first frame of the cleanup: (gap, point, count,
/// arrival tag).
fn takes(frames: &[Frame], upto: usize) -> Vec<(u64, TakePoint, u64, u8)> {
    frames[..upto]
        .iter()
        .filter_map(|f| match f {
            Frame::Take {
                gap,
                point,
                count,
                body,
            } => Some((*gap, *point, *count, body.bytes()[0])),
            _ => None,
        })
        .collect()
}

/// The index of the `n`th frame that `f` matches, counting from 1.
fn nth(frames: &[Frame], n: usize, f: impl Fn(&Frame) -> bool) -> usize {
    frames
        .iter()
        .enumerate()
        .filter(|(_, x)| f(x))
        .nth(n - 1)
        .unwrap()
        .0
}

fn is_close(f: &Frame) -> bool {
    matches!(f, Frame::Op { op, .. } if *op == hirsel_tape_format::op::CLOSE)
}

/// RL9 over an RL8 recording: every accepted `CONN_TRY_SEND`'s item, in
/// order, against `sent`, and every item `TAKE` of connection ordinal `conn`
/// against the same list, in connection order.
fn rl9(frames: &[Frame], conn: u8, sent: &[u64]) {
    // recording.md, "Values": `plain` as `bytes` (length 8, then the
    // inline bytes), then a part count of 0.
    let canonical = |v: &u64| {
        let mut b = vec![8u8];
        b.extend_from_slice(&v.to_le_bytes());
        b.push(0);
        b
    };
    let want: Vec<_> = sent.iter().map(canonical).collect();
    let sends: Vec<_> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Op { op, args, out, .. }
                if *op == hirsel_tape_format::op::CONN_TRY_SEND && out.value == 0 =>
            {
                match args.operands() {
                    Args::Literal(b) => Some(b.clone()),
                    a => panic!("an 8-byte item's args are literal: {a:?}"),
                }
            }
            _ => None,
        })
        .collect();
    let taken: Vec<_> = frames
        .iter()
        .filter_map(|f| match f {
            Frame::Take { body, .. } if body.bytes()[..2] == [ITEM, conn] => {
                Some(body.bytes()[2..].to_vec())
            }
            _ => None,
        })
        .collect();
    assert_eq!(sends, want, "sent items");
    assert_eq!(taken, want, "taken items");
}

/// Two `try_receive`s with no event between, the second of which takes: the
/// count is 2, the gap counts the second send's two events. Then two events
/// and a reach between takes: gap 2, count 1 (only the last `try_receive`).
#[test]
fn rl8_two_receives_and_no_event_between() {
    use Act::*;
    let (got, frames) = record(
        1,
        &[
            Hook(true),
            TrySend(0, 10),
            TrySend(0, 11),
            TryReceive(0),
            Reach,
            TryReceive(0),
            Event,
            Reach,
            Event,
            TryReceive(0),
        ],
    );
    assert_eq!(got, [None, Some(10), Some(11)]);
    let cleanup = nth(&frames, 1, is_close);
    assert_eq!(
        takes(&frames, cleanup),
        [(2, Operation, 2, ITEM), (2, Operation, 1, ITEM)]
    );
    // The connection's ordinal is the stream's first.
    rl9(&frames, 1, &[10, 11]);
}

/// `Empty`, then a pump with no event between that takes the item (point
/// kind 1; the `try_receive` before it is its count), then `Ok` with no take.
#[test]
fn rl8_pump_takes_between_receives() {
    use Act::*;
    let (got, frames) = record(
        1,
        &[
            Hook(true),
            TrySend(0, 7),
            TryReceive(0),
            Reach,
            Pump,
            TryReceive(0),
        ],
    );
    assert_eq!(got, [None, Some(7)]);
    let cleanup = nth(&frames, 1, is_close);
    assert_eq!(takes(&frames, cleanup), [(2, TakePoint::Pump, 1, ITEM)]);
    rl9(&frames, 1, &[7]);
}

/// The owner's own release takes, in port order, the older item of
/// connection 2 that had reached it, then connection 1's retirement notice
/// (`release_take`). Connection 1's inbox is cleaned first, so dropping its
/// client is the last release: its `CLOSE` is a frame (count from 0), the
/// release reaches one position, and the two takes share it. The item's gap
/// is the send's two events; the notice follows with no event.
#[test]
fn rl8_release_takes_an_older_item_before_its_notice() {
    use Act::*;
    // The hooked release reaches its notice by name.
    crate::units::ledger::NAMING.store(true, std::sync::atomic::Ordering::Relaxed);
    let (got, frames) = record(
        2,
        &[
            DropInbox(0),
            Hook(true),
            TrySend(1, 20),
            Reach,
            DropClient(0),
            Hook(false),
            TryReceive(1),
        ],
    );
    assert_eq!(got, [Some(20)]);
    // From the client's `CLOSE` (the second; the inbox's is the first) up
    // to the cleanup's first `CLOSE`.
    let release = nth(&frames, 2, is_close);
    let cleanup = nth(&frames, 3, is_close);
    assert_eq!(
        takes(&frames[release..], cleanup - release),
        [(2, Operation, 1, ITEM), (0, Operation, 1, LAST_RELEASE)]
    );
    // Ordinals at `conn.new`: connection 1 is 1, its client 2; connection 2
    // is 3.
    rl9(&frames, 3, &[20]);
}

/// Hook off: the item is all the port holds while its receiver runs, so
/// `try_receive` takes it through `take_single_item`. Gap: the send's two
/// events; count: the `try_receive`'s position.
#[test]
fn rl8_take_single_item() {
    use Act::*;
    let (got, frames) = record(1, &[TrySend(0, 5), TryReceive(0)]);
    assert_eq!(got, [Some(5)]);
    let cleanup = nth(&frames, 1, is_close);
    assert_eq!(takes(&frames, cleanup), [(2, Operation, 1, ITEM)]);
    rl9(&frames, 1, &[5]);
}

/// One receiver recorded twice: the item reaches it before its
/// `try_receive` (taken there, kind 0), or after the `try_receive` found
/// nothing and before the scheduler's pump (taken there, kind 1). Both takes
/// are at the same gap (the send's two events) and count (the one
/// `try_receive` position), and differ only in point kind.
#[test]
fn rl8_one_receiver_at_kinds_0_and_1() {
    use Act::*;
    let (early, at_op) = record(1, &[Hook(true), TrySend(0, 9), Reach, TryReceive(0)]);
    let (late, at_pump) = record(
        1,
        &[
            Hook(true),
            TrySend(0, 9),
            TryReceive(0),
            Reach,
            Pump,
            TryReceive(0),
        ],
    );
    assert_eq!(early, [Some(9)]);
    assert_eq!(late, [None, Some(9)]);
    let upto = |f: &[Frame]| nth(f, 1, is_close);
    assert_eq!(takes(&at_op, upto(&at_op)), [(2, Operation, 1, ITEM)]);
    assert_eq!(
        takes(&at_pump, upto(&at_pump)),
        [(2, TakePoint::Pump, 1, ITEM)]
    );
    rl9(&at_op, 1, &[9]);
    rl9(&at_pump, 1, &[9]);
}
