//! FAIL-17: the glue worklist (`hirsel-core-ir` crate docs, "Glue worklist").
//!
//! The equality, ordering and hash glue of a recursive type keeps each place it
//! has to come back to as one four-word entry here, never as a frame, so it uses
//! constant stack on a value of any depth. The worklist is engine bookkeeping:
//! it admits nothing and never fails; the host refusing to grow it is the run
//! stop `HostMemory`.
use crate::Context;

/// The worklist depth: the mark a walk truncates back to.
///
/// # Safety
/// `context` is the running task's context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_glue_mark(context: *mut Context) -> u64 {
    // SAFETY: the engine supplies the exclusively running task's context.
    unsafe { (*context).glue.len() as u64 }
}

/// Push the entry `(a, b, resume, index)`.
///
/// # Safety
/// `context` is the running task's context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_glue_push(
    context: *mut Context,
    a: *const u8,
    b: *const u8,
    resume: u64,
    index: u64,
) {
    // SAFETY: the engine supplies the exclusively running task's context.
    let glue = unsafe { &mut (*context).glue };
    if glue.len() == glue.capacity() && glue.try_reserve(glue.len().max(64)).is_err() {
        crate::run_stop::raise(crate::run_stop::Stop::from_static(b"HostMemory"));
    }
    glue.push([a as u64, b as u64, resume, index]);
}

/// Pop the top entry above `mark`: a pointer to its four words, valid until
/// the next push, or null when the worklist is at `mark`.
///
/// # Safety
/// `context` is the running task's context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_glue_pop(context: *mut Context, mark: u64) -> *const u64 {
    // SAFETY: the engine supplies the exclusively running task's context.
    let glue = unsafe { &mut (*context).glue };
    if glue.len() as u64 <= mark {
        return std::ptr::null();
    }
    let len = glue.len() - 1;
    // SAFETY: `len` is in bounds; the entry's words stay in the buffer until a
    // later push overwrites them.
    unsafe {
        glue.set_len(len);
        glue.as_ptr().add(len).cast()
    }
}

/// Truncate the worklist to `mark`.
///
/// # Safety
/// `context` is the running task's context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hirsel_core_glue_cut(context: *mut Context, mark: u64) {
    // SAFETY: the engine supplies the exclusively running task's context.
    let glue = unsafe { &mut (*context).glue };
    glue.truncate(mark as usize);
}
