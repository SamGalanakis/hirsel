use super::*;

/// `provide.payload(c, field, mut out)`: append the field's bytes while the
/// call is unsettled (a settled call's caller storage is gone: nothing); a
/// short spare capacity raises Bounds before copying anything.
/// # Safety
/// Context is running, `call` is a live `Call` key and `out` a live mutable
/// Bytes carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_call_payload(
    context: *mut Context,
    call: u64,
    field: u64,
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded.
    unsafe { hirsel_core_call_payload_prefix(context, call, field, u64::MAX, out, site) }
}

/// `provide.payload_prefix(c, field, max, mut out)`: append at most `max`
/// bytes of an unsettled call's field and return the omitted byte count in `value`.
/// # Safety
/// Context is running, `call` is a live `Call` key and `out` a live mutable
/// Bytes carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_call_payload_prefix(
    context: *mut Context,
    call: u64,
    field: u64,
    max: u64,
    out: *mut Buffer,
    site: u64,
) -> Outcome {
    // SAFETY: forwarded; an unsettled call's caller is parked, so its request
    // and the bytes it borrows are pinned.
    unsafe {
        let (port, id) = call_of(context, call);
        let port = lock(port);
        let c = &port.calls[&id];
        if c.finished || c.answer.is_some() {
            return Outcome::default();
        }
        let from = field_bytes(port.vocab, c, field);
        let count = (from.len() as u64).min(max) as usize;
        let out = &mut *out;
        if out.capacity.saturating_sub(out.len) < count {
            causes::record(&mut *context, causes::Cause::new(causes::BOUNDS, site));
            return CANCELLED;
        }
        if count != 0 {
            ptr::copy_nonoverlapping(from.as_ptr(), out.data.add(out.len), count);
            out.len += count;
        }
        Outcome {
            value: (from.len() - count) as u64,
            written: count as u64,
            ..Outcome::default()
        }
    }
}
