//! The served Exec call and reply words shared by the evaluator and runtime.
pub const SPAWN: u64 = 0;
pub const AWAIT: u64 = 1;
pub const SIGNAL: u64 = 2;
pub const RECV: u64 = 3;
pub const SEND: u64 = 4;
pub const RELEASED: u64 = 5;

pub const STARTED: u64 = 17;

pub fn exec_error_op(op: u64) -> bool {
    matches!(op, SPAWN | AWAIT | SIGNAL)
}

/// Validate a reply before allocating new designations or committing staged bytes.
pub fn answer(
    call: &[u64; 9],
    staged: u64,
    tag: u64,
    words: &[u64],
    occupied: impl Fn(u64) -> bool,
) -> Option<[u64; 4]> {
    let [a, b, c, d] = words[..4].try_into().unwrap();
    let reply = match (call[0], tag) {
        (SPAWN | AWAIT | SIGNAL, 1) if (1..=4).contains(&a) => [0, a, b, 0],
        (RECV | SEND, 16) if (1..=9).contains(&a) => [0, a, b, 0],
        (SIGNAL, 0) => [0; 4],
        (AWAIT, 18) if a <= 511 => [a, 0, 0, 0],
        (RECV, 3) if a == staged => [a, 0, 0, 0],
        (SEND, 3) if a <= call[2].saturating_add(call[3]) => [a, 0, 0, 0],
        (SPAWN, STARTED) => {
            let ids = [a, b, c, d];
            for (i, (id, required)) in ids
                .into_iter()
                .zip([1, call[6], call[7], call[8]])
                .enumerate()
            {
                if (id != 0) != (required != 0)
                    || id != 0 && (ids[..i].contains(&id) || occupied(id))
                {
                    return None;
                }
            }
            [a, 0, 0, 0]
        }
        _ => return None,
    };
    Some(reply)
}
