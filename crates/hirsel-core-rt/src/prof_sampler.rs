//! macOS CPU samples for the runtime observation stream. Linux samples a run
//! from outside with perf events; macOS lets a process read only its own
//! threads, so a runtime thread samples the others: at each tick it suspends
//! each running thread in turn, reads its registers and walks its frame
//! records (the arm64 macOS ABI keeps the frame pointer), then resumes it.
//! While a thread is suspended the sampler only makes Mach calls into
//! preallocated memory: the thread may hold the allocator's locks.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::prof::{Kind, Worker, append};

/// The deepest stack a sample keeps.
const DEPTH: usize = 128;
/// Code addresses: arm64e return addresses carry a pointer signature above.
const ADDRESS: u64 = 0x0000_7fff_ffff_ffff;

type Port = u32;

#[repr(C)]
#[derive(Default)]
struct ThreadState {
    x: [u64; 29],
    fp: u64,
    lr: u64,
    sp: u64,
    pc: u64,
    cpsr: u32,
    flags: u32,
}

#[repr(C)]
#[derive(Default)]
struct BasicInfo {
    user_time: [i32; 2],
    system_time: [i32; 2],
    cpu_usage: i32,
    policy: i32,
    run_state: i32,
    flags: i32,
    suspend_count: i32,
    sleep_time: i32,
}

const ARM_THREAD_STATE64: i32 = 6;
const THREAD_BASIC_INFO: u32 = 3;
const TH_STATE_RUNNING: i32 = 1;

unsafe extern "C" {
    /// The task's own port, which `mach_task_self()` reads.
    static mach_task_self_: Port;
    fn task_threads(task: Port, list: *mut *mut Port, count: *mut u32) -> i32;
    fn thread_suspend(thread: Port) -> i32;
    fn thread_resume(thread: Port) -> i32;
    fn thread_get_state(thread: Port, flavor: i32, state: *mut u32, count: *mut u32) -> i32;
    fn thread_info(thread: Port, flavor: u32, info: *mut i32, count: *mut u32) -> i32;
    fn mach_thread_self() -> Port;
    fn mach_port_deallocate(task: Port, name: Port) -> i32;
    fn vm_deallocate(task: Port, address: usize, size: usize) -> i32;
    fn vm_read_overwrite(
        task: Port,
        address: usize,
        size: usize,
        data: usize,
        out: *mut usize,
    ) -> i32;
}

