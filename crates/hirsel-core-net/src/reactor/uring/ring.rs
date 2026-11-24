//! One io_uring instance whose ring memory the runtime provides.
//!
//! The kernel's own ring allocation (`io_uring_setup` without `NO_MMAP`) wants
//! physically contiguous pages: a 65,536-entry CQ is 257 pages, so it asks for
//! an order-9 block and fails with `ENOMEM` once fragmentation has used those
//! up, with no memory cap reached. `IORING_SETUP_NO_MMAP` makes the kernel pin
//! and map pages from two ordinary anonymous mappings instead, so setup needs
//! only order-0 pages. `NO_SQARRAY` drops the SQ index array: the kernel reads
//! SQEs in ring order.
//!
//! Only the owner of a ring pushes submissions and pops completions; the
//! kernel is the other side of each queue.
use io_uring::{cqueue, squeue};
use std::{
    io, mem,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    ptr,
    sync::atomic::{AtomicU32, Ordering},
};

// Values from include/uapi/linux/io_uring.h.
const SETUP_CQSIZE: u32 = 1 << 3;
const SETUP_CLAMP: u32 = 1 << 4;
const SETUP_COOP_TASKRUN: u32 = 1 << 8;
const SETUP_TASKRUN_FLAG: u32 = 1 << 9;
const SETUP_NO_MMAP: u32 = 1 << 14;
const SETUP_NO_SQARRAY: u32 = 1 << 16;
const FEAT_NODROP: u32 = 1 << 1;
const FEAT_EXT_ARG: u32 = 1 << 8;
const SQ_CQ_OVERFLOW: u32 = 1 << 1;
const SQ_TASKRUN: u32 = 1 << 2;
const ENTER_GETEVENTS: u32 = 1 << 0;
const ENTER_EXT_ARG: u32 = 1 << 3;
const REGISTER_PROBE: u32 = 8;
const REGISTER_BUFFERS2: u32 = 15;
const REGISTER_BUFFERS_UPDATE: u32 = 16;
const REGISTER_IOWQ_MAX_WORKERS: u32 = 19;
const RSRC_REGISTER_SPARSE: u32 = 1 << 0;
const OP_SUPPORTED: u16 = 1 << 0;
const PROBE_OPS: usize = 256;

const SQE_BYTES: usize = 64;
const CQE_BYTES: usize = 16;
const _: () = assert!(mem::size_of::<squeue::Entry>() == SQE_BYTES);
const _: () = assert!(mem::size_of::<cqueue::Entry>() == CQE_BYTES);

#[repr(C)]
#[derive(Default)]
struct SqOffsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    flags: u32,
    dropped: u32,
    array: u32,
    resv1: u32,
    user_addr: u64,
}

#[repr(C)]
#[derive(Default)]
struct CqOffsets {
    head: u32,
    tail: u32,
    ring_mask: u32,
    ring_entries: u32,
    overflow: u32,
    cqes: u32,
    flags: u32,
    resv1: u32,
    user_addr: u64,
}

#[repr(C)]
#[derive(Default)]
struct Params {
    sq_entries: u32,
    cq_entries: u32,
    flags: u32,
    sq_thread_cpu: u32,
    sq_thread_idle: u32,
    features: u32,
    wq_fd: u32,
    resv: [u32; 3],
    sq_off: SqOffsets,
    cq_off: CqOffsets,
}
const _: () = assert!(mem::size_of::<Params>() == 120);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ProbeOp {
    op: u8,
    resv: u8,
    flags: u16,
    resv2: u32,
}

#[repr(C)]
struct Probe {
    last_op: u8,
    ops_len: u8,
    resv: u16,
    resv2: [u32; 3],
    ops: [ProbeOp; PROBE_OPS],
}

#[repr(C)]
#[derive(Default)]
struct RsrcRegister {
    nr: u32,
    flags: u32,
    resv2: u64,
    data: u64,
    tags: u64,
}

#[repr(C)]
#[derive(Default)]
struct RsrcUpdate2 {
    offset: u32,
    resv: u32,
    data: u64,
    tags: u64,
    nr: u32,
    resv2: u32,
}

