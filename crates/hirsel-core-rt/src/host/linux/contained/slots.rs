//! FFI-13 slot reuse: the contained engine's linear memories and fiber
//! stacks come from bounded idle caches. A slot that ends goes back to its
//! cache only after a reset that leaves it as a fresh `mmap` would be: every
//! page it made accessible is replaced with fresh zero pages under
//! `PROT_NONE`. A miss maps a fresh slot as Wasmtime's own on-demand
//! allocator does, so the cache bound limits only what is held idle, never
//! how many instances are live.
//!
//! The layout is Wasmtime 48.0.10's `MmapMemory` (`runtime/vm/memory/mmap.rs`)
//! with the engine's tunables: a pre-guard (`guard_before_linear_memory`,
//! on by default) of the guard size, the reservation, then the guard. Compiled
//! code elides bounds checks against exactly that reservation and guard
//! (`MemoryCreator::new_memory`'s contract), which every slot keeps: the base
//! never moves and nothing past the accessible bytes is ever mapped.
use hirsel_core_contained::wasmtime::{
    Error, LinearMemory, MemoryCreator, MemoryType, StackCreator, StackMemory,
};
use std::ops::Range;
use std::sync::Mutex;

/// The engine's fiber stack size (`async_stack_size`), which bounds what an
/// idle stack keeps resident.
const STACK_BYTES: u64 = 2 << 20;

/// At most this many idle slots of each kind, 4 TiB of idle reservations.
const MOST_IDLE: u64 = 1024;

fn page() -> usize {
    // SAFETY: sysconf has no preconditions.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize }
}

fn round_up(bytes: usize, page: usize) -> usize {
    bytes.div_ceil(page) * page
}

/// A mapping's start and length; idle while held by a cache.
struct Mapping(*mut u8, usize);

