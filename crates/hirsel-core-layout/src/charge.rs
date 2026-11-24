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
/// Charge of a connection's bookkeeping, besides its slots and room (CONC-17).
pub const CONNECTION_BASE: u64 = 64;
/// The largest slot count of a connection (CONC-17).
pub const CONNECTION_MAX_CAPACITY: u64 = (1 << 22) - 1;
/// The largest byte room of a connection (CONC-17).
pub const CONNECTION_MAX_ROOM: u64 = (1 << 40) - 1;
/// A `Latest` root's bookkeeping: its `Arc` holder, handle count, identity and
/// the holder's drop (CONC-13), exactly the record the engine keeps.
pub const LATEST_ROOT: u64 = 32;
/// A `Shared` node's bookkeeping beside its payload (CONC-15): the atomic
/// count, the home Unit's port, the payload's owners table and its length,
/// and the node's own charge, so whichever Unit drops the last holder can
/// send the node home to be released.
pub const SHARED_NODE: u64 = 40;
/// A `provide.clock`, `provide.random` or `provide.process` cell (MEM-24),
/// held by the constructing payer until the run ends, as a narrowed grant.
pub const PROVIDE_CELL: u64 = 32;
/// Each mechanical Proc or pipe designation admitted by Exec.spawn (EXEC-3).
pub const EXEC_HANDLE: u64 = 8;
/// Logical size of a Map entry's bookkeeping beside its key and value.
pub const MAP_ENTRY_EXTRA: u64 = 16;
/// A flow port's bookkeeping, besides its slots.
pub const FLOW_PORT_BASE: u64 = 64;
/// A flow port slot's bookkeeping, besides its item.
pub const FLOW_SLOT_EXTRA: u64 = 16;
/// The count in an Rc, Arc or Unique node.
pub const COUNTED_EXTRA: u64 = 8;
/// The low bits cleared when rounding a charge to a multiple of 8.
pub const ROUND_MASK: u64 = 7;

/// Logical charge for a held value. Resource and region blocks are admitted
/// separately, so their holders only count the resource or region reference.
pub fn held(repr: crate::Repr, recipe: &crate::Recipe) -> Option<u64> {
    match repr {
        crate::Repr::Resource => Some(crate::POINTER),
        crate::Repr::Region => Some(16),
        _ => recipe.logical_size,
    }
}
/// Growth that doubles capacity starts from this many elements.
pub const FIRST_GROWTH: u64 = 4;

/// `n` rounded up to a multiple of 8. `None` when the rounding overflows u64; the
/// caller's admission then fails with `Capacity` (no limit holds such a charge).
pub const fn round8(n: u64) -> Option<u64> {
    match n.checked_add(ROUND_MASK) {
        Some(m) => Some(m & !ROUND_MASK),
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

/// The largest capacity, so also length, an own `List[T]` carrier can hold when `T`
/// has logical size `size` (FORM #113): the largest `capacity` whose [`seq`] charge
/// is admitted, `floor((2^64 - 8) / size)`. By logical size, not stride: MEM-24
/// charges `round8(capacity x size(T))` and SEM-4 says an own List's length is never
/// more than its capacity, so this is the bound the spec gives. No fact for sizes 0
/// and 1: a zero-sized element's length is unbounded (SEM-4), and a static
/// `as_bytes` or argument view is a `Bytes` carrier whose length exceeds its capacity.
pub const fn seq_len_bound(size: u64) -> Option<u64> {
    if size < 2 {
        None
    } else {
        Some((u64::MAX - 7) / size)
    }
}

/// `Map[K, V]` storage of `capacity` entries.
pub const fn map(capacity: u64, key: u64, value: u64) -> Option<u64> {
    match key.checked_add(value) {
        Some(kv) => match kv.checked_add(MAP_ENTRY_EXTRA) {
            Some(entry) => seq(capacity, entry),
            None => None,
        },
        None => None,
    }
}

/// An `Rc` or `Arc` node: payload plus its count, rounded up to 8.
pub const fn counted(payload: u64) -> Option<u64> {
    match payload.checked_add(COUNTED_EXTRA) {
        Some(n) => round8(n),
        None => None,
    }
}

/// A `Shared` node: payload plus `SHARED_NODE`, rounded up to 8.
pub const fn shared(payload: u64) -> Option<u64> {
    match payload.checked_add(SHARED_NODE) {
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

/// A `Text` or `FixedSpan` made outside a compact type: its 8-byte count and its
/// payload bytes, rounded up to 8 (it is encoded with `W = u64`, MEM-9).
pub const fn standalone(payload: u64) -> Option<u64> {
    match payload.checked_add(8) {
        Some(n) => round8(n),
        None => None,
    }
}

/// A span builder's allocation: a region allocation of `capacity` elements of
/// logical size `elem` plus its 8-byte count (MEM-24, MEM-3).
pub const fn span_builder(capacity: u64, elem: u64) -> Option<u64> {
    match capacity.checked_mul(elem) {
        Some(n) => match n.checked_add(8) {
            Some(m) => round8(m),
            None => None,
        },
        None => None,
    }
}

/// `seq.index_in` of a sequence of `count` elements (MEM-24).
pub const fn seq_index(count: u64) -> Option<u64> {
    match count.checked_mul(8) {
        Some(n) => match n.checked_add(24) {
            Some(m) => round8(m),
            None => None,
        },
        None => None,
    }
}

/// A region allocation (a sized destination, `build_in` and
/// `build_stream_in` of `n` bytes; `finish_trim` lowers it to the used bytes).
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

/// A flow port: a record and `capacity` slots, each holding an item.
pub fn port(capacity: u64, item: u64) -> Option<u64> {
    item.checked_add(FLOW_SLOT_EXTRA)?
        .checked_mul(capacity)?
        .checked_add(FLOW_PORT_BASE)
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
