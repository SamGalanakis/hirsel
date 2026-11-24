use super::{Captured, HEADER_BYTES, Options, socket_error};
use std::{
    cell::UnsafeCell,
    io,
    mem::MaybeUninit,
    net::SocketAddr,
    num::NonZeroU64,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::*, fence},
    },
};

#[derive(Clone, Copy)]
struct Header {
    peer: SocketAddr,
    length: usize,
    appended: usize,
    ecn: u8,
}

const _: () = assert!(size_of::<Header>() <= HEADER_BYTES && align_of::<Header>() <= 8);

struct Storage {
    _bytes: Box<[UnsafeCell<MaybeUninit<u64>>]>,
    base: NonNull<u8>,
    options: Options,
    read: AtomicU64,
    written: AtomicU64,
    waiter: AtomicU64,
}

// SAFETY: member_queue gives out exactly one producer and reader. Index publication
// gives the other endpoint access only after the preceding endpoint has finished.
unsafe impl Send for Storage {}
unsafe impl Sync for Storage {}

impl Storage {
    fn slot(&self, index: u64) -> *mut u8 {
        let index = index as usize & (self.options.slots() - 1);
        // SAFETY: the mask selects one allocation slot and base pins the array.
        unsafe { self.base.as_ptr().add(index * self.options.slot_bytes()) }
    }
}

pub struct MemberProducer {
    storage: Arc<Storage>,
}

pub struct MemberReader {
    storage: Arc<Storage>,
}

pub fn member_queue(options: Options) -> io::Result<(MemberProducer, MemberReader)> {
    let words = options.slots() * (options.slot_bytes() / 8);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(words)
        .map_err(|_| socket_error(libc::ENOMEM))?;
    bytes.resize_with(words, || UnsafeCell::new(MaybeUninit::uninit()));
    let mut bytes = bytes.into_boxed_slice();
    let base = NonNull::new(bytes.as_mut_ptr().cast()).unwrap();
    let storage = Arc::new(Storage {
        _bytes: bytes,
        base,
        options,
        read: AtomicU64::new(0),
        written: AtomicU64::new(0),
        waiter: AtomicU64::new(0),
    });
    Ok((
        MemberProducer {
            storage: storage.clone(),
        },
        MemberReader { storage },
    ))
}

impl MemberProducer {
    /// Publish one datagram into a free slot. A full queue keeps its existing prefix.
    pub fn push(&mut self, peer: SocketAddr, ecn: u8, length: usize, payload: &[u8]) -> bool {
        let s = &self.storage;
        assert!(payload.len() <= s.options.payload_room() && payload.len() <= length);
        let index = s.written.load(Relaxed);
        if index.wrapping_sub(s.read.load(Acquire)) == s.options.slots() as u64 {
            return false;
        }
        let slot = s.slot(index);
        // SAFETY: the writer owns the unpublished slot. The reader's acquire/release
        // index prevents overwriting a slot it can still read.
        unsafe {
            slot.cast::<Header>().write(Header {
                peer,
                length,
                appended: payload.len(),
                ecn,
            });
            std::ptr::copy_nonoverlapping(payload.as_ptr(), slot.add(HEADER_BYTES), payload.len());
        }
        s.written.store(index.wrapping_add(1), Release);
        true
    }

    pub fn notify(&self) -> Option<NonZeroU64> {
        fence(SeqCst);
        self.take_waiter()
    }

    pub(crate) fn take_waiter(&self) -> Option<NonZeroU64> {
        NonZeroU64::new(self.storage.waiter.swap(0, AcqRel))
    }
}

impl MemberReader {
    pub fn front(&self) -> Option<Captured<'_>> {
        let s = &self.storage;
        let index = s.read.load(Relaxed);
        if index == s.written.load(Acquire) {
            return None;
        }
        let slot = s.slot(index);
        // SAFETY: the producer published the header and payload. Advancing requires
        // exclusive access to this reader, so the producer cannot reclaim this loan.
        let (header, payload) = unsafe {
            let header = slot.cast::<Header>().read();
            let payload = std::slice::from_raw_parts(slot.add(HEADER_BYTES), header.appended);
            (header, payload)
        };
        Some(Captured::Datagram {
            peer: header.peer,
            ecn: header.ecn,
            length: header.length,
            payload,
        })
    }

    pub fn advance(&mut self) {
        assert!(self.front().is_some());
        let index = self.storage.read.load(Relaxed);
        self.storage.read.store(index.wrapping_add(1), Release);
    }

    pub fn publish_waiter(&mut self, token: NonZeroU64) {
        self.storage.waiter.store(token.get(), Release);
    }

    /// True once this reader can park. With a queued datagram the reader withdraws
    /// its word and retries. An already-posted wake remains safe to receive.
    pub fn recheck_waiter(&mut self) -> bool {
        fence(SeqCst);
        if self.front().is_none() {
            true
        } else {
            self.cancel_waiter();
            false
        }
    }

    pub fn cancel_waiter(&mut self) {
        self.storage.waiter.swap(0, AcqRel);
    }
}

#[derive(Default)]
struct Writer {
    waiter: AtomicU64,
    linked: AtomicBool,
    next: AtomicU32,
}

pub struct WriterList {
    head: AtomicU32,
    members: Box<[Writer]>,
}

impl WriterList {
    pub fn new(members: u16) -> Self {
        Self {
            head: AtomicU32::new(0),
            members: std::iter::repeat_with(Writer::default)
                .take(usize::from(members))
                .collect(),
        }
    }

    pub fn publish(&self, member: usize, token: NonZeroU64) {
        let writer = &self.members[member];
        writer.waiter.store(token.get(), Release);
        if !writer.linked.swap(true, AcqRel) {
            let tag = member as u32 + 1;
            let mut head = self.head.load(Relaxed);
            loop {
                writer.next.store(head, Relaxed);
                match self.head.compare_exchange_weak(head, tag, SeqCst, Relaxed) {
                    Ok(_) => break,
                    Err(current) => head = current,
                }
            }
        }
        fence(SeqCst);
    }

    pub fn cancel(&self, member: usize) {
        self.members[member].waiter.swap(0, AcqRel);
    }

    pub fn take(&self) -> WriterBatch<'_> {
        fence(SeqCst);
        self.resume(self.head.swap(0, AcqRel))
    }

    pub(crate) fn resume(&self, next: u32) -> WriterBatch<'_> {
        WriterBatch { list: self, next }
    }
}

pub struct WriterBatch<'a> {
    list: &'a WriterList,
    next: u32,
}

impl WriterBatch<'_> {
    pub(crate) fn cursor(&self) -> u32 {
        self.next
    }
}

impl Iterator for WriterBatch<'_> {
    /// Some(None) is a cancelled waiter. The caller budgets each visited node.
    type Item = Option<NonZeroU64>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.next.checked_sub(1)? as usize;
        let writer = &self.list.members[index];
        self.next = writer.next.load(Relaxed);
        writer.linked.store(false, Release);
        Some(NonZeroU64::new(writer.waiter.swap(0, AcqRel)))
    }
}
