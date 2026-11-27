//! File SQEs have one issuer. A unit lends its request to the issuer until
//! its callback. Requests past the ring depth wait on their own parked stacks.
use super::ring::{Mapping, Ring};
use io_uring::{opcode, squeue, types};
use std::{
    cell::{Cell, RefCell},
    io,
    os::fd::{AsRawFd, OwnedFd, RawFd},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, ThreadId},
};

/// Two page-aligned, independently writable group buffers. Each issuer
/// keeps the backing mappings alive while its kernel registration exists.
pub struct FileBuffers(Arc<Buffers>);

struct Buffers {
    maps: [Mapping; 2],
    owned: AtomicBool,
}

impl Drop for FileBuffers {
    fn drop(&mut self) {
        self.0.owned.store(false, Ordering::Release);
    }
}

impl FileBuffers {
    pub fn new(bytes_per_buffer: usize) -> io::Result<Self> {
        Ok(Self(Arc::new(Buffers {
            maps: [
                Mapping::new(bytes_per_buffer)?,
                Mapping::new(bytes_per_buffer)?,
            ],
            owned: AtomicBool::new(true),
        })))
    }

    pub fn buffer(&mut self, index: usize) -> &mut [u8] {
        let mapping = &self.0.maps[index];
        // SAFETY: FileBuffers is the only writer. Issuer references keep the
        // mappings alive and only inspect their addresses. Writes borrow
        // the bytes until their CQE.
        unsafe { std::slice::from_raw_parts_mut(mapping.base, mapping.len) }
    }
}

pub struct FileRequest {
    entry: squeue::Entry,
    context: *const (),
    complete: unsafe fn(*const (), i32),
    next: Cell<*const Self>,
}

impl FileRequest {
    pub fn new(
        entry: squeue::Entry,
        context: *const (),
        complete: unsafe fn(*const (), i32),
    ) -> Self {
        Self {
            entry,
            context,
            complete,
            next: Cell::new(ptr::null()),
        }
    }

    /// # Safety
    /// The request and its callback context remain pinned through this call.
    pub(super) unsafe fn finish(request: *const Self, result: i32) {
        let (complete, context) = unsafe { ((*request).complete, (*request).context) };
        // SAFETY: the issuer releases all references before publishing.
        unsafe { complete(context, result) };
    }
}

struct State {
    slots: Box<[*const FileRequest]>,
    free: Vec<usize>,
    first: *const FileRequest,
    last: *const FileRequest,
    registered: Option<Arc<Buffers>>,
}

pub struct FileRing {
    ring: Ring,
    owner: ThreadId,
    state: RefCell<State>,
}

// SAFETY: only the creating thread can borrow state or touch the SQ/CQ.
// Other threads may observe readiness through the kernel ring flags.
unsafe impl Sync for FileRing {}
unsafe impl Send for FileRing {}

impl FileRing {
    pub fn new(depth: u32) -> io::Result<Self> {
        Self::with_workers(depth, 2, 1)
    }

