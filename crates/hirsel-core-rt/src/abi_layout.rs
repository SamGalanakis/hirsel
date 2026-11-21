// Frozen memory ABI for both supported 64-bit targets. This file is standalone:
// native emitters include these constants without linking runtime semantics.
pub const CONTEXT_CURRENT_LIMIT: usize = 0;
// The process C ABI is identical on the two supported 64-bit targets.
pub const OS_CONFIG_BYTES: u32 = 152;
pub const OS_CONFIG_SECRET_OPS: i32 = 144;
pub const OS_CONFIG_SECRET_GRANTS: i32 = 128;
pub const OS_CONFIG_SECRET_GRANTS_LEN: i32 = 136;
pub const OS_CONFIG_NET_RULES: i32 = 72;
pub const OS_CONFIG_NET_RULES_LEN: i32 = 80;
pub const OS_CONFIG_ROOT_LIMIT: i32 = 0;
pub const OS_CONFIG_ARGS: i32 = 8;
pub const OS_CONFIG_ARGS_LEN: i32 = 16;
pub const OS_CONFIG_DIRECTORIES: i32 = 24;
pub const OS_CONFIG_DIRECTORIES_LEN: i32 = 32;
pub const OS_CONFIG_GRANT_MASK: i32 = 40;
pub const OS_CONFIG_STREAMS: i32 = 48;
pub const OS_CONFIG_PARAMETERS: i32 = 56;
pub const OS_CONFIG_PARAMETERS_LEN: i32 = 64;
pub const OS_CONFIG_TARGET: i32 = 88;
pub const OS_CONFIG_TARGET_LEN: i32 = 96;
pub const OS_CONFIG_DIRECTORY_OPS: i32 = 104;
pub const OS_CONFIG_PROCESS_OPS: i32 = 112;
pub const OS_CONFIG_SOCKET_ACTIVATION: i32 = 120;
pub const PROCESS_CONFIG_GRANTS: i32 = 24;
pub const MAIN_RESULT_KIND: i32 = 0;
pub const MAIN_RESULT_EXIT: i32 = 1;
pub const CONTEXT_EVENTS: usize = 8;
pub const CONTEXT_STEPS: usize = 16;
pub const TASK_ARC_REPLAY: usize = 0;
// Nonzero when the task's init/share/release hooks must run whatever its credit:
// a tape group, an open LATEST pair, an event sink or test schedule, or a
// serial run with sockets.
pub const TASK_ARC_HOOKS: usize = 1;
// The payer word at CONTEXT_CURRENT_LIMIT is a slot; its domain pointer sits at
// SLOT_DOMAIN and the domain's posted-refund flag (an atomic byte, read with
// Acquire) at DOMAIN_PENDING.
pub const SLOT_DOMAIN: usize = 16;
pub const DOMAIN_PENDING: usize = 0;
pub const LIMIT_AVAILABLE: usize = 0;
pub const LIMIT_OUTSTANDING: usize = 8;
pub const REGION_CURSOR: usize = 0;
pub const REGION_END: usize = 8;
pub const REGION_ENTRY: usize = 16;
pub const REGION_PAYER: usize = 24;
pub const REGION_CHARGE: usize = 32;
pub const REGION_SIZE: usize = 40;
pub const REGION_ALIGNMENT: usize = 48;
pub const REGION_OCCUPIED: usize = 56;
pub const REGION_STRIDE: usize = 64;
pub const NODE_COUNT: usize = 0;
pub const NODE_PAYLOAD: usize = 8;
pub const GRAPH_PREFIX_BYTES: usize = 40;
pub const RESOURCE_PREFIX_BYTES: usize = 8;
pub const NATIVE_PLAIN: u8 = 0;
pub const NATIVE_GRAPH: u8 = 1;
pub const NATIVE_RESOURCE: u8 = 2;
pub const FIXED_DESCRIPTOR_RECIPE: usize = 0;
pub const FIXED_DESCRIPTOR_CHARGE: usize = 8;
pub const FIXED_DESCRIPTOR_PAYLOAD_OFFSET: usize = 16;
pub const FIXED_DESCRIPTOR_BYTES: usize = 24;
pub const FIXED_POINT_BACK: usize = 0;
pub const FIXED_POINT_CURSOR: usize = 8;
pub const FIXED_POINT_A: usize = 16;
pub const FIXED_POINT_B: usize = 24;
pub const FIXED_POINT_INDEX: usize = 32;
pub const FIXED_POINT_BYTES: usize = 40;
pub const FIXED_POINT_NO_INDEX: u64 = u64::MAX;
// A point's cursor word for a yield after which its glue has nothing left
// (hirsel_core_layout::Point::last): the drain frees the block on resume.
pub const FIXED_POINT_DONE: u64 = u64::MAX;
// A drain's worklist (counted::Worklist), written inline by generated glue to
// hand over one descendant: node, then descriptor (0 for a GRAPH buffer whose
// frame stage_buffer wrote), then the running fixed block's point (0 under a buffer).
// Recipes read the release's site word and OR a failed finalizer or handle into
// the failed byte, then go on cleaning.
pub const WORKLIST_HEAD: usize = 0;
pub const WORKLIST_NODE: usize = 8;
pub const WORKLIST_DESCRIPTOR: usize = 16;
pub const WORKLIST_POINT: usize = 24;
pub const WORKLIST_SITE: usize = 32;
pub const WORKLIST_FAILED: usize = 40;
/// Byte offset of point `k` (1-based) from its descriptor.
pub const fn fixed_point(k: u64) -> usize {
    FIXED_DESCRIPTOR_BYTES + FIXED_POINT_BYTES * (k as usize - 1)
}
pub const fn own_prefix(kind: u8) -> usize {
    match kind {
        NATIVE_PLAIN => 0,
        NATIVE_GRAPH => GRAPH_PREFIX_BYTES,
        NATIVE_RESOURCE => RESOURCE_PREFIX_BYTES,
        _ => panic!("native block kind"),
    }
}
// Fixed blocks are PLAIN like terminal ones: the kind is a physical layout,
// never a cleanup decision; emitters pick fixed glue from cleanup_class. Each
// concrete (owner, type) has an immutable descriptor {recipe, charge, payload
// offset} followed inline by its suspension points, point k at fixed_point(k):
// {back = fixed_point(k), cursor, a, b, index or FIXED_POINT_NO_INDEX}. When a
// fixed block's glue yields a descendant at point k, the drain writes the point
// address at payload+a, the next frame link at payload+b and, for a point
// inside an inline array, the cursor at payload+index: words that glue never
// reads again (hirsel_core_layout::frames). Dynamic buffers retain their GRAPH frame;
// resources their separate prefix. Kind 3 is unused.

