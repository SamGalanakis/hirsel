//! FORM data_decode. A task owns its uncommitted decode image across step
//! checkpoints and task-heap allocations, either of which can abandon a fiber.
use crate::{Context, block, causes, steps};
use hirsel_data::{DataError, ErrorKind, Limits, guest::Plan, image::Image};
use std::ptr::NonNull;

#[derive(Default)]
pub(super) struct Pending {
    scratch: u64,
    image: Image,
}

pub(super) fn discard(cx: &mut Context) {
    let Some(pending) = cx.decode.take() else {
        return;
    };
    for b in pending.image.blocks.into_iter().skip(1) {
        if let Some(p) = NonNull::new(b.address as *mut u8) {
            // SAFETY: the block has never been published. Its children
            // are separate entries in the same allocation journal.
            unsafe {
                block::free_in(cx, p, b.charge);
            }
        }
    }
    if pending.scratch != 0 {
        cx.payer().refund(pending.scratch);
    }
}

unsafe fn borrowed<'a>(header: *const crate::provider::Buffer) -> &'a [u8] {
    // SAFETY: FORM lends an initialized, pinned sequence header and its range.
    unsafe {
        if (*header).len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts((*header).data, (*header).len)
        }
    }
}

/// # Safety
/// FORM supplies live borrowed Witness and Bytes headers, the exact writable
/// result slot described by the witness, and the exclusively running context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_data_decode(
    cx: *mut Context,
    witness: *const crate::provider::Buffer,
    bytes: *const crate::provider::Buffer,
    input: u64,
    depth: u64,
    nodes: u64,
    collection: u64,
    decoded: u64,
    comparison: u64,
    diagnostics: u64,
    output: *mut u8,
    site: u64,
) -> u8 {
    let limits = Limits::from([
        input,
        depth,
        nodes,
        collection,
        decoded,
        comparison,
        diagnostics,
    ]);
    // SAFETY: both headers and their buffers remain borrowed across any slice boundary.
    let (witness, bytes) = unsafe { (borrowed(witness), borrowed(bytes)) };
    let work = hirsel_data::guest::steps(witness.len(), bytes.len(), limits);
    // SAFETY: debit on the task fiber, before any host-stack frame or live scratch.
    if !unsafe { steps::take_many(cx, work, site) } {
        return 1;
    }
    // SAFETY: FORM gives the task exclusive context access through this operation.
    let context = unsafe { &mut *cx };
    context.decode = Some(Pending::default());
    let payer = context.payer();
    let key = context.map_strong_key;
    // SAFETY: preparation borrows the inputs synchronously. It admits scratch
    // without scheduling. The only data returned on the stack is a status.
    let result = unsafe {
        crate::tasks::on_host_stack(
            false,
            #[cfg(feature = "prof")]
            site,
            || {
                let mut pay = |n| {
                    payer.admit(Some(n)).map_err(|c| {
                        DataError::resource(ErrorKind::Capacity {
                            requested: c.requested,
                            available: c.available,
                        })
                    })?;
                    context.decode.as_mut().unwrap().scratch += n;
                    Ok(())
                };
                pay(hirsel_data::guest::witness_charge(witness.len()))?;
                let (schema, plan) = Plan::from_compiler(witness)?;
                let image = Image::prepare(&schema, &plan, bytes, limits, key, &mut pay)?;
                context.decode.as_mut().unwrap().image = image;
                Ok::<_, DataError>(())
            },
        )
    };
    if let Err(error) = result {
        discard(context);
        return refusal(context, error.kind, site);
    }
    let count = context.decode.as_ref().unwrap().image.blocks.len();
    for i in 1..count {
        let block_image = &context.decode.as_ref().unwrap().image.blocks[i];
        let charge = block_image.charge;
        let (size, align, graph) = (
            block_image.bytes.len(),
            block_image.align as usize,
            block_image.graph,
        );
        if let Err(c) = block::admit_storage(payer, Some(charge)) {
            discard(context);
            return refusal(
                context,
                ErrorKind::Capacity {
                    requested: c.requested,
                    available: c.available,
                },
                site,
            );
        }
        // Scratch keeps the admission refundable if the heap parks this task
        // for an extent and its generation ends before it receives the block.
        context.decode.as_mut().unwrap().scratch += charge;
        let p = block::allocate_admitted(
            context,
            payer,
            size,
            align,
            if graph { block::GRAPH } else { block::PLAIN },
        );
        context.decode.as_mut().unwrap().scratch -= charge;
        let blocks = &mut context.decode.as_mut().unwrap().image.blocks;
        blocks[i].address = p.as_ptr() as u64;
        let (parent, off) = blocks[i].parent.unwrap();
        blocks[parent].bytes[off..off + 8].copy_from_slice(&(p.as_ptr() as u64).to_le_bytes());
    }
    let pending = context.decode.take().unwrap();
    let image = pending.image;
    for (i, b) in image.blocks.into_iter().enumerate().rev() {
        let dest = if i == 0 { output } else { b.address as *mut u8 };
        // SAFETY: every block was admitted at precisely this size. The image
        // and destination are disjoint, and children are initialized first.
        unsafe {
            std::ptr::copy_nonoverlapping(b.bytes.as_ptr(), dest, b.bytes.len());
        }
    }
    payer.refund(pending.scratch);
    0
}

fn refusal(cx: &mut Context, kind: ErrorKind, site: u64) -> u8 {
    match kind {
        ErrorKind::HostMemory => {
            crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostMemory"))
        }
        ErrorKind::Capacity {
            requested,
            available,
        } => {
            // SAFETY: the allocating FORM call owns its prepaid failure cell.
            unsafe {
                causes::record_capacity(
                    cx,
                    crate::Capacity {
                        requested,
                        available,
                    },
                    site,
                );
            }
            1
        }
        _ => unreachable!("document errors are guest values"),
    }
}