#[repr(C)]
struct KernelTimespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[repr(C)]
struct GetEventsArg {
    sigmask: u64,
    sigmask_sz: u32,
    min_wait_usec: u32,
    ts: u64,
}

fn result(rc: libc::c_long) -> io::Result<i32> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc as i32)
    }
}

/// A private anonymous mapping of order-0 pages, not inherited by children.
struct Mapping {
    base: *mut u8,
    len: usize,
}

impl Mapping {
    fn new(len: usize) -> io::Result<Self> {
        // SAFETY: a fresh anonymous mapping aliases nothing.
        let base = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let mapping = Mapping {
            base: base.cast(),
            len,
        };
        // A fork would otherwise copy the pinned ring pages into the child.
        // SAFETY: the range is this mapping.
        if unsafe { libc::madvise(base, len, libc::MADV_DONTFORK) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(mapping)
    }

    /// `count` values of `T` at byte `offset`, checked to lie inside the mapping.
    fn at<T>(&self, offset: u32, count: usize) -> io::Result<*mut T> {
        let end = (offset as usize).checked_add(count * mem::size_of::<T>());
        if end.is_none_or(|end| end > self.len) || offset as usize % mem::align_of::<T>() != 0 {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        // SAFETY: the range lies inside the mapping.
        Ok(unsafe { self.base.add(offset as usize) }.cast())
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the mapping is ours; the kernel keeps its own pin on the pages.
        unsafe { libc::munmap(self.base.cast(), self.len) };
    }
}

pub(in crate::reactor) struct Ring {
    /// Declared first so the ring closes before its memory is unmapped.
    fd: OwnedFd,
    _rings: Mapping,
    _sqes: Mapping,
    sq_head: *const AtomicU32,
    sq_tail: *const AtomicU32,
    sq_flags: *const AtomicU32,
    sq_mask: u32,
    sq_entries: u32,
    sqes: *mut squeue::Entry,
    cq_head: *const AtomicU32,
    cq_tail: *const AtomicU32,
    cq_dropped: *const AtomicU32,
    cq_mask: u32,
    cq_entries: u32,
    cqes: *const cqueue::Entry,
}

// SAFETY: the pointers name this ring's own mappings; the owner protocol
// (one SQ writer, one CQ reader) is the caller's.
unsafe impl Send for Ring {}
// SAFETY: as above.
unsafe impl Sync for Ring {}

impl Ring {
    /// A `COOP_TASKRUN | TASKRUN_FLAG` ring of `sq` submissions and `cq`
    /// completions (`clamp`: cut sizes past the kernel's maximum). A kernel
    /// without NODROP or EXT_ARG is refused as unsupported.
    pub(in crate::reactor) fn new(sq: u32, cq: u32, clamp: bool) -> io::Result<Self> {
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let round = |bytes: usize| bytes.div_ceil(page) * page;
        // The kernel puts the ring header (64 bytes on Linux 7.0) before the
        // CQEs; one page covers it.
        let rings = Mapping::new(round(cq.next_power_of_two() as usize * CQE_BYTES) + page)?;
        let sqes = Mapping::new(round(sq.next_power_of_two() as usize * SQE_BYTES))?;
        let mut p = Params {
            cq_entries: cq,
            flags: SETUP_CQSIZE
                | SETUP_COOP_TASKRUN
                | SETUP_TASKRUN_FLAG
                | SETUP_NO_MMAP
                | SETUP_NO_SQARRAY
                | if clamp { SETUP_CLAMP } else { 0 },
            ..Params::default()
        };
        p.cq_off.user_addr = rings.base as u64;
        p.sq_off.user_addr = sqes.base as u64;
        // SAFETY: the kernel reads and fills `p` and pins both mappings.
        let fd = result(unsafe { libc::syscall(libc::SYS_io_uring_setup, sq, &raw mut p) })?;
        // SAFETY: setup returned a fresh descriptor that we now own.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if p.features & (FEAT_NODROP | FEAT_EXT_ARG) != FEAT_NODROP | FEAT_EXT_ARG {
            return Err(io::Error::from(io::ErrorKind::Unsupported));
        }
        let s = &p.sq_off;
        let c = &p.cq_off;
        // SAFETY: the kernel has initialised both masks in the pinned header.
        let (sq_mask, cq_mask) = unsafe {
            (
                *rings.at::<u32>(s.ring_mask, 1)?,
                *rings.at::<u32>(c.ring_mask, 1)?,
            )
        };
        Ok(Ring {
            sq_head: rings.at(s.head, 1)?,
            sq_tail: rings.at(s.tail, 1)?,
            sq_flags: rings.at(s.flags, 1)?,
            sq_mask,
            sq_entries: p.sq_entries,
            sqes: sqes.at(0, p.sq_entries as usize)?,
            cq_head: rings.at(c.head, 1)?,
            cq_tail: rings.at(c.tail, 1)?,
            cq_dropped: rings.at(c.overflow, 1)?,
            cq_mask,
            cq_entries: p.cq_entries,
            cqes: rings.at(c.cqes, p.cq_entries as usize)?,
            fd,
            _rings: rings,
            _sqes: sqes,
        })
    }

    pub(in crate::reactor) fn cq_entries(&self) -> usize {
        self.cq_entries as usize
    }

    fn load(field: *const AtomicU32, order: Ordering) -> u32 {
        // SAFETY: every field pointer names an aligned u32 in the pinned header.
        unsafe { &*field }.load(order)
    }

    fn store(field: *const AtomicU32, value: u32) {
        // SAFETY: as in `load`.
        unsafe { &*field }.store(value, Ordering::Release);
    }

    /// Add `entry` to the SQ; false when the kernel has not consumed room.
    ///
    /// # Safety
    /// Only the owner pushes. Every descriptor and buffer `entry` names stays
    /// valid until its completion.
    pub(in crate::reactor) unsafe fn push(&self, entry: &squeue::Entry) -> bool {
        let tail = Self::load(self.sq_tail, Ordering::Relaxed);
        if tail.wrapping_sub(Self::load(self.sq_head, Ordering::Acquire)) == self.sq_entries {
            return false;
        }
        // SAFETY: the slot is inside the SQE mapping and the kernel has consumed it.
        unsafe {
            self.sqes
                .add((tail & self.sq_mask) as usize)
                .write(entry.clone())
        };
        Self::store(self.sq_tail, tail.wrapping_add(1));
        true
    }

    /// Submissions the kernel has not consumed yet.
    pub(in crate::reactor) fn sq_len(&self) -> usize {
        Self::load(self.sq_tail, Ordering::Relaxed)
            .wrapping_sub(Self::load(self.sq_head, Ordering::Acquire)) as usize
    }

    fn sq_flags(&self) -> u32 {
        Self::load(self.sq_flags, Ordering::Acquire)
    }

    /// The kernel queued task work for the owner (`IORING_SQ_TASKRUN`).
    pub(in crate::reactor) fn taskrun(&self) -> bool {
        self.sq_flags() & SQ_TASKRUN != 0
    }

    /// The kernel holds completions past the CQ (NODROP) until an enter
    /// with GETEVENTS flushes them.
    pub(in crate::reactor) fn overflowed(&self) -> bool {
        self.sq_flags() & SQ_CQ_OVERFLOW != 0
    }

    /// Completions the kernel dropped for want of room.
    pub(in crate::reactor) fn dropped(&self) -> u32 {
        Self::load(self.cq_dropped, Ordering::Acquire)
    }

    /// Posted completions not yet taken.
    pub(in crate::reactor) fn cq_len(&self) -> usize {
        Self::load(self.cq_tail, Ordering::Acquire)
            .wrapping_sub(Self::load(self.cq_head, Ordering::Relaxed)) as usize
    }

    /// Take the oldest posted completion.
    ///
    /// # Safety
    /// Only the owner takes completions.
    pub(in crate::reactor) unsafe fn next(&self) -> Option<cqueue::Entry> {
        let head = Self::load(self.cq_head, Ordering::Relaxed);
        if head == Self::load(self.cq_tail, Ordering::Acquire) {
            return None;
        }
        // SAFETY: the kernel published this CQE before its tail store.
        let cqe = unsafe { self.cqes.add((head & self.cq_mask) as usize).read() };
        Self::store(self.cq_head, head.wrapping_add(1));
        Some(cqe)
    }

    /// Submit the SQ and wait for `want` completions, at most `timeout_ns`
    /// (None: no bound). An overflowed CQ or pending task work is flushed.
    pub(in crate::reactor) fn submit(
        &self,
        want: u32,
        timeout_ns: Option<u64>,
    ) -> io::Result<usize> {
        let mut flags = 0;
        if want > 0 || self.sq_flags() & (SQ_CQ_OVERFLOW | SQ_TASKRUN) != 0 {
            flags |= ENTER_GETEVENTS;
        }
        let ts = timeout_ns.map(|ns| KernelTimespec {
            tv_sec: (ns / 1_000_000_000) as i64,
            tv_nsec: (ns % 1_000_000_000) as i64,
        });
        let arg = ts.as_ref().map(|ts| GetEventsArg {
            sigmask: 0,
            sigmask_sz: 0,
            min_wait_usec: 0,
            ts: ptr::from_ref(ts) as u64,
        });
        let (arg, size) = match &arg {
            Some(arg) => {
                flags |= ENTER_EXT_ARG;
                (ptr::from_ref(arg).cast(), mem::size_of::<GetEventsArg>())
            }
            None => (ptr::null::<libc::c_void>(), 0),
        };
        // SAFETY: `arg` and its timespec outlive the call.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_io_uring_enter,
                self.fd.as_raw_fd(),
                self.sq_len() as u32,
                want,
                flags,
                arg,
                size,
            )
        };
        result(rc).map(|n| n as usize)
    }

    fn register(&self, op: u32, arg: *const libc::c_void, nr: u32) -> io::Result<i32> {
        // SAFETY: each caller passes the argument layout `op` defines.
        result(unsafe {
            libc::syscall(
                libc::SYS_io_uring_register,
                self.fd.as_raw_fd(),
                op,
                arg,
                nr,
            )
        })
    }

    /// Whether the kernel supports every opcode in `ops`.
    pub(in crate::reactor) fn supports(&self, ops: &[u8]) -> io::Result<bool> {
        let mut probe = Probe {
            last_op: 0,
            ops_len: 0,
            resv: 0,
            resv2: [0; 3],
            ops: [ProbeOp::default(); PROBE_OPS],
        };
        self.register(REGISTER_PROBE, (&raw mut probe).cast(), PROBE_OPS as u32)?;
        Ok(ops
            .iter()
            .all(|&op| op <= probe.last_op && probe.ops[op as usize].flags & OP_SUPPORTED != 0))
    }

    /// Cap the ring's io-wq at `bounded` and `unbounded` workers.
    pub(in crate::reactor) fn iowq_max_workers(
        &self,
        bounded: u32,
        unbounded: u32,
    ) -> io::Result<()> {
        let mut max = [bounded, unbounded];
        self.register(REGISTER_IOWQ_MAX_WORKERS, max.as_mut_ptr().cast(), 2)
            .map(drop)
    }

    /// Register `nr` empty fixed-buffer slots.
    pub(in crate::reactor) fn buffers_sparse(&self, nr: u32) -> io::Result<()> {
        let rr = RsrcRegister {
            nr,
            flags: RSRC_REGISTER_SPARSE,
            ..RsrcRegister::default()
        };
        self.register(
            REGISTER_BUFFERS2,
            ptr::from_ref(&rr).cast(),
            mem::size_of::<RsrcRegister>() as u32,
        )
        .map(drop)
    }

    /// Point fixed-buffer slot `index` at `iov` (empty: no buffer).
    ///
    /// # Safety
    /// The memory `iov` names stays valid until the slot is replaced or the
    /// ring is gone.
    pub(in crate::reactor) unsafe fn update_buffer(
        &self,
        index: u32,
        iov: libc::iovec,
    ) -> io::Result<()> {
        let update = RsrcUpdate2 {
            offset: index,
            data: ptr::from_ref(&iov) as u64,
            nr: 1,
            ..RsrcUpdate2::default()
        };
        self.register(
            REGISTER_BUFFERS_UPDATE,
            ptr::from_ref(&update).cast(),
            mem::size_of::<RsrcUpdate2>() as u32,
        )
        .map(drop)
    }
}

impl AsRawFd for Ring {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}
