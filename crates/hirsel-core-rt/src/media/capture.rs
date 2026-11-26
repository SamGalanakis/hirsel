//! Capture one synchronous opaque media operation at the unit's replay point.
use crate::{
    Context,
    provider::{Buffer, MEDIA_CAPTURE, Outcome, Provider, Request, STEP_DONE, Step},
};
use hirsel_tape_format::{Args, ERROR, OK, Out, put_varint};

#[derive(Clone, Copy)]
pub(crate) enum Delivery {
    Child {
        // The refusals, as (tag, code), that are deliveries: a length `Invalid`, and for a
        // send SFrame import the registry's `Spent`. Each consumes the secret.
        refusals: &'static [(u8, u64)],
    },
    Scalar {
        max: u64,
    },
    Packet {
        value: u64,
        bytes: usize,
        // The adapter derives the permitted reason from the pinned input:
        // 1 for short input or 2 for a tag mismatch.
        tampered: Option<u32>,
    },
}

pub(crate) struct Capture<'a> {
    args: Args,
    delivery: Delivery,
    live: &'a mut dyn FnMut(&mut Buffer) -> Outcome,
}

pub(crate) const PROVIDER: Provider = Provider {
    issue: issue_live,
    ..crate::secret::MAC_PROVIDER
};

unsafe extern "C" fn issue_live(_: *mut u8, request: *mut Request, _: u64) -> Step {
    // SAFETY: the adapter pins the exclusive request and callback through delivery.
    let request = unsafe { &mut *request };
    let call = unsafe { &mut *request.path.cast_mut().cast::<Capture<'_>>() };
    Step {
        kind: STEP_DONE,
        outcome: (call.live)(&mut request.buffer),
        deadline_ns: 0,
    }
}

/// # Safety
/// The context and carrier stay live through delivery. The operation pins
/// its public inputs, and the callback only writes the carrier's spare.
pub(crate) unsafe fn issue(
    context: *mut Context,
    operation: u8,
    args: Args,
    delivery: Delivery,
    output: &mut Buffer,
    mut live: impl FnMut(&mut Buffer) -> Outcome,
    site: u64,
) -> Outcome {
    let mut call = Capture {
        args,
        delivery,
        live: &mut live,
    };
    let mut request = Request {
        operation: MEDIA_CAPTURE,
        mode: operation,
        handle: 0,
        path: (&raw mut call).cast(),
        path_len: size_of::<Capture<'_>>(),
        buffer: *output,
        site,
    };
    // SAFETY: all operands and the synchronous closure live through the call.
    let result = unsafe { crate::secret::drive_secret_sink(context, PROVIDER, &mut request) };
    *output = request.buffer;
    result
}

/// Hash public inputs before any overlapping destination can be mutated.
pub(crate) fn arguments(operation: u8, spare: u64, inputs: &[&[u8]]) -> Args {
    let mut head = vec![operation];
    put_varint(&mut head, spare);
    for part in inputs {
        put_varint(&mut head, part.len() as u64);
    }
    let mut parts = Vec::with_capacity(inputs.len() + 1);
    parts.push(head.as_slice());
    parts.extend_from_slice(inputs);
    Args::of_parts(&parts)
}

/// # Safety
/// The operation is MEDIA_CAPTURE and the adapter still pins its descriptor.
pub(crate) unsafe fn canonical(request: &Request) -> Args {
    let call = unsafe { &*request.path.cast::<Capture<'_>>() };
    let output = request.buffer.capacity - request.buffer.len;
    call.args.clone().buffered([output as u64, 0])
}

/// # Safety
/// The operation is MEDIA_CAPTURE with a live descriptor.
pub(crate) unsafe fn creates_child(request: &Request) -> bool {
    let call = unsafe { &*request.path.cast::<Capture<'_>>() };
    matches!(call.delivery, Delivery::Child { .. })
}

/// Validate the operation's complete delivery before replay touches any caller storage.
/// # Safety
/// The operation is MEDIA_CAPTURE with a live descriptor.
pub(crate) unsafe fn valid(request: &Request, answer: &Out, event: u64) -> bool {
    let call = unsafe { &*request.path.cast::<Capture<'_>>() };
    match (call.delivery, answer.status) {
        (Delivery::Child { .. }, OK) => answer.value == event && answer.payload.is_empty(),
        (Delivery::Scalar { max }, OK) => answer.value <= max && answer.payload.is_empty(),
        (Delivery::Child { refusals }, ERROR) => {
            answer.value == 0 && refusals.contains(&answer.error) && answer.payload.is_empty()
        }
        (
            Delivery::Packet {
                value,
                bytes,
                tampered,
            },
            OK,
        ) => tampered != Some(1) && answer.value == value && answer.payload.len() == bytes,
        (
            Delivery::Packet {
                tampered: Some(reason),
                ..
            },
            ERROR,
        ) => {
            answer.value == 0 && answer.error == (4, u64::from(reason)) && answer.payload.is_empty()
        }
        _ => false,
    }
}
