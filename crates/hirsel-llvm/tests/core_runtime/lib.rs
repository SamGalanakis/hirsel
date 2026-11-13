//! C executable fixtures inspect the real runtime, with no shadow implementation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_test_available(
    context: *const hirsel_core_rt::Context,
) -> u64 {
    // SAFETY: native fixture passes its live task context.
    unsafe { (&*context).available() }
}
// Keep the actual native runtime entry points in this staticlib.
#[used]
static CONTEXT: extern "C" fn(u64) -> *mut hirsel_core_rt::Context =
    hirsel_core_rt::abi::hirsel_core_context_new;