    pub(super) fn with_workers(depth: u32, bounded: u32, unbounded: u32) -> io::Result<Self> {
        if depth == 0 || depth > 32768 {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let ring = Ring::file(depth)?;
        ring.iowq_max_workers(bounded, unbounded)?;
        let slots = vec![ptr::null(); depth as usize].into_boxed_slice();
        Ok(Self {
            ring,
            owner: thread::current().id(),
            state: RefCell::new(State {
                free: (0..slots.len()).rev().collect(),
                slots,
                first: ptr::null(),
                last: ptr::null(),
                registered: None,
            }),
        })
    }

    fn check_owner(&self) {
        assert_eq!(
            thread::current().id(),
            self.owner,
            "file ring issuer changed"
        );
    }

    /// DEFER_TASKRUN can leave the CQ empty until the issuer enters.
    /// Linux signals a registered eventfd as soon as it queues local work.
    pub(super) fn work_event(&self) -> io::Result<OwnedFd> {
        let fd = super::super::sys::eventfd()?;
        self.ring.notify_via(fd.as_raw_fd())?;
        Ok(fd)
    }

    pub(super) fn due(&self) -> bool {
        self.ring.sq_len() != 0
            || self.ring.cq_len() != 0
            || self.ring.taskrun()
            || self.ring.overflowed()
    }

    /// # Safety
    /// The issuing owner pins the request, context, descriptor and all SQE
    /// operands until its sole completion. A request can be queued only once
    /// at a time. Its callback may neither unwind nor reenter this ring.
    pub unsafe fn queue(&self, request: &FileRequest) {
        self.check_owner();
        let mut state = self.state.borrow_mut();
        let node = ptr::from_ref(request);
        if state.last.is_null() {
            state.first = node;
        } else {
            // SAFETY: a waiting request is still pinned and only the issuer links it.
            unsafe { (*state.last).next.set(node) };
        }
        state.last = node;
        self.publish(&mut state);
    }

    fn publish(&self, state: &mut State) {
        while !state.first.is_null() {
            let Some(&slot) = state.free.last() else {
                break;
            };
            // SAFETY: queue pins the request until its callback.
            let request = unsafe { &*state.first };
            let entry = request.entry.clone().user_data(slot as u64);
            // SAFETY: only this issuer touches the SQ; the caller pins all operands.
            if !unsafe { self.ring.push(&entry) } {
                break;
            }
            state.free.pop();
            state.slots[slot] = state.first;
            state.first = request.next.replace(ptr::null());
            if state.first.is_null() {
                state.last = ptr::null();
            }
        }
    }

    /// Submit queued entries and dispatch CQEs in kernel order. A tied
    /// caller can wait here. A pool worker waits on its readiness descriptor.
    pub fn drive(&self, wait: bool) -> io::Result<usize> {
        self.check_owner();
        let mut state = self.state.borrow_mut();
        if state.free.len() == state.slots.len() {
            if state
                .registered
                .as_ref()
                .is_some_and(|b| !b.owned.load(Ordering::Acquire))
            {
                self.ring.unregister_file_buffers()?;
                state.registered = None;
            }
            return Ok(0);
        }
        if self.ring.dropped() != 0 {
            return Err(io::Error::from_raw_os_error(libc::ENOMEM));
        }
        if self.ring.cq_len() == 0 && (wait || self.due()) {
            match self.ring.submit(u32::from(wait), None) {
                Ok(_) => {}
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::EINTR | libc::EBUSY | libc::EAGAIN)
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        let mut completed = 0;
        // At most one slabful is dispatched per turn before sockets run.
        // SAFETY: this issuer is the sole reader of the CQ.
        while let Some(cqe) = unsafe { self.ring.next() } {
            let slot = cqe.user_data() as usize;
            let node = std::mem::replace(&mut state.slots[slot], ptr::null());
            state.free.push(slot);
            completed += 1;
            // SAFETY: release the slot before the callback wakes its owner.
            unsafe { FileRequest::finish(node, cqe.result()) };
        }
        self.publish(&mut state);
        Ok(completed)
    }

    /// Choose WRITE_FIXED for a borrowed prefix (including a short-write
    /// remainder) of either registered buffer, and WRITE for ordinary bytes.
    pub fn write_entry(&self, fd: RawFd, bytes: &[u8], offset: u64) -> squeue::Entry {
        self.check_owner();
        let addr = bytes.as_ptr().addr();
        let len = u32::try_from(bytes.len()).expect("file write exceeds SQE length");
        if let Some(buffers) = &self.state.borrow().registered
            && let Some(slot) = buffers.maps.iter().position(|mapping| {
                let start = mapping.base.addr();
                addr.checked_sub(start).is_some_and(|offset| {
                    offset < mapping.len && bytes.len() <= mapping.len - offset
                })
            })
        {
            return opcode::WriteFixed::new(types::Fd(fd), bytes.as_ptr(), len, slot as u16)
                .offset(offset)
                .build();
        }
        opcode::Write::new(types::Fd(fd), bytes.as_ptr(), len)
            .offset(offset)
            .build()
    }

    /// Select this pair on the current issuer. Repeated writes use the same
    /// registration. A migrating writer can select its pair on the new issuer.
    pub fn register_buffers(&self, buffers: &FileBuffers) -> io::Result<()> {
        self.check_owner();
        let mut state = self.state.borrow_mut();
        if let Some(current) = &state.registered {
            if Arc::ptr_eq(current, &buffers.0) {
                return Ok(());
            }
            // Queued WRITE_FIXED entries still name the old slot indices.
            if state.free.len() != state.slots.len() {
                return Err(io::Error::from_raw_os_error(libc::EBUSY));
            }
            self.ring.unregister_file_buffers()?;
            state.registered = None;
        }
        let iov = buffers.0.maps.each_ref().map(|m| libc::iovec {
            iov_base: m.base.cast(),
            iov_len: m.len,
        });
        // SAFETY: the issuer retains these mappings until replacement or close.
        unsafe { self.ring.register_file_buffers(&iov) }?;
        state.registered = Some(buffers.0.clone());
        Ok(())
    }

    pub fn drain(&self) -> io::Result<()> {
        self.check_owner();
        while {
            let state = self.state.borrow();
            state.free.len() != state.slots.len()
        } {
            self.drive(true)?;
        }
        let mut state = self.state.borrow_mut();
        if state.registered.is_some() {
            self.ring.unregister_file_buffers()?;
            state.registered = None;
        }
        Ok(())
    }
}

impl Drop for FileRing {
    fn drop(&mut self) {
        let state = self.state.get_mut();
        if state.free.len() != state.slots.len() && self.drain().is_err() {
            std::process::abort();
        }
    }
}
