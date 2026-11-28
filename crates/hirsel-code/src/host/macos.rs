//! macOS arm64: a `MAP_JIT` text band that one loader thread writes through its
//! per-thread write window, so no other thread ever sees the band writable, and a
//! publication that takes every other thread through the kernel (PACK-5).

use super::Access;
use std::io;
use std::sync::{Mutex, OnceLock, mpsc};

pub(crate) const HOST_TARGET: hirsel_core_layout::Target =
    hirsel_core_layout::Target::Aarch64Darwin;

unsafe extern "C" {
    fn pthread_jit_write_protect_supported_np() -> libc::c_int;
    fn pthread_jit_write_protect_np(enabled: libc::c_int);
    fn sys_icache_invalidate(start: *mut libc::c_void, len: usize);
}

pub(crate) fn page() -> usize {
    // SAFETY: sysconf reads one integer.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

/// Say why a host call failed, in the laws' output.
fn failed(what: &str) -> io::Error {
    let error = io::Error::last_os_error();
    #[cfg(test)]
    eprintln!("hirsel-code: {what}: {error}");
    let _ = what;
    error
}

pub(crate) fn register() -> io::Result<()> {
    // SAFETY: the call reads one fact about the process.
    match unsafe { pthread_jit_write_protect_supported_np() } {
        0 => {
            #[cfg(test)]
            eprintln!("hirsel-code: this process has no JIT write window");
            Err(io::ErrorKind::Unsupported.into())
        }
        _ => Ok(()),
    }
}

/// Reserve `bytes` as one `MAP_JIT` range: a `MAP_JIT` mapping cannot be placed at a
/// chosen address, so the data band is part of it and is never mapped executable.
pub(crate) fn reserve(bytes: usize) -> io::Result<(usize, usize)> {
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT;
    // SAFETY: fresh reservation. Per-thread JIT protection applies to the text half.
    unsafe {
        let at = libc::mmap(
            std::ptr::null_mut(),
            2 * bytes,
            libc::PROT_NONE,
            flags,
            -1,
            0,
        );
        let at = if at == libc::MAP_FAILED {
            // Some Darwin kernels require full initial protection for MAP_JIT.
            let at = libc::mmap(
                std::ptr::null_mut(),
                2 * bytes,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                flags,
                -1,
                0,
            );
            if at == libc::MAP_FAILED {
                return Err(failed("mmap MAP_JIT"));
            }
            if libc::mprotect(at.byte_add(bytes), bytes, libc::PROT_NONE) != 0 {
                let error = failed("close data");
                libc::munmap(at, 2 * bytes);
                return Err(error);
            }
            at
        } else {
            if libc::mprotect(
                at,
                bytes,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
            ) != 0
            {
                let error = failed("map text");
                libc::munmap(at, 2 * bytes);
                return Err(error);
            }
            at
        };
        Ok((at as usize, at as usize))
    }
}

pub(crate) fn release(at: usize, bytes: usize) {
    // SAFETY: the caller reserved exactly this range and nothing uses it now.
    unsafe { libc::munmap(at as *mut libc::c_void, bytes) };
}

pub(crate) fn protect(at: usize, bytes: usize, access: Access) -> io::Result<()> {
    let protection = match access {
        Access::None => libc::PROT_NONE,
        Access::Write => libc::PROT_READ | libc::PROT_WRITE,
        Access::Read => libc::PROT_READ,
    };
    // SAFETY: the range is inside the caller's reservation.
    match unsafe { libc::mprotect(at as *mut libc::c_void, bytes, protection) } {
        0 => Ok(()),
        _ => Err(failed("mprotect")),
    }
}

/// Give the pages of a range back. They may keep their bytes.
pub(crate) fn discard(at: usize, bytes: usize) {
    // SAFETY: the range is inside the caller's reservation and holds no live image.
    unsafe { libc::madvise(at as *mut libc::c_void, bytes, libc::MADV_FREE) };
}

/// How many mappings `[low, high)` is in, and how many of them allow no access. This
/// host does not count them.
pub(crate) fn mappings(_rx: usize, _rw: usize, _bytes: usize) -> Option<(usize, usize)> {
    None
}

struct Job(*mut (dyn FnMut() + 'static));
// SAFETY: the sender waits for the job to end, so the closure outlives its one use.
unsafe impl Send for Job {}

type Jobs = mpsc::Sender<(Job, mpsc::SyncSender<()>)>;
static LOADER: OnceLock<Mutex<Jobs>> = OnceLock::new();

/// Run `work`, which writes text runs mapped for `Access::Patch`, on the loader thread
/// with its write window open, and wait for it.
pub(crate) fn write(work: &mut dyn FnMut()) {
    let jobs = LOADER.get_or_init(|| {
        let (jobs, queued) = mpsc::channel::<(Job, mpsc::SyncSender<()>)>();
        let run = move || {
            for (job, done) in queued {
                // SAFETY: the window is this thread's own, and the job's sender keeps
                // the closure alive until `done` is answered.
                unsafe {
                    pthread_jit_write_protect_np(0);
                    (*job.0)();
                    pthread_jit_write_protect_np(1);
                }
                let _ = done.send(());
            }
        };
        std::thread::Builder::new()
            .name("hirsel-code-loader".into())
            .spawn(run)
            .expect("the loader thread");
        Mutex::new(jobs)
    });
    let (done, ended) = mpsc::sync_channel(1);
    let work: *mut (dyn FnMut() + '_) = work;
    // SAFETY: only the lifetime changes, and this call returns after the job ended.
    let job = Job(unsafe {
        std::mem::transmute::<*mut (dyn FnMut() + '_), *mut (dyn FnMut() + 'static)>(work)
    });
    jobs.lock()
        .unwrap()
        .send((job, done))
        .expect("the loader thread");
    ended.recv().expect("the loader thread");
}

/// Take every other thread of this task through the kernel. The kernel reads a
/// thread's state only once the thread is off its core, and the thread comes back to
/// user code through an exception return: each is a context synchronization event, so
/// no thread goes on running instructions it fetched before the invalidation.
pub(crate) fn synchronize() -> io::Result<()> {
    const KERN_INVALID_ARGUMENT: i32 = 4;
    const KERN_TERMINATED: i32 = 37;
    const MACH_SEND_INVALID_DEST: i32 = 0x1000_0003;
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn task_threads(task: u32, threads: *mut *mut u32, count: *mut u32) -> i32;
        fn mach_port_deallocate(task: u32, name: u32) -> i32;
        fn vm_deallocate(task: u32, address: usize, bytes: usize) -> i32;
    }
    let (mut threads, mut count) = (std::ptr::null_mut::<u32>(), 0u32);
    // SAFETY: the call fills both locals, and the array and each right in it are this
    // caller's to give back.
    unsafe {
        let task = mach_task_self_;
        let listed = task_threads(task, &mut threads, &mut count);
        if listed != 0 {
            return Err(refused("task_threads", listed));
        }
        let me = libc::pthread_mach_thread_np(libc::pthread_self());
        let mut result = Ok(());
        for thread in std::slice::from_raw_parts(threads, count as usize) {
            if *thread != me && result.is_ok() {
                result = match stopped(*thread) {
                    // The last three are a thread that is exiting, which runs no more
                    // user code: inactive, its port disabled (the kernel then names no
                    // thread), or its port gone.
                    0 | KERN_TERMINATED | KERN_INVALID_ARGUMENT | MACH_SEND_INVALID_DEST => Ok(()),
                    code => Err(refused("thread_get_state", code)),
                };
            }
            mach_port_deallocate(task, *thread);
        }
        vm_deallocate(task, threads.addr(), count as usize * size_of::<u32>());
        result
    }
}

/// Have the kernel stop `thread` off its core and let it go on: the kernel's answer.
pub(crate) fn stopped(thread: u32) -> i32 {
    /// `ARM_THREAD_STATE64` and the words of its `arm_thread_state64_t`.
    const FLAVOR: i32 = 6;
    const WORDS: usize = 68;
    const KERN_ABORTED: i32 = 14;
    unsafe extern "C" {
        fn thread_get_state(thread: u32, flavor: i32, state: *mut u32, count: *mut u32) -> i32;
    }
    let mut state = [0u32; WORDS];
    loop {
        let mut words = WORDS as u32;
        // SAFETY: the kernel writes at most `words` words of `state`.
        let answer = unsafe { thread_get_state(thread, FLAVOR, state.as_mut_ptr(), &mut words) };
        // A signal to this thread interrupted its wait for the stop.
        if answer != KERN_ABORTED {
            return answer;
        }
    }
}

/// A Mach call's refusal, said in the laws' output.
fn refused(what: &str, code: i32) -> io::Error {
    #[cfg(test)]
    eprintln!("hirsel-code: {what}: kern_return {code:#x}");
    io::Error::other(format!("{what}: kern_return {code:#x}"))
}

/// After the text run at `at` was written: make every core fetch the new bytes.
pub(crate) fn publish(_writer: usize, at: usize, bytes: usize) -> io::Result<()> {
    // SAFETY: the range is mapped, and ISB synchronizes this thread's own stream.
    unsafe {
        sys_icache_invalidate(at as *mut libc::c_void, bytes);
        std::arch::asm!("isb", options(nostack, preserves_flags));
    }
    synchronize()
}

/// Whether this process can have a slab; the laws of one run only where it can.
#[cfg(test)]
pub(crate) fn slab() -> bool {
    register().is_ok()
}
#[cfg(test)]
pub(crate) const LIVE: &str = "rwxp";
#[cfg(test)]
pub(crate) const OPEN: &str = "rwxp";
#[cfg(test)]
pub(crate) const FAULT: i32 = libc::SIGILL;

/// The protection letters of the region that holds `at`, as the kernel reports them.
#[cfg(test)]
pub(crate) fn protection(at: usize) -> String {
    /// `vm_region_basic_info_64`, which is packed to four bytes.
    #[repr(C, packed(4))]
    #[derive(Default)]
    struct Basic {
        protection: i32,
        max_protection: i32,
        inheritance: u32,
        shared: u32,
        reserved: u32,
        offset: u64,
        behavior: i32,
        user_wired_count: u16,
    }
    const VM_REGION_BASIC_INFO_64: i32 = 9;
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn mach_vm_region(
            task: u32,
            address: *mut u64,
            size: *mut u64,
            flavor: i32,
            info: *mut Basic,
            count: *mut u32,
            object: *mut u32,
        ) -> i32;
    }
    let (mut address, mut size, mut object) = (at as u64, 0u64, 0u32);
    let mut info = Basic::default();
    let mut count = (size_of::<Basic>() / 4) as u32;
    // SAFETY: every pointer is to a local of the type the call fills.
    let result = unsafe {
        mach_vm_region(
            mach_task_self_,
            &mut address,
            &mut size,
            VM_REGION_BASIC_INFO_64,
            &mut info,
            &mut count,
            &mut object,
        )
    };
    let bits = info.protection;
    if result != 0 || address > at as u64 {
        return String::new();
    }
    let letter = |bit: i32, letter: char| if bits & bit != 0 { letter } else { '-' };
    [letter(1, 'r'), letter(2, 'w'), letter(4, 'x'), 'p']
        .iter()
        .collect()
}

/// The faulting code address a signal handler was given.
#[cfg(test)]
pub(crate) fn pc(
    _signal: libc::c_int,
    _info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) -> usize {
    // SAFETY: the kernel gave the handler this context.
    unsafe {
        (*(*context.cast::<libc::ucontext_t>()).uc_mcontext)
            .__ss
            .__pc as usize
    }
}
