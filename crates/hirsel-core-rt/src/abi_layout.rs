// Frozen memory ABI for both supported 64-bit targets. This file is standalone:
// native emitters include these constants without linking runtime semantics.
pub const CONTEXT_CURRENT_LIMIT: usize = 0;
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

// Arc count mutations are inline under the scheduler's event turn. Call
// hirsel_core_arc_initialized/shared/released/unwrapped after the operation,
// with released before last-release glue. These constants identify trace events.
pub const ARC_EVENT_NEW: u8 = 10;
pub const ARC_EVENT_SHARE: u8 = 11;
pub const ARC_EVENT_RELEASE: u8 = 12;
pub const ARC_EVENT_UNWRAP: u8 = 13;
