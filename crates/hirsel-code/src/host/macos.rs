//! macOS arm64: a `MAP_JIT` text band that one loader thread writes through its
//! per-thread write window, so no other thread ever sees the band writable (PACK-5).

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
pub(crate) fn reserve(bytes: usize) -> io::Result<usize> {
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT;
    // SAFETY: a fresh anonymous mapping at an address the kernel picks.
    let at = unsafe { libc::mmap(std::ptr::null_mut(), bytes, libc::PROT_NONE, flags, -1, 0) };
    if at != libc::MAP_FAILED {
        return Ok(at as usize);
    }
    failed("mmap MAP_JIT PROT_NONE");
    // Some kernels give a `MAP_JIT` range only with its full protection.
    let all = libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC;
    // SAFETY: as above; the range is closed before anything can use it.
    unsafe {
        let at = libc::mmap(std::ptr::null_mut(), bytes, all, flags, -1, 0);
        if at == libc::MAP_FAILED {
            return Err(failed("mmap MAP_JIT rwx"));
        }
        if libc::mprotect(at, bytes, libc::PROT_NONE) != 0 {
            let error = failed("mprotect MAP_JIT none");
            libc::munmap(at, bytes);
            return Err(error);
        }
        Ok(at as usize)
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
        // A `MAP_JIT` page is writable to a thread whose write window is open and
        // executable to every other.
        Access::Patch => libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        Access::Execute => return Ok(()),
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

pub(crate) const KEEPS: bool = true;

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

/// After the text run at `at` was written: make every core fetch the new bytes.
pub(crate) fn publish(at: usize, bytes: usize) -> io::Result<()> {
    // SAFETY: the range is mapped.
    unsafe { sys_icache_invalidate(at as *mut libc::c_void, bytes) };
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
pub(crate) const FAULT: i32 = libc::SIGBUS;

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
pub(crate) fn pc(_info: *mut libc::siginfo_t, context: *mut libc::c_void) -> usize {
    // SAFETY: the kernel gave the handler this context.
    unsafe {
        (*(*context.cast::<libc::ucontext_t>()).uc_mcontext)
            .__ss
            .__pc as usize
    }
}
