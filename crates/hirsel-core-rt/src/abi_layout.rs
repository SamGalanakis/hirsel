// Frozen memory ABI for both supported 64-bit targets. This file is standalone:
// native emitters include these constants without linking runtime semantics.
pub const CONTEXT_CURRENT_LIMIT: usize = 0;
// The process C ABI is identical on the two supported 64-bit targets.
pub const OS_CONFIG_BYTES: u32 = 104;
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
pub const PROCESS_CONFIG_GRANTS: i32 = 24;
pub const MAIN_RESULT_KIND: i32 = 0;
pub const MAIN_RESULT_EXIT: i32 = 1;
pub const CONTEXT_EVENTS: usize = 8;
pub const CONTEXT_STEPS: usize = 16;
pub const TASK_ARC_GATE: usize = 0;
pub const LIMIT_AVAILABLE: usize = 0;
pub const LIMIT_OUTSTANDING: usize = 8;
pub const REGION_CURSOR: usize = 0;
pub const REGION_END: usize = 8;
pub const REGION_ENTRY: usize = 16;
pub const REGION_PAYER: usize = 24;
pub const REGION_CHARGE: usize = 32;
pub const NODE_COUNT: usize = 0;
pub const NODE_PAYLOAD: usize = 8;
pub const OWN_PAYER_BACK: usize = 8;
pub const OWN_TAG_MASK: usize = 31;
pub const OWN_KIND_MASK: usize = 3;
pub const OWN_ALIGNMENT_SHIFT: usize = 2;
pub const PLAIN_PREFIX_BYTES: usize = 8;
pub const GRAPH_PREFIX_BYTES: usize = 48;
pub const RESOURCE_PREFIX_BYTES: usize = 16;
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
        NATIVE_PLAIN => PLAIN_PREFIX_BYTES,
        NATIVE_GRAPH => GRAPH_PREFIX_BYTES,
        NATIVE_RESOURCE => RESOURCE_PREFIX_BYTES,
        _ => panic!("native block kind"),
    }
}
// Fixed blocks are PLAIN8 like terminal ones: the kind is a physical layout,
// never a cleanup decision; emitters pick fixed glue from cleanup_class. Each
// concrete (owner, type) has an immutable descriptor {recipe, charge, payload
// offset} followed inline by its suspension points, point k at fixed_point(k):
// {back = fixed_point(k), cursor, a, b, index or FIXED_POINT_NO_INDEX}. When a
// fixed block's glue yields a descendant at point k, the drain writes the point
// address at payload+a, the next frame link at payload+b and, for a point
// inside an inline array, the cursor at payload+index: words that glue never
// reads again (hirsel_core_layout::frames). Dynamic buffers retain GRAPH48;
// resources their separate prefix. Kind 3 is unused.

// Arc count mutations are inline under the scheduler's event turn. After the
// operation, with released before last-release glue, call the context-first
// cold hook when task = context[CONTEXT_EVENTS] != 0: hirsel_core_arc_unwrapped
// always (it may poll), hirsel_core_arc_initialized/shared/released only when
// the acquire byte task[TASK_ARC_GATE] is nonzero. The runtime keeps that byte
// set while the run records, and otherwise exactly while the task has a refund
// to deliver (MEM-20). Their trace kinds are hirsel_core_layout::event's ARC_*.

// General own free/refund: load the raw payer word at payload-OWN_PAYER_BACK
// and compare it directly with context[CONTEXT_CURRENT_LIMIT]. On equality and
// nonzero charge, add to LIMIT_AVAILABLE and subtract from LIMIT_OUTSTANDING.
// On inequality (including native tag bits), call hirsel_core_refund(original,
// charge), which strips tags and follows forwarding records. The C slow entry
// hirsel_core_refund_in(context, original, charge) implements the same sequence.
// An outstanding charge prevents reuse of its payer slot, so an equal word
// cannot be an earlier incarnation after exit/re-entry. Current is canonical;
// forwarded parents and handed-over survivors differ until lookup resolves them.

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

// Ordinary allocation contract: nonzero exact charge, native size fitting a
// Layout, canonical task-current payer, and ordinary native alignment (<=8).
// An overflow operand being present is not a reason to bypass this path: test
// its value. Current/null Like use context current; nonnull Like first proves
// its untagged payer equals current, otherwise calls hirsel_core_alloc once.
// Compare available before the debit. Shortage, overflow, zero charge and foreign
// Like take that same existing cold entry, retaining Capacity events/fairness.
// Native allocate then receives max(bytes,1)+prefix and alignment 8; the emitted
// store at payload-OWN_PAYER_BACK records current|kind. No semantic tag layout
// (including variants/niches) is involved.
// hirsel_core_native_alloc_ordinary(native_bytes) is the fixed-alignment backend
// when the producer knows alignment <=16: nonnull base or HostMemory run stop,
// no admission/event/status and no dynamic alignment operand. A native payload
// offset lies within that returned allocation, so LLVM may use nonnull/inbounds.
// A producer may fold positive charge plus fitting room into unsigned
// (charge - 1) < available, with wrapping subtraction: zero becomes MAX and
// never fits, including available=MAX. Preserve the zero-pin/shortage cold entry
// and probe only fitting positive admissions. Both choices use the same offsets.
pub const OWN_ORDINARY_PLAIN_TAG: usize = 0;
pub const OWN_ORDINARY_GRAPH_TAG: usize = 1;
pub const OWN_ORDINARY_MAX_TAG: usize = OWN_ORDINARY_GRAPH_TAG;
// Free: raw_header XOR current <= OWN_ORDINARY_MAX_TAG proves exact current
// identity plus ordinary plain/GRAPH native layout. For nonzero charge, refund
// available/outstanding directly and free(payload - the selected prefix).
// Otherwise call free_value; it preserves original-payer/remote-event handling.
// Outstanding bytes retain the old slot across scope exit and re-entry. Resume
// only binds/normalizes custody; owner events deliver eligible pending refunds.
// Ordinary admission does not apply pending refunds or add an event. Refusal
// snapshots Capacity before its existing event. This local path adds no TLS
// lookup, poll, drain, lock or atomic update.

// FORM #84/#89: an alloc whose named owning edge is terminal
// (hirsel_core_layout::cleanup_class) or a Fixed one selects PLAIN_PREFIX_BYTES;
// a Fixed block's native payload is Frames::native_bytes of its value, adding
// uncharged spare words. Dynamic uses GRAPH_PREFIX_BYTES. Count stays at
// NODE_COUNT, payload at NODE_PAYLOAD. A terminal owning edge frees immediately
// after its last count event; a Fixed one goes through its descriptor.
// Like/growth retain the same property and original payer; logical charges and
// admission/refund ordering are independent of this native prefix selection.

/// Reuse an owned sequence buffer from this many OLD native payload bytes.
/// Capacity times native element stride, excluding the prefix. Both emitters
/// use this one allocator policy; logical admission remains the full new charge.
pub const GROW_REALLOC_MIN_PAYLOAD_BYTES: u64 = 2048;