pub(crate) struct Sampler {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Sampler {
    /// Start sampling every running thread `freq` times a second, when
    /// `HIRSEL_PROFILE_CPU` asks for it.
    pub(crate) fn start() -> Option<Self> {
        let freq = std::env::var("HIRSEL_PROFILE_CPU").ok()?;
        // SAFETY: called from the profile's start at process entry.
        unsafe { std::env::remove_var("HIRSEL_PROFILE_CPU") };
        let freq = freq
            .parse::<u64>()
            .ok()
            .filter(|f| (1..=10_000).contains(f))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::Builder::new()
            .name("hirsel-sampler".into())
            .spawn(move || {
                let _worker = Worker::enter();
                sample(&stopping, Duration::from_nanos(1_000_000_000 / freq));
            })
            .ok()?;
        Some(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn sample(stop: &AtomicBool, period: Duration) {
    // SAFETY: these return this task's and this thread's own ports.
    let (task, me) = unsafe { (mach_task_self_, mach_thread_self()) };
    let mut named = std::collections::HashSet::new();
    let mut words = vec![0u64; DEPTH + 2];
    while !stop.load(Ordering::Acquire) {
        thread::sleep(period);
        let mut list: *mut Port = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: the kernel allocates the list; it is deallocated below.
        if unsafe { task_threads(task, &mut list, &mut count) } != 0 {
            continue;
        }
        // SAFETY: task_threads returned `count` ports at `list`.
        let threads = unsafe { std::slice::from_raw_parts(list, count as usize) };
        for &thread in threads {
            if thread != me && running(thread) {
                if named.insert(thread) {
                    name(thread);
                }
                if let Some(depth) = stack(task, thread, &mut words[1..]) {
                    words[0] = u64::from(thread);
                    append(Kind::CpuSample, &words[..=depth], &[]);
                }
            }
            // SAFETY: each listed port is a send right this task now holds.
            unsafe { mach_port_deallocate(task, thread) };
        }
        // SAFETY: the list task_threads allocated, by its exact size.
        unsafe {
            vm_deallocate(task, list as usize, count as usize * size_of::<Port>());
        }
    }
}

fn running(thread: Port) -> bool {
    let mut info = BasicInfo::default();
    let mut count = (size_of::<BasicInfo>() / 4) as u32;
    // SAFETY: the info buffer holds `count` integers.
    unsafe {
        thread_info(
            thread,
            THREAD_BASIC_INFO,
            (&raw mut info).cast(),
            &mut count,
        ) == 0
            && info.run_state == TH_STATE_RUNNING
            && info.suspend_count == 0
    }
}

/// Record the thread's name once, keyed by its port as samples are.
fn name(thread: Port) {
    // SAFETY: a null result names no thread; the buffer bounds the copy.
    unsafe {
        let pthread = libc::pthread_from_mach_thread_np(thread);
        if pthread == 0 {
            return;
        }
        let mut name = [0u8; 64];
        if libc::pthread_getname_np(pthread, name.as_mut_ptr().cast(), name.len()) == 0 {
            let len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
            append(Kind::ThreadName, &[u64::from(thread)], &name[..len]);
        }
    }
}

/// The thread's program counter and return addresses, innermost first.
fn stack(task: Port, thread: Port, out: &mut [u64]) -> Option<usize> {
    let mut state = ThreadState::default();
    let mut count = (size_of::<ThreadState>() / 4) as u32;
    // SAFETY: suspension stops the thread only; every call below reads into
    // memory owned here, and the thread is resumed on every path.
    unsafe {
        if thread_suspend(thread) != 0 {
            return None;
        }
        if thread_get_state(
            thread,
            ARM_THREAD_STATE64,
            (&raw mut state).cast(),
            &mut count,
        ) != 0
        {
            thread_resume(thread);
            return None;
        }
        out[0] = state.pc & ADDRESS;
        let mut depth = 1;
        let mut fp = state.fp;
        while depth < out.len() && fp != 0 && fp % 8 == 0 {
            let mut record = [0u64; 2];
            let mut read = 0usize;
            // A frame record is the caller's frame pointer, then the return
            // address. The read fails rather than faults on a bad pointer.
            if vm_read_overwrite(
                task,
                fp as usize,
                16,
                record.as_mut_ptr() as usize,
                &mut read,
            ) != 0
                || read != 16
            {
                break;
            }
            let ret = record[1] & ADDRESS;
            if ret == 0 {
                break;
            }
            out[depth] = ret;
            depth += 1;
            if record[0] <= fp {
                break;
            }
            fp = record[0];
        }
        thread_resume(thread);
        Some(depth)
    }
}

/// The loaded images as `/proc/self/maps`-style lines: each image's
/// `__TEXT` range, its file offset 0 and its path.
pub(crate) fn mappings() -> Vec<Vec<u8>> {
    unsafe extern "C" {
        fn _dyld_image_count() -> u32;
        fn _dyld_get_image_header(index: u32) -> *const u8;
        fn _dyld_get_image_name(index: u32) -> *const libc::c_char;
    }
    let mut lines = Vec::new();
    // SAFETY: dyld's image list is stable for each index while it is read,
    // and each header's load commands are mapped with it.
    unsafe {
        for index in 0.._dyld_image_count() {
            let header = _dyld_get_image_header(index);
            let name = _dyld_get_image_name(index);
            if header.is_null() || name.is_null() {
                continue;
            }
            let Some(size) = text_size(header) else {
                continue;
            };
            let start = header as u64;
            let mut line =
                format!("{start:x}-{:x} r-xp 00000000 00:00 0 ", start + size).into_bytes();
            line.extend_from_slice(std::ffi::CStr::from_ptr(name).to_bytes());
            lines.push(line);
        }
    }
    lines
}

/// The `__TEXT` segment's size in a mapped 64-bit Mach-O header.
///
/// # Safety
/// `header` is a mapped image's header with its load commands.
unsafe fn text_size(header: *const u8) -> Option<u64> {
    let read32 = |at: *const u8| unsafe { at.cast::<u32>().read_unaligned() };
    let read64 = |at: *const u8| unsafe { at.cast::<u64>().read_unaligned() };
    if read32(header) != 0xfeed_facf {
        return None;
    }
    let count = read32(unsafe { header.add(16) });
    let mut command = unsafe { header.add(32) };
    for _ in 0..count {
        let (cmd, size) = (read32(command), read32(unsafe { command.add(4) }));
        // LC_SEGMENT_64 named __TEXT.
        if cmd == 0x19
            && unsafe { std::slice::from_raw_parts(command.add(8), 16) }.starts_with(b"__TEXT\0")
        {
            return Some(read64(unsafe { command.add(32) }));
        }
        command = unsafe { command.add(size as usize) };
    }
    None
}