// SAFETY: a mapping is owned by exactly one holder at a time, and shared
// references only read its address and length.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    /// A fresh `PROT_NONE` private anonymous mapping, as Wasmtime's
    /// `Mmap::reserve` makes.
    fn reserve(len: usize) -> std::io::Result<Self> {
        // SAFETY: a fresh anonymous mapping aliases nothing.
        let at = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if at == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(at.cast(), len))
    }

    /// Make `[offset, offset + len)` readable and writable.
    fn open(&self, offset: usize, len: usize) -> std::io::Result<()> {
        if len == 0 {
            return Ok(());
        }
        // SAFETY: the range lies inside this mapping.
        let status = unsafe {
            libc::mprotect(
                self.0.add(offset).cast(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        };
        if status != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Replace `[offset, offset + len)` with fresh zero pages under
    /// `PROT_NONE`, in one call.
    fn reset(&self, offset: usize, len: usize) -> bool {
        if len == 0 {
            return true;
        }
        // SAFETY: the range lies inside this mapping, which nothing else
        // uses while it is reset.
        let at = unsafe {
            libc::mmap(
                self.0.add(offset).cast(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
                -1,
                0,
            )
        };
        at != libc::MAP_FAILED
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the mapping is owned and no longer used.
        unsafe { libc::munmap(self.0.cast(), self.1) };
    }
}

/// One kind's idle slots, most recently ended last.
struct Idle {
    most: usize,
    slots: Mutex<Vec<Mapping>>,
}

impl Idle {
    fn take(&self, len: usize) -> Option<Mapping> {
        let mut slots = self.slots.lock().unwrap();
        let at = slots.iter().rposition(|slot| slot.1 == len)?;
        Some(slots.swap_remove(at))
    }

    fn give(&self, slot: Mapping) {
        let mut slots = self.slots.lock().unwrap();
        if slots.len() < self.most {
            slots.push(slot);
        }
    }
}

/// The run's slot caches, sized from its declared root limit: idle stacks
/// keep at most the program's own limit resident.
pub(crate) struct Slots {
    memories: Idle,
    stacks: Idle,
    page: usize,
}

impl Slots {
    pub(crate) fn new(root_limit: u64) -> Self {
        let most = (root_limit / STACK_BYTES).min(MOST_IDLE) as usize;
        Self {
            memories: Idle {
                most,
                slots: Mutex::default(),
            },
            stacks: Idle {
                most,
                slots: Mutex::default(),
            },
            page: page(),
        }
    }
}

/// A linear memory in its slot.
struct Memory {
    slots: &'static Slots,
    slot: Option<Mapping>,
    /// The pre-guard's length: the base's offset in the slot.
    pre: usize,
    reserved: usize,
    len: usize,
    /// Accessible bytes from the base, page-rounded.
    open: usize,
}

impl Memory {
    fn slot(&self) -> &Mapping {
        self.slot.as_ref().expect("a live memory holds its slot")
    }
}

// SAFETY: as `MmapMemory`: the base never moves (`byte_capacity` is the
// whole reservation), the accessible bytes are exactly `byte_size` rounded
// up to a page, and every byte past them up to the guard's end is
// `PROT_NONE`.
unsafe impl LinearMemory for Memory {
    fn byte_size(&self) -> usize {
        self.len
    }

    fn byte_capacity(&self) -> usize {
        self.reserved
    }

    /// A host refusal returns its `ENOMEM`, which `memory_grow_failed`
    /// stops as `HostMemory` (FFI-13 Bytes).
    fn grow_to(&mut self, size: usize) -> Result<(), Error> {
        if size > self.reserved {
            return Err(Error::msg("contained memory past its reservation"));
        }
        let open = round_up(size, self.slots.page);
        if open > self.open {
            self.slot()
                .open(self.pre + self.open, open - self.open)
                .map_err(Error::from)?;
            self.open = open;
        }
        self.len = size;
        Ok(())
    }

    fn as_ptr(&self) -> *mut u8 {
        // SAFETY: the base lies inside the slot.
        unsafe { self.slot().0.add(self.pre) }
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        let slot = self.slot.take().expect("a live memory holds its slot");
        if slot.reset(self.pre, self.open) {
            self.slots.memories.give(slot);
        }
    }
}

/// A host refusal of a slot stops the run as `HostMemory` here, since
/// `new_memory` can return only a message (FFI-13 Bytes, FAIL-15).
fn refused(error: std::io::Error) -> String {
    if error.raw_os_error() == Some(libc::ENOMEM) {
        crate::process::stop(b"HostMemory");
    }
    error.to_string()
}

/// The engine's creators over the run's caches.
#[derive(Clone, Copy)]
pub(crate) struct Creator(pub(crate) &'static Slots);

// SAFETY: each memory is zero-filled and laid out as `LinearMemory` above
// says, with the requested reservation and guard.
unsafe impl MemoryCreator for Creator {
    fn new_memory(
        &self,
        _: MemoryType,
        minimum: usize,
        _: Option<usize>,
        reserved: Option<usize>,
        guard: usize,
    ) -> Result<Box<dyn LinearMemory>, String> {
        let slots = self.0;
        let reserved = reserved.expect("the on-demand allocator gives a reservation");
        if minimum > reserved {
            return Err("contained memory past its reservation".into());
        }
        let len = guard + reserved + guard;
        let slot = match slots.memories.take(len) {
            Some(slot) => slot,
            None => Mapping::reserve(len).map_err(refused)?,
        };
        let open = round_up(minimum, slots.page);
        if let Err(error) = slot.open(guard, open) {
            // The slot was never handed out, so it is unmapped, not reused.
            drop(slot);
            return Err(refused(error));
        }
        Ok(Box::new(Memory {
            slots,
            slot: Some(slot),
            pre: guard,
            reserved,
            len: minimum,
            open,
        }))
    }
}

/// A fiber stack in its slot: one guard page, then the stack.
struct Stack {
    slots: &'static Slots,
    slot: Option<Mapping>,
}

impl Stack {
    fn slot(&self) -> &Mapping {
        self.slot.as_ref().expect("a live stack holds its slot")
    }
}

// SAFETY: as Wasmtime 48.0.10's `MmapFiberStack` (`wasmtime-internal-fiber`
// `unix.rs`): page-aligned, the guard page `PROT_NONE` below the stack.
unsafe impl StackMemory for Stack {
    fn top(&self) -> *mut u8 {
        let slot = self.slot();
        slot.0.wrapping_add(slot.1)
    }

    fn range(&self) -> Range<usize> {
        let slot = self.slot();
        slot.0 as usize + self.slots.page..slot.0 as usize + slot.1
    }

    fn guard_range(&self) -> Range<*mut u8> {
        let slot = self.slot();
        slot.0..slot.0.wrapping_add(self.slots.page)
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        let slot = self.slot.take().expect("a live stack holds its slot");
        self.slots.stacks.give(slot);
    }
}

// SAFETY: each stack has its guard page. A reused stack holds only host
// frames of an ended call, which no guest can read; unless `zeroed` asks, it
// is not cleared, as Wasmtime's own stack pool does without
// `async_stack_zeroing` (`pooling/unix_stack_pool.rs`, `zero_stack`). A host
// refusal returns its `ENOMEM`, which `settle` stops as `HostMemory`.
unsafe impl StackCreator for Creator {
    fn new_stack(&self, size: usize, zeroed: bool) -> Result<Box<dyn StackMemory>, Error> {
        let slots = self.0;
        let page = slots.page;
        let size = round_up(size.max(1), page);
        let len = page + size;
        let slot = match slots.stacks.take(len) {
            Some(slot) => {
                if zeroed {
                    // SAFETY: the idle stack is owned and unused.
                    let status = unsafe {
                        libc::madvise(slot.0.add(page).cast(), size, libc::MADV_DONTNEED)
                    };
                    if status != 0 {
                        return Err(std::io::Error::last_os_error().into());
                    }
                }
                slot
            }
            None => {
                let slot = Mapping::reserve(len)?;
                slot.open(page, size)?;
                slot
            }
        };
        Ok(Box::new(Stack {
            slots,
            slot: Some(slot),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The permissions `/proc/self/maps` gives the page at `at`.
    fn protection(at: usize) -> String {
        let maps = std::fs::read_to_string("/proc/self/maps").unwrap();
        for line in maps.lines() {
            let (range, rest) = line.split_once(' ').unwrap();
            let (start, end) = range.split_once('-').unwrap();
            let start = usize::from_str_radix(start, 16).unwrap();
            let end = usize::from_str_radix(end, 16).unwrap();
            if (start..end).contains(&at) {
                return rest[..4].to_owned();
            }
        }
        panic!("{at:#x} is not mapped");
    }

    /// FW31 at the slot: a memory made in the slot of one that grew to 18
    /// pages and wrote them sees zeros in its 2 pages, and the kernel holds
    /// every page past them `PROT_NONE` again, so a guest access there traps
    /// as it would in a fresh reservation.
    #[test]
    fn reused_slot_is_zeroed_and_closed_past_its_size() {
        const PAGE: usize = 65536;
        let creator = Creator(Box::leak(Box::new(Slots::new(256 << 20))));
        let make = || {
            creator
                .new_memory(
                    MemoryType::new(2, None),
                    2 * PAGE,
                    None,
                    Some(1 << 32),
                    32 << 20,
                )
                .unwrap()
        };
        let mut first = make();
        first.grow_to(18 * PAGE).unwrap();
        let base = first.as_ptr();
        // SAFETY: the 18 pages are accessible.
        unsafe { std::ptr::write_bytes(base, 0xAB, 18 * PAGE) };
        drop(first);
        let second = make();
        assert_eq!(second.as_ptr(), base, "the slot was not reused");
        assert_eq!(second.byte_size(), 2 * PAGE);
        // SAFETY: the 2 pages are accessible.
        let bytes = unsafe { std::slice::from_raw_parts(base, 2 * PAGE) };
        assert!(bytes.iter().all(|&b| b == 0));
        assert_eq!(protection(base as usize), "rw-p");
        for page in 2..18 {
            assert_eq!(
                protection(base as usize + page * PAGE),
                "---p",
                "page {page}"
            );
        }
    }
}