// Arc count mutations are inline under the scheduler's event turn. After the
// operation, with released before last-release glue, take the context-first
// hook when task = context[CONTEXT_EVENTS] != 0. Initialized, shared and
// released call it only when task[TASK_ARC_HOOKS] != 0 or the payer's
// domain pending byte is set; otherwise the hook would neither fold credit nor
// record. Unwrapped always calls it: a unique test is a poll. The hook folds
// the owner's posted credits and records the event. Trace kinds are
// layout::event::ARC_*.

// Each allocation page owns a payer identity. Its outstanding charges keep the
// original slot alive across scope exit. Free resolves the payer before its
// slot returns to the allocating task, including on another worker.

// FORM #55. cause_value borrows this exact core layout, not a translated view.
// Cells begin with Cause followed by next; native funding follows the prefix.
// CauseKind is Fail's String's niche (FORM #75): kind k other than Fail (4) is the
// capacity word CAUSE_KIND_START + k at CAUSE_KIND; a Fail's capacity is below it.
pub const CAUSE_BYTES: usize = 32;
pub const CAUSE_MESSAGE: usize = 0;
pub const CAUSE_MESSAGE_DATA: usize = 0;
pub const CAUSE_MESSAGE_LENGTH: usize = 8;
pub const CAUSE_MESSAGE_CAPACITY: usize = 16;
pub const CAUSE_KIND: usize = CAUSE_MESSAGE_CAPACITY;
pub const CAUSE_KIND_START: u64 = 1 << 63;
pub const CAUSE_FAIL: u8 = 4;
pub const CAUSE_SITE: usize = 24;
pub const CAUSE_CELL_VALUE: usize = 0;
pub const CAUSE_CELL_NEXT: usize = 32;
/// The word at CAUSE_KIND of a cause of `kind`, which is not Fail.
pub const fn cause_kind_word(kind: u8) -> u64 {
    CAUSE_KIND_START + kind as u64
}
/// The kind of a cause whose CAUSE_KIND word is `word`.
pub const fn cause_kind(word: u64) -> u8 {
    if word < CAUSE_KIND_START {
        CAUSE_FAIL
    } else {
        (word - CAUSE_KIND_START) as u8
    }
}

// Emitters admit positive charges inline and pass the context and payer to
// hirsel_core_alloc_admitted. Refusals, zero-charge pins and foreign growth use
// the full allocation entry. Native exhaustion remains a HostMemory run stop.

// FORM #84/#89: an alloc whose named owning edge is terminal
// (hirsel_core_layout::cleanup_class) or a Fixed one selects NATIVE_PLAIN;
// a Fixed block's native payload is Frames::native_bytes of its value, adding
// uncharged spare words. Dynamic reserves GRAPH_PREFIX_BYTES. Count stays at
// NODE_COUNT, payload at NODE_PAYLOAD. A terminal owning edge frees immediately
// after its last count event; a Fixed one goes through its descriptor.
// Like/growth retain the same cleanup property; logical charges and
// admission/refund ordering are independent of this native prefix selection.

/// Reuse an owned sequence buffer from this many OLD native payload bytes.
/// Capacity times native element stride, excluding the prefix. Both emitters
/// use this one allocator policy; logical admission remains the full new charge.
pub const GROW_CALL_MIN_PAYLOAD_BYTES: u64 = 2048;
