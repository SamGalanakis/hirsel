//! CONC-10 event kinds: the one table of trace numbers. The runtime records these,
//! the evaluator records the same list, and C readers take `HIRSEL_CORE_EVENT_*`
//! from `hirsel_core_rt_host.h`, which a runtime test compares with `ALL`. A number
//! names one kind everywhere; the check below refuses two kinds that share one.

pub const SPAWN: u8 = 0;
pub const END: u8 = 1;
pub const CANCEL: u8 = 2;
pub const CANCELLED: u8 = 3;
pub const JOIN: u8 = 4;
pub const PUBLISH: u8 = 5;
pub const RECEIVE: u8 = 6;
pub const TRY_SEND: u8 = 7;
pub const SENDERS: u8 = 8;
pub const CLOSE: u8 = 9;
pub const ARC_NEW: u8 = 10;
pub const ARC_SHARE: u8 = 11;
pub const ARC_RELEASE: u8 = 12;
pub const ARC_UNWRAP: u8 = 13;
pub const REFUND: u8 = 14;
pub const CHECK_CANCEL: u8 = 16;
pub const CAPACITY: u8 = 17;
/// CONC-13: a `Latest` root replaced, or read. Neither records a value.
pub const LATEST_PUBLISH: u8 = 18;
pub const LATEST_READ: u8 = 19;
pub const TRY_RECEIVE: u8 = 20;
/// MEM-30: the 65,536th taken source Step under a bound, before scheduling.
pub const STEP_SLICE: u8 = 21;

/// Value for a blocking PUBLISH or RECEIVE that ends with Closed. A transfer records 0.
pub const CHANNEL_CLOSED: u64 = 2;
pub const CHANNEL_EMPTY: u64 = 1;
pub const CHANNEL_PARKED: u64 = 3;

/// Every kind by its header name.
pub const ALL: [(&str, u8); 21] = [
    ("SPAWN", SPAWN),
    ("END", END),
    ("CANCEL", CANCEL),
    ("CANCELLED", CANCELLED),
    ("JOIN", JOIN),
    ("PUBLISH", PUBLISH),
    ("RECEIVE", RECEIVE),
    ("TRY_SEND", TRY_SEND),
    ("SENDERS", SENDERS),
    ("CLOSE", CLOSE),
    ("ARC_NEW", ARC_NEW),
    ("ARC_SHARE", ARC_SHARE),
    ("ARC_RELEASE", ARC_RELEASE),
    ("ARC_UNWRAP", ARC_UNWRAP),
    ("REFUND", REFUND),
    ("CHECK_CANCEL", CHECK_CANCEL),
    ("CAPACITY", CAPACITY),
    ("LATEST_PUBLISH", LATEST_PUBLISH),
    ("LATEST_READ", LATEST_READ),
    ("TRY_RECEIVE", TRY_RECEIVE),
    ("STEP_SLICE", STEP_SLICE),
];

const _: () = {
    let mut i = 0;
    while i < ALL.len() {
        let mut j = i + 1;
        while j < ALL.len() {
            assert!(ALL[i].1 != ALL[j].1, "two event kinds share a number");
            j += 1;
        }
        i += 1;
    }
};

/// CONC-10's no-effect polls, which use the task's poll allowance: a try form that
/// finds the queue Full or Empty (value 1), a refused `try_unwrap` or a check with no
/// request (value 0), and a refused admission.
#[inline(always)]
pub const fn is_poll(kind: u8, value: u64) -> bool {
    match kind {
        TRY_SEND | TRY_RECEIVE => value == 1,
        RECEIVE => value == CHANNEL_EMPTY,
        ARC_UNWRAP | CHECK_CANCEL => value == 0,
        CAPACITY => true,
        _ => false,
    }
}
