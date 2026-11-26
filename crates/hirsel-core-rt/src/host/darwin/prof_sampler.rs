//! CPU sampling of this process's own threads for `hirsel prof run` on
//! macOS, where no perf counter reaches another process without
//! `task_for_pid`. Each round suspends one running thread at a time, reads
//! its registers and walks its frame-pointer chain (the arm64 Darwin ABI
//! keeps one in every frame), then resumes it before anything is recorded.
//! While a thread is suspended the sampler neither allocates nor locks: the
//! thread may hold the allocator's or the profile's locks.
use std::io;

use crate::prof::Stack;

type Port = u32;
type KernReturn = i32;

const THREAD_BASIC_INFO: u32 = 3;
const THREAD_BASIC_INFO_COUNT: u32 = 10;
/// `run_state` and `flags` in `thread_basic_info`, in 32-bit words.
const RUN_STATE: usize = 6;
const FLAGS: usize = 7;
const TH_STATE_RUNNING: i32 = 1;
const TH_FLAGS_IDLE: i32 = 2;
const THREAD_IDENTIFIER_INFO: u32 = 4;
const THREAD_IDENTIFIER_INFO_COUNT: u32 = 6;
const ARM_THREAD_STATE64: i32 = 6;
const ARM_THREAD_STATE64_COUNT: u32 = 68;
/// `fp`, `sp` and `pc` in `arm_thread_state64_t`, in 64-bit words.
const FP: usize = 29;
const PC: usize = 32;
/// Darwin user addresses fit in 47 bits; the bits above may carry a pointer
/// authentication code in a return address an arm64e library saved.
const ADDRESS: u64 = (1 << 47) - 1;

unsafe extern "C" {
    static mach_task_self_: Port;
    fn task_threads(task: Port, threads: *mut *mut Port, count: *mut u32) -> KernReturn;
    fn thread_info(thread: Port, flavor: u32, info: *mut i32, count: *mut u32) -> KernReturn;
    fn thread_suspend(thread: Port) -> KernReturn;
    fn thread_resume(thread: Port) -> KernReturn;
    fn thread_get_state(thread: Port, flavor: i32, state: *mut u32, count: *mut u32) -> KernReturn;
    fn mach_vm_read_overwrite(
        task: Port,
        address: u64,
        size: u64,
        data: u64,
        out: *mut u64,
    ) -> KernReturn;
    fn mach_port_deallocate(task: Port, name: Port) -> KernReturn;
    fn vm_deallocate(task: Port, address: usize, size: usize) -> KernReturn;
}

pub(crate) struct Sampler {
    own: u64,
}

impl Sampler {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            own: super::sys::thread_id(),
        })
    }

    /// Walk every other running thread once and pass each stack to `emit`
    /// after its thread resumed.
    pub(crate) fn round(&mut self, stack: &mut Stack, emit: &mut dyn FnMut(&Stack)) {
        // SAFETY: mach_task_self_ is the task's own port, set before main.
        let task = unsafe { mach_task_self_ };
        let mut threads: *mut Port = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: the kernel writes an array of send rights it allocated in
        // this task; both outputs are live.
        if unsafe { task_threads(task, &mut threads, &mut count) } != 0 {
            return;
        }
        // SAFETY: task_threads returned `count` ports at `threads`.
        let ports = unsafe { std::slice::from_raw_parts(threads, count as usize) };
        for &thread in ports {
            if let Some(tid) = running(thread)
                && tid != self.own
                && walk(task, thread, stack)
            {
                stack.tid = tid;
                emit(stack);
            }
            // SAFETY: each port is a send right task_threads gave us.
            unsafe { mach_port_deallocate(task, thread) };
        }
        // SAFETY: the array is the kernel's allocation of `count` ports.
        unsafe { vm_deallocate(task, threads.addr(), count as usize * size_of::<Port>()) };
    }
}

/// The thread's id when it is running and not idle.
fn running(thread: Port) -> Option<u64> {
    let mut basic = [0i32; THREAD_BASIC_INFO_COUNT as usize];
    let mut count = THREAD_BASIC_INFO_COUNT;
    // SAFETY: basic holds THREAD_BASIC_INFO_COUNT words.
    if unsafe { thread_info(thread, THREAD_BASIC_INFO, basic.as_mut_ptr(), &mut count) } != 0
        || basic[RUN_STATE] != TH_STATE_RUNNING
        || basic[FLAGS] & TH_FLAGS_IDLE != 0
    {
        return None;
    }
    let mut identifier = [0u64; 3];
    let mut count = THREAD_IDENTIFIER_INFO_COUNT;
    // SAFETY: identifier holds THREAD_IDENTIFIER_INFO_COUNT 32-bit words.
    let rc = unsafe {
        thread_info(
            thread,
            THREAD_IDENTIFIER_INFO,
            identifier.as_mut_ptr().cast(),
            &mut count,
        )
    };
    (rc == 0).then_some(identifier[0])
}

/// Suspend `thread`, record its PC and frame-pointer chain in `stack`, and
/// resume it. Nothing here allocates or locks.
fn walk(task: Port, thread: Port, stack: &mut Stack) -> bool {
    // SAFETY: a send right to a thread of this task.
    if unsafe { thread_suspend(thread) } != 0 {
        return false;
    }
    let mut state = [0u64; ARM_THREAD_STATE64_COUNT as usize / 2];
    let mut count = ARM_THREAD_STATE64_COUNT;
    // SAFETY: state holds ARM_THREAD_STATE64_COUNT 32-bit words.
    let read = unsafe {
        thread_get_state(
            thread,
            ARM_THREAD_STATE64,
            state.as_mut_ptr().cast(),
            &mut count,
        )
    } == 0;
    stack.len = 0;
    stack.truncated = false;
    if read {
        stack.pcs[0] = state[PC] & ADDRESS;
        stack.len = 1;
        let mut fp = state[FP] & ADDRESS;
        // Each frame record is the caller's frame pointer, then the return
        // address. A chain that ends, leaves 16-byte alignment or stops
        // growing toward the stack base ends the walk.
        while fp != 0 && fp.is_multiple_of(16) {
            let mut record = [0u64; 2];
            let mut out = 0u64;
            // SAFETY: the kernel copies 16 bytes into record or fails on an
            // unmapped address; nothing here dereferences fp.
            let rc = unsafe {
                mach_vm_read_overwrite(task, fp, 16, record.as_mut_ptr().addr() as u64, &mut out)
            };
            let (next, ret) = (record[0] & ADDRESS, record[1] & ADDRESS);
            if rc != 0 || out != 16 || ret == 0 {
                break;
            }
            if stack.len == stack.pcs.len() {
                stack.truncated = true;
                break;
            }
            stack.pcs[stack.len] = ret;
            stack.len += 1;
            if next <= fp {
                break;
            }
            fp = next;
        }
    }
    // SAFETY: this sampler suspended the thread above.
    unsafe { thread_resume(thread) };
    read
}
