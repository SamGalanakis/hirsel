//! The logical storage policy `charge-v1` (MEM-26). Every admission and refund the
//! front end emits computes its amount with these functions, from logical sizes
//! (`Recipe::logical_size`), never from native sizes, alignments or allocator
//! classes, so a program reaches `Capacity` at the same point on every engine.
//! A later policy is a new module with a new name; packages state which they use.

/// The policy's name as packages and cache keys spell it.
pub const POLICY: &str = "charge-v1";

/// Charge of one task: its bookkeeping plus its primary cause cell (MEM-26, FAIL-10).
pub const TASK: u64 = 256 + 32;
/// Charge of one resource value's cause cell, besides its storage (FAIL-10).
pub const RESOURCE_CAUSE: u64 = 32;
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

/// An `Rc` or `Arc` node: payload plus its count.
pub const fn counted(payload: u64) -> Option<u64> {
    payload.checked_add(8)
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
