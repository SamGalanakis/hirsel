//! macOS arm64: a `MAP_JIT` text band that one loader thread writes through its
//! per-thread write window, so no other thread ever sees the band writable (PACK-5).

use super::Access;
use std::cell::Cell;
use std::io;
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};

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

// Registration and the publisher's snapshot share a lock. Each executor writes
// only its own gate on entry/exit. The gate remains closed through return from JIT.
#[repr(align(128))]
struct Gate(Mutex<()>);

static EXECUTORS: Mutex<Vec<Weak<Gate>>> = Mutex::new(Vec::new());
thread_local! {
    static GATE: Arc<Gate> = {
        let gate = Arc::new(Gate(Mutex::new(())));
        EXECUTORS.lock().unwrap().push(Arc::downgrade(&gate));
        gate
    };
    static ENTERED: Cell<bool> = const { Cell::new(false) };
}

fn isb() {
    // SAFETY: ISB synchronizes this executor's instruction stream.
    unsafe { std::arch::asm!("isb", options(nostack, preserves_flags)) };
}

struct Exit;
impl Drop for Exit {
    fn drop(&mut self) {
        isb();
        ENTERED.set(false);
    }
}

pub(crate) fn execute<R>(work: impl FnOnce() -> R) -> R {
    if ENTERED.get() {
        return work();
    }
    GATE.with(|gate| {
        let _active = gate.0.lock().unwrap_or_else(|p| p.into_inner());
        isb();
        ENTERED.set(true);
        let _exit = Exit;
        work()
    })
}

/// After the text run at `at` was written: make every core fetch the new bytes.
pub(crate) fn publish(_writer: usize, at: usize, bytes: usize) -> io::Result<()> {
    // SAFETY: the range is mapped.
    unsafe { sys_icache_invalidate(at as *mut libc::c_void, bytes) };
    // New registrations synchronize through the registry lock. For old executors,
    // a running interval must finish its exit ISB. Every subsequent entry takes
    // its gate and executes ISB before transferring control back to slab code.
    let active: Vec<_> = {
        let mut registry = EXECUTORS.lock().unwrap();
        let active = registry.iter().filter_map(Weak::upgrade).collect();
        registry.retain(|gate| gate.strong_count() != 0);
        active
    };
    #[cfg(test)]
    BEFORE_ACK.with(|hook| {
        if let Some(ready) = hook.take() {
            ready.send(()).unwrap();
        }
    });
    for gate in active {
        drop(gate.0.lock().unwrap_or_else(|p| p.into_inner()));
    }
    Ok(())
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

#[cfg(test)]
thread_local! {
    pub(crate) static BEFORE_ACK: Cell<Option<mpsc::Sender<()>>> = const { Cell::new(None) };
}
