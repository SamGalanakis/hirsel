use super::{Allocation, Context, NonNull};

/// Admit a small byte buffer, with physical backing for one second buffer.
/// # Safety
/// Context is live and task-exclusive. The nonempty buffer has the given charge.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_alloc_byte_head(
    context: *const Context,
    bytes: usize,
    charge: u64,
    overflow: bool,
) -> Allocation {
    crate::split_bytes::key(unsafe { &*context }, bytes, (!overflow).then_some(charge)).into()
}

/// Admit a byte buffer, using unused backing beside a lent head when it fits.
/// # Safety
/// Context is live and task-exclusive. A nonnull head belongs to a live
/// exclusively lent Bytes carrier with nonzero capacity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_alloc_byte_tail(
    context: *const Context,
    head: *mut u8,
    bytes: usize,
    charge: u64,
    overflow: bool,
) -> Allocation {
    unsafe {
        crate::split_bytes::tail(
            &*context,
            NonNull::new(head),
            bytes,
            (!overflow).then_some(charge),
        )
    }
    .into()
}

fn allocate_file_bytes(
    context: &Context,
    head: Option<usize>,
    bytes: usize,
    charge: u64,
    overflow: bool,
) -> Allocation {
    let payer = context.payer();
    if let Err(e) = crate::block::admit_storage(payer, (!overflow).then_some(charge)) {
        return Err(e).into();
    }
    if let Some(slot) = crate::host::file::allocate_buffer(context, head, bytes) {
        return Ok(slot).into();
    }
    Ok(crate::block::allocate_admitted(
        context,
        payer,
        bytes,
        4096,
        crate::block::PLAIN,
    ))
    .into()
}

/// # Safety
/// Context is live and exclusively held by this task.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_alloc_file_buffer(
    context: *const Context,
    bytes: usize,
    charge: u64,
    overflow: bool,
) -> Allocation {
    allocate_file_bytes(unsafe { &*context }, None, bytes, charge, overflow)
}

/// # Safety
/// Context is live and task-exclusive. Head identifies the lent first byte
/// buffer, or is null for an empty carrier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_alloc_file_partner(
    context: *const Context,
    head: *mut u8,
    bytes: usize,
    charge: u64,
    overflow: bool,
) -> Allocation {
    allocate_file_bytes(
        unsafe { &*context },
        Some(head.addr()),
        bytes,
        charge,
        overflow,
    )
}
