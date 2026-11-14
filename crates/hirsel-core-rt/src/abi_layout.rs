// Frozen memory ABI for both supported 64-bit targets. This file is standalone:
// native emitters include these constants without linking runtime semantics.
pub const CONTEXT_CURRENT_LIMIT: usize = 0;
pub const CONTEXT_EVENTS: usize = 8;
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

// Arc count mutations are inline under the scheduler's event turn.
// If context[CONTEXT_EVENTS] != 0, call the context-first cold hook
// hirsel_core_arc_initialized/shared/released/unwrapped after the operation,
// with released before last-release glue. These constants identify trace events.
pub const ARC_EVENT_NEW: u8 = 10;
pub const ARC_EVENT_SHARE: u8 = 11;
pub const ARC_EVENT_RELEASE: u8 = 12;
pub const ARC_EVENT_UNWRAP: u8 = 13;

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

// FORM #84: alloc plain and counted { plain:true } select PLAIN_PREFIX_BYTES.
// Count stays at NODE_COUNT, payload at NODE_PAYLOAD. A terminal owning edge
// frees immediately after its last count event; it never borrows a graph frame.
// Recipes derive terminal from hirsel_core_layout::terminal. Generic allocation
// producers repeat this derivation after substituting the concrete owning edge.
// Like/growth retain the same property and original payer; logical charges and
// admission/refund ordering are independent of this native prefix selection.
