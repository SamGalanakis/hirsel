//! FAIL-17's stack numbers, shared by the front (the frame ceiling), the
//! evaluator and the native runtime (the task stack's limit word).
//!
//! A task stack is a 16 MiB reservation above one guard page. Pages become
//! writable when touched, so the reservation costs address space only. From
//! the top down:
//! - the **allowance**, 7.5 MiB: user code's activations, refused (`Stack`) at
//!   an entry whose stack pointer, its frame made, lies below it;
//! - the **cleanup margin**, 256 KiB: released by a `Stack` refusal so the
//!   finalizers cleanup runs after it can run, and taken back when an
//!   `isolate` catches the failure;
//! - the **engine reserve**, the rest: at least one whole frame of the largest
//!   size the ceiling admits, plus the runtime's own calls, so a frame made just
//!   above the limit never reaches the guard page.

/// Bytes of a task stack user code may use (FAIL-17's stack allowance).
pub const ALLOWANCE: u64 = 7680 << 10;
/// Bytes below the allowance that finalizers may use after a `Stack` refusal.
/// 256 KiB is 64 activations of a 4 KiB frame: room for a finalizer and the
/// callees it makes, as HotSpot's reserved zone keeps room for critical code,
/// while a finalizer that recurses without bound is still refused.
pub const MARGIN: u64 = 256 << 10;
/// The virtual size of a task stack, above its guard page.
pub const RESERVATION: u64 = 16 << 20;
/// The largest frame, in bytes of slots, a function may have (FAIL-17's frame
/// ceiling): one frame bigger than the allowance can never be entered.
pub const FRAME_CEILING: u64 = ALLOWANCE;

const _: () = assert!(RESERVATION - ALLOWANCE - MARGIN >= FRAME_CEILING + (512 << 10));
