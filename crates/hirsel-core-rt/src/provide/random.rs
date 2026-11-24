use super::{Cell, Context, cell, files, lock};
use crate::provider::{Buffer, IoError, Outcome, Provider};
use hirsel_core_layout::charge::PROVIDE_CELL;
use hirsel_core_net::random_stream::RandomStream;
use std::{slice, sync::Mutex};

pub(crate) const TAPE_BOUNDS: u8 = 255;

/// # Safety
/// This provider is a run-pinned virtual Random cell.
pub(crate) unsafe fn fill_virtual(provider: &Provider, out: &mut [u8]) -> Result<(), ()> {
    let Cell::Random(random) = (unsafe { cell(provider.state) }) else {
        unreachable!("a Random cell");
    };
    lock(random).fill(out).then_some(()).ok_or(())
}

/// # Safety
/// A running context and a pinned, initialized Bytes carrier consumed by the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_provide_random_tape(
    context: *mut Context,
    source: *const Buffer,
    _site: u64,
) -> Outcome {
    // SAFETY: forwarded; no carrier pointer escapes the call.
    let (files, bytes) = unsafe { (files(context), crate::tls::bytes(&*source)) };
    let charge = PROVIDE_CELL + bytes.len() as u64;
    // SAFETY: the running context has a current payer.
    match unsafe {
        files.registry.run_grants().cells.add(
            context,
            || Cell::Random(Mutex::new(RandomStream::tape(bytes.to_vec()))),
            charge,
        )
    } {
        Ok(provider) => Outcome {
            value: files.registry.grant(provider, 0),
            ..Outcome::default()
        },
        Err(mut refused) => {
            refused.value = charge;
            refused.error.tag = 1;
            refused
        }
    }
}

/// # Safety
/// The request pins exclusive writable storage for the buffer's spare.
pub(super) unsafe fn draw_request(random: &mut RandomStream, buffer: &mut Buffer) -> Outcome {
    let spare = buffer.capacity.saturating_sub(buffer.len);
    if spare == 0 {
        return Outcome::default();
    }
    // SAFETY: spare lies within the caller's exclusive writable storage.
    let out = unsafe { slice::from_raw_parts_mut(buffer.data.add(buffer.len).cast(), spare) };
    if random.draw(out) {
        buffer.len += spare;
        Outcome {
            written: spare as u64,
            ..Outcome::default()
        }
    } else {
        Outcome {
            error: IoError {
                tag: TAPE_BOUNDS,
                code: 0,
            },
            ..Outcome::default()
        }
    }
}
