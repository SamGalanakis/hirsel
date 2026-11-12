//! The logical storage policy `charge-v1` (MEM-24). Every admission and refund the
//! front end emits computes its amount with these functions, from logical sizes
//! (`Recipe::logical_size`), never from native sizes, alignments or allocator
//! classes, so a program reaches `Capacity` at the same point on every engine.
//! A later policy is a new module with a new name; packages state which they use.
//!
//! Arithmetic is exact (MEM-22): a `None` result is a charge past `2^64 - 1`, which
//! every admission refuses with `requested = 2^64 - 1`.

/// The policy's name as packages and cache keys spell it.
pub const POLICY: &str = "charge-v1";

/// A task's bookkeeping, besides its result, cause cell and reserved limit.
pub const TASK_RECORD: u64 = 256;
/// One cause cell: a task's primary cell or a resource's cell (FAIL-10).
pub const CAUSE_CELL: u64 = 32;
/// Charge of a channel's bookkeeping, besides its items.
pub const CHANNEL_BASE: u64 = 64;
/// Logical size of a Map entry's bookkeeping beside its key and value.
pub const MAP_ENTRY_EXTRA: u64 = 16;
/// Growth that doubles capacity starts from this many elements.
pub const FIRST_GROWTH: u64 = 4;

/// `n` rounded up to a multiple of 8. `None` when the rounding overflows u64; the
/// caller's admission then fails with `Capacity` (no limit holds such a charge).
pub const fn round8(n: u64) -> Option<u64> {
    match n.checked_add(7) {
        Some(m) => Some(m & !7),
        None => None,
    }
}

/// `Box[T]` node: the payload's logical size, rounded up to 8.
pub const fn boxed(payload: u64) -> Option<u64> {
    round8(payload)
}

/// `List[T]`, `String`, `Bytes` storage of `capacity` elements.
pub const fn seq(capacity: u64, elem: u64) -> Option<u64> {
    match capacity.checked_mul(elem) {
        Some(n) => round8(n),
        None => None,
    }
}

/// `Map[K, V]` storage of `capacity` entries.
pub const fn map(capacity: u64, key: u64, value: u64) -> Option<u64> {
    match key.checked_add(value) {
        Some(kv) => match capacity.checked_mul(kv + MAP_ENTRY_EXTRA) {
            Some(n) => round8(n),
            None => None,
        },
        None => None,
    }
}

/// An `Rc` or `Arc` node: payload plus its count, rounded up to 8.
pub const fn counted(payload: u64) -> Option<u64> {
    match payload.checked_add(8) {
        Some(n) => round8(n),
        None => None,
    }
}

/// An owning closure's environment: the sum of its captures' sizes, rounded up to 8.
pub const fn closure_env(captures: u64) -> Option<u64> {
    round8(captures)
}

/// A resource value: its storage, its cause cell and its finalizer grant (MEM-27).
pub const fn resource(storage: u64, grant: u64) -> Option<u64> {
    match storage.checked_add(CAUSE_CELL) {
        Some(n) => n.checked_add(grant),
        None => None,
    }
}

/// A task: record, result, primary cause cell and its reserved limit `n` (CONC-3).
pub const fn task(result: u64, n: u64) -> Option<u64> {
    match (TASK_RECORD + CAUSE_CELL).checked_add(result) {
        Some(r) => r.checked_add(n),
        None => None,
    }
}

/// A `Text` or span made outside a compact type: count, extent, one offset per
/// element when `indexed`, then the payload bytes, rounded up to 8.
pub const fn standalone_span(elements: u64, payload: u64, indexed: bool) -> Option<u64> {
    let table = if indexed {
        elements.checked_mul(8)
    } else {
        Some(0)
    };
    match table {
        Some(t) => match (16 + t).checked_add(payload) {
            Some(n) => round8(n),
            None => None,
        },
        None => None,
    }
}

/// A region allocation (a placed literal, a builder chunk, a sized destination).
pub const fn region(bytes: u64) -> Option<u64> {
    round8(bytes)
}

/// A channel of `capacity` items of logical size `item`, admitted whole at creation.
pub const fn channel(capacity: u64, item: u64) -> Option<u64> {
    match capacity.checked_mul(item) {
        Some(n) => n.checked_add(CHANNEL_BASE),
        None => None,
    }
}

/// The capacity a doubling growth step chooses: `max(4, 2 * current)`, and at least
/// `needed`. `None` when doubling overflows.
pub const fn grown_capacity(current: u64, needed: u64) -> Option<u64> {
    let doubled = match current.checked_mul(2) {
        Some(d) => d,
        None => return None,
    };
    let c = if doubled < FIRST_GROWTH {
        FIRST_GROWTH
    } else {
        doubled
    };
    Some(if c < needed { needed } else { c })
}
