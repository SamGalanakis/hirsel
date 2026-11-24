use crate::{Context, provider::Buffer};
use hirsel_core_net::argon2id::{self, Error, Plan};

#[repr(C)]
#[derive(Default)]
pub struct Argon2Outcome {
    requested: u64,
    available: u64,
    error: u8,
    status: u8,
}

impl Argon2Outcome {
    fn params() -> Self {
        Self {
            error: 1,
            ..Self::default()
        }
    }

    fn capacity(requested: u64, available: u64) -> Self {
        Self {
            requested,
            available,
            error: 2,
            status: 0,
        }
    }
}

/// Append a tag in the caller's spare capacity.
///
/// # Safety
/// Context is the exclusively running task's context. The four input ranges
/// contain initialized, immutable bytes for the call, including at step slice
/// boundaries. An empty range may have a null pointer. The exclusive output
/// carrier and its writable spare are valid and disjoint from the inputs.
#[allow(clippy::too_many_arguments)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_argon2id(
    context: *mut Context,
    password: *const u8,
    password_len: u64,
    salt: *const u8,
    salt_len: u64,
    key: *const u8,
    key_len: u64,
    data: *const u8,
    data_len: u64,
    memory_kib: u32,
    passes: u32,
    lanes: u32,
    len: u64,
    out: *mut Buffer,
    site: u64,
) -> Argon2Outcome {
    if !argon2id::valid_input_lengths([password_len, salt_len, key_len, data_len]) {
        return Argon2Outcome::params();
    }
    let Ok(plan) = Plan::new(memory_kib, passes, lanes, len) else {
        return Argon2Outcome::params();
    };
    // SAFETY: FORM lends an exclusive live Bytes header throughout the call.
    let out = unsafe { &mut *out };
    let spare = out.capacity.saturating_sub(out.len) as u64;
    if len > spare {
        return Argon2Outcome::capacity(len, spare);
    }
    // SAFETY: the executing task owns context and this call's source site.
    if !unsafe { crate::steps::take_many(context, plan.steps(), site) } {
        return Argon2Outcome {
            status: 1,
            ..Argon2Outcome::default()
        };
    }
    // SAFETY: the current task still owns context after the step checkpoint.
    let payer = unsafe { (*context).payer() };
    let charge = plan.memory_bytes();
    if let Err(c) = payer.admit(Some(charge)) {
        return Argon2Outcome::capacity(c.requested, c.available);
    }
    let range = |ptr, len| {
        if len == 0 {
            &[]
        } else {
            // SAFETY: nonempty input ranges are valid and read-only for the call.
            unsafe { std::slice::from_raw_parts(ptr, len as usize) }
        }
    };
    let len = len as usize;
    // SAFETY: the proved spare is exclusive, possibly uninitialized storage.
    let tag = unsafe {
        let tag = out.data.add(out.len);
        tag.write_bytes(0, len);
        std::slice::from_raw_parts_mut(tag, len)
    };
    let result = plan.hash(
        range(password, password_len),
        range(salt, salt_len),
        range(key, key_len),
        range(data, data_len),
        tag,
    );
    payer.refund(charge);
    match result {
        Ok(()) => {
            out.len += len;
            Argon2Outcome::default()
        }
        Err(Error::Params) => Argon2Outcome::params(),
        Err(Error::HostMemory) => crate::process::stop(b"HostMemory"),
    }
}
